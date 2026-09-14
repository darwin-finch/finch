# colisp capsule: CoLisp source frontend

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-colisp/src/`: the CoLisp reader, source values, and compiler from
span-preserving CoLisp syntax to Finch's shared typed stack IR. It owns no interpreter, runtime,
fiber scheduler, capability broker, or checkpoint codec.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`. Applications continue
to use the `finch-vm` compatibility facade; this unpublished crate is the downward frontend seam.

**Dependencies:** `finch-colisp` depends only on `finch-vm-core` plus reader serialization and
error-support crates. It never depends on `finch-vm` or the root `finch` crate. Shared surface-type
grammar comes from the two compiler-support exports documented by `finch-vm-core`.

**Invariants:** CoLisp lowers directly to the same typed IR and verifier contract as Co-Forth.
Reader spans and diagnostic source origins must survive lowering without generated source text.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-colisp --lib`. Also run the
`finch-vm` frontend-equivalence integration tests when changing lowering behavior or public exports.
