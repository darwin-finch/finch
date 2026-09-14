# finch-coforth — public interface

Generated from [`crates/finch-coforth/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-coforth/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Functions

```rust
/// Compile user/model-entered Co-Forth source text directly into Finch typed stack IR and run the common verifier.
pub fn compile_forth(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile Co-Forth source with additional already-lowered functions available for definition calls, then verify the complete typed module.
pub fn compile_forth_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
```
