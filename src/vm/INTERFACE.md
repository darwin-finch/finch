# vm — public interface

Generated from [`src/vm/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/vm/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)
- **May depend on:** nothing.

Everything below is what callers outside this subsystem can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub enum ApprovalChoice { Deny, AllowOnce, AllowTask, AllowSession, AllowProjectExact, AllowProjectPattern, AllowGlobal }
pub struct ApprovalPrompt { … }
pub struct AuthorizationContext { … }
pub enum AuthorizationDecision { Allowed, ApprovalRequired, Denied }
pub struct BasicBlock { … }
pub enum CapabilityAuditAction { Granted, Revoked, Consumed }
pub struct CapabilityAuditEntry { … }
/// Source-free record of one authorization decision.
pub struct CapabilityAuthorizationAuditEntry { … }
pub enum CapabilityAvailability { Disabled, Unsupported, PermissionRequired, Available, Degraded }
pub struct CapabilityGrant { … }
pub enum CapabilityKind { VmRead, VmWrite, FileRead, FileWrite, NetworkConnect, AutomationInspect, AutomationWrite, AgentSpawn, AgentAwait, AgentPoll, AgentCancel, ProcessRun, SessionEmit, MemoryRead, MemoryWrite, MemoryConsolidate, ScheduleCreate, ScheduleRead, ScheduleManage, ProgramInvoke, McpCall, UnsafeMemory }
/// Application-owned authority records.
pub struct CapabilityLedger { … }
/// Host-owned approval policy.
pub struct CapabilityPolicy { … }
pub struct CapabilityRequest { … }
pub struct CapabilityRequirement { … }
pub enum ControlEffect { Returns, MayThrow, MaySuspend, NeverReturns }
/// The executable destination of a built-in word after verification.
pub enum CoreHostBinding { SessionEmit, VmVocabulary, CapabilityList, FileRead, FileHash, TreeList, TreeMerkle, FileSize, FileSlice, FileLinesOpen, FileLinesNext, FileLinesClose, CsvOpen, CsvSummary, CsvNext, CsvClose, WorkbookOpen, WorkbookSheetOpen, WorkbookSheets, WorkbookRange, WorkbookSummary, StreamNext, StreamClose, FileWrite, ProcessRun, McpCall, ProposalOpen, NetworkConnect, NetworkSend, MemoryRecall, MemoryIndexStatus, MemoryStore, ScheduleCreate, ScheduleGet, ScheduleCancel, AgentSpawn, AgentSpawnWith, AgentAwait, AgentPoll, AgentCancel, AutomationAvailability, AutomationDisplays, AutomationWindows, AutomationClick, AutomationType }
/// Provider-facing protocol documentation for an executable core word.
pub struct CoreWordDocumentation { … }
pub enum CoreWordImplementation { Interpreter, VmInstruction, HostEffect }
/// One inspectable production core-word contract.
pub struct CoreWordSpec { … }
pub enum DiagnosticPhase { Reader, MacroExpansion, NameResolution, TypeInference, Verification, Linking, Authorization, Availability, Approval, Interpretation, HostCall, NativeExecution, TransactionCommit, ChildExecution, Cancellation, ResourceLimit }
/// One durable, idempotently-addressable host-effect record.
pub struct EffectJournalEntry { … }
pub enum EffectJournalState { Proposed, AwaitingApproval, AwaitingHostResult, Acknowledged, Denied, Cancelled, Failed }
pub struct EffectSet(pub BTreeSet<CapabilityRequirement>);
pub enum FileOperation { Read, Write }
/// A normalized pattern relative to an immutable resource root.
pub struct FileSelector { … }
/// A deliberately small expression language for argument-dependent file capabilities.
pub struct FileSelectorTemplate { … }
pub enum FileSelectorTemplatePart { Literal, Argument }
pub struct Function { … }
pub enum GrantScope { Once, Task, Session, Project, Global }
pub struct GrantSet { … }
pub enum HostSideEffect { Emit, Ui, Request }
pub enum Instruction { Constant, MakeList, MakeMap, MakeRecord, MakeVariant, VariantGet, RecordGet, RecordSet, Dup, Drop, Swap, LocalGet, LocalSet, CaptureGet, MakeClosure, Call, CallClosure, CapabilityRequest, OutputOpen, UiEffect, Yield, DeferFiber, NextFiber, JoinFiber, CancelFiber, DeferCpu, PollCpuFiber, JoinCpuFiber, CancelCpuFiber, PropagateResult, Jump, Branch, Return, Trap }
pub struct LocatedInstruction { … }
/// Argument-dependent MCP authority.
pub struct McpSelectorTemplate { … }
pub struct Module { … }
/// Argument-dependent network authority.
pub struct NetworkSelectorTemplate { … }
pub struct PendingHostCall { … }
/// Argument-dependent process authority.
pub struct ProcessSelectorTemplate { … }
pub struct ProducerFiberRecord { … }
pub enum ProducerFiberState { Ready, Completed, Failed, Cancelled }
/// Language in which a stored program's canonical source is written.
pub enum ProgramLanguage { Forth, Lisp }
/// Argument-dependent proposal authority.
pub struct ProgramSelectorTemplate { … }
pub enum ResourceRoot { Workspace, Project, TaskOutput, HostMachine, Named }
pub enum ResourceSelector { None, File, FileTemplate, NetworkTemplate, Network, Automation, Agent, Process, ProcessTemplate, Program, ProgramTemplate, Mcp, McpTemplate, Memory, Schedule }
pub enum SelectorError { Empty, AbsolutePath, ParentTraversal, UnknownRoot, InvalidRecursiveWildcard, DifferentRoots, IndeterminateIntersection, InvalidTemplateArgument, TemplateArgumentOutOfBounds, WildcardInRuntimePath, InvalidSeparator, InvalidNetworkTemplateArgument, NetworkTemplateArgumentOutOfBounds, InvalidProcessTemplateArgument, ProcessTemplateArgumentOutOfBounds, InvalidProgramTemplateArgument, ProgramTemplateArgumentOutOfBounds, InvalidMcpTemplateArgument, McpTemplateArgumentOutOfBounds }
pub enum Severity { Note, Warning, Error }
pub enum SourceLanguage { Forth, Lisp, FinchIr, Native, Provider }
pub struct SourceOrigin { … }
pub struct SourceSpan { … }
/// A typed stack row.
pub struct StackRow { … }
/// Complete contract for a callable word or function.
pub struct StackSignature { … }
/// Typed contract for a callable that may cooperatively suspend.
pub struct SuspensionSignature { … }
/// Portable typed value used at VM, task, suspension, and wire boundaries.
pub enum TaskKind { Agent, CpuFiber }
/// A language-level type shared by Co-Forth and Finch Lisp.
pub enum Type { Unit, Bool, Int, UInt, Float, Char, Symbol, String, Bytes, Json, Path, List, Map, Option, Result, Record, Variant, Function, Task, Fiber, Stream, Resource, Capability, Variable, Dynamic }
/// Result of compiling and interpreting one source submission.
pub struct TypedExecution { … }
pub enum TypedExecutionStatus { Completed, Suspended, AuthorizationRequired, Failed }
/// Persistent typed stack shared by Finch Lisp and Co-Forth source.
pub struct TypedRuntime { … }
/// Serializable reducible state of a typed runtime at a successful commit boundary.
pub struct TypedRuntimeCheckpoint { … }
/// Durable state for an execution paused at an explicit VM boundary.
pub struct TypedSuspension { … }
/// Portable typed value used at VM, task, suspension, and wire boundaries.
pub enum TypedValue { Unit, Bool, Int, UInt, Float, Char, Symbol, String, Bytes, Json, Path, List, Map, Option, Result, Record, Variant, Closure, Task, Fiber, Stream, Resource, Dynamic }
pub enum UiOperation { Create, Append, Replace, Status, Progress, Complete, Fail }
/// Bounded or indeterminate progress metadata carried as data, rather than terminal control codes.
pub struct UiProgress { … }
pub struct VerifiedFunction { … }
/// A verified module is immutable execution data.
pub struct VerifiedModule { … }
pub struct Verifier<'a> { … }
/// The state closed over by an internal zero-argument VM thunk.
pub struct VmContinuation { … }
pub struct VmDiagnostic { … }
/// Serializable activation record for the VM's internal trampoline.
pub struct VmFrame { … }
/// One portable, ordered side-effect requested by the VM.
pub struct VmSideEffect { … }
/// One result from running the VM until its next observable boundary.
pub enum VmStep { Yielded, SpawnFiber, NextFiber, JoinFiber, CancelFiber, Emit, Await, SpawnCpuFiber, PollCpuFiber, JoinCpuFiber, CancelCpuFiber, Complete, Failed }
/// The handler-free execution core used by the runtime event-loop trampoline.
pub struct VmTrampoline<'a> { … }
pub type Vocabulary = BTreeMap<String, StackSignature>;
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
pub fn agent_task_result_type() -> Type { … }
pub fn agent_task_snapshot_type() -> Type { … }
/// Canonical signatures for the first verified core.
pub fn agent_task_spec_type() -> Type { … }
pub fn capability_grant_entry_type() -> Type { … }
/// Compile user/model-entered Co-Forth source text directly into Finch typed stack IR and run the common verifier.
pub fn compile_forth(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<VerifiedModule, Vec<VmDiagnostic>> { … }
pub fn compile_forth_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<VerifiedModule, Vec<VmDiagnostic>> { … }
/// Parse and compile Finch Lisp directly into the common typed stack IR.
pub fn compile_lisp(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<VerifiedModule, Vec<VmDiagnostic>> { … }
pub fn compile_lisp_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<VerifiedModule, Vec<VmDiagnostic>> { … }
/// Canonical signatures for verifier-facing consumers.
pub fn core_vocabulary() -> Vocabulary { … }
/// Return provider-neutral documentation for a registered core word.
pub fn core_word_documentation(name: &str) -> CoreWordDocumentation { … }
/// Return the immutable production registry.
pub fn core_word_registry() -> &'static BTreeMap<String, CoreWordSpec> { … }
/// Return the complete contract for one core word.
pub fn core_word_spec(name: &str) -> Option<CoreWordSpec> { … }
/// Instantiate a selector template in a declared capability requirement against the arguments of a call.
pub fn instantiate_requirement(requirement: &CapabilityRequirement, arguments: &[TypedValue]) -> Result<CapabilityRequirement, String> { … }
pub fn tree_entry_type() -> Type { … }
pub fn tree_listing_type() -> Type { … }
```

## Constants

```rust
/// Version of the typed VM contract and serialized IR family.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;
```
