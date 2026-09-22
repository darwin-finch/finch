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

**Surface tiers (issue #963 audit).** The `pub use` list in [`src/lib.rs`](src/lib.rs) is the
cross-crate contract; everything below it is tiered so implementation detail cannot leak back in:

- **Crate-internal (`pub(crate)`):** `VmStep`, `VmTrampoline` and all of
  `{new, start, start_function, resume, run}`, and `TypedRuntime::execute_with_declaration`.
  The trampoline is the handler-free execution core that `TypedRuntime` drives; external callers
  execute through `TypedRuntime::{execute, execute_with_handler}`. Widening any of these is a
  capsule change, not cleanup.
- **Test-only (`#[cfg(test)]`):** `TypedRuntime::grant` (unit tests seed grants directly).
- **Contract (stays `pub`):** `PendingHostCall`. The audit's type-name reference pass saw no
  external callers, but its values are carried by the pub, wire-serialized
  `TypedSuspension.pending_host_call` field and read by `finch-runtime`'s approval-prompt flow
  (`crates/finch-runtime/src/lib.rs` `approval_prompts`); narrowing it would break host
  authorization, so it remains contract surface.

Deleted as unreferenced by the same audit: the `SourceSpan::bytes` constructor in
`finch-vm-core/src/diagnostic.rs`.

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
