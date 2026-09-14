# Co-Forth capsule: typed stack-language frontend

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-coforth/src/`: the Co-Forth source parser, syntax tree, type
elaboration, and lowering into shared typed IR. It owns no verifier, interpreter, runtime,
fiber scheduler, checkpoint codec, or CoLisp behavior.

**Interface:** [`INTERFACE.md`](INTERFACE.md) is generated from `src/lib.rs`. The facade exports
only the two source-compilation entry points. Application callers continue to use the compatible
`finch-vm` facade rather than depending on this unpublished crate directly.

**Documentation:** [`docs/README.md`](docs/README.md) owns implemented Co-Forth syntax and lowering
reference material. Cross-frontend planned semantics remain in the shared
[language design](../../docs/language/README.md).

**Dependencies:** `finch-coforth` depends only on [`finch-vm-core`](../finch-vm-core/AGENTS.md)
inside the workspace. Shared type-spelling grammar comes from core's restricted compiler-support
seam; Co-Forth never depends on CoLisp or `finch-vm`.

**Invariants:** compilation preserves source spans and lowers to the same typed IR accepted by the
shared verifier. This crate introduces no execution, host-effect, authority, or persistence path.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-coforth --lib`. Compile-to-execute
equivalence belongs to `finch-vm`, where an interpreter is available without creating a dependency
cycle.
