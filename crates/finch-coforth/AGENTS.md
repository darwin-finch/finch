# Co-Forth capsule: typed stack-language frontend

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-coforth/src/`: the Co-Forth source parser, syntax tree, the
reader lexicon used by the published compact-wire grammar, syntactic completeness
(`read_forth_source`), and translation into the shared semantic-construction protocol.
It owns no verifier, interpreter, runtime, fiber scheduler, checkpoint codec, or
CoLisp behavior. It does not mint `ModuleVerified` certificates except by calling the
shared certify pipeline.

**Interface:** `src/lib.rs` is the facade; read it directly for exact signatures. It exports
the two source-compilation entry points, `read_forth_source` (syntactic completeness), and the
reader lexicon used by the published compact-wire grammar. Application callers continue to use the
compatible `finch-vm` facade rather than depending on this unpublished crate directly.

**Documentation:** [`README.md`](README.md) owns implemented Co-Forth syntax and lowering
reference material. Cross-frontend planned semantics remain in the shared
[language design](../../docs/language/README.md); boundary changes follow the shared
[implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md).

**Dependencies:** `finch-coforth` depends only on [`finch-vm-core`](../finch-vm-core/AGENTS.md)
inside the workspace. Shared type-spelling grammar comes from core's restricted compiler-support
seam; Co-Forth never depends on CoLisp or `finch-vm`.

**Invariants:** compilation preserves source spans and lowers to the same typed IR accepted by the
shared verifier. This crate introduces no execution, host-effect, authority, or persistence path.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-coforth --lib`. Compile-to-execute
equivalence belongs to `finch-vm`, where an interpreter is available without creating a dependency
cycle.
