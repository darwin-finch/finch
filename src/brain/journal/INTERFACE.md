# brain::journal — public interface

Generated from [`src/brain/journal/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/brain/journal/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub struct BrainApprovalDecisionReservation { … }
pub struct BrainEvent { … }
pub enum BrainEventKind { MutationRecorded, RunnerLeaseAcquired, RunnerLeaseReleased, RunnerHandoffRequested, RunnerHandoffCompleted, RunnerHandoffCancelled, ClientAttached, ClientDetached, RunStarted, RunStatusChanged, Prompt, SpeculativePrompt, ParticipantMessage, TaskListReplaced, CommittedMemoriesReplaced, ToolCall, ToolResult, ApprovalRequested, ApprovalDecided, Program, ProgramPopped, Result, RuntimeCommitted, EffectRecorded, EffectAuditTransition, ScheduleChanged, ScheduleDue }
pub struct BrainExecutableMutationAppend { … }
/// Stable identity of one durable Brain.
pub struct BrainId(pub uuid::Uuid);
/// One physical canonical journal append.
pub enum BrainJournalRecord { EventBatch }
pub struct BrainMetadata { … }
pub struct BrainMutationAppend { … }
pub enum BrainMutationOutcome { RunCancellationReserved, RunCancellationDispatching, RunCancellationReconciled, RunAlreadyCancelled, RunCancellationNoop, ScheduleCancellationNoop, HandoffCancellationNoop, ApprovalDecisionDelivered }
/// Durable identity and preconditions for one authorized Brain mutation.
pub struct BrainMutationReceipt { … }
pub struct BrainProgram { … }
/// Secret-free provider/model overlay stored on a named Brain.
pub struct BrainProviderSelection { … }
impl BrainProviderSelection {
    pub fn is_empty(&self) -> bool;
}
/// One MemTree leaf the query processor has promoted into this Brain's durable, byte-stable recall prefix (#940).
pub struct CommittedMemoryRecord { … }
/// Append-only event log rooted at a Brain store directory.
pub struct EventJournal { … }
impl EventJournal {
    pub fn append(&self, name: &str, event: &BrainEvent) -> Result<()>;
    pub fn append_batch(&self, name: &str, events: &[BrainEvent]) -> Result<()>;
    pub fn new(root: Option<PathBuf>) -> Self;
    pub fn read(&self, name: &str) -> Result<Vec<BrainEvent>>;
    pub fn rewrite(&self, name: &str, events: &[BrainEvent]) -> Result<()>;
    pub fn root(&self) -> Option<&Path>;
}
pub struct JournalProjection { … }
/// Path + digest + payload for one `@` mention attached to a Prompt.
pub struct PromptAttachment { … }
```

## Functions

```rust
pub fn append_event(root: Option<&Path>, name: &str, event: &BrainEvent) -> Result<()> { … }
pub fn append_event_batch(root: Option<&Path>, name: &str, events: &[BrainEvent]) -> Result<()> { … }
pub fn append_journal_value<T: Serialize>(root: Option<&Path>, name: &str, value: &T) -> Result<()> { … }
/// Schema v14 added explicit event-envelope RunId correlation.
pub fn backfill_legacy_speculative_run_correlation(events: &mut [BrainEvent]) { … }
/// Create and durably link every missing directory component, including the configured Brain root when it does not yet exist.
pub fn create_dir_all_durable(path: &std::path::Path) -> Result<()> { … }
pub fn event_path(root: Option<&Path>, name: &str) -> Option<PathBuf> { … }
pub fn read_events(root: Option<&Path>, name: &str) -> Result<Vec<BrainEvent>> { … }
/// Locate the first canonical event for a mutation without applying a new transition.
pub fn replay_mutation<'a>(events: &'a [BrainEvent], receipt: &BrainMutationReceipt) -> Result<Option<&'a BrainEvent>, anyhow::Error> { … }
/// Atomically replace the canonical journal with an equivalent sequence of individual events.
pub fn rewrite_events(root: Option<&Path>, name: &str, events: &[BrainEvent]) -> Result<()> { … }
/// Best-effort read of a Brain journal.
pub fn scan_readonly(path: &Path) -> JournalProjection { … }
pub fn sync_directory(path: &std::path::Path) -> Result<()> { … }
```

## Constants

```rust
pub const BRAIN_EVENT_SCHEMA_VERSION: u32 = 15;
pub const BRAIN_METADATA_VERSION: u32 = 1;
pub const fn initial_environment_generation() -> u64 { … }
pub const fn legacy_brain_event_schema_version() -> u32 { … }
```
