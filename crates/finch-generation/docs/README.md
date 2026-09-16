# `finch-generation` documentation

This directory owns implemented reference material for the extracted generation
crate: the request/event contract, lifecycle, identity, injected ports, and
the translation from `finch-providers` stream chunks.

Current ownership and test instructions are in [`../AGENTS.md`](../AGENTS.md),
and the exact Rust surface is generated in [`../INTERFACE.md`](../INTERFACE.md).

The public contribution surface is the generation contract (`GenerationBackend`,
`GenerationRequest`, `GenerationEvent`, `ToolCall`, `ToolResult`), readiness,
identity (requested vs resolved vs actual), and injected environmental ports.
Provider-specific parsers stay in `finch-providers`. Tool execution, Brain,
TUI, daemon, CLI, and application `Config` stay in Finch.

Tool execution is owned by Finch `ToolLoop` (`src/tools/tool_loop.rs`), not this crate.
Provider tool-binding tables live in `finch-providers` (issue #241). This crate
keeps semantic Finch tool identities on `GenerationRequest` and `ToolCall`.
