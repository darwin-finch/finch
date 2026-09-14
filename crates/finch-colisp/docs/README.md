# `finch-colisp` documentation

This directory owns implemented reference material for the CoLisp reader, syntax, semantic nodes,
and lowering into Finch typed stack IR.

Current concise ownership and test instructions are in [`../AGENTS.md`](../AGENTS.md), and the exact
Rust surface is generated in [`../INTERFACE.md`](../INTERFACE.md). Planned semantics shared with
Co-Forth remain in the [Finch language design](../../../docs/language/README.md); they should not be
copied here. Extract a CoLisp syntax reference here as those forms become implemented contracts.
