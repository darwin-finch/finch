# programs — public interface

Generated from [`src/programs/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/programs/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Upper bound on what executing a program may affect.
pub enum ExecutionEffect { Pure, VmRead, VmWrite, WorkspaceRead, ExternalRead, WorkspaceWrite, ExternalWrite, Destructive, Unclassified }
impl ExecutionEffect {
    pub fn as_str(self) -> &'static str;
    pub fn runs_autonomously(self) -> bool;
}
/// A self-executing Finch source file after its shebang has been removed.
pub struct FinchScript { … }
/// Incremental lexical receiver for the compact Co-Forth wire form.
pub struct ForthWireBuffer { … }
impl ForthWireBuffer {
    pub fn finish(&mut self) -> Result<Vec<ForthWireToken>>;
    pub fn push(&mut self, fragment: &str) -> Result<Vec<ForthWireToken>>;
    pub fn source(&self) -> &str;
}
/// One complete Co-Forth lexical token observed while a provider response is still streaming.
pub struct ForthWireToken { … }
/// Identity of one normative artifact handed to a provider.
pub struct LanguagePackageIdentity { … }
/// Canonical definition and review metadata for one program version.
pub struct ProgramDefinition { … }
impl ProgramDefinition {
    /// Create a new session candidate.
    pub fn candidate(name: impl Into<String>, language: ProgramLanguage, source: impl Into<String>) -> Self;
    /// Project a persisted top-level Lisp `define` expression into the registry.
    pub fn from_lisp_define(source: &str, scope_key: Option<String>) -> Option<Self>;
    /// Load one plain-text `.forth` or `.lisp` file as a canonical definition.
    pub fn from_source_file(path: &Path, root: &Path, scope: ProgramScope) -> Result<Self>;
}
/// Language in which a stored program's canonical source is written. Re-exported from `vm`.
pub enum ProgramLanguage { Forth, Lisp }
/// Immutable address of a stored program version.
pub struct ProgramRef { … }
/// Persistence and visibility boundary for a definition.
pub enum ProgramScope { Builtin, Task, Session, Project, Personal, User, Published, Imported }
impl ProgramScope {
    pub fn as_str(self) -> &'static str;
}
/// Compact definition supplied to an LLM during the VM handshake.
pub struct ProgramSummary { … }
/// Portable values accepted by the initial pure cross-language ABI.
pub enum ProgramValue { Nil, Bool, Int, Float, Symbol, String, Bytes, Json, List, Map, Option, Result, Record, Variant, Task, Fiber, Resource }
/// Review state for executable vocabulary.
pub enum TrustState { Candidate, Tested, Approved, Quarantined, Deprecated }
impl TrustState {
    pub fn as_str(self) -> &'static str;
}
/// Compact runtime discovery document refreshed across model/session changes.
pub struct VmManifest { … }
impl VmManifest {
    /// Format a deliberately compact block suitable for prompt injection.
    pub fn prompt_block(&self) -> String;
}
pub enum WireCorpusAttempt { FirstPass, Repair }
pub struct WireCorpusAudit { … }
pub struct WireCorpusCounts { … }
```

## Functions

```rust
/// Compile and verify every retained source without interpreting any module.
pub fn audit(path: &Path) -> Result<WireCorpusAudit> { … }
pub fn capture_from_env(provider: &str, model: &str, surface: &str, attempt: WireCorpusAttempt, source: &str) { … }
pub fn capture_with_runtime_from_env(runtime: &crate::runtime::ProgramRuntime, provider: &str, model: &str, surface: &str, attempt: WireCorpusAttempt, source: &str) { … }
/// Classify a rejected provider submission for aggregate conformance metrics.
pub fn classify_wire_failure(source: &str, diagnostic: &str) -> crate::metrics::WireFailureClass { … }
/// SHA-256 of canonical source or environment material.
pub fn hash_text(text: &str) -> String { … }
/// Whether a rejected wire program is eligible for one source-only repair.
pub fn is_repairable_wire_diagnostic(diagnostic: &str) -> bool { … }
pub fn language_package_identities() -> Vec<LanguagePackageIdentity> { … }
/// Discover canonical plain-text programs below a vocabulary directory.
pub fn load_program_files(root: &Path, scope: ProgramScope) -> Result<Vec<ProgramDefinition>> { … }
/// Parse a Finch executable script header.
pub fn parse_finch_script(path: &Path, contents: &str) -> Result<FinchScript> { … }
/// Locate `<git-root>/vocabulary/programs` for the current project.
pub fn project_program_root(start: &Path) -> Option<PathBuf> { … }
/// Return the stable leading diagnostic code without retaining the diagnostic prose in conformance metrics.
pub fn wire_diagnostic_code(diagnostic: &str) -> Option<String> { … }
/// Construct the provider-neutral correction request for a rejected program.
pub fn wire_repair_request(rejected_source: &str, diagnostic: &str) -> String { … }
```

## Constants

```rust
/// Minimal language/runtime definition supplied to every fresh model context.
pub const BOOT_CAPSULE: &str = include_str!("../../vocabulary/BOOT.md");
pub const FORTH_LANGUAGE_DEFINITION: &str = include_str!("../../vocabulary/language/FINCH_FORTH.md");
pub const LANGUAGE_SCHEMA: &str = include_str!("../../vocabulary/language/schema.json");
pub const LISP_LANGUAGE_DEFINITION: &str = include_str!("../../vocabulary/language/FINCH_LISP.md");
/// Version of the model/runtime vocabulary handshake.
pub const MANIFEST_PROTOCOL_VERSION: u32 = 1;
pub const VM_LANGUAGE_DEFINITION: &str = include_str!("../../vocabulary/language/FINCH_VM.md");
```
