//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

use super::attachment::sorted_attachments;
use super::attachment::{self};
pub use super::attachment::{
    AttachmentId, AttachmentRole, BrainApprovalAudience, BrainAttachment, ConnectionId,
};
pub(crate) use super::journal::BrainJournalRecord;
use super::journal::{
    self, backfill_legacy_speculative_run_correlation, initial_environment_generation,
    BRAIN_METADATA_VERSION,
};
pub(super) use super::journal::{create_dir_all_durable, sync_directory};
pub use super::journal::{
    BrainApprovalDecisionReservation, BrainEvent, BrainEventKind, BrainExecutableMutationAppend,
    BrainId, BrainMetadata, BrainMutationAppend, BrainMutationOutcome, BrainMutationReceipt,
    BrainProgram, BRAIN_EVENT_SCHEMA_VERSION,
};
use super::projection::{self, observer_effect_audit_event};
pub use super::projection::{
    BrainEnvironment, BrainListAgent, BrainListAttachment, BrainListSummary, BrainSnapshot,
    BrainWireMessage,
};
use super::run::sorted_runs;
use super::run::{self, validate_run_transition, DisconnectTerminalizationIntent};
pub use super::run::{
    BrainRun, BrainRunCancellationReservation, BrainRunKind, BrainRunStatus, BrainRunnerHandoff,
    BrainRunnerLease, RunId, RunnerHandoffId, RunnerLeaseId,
};
pub(crate) use super::schedule::DEFAULT_INITIALIZATION_SOURCE;
use super::schedule::{
    legacy_schedule_attachment_id, queued_schedule_run, schedule_due_window, sorted_schedule_dues,
    sorted_schedules, ScheduleIndex,
};
pub use super::schedule::{
    BrainInitialization, BrainSchedule, BrainScheduleDeliveryPolicy, BrainScheduleDue,
    BrainScheduleModuleIdentity, ProgramLanguage, ScheduleId,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Completed audit histories retained per named Brain. Together with the
/// bounded intent/outcome encodings this caps the audit projection and its
/// share of `events.jsonl`; unresolved write-ahead entries are never pruned.
const MAX_RETAINED_TERMINAL_EFFECT_AUDITS: usize = 128;

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
    effect_audits: crate::runtime::EffectAuditReducer,
    recent_effect_audits: std::collections::VecDeque<crate::runtime::EffectAuditEntry>,
    revision: u64,
    tx: broadcast::Sender<BrainEvent>,
}

#[cfg(test)]
thread_local! {
    /// Runs inside `prune_schedules_if_brain_is_absent`, in the window between
    /// the existence check answering "absent" and the index write guard being
    /// taken — the interval in which a concurrent `create_schedule` can index a
    /// schedule that the prune then forgets.
    ///
    /// A hook rather than a two-thread race because that window has no lock and
    /// no other observable boundary, so a `Barrier` alone cannot be placed
    /// inside it. It fires on the pruning thread, which is why a thread-local
    /// suffices: the racing creation runs on a *different* thread and must not
    /// see it.
    static PRUNE_GAP_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `body` with `hook` armed for the next prune gap on this thread. The hook
/// fires at most once; an unfired hook is disarmed when `body` returns.
#[cfg(test)]
pub(crate) fn run_with_prune_gap_hook<T>(hook: Box<dyn FnOnce()>, body: impl FnOnce() -> T) -> T {
    PRUNE_GAP_HOOK.with(|slot| *slot.borrow_mut() = Some(hook));
    let outcome = body();
    PRUNE_GAP_HOOK.with(|slot| *slot.borrow_mut() = None);
    outcome
}

/// A Brain with one recurring schedule, created through the real API so the
/// index is populated the way production populates it.
///
/// Lives here rather than in a test module because the production-boundary
/// tests for #383 (schedules for a deleted Brain are delivered, and delivery
/// recreates the Brain) are in `src/server/handlers.rs` and need the identical
/// fixture; two verbatim copies of it drifted apart once already.
#[cfg(test)]
pub(crate) fn seed_scheduled_brain_for_tests(
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

/// What is actually on disk under `path`, for assertion diagnostics. A
/// resurrection assertion has to say *what* it found, not merely that it found
/// something. Shared with the `src/server/handlers.rs` boundary tests for the
/// same reason as `seed_scheduled_brain_for_tests`.
#[cfg(test)]
pub(crate) fn directory_listing_for_tests(path: &std::path::Path) -> String {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            let mut names = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            names.sort();
            if names.is_empty() {
                "<empty directory>".to_string()
            } else {
                names.join(", ")
            }
        }
        Err(error) => format!("<not readable: {error}>"),
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
            effect_audits: crate::runtime::EffectAuditReducer::default(),
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
            BrainEventKind::ClientAttached { .. } | BrainEventKind::ClientDetached { .. } => {
                attachment::apply_event(&mut self.attachments, &event);
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
                let identity = crate::runtime::EffectAuditIdentity {
                    brain_id: self.brain_id.0,
                    run_id: event.run_id.unwrap_or(RunId(uuid::Uuid::nil())).0,
                    request_seq: *request_seq,
                    execution_id: *execution_id,
                    effect_sequence: effect.sequence,
                };
                let authority = crate::runtime::EffectAuditAuthority {
                    authority_id: uuid::Uuid::nil(),
                    runner_lease_id: uuid::Uuid::nil(),
                    runner_subject: "legacy-v14".into(),
                    connection_id: None,
                    environment_generation: event.environment_generation,
                };
                self.effect_audits
                    .apply(crate::runtime::EffectAuditTransition::Reserve {
                        intent: crate::runtime::EffectAuditIntent::from_effect(identity, effect)
                            .expect("legacy effect audit payload must be bounded"),
                        authority: authority.clone(),
                    })
                    .expect("legacy effect reservation must project");
                self.effect_audits
                    .apply(crate::runtime::EffectAuditTransition::Finish {
                        identity,
                        authority_id: authority.authority_id,
                        outcome: crate::runtime::EffectAuditTerminalOutcome::LegacyV14Snapshot {
                            state: state.clone(),
                        },
                    })
                    .expect("legacy effect terminal snapshot must project");
            }
        }
        self.events.push(event);
    }
}

