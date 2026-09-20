# finch-vm: typed VM execution

This crate is where a compiled program actually runs: the reference interpreter, the typed
runtime, fibers, checkpointing, and compiler-boundary wire-failure classification. It consumes a
`ModuleVerified` certificate from `finch-language`; it never selects or invokes a frontend itself,
which is what keeps execution decoupled from source syntax. It also carries the compatibility
facade for the former `finch::lisp` reader and types paths.

Current concise ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust
surface is [`src/lib.rs`](src/lib.rs).

## Further documentation

Planned language/runtime semantics remain in the shared
[Finch language design](../../docs/language/README.md) until implemented and stable enough to
extract into an execution reference here. Source-compilation separation follows the shared
[implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md). The language contracts in
`vocabulary/language/` are compiled into the binary and shown to the model, so editing them changes
model-facing behavior — they are reference, not optional reading.
