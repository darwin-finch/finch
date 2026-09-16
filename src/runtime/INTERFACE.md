# runtime — public interface

Generated from [`src/runtime/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/runtime/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// One authoritative active-task entry used to rebuild a lagged frontend projection.
pub struct AgentActivitySnapshot { … }
pub struct AgentBudget { … }
pub struct AgentContextReference { … }
pub enum AgentEvent { Resnapshot, TaskQueued, TaskStarted, UsageUpdated, ToolStarted, ToolCompleted, TaskFinished }
pub struct AgentIdentity { … }
pub enum AgentRole { General, Explore, Research, Code }
pub struct AgentTaskResult { … }
pub struct AgentTaskSnapshot { … }
pub struct AgentTaskSpec { … }
pub enum AgentTaskStatus { Queued, Running, Completed, Failed, Cancelled }
/// Provider-reported usage for a child task.
pub struct AgentUsage { … }
/// Truthfulness of provider-reported token usage across a child's attempts.
pub enum AgentUsageState { Complete, Partial, Unavailable }
pub struct AgentVmBinding { … }
impl AgentVmBinding {
    pub async fn cancel(&self, task_id: Uuid) -> Result<()>;
    pub async fn poll(&self, task_id: Uuid) -> Result<AgentTaskSnapshot>;
    pub async fn spawn(&self, task: String) -> Result<AgentIdentity>;
    pub async fn spawn_spec(&self, spec: AgentTaskSpec) -> Result<AgentIdentity>;
    pub async fn wait(&self, task_id: Uuid) -> Result<AgentTaskResult>;
}
pub struct AutomationAvailability { … }
/// Configuration gate and platform dispatcher for automation operations.
pub struct AutomationBroker { … }
impl AutomationBroker {
    pub fn availability(&self) -> AutomationAvailability;
    pub fn execute(&self, request: AutomationRequest) -> Result<Value>;
    pub fn is_enabled(&self) -> bool;
    pub fn new(enabled: bool) -> Self;
    /// Open the macOS Accessibility privacy pane after an explicit user action.
    pub fn open_permission_settings(&self, context: AutomationPromptContext) -> Result<bool>;
    /// Explicitly request the platform permission, when appropriate, and then perform a fresh passive check.
    pub fn request_permission(&self, context: AutomationPromptContext) -> AutomationPermissionResult;
}
pub struct AutomationPermissionResult { … }
/// Session facts that determine whether it is safe to raise a native prompt.
pub struct AutomationPromptContext { … }
impl AutomationPromptContext {
    /// Build prompt policy for the current launch.
    pub fn for_current_session(interactive: bool) -> Self;
}
/// Whether a native permission request was made for this process.
pub enum AutomationPromptDisposition { NotNeeded, Requested, SuppressedNonInteractive, SuppressedRemote }
pub enum AutomationRequest { Availability, Displays, Windows, Click, Type }
pub enum AutomationState { Disabled, Unsupported, PermissionRequired, Available }
/// Selects which awaited host calls leave the VM suspended for an external embedder.
pub enum DeferredHostEffects { None, ProgramInvocations, Schedules, AllAwaited }
/// Embedder-neutral delivery consumer.
pub struct DeliveryConsumerIdentity { … }
impl DeliveryConsumerIdentity {
    /// Construct a Brain/client delivery identity.
    pub fn new(brain_id: Uuid, client_id: Uuid) -> Self;
    /// Parse a canonical Brain/client wire key.
    pub fn parse_wire_key(key: &str) -> Option<Self>;
    /// Canonical durable-log key for this identity.
    pub fn wire_key(self) -> String;
}
/// Per-consumer delivery cursor over one ProgramRun's effect sequence.
pub struct DeliveryCursor { … }
impl DeliveryCursor {
    /// Cursor that has acknowledged `through_sequence` inclusive.
    pub fn through(execution_id: Uuid, through_sequence: u64) -> Self;
}
/// Provenance minted by the daemon when it creates a run-scoped reverse capability.
pub struct EffectAuditAuthority { … }
pub struct EffectAuditEntry { … }
/// Complete immutable identity of one named-Brain host effect.
pub struct EffectAuditIdentity { … }
pub struct EffectAuditIntent { … }
impl EffectAuditIntent {
    pub fn from_effect(identity: EffectAuditIdentity, effect: &crate::vm::VmSideEffect) -> Result<Self>;
}
pub struct EffectAuditReducer { … }
impl EffectAuditReducer {
    pub fn active_bytes(&self) -> usize;
    pub fn active_count(&self) -> usize;
    pub fn active_for_run(&self, run_id: Uuid) -> usize;
    /// Validate and apply one monotonic transition.
    pub fn apply(&mut self, transition: EffectAuditTransition) -> Result<bool>;
    pub fn entries(&self) -> &BTreeMap<EffectAuditIdentity, EffectAuditEntry>;
    pub fn get(&self, identity: &EffectAuditIdentity) -> Option<&EffectAuditEntry>;
    pub fn replay_fence_count(&self) -> usize;
    pub fn total_count(&self) -> usize;
    /// Validate one transition without cloning the complete replay index.
    pub fn validate(&self, transition: &EffectAuditTransition) -> Result<bool>;
}
pub enum EffectAuditState { IntentAccepted, AwaitingHostResult, Terminal }
impl EffectAuditState {
    pub fn is_terminal(&self) -> bool;
}
pub enum EffectAuditTerminalOutcome { Acknowledged, NotApplied, FailedPartial, AbandonedNotApplied, UncertainProcessLoss, Compacted, Redacted, LegacyV14Snapshot }
/// One canonical reducer input.
pub enum EffectAuditTransition { Reserve, Begin, Finish, Fence }
impl EffectAuditTransition {
    pub fn identity(&self) -> EffectAuditIdentity;
}
pub enum ExecutionBackend { TypedVm }
/// Hard limits applied before an execution result is returned to a provider.
pub struct ExecutionBudget { … }
/// Identity and resource limits attached to one VM execution.
pub struct ExecutionContext { … }
impl ExecutionContext {
    pub fn new(manifest_generation: u64, budget: ExecutionBudget) -> Self;
}
/// Provider-neutral result of evaluating Forth or Lisp source.
pub struct ExecutionOutcome { … }
impl ExecutionOutcome {
    pub fn failed(execution_id: Uuid, revision: u64, effect: ExecutionEffect, backend: ExecutionBackend, diagnostic: impl Into<String>, elapsed_ms: u64) -> Self;
    /// Frozen ProgramRun identity for this outcome.
    pub fn program_run(&self) -> crate::runtime::ProgramRun;
}
pub enum ExecutionStatus { Completed, Suspended, AuthorizationRequired, Failed, Cancelled }
/// Proof that the daemon durably committed `AwaitingHostResult`.
pub struct HostEffectPermit { … }
impl HostEffectPermit {
    pub fn identity(&self) -> EffectAuditIdentity;
}
/// The spawner a runtime has before a host attaches one.
pub struct NoAgentSpawning;
/// Host-issued concurrent output handle bound to one ProgramRun.
pub struct OutputHandleRef { … }
impl OutputHandleRef {
    /// Construct a ProgramRun-owned output handle reference.
    pub fn new(execution_id: Uuid, handle: impl Into<String>, generation: u64) -> Self;
}
/// UI-safe metadata for a daemon-owned typed continuation.
pub struct PendingTypedExecutionInfo { … }
pub enum PendingTypedReason { Yielded, AwaitingHostEffect, AuthorizationRequired }
/// Frozen identity of one verified ProgramRun.
pub struct ProgramRun { … }
impl ProgramRun {
    /// Named handle for one journaled effect on this run.
    pub fn effect_handle(self, sequence: u64) -> VmEffectHandle;
    /// Identify a ProgramRun from its VM execution id.
    pub fn new(execution_id: Uuid) -> Self;
    /// Bind embedder-neutral Brain/client ports onto this run identity.
    pub fn with_identity(mut self, identity: DeliveryConsumerIdentity) -> Self;
}
/// One session's persistent language runtimes.
pub struct ProgramRuntime { … }
impl ProgramRuntime {
    /// Install the application-owned MCP transport and atomically replace its discovered, validated namespaced vocabulary.
    pub async fn bind_mcp_client(&self, client: Arc<crate::tools::McpClient>) -> Result<Vec<String>>;
    /// Cancel an awaited portable effect only when it still owns the supplied `(execution_id, sequence)` boundary.
    pub async fn cancel_typed_execution_for_effect(&self, execution_id: uuid::Uuid, effect_sequence: u64, reason: Option<String>) -> Result<ExecutionOutcome>;
    /// Install a newer application-owned reducible checkpoint without replacing this frontend's host authority or resource bindings.
    pub async fn hydrate_reducible_state_if_newer(&self, checkpoint: TypedRuntimeCheckpoint, revision: u64) -> Result<bool>;
    pub async fn inspect(&self) -> Result<VmStateSnapshot>;
    /// Replace reducible VM state with an exact application-owned checkpoint, even when its revision is numerically lower than the current lineage.
    pub async fn replace_reducible_state(&self, checkpoint: TypedRuntimeCheckpoint, revision: u64) -> Result<()>;
    /// Apply one user approval decision to the exact prompt emitted for a suspended ProgramRun.
    pub async fn resolve_typed_approval(&self, prompt: &ApprovalPrompt, choice: ApprovalChoice, actor: impl Into<String>) -> Result<ExecutionOutcome>;
    /// Resume a typed execution that previously yielded or awaited approval.
    pub async fn resume_typed_execution(&self, execution_id: uuid::Uuid) -> Result<ExecutionOutcome>;
    /// Resume an awaited host effect only if it is still the same portable `(execution_id, sequence)` boundary.
    pub async fn resume_typed_execution_for_effect(&self, execution_id: uuid::Uuid, effect_sequence: u64) -> Result<ExecutionOutcome>;
    /// Resume a specific awaited host effect with an externally produced, verifier-checked result.
    pub async fn resume_typed_execution_with_effect_result(&self, execution_id: uuid::Uuid, effect_sequence: u64, values: Vec<TypedValue>) -> Result<ExecutionOutcome>;
    /// Apply one portable host reply.
    pub async fn resume_vm_effect(&self, resume: VmResume) -> Result<ExecutionOutcome>;
    pub async fn submit(&self, submission: ProgramSubmission) -> Result<ExecutionOutcome>;
    pub async fn submit_as(&self, submission: ProgramSubmission, caller: Option<agents::AgentIdentity>) -> Result<ExecutionOutcome>;
    /// Typed-only variant for a child/agent caller.
    pub async fn submit_as_typed_only(&self, submission: ProgramSubmission, caller: Option<agents::AgentIdentity>) -> Result<ExecutionOutcome>;
    /// Typed-only variant retaining the per-ProgramRun presentation binding.
    pub async fn submit_as_typed_only_with_typed_effect_sink(&self, submission: ProgramSubmission, caller: Option<agents::AgentIdentity>, effect_sink: TypedEffectSink) -> Result<ExecutionOutcome>;
    /// Equivalent to [`Self::submit_with_typed_effect_sink`] for a child agent whose ancestry must be preserved by host capability bindings.
    pub async fn submit_as_with_typed_effect_sink(&self, submission: ProgramSubmission, caller: Option<agents::AgentIdentity>, effect_sink: TypedEffectSink) -> Result<ExecutionOutcome>;
    /// Execute source through the shared typed runtime only.
    pub async fn submit_typed_only(&self, submission: ProgramSubmission) -> Result<ExecutionOutcome>;
    /// Submit with a portable host boundary for every awaited capability.
    pub async fn submit_with_deferred_host_effects(&self, submission: ProgramSubmission, effect_sink: TypedEffectSink) -> Result<ExecutionOutcome>;
    /// Submit with an event-loop binding that explicitly owns proposal editing.
    pub async fn submit_with_deferred_program_effects(&self, submission: ProgramSubmission, effect_sink: TypedEffectSink) -> Result<ExecutionOutcome>;
    /// Submit one ProgramRun with a presentation binding owned by the caller.
    pub async fn submit_with_typed_effect_sink(&self, submission: ProgramSubmission, effect_sink: TypedEffectSink) -> Result<ExecutionOutcome>;
    /// Replace the host-owned policy atomically with the corresponding grant revocations.
    pub fn apply_capability_policy(&self, policy: CapabilityPolicy, actor: impl Into<String>) -> Result<Vec<uuid::Uuid>>;
    pub fn archive(&self) -> Result<ProgramRuntimeArchive>;
    /// Attach the application-owned agent scheduler, replacing any prior attachment.
    pub fn attach_agent_scheduler<S: agents::AgentSpawning + 'static>(&self, scheduler: &Arc<S>);
    /// Attach the host's MemTree service to the typed capability boundary.
    pub fn attach_memory(&self, memory: Arc<crate::memory::MemorySystem>);
    pub fn authority_state(&self) -> Result<ProgramRuntimeAuthorityState>;
    pub fn automation(&self) -> Arc<AutomationBroker>;
    /// Install the application-owned effect delivery log.
    pub fn bind_effect_delivery_log(&self, log: Arc<Mutex<VmEffectDeliveryLog>>) -> Result<()>;
    /// Install the host-owned root behind `root<host-machine>`.
    pub fn bind_host_machine_root(&self, root: impl Into<PathBuf>) -> Result<()>;
    /// Install an application-selected project root.
    pub fn bind_project_root(&self, root: impl Into<PathBuf>) -> Result<()>;
    /// Install the output directory assigned to this task/session.
    pub fn bind_task_output_root(&self, root: impl Into<PathBuf>) -> Result<()>;
    /// Deliberately expose the filesystem root as `root<host-machine>`.
    pub fn bind_whole_machine_root(&self) -> Result<()>;
    /// Compatibility boolean form of [`Self::cancel_typed_execution_with_outcome`].
    pub fn cancel_typed_execution(&self, execution_id: uuid::Uuid) -> Result<bool>;
    /// Cancel a suspended VM execution and return its durable audit outcome.
    pub fn cancel_typed_execution_with_outcome(&self, execution_id: uuid::Uuid) -> Result<Option<ExecutionOutcome>>;
    /// Report whether the application has an implementation for a capability independently of whether this ProgramRun currently has a grant.
    pub fn capability_availability(&self, requirement: &CapabilityRequirement) -> CapabilityAvailability;
    pub fn capability_ledger(&self) -> Result<CapabilityLedger>;
    pub fn capability_policy(&self) -> Result<CapabilityPolicy>;
    pub fn capability_project_id(&self) -> &str;
    pub fn capability_session_id(&self) -> uuid::Uuid;
    /// Disconnect an archived/detached runtime from its former policy file.
    pub fn clear_authority_sink(&self) -> Result<()>;
    /// Remove the host binding.
    pub fn clear_host_machine_root(&self) -> Result<()>;
    pub fn clear_project_root(&self) -> Result<()>;
    pub fn clear_task_output_root(&self) -> Result<()>;
    /// Snapshot only source-language linking context for report-only replay.
    pub fn compiler_context(&self) -> Result<ProgramCompilerContext>;
    /// Record a deliberate denial for the exact awaited portable effect and discard its uncommitted continuation.
    pub fn deny_typed_execution_for_effect(&self, execution_id: uuid::Uuid, effect_sequence: u64, reason: impl Into<String>) -> Result<ExecutionOutcome>;
    /// The bound delivery log, if this runtime is a production Brain instance.
    pub fn effect_delivery_log(&self) -> Option<Arc<Mutex<VmEffectDeliveryLog>>>;
    /// Restore a retained reducible revision window.
    pub fn from_archive(archive: ProgramRuntimeArchive) -> Result<Self>;
    /// Restore reducible VM state and host authority as two independently validated records.
    pub fn from_archive_with_authority(archive: ProgramRuntimeArchive, authority: ProgramRuntimeAuthorityState) -> Result<Self>;
    /// Construct a fresh shared program runtime from reducible typed VM state.
    pub fn from_checkpoint(checkpoint: TypedRuntimeCheckpoint) -> Result<Self>;
    /// Restore reducible state at a durable application-owned revision.
    pub fn from_checkpoint_at_revision(checkpoint: TypedRuntimeCheckpoint, revision: u64) -> Result<Self>;
    /// Grant a typed capability after an approval decision.
    pub fn grant_typed_capability(&self, requirement: CapabilityRequirement) -> Result<uuid::Uuid>;
    /// Whether this runtime already has an application-owned MCP transport.
    pub fn has_mcp_client(&self) -> bool;
    /// Record reusable or exact authority without placing it in ambient VM state.
    pub fn issue_typed_capability(&self, mut requirement: CapabilityRequirement, scope: GrantScope, actor: impl Into<String>, expires_at_unix_ms: Option<u64>) -> Result<uuid::Uuid>;
    pub fn manifest_generation(&self) -> u64;
    pub fn new() -> Self;
    /// Return a UI-safe summary of a suspended execution without exposing its stack, captures, or capability arguments to an unrelated client.
    pub fn pending_typed_execution(&self, execution_id: uuid::Uuid) -> Result<Option<PendingTypedExecutionInfo>>;
    /// Number of private continuations retained by this runtime.
    pub fn pending_typed_execution_count(&self) -> Result<usize>;
    /// Restore authority only before this runtime is shared with concurrent callers.
    pub fn restore_authority_state(&mut self, state: ProgramRuntimeAuthorityState) -> Result<()>;
    /// Restore host-owned authority records independently of a VM checkpoint.
    pub fn restore_capability_ledger(&self, ledger: CapabilityLedger) -> Result<()>;
    pub fn revision(&self) -> u64;
    pub fn revision_history(&self) -> Result<Vec<VmRevisionSnapshot>>;
    /// Revoke one recorded grant by stable identity.
    pub fn revoke_typed_capability(&self, grant_id: uuid::Uuid) -> Result<bool>;
    /// Install application persistence for subsequent authority mutations.
    pub fn set_authority_sink(&self, sink: ProgramRuntimeAuthoritySink) -> Result<()>;
    pub fn with_automation(enabled: bool) -> Self;
}
/// Versioned bounded window of reducible state for a persistent shared VM.
pub struct ProgramRuntimeArchive { … }
/// Durable storage for one persistent [`ProgramRuntime`].
pub struct ProgramRuntimeArchiveStore { … }
impl ProgramRuntimeArchiveStore {
    pub fn load(&self) -> Result<Option<ProgramRuntime>>;
    pub fn load_archive(&self) -> Result<Option<ProgramRuntimeArchive>>;
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn path(&self) -> &Path;
    /// Validate and atomically replace the stored runtime archive.
    pub fn save(&self, runtime: &ProgramRuntime) -> Result<()>;
}
/// Application-owned persistence hook for host authority.
pub type ProgramRuntimeAuthoritySink = Arc<dyn Fn(ProgramRuntimeAuthorityState) -> Result<()> + Send + Sync>;
/// Host-owned authority state persisted beside, never inside, the reducible VM archive.
pub struct ProgramRuntimeAuthorityState { … }
/// Durable storage for application-owned authority associated with one [`ProgramRuntime`].
pub struct ProgramRuntimeAuthorityStore { … }
impl ProgramRuntimeAuthorityStore {
    pub fn load_state(&self) -> Result<Option<ProgramRuntimeAuthorityState>>;
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn path(&self) -> &Path;
    /// Restore a stored authority record into a newly constructed runtime.
    pub fn restore_into(&self, runtime: &mut ProgramRuntime) -> Result<bool>;
    /// Validate and atomically replace the stored authority record.
    pub fn save(&self, runtime: &ProgramRuntime) -> Result<()>;
    /// Validate and atomically replace an application-supplied authority snapshot.
    pub fn save_state(&self, authority: ProgramRuntimeAuthorityState) -> Result<()>;
}
pub struct ProgramSubmission { … }
pub enum ResourceRootAuditAction { Bound, Revoked }
pub struct ResourceRootAuditEntry { … }
pub struct ResourceRootBindingRecord { … }
/// Send-safe proxy for the daemon-owned run-scoped effect audit capability.
pub struct RunnerEffectAuditControl { … }
impl RunnerEffectAuditControl {
    pub async fn reserve(&self, execution_id: uuid::Uuid, effect: crate::vm::VmSideEffect) -> Result<RunnerEffectAuditReservation, String>;
}
pub(crate) enum RunnerEffectAuditControlRequest { Reserve }
/// One accepted intent.
pub struct RunnerEffectAuditReservation { … }
impl RunnerEffectAuditReservation {
    pub async fn begin(self) -> Result<RunnerHostEffectPermit, String>;
    pub async fn not_applied(self, reason: impl Into<String>) -> Result<(), String>;
}
pub(crate) enum RunnerEffectAuditReservationRequest { Begin, NotApplied }
pub(crate) struct RunnerHostEffectFinishRequest { … }
pub enum RunnerHostEffectOutcome { Acknowledged, NotApplied, FailedPartial }
/// Opaque proof that the daemon fsynced `AwaitingHostResult`.
pub struct RunnerHostEffectPermit { … }
impl RunnerHostEffectPermit {
    pub async fn finish(self, outcome: RunnerHostEffectOutcome) -> Result<(), String>;
}
/// One versioned Runtime/Application ABI record.
pub enum RuntimeApplicationMessage { ProgramRun, Diagnostic, Envelope, Resume, EffectHandle, OutputHandle, CursorAck }
impl RuntimeApplicationMessage {
    /// ABI version to stamp on a packed frame.
    pub fn abi_version(&self) -> u32;
}
/// Host-specific projection of one portable typed VM event.
pub type TypedEffectSink = Arc<dyn Fn(VmEffectEnvelope) + Send + Sync>;
pub struct TypedVmStackCell { … }
/// Append-only effect delivery state owned by an embedding application.
pub struct VmEffectDeliveryLog { … }
impl VmEffectDeliveryLog {
    /// Record that one consumer durably projected a contiguous prefix.
    pub fn acknowledge(&mut self, consumer: impl Into<String>, execution_id: Uuid, through_sequence: u64) -> Result<bool>;
    /// Record that one Brain/client identity durably projected a cursor.
    pub fn acknowledge_identity(&mut self, consumer: DeliveryConsumerIdentity, cursor: DeliveryCursor) -> Result<bool>;
    /// Persist one event before projecting it.
    pub fn append(&mut self, envelope: VmEffectEnvelope) -> Result<bool>;
    /// Optional Brain identity this log is bound to.
    pub fn brain_id(&self) -> Option<Uuid>;
    /// Current cursor for a string consumer on one ProgramRun, if any.
    pub fn cursor(&self, consumer: &str, execution_id: Uuid) -> Option<DeliveryCursor>;
    /// Current cursor for a Brain/client identity on one ProgramRun, if any.
    pub fn cursor_for(&self, consumer: &DeliveryConsumerIdentity, execution_id: Uuid) -> Option<DeliveryCursor>;
    /// Look up one persisted envelope by its effect handle.
    pub fn get(&self, handle: VmEffectHandle) -> Option<&VmEffectEnvelope>;
    pub fn open(path: impl Into<PathBuf>) -> Result<Self>;
    /// Open a delivery log bound to one Brain identity.
    pub fn open_bound(path: impl Into<PathBuf>, brain_id: Uuid) -> Result<Self>;
    /// Concurrent output handles observed for one ProgramRun.
    pub fn output_handles(&self, execution_id: Uuid) -> Vec<OutputHandleRef>;
    pub fn path(&self) -> &Path;
    pub fn pending(&self, consumer: &str) -> Vec<VmEffectEnvelope>;
    /// Unacknowledged suffix for one Brain/client identity.
    pub fn pending_for(&self, consumer: &DeliveryConsumerIdentity) -> Vec<VmEffectEnvelope>;
    /// Unacknowledged events that target one concurrent output handle at its exact generation.
    pub fn pending_for_handle(&self, consumer: &DeliveryConsumerIdentity, handle: &OutputHandleRef) -> Vec<VmEffectEnvelope>;
}
/// A portable VM event attached to its owning ProgramRun.
pub struct VmEffectEnvelope { … }
impl VmEffectEnvelope {
    /// Stable `(execution_id, sequence)` handle for this envelope.
    pub fn handle(&self) -> VmEffectHandle;
    /// Concurrent output handle targeted by this event, if any.
    pub fn output_handle(&self) -> Option<OutputHandleRef>;
    /// ProgramRun identity carried by this envelope.
    pub fn program_run(&self) -> ProgramRun;
}
/// Stable identity for one journaled VM effect.
pub struct VmEffectHandle { … }
/// The portable, correlated reply to one awaited VM effect.
pub struct VmResume { … }
/// Host outcome carried by [`VmResume`].
pub enum VmResumeResponse { Result, Denied, Cancelled }
/// An immutable in-memory checkpoint at a successful VM commit boundary.
pub struct VmRevisionSnapshot { … }
pub struct VmStackCell { … }
pub struct VmStateSnapshot { … }
pub struct VmVocabularyEntry { … }
```

## Traits

```rust
/// Somewhere a typed program's child agents can be run.
pub trait AgentSpawning: Send + Sync {
    async fn spawn(&self, spec: AgentTaskSpec, parent: Option<&AgentIdentity>) -> Result<AgentIdentity>;
    async fn authorize(&self, task_id: Uuid, parent: Option<&AgentIdentity>) -> Result<()>;
    async fn poll(&self, task_id: Uuid) -> Result<AgentTaskSnapshot>;
    async fn wait(&self, task_id: Uuid) -> Result<AgentTaskResult>;
    async fn cancel(&self, task_id: Uuid) -> Result<()>;
}
```

## Functions

```rust
/// Persist each new envelope before projecting it to a live observer.
pub fn bind_delivery_log(log: Arc<Mutex<VmEffectDeliveryLog>>, downstream: Option<TypedEffectSink>) -> TypedEffectSink { … }
pub fn parse_task_id(value: &str) -> Result<Uuid> { … }
/// Conservative key for scoping prior permission observations.
pub fn permission_context_key() -> String { … }
/// Human-readable context for the process asking macOS for TCC trust.
pub fn permission_target_description() -> String { … }
pub(crate) fn replay_fence_transition(entry: &EffectAuditEntry) -> Result<EffectAuditTransition> { … }
/// Create a thread-safe, single-consumer adapter for portable VM effects.
pub fn typed_effect_channel() -> (TypedEffectSink, mpsc::Receiver<VmEffectEnvelope>) { … }
/// Render one spreadsheet cell as the text a user or program sees.
pub(crate) fn workbook_cell_to_string(cell: &calamine::Data) -> String { … }
```

## Constants

```rust
/// Byte budget reserved for the compact replay index.
pub const EFFECT_AUDIT_REPLAY_INDEX_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// Brain-wide unresolved admission bound across concurrent runs.
pub const MAX_ACTIVE_EFFECT_AUDITS_PER_BRAIN: usize = 256;
/// Maximum number of non-terminal effects owned by one named-Brain run.
pub const MAX_ACTIVE_EFFECT_AUDITS_PER_RUN: usize = 64;
/// Brain-wide bound for canonical pre-redaction effect payloads represented by unresolved reservations.
pub const MAX_ACTIVE_EFFECT_AUDIT_BYTES_PER_BRAIN: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CONTEXT_ARTIFACT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_CONTEXT_FIELD_BYTES: usize = 1024;
pub(crate) const MAX_CONTEXT_REFERENCES: usize = 64;
pub(crate) const MAX_CONTEXT_TOTAL_BYTES: usize = 256 * 1024;
pub(crate) const MAX_DEPTH: usize = 4;
/// Maximum serialized intent admitted for one host effect.
pub const MAX_EFFECT_AUDIT_INTENT_BYTES: usize = 256 * 1024;
/// Upper bound for the audit share of the canonical Brain journal.
pub const MAX_EFFECT_AUDIT_JOURNAL_BYTES_PER_BRAIN: usize = 64 * 1024 * 1024;
/// Maximum serialized terminal outcome retained for one host effect.
pub const MAX_EFFECT_AUDIT_OUTCOME_BYTES: usize = 64 * 1024;
/// Capacity derived from the fixed event bound and replay-index budget.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN: usize = EFFECT_AUDIT_REPLAY_INDEX_BUDGET_BYTES / MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES;
/// Maximum encoded bytes for its complete canonical Brain event envelope.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES: usize = 1_024;
/// Maximum encoded bytes for one compact replay fence transition.
pub const MAX_EFFECT_AUDIT_REPLAY_FENCE_TRANSITION_BYTES: usize = 768;
pub(crate) const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Hard per-runtime bound for private transactional continuations.
pub const MAX_PENDING_TYPED_EXECUTIONS: usize = 256;
/// Full reducible checkpoints are intentionally expensive.
pub const MAX_RETAINED_VM_REVISIONS: usize = 256;
pub(crate) const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub(crate) const MAX_TURNS: usize = 10;
pub const PROGRAM_RUNTIME_ARCHIVE_VERSION: u32 = 1;
pub const PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION: u32 = 2;
```
