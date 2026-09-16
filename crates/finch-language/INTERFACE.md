# finch-language — public interface

Generated from [`crates/finch-language/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-language/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Syntax-neutral module under construction. Re-exported from `finch-vm-core`.
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
/// A function whose local structural, type, stack, and dependency checks passed. Re-exported from `finch-vm-core`.
pub struct FunctionCertified { … }
impl FunctionCertified {
    /// Local verification of one function against the functions it may call.
    pub fn certify(function: Function, vocabulary: &Vocabulary, module_functions: &BTreeMap<String, Function>) -> Result<Self, Vec<VmDiagnostic>>;
    /// Verifier facts for this function, never a module certificate.
    pub fn facts(&self) -> &VerifiedFunction;
    /// The certified function IR.
    pub fn function(&self) -> &Function;
}
/// A closed declaration graph with frozen exports. Re-exported from `finch-vm-core`.
pub struct ModuleSealed { … }
impl ModuleSealed {
    /// The sealed but unverified module IR.
    pub fn module(&self) -> &Module;
    /// Independent composition and security verification.
    pub fn verify(self, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>>;
}
/// The only compiler phase permitted to reach execution. Re-exported from `finch-vm-core`.
pub struct ModuleVerified { … }
impl ModuleVerified {
    /// The independently verified module retained for interpretation.
    pub fn as_verified(&self) -> &VerifiedModule;
    /// Unwrap the serializable verified module used by checkpoints.
    pub fn into_verified(self) -> VerifiedModule;
}
/// A frontend syntax tree that has not yet entered semantic construction. Re-exported from `finch-vm-core`.
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
/// Syntax-neutral IR constructor used by every frontend. Re-exported from `finch-vm-core`.
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
/// A reader value paired with the exact byte range that produced it. Re-exported from `finch-colisp`.
pub struct SpannedVal { … }
/// Re-exported from `finch-colisp`.
pub enum Val { Nil, Bool, Int, Float, Str, Symbol, Bytes, List }
impl Val {
    pub fn as_bytes(&self) -> anyhow::Result<&[u8]>;
    pub fn as_float(&self) -> anyhow::Result<f64>;
    pub fn as_int(&self) -> anyhow::Result<i64>;
    pub fn as_list(&self) -> anyhow::Result<&[Val]>;
    pub fn as_str(&self) -> anyhow::Result<&str>;
    pub fn is_truthy(&self) -> bool;
    /// Like Display but wraps strings in quotes (for printing inside lists).
    pub fn repr(&self) -> String;
    pub fn type_name(&self) -> &'static str;
}
/// Why a compact-wire submission was rejected before compilation.
pub struct WireReject { … }
```

## Functions

```rust
/// True when the published GBNF accepts `source` as a complete submission.
pub fn accepts_published_grammar(source: &str) -> bool { … }
/// Production-reader oracle for a complete compact-wire `ProgramSubmission`.
pub fn accepts_wire_source(source: &str) -> Result<ProgramLanguage, WireReject> { … }
/// Seal a complete function map and independently verify it. Re-exported from `finch-vm-core`.
pub fn certify_module(name: impl Into<String>, entry: impl Into<String>, functions: BTreeMap<String, Function>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile source in `language` through the shared compiler pipeline.
pub fn compile(language: ProgramLanguage, source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile user/model-entered Co-Forth source text directly into Finch typed stack IR and run the common verifier. Re-exported from `finch-coforth`.
pub fn compile_forth(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile Co-Forth source with additional already-lowered functions available for definition calls, then verify the complete typed module. Re-exported from `finch-coforth`.
pub fn compile_forth_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Parse and compile Finch Lisp directly into the common typed stack IR. Re-exported from `finch-colisp`.
pub fn compile_lisp(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Re-exported from `finch-colisp`.
pub fn compile_lisp_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile source with additional already-lowered functions available for calls.
pub fn compile_with_functions(language: ProgramLanguage, source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Parse a full math expression from `src` into a Lisp Val tree. Re-exported from `finch-colisp`.
pub fn parse_math(src: &str) -> Result<Val> { … }
/// Parse all top-level expressions from `src`. Re-exported from `finch-colisp`.
pub fn parse_str(src: &str) -> Result<Vec<Val>> { … }
/// Parse all top-level expressions while retaining their source structure. Re-exported from `finch-colisp`.
pub fn parse_str_spanned(src: &str) -> Result<Vec<SpannedVal>> { … }
/// Render the canonical compact-wire GBNF from production reader lexicons.
pub fn render_wire_gbnf() -> String { … }
```

## Constants

```rust
/// Version of the frontend-facing semantic-construction protocol. Re-exported from `finch-vm-core`.
pub const SEMANTIC_CONSTRUCTION_VERSION: u32 = 1;
/// Path of the committed grammar artifact, relative to the repository root.
pub const WIRE_GRAMMAR_ARTIFACT: &str = "vocabulary/language/wire.gbnf";
/// Version of the published compact-wire GBNF.
pub const WIRE_GRAMMAR_VERSION: u32 = 1;
```
