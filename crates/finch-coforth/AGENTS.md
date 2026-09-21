# Co-Forth capsule: typed stack-language frontend

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-coforth/src/`: the Co-Forth source parser, syntax tree, the
reader lexicon used by the published compact-wire grammar, syntactic completeness
(`read_forth_source`), and translation into the shared semantic-construction protocol.
It owns no verifier, interpreter, runtime, fiber scheduler, checkpoint codec, or
CoLisp behavior. It does not mint `ModuleVerified` certificates except by calling the
shared certify pipeline.

**Boundary:** the [README](README.md) traces compilation and compact-wire recognition;
[`src/lib.rs`](src/lib.rs) is the flat facade. `cargo doc -p finch-coforth --no-deps --open` shows
public methods without a generated catalog. Applications use the compatible `finch-vm` facade
rather than depending on this unpublished compiler crate directly.

**Dependencies:** `finch-coforth` depends only on [`finch-vm-core`](../finch-vm-core/AGENTS.md)
inside the workspace. Shared type-spelling grammar comes from core's restricted compiler-support
seam; Co-Forth never depends on CoLisp or `finch-vm`.

**Invariants:** compilation preserves source spans and lowers to the same typed IR accepted by the
shared verifier. This crate introduces no execution, host-effect, authority, or persistence path.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-coforth --lib`. Compile-to-execute
equivalence belongs to `finch-vm`, where an interpreter is available without creating a dependency
cycle.
