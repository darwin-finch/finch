# colisp capsule: CoLisp source frontend

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-colisp/src/`: the CoLisp reader, source values, the reader
lexicon used by the published compact-wire grammar, and translation from
span-preserving CoLisp syntax into Finch's shared semantic-construction protocol. It owns no
interpreter, runtime, fiber scheduler, capability broker, or checkpoint codec. It does not infer
types or effects privately or mint `ModuleVerified` certificates.

**Boundary:** the [README](README.md) traces compilation and compact-wire recognition;
[`src/lib.rs`](src/lib.rs) is the flat facade. `cargo doc -p finch-colisp --no-deps --open` shows
public methods without a generated catalog. Applications continue to use the `finch-vm`
compatibility facade; this unpublished crate is a downward compiler seam.

**Dependencies:** `finch-colisp` depends only on `finch-vm-core` plus reader serialization and
error-support crates. It never depends on `finch-vm` or the root `finch` crate. Shared surface-type
grammar comes from the two compiler-support exports documented by `finch-vm-core`.

**Invariants:** CoLisp lowers directly to the same typed IR and verifier contract as Co-Forth.
Reader spans and diagnostic source origins must survive lowering without generated source text.

**Surface tiers (issue #961 audit).** The `pub use` list in [`src/lib.rs`](src/lib.rs) is the
cross-crate contract; everything below it is tiered so implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** `lisp_atom_delimiter` (tokenizer helper used by the reader)
  and `Val::type_name` (typed-lowering diagnostics). Widening either is a capsule change, not
  cleanup.
- **Module-internal (private):** `Val::repr` (used by `Val`'s own `Display` impl).
- **Test-only (`#[cfg(test)]`):** `Val::{is_truthy, as_int, as_bytes}` — external callers match on
  the enum instead of through accessors.

`lisp_lexicon`, `LispLexicon`, and `Val` itself stay `pub`: `finch-language`'s compact-wire
recognizer calls `lisp_lexicon` and re-exports `Val` toward `finch-programs` and the root crate's
`finch::lisp` compatibility namespace, and the audit resolved its "narrow both or retain both" for
the lexicon pair to "retain both". Deleted as unreferenced by the same audit (no callers anywhere
including tests; no trait or dynamic dispatch through them): `Val::{as_float, as_str, as_list}`.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-colisp --lib`. Also run the
`finch-vm` frontend-equivalence integration tests when changing lowering behavior or public exports.
