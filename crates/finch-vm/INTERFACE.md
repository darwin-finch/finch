# finch-vm — public interface

Generated from [`crates/finch-vm/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-vm/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Re-exported from `finch-vm-core`.
pub enum ApprovalChoice { Deny, AllowOnce, AllowTask, AllowSession, AllowProjectExact, AllowProjectPattern, AllowGlobal }
/// Re-exported from `finch-vm-core`.
pub struct ApprovalPrompt { … }
impl ApprovalPrompt {
    pub fn for_request(request: CapabilityRequest) -> Self;
}
/// Re-exported from `finch-vm-core`.
pub struct AuthorizationContext { … }
/// Re-exported from `finch-vm-core`.
pub enum AuthorizationDecision { Allowed, ApprovalRequired, Denied }
/// Re-exported from `finch-vm-core`.
pub struct BasicBlock { … }
/// Re-exported from `finch-vm-core`.
pub enum CapabilityAuditAction { Granted, Revoked, Consumed }
/// Re-exported from `finch-vm-core`.
pub struct CapabilityAuditEntry { … }
/// Source-free record of one authorization decision. Re-exported from `finch-vm-core`.
pub struct CapabilityAuthorizationAuditEntry { … }
/// Re-exported from `finch-vm-core`.
pub enum CapabilityAvailability { Disabled, Unsupported, PermissionRequired, Available, Degraded }
/// Re-exported from `finch-vm-core`.
pub struct CapabilityGrant { … }
impl CapabilityGrant {
    pub fn is_active(&self, now_unix_ms: u64) -> bool;
}
/// Re-exported from `finch-vm-core`.
pub enum CapabilityKind { VmRead, VmWrite, FileRead, FileWrite, NetworkConnect, AutomationInspect, AutomationWrite, AgentSpawn, AgentAwait, AgentPoll, AgentCancel, ProcessRun, SessionEmit, MemoryRead, MemoryWrite, MemoryConsolidate, ScheduleCreate, ScheduleRead, ScheduleManage, ProgramInvoke, McpCall, UnsafeMemory }
/// Application-owned authority records. Re-exported from `finch-vm-core`.
pub struct CapabilityLedger { … }
impl CapabilityLedger {
    /// Authorize and audit one concrete request.
    pub fn authorize(&mut self, request: &CapabilityRequest, context: &AuthorizationContext, actor: impl Into<String>) -> AuthorizationDecision;
    pub fn deny(&mut self, request: &CapabilityRequest, reason: impl Into<String>, actor: impl Into<String>, now_unix_ms: u64) -> AuthorizationDecision;
    pub fn grant_global(&mut self, requirement: CapabilityRequirement, policy_hash: impl Into<String>, actor: impl Into<String>, now_unix_ms: u64) -> Uuid;
    pub fn issue(&mut self, requirement: CapabilityRequirement, scope: GrantScope, policy_hash: impl Into<String>, actor: impl Into<String>, now_unix_ms: u64, expires_at_unix_ms: Option<u64>) -> Result<Uuid, String>;
    /// Return the decision already recorded for this exact effect boundary.
    pub fn recorded_authorization(&self, request: &CapabilityRequest, context: &AuthorizationContext) -> Option<AuthorizationDecision>;
    pub fn revoke(&mut self, grant_id: Uuid, actor: impl Into<String>, now_unix_ms: u64) -> bool;
    /// Remove an authorization attempt which provably reached no host use.
    pub fn rollback_authorization(&mut self, request: &CapabilityRequest) -> bool;
}
/// Host-owned approval policy. Re-exported from `finch-vm-core`.
pub struct CapabilityPolicy { … }
impl CapabilityPolicy {
    pub fn permits(&self, requirement: &CapabilityRequirement) -> bool;
    pub fn validate(&self) -> Result<(), String>;
}
/// Re-exported from `finch-vm-core`.
pub struct CapabilityRequest { … }
/// Re-exported from `finch-vm-core`.
pub struct CapabilityRequirement { … }
impl CapabilityRequirement {
    /// Whether this grant covers the requested capability.
    pub fn covers(&self, requested: &Self) -> bool;
    pub fn file(operation: FileOperation, selector: FileSelector) -> Self;
}
/// Re-exported from `finch-vm-core`.
pub enum ControlEffect { Returns, MayThrow, MaySuspend, NeverReturns }
/// The executable destination of a built-in word after verification. Re-exported from `finch-vm-core`.
pub enum CoreHostBinding { SessionEmit, VmVocabulary, CapabilityList, FileRead, FileHash, TreeList, TreeMerkle, FileSize, FileSlice, FileLinesOpen, FileLinesNext, FileLinesClose, CsvOpen, CsvSummary, CsvNext, CsvClose, WorkbookOpen, WorkbookSheetOpen, WorkbookSheets, WorkbookRange, WorkbookSummary, StreamNext, StreamClose, FileWrite, ProcessRun, McpCall, ProposalOpen, NetworkConnect, NetworkSend, MemoryRecall, MemoryIndexStatus, MemoryStore, ScheduleCreate, ScheduleGet, ScheduleCancel, AgentSpawn, AgentSpawnWith, AgentAwait, AgentPoll, AgentCancel, AutomationAvailability, AutomationDisplays, AutomationWindows, AutomationClick, AutomationType }
/// Provider-facing protocol documentation for an executable core word. Re-exported from `finch-vm-core`.
pub struct CoreWordDocumentation { … }
/// Re-exported from `finch-vm-core`.
pub enum CoreWordImplementation { Interpreter, VmInstruction, HostEffect }
/// One inspectable production core-word contract. Re-exported from `finch-vm-core`.
pub struct CoreWordSpec { … }
/// Re-exported from `finch-vm-core`.
pub enum DiagnosticPhase { Reader, MacroExpansion, NameResolution, TypeInference, Verification, Linking, Authorization, Availability, Approval, Interpretation, HostCall, NativeExecution, TransactionCommit, ChildExecution, Cancellation, ResourceLimit }
impl DiagnosticPhase {
    /// The phase as a reader would name it, matching the serialised form.
    pub fn label(&self) -> &'static str;
}
/// One durable, idempotently-addressable host-effect record.
pub struct EffectJournalEntry { … }
pub enum EffectJournalState { Proposed, AwaitingApproval, AwaitingHostResult, Acknowledged, Denied, Cancelled, Failed }
/// Re-exported from `finch-vm-core`.
pub struct EffectSet(pub BTreeSet<CapabilityRequirement>);
impl EffectSet {
    pub fn from_requirement(requirement: CapabilityRequirement) -> Self;
    pub fn grants(&self, requested: &Self) -> bool;
    pub fn is_pure(&self) -> bool;
    pub fn pure() -> Self;
    pub fn union(&self, other: &Self) -> Self;
}
/// Re-exported from `finch-vm-core`.
pub enum FileOperation { Read, Write }
/// A normalized pattern relative to an immutable resource root. Re-exported from `finch-vm-core`.
pub struct FileSelector { … }
impl FileSelector {
    /// Returns true only when containment is proven by the restricted selector algebra.
    pub fn contains_selector(&self, requested: &Self) -> bool;
    pub fn intersection(&self, other: &Self) -> Result<Self, SelectorError>;
    pub fn matches(&self, relative_path: &str) -> bool;
    pub fn parse(input: &str) -> Result<Self, SelectorError>;
}
/// A deliberately small expression language for argument-dependent file capabilities. Re-exported from `finch-vm-core`.
pub struct FileSelectorTemplate { … }
impl FileSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<FileSelector, SelectorError>;
}
/// Re-exported from `finch-vm-core`.
pub enum FileSelectorTemplatePart { Literal, Argument }
/// Re-exported from `finch-vm-core`.
pub struct Function { … }
/// Re-exported from `finch-vm-core`.
pub enum GrantScope { Once, Task, Session, Project, Global }
/// Re-exported from `finch-vm-core`.
pub struct GrantSet { … }
impl GrantSet {
    pub fn active_global_requirements(&self, now_unix_ms: u64) -> impl Iterator<Item = &CapabilityRequirement>;
    /// Active reusable grants with their stable host-owned identity retained.
    pub fn active_grants_for<'a>(&'a self, context: &'a AuthorizationContext) -> impl Iterator<Item = &'a CapabilityGrant> + 'a;
    /// Active reusable authority applicable to one ProgramRun.
    pub fn active_requirements_for<'a>(&'a self, context: &'a AuthorizationContext) -> impl Iterator<Item = &'a CapabilityRequirement> + 'a;
    pub fn authorize(&self, request: &CapabilityRequest, context: &AuthorizationContext) -> AuthorizationDecision;
    pub fn revoke(&mut self, grant_id: Uuid, now_unix_ms: u64) -> bool;
}
pub enum HostSideEffect { Emit, Ui, Request }
/// Re-exported from `finch-vm-core`.
pub enum Instruction { Constant, MakeList, MakeMap, MakeRecord, MakeVariant, VariantGet, RecordGet, RecordSet, Dup, Drop, Swap, LocalGet, LocalSet, CaptureGet, MakeClosure, Call, CallClosure, CapabilityRequest, OutputOpen, UiEffect, Yield, DeferFiber, NextFiber, JoinFiber, CancelFiber, DeferCpu, PollCpuFiber, JoinCpuFiber, CancelCpuFiber, PropagateResult, Jump, Branch, Return, Trap }
impl Instruction {
    pub fn is_terminator(&self) -> bool;
}
pub struct InterpreterConfig { … }
/// Re-exported from `finch-vm-core`.
pub struct LocatedInstruction { … }
impl LocatedInstruction {
    pub fn generated(instruction: Instruction, word: impl Into<String>) -> Self;
}
/// Argument-dependent MCP authority. Re-exported from `finch-vm-core`.
pub struct McpSelectorTemplate { … }
impl McpSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<(String, String), SelectorError>;
}
/// Re-exported from `finch-vm-core`.
pub struct Module { … }
impl Module {
    pub fn single(function: Function) -> Self;
}
/// The only compiler phase permitted to reach execution. Re-exported from `finch-vm-core`.
pub struct ModuleVerified { … }
impl ModuleVerified {
    /// The independently verified module retained for interpretation.
    pub fn as_verified(&self) -> &VerifiedModule;
    /// Unwrap the serializable verified module used by checkpoints.
    pub fn into_verified(self) -> VerifiedModule;
}
/// Argument-dependent network authority. Re-exported from `finch-vm-core`.
pub struct NetworkSelectorTemplate { … }
impl NetworkSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<(String, u16), SelectorError>;
}
pub struct PendingHostCall { … }
/// Argument-dependent process authority. Re-exported from `finch-vm-core`.
pub struct ProcessSelectorTemplate { … }
impl ProcessSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<String, SelectorError>;
}
pub struct ProducerFiberRecord { … }
pub enum ProducerFiberState { Ready, Completed, Failed, Cancelled }
/// Language in which a stored program's canonical source is written. Re-exported from `finch-vm-core`.
pub enum ProgramLanguage { Forth, Lisp }
impl ProgramLanguage {
    pub fn as_str(self) -> &'static str;
    /// Compact wire-format inference used only when the submission envelope omits `language`; the resolved value is recorded before execution.
    pub fn infer_source(source: &str) -> Self;
    /// Resolve the compact provider wire form before parsing.
    pub fn infer_wire_source(source: &str) -> Result<Self>;
}
/// Argument-dependent proposal authority. Re-exported from `finch-vm-core`.
pub struct ProgramSelectorTemplate { … }
impl ProgramSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<String, SelectorError>;
}
/// Re-exported from `finch-vm-core`.
pub enum ResourceRoot { Workspace, Project, TaskOutput, HostMachine, Named }
/// Re-exported from `finch-vm-core`.
pub enum ResourceSelector { None, File, FileTemplate, NetworkTemplate, Network, Automation, Agent, Process, ProcessTemplate, Program, ProgramTemplate, Mcp, McpTemplate, Memory, Schedule }
/// Re-exported from `finch-vm-core`.
pub enum SelectorError { Empty, AbsolutePath, ParentTraversal, UnknownRoot, InvalidRecursiveWildcard, DifferentRoots, IndeterminateIntersection, InvalidTemplateArgument, TemplateArgumentOutOfBounds, WildcardInRuntimePath, InvalidSeparator, InvalidNetworkTemplateArgument, NetworkTemplateArgumentOutOfBounds, InvalidProcessTemplateArgument, ProcessTemplateArgumentOutOfBounds, InvalidProgramTemplateArgument, ProgramTemplateArgumentOutOfBounds, InvalidMcpTemplateArgument, McpTemplateArgumentOutOfBounds }
/// Re-exported from `finch-vm-core`.
pub enum Severity { Note, Warning, Error }
/// Re-exported from `finch-vm-core`.
pub enum SourceLanguage { Forth, Lisp, FinchIr, Native, Provider }
/// Re-exported from `finch-vm-core`.
pub struct SourceOrigin { … }
impl SourceOrigin {
    pub fn generated(word: impl Into<String>) -> Self;
}
/// Re-exported from `finch-vm-core`.
pub struct SourceSpan { … }
impl SourceSpan {
    pub fn bytes(source_id: impl Into<String>, start_byte: usize, end_byte: usize) -> Self;
}
/// A typed stack row. Re-exported from `finch-vm-core`.
pub struct StackRow { … }
impl StackRow {
    pub fn closed(values: Vec<Type>) -> Self;
    pub fn polymorphic(tail: impl Into<String>, values: Vec<Type>) -> Self;
}
/// Complete contract for a callable word or function. Re-exported from `finch-vm-core`.
pub struct StackSignature { … }
impl StackSignature {
    pub fn pure(input: StackRow, output: StackRow) -> Self;
}
/// Typed contract for a callable that may cooperatively suspend. Re-exported from `finch-vm-core`.
pub struct SuspensionSignature { … }
impl SuspensionSignature {
    pub fn one_way(yield_type: Type) -> Self;
}
/// Portable typed value used at VM, task, suspension, and wire boundaries. Re-exported from `finch-vm-core`.
pub enum TaskKind { Agent, CpuFiber }
/// A language-level type shared by Co-Forth and Finch Lisp. Re-exported from `finch-vm-core`.
pub enum Type { Unit, Bool, Int, UInt, Float, Char, Symbol, String, Bytes, Json, Path, List, Map, Option, Result, Record, Variant, Function, Task, Fiber, Stream, Resource, Capability, Variable, Dynamic }
impl Type {
    /// Non-binding compatibility.
    pub fn accepts(&self, actual: &Type) -> bool;
    pub fn fiber_step(yield_type: Type, result_type: Type) -> Self;
    pub fn list(element: Type) -> Self;
    pub fn result(ok: Type, error: Type) -> Self;
    /// Result of observing a CPU task without consuming its only handle.
    pub fn task_poll(result_type: Type) -> Self;
}
/// Result of compiling and interpreting one source submission.
pub struct TypedExecution { … }
impl TypedExecution {
    /// Construct a failed execution from compiler or verifier diagnostics.
    pub fn failed(diagnostics: Vec<VmDiagnostic>) -> Self;
}
pub enum TypedExecutionStatus { Completed, Suspended, AuthorizationRequired, Failed }
/// Persistent typed stack shared by Finch Lisp and Co-Forth source.
pub struct TypedRuntime { … }
impl TypedRuntime {
    /// Cancel the private CPU worker referenced by a saved suspension, if the parent was waiting on one.
    pub fn cancel_suspended_cpu_fiber(&self, suspension: &TypedSuspension) -> Result<bool, VmDiagnostic>;
    /// Capture only state that can safely survive outside this process.
    pub fn checkpoint(&self) -> Result<TypedRuntimeCheckpoint, VmDiagnostic>;
    /// Execute a verified module.
    pub fn execute(&mut self, module: &ModuleVerified, fuel: u64) -> TypedExecution;
    pub fn execute_with_declaration(&mut self, module: &ModuleVerified, fuel: u64, declared: Option<&EffectSet>) -> TypedExecution;
    /// Execute using a host-owned capability handler.
    pub fn execute_with_handler<H: CapabilityHandler>(&mut self, module: &ModuleVerified, fuel: u64, declared: Option<&EffectSet>, handler: &mut H) -> TypedExecution;
    /// Restore a checkpoint into a fresh typed runtime and reverify every persisted definition against the current core vocabulary.
    pub fn from_checkpoint(checkpoint: TypedRuntimeCheckpoint) -> Result<Self, Vec<VmDiagnostic>>;
    pub fn functions(&self) -> &BTreeMap<String, Function>;
    pub fn grant(&mut self, requirement: CapabilityRequirement);
    pub fn grants(&self) -> &EffectSet;
    pub fn intrinsic_grants() -> EffectSet;
    pub fn new() -> Self;
    /// Replace application-supplied host vocabulary without allowing it to shadow a core word or a source-defined function.
    pub fn replace_host_vocabulary(&mut self, previous_names: impl IntoIterator<Item = String>, replacements: &BTreeMap<String, StackSignature>) -> Result<(), VmDiagnostic>;
    /// Resume exactly the host call already captured by `suspension` after the application has independently authorized its concrete request.
    pub fn resume_authorized_host_call_with_handler<H: CapabilityHandler>(&mut self, suspension: TypedSuspension, handler: &mut H) -> TypedExecution;
    /// Acknowledge one already-journaled host effect with values supplied by an external event loop.
    pub fn resume_with_effect_result<H: CapabilityHandler>(&mut self, suspension: TypedSuspension, effect_sequence: u64, values: Vec<TypedValue>, handler: &mut H) -> TypedExecution;
    /// Continue a previously returned VM boundary.
    pub fn resume_with_handler<H: CapabilityHandler>(&mut self, suspension: TypedSuspension, values: Vec<TypedValue>, handler: &mut H) -> TypedExecution;
    pub fn set_grants(&mut self, grants: EffectSet);
    pub fn stack(&self) -> &[TypedValue];
    pub fn vocabulary(&self) -> &Vocabulary;
}
/// Serializable reducible state of a typed runtime at a successful commit boundary.
pub struct TypedRuntimeCheckpoint { … }
/// Durable state for an execution paused at an explicit VM boundary.
pub struct TypedSuspension { … }
/// Portable typed value used at VM, task, suspension, and wire boundaries. Re-exported from `finch-vm-core`.
pub enum TypedValue { Unit, Bool, Int, UInt, Float, Char, Symbol, String, Bytes, Json, Path, List, Map, Option, Result, Record, Variant, Closure, Task, Fiber, Stream, Resource, Dynamic }
impl TypedValue {
    pub fn value_type(&self) -> Type;
}
/// A portable mutation requested for a host-owned output surface. Re-exported from `finch-vm-core`.
pub enum UiOperation { Create, Append, Replace, Status, Progress, Complete, Fail }
/// Bounded or indeterminate progress metadata carried as data, rather than terminal control codes.
pub struct UiProgress { … }
/// Re-exported from `finch-vm-core`.
pub struct VerifiedFunction { … }
/// A verified module is immutable execution data. Re-exported from `finch-vm-core`.
pub struct VerifiedModule { … }
/// Re-exported from `finch-vm-core`.
pub struct Verifier<'a> { … }
impl Verifier {
    /// Local structural certification of one function.
    pub fn certify_function(&self, function: &Function, module_functions: &BTreeMap<String, Function>) -> Result<VerifiedFunction, Vec<VmDiagnostic>>;
    pub fn new(vocabulary: &'a Vocabulary) -> Self;
    pub fn verify(&self, module: Module) -> Result<VerifiedModule, Vec<VmDiagnostic>>;
}
/// The state closed over by an internal zero-argument VM thunk.
pub struct VmContinuation { … }
/// Re-exported from `finch-vm-core`.
pub struct VmDiagnostic { … }
impl VmDiagnostic {
    pub fn error(code: impl Into<String>, phase: DiagnosticPhase, message: impl Into<String>, primary: Option<SourceOrigin>) -> Self;
    /// A report that names the offending source, not just the failure.
    pub fn render(&self, source: Option<&str>) -> String;
    pub fn type_mismatch(expected: Type, found: Type, primary: Option<SourceOrigin>) -> Self;
}
/// Serializable activation record for the VM's internal trampoline.
pub struct VmFrame { … }
/// One portable, ordered side-effect requested by the VM.
pub struct VmSideEffect { … }
/// One result from running the VM until its next observable boundary.
pub enum VmStep { Yielded, SpawnFiber, NextFiber, JoinFiber, CancelFiber, Emit, Await, SpawnCpuFiber, PollCpuFiber, JoinCpuFiber, CancelCpuFiber, Complete, Failed }
/// The handler-free execution core used by the runtime event-loop trampoline.
pub struct VmTrampoline<'a> { … }
impl VmTrampoline {
    pub fn new(module: &'a VerifiedModule, config: &InterpreterConfig) -> Self;
    /// Resume a yielded continuation with the typed values supplied by the event loop (for example, an approved file read or child result).
    pub fn resume(&self, mut continuation: VmContinuation, values: Vec<TypedValue>) -> VmStep;
    /// Run a bounded slice until it emits, awaits, completes, or fails.
    pub fn run(&self, mut continuation: VmContinuation) -> VmStep;
    pub fn start(&self, stack: Vec<TypedValue>) -> Result<VmContinuation, VmDiagnostic>;
    /// Start an isolated invocation of a verified function.
    pub fn start_function(&self, function: &str, captures: Vec<TypedValue>, stack: Vec<TypedValue>) -> Result<VmContinuation, VmDiagnostic>;
}
/// Re-exported from `finch-vm-core`.
pub type Vocabulary = BTreeMap<String, StackSignature>;
/// Why a provider's first Finch VM wire submission was not accepted.
pub enum WireFailureClass { RawProse, MarkdownFence, InventedWord, StackOrType, WrongLanguageDispatch, MissingOutputEffect, Capability, Other }
```

## Traits

```rust
pub trait CapabilityHandler {
    fn request(&mut self, requirement: &CapabilityRequirement, arguments: Vec<TypedValue>, origin: &SourceOrigin) -> Result<Vec<TypedValue>, VmDiagnostic>;
    fn request_with_authority_lease(&mut self, requirement: &CapabilityRequirement, arguments: Vec<TypedValue>, origin: &SourceOrigin) -> Result<Vec<TypedValue>, VmDiagnostic>;
    fn request_effect(&mut self, effect: &VmSideEffect) -> Result<Vec<TypedValue>, VmDiagnostic>;
    fn prepare_awaited_effect(&mut self, _effect: &mut VmSideEffect) -> Result<(), VmDiagnostic>;
    fn observe_awaited_effect(&mut self, _effect: &VmSideEffect) -> Result<(), VmDiagnostic>;
    fn authorize_awaited_effect(&mut self, _effect: &VmSideEffect) -> Result<(), VmDiagnostic>;
    fn defer_awaited_effect(&self, _effect: &VmSideEffect) -> bool;
    fn output(&self) -> String;
    fn emit(&mut self, _chunk: &str);
    fn side_effect(&mut self, effect: &VmSideEffect) -> Result<(), VmDiagnostic>;
    fn output_chunks(&self) -> Vec<String>;
    fn side_effects(&self) -> Vec<HostSideEffect>;
}
```

## Functions

```rust
/// Re-exported from `finch-vm-core`.
pub fn agent_task_result_type() -> Type { … }
/// Re-exported from `finch-vm-core`.
pub fn agent_task_snapshot_type() -> Type { … }
/// Canonical signatures for the first verified core. Re-exported from `finch-vm-core`.
pub fn agent_task_spec_type() -> Type { … }
/// Re-exported from `finch-vm-core`.
pub fn capability_grant_entry_type() -> Type { … }
/// Classify a rejected provider submission for aggregate conformance metrics.
pub fn classify_wire_failure(source: &str, diagnostic: &str) -> WireFailureClass { … }
/// Canonical signatures for verifier-facing consumers. Re-exported from `finch-vm-core`.
pub fn core_vocabulary() -> Vocabulary { … }
/// Return provider-neutral documentation for a registered core word. Re-exported from `finch-vm-core`.
pub fn core_word_documentation(name: &str) -> CoreWordDocumentation { … }
/// Return the immutable production registry. Re-exported from `finch-vm-core`.
pub fn core_word_registry() -> &'static BTreeMap<String, CoreWordSpec> { … }
/// Return the complete contract for one core word. Re-exported from `finch-vm-core`.
pub fn core_word_spec(name: &str) -> Option<CoreWordSpec> { … }
/// Instantiate a selector template in a declared capability requirement against the arguments of a call.
pub fn instantiate_requirement(requirement: &CapabilityRequirement, arguments: &[TypedValue]) -> Result<CapabilityRequirement, String> { … }
/// Re-exported from `finch-vm-core`.
pub fn tree_entry_type() -> Type { … }
/// Re-exported from `finch-vm-core`.
pub fn tree_listing_type() -> Type { … }
/// Return the stable leading diagnostic code without retaining diagnostic prose.
pub fn wire_diagnostic_code(diagnostic: &str) -> Option<String> { … }
```

## Constants

```rust
/// Version of the portable Runtime/Application effect, resume, and delivery ABI.
pub const RUNTIME_APPLICATION_ABI_VERSION: u32 = 1;
/// Version of the frontend-facing semantic-construction protocol. Re-exported from `finch-vm-core`.
pub const SEMANTIC_CONSTRUCTION_VERSION: u32 = 1;
/// Version of the typed VM contract and serialized IR family. Re-exported from `finch-vm-core`.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;
```
