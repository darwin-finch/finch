# vm capsule: typed VM execution and compatibility facade

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm/src/` (the interpreter, typed runtime, fibers, and compatibility
facade), and the shared repository-root `vocabulary/` and `examples/finch/` integration corpus.
The CoLisp and CoForth source frontends live in sibling crates.
Shared typed IR, verification, capability/effect descriptions, and vocabulary contracts live in
[`finch-vm-core`](../finch-vm-core/AGENTS.md). The program runtime service is `src/runtime/`; the
program catalog is `src/programs/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature;
it is generated from the `pub use` list in `src/lib.rs`, which is the whole public surface. Child
modules are private, so reaching past it is a compile error. To expose something new, re-export it
deliberately. The root crate provides compatibility namespaces for the former `finch::lisp`
reader and types paths.

**Documentation:** [`docs/README.md`](docs/README.md) owns implemented interpreter, fiber,
checkpoint, and execution reference material. Cross-frontend planned semantics remain in the shared
[language design](../../docs/language/README.md); source-compilation separation follows the shared
[implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md).

**Dependencies:** `finch-vm` depends downward on the unpublished `finch-vm-core`, `finch-colisp`,
and `finch-coforth` crates and never on the root `finch` crate. Each frontend reaches only the core
crate's documented, restricted compiler-support seam. Frontends never depend on each other or back
on this execution crate. The pure CPU fiber scheduler remains here; `programs` re-exports the
core-owned `ProgramLanguage` through this compatibility facade.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm --lib` and
`./scripts/test_brains.sh cargo test -p finch-colisp --lib`,
`./scripts/test_brains.sh cargo test -p finch-coforth --lib`, and
`./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Run the full suite when changing a
re-exported `pub` item, frontend boundary, IR, verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
