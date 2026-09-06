//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const EVENT_CHANNEL_CAPACITY: usize = 256;
const BRAIN_EVENT_SCHEMA_VERSION: u32 = 15;
/// Completed audit histories retained per named Brain. Together with the
/// bounded intent/outcome encodings this caps the audit projection and its
/// share of `events.jsonl`; unresolved write-ahead entries are never pruned.
const MAX_RETAINED_TERMINAL_EFFECT_AUDITS: usize = 128;
const BRAIN_METADATA_VERSION: u32 = 1;
const BRAIN_INITIALIZATION_VERSION: u32 = 1;
const DEFAULT_INITIALIZATION_MODULE: &str = "finch.brain.initialization";
const DEFAULT_INITIALIZATION_SOURCE: &str = "(define (finch-brain-initialized) : int 1)";

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

/// Durable identity and preconditions for one authorized Brain mutation.
/// The receipt lives on the first canonical event produced by the mutation,
/// making acceptance and deduplication one append-only commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainMutationReceipt {
    pub mutation_id: uuid::Uuid,
    pub attachment_id: AttachmentId,
    pub expected_revision: u64,
    pub environment_generation: u64,
    pub command_sha256: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrainMutationAppend {
    pub event: BrainEvent,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrainExecutableMutationAppend {
    pub accepted: BrainEvent,
    pub run: BrainRun,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrainRunCancellationReservation {
    pub run: BrainRun,
    pub needs_runner_cancel: bool,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BrainApprovalDecisionReservation {
    pub event: BrainEvent,
    pub delivered: bool,
    pub replayed: bool,
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

// `Ord` so the due index can key on `(next_due_ms, ScheduleId)` and keep a
// total order: two schedules due in the same millisecond still have a stable,
// deterministic position rather than colliding (#374).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

/// Opaque daemon-side authority for one runner capability. The Cap'n Proto
/// peer never receives these fields; it can only invoke the capability that
/// holds this grant.
#[derive(Debug, Clone)]
pub(crate) struct EffectAuditAuthorityGrant {
    brain: String,
    run_id: RunId,
    authority: crate::runtime::effect_log::EffectAuditAuthority,
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
        anyhow::ensure!(
            self.version == BRAIN_INITIALIZATION_VERSION,
            "unsupported Brain initialization version {}",
            self.version
        );
        anyhow::ensure!(
            self.brain_id == brain_id,
            "Brain initialization identity does not match metadata"
        );
        anyhow::ensure!(
            !self.module.trim().is_empty() && self.module_revision > 0,
            "Brain initialization module identity is invalid"
        );
        anyhow::ensure!(
            !self.source.trim().is_empty(),
            "Brain initialization program is empty"
        );
        let actual = hex::encode(Sha256::digest(self.source.as_bytes()));
        anyhow::ensure!(
            self.source_sha256 == actual,
            "Brain initialization program digest does not match its source"
        );
        anyhow::ensure!(
            self == &Self::reviewed_default(brain_id),
            "Brain initialization contract is not the reviewed built-in module"
        );
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
        anyhow::ensure!(
            identity == &self.module_identity(),
            "Brain-module schedule identity does not match the reviewed initialization module"
        );
        anyhow::ensure!(
            schedule.language == self.language,
            "Brain initialization schedule language does not match the reviewed module"
        );
        let actual = hex::encode(Sha256::digest(schedule.source.as_bytes()));
        anyhow::ensure!(
            identity.source_sha256 == actual,
            "Brain initialization schedule digest does not match its source"
        );
        anyhow::ensure!(
            schedule.source == self.source,
            "Brain initialization schedule source is not the reviewed module"
        );
        anyhow::ensure!(
            schedule.grant_ceiling == self.capability_budget,
            "Brain initialization schedule capability budget is not the reviewed ceiling"
        );
        anyhow::ensure!(
            schedule.interval_ms.is_none()
                && schedule.delivery_policy == BrainScheduleDeliveryPolicy::Coalesce,
            "Brain initialization schedule must be a coalesced one-shot"
        );
        Ok(())
    }

    fn validate_schedule_due(
        &self,
        schedule: &BrainSchedule,
        due: &BrainScheduleDue,
    ) -> Result<()> {
        self.validate_schedule(schedule)?;
        anyhow::ensure!(
            due.language == schedule.language
                && due.source == schedule.source
                && due.grant_ceiling == schedule.grant_ceiling,
            "Brain initialization delivery does not match its reviewed schedule"
        );
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
    /// Durable typed result for an accepted mutation that either reserves an
    /// external effect or makes no projection change.
    MutationRecorded {
        outcome: BrainMutationOutcome,
    },
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
        /// Exact ordered provider/tool continuation. Legacy results decode as
        /// empty and retain their historical projection.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        continuation_messages: Vec<crate::claude::Message>,
        /// Provider identity/accounting captured at the completed invocation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_metadata: Option<crate::providers::types::InvocationMetadata>,
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
    /// Monotonic write-ahead audit transition for one physical host effect.
    /// Schema-v14 `EffectRecorded` values remain readable as legacy terminal
    /// snapshots; all new execution uses this reducer-backed form.
    EffectAuditTransition {
        transition: crate::runtime::effect_log::EffectAuditTransition,
    },
    ScheduleChanged {
        schedule: BrainSchedule,
    },
    ScheduleDue {
        due: BrainScheduleDue,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum BrainMutationOutcome {
    RunCancellationReserved {
        run_id: RunId,
    },
    RunCancellationDispatching {
        run_id: RunId,
        mutation_id: uuid::Uuid,
    },
    RunCancellationReconciled {
        run_id: RunId,
        mutation_id: uuid::Uuid,
    },
    RunAlreadyCancelled {
        run_id: RunId,
    },
    RunCancellationNoop {
        run_id: RunId,
    },
    ScheduleCancellationNoop {
        schedule_id: ScheduleId,
    },
    HandoffCancellationNoop {
        handoff_id: RunnerHandoffId,
    },
    ApprovalDecisionDelivered {
        request_seq: u64,
        approval_id: String,
        mutation_id: uuid::Uuid,
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
    /// Canonical lifecycle correlation. Legacy and non-run events have none.
    #[serde(
        default,
        rename = "correlation_run_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation: Option<BrainMutationReceipt>,
    #[serde(flatten)]
    pub kind: BrainEventKind,
}

/// One physical canonical journal append. Legacy lines remain bare events;
/// compound transitions use this tagged record while preserving a cursor for
/// every contained logical event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "journal_record", rename_all = "snake_case")]
enum BrainJournalRecord {
    EventBatch {
        /// New batches carry their logical framing inside the physical JSONL
        /// record. Optional fields keep journals written by the first batch
        /// implementation readable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_count: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload_sha256: Option<String>,
        events: Vec<BrainEvent>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DisconnectTerminalizationIntent {
    sender: String,
    run_id: RunId,
    request_seq: u64,
    status: BrainRunStatus,
    detail: String,
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
    /// Canonical projection of the schema-v15 effect-audit transitions.
    #[serde(default)]
    pub effect_audits: Vec<crate::runtime::effect_log::EffectAuditEntry>,
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

fn observer_effect_audit_event(event: &BrainEvent) -> BrainEvent {
    let mut projected = event.clone();
    if let BrainEventKind::EffectAuditTransition { transition } = &event.kind {
        projected.kind = BrainEventKind::EffectAuditTransition {
            transition: transition.observer_projection(),
        };
    }
    projected
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
    effect_audits: crate::runtime::effect_log::EffectAuditReducer,
    recent_effect_audits: std::collections::VecDeque<crate::runtime::effect_log::EffectAuditEntry>,
    revision: u64,
    tx: broadcast::Sender<BrainEvent>,
}

/// Every active schedule in the store, ordered by when it next comes due.
///
/// Schedule delivery used to select by *Brain*: the daemon enumerated the whole
/// Brain root once a second and hydrated every Brain to discover whether any of
/// them had work. Ordering by `next_due_ms` existed, but only inside a single
/// Brain and only after that Brain was already loaded, because
/// `BrainState::schedules` is a per-Brain `HashMap`. This is the same ordering
/// lifted to the store, so the daemon can select by *due time* and hydrate only
/// the Brains that actually have work (#374).
///
/// `due` is the ordering; `by_brain` exists so one Brain's entries can be
/// replaced without scanning the whole map. Keying on `(next_due_ms,
/// ScheduleId)` keeps the order total, so two schedules due in the same
/// millisecond still have a stable position.
#[derive(Debug, Default)]
struct ScheduleIndex {
    due: std::collections::BTreeMap<(u64, ScheduleId), String>,
    by_brain: HashMap<String, HashMap<ScheduleId, u64>>,
}

impl ScheduleIndex {
    /// Replace everything known about `name` with its current active schedules.
    ///
    /// Called with the Brain's state in hand, so it is O(that Brain's
    /// schedules) rather than O(the index).
    fn reindex(&mut self, name: &str, schedules: &HashMap<ScheduleId, BrainSchedule>) {
        if let Some(previous) = self.by_brain.remove(name) {
            for (schedule_id, next_due_ms) in previous {
                self.due.remove(&(next_due_ms, schedule_id));
            }
        }
        let mut current = HashMap::new();
        for schedule in schedules.values().filter(|schedule| schedule.active) {
            self.due.insert(
                (schedule.next_due_ms, schedule.schedule_id),
                name.to_string(),
            );
            current.insert(schedule.schedule_id, schedule.next_due_ms);
        }
        if !current.is_empty() {
            self.by_brain.insert(name.to_string(), current);
        }
    }

    /// Forget a Brain entirely, for removal and archival.
    fn forget(&mut self, name: &str) {
        if let Some(previous) = self.by_brain.remove(name) {
            for (schedule_id, next_due_ms) in previous {
                self.due.remove(&(next_due_ms, schedule_id));
            }
        }
    }

    /// When the earliest active schedule in the store comes due.
    fn next_due_ms(&self) -> Option<u64> {
        self.due.keys().next().map(|(next_due_ms, _)| *next_due_ms)
    }

    /// Brains holding at least one schedule due at or before `now_ms`, in due
    /// order, each named once.
    fn due_brains(&self, now_ms: u64) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut brains = Vec::new();
        // The upper bound is the largest id at `now_ms`, so every schedule due at
        // exactly `now_ms` is included rather than dropped by an exclusive range.
        let ceiling = ScheduleId(uuid::Uuid::from_bytes([0xff; 16]));
        for (_, name) in self.due.range(..=(now_ms, ceiling)) {
            if seen.insert(name.clone()) {
                brains.push(name.clone());
            }
        }
        brains
    }
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
            effect_audits: crate::runtime::effect_log::EffectAuditReducer::default(),
            recent_effect_audits: std::collections::VecDeque::new(),
            revision: 0,
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
        self.revision = self.revision.max(event.seq);
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
                if self
                    .runtime_checkpoint
                    .as_ref()
                    .is_none_or(|current| request_seq >= &current.request_seq)
                {
                    self.runtime_checkpoint = Some(RuntimeCheckpointState {
                        request_seq: *request_seq,
                        durable_revision,
                        checkpoint_sha256: checkpoint_sha256.clone(),
                    });
                } else if let Some(current) = self.runtime_checkpoint.as_mut() {
                    current.durable_revision = durable_revision;
                }
            }
            BrainEventKind::EffectAuditTransition { transition } => {
                self.effect_audits
                    .apply(transition.clone())
                    .expect("validated effect audit transition became invalid while projecting");
            }
            BrainEventKind::Prompt { .. }
            | BrainEventKind::SpeculativePrompt { .. }
            | BrainEventKind::MutationRecorded { .. }
            | BrainEventKind::ParticipantMessage { .. }
            | BrainEventKind::ToolCall { .. }
            | BrainEventKind::ToolResult { .. }
            | BrainEventKind::ApprovalRequested { .. }
            | BrainEventKind::ApprovalDecided { .. }
            | BrainEventKind::Result { .. } => {}
            BrainEventKind::EffectRecorded {
                request_seq,
                execution_id,
                effect,
                state,
            } => {
                let identity = crate::runtime::effect_log::EffectAuditIdentity {
                    brain_id: self.brain_id.0,
                    run_id: event.run_id.unwrap_or(RunId(uuid::Uuid::nil())).0,
                    request_seq: *request_seq,
                    execution_id: *execution_id,
                    effect_sequence: effect.sequence,
                };
                let authority = crate::runtime::effect_log::EffectAuditAuthority {
                    authority_id: uuid::Uuid::nil(),
                    runner_lease_id: uuid::Uuid::nil(),
                    runner_subject: "legacy-v14".into(),
                    connection_id: None,
                    environment_generation: event.environment_generation,
                };
                self.effect_audits
                    .apply(crate::runtime::effect_log::EffectAuditTransition::Reserve {
                        intent: crate::runtime::effect_log::EffectAuditIntent::from_effect(
                            identity, effect,
                        )
                        .expect("legacy effect audit payload must be bounded"),
                        authority: authority.clone(),
                    })
                    .expect("legacy effect reservation must project");
                self.effect_audits.apply(
                    crate::runtime::effect_log::EffectAuditTransition::Finish {
                        identity, authority_id: authority.authority_id,
                        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::LegacyV14Snapshot {
                            state: state.clone(),
                        },
                    },
                ).expect("legacy effect terminal snapshot must project");
            }
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
    runtime_checkpoints: Arc<RwLock<HashMap<String, crate::vm::TypedRuntimeCheckpoint>>>,
    /// One ordered turn lane per Brain. HTTP/WebSocket clients may submit
    /// concurrently, but accepted input, VM commit, and its Result event must
    /// remain an indivisible sequence against the authoritative revision.
    execution_locks: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    run_publication_gates:
        Arc<RwLock<HashMap<(String, RunId), Arc<tokio::sync::Mutex<RunPublicationGate>>>>>,
    /// Ephemeral transport generation that currently supervises a live run.
    /// This is deliberately not replayed: restored queued work is transport
    /// independent until a new connection explicitly dispatches it.
    run_connection_authority: Arc<RwLock<RunConnectionAuthority>>,
    disconnect_retry_owners: Arc<std::sync::Mutex<HashSet<(String, RunId)>>>,
    effect_audit_storage: Arc<std::sync::Mutex<HashMap<String, EffectAuditStorage>>>,
    /// Active schedules across every resident Brain, ordered by due time (#374).
    schedule_index: Arc<RwLock<ScheduleIndex>>,
    /// Woken whenever the index gains an entry that may be due sooner than the
    /// head the delivery loop is currently sleeping towards. Without this a
    /// schedule created during a long sleep would not fire until the loop woke
    /// for the older head.
    schedule_wakeup: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    fail_event_batches: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    fail_cancellation_terminal_appends: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    cancellation_reservation_pause:
        Arc<std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>,
}

#[derive(Debug, Default)]
pub(crate) struct RunPublicationGate {
    cancel_requested: bool,
}

#[derive(Debug, Default)]
struct RunConnectionAuthority {
    owners: HashMap<(String, RunId), ConnectionId>,
    retired: HashSet<(String, AttachmentId, ConnectionId)>,
}

struct EffectAuditStorage {
    active: super::effect_audit_archive::EffectAuditActiveJournal,
    replay: super::effect_audit_archive::EffectAuditReplayArchive,
}

impl RunPublicationGate {
    pub(crate) fn cancel_requested(&self) -> bool {
        self.cancel_requested
    }
}

impl BrainStore {
    fn state_has_run_cancellation_reservation(state: &BrainState, run_id: RunId) -> bool {
        state.events.iter().any(|event| {
            matches!(
                event.kind,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationReserved { run_id: recorded },
                } if recorded == run_id
            )
        })
    }

    /// Durable cancellation intent is itself the publication fence. The
    /// volatile gate exists only to serialize competing publishers.
    pub(crate) fn run_cancellation_reserved(&self, name: &str, run_id: RunId) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        Ok(Self::state_has_run_cancellation_reservation(state, run_id))
    }

    pub(crate) fn schedule_disconnect_terminalization_retry(
        &self,
        name: String,
        sender: String,
        run_id: RunId,
        request_seq: u64,
        status: BrainRunStatus,
        detail: String,
    ) {
        let key = (name.clone(), run_id);
        if !self
            .disconnect_retry_owners
            .lock()
            .expect("disconnect retry registry poisoned")
            .insert(key.clone())
        {
            return;
        }
        let store = self.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.disconnect_retry_owners
                .lock()
                .expect("disconnect retry registry poisoned")
                .remove(&key);
            tracing::error!(brain = %name, run_id = %run_id.0,
                "disconnect terminalization has durable intent but no live retry runtime");
            return;
        };
        runtime.spawn(async move {
            let mut delay = std::time::Duration::from_millis(10);
            loop {
                match store.terminalize_run_with_result_if_active(
                    &name,
                    &sender,
                    run_id,
                    request_seq,
                    status,
                    detail.clone(),
                ) {
                    Ok(_) => {
                        let terminal = store
                            .inspect_run(&name, run_id)
                            .is_ok_and(|run| run.status.is_terminal());
                        let intent_pending = store
                            .disconnect_intent_path(&name, run_id)
                            .is_some_and(|path| path.exists());
                        if terminal && !intent_pending {
                            break;
                        }
                    }
                    Err(error) => {
                        tracing::error!(brain = %name, run_id = %run_id.0, %error,
                            retry_ms = delay.as_millis(),
                            "disconnect terminalization remains pending");
                    }
                }
                let jitter = u64::from(run_id.0.as_bytes()[0]) % 7;
                tokio::time::sleep(delay + std::time::Duration::from_millis(jitter)).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
            store
                .disconnect_retry_owners
                .lock()
                .expect("disconnect retry registry poisoned")
                .remove(&key);
        });
    }

    pub(crate) fn schedule_reserved_cancellation_retry(
        &self,
        name: String,
        sender: String,
        run_id: RunId,
    ) {
        let key = (name.clone(), run_id);
        if !self
            .disconnect_retry_owners
            .lock()
            .expect("terminalization retry registry poisoned")
            .insert(key.clone())
        {
            return;
        }
        let store = self.clone();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.disconnect_retry_owners
                .lock()
                .expect("terminalization retry registry poisoned")
                .remove(&key);
            tracing::error!(brain = %name, run_id = %run_id.0,
                "reserved cancellation has no live retry runtime");
            return;
        };
        runtime.spawn(async move {
            let mut delay = std::time::Duration::from_millis(10);
            loop {
                let publication = match store.acquire_run_publication(&name, run_id).await {
                    Ok(publication) => publication,
                    Err(error) => {
                        tracing::error!(brain = %name, run_id = %run_id.0, %error,
                            "reserved cancellation retry could not acquire publication gate");
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(std::time::Duration::from_secs(5));
                        continue;
                    }
                };
                let terminal = store
                    .inspect_run(&name, run_id)
                    .is_ok_and(|run| run.status.is_terminal());
                let reserved = store
                    .run_cancellation_reserved(&name, run_id)
                    .unwrap_or(false);
                if terminal || !reserved {
                    drop(publication);
                    break;
                }
                let result = store.complete_reserved_run_cancellation(&name, &sender, run_id);
                drop(publication);
                match result {
                    Ok(_) => {
                        let _ = store.prune_run_publication(&name, run_id);
                        break;
                    }
                    Err(error) => tracing::error!(brain = %name, run_id = %run_id.0, %error,
                        retry_ms = delay.as_millis(),
                        "reserved cancellation terminalization remains pending"),
                }
                let jitter = u64::from(run_id.0.as_bytes()[1]) % 7;
                tokio::time::sleep(delay + std::time::Duration::from_millis(jitter)).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(5));
            }
            store
                .disconnect_retry_owners
                .lock()
                .expect("terminalization retry registry poisoned")
                .remove(&key);
        });
    }

    /// Number of disconnect terminalizations currently awaiting durable
    /// publication. Health reporting uses this to expose persistent failures.
    pub fn pending_disconnect_terminalization_retries(&self) -> usize {
        self.disconnect_retry_owners
            .lock()
            .expect("disconnect retry registry poisoned")
            .len()
    }

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
            run_publication_gates: Arc::new(RwLock::new(HashMap::new())),
            run_connection_authority: Arc::new(RwLock::new(RunConnectionAuthority::default())),
            disconnect_retry_owners: Arc::new(std::sync::Mutex::new(HashSet::new())),
            effect_audit_storage: Arc::new(std::sync::Mutex::new(HashMap::new())),
            schedule_index: Arc::new(RwLock::new(ScheduleIndex::default())),
            schedule_wakeup: Arc::new(tokio::sync::Notify::new()),
            #[cfg(test)]
            fail_event_batches: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            fail_cancellation_terminal_appends: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            #[cfg(test)]
            cancellation_reservation_pause: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn environment(&self) -> &BrainEnvironment {
        &self.environment
    }

    #[cfg(test)]
    pub(crate) fn with_test_environment_generation(
        machine: impl Into<String>,
        root: Option<PathBuf>,
        generation: u64,
    ) -> Self {
        let mut store = Self::with_root(machine, root);
        store.environment.generation = generation;
        store
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

    pub(crate) fn bind_run_connection(
        &self,
        name: &str,
        run_id: RunId,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        anyhow::ensure!(
            attachment.connected && attachment.connection_id == Some(connection_id),
            "Brain attachment connection is no longer current"
        );
        anyhow::ensure!(
            state
                .runs
                .get(&run_id)
                .is_some_and(|run| !run.status.is_terminal()),
            "terminal Brain run cannot acquire connection ownership"
        );
        let mut authority = self
            .run_connection_authority
            .write()
            .expect("shared Brain run-authority map poisoned");
        anyhow::ensure!(
            !authority
                .retired
                .contains(&(name.to_string(), attachment_id, connection_id)),
            "Brain attachment connection is no longer current"
        );
        authority
            .owners
            .insert((name.to_string(), run_id), connection_id);
        Ok(())
    }

    pub(crate) fn retire_connection_and_owned_active_runs(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<Vec<RunId>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        anyhow::ensure!(
            attachment.connected && attachment.connection_id == Some(connection_id),
            "Brain attachment connection is no longer current"
        );
        let mut authority = self
            .run_connection_authority
            .write()
            .expect("shared Brain run-authority map poisoned");
        authority
            .retired
            .insert((name.to_string(), attachment_id, connection_id));
        Ok(state
            .runs
            .values()
            .filter(|run| {
                run.initiating_attachment_id == attachment_id
                    && matches!(
                        run.status,
                        BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
                    )
                    && authority.owners.get(&(name.to_string(), run.run_id)) == Some(&connection_id)
            })
            .map(|run| run.run_id)
            .collect())
    }

    fn run_publication_gate(
        &self,
        name: &str,
        run_id: RunId,
    ) -> Result<Arc<tokio::sync::Mutex<RunPublicationGate>>> {
        let name = Self::validate_name(name)?;
        if let Some(gate) = self
            .run_publication_gates
            .read()
            .expect("shared Brain run-gate map poisoned")
            .get(&(name.to_string(), run_id))
            .cloned()
        {
            return Ok(gate);
        }
        let mut gates = self
            .run_publication_gates
            .write()
            .expect("shared Brain run-gate map poisoned");
        Ok(gates
            .entry((name.to_string(), run_id))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(RunPublicationGate::default())))
            .clone())
    }

    pub(crate) async fn acquire_run_publication(
        &self,
        name: &str,
        run_id: RunId,
    ) -> Result<tokio::sync::OwnedMutexGuard<RunPublicationGate>> {
        Ok(self.run_publication_gate(name, run_id)?.lock_owned().await)
    }

    pub(crate) async fn reserve_run_publication_cancellation(
        &self,
        name: &str,
        run_id: RunId,
    ) -> Result<()> {
        let mut gate = self.acquire_run_publication(name, run_id).await?;
        let run = self.inspect_run(name, run_id)?;
        anyhow::ensure!(!run.status.is_terminal(), "Brain run has already finished");
        gate.cancel_requested = true;
        Ok(())
    }

    pub(crate) async fn clear_run_cancellation(&self, name: &str, run_id: RunId) -> Result<()> {
        self.acquire_run_publication(name, run_id)
            .await?
            .cancel_requested = false;
        Ok(())
    }

    pub(crate) fn prune_run_publication(&self, name: &str, run_id: RunId) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.run_publication_gates
            .write()
            .expect("shared Brain run-gate map poisoned")
            .remove(&(name.to_string(), run_id));
        self.run_connection_authority
            .write()
            .expect("shared Brain run-authority map poisoned")
            .owners
            .remove(&(name.to_string(), run_id));
        Ok(())
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
            revision: state.revision,
            events: state
                .events
                .iter()
                .map(observer_effect_audit_event)
                .collect(),
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
            effect_audits: state
                .effect_audits
                .entries()
                .values()
                .map(crate::runtime::effect_log::EffectAuditEntry::observer_projection)
                .chain(state.recent_effect_audits.iter().cloned())
                .collect(),
        })
    }

    /// Mint daemon-local authority for the currently active runner lease.
    pub(crate) fn issue_effect_audit_authority(
        &self,
        name: &str,
        run_id: RunId,
        lease_id: RunnerLeaseId,
        _connection_id: Option<ConnectionId>,
    ) -> Result<EffectAuditAuthorityGrant> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let run = state
            .runs
            .get(&run_id)
            .context("Brain run does not exist")?;
        anyhow::ensure!(
            !run.status.is_terminal(),
            "terminal Brain run has no effect authority"
        );
        let lease = state
            .runner_lease
            .as_ref()
            .context("Brain has no runner lease")?;
        anyhow::ensure!(
            lease.lease_id == lease_id
                && lease.environment_generation == self.environment.generation
                && lease.expires_ms > unix_millis(),
            "runner lease is stale or belongs to a successor"
        );
        let authority_name = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            state.brain_id.0,
            run_id.0,
            run.request_seq,
            lease_id.0,
            lease.subject,
            self.environment.generation,
        );
        Ok(EffectAuditAuthorityGrant {
            brain: name.to_string(),
            run_id,
            authority: crate::runtime::effect_log::EffectAuditAuthority {
                authority_id: uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    authority_name.as_bytes(),
                ),
                runner_lease_id: lease_id.0,
                runner_subject: lease.subject.clone(),
                // Connection generations are enforced by the live reverse
                // capability, not persisted in replay identity. A lost
                // reserve response may be retried after reconnect without
                // minting a conflicting durable authority.
                connection_id: None,
                environment_generation: self.environment.generation,
            },
        })
    }

    /// Resolve every still-open intent owned by one completed or disconnected
    /// runner request. An unbegun reservation is known not to have reached a
    /// physical binding; a begun permit is conservatively uncertain.
    pub(crate) fn reconcile_effect_audit_authority(
        &self,
        grant: &EffectAuditAuthorityGrant,
    ) -> Result<usize> {
        self.terminalize_effect_audit_authority(grant, true)
    }

    /// Durably reconcile every unresolved audit whose exact runner lease was
    /// owned by a transport connection that has terminated. The caller keeps
    /// those lease claims fenced until this single per-Brain batch succeeds.
    pub(crate) fn reconcile_effect_audits_for_disconnected_leases(
        &self,
        name: &str,
        lease_ids: &[RunnerLeaseId],
    ) -> Result<usize> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let lease_ids = lease_ids
            .iter()
            .map(|lease_id| lease_id.0)
            .collect::<std::collections::BTreeSet<_>>();
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let unresolved = state
            .effect_audits
            .entries()
            .values()
            .filter_map(|entry| {
                (lease_ids.contains(&entry.authority.runner_lease_id)
                    && entry.authority.environment_generation == self.environment.generation)
                    .then(|| match &entry.state {
                        crate::runtime::effect_log::EffectAuditState::IntentAccepted => Some((
                            entry.intent.identity,
                            entry.authority.authority_id,
                            crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied,
                        )),
                        crate::runtime::effect_log::EffectAuditState::AwaitingHostResult => Some((
                            entry.intent.identity,
                            entry.authority.authority_id,
                            crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss,
                        )),
                        crate::runtime::effect_log::EffectAuditState::Terminal { .. } => None,
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let transitions = unresolved
            .iter()
            .map(|(identity, authority_id, outcome)| {
                crate::runtime::effect_log::EffectAuditTransition::Finish {
                    identity: *identity,
                    authority_id: *authority_id,
                    outcome: outcome.clone(),
                }
            })
            .collect::<Vec<_>>();
        self.append_effect_audit_transition_batch_locked(name, state, transitions)?;
        Ok(unresolved.len())
    }

    /// A normally returned runner can prove that accepted-but-unbegun
    /// reservations were never dispatched. Begun effects retain their
    /// detached permit so one late authoritative completion may still land.
    pub(crate) fn abandon_unbegun_effect_audits(
        &self,
        grant: &EffectAuditAuthorityGrant,
    ) -> Result<usize> {
        self.terminalize_effect_audit_authority(grant, false)
    }

    fn terminalize_effect_audit_authority(
        &self,
        grant: &EffectAuditAuthorityGrant,
        process_lost: bool,
    ) -> Result<usize> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let unresolved = state.effect_audits.entries().values().filter_map(|entry| {
            (entry.intent.identity.run_id == grant.run_id.0
                && entry.authority.authority_id == grant.authority.authority_id)
                .then(|| match &entry.state {
                    crate::runtime::effect_log::EffectAuditState::IntentAccepted => Some((
                        entry.intent.identity,
                        crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied,
                    )),
                    crate::runtime::effect_log::EffectAuditState::AwaitingHostResult
                        if process_lost => Some((entry.intent.identity,
                            crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss)),
                    crate::runtime::effect_log::EffectAuditState::AwaitingHostResult => None,
                    crate::runtime::effect_log::EffectAuditState::Terminal { .. } => None,
                })
                .flatten()
        }).collect::<Vec<_>>();
        let transitions = unresolved
            .iter()
            .map(
                |(identity, outcome)| crate::runtime::effect_log::EffectAuditTransition::Finish {
                    identity: *identity,
                    authority_id: grant.authority.authority_id,
                    outcome: outcome.clone(),
                },
            )
            .collect::<Vec<_>>();
        self.append_effect_audit_transition_batch_locked(name, state, transitions)?;
        Ok(unresolved.len())
    }

    /// Resolve an exact durable reserve retry before applying current callback,
    /// lease, or run-terminal checks. A changed intent or authority fails
    /// closed; absence returns `None` and grants no authority for new work.
    pub(crate) fn retry_effect_audit_reservation(
        &self,
        grant: &EffectAuditAuthorityGrant,
        execution_id: uuid::Uuid,
        effect: &crate::vm::VmSideEffect,
    ) -> Result<Option<crate::runtime::effect_log::EffectAuditIdentity>> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let run = state
            .runs
            .get(&grant.run_id)
            .context("Brain run does not exist")?;
        let identity = crate::runtime::effect_log::EffectAuditIdentity {
            brain_id: state.brain_id.0,
            run_id: grant.run_id.0,
            request_seq: run.request_seq,
            execution_id,
            effect_sequence: effect.sequence,
        };
        let transition = crate::runtime::effect_log::EffectAuditTransition::Reserve {
            intent: crate::runtime::effect_log::EffectAuditIntent::from_effect(identity, effect)?,
            authority: grant.authority.clone(),
        };
        if state.effect_audits.get(&identity).is_some() {
            anyhow::ensure!(
                !state.effect_audits.validate(&transition)?,
                "existing effect audit reservation unexpectedly changed state"
            );
            return Ok(Some(identity));
        }
        if let Some(Some(fence)) =
            self.with_effect_audit_storage_mut(name, state.brain_id, |storage| {
                storage.replay.lookup(&identity)
            })?
        {
            let mut archived = crate::runtime::effect_log::EffectAuditReducer::default();
            archived.apply(fence)?;
            anyhow::ensure!(
                !archived.validate(&transition)?,
                "archived effect audit reservation unexpectedly changed state"
            );
            return Ok(Some(identity));
        }
        Ok(None)
    }

    /// Durably accept an effect intent without permitting physical application.
    pub(crate) fn reserve_effect_audit(
        &self,
        grant: &EffectAuditAuthorityGrant,
        execution_id: uuid::Uuid,
        effect: crate::vm::VmSideEffect,
    ) -> Result<crate::runtime::effect_log::EffectAuditIdentity> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let run = state
            .runs
            .get(&grant.run_id)
            .context("Brain run does not exist")?;
        let identity = crate::runtime::effect_log::EffectAuditIdentity {
            brain_id: state.brain_id.0,
            run_id: grant.run_id.0,
            request_seq: run.request_seq,
            execution_id,
            effect_sequence: effect.sequence,
        };
        let transition = crate::runtime::effect_log::EffectAuditTransition::Reserve {
            intent: crate::runtime::effect_log::EffectAuditIntent::from_effect(identity, &effect)?,
            authority: grant.authority.clone(),
        };
        // A caller that lost the original durable ACK may retry its exact
        // reservation after disconnect, terminalization, or daemon reload.
        // Compare complete canonical intent and daemon-issued authority before
        // rejecting new work under the current run/lease state. Conflicting
        // reuse still fails closed in the reducer.
        if state.effect_audits.get(&identity).is_some() {
            anyhow::ensure!(
                !state.effect_audits.validate(&transition)?,
                "existing effect audit reservation unexpectedly changed state"
            );
            return Ok(identity);
        }
        if let Some(Some(fence)) =
            self.with_effect_audit_storage_mut(name, state.brain_id, |storage| {
                storage.replay.lookup(&identity)
            })?
        {
            let mut archived = crate::runtime::effect_log::EffectAuditReducer::default();
            archived.apply(fence)?;
            anyhow::ensure!(
                !archived.validate(&transition)?,
                "archived effect audit reservation unexpectedly changed state"
            );
            return Ok(identity);
        }
        self.validate_effect_grant_for_new_work(state, grant)?;
        self.append_effect_audit_transition_locked(name, state, transition)?;
        Ok(identity)
    }

    /// Durably commit `AwaitingHostResult` before issuing a physical permit.
    pub(crate) fn begin_effect_audit(
        &self,
        grant: &EffectAuditAuthorityGrant,
        identity: crate::runtime::effect_log::EffectAuditIdentity,
    ) -> Result<crate::runtime::effect_log::HostEffectPermit> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        self.validate_effect_grant_for_new_work(state, grant)?;
        anyhow::ensure!(
            identity.brain_id == state.brain_id.0 && identity.run_id == grant.run_id.0,
            "effect audit identity does not belong to this authority"
        );
        anyhow::ensure!(
            state
                .effect_audits
                .get(&identity)
                .is_some_and(|entry| matches!(
                    entry.state,
                    crate::runtime::effect_log::EffectAuditState::IntentAccepted
                )),
            "effect audit reservation was already begun or terminalized"
        );
        self.append_effect_audit_transition_locked(
            name,
            state,
            crate::runtime::effect_log::EffectAuditTransition::Begin {
                identity,
                authority_id: grant.authority.authority_id,
            },
        )?;
        Ok(crate::runtime::effect_log::HostEffectPermit::new(
            identity,
            grant.authority.authority_id,
        ))
    }

    /// Record a monotonic outcome. Current lease state is deliberately
    /// irrelevant to the original detached owner's already-begun effect.
    pub(crate) fn finish_effect_audit(
        &self,
        grant: &EffectAuditAuthorityGrant,
        permit: Option<&crate::runtime::effect_log::HostEffectPermit>,
        identity: crate::runtime::effect_log::EffectAuditIdentity,
        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome,
    ) -> Result<()> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        anyhow::ensure!(
            identity.run_id == grant.run_id.0,
            "effect audit identity does not belong to this authority"
        );
        if let Some(permit) = permit {
            anyhow::ensure!(
                permit.identity() == identity
                    && permit.authority_id() == grant.authority.authority_id,
                "host effect permit does not match the terminal outcome"
            );
        } else {
            anyhow::ensure!(
                matches!(
                    outcome,
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::NotApplied { .. }
                ),
                "a physical host outcome requires its durable permit"
            );
        }
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if permit.is_none() {
            anyhow::ensure!(
                state
                    .effect_audits
                    .get(&identity)
                    .is_some_and(|entry| matches!(
                        entry.state,
                        crate::runtime::effect_log::EffectAuditState::IntentAccepted
                    )),
                "begun effect outcome requires its durable host permit"
            );
        }
        self.append_effect_audit_transition_locked(
            name,
            state,
            crate::runtime::effect_log::EffectAuditTransition::Finish {
                identity,
                authority_id: grant.authority.authority_id,
                outcome,
            },
        )?;
        Ok(())
    }

    fn validate_effect_grant_for_new_work(
        &self,
        state: &BrainState,
        grant: &EffectAuditAuthorityGrant,
    ) -> Result<()> {
        let run = state
            .runs
            .get(&grant.run_id)
            .context("Brain run does not exist")?;
        anyhow::ensure!(
            !run.status.is_terminal(),
            "terminal Brain run cannot begin a host effect"
        );
        let lease = state
            .runner_lease
            .as_ref()
            .context("Brain has no runner lease")?;
        anyhow::ensure!(
            lease.lease_id.0 == grant.authority.runner_lease_id
                && lease.subject == grant.authority.runner_subject
                && lease.environment_generation == grant.authority.environment_generation
                && lease.expires_ms > unix_millis(),
            "effect audit authority is stale or belongs to a successor runner"
        );
        Ok(())
    }

    fn append_effect_audit_transition_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        transition: crate::runtime::effect_log::EffectAuditTransition,
    ) -> Result<bool> {
        if !state.effect_audits.validate(&transition)? {
            self.compact_terminal_effect_audits_locked(
                name,
                state,
                MAX_RETAINED_TERMINAL_EFFECT_AUDITS,
            )?;
            return Ok(false);
        }
        let event = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: state.revision + 1,
            environment_generation: self.environment.generation,
            sender: "daemon/effect-audit".into(),
            created_ms: unix_millis(),
            run_id: Some(RunId(transition.identity().run_id)),
            mutation: None,
            kind: BrainEventKind::EffectAuditTransition { transition },
        };
        if self.root.is_some() {
            self.with_effect_audit_storage_mut(name, state.brain_id, |storage| {
                let BrainEventKind::EffectAuditTransition { transition } = &event.kind else {
                    unreachable!("effect-audit append constructed a non-audit event");
                };
                if matches!(
                    transition,
                    crate::runtime::effect_log::EffectAuditTransition::Reserve { .. }
                ) {
                    storage.active.ensure_reserve_capacity(
                        &storage.replay,
                        serde_json::to_vec(transition)?.len(),
                    )?;
                }
                storage.active.append(event.seq, transition)
            })?;
        } else {
            self.append_event(name, &event)?;
        }
        state.apply(event.clone());
        if self.root.is_some() {
            state.events.pop();
            let BrainEventKind::EffectAuditTransition { transition } = &event.kind else {
                unreachable!("effect-audit append constructed a non-audit event");
            };
            self.archive_terminal_effect_audits_batch_locked(
                name,
                state,
                &[(event.seq, transition.identity())],
            )?;
        } else {
            self.compact_terminal_effect_audits_locked(
                name,
                state,
                MAX_RETAINED_TERMINAL_EFFECT_AUDITS,
            )?;
        }
        let _ = state.tx.send(observer_effect_audit_event(&event));
        Ok(true)
    }

    fn append_effect_audit_transition_batch_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        transitions: Vec<crate::runtime::effect_log::EffectAuditTransition>,
    ) -> Result<usize> {
        let mut events = Vec::new();
        let mut seen = HashSet::new();
        let mut next_seq = state.revision + 1;
        for transition in transitions {
            anyhow::ensure!(
                seen.insert(transition.identity()),
                "duplicate effect audit identity in one durable batch"
            );
            if !state.effect_audits.validate(&transition)? {
                continue;
            }
            events.push(BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: state.brain_id,
                seq: next_seq,
                environment_generation: self.environment.generation,
                sender: "daemon/effect-audit-reconcile".into(),
                created_ms: unix_millis(),
                run_id: Some(RunId(transition.identity().run_id)),
                mutation: None,
                kind: BrainEventKind::EffectAuditTransition { transition },
            });
            next_seq += 1;
        }
        if events.is_empty() {
            return Ok(0);
        }
        if self.root.is_some() {
            self.with_effect_audit_storage_mut(name, state.brain_id, |storage| {
                let transitions = events
                    .iter()
                    .map(|event| {
                        let BrainEventKind::EffectAuditTransition { transition } = &event.kind
                        else {
                            unreachable!("effect-audit batch constructed a non-audit event");
                        };
                        (event.seq, transition.clone())
                    })
                    .collect::<Vec<_>>();
                storage.active.append_batch(&transitions)
            })?;
        } else {
            self.append_event_batch(name, &events)?;
        }
        let mut terminal = Vec::new();
        for event in &events {
            state.apply(event.clone());
            if self.root.is_some() {
                state.events.pop();
                let BrainEventKind::EffectAuditTransition { transition } = &event.kind else {
                    unreachable!("effect-audit batch constructed a non-audit event");
                };
                if state
                    .effect_audits
                    .get(&transition.identity())
                    .is_some_and(|entry| entry.state.is_terminal())
                {
                    terminal.push((event.seq, transition.identity()));
                }
            }
            let _ = state.tx.send(observer_effect_audit_event(event));
        }
        if self.root.is_some() {
            self.archive_terminal_effect_audits_batch_locked(name, state, &terminal)?;
        }
        if self.root.is_none() {
            self.compact_terminal_effect_audits_locked(
                name,
                state,
                MAX_RETAINED_TERMINAL_EFFECT_AUDITS,
            )?;
        }
        Ok(events.len())
    }

    fn archive_terminal_effect_audits_batch_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        terminal: &[(u64, crate::runtime::effect_log::EffectAuditIdentity)],
    ) -> Result<()> {
        let mut fences = Vec::new();
        let mut observers = Vec::new();
        for (seq, identity) in terminal {
            let Some(entry) = state.effect_audits.get(identity) else {
                continue;
            };
            if !entry.state.is_terminal() {
                continue;
            }
            observers.push((*identity, entry.observer_projection()));
            fences.push((
                *seq,
                crate::runtime::effect_log::replay_fence_transition(entry)?,
            ));
        }
        if fences.is_empty() {
            return Ok(());
        }
        let identities = observers
            .iter()
            .map(|(identity, _)| *identity)
            .collect::<Vec<_>>();
        self.with_effect_audit_storage_mut(name, state.brain_id, |storage| {
            let active_bytes = storage.active.file_bytes()?;
            storage.replay.append_fences(&fences, active_bytes)?;
            storage.active.remove_identities(&identities)
        })?;
        for (identity, observer) in observers {
            state.effect_audits.forget_archived(&identity)?;
            state.recent_effect_audits.push_back(observer);
        }
        while state.recent_effect_audits.len() > MAX_RETAINED_TERMINAL_EFFECT_AUDITS {
            state.recent_effect_audits.pop_front();
        }
        Ok(())
    }

    fn compact_terminal_effect_audits_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        retained_limit: usize,
    ) -> Result<()> {
        let removed = state
            .effect_audits
            .terminal_compaction_candidates(retained_limit)
            .into_iter()
            .collect::<HashSet<_>>();
        if removed.is_empty() {
            return Ok(());
        }
        let mut retained_events = Vec::with_capacity(state.events.len());
        for event in &state.events {
            let BrainEventKind::EffectAuditTransition { transition } = &event.kind else {
                retained_events.push(event.clone());
                continue;
            };
            if !removed.contains(&transition.identity()) {
                retained_events.push(event.clone());
                continue;
            }
            match transition {
                crate::runtime::effect_log::EffectAuditTransition::Reserve { .. } => {}
                crate::runtime::effect_log::EffectAuditTransition::Begin { .. } => {}
                crate::runtime::effect_log::EffectAuditTransition::Finish { identity, .. } => {
                    let mut compacted = event.clone();
                    let entry = state
                        .effect_audits
                        .get(identity)
                        .context("terminal effect audit disappeared during compaction")?;
                    compacted.kind = BrainEventKind::EffectAuditTransition {
                        transition: crate::runtime::effect_log::replay_fence_transition(entry)?,
                    };
                    anyhow::ensure!(serde_json::to_vec(&compacted)?.len()
                            <= crate::runtime::effect_log::MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES,
                        "effect audit replay fence event exceeds its fixed encoded bound");
                    retained_events.push(compacted);
                }
                crate::runtime::effect_log::EffectAuditTransition::Fence { .. } => {
                    retained_events.push(event.clone())
                }
            }
        }

        self.rewrite_events(name, &retained_events)?;
        for identity in &removed {
            state.effect_audits.compact_terminal(identity)?;
        }
        state.events = retained_events;
        Ok(())
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
        self.schedule_initialization_with_receipt(
            name,
            initiating_attachment_id,
            connection_id,
            next_due_ms,
            None,
        )
    }

    pub fn schedule_initialization_with_receipt(
        &self,
        name: &str,
        initiating_attachment_id: AttachmentId,
        connection_id: ConnectionId,
        next_due_ms: u64,
        mutation: Option<BrainMutationReceipt>,
    ) -> Result<BrainSchedule> {
        let name = Self::validate_name(name)?;
        let initialization = self.initialization(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(receipt) = mutation.as_ref() {
            if let Some(existing) = state.events.iter().find(|event| {
                event.mutation.as_ref().is_some_and(|recorded| {
                    recorded.attachment_id == receipt.attachment_id
                        && recorded.mutation_id == receipt.mutation_id
                })
            }) {
                anyhow::ensure!(existing.mutation.as_ref() == Some(receipt),
                    "Brain mutation idempotency key was reused with a different command or precondition");
                let BrainEventKind::ScheduleChanged { schedule } = &existing.kind else {
                    anyhow::bail!("replayed mutation outcome is not an initialization schedule");
                };
                anyhow::ensure!(
                    schedule.module_identity.is_some(),
                    "replayed schedule is not Brain initialization"
                );
                return Ok(schedule.clone());
            }
        }
        let attachment = state
            .attachments
            .get(&initiating_attachment_id)
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
        let attempt_ids = state
            .events
            .iter()
            .rev()
            .filter_map(|event| match &event.kind {
                BrainEventKind::ScheduleChanged { schedule }
                    if schedule.module_identity.as_ref() == Some(&module_identity)
                        && seen_attempts.insert(schedule.schedule_id) =>
                {
                    Some(schedule.schedule_id)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for schedule_id in attempt_ids {
            let existing = state
                .schedules
                .get(&schedule_id)
                .expect("module schedule event was projected")
                .clone();
            initialization.validate_schedule(&existing)?;
            if existing.active {
                if let Some(receipt) = mutation {
                    self.push_idempotent_locked(
                        name,
                        state,
                        &sender,
                        BrainEventKind::ScheduleChanged {
                            schedule: existing.clone(),
                        },
                        receipt,
                    )?;
                }
                return Ok(existing);
            }
            let run_status = state
                .events
                .iter()
                .rev()
                .find_map(|event| match &event.kind {
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
                if let Some(receipt) = mutation {
                    self.push_idempotent_locked(
                        name,
                        state,
                        &sender,
                        BrainEventKind::ScheduleChanged {
                            schedule: existing.clone(),
                        },
                        receipt,
                    )?;
                }
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
        let kind = BrainEventKind::ScheduleChanged {
            schedule: schedule.clone(),
        };
        match mutation {
            Some(receipt) => {
                self.push_idempotent_locked(name, state, &sender, kind, receipt)?;
            }
            None => {
                self.push_locked(name, state, &sender, kind)?;
            }
        }
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
        self.create_schedule_with_receipt(
            name,
            created_by,
            initiating_attachment_id,
            language,
            source.into(),
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule_with_receipt(
        &self,
        name: &str,
        created_by: &str,
        initiating_attachment_id: AttachmentId,
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
        mutation: Option<BrainMutationReceipt>,
    ) -> Result<BrainSchedule> {
        let name = Self::validate_name(name)?;
        let created_by = validate_participant_subject("schedule creator", created_by)?;
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
                anyhow::bail!("bounded catch-up requires a positive backlog bound and expiry");
            }
        }
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&initiating_attachment_id)
            .context("schedule creator attachment does not exist")?;
        if attachment.subject != created_by {
            anyhow::bail!("schedule creator does not own the initiating attachment");
        }
        if !matches!(
            attachment.role,
            AttachmentRole::Runner | AttachmentRole::Driver
        ) {
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
        let kind = BrainEventKind::ScheduleChanged {
            schedule: schedule.clone(),
        };
        match mutation {
            Some(receipt) => {
                let appended =
                    self.push_idempotent_locked(name, state, created_by, kind, receipt)?;
                match appended.event.kind {
                    BrainEventKind::ScheduleChanged { schedule } => Ok(schedule),
                    _ => anyhow::bail!("Brain mutation receipt does not identify a schedule"),
                }
            }
            None => {
                self.push_locked(name, state, created_by, kind)?;
                Ok(schedule)
            }
        }
    }

    /// Atomically advance due schedules and append the exact queued ProgramRun
    /// for each delivery. The returned runs are durable before this method
    /// returns and are safe for the runner broker to dispatch immediately.
    /// Bring the due index into line with one Brain's current schedules.
    ///
    /// Called wherever a schedule is created, cancelled, or advanced, and when
    /// a Brain becomes resident. Takes the state the caller already holds, so
    /// it never hydrates anything itself.
    fn reindex_schedules_locked(&self, name: &str, state: &BrainState) {
        let earliest_before = {
            let index = self
                .schedule_index
                .read()
                .expect("schedule index lock poisoned");
            index.next_due_ms()
        };
        let earliest_after = {
            let mut index = self
                .schedule_index
                .write()
                .expect("schedule index lock poisoned");
            index.reindex(name, &state.schedules);
            index.next_due_ms()
        };
        // Only wake the delivery loop when the head actually moved earlier.
        // A schedule created far in the future must not interrupt a sleep it
        // does not shorten.
        let moved_earlier = match (earliest_before, earliest_after) {
            (Some(before), Some(after)) => after < before,
            (None, Some(_)) => true,
            _ => false,
        };
        if moved_earlier {
            self.schedule_wakeup.notify_waiters();
        }
    }

    /// Drop everything the index knows about a Brain that no longer exists.
    fn forget_schedules_locked(&self, name: &str) {
        self.schedule_index
            .write()
            .expect("schedule index lock poisoned")
            .forget(name);
    }

    /// When the earliest active schedule in the store next comes due.
    ///
    /// `None` means nothing is scheduled, and the delivery loop may wait for
    /// [`BrainStore::schedule_wakeup`] rather than polling.
    pub fn next_schedule_due_ms(&self) -> Option<u64> {
        self.schedule_index
            .read()
            .expect("schedule index lock poisoned")
            .next_due_ms()
    }

    /// The Brains holding work due at or before `now_ms`, in due order.
    ///
    /// This is the whole point of the index: it answers which Brains to hydrate
    /// without hydrating any of them. Selecting by Brain instead meant
    /// enumerating and replaying the entire store once a second to discover
    /// that nothing was due (#374).
    pub fn due_schedule_brains(&self, now_ms: u64) -> Vec<String> {
        self.schedule_index
            .read()
            .expect("schedule index lock poisoned")
            .due_brains(now_ms)
    }

    /// Populate the due index from every Brain on disk, once.
    ///
    /// Schedules only become known when a Brain is loaded, so a freshly started
    /// daemon has an empty index and would deliver nothing. This is the one
    /// remaining full enumeration; after it, selection comes from the index and
    /// only Brains with due work are hydrated (#374).
    ///
    /// A Brain that cannot be replayed is skipped rather than aborting the
    /// warm-up, so one unreadable Brain cannot leave every other Brain
    /// unscheduled. That is deliberately the minimum needed for the index to
    /// exist: the real discovery and retry semantics -- naming the failure,
    /// bounding its diagnostics, and picking a Brain up again once repaired --
    /// belong to #371, and this should consume that API rather than keep its
    /// own once it lands.
    pub fn warm_schedule_index(&self) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            // No Brain root yet is not a failure; it is a daemon with no Brains.
            Err(_) => return Ok(()),
        };
        let mut skipped = 0usize;
        for entry in entries.flatten() {
            if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if Self::validate_name(&name).is_err() {
                continue;
            }
            if let Err(error) = self.ensure_loaded(&name) {
                skipped += 1;
                tracing::warn!(
                    brain = %name,
                    %error,
                    "Brain could not be loaded while warming the schedule index; its \
                     schedules will not be delivered until it is repaired"
                );
            }
        }
        if skipped > 0 {
            tracing::warn!(
                skipped,
                indexed = self.indexed_schedule_count(),
                "some Brains were skipped while warming the schedule index"
            );
        }
        Ok(())
    }

    /// Woken when a schedule appears that is due sooner than the current head.
    pub fn schedule_wakeup(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.schedule_wakeup)
    }

    /// How many active schedules the index is tracking, for tests and
    /// diagnostics. Counts entries, never contents.
    pub fn indexed_schedule_count(&self) -> usize {
        self.schedule_index
            .read()
            .expect("schedule index lock poisoned")
            .due
            .len()
    }

    pub fn queue_due_schedules(&self, name: &str, now_ms: u64) -> Result<Vec<BrainRun>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
                    matches!(
                        run.status,
                        BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
                    )
                })
            }) {
                continue;
            }

            let (occurrence_count, last_due_ms, next_due_ms) =
                schedule_due_window(&schedule, now_ms)?;
            match &schedule.delivery_policy {
                BrainScheduleDeliveryPolicy::Coalesce => {
                    if let Some(existing) = pending.into_iter().find(|due| {
                        state
                            .runs
                            .get(&due.run.run_id)
                            .is_some_and(|run| run.status == BrainRunStatus::QueuedForEnvironment)
                    }) {
                        let event_seq = state.revision + 1;
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
                            missed_count: existing.missed_count.saturating_add(occurrence_count),
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
        self.cancel_schedule_with_receipt(
            name,
            cancelled_by,
            initiating_attachment_id,
            schedule_id,
            None,
        )
    }

    pub fn cancel_schedule_with_receipt(
        &self,
        name: &str,
        cancelled_by: &str,
        initiating_attachment_id: AttachmentId,
        schedule_id: ScheduleId,
        receipt: Option<BrainMutationReceipt>,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        let cancelled_by = validate_participant_subject("schedule canceller", cancelled_by)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(receipt) = receipt.as_ref() {
            if let Some(existing) = state.events.iter().find(|event| {
                event.mutation.as_ref().is_some_and(|recorded| {
                    recorded.attachment_id == receipt.attachment_id
                        && recorded.mutation_id == receipt.mutation_id
                })
            }) {
                anyhow::ensure!(existing.mutation.as_ref() == Some(receipt),
                    "Brain mutation idempotency key was reused with a different command or precondition");
                return match &existing.kind {
                    BrainEventKind::ScheduleChanged { schedule }
                        if schedule.schedule_id == schedule_id && !schedule.active =>
                    {
                        Ok(true)
                    }
                    BrainEventKind::MutationRecorded {
                        outcome:
                            BrainMutationOutcome::ScheduleCancellationNoop {
                                schedule_id: recorded,
                            },
                    } if *recorded == schedule_id => Ok(false),
                    _ => anyhow::bail!(
                        "replayed mutation outcome does not match schedule cancellation"
                    ),
                };
            }
        }
        let Some(mut schedule) = state.schedules.get(&schedule_id).cloned() else {
            if let Some(receipt) = receipt {
                self.push_idempotent_locked(
                    name,
                    state,
                    cancelled_by,
                    BrainEventKind::MutationRecorded {
                        outcome: BrainMutationOutcome::ScheduleCancellationNoop { schedule_id },
                    },
                    receipt,
                )?;
            }
            return Ok(false);
        };
        if !schedule.active {
            if let Some(receipt) = receipt {
                self.push_idempotent_locked(
                    name,
                    state,
                    cancelled_by,
                    BrainEventKind::MutationRecorded {
                        outcome: BrainMutationOutcome::ScheduleCancellationNoop { schedule_id },
                    },
                    receipt,
                )?;
            }
            return Ok(false);
        }
        if schedule.created_by != cancelled_by
            || schedule.initiating_attachment_id != initiating_attachment_id
        {
            anyhow::bail!("only the schedule creator attachment may cancel this schedule");
        }
        schedule.active = false;
        let kind = BrainEventKind::ScheduleChanged { schedule };
        match receipt {
            Some(receipt) => {
                self.push_idempotent_locked(name, state, cancelled_by, kind, receipt)?;
            }
            None => {
                self.push_locked(name, state, cancelled_by, kind)?;
            }
        }
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

    /// Atomically allocate and journal an explicit speculative request and its
    /// queued run under the aggregate lock. The request event carries the RunId
    /// from its first durable appearance.
    pub fn accept_speculative_run(
        &self,
        name: &str,
        sender: &str,
        initiating_attachment_id: AttachmentId,
        text: String,
    ) -> Result<(BrainEvent, BrainRun)> {
        let name = Self::validate_name(name)?;
        let sender = validate_participant_subject("run initiator", sender)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let run_id = RunId::new();
        let accepted = self.push_locked_for_run(
            name,
            state,
            sender,
            Some(run_id),
            BrainEventKind::SpeculativePrompt { text },
        )?;
        let now = unix_millis();
        let run = BrainRun {
            run_id,
            kind: BrainRunKind::Speculative,
            parent_run_id: None,
            request_seq: accepted.seq,
            initiating_attachment_id,
            initiated_by: sender.to_string(),
            status: BrainRunStatus::QueuedForEnvironment,
            started_ms: now,
            updated_ms: now,
            detail: None,
        };
        self.push_locked_for_run(
            name,
            state,
            sender,
            Some(run_id),
            BrainEventKind::RunStarted { run: run.clone() },
        )?;
        Ok((accepted, run))
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        self.push_locked_for_run(
            name,
            state,
            sender,
            Some(run_id),
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let current = state
            .runs
            .get(&run_id)
            .with_context(|| format!("Brain run {} does not exist", run_id.0))?;
        validate_run_transition(current.status, status)?;
        self.push_locked_for_run(
            name,
            state,
            sender,
            Some(run_id),
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            },
        )?;
        let transitioned = state
            .runs
            .get(&run_id)
            .expect("run transition was projected")
            .clone();
        drop(brains);
        if status.is_terminal() {
            let gate = self
                .run_publication_gates
                .read()
                .expect("shared Brain run-gate map poisoned")
                .get(&(name.to_string(), run_id))
                .cloned();
            let idle = gate
                .as_ref()
                .and_then(|gate| gate.try_lock().ok())
                .is_some_and(|gate| !gate.cancel_requested());
            if idle {
                self.prune_run_publication(name, run_id)?;
            }
        }
        Ok(transitioned)
    }

    pub(crate) fn terminalize_run_with_result_if_active(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        request_seq: u64,
        status: BrainRunStatus,
        detail: String,
    ) -> Result<Option<BrainEvent>> {
        anyhow::ensure!(
            status.is_terminal(),
            "run terminalization requires a terminal status"
        );
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let publication = match self.run_publication_gate(name, run_id)?.try_lock_owned() {
            Ok(publication) => publication,
            Err(_) => return Ok(None),
        };
        if publication.cancel_requested() || self.run_cancellation_reserved(name, run_id)? {
            return Ok(None);
        }
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let current = state
            .runs
            .get(&run_id)
            .with_context(|| format!("Brain run {} does not exist", run_id.0))?;
        if current.status.is_terminal() {
            self.clear_disconnect_intent(name, run_id)?;
            return Ok(None);
        }
        validate_run_transition(current.status, status)?;
        let intent = DisconnectTerminalizationIntent {
            sender: sender.to_string(),
            run_id,
            request_seq,
            status,
            detail: detail.clone(),
        };
        self.persist_disconnect_intent(name, &intent)?;
        let existing = state
            .events
            .iter()
            .find(|event| {
                event.run_id == Some(run_id) && matches!(event.kind, BrainEventKind::Result { .. })
            })
            .cloned();
        let next_seq = state.revision + 1;
        let now = unix_millis();
        let result = existing.unwrap_or_else(|| BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: next_seq,
            environment_generation: self.environment.generation,
            sender: sender.to_string(),
            created_ms: now,
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::Result {
                request_seq,
                output: String::new(),
                error: Some(detail.clone()),
                continuation_messages: Vec::new(),
                invocation_metadata: None,
            },
        });
        let result_is_durable = state.events.iter().any(|event| event.seq == result.seq);
        let terminal = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: next_seq + u64::from(!result_is_durable),
            environment_generation: self.environment.generation,
            sender: sender.to_string(),
            created_ms: now,
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail: Some(detail),
            },
        };
        let events = if result_is_durable {
            vec![terminal]
        } else {
            vec![result.clone(), terminal]
        };
        self.append_event_batch(name, &events)?;
        for event in events {
            state.apply(event.clone());
            let _ = state.tx.send(event);
        }
        self.clear_disconnect_intent(name, run_id)?;
        drop(brains);
        drop(publication);
        self.prune_run_publication(name, run_id)?;
        Ok(Some(result))
    }

    pub(crate) fn begin_run_approval_for_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        request_seq: u64,
        approval_id: String,
        approval_kind: String,
        subject: String,
        audience: BrainApprovalAudience,
        detail: serde_json::Value,
    ) -> Result<RunId> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        anyhow::ensure!(
            attachment.connected && attachment.connection_id == Some(connection_id),
            "approval audience connection is no longer current"
        );
        let run_id = state
            .runs
            .values()
            .find(|run| run.request_seq == request_seq && run.status == BrainRunStatus::Running)
            .map(|run| run.run_id)
            .context("approval request does not address a running Brain run")?;
        anyhow::ensure!(
            !Self::state_has_run_cancellation_reservation(state, run_id),
            "named Brain run cancelled"
        );
        self.push_locked_for_run(
            name,
            state,
            "daemon",
            Some(run_id),
            BrainEventKind::RunStatusChanged {
                run_id,
                status: BrainRunStatus::AwaitingApproval,
                detail: Some(format!("awaiting approval {approval_id}")),
            },
        )?;
        if approval_kind == "tool" {
            let input = detail
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            self.push_locked_for_run(
                name,
                state,
                "provider",
                Some(run_id),
                BrainEventKind::ToolCall {
                    request_seq,
                    tool_id: approval_id.clone(),
                    name: subject.clone(),
                    input,
                },
            )?;
        }
        self.push_locked_for_run(
            name,
            state,
            "runner",
            Some(run_id),
            BrainEventKind::ApprovalRequested {
                request_seq,
                approval_id,
                approval_kind,
                subject,
                audience: Some(audience),
                detail,
            },
        )?;
        Ok(run_id)
    }

    pub async fn reserve_run_cancellation(
        &self,
        name: &str,
        sender: &str,
        initiating_attachment_id: AttachmentId,
        run_id: RunId,
        receipt: BrainMutationReceipt,
    ) -> Result<BrainRunCancellationReservation> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut publication = self.acquire_run_publication(name, run_id).await?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(existing) = state.events.iter().find(|event| {
            event.mutation.as_ref().is_some_and(|recorded| {
                recorded.attachment_id == receipt.attachment_id
                    && recorded.mutation_id == receipt.mutation_id
            })
        }) {
            anyhow::ensure!(existing.mutation.as_ref() == Some(&receipt),
                "Brain mutation idempotency key was reused with a different command or precondition");
            let needs_runner_cancel = match &existing.kind {
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationReserved { run_id: recorded },
                } if *recorded == run_id => {
                    !state.events.iter().any(|event| {
                        matches!(
                            &event.kind,
                            BrainEventKind::MutationRecorded {
                                outcome: BrainMutationOutcome::RunCancellationReconciled {
                                    run_id: progress_run, mutation_id,
                                },
                            } if *progress_run == run_id && *mutation_id == receipt.mutation_id
                        )
                    }) && state
                        .runs
                        .get(&run_id)
                        .is_some_and(|run| run.status != BrainRunStatus::Cancelled)
                }
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunAlreadyCancelled { run_id: recorded },
                } if *recorded == run_id => false,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationNoop { run_id: recorded },
                } if *recorded == run_id => anyhow::bail!(
                    "Brain run {} does not exist or has already finished",
                    run_id.0
                ),
                _ => anyhow::bail!("replayed mutation outcome does not match run cancellation"),
            };
            let run = state
                .runs
                .get(&run_id)
                .cloned()
                .with_context(|| format!("Brain run {} does not exist", run_id.0))?;
            return Ok(BrainRunCancellationReservation {
                run,
                needs_runner_cancel,
                replayed: true,
            });
        }
        anyhow::ensure!(
            receipt.attachment_id == initiating_attachment_id,
            "Brain mutation attachment does not match run initiator"
        );
        let Some(current) = state.runs.get(&run_id) else {
            self.push_idempotent_locked(
                name,
                state,
                sender,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationNoop { run_id },
                },
                receipt,
            )?;
            anyhow::bail!("Brain run {} does not exist", run_id.0);
        };
        anyhow::ensure!(
            current.initiating_attachment_id == initiating_attachment_id,
            "a Brain run can only be cancelled by its initiating attachment"
        );
        if current.status == BrainRunStatus::Cancelled {
            let run = current.clone();
            self.push_idempotent_locked(
                name,
                state,
                sender,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunAlreadyCancelled { run_id },
                },
                receipt,
            )?;
            return Ok(BrainRunCancellationReservation {
                run,
                needs_runner_cancel: false,
                replayed: false,
            });
        }
        if current.status.is_terminal() {
            self.push_idempotent_locked(
                name,
                state,
                sender,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationNoop { run_id },
                },
                receipt,
            )?;
            anyhow::bail!("Brain run {} has already finished", run_id.0);
        }
        validate_run_transition(current.status, BrainRunStatus::Cancelled)?;
        let run = current.clone();
        self.push_idempotent_locked(
            name,
            state,
            sender,
            BrainEventKind::MutationRecorded {
                outcome: BrainMutationOutcome::RunCancellationReserved { run_id },
            },
            receipt,
        )?;
        #[cfg(test)]
        let pause = self
            .cancellation_reservation_pause
            .lock()
            .expect("cancellation reservation test hook poisoned")
            .take();
        drop(brains);
        #[cfg(test)]
        if let Some((reached, release)) = pause {
            let _ = reached.send(());
            let _ = release.recv();
        }
        publication.cancel_requested = true;
        Ok(BrainRunCancellationReservation {
            run,
            needs_runner_cancel: true,
            replayed: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn pause_after_cancellation_reservation_for_test(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .cancellation_reservation_pause
            .lock()
            .expect("cancellation reservation test hook poisoned") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    pub fn complete_reserved_run_cancellation(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
    ) -> Result<BrainRun> {
        let current = self.inspect_run(name, run_id)?;
        if current.status == BrainRunStatus::Cancelled {
            return Ok(current);
        }
        self.transition_run(
            name,
            sender,
            run_id,
            BrainRunStatus::Cancelled,
            Some("cancelled by initiating driver".into()),
        )
    }

    pub(crate) fn pending_reserved_cancellations_for_attachment(
        &self,
        name: &str,
        attachment_id: AttachmentId,
    ) -> Result<Vec<RunId>> {
        let snapshot = self.snapshot(name)?;
        Ok(snapshot
            .runs
            .iter()
            .filter(|run| {
                run.initiating_attachment_id == attachment_id
                    && !run.status.is_terminal()
                    && snapshot.events.iter().any(|event| {
                        matches!(
                            event.kind,
                            BrainEventKind::MutationRecorded {
                                outcome: BrainMutationOutcome::RunCancellationReserved { run_id },
                            } if run_id == run.run_id
                        )
                    })
            })
            .map(|run| run.run_id)
            .collect())
    }

    pub(crate) fn complete_reserved_run_cancellation_on_disconnect(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
    ) -> Result<bool> {
        let publication = match self.run_publication_gate(name, run_id)?.try_lock_owned() {
            Ok(publication) => publication,
            Err(_) => return Ok(false),
        };
        if !self.run_cancellation_reserved(name, run_id)? {
            return Ok(false);
        }
        let current = self.inspect_run(name, run_id)?;
        if current.status.is_terminal() {
            return Ok(false);
        }
        self.complete_reserved_run_cancellation(name, sender, run_id)?;
        drop(publication);
        self.prune_run_publication(name, run_id)?;
        Ok(true)
    }

    pub fn mark_run_cancellation_dispatching(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        mutation_id: uuid::Uuid,
    ) -> Result<()> {
        self.record_run_cancellation_progress(name, sender, run_id, mutation_id, false)
    }

    pub fn mark_run_cancellation_reconciled(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        mutation_id: uuid::Uuid,
    ) -> Result<()> {
        self.record_run_cancellation_progress(name, sender, run_id, mutation_id, true)
    }

    fn record_run_cancellation_progress(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        mutation_id: uuid::Uuid,
        reconciled: bool,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        anyhow::ensure!(
            state.events.iter().any(|event| {
                event
                    .mutation
                    .as_ref()
                    .is_some_and(|receipt| receipt.mutation_id == mutation_id)
                    && matches!(event.kind, BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationReserved { run_id: recorded },
                } if recorded == run_id)
            }),
            "run cancellation reservation does not exist"
        );
        let already = state.events.iter().any(|event| {
            matches!(&event.kind,
                BrainEventKind::MutationRecorded {
                    outcome: BrainMutationOutcome::RunCancellationReconciled {
                        run_id: recorded, mutation_id: recorded_mutation,
                    },
                } if *recorded == run_id && *recorded_mutation == mutation_id
            )
        }) || (!reconciled
            && state.events.iter().any(|event| {
                matches!(&event.kind,
                    BrainEventKind::MutationRecorded {
                        outcome: BrainMutationOutcome::RunCancellationDispatching {
                            run_id: recorded, mutation_id: recorded_mutation,
                        },
                    } if *recorded == run_id && *recorded_mutation == mutation_id
                )
            }));
        if already {
            return Ok(());
        }
        let outcome = if reconciled {
            BrainMutationOutcome::RunCancellationReconciled {
                run_id,
                mutation_id,
            }
        } else {
            BrainMutationOutcome::RunCancellationDispatching {
                run_id,
                mutation_id,
            }
        };
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::MutationRecorded { outcome },
        )?;
        Ok(())
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        self.request_runner_handoff_with_receipt(
            name,
            requested_by,
            target_subject,
            expected_lease_id,
            environment_generation,
            ttl_ms,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_runner_handoff_with_receipt(
        &self,
        name: &str,
        requested_by: &str,
        target_subject: &str,
        expected_lease_id: RunnerLeaseId,
        environment_generation: u64,
        ttl_ms: u64,
        mutation: Option<BrainMutationReceipt>,
    ) -> Result<BrainRunnerHandoff> {
        let name = Self::validate_name(name)?;
        let requested_by = validate_participant_subject("handoff requester", requested_by)?;
        let target_subject = validate_participant_subject("handoff target", target_subject)?;
        if !(5_000..=300_000).contains(&ttl_ms) {
            anyhow::bail!("runner handoff TTL must be between 5 and 300 seconds");
        }
        self.ensure_loaded(name)?;
        let now = unix_millis();
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(receipt) = mutation.as_ref() {
            if let Some(existing) = state.events.iter().find(|event| {
                event.mutation.as_ref().is_some_and(|recorded| {
                    recorded.attachment_id == receipt.attachment_id
                        && recorded.mutation_id == receipt.mutation_id
                })
            }) {
                anyhow::ensure!(
                    existing.mutation.as_ref() == Some(receipt),
                    "Brain mutation idempotency key was reused with a different command or precondition"
                );
                return match &existing.kind {
                    BrainEventKind::RunnerHandoffRequested { handoff } => Ok(handoff.clone()),
                    _ => anyhow::bail!("Brain mutation receipt does not identify a handoff"),
                };
            }
        }
        if environment_generation != self.environment.generation {
            anyhow::bail!("runner handoff environment generation does not match this Brain");
        }
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
        let kind = BrainEventKind::RunnerHandoffRequested {
            handoff: handoff.clone(),
        };
        match mutation {
            Some(receipt) => {
                let appended =
                    self.push_idempotent_locked(name, state, requested_by, kind, receipt)?;
                match appended.event.kind {
                    BrainEventKind::RunnerHandoffRequested { handoff } => Ok(handoff),
                    _ => anyhow::bail!("Brain mutation receipt does not identify a handoff"),
                }
            }
            None => {
                self.push_locked(name, state, requested_by, kind)?;
                Ok(handoff)
            }
        }
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        self.cancel_runner_handoff_with_receipt(name, handoff_id, sender, None)
    }

    pub fn cancel_runner_handoff_with_receipt(
        &self,
        name: &str,
        handoff_id: RunnerHandoffId,
        sender: &str,
        receipt: Option<BrainMutationReceipt>,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        let sender = validate_participant_subject("handoff canceller", sender)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(receipt) = receipt.as_ref() {
            if let Some(existing) = state.events.iter().find(|event| {
                event.mutation.as_ref().is_some_and(|recorded| {
                    recorded.attachment_id == receipt.attachment_id
                        && recorded.mutation_id == receipt.mutation_id
                })
            }) {
                anyhow::ensure!(existing.mutation.as_ref() == Some(receipt),
                    "Brain mutation idempotency key was reused with a different command or precondition");
                anyhow::ensure!(
                    match &existing.kind {
                        BrainEventKind::RunnerHandoffCancelled {
                            handoff_id: recorded,
                        } => *recorded == handoff_id,
                        BrainEventKind::MutationRecorded {
                            outcome:
                                BrainMutationOutcome::HandoffCancellationNoop {
                                    handoff_id: recorded,
                                },
                        } => *recorded == handoff_id,
                        _ => false,
                    },
                    "replayed mutation outcome does not match runner handoff cancellation"
                );
                return Ok(());
            }
        }
        if !state
            .runner_handoff
            .as_ref()
            .is_some_and(|handoff| handoff.handoff_id == handoff_id)
        {
            if let Some(receipt) = receipt {
                self.push_idempotent_locked(
                    name,
                    state,
                    sender,
                    BrainEventKind::MutationRecorded {
                        outcome: BrainMutationOutcome::HandoffCancellationNoop { handoff_id },
                    },
                    receipt,
                )?;
                return Ok(());
            }
            anyhow::bail!("runner handoff is no longer current");
        }
        let kind = BrainEventKind::RunnerHandoffCancelled { handoff_id };
        match receipt {
            Some(receipt) => {
                self.push_idempotent_locked(name, state, sender, kind, receipt)?;
            }
            None => {
                self.push_locked(name, state, sender, kind)?;
            }
        }
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
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
            self.forget_schedules_locked(name);
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
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let head = state.revision;
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
        self.run_publication_gates
            .write()
            .expect("shared Brain run-gate map poisoned")
            .retain(|(brain, _), _| brain != name);
        Ok(archived_to)
    }

    pub fn push(&self, name: &str, sender: &str, kind: BrainEventKind) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        self.push_locked(name, state, sender, kind)
    }

    /// Append a mutation's first canonical event exactly once. Replays are
    /// resolved before revision validation because a successful retry is
    /// necessarily stale relative to its own original append.
    pub fn push_idempotent(
        &self,
        name: &str,
        sender: &str,
        kind: BrainEventKind,
        receipt: BrainMutationReceipt,
    ) -> Result<BrainMutationAppend> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        self.push_idempotent_locked(name, state, sender, kind, receipt)
    }

    pub fn reserve_approval_decision(
        &self,
        name: &str,
        sender: &str,
        request_seq: u64,
        approval_id: &str,
        decision: serde_json::Value,
        receipt: BrainMutationReceipt,
    ) -> Result<BrainApprovalDecisionReservation> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        let expected_decision = decision.clone();
        let append = self.push_idempotent_locked(
            name,
            state,
            sender,
            BrainEventKind::ApprovalDecided {
                request_seq,
                approval_id: approval_id.to_string(),
                decision,
            },
            receipt.clone(),
        )?;
        anyhow::ensure!(
            matches!(&append.event.kind,
            BrainEventKind::ApprovalDecided {
                request_seq: recorded_seq, approval_id: recorded_id,
                decision: recorded_decision,
            } if *recorded_seq == request_seq && recorded_id == approval_id
                && recorded_decision == &expected_decision),
            "replayed mutation outcome is not this approval decision"
        );
        let delivered = state.events.iter().any(|event| {
            matches!(&event.kind,
            BrainEventKind::MutationRecorded {
                outcome: BrainMutationOutcome::ApprovalDecisionDelivered {
                    request_seq: recorded_seq, approval_id: recorded_id, mutation_id,
                },
            } if *recorded_seq == request_seq && recorded_id == approval_id
                && *mutation_id == receipt.mutation_id)
        });
        Ok(BrainApprovalDecisionReservation {
            event: append.event,
            delivered,
            replayed: append.replayed,
        })
    }

    pub fn complete_approval_decision_delivery(
        &self,
        name: &str,
        sender: &str,
        request_seq: u64,
        approval_id: &str,
        mutation_id: uuid::Uuid,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        anyhow::ensure!(
            state.events.iter().any(|event| {
                event
                    .mutation
                    .as_ref()
                    .is_some_and(|receipt| receipt.mutation_id == mutation_id)
                    && matches!(&event.kind, BrainEventKind::ApprovalDecided {
                    request_seq: recorded_seq, approval_id: recorded_id, ..
                } if *recorded_seq == request_seq && recorded_id == approval_id)
            }),
            "approval decision reservation does not exist"
        );
        if state.events.iter().any(|event| {
            matches!(&event.kind,
            BrainEventKind::MutationRecorded {
                outcome: BrainMutationOutcome::ApprovalDecisionDelivered {
                    request_seq: recorded_seq, approval_id: recorded_id,
                    mutation_id: recorded_mutation,
                },
            } if *recorded_seq == request_seq && recorded_id == approval_id
                && *recorded_mutation == mutation_id)
        }) {
            return Ok(());
        }
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::MutationRecorded {
                outcome: BrainMutationOutcome::ApprovalDecisionDelivered {
                    request_seq,
                    approval_id: approval_id.to_string(),
                    mutation_id,
                },
            },
        )?;
        Ok(())
    }

    pub fn approval_decision_delivery_completed(
        &self,
        name: &str,
        mutation_id: uuid::Uuid,
    ) -> Result<bool> {
        let snapshot = self.snapshot(name)?;
        Ok(snapshot.events.iter().any(|event| {
            matches!(&event.kind,
            BrainEventKind::MutationRecorded {
                outcome: BrainMutationOutcome::ApprovalDecisionDelivered {
                    mutation_id: recorded, ..
                },
            } if *recorded == mutation_id)
        }))
    }

    /// Resolve an exact durable replay without applying a new transition.
    /// Authorization remains the caller's responsibility on every attempt.
    pub fn replay_mutation(
        &self,
        name: &str,
        receipt: &BrainMutationReceipt,
    ) -> Result<Option<BrainEvent>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let Some(event) = state.events.iter().find(|event| {
            event.mutation.as_ref().is_some_and(|recorded| {
                recorded.attachment_id == receipt.attachment_id
                    && recorded.mutation_id == receipt.mutation_id
            })
        }) else {
            return Ok(None);
        };
        anyhow::ensure!(
            event.mutation.as_ref() == Some(receipt),
            "Brain mutation idempotency key was reused with a different command or precondition"
        );
        Ok(Some(event.clone()))
    }

    fn push_idempotent_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
        receipt: BrainMutationReceipt,
    ) -> Result<BrainMutationAppend> {
        if let Some(existing) = state.events.iter().find(|event| {
            event.mutation.as_ref().is_some_and(|recorded| {
                recorded.attachment_id == receipt.attachment_id
                    && recorded.mutation_id == receipt.mutation_id
            })
        }) {
            let recorded = existing
                .mutation
                .as_ref()
                .expect("matched mutation receipt");
            anyhow::ensure!(
                recorded == &receipt,
                "Brain mutation idempotency key was reused with a different command or precondition"
            );
            return Ok(BrainMutationAppend {
                event: existing.clone(),
                replayed: true,
            });
        }
        anyhow::ensure!(
            receipt.environment_generation == self.environment.generation,
            "Brain mutation environment generation is stale"
        );
        let revision = state.revision;
        anyhow::ensure!(
            receipt.expected_revision == revision,
            "Brain mutation expected revision {} but current revision is {revision}",
            receipt.expected_revision
        );
        let event = self.push_locked_with_mutation(name, state, sender, kind, Some(receipt))?;
        Ok(BrainMutationAppend {
            event,
            replayed: false,
        })
    }

    /// Durably append an executable request and its RunStarted projection in
    /// one physical record, then apply and broadcast both logical events in
    /// native sequence order.
    pub fn push_executable_idempotent(
        &self,
        name: &str,
        sender: &str,
        kind: BrainEventKind,
        receipt: BrainMutationReceipt,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
    ) -> Result<BrainExecutableMutationAppend> {
        anyhow::ensure!(
            matches!(
                kind,
                BrainEventKind::Prompt { .. } | BrainEventKind::Program { .. }
            ),
            "only executable Prompt or Program events can atomically start a run"
        );
        let name = Self::validate_name(name)?;
        let sender = validate_participant_subject("run initiator", sender)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        if let Some(existing) = state.events.iter().find(|event| {
            event.mutation.as_ref().is_some_and(|recorded| {
                recorded.attachment_id == receipt.attachment_id
                    && recorded.mutation_id == receipt.mutation_id
            })
        }) {
            anyhow::ensure!(
                existing.mutation.as_ref() == Some(&receipt),
                "Brain mutation idempotency key was reused with a different command or precondition"
            );
            let run = state
                .runs
                .values()
                .find(|run| run.request_seq == existing.seq)
                .cloned()
                .context("replayed executable Brain mutation has no canonical run")?;
            return Ok(BrainExecutableMutationAppend {
                accepted: existing.clone(),
                run,
                replayed: true,
            });
        }
        anyhow::ensure!(
            receipt.attachment_id == initiating_attachment_id,
            "Brain mutation attachment does not match run initiator"
        );
        anyhow::ensure!(
            receipt.environment_generation == self.environment.generation,
            "Brain mutation environment generation is stale"
        );
        let revision = state.revision;
        anyhow::ensure!(
            receipt.expected_revision == revision,
            "Brain mutation expected revision {} but current revision is {revision}",
            receipt.expected_revision
        );
        let now = unix_millis();
        let run_id = RunId::new();
        let accepted = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: revision + 1,
            environment_generation: self.environment.generation,
            sender: sender.to_string(),
            created_ms: now,
            run_id: Some(run_id),
            mutation: Some(receipt),
            kind,
        };
        let run = BrainRun {
            run_id,
            kind: BrainRunKind::Interactive,
            parent_run_id: None,
            request_seq: accepted.seq,
            initiating_attachment_id,
            initiated_by: sender.to_string(),
            status,
            started_ms: now,
            updated_ms: now,
            detail: None,
        };
        let started = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: revision + 2,
            environment_generation: self.environment.generation,
            sender: sender.to_string(),
            created_ms: now,
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::RunStarted { run: run.clone() },
        };
        self.append_event_batch(name, &[accepted.clone(), started.clone()])?;
        for event in [accepted.clone(), started] {
            state.apply(event.clone());
            let _ = state.tx.send(event);
        }
        Ok(BrainExecutableMutationAppend {
            accepted,
            run,
            replayed: false,
        })
    }

    fn push_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
    ) -> Result<BrainEvent> {
        self.push_locked_with_context(name, state, sender, None, kind, None)
    }

    fn push_locked_for_run(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        run_id: Option<RunId>,
        kind: BrainEventKind,
    ) -> Result<BrainEvent> {
        self.push_locked_with_context(name, state, sender, run_id, kind, None)
    }

    fn push_locked_with_mutation(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
        mutation: Option<BrainMutationReceipt>,
    ) -> Result<BrainEvent> {
        self.push_locked_with_context(name, state, sender, None, kind, mutation)
    }

    fn push_locked_with_context(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        run_id: Option<RunId>,
        kind: BrainEventKind,
        mutation: Option<BrainMutationReceipt>,
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
            seq: state.revision + 1,
            environment_generation: self.environment.generation,
            sender: sender.trim().to_string(),
            created_ms: unix_millis(),
            run_id,
            mutation,
            kind,
        };
        self.append_event(name, &event)?;
        let touches_schedules = matches!(
            event.kind,
            BrainEventKind::ScheduleChanged { .. } | BrainEventKind::ScheduleDue { .. }
        );
        state.apply(event.clone());
        // Every schedule creation, cancellation and advance reaches state
        // through `ScheduleChanged`/`ScheduleDue` here, so the index has one
        // maintenance point rather than one per call site (#374).
        if touches_schedules {
            self.reindex_schedules_locked(name, state);
        }
        let _ = state.tx.send(event.clone());
        Ok(event)
    }

    pub fn push_for_run(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        kind: BrainEventKind,
    ) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        anyhow::ensure!(state.runs.contains_key(&run_id), "Brain run does not exist");
        self.push_locked_for_run(name, state, sender, Some(run_id), kind)
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
        self.commit_runner_runtime_inner(name, None, request_seq, runtime_revision, checkpoint)
    }

    pub fn commit_runner_runtime_for_run(
        &self,
        name: &str,
        run_id: RunId,
        request_seq: u64,
        runtime_revision: u64,
        checkpoint: crate::vm::TypedRuntimeCheckpoint,
    ) -> Result<BrainEvent> {
        self.commit_runner_runtime_inner(
            name,
            Some(run_id),
            request_seq,
            runtime_revision,
            checkpoint,
        )
    }

    fn commit_runner_runtime_inner(
        &self,
        name: &str,
        run_id: Option<RunId>,
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
        let kind = BrainEventKind::RuntimeCommitted {
            request_seq,
            runtime_revision,
            checkpoint_sha256,
        };
        match run_id {
            Some(run_id) => self.push_for_run(name, "runner", run_id, kind),
            None => self.push(name, "runner", kind),
        }
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
        let directory = root.join(name).join("runtime");
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
        let encoded = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
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
        create_dir_all_durable(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(format!("{checkpoint_sha256}.capnp"));
        if path.exists() {
            return Ok(());
        }
        let temporary =
            directory.join(format!(".{checkpoint_sha256}.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &path).with_context(|| format!("commit {}", path.display()))?;
        sync_directory(&directory)?;
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
        let mut events = self.read_events(name)?;
        if self.root.is_some() {
            let legacy_audits = events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::EffectAuditTransition { .. }))
                .cloned()
                .collect::<Vec<_>>();
            if !legacy_audits.is_empty() {
                self.with_effect_audit_storage_mut(name, brain_id, |storage| {
                    for event in &legacy_audits {
                        let BrainEventKind::EffectAuditTransition { transition } = &event.kind
                        else {
                            unreachable!("filtered effect-audit event changed kind");
                        };
                        storage.active.append(event.seq, transition)?;
                    }
                    Ok(())
                })?;
                events.retain(|event| {
                    !matches!(event.kind, BrainEventKind::EffectAuditTransition { .. })
                });
                self.rewrite_events(name, &events)?;
            }
            let segmented = self
                .with_effect_audit_storage_mut(name, brain_id, |storage| storage.active.load())?
                .unwrap_or_default();
            events.extend(segmented.into_iter().map(|(seq, transition)| BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id,
                seq,
                environment_generation: self.environment.generation,
                sender: "daemon/effect-audit-segment".into(),
                created_ms: 0,
                run_id: Some(RunId(transition.identity().run_id)),
                mutation: None,
                kind: BrainEventKind::EffectAuditTransition { transition },
            }));
            events.sort_by_key(|event| event.seq);
            for pair in events.windows(2) {
                anyhow::ensure!(
                    pair[0].seq < pair[1].seq,
                    "Brain '{name}' has duplicate or reordered canonical event sequence {}",
                    pair[1].seq
                );
            }
        }
        backfill_legacy_speculative_run_correlation(&mut events);
        let canonical_run_requests = events
            .iter()
            .filter_map(|event| {
                let BrainEventKind::RunStarted { run } = &event.kind else {
                    return None;
                };
                Some((run.run_id.0, run.request_seq))
            })
            .collect::<HashMap<_, _>>();
        let mut reviewed_schedules = HashMap::new();
        let mut validated_effect_audits = crate::runtime::effect_log::EffectAuditReducer::default();
        for event in &events {
            if event.schema_version > BRAIN_EVENT_SCHEMA_VERSION {
                anyhow::bail!(
                    "Brain '{name}' contains unsupported event schema version {}",
                    event.schema_version
                );
            }
            if event.schema_version < 14 && event.mutation.is_some() {
                anyhow::bail!(
                    "Brain '{name}' event #{} carries a mutation receipt under legacy schema {}",
                    event.seq,
                    event.schema_version,
                );
            }
            match &event.kind {
                BrainEventKind::EffectAuditTransition { transition } => {
                    anyhow::ensure!(event.schema_version >= 15,
                        "Brain '{name}' event #{} carries an effect-audit transition under legacy schema {}",
                        event.seq, event.schema_version);
                    let identity = transition.identity();
                    anyhow::ensure!(
                        identity.brain_id == brain_id.0
                            && event.run_id.map(|run_id| run_id.0) == Some(identity.run_id),
                        "Brain '{name}' event #{} has mismatched effect-audit identity",
                        event.seq
                    );
                    anyhow::ensure!(
                        canonical_run_requests.get(&identity.run_id)
                            == Some(&identity.request_seq),
                        "Brain '{name}' event #{} has a non-canonical effect-audit run/request identity",
                        event.seq,
                    );
                    validated_effect_audits
                        .apply(transition.clone())
                        .with_context(|| {
                            format!(
                        "Brain '{name}' event #{} contains an invalid effect-audit transition",
                        event.seq
                    )
                        })?;
                }
                BrainEventKind::EffectRecorded {
                    request_seq,
                    execution_id,
                    effect,
                    state,
                } => {
                    anyhow::ensure!(
                        event.schema_version <= 14,
                        "Brain '{name}' event #{} uses legacy EffectRecorded under schema {}",
                        event.seq,
                        event.schema_version
                    );
                    let identity = crate::runtime::effect_log::EffectAuditIdentity {
                        brain_id: brain_id.0,
                        run_id: event.run_id.unwrap_or(RunId(uuid::Uuid::nil())).0,
                        request_seq: *request_seq,
                        execution_id: *execution_id,
                        effect_sequence: effect.sequence,
                    };
                    let authority = crate::runtime::effect_log::EffectAuditAuthority {
                        authority_id: uuid::Uuid::nil(),
                        runner_lease_id: uuid::Uuid::nil(),
                        runner_subject: "legacy-v14".into(),
                        connection_id: None,
                        environment_generation: event.environment_generation,
                    };
                    validated_effect_audits.apply(
                        crate::runtime::effect_log::EffectAuditTransition::Reserve {
                            intent: crate::runtime::effect_log::EffectAuditIntent::from_effect(
                                identity, effect,
                            )?,
                            authority: authority.clone(),
                        },
                    )?;
                    validated_effect_audits.apply(
                        crate::runtime::effect_log::EffectAuditTransition::Finish {
                            identity, authority_id: authority.authority_id,
                            outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::LegacyV14Snapshot {
                                state: state.clone(),
                            },
                        },
                    )?;
                }
                _ => {}
            }
            if event.brain_id != BrainId::nil() && event.brain_id != brain_id {
                anyhow::bail!(
                    "Brain '{name}' event #{} belongs to a different Brain identity",
                    event.seq
                );
            }
            if let BrainEventKind::ScheduleChanged { schedule } = &event.kind {
                if schedule.module_identity.is_some() {
                    initialization
                        .validate_schedule(schedule)
                        .with_context(|| {
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
                    initialization
                        .validate_schedule_due(schedule, due)
                        .with_context(|| {
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
            create_dir_all_durable(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
        }
        let cursors = self.read_attachment_cursors(name, brain_id)?;
        let mut state = BrainState::from_events(brain_id, events);
        if self.root.is_some() {
            state.events.retain(|event| {
                !matches!(event.kind, BrainEventKind::EffectAuditTransition { .. })
            });
            let replay_max = self
                .with_effect_audit_storage_mut(name, brain_id, |storage| {
                    Ok(storage.replay.max_seq())
                })?
                .unwrap_or(0);
            state.revision = state.revision.max(replay_max);
            let terminal_identities = state
                .effect_audits
                .entries()
                .values()
                .filter_map(|entry| entry.state.is_terminal().then_some(entry.intent.identity))
                .collect::<Vec<_>>();
            let mut terminal = Vec::new();
            for identity in terminal_identities {
                let terminal_seq = self
                    .with_effect_audit_storage_mut(name, brain_id, |storage| {
                        storage.active.last_seq_for(&identity)
                    })?
                    .flatten()
                    .unwrap_or(state.revision);
                terminal.push((terminal_seq, identity));
            }
            self.archive_terminal_effect_audits_batch_locked(name, &mut state, &terminal)?;
            let recent = self
                .with_effect_audit_storage_mut(name, brain_id, |storage| {
                    storage.replay.latest(MAX_RETAINED_TERMINAL_EFFECT_AUDITS)
                })?
                .unwrap_or_default();
            state.recent_effect_audits.clear();
            for fence in recent {
                let mut observer = crate::runtime::effect_log::EffectAuditReducer::default();
                observer.apply(fence)?;
                state.recent_effect_audits.extend(
                    observer
                        .entries()
                        .values()
                        .map(crate::runtime::effect_log::EffectAuditEntry::observer_projection),
                );
            }
        }
        for attachment in state.attachments.values_mut() {
            // A process restart disconnects every transport projection;
            // reconnect appends a fresh ClientAttached event.
            attachment.connected = false;
            attachment.connection_id = None;
        }
        for (attachment_id, acknowledged_seq) in cursors {
            if let Some(attachment) = state.attachments.get_mut(&attachment_id) {
                attachment.acknowledged_seq = acknowledged_seq.min(state.revision);
            }
        }
        // Reconcile unresolved write-ahead state before a successor can run.
        let unresolved_effects = state
            .effect_audits
            .entries()
            .values()
            .filter(|entry| !entry.state.is_terminal())
            .cloned()
            .collect::<Vec<_>>();
        let restart_transitions =
            unresolved_effects
                .into_iter()
                .filter_map(|entry| {
                    let outcome = match entry.state {
                crate::runtime::effect_log::EffectAuditState::IntentAccepted =>
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied,
                crate::runtime::effect_log::EffectAuditState::AwaitingHostResult =>
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss,
                crate::runtime::effect_log::EffectAuditState::Terminal { .. } => return None,
            };
                    Some(crate::runtime::effect_log::EffectAuditTransition::Finish {
                        identity: entry.intent.identity,
                        authority_id: entry.authority.authority_id,
                        outcome,
                    })
                })
                .collect::<Vec<_>>();
        self.append_effect_audit_transition_batch_locked(name, &mut state, restart_transitions)?;
        self.compact_terminal_effect_audits_locked(
            name,
            &mut state,
            MAX_RETAINED_TERMINAL_EFFECT_AUDITS,
        )?;
        // Queued work has not executed and may be safely offered to the next
        // valid runner lease. A run that was already executing or suspended
        // for approval has unknown external progress after daemon restart and
        // must never be replayed implicitly.
        let mut disconnect_intents = self.read_disconnect_intents(name)?;
        let mut retry_after_load = Vec::new();
        let mut retry_cancellations_after_load = Vec::new();
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
            if Self::state_has_run_cancellation_reservation(&state, run_id) {
                let event = BrainEvent {
                    schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                    brain_id: state.brain_id,
                    seq: state.revision + 1,
                    environment_generation: self.environment.generation,
                    sender: "daemon".into(),
                    created_ms: unix_millis(),
                    run_id: Some(run_id),
                    mutation: None,
                    kind: BrainEventKind::RunStatusChanged {
                        run_id,
                        status: BrainRunStatus::Cancelled,
                        detail: Some("cancelled by initiating driver".into()),
                    },
                };
                match self.append_event(name, &event) {
                    Ok(()) => state.apply(event),
                    Err(error) => {
                        tracing::error!(brain = %name, run_id = %run_id.0, %error,
                            "restart could not complete reserved cancellation; requeueing");
                        retry_cancellations_after_load.push(run_id);
                    }
                }
                continue;
            }
            if let Some(intent) = disconnect_intents.remove(&run_id) {
                let next_seq = state.revision + 1;
                let now = unix_millis();
                let result = BrainEvent {
                    schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                    brain_id: state.brain_id,
                    seq: next_seq,
                    environment_generation: self.environment.generation,
                    sender: intent.sender.clone(),
                    created_ms: now,
                    run_id: Some(run_id),
                    mutation: None,
                    kind: BrainEventKind::Result {
                        request_seq: intent.request_seq,
                        output: String::new(),
                        error: Some(intent.detail.clone()),
                        continuation_messages: Vec::new(),
                        invocation_metadata: None,
                    },
                };
                let terminal = BrainEvent {
                    schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                    brain_id: state.brain_id,
                    seq: next_seq + 1,
                    environment_generation: self.environment.generation,
                    sender: intent.sender.clone(),
                    created_ms: now,
                    run_id: Some(run_id),
                    mutation: None,
                    kind: BrainEventKind::RunStatusChanged {
                        run_id,
                        status: intent.status,
                        detail: Some(intent.detail.clone()),
                    },
                };
                match self.append_event_batch(name, &[result.clone(), terminal.clone()]) {
                    Ok(()) => {
                        state.apply(result);
                        state.apply(terminal);
                        self.clear_disconnect_intent(name, run_id)?;
                    }
                    Err(error) => {
                        tracing::error!(brain = %name, run_id = %run_id.0, %error,
                            "restart could not reconcile disconnect terminalization; requeueing");
                        retry_after_load.push(intent);
                    }
                }
                continue;
            }
            let event = BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: state.brain_id,
                seq: state.revision + 1,
                environment_generation: self.environment.generation,
                sender: "daemon".into(),
                created_ms: unix_millis(),
                run_id: Some(run_id),
                mutation: None,
                kind: BrainEventKind::RunStatusChanged {
                    run_id,
                    status: BrainRunStatus::Interrupted,
                    detail: Some("daemon restarted before the run reached a terminal state".into()),
                },
            };
            self.append_event(name, &event)?;
            state.apply(event);
        }
        for run_id in disconnect_intents.keys().copied().collect::<Vec<_>>() {
            if state
                .runs
                .get(&run_id)
                .is_some_and(|run| run.status.is_terminal())
            {
                self.clear_disconnect_intent(name, run_id)?;
            }
        }
        self.initializations
            .write()
            .expect("shared Brain initialization lock poisoned")
            .entry(name.to_string())
            .or_insert(initialization);
        {
            let mut brains = self.brains.write().expect("shared brain lock poisoned");
            let resident = brains.entry(name.to_string()).or_insert(state);
            // A Brain's schedules only become known once it is loaded, so this
            // is where they enter the due index (#374). The index therefore
            // covers exactly the resident Brains; a daemon that has not yet
            // warmed it must enumerate once, which is the one remaining cost
            // and is why the delivery loop warms it at startup.
            self.reindex_schedules_locked(name, resident);
        }
        for intent in retry_after_load {
            self.schedule_disconnect_terminalization_retry(
                name.to_string(),
                intent.sender,
                intent.run_id,
                intent.request_seq,
                intent.status,
                intent.detail,
            );
        }
        for run_id in retry_cancellations_after_load {
            self.schedule_reserved_cancellation_retry(name.to_string(), "daemon".into(), run_id);
        }
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
        create_dir_all_durable(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("initialization.json");
        if path.exists() {
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let initialization: BrainInitialization = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            initialization.validate(brain_id)?;
            return Ok(initialization);
        }
        let initialization = BrainInitialization::reviewed_default(brain_id);
        let temporary = directory.join(format!(".initialization.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&initialization)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::File::open(&temporary)?.sync_all()?;
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                sync_directory(&directory)?;
                Ok(initialization)
            }
            Err(_error) if path.exists() => {
                let _ = std::fs::remove_file(&temporary);
                let bytes = std::fs::read(&path).with_context(|| {
                    format!("read {} after initialization race", path.display())
                })?;
                let initialization: BrainInitialization = serde_json::from_slice(&bytes)
                    .with_context(|| {
                        format!("parse {} after initialization race", path.display())
                    })?;
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
        let file: AttachmentCursorFile =
            serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;
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
        create_dir_all_durable(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("attachments.json");
        let temporary = directory.join(format!(".attachments.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&file)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path).with_context(|| format!("commit {}", path.display()))?;
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
        create_dir_all_durable(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("metadata.json");
        if path.exists() {
            let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            let metadata: BrainMetadata = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if metadata.version != BRAIN_METADATA_VERSION || metadata.brain_id == BrainId::nil() {
                anyhow::bail!(
                    "unsupported or invalid Brain metadata at {}",
                    path.display()
                );
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
        std::fs::File::open(&temporary)?.sync_all()?;
        // A rename can replace a winner on Unix. Linking the fully written
        // temporary file creates the final name only if it is still absent,
        // so concurrent daemon starts cannot split one alias into two IDs.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                sync_directory(&directory)?;
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

    fn with_effect_audit_storage_mut<T>(
        &self,
        name: &str,
        brain_id: BrainId,
        operation: impl FnOnce(&mut EffectAuditStorage) -> Result<T>,
    ) -> Result<Option<T>> {
        let Some(root) = &self.root else {
            return Ok(None);
        };
        let brain_directory = root.join(name);
        create_dir_all_durable(&brain_directory)?;
        let mut stores = self
            .effect_audit_storage
            .lock()
            .expect("effect-audit storage map poisoned");
        if !stores.contains_key(name) {
            stores.insert(
                name.to_string(),
                EffectAuditStorage {
                    active: super::effect_audit_archive::EffectAuditActiveJournal::open(
                        &brain_directory,
                    )?,
                    replay: super::effect_audit_archive::EffectAuditReplayArchive::open(
                        &brain_directory,
                        brain_id.0,
                    )?,
                },
            );
        }
        operation(
            stores
                .get_mut(name)
                .context("effect-audit storage disappeared during operation")?,
        )
        .map(Some)
    }

    #[cfg(test)]
    pub(crate) fn seed_mature_effect_audit_history_for_test(
        &self,
        name: &str,
        records: usize,
        epochs: usize,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .context("Brain was removed concurrently")?
            .brain_id;
        let max_seq = self
            .with_effect_audit_storage_mut(name, brain_id, |storage| {
                storage
                    .replay
                    .seed_mature_history_for_test(brain_id.0, records, epochs)?;
                Ok(storage.replay.max_seq())
            })?
            .unwrap_or(0);
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains
            .get_mut(name)
            .context("Brain was removed concurrently")?;
        state.revision = state.revision.max(max_seq);
        Ok(())
    }

    #[cfg(test)]
    fn exhaust_effect_audit_storage_for_test(&self, name: &str) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .context("Brain was removed concurrently")?
            .brain_id;
        self.with_effect_audit_storage_mut(name, brain_id, |storage| {
            storage.replay.exhaust_storage_for_test()
        })?;
        Ok(())
    }

    fn disconnect_intent_path(&self, name: &str, run_id: RunId) -> Option<PathBuf> {
        self.root.as_ref().map(|root| {
            root.join(name)
                .join("disconnect-terminalizations")
                .join(format!("{}.json", run_id.0))
        })
    }

    fn persist_disconnect_intent(
        &self,
        name: &str,
        intent: &DisconnectTerminalizationIntent,
    ) -> Result<()> {
        let Some(path) = self.disconnect_intent_path(name, intent.run_id) else {
            return Ok(());
        };
        let directory = path.parent().expect("disconnect intent has a parent");
        create_dir_all_durable(directory)?;
        let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec(intent)?)?;
        std::fs::File::open(&temporary)?.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        sync_directory(directory)
    }

    fn clear_disconnect_intent(&self, name: &str, run_id: RunId) -> Result<()> {
        let Some(path) = self.disconnect_intent_path(name, run_id) else {
            return Ok(());
        };
        if path.exists() {
            std::fs::remove_file(&path)?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
        }
        Ok(())
    }

    fn read_disconnect_intents(
        &self,
        name: &str,
    ) -> Result<HashMap<RunId, DisconnectTerminalizationIntent>> {
        let Some(root) = &self.root else {
            return Ok(HashMap::new());
        };
        let directory = root.join(name).join("disconnect-terminalizations");
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Ok(HashMap::new());
        };
        let mut intents = HashMap::new();
        for entry in entries {
            let entry = entry?;
            if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let intent: DisconnectTerminalizationIntent =
                serde_json::from_slice(&std::fs::read(entry.path())?)?;
            intents.insert(intent.run_id, intent);
        }
        Ok(intents)
    }

    fn read_events(&self, name: &str) -> Result<Vec<BrainEvent>> {
        let Some(path) = self.event_path(name) else {
            return Ok(Vec::new());
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(Vec::new());
        };
        if !bytes.is_empty() && bytes.last() != Some(&b'\n') {
            let committed_len = bytes
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map(|offset| offset + 1)
                .unwrap_or(0);
            let file = OpenOptions::new()
                .write(true)
                .open(&path)
                .with_context(|| format!("open {} for torn-tail recovery", path.display()))?;
            file.set_len(committed_len as u64)
                .with_context(|| format!("truncate torn tail in {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("sync recovered {}", path.display()))?;
        }
        let mut events = Vec::new();
        let records = bytes
            .split_inclusive(|byte| *byte == b'\n')
            .collect::<Vec<_>>();
        let mut committed_offset = 0usize;
        for (line_no, terminated) in records.iter().enumerate() {
            // Committed records are newline-terminated. A torn final append
            // projects none of its logical events after restart.
            if terminated.last() != Some(&b'\n') {
                break;
            }
            let line = &terminated[..terminated.len() - 1];
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let parsed = match serde_json::from_slice::<BrainJournalRecord>(line) {
                Ok(BrainJournalRecord::EventBatch {
                    event_count,
                    payload_sha256,
                    events: batch,
                }) => {
                    let valid_framing = match (event_count, payload_sha256) {
                        (None, None) => true,
                        (Some(count), Some(checksum)) => {
                            count == batch.len()
                                && serde_json::to_vec(&batch).is_ok_and(|payload| {
                                    hex::encode(Sha256::digest(payload)) == checksum
                                })
                        }
                        _ => false,
                    };
                    valid_framing.then_some(batch)
                }
                Err(_) => serde_json::from_slice::<BrainEvent>(line)
                    .ok()
                    .map(|event| vec![event]),
            };
            if let Some(batch) = parsed {
                events.extend(batch);
                committed_offset += terminated.len();
                continue;
            }
            if line_no + 1 == records.len() {
                let file = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .with_context(|| {
                        format!("open {} for corrupt-tail recovery", path.display())
                    })?;
                file.set_len(committed_offset as u64)
                    .with_context(|| format!("truncate corrupt tail in {}", path.display()))?;
                file.sync_all()
                    .with_context(|| format!("sync recovered {}", path.display()))?;
                break;
            }
            anyhow::bail!("parse {} line {}", path.display(), line_no + 1);
        }
        Ok(events)
    }

    fn append_event(&self, name: &str, event: &BrainEvent) -> Result<()> {
        #[cfg(test)]
        if matches!(
            event.kind,
            BrainEventKind::RunStatusChanged {
                status: BrainRunStatus::Cancelled,
                ..
            }
        ) && self
            .fail_cancellation_terminal_appends
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| {
                    if remaining > 0 {
                        Some(remaining - 1)
                    } else {
                        None
                    }
                },
            )
            .is_ok()
        {
            anyhow::bail!("injected reserved cancellation terminal append failure");
        }
        self.append_journal_value(name, event)
    }

    fn append_event_batch(&self, name: &str, events: &[BrainEvent]) -> Result<()> {
        anyhow::ensure!(!events.is_empty(), "Brain event batch cannot be empty");
        #[cfg(test)]
        if self
            .fail_event_batches
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |remaining| {
                    if remaining > 0 {
                        Some(remaining - 1)
                    } else {
                        None
                    }
                },
            )
            .is_ok()
        {
            anyhow::bail!("injected Brain event batch append failure");
        }
        let payload = serde_json::to_vec(events)?;
        self.append_journal_value(
            name,
            &BrainJournalRecord::EventBatch {
                event_count: Some(events.len()),
                payload_sha256: Some(hex::encode(Sha256::digest(payload))),
                events: events.to_vec(),
            },
        )
    }

    /// Atomically replace the canonical journal with an equivalent sequence
    /// of individual events. This is used only for bounded audit-history
    /// compaction; mutation receipts and every non-audit event are preserved.
    fn rewrite_events(&self, name: &str, events: &[BrainEvent]) -> Result<()> {
        let Some(path) = self.event_path(name) else {
            return Ok(());
        };
        let directory = path.parent().context("Brain event log has no parent")?;
        create_dir_all_durable(directory)?;
        let temporary = directory.join(format!(".events.{}.tmp", uuid::Uuid::new_v4()));
        let rewrite = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .with_context(|| format!("create {}", temporary.display()))?;
            for event in events {
                serde_json::to_writer(&mut file, event)?;
                file.write_all(b"\n")?;
            }
            file.sync_all()
                .with_context(|| format!("sync {}", temporary.display()))?;
            std::fs::rename(&temporary, &path)
                .with_context(|| format!("replace {}", path.display()))?;
            sync_directory(directory)
        })();
        if rewrite.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        rewrite
    }

    #[cfg(test)]
    pub(crate) fn fail_next_event_batch_for_test(&self) {
        self.fail_event_batches
            .store(1, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_next_effect_audit_batch_for_test(&self, name: &str) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .context("Brain was removed concurrently")?
            .brain_id;
        self.with_effect_audit_storage_mut(name, brain_id, |storage| {
            storage.active.fail_next_batch_before_commit_for_test();
            Ok(())
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn fail_event_batches_for_test(&self, count: usize) {
        self.fail_event_batches
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn fail_cancellation_terminal_appends_for_test(&self, count: usize) {
        self.fail_cancellation_terminal_appends
            .store(count, std::sync::atomic::Ordering::SeqCst);
    }

    fn append_journal_value<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        let Some(path) = self.event_path(name) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            create_dir_all_durable(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let new_file = !path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        let mut record = serde_json::to_vec(value)?;
        record.push(b'\n');
        file.write_all(&record)?;
        file.sync_all()
            .with_context(|| format!("sync {}", path.display()))?;
        if new_file {
            if let Some(parent) = path.parent() {
                std::fs::File::open(parent)
                    .with_context(|| format!("open {} for directory sync", parent.display()))?
                    .sync_all()
                    .with_context(|| format!("sync directory {}", parent.display()))?;
            }
        }
        Ok(())
    }
}

pub(super) fn sync_directory(path: &std::path::Path) -> Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("open {} for directory sync", path.display()))?
        .sync_all()
        .with_context(|| format!("sync directory {}", path.display()))
}

/// Create and durably link every missing directory component, including the
/// configured Brain root when it does not yet exist.
pub(super) fn create_dir_all_durable(path: &std::path::Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut cursor = path;
    while !cursor.exists() {
        missing.push(cursor.to_path_buf());
        let Some(parent) = cursor.parent() else { break };
        cursor = parent;
    }
    std::fs::create_dir_all(path)?;
    for directory in missing {
        sync_directory(&directory)?;
        if let Some(parent) = directory.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
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

fn queued_schedule_run(state: &BrainState, schedule: &BrainSchedule, now_ms: u64) -> BrainRun {
    BrainRun {
        run_id: RunId::new(),
        kind: BrainRunKind::Scheduled,
        parent_run_id: None,
        request_seq: state.revision + 1,
        initiating_attachment_id: schedule.initiating_attachment_id,
        initiated_by: schedule.created_by.clone(),
        status: BrainRunStatus::QueuedForEnvironment,
        started_ms: now_ms,
        updated_ms: now_ms,
        detail: None,
    }
}

fn schedule_due_window(schedule: &BrainSchedule, now_ms: u64) -> Result<(u32, u64, Option<u64>)> {
    let Some(interval_ms) = schedule.interval_ms else {
        return Ok((1, schedule.next_due_ms, None));
    };
    let elapsed = now_ms.saturating_sub(schedule.next_due_ms);
    let count = elapsed / interval_ms + 1;
    let last_due_ms = schedule
        .next_due_ms
        .checked_add(
            (count - 1)
                .checked_mul(interval_ms)
                .context("schedule overflow")?,
        )
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

fn sorted_schedule_dues(dues: &HashMap<RunId, BrainScheduleDue>) -> Vec<BrainScheduleDue> {
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

/// Schema v14 added explicit event-envelope RunId correlation. Reconstruct
/// completed v13 speculative transcripts from the durable run lifecycle so an
/// old helper turn cannot enter ordinary provider context after upgrade.
fn backfill_legacy_speculative_run_correlation(events: &mut [BrainEvent]) {
    struct LegacySpeculativeRun {
        run_id: RunId,
        request_seq: u64,
        started_seq: u64,
        terminal_seq: Option<u64>,
    }

    let runs = events
        .iter()
        .filter_map(|event| match &event.kind {
            BrainEventKind::RunStarted { run }
                if event.schema_version < 14 && run.kind == BrainRunKind::Speculative =>
            {
                let terminal_seq = events.iter().find_map(|candidate| match &candidate.kind {
                    BrainEventKind::RunStatusChanged { run_id, status, .. }
                        if *run_id == run.run_id
                            && candidate.seq > event.seq
                            && status.is_terminal() =>
                    {
                        Some(candidate.seq)
                    }
                    _ => None,
                });
                Some(LegacySpeculativeRun {
                    run_id: run.run_id,
                    request_seq: run.request_seq,
                    started_seq: event.seq,
                    terminal_seq,
                })
            }
            _ => None,
        })
        .collect::<Vec<_>>();

    for run in runs {
        let end_seq = run.terminal_seq.unwrap_or_else(|| {
            events
                .iter()
                .filter_map(|event| match &event.kind {
                    BrainEventKind::RunStarted { run: later }
                        if event.seq > run.started_seq && later.request_seq > run.request_seq =>
                    {
                        Some(later.request_seq.saturating_sub(1))
                    }
                    _ => None,
                })
                .min()
                .unwrap_or(u64::MAX)
        });
        let provider_program_seqs = events
            .iter()
            .filter(|event| {
                event.seq > run.started_seq
                    && event.seq <= end_seq
                    && event.sender == "provider"
                    && matches!(event.kind, BrainEventKind::Program { .. })
            })
            .map(|event| event.seq)
            .collect::<HashSet<_>>();

        for event in events.iter_mut() {
            if event.run_id.is_some() || event.schema_version >= 14 {
                continue;
            }
            let lifecycle_match = match &event.kind {
                BrainEventKind::RunStarted { run: started } => started.run_id == run.run_id,
                BrainEventKind::RunStatusChanged { run_id, .. } => *run_id == run.run_id,
                _ => false,
            };
            let referenced_seq = match &event.kind {
                BrainEventKind::ToolCall { request_seq, .. }
                | BrainEventKind::ToolResult { request_seq, .. }
                | BrainEventKind::ApprovalRequested { request_seq, .. }
                | BrainEventKind::ApprovalDecided { request_seq, .. }
                | BrainEventKind::EffectRecorded { request_seq, .. }
                | BrainEventKind::Result { request_seq, .. }
                | BrainEventKind::RuntimeCommitted { request_seq, .. } => Some(*request_seq),
                _ => None,
            };
            let correlated = event.seq == run.request_seq
                || lifecycle_match
                || (event.seq > run.started_seq
                    && event.seq <= end_seq
                    && event.sender == "provider"
                    && matches!(event.kind, BrainEventKind::Program { .. }))
                || referenced_seq.is_some_and(|seq| {
                    seq == run.request_seq || provider_program_seqs.contains(&seq)
                });
            if correlated {
                event.run_id = Some(run.run_id);
            }
        }
    }
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

    fn journal_event(brain_id: BrainId, seq: u64, text: &str) -> BrainEvent {
        BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id,
            seq,
            environment_generation: 1,
            sender: "alice".into(),
            created_ms: seq,
            run_id: None,
            mutation: None,
            kind: BrainEventKind::Prompt { text: text.into() },
        }
    }

    #[test]
    fn journal_reads_legacy_and_batch_records_with_logical_cursors() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let id = store.snapshot("shared").unwrap().brain_id;
        store
            .append_event("shared", &journal_event(id, 1, "legacy"))
            .unwrap();
        store
            .append_event_batch(
                "shared",
                &[
                    journal_event(id, 2, "first"),
                    journal_event(id, 3, "second"),
                ],
            )
            .unwrap();
        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.snapshot("shared").unwrap().revision, 3);
    }

    #[test]
    fn restart_ignores_torn_batch_tail() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let id = store.snapshot("shared").unwrap().brain_id;
        store
            .append_event("shared", &journal_event(id, 1, "committed"))
            .unwrap();
        let encoded = serde_json::to_vec(&BrainJournalRecord::EventBatch {
            event_count: None,
            payload_sha256: None,
            events: vec![
                journal_event(id, 2, "hidden"),
                journal_event(id, 3, "hidden-too"),
            ],
        })
        .unwrap();
        OpenOptions::new()
            .append(true)
            .open(temp.path().join("shared/events.jsonl"))
            .unwrap()
            .write_all(&encoded[..encoded.len() / 2])
            .unwrap();
        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.snapshot("shared").unwrap().revision, 1);
        restarted
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "after recovery".into(),
                },
            )
            .unwrap();
        drop(restarted);
        let restarted_again = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted_again.snapshot("shared").unwrap().revision, 2);
    }

    fn mutation_receipt(
        store: &BrainStore,
        attachment_id: AttachmentId,
        mutation_id: uuid::Uuid,
        expected_revision: u64,
        fingerprint: &str,
    ) -> BrainMutationReceipt {
        BrainMutationReceipt {
            mutation_id,
            attachment_id,
            expected_revision,
            environment_generation: store.environment().generation,
            command_sha256: fingerprint.into(),
        }
    }

    #[test]
    fn mutation_receipt_replays_from_the_canonical_log_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let mutation_id = uuid::Uuid::new_v4();
        let receipt = mutation_receipt(
            &store,
            attachment.attachment_id,
            mutation_id,
            0,
            "sha256:first",
        );
        let first = store
            .push_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into(),
                },
                receipt.clone(),
            )
            .unwrap();
        assert!(!first.replayed);
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let replayed = restarted
            .push_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into(),
                },
                receipt,
            )
            .unwrap();
        assert!(replayed.replayed);
        assert_eq!(replayed.event.seq, first.event.seq);
        assert_eq!(restarted.snapshot("shared").unwrap().revision, 1);
    }

    #[test]
    fn executable_mutation_and_run_replay_from_one_record_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = AttachmentId::new();
        let receipt = mutation_receipt(&store, attachment, uuid::Uuid::new_v4(), 0, "prompt");
        let first = store
            .push_executable_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into(),
                },
                receipt.clone(),
                attachment,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(temp.path().join("shared/events.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let replay = restarted
            .push_executable_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into(),
                },
                receipt,
                attachment,
                BrainRunStatus::Running,
            )
            .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.accepted, first.accepted);
        assert_eq!(replay.run, first.run);
        assert_eq!(restarted.snapshot("shared").unwrap().revision, 2);
    }

    #[test]
    fn mutation_receipt_rejects_stale_revision_and_fingerprint_reuse() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let mutation_id = uuid::Uuid::new_v4();
        store
            .push_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into(),
                },
                mutation_receipt(
                    &store,
                    attachment.attachment_id,
                    mutation_id,
                    0,
                    "sha256:first",
                ),
            )
            .unwrap();

        let conflict = store.push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "forged".into(),
            },
            mutation_receipt(
                &store,
                attachment.attachment_id,
                mutation_id,
                0,
                "sha256:different",
            ),
        );
        assert!(conflict
            .unwrap_err()
            .to_string()
            .contains("reused with a different command"));

        let changed_revision = store.push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "once".into(),
            },
            mutation_receipt(
                &store,
                attachment.attachment_id,
                mutation_id,
                99,
                "sha256:first",
            ),
        );
        assert!(changed_revision
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));
        let mut changed_environment = mutation_receipt(
            &store,
            attachment.attachment_id,
            mutation_id,
            0,
            "sha256:first",
        );
        changed_environment.environment_generation += 1;
        assert!(store
            .push_idempotent(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "once".into()
                },
                changed_environment,
            )
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));

        let stale = store.push_idempotent(
            "shared",
            "alice",
            BrainEventKind::Prompt {
                text: "stale".into(),
            },
            mutation_receipt(
                &store,
                attachment.attachment_id,
                uuid::Uuid::new_v4(),
                0,
                "sha256:stale",
            ),
        );
        assert!(stale
            .unwrap_err()
            .to_string()
            .contains("expected revision 0 but current revision is 1"));
        assert_eq!(store.snapshot("shared").unwrap().revision, 1);
    }

    #[test]
    fn schedule_creation_replays_the_original_identity_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let receipt = mutation_receipt(
            &store,
            attachment.attachment_id,
            uuid::Uuid::new_v4(),
            0,
            "sha256:schedule",
        );
        let created = store
            .create_schedule_with_receipt(
                "shared",
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Lisp,
                "(say \"once\")".into(),
                crate::vm::EffectSet::pure(),
                1_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
                Some(receipt.clone()),
            )
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(attachment.attachment_id),
            )
            .unwrap();
        let replayed = restarted
            .create_schedule_with_receipt(
                "shared",
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Lisp,
                "(say \"once\")".into(),
                crate::vm::EffectSet::pure(),
                1_000,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
                Some(receipt),
            )
            .unwrap();
        assert_eq!(replayed.schedule_id, created.schedule_id);
        assert_eq!(restarted.snapshot("shared").unwrap().schedules.len(), 1);
    }

    // ── #374: due-ordered selection instead of per-Brain enumeration ────────

    /// A Brain with one recurring schedule, created through the real API so the
    /// index is populated the way production populates it.
    fn seed_scheduled_brain(
        store: &BrainStore,
        name: &str,
        next_due_ms: u64,
    ) -> (AttachmentId, ScheduleId) {
        let attachment = store
            .attach(name, "alice", AttachmentRole::Driver, None)
            .unwrap();
        let schedule = store
            .create_schedule(
                name,
                "alice",
                attachment.attachment_id,
                ProgramLanguage::Lisp,
                "(say \"tick\")",
                crate::vm::EffectSet::pure(),
                next_due_ms,
                Some(1_000),
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        (attachment.attachment_id, schedule.schedule_id)
    }

    #[test]
    fn test_due_selection_names_only_brains_with_work() {
        // The property the index exists for. Selecting by Brain meant
        // enumerating and replaying the whole store once a second to discover
        // that nothing was due.
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "due-now", 1_000);
        seed_scheduled_brain(&store, "due-later", 9_000_000);

        let due = store.due_schedule_brains(2_000);

        assert_eq!(
            due,
            vec!["due-now".to_string()],
            "selection must name only Brains with schedules due at or before \
             the given instant; a Brain due far in the future must not be \
             woken, or the daemon is back to hydrating the whole store"
        );
    }

    #[test]
    fn test_due_selection_orders_across_brains_by_due_time_not_by_name() {
        // Ordering has to be global. Per-Brain sorting -- which is what
        // existed -- cannot express "this Brain's 10:00 comes before that
        // Brain's 09:00".
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "zulu", 1_000);
        seed_scheduled_brain(&store, "alpha", 5_000);

        let due = store.due_schedule_brains(10_000);

        assert_eq!(
            due,
            vec!["zulu".to_string(), "alpha".to_string()],
            "due order must follow next_due_ms across Brains, not Brain name: \
             'zulu' is due at 1000 and 'alpha' at 5000, so a name-ordered or \
             enumeration-ordered result would put them the other way round"
        );
    }

    #[test]
    fn test_a_schedule_due_exactly_now_is_selected() {
        // An exclusive upper bound would skip a schedule landing precisely on
        // the instant the loop woke for, deferring it a whole cycle.
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "boundary", 4_000);

        assert_eq!(
            store.due_schedule_brains(4_000),
            vec!["boundary".to_string()],
            "a schedule due at exactly the selection instant must be included"
        );
        assert!(
            store.due_schedule_brains(3_999).is_empty(),
            "and one due a millisecond later must not be"
        );
    }

    #[test]
    fn test_the_index_head_is_the_earliest_schedule_in_the_store() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            store.next_schedule_due_ms(),
            None,
            "an empty store has no head, which is what lets the delivery loop \
             wait instead of polling"
        );

        seed_scheduled_brain(&store, "later", 8_000);
        assert_eq!(store.next_schedule_due_ms(), Some(8_000));

        seed_scheduled_brain(&store, "sooner", 2_000);
        assert_eq!(
            store.next_schedule_due_ms(),
            Some(2_000),
            "a schedule created after the loop began sleeping must become the \
             head, or it fires late by the length of the previous sleep"
        );
    }

    #[test]
    fn test_cancelling_a_schedule_removes_it_from_the_index() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (attachment_id, schedule_id) = seed_scheduled_brain(&store, "shared", 1_000);
        assert_eq!(store.indexed_schedule_count(), 1);

        assert!(store
            .cancel_schedule("shared", "alice", attachment_id, schedule_id)
            .unwrap());

        assert_eq!(
            store.indexed_schedule_count(),
            0,
            "a cancelled schedule must leave the index, or the delivery loop \
             keeps waking for work that will never run"
        );
        assert_eq!(store.next_schedule_due_ms(), None);
        assert!(store.due_schedule_brains(u64::MAX).is_empty());
    }

    #[test]
    fn test_firing_a_schedule_advances_its_index_position() {
        // Remove-and-reinsert on fire. If the index kept the old key the loop
        // would re-select the same schedule forever.
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        seed_scheduled_brain(&store, "shared", 1_000);
        assert_eq!(store.next_schedule_due_ms(), Some(1_000));

        let queued = store.queue_due_schedules("shared", 1_500).unwrap();
        assert_eq!(queued.len(), 1, "the schedule was due and must have queued");

        assert_eq!(
            store.next_schedule_due_ms(),
            Some(2_000),
            "firing must advance the indexed position by the interval, or the \
             same occurrence is selected again on the next pass; the schedule \
             was due at 1000 with a 1000 ms interval"
        );
        assert_eq!(
            store.indexed_schedule_count(),
            1,
            "and advancing must replace the entry rather than add a second one"
        );
    }

    #[test]
    fn test_the_index_survives_a_store_restart_via_warm_up() {
        // Schedules are only known once a Brain is loaded, so a fresh daemon
        // starts with an empty index and must warm it once.
        let temp = tempfile::tempdir().unwrap();
        {
            let store = BrainStore::with_root("box.local", Some(temp.path().into()));
            seed_scheduled_brain(&store, "persisted", 3_000);
        }

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted.next_schedule_due_ms(),
            None,
            "a restarted store has hydrated nothing yet, so its index is empty \
             -- this is the state that makes the warm-up necessary"
        );

        restarted.warm_schedule_index().unwrap();

        assert_eq!(
            restarted.next_schedule_due_ms(),
            Some(3_000),
            "after warming, the persisted schedule is selectable again"
        );
        assert_eq!(
            restarted.due_schedule_brains(3_000),
            vec!["persisted".to_string()]
        );
    }

    #[test]
    fn test_warm_up_skips_a_brain_it_cannot_replay_and_keeps_the_rest() {
        // One unreadable Brain must not leave every other Brain unscheduled.
        // The naming, diagnostics and repair-retry semantics are #371's; what
        // is asserted here is only that the index still gets built.
        let temp = tempfile::tempdir().unwrap();
        {
            let store = BrainStore::with_root("box.local", Some(temp.path().into()));
            seed_scheduled_brain(&store, "healthy-one", 1_000);
            seed_scheduled_brain(&store, "healthy-two", 2_000);
        }
        let corrupt = temp.path().join("corrupt");
        std::fs::create_dir_all(&corrupt).unwrap();
        std::fs::write(corrupt.join("metadata.json"), "{not json").unwrap();

        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(
            store.list().is_err(),
            "negative control: this root genuinely defeats the enumerating \
             path, which is what made one bad Brain fatal to delivery"
        );

        store.warm_schedule_index().unwrap();

        assert_eq!(
            store.due_schedule_brains(5_000),
            vec!["healthy-one".to_string(), "healthy-two".to_string()],
            "both healthy Brains must still be scheduled, in due order, with \
             the unreplayable one simply absent"
        );
    }

    #[test]
    fn test_selection_does_not_hydrate_brains_without_due_work() {
        // The cost property, asserted structurally. A wall-clock assertion here
        // would be exactly the mistake #242 made four times.
        let temp = tempfile::tempdir().unwrap();
        {
            let seeding = BrainStore::with_root("box.local", Some(temp.path().into()));
            seed_scheduled_brain(&seeding, "has-work", 1_000);
            for index in 0..64 {
                seeding.snapshot(&format!("idle-{index:04}")).unwrap();
            }
        }

        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        store.warm_schedule_index().unwrap();

        let due = store.due_schedule_brains(2_000);

        assert_eq!(
            due,
            vec!["has-work".to_string()],
            "only the Brain with due work may be selected, out of 65 on disk;              selecting more means the delivery loop is about to hydrate Brains              that have nothing to do"
        );
        assert_eq!(
            store.indexed_schedule_count(),
            1,
            "the 64 idle Brains contribute no index entries, so selecting due \
             work is bounded by the number of schedules rather than by the \
             number of Brains"
        );
    }

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
        assert!(store
            .queue_due_schedules("shared", 3_500)
            .unwrap()
            .is_empty());

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
        assert_eq!(
            snapshot.runs.len(),
            1,
            "coalescing must reuse the queued run"
        );

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
        assert!(store
            .queue_due_schedules("shared", 5_000)
            .unwrap()
            .is_empty());

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
            store
                .snapshot("shared")
                .unwrap()
                .pending_schedule_dues
                .len(),
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
            store
                .inspect_schedule("shared", schedule.schedule_id)
                .unwrap(),
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
        assert!(
            !restarted
                .inspect_schedule("shared", schedule.schedule_id)
                .unwrap()
                .unwrap()
                .active
        );
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
    fn explicit_cancel_racing_disconnect_never_publishes_result_after_terminal() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "race".into(),
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
                BrainRunStatus::Running,
            )
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let cancel_store = store.clone();
        let cancel_barrier = barrier.clone();
        let run_id = run.run_id;
        let cancel = std::thread::spawn(move || {
            cancel_barrier.wait();
            cancel_store.transition_run(
                "shared",
                "alice",
                run_id,
                BrainRunStatus::Cancelled,
                Some("cancelled by initiating driver".into()),
            )
        });
        let disconnect_store = store.clone();
        let disconnect_barrier = barrier.clone();
        let disconnect = std::thread::spawn(move || {
            disconnect_barrier.wait();
            disconnect_store.terminalize_run_with_result_if_active(
                "shared",
                "daemon",
                run_id,
                prompt.seq,
                BrainRunStatus::Failed,
                "initiating Brain connection disconnected".into(),
            )
        });
        barrier.wait();
        let _ = cancel.join().unwrap();
        disconnect.join().unwrap().unwrap();

        let snapshot = store.snapshot("shared").unwrap();
        let terminal = snapshot
            .events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                        if event_run_id == run_id && status.is_terminal()
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1);
        assert!(!snapshot.events.iter().any(|event| {
            event.seq > terminal[0].seq
                && event.run_id == Some(run_id)
                && matches!(event.kind, BrainEventKind::Result { .. })
        }));
        assert!(
            snapshot
                .events
                .iter()
                .filter(|event| {
                    event.run_id == Some(run_id)
                        && matches!(event.kind, BrainEventKind::Result { .. })
                })
                .count()
                <= 1
        );
    }

    #[test]
    fn disconnect_terminal_batch_is_all_or_nothing_across_failure_and_restart() {
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
                    text: "crash".into(),
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
                BrainRunStatus::Running,
            )
            .unwrap();
        let before = store.snapshot("shared").unwrap().revision;
        store.fail_next_event_batch_for_test();
        assert!(store
            .terminalize_run_with_result_if_active(
                "shared",
                "daemon",
                run.run_id,
                prompt.seq,
                BrainRunStatus::Failed,
                "initiating Brain connection disconnected".into(),
            )
            .is_err());
        let failed_append = store.snapshot("shared").unwrap();
        assert_eq!(failed_append.revision, before);
        assert_eq!(
            failed_append
                .runs
                .iter()
                .find(|candidate| { candidate.run_id == run.run_id })
                .unwrap()
                .status,
            BrainRunStatus::Running
        );
        assert!(!failed_append.events.iter().any(|event| {
            event.run_id == Some(run.run_id) && matches!(event.kind, BrainEventKind::Result { .. })
        }));

        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let recovered = restarted.snapshot("shared").unwrap();
        assert_eq!(
            recovered
                .runs
                .iter()
                .find(|candidate| { candidate.run_id == run.run_id })
                .unwrap()
                .status,
            BrainRunStatus::Failed
        );
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|event| {
                    event.run_id == Some(run.run_id)
                        && matches!(event.kind, BrainEventKind::Result { .. })
                })
                .count(),
            1
        );
        assert_eq!(
            recovered
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                        if event_run_id == run.run_id && status.is_terminal()
                ))
                .count(),
            1
        );
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_terminal_retry_owner_survives_extended_transient_failure() {
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
                    text: "retry".into(),
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
                BrainRunStatus::Running,
            )
            .unwrap();
        store.fail_event_batches_for_test(12);
        let detail = "initiating Brain connection disconnected".to_string();
        assert!(store
            .terminalize_run_with_result_if_active(
                "shared",
                "daemon",
                run.run_id,
                prompt.seq,
                BrainRunStatus::Failed,
                detail.clone(),
            )
            .is_err());
        store.schedule_disconnect_terminalization_retry(
            "shared".into(),
            "daemon".into(),
            run.run_id,
            prompt.seq,
            BrainRunStatus::Failed,
            detail.clone(),
        );
        store.schedule_disconnect_terminalization_retry(
            "shared".into(),
            "daemon".into(),
            run.run_id,
            prompt.seq,
            BrainRunStatus::Failed,
            detail,
        );
        assert_eq!(store.pending_disconnect_terminalization_retries(), 1);
        let publication = store
            .acquire_run_publication("shared", run.run_id)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        assert_eq!(store.pending_disconnect_terminalization_retries(), 1);
        assert_eq!(
            store.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Running
        );
        drop(publication);
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                if store.inspect_run("shared", run.run_id).unwrap().status == BrainRunStatus::Failed
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(store.pending_disconnect_terminalization_retries(), 0);
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| {
                    event.run_id == Some(run.run_id)
                        && matches!(event.kind, BrainEventKind::Result { .. })
                })
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                        if event_run_id == run.run_id && status.is_terminal()
                ))
                .count(),
            1
        );
    }

    #[test]
    fn restart_ignores_newline_terminated_batch_with_invalid_checksum() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "committed".into(),
                },
            )
            .unwrap();
        let snapshot = store.snapshot("shared").unwrap();
        let next_seq = snapshot.revision + 1;
        let run_id = RunId(uuid::Uuid::new_v4());
        let result = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: snapshot.brain_id,
            seq: next_seq,
            environment_generation: snapshot.environment.generation,
            sender: "daemon".into(),
            created_ms: unix_millis(),
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::Result {
                request_seq: 1,
                output: String::new(),
                error: Some("torn disconnect".into()),
                continuation_messages: Vec::new(),
                invocation_metadata: None,
            },
        };
        let terminal = BrainEvent {
            seq: next_seq + 1,
            kind: BrainEventKind::RunStatusChanged {
                run_id,
                status: BrainRunStatus::Failed,
                detail: Some("torn disconnect".into()),
            },
            ..result.clone()
        };
        let encoded = serde_json::to_vec(&BrainJournalRecord::EventBatch {
            event_count: Some(2),
            payload_sha256: Some("invalid-checksum".into()),
            events: vec![result, terminal],
        })
        .unwrap();
        OpenOptions::new()
            .append(true)
            .open(temp.path().join("shared/events.jsonl"))
            .unwrap()
            .write_all(&[encoded.as_slice(), b"\n"].concat())
            .unwrap();
        drop(store);

        let recovered = BrainStore::with_root("box.local", Some(temp.path().into()));
        let recovered_snapshot = recovered.snapshot("shared").unwrap();
        assert_eq!(recovered_snapshot.revision, snapshot.revision);
        assert!(recovered_snapshot
            .events
            .iter()
            .all(|event| event.run_id != Some(run_id)));
    }

    #[tokio::test]
    async fn terminal_transition_and_archive_prune_run_publication_gates() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let (_, run) = store
            .accept_speculative_run("shared", "alice", attachment.attachment_id, "one".into())
            .unwrap();
        drop(
            store
                .acquire_run_publication("shared", run.run_id)
                .await
                .unwrap(),
        );
        assert_eq!(store.run_publication_gates.read().unwrap().len(), 1);
        store
            .transition_run(
                "shared",
                "daemon",
                run.run_id,
                BrainRunStatus::Cancelled,
                None,
            )
            .unwrap();
        assert!(store.run_publication_gates.read().unwrap().is_empty());

        let (_, second) = store
            .accept_speculative_run("shared", "alice", attachment.attachment_id, "two".into())
            .unwrap();
        drop(
            store
                .acquire_run_publication("shared", second.run_id)
                .await
                .unwrap(),
        );
        store.archive("shared").unwrap();
        assert!(store.run_publication_gates.read().unwrap().is_empty());
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
            store
                .inspect_run("shared", child.run_id)
                .unwrap()
                .parent_run_id,
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
        assert!(snapshot
            .events
            .iter()
            .all(|event| !matches!(event.kind, BrainEventKind::EffectRecorded { .. })));

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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let contract = store.initialization("shared").unwrap();
        let schedule = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
        let same = restarted
            .schedule_initialization(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
                20,
            )
            .unwrap();
        assert_eq!(same.schedule_id, schedule.schedule_id);
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(snapshot.schedules.len(), 1);
        assert_eq!(snapshot.runs.len(), 1);
        assert!(snapshot
            .events
            .iter()
            .all(|event| !matches!(event.kind, BrainEventKind::EffectRecorded { .. })));
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
            stale_store.schedule_initialization("shared", attachment_id, stale_connection, 10)
        });
        let current_store = store.clone();
        let current_barrier = barrier.clone();
        let accepted = std::thread::spawn(move || {
            current_barrier.wait();
            current_store.schedule_initialization("shared", attachment_id, current_connection, 10)
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
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
        assert!(
            !store
                .inspect_schedule("shared", delivered_ordinary.schedule_id)
                .unwrap()
                .unwrap()
                .active
        );
        assert_eq!(active_ordinary.module_identity, None);
        assert_eq!(cancelled_ordinary.module_identity, None);
        assert_eq!(delivered_ordinary.module_identity, None);

        let initialization = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let cancelled = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                20,
            )
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
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                30,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let first = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
        restarted
            .activate_connection(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
            )
            .unwrap();
        assert_eq!(
            restarted.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Interrupted
        );
        let retry = restarted
            .schedule_initialization(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
                20,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10_000,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let mut schedule = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
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
                    event["due"]["source"] =
                        serde_json::Value::String("(define (unreviewed-delivery) : int 2)".into());
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
        store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let schedule = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                10,
            )
            .unwrap();
        let run = store.queue_due_schedules("shared", 10).unwrap().remove(0);
        let delivered = store
            .schedule_initialization(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
                20,
            )
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
            .schedule_initialization(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
                30,
            )
            .unwrap();
        assert_eq!(completed.schedule_id, schedule.schedule_id);
        assert_eq!(restarted.snapshot("shared").unwrap().schedules.len(), 1);
    }

    #[tokio::test]
    async fn reviewed_initialization_module_is_typed_and_pure() {
        let store = BrainStore::with_root("box.local", None);
        let contract = store.initialization("reviewed").unwrap();
        let runtime = crate::runtime::ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some(format!("brain-initialization:{}", contract.source_sha256)),
                source: contract.source,
                intent: "reviewed Brain initialization module".into(),
                effect: crate::programs::ExecutionEffect::Pure,
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
        )
        .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted
            .initialization("tampered")
            .unwrap_err()
            .to_string()
            .contains("not the reviewed built-in module"));
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
            .detach("pending", first.attachment_id, first.connection_id.unwrap())
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
        assert!(snapshot.events[0].mutation.is_none());

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
    fn v12_and_v13_event_logs_restart_without_run_correlation() {
        for schema_version in [12, 13] {
            let temp = tempfile::tempdir().unwrap();
            let name = format!("legacy-v{schema_version}");
            let directory = temp.path().join(&name);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("events.jsonl"),
                format!(
                    "{{\"schema_version\":{schema_version},\"brain_id\":\"00000000-0000-0000-0000-000000000000\",\"seq\":1,\"environment_generation\":1,\"sender\":\"alice\",\"created_ms\":0,\"kind\":\"prompt\",\"text\":\"hello\"}}\n"
                ),
            )
            .unwrap();

            let store = BrainStore::with_root("box.local", Some(temp.path().into()));
            let snapshot = store.snapshot(&name).unwrap();
            assert_eq!(snapshot.events[0].schema_version, schema_version);
            assert_eq!(snapshot.events[0].run_id, None);
        }
    }

    #[test]
    fn correlated_run_started_json_roundtrip_uses_distinct_envelope_key() {
        let run_id = RunId::new();
        let run = BrainRun {
            run_id,
            kind: BrainRunKind::Speculative,
            parent_run_id: None,
            request_seq: 1,
            initiating_attachment_id: AttachmentId::new(),
            initiated_by: "alice".into(),
            status: BrainRunStatus::QueuedForEnvironment,
            started_ms: 1,
            updated_ms: 1,
            detail: None,
        };
        let event = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: BrainId::new(),
            seq: 2,
            environment_generation: 1,
            sender: "alice".into(),
            created_ms: 1,
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::RunStarted { run },
        };

        let encoded = serde_json::to_string(&event).unwrap();
        assert!(encoded.contains("\"correlation_run_id\""));
        let decoded: BrainEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn run_status_payload_id_remains_compatible_with_envelope_correlation() {
        let run_id = RunId::new();
        let legacy = serde_json::json!({
            "schema_version": 13,
            "brain_id": BrainId::nil(),
            "seq": 2,
            "environment_generation": 1,
            "sender": "daemon",
            "created_ms": 1,
            "kind": "run_status_changed",
            "run_id": run_id,
            "status": "cancelled"
        });
        let legacy: BrainEvent = serde_json::from_value(legacy).unwrap();
        assert_eq!(legacy.run_id, None);
        assert!(matches!(
            legacy.kind,
            BrainEventKind::RunStatusChanged {
                run_id: payload_run_id,
                status: BrainRunStatus::Cancelled,
                ..
            } if payload_run_id == run_id
        ));

        let current = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: BrainId::new(),
            seq: 2,
            environment_generation: 1,
            sender: "daemon".into(),
            created_ms: 1,
            run_id: Some(run_id),
            mutation: None,
            kind: BrainEventKind::RunStatusChanged {
                run_id,
                status: BrainRunStatus::Cancelled,
                detail: None,
            },
        };
        let encoded = serde_json::to_string(&current).unwrap();
        let decoded: BrainEvent = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, current);
    }

    #[test]
    fn schema_thirteen_without_mutation_receipts_remains_compatible() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let brain_id = store.snapshot("legacy-v13").unwrap().brain_id;
        drop(store);
        let mut event = journal_event(brain_id, 1, "legacy v13");
        event.schema_version = 13;
        std::fs::write(
            temp.path().join("legacy-v13/events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.snapshot("legacy-v13").unwrap().revision, 1);
    }

    #[test]
    fn schema_thirteen_cannot_silently_carry_mutation_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = store.snapshot("legacy-receipt").unwrap();
        drop(store);
        let mut event = journal_event(snapshot.brain_id, 1, "invalid legacy receipt");
        event.schema_version = 13;
        event.mutation = Some(BrainMutationReceipt {
            mutation_id: uuid::Uuid::new_v4(),
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            expected_revision: 0,
            environment_generation: snapshot.environment.generation,
            command_sha256: "sha256".into(),
        });
        std::fs::write(
            temp.path().join("legacy-receipt/events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let error = restarted.snapshot("legacy-receipt").unwrap_err();
        assert!(error
            .to_string()
            .contains("mutation receipt under legacy schema 13"));
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
            .acknowledge("shared", attachment.attachment_id, connection_id, head + 1,)
            .is_err());
        assert!(store
            .acknowledge("shared", attachment.attachment_id, connection_id, head - 1,)
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
            .activate_connection("shared", first.attachment_id, first.connection_id.unwrap())
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
            .detach("shared", first.attachment_id, first.connection_id.unwrap(),)
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
            .accept_runner_handoff("shared", "runner-c", handoff.handoff_id, generation, 60_000,)
            .is_err());
        let replacement = restarted
            .accept_runner_handoff("shared", "runner-b", handoff.handoff_id, generation, 60_000)
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
    fn handoff_replay_requires_exact_revision_and_environment_preconditions() {
        let mut store = BrainStore::with_root("box.local", None);
        let generation = store.environment().generation;
        let source = store
            .acquire_runner_lease("shared", "runner-a", generation, None, 60_000)
            .unwrap();
        let receipt = mutation_receipt(
            &store,
            AttachmentId::new(),
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "request-handoff",
        );
        let handoff = store
            .request_runner_handoff_with_receipt(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
                Some(receipt.clone()),
            )
            .unwrap();
        store.environment.generation += 1;
        assert_eq!(
            store
                .request_runner_handoff_with_receipt(
                    "shared",
                    "controller",
                    "runner-b",
                    source.lease_id,
                    generation,
                    30_000,
                    Some(receipt.clone()),
                )
                .unwrap(),
            handoff
        );
        let mut changed_revision = receipt.clone();
        changed_revision.expected_revision += 1;
        assert!(store
            .request_runner_handoff_with_receipt(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
                Some(changed_revision),
            )
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));
        let mut changed_environment = receipt;
        changed_environment.environment_generation += 1;
        assert!(store
            .request_runner_handoff_with_receipt(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                generation,
                30_000,
                Some(changed_environment),
            )
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));
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
            .accept_runner_handoff("shared", "runner-b", second.handoff_id, generation, 60_000,)
            .is_err());
    }

    #[test]
    fn cancelled_handoff_replays_success_after_response_loss_and_restart() {
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
        let receipt = mutation_receipt(
            &store,
            AttachmentId::new(),
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "cancel-handoff",
        );
        store
            .cancel_runner_handoff_with_receipt(
                "shared",
                handoff.handoff_id,
                "controller",
                Some(receipt.clone()),
            )
            .unwrap();
        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        restarted
            .cancel_runner_handoff_with_receipt(
                "shared",
                handoff.handoff_id,
                "controller",
                Some(receipt),
            )
            .unwrap();
        assert!(restarted
            .snapshot("shared")
            .unwrap()
            .runner_handoff
            .is_none());
    }

    #[tokio::test]
    async fn accepted_noop_mutations_consume_their_uuid_with_typed_outcomes() {
        let store = BrainStore::with_root("box.local", None);
        let attachment = AttachmentId::new();
        let schedule_id = ScheduleId::new();
        let schedule_receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            0,
            "cancel-missing-schedule",
        );
        assert!(!store
            .cancel_schedule_with_receipt(
                "shared",
                "alice",
                attachment,
                schedule_id,
                Some(schedule_receipt.clone()),
            )
            .unwrap());
        assert!(!store
            .cancel_schedule_with_receipt(
                "shared",
                "alice",
                attachment,
                schedule_id,
                Some(schedule_receipt.clone()),
            )
            .unwrap());
        let mut changed = schedule_receipt;
        changed.expected_revision += 1;
        assert!(
            store
                .cancel_schedule_with_receipt(
                    "shared",
                    "alice",
                    attachment,
                    schedule_id,
                    Some(changed),
                )
                .is_err()
        );

        let handoff_id = RunnerHandoffId::new();
        let handoff_receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            1,
            "cancel-missing-handoff",
        );
        store
            .cancel_runner_handoff_with_receipt(
                "shared",
                handoff_id,
                "alice",
                Some(handoff_receipt.clone()),
            )
            .unwrap();
        store
            .cancel_runner_handoff_with_receipt(
                "shared",
                handoff_id,
                "alice",
                Some(handoff_receipt),
            )
            .unwrap();

        let accepted = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "queued".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                accepted.seq,
                attachment,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "alice",
                run.run_id,
                BrainRunStatus::Cancelled,
                None,
            )
            .unwrap();
        let run_receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "cancel-run-again",
        );
        let reserved = store
            .reserve_run_cancellation(
                "shared",
                "alice",
                attachment,
                run.run_id,
                run_receipt.clone(),
            )
            .await
            .unwrap();
        assert!(!reserved.needs_runner_cancel);
        assert!(
            store
                .reserve_run_cancellation("shared", "alice", attachment, run.run_id, run_receipt,)
                .await
                .unwrap()
                .replayed
        );

        let missing_run = RunId::new();
        let missing_receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "cancel-missing-run",
        );
        assert!(store
            .reserve_run_cancellation(
                "shared",
                "alice",
                attachment,
                missing_run,
                missing_receipt.clone(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("does not exist"));
        assert!(store
            .reserve_run_cancellation(
                "shared",
                "alice",
                attachment,
                missing_run,
                missing_receipt.clone(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("does not exist or has already finished"));
        let mut changed = missing_receipt;
        changed.environment_generation += 1;
        assert!(store
            .reserve_run_cancellation("shared", "alice", attachment, missing_run, changed,)
            .await
            .unwrap_err()
            .to_string()
            .contains("different command or precondition"));
    }

    #[tokio::test]
    async fn run_cancellation_recovers_each_persisted_crash_boundary() {
        for boundary in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let store = BrainStore::with_root("box.local", Some(temp.path().into()));
            let attachment = AttachmentId::new();
            let accepted = store
                .push(
                    "shared",
                    "alice",
                    BrainEventKind::Prompt {
                        text: "cancel me".into(),
                    },
                )
                .unwrap();
            let run = store
                .start_run(
                    "shared",
                    "alice",
                    BrainRunKind::Interactive,
                    accepted.seq,
                    attachment,
                    BrainRunStatus::Running,
                )
                .unwrap();
            let receipt = mutation_receipt(
                &store,
                attachment,
                uuid::Uuid::new_v4(),
                store.snapshot("shared").unwrap().revision,
                "cancel-run-crash",
            );
            store
                .reserve_run_cancellation(
                    "shared",
                    "alice",
                    attachment,
                    run.run_id,
                    receipt.clone(),
                )
                .await
                .unwrap();
            if boundary >= 1 {
                store
                    .mark_run_cancellation_dispatching(
                        "shared",
                        "alice",
                        run.run_id,
                        receipt.mutation_id,
                    )
                    .unwrap();
            }
            if boundary >= 2 {
                store
                    .mark_run_cancellation_reconciled(
                        "shared",
                        "alice",
                        run.run_id,
                        receipt.mutation_id,
                    )
                    .unwrap();
            }
            drop(store);

            let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
            assert_eq!(
                restarted.inspect_run("shared", run.run_id).unwrap().status,
                BrainRunStatus::Cancelled
            );
            let replay = restarted
                .reserve_run_cancellation(
                    "shared",
                    "alice",
                    attachment,
                    run.run_id,
                    receipt.clone(),
                )
                .await
                .unwrap();
            assert!(replay.replayed);
            restarted
                .mark_run_cancellation_dispatching(
                    "shared",
                    "alice",
                    run.run_id,
                    receipt.mutation_id,
                )
                .unwrap();
            restarted
                .mark_run_cancellation_reconciled(
                    "shared",
                    "alice",
                    run.run_id,
                    receipt.mutation_id,
                )
                .unwrap();
            restarted
                .complete_reserved_run_cancellation("shared", "alice", run.run_id)
                .unwrap();
            assert_eq!(
                restarted.inspect_run("shared", run.run_id).unwrap().status,
                BrainRunStatus::Cancelled
            );
            assert!(
                !restarted
                    .reserve_run_cancellation(
                        "shared",
                        "alice",
                        attachment,
                        run.run_id,
                        receipt.clone(),
                    )
                    .await
                    .unwrap()
                    .needs_runner_cancel
            );
            let mut conflicting = receipt.clone();
            conflicting.expected_revision += 1;
            assert!(restarted
                .reserve_run_cancellation("shared", "alice", attachment, run.run_id, conflicting,)
                .await
                .unwrap_err()
                .to_string()
                .contains("different command or precondition"));
            let conflicting_uuid = BrainMutationReceipt {
                mutation_id: uuid::Uuid::new_v4(),
                ..receipt
            };
            let error = restarted
                .reserve_run_cancellation(
                    "shared",
                    "alice",
                    attachment,
                    run.run_id,
                    conflicting_uuid,
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("revision"), "{error:#}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reserved_cancellation_restart_retries_until_durable_terminal() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = AttachmentId::new();
        let accepted = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "cancel me".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                accepted.seq,
                attachment,
                BrainRunStatus::Running,
            )
            .unwrap();
        let receipt = mutation_receipt(
            &store,
            attachment,
            uuid::Uuid::new_v4(),
            store.snapshot("shared").unwrap().revision,
            "cancel-run-restart-retry",
        );
        store
            .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt.clone())
            .await
            .unwrap();
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        restarted.fail_cancellation_terminal_appends_for_test(5);
        restarted.list().unwrap();
        assert_eq!(restarted.pending_disconnect_terminalization_retries(), 1);
        assert_eq!(
            restarted.inspect_run("shared", run.run_id).unwrap().status,
            BrainRunStatus::Running
        );
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                if restarted.inspect_run("shared", run.run_id).unwrap().status
                    == BrainRunStatus::Cancelled
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(restarted.pending_disconnect_terminalization_retries(), 0);
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                        if event_run_id == run.run_id && status == BrainRunStatus::Cancelled
                ))
                .count(),
            1
        );
        assert!(!snapshot.events.iter().any(|event| {
            event.run_id == Some(run.run_id) && matches!(event.kind, BrainEventKind::Result { .. })
        }));
        assert!(
            restarted
                .reserve_run_cancellation("shared", "alice", attachment, run.run_id, receipt,)
                .await
                .unwrap()
                .replayed
        );
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
                capability: crate::vm::CapabilityKind::AgentSpawn,
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
                checkpoint_sha256, ..
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
            .commit_runner_runtime("dogfood", prompt.seq, outcome.output_revision, checkpoint)
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
        assert_eq!(
            snapshot.runner_lease.unwrap().lease_id,
            replacement.lease_id
        );
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
                    capability: crate::vm::CapabilityKind::AgentSpawn,
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
                    capability: crate::vm::CapabilityKind::AgentSpawn,
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
        assert_eq!(
            restored.capability_ledger().unwrap().grants.grants[0].id,
            grant_id
        );

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
            capability: crate::vm::CapabilityKind::AgentSpawn,
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
        denied.insert(crate::vm::CapabilityKind::AgentSpawn);
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
                capability: crate::vm::CapabilityKind::AgentSpawn,
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
            .submit_typed_only(submit(": square ( S n:int -- S int ! pure ) n n * ;", 0))
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
            .commit_runtime("brain", 3, legacy_commit.output_revision, &legacy_restarted)
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

    fn audit_effect(sequence: u64, text: &str) -> crate::vm::VmSideEffect {
        crate::vm::VmSideEffect {
            protocol_version: 1,
            sequence,
            requirement: crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            },
            output: Vec::new(),
            event: crate::vm::HostSideEffect::Emit { text: text.into() },
            origin: crate::vm::SourceOrigin::generated("effect-audit-store-test"),
        }
    }

    fn audit_run_fixture(
        store: &BrainStore,
    ) -> (BrainRun, BrainRunnerLease, EffectAuditAuthorityGrant) {
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "effect".into(),
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
                BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let grant = store
            .issue_effect_audit_authority("shared", run.run_id, lease.lease_id, None)
            .unwrap();
        (run, lease, grant)
    }

    #[test]
    fn effect_audit_permit_precedes_host_outcome_and_survives_turn_terminalization() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (run, _lease, grant) = audit_run_fixture(&store);
        let identity = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "write"))
            .unwrap();
        let permit = store.begin_effect_audit(&grant, identity).unwrap();
        store
            .transition_run(
                "shared",
                "daemon",
                run.run_id,
                BrainRunStatus::Cancelled,
                Some("conversation cancelled".into()),
            )
            .unwrap();
        store
            .finish_effect_audit(
                &grant,
                Some(&permit),
                identity,
                crate::runtime::effect_log::EffectAuditTerminalOutcome::Acknowledged {
                    response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
                },
            )
            .unwrap();
        let snapshot = store.snapshot("shared").unwrap();
        assert!(matches!(snapshot.effect_audits[0].state,
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                    ref outcome_kind
                }
            } if outcome_kind == "acknowledged"));
        let fence = store
            .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
                storage.replay.lookup(&identity)
            })
            .unwrap()
            .flatten()
            .unwrap();
        assert!(matches!(fence,
            crate::runtime::effect_log::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "acknowledged"));
        assert!(!snapshot
            .events
            .iter()
            .any(|event| matches!(event.kind, BrainEventKind::ToolResult { .. })));
    }

    #[test]
    fn effect_audit_archive_exhaustion_fails_before_reserve_or_host_permit() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (_run, _lease, grant) = audit_run_fixture(&store);
        store
            .exhaust_effect_audit_storage_for_test("shared")
            .unwrap();
        let error = store
            .reserve_effect_audit(
                &grant,
                uuid::Uuid::new_v4(),
                audit_effect(0, "must not execute"),
            )
            .unwrap_err();
        assert!(error.to_string().contains("storage exhausted"));
        assert!(
            store.snapshot("shared").unwrap().effect_audits.is_empty(),
            "storage exhaustion must fail before durable reserve or host permit"
        );
    }

    #[test]
    fn effect_audit_begin_is_linear_and_never_reissues_a_physical_permit() {
        let store = BrainStore::with_root("box.local", None);
        let (_run, _lease, grant) = audit_run_fixture(&store);
        let identity = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "once"))
            .unwrap();
        let _permit = store.begin_effect_audit(&grant, identity).unwrap();
        assert!(store
            .begin_effect_audit(&grant, identity)
            .unwrap_err()
            .to_string()
            .contains("already begun"));
    }

    #[test]
    fn effect_audit_observers_never_receive_secret_host_arguments() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (_run, _lease, grant) = audit_run_fixture(&store);
        for (sequence, capability, secret) in [
            (0, crate::vm::CapabilityKind::FileWrite, "FILE_SECRET_163"),
            (
                1,
                crate::vm::CapabilityKind::NetworkConnect,
                "NETWORK_SECRET_163",
            ),
            (
                2,
                crate::vm::CapabilityKind::ProcessRun,
                "PROCESS_SECRET_163",
            ),
        ] {
            let effect = crate::vm::VmSideEffect {
                protocol_version: 1,
                sequence,
                requirement: crate::vm::CapabilityRequirement {
                    capability,
                    selector: crate::vm::ResourceSelector::None,
                },
                output: Vec::new(),
                event: crate::vm::HostSideEffect::Request {
                    arguments: vec![crate::vm::TypedValue::String(secret.into())],
                },
                origin: crate::vm::SourceOrigin::generated(secret),
            };
            store
                .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), effect)
                .unwrap();
        }
        let private_digests = store.brains.read().unwrap()["shared"]
            .effect_audits
            .entries()
            .values()
            .map(|entry| entry.intent.canonical_sha256.clone())
            .collect::<Vec<_>>();
        let snapshot = store.snapshot("shared").unwrap();
        let snapshot_json = serde_json::to_string(&snapshot).unwrap();
        let journal = std::fs::read_to_string(temp.path().join("shared/events.jsonl")).unwrap();
        for secret in [
            "FILE_SECRET_163",
            "NETWORK_SECRET_163",
            "PROCESS_SECRET_163",
        ] {
            assert!(!snapshot_json.contains(secret), "snapshot leaked {secret}");
            assert!(
                !journal.contains(secret),
                "canonical audit journal leaked {secret}"
            );
        }
        assert!(snapshot.effect_audits.iter().all(|entry| entry
            .intent
            .canonical_sha256
            .is_empty()
            && entry.authority.authority_id == uuid::Uuid::nil()
            && entry.authority.runner_lease_id == uuid::Uuid::nil()));
        assert!(
            private_digests
                .iter()
                .all(|digest| !snapshot_json.contains(digest)),
            "ordinary snapshot leaked a private canonical replay digest"
        );
    }

    #[test]
    fn effect_audit_reconnect_reuses_request_authority_and_exact_reservation() {
        let store = BrainStore::with_root("box.local", None);
        let (run, lease, first) = audit_run_fixture(&store);
        let second = store
            .issue_effect_audit_authority(
                "shared",
                run.run_id,
                lease.lease_id,
                Some(ConnectionId(uuid::Uuid::new_v4())),
            )
            .unwrap();
        assert_eq!(first.authority, second.authority);
        assert_eq!(first.authority.connection_id, None);
        let execution_id = uuid::Uuid::new_v4();
        let effect = audit_effect(0, "replayed reserve");
        let first_identity = store
            .reserve_effect_audit(&first, execution_id, effect.clone())
            .unwrap();
        let replayed_identity = store
            .reserve_effect_audit(&second, execution_id, effect)
            .unwrap();
        assert_eq!(first_identity, replayed_identity);
    }

    #[test]
    fn effect_audit_request_end_reconciles_before_and_after_physical_boundary() {
        let store = BrainStore::with_root("box.local", None);
        let (_run, _lease, grant) = audit_run_fixture(&store);
        let unbegun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "before"))
            .unwrap();
        let begun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "after"))
            .unwrap();
        let _permit = store.begin_effect_audit(&grant, begun).unwrap();
        assert_eq!(store.reconcile_effect_audit_authority(&grant).unwrap(), 2);
        assert_eq!(store.reconcile_effect_audit_authority(&grant).unwrap(), 0);
        let snapshot = store.snapshot("shared").unwrap();
        assert!(snapshot.effect_audits.iter().any(|entry|
            entry.intent.identity == unbegun && matches!(entry.state,
                crate::runtime::effect_log::EffectAuditState::Terminal {
                    outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
                })));
        assert!(snapshot.effect_audits.iter().any(|entry|
            entry.intent.identity == begun && matches!(entry.state,
                crate::runtime::effect_log::EffectAuditState::Terminal {
                    outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss
                })));
    }

    #[test]
    fn effect_audit_connection_teardown_batch_is_exact_idempotent_and_retryable() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (_run, lease, grant) = audit_run_fixture(&store);
        let unbegun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "unbegun"))
            .unwrap();
        let begun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "begun"))
            .unwrap();
        let _permit = store.begin_effect_audit(&grant, begun).unwrap();

        assert_eq!(
            store
                .reconcile_effect_audits_for_disconnected_leases(
                    "shared",
                    &[RunnerLeaseId(uuid::Uuid::new_v4())],
                )
                .unwrap(),
            0,
            "a different connection lease must not own these audit identities"
        );
        store
            .fail_next_effect_audit_batch_for_test("shared")
            .unwrap();
        assert!(store
            .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
            .is_err());
        let failed = store.snapshot("shared").unwrap();
        assert!(failed.effect_audits.iter().any(|entry| {
            entry.intent.identity == unbegun
                && matches!(
                    entry.state,
                    crate::runtime::effect_log::EffectAuditState::IntentAccepted
                )
        }));
        assert!(failed.effect_audits.iter().any(|entry| {
            entry.intent.identity == begun
                && matches!(
                    entry.state,
                    crate::runtime::effect_log::EffectAuditState::AwaitingHostResult
                )
        }));

        assert_eq!(
            store
                .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
                .unwrap(),
            0,
            "an exact teardown retry must not append duplicate terminal receipts"
        );
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("shared").unwrap();
        assert!(snapshot
            .effect_audits
            .iter()
            .any(|entry| entry.intent.identity == unbegun
                && matches!(entry.state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "abandoned_not_applied")));
        assert!(snapshot
            .effect_audits
            .iter()
            .any(|entry| entry.intent.identity == begun
                && matches!(entry.state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "uncertain_process_loss")));
    }

    #[test]
    fn effect_audit_epochs_bound_active_history_and_fence_replay_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (_run, _lease, grant) = audit_run_fixture(&store);
        let mut completed = Vec::new();
        for sequence in 0..3 {
            let execution_id = uuid::Uuid::new_v4();
            let effect = audit_effect(sequence, "terminal");
            let identity = store
                .reserve_effect_audit(&grant, execution_id, effect.clone())
                .unwrap();
            let permit = store.begin_effect_audit(&grant, identity).unwrap();
            store
                .finish_effect_audit(
                    &grant,
                    Some(&permit),
                    identity,
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::Acknowledged {
                        response: crate::runtime::VmResumeResponse::Result { values: Vec::new() },
                    },
                )
                .unwrap();
            completed.push((identity, effect));
        }
        let unresolved = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(3, "unresolved"))
            .unwrap();
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(
            snapshot.effect_audits.len(),
            4,
            "ordinary snapshots retain a bounded redacted terminal tail plus unresolved state"
        );
        assert!(snapshot
            .effect_audits
            .iter()
            .any(|entry| entry.intent.identity == unresolved && !entry.state.is_terminal()));
        let journal = std::fs::read(temp.path().join("shared/events.jsonl")).unwrap();
        assert!(
            !String::from_utf8_lossy(&journal).contains("terminal"),
            "detailed audit rows do not force transcript-history rewrites"
        );
        drop(store);

        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(
            snapshot.effect_audits.len(),
            4,
            "restart reconstructs the bounded redacted terminal observer tail"
        );
        let (identity, effect) = &completed[0];
        assert_eq!(
            restarted
                .reserve_effect_audit(&grant, identity.execution_id, effect.clone(),)
                .unwrap(),
            *identity,
            "exact lost reserve ACK replays before stale run/lease rejection"
        );
        let mut changed_effect = effect.clone();
        changed_effect.event = crate::vm::HostSideEffect::Emit {
            text: "changed secret".into(),
        };
        assert!(restarted
            .reserve_effect_audit(&grant, identity.execution_id, changed_effect,)
            .unwrap_err()
            .to_string()
            .contains("conflicting"));
        let mut forged = grant.clone();
        forged.authority.authority_id = uuid::Uuid::new_v4();
        assert!(restarted
            .reserve_effect_audit(&forged, identity.execution_id, effect.clone(),)
            .unwrap_err()
            .to_string()
            .contains("conflicting"));
    }

    #[test]
    fn effect_audit_stale_successor_cannot_start_but_original_permit_can_finish() {
        let store = BrainStore::with_root("box.local", None);
        let (_run, lease, grant) = audit_run_fixture(&store);
        let identity = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "write"))
            .unwrap();
        let permit = store.begin_effect_audit(&grant, identity).unwrap();
        store
            .release_runner_lease("shared", lease.lease_id)
            .unwrap();
        store
            .acquire_runner_lease("shared", "successor", 1, None, 300_000)
            .unwrap();
        assert!(store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "forged"),)
            .unwrap_err()
            .to_string()
            .contains("successor"));
        store
            .finish_effect_audit(
                &grant,
                Some(&permit),
                identity,
                crate::runtime::effect_log::EffectAuditTerminalOutcome::FailedPartial {
                    detail: "host reported a partial write".into(),
                },
            )
            .unwrap();
    }

    #[test]
    fn effect_audit_restart_reconciles_without_reapplication() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let (_run, _lease, grant) = audit_run_fixture(&store);
        let unbegun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(0, "unbegun"))
            .unwrap();
        let begun = store
            .reserve_effect_audit(&grant, uuid::Uuid::new_v4(), audit_effect(1, "begun"))
            .unwrap();
        let _permit = store.begin_effect_audit(&grant, begun).unwrap();
        drop(store);
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(snapshot.effect_audits.len(), 2);
        let unbegun_fence = restarted
            .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
                storage.replay.lookup(&unbegun)
            })
            .unwrap()
            .flatten()
            .unwrap();
        let begun_fence = restarted
            .with_effect_audit_storage_mut("shared", snapshot.brain_id, |storage| {
                storage.replay.lookup(&begun)
            })
            .unwrap()
            .flatten()
            .unwrap();
        assert!(matches!(unbegun_fence,
            crate::runtime::effect_log::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "abandoned_not_applied"));
        assert!(matches!(begun_fence,
            crate::runtime::effect_log::EffectAuditTransition::Fence {
                ref outcome_kind, ..
            } if outcome_kind == "uncertain_process_loss"));
    }

    #[test]
    fn effect_audit_schema_v14_reconstructs_as_legacy_terminal_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let brain_id = store.snapshot("legacy-audit").unwrap().brain_id;
        drop(store);
        let event = BrainEvent {
            schema_version: 14,
            brain_id,
            seq: 1,
            environment_generation: 1,
            sender: "legacy-runner".into(),
            created_ms: 1,
            run_id: None,
            mutation: None,
            kind: BrainEventKind::EffectRecorded {
                request_seq: 1,
                execution_id: uuid::Uuid::new_v4(),
                effect: audit_effect(0, "legacy"),
                state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
            },
        };
        std::fs::write(
            temp.path().join("legacy-audit/events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("legacy-audit").unwrap();
        assert_eq!(snapshot.effect_audits.len(), 1);
    }

    #[test]
    fn future_effect_audit_schema_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let store = BrainStore::with_root("box.local", Some(temp.path().into()));
        let mut event = store
            .push(
                "future-audit",
                "alice",
                BrainEventKind::Prompt { text: "hi".into() },
            )
            .unwrap();
        drop(store);
        event.schema_version = BRAIN_EVENT_SCHEMA_VERSION + 1;
        std::fs::write(
            temp.path().join("future-audit/events.jsonl"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();
        let restarted = BrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted
            .snapshot("future-audit")
            .unwrap_err()
            .to_string()
            .contains("unsupported event schema version"));
    }
}
