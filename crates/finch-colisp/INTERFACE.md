# finch-colisp — public interface

Generated from [`crates/finch-colisp/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-colisp/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Surface-syntax facts consumed by the CoLisp tokenizer and the wire GBNF.
pub struct LispLexicon { … }
/// A reader value paired with the exact byte range that produced it.
pub struct SpannedVal { … }
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
```

## Functions

```rust
/// Parse and compile Finch Lisp directly into the common typed stack IR.
pub fn compile_lisp(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
pub fn compile_lisp_with_functions(source_id: &str, source: &str, initial_stack: Vec<Type>, vocabulary: &Vocabulary, linked_functions: &BTreeMap<String, Function>) -> Result<ModuleVerified, Vec<VmDiagnostic>> { … }
/// True when `ch` ends a CoLisp atom outside a `<...>` type argument list.
pub fn lisp_atom_delimiter(ch: char, angle_depth: usize) -> bool { … }
/// Production CoLisp lexical facts.
pub fn lisp_lexicon() -> LispLexicon { … }
/// Parse a full math expression from `src` into a Lisp Val tree.
pub fn parse_math(src: &str) -> Result<Val> { … }
/// Parse all top-level expressions from `src`.
pub fn parse_str(src: &str) -> Result<Vec<Val>> { … }
/// Parse all top-level expressions while retaining their source structure.
pub fn parse_str_spanned(src: &str) -> Result<Vec<SpannedVal>> { … }
```
