# vm capsule: typed VM execution and compatibility facade

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm/src/` (the interpreter, typed runtime, fibers, compiler-boundary wire
failure classification, and compatibility facade), and the shared repository-root `vocabulary/`
and `examples/finch/` integration corpus.
Source compilation lives in [`finch-language`](../finch-language/AGENTS.md); this crate consumes
`ModuleVerified` and does not select or invoke a frontend.
Shared typed IR, verification, capability/effect descriptions, and vocabulary contracts live in
[`finch-vm-core`](../finch-vm-core/AGENTS.md). The program runtime service is
[`finch-runtime`](../finch-runtime/AGENTS.md); the
program-definition and corpus metadata live in `crates/finch-programs/`.

**Boundary:** the [README](README.md) traces runtime execution and wire-failure classification;
[`src/lib.rs`](src/lib.rs) is the flat facade, and `cargo doc -p finch-vm --no-deps --open`
renders public methods. Child modules remain private. To expose something new, re-export it
deliberately; do not generate a signature catalog. The root crate keeps compatibility namespaces
for former `finch::lisp` reader and type paths.

**Dependencies:** `finch-vm` depends downward on the unpublished `finch-vm-core` crate and never
on `finch-colisp`, `finch-coforth`, `finch-language`, or the root `finch` crate. The language
facade is a test-only dependency so execution-equivalence tests can compile fixtures without
making production VM code select a frontend. The pure CPU fiber scheduler remains here;
`programs` re-exports the core-owned `ProgramLanguage` through this compatibility facade.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).
`RUNTIME_APPLICATION_ABI_VERSION` versions the portable effect/resume/delivery
boundary independently of `VM_TYPE_SYSTEM_VERSION` (IR/checkpoints). Version 1
is not a production freeze: change the types when justified, bump the constant,
and fail closed. Do not add a second journal or external schema on these records.

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm --lib` and
`./scripts/test_brains.sh cargo test -p finch-colisp --lib`,
`./scripts/test_brains.sh cargo test -p finch-coforth --lib`, and
`./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Run the full suite when changing a
re-exported `pub` item, frontend boundary, IR, verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
