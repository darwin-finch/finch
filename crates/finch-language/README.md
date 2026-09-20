# finch-language: compilation facade

This crate is where source compilation happens: it selects a frontend (CoLisp or Co-Forth),
drives the shared semantic-construction pipeline both frontends submit to, and returns a sealed
`ModuleVerified` certificate to `finch-vm` for execution. It exists so the VM never has to know
which syntax produced a program, and so frontend selection lives in exactly one place.

Current concise ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust
surface is [`src/lib.rs`](src/lib.rs).

## Further documentation

Planned language semantics remain in the shared
[Finch language design](../../docs/language/README.md) until implemented and stable enough to
extract into a compiler reference here. The compact-wire GBNF generated from the frontend reader
lexicons (`vocabulary/language/wire.gbnf`) is compiled into the binary and shown to the model —
editing it changes model-facing behavior.
