//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const EVENT_CHANNEL_CAPACITY: usize = 256;
const BRAIN_EVENT_SCHEMA_VERSION: u32 = 13;
const BRAIN_METADATA_VERSION: u32 = 1;
const BRAIN_INITIALIZATION_VERSION: u32 = 1;
const DEFAULT_INITIALIZATION_MODULE: &str = "finch.brain.initialization";
const DEFAULT_INITIALIZATION_SOURCE: &str =
    "(define (finch-brain-initialized) : int 1)";

/// Stable identity of one durable Brain. Names are mutable human aliases;
/// this ID is what future runs, attachments, cursors, and grants reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrainId(pub uuid::Uuid);

impl BrainId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }
}

/// Stable identity of one client projection of a Brain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentId(pub uuid::Uuid);

impl AttachmentId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Identity of one live transport connection for a durable attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(pub uuid::Uuid);

impl ConnectionId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self(uuid::Uuid::nil())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerLeaseId(pub uuid::Uuid);

impl RunnerLeaseId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerHandoffId(pub uuid::Uuid);

impl RunnerHandoffId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub uuid::Uuid);

impl RunId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScheduleId(pub uuid::Uuid);

impl ScheduleId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunKind {
    Interactive,
    Speculative,
    Scheduled,
    Subagent,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunStatus {
    QueuedForEnvironment,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl BrainRunStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRun {
    pub run_id: RunId,
    pub kind: BrainRunKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    pub request_seq: u64,
    pub initiating_attachment_id: AttachmentId,
    pub initiated_by: String,
    pub status: BrainRunStatus,
    pub started_ms: u64,
    pub updated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRunnerLease {
    pub lease_id: RunnerLeaseId,
    pub subject: String,
    pub environment_generation: u64,
    pub acquired_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRunnerHandoff {
    pub handoff_id: RunnerHandoffId,
    pub from_lease_id: RunnerLeaseId,
    pub requested_by: String,
    pub target_subject: String,
    pub environment_generation: u64,
    pub requested_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    Runner,
    Driver,
    Consultant,
    Observer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainAttachment {
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub acknowledged_seq: u64,
    pub connected: bool,
    pub connection_id: Option<ConnectionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttachmentCursorFile {
    version: u32,
    brain_id: BrainId,
    cursors: HashMap<AttachmentId, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BrainMetadata {
    version: u32,
    brain_id: BrainId,
    created_ms: u64,
}

/// Reviewed, immutable program that establishes a Brain's initial typed state.
/// Loading this record is inert: execution requires an explicit scheduled run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainInitialization {
    pub version: u32,
    pub brain_id: BrainId,
    pub module: String,
    pub module_revision: u32,
    pub language: ProgramLanguage,
    pub source: String,
    pub source_sha256: String,
    pub capability_budget: crate::vm::EffectSet,
}

/// Durable, non-authority-bearing identity for a reviewed module scheduled by
/// the Brain itself. Public schedule creation never accepts this marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainScheduleModuleIdentity {
    pub module: String,
    pub module_revision: u32,
    pub source_sha256: String,
}

impl BrainInitialization {
    fn reviewed_default(brain_id: BrainId) -> Self {
        Self {
            version: BRAIN_INITIALIZATION_VERSION,
            brain_id,
            module: DEFAULT_INITIALIZATION_MODULE.into(),
            module_revision: 1,
            language: ProgramLanguage::Lisp,
            source: DEFAULT_INITIALIZATION_SOURCE.into(),
            source_sha256: hex::encode(Sha256::digest(DEFAULT_INITIALIZATION_SOURCE.as_bytes())),
            capability_budget: crate::vm::EffectSet::pure(),
        }
    }

    fn validate(&self, brain_id: BrainId) -> Result<()> {
        anyhow::ensure!(self.version == BRAIN_INITIALIZATION_VERSION,
            "unsupported Brain initialization version {}", self.version);
        anyhow::ensure!(self.brain_id == brain_id,
            "Brain initialization identity does not match metadata");
        anyhow::ensure!(!self.module.trim().is_empty() && self.module_revision > 0,
            "Brain initialization module identity is invalid");
        anyhow::ensure!(!self.source.trim().is_empty(),
            "Brain initialization program is empty");
        let actual = hex::encode(Sha256::digest(self.source.as_bytes()));
        anyhow::ensure!(self.source_sha256 == actual,
            "Brain initialization program digest does not match its source");
        anyhow::ensure!(self == &Self::reviewed_default(brain_id),
            "Brain initialization contract is not the reviewed built-in module");
        Ok(())
    }

    fn module_identity(&self) -> BrainScheduleModuleIdentity {
        BrainScheduleModuleIdentity {
            module: self.module.clone(),
            module_revision: self.module_revision,
            source_sha256: self.source_sha256.clone(),
        }
    }

    fn validate_schedule(&self, schedule: &BrainSchedule) -> Result<()> {
        let identity = schedule
            .module_identity
            .as_ref()
            .context("reviewed Brain-module schedule is missing its module identity")?;
        anyhow::ensure!(identity == &self.module_identity(),
            "Brain-module schedule identity does not match the reviewed initialization module");
        anyhow::ensure!(schedule.language == self.language,
            "Brain initialization schedule language does not match the reviewed module");
        let actual = hex::encode(Sha256::digest(schedule.source.as_bytes()));
        anyhow::ensure!(identity.source_sha256 == actual,
            "Brain initialization schedule digest does not match its source");
        anyhow::ensure!(schedule.source == self.source,
            "Brain initialization schedule source is not the reviewed module");
        anyhow::ensure!(schedule.grant_ceiling == self.capability_budget,
            "Brain initialization schedule capability budget is not the reviewed ceiling");
        anyhow::ensure!(schedule.interval_ms.is_none()
                && schedule.delivery_policy == BrainScheduleDeliveryPolicy::Coalesce,
            "Brain initialization schedule must be a coalesced one-shot");
        Ok(())
    }

    fn validate_schedule_due(
        &self,
        schedule: &BrainSchedule,
        due: &BrainScheduleDue,
    ) -> Result<()> {
        self.validate_schedule(schedule)?;
        anyhow::ensure!(due.language == schedule.language
                && due.source == schedule.source
                && due.grant_ceiling == schedule.grant_ceiling,
            "Brain initialization delivery does not match its reviewed schedule");
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainScheduleDeliveryPolicy {
    Coalesce,
    BoundedCatchUp {
        max_catch_up: u32,
        expires_after_ms: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainSchedule {
    pub schedule_id: ScheduleId,
    #[serde(default = "legacy_schedule_attachment_id")]
    pub initiating_attachment_id: AttachmentId,
    #[serde(default)]
    pub created_by: String,
    #[serde(default)]
    pub grant_ceiling: crate::vm::EffectSet,
    pub language: ProgramLanguage,
    pub source: String,
    pub next_due_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
    pub delivery_policy: BrainScheduleDeliveryPolicy,
    /// Set only by trusted, reviewed Brain-module scheduling paths. This is
    /// persisted so source-equivalent public schedules cannot impersonate or
    /// suppress the module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_identity: Option<BrainScheduleModuleIdentity>,
    pub active: bool,
}

/// One durable schedule delivery and the queued run that owns it. Keeping the
/// run in this event makes due calculation -> runnable work one atomic append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainScheduleDue {
    pub schedule_id: ScheduleId,
    pub run: BrainRun,
    /// Immutable program snapshot for this delivery. Later schedule edits do
    /// not change already queued work.
    #[serde(default = "legacy_schedule_language")]
    pub language: ProgramLanguage,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub grant_ceiling: crate::vm::EffectSet,
    pub due_at_ms: u64,
    pub first_missed_at_ms: u64,
    pub missed_count: u32,
    /// The next occurrence after all ticks represented by this delivery.
    /// `None` atomically retires a one-shot schedule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_ms: Option<u64>,
}

/// The one machine/workspace boundary in which a brain may cause effects.
///
/// There is deliberately no separate `execution_head`: the machine that owns
/// the workspace is the only machine allowed to execute the brain's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainEnvironment {
    pub machine: String,
    pub workspace: PathBuf,
    pub generation: u64,
}

/// Exact participant/environment boundary to which a Brain-owned approval
/// request is addressed. This is policy input, not a bearer credential;
/// possession of this record does not authorize a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainApprovalAudience {
    pub brain_id: BrainId,
    pub brain: String,
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub environment_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainEventKind {
    RunnerLeaseAcquired {
        lease: BrainRunnerLease,
    },
    RunnerLeaseReleased {
        lease_id: RunnerLeaseId,
    },
    RunnerHandoffRequested {
        handoff: BrainRunnerHandoff,
    },
    RunnerHandoffCompleted {
        handoff_id: RunnerHandoffId,
        lease: BrainRunnerLease,
    },
    RunnerHandoffCancelled {
        handoff_id: RunnerHandoffId,
    },
    ClientAttached {
        attachment_id: AttachmentId,
        #[serde(default)]
        connection_id: ConnectionId,
        subject: String,
        role: AttachmentRole,
    },
    ClientDetached {
        attachment_id: AttachmentId,
        #[serde(default)]
        connection_id: ConnectionId,
    },
    RunStarted {
        run: BrainRun,
    },
    RunStatusChanged {
        run_id: RunId,
        status: BrainRunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Prompt {
        text: String,
    },
    /// An explicitly requested helper turn. Its transcript is durable and
    /// inspectable, but is never injected into later interactive context.
    SpeculativePrompt {
        text: String,
    },
    /// A participant-to-participant message. It is durable and enters later
    /// prompt context, but never schedules a provider turn by itself.
    ParticipantMessage {
        text: String,
    },
    /// Atomically replace the Brain-owned task list. The append-only event is
    /// authoritative; frontend lists are projections rebuilt from snapshots.
    TaskListReplaced {
        tasks: Vec<super::tasks::BrainTask>,
    },
    ToolCall {
        request_seq: u64,
        tool_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        request_seq: u64,
        tool_id: String,
        output: String,
        is_error: bool,
    },
    ApprovalRequested {
        request_seq: u64,
        approval_id: String,
        approval_kind: String,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<BrainApprovalAudience>,
        detail: serde_json::Value,
    },
    ApprovalDecided {
        request_seq: u64,
        approval_id: String,
        decision: serde_json::Value,
    },
    Program {
        language: ProgramLanguage,
        source: String,
    },
    ProgramPopped {
        program_seq: u64,
    },
    Result {
        request_seq: u64,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Content-addressed typed-VM state committed after one accepted program.
    /// This is an internal Brain event, not a request to replay source after
    /// restart; the checkpoint bytes live beside the append-only log.
    RuntimeCommitted {
        request_seq: u64,
        runtime_revision: u64,
        checkpoint_sha256: String,
    },
    /// Execute-once VM host-effect fact. This is deliberately stored in the
    /// append-only event log rather than the reducible runtime checkpoint.
    EffectRecorded {
        request_seq: u64,
        execution_id: uuid::Uuid,
        effect: crate::vm::VmSideEffect,
        state: crate::vm::EffectJournalState,
    },
    ScheduleChanged {
        schedule: BrainSchedule,
    },
    ScheduleDue {
        due: BrainScheduleDue,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainEvent {
    /// Version of this durable event envelope. Old logs deserialize as v1 and
    /// are projected into the owning Brain's stable identity while loading.
    #[serde(default = "legacy_brain_event_schema_version")]
    pub schema_version: u32,
    #[serde(default = "BrainId::nil")]
    pub brain_id: BrainId,
    pub seq: u64,
    /// Binds this event to the exact environment revision in which it ran.
    #[serde(default = "initial_environment_generation")]
    pub environment_generation: u64,
    pub sender: String,
    pub created_ms: u64,
    #[serde(flatten)]
    pub kind: BrainEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainProgram {
    pub seq: u64,
    pub sender: String,
    pub language: ProgramLanguage,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    pub brain_id: BrainId,
    pub name: String,
    pub environment: BrainEnvironment,
    pub revision: u64,
    pub events: Vec<BrainEvent>,
    pub program_stack: Vec<BrainProgram>,
    pub attachments: Vec<BrainAttachment>,
    pub runner_lease: Option<BrainRunnerLease>,
    #[serde(default)]
    pub runner_handoff: Option<BrainRunnerHandoff>,
    #[serde(default)]
    pub runs: Vec<BrainRun>,
    /// Current task-list projection derived from `TaskListReplaced` events.
    #[serde(default)]
    pub tasks: Vec<super::tasks::BrainTask>,
    #[serde(default)]
    pub schedules: Vec<BrainSchedule>,
    #[serde(default)]
    pub pending_schedule_dues: Vec<BrainScheduleDue>,
}

impl BrainSnapshot {
    /// Whether this exact runner lease was durably replaced by an addressed
    /// handoff. Frontends use this terminal fact to stop renewal instead of
    /// treating a deliberate transfer like an incidental lease expiry.
    pub fn runner_lease_was_handed_off(&self, lease_id: RunnerLeaseId) -> bool {
        let requested: std::collections::HashSet<_> = self
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                BrainEventKind::RunnerHandoffRequested { handoff }
                    if handoff.from_lease_id == lease_id =>
                {
                    Some(handoff.handoff_id)
                }
                _ => None,
            })
            .collect();
        self.events.iter().any(|event| {
            matches!(
                &event.kind,
                BrainEventKind::RunnerHandoffCompleted { handoff_id, .. }
                    if requested.contains(handoff_id)
            )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrainWireMessage {
    Snapshot { brain: BrainSnapshot },
    Event { event: BrainEvent },
}

struct BrainState {
    brain_id: BrainId,
    events: Vec<BrainEvent>,
    program_stack: Vec<BrainProgram>,
    attachments: HashMap<AttachmentId, BrainAttachment>,
    runs: HashMap<RunId, BrainRun>,
    tasks: Vec<super::tasks::BrainTask>,
    schedules: HashMap<ScheduleId, BrainSchedule>,
    pending_schedule_dues: HashMap<RunId, BrainScheduleDue>,
    runner_lease: Option<BrainRunnerLease>,
    runner_handoff: Option<BrainRunnerHandoff>,
    runtime_checkpoint: Option<RuntimeCheckpointState>,
    runtime_commit_count: u64,
    tx: broadcast::Sender<BrainEvent>,
}

#[derive(Debug, Clone)]
struct RuntimeCheckpointState {
    request_seq: u64,
    durable_revision: u64,
    checkpoint_sha256: String,
}

impl BrainState {
    fn from_events(brain_id: BrainId, events: Vec<BrainEvent>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut state = Self {
            brain_id,
            events: Vec::new(),
            program_stack: Vec::new(),
            attachments: HashMap::new(),
            runs: HashMap::new(),
            tasks: Vec::new(),
            schedules: HashMap::new(),
            pending_schedule_dues: HashMap::new(),
            runner_lease: None,
            runner_handoff: None,
            runtime_checkpoint: None,
            runtime_commit_count: 0,
            tx,
        };
        for mut event in events {
            if event.brain_id == BrainId::nil() {
                event.brain_id = brain_id;
            }
            state.apply(event);
        }
        state
    }

    fn apply(&mut self, event: BrainEvent) {
        match &event.kind {
            BrainEventKind::RunnerLeaseAcquired { lease } => {
                if self
                    .runner_handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.from_lease_id != lease.lease_id)
                {
                    self.runner_handoff = None;
                }
                self.runner_lease = Some(lease.clone());
            }
            BrainEventKind::RunnerLeaseReleased { lease_id } => {
                if self
                    .runner_lease
                    .as_ref()
                    .is_some_and(|lease| lease.lease_id == *lease_id)
                {
                    self.runner_lease = None;
                }
                if self
                    .runner_handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.from_lease_id == *lease_id)
                {
                    self.runner_handoff = None;
                }
            }
            BrainEventKind::RunnerHandoffRequested { handoff } => {
                self.runner_handoff = Some(handoff.clone());
            }
            BrainEventKind::RunnerHandoffCompleted { handoff_id, lease } => {
                if self
                    .runner_handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.handoff_id == *handoff_id)
                {
                    self.runner_handoff = None;
                    self.runner_lease = Some(lease.clone());
                }
            }
            BrainEventKind::RunnerHandoffCancelled { handoff_id } => {
                if self
                    .runner_handoff
                    .as_ref()
                    .is_some_and(|handoff| handoff.handoff_id == *handoff_id)
                {
                    self.runner_handoff = None;
                }
            }
            BrainEventKind::ClientAttached {
                attachment_id,
                connection_id,
                subject,
                role,
            } => {
                let acknowledged_seq = self
                    .attachments
                    .get(attachment_id)
                    .map(|attachment| attachment.acknowledged_seq)
                    .unwrap_or(0);
                self.attachments.insert(
                    *attachment_id,
                    BrainAttachment {
                        attachment_id: *attachment_id,
                        subject: subject.clone(),
                        role: *role,
                        acknowledged_seq,
                        connected: true,
                        connection_id: Some(*connection_id),
                    },
                );
            }
            BrainEventKind::ClientDetached {
                attachment_id,
                connection_id,
            } => {
                if let Some(attachment) = self.attachments.get_mut(attachment_id) {
                    if attachment.connection_id == Some(*connection_id) {
                        attachment.connected = false;
                        attachment.connection_id = None;
                    }
                }
            }
            BrainEventKind::RunStarted { run } => {
                self.runs.insert(run.run_id, run.clone());
            }
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            } => {
                if let Some(run) = self.runs.get_mut(run_id) {
                    run.status = *status;
                    run.updated_ms = event.created_ms;
                    run.detail.clone_from(detail);
                }
                if status.is_terminal() {
                    self.pending_schedule_dues
                        .retain(|_, due| due.run.run_id != *run_id);
                }
            }
            BrainEventKind::ScheduleChanged { schedule } => {
                self.schedules
                    .insert(schedule.schedule_id, schedule.clone());
                if !schedule.active {
                    self.pending_schedule_dues
                        .retain(|_, due| due.schedule_id != schedule.schedule_id);
                }
            }
            BrainEventKind::ScheduleDue { due } => {
                let mut due = due.clone();
                if let Some(schedule) = self.schedules.get_mut(&due.schedule_id) {
                    if event.schema_version < 10 {
                        due.language = schedule.language;
                        due.source.clone_from(&schedule.source);
                        due.grant_ceiling.clone_from(&schedule.grant_ceiling);
                        if schedule.initiating_attachment_id == legacy_schedule_attachment_id() {
                            schedule.initiating_attachment_id = due.run.initiating_attachment_id;
                            schedule.created_by.clone_from(&due.run.initiated_by);
                        }
                    } else {
                        match due.next_due_ms {
                            Some(next_due_ms) => schedule.next_due_ms = next_due_ms,
                            None => schedule.active = false,
                        }
                    }
                }
                self.runs.insert(due.run.run_id, due.run.clone());
                self.pending_schedule_dues.insert(due.run.run_id, due);
            }
            BrainEventKind::TaskListReplaced { tasks } => {
                self.tasks.clone_from(tasks);
            }
            BrainEventKind::Program { language, source } => {
                self.program_stack.push(BrainProgram {
                    seq: event.seq,
                    sender: event.sender.clone(),
                    language: *language,
                    source: source.clone(),
                });
            }
            BrainEventKind::ProgramPopped { program_seq } => {
                if self.program_stack.last().map(|p| p.seq) == Some(*program_seq) {
                    self.program_stack.pop();
                }
            }
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision,
                checkpoint_sha256,
            } => {
                self.runtime_commit_count += 1;
                let durable_revision = self
                    .runtime_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.durable_revision)
                    .unwrap_or(0)
                    .max(*runtime_revision)
                    .max(self.runtime_commit_count);
                if self.runtime_checkpoint.as_ref().is_none_or(|current| {
                    request_seq >= &current.request_seq
                }) {
                    self.runtime_checkpoint = Some(RuntimeCheckpointState {
                        request_seq: *request_seq,
                        durable_revision,
                        checkpoint_sha256: checkpoint_sha256.clone(),
                    });
                } else if let Some(current) = self.runtime_checkpoint.as_mut() {
                    current.durable_revision = durable_revision;
                }
            }
            BrainEventKind::Prompt { .. }
            | BrainEventKind::SpeculativePrompt { .. }
            | BrainEventKind::ParticipantMessage { .. }
            | BrainEventKind::ToolCall { .. }
            | BrainEventKind::ToolResult { .. }
            | BrainEventKind::ApprovalRequested { .. }
            | BrainEventKind::ApprovalDecided { .. }
            | BrainEventKind::EffectRecorded { .. }
            | BrainEventKind::Result { .. } => {}
        }
        self.events.push(event);
    }
}

/// Authoritative persistent store of named Brains.
///
/// Each brain is stored as human-browsable JSON Lines under
/// `~/.finch/brains/<name>/events.jsonl`.  The log is authoritative; the
/// program stack is rebuilt from it after a daemon restart.
#[derive(Clone)]
pub struct BrainStore {
    root: Option<PathBuf>,
    environment: BrainEnvironment,
    brains: Arc<RwLock<HashMap<String, BrainState>>>,
    initializations: Arc<RwLock<HashMap<String, BrainInitialization>>>,
    runtimes: Arc<RwLock<HashMap<String, Arc<crate::runtime::ProgramRuntime>>>>,
    runtime_checkpoints:
        Arc<RwLock<HashMap<String, crate::vm::TypedRuntimeCheckpoint>>>,
    /// One ordered turn lane per Brain. HTTP/WebSocket clients may submit
    /// concurrently, but accepted input, VM commit, and its Result event must
    /// remain an indivisible sequence against the authoritative revision.
    execution_locks: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl BrainStore {
    pub fn new(machine: impl Into<String>) -> Self {
        let root = dirs::home_dir().map(|p| p.join(".finch").join("brains"));
        Self::with_root(machine, root)
    }

    pub fn with_root(machine: impl Into<String>, root: Option<PathBuf>) -> Self {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_environment(machine, workspace, root)
    }

    pub fn with_environment(
        machine: impl Into<String>,
        workspace: impl Into<PathBuf>,
        root: Option<PathBuf>,
    ) -> Self {
        let workspace = workspace.into();
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        Self {
            root,
            environment: BrainEnvironment {
                machine: machine.into(),
                workspace,
                generation: initial_environment_generation(),
            },
            brains: Arc::new(RwLock::new(HashMap::new())),
            initializations: Arc::new(RwLock::new(HashMap::new())),
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            runtime_checkpoints: Arc::new(RwLock::new(HashMap::new())),
            execution_locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn environment(&self) -> &BrainEnvironment {
        &self.environment
    }

    pub(crate) fn execution_lock(&self, name: &str) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let name = Self::validate_name(name)?;
        if let Some(lock) = self
            .execution_locks
            .read()
            .expect("shared brain execution-lock map poisoned")
            .get(name)
            .cloned()
        {
            return Ok(lock);
        }
        let mut locks = self
            .execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned");
        Ok(locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    pub fn validate_name(name: &str) -> Result<&str> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            anyhow::bail!("brain name must use 1-64 letters, numbers, '-' or '_'");
        }
        Ok(name)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        self.load_all()?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let mut names: Vec<_> = brains.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    pub fn snapshot(&self, name: &str) -> Result<BrainSnapshot> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        Ok(BrainSnapshot {
            brain_id: state.brain_id,
            name: name.to_string(),
            environment: self.environment.clone(),
            revision: state.events.last().map(|e| e.seq).unwrap_or(0),
            events: state.events.clone(),
            program_stack: state.program_stack.clone(),
            attachments: sorted_attachments(&state.attachments),
            runner_lease: state
                .runner_lease
                .clone()
                .filter(|lease| lease.expires_ms > unix_millis()),
            runner_handoff: state
                .runner_handoff
                .clone()
                .filter(|handoff| handoff.expires_ms > unix_millis()),
            runs: sorted_runs(&state.runs),
            tasks: state.tasks.clone(),
            schedules: sorted_schedules(&state.schedules),
            pending_schedule_dues: sorted_schedule_dues(&state.pending_schedule_dues),
        })
    }

    /// Return the persisted initialization contract without executing it or
    /// appending any observable Brain event.
    pub fn initialization(&self, name: &str) -> Result<BrainInitialization> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        self.initializations
            .read()
            .expect("shared Brain initialization lock poisoned")
            .get(name)
            .cloned()
            .context("Brain initialization was removed concurrently")
    }

    /// Idempotently journal initialization as a one-shot schedule.
    ///
    /// An active or delivered-but-nonterminal attempt is returned unchanged.
    /// A successfully completed attempt remains idempotently complete. A
    /// schedule cancelled before delivery, or an attempt whose run failed,
    /// was cancelled, or was interrupted, is retried with a new schedule ID.
    /// The ordinary
    /// scheduler creates the explicit `BrainRun`, and the runner is limited to
    /// the reviewed contract's capability ceiling.
    pub fn schedule_initialization(
        &self,
        name: &str,
        initiating_attachment_id: AttachmentId,
        connection_id: ConnectionId,
        next_due_ms: u64,
    ) -> Result<BrainSchedule> {
        let name = Self::validate_name(name)?;
        let initialization = self.initialization(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let attachment = state.attachments.get(&initiating_attachment_id)
            .context("initialization scheduler attachment does not exist")?;
        anyhow::ensure!(
            attachment.connected && attachment.connection_id == Some(connection_id),
            "Brain attachment connection is no longer current"
        );
        anyhow::ensure!(
            attachment.role == AttachmentRole::Driver,
            "only an active Brain driver can schedule initialization"
        );
        let sender = attachment.subject.clone();
        let module_identity = initialization.module_identity();
        let mut seen_attempts = std::collections::HashSet::new();
        let attempt_ids = state.events.iter().rev().filter_map(|event| match &event.kind {
            BrainEventKind::ScheduleChanged { schedule }
                if schedule.module_identity.as_ref() == Some(&module_identity)
                    && seen_attempts.insert(schedule.schedule_id) =>
            {
                Some(schedule.schedule_id)
            }
            _ => None,
        }).collect::<Vec<_>>();
        for schedule_id in attempt_ids {
            let existing = state.schedules.get(&schedule_id)
                .expect("module schedule event was projected")
                .clone();
            initialization.validate_schedule(&existing)?;
            if existing.active {
                return Ok(existing);
            }
            let run_status = state.events.iter().rev().find_map(|event| match &event.kind {
                BrainEventKind::ScheduleDue { due }
                    if due.schedule_id == existing.schedule_id =>
                {
                    state.runs.get(&due.run.run_id).map(|run| run.status)
                }
                _ => None,
            });
            if run_status.is_some_and(|status| {
                !matches!(
                    status,
                    BrainRunStatus::Failed
                        | BrainRunStatus::Cancelled
                        | BrainRunStatus::Interrupted
                )
            }) {
                return Ok(existing);
            }
        }
        let schedule = BrainSchedule {
            schedule_id: ScheduleId::new(),
            initiating_attachment_id,
            created_by: sender.clone(),
            grant_ceiling: initialization.capability_budget.clone(),
            language: initialization.language,
            source: initialization.source.clone(),
            next_due_ms,
            interval_ms: None,
            delivery_policy: BrainScheduleDeliveryPolicy::Coalesce,
            module_identity: Some(module_identity),
            active: true,
        };
        initialization.validate_schedule(&schedule)?;
        self.push_locked(name, state, &sender, BrainEventKind::ScheduleChanged {
            schedule: schedule.clone(),
        })?;
        Ok(schedule)
    }

    pub fn create_schedule(
        &self,
        name: &str,
        created_by: &str,
        initiating_attachment_id: AttachmentId,
        language: ProgramLanguage,
        source: impl Into<String>,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
    ) -> Result<BrainSchedule> {
        let name = Self::validate_name(name)?;
        let created_by = validate_participant_subject("schedule creator", created_by)?;
        let source = source.into();
        if source.trim().is_empty() {
            anyhow::bail!("scheduled program source cannot be empty");
        }
        if interval_ms == Some(0) {
            anyhow::bail!("schedule interval must be greater than zero");
        }
        if let BrainScheduleDeliveryPolicy::BoundedCatchUp {
            max_catch_up,
            expires_after_ms,
        } = &delivery_policy
        {
            if *max_catch_up == 0 || *expires_after_ms == 0 {
                anyhow::bail!(
                    "bounded catch-up requires a positive backlog bound and expiry"
                );
            }
        }
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&initiating_attachment_id)
            .context("schedule creator attachment does not exist")?;
        if attachment.subject != created_by {
            anyhow::bail!("schedule creator does not own the initiating attachment");
        }
        if !matches!(attachment.role, AttachmentRole::Runner | AttachmentRole::Driver) {
            anyhow::bail!("attachment role cannot create scheduled ProgramRuns");
        }
        let schedule = BrainSchedule {
            schedule_id: ScheduleId::new(),
            initiating_attachment_id,
            created_by: created_by.to_string(),
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
            module_identity: None,
            active: true,
        };
        self.push_locked(
            name,
            state,
            created_by,
            BrainEventKind::ScheduleChanged {
                schedule: schedule.clone(),
            },
        )?;
        Ok(schedule)
    }

    /// Atomically advance due schedules and append the exact queued ProgramRun
    /// for each delivery. The returned runs are durable before this method
    /// returns and are safe for the runner broker to dispatch immediately.
    pub fn queue_due_schedules(&self, name: &str, now_ms: u64) -> Result<Vec<BrainRun>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let mut schedules = state
            .schedules
            .values()
            .filter(|schedule| schedule.active && schedule.next_due_ms <= now_ms)
            .cloned()
            .collect::<Vec<_>>();
        schedules.sort_by_key(|schedule| (schedule.next_due_ms, schedule.schedule_id.0));

        let mut queued = Vec::new();
        for schedule in schedules {
            let pending = state
                .pending_schedule_dues
                .values()
                .filter(|due| due.schedule_id == schedule.schedule_id)
                .cloned()
                .collect::<Vec<_>>();
            if pending.iter().any(|due| {
                state.runs.get(&due.run.run_id).is_some_and(|run| {
                    matches!(run.status, BrainRunStatus::Running | BrainRunStatus::AwaitingApproval)
                })
            }) {
                continue;
            }

            let (occurrence_count, last_due_ms, next_due_ms) =
                schedule_due_window(&schedule, now_ms)?;
            match &schedule.delivery_policy {
                BrainScheduleDeliveryPolicy::Coalesce => {
                    if let Some(existing) = pending.into_iter().find(|due| {
                        state.runs.get(&due.run.run_id).is_some_and(|run| {
                            run.status == BrainRunStatus::QueuedForEnvironment
                        })
                    }) {
                        let event_seq = state.events.last().map(|event| event.seq + 1).unwrap_or(1);
                        let mut run = existing.run;
                        run.request_seq = event_seq;
                        run.updated_ms = now_ms;
                        let due = BrainScheduleDue {
                            schedule_id: schedule.schedule_id,
                            run: run.clone(),
                            language: schedule.language,
                            source: schedule.source.clone(),
                            grant_ceiling: schedule.grant_ceiling.clone(),
                            due_at_ms: last_due_ms,
                            first_missed_at_ms: existing.first_missed_at_ms,
                            missed_count: existing
                                .missed_count
                                .saturating_add(occurrence_count),
                            next_due_ms,
                        };
                        self.push_locked(
                            name,
                            state,
                            "daemon:scheduler",
                            BrainEventKind::ScheduleDue { due },
                        )?;
                        queued.push(run);
                    } else {
                        let run = queued_schedule_run(state, &schedule, now_ms);
                        let due = BrainScheduleDue {
                            schedule_id: schedule.schedule_id,
                            run: run.clone(),
                            language: schedule.language,
                            source: schedule.source.clone(),
                            grant_ceiling: schedule.grant_ceiling.clone(),
                            due_at_ms: last_due_ms,
                            first_missed_at_ms: schedule.next_due_ms,
                            missed_count: occurrence_count,
                            next_due_ms,
                        };
                        self.push_locked(
                            name,
                            state,
                            "daemon:scheduler",
                            BrainEventKind::ScheduleDue { due },
                        )?;
                        queued.push(run);
                    }
                }
                BrainScheduleDeliveryPolicy::BoundedCatchUp {
                    max_catch_up,
                    expires_after_ms,
                } => {
                    let capacity = (*max_catch_up as usize).saturating_sub(pending.len());
                    if capacity == 0 {
                        continue;
                    }
                    let cutoff = now_ms.saturating_sub(*expires_after_ms);
                    let mut due_at_ms = schedule.next_due_ms.max(cutoff);
                    if let Some(interval_ms) = schedule.interval_ms {
                        if due_at_ms > schedule.next_due_ms {
                            let skipped = due_at_ms
                                .saturating_sub(schedule.next_due_ms)
                                .div_ceil(interval_ms);
                            due_at_ms = schedule
                                .next_due_ms
                                .saturating_add(skipped.saturating_mul(interval_ms));
                        }
                    }
                    for _ in 0..capacity {
                        if due_at_ms > now_ms {
                            break;
                        }
                        let delivery_next = schedule
                            .interval_ms
                            .and_then(|interval| due_at_ms.checked_add(interval));
                        let run = queued_schedule_run(state, &schedule, now_ms);
                        let due = BrainScheduleDue {
                            schedule_id: schedule.schedule_id,
                            run: run.clone(),
                            language: schedule.language,
                            source: schedule.source.clone(),
                            grant_ceiling: schedule.grant_ceiling.clone(),
                            due_at_ms,
                            first_missed_at_ms: due_at_ms,
                            missed_count: 1,
                            next_due_ms: delivery_next,
                        };
                        self.push_locked(
                            name,
                            state,
                            "daemon:scheduler",
                            BrainEventKind::ScheduleDue { due },
                        )?;
                        queued.push(run);
                        let Some(next) = delivery_next else {
                            break;
                        };
                        due_at_ms = next;
                    }
                }
            }
        }
        Ok(queued)
    }

    pub fn inspect_schedule(
        &self,
        name: &str,
        schedule_id: ScheduleId,
    ) -> Result<Option<BrainSchedule>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        Ok(self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.schedules.get(&schedule_id))
            .cloned())
    }

    pub fn cancel_schedule(
        &self,
        name: &str,
        cancelled_by: &str,
        initiating_attachment_id: AttachmentId,
        schedule_id: ScheduleId,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        let cancelled_by = validate_participant_subject("schedule canceller", cancelled_by)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let Some(mut schedule) = state.schedules.get(&schedule_id).cloned() else {
            return Ok(false);
        };
        if !schedule.active {
            return Ok(false);
        }
        if schedule.created_by != cancelled_by
            || schedule.initiating_attachment_id != initiating_attachment_id
        {
            anyhow::bail!("only the schedule creator attachment may cancel this schedule");
        }
        schedule.active = false;
        self.push_locked(
            name,
            state,
            cancelled_by,
            BrainEventKind::ScheduleChanged { schedule },
        )?;
        Ok(true)
    }

    pub fn start_run(
        &self,
        name: &str,
        sender: &str,
        kind: BrainRunKind,
        request_seq: u64,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
    ) -> Result<BrainRun> {
        self.start_run_with_parent(
            name,
            sender,
            kind,
            request_seq,
            initiating_attachment_id,
            status,
            None,
        )
    }

    pub fn start_run_with_parent(
        &self,
        name: &str,
        sender: &str,
        kind: BrainRunKind,
        request_seq: u64,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
        parent_run_id: Option<RunId>,
    ) -> Result<BrainRun> {
        self.start_run_with_parent_id(
            name,
            sender,
            RunId::new(),
            kind,
            request_seq,
            initiating_attachment_id,
            status,
            parent_run_id,
            None,
        )
    }

    /// Start a run whose identity was allocated by the authoritative caller.
    /// This is used for child tasks whose task UUID is also their durable
    /// BrainRun UUID. An exact retry returns the existing run; conflicting
    /// identity reuse fails closed.
    #[allow(clippy::too_many_arguments)]
    pub fn start_run_with_parent_id(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        kind: BrainRunKind,
        request_seq: u64,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
        parent_run_id: Option<RunId>,
        detail: Option<String>,
    ) -> Result<BrainRun> {
        let name = Self::validate_name(name)?;
        let sender = validate_participant_subject("run initiator", sender)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        if !state.events.iter().any(|event| event.seq == request_seq) {
            anyhow::bail!("Brain run request event {request_seq} does not exist");
        }
        if let Some(existing) = state.runs.get(&run_id) {
            anyhow::ensure!(
                existing.kind == kind
                    && existing.parent_run_id == parent_run_id
                    && existing.request_seq == request_seq
                    && existing.initiating_attachment_id == initiating_attachment_id
                    && existing.initiated_by == sender,
                "Brain run identity {} was reused with conflicting ancestry or principal",
                run_id.0
            );
            return Ok(existing.clone());
        }
        if let Some(parent_run_id) = parent_run_id {
            let parent = state
                .runs
                .get(&parent_run_id)
                .with_context(|| format!("parent Brain run {} does not exist", parent_run_id.0))?;
            if parent.status.is_terminal() {
                anyhow::bail!("terminal Brain run cannot start a child");
            }
        }
        let now = unix_millis();
        let run = BrainRun {
            run_id,
            kind,
            parent_run_id,
            request_seq,
            initiating_attachment_id,
            initiated_by: sender.to_string(),
            status,
            started_ms: now,
            updated_ms: now,
            detail,
        };
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::RunStarted { run: run.clone() },
        )?;
        Ok(run)
    }

    pub fn inspect_run(&self, name: &str, run_id: RunId) -> Result<BrainRun> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        brains
            .get(name)
            .context("Brain was removed concurrently")?
            .runs
            .get(&run_id)
            .cloned()
            .with_context(|| format!("Brain run {} does not exist", run_id.0))
    }

    pub fn transition_run(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        status: BrainRunStatus,
        detail: Option<String>,
    ) -> Result<BrainRun> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let current = state
            .runs
            .get(&run_id)
            .with_context(|| format!("Brain run {} does not exist", run_id.0))?;
        validate_run_transition(current.status, status)?;
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            },
        )?;
        Ok(state
            .runs
            .get(&run_id)
            .expect("run transition was projected")
            .clone())
    }

    pub fn acquire_runner_lease(
        &self,
        name: &str,
        subject: &str,
        environment_generation: u64,
        lease_id: Option<RunnerLeaseId>,
        ttl_ms: u64,
    ) -> Result<BrainRunnerLease> {
        let name = Self::validate_name(name)?;
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
            anyhow::bail!("runner subject must be 1-128 printable characters");
        }
        if environment_generation != self.environment.generation {
            anyhow::bail!("runner environment generation does not match this Brain");
        }
        if !(5_000..=300_000).contains(&ttl_ms) {
            anyhow::bail!("runner lease TTL must be between 5 and 300 seconds");
        }
        self.ensure_loaded(name)?;
        let now = unix_millis();
        let expires_ms = now.saturating_add(ttl_ms);
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        if let Some(current) = state
            .runner_lease
            .clone()
            .filter(|current| current.expires_ms > now)
        {
            if current.subject != subject || lease_id != Some(current.lease_id) {
                anyhow::bail!("Brain already has a live runner lease");
            }
            state
                .runner_lease
                .as_mut()
                .expect("live runner lease checked above")
                .expires_ms = expires_ms;
            let mut renewed = current;
            renewed.expires_ms = expires_ms;
            return Ok(renewed);
        }
        if lease_id.is_some() {
            anyhow::bail!("runner lease expired; acquire a new lease identity");
        }
        let lease = BrainRunnerLease {
            lease_id: RunnerLeaseId::new(),
            subject: subject.to_string(),
            environment_generation,
            acquired_ms: now,
            expires_ms,
        };
        self.push_locked(
            name,
            state,
            subject,
            BrainEventKind::RunnerLeaseAcquired {
                lease: lease.clone(),
            },
        )?;
        Ok(lease)
    }

