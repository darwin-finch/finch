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

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-colisp --lib`. Also run the
`finch-vm` frontend-equivalence integration tests when changing lowering behavior or public exports.
