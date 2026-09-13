# vm capsule: typed VM, CoForth, CoLisp

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `crates/finch-vm/src/` (the interpreter, typed runtime, fibers, Co-Forth and Co-Lisp
frontends, plus the Lisp reader), and the repository-root `vocabulary/` and `examples/finch/`.
Shared typed IR, verification, capability/effect descriptions, and vocabulary contracts live in
[`finch-vm-core`](../finch-vm-core/AGENTS.md). The program runtime service is `src/runtime/`; the
program catalog is `src/programs/`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature;
it is generated from the `pub use` list in `src/lib.rs`, which is the whole public surface. Child
modules are private, so reaching past it is a compile error. To expose something new, re-export it
deliberately. The root crate provides compatibility namespaces for the former `finch::lisp`
reader and types paths.

**Dependencies:** `finch-vm` depends downward on the unpublished `finch-vm-core` contract crate and
never on the root `finch` crate. Co-Lisp and Co-Forth reach only the core crate's documented,
restricted compiler-support seam for shared verifier helpers. The pure CPU fiber scheduler remains
here; `programs` re-exports the core-owned `ProgramLanguage` through this compatibility facade.

**Effects** go through the capability broker, never around it
([capability boundaries](../../CLAUDE.md#key-principles)).

**Focused tests:** `./scripts/test_brains.sh cargo test -p finch-vm --lib` and
`./scripts/test_brains.sh cargo test -p finch-vm-core --lib`. Run the full suite when changing a
re-exported `pub` item, the IR, the verifier, or `vocabulary/`.

**Reference, not required:** the language contracts in `vocabulary/language/` are compiled into
the binary and shown to the model, so editing them changes model-facing behavior.
