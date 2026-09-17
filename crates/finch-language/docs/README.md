# `finch-language` documentation

This directory owns implemented reference material for the compilation facade:
source-language selection, the shared semantic-construction pipeline, the
`ModuleVerified` certificate submitted to execution, and the compact-wire GBNF
generated from frontend reader lexicons (`vocabulary/language/wire.gbnf`).

Current concise ownership and test instructions are in [`../AGENTS.md`](../AGENTS.md),
and the exact Rust surface is generated in [`../INTERFACE.md`](../INTERFACE.md).
Planned language semantics remain in the shared
[Finch language design](../../../docs/language/README.md) until implemented and
stable enough to extract into a compiler reference here.
