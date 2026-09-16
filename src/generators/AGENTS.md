# generators capsule: Finch adapters over the generation contract

Supplements the root [`AGENTS.md`](../../CLAUDE.md), which still applies in full.

**Owns** `src/generators/`: Finch-facing adapters (`ClaudeGenerator`,
`DaemonLocalGenerator`, `QwenGenerator`, `ProfiledGenerator`) and the
compatibility `Generator` trait used by the REPL and scheduler. The
generation contract (`GenerationBackend`, `GenerationEvent`, `ToolCall`,
`ToolResult`, readiness, identity, ports) lives in
[`crates/finch-generation`](../../crates/finch-generation/AGENTS.md).
DESIGN.md lists this tree on the models row; models owns the Finch adapters.
Provider transports live in `finch-providers`.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its
signature. Child modules are private, so the `pub use` list in
`src/generators/mod.rs` is the whole public surface. Callers outside this
directory use `crate::generators::Item`; they must not name `claude`,
`daemon_local`, or `qwen`.

**Dependencies:** `finch-generation` (contract), `finch-providers` /
`claude` (`ContentBlock`, `Message`), `tools` (`ToolDefinition` wire type
only), `local`, `models` (tokenizer/parser for Qwen), `client` (daemon-local),
and `context` (instruction files for Claude). Implementations may not reach
into `cli` or own a `ToolExecutor`. Callers inject everything a generator
needs.

**Qwen does not execute tools.** It parses local markup and returns
`tool_uses` / `ToolCall` events. The event loop owns execution via
[`crate::tools::ToolLoop`] (REPL and scheduler).

This facade re-exports `translate_provider_chunk` so
`ContentBlockComplete(ToolUse)` becomes `ToolCallComplete` at the generation
layer. Native OpenAI/Claude deltas pass through the same ToolLoop.

Add public surface by re-exporting it from `mod.rs`, then regenerate
`INTERFACE.md` with `python3 scripts/generate_interfaces.py --write`.
