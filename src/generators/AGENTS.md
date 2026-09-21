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

**Boundary:** the [README](README.md) traces REPL assembly and query streaming.
[`mod.rs`](mod.rs) is the facade; rustdoc renders methods on exported types. Child modules
are private, so callers outside this directory use `crate::generators::Item`, not `claude`,
`daemon_local`, or `qwen`. Do not recreate a signature catalog.

**Dependencies:** `finch-generation` (contract), `finch-providers` /
`claude` (`ContentBlock`, `Message`), `tools` (`ToolDefinition` wire type
only), `local`, `models` (tokenizer/parser for Qwen), `client` (daemon-local),
and `context` (instruction files for Claude). Implementations may not reach
into `cli` or own a `ToolExecutor`. Callers inject everything a generator
needs.

**Qwen does not execute tools.** It parses local markup and returns
`tool_uses` / `ToolCall` events. The event loop owns execution via
[`crate::tools::ToolLoop`] (REPL and scheduler).

**Generator names are family-truthful.** `QwenGenerator::name()` reports the
served family path (`QWEN_LOCAL_GENERATOR_NAME`, "qwen2.5-onnx"), never a bare
"Local"; capability claims derive from the models family catalog
(`ModelFamily::local_engine_capabilities`) instead of hardcoded prose. The
adapter buffers complete turns — engine streaming is served through the daemon
SSE path and recorded in the catalog, not claimed here. `DaemonLocalGenerator`
keeps the local profile's name (family-derived for local entries); the exact
model arrives per turn in `ResponseMetadata.model`.

This facade re-exports `translate_provider_chunk` for the development generation contract.
The current REPL handles provider `StreamChunk` directly; do not claim it calls the translator.
Native OpenAI/Claude deltas pass through the application ToolLoop.

Keep new child-module surface behind deliberate `mod.rs` re-exports; root-defined contracts
also belong in that facade. Run focused tests with
`./scripts/test_brains.sh cargo test --lib -- generators::` and the REPL query tests with
`./scripts/test_brains.sh cargo test --lib -- cli::repl_event::query_processor::`.
