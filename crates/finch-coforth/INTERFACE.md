# finch-coforth — public interface

Generated from [`crates/finch-coforth/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-coforth/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Surface-syntax facts consumed by the Co-Forth tokenizer and the wire GBNF.
pub struct ForthLexicon { … }
/// One Co-Forth string/raw-string opener as recognized by the tokenizer.
pub struct ForthStringOpener { … }
```

## Functions

```rust
/// Compile user/model-entered Co-Forth source text directly into Finch typed stack IR and run the common verifier.
pub fn compile_forth(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Compile Co-Forth source with additional already-lowered functions available for definition calls, then verify the complete typed module.
pub fn compile_forth_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// Production Co-Forth lexical facts.
pub fn forth_lexicon() -> ForthLexicon { … }
/// Bytes that end a Co-Forth word in the generic (non-type) token loop.
pub fn forth_word_terminator(byte: u8) -> bool { … }
/// Syntactic completeness of Co-Forth source: tokenization, typed-definition structure, and collection/quotation delimiter balance.
pub fn read_forth_source(source_id: &str, source: &str) -> Result<(), Vec<VmDiagnostic>> { … }
```
