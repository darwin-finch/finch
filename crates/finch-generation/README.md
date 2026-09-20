# finch-generation: the generation contract

This crate is the seam between Finch and whatever actually produces a model turn — provider,
local engine, or test double. It defines the provider-neutral request/event contract
(`GenerationBackend`, `GenerationRequest`, `GenerationEvent`, `ToolCall`, `ToolResult`), readiness
and load phases, identity (requested vs. resolved vs. actual model), and the injected ports a
backend needs, so Finch can swap backends without the application layer knowing which one is live.
It exists as its own crate so a worker can implement or test a new backend against this contract
without opening Finch application code.

Current ownership and test instructions are in [`AGENTS.md`](AGENTS.md); the exact Rust surface is
[`src/lib.rs`](src/lib.rs).

## Scope

Provider-specific parsers stay in `finch-providers`; this crate only translates their
`StreamChunk`s into `GenerationEvent`s. Tool execution is owned by Finch's `ToolLoop`
(`src/tools/tool_loop.rs`), not this crate — generators return `ToolCall`s, they never execute
them. Application configuration, Brain, TUI, daemon, and CLI orchestration stay in Finch.