    pub fn release_runner_lease(&self, name: &str, lease_id: RunnerLeaseId) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let current = state
            .runner_lease
            .as_ref()
            .context("Brain has no runner lease")?;
        if current.lease_id != lease_id {
            anyhow::bail!("runner lease is no longer current");
        }
        let subject = current.subject.clone();
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::RunnerLeaseReleased { lease_id },
        )?;
        Ok(())
    }

    pub fn request_runner_handoff(
        &self,
        name: &str,
        requested_by: &str,
        target_subject: &str,
        expected_lease_id: RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
    ) -> Result<BrainRunnerHandoff> {
        let name = Self::validate_name(name)?;
        let requested_by = validate_participant_subject("handoff requester", requested_by)?;
        let target_subject = validate_participant_subject("handoff target", target_subject)?;
        if environment_generation != self.environment.generation {
            anyhow::bail!("runner handoff environment generation does not match this Brain");
        }
        if !(5_000..=300_000).contains(&ttl_ms) {
            anyhow::bail!("runner handoff TTL must be between 5 and 300 seconds");
        }
        self.ensure_loaded(name)?;
        let now = unix_millis();
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let current = state
            .runner_lease
            .as_ref()
            .filter(|lease| lease.expires_ms > now)
            .context("Brain has no live runner lease to hand off")?;
        if current.lease_id != expected_lease_id {
            anyhow::bail!("runner lease is no longer current");
        }
        if current.environment_generation != environment_generation {
            anyhow::bail!("runner lease belongs to a different environment generation");
        }
        if current.subject == target_subject {
            anyhow::bail!("runner handoff target already owns the lease");
        }
        if state
            .runner_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.expires_ms > now)
        {
            anyhow::bail!("Brain already has a pending runner handoff");
        }
        let expires_ms = now.saturating_add(ttl_ms).min(current.expires_ms);
        if expires_ms.saturating_sub(now) < 5_000 {
            anyhow::bail!("runner lease expires too soon to create a handoff");
        }
        let handoff = BrainRunnerHandoff {
            handoff_id: RunnerHandoffId::new(),
            from_lease_id: current.lease_id,
            requested_by: requested_by.to_string(),
            target_subject: target_subject.to_string(),
            environment_generation,
            requested_ms: now,
            expires_ms,
        };
        self.push_locked(
            name,
            state,
            requested_by,
            BrainEventKind::RunnerHandoffRequested {
                handoff: handoff.clone(),
            },
        )?;
        Ok(handoff)
    }

    pub fn accept_runner_handoff(
        &self,
        name: &str,
        target_subject: &str,
        handoff_id: RunnerHandoffId,
        environment_generation: u64,
        ttl_ms: u64,
    ) -> Result<BrainRunnerLease> {
        let name = Self::validate_name(name)?;
        let target_subject = validate_participant_subject("handoff target", target_subject)?;
        if environment_generation != self.environment.generation {
            anyhow::bail!("runner handoff environment generation does not match this Brain");
        }
        if !(5_000..=300_000).contains(&ttl_ms) {
            anyhow::bail!("runner lease TTL must be between 5 and 300 seconds");
        }
        self.ensure_loaded(name)?;
        let now = unix_millis();
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let handoff = state
            .runner_handoff
            .as_ref()
            .filter(|handoff| handoff.handoff_id == handoff_id)
            .context("runner handoff is no longer current")?;
        if handoff.expires_ms <= now {
            anyhow::bail!("runner handoff has expired");
        }
        if handoff.target_subject != target_subject {
            anyhow::bail!("runner handoff is addressed to a different subject");
        }
        if handoff.environment_generation != environment_generation {
            anyhow::bail!("runner handoff belongs to a different environment generation");
        }
        let current = state
            .runner_lease
            .as_ref()
            .filter(|lease| lease.expires_ms > now)
            .context("source runner lease is no longer live")?;
        if current.lease_id != handoff.from_lease_id {
            anyhow::bail!("source runner lease is no longer current");
        }
        let lease = BrainRunnerLease {
            lease_id: RunnerLeaseId::new(),
            subject: target_subject.to_string(),
            environment_generation,
            acquired_ms: now,
            expires_ms: now.saturating_add(ttl_ms),
        };
        self.push_locked(
            name,
            state,
            target_subject,
            BrainEventKind::RunnerHandoffCompleted {
                handoff_id,
                lease: lease.clone(),
            },
        )?;
        Ok(lease)
    }

    pub fn cancel_runner_handoff(
        &self,
        name: &str,
        handoff_id: RunnerHandoffId,
        sender: &str,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        let sender = validate_participant_subject("handoff canceller", sender)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        if !state
            .runner_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.handoff_id == handoff_id)
        {
            anyhow::bail!("runner handoff is no longer current");
        }
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::RunnerHandoffCancelled { handoff_id },
        )?;
        Ok(())
    }

    pub fn expire_runner_handoff(
        &self,
        name: &str,
        handoff_id: RunnerHandoffId,
        now_ms: u64,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let Some(current) = state.runner_handoff.as_ref() else {
            return Ok(false);
        };
        if current.handoff_id != handoff_id || current.expires_ms > now_ms {
            return Ok(false);
        }
        self.push_locked(
            name,
            state,
            "daemon",
            BrainEventKind::RunnerHandoffCancelled { handoff_id },
        )?;
        Ok(true)
    }

    pub fn expire_runner_lease(
        &self,
        name: &str,
        lease_id: RunnerLeaseId,
        now_ms: u64,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let Some(current) = state.runner_lease.as_ref() else {
            return Ok(false);
        };
        if current.lease_id != lease_id || current.expires_ms > now_ms {
            return Ok(false);
        }
        self.push_locked(
            name,
            state,
            "daemon",
            BrainEventKind::RunnerLeaseReleased { lease_id },
        )?;
        Ok(true)
    }

    pub fn attach(
        &self,
        name: &str,
        subject: &str,
        role: AttachmentRole,
        attachment_id: Option<AttachmentId>,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
            anyhow::bail!("attachment subject must be 1-128 printable characters");
        }
        let attachment_id = attachment_id.unwrap_or_else(AttachmentId::new);
        let connection_id = ConnectionId::new();
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        if let Some(existing) = state.attachments.get(&attachment_id) {
            if existing.subject != subject || existing.role != role {
                anyhow::bail!("attachment identity cannot change subject or role");
            }
            if existing.connected || existing.connection_id.is_some() {
                anyhow::bail!("Brain attachment already has a live or pending connection");
            }
        }
        let acknowledged_seq = state
            .attachments
            .get(&attachment_id)
            .map(|attachment| attachment.acknowledged_seq)
            .unwrap_or(0);
        let attachment = BrainAttachment {
            attachment_id,
            subject: subject.to_string(),
            role,
            acknowledged_seq,
            connected: false,
            connection_id: Some(connection_id),
        };
        state.attachments.insert(attachment_id, attachment.clone());
        Ok(attachment)
    }

    /// Promote an exact pending REST reservation into the live transport
    /// projection. Only this transition writes `ClientAttached`; abandoned
    /// reservations therefore never look like connected participants in the
    /// canonical event log.
    pub fn activate_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        if attachment.connected {
            anyhow::bail!("Brain attachment transport is already active");
        }
        let subject = attachment.subject.clone();
        let role = attachment.role;
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::ClientAttached {
                attachment_id,
                connection_id,
                subject: subject.clone(),
                role,
            },
        )?;
        state
            .attachments
            .get(&attachment_id)
            .cloned()
            .context("activated client missing from Brain projection")
    }

    /// Clear an abandoned pending connection without advancing the Brain log
    /// or its durable acknowledgement cursor. A timer for an older reservation
    /// cannot affect a later connection because both opaque IDs must match.
    pub fn expire_pending_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let Some(attachment) = state.attachments.get_mut(&attachment_id) else {
            return Ok(false);
        };
        if attachment.connected || attachment.connection_id != Some(connection_id) {
            return Ok(false);
        }
        attachment.connection_id = None;
        Ok(true)
    }

    pub fn detach(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        if !attachment.connected {
            state
                .attachments
                .get_mut(&attachment_id)
                .expect("attachment checked above")
                .connection_id = None;
            return Ok(());
        }
        let subject = attachment.subject.clone();
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::ClientDetached {
                attachment_id,
                connection_id,
            },
        )?;
        Ok(())
    }

    /// Remove a provisional Brain once its last live participant has left.
    ///
    /// Attachment and runner-lease events are transport bookkeeping, not
    /// conversation history. A Brain becomes durable as soon as it contains
    /// a user prompt, submitted program, result, or committed runtime state.
    pub fn remove_if_unused(&self, name: &str) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        {
            // Keep the state lock from the eligibility check through removal.
            // Otherwise a concurrent attach could recreate a live participant
            // between the check and deletion of the provisional directory.
            let mut brains = self.brains.write().expect("shared brain lock poisoned");
            let state = brains.get(name).context("Brain was removed concurrently")?;
            let has_substantive_history = state.events.iter().any(|event| {
                matches!(
                    event.kind,
                    BrainEventKind::Prompt { .. }
                        | BrainEventKind::SpeculativePrompt { .. }
                        | BrainEventKind::ParticipantMessage { .. }
                        | BrainEventKind::TaskListReplaced { .. }
                        | BrainEventKind::ToolCall { .. }
                        | BrainEventKind::ToolResult { .. }
                        | BrainEventKind::ApprovalRequested { .. }
                        | BrainEventKind::ApprovalDecided { .. }
                        | BrainEventKind::Program { .. }
                        | BrainEventKind::ProgramPopped { .. }
                        | BrainEventKind::Result { .. }
                        | BrainEventKind::RuntimeCommitted { .. }
                        | BrainEventKind::RunStarted { .. }
                        | BrainEventKind::RunStatusChanged { .. }
                        | BrainEventKind::ScheduleChanged { .. }
                        | BrainEventKind::ScheduleDue { .. }
                )
            });
            // A pending reservation already represents a live participant.
            // Removing the Brain while another transport is between `attach`
            // and `watch` invalidates that participant's signed connection and
            // lets an unrelated detach race erase the shared session.
            let has_live_attachment = state
                .attachments
                .values()
                .any(|attachment| attachment.connection_id.is_some());
            if has_substantive_history || has_live_attachment || state.runner_lease.is_some() {
                return Ok(false);
            }

            if let Some(runtime) = self
                .runtimes
                .read()
                .expect("shared brain runtime lock poisoned")
                .get(name)
                .cloned()
            {
                runtime.clear_authority_sink()?;
            }
            if let Some(root) = &self.root {
                let directory = root.join(name);
                if directory.exists() {
                    std::fs::remove_dir_all(&directory)
                        .with_context(|| format!("remove unused Brain {}", directory.display()))?;
                }
            }
            brains.remove(name);
        }
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .remove(name);
        self.initializations
            .write()
            .expect("shared Brain initialization lock poisoned")
            .remove(name);
        self.execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned")
            .remove(name);
        Ok(true)
    }

    pub fn require_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let attachment = brains
            .get(name)
            .and_then(|state| state.attachments.get(&attachment_id))
            .context("unknown Brain attachment")?;
        if !attachment.connected || attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        Ok(attachment.clone())
    }

    /// Persist a projection cursor without appending another numbered Brain
    /// event. Otherwise acknowledging the acknowledgement event would create
    /// an unbounded self-sustaining event stream.
    pub fn acknowledge(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        seq: u64,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        let head = state.events.last().map(|event| event.seq).unwrap_or(0);
        if seq > head {
            anyhow::bail!("cannot acknowledge event {seq}; Brain head is {head}");
        }
        let previous = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if !previous.connected || previous.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        let previous = previous.acknowledged_seq;
        if seq < previous {
            anyhow::bail!("attachment cursor cannot move backward from {previous} to {seq}");
        }
        state
            .attachments
            .get_mut(&attachment_id)
            .expect("attachment checked above")
            .acknowledged_seq = seq;
        if let Err(error) = self.write_attachment_cursors(name, state) {
            state
                .attachments
                .get_mut(&attachment_id)
                .expect("attachment checked above")
                .acknowledged_seq = previous;
            return Err(error);
        }
        Ok(state
            .attachments
            .get(&attachment_id)
            .expect("attachment checked above")
            .clone())
    }

    /// Remove a Brain from the active namespace without destroying its log.
    /// Persistent state is moved beside the store into `brains-archive`.
    pub fn archive(&self, name: &str) -> Result<Option<PathBuf>> {
        let name = Self::validate_name(name)?;
        let archived_to = if let Some(root) = &self.root {
            let source = root.join(name);
            if source.exists() {
                let archive_root = root
                    .parent()
                    .map(|parent| parent.join("brains-archive"))
                    .unwrap_or_else(|| root.join("archive"));
                std::fs::create_dir_all(&archive_root)
                    .with_context(|| format!("create {}", archive_root.display()))?;
                let destination = archive_root.join(format!("{name}-{}", unix_millis()));
                std::fs::rename(&source, &destination).with_context(|| {
                    format!("archive {} as {}", source.display(), destination.display())
                })?;
                Some(destination)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(runtime) = self
            .runtimes
            .read()
            .expect("shared brain runtime lock poisoned")
            .get(name)
            .cloned()
        {
            runtime.clear_authority_sink()?;
        }
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .remove(name);
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .remove(name);
        self.initializations
            .write()
            .expect("shared Brain initialization lock poisoned")
            .remove(name);
        self.execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned")
            .remove(name);
        Ok(archived_to)
    }

    pub fn push(&self, name: &str, sender: &str, kind: BrainEventKind) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).context("Brain was removed concurrently")?;
        self.push_locked(name, state, sender, kind)
    }

    fn push_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
    ) -> Result<BrainEvent> {
        if let BrainEventKind::ScheduleChanged { schedule } = &kind {
            if schedule.module_identity.is_some() {
                let initialization = self
                    .initializations
                    .read()
                    .expect("shared Brain initialization lock poisoned")
                    .get(name)
                    .cloned()
                    .context("reviewed Brain initialization contract is not loaded")?;
                initialization.validate_schedule(schedule)?;
            }
        }
        if let BrainEventKind::ScheduleDue { due } = &kind {
            if let Some(schedule) = state.schedules.get(&due.schedule_id) {
                if schedule.module_identity.is_some() {
                    let initialization = self
                        .initializations
                        .read()
                        .expect("shared Brain initialization lock poisoned")
                        .get(name)
                        .cloned()
                        .context("reviewed Brain initialization contract is not loaded")?;
                    initialization.validate_schedule_due(schedule, due)?;
                }
            }
        }
        let event = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: state.events.last().map(|e| e.seq + 1).unwrap_or(1),
            environment_generation: self.environment.generation,
            sender: sender.trim().to_string(),
            created_ms: unix_millis(),
            kind,
        };
        self.append_event(name, &event)?;
        state.apply(event.clone());
        let _ = state.tx.send(event.clone());
        Ok(event)
    }

    pub fn pop_program(&self, name: &str, sender: &str) -> Result<Option<BrainEvent>> {
        let snapshot = self.snapshot(name)?;
        let Some(program) = snapshot.program_stack.last() else {
            return Ok(None);
        };
        self.push(
            name,
            sender,
            BrainEventKind::ProgramPopped {
                program_seq: program.seq,
            },
        )
        .map(Some)
    }

    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<BrainEvent>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        Ok(brains
            .get(name)
            .context("Brain was removed concurrently")?
            .tx
            .subscribe())
    }

    /// Return the one live typed runtime for a named Brain, restoring its
    /// latest reducible checkpoint on first access after daemon restart.
    pub fn program_runtime(&self, name: &str) -> Result<Arc<crate::runtime::ProgramRuntime>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(runtime) = self
            .runtimes
            .read()
            .expect("shared brain runtime lock poisoned")
            .get(name)
            .cloned()
        {
            return Ok(runtime);
        }
        let checkpoint = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.clone());
        let mut runtime = match checkpoint {
            Some(checkpoint) => crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
                self.read_runtime_checkpoint(name, &checkpoint.checkpoint_sha256)?,
                checkpoint.durable_revision,
            )?,
            None => crate::runtime::ProgramRuntime::new(),
        };
        if let Some(authority_store) = self.runtime_authority_store(name) {
            authority_store
                .restore_into(&mut runtime)
                .with_context(|| format!("restore authority for named Brain '{name}'"))?;
            let sink_store = authority_store.clone();
            runtime.set_authority_sink(Arc::new(move |state| sink_store.save_state(state)))?;
        }
        let runtime = Arc::new(runtime);
        let mut runtimes = self
            .runtimes
            .write()
            .expect("shared brain runtime lock poisoned");
        Ok(runtimes
            .entry(name.to_string())
            .or_insert_with(|| Arc::clone(&runtime))
            .clone())
    }

    /// Return the durable reducible state a newly connected environment
    /// runner must install before accepting ProgramRuns. Authority and live
    /// host resources are intentionally stored and rebound separately.
    pub fn runner_checkpoint(
        &self,
        name: &str,
    ) -> Result<(u64, crate::vm::TypedRuntimeCheckpoint)> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let state = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.clone());
        match state {
            Some(state) => Ok((
                state.durable_revision,
                self.read_runtime_checkpoint(name, &state.checkpoint_sha256)?,
            )),
            None => {
                let runtime = crate::runtime::ProgramRuntime::new();
                let snapshot = runtime
                    .revision_history()?
                    .pop()
                    .context("fresh typed runtime has no initial checkpoint")?;
                Ok((
                    snapshot.revision,
                    snapshot
                        .checkpoint
                        .context("fresh typed runtime is not checkpointable")?,
                ))
            }
        }
    }

    /// Journal the latest checkpoint only after a ProgramRuntime commit. The
    /// source event remains the audit record; restart restores state rather
    /// than replaying effects from that source.
    pub fn commit_runtime(
        &self,
        name: &str,
        request_seq: u64,
        runtime_revision: u64,
        runtime: &crate::runtime::ProgramRuntime,
    ) -> Result<BrainEvent> {
        let snapshot = runtime
            .revision_history()?
            .into_iter()
            .find(|snapshot| snapshot.revision == runtime_revision)
            .with_context(|| {
                format!("typed runtime has no revision snapshot {runtime_revision}")
            })?;
        let checkpoint = snapshot.checkpoint.context(
            "typed runtime revision contains host-owned handles and cannot be persisted yet",
        )?;
        let encoded = crate::ipc::checkpoint_codec::encode_checkpoint_bytes(&checkpoint)?;
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        self.write_runtime_checkpoint(name, &checkpoint_sha256, &encoded)?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.clone(), checkpoint);
        if let Some(authority_store) = self.runtime_authority_store(name) {
            authority_store
                .save(runtime)
                .with_context(|| format!("persist authority for named Brain '{name}'"))?;
        }
        self.push(
            name,
            "daemon",
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision: snapshot.revision,
                checkpoint_sha256,
            },
        )
    }

    /// Commit reducible state returned by the frontend that owns this Brain's
    /// environment. The daemon validates and journals the checkpoint but does
    /// not execute the source or inherit the frontend's host authority.
    pub fn commit_runner_runtime(
        &self,
        name: &str,
        request_seq: u64,
        runtime_revision: u64,
        checkpoint: crate::vm::TypedRuntimeCheckpoint,
    ) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(current) = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.as_ref())
        {
            if runtime_revision <= current.durable_revision {
                anyhow::bail!(
                    "runner checkpoint revision {runtime_revision} does not advance durable revision {}",
                    current.durable_revision
                );
            }
        }
        let restored = crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
            checkpoint.clone(),
            runtime_revision,
        )?;
        let encoded = crate::ipc::checkpoint_codec::encode_checkpoint_bytes(&checkpoint)?;
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        self.write_runtime_checkpoint(name, &checkpoint_sha256, &encoded)?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.clone(), checkpoint);
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .insert(name.to_string(), Arc::new(restored));
        self.push(
            name,
            "runner",
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision,
                checkpoint_sha256,
            },
        )
    }

    fn read_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
    ) -> Result<crate::vm::TypedRuntimeCheckpoint> {
        if let Some(checkpoint) = self
            .runtime_checkpoints
            .read()
            .expect("shared brain checkpoint lock poisoned")
            .get(checkpoint_sha256)
            .cloned()
        {
            return Ok(checkpoint);
        }
        let root = self
            .root
            .as_ref()
            .context("named Brain checkpoint is not available in this process")?;
        let directory = root
            .join(name)
            .join("runtime");
        let native_path = directory.join(format!("{checkpoint_sha256}.capnp"));
        let legacy_path = directory.join(format!("{checkpoint_sha256}.json"));
        let (path, native) = if native_path.exists() {
            (native_path, true)
        } else if legacy_path.exists() {
            (legacy_path, false)
        } else {
            anyhow::bail!(
                "named Brain checkpoint {checkpoint_sha256} is missing from {}",
                directory.display()
            );
        };
        let encoded = std::fs::read(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let actual = hex::encode(Sha256::digest(&encoded));
        if actual != checkpoint_sha256 {
            anyhow::bail!("typed runtime checkpoint hash mismatch for {checkpoint_sha256}");
        }
        let checkpoint = if native {
            crate::ipc::checkpoint_codec::decode_checkpoint_bytes(&encoded)
                .with_context(|| format!("parse {}", path.display()))?
        } else {
            serde_json::from_slice(&encoded)
                .with_context(|| format!("parse legacy checkpoint {}", path.display()))?
        };
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.to_string(), checkpoint.clone());
        Ok(checkpoint)
    }

    fn write_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
        encoded: &[u8],
    ) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let directory = root.join(name).join("runtime");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(format!("{checkpoint_sha256}.capnp"));
        if path.exists() {
            return Ok(());
        }
        let temporary = directory.join(format!(
            ".{checkpoint_sha256}.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    /// Return the host-policy record associated with this named Brain. This
    /// path is deliberately neither content-addressed nor part of a VM
    /// checkpoint: restoring executable state alone must never restore
    /// authority.
    fn runtime_authority_store(
        &self,
        name: &str,
    ) -> Option<crate::runtime::archive_store::ProgramRuntimeAuthorityStore> {
        self.root.as_ref().map(|root| {
            crate::runtime::archive_store::ProgramRuntimeAuthorityStore::new(
                root.join(name).join("authority.json"),
            )
        })
    }

    fn ensure_loaded(&self, name: &str) -> Result<()> {
        if self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .contains_key(name)
        {
            return Ok(());
        }
        let brain_id = self.load_or_create_metadata(name)?.brain_id;
        let initialization = self.load_or_create_initialization(name, brain_id)?;
        let events = self.read_events(name)?;
        let mut reviewed_schedules = HashMap::new();
        for event in &events {
            if event.schema_version > BRAIN_EVENT_SCHEMA_VERSION {
                anyhow::bail!(
                    "Brain '{name}' contains unsupported event schema version {}",
                    event.schema_version
                );
            }
            if event.brain_id != BrainId::nil() && event.brain_id != brain_id {
                anyhow::bail!(
                    "Brain '{name}' event #{} belongs to a different Brain identity",
                    event.seq
                );
            }
            if let BrainEventKind::ScheduleChanged { schedule } = &event.kind {
                if schedule.module_identity.is_some() {
                    initialization.validate_schedule(schedule).with_context(|| {
                        format!(
                            "Brain '{name}' event #{} contains an invalid reviewed-module schedule",
                            event.seq
                        )
                    })?;
                    reviewed_schedules.insert(schedule.schedule_id, schedule.clone());
                }
            }
        }
        for event in &events {
            if let BrainEventKind::ScheduleDue { due } = &event.kind {
                if let Some(schedule) = reviewed_schedules.get(&due.schedule_id) {
                    initialization.validate_schedule_due(schedule, due).with_context(|| {
                        format!(
                            "Brain '{name}' event #{} contains an invalid reviewed-module delivery",
                            event.seq
                        )
                    })?;
                }
            }
        }
        // Preserve an empty named Brain across daemon restarts, even before
        // its first conversational event is appended.
        if let Some(root) = &self.root {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
        }
        let cursors = self.read_attachment_cursors(name, brain_id)?;
        let mut state = BrainState::from_events(brain_id, events);
        for attachment in state.attachments.values_mut() {
            // A process restart disconnects every transport projection;
            // reconnect appends a fresh ClientAttached event.
            attachment.connected = false;
            attachment.connection_id = None;
        }
        for (attachment_id, acknowledged_seq) in cursors {
            if let Some(attachment) = state.attachments.get_mut(&attachment_id) {
                attachment.acknowledged_seq = acknowledged_seq.min(
                    state.events.last().map(|event| event.seq).unwrap_or(0),
                );
            }
        }
        // Queued work has not executed and may be safely offered to the next
        // valid runner lease. A run that was already executing or suspended
        // for approval has unknown external progress after daemon restart and
        // must never be replayed implicitly.
        let orphaned_runs = state
            .runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
                )
            })
            .map(|run| run.run_id)
            .collect::<Vec<_>>();
        for run_id in orphaned_runs {
            let event = BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: state.brain_id,
                seq: state.events.last().map(|event| event.seq + 1).unwrap_or(1),
                environment_generation: self.environment.generation,
                sender: "daemon".into(),
                created_ms: unix_millis(),
                kind: BrainEventKind::RunStatusChanged {
                    run_id,
                    status: BrainRunStatus::Interrupted,
                    detail: Some("daemon restarted before the run reached a terminal state".into()),
                },
            };
            self.append_event(name, &event)?;
            state.apply(event);
        }
        self.initializations
            .write()
            .expect("shared Brain initialization lock poisoned")
            .entry(name.to_string())
            .or_insert(initialization);
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .entry(name.to_string())
            .or_insert(state);
        Ok(())
    }

    fn load_or_create_initialization(
        &self,
        name: &str,
        brain_id: BrainId,
    ) -> Result<BrainInitialization> {
        let Some(root) = &self.root else {
            return Ok(BrainInitialization::reviewed_default(brain_id));
        };
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("initialization.json");
        if path.exists() {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let initialization: BrainInitialization = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            initialization.validate(brain_id)?;
            return Ok(initialization);
        }
        let initialization = BrainInitialization::reviewed_default(brain_id);
        let temporary = directory.join(format!(".initialization.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&initialization)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                Ok(initialization)
            }
            Err(_error) if path.exists() => {
                let _ = std::fs::remove_file(&temporary);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read {} after initialization race", path.display()))?;
                let initialization: BrainInitialization = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {} after initialization race", path.display()))?;
                initialization.validate(brain_id)?;
                Ok(initialization)
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(error).with_context(|| format!("commit {}", path.display()))
            }
        }
    }

    fn read_attachment_cursors(
        &self,
        name: &str,
        brain_id: BrainId,
    ) -> Result<HashMap<AttachmentId, u64>> {
        let Some(root) = &self.root else {
            return Ok(HashMap::new());
        };
        let path = root.join(name).join("attachments.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(HashMap::new());
        };
        let file: AttachmentCursorFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", path.display()))?;
        if file.version != 1 || file.brain_id != brain_id {
            anyhow::bail!("attachment cursor identity mismatch at {}", path.display());
        }
        Ok(file.cursors)
    }

    fn write_attachment_cursors(&self, name: &str, state: &BrainState) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let file = AttachmentCursorFile {
            version: 1,
            brain_id: state.brain_id,
            cursors: state
                .attachments
                .iter()
                .map(|(id, attachment)| (*id, attachment.acknowledged_seq))
                .collect(),
        };
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("attachments.json");
        let temporary = directory.join(format!(".attachments.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&file)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    fn load_or_create_metadata(&self, name: &str) -> Result<BrainMetadata> {
        let Some(root) = &self.root else {
            return Ok(BrainMetadata {
                version: BRAIN_METADATA_VERSION,
                brain_id: BrainId::new(),
                created_ms: unix_millis(),
            });
        };
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("metadata.json");
        if path.exists() {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let metadata: BrainMetadata = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if metadata.version != BRAIN_METADATA_VERSION || metadata.brain_id == BrainId::nil() {
                anyhow::bail!("unsupported or invalid Brain metadata at {}", path.display());
            }
            return Ok(metadata);
        }
        let metadata = BrainMetadata {
            version: BRAIN_METADATA_VERSION,
            brain_id: BrainId::new(),
            created_ms: unix_millis(),
        };
        let encoded = serde_json::to_vec_pretty(&metadata)?;
        let temporary = directory.join(format!(".metadata.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        // A rename can replace a winner on Unix. Linking the fully written
        // temporary file creates the final name only if it is still absent,
        // so concurrent daemon starts cannot split one alias into two IDs.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                Ok(metadata)
            }
            Err(_error) if path.exists() => {
                let _ = std::fs::remove_file(&temporary);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read {} after metadata race", path.display()))?;
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {} after metadata race", path.display()))
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(error).with_context(|| format!("commit {}", path.display()))
            }
        }
    }

    fn load_all(&self) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    if Self::validate_name(name).is_ok() {
                        self.ensure_loaded(name)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn event_path(&self, name: &str) -> Option<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(name).join("events.jsonl"))
    }

    fn read_events(&self, name: &str) -> Result<Vec<BrainEvent>> {
        let Some(path) = self.event_path(name) else {
            return Ok(Vec::new());
        };
        let Ok(file) = std::fs::File::open(&path) else {
            return Ok(Vec::new());
        };
        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_no, line)| match line {
                Ok(line) if line.trim().is_empty() => None,
                other => Some((line_no, other)),
            })
            .map(|(line_no, line)| {
                let line = line.with_context(|| format!("read {}", path.display()))?;
                serde_json::from_str(&line)
                    .with_context(|| format!("parse {} line {}", path.display(), line_no + 1))
            })
            .collect()
    }

    fn append_event(&self, name: &str, event: &BrainEvent) -> Result<()> {
        let Some(path) = self.event_path(name) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

fn validate_participant_subject<'a>(label: &str, subject: &'a str) -> Result<&'a str> {
    let subject = subject.trim();
    if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
        anyhow::bail!("{label} must be 1-128 printable characters");
    }
    Ok(subject)
}

