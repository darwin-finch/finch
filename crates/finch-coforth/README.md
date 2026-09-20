# Co-Forth: typed stack-language frontend

Co-Forth is one of Finch's two source-language frontends — a stack-oriented syntax that reads
source and translates it into the same shared semantic-construction protocol CoLisp uses, so both
syntaxes lower to identical typed IR. This crate owns only the reader, the syntax tree, and that
translation step; it has no type inference, verifier, interpreter, or runtime of its own (those
live in `finch-vm-core` and `finch-vm`).

Current concise ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust
surface is [`src/lib.rs`](src/lib.rs).

## Further documentation

Planned semantics shared with CoLisp remain in the
[Finch language design](../../docs/language/README.md) and the
[implementation roadmap](../../docs/language/IMPLEMENTATION_ROADMAP.md); they are intended
direction, not implemented behavior, and should not be copied here. This directory is where an
implemented Co-Forth syntax reference gets extracted from that design as forms become concrete
contracts.