/// Opaque daemon-side authority for one runner capability. The Cap'n Proto
/// peer never receives these fields; it can only invoke the capability that
/// holds this grant.
#[derive(Debug, Clone)]
pub(crate) struct EffectAuditAuthorityGrant {
    brain: String,
    run_id: RunId,
    authority: crate::runtime::EffectAuditAuthority,
}

pub(crate) fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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
    /// Brain-bound portable effect delivery logs. Distinct from the typed
    /// continuation journal and from write-ahead effect audit.
    delivery_logs:
        Arc<RwLock<HashMap<String, Arc<std::sync::Mutex<crate::runtime::VmEffectDeliveryLog>>>>>,
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
            delivery_logs: Arc::new(RwLock::new(HashMap::new())),
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

    /// On-disk directory that holds named Brain folders, if this store persists.
    pub fn root(&self) -> Option<&std::path::Path> {
        self.root.as_deref()
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

    /// Every Brain name this store would answer for, without hydrating any of
    /// them (#364).
    ///
    /// `list` calls `load_all`, which replays every Brain's whole event log,
    /// opens its three effect-audit SQLite databases and folds every event
    /// through the reducer. Callers that only need the set of names -- the
    /// unauthenticated health and status probes that gate `finch` startup --
    /// were paying a full store hydration for a directory question.
    ///
    /// The answer matches `list`'s in both membership and order for every
    /// Brain that loads: the same directories-only rule and the same
    /// `validate_name` filter `load_all` applies, over the same root, unioned
    /// with names already resident (an in-memory store has no root at all).
    /// Names are compared untrimmed exactly as `load_all` passes them to
    /// `ensure_loaded`, so a directory whose name only validates after a trim
    /// is reported under the name the loader would have used.
    ///
    /// Two differences are deliberate. This cannot fail, so one unreadable
    /// Brain no longer turns the whole probe into an error (see #344); and it
    /// counts a Brain directory whose replay would fail, which is the more
    /// truthful answer to "how many Brains are there". It also cannot create
    /// files, where `ensure_loaded` writes `metadata.json`, `initialization.json`
    /// and the effect-audit databases for a directory that lacks them.
    pub fn list_names_unhydrated(&self) -> Vec<String> {
        let mut names: std::collections::BTreeSet<String> = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .keys()
            .cloned()
            .collect();
        let Some(root) = &self.root else {
            return names.into_iter().collect();
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return names.into_iter().collect();
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if Self::validate_name(&name).is_ok() {
                names.insert(name);
            }
        }
        names.into_iter().collect()
    }

    /// How many Brains this store would answer for, without hydrating any.
    ///
    /// See [`BrainStore::list_names_unhydrated`]. This is what the health and
    /// status probes want; they were calling `list()?.len()`.
    pub fn count_unhydrated(&self) -> usize {
        self.list_names_unhydrated().len()
    }

    /// Per-Brain listing facts without hydrating any of them.
    ///
    /// Membership and order match [`BrainStore::list_names_unhydrated`]. Each
    /// summary is read from `metadata.json` and `events.jsonl` when those
    /// files already exist; missing or unreadable files yield empty counters
    /// rather than an error, and the journal is never truncated. This does
    /// not create `metadata.json`, `initialization.json`, or effect-audit
    /// databases, and it does not fold events through the reducer.
    pub fn list_summaries_unhydrated(&self) -> Vec<BrainListSummary> {
        self.list_names_unhydrated()
            .into_iter()
            .map(|name| self.summarize_unhydrated(&name))
            .collect()
    }

    fn summarize_unhydrated(&self, name: &str) -> BrainListSummary {
        projection::summarize_unhydrated(self.root.as_deref(), name, unix_millis())
    }

    /// How many Brains are actually resident in memory, i.e. hydrated.
    ///
    /// Test-only. This is the observable that separates counting from loading:
    /// a probe that hydrates leaves Brains here, and one that does not leaves
    /// zero (#364).
    #[cfg(test)]
    pub(crate) fn resident_brain_count(&self) -> usize {
        self.brains
            .read()
            .expect("shared brain lock poisoned")
            .len()
    }

    /// Give a Brain `events` journal entries, through the ordinary append path.
    ///
    /// Test-only. Hand-written JSON does not survive `ensure_loaded`'s identity
    /// and sequence validation, so a fixture that needs real history has to be
    /// built with the real writer.
    #[cfg(test)]
    pub(crate) fn seed_history_for_test(&self, name: &str, events: u64) -> Result<()> {
        let brain_id = self.snapshot(name)?.brain_id;
        for seq in 1..=events {
            self.append_event(
                name,
                &BrainEvent {
                    schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                    brain_id,
                    seq,
                    environment_generation: 1,
                    sender: "seed".into(),
                    created_ms: seq,
                    run_id: None,
                    mutation: None,
                    kind: BrainEventKind::Prompt {
                        text: "seed".into(),
                    },
                },
            )?;
        }
        Ok(())
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
                .map(crate::runtime::EffectAuditEntry::observer_projection)
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
            authority: crate::runtime::EffectAuditAuthority {
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
                        crate::runtime::EffectAuditState::IntentAccepted => Some((
                            entry.intent.identity,
                            entry.authority.authority_id,
                            crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied,
                        )),
                        crate::runtime::EffectAuditState::AwaitingHostResult => Some((
                            entry.intent.identity,
                            entry.authority.authority_id,
                            crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss,
                        )),
                        crate::runtime::EffectAuditState::Terminal { .. } => None,
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let transitions = unresolved
            .iter()
            .map(|(identity, authority_id, outcome)| {
                crate::runtime::EffectAuditTransition::Finish {
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
        let unresolved = state
            .effect_audits
            .entries()
            .values()
            .filter_map(|entry| {
                (entry.intent.identity.run_id == grant.run_id.0
                    && entry.authority.authority_id == grant.authority.authority_id)
                    .then(|| match &entry.state {
                        crate::runtime::EffectAuditState::IntentAccepted => Some((
                            entry.intent.identity,
                            crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied,
                        )),
                        crate::runtime::EffectAuditState::AwaitingHostResult if process_lost => {
                            Some((
                                entry.intent.identity,
                                crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss,
                            ))
                        }
                        crate::runtime::EffectAuditState::AwaitingHostResult => None,
                        crate::runtime::EffectAuditState::Terminal { .. } => None,
                    })
                    .flatten()
            })
            .collect::<Vec<_>>();
        let transitions = unresolved
            .iter()
            .map(
                |(identity, outcome)| crate::runtime::EffectAuditTransition::Finish {
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
    ) -> Result<Option<crate::runtime::EffectAuditIdentity>> {
        let name = Self::validate_name(&grant.brain)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).context("Brain was removed concurrently")?;
        let run = state
            .runs
            .get(&grant.run_id)
            .context("Brain run does not exist")?;
        let identity = crate::runtime::EffectAuditIdentity {
            brain_id: state.brain_id.0,
            run_id: grant.run_id.0,
            request_seq: run.request_seq,
            execution_id,
            effect_sequence: effect.sequence,
        };
        let transition = crate::runtime::EffectAuditTransition::Reserve {
            intent: crate::runtime::EffectAuditIntent::from_effect(identity, effect)?,
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
            let mut archived = crate::runtime::EffectAuditReducer::default();
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
    ) -> Result<crate::runtime::EffectAuditIdentity> {
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
        let identity = crate::runtime::EffectAuditIdentity {
            brain_id: state.brain_id.0,
            run_id: grant.run_id.0,
            request_seq: run.request_seq,
            execution_id,
            effect_sequence: effect.sequence,
        };
        let transition = crate::runtime::EffectAuditTransition::Reserve {
            intent: crate::runtime::EffectAuditIntent::from_effect(identity, &effect)?,
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
            let mut archived = crate::runtime::EffectAuditReducer::default();
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
        identity: crate::runtime::EffectAuditIdentity,
    ) -> Result<crate::runtime::HostEffectPermit> {
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
                    crate::runtime::EffectAuditState::IntentAccepted
                )),
            "effect audit reservation was already begun or terminalized"
        );
        self.append_effect_audit_transition_locked(
            name,
            state,
            crate::runtime::EffectAuditTransition::Begin {
                identity,
                authority_id: grant.authority.authority_id,
            },
        )?;
        Ok(crate::runtime::HostEffectPermit::new(
            identity,
            grant.authority.authority_id,
        ))
    }

    /// Record a monotonic outcome. Current lease state is deliberately
    /// irrelevant to the original detached owner's already-begun effect.
    pub(crate) fn finish_effect_audit(
        &self,
        grant: &EffectAuditAuthorityGrant,
        permit: Option<&crate::runtime::HostEffectPermit>,
        identity: crate::runtime::EffectAuditIdentity,
        outcome: crate::runtime::EffectAuditTerminalOutcome,
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
                    crate::runtime::EffectAuditTerminalOutcome::NotApplied { .. }
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
                        crate::runtime::EffectAuditState::IntentAccepted
                    )),
                "begun effect outcome requires its durable host permit"
            );
        }
        self.append_effect_audit_transition_locked(
            name,
            state,
            crate::runtime::EffectAuditTransition::Finish {
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
        transition: crate::runtime::EffectAuditTransition,
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
                    crate::runtime::EffectAuditTransition::Reserve { .. }
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
        transitions: Vec<crate::runtime::EffectAuditTransition>,
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
        terminal: &[(u64, crate::runtime::EffectAuditIdentity)],
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
            fences.push((*seq, crate::runtime::replay_fence_transition(entry)?));
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
                crate::runtime::EffectAuditTransition::Reserve { .. } => {}
                crate::runtime::EffectAuditTransition::Begin { .. } => {}
                crate::runtime::EffectAuditTransition::Finish { identity, .. } => {
                    let mut compacted = event.clone();
                    let entry = state
                        .effect_audits
                        .get(identity)
                        .context("terminal effect audit disappeared during compaction")?;
                    compacted.kind = BrainEventKind::EffectAuditTransition {
                        transition: crate::runtime::replay_fence_transition(entry)?,
                    };
                    anyhow::ensure!(
                        serde_json::to_vec(&compacted)?.len()
                            <= crate::runtime::MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES,
                        "effect audit replay fence event exceeds its fixed encoded bound"
                    );
                    retained_events.push(compacted);
                }
                crate::runtime::EffectAuditTransition::Fence { .. } => {
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

    /// Bring the due index into line with one Brain's current schedules.
    ///
    /// Called wherever a schedule is created, cancelled, or advanced, and when
    /// a Brain becomes resident. Takes the state the caller already holds, so
    /// it never hydrates anything itself.
    fn reindex_schedules_locked(&self, name: &str, state: &BrainState) {
        // Both reads happen inside the single write critical section. An
        // earlier version sampled the head under a read guard, released it, and
        // then took the write guard -- so a writer that blocked on the write
        // lock compared a stale `before` against a fresh `after` and could
        // conclude the head had not moved earlier when it had. The schedule it
        // inserted then waited out the loop's existing sleep, up to the 60 s
        // ceiling, silently.
        let moved_earlier = {
            let mut index = self
                .schedule_index
                .write()
                .expect("schedule index lock poisoned");
            let earliest_before = index.next_due_ms();
            index.reindex(name, &state.schedules);
            let earliest_after = index.next_due_ms();
            // Only wake the delivery loop when the head actually moved earlier.
            // A schedule created far in the future must not interrupt a sleep
            // it does not shorten.
            match (earliest_before, earliest_after) {
                (Some(before), Some(after)) => after < before,
                (None, Some(_)) => true,
                _ => false,
            }
        };
        if moved_earlier {
            // `notify_one`, not `notify_waiters`: the latter stores no permit,
            // so a notification sent while the loop is between reading the head
            // and registering its waiter is simply lost -- which is the
            // expected interleaving under contention, not a rare race, because
            // a writer blocked on this lock resumes exactly when the loop
            // releases it. `notify_one` stores a permit, so the wake survives
            // the window. A spurious extra wake costs one re-read of the head.
            self.schedule_wakeup.notify_one();
        }
    }

    /// Move one schedule in the due index, waking the loop if it became the
    /// head. The per-event path: O(log n), touching only this schedule.
    fn upsert_schedule_locked(&self, name: &str, schedule: &BrainSchedule) {
        let moved_earlier = {
            let mut index = self
                .schedule_index
                .write()
                .expect("schedule index lock poisoned");
            let earliest_before = index.next_due_ms();
            index.upsert(name, schedule);
            let earliest_after = index.next_due_ms();
            match (earliest_before, earliest_after) {
                (Some(before), Some(after)) => after < before,
                (None, Some(_)) => true,
                _ => false,
            }
        };
        if moved_earlier {
            self.schedule_wakeup.notify_one();
        }
    }

    /// Drop the in-memory copy of a Brain without touching its durable state
    /// or its index entries. Returns whether it was resident. Test-only.
    ///
    /// "Indexed but not resident" is the state the due index exists to make
    /// possible -- selecting a Brain *without* hydrating it is the whole point
    /// of the due-ordered index (#374, selecting due schedules from a
    /// due-ordered index) -- and it is the state in which
    /// `ensure_loaded` -> `load_or_create_metadata` actually runs, which is the
    /// step that mints a new `BrainId` for a Brain whose `metadata.json` is
    /// gone. `warm_schedule_index` happens to hydrate as a side effect of how
    /// it discovers schedules, so without this the recreation half of #383
    /// (schedules for a deleted Brain are delivered, and delivery recreates the
    /// Brain) is
    /// unreachable from a test and delivery would be silently relying on that
    /// accident. Lives on the store rather than in one test module because the
    /// production-boundary test in `src/server/handlers.rs` needs it too.
    #[cfg(test)]
    pub(crate) fn evict_resident_brain_for_tests(&self, name: &str) -> bool {
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .remove(name)
            .is_some()
    }

    /// Whether a Brain's state is currently held in memory. Test-only; the
    /// counterpart to `evict_resident_brain_for_tests`.
    #[cfg(test)]
    pub(crate) fn is_resident_for_tests(&self, name: &str) -> bool {
        self.brains
            .read()
            .expect("shared brain lock poisoned")
            .contains_key(name)
    }

    /// Drop everything the index knows about a Brain that no longer exists.
    fn forget_schedules_locked(&self, name: &str) {
        self.schedule_index
            .write()
            .expect("schedule index lock poisoned")
            .forget(name);
    }

    /// Whether the Brain's durable state is gone, dropping its index entries if
    /// it is. `true` means delivery must skip it rather than load it.
    ///
    /// `forget_schedules_locked` has only ever been reachable from
    /// `remove_if_unused` and `archive` -- the two removals the daemon performs
    /// itself -- while `warm_schedule_index` only ever *adds*. A Brain that left
    /// by any other route (a directory deleted by hand, a volume unmounted, a
    /// restore that dropped one) therefore kept its index entry, and delivering
    /// against that entry *recreated* it: `queue_due_schedules` ->
    /// `ensure_loaded` -> `load_or_create_metadata` writes a fresh
    /// `metadata.json` with a **new** `BrainId`, and `append_journal_value`
    /// recreates the directory around the events it appends. The Brain came
    /// back as an empty one under a different identity (#383). Pruning here,
    /// lazily at delivery, is the hook an external deletion never had.
    ///
    /// The check is deliberately **existence**, not "did the load fail". Those
    /// are different faults with opposite handling:
    ///
    /// - *Absent* -- nothing at `root/name` -- means the Brain is gone. Prune.
    /// - *Unreadable* -- the directory stands but cannot be replayed -- is a
    ///   corruption to report and repair (#371, #377, #379). Pruning it would
    ///   silently retire the schedules of a Brain that still exists and hide
    ///   exactly the failure those issues exist to surface, so this returns
    ///   `false` for it and lets `ensure_loaded`'s error propagate to the
    ///   delivery loop's warning (#380's transition logging).
    /// - *Unknown* -- the existence check itself failed (EACCES on the Brain
    ///   root, ESTALE from a stale network handle, EIO from a failing disk) --
    ///   is not evidence of absence, so it is handled as unreadable rather than
    ///   as gone. This is why the check is `try_exists` and not `exists`: the
    ///   latter collapses every error into `false` and would prune a Brain that
    ///   is sitting right there.
    ///
    /// A store with no root keeps no durable state at all, so durable absence
    /// is not a concept there and nothing is ever pruned for it.
    ///
    /// Pruning is idempotent and loses nothing permanently: the index is
    /// derived from the log, so a directory that reappears is picked up by the
    /// next `warm_schedule_index`. That is also what bounds the one race this
    /// leaves open — a `create_schedule` that lands between the existence check
    /// and the forget has its brand-new index entry dropped, and gets it back on
    /// the next warm rather than losing it. See the comment on the check itself
    /// for why that trade is taken instead of holding the guard across a `stat`.
    fn prune_schedules_if_brain_is_absent(&self, name: &str) -> bool {
        let Some(root) = &self.root else {
            return false;
        };
        // The `stat` runs *outside* the index write guard, deliberately, and
        // this is the one decision here worth arguing.
        //
        // Under the guard it would order a prune against a concurrent
        // `create_schedule` exactly: `push_locked` recreates the directory
        // (`append_event` -> `append_journal_value` -> `create_dir_all_durable`)
        // before it takes this same lock in `upsert_schedule_locked`, so
        // whichever side won the lock would be right. That was tried, and
        // rejected: it makes a filesystem call block the store.
        //
        // `stat` is not bounded. On a Brain root that is an unresponsive NFS or
        // SMB mount it blocks for as long as the mount is wedged. Held across
        // the index write guard, that stalls `warm_schedule_index` (which takes
        // `brains.read()` and then this guard), and every `brains.write()`
        // caller -- `push`, `create_schedule`, `attach`, `transition_run`, every
        // mutating HTTP handler, for *every* Brain -- then queues behind that
        // read guard. One unresponsive directory freezes the whole store, with
        // no timeout to end it.
        //
        // This is a hazard the guard would *create*, not one it would widen:
        // `ensure_loaded` does all of its filesystem work before it takes
        // `brains.write()` at the end, so a hung mount there blocks only the
        // calling task and holds no store lock at all. An earlier revision of
        // this comment claimed the reverse; it was wrong.
        //
        // Outside the guard the interleaving [stat says absent] ->
        // [create_schedule recreates and indexes] -> [prune forgets] can drop a
        // just-created entry. That loss is **bounded**: the schedule is durable
        // in the log either way, the creation has put the directory back on
        // disk, and the next `warm_schedule_index` re-indexes the Brain and
        // restores it -- 60 s at worst, once, with no operator action. Bounded
        // index staleness in a rare race is the better trade against an
        // unbounded, silent, process-wide freeze; a daemon must stay responsive
        // when a mount does not.
        // (`test_a_schedule_created_during_the_prune_gap_is_restored_by_the_next_warm`
        // pins that bound.)
        match root.join(name).try_exists() {
            // Nothing is there: the Brain is gone. Prune.
            Ok(false) => {}
            // Present, or *unknown*. `Path::exists` cannot tell those apart --
            // it is `metadata().is_ok()`, so EACCES on the Brain root, ESTALE
            // from a stale NFS handle and EIO from a failing disk all read as
            // "absent" and would silently retire the schedules of a Brain that
            // is still on disk, which is the exact silencing this function was
            // written to prevent. `try_exists` distinguishes them: an `Err`
            // means the answer is unknown, so fall through to `ensure_loaded`
            // and let its error propagate to the delivery loop's warning, the
            // same handling an unreadable Brain already gets. `try_exists`
            // follows symlinks exactly as `exists` does, so this changes
            // nothing for them.
            Ok(true) | Err(_) => return false,
        }
        // The prune gap: no lock is held here, which is the point above and the
        // window the test hook reproduces.
        #[cfg(test)]
        if let Some(hook) = PRUNE_GAP_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        let forgotten = {
            let mut index = self
                .schedule_index
                .write()
                .expect("schedule index lock poisoned");
            let was_indexed = index.has_active(name);
            index.forget(name);
            was_indexed
        };
        if forgotten {
            // Bounded by construction rather than by a rate limit: the entries
            // that named this Brain are gone, so it cannot be selected again
            // until a warm finds the directory back on disk.
            tracing::warn!(
                brain = %name,
                "a due schedule named a Brain whose directory is gone; dropping its \
                 index entries instead of recreating the Brain"
            );
        }
        true
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
    ///
    /// It is also the repair for a pruned entry: a Brain that is already
    /// resident is re-indexed here, because `ensure_loaded` short-circuits on
    /// residency before it would do it (#383, schedules for a deleted Brain
    /// are delivered, and delivery recreates the Brain).
    pub fn warm_schedule_index(&self) {
        // Infallible by construction: an absent root and a Brain that cannot be
        // replayed are both ordinary states, not errors. It previously returned
        // `Result` and never an `Err`, so the caller's error branch and its
        // diagnostic were unreachable — a promise of a warning that could not
        // fire.
        let Some(root) = &self.root else {
            return;
        };
        let entries = match std::fs::read_dir(root) {
            // No Brain root yet is not a failure; it is a daemon with no Brains.
            Ok(entries) => entries,
            Err(_) => return,
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
                continue;
            }
            // Re-index a Brain the index does not know about, not only one this
            // warm loaded. `ensure_loaded` returns `Ok(())` at its first line
            // for a Brain that is already resident, *before* the
            // `reindex_schedules_locked` on its load path -- so entries enter
            // the index only when a Brain becomes resident or when a schedule
            // event fires, and `brains` entries are dropped only by
            // `remove_if_unused` and `archive`, which forget the schedules too.
            // "Indexed" therefore implied "resident", and
            // `prune_schedules_if_brain_is_absent` broke that: a resident Brain
            // whose directory blinked out is left resident-but-unindexed with
            // no route back short of a restart. Because this warm hydrates
            // every Brain at startup, every Brain is resident, so one blip of
            // the Brain root -- a removable or network volume gone for two
            // seconds -- would empty the whole index permanently and the daemon
            // would silently deliver nothing again, ever. Reindexing here is
            // that route back, and it is the repair for the prune-gap race in
            // `prune_schedules_if_brain_is_absent` as well.
            //
            // Gated on `is_indexed`, because `reindex` is O(every schedule the
            // Brain has ever held): `BrainState::apply` only ever *inserts*
            // into `state.schedules`, so cancelled schedules and spent
            // one-shots stay there for the Brain's lifetime. Running it
            // unconditionally would scan all of that for every Brain once a
            // minute, under the index write guard -- the same unbounded curve
            // `ScheduleIndex::upsert` was written to get off, at 1/60 Hz
            // instead of once per schedule event. The gate is O(1) and exact:
            // `reindex` is the only thing that marks a Brain known and `forget`
            // the only thing that unmarks it, so "not indexed" is precisely
            // "pruned, removed, or never loaded" -- the cases that need the
            // scan -- and nothing else.
            //
            // Reindexing is idempotent regardless: an unchanged schedule set
            // leaves the head where it was, so no spurious wake is sent.
            let brains = self.brains.read().expect("shared brain lock poisoned");
            let Some(state) = brains.get(&name) else {
                continue;
            };
            let already_indexed = self
                .schedule_index
                .read()
                .expect("schedule index lock poisoned")
                .is_indexed(&name);
            if already_indexed {
                continue;
            }
            self.reindex_schedules_locked(&name, state);
        }
        if skipped > 0 {
            tracing::warn!(
                skipped,
                indexed = self.indexed_schedule_count(),
                "some Brains were skipped while warming the schedule index; \
                 they will be retried on the next warm"
            );
        }
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
            .len()
    }

    /// Atomically advance due schedules and append the exact queued ProgramRun
    /// for each delivery. The returned runs are durable before this method
    /// returns and are safe for the runner broker to dispatch immediately.
    pub fn queue_due_schedules(&self, name: &str, now_ms: u64) -> Result<Vec<BrainRun>> {
        let name = Self::validate_name(name)?;
        // Before the load, not after a failed one: a Brain that is gone is
        // pruned, a Brain that is merely unreadable is reported (#383).
        if self.prune_schedules_if_brain_is_absent(name) {
            return Ok(Vec::new());
        }
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
                        let run = queued_schedule_run(&schedule, state.revision + 1, now_ms);
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
                        let run = queued_schedule_run(&schedule, state.revision + 1, now_ms);
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
        self.forget_delivery_log(name);
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
        // Without this the index keeps pointing at an archived Brain, the
        // delivery loop selects it, and `queue_due_schedules` -> `ensure_loaded`
        // -> `load_or_create_metadata` recreates the directory with a *new*
        // BrainId. Archiving a Brain that had an active schedule would silently
        // resurrect it as an empty one.
        self.forget_schedules_locked(name);
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .remove(name);
        self.forget_delivery_log(name);
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
        Ok(journal::replay_mutation(&state.events, receipt)?.cloned())
    }

    fn push_idempotent_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
        receipt: BrainMutationReceipt,
    ) -> Result<BrainMutationAppend> {
        if let Some(existing) = journal::replay_mutation(&state.events, &receipt)? {
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
        // Which schedule this event moves, if any. Every creation,
        // cancellation and advance reaches state through these two kinds, so
        // the index has one maintenance point rather than one per call site.
        let touched = match &event.kind {
            BrainEventKind::ScheduleChanged { schedule } => Some(schedule.schedule_id),
            BrainEventKind::ScheduleDue { due } => Some(due.schedule_id),
            _ => None,
        };
        state.apply(event.clone());
        // Targeted, not a whole-Brain rescan: `state.schedules` is never pruned,
        // so rescanning cost every schedule the Brain had ever held, inside the
        // process-wide `brains` write guard.
        if let Some(schedule_id) = touched {
            if let Some(schedule) = state.schedules.get(&schedule_id).cloned() {
                self.upsert_schedule_locked(name, &schedule);
            }
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
            self.bind_runtime_delivery_log(name, &runtime)?;
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
        self.bind_runtime_delivery_log(name, &runtime)?;
        let mut runtimes = self
            .runtimes
            .write()
            .expect("shared brain runtime lock poisoned");
        Ok(runtimes
            .entry(name.to_string())
            .or_insert_with(|| Arc::clone(&runtime))
            .clone())
    }

    fn forget_delivery_log(&self, name: &str) {
        self.delivery_logs
            .write()
            .expect("shared brain delivery-log lock poisoned")
            .remove(name);
        self.effect_audit_storage
            .lock()
            .expect("effect-audit storage map poisoned")
            .remove(name);
    }

    fn bind_runtime_delivery_log(
        &self,
        name: &str,
        runtime: &crate::runtime::ProgramRuntime,
    ) -> Result<()> {
        if runtime.effect_delivery_log().is_some() {
            return Ok(());
        }
        if let Some(log) = self.effect_delivery_log(name)? {
            runtime.bind_effect_delivery_log(log)?;
        }
        Ok(())
    }

    /// Open or reuse the Brain-bound portable effect delivery log.
    pub fn effect_delivery_log(
        &self,
        name: &str,
    ) -> Result<Option<Arc<std::sync::Mutex<crate::runtime::VmEffectDeliveryLog>>>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(log) = self
            .delivery_logs
            .read()
            .expect("shared brain delivery-log lock poisoned")
            .get(name)
            .cloned()
        {
            return Ok(Some(log));
        }
        let Some(root) = &self.root else {
            return Ok(None);
        };
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .map(|state| state.brain_id.0)
            .context("Brain was removed concurrently")?;
        let path = root.join(name).join("runtime").join("effects.jsonl");
        let log = Arc::new(std::sync::Mutex::new(
            crate::runtime::VmEffectDeliveryLog::open_bound(path, brain_id)?,
        ));
        let mut logs = self
            .delivery_logs
            .write()
            .expect("shared brain delivery-log lock poisoned");
        Ok(Some(
            logs.entry(name.to_string())
                .or_insert_with(|| Arc::clone(&log))
                .clone(),
        ))
    }

    /// Persist envelopes before local Brain handling. Exact replay is
    /// idempotent; conflicting `(execution_id, sequence)` fails closed.
    pub fn record_effect_delivery(
        &self,
        name: &str,
        envelopes: &[crate::runtime::VmEffectEnvelope],
    ) -> Result<()> {
        let Some(log) = self.effect_delivery_log(name)? else {
            return Ok(());
        };
        let mut log = log
            .lock()
            .map_err(|_| anyhow::anyhow!("VM effect delivery log lock poisoned"))?;
        for envelope in envelopes {
            log.append(envelope.clone())?;
        }
        Ok(())
    }

    /// Unacknowledged suffix for one Brain/client identity.
    pub fn pending_effect_delivery(
        &self,
        name: &str,
        consumer: crate::runtime::DeliveryConsumerIdentity,
    ) -> Result<Vec<crate::runtime::VmEffectEnvelope>> {
        let Some(log) = self.effect_delivery_log(name)? else {
            return Ok(Vec::new());
        };
        let log = log
            .lock()
            .map_err(|_| anyhow::anyhow!("VM effect delivery log lock poisoned"))?;
        if let Some(bound) = log.brain_id() {
            anyhow::ensure!(
                bound == consumer.brain_id,
                "delivery consumer Brain {} does not match log bound to {bound}",
                consumer.brain_id
            );
        }
        Ok(log.pending_for(&consumer))
    }

    /// Packed Runtime/Application ABI frames for the unacknowledged suffix.
    pub fn pending_effect_delivery_frames(
        &self,
        name: &str,
        consumer: crate::runtime::DeliveryConsumerIdentity,
    ) -> Result<Vec<Vec<u8>>> {
        self.pending_effect_delivery(name, consumer)?
            .into_iter()
            .map(|envelope| {
                crate::ipc::checkpoint_codec::encode_runtime_application_message_packed(
                    &crate::runtime::RuntimeApplicationMessage::Envelope { envelope },
                )
            })
            .collect()
    }

    /// Record that one Brain/client identity durably projected a cursor.
    pub fn acknowledge_effect_delivery(
        &self,
        name: &str,
        consumer: crate::runtime::DeliveryConsumerIdentity,
        cursor: crate::runtime::DeliveryCursor,
    ) -> Result<bool> {
        let Some(log) = self.effect_delivery_log(name)? else {
            return Ok(false);
        };
        let mut log = log
            .lock()
            .map_err(|_| anyhow::anyhow!("VM effect delivery log lock poisoned"))?;
        log.acknowledge_identity(consumer, cursor)
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
        let restored = Arc::new(crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
            checkpoint.clone(),
            runtime_revision,
        )?);
        self.bind_runtime_delivery_log(name, &restored)?;
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
            .insert(name.to_string(), restored);
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
    ) -> Option<crate::runtime::ProgramRuntimeAuthorityStore> {
        self.root.as_ref().map(|root| {
            crate::runtime::ProgramRuntimeAuthorityStore::new(
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
        let mut validated_effect_audits = crate::runtime::EffectAuditReducer::default();
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
                    let identity = crate::runtime::EffectAuditIdentity {
                        brain_id: brain_id.0,
                        run_id: event.run_id.unwrap_or(RunId(uuid::Uuid::nil())).0,
                        request_seq: *request_seq,
                        execution_id: *execution_id,
                        effect_sequence: effect.sequence,
                    };
                    let authority = crate::runtime::EffectAuditAuthority {
                        authority_id: uuid::Uuid::nil(),
                        runner_lease_id: uuid::Uuid::nil(),
                        runner_subject: "legacy-v14".into(),
                        connection_id: None,
                        environment_generation: event.environment_generation,
                    };
                    validated_effect_audits.apply(
                        crate::runtime::EffectAuditTransition::Reserve {
                            intent: crate::runtime::EffectAuditIntent::from_effect(
                                identity, effect,
                            )?,
                            authority: authority.clone(),
                        },
                    )?;
                    validated_effect_audits.apply(
                        crate::runtime::EffectAuditTransition::Finish {
                            identity,
                            authority_id: authority.authority_id,
                            outcome:
                                crate::runtime::EffectAuditTerminalOutcome::LegacyV14Snapshot {
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
                let mut observer = crate::runtime::EffectAuditReducer::default();
                observer.apply(fence)?;
                state.recent_effect_audits.extend(
                    observer
                        .entries()
                        .values()
                        .map(crate::runtime::EffectAuditEntry::observer_projection),
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
        let restart_transitions = unresolved_effects
            .into_iter()
            .filter_map(|entry| {
                let outcome = match entry.state {
                    crate::runtime::EffectAuditState::IntentAccepted => {
                        crate::runtime::EffectAuditTerminalOutcome::AbandonedNotApplied
                    }
                    crate::runtime::EffectAuditState::AwaitingHostResult => {
                        crate::runtime::EffectAuditTerminalOutcome::UncertainProcessLoss
                    }
                    crate::runtime::EffectAuditState::Terminal { .. } => return None,
                };
                Some(crate::runtime::EffectAuditTransition::Finish {
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
        attachment::read_cursors(self.root.as_deref(), name, brain_id)
    }

    fn write_attachment_cursors(&self, name: &str, state: &BrainState) -> Result<()> {
        attachment::write_cursors(
            self.root.as_deref(),
            name,
            state.brain_id,
            &state.attachments,
        )
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
        journal::event_path(self.root.as_deref(), name)
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
        run::disconnect_intent_path(self.root.as_deref(), name, run_id)
    }

    fn persist_disconnect_intent(
        &self,
        name: &str,
        intent: &DisconnectTerminalizationIntent,
    ) -> Result<()> {
        run::persist_disconnect_intent(self.root.as_deref(), name, intent)
    }

    fn clear_disconnect_intent(&self, name: &str, run_id: RunId) -> Result<()> {
        run::clear_disconnect_intent(self.root.as_deref(), name, run_id)
    }

    fn read_disconnect_intents(
        &self,
        name: &str,
    ) -> Result<HashMap<RunId, DisconnectTerminalizationIntent>> {
        run::read_disconnect_intents(self.root.as_deref(), name)
    }

    fn read_events(&self, name: &str) -> Result<Vec<BrainEvent>> {
        journal::read_events(self.root.as_deref(), name)
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
        journal::append_event(self.root.as_deref(), name, event)
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
        journal::append_event_batch(self.root.as_deref(), name, events)
    }

    /// Atomically replace the canonical journal with an equivalent sequence
    /// of individual events. This is used only for bounded audit-history
    /// compaction; mutation receipts and every non-audit event are preserved.
    fn rewrite_events(&self, name: &str, events: &[BrainEvent]) -> Result<()> {
        journal::rewrite_events(self.root.as_deref(), name, events)
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

    /// Override the durable byte ceiling of this Brain's active effect-audit
    /// journal so a regression can reach the bound without writing 48 MiB.
    #[cfg(test)]
    pub(crate) fn set_effect_audit_journal_max_bytes_for_test(
        &self,
        name: &str,
        bytes: u64,
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
        self.with_effect_audit_storage_mut(name, brain_id, |storage| {
            storage.active.set_max_bytes_for_test(bytes);
            Ok(())
        })?;
        Ok(())
    }

    /// Size of this Brain's active effect-audit journal file on disk.
    #[cfg(test)]
    pub(crate) fn effect_audit_journal_bytes_for_test(&self, name: &str) -> Result<u64> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .context("Brain was removed concurrently")?
            .brain_id;
        Ok(self
            .with_effect_audit_storage_mut(name, brain_id, |storage| storage.active.file_bytes())?
            .unwrap_or(0))
    }

    /// Canonical sequence numbers currently held by this Brain's active
    /// effect-audit journal, in ascending order.
    #[cfg(test)]
    pub(crate) fn effect_audit_journal_seqs_for_test(&self, name: &str) -> Result<Vec<u64>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brain_id = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .context("Brain was removed concurrently")?
            .brain_id;
        Ok(self
            .with_effect_audit_storage_mut(name, brain_id, |storage| {
                Ok(storage
                    .active
                    .load()?
                    .into_iter()
                    .map(|(seq, _)| seq)
                    .collect::<Vec<_>>())
            })?
            .unwrap_or_default())
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
}

fn validate_participant_subject<'a>(label: &str, subject: &'a str) -> Result<&'a str> {
    let subject = subject.trim();
    if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
        anyhow::bail!("{label} must be 1-128 printable characters");
    }
    Ok(subject)
}

#[cfg(test)]
mod tests;