pub(crate) fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sorted_attachments(
    attachments: &HashMap<AttachmentId, BrainAttachment>,
) -> Vec<BrainAttachment> {
    let mut attachments = attachments.values().cloned().collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.attachment_id.0);
    attachments
}

fn sorted_runs(runs: &HashMap<RunId, BrainRun>) -> Vec<BrainRun> {
    let mut runs = runs.values().cloned().collect::<Vec<_>>();
    runs.sort_by_key(|run| (run.request_seq, run.run_id.0));
    runs
}

fn queued_schedule_run(
    state: &BrainState,
    schedule: &BrainSchedule,
    now_ms: u64,
) -> BrainRun {
    BrainRun {
        run_id: RunId::new(),
        kind: BrainRunKind::Scheduled,
        parent_run_id: None,
        request_seq: state.events.last().map(|event| event.seq + 1).unwrap_or(1),
        initiating_attachment_id: schedule.initiating_attachment_id,
        initiated_by: schedule.created_by.clone(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: now_ms,
        updated_ms: now_ms,
        detail: None,
    }
}

fn schedule_due_window(
    schedule: &BrainSchedule,
    now_ms: u64,
) -> Result<(u32, u64, Option<u64>)> {
    let Some(interval_ms) = schedule.interval_ms else {
        return Ok((1, schedule.next_due_ms, None));
    };
    let elapsed = now_ms.saturating_sub(schedule.next_due_ms);
    let count = elapsed / interval_ms + 1;
    let last_due_ms = schedule
        .next_due_ms
        .checked_add((count - 1).checked_mul(interval_ms).context("schedule overflow")?)
        .context("schedule overflow")?;
    let next_due_ms = last_due_ms
        .checked_add(interval_ms)
        .context("schedule overflow")?;
    Ok((
        u32::try_from(count).unwrap_or(u32::MAX),
        last_due_ms,
        Some(next_due_ms),
    ))
}

fn sorted_schedules(schedules: &HashMap<ScheduleId, BrainSchedule>) -> Vec<BrainSchedule> {
    let mut schedules = schedules.values().cloned().collect::<Vec<_>>();
    schedules.sort_by_key(|schedule| (schedule.next_due_ms, schedule.schedule_id.0));
    schedules
}

fn sorted_schedule_dues(
    dues: &HashMap<RunId, BrainScheduleDue>,
) -> Vec<BrainScheduleDue> {
    let mut dues = dues.values().cloned().collect::<Vec<_>>();
    dues.sort_by_key(|due| (due.due_at_ms, due.schedule_id.0, due.run.run_id.0));
    dues
}

fn validate_run_transition(from: BrainRunStatus, to: BrainRunStatus) -> Result<()> {
    use BrainRunStatus::*;
    let allowed = match from {
        QueuedForEnvironment => matches!(to, Running | Cancelled | Failed),
        Running => matches!(
            to,
            AwaitingApproval | Completed | Failed | Cancelled | Interrupted
        ),
        AwaitingApproval => matches!(to, Running | Failed | Cancelled | Interrupted),
        Interrupted => matches!(to, Running | Failed | Cancelled),
        Completed | Failed | Cancelled => false,
    };
    if !allowed {
        anyhow::bail!("invalid Brain run transition from {from:?} to {to:?}");
    }
    if from.is_terminal() {
        anyhow::bail!("terminal Brain run cannot transition");
    }
    Ok(())
}

const fn initial_environment_generation() -> u64 {
    1
}

const fn legacy_brain_event_schema_version() -> u32 {
    1
}

fn legacy_schedule_attachment_id() -> AttachmentId {
    AttachmentId(uuid::Uuid::nil())
}

const fn legacy_schedule_language() -> ProgramLanguage {
    ProgramLanguage::Forth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_due_is_one_durable_coalesced_run_until_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Lisp,
                "(say \"tick\")",
                crate::vm::EffectSet::pure(),
                1_000,
                Some(1_000),
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let queued = store.queue_due_schedules("shared", 3_500).unwrap();
        assert_eq!(queued.len(), 1);
        let run = queued[0].clone();
        let first = store.snapshot("shared").unwrap();
        let first_due = first.pending_schedule_dues[0].clone();
        assert_eq!(first_due.missed_count, 3);
        assert_eq!(first_due.due_at_ms, 3_000);
        assert_eq!(first_due.next_due_ms, Some(4_000));
        assert_eq!(run.request_seq, first.revision);
        assert!(store.queue_due_schedules("shared", 3_500).unwrap().is_empty());

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.snapshot("shared").unwrap();
        assert_eq!(restored.schedules[0].next_due_ms, 4_000);
        assert_eq!(restored.pending_schedule_dues, vec![first_due]);
        assert_eq!(restored.runs, vec![run.clone()]);

        let requeued = restarted.queue_due_schedules("shared", 5_500).unwrap();
        assert_eq!(requeued.len(), 1);
        assert_eq!(requeued[0].run_id, run.run_id);
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(snapshot.pending_schedule_dues[0].missed_count, 5);
        assert_eq!(snapshot.pending_schedule_dues[0].next_due_ms, Some(6_000));
        assert_eq!(snapshot.runs.len(), 1, "coalescing must reuse the queued run");

        restarted
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        restarted
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert!(restarted
            .snapshot("shared")
            .unwrap()
            .pending_schedule_dues
            .is_empty());
    }

    #[test]
    fn bounded_catch_up_limits_pending_runs_and_skips_expired_ticks() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Forth,
                "\"tick\" say",
                crate::vm::EffectSet::pure(),
                1_000,
                Some(1_000),
                BrainScheduleDeliveryPolicy::BoundedCatchUp {
                    max_catch_up: 2,
                    expires_after_ms: 2_500,
                },
            )
            .unwrap();

        let queued = store.queue_due_schedules("shared", 5_000).unwrap();
        assert_eq!(queued.len(), 2);
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .pending_schedule_dues
                .iter()
                .map(|due| due.due_at_ms)
                .collect::<Vec<_>>(),
            vec![3_000, 4_000]
        );
        assert_eq!(snapshot.schedules[0].next_due_ms, 5_000);
        assert!(store.queue_due_schedules("shared", 5_000).unwrap().is_empty());

        store
            .transition_run(
                "shared",
                "runner",
                queued[0].run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "runner",
                queued[0].run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        let next = store.queue_due_schedules("shared", 5_000).unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(
            store.snapshot("shared").unwrap().pending_schedule_dues.len(),
            2
        );
    }

    #[test]
    fn schedule_inspection_and_cancellation_are_durable_and_creator_bound() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let sibling = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let schedule = store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Lisp,
                "(say \"later\")",
                crate::vm::EffectSet::pure(),
                10_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert_eq!(
            store.inspect_schedule("shared", schedule.schedule_id).unwrap(),
            Some(schedule.clone())
        );
        assert!(store
            .cancel_schedule(
                "shared",
                "bob",
                attachment.attachment_id,
                schedule.schedule_id,
            )
            .unwrap_err()
            .to_string()
            .contains("only the schedule creator attachment"));
        assert!(store
            .cancel_schedule(
                "shared",
                "alice",
                sibling.attachment_id,
                schedule.schedule_id,
            )
            .unwrap_err()
            .to_string()
            .contains("only the schedule creator attachment"));
        assert!(store
            .cancel_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                schedule.schedule_id,
            )
            .unwrap());
        assert!(!store
            .cancel_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                schedule.schedule_id,
            )
            .unwrap());
        assert!(store
            .queue_due_schedules("shared", 20_000)
            .unwrap()
            .is_empty());

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(!restarted
            .inspect_schedule("shared", schedule.schedule_id)
            .unwrap()
            .unwrap()
            .active);
    }

    #[test]
    fn run_lifecycle_is_event_sourced_and_terminal_state_is_final() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "inspect".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert!(store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .is_err());

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = &restarted.snapshot("shared").unwrap().runs[0];
        assert_eq!(restored.run_id, run.run_id);
        assert_eq!(restored.request_seq, prompt.seq);
        assert_eq!(restored.status, BrainRunStatus::Completed);
        assert!(restored.updated_ms >= restored.started_ms);
    }

    #[test]
    fn run_ancestry_is_validated_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let parent_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "parent".into(),
                },
            )
            .unwrap();
        let parent = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                parent_request.seq,
                attachment.attachment_id,
                BrainRunStatus::Running,
            )
            .unwrap();
        let child_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "child".into(),
                },
            )
            .unwrap();
        let child = store
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                child_request.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
                Some(parent.run_id),
            )
            .unwrap();
        assert_eq!(
            store.inspect_run("shared", child.run_id).unwrap().parent_run_id,
            Some(parent.run_id)
        );
        assert!(store
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                child_request.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
                Some(RunId::new()),
            )
            .is_err());

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted
                .inspect_run("shared", child.run_id)
                .unwrap()
                .parent_run_id,
            Some(parent.run_id)
        );
        restarted
            .transition_run(
                "shared",
                "runner",
                parent.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        restarted
            .transition_run(
                "shared",
                "runner",
                parent.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert!(restarted
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                child_request.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
                Some(parent.run_id),
            )
            .is_err());
    }

    #[test]
    fn restart_interrupts_started_runs_without_replaying_queued_runs() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let running_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "started".into(),
                },
            )
            .unwrap();
        let running = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                running_request.seq,
                attachment.attachment_id,
                BrainRunStatus::Running,
            )
            .unwrap();
        let queued_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "queued".into(),
                },
            )
            .unwrap();
        let queued = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                queued_request.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .runs
                .iter()
                .find(|candidate| candidate.run_id == running.run_id)
                .unwrap()
                .status,
            BrainRunStatus::Interrupted
        );
        assert_eq!(
            snapshot
                .runs
                .iter()
                .find(|candidate| candidate.run_id == queued.run_id)
                .unwrap()
                .status,
            BrainRunStatus::QueuedForEnvironment
        );
        assert!(snapshot.events.iter().any(|event| {
            matches!(
                event.kind,
                BrainEventKind::RunStatusChanged {
                    run_id,
                    status: BrainRunStatus::Interrupted,
                    ..
                } if run_id == running.run_id
            )
        }));
    }

    #[test]
    fn program_stack_is_rebuilt_from_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("workstation.local", Some(temp.path().into()));
        let first = store
            .push(
                "finch",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Forth,
                    source: "2 3 +".into(),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "bob",
                BrainEventKind::Prompt {
                    text: "explain that".into(),
                },
            )
            .unwrap();

        let restarted = BrainStore::with_root("workstation.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("finch").unwrap();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.program_stack.len(), 1);
        assert_eq!(snapshot.program_stack[0].seq, first.seq);
        assert_eq!(snapshot.environment.machine, "workstation.local");
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn tool_and_approval_lifecycle_is_rebuilt_from_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("workstation.local", Some(temp.path().into()));
        let brain_id = store.snapshot("finch").unwrap().brain_id;
        let audience = BrainApprovalAudience {
            brain_id,
            brain: "finch".into(),
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: "alice".into(),
            role: AttachmentRole::Driver,
            environment_generation: 1,
        };
        store
            .push(
                "finch",
                "provider",
                BrainEventKind::ToolCall {
                    request_seq: 1,
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "runner",
                BrainEventKind::ApprovalRequested {
                    request_seq: 1,
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: Some(audience.clone()),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "alice",
                BrainEventKind::ApprovalDecided {
                    request_seq: 1,
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "runner",
                BrainEventKind::ToolResult {
                    request_seq: 1,
                    tool_id: "tool-1".into(),
                    output: "found".into(),
                    is_error: false,
                },
            )
            .unwrap();

        let restarted = BrainStore::with_root("workstation.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("finch").unwrap();
        assert!(matches!(
            &snapshot.events[0].kind,
            BrainEventKind::ToolCall { tool_id, input, .. }
                if tool_id == "tool-1" && input["query"] == "fib"
        ));
        assert!(matches!(
            &snapshot.events[1].kind,
            BrainEventKind::ApprovalRequested {
                approval_id,
                subject,
                audience: Some(event_audience),
                ..
            }
                if approval_id == "tool-1"
                    && subject == "search_word"
                    && event_audience == &audience
        ));
        assert!(matches!(
            &snapshot.events[2].kind,
            BrainEventKind::ApprovalDecided { approval_id, decision, .. }
                if approval_id == "tool-1" && decision["choice"] == "approve_once"
        ));
        assert!(matches!(
            &snapshot.events[3].kind,
            BrainEventKind::ToolResult { tool_id, output, is_error: false, .. }
                if tool_id == "tool-1" && output == "found"
        ));
    }

    #[test]
    fn legacy_approval_event_without_audience_still_deserializes() {
        let event: BrainEvent = serde_json::from_value(serde_json::json!({
            "schema_version": 6,
            "brain_id": uuid::Uuid::nil(),
            "seq": 1,
            "environment_generation": 1,
            "sender": "runner",
            "created_ms": 0,
            "kind": "approval_requested",
            "request_seq": 1,
            "approval_id": "approval-1",
            "approval_kind": "tool",
            "subject": "search_word",
            "detail": {}
        }))
        .unwrap();
        assert!(matches!(
            event.kind,
            BrainEventKind::ApprovalRequested { audience: None, .. }
        ));
    }

    #[test]
    fn pop_is_an_event_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        store
            .push(
                "brain",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "(+ 1 2)".into(),
                },
            )
            .unwrap();
        let popped = store.pop_program("brain", "alice").unwrap().unwrap();
        assert!(matches!(popped.kind, BrainEventKind::ProgramPopped { .. }));

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted
            .snapshot("brain")
            .unwrap()
            .program_stack
            .is_empty());
    }

    #[test]
    fn subscribers_receive_the_authoritative_sequence() {
        let store = BrainStore::with_root("box.local", None);
        let mut first = store.subscribe("brain").unwrap();
        let mut second = store.subscribe("brain").unwrap();
        let event = store
            .push(
                "brain",
                "alice",
                BrainEventKind::Prompt { text: "hi".into() },
            )
            .unwrap();
        assert_eq!(first.try_recv().unwrap(), event);
        assert_eq!(second.try_recv().unwrap(), event);
    }

    #[test]
    fn empty_named_brain_remains_listed_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let original = store.snapshot("quiet-brain").unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.list().unwrap(), vec!["quiet-brain"]);
        let restored = restarted.snapshot("quiet-brain").unwrap();
        assert_eq!(restored.revision, 0);
        assert_eq!(restored.brain_id, original.brain_id);
        assert_ne!(restored.brain_id, BrainId::nil());
        assert!(temp.path().join("quiet-brain/metadata.json").exists());
    }

    #[test]
    fn initialization_contract_is_persisted_and_inert_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let before = store.initialization("quiet-brain").unwrap();
        let snapshot = store.snapshot("quiet-brain").unwrap();
        assert_eq!(before.brain_id, snapshot.brain_id);
        assert_eq!(before.capability_budget, crate::vm::EffectSet::pure());
        assert_eq!(snapshot.revision, 0);
        assert!(snapshot.runs.is_empty() && snapshot.schedules.is_empty());
        assert!(snapshot.events.iter().all(|event| !matches!(
            event.kind, BrainEventKind::EffectRecorded { .. }
        )));

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.initialization("quiet-brain").unwrap(), before);
        assert_eq!(restarted.snapshot("quiet-brain").unwrap().revision, 0);
        assert!(temp.path().join("quiet-brain/initialization.json").exists());
    }

    #[test]
    fn initialization_requires_an_explicit_scheduled_brain_run() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let contract = store.initialization("shared").unwrap();
        let schedule = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        assert_eq!(schedule.source, contract.source);
        assert_eq!(schedule.grant_ceiling, contract.capability_budget);
        assert!(store.queue_due_schedules("shared", 9).unwrap().is_empty());
        let runs = store.queue_due_schedules("shared", 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].kind, BrainRunKind::Scheduled);
        assert_eq!(runs[0].status, BrainRunStatus::QueuedForEnvironment);

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let reattached = restarted.attach(
            "shared",
            "alice",
            AttachmentRole::Driver,
            Some(attachment.attachment_id),
        ).unwrap();
        restarted.activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        ).unwrap();
        let same = restarted
            .schedule_initialization("shared", reattached.attachment_id, reattached.connection_id.unwrap(), 20)
            .unwrap();
        assert_eq!(same.schedule_id, schedule.schedule_id);
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(snapshot.schedules.len(), 1);
        assert_eq!(snapshot.runs.len(), 1);
        assert!(snapshot.events.iter().all(|event| !matches!(
            event.kind, BrainEventKind::EffectRecorded { .. }
        )));
    }

    #[test]
    fn stale_connection_cannot_win_initialization_scheduling_race() {
        let store = BrainStore::with_root("box.local", None);
        let first = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let stale_connection = first.connection_id.unwrap();
        store
            .activate_connection("shared", first.attachment_id, stale_connection)
            .unwrap();
        store
            .detach("shared", first.attachment_id, stale_connection)
            .unwrap();
        let current = store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .unwrap();
        let current_connection = current.connection_id.unwrap();
        store
            .activate_connection("shared", current.attachment_id, current_connection)
            .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let stale_store = store.clone();
        let stale_barrier = barrier.clone();
        let attachment_id = current.attachment_id;
        let stale = std::thread::spawn(move || {
            stale_barrier.wait();
            stale_store.schedule_initialization(
                "shared",
                attachment_id,
                stale_connection,
                10,
            )
        });
        let current_store = store.clone();
        let current_barrier = barrier.clone();
        let accepted = std::thread::spawn(move || {
            current_barrier.wait();
            current_store.schedule_initialization(
                "shared",
                attachment_id,
                current_connection,
                10,
            )
        });
        barrier.wait();

        assert!(stale.join().unwrap().is_err());
        assert!(accepted.join().unwrap().is_ok());
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.schedules.len(), 1);
        assert_eq!(snapshot.schedules[0].created_by, "alice");
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ScheduleChanged { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn public_source_collision_cannot_claim_initialization_identity() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let contract = store.initialization("shared").unwrap();
        let active_ordinary = store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                contract.language,
                contract.source.clone(),
                contract.capability_budget.clone(),
                50,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        let cancelled_ordinary = store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                contract.language,
                contract.source.clone(),
                contract.capability_budget.clone(),
                60,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert!(store
            .cancel_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                cancelled_ordinary.schedule_id,
            )
            .unwrap());
        let delivered_ordinary = store
            .create_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                contract.language,
                contract.source.clone(),
                contract.capability_budget.clone(),
                5,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert_eq!(store.queue_due_schedules("shared", 5).unwrap().len(), 1);
        assert!(!store
            .inspect_schedule("shared", delivered_ordinary.schedule_id)
            .unwrap()
            .unwrap()
            .active);
        assert_eq!(active_ordinary.module_identity, None);
        assert_eq!(cancelled_ordinary.module_identity, None);
        assert_eq!(delivered_ordinary.module_identity, None);

        let initialization = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        assert_ne!(initialization.schedule_id, active_ordinary.schedule_id);
        assert_ne!(initialization.schedule_id, cancelled_ordinary.schedule_id);
        assert_ne!(initialization.schedule_id, delivered_ordinary.schedule_id);
        assert_eq!(
            initialization.module_identity,
            Some(contract.module_identity())
        );
        assert_eq!(store.snapshot("shared").unwrap().schedules.len(), 4);
    }

    #[test]
    fn cancelled_or_failed_initialization_attempt_is_retried() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let cancelled = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        assert!(store
            .cancel_schedule(
                "shared",
                "alice",
                attachment.attachment_id,
                cancelled.schedule_id,
            )
            .unwrap());
        let retry = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 20)
            .unwrap();
        assert_ne!(retry.schedule_id, cancelled.schedule_id);

        let run = store.queue_due_schedules("shared", 20).unwrap().remove(0);
        store
            .transition_run(
                "shared",
                "daemon:runner",
                run.run_id,
                BrainRunStatus::Failed,
                Some("retryable initialization failure".into()),
            )
            .unwrap();
        let second_retry = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 30)
            .unwrap();
        assert_ne!(second_retry.schedule_id, retry.schedule_id);
        assert!(second_retry.active);
    }

    #[test]
    fn interrupted_initialization_attempt_is_retried_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let first = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        let run = store.queue_due_schedules("shared", 10).unwrap().remove(0);
        store
            .transition_run(
                "shared",
                "daemon:runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let reattached = restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(attachment.attachment_id),
            )
            .unwrap();
        restarted.activate_connection(
            "shared",
            reattached.attachment_id,
            reattached.connection_id.unwrap(),
        ).unwrap();
        assert_eq!(
            restarted.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Interrupted
        );
        let retry = restarted
            .schedule_initialization("shared", reattached.attachment_id, reattached.connection_id.unwrap(), 20)
            .unwrap();
        assert_ne!(retry.schedule_id, first.schedule_id);
        assert!(retry.active);
    }

    #[test]
    fn scheduled_initialization_prevents_provisional_brain_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10_000)
            .unwrap();
        store
            .detach(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();

        assert!(!store.remove_if_unused("shared").unwrap());
        assert_eq!(store.snapshot("shared").unwrap().schedules.len(), 1);
        assert!(temp.path().join("shared/initialization.json").exists());
    }

    #[test]
    fn tagged_initialization_schedule_must_match_reviewed_payload() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let mut schedule = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        schedule.source = "(define (unreviewed-startup) : int 2)".into();

        let error = store
            .push(
                "shared",
                "daemon",
                BrainEventKind::ScheduleChanged { schedule },
            )
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("schedule digest does not match its source"));
    }

    #[test]
    fn invalid_tagged_initialization_schedule_is_rejected_during_replay() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        drop(store);

        let path = temp.path().join("shared/events.jsonl");
        let encoded = std::fs::read_to_string(&path).unwrap();
        let tampered = encoded.replace(
            DEFAULT_INITIALIZATION_SOURCE,
            "(define (unreviewed-startup) : int 2)",
        );
        assert_ne!(tampered, encoded);
        std::fs::write(&path, tampered).unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let error = restarted.snapshot("shared").unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid reviewed-module schedule"));
    }

    #[test]
    fn invalid_initialization_delivery_is_rejected_during_replay() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        store.queue_due_schedules("shared", 10).unwrap();
        drop(store);

        let path = temp.path().join("shared/events.jsonl");
        let encoded = std::fs::read_to_string(&path).unwrap();
        let mut changed = false;
        let tampered = encoded
            .lines()
            .map(|line| {
                let mut event: serde_json::Value = serde_json::from_str(line).unwrap();
                if event["kind"] == "schedule_due" {
                    event["due"]["source"] = serde_json::Value::String(
                        "(define (unreviewed-delivery) : int 2)".into(),
                    );
                    changed = true;
                }
                serde_json::to_string(&event).unwrap()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(changed);
        std::fs::write(&path, format!("{tampered}\n")).unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let error = restarted.snapshot("shared").unwrap_err();
        assert!(error
            .to_string()
            .contains("invalid reviewed-module delivery"));
    }

    #[test]
    fn delivered_or_completed_initialization_is_idempotent_across_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store.activate_connection(
            "shared",
            attachment.attachment_id,
            attachment.connection_id.unwrap(),
        ).unwrap();
        let schedule = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 10)
            .unwrap();
        let run = store.queue_due_schedules("shared", 10).unwrap().remove(0);
        let delivered = store
            .schedule_initialization("shared", attachment.attachment_id, attachment.connection_id.unwrap(), 20)
            .unwrap();
        assert_eq!(delivered.schedule_id, schedule.schedule_id);
        store
            .transition_run(
                "shared",
                "daemon:runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "daemon:runner",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let reattached = restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(attachment.attachment_id),
            )
            .unwrap();
        restarted
            .activate_connection(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
            )
            .unwrap();
        let completed = restarted
            .schedule_initialization("shared", reattached.attachment_id, reattached.connection_id.unwrap(), 30)
            .unwrap();
        assert_eq!(completed.schedule_id, schedule.schedule_id);
        assert_eq!(restarted.snapshot("shared").unwrap().schedules.len(), 1);
    }

    #[tokio::test]
    async fn reviewed_initialization_module_is_typed_and_pure() {
        let store = BrainStore::with_root("box.local", None);
        let contract = store.initialization("reviewed").unwrap();
        let runtime = crate::runtime::ProgramRuntime::new();
        let outcome = runtime.submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Lisp,
            source_id: Some(format!("brain-initialization:{}", contract.source_sha256)),
            source: contract.source,
            intent: "reviewed Brain initialization module".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(runtime.revision()),
            budget: None,
        }).await.unwrap();
        assert_eq!(outcome.status, crate::runtime::outcome::ExecutionStatus::Completed);
        assert!(outcome.inferred_capabilities.is_empty());
        assert!(outcome.vm_side_effects.is_empty());
    }

    #[test]
    fn unreviewed_persisted_initialization_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let mut contract = store.initialization("tampered").unwrap();
        contract.source = "(define (ambient-startup) : int 2)".into();
        contract.source_sha256 = hex::encode(Sha256::digest(contract.source.as_bytes()));
        std::fs::write(
            temp.path().join("tampered/initialization.json"),
            serde_json::to_vec_pretty(&contract).unwrap(),
        ).unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted.initialization("tampered").unwrap_err()
            .to_string().contains("not the reviewed built-in module"));
    }

    #[test]
    fn unused_brain_is_removed_after_its_last_participant_leaves() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let attachment = store
            .attach("provisional", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let lease = store
            .acquire_runner_lease("provisional", "alice", generation, None, 60_000)
            .unwrap();

        store
            .detach(
                "provisional",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        assert!(!store.remove_if_unused("provisional").unwrap());
        store
            .release_runner_lease("provisional", lease.lease_id)
            .unwrap();
        assert!(store.remove_if_unused("provisional").unwrap());
        assert!(!temp.path().join("provisional").exists());
        assert!(!store.list().unwrap().contains(&"provisional".to_string()));
    }

    #[test]
    fn pending_attachment_prevents_another_participant_from_removing_brain() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let first = store
            .attach("pending", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let second = store
            .attach("pending", "bob", AttachmentRole::Driver, None)
            .unwrap();

        store
            .detach(
                "pending",
                first.attachment_id,
                first.connection_id.unwrap(),
            )
            .unwrap();
        assert!(!store.remove_if_unused("pending").unwrap());
        assert_eq!(
            store
                .snapshot("pending")
                .unwrap()
                .attachments
                .into_iter()
                .find(|attachment| attachment.attachment_id == second.attachment_id)
                .unwrap()
                .connection_id,
            second.connection_id
        );

        store
            .detach(
                "pending",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap();
        assert!(store.remove_if_unused("pending").unwrap());
    }

    #[test]
    fn substantive_brain_survives_after_every_participant_leaves() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("durable", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .push(
                "durable",
                "alice",
                BrainEventKind::Prompt {
                    text: "remember this".into(),
                },
            )
            .unwrap();
        store
            .detach(
                "durable",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();

        assert!(!store.remove_if_unused("durable").unwrap());
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted.snapshot("durable").unwrap().program_stack.len(),
            0
        );
        assert!(restarted
            .snapshot("durable")
            .unwrap()
            .events
            .iter()
            .any(|event| {
                matches!(&event.kind, BrainEventKind::Prompt { text } if text == "remember this")
            }));
    }

    #[test]
    fn task_list_projection_survives_daemon_restart() {
        use crate::brain::tasks::{BrainTask, BrainTaskPriority, BrainTaskStatus};

        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let tasks = vec![BrainTask {
            id: "compile".into(),
            content: "Compile the candidate frontend".into(),
            status: BrainTaskStatus::InProgress,
            priority: BrainTaskPriority::High,
        }];
        store
            .push(
                "durable-tasks",
                "provider",
                BrainEventKind::TaskListReplaced {
                    tasks: tasks.clone(),
                },
            )
            .unwrap();
        assert_eq!(store.snapshot("durable-tasks").unwrap().tasks, tasks);
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("durable-tasks").unwrap();
        assert_eq!(snapshot.tasks, tasks);
        assert!(matches!(
            &snapshot.events.last().unwrap().kind,
            BrainEventKind::TaskListReplaced { tasks: restored } if restored == &tasks
        ));
    }

    #[test]
    fn legacy_events_are_projected_into_the_persisted_brain_identity() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("legacy");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("events.jsonl"),
            r#"{"seq":1,"environment_generation":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hello"}
"#,
        )
        .unwrap();

        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = store.snapshot("legacy").unwrap();
        assert_ne!(snapshot.brain_id, BrainId::nil());
        assert_eq!(snapshot.events[0].schema_version, 1);
        assert_eq!(snapshot.events[0].brain_id, snapshot.brain_id);

        let appended = store
            .push(
                "legacy",
                "bob",
                BrainEventKind::Prompt {
                    text: "again".into(),
                },
            )
            .unwrap();
        assert_eq!(appended.schema_version, BRAIN_EVENT_SCHEMA_VERSION);
        assert_eq!(appended.brain_id, snapshot.brain_id);
    }

    #[test]
    fn concurrent_metadata_creation_converges_on_one_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let workers = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    BrainStore::with_root("box.local", Some(root))
                        .snapshot("shared")
                        .unwrap()
                        .brain_id
                })
            })
            .collect::<Vec<_>>();
        let ids = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_ne!(ids[0], BrainId::nil());
    }

    #[test]
    fn attachment_cursor_is_monotonic_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let attachment = store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let head = store.snapshot("shared").unwrap().revision;
        let connection_id = attachment.connection_id.unwrap();
        let acknowledged = store
            .acknowledge("shared", attachment.attachment_id, connection_id, head)
            .unwrap();
        assert_eq!(acknowledged.acknowledged_seq, head);
        assert!(store
            .acknowledge(
                "shared",
                attachment.attachment_id,
                connection_id,
                head + 1,
            )
            .is_err());
        assert!(store
            .acknowledge(
                "shared",
                attachment.attachment_id,
                connection_id,
                head - 1,
            )
            .is_err());
        store
            .detach(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted
            .snapshot("shared")
            .unwrap()
            .attachments
            .into_iter()
            .find(|candidate| candidate.attachment_id == attachment.attachment_id)
            .unwrap();
        assert_eq!(restored.acknowledged_seq, head);
        assert!(!restored.connected);
        let reattached = restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(attachment.attachment_id),
            )
            .unwrap();
        assert!(!reattached.connected);
        assert_eq!(reattached.acknowledged_seq, head);
        let reattached = restarted
            .activate_connection(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
            )
            .unwrap();
        assert!(reattached.connected);
        assert!(restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Observer,
                Some(attachment.attachment_id),
            )
            .is_err());
    }

    #[test]
    fn concurrent_attach_cannot_rebind_one_identity() {
        let store = Arc::new(BrainStore::with_root("box.local", None));
        let attachment_id = AttachmentId::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let attempts = [AttachmentRole::Driver, AttachmentRole::Observer]
            .into_iter()
            .map(|role| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.attach("shared", "alice", role, Some(attachment_id))
                })
            })
            .collect::<Vec<_>>();
        let results = attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let winner = results.into_iter().find_map(Result::ok).unwrap();
        store
            .activate_connection(
                "shared",
                winner.attachment_id,
                winner.connection_id.unwrap(),
            )
            .unwrap();
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ClientAttached { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn stale_connection_cannot_disconnect_a_reattached_client() {
        let store = BrainStore::with_root("box.local", None);
        let first = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .activate_connection(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
            )
            .unwrap();
        store
            .detach("shared", first.attachment_id, first.connection_id.unwrap())
            .unwrap();
        let second = store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .unwrap();
        store
            .activate_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap();

        assert_ne!(first.connection_id, second.connection_id);
        assert!(store
            .detach(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
            )
            .is_err());
        let head = store.snapshot("shared").unwrap().revision;
        assert!(store
            .acknowledge(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
                head,
            )
            .is_err());
        assert!(store
            .require_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .is_ok());
    }

    #[test]
    fn abandoned_attachment_reservation_expires_without_advancing_cursor_or_log() {
        let store = BrainStore::with_root("box.local", None);
        let first = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let first_connection = first.connection_id.unwrap();
        assert!(!first.connected);
        assert_eq!(store.snapshot("shared").unwrap().revision, 0);
        assert!(store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .is_err());

        assert!(store
            .expire_pending_connection("shared", first.attachment_id, first_connection)
            .unwrap());
        let second = store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .unwrap();
        assert_eq!(second.acknowledged_seq, 0);
        assert_eq!(store.snapshot("shared").unwrap().revision, 0);
        assert!(!store
            .expire_pending_connection("shared", first.attachment_id, first_connection)
            .unwrap());

        let active = store
            .activate_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap();
        assert!(active.connected);
        assert!(!store
            .expire_pending_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap());
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.attachments[0].acknowledged_seq, 0);
    }

    #[test]
    fn runner_lease_is_exclusive_renewable_and_event_sourced() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let lease = store
            .acquire_runner_lease("shared", "console-a", generation, None, 60_000)
            .unwrap();
        assert!(store
            .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
            .is_err());
        assert!(store
            .acquire_runner_lease(
                "shared",
                "console-a",
                generation,
                Some(RunnerLeaseId(uuid::Uuid::new_v4())),
                60_000,
            )
            .is_err());
        let renewed = store
            .acquire_runner_lease(
                "shared",
                "console-a",
                generation,
                Some(lease.lease_id),
                120_000,
            )
            .unwrap();
        assert_eq!(renewed.lease_id, lease.lease_id);
        assert!(renewed.expires_ms >= lease.expires_ms);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted
                .snapshot("shared")
                .unwrap()
                .runner_lease
                .unwrap()
                .lease_id,
            lease.lease_id
        );
        restarted
            .release_runner_lease("shared", lease.lease_id)
            .unwrap();
        assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
        assert!(restarted
            .acquire_runner_lease("shared", "console-b", generation + 1, None, 60_000)
            .is_err());
        let replacement = restarted
            .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
            .unwrap();
        assert!(restarted
            .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms - 1)
            .is_ok_and(|expired| !expired));
        assert!(restarted
            .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms)
            .unwrap());
        assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
    }

    #[test]
    fn runner_handoff_is_addressed_durable_and_atomically_replaces_the_lease() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let source = store
            .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
            .unwrap();
        let handoff = store
            .request_runner_handoff(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
            )
            .unwrap();
        assert!(store
            .acquire_runner_lease("shared", "runner-b", generation, None, 60_000)
            .is_err());

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted
                .snapshot("shared")
                .unwrap()
                .runner_handoff
                .as_ref()
                .unwrap()
                .handoff_id,
            handoff.handoff_id
        );
        assert!(restarted
            .accept_runner_handoff(
                "shared",
                "runner-c",
                handoff.handoff_id,
                generation,
                60_000,
            )
            .is_err());
        let replacement = restarted
            .accept_runner_handoff(
                "shared",
                "runner-b",
                handoff.handoff_id,
                generation,
                60_000,
            )
            .unwrap();
        assert_ne!(replacement.lease_id, source.lease_id);
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(snapshot.runner_lease.as_ref().unwrap(), &replacement);
        assert!(snapshot.runner_handoff.is_none());
        assert!(snapshot.runner_lease_was_handed_off(source.lease_id));
        assert!(!snapshot.runner_lease_was_handed_off(replacement.lease_id));
        assert!(snapshot.events.iter().any(|event| matches!(
            event.kind,
            BrainEventKind::RunnerHandoffCompleted { handoff_id, .. }
                if handoff_id == handoff.handoff_id
        )));
    }

    #[test]
    fn releasing_or_cancelling_the_source_invalidates_a_runner_handoff() {
        let store = BrainStore::with_root("box.local", None);
        let generation = store.environment().generation;
        let source = store
            .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
            .unwrap();
        let first = store
            .request_runner_handoff(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
            )
            .unwrap();
        store
            .cancel_runner_handoff("shared", first.handoff_id, "controller")
            .unwrap();
        assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());

        let second = store
            .request_runner_handoff(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
            )
            .unwrap();
        store
            .release_runner_lease("shared", source.lease_id)
            .unwrap();
        assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());
        assert!(store
            .accept_runner_handoff(
                "shared",
                "runner-b",
                second.handoff_id,
                generation,
                60_000,
            )
            .is_err());
    }

    #[test]
    fn runner_handoff_expiry_is_exact_and_durable() {
        let store = BrainStore::with_root("box.local", None);
        let generation = store.environment().generation;
        let source = store
            .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
            .unwrap();
        let handoff = store
            .request_runner_handoff(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
            )
            .unwrap();
        assert!(!store
            .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms - 1)
            .unwrap());
        assert!(store
            .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms)
            .unwrap());
        assert!(store.snapshot("shared").unwrap().runner_handoff.is_none());
        assert!(!store
            .expire_runner_handoff("shared", handoff.handoff_id, handoff.expires_ms)
            .unwrap());
    }

    #[test]
    fn archive_removes_a_brain_but_preserves_its_log() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brains");
        let store = BrainStore::with_root("box.local", Some(root.clone()));
        store
            .push("old", "alice", BrainEventKind::Prompt { text: "hi".into() })
            .unwrap();
        let retained_runtime = store.program_runtime("old").unwrap();
        retained_runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();

        let archive = store.archive("old").unwrap().unwrap();
        assert!(!store.list().unwrap().contains(&"old".to_string()));
        assert!(!root.join("old").exists());
        assert!(archive.join("events.jsonl").exists());
        assert!(archive.join("authority.json").exists());

        retained_runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::MemoryRead,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        assert!(
            !root.join("old").exists(),
            "a retained runtime must not recreate its archived authority path"
        );
    }

    #[tokio::test]
    async fn attached_clients_share_one_ordered_turn_lane_per_brain() {
        let store = BrainStore::with_root("box.local", None);
        let first = store.execution_lock("brain").unwrap();
        let same_brain = store.execution_lock("brain").unwrap();
        let other_brain = store.execution_lock("other").unwrap();
        assert!(Arc::ptr_eq(&first, &same_brain));
        assert!(!Arc::ptr_eq(&first, &other_brain));

        let first_turn = first.lock_owned().await;
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let waiting = tokio::spawn(async move {
            let _second_turn = same_brain.lock_owned().await;
            entered_tx.send(()).unwrap();
        });
        tokio::task::yield_now().await;
        assert!(entered_rx.try_recv().is_err());

        drop(first_turn);
        entered_rx.recv().await.unwrap();
        waiting.await.unwrap();
    }

    #[tokio::test]
    async fn named_brain_restores_one_typed_runtime_without_replaying_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("brain:event:1".into()),
                source: ": square ( S n:int -- S int ! pure ) n n * ;".into(),
                intent: "define square".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        let committed_revision = outcome.output_revision;
        let committed = store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();
        let checkpoint_sha256 = match committed.kind {
            BrainEventKind::RuntimeCommitted {
                checkpoint_sha256,
                ..
            } => checkpoint_sha256,
            other => panic!("expected runtime checkpoint, found {other:?}"),
        };
        assert!(temp
            .path()
            .join("brain/runtime")
            .join(format!("{checkpoint_sha256}.capnp"))
            .is_file());
        assert!(!temp
            .path()
            .join("brain/runtime")
            .join(format!("{checkpoint_sha256}.json"))
            .exists());

        let event_log = std::fs::read_to_string(temp.path().join("brain/events.jsonl")).unwrap();
        for line in event_log.lines() {
            if let Err(error) = serde_json::from_str::<BrainEvent>(line) {
                panic!("checkpoint event must round-trip through JSONL: {error}\n{line}");
            }
        }

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), committed_revision);
        let outcome = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("brain:event:2".into()),
                source: "(square 7)".into(),
                intent: "call restored definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.values, vec![crate::programs::ProgramValue::Int(49)]);
        assert_eq!(outcome.output_revision, committed_revision + 1);
    }

    #[tokio::test]
    async fn named_brain_reads_legacy_json_checkpoint_without_rewriting_history() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        store.snapshot("brain").unwrap();
        let runtime = crate::runtime::ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("legacy-checkpoint.lisp".into()),
                source: "(define (double (n : int)) (* n 2))".into(),
                intent: "create legacy checkpoint".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|revision| revision.revision == outcome.output_revision)
            .and_then(|revision| revision.checkpoint)
            .unwrap();
        let encoded = serde_json::to_vec(&checkpoint).unwrap();
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        let runtime_directory = temp.path().join("brain/runtime");
        std::fs::create_dir_all(&runtime_directory).unwrap();
        std::fs::write(
            runtime_directory.join(format!("{checkpoint_sha256}.json")),
            encoded,
        )
        .unwrap();
        store
            .push(
                "brain",
                "legacy-daemon",
                BrainEventKind::RuntimeCommitted {
                    request_seq: 1,
                    runtime_revision: outcome.output_revision,
                    checkpoint_sha256: checkpoint_sha256.clone(),
                },
            )
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("legacy-checkpoint.forth".into()),
                source: "21 double".into(),
                intent: "restore legacy checkpoint".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
        assert!(runtime_directory
            .join(format!("{checkpoint_sha256}.json"))
            .is_file());
        assert!(!runtime_directory
            .join(format!("{checkpoint_sha256}.capnp"))
            .exists());
    }

    #[tokio::test]
    async fn named_brain_commits_a_validated_frontend_runner_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        store.snapshot("brain").unwrap();
        let runner = crate::runtime::ProgramRuntime::new();
        let outcome = runner
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("runner:event:1".into()),
                source: "(define (triple (n : int)) (* n 3))".into(),
                intent: "frontend runner definition".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runner.manifest_generation(),
                expected_revision: Some(runner.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let checkpoint = runner
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|revision| revision.revision == outcome.output_revision)
            .and_then(|revision| revision.checkpoint)
            .unwrap();
        store
            .commit_runner_runtime("brain", 1, outcome.output_revision, checkpoint)
            .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("test:restored-runner".into()),
                source: "14 triple".into(),
                intent: "call frontend definition after daemon restart".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
    }

    #[tokio::test]
    async fn frontend_replacement_reacquires_the_same_durable_brain() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let lease = store
            .acquire_runner_lease("dogfood", "frontend-a", generation, None, 60_000)
            .unwrap();
        let prompt = store
            .push(
                "dogfood",
                "developer",
                BrainEventKind::Prompt {
                    text: "continue the self-upgrade goal".into(),
                },
            )
            .unwrap();
        let runner = crate::runtime::ProgramRuntime::new();
        let outcome = runner
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("dogfood:define".into()),
                source: "(define (next-step (n : int)) (+ n 1))".into(),
                intent: "retain work across frontend replacement".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runner.manifest_generation(),
                expected_revision: Some(runner.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let checkpoint = runner
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|revision| revision.revision == outcome.output_revision)
            .and_then(|revision| revision.checkpoint)
            .unwrap();
        store
            .commit_runner_runtime(
                "dogfood",
                prompt.seq,
                outcome.output_revision,
                checkpoint,
            )
            .unwrap();
        store
            .release_runner_lease("dogfood", lease.lease_id)
            .unwrap();
        drop(store);

        // Model both halves of the production handoff: a daemon opens the
        // durable store again, then the replacement frontend acquires a fresh
        // lease for the same Brain identity.
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let replacement = restarted
            .acquire_runner_lease("dogfood", "frontend-a", generation, None, 60_000)
            .unwrap();
        let snapshot = restarted.snapshot("dogfood").unwrap();
        assert_eq!(snapshot.runner_lease.unwrap().lease_id, replacement.lease_id);
        assert!(snapshot.events.iter().any(|event| {
            matches!(
                &event.kind,
                BrainEventKind::Prompt { text }
                    if text == "continue the self-upgrade goal"
            )
        }));

        let restored = restarted.program_runtime("dogfood").unwrap();
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("dogfood:resume".into()),
                source: "41 next-step".into(),
                intent: "resume after frontend replacement".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
    }

    #[tokio::test]
    async fn named_brain_restores_scoped_authority_from_its_separate_policy_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let session_id = runtime.capability_session_id();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::ProcessRun,
                    selector: crate::vm::ResourceSelector::None,
                },
                crate::vm::GrantScope::Session { session_id },
                "test-user",
                None,
            )
            .unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("brain:event:1".into()),
                source: "42".into(),
                intent: "create a durable revision".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();

        let authority_path = temp.path().join("brain/authority.json");
        assert!(authority_path.exists());
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.capability_session_id(), session_id);
        let ledger = restored.capability_ledger().unwrap();
        assert_eq!(ledger.grants.grants.len(), 1);
        assert_eq!(ledger.grants.grants[0].id, grant_id);
        assert!(matches!(
            ledger.grants.grants[0].scope,
            crate::vm::GrantScope::Session { session_id: restored_id } if restored_id == session_id
        ));
        assert_eq!(ledger.audit.len(), 1);
    }

    #[test]
    fn named_brain_persists_grants_and_revocation_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::ProcessRun,
                    selector: crate::vm::ResourceSelector::None,
                },
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();

        let after_grant = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = after_grant.program_runtime("brain").unwrap();
        assert_eq!(restored.capability_ledger().unwrap().grants.grants[0].id, grant_id);

        runtime.revoke_typed_capability(grant_id).unwrap();
        let after_revoke = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = after_revoke.program_runtime("brain").unwrap();
        let ledger = restored.capability_ledger().unwrap();
        assert!(ledger.grants.grants[0].revoked_at_unix_ms.is_some());
        assert_eq!(ledger.audit.len(), 2);
        assert!(matches!(
            ledger.audit[1].action,
            crate::vm::CapabilityAuditAction::Revoked
        ));
    }

    #[test]
    fn named_brain_persists_policy_changes_and_denials_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let requirement = crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProcessRun,
            selector: crate::vm::ResourceSelector::None,
        };
        let grant_id = runtime
            .issue_typed_capability(
                requirement.clone(),
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let mut denied = std::collections::BTreeSet::new();
        denied.insert(crate::vm::CapabilityKind::ProcessRun);
        assert_eq!(
            runtime
                .apply_capability_policy(
                    crate::vm::CapabilityPolicy {
                        policy_hash: "locked-policy-v2".into(),
                        denied_capabilities: denied.clone(),
                    },
                    "policy-admin",
                )
                .unwrap(),
            vec![grant_id]
        );

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(
            restored.capability_policy().unwrap(),
            crate::vm::CapabilityPolicy {
                policy_hash: "locked-policy-v2".into(),
                denied_capabilities: denied,
            }
        );
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(restored
            .issue_typed_capability(
                requirement,
                crate::vm::GrantScope::Global,
                "test-user",
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("denied by policy"));
    }

    #[tokio::test]
    async fn named_brain_persists_denial_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let pending = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: "(file-read (path \"Cargo.toml\"))".into(),
                intent: "test durable denial".into(),
                effect: crate::programs::ExecutionEffect::WorkspaceRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let denied = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                crate::vm::ApprovalChoice::Deny,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(
            denied.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert!(matches!(
            restored
                .capability_ledger()
                .unwrap()
                .authorization_audit
                .last()
                .map(|entry| &entry.decision),
            Some(crate::vm::AuthorizationDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn named_brain_persists_host_authorization_even_when_the_run_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
                ),
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let failed = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "s\"Cargo.toml\" path file-read drop 1 0 /".into(),
                intent: "read then fail".into(),
                effect: crate::programs::ExecutionEffect::WorkspaceRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            failed.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );
        assert_eq!(runtime.revision(), 0, "failed VM state must roll back");

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let ledger = restarted
            .program_runtime("brain")
            .unwrap()
            .capability_ledger()
            .unwrap();
        assert!(matches!(
            ledger.authorization_audit.last().map(|entry| &entry.decision),
            Some(crate::vm::AuthorizationDecision::Allowed { grant_id: used }) if *used == grant_id
        ));
    }

    #[tokio::test]
    async fn named_brain_checkpoint_without_authority_record_restores_without_grants() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "7".into(),
                intent: "checkpoint without authority".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();
        std::fs::remove_file(temp.path().join("brain/authority.json")).unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), outcome.output_revision);
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
    }

    #[tokio::test]
    async fn named_brain_rejects_a_tampered_authority_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "1".into(),
                intent: "persist authority envelope".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();
        let authority_path = temp.path().join("brain/authority.json");
        let mut authority: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&authority_path).unwrap()).unwrap();
        authority["authority"]["project_id"] = serde_json::json!("tampered-project");
        std::fs::write(&authority_path, serde_json::to_vec(&authority).unwrap()).unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let error = restarted
            .program_runtime("brain")
            .err()
            .expect("tampered authority must fail closed");
        assert!(error.to_string().contains("restore authority"));
        assert!(format!("{error:#}").contains("integrity check"));
    }

    #[tokio::test]
    async fn out_of_order_checkpoint_events_never_regress_a_brain_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "concurrent checkpoint ordering".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime.submit_typed_only(submit("1", 0)).await.unwrap();
        let second = runtime.submit_typed_only(submit("2", 1)).await.unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), second.output_revision);
        let values = restored
            .inspect()
            .await
            .unwrap()
            .stack
            .into_iter()
            .map(|cell| cell.value)
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                crate::programs::ProgramValue::Int(1),
                crate::programs::ProgramValue::Int(2),
            ]
        );
    }

    #[tokio::test]
    async fn legacy_restart_revision_reset_keeps_the_latest_request_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = crate::runtime::ProgramRuntime::new();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "legacy revision migration".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime
            .submit_typed_only(submit(
                ": square ( S n:int -- S int ! pure ) n n * ;",
                0,
            ))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();
        let second = runtime
            .submit_typed_only(submit("1 drop", first.output_revision))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();

        // ProgramRuntime::from_checkpoint historically reset its local
        // revision. Simulate an old daemon adding newer state as revision 1.
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|snapshot| snapshot.checkpoint.clone())
            .unwrap();
        let legacy_restarted = crate::runtime::ProgramRuntime::from_checkpoint(checkpoint).unwrap();
        let legacy_commit = legacy_restarted
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: ": cube ( S n:int -- S int ! pure ) n n * n * ;".into(),
                intent: "new state after legacy restart".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: legacy_restarted.manifest_generation(),
                expected_revision: Some(0),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(legacy_commit.output_revision, 1);
        store
            .commit_runtime(
                "brain",
                3,
                legacy_commit.output_revision,
                &legacy_restarted,
            )
            .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), 3);
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: "(cube 4)".into(),
                intent: "call latest migrated definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(3),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(64)]);
        assert_eq!(called.output_revision, 4);
    }

    #[test]
    fn names_cannot_escape_the_storage_root() {
        assert!(BrainStore::validate_name("../other").is_err());
        assert!(BrainStore::validate_name("valid-brain_2").is_ok());
    }

    #[test]
    // An environment is an indivisible authority boundary, not two routable heads.
    fn environment_binds_machine_and_workspace_as_one_revision() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = BrainStore::with_environment(
            "gpu-box.local",
            workspace.path(),
            Some(state.path().into()),
        );

        store
            .push(
                "project",
                "laptop.local",
                BrainEventKind::Prompt { text: "go".into() },
            )
            .unwrap();
        let snapshot = store.snapshot("project").unwrap();

        assert_eq!(snapshot.environment.machine, "gpu-box.local");
        assert_eq!(
            snapshot.environment.workspace,
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(snapshot.environment.generation, 1);
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn old_events_default_to_the_initial_environment_generation() {
        let event: BrainEvent = serde_json::from_str(
            r#"{"seq":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hi"}"#,
        )
        .unwrap();
        assert_eq!(event.environment_generation, 1);
    }
}
