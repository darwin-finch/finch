# brain — public interface

Generated from [`src/brain/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/brain/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// One client projection over the canonical Brain service.
pub struct AttachedBrainClient { … }
impl AttachedBrainClient {
    pub async fn acknowledge(&mut self, seq: u64) -> Result<()>;
    pub async fn attach(&mut self, subject: &str, role: AttachmentRole, attachment_id: Option<AttachmentId>) -> Result<BrainAttachment>;
    pub async fn attach_invited_persistent(&mut self, subject: &str, client_slot: &str) -> Result<(AttachmentRole, BrainAttachment)>;
    pub async fn attach_persistent(&mut self, subject: &str, role: AttachmentRole, client_slot: &str) -> Result<BrainAttachment>;
    pub async fn cancel_run(&self, run_id: super::store::RunId) -> Result<super::store::BrainRun>;
    pub async fn cancel_schedule(&self, schedule_id: super::store::ScheduleId) -> Result<bool>;
    pub async fn create_schedule(&self, language: super::store::ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: super::store::BrainScheduleDeliveryPolicy) -> Result<super::store::BrainSchedule>;
    pub async fn disconnect(&self) -> Result<()>;
    pub async fn inspect_schedule(&self, schedule_id: super::store::ScheduleId) -> Result<Option<super::store::BrainSchedule>>;
    pub async fn push(&self, kind: BrainEventKind) -> Result<()>;
    pub async fn schedule_initialization(&self, next_due_ms: u64) -> Result<super::store::BrainSchedule>;
    pub async fn snapshot(&self) -> Result<BrainSnapshot>;
    pub async fn start_speculative(&self, prompt: String) -> Result<super::store::BrainRun>;
    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>>;
    /// Connect while retaining a transport failure as data.
    pub async fn watch_with_errors(&self) -> Result<mpsc::UnboundedReceiver<Result<BrainWireMessage>>>;
    pub fn attachment(&self) -> Option<&BrainAttachment>;
    pub fn local(target: RemoteBrainTarget, ipc: crate::ipc::IpcClient) -> Self;
    pub fn remote(client: RemoteBrainClient) -> Self;
}
/// Stable identity of one client projection of a Brain. Re-exported from `brain::attachment`.
pub struct AttachmentId(pub uuid::Uuid);
/// Re-exported from `brain::attachment`.
pub enum AttachmentRole { Runner, Driver, Consultant, Observer }
/// Stable identifier for one background task, returned immediately by [`BackgroundTaskManager::start`].
pub struct BackgroundTaskId(String);
impl BackgroundTaskId {
    /// The identifier as presented in tool results.
    pub fn as_str(&self) -> &str;
}
/// Bounded lifecycle owner for long-lived commands.
pub struct BackgroundTaskManager { … }
impl BackgroundTaskManager {
    /// Poll one task: current state plus the retained stdout/stderr rings.
    pub async fn poll(&self, id: &str) -> Result<BackgroundTaskSnapshot>;
    /// Number of tasks currently in `Running` state.
    pub async fn running_count(&self) -> usize;
    /// Kill and reap every running task.
    pub async fn shutdown_all(&self);
    /// Start `command` under `bash -c` and return its task ID immediately.
    pub async fn start(&self, command: &str, description: &str) -> Result<BackgroundTaskId>;
    /// Stop a task: SIGKILL its recorded direct child and reap it.
    pub async fn stop(&self, id: &str) -> Result<BackgroundTaskSnapshot>;
    /// Total retained entries, running and finished.
    pub async fn total_count(&self) -> usize;
    /// Manager with the documented default bounds.
    pub fn new() -> Self;
    /// Manager with explicit bounds (used by tests to make bounds reachable).
    pub fn with_limits(max_running: usize, max_total: usize, ring_bytes: usize) -> Self;
}
/// One poll result: identity, lifecycle state, and the bounded output rings.
pub struct BackgroundTaskSnapshot { … }
impl BackgroundTaskSnapshot {
    /// Human-readable poll rendering used by tool results.
    pub fn render(&self) -> String;
}
/// Lifecycle state of one background task.
pub enum BackgroundTaskState { Running, Completed, Stopped }
impl BackgroundTaskState {
    /// True while the task still holds a slot a new task cannot take.
    pub fn is_running(&self) -> bool;
}
/// Exact participant/environment boundary to which a Brain-owned approval request is addressed. Re-exported from `brain::attachment`.
pub struct BrainApprovalAudience { … }
/// Re-exported from `brain::attachment`.
pub struct BrainAttachment { … }
pub struct BrainCredentialAuthority { … }
impl BrainCredentialAuthority {
    /// Narrow an already verified participant credential to one pending remote attachment.
    pub fn bind_attachment(&self, parent: &BrainCredentialClaims, attachment_id: AttachmentId, connection_id: ConnectionId, now_ms: u64) -> Result<(String, BrainCredentialClaims)>;
    /// Verify an invitation without consuming it.
    pub fn inspect_invitation(&self, token: &str, now_ms: u64) -> Result<BrainInvitationClaims>;
    pub fn invitation_public_key(&self) -> [u8; 32];
    pub fn issue(&self, request: BrainCredentialRequest, now_ms: u64) -> Result<String>;
    pub fn issue_invitation(&self, request: BrainInvitationRequest, now_ms: u64) -> Result<(String, BrainInvitationClaims)>;
    /// Load the daemon credential authority from a private state directory.
    pub fn load_or_create(state_directory: &Path) -> Result<Self>;
    /// Atomically bind one invitation to a participant and mint the ordinary credential used by every later attachment operation.
    pub fn redeem_invitation(&self, token: &str, subject: &str, now_ms: u64) -> Result<(String, BrainCredentialClaims)>;
    pub fn revoke(&self, credential_id: uuid::Uuid) -> Result<()>;
    pub fn verify(&self, token: &str, now_ms: u64) -> Result<BrainCredentialClaims>;
}
pub struct BrainCredentialClaims { … }
impl BrainCredentialClaims {
    /// Derive the ancestry for an attenuated child credential.
    pub fn attenuate(&self, child_scopes: &BTreeSet<BrainCredentialScope>, child_ttl_ms: u64, now_ms: u64) -> Result<Vec<uuid::Uuid>>;
    pub fn permits(&self, scope: BrainCredentialScope) -> bool;
    pub fn require_attachment(&self, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<()>;
    pub fn require_audience(&self, brain_id: BrainId, brain: &str, environment_generation: u64, scope: BrainCredentialScope) -> Result<()>;
    pub fn require_participant(&self, subject: &str, role: AttachmentRole) -> Result<()>;
}
pub struct BrainCredentialRequest { … }
pub enum BrainCredentialScope { BrainRead, BrainAttach, BrainDetach, BrainSubmit, BrainApprove, BrainControl, EnvironmentExecute, EnvironmentAdmin, ComputeSubmit }
/// The one machine/workspace boundary in which a brain may cause effects. Re-exported from `brain::projection`.
pub struct BrainEnvironment { … }
/// Re-exported from `brain::journal`.
pub struct BrainEvent { … }
/// Re-exported from `brain::journal`.
pub enum BrainEventKind { MutationRecorded, RunnerLeaseAcquired, RunnerLeaseReleased, RunnerHandoffRequested, RunnerHandoffCompleted, RunnerHandoffCancelled, ClientAttached, ClientDetached, RunStarted, RunStatusChanged, Prompt, SpeculativePrompt, ParticipantMessage, TaskListReplaced, CommittedMemoriesReplaced, ToolCall, ToolResult, ApprovalRequested, ApprovalDecided, Program, ProgramPopped, Result, RuntimeCommitted, EffectRecorded, EffectAuditTransition, ScheduleChanged, ScheduleDue }
/// Stable identity of one durable Brain. Re-exported from `brain::journal`.
pub struct BrainId(pub uuid::Uuid);
/// Reviewed, immutable program that establishes a Brain's initial typed state. Re-exported from `brain::schedule`.
pub struct BrainInitialization { … }
/// A short-lived, single-use bootstrap grant that can be handed to a collaborator without disclosing the daemon-wide Brain password.
pub struct BrainInvitationClaims { … }
pub struct BrainInvitationRequest { … }
/// Re-exported from `brain::journal`.
pub enum BrainMutationOutcome { RunCancellationReserved, RunCancellationDispatching, RunCancellationReconciled, RunAlreadyCancelled, RunCancellationNoop, ScheduleCancellationNoop, HandoffCancellationNoop, ApprovalDecisionDelivered }
/// Durable identity and preconditions for one authorized Brain mutation. Re-exported from `brain::journal`.
pub struct BrainMutationReceipt { … }
/// Re-exported from `brain::journal`.
pub struct BrainProgram { … }
/// Re-exported from `brain::run`.
pub struct BrainRun { … }
/// Re-exported from `brain::run`.
pub struct BrainRunCancellationReservation { … }
/// Re-exported from `brain::run`.
pub enum BrainRunKind { Interactive, Speculative, Scheduled, Subagent, Maintenance }
/// Re-exported from `brain::run`.
pub enum BrainRunStatus { QueuedForEnvironment, Running, AwaitingApproval, Completed, Failed, Cancelled, Interrupted }
/// Re-exported from `brain::run`.
pub struct BrainRunnerHandoff { … }
/// Re-exported from `brain::run`.
pub struct BrainRunnerLease { … }
/// Re-exported from `brain::schedule`.
pub struct BrainSchedule { … }
/// Re-exported from `brain::schedule`.
pub enum BrainScheduleDeliveryPolicy { Coalesce, BoundedCatchUp }
/// One durable schedule delivery and the queued run that owns it. Re-exported from `brain::schedule`.
pub struct BrainScheduleDue { … }
/// Durable, non-authority-bearing identity for a reviewed module scheduled by the Brain itself. Re-exported from `brain::schedule`.
pub struct BrainScheduleModuleIdentity { … }
/// Re-exported from `brain::projection`.
pub struct BrainSnapshot { … }
/// Authoritative persistent store of named Brains.
pub struct BrainStore { … }
impl BrainStore {
    pub async fn reserve_run_cancellation(&self, name: &str, sender: &str, initiating_attachment_id: AttachmentId, run_id: RunId, receipt: BrainMutationReceipt) -> Result<BrainRunCancellationReservation>;
    pub fn accept_runner_handoff(&self, name: &str, target_subject: &str, handoff_id: RunnerHandoffId, environment_generation: u64, ttl_ms: u64) -> Result<BrainRunnerLease>;
    /// Atomically allocate and journal an explicit speculative request and its queued run under the aggregate lock.
    pub fn accept_speculative_run(&self, name: &str, sender: &str, initiating_attachment_id: AttachmentId, text: String) -> Result<(BrainEvent, BrainRun)>;
    /// Persist a projection cursor without appending another numbered Brain event.
    pub fn acknowledge(&self, name: &str, attachment_id: AttachmentId, connection_id: ConnectionId, seq: u64) -> Result<BrainAttachment>;
    /// Record that one Brain/client identity durably projected a cursor.
    pub fn acknowledge_effect_delivery(&self, name: &str, consumer: crate::runtime::DeliveryConsumerIdentity, cursor: crate::runtime::DeliveryCursor) -> Result<bool>;
    pub fn acquire_runner_lease(&self, name: &str, subject: &str, environment_generation: u64, lease_id: Option<RunnerLeaseId>, ttl_ms: u64) -> Result<BrainRunnerLease>;
    /// Promote an exact pending REST reservation into the live transport projection.
    pub fn activate_connection(&self, name: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainAttachment>;
    pub fn approval_decision_delivery_completed(&self, name: &str, mutation_id: uuid::Uuid) -> Result<bool>;
    /// Remove a Brain from the active namespace without destroying its log.
    pub fn archive(&self, name: &str) -> Result<Option<PathBuf>>;
    pub fn attach(&self, name: &str, subject: &str, role: AttachmentRole, attachment_id: Option<AttachmentId>) -> Result<BrainAttachment>;
    pub fn cancel_runner_handoff(&self, name: &str, handoff_id: RunnerHandoffId, sender: &str) -> Result<()>;
    pub fn cancel_runner_handoff_with_receipt(&self, name: &str, handoff_id: RunnerHandoffId, sender: &str, receipt: Option<BrainMutationReceipt>) -> Result<()>;
    pub fn cancel_schedule(&self, name: &str, cancelled_by: &str, initiating_attachment_id: AttachmentId, schedule_id: ScheduleId) -> Result<bool>;
    pub fn cancel_schedule_with_receipt(&self, name: &str, cancelled_by: &str, initiating_attachment_id: AttachmentId, schedule_id: ScheduleId, receipt: Option<BrainMutationReceipt>) -> Result<bool>;
    /// Commit reducible state returned by the frontend that owns this Brain's environment.
    pub fn commit_runner_runtime(&self, name: &str, request_seq: u64, runtime_revision: u64, checkpoint: crate::vm::TypedRuntimeCheckpoint) -> Result<BrainEvent>;
    pub fn commit_runner_runtime_for_run(&self, name: &str, run_id: RunId, request_seq: u64, runtime_revision: u64, checkpoint: crate::vm::TypedRuntimeCheckpoint) -> Result<BrainEvent>;
    /// Journal the latest checkpoint only after a ProgramRuntime commit.
    pub fn commit_runtime(&self, name: &str, request_seq: u64, runtime_revision: u64, runtime: &crate::runtime::ProgramRuntime) -> Result<BrainEvent>;
    pub fn complete_approval_decision_delivery(&self, name: &str, sender: &str, request_seq: u64, approval_id: &str, mutation_id: uuid::Uuid) -> Result<()>;
    pub fn complete_reserved_run_cancellation(&self, name: &str, sender: &str, run_id: RunId) -> Result<BrainRun>;
    /// How many Brains this store would answer for, without hydrating any.
    pub fn count_unhydrated(&self) -> usize;
    pub fn create_schedule(&self, name: &str, created_by: &str, initiating_attachment_id: AttachmentId, language: ProgramLanguage, source: impl Into<String>, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: BrainScheduleDeliveryPolicy) -> Result<BrainSchedule>;
    pub fn create_schedule_with_receipt(&self, name: &str, created_by: &str, initiating_attachment_id: AttachmentId, language: ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: BrainScheduleDeliveryPolicy, mutation: Option<BrainMutationReceipt>) -> Result<BrainSchedule>;
    pub fn detach(&self, name: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<()>;
    /// The Brains holding work due at or before `now_ms`, in due order.
    pub fn due_schedule_brains(&self, now_ms: u64) -> Vec<String>;
    /// Open or reuse the Brain-bound portable effect delivery log.
    pub fn effect_delivery_log(&self, name: &str) -> Result<Option<Arc<std::sync::Mutex<crate::runtime::VmEffectDeliveryLog>>>>;
    pub fn environment(&self) -> &BrainEnvironment;
    /// Clear an abandoned pending connection without advancing the Brain log or its durable acknowledgement cursor.
    pub fn expire_pending_connection(&self, name: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<bool>;
    pub fn expire_runner_handoff(&self, name: &str, handoff_id: RunnerHandoffId, now_ms: u64) -> Result<bool>;
    pub fn expire_runner_lease(&self, name: &str, lease_id: RunnerLeaseId, now_ms: u64) -> Result<bool>;
    /// How many active schedules the index is tracking, for tests and diagnostics.
    pub fn indexed_schedule_count(&self) -> usize;
    /// Return the persisted initialization contract without executing it or appending any observable Brain event.
    pub fn initialization(&self, name: &str) -> Result<BrainInitialization>;
    pub fn inspect_run(&self, name: &str, run_id: RunId) -> Result<BrainRun>;
    pub fn inspect_schedule(&self, name: &str, schedule_id: ScheduleId) -> Result<Option<BrainSchedule>>;
    pub fn list(&self) -> Result<Vec<String>>;
    /// Every Brain name this store would answer for, without hydrating any of them (#364).
    pub fn list_names_unhydrated(&self) -> Vec<String>;
    /// Per-Brain listing facts without hydrating any of them.
    pub fn list_summaries_unhydrated(&self) -> Vec<BrainListSummary>;
    pub fn mark_run_cancellation_dispatching(&self, name: &str, sender: &str, run_id: RunId, mutation_id: uuid::Uuid) -> Result<()>;
    pub fn mark_run_cancellation_reconciled(&self, name: &str, sender: &str, run_id: RunId, mutation_id: uuid::Uuid) -> Result<()>;
    pub fn new(machine: impl Into<String>) -> Self;
    /// When the earliest active schedule in the store next comes due.
    pub fn next_schedule_due_ms(&self) -> Option<u64>;
    /// Number of disconnect terminalizations currently awaiting durable publication.
    pub fn pending_disconnect_terminalization_retries(&self) -> usize;
    /// Unacknowledged suffix for one Brain/client identity.
    pub fn pending_effect_delivery(&self, name: &str, consumer: crate::runtime::DeliveryConsumerIdentity) -> Result<Vec<crate::runtime::VmEffectEnvelope>>;
    /// Packed Runtime/Application ABI frames for the unacknowledged suffix.
    pub fn pending_effect_delivery_frames(&self, name: &str, consumer: crate::runtime::DeliveryConsumerIdentity) -> Result<Vec<Vec<u8>>>;
    pub fn pop_program(&self, name: &str, sender: &str) -> Result<Option<BrainEvent>>;
    /// Return the one live typed runtime for a named Brain, restoring its latest reducible checkpoint on first access after daemon restart.
    pub fn program_runtime(&self, name: &str) -> Result<Arc<crate::runtime::ProgramRuntime>>;
    pub fn push(&self, name: &str, sender: &str, kind: BrainEventKind) -> Result<BrainEvent>;
    /// Durably append an executable request and its RunStarted projection in one physical record, then apply and broadcast both logical events in native sequence or…
    pub fn push_executable_idempotent(&self, name: &str, sender: &str, kind: BrainEventKind, receipt: BrainMutationReceipt, initiating_attachment_id: AttachmentId, status: BrainRunStatus) -> Result<BrainExecutableMutationAppend>;
    pub fn push_for_run(&self, name: &str, sender: &str, run_id: RunId, kind: BrainEventKind) -> Result<BrainEvent>;
    /// Append a mutation's first canonical event exactly once.
    pub fn push_idempotent(&self, name: &str, sender: &str, kind: BrainEventKind, receipt: BrainMutationReceipt) -> Result<BrainMutationAppend>;
    /// Atomically advance due schedules and append the exact queued ProgramRun for each delivery.
    pub fn queue_due_schedules(&self, name: &str, now_ms: u64) -> Result<Vec<BrainRun>>;
    /// Persist envelopes before local Brain handling.
    pub fn record_effect_delivery(&self, name: &str, envelopes: &[crate::runtime::VmEffectEnvelope]) -> Result<()>;
    pub fn release_runner_lease(&self, name: &str, lease_id: RunnerLeaseId) -> Result<()>;
    /// Remove a provisional Brain once its last live participant has left.
    pub fn remove_if_unused(&self, name: &str) -> Result<bool>;
    /// Resolve an exact durable replay without applying a new transition.
    pub fn replay_mutation(&self, name: &str, receipt: &BrainMutationReceipt) -> Result<Option<BrainEvent>>;
    pub fn request_runner_handoff(&self, name: &str, requested_by: &str, target_subject: &str, expected_lease_id: RunnerLeaseId, environment_generation: u64, ttl_ms: u64) -> Result<BrainRunnerHandoff>;
    pub fn request_runner_handoff_with_receipt(&self, name: &str, requested_by: &str, target_subject: &str, expected_lease_id: RunnerLeaseId, environment_generation: u64, ttl_ms: u64, mutation: Option<BrainMutationReceipt>) -> Result<BrainRunnerHandoff>;
    pub fn require_connection(&self, name: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainAttachment>;
    pub fn reserve_approval_decision(&self, name: &str, sender: &str, request_seq: u64, approval_id: &str, decision: serde_json::Value, receipt: BrainMutationReceipt) -> Result<BrainApprovalDecisionReservation>;
    /// On-disk directory that holds named Brain folders, if this store persists.
    pub fn root(&self) -> Option<&std::path::Path>;
    /// Return the durable reducible state a newly connected environment runner must install before accepting ProgramRuns.
    pub fn runner_checkpoint(&self, name: &str) -> Result<(u64, crate::vm::TypedRuntimeCheckpoint)>;
    /// Idempotently journal initialization as a one-shot schedule.
    pub fn schedule_initialization(&self, name: &str, initiating_attachment_id: AttachmentId, connection_id: ConnectionId, next_due_ms: u64) -> Result<BrainSchedule>;
    pub fn schedule_initialization_with_receipt(&self, name: &str, initiating_attachment_id: AttachmentId, connection_id: ConnectionId, next_due_ms: u64, mutation: Option<BrainMutationReceipt>) -> Result<BrainSchedule>;
    /// Woken when a schedule appears that is due sooner than the current head.
    pub fn schedule_wakeup(&self) -> Arc<tokio::sync::Notify>;
    pub fn snapshot(&self, name: &str) -> Result<BrainSnapshot>;
    pub fn start_run(&self, name: &str, sender: &str, kind: BrainRunKind, request_seq: u64, initiating_attachment_id: AttachmentId, status: BrainRunStatus) -> Result<BrainRun>;
    pub fn start_run_with_parent(&self, name: &str, sender: &str, kind: BrainRunKind, request_seq: u64, initiating_attachment_id: AttachmentId, status: BrainRunStatus, parent_run_id: Option<RunId>) -> Result<BrainRun>;
    /// Start a run whose identity was allocated by the authoritative caller.
    pub fn start_run_with_parent_id(&self, name: &str, sender: &str, run_id: RunId, kind: BrainRunKind, request_seq: u64, initiating_attachment_id: AttachmentId, status: BrainRunStatus, parent_run_id: Option<RunId>, detail: Option<String>) -> Result<BrainRun>;
    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<BrainEvent>>;
    pub fn transition_run(&self, name: &str, sender: &str, run_id: RunId, status: BrainRunStatus, detail: Option<String>) -> Result<BrainRun>;
    pub fn validate_name(name: &str) -> Result<&str>;
    /// Populate the due index from every Brain on disk, once.
    pub fn warm_schedule_index(&self);
    pub fn with_environment(machine: impl Into<String>, workspace: impl Into<PathBuf>, root: Option<PathBuf>) -> Self;
    pub fn with_root(machine: impl Into<String>, root: Option<PathBuf>) -> Self;
}
/// One typed task in the Brain's authoritative task-list projection.
pub struct BrainTask { … }
/// Priority of one Brain-owned task.
pub enum BrainTaskPriority { High, Medium, Low }
/// Lifecycle status of one Brain-owned task.
pub enum BrainTaskStatus { Pending, InProgress, Completed }
/// Re-exported from `brain::projection`.
pub enum BrainWireMessage { Snapshot, Event }
/// One MemTree leaf the query processor has promoted into this Brain's durable, byte-stable recall prefix (#940). Re-exported from `brain::journal`.
pub struct CommittedMemoryRecord { … }
/// Identity of one live transport connection for a durable attachment. Re-exported from `brain::attachment`.
pub struct ConnectionId(pub uuid::Uuid);
/// Opaque daemon-side authority for one runner capability.
pub(crate) struct EffectAuditAuthorityGrant { … }
/// How a stopped process ended, recorded by the reaping stop path.
pub enum ExitOutcome { Code, Signal }
pub struct IsolatedTestProof { … }
impl IsolatedTestProof {
    pub fn brain_address(&self) -> &str;
    pub fn brain_password(&self) -> anyhow::Result<String>;
    pub fn daemon_address(&self) -> &str;
    pub fn duplicate_brain_listener(&self) -> anyhow::Result<std::net::TcpListener>;
    pub fn duplicate_daemon_listener(&self) -> anyhow::Result<std::net::TcpListener>;
    pub fn duplicate_ipc_listener(&self) -> anyhow::Result<std::os::unix::net::UnixListener>;
}
/// Observable pathname version for accidental-race detection, not a non-repeating identity.
#[cfg(all(test, unix))]
pub(crate) struct IsolatedTestSocketIdentity { … }
/// Re-exported from `brain::schedule`.
pub enum ProgramLanguage { Forth, Lisp }
/// Path + digest + payload for one `@` mention attached to a Prompt. Re-exported from `brain::journal`.
pub struct PromptAttachment { … }
/// Dynamic node information returned only after Brain-scoped authentication.
pub struct RemoteBrainCapabilities { … }
pub struct RemoteBrainClient { … }
impl RemoteBrainClient {
    /// Advance this live connection's projection cursor.
    pub async fn acknowledge(&mut self, seq: u64) -> Result<()>;
    /// Archive an inactive Brain with an explicitly elevated administrative credential.
    pub async fn archive(&self, subject: &str) -> Result<Option<String>>;
    pub async fn attach(&mut self, subject: &str, role: AttachmentRole, attachment_id: Option<AttachmentId>) -> Result<BrainAttachment>;
    /// Redeem this client's invitation and attach using the role and scopes fixed by its issuer.
    pub async fn attach_invited_persistent(&mut self, subject: &str, client_slot: &str) -> Result<(AttachmentRole, BrainAttachment)>;
    /// Reuse this console slot's daemon-owned attachment identity across frontend restarts.
    pub async fn attach_persistent(&mut self, subject: &str, role: AttachmentRole, client_slot: &str) -> Result<BrainAttachment>;
    /// Explicitly replace this client's ordinary participant credential with one that may request or cancel an addressed runner handoff.
    pub async fn authorize_runner_handoff_control(&self, subject: &str, role: AttachmentRole) -> Result<()>;
    pub async fn cancel_run(&self, run_id: super::store::RunId) -> Result<super::store::BrainRun>;
    pub async fn cancel_run_with_handle(&self, run_id: super::store::RunId, handle: &BrainMutationHandle) -> Result<super::store::BrainRun>;
    pub async fn cancel_runner_handoff(&self, handoff_id: super::store::RunnerHandoffId) -> Result<()>;
    pub async fn cancel_runner_handoff_with_handle(&self, handoff_id: super::store::RunnerHandoffId, handle: &BrainMutationHandle) -> Result<()>;
    pub async fn cancel_schedule(&self, schedule_id: super::store::ScheduleId) -> Result<bool>;
    pub async fn cancel_schedule_with_handle(&self, schedule_id: super::store::ScheduleId, handle: &BrainMutationHandle) -> Result<bool>;
    /// Retrieve live node/model availability after authenticating to the exact Brain audience named by this client's scoped credential.
    pub async fn capabilities(&self) -> Result<RemoteBrainCapabilities>;
    /// Explicitly create this target alias in the remote daemon's own environment.
    pub async fn create(&self) -> Result<BrainSnapshot>;
    pub async fn create_schedule(&self, language: super::store::ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: super::store::BrainScheduleDeliveryPolicy) -> Result<super::store::BrainSchedule>;
    pub async fn create_schedule_with_handle(&self, language: super::store::ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: super::store::BrainScheduleDeliveryPolicy, handle: &BrainMutationHandle) -> Result<super::store::BrainSchedule>;
    /// Detach the current transport projection.
    pub async fn disconnect(&self) -> Result<()>;
    /// Mint an explicitly attenuated participant credential using an unbound `brain:control` credential already held by this client.
    pub async fn issue_credential(&self, subject: &str, role: AttachmentRole, scopes: std::collections::BTreeSet<super::credential::BrainCredentialScope>, ttl_ms: Option<u64>) -> Result<(String, super::credential::BrainCredentialClaims)>;
    /// Create a short-lived, single-participant invitation without exposing the daemon bootstrap password to the recipient.
    pub async fn issue_invitation(&self, role: AttachmentRole, ttl_ms: Option<u64>) -> Result<(String, super::credential::BrainInvitationClaims)>;
    /// Delegate an explicitly attenuated participant invitation.
    pub async fn issue_invitation_with_scopes(&self, role: AttachmentRole, scopes: Option<std::collections::BTreeSet<super::credential::BrainCredentialScope>>, ttl_ms: Option<u64>) -> Result<(String, super::credential::BrainInvitationClaims)>;
    pub async fn prepare_cancel_run_mutation(&self, run_id: super::store::RunId) -> Result<BrainMutationHandle>;
    pub async fn prepare_cancel_runner_handoff_mutation(&self, handoff_id: super::store::RunnerHandoffId) -> Result<BrainMutationHandle>;
    pub async fn prepare_cancel_schedule_mutation(&self, schedule_id: super::store::ScheduleId) -> Result<BrainMutationHandle>;
    pub async fn prepare_create_schedule_mutation(&self, language: super::store::ProgramLanguage, source: &str, grant_ceiling: &crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: super::store::BrainScheduleDeliveryPolicy) -> Result<BrainMutationHandle>;
    pub async fn prepare_push_mutation(&self, kind: &BrainEventKind) -> Result<BrainMutationHandle>;
    pub async fn prepare_runner_handoff_mutation(&self, target_subject: &str, expected_lease_id: super::store::RunnerLeaseId, environment_generation: u64, ttl_ms: u64) -> Result<BrainMutationHandle>;
    pub async fn prepare_schedule_initialization_mutation(&self, next_due_ms: u64) -> Result<BrainMutationHandle>;
    /// Verify that an invitation's advertised TLS endpoint is reachable and presents the exact certificate embedded in the invitation, without consuming the single-…
    pub async fn probe_invitation_endpoint(&self) -> Result<()>;
    pub async fn push(&self, kind: BrainEventKind) -> Result<()>;
    /// Retry-safe submission using a caller-persisted immutable envelope.
    pub async fn push_with_handle(&self, kind: BrainEventKind, handle: &BrainMutationHandle) -> Result<()>;
    /// Redeem the signed invitation into an unbound scoped credential without creating an attachment.
    pub async fn redeem_invitation(&self, subject: &str) -> Result<AttachmentRole>;
    pub async fn request_runner_handoff(&self, target_subject: &str, expected_lease_id: super::store::RunnerLeaseId, environment_generation: u64, ttl_ms: u64) -> Result<super::store::BrainRunnerHandoff>;
    pub async fn request_runner_handoff_with_handle(&self, target_subject: &str, expected_lease_id: super::store::RunnerLeaseId, environment_generation: u64, ttl_ms: u64, handle: &BrainMutationHandle) -> Result<super::store::BrainRunnerHandoff>;
    /// Revoke a credential descended from this client's unbound controlling credential.
    pub async fn revoke_delegated_credential(&self, credential: &str) -> Result<()>;
    /// Revoke a signed invitation descended from this controller.
    pub async fn revoke_delegated_invitation(&self, invitation: &str) -> Result<()>;
    pub async fn schedule_initialization(&self, next_due_ms: u64) -> Result<super::store::BrainSchedule>;
    pub async fn schedule_initialization_with_handle(&self, next_due_ms: u64, handle: &BrainMutationHandle) -> Result<super::store::BrainSchedule>;
    pub async fn snapshot(&self) -> Result<BrainSnapshot>;
    pub async fn start_speculative(&self, prompt: String) -> Result<super::store::BrainRun>;
    /// Connect to the brain's snapshot/live-event stream.
    pub async fn watch(&self) -> Result<mpsc::UnboundedReceiver<BrainWireMessage>>;
    pub fn attachment(&self) -> Option<&BrainAttachment>;
    pub fn invited_node_public_key(&self) -> Option<[u8; 32]>;
    pub fn new(target: RemoteBrainTarget, password: impl Into<String>) -> Result<Self>;
    pub fn new_with_invitation(target: RemoteBrainTarget, invitation: impl Into<String>) -> Result<Self>;
}
pub struct RemoteBrainTarget { … }
impl RemoteBrainTarget {
    /// Exact target spelling suitable for `/brain join`.
    pub fn command_target(&self) -> String;
    pub fn display_name(&self) -> String;
    /// Build the endpoint printed with an invitation minted through the local plaintext daemon.
    pub fn invitation_recipient(brain: &str, certificate_hostname: &str, brain_bind_address: &str) -> Result<Self>;
    /// Resolve a bare Brain name through the already-connected local daemon.
    pub fn local(brain: &str, daemon_base_url: &str) -> Result<Self>;
    pub fn parse(value: &str) -> Result<Self>;
}
/// Re-exported from `brain::run`.
pub struct RunId(pub uuid::Uuid);
/// Re-exported from `brain::run`.
pub struct RunnerHandoffId(pub uuid::Uuid);
/// Re-exported from `brain::run`.
pub struct RunnerLeaseId(pub uuid::Uuid);
/// Re-exported from `brain::schedule`.
pub struct ScheduleId(pub uuid::Uuid);
```

## Functions

```rust
#[cfg(all(test, unix))]
pub(crate) fn authenticate_isolated_test_peer(stream: &tokio::net::UnixStream) -> anyhow::Result<()> { … }
pub fn authenticated_isolated_test_proof_text() -> anyhow::Result<Vec<u8>> { … }
/// Baseline authority granted when the bootstrap administrator selects only a participant role.
pub fn default_participant_scopes(role: AttachmentRole) -> BTreeSet<BrainCredentialScope> { … }
/// What is actually on disk under `path`, for assertion diagnostics.
#[cfg(test)]
pub(crate) fn directory_listing_for_tests(path: &std::path::Path) -> String { … }
/// Generate a cute, practically unique Brain name: "quiet-hill-a13f09", etc.
pub fn generate() -> String { … }
pub fn isolated_test_proof() -> anyhow::Result<IsolatedTestProof> { … }
pub fn isolated_test_proof_if_present() -> anyhow::Result<Option<IsolatedTestProof>> { … }
/// Maximum scopes this participant credential endpoint may mint for a role.
pub fn permitted_participant_scopes(role: AttachmentRole) -> BTreeSet<BrainCredentialScope> { … }
/// Number of times the full FD9 restore-and-verify transaction actually ran in this process.
#[cfg(test)]
pub(crate) fn proof_validation_call_count_for_tests() -> usize { … }
/// A Brain with one recurring schedule, created through the real API so the index is populated the way production populates it.
#[cfg(test)]
pub(crate) fn seed_scheduled_brain_for_tests(store: &BrainStore, name: &str, next_due_ms: u64) -> (AttachmentId, ScheduleId) { … }
#[cfg(all(test, unix))]
pub(crate) fn supervised_test_subprocess_command() -> std::process::Command { … }
/// Number of times the supervisor executable was read and hashed in this process.
#[cfg(test)]
pub(crate) fn supervisor_image_hash_count_for_tests() -> usize { … }
pub(crate) fn unix_millis() -> u64 { … }
#[cfg(all(test, unix))]
pub(crate) fn validate_isolated_test_socket(proof: &IsolatedTestProof, path: &std::path::Path) -> anyhow::Result<IsolatedTestSocketIdentity> { … }
```

## Constants

```rust
/// Default bound on concurrently running background tasks.
pub const DEFAULT_MAX_RUNNING_TASKS: usize = 16;
/// Default bound on total retained task entries (running + finished).
pub const DEFAULT_MAX_TOTAL_TASKS: usize = 64;
/// Default per-stream ring-buffer retention budget in bytes.
pub const DEFAULT_RING_BYTES_PER_STREAM: usize = 64 * 1024;
```

## Modules

```rust
pub(crate) mod effect_audit_archive;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `BrainMutationHandle`
