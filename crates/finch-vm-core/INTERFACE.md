# finch-vm-core — public interface

Generated from [`crates/finch-vm-core/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-vm-core/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
pub enum ApprovalChoice { Deny, AllowOnce, AllowTask, AllowSession, AllowProjectExact, AllowProjectPattern, AllowGlobal }
pub struct ApprovalPrompt { … }
impl ApprovalPrompt {
    pub fn for_request(request: CapabilityRequest) -> Self;
}
pub struct AuthorizationContext { … }
pub enum AuthorizationDecision { Allowed, ApprovalRequired, Denied }
pub struct BasicBlock { … }
/// Compiler-support identifier for a basic block in shared typed IR.
pub type BlockId = u32;
/// Then/else/merge blocks created for a boolean branch.
pub struct BoolBranch { … }
pub enum CapabilityAuditAction { Granted, Revoked, Consumed }
pub struct CapabilityAuditEntry { … }
/// Source-free record of one authorization decision.
pub struct CapabilityAuthorizationAuditEntry { … }
pub enum CapabilityAvailability { Disabled, Unsupported, PermissionRequired, Available, Degraded }
pub struct CapabilityGrant { … }
impl CapabilityGrant {
    pub fn is_active(&self, now_unix_ms: u64) -> bool;
}
pub enum CapabilityKind { VmRead, VmWrite, FileRead, FileWrite, NetworkConnect, AutomationInspect, AutomationWrite, AgentSpawn, AgentAwait, AgentPoll, AgentCancel, ProcessRun, SessionEmit, MemoryRead, MemoryWrite, MemoryConsolidate, ScheduleCreate, ScheduleRead, ScheduleManage, ProgramInvoke, McpCall, UnsafeMemory }
/// Application-owned authority records.
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
/// Host-owned approval policy.
pub struct CapabilityPolicy { … }
impl CapabilityPolicy {
    pub fn permits(&self, requirement: &CapabilityRequirement) -> bool;
    pub fn validate(&self) -> Result<(), String>;
}
pub struct CapabilityRequest { … }
pub struct CapabilityRequirement { … }
impl CapabilityRequirement {
    /// Whether this grant covers the requested capability.
    pub fn covers(&self, requested: &Self) -> bool;
    pub fn file(operation: FileOperation, selector: FileSelector) -> Self;
}
pub enum ControlEffect { Returns, MayThrow, MaySuspend, NeverReturns }
/// The executable destination of a built-in word after verification.
pub enum CoreHostBinding { SessionEmit, VmVocabulary, CapabilityList, FileRead, FileHash, TreeList, TreeMerkle, FileSize, FileSlice, FileLinesOpen, FileLinesNext, FileLinesClose, CsvOpen, CsvSummary, CsvNext, CsvClose, WorkbookOpen, WorkbookSheetOpen, WorkbookSheets, WorkbookRange, WorkbookSummary, StreamNext, StreamClose, FileWrite, ProcessRun, McpCall, ProposalOpen, NetworkConnect, NetworkSend, MemoryRecall, MemoryIndexStatus, MemoryStore, ScheduleCreate, ScheduleGet, ScheduleCancel, AgentSpawn, AgentSpawnWith, AgentAwait, AgentPoll, AgentCancel, AutomationAvailability, AutomationDisplays, AutomationWindows, AutomationClick, AutomationType }
/// Provider-facing protocol documentation for an executable core word.
pub struct CoreWordDocumentation { … }
pub enum CoreWordImplementation { Interpreter, VmInstruction, HostEffect }
/// One inspectable production core-word contract.
pub struct CoreWordSpec { … }
pub enum DiagnosticPhase { Reader, MacroExpansion, NameResolution, TypeInference, Verification, Linking, Authorization, Availability, Approval, Interpretation, HostCall, NativeExecution, TransactionCommit, ChildExecution, Cancellation, ResourceLimit }
impl DiagnosticPhase {
    /// The phase as a reader would name it, matching the serialised form.
    pub fn label(&self) -> &'static str;
}
pub struct EffectSet(pub BTreeSet<CapabilityRequirement>);
impl EffectSet {
    pub fn from_requirement(requirement: CapabilityRequirement) -> Self;
    pub fn grants(&self, requested: &Self) -> bool;
    pub fn is_pure(&self) -> bool;
    pub fn pure() -> Self;
    pub fn union(&self, other: &Self) -> Self;
}
/// Syntax-neutral module under construction.
pub struct Elaborated { … }
impl Elaborated {
    /// Record a locally certified function.
    pub fn add_function(&mut self, certified: FunctionCertified);
    /// Record an already-lowered dependency that this module may call.
    pub fn add_linked_function(&mut self, function: Function);
    /// Start an elaborated module with no functions.
    pub fn new(name: impl Into<String>, entry: impl Into<String>) -> Self;
    /// Freeze declarations, exports, and content identity.
    pub fn seal(self) -> ModuleSealed;
}
pub enum FileOperation { Read, Write }
/// A normalized pattern relative to an immutable resource root.
pub struct FileSelector { … }
impl FileSelector {
    /// Returns true only when containment is proven by the restricted selector algebra.
    pub fn contains_selector(&self, requested: &Self) -> bool;
    pub fn intersection(&self, other: &Self) -> Result<Self, SelectorError>;
    pub fn matches(&self, relative_path: &str) -> bool;
    pub fn parse(input: &str) -> Result<Self, SelectorError>;
}
/// A deliberately small expression language for argument-dependent file capabilities.
pub struct FileSelectorTemplate { … }
impl FileSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<FileSelector, SelectorError>;
}
pub enum FileSelectorTemplatePart { Literal, Argument }
pub struct Function { … }
/// A function whose local structural, type, stack, and dependency checks passed.
pub struct FunctionCertified { … }
impl FunctionCertified {
    /// Local verification of one function against the functions it may call.
    pub fn certify(function: Function, vocabulary: &Vocabulary, module_functions: &BTreeMap<String, Function>) -> Result<Self, Vec<VmDiagnostic>>;
    /// Verifier facts for this function, never a module certificate.
    pub fn facts(&self) -> &VerifiedFunction;
    /// The certified function IR.
    pub fn function(&self) -> &Function;
}
pub enum GrantScope { Once, Task, Session, Project, Global }
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
pub enum Instruction { Constant, MakeList, MakeMap, MakeRecord, MakeVariant, VariantGet, RecordGet, RecordSet, Dup, Drop, Swap, LocalGet, LocalSet, CaptureGet, MakeClosure, Call, CallClosure, CapabilityRequest, OutputOpen, UiEffect, Yield, DeferFiber, NextFiber, JoinFiber, CancelFiber, DeferCpu, PollCpuFiber, JoinCpuFiber, CancelCpuFiber, PropagateResult, Jump, Branch, Return, Trap }
impl Instruction {
    pub fn is_terminator(&self) -> bool;
}
pub struct LocatedInstruction { … }
impl LocatedInstruction {
    pub fn generated(instruction: Instruction, word: impl Into<String>) -> Self;
}
/// A lexically active structured loop.
pub struct LoopBinding { … }
/// Argument-dependent MCP authority.
pub struct McpSelectorTemplate { … }
impl McpSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<(String, String), SelectorError>;
}
pub struct Module { … }
impl Module {
    pub fn single(function: Function) -> Self;
}
/// A closed declaration graph with frozen exports.
pub struct ModuleSealed { … }
impl ModuleSealed {
    /// The sealed but unverified module IR.
    pub fn module(&self) -> &Module;
    /// Independent composition and security verification.
    pub fn verify(self, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>>;
}
/// The only compiler phase permitted to reach execution.
pub struct ModuleVerified { … }
impl ModuleVerified {
    /// The independently verified module retained for interpretation.
    pub fn as_verified(&self) -> &VerifiedModule;
    /// Unwrap the serializable verified module used by checkpoints.
    pub fn into_verified(self) -> VerifiedModule;
}
/// Argument-dependent network authority.
pub struct NetworkSelectorTemplate { … }
impl NetworkSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<(String, u16), SelectorError>;
}
/// A frontend syntax tree that has not yet entered semantic construction.
pub struct Parsed<Ast> { … }
impl Parsed {
    /// Borrow the frontend syntax tree.
    pub fn ast(&self) -> &Ast;
    /// Wrap a frontend AST as the parse-complete phase.
    pub fn from_frontend(source_id: impl Into<String>, ast: Ast) -> Self;
    /// Consume the wrapper and return the frontend syntax tree.
    pub fn into_ast(self) -> Ast;
    /// Source identity retained from the parse boundary.
    pub fn source_id(&self) -> &str;
}
/// Argument-dependent process authority.
pub struct ProcessSelectorTemplate { … }
impl ProcessSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<String, SelectorError>;
}
/// Language in which a stored program's canonical source is written.
pub enum ProgramLanguage { Forth, Lisp }
impl ProgramLanguage {
    pub fn as_str(self) -> &'static str;
    /// Compact wire-format inference used only when the submission envelope omits `language`; the resolved value is recorded before execution.
    pub fn infer_source(source: &str) -> Self;
    /// Resolve the compact provider wire form before parsing.
    pub fn infer_wire_source(source: &str) -> Result<Self>;
}
/// Argument-dependent proposal authority.
pub struct ProgramSelectorTemplate { … }
impl ProgramSelectorTemplate {
    pub fn instantiate(&self, arguments: &[super::types::TypedValue]) -> Result<String, SelectorError>;
}
pub enum ResourceRoot { Workspace, Project, TaskOutput, HostMachine, Named }
pub enum ResourceSelector { None, File, FileTemplate, NetworkTemplate, Network, Automation, Agent, Process, ProcessTemplate, Program, ProgramTemplate, Mcp, McpTemplate, Memory, Schedule }
pub enum SelectorError { Empty, AbsolutePath, ParentTraversal, UnknownRoot, InvalidRecursiveWildcard, DifferentRoots, IndeterminateIntersection, InvalidTemplateArgument, TemplateArgumentOutOfBounds, WildcardInRuntimePath, InvalidSeparator, InvalidNetworkTemplateArgument, NetworkTemplateArgumentOutOfBounds, InvalidProcessTemplateArgument, ProcessTemplateArgumentOutOfBounds, InvalidProgramTemplateArgument, ProgramTemplateArgumentOutOfBounds, InvalidMcpTemplateArgument, McpTemplateArgumentOutOfBounds }
/// A lexical binding introduced during semantic construction.
pub enum SemanticBinding { Local, Capture }
impl SemanticBinding {
    /// Type of the bound value.
    pub fn ty(&self) -> &Type;
}
/// Syntax-neutral IR constructor used by every frontend.
pub struct SemanticBuilder { … }
impl SemanticBuilder {
    /// Allocate a local slot of `ty`.
    pub fn allocate_local(&mut self, ty: Type) -> u32;
    /// Append an instruction unless the current block already terminated.
    pub fn emit(&mut self, instruction: Instruction, origin: SourceOrigin);
    /// Finish a function with the live output row.
    pub fn finish(self, output: Vec<Type>) -> Function;
    /// Finish with an explicit closed stack signature, as Co-Forth definitions do.
    pub fn finish_closed(self, input: Vec<Type>, output: Vec<Type>, documentation: Option<String>) -> Function;
    /// Jump from the current alternative to `branch.merge_block`.
    pub fn jump_to_merge(&mut self, branch: &BoolBranch, origin: SourceOrigin);
    /// Merge an incoming yield contract into this callable.
    pub fn merge_suspension(&mut self, incoming: Option<&SuspensionSignature>, origin: &SourceOrigin) -> Result<(), Vec<VmDiagnostic>>;
    /// Start a function body with `input` already on the stack.
    pub fn new(name: impl Into<String>, input: Vec<Type>) -> Self;
    /// Allocate a fresh basic block.
    pub fn new_block(&mut self) -> BlockId;
    /// Resolve a name through nested scopes, innermost first.
    pub fn resolve(&self, name: &str) -> Option<SemanticBinding>;
    /// Consume a live `bool` and emit a then/else/merge branch skeleton.
    pub fn start_bool_branch(&mut self, origin: SourceOrigin) -> Result<BoolBranch, Vec<VmDiagnostic>>;
    /// Continue lowering in `block` with `stack` as the live row.
    pub fn switch_to(&mut self, block: BlockId, stack: Vec<Type>);
    /// Visible bindings, innermost first then sorted by name within a scope.
    pub fn visible_bindings(&self) -> Vec<(String, SemanticBinding)>;
}
pub enum Severity { Note, Warning, Error }
pub enum SourceLanguage { Forth, Lisp, FinchIr, Native, Provider }
pub struct SourceOrigin { … }
impl SourceOrigin {
    pub fn generated(word: impl Into<String>) -> Self;
}
pub struct SourceSpan { … }
impl SourceSpan {
    pub fn bytes(source_id: impl Into<String>, start_byte: usize, end_byte: usize) -> Self;
}
/// A typed stack row.
pub struct StackRow { … }
impl StackRow {
    pub fn closed(values: Vec<Type>) -> Self;
    pub fn polymorphic(tail: impl Into<String>, values: Vec<Type>) -> Self;
}
/// Complete contract for a callable word or function.
pub struct StackSignature { … }
impl StackSignature {
    pub fn pure(input: StackRow, output: StackRow) -> Self;
}
/// Typed contract for a callable that may cooperatively suspend.
pub struct SuspensionSignature { … }
impl SuspensionSignature {
    pub fn one_way(yield_type: Type) -> Self;
}
/// Portable typed value used at VM, task, suspension, and wire boundaries.
pub enum TaskKind { Agent, CpuFiber }
/// A language-level type shared by Co-Forth and Finch Lisp.
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
/// Portable typed value used at VM, task, suspension, and wire boundaries.
pub enum TypedValue { Unit, Bool, Int, UInt, Float, Char, Symbol, String, Bytes, Json, Path, List, Map, Option, Result, Record, Variant, Closure, Task, Fiber, Stream, Resource, Dynamic }
impl TypedValue {
    pub fn value_type(&self) -> Type;
}
/// A portable mutation requested for a host-owned output surface.
pub enum UiOperation { Create, Append, Replace, Status, Progress, Complete, Fail }
pub struct VerifiedFunction { … }
/// A verified module is immutable execution data.
pub struct VerifiedModule { … }
pub struct Verifier<'a> { … }
impl Verifier {
    /// Local structural certification of one function.
    pub fn certify_function(&self, function: &Function, module_functions: &BTreeMap<String, Function>) -> Result<VerifiedFunction, Vec<VmDiagnostic>>;
    pub fn new(vocabulary: &'a Vocabulary) -> Self;
    pub fn verify(&self, module: Module) -> Result<VerifiedModule, Vec<VmDiagnostic>>;
}
pub struct VmDiagnostic { … }
impl VmDiagnostic {
    pub fn error(code: impl Into<String>, phase: DiagnosticPhase, message: impl Into<String>, primary: Option<SourceOrigin>) -> Self;
    /// A report that names the offending source, not just the failure.
    pub fn render(&self, source: Option<&str>) -> String;
    pub fn type_mismatch(expected: Type, found: Type, primary: Option<SourceOrigin>) -> Self;
}
pub type Vocabulary = BTreeMap<String, StackSignature>;
```

## Functions

```rust
pub fn agent_task_result_type() -> Type { … }
pub fn agent_task_snapshot_type() -> Type { … }
/// Canonical signatures for the first verified core.
pub fn agent_task_spec_type() -> Type { … }
/// Apply a word signature to a concrete virtual stack.
pub fn apply_signature_types(signature: &StackSignature, stack: &mut Vec<Type>, origin: &SourceOrigin) -> Result<(), VmDiagnostic> { … }
pub fn capability_grant_entry_type() -> Type { … }
/// Seal a complete function map and independently verify it.
pub fn certify_module(name: impl Into<String>, entry: impl Into<String>, functions: BTreeMap<String, Function>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Canonical signatures for verifier-facing consumers.
pub fn core_vocabulary() -> Vocabulary { … }
/// Return provider-neutral documentation for a registered core word.
pub fn core_word_documentation(name: &str) -> CoreWordDocumentation { … }
/// Return the immutable production registry.
pub fn core_word_registry() -> &'static BTreeMap<String, CoreWordSpec> { … }
/// Return the complete contract for one core word.
pub fn core_word_spec(name: &str) -> Option<CoreWordSpec> { … }
/// Instantiate a polymorphic signature against the current concrete suffix without mutating the frontend's virtual stack.
pub fn instantiate_signature_types(signature: &StackSignature, stack: &[Type], origin: &SourceOrigin) -> Result<StackSignature, VmDiagnostic> { … }
/// Names close enough to `target` to be what the author meant, nearest first.
pub fn nearest_names<'a>(target: &str, candidates: impl Iterator<Item = &'a str>) -> Vec<String> { … }
/// Parse the compact type spelling shared by CoLisp annotations and Co-Forth stack signatures.
pub fn parse_type_name(name: &str) -> Result<Type, Vec<VmDiagnostic>> { … }
pub fn tree_entry_type() -> Type { … }
pub fn tree_listing_type() -> Type { … }
```

## Constants

```rust
/// Version of the frontend-facing semantic-construction protocol.
pub const SEMANTIC_CONSTRUCTION_VERSION: u32 = 1;
/// Version of the typed VM contract and serialized IR family.
pub const VM_TYPE_SYSTEM_VERSION: u32 = 5;
```
