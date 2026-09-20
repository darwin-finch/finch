//! Append-only Brain event journal: envelope, persist, and replay helpers.
//!
//! The daemon is the sole writer. Physical JSONL records are either a bare
//! event or a checksummed batch; a torn final append projects none of its
//! logical events after restart.

use serde::{Deserialize, Serialize};

use super::attachment::{AttachmentId, AttachmentRole, BrainApprovalAudience, ConnectionId};
use super::run::{
    BrainRun, BrainRunStatus, BrainRunnerHandoff, BrainRunnerLease, RunId, RunnerHandoffId,
    RunnerLeaseId,
};
use super::schedule::{BrainSchedule, BrainScheduleDue, ProgramLanguage, ScheduleId};

mod persist;

pub use persist::{
    append_event, append_event_batch, append_journal_value, create_dir_all_durable, event_path,
    read_events, rewrite_events, scan_readonly, sync_directory, EventJournal, JournalProjection,
};

pub const BRAIN_EVENT_SCHEMA_VERSION: u32 = 15;
pub const BRAIN_METADATA_VERSION: u32 = 1;

/// Stable identity of one durable Brain. Names are mutable human aliases;
/// this ID is what future runs, attachments, cursors, and grants reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrainId(pub uuid::Uuid);

impl BrainId {
    pub(crate) fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    pub(crate) fn nil() -> Self {
        Self(uuid::Uuid::nil())
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
pub struct BrainApprovalDecisionReservation {
    pub event: BrainEvent,
    pub delivered: bool,
    pub replayed: bool,
}

/// Path + digest + payload for one `@` mention attached to a Prompt.
///
/// Replay and named-Brain restart must use `content` and `sha256` from this
/// record. They must not reread the project file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptAttachment {
    /// Project-relative path using `/` separators.
    pub path: String,
    /// `"file"` or `"directory"`.
    pub kind: String,
    /// Hex SHA-256 of `content`.
    pub sha256: String,
    /// Attached payload size in bytes.
    pub byte_len: u64,
    /// True when a directory expansion named a budget truncation.
    pub truncated: bool,
    /// Speakable truncation or skip note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncation_note: Option<String>,
    /// Exact attached UTF-8 contents.
    pub content: String,
}

/// One MemTree leaf the query processor has promoted into this Brain's
/// durable, byte-stable recall prefix (#940). `score` is the weighted score
/// (`cosine_similarity * importance_boost`) at the time it last (re)joined
/// or was reconfirmed, so a client renders the same deterministic block
/// without recomputing anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommittedMemoryRecord {
    pub node_id: u64,
    pub text: String,
    pub score: f32,
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
        /// File/directory mention snapshots supplied with this turn. Empty on
        /// legacy events; replay must use these bytes, not a later disk read.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attached_mentions: Vec<PromptAttachment>,
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
    /// Atomically replace the Brain's committed (byte-stable) recall set
    /// (#940). Whole-set replace, mirroring `TaskListReplaced`, rather than
    /// per-item add/remove events -- the query processor always decides the
    /// full resulting set for a turn, so one event per change is enough and
    /// reuses an already-proven event/projection/wire shape.
    CommittedMemoriesReplaced {
        memories: Vec<CommittedMemoryRecord>,
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
        continuation_messages: Vec<finch_providers::Message>,
        /// Provider identity/accounting captured at the completed invocation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invocation_metadata: Option<finch_providers::InvocationMetadata>,
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
        transition: crate::runtime::EffectAuditTransition,
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
pub enum BrainJournalRecord {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainProgram {
    pub seq: u64,
    pub sender: String,
    pub language: ProgramLanguage,
    pub source: String,
}

/// Secret-free provider/model overlay stored on a named Brain.
///
/// This is Brain metadata, not a journal event and not a `[[providers]]` row.
/// Optional fields use serde defaults so version-1 files written before the
/// overlay still load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BrainProviderSelection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// True when `provider` was copied from the global default at creation
    /// and has not been explicitly overridden.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provider_inherited: bool,
}

impl BrainProviderSelection {
    pub fn is_empty(&self) -> bool {
        self.provider.is_none() && self.model.is_none() && self.reasoning_effort.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainMetadata {
    pub version: u32,
    pub brain_id: BrainId,
    pub created_ms: u64,
    #[serde(flatten)]
    #[serde(default)]
    pub selection: BrainProviderSelection,
}

pub const fn initial_environment_generation() -> u64 {
    1
}

pub const fn legacy_brain_event_schema_version() -> u32 {
    1
}

/// Schema v14 added explicit event-envelope RunId correlation. Reconstruct
/// completed v13 speculative transcripts from the durable run lifecycle so an
/// old helper turn cannot enter ordinary provider context after upgrade.
pub fn backfill_legacy_speculative_run_correlation(events: &mut [BrainEvent]) {
    use super::run::BrainRunKind;
    use std::collections::HashSet;

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

/// Locate the first canonical event for a mutation without applying a new
/// transition. Receipt mismatch is an error: the idempotency key was reused.
pub fn replay_mutation<'a>(
    events: &'a [BrainEvent],
    receipt: &BrainMutationReceipt,
) -> Result<Option<&'a BrainEvent>, anyhow::Error> {
    let Some(event) = events.iter().find(|event| {
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
    Ok(Some(event))
}

#[cfg(test)]
mod tests;
