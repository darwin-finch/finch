# Application tool execution

This module connects Finch's shared tool contract to application-owned execution. It owns the
executor, concrete local and GUI tools, task-list and post-edit diagnostic adapters, and the
wiring for external MCP tools. The `Tool` contract, registry, typed calls/results, permission
policy, and tool-round protocol live in `finch-tools-api`; named-Brain background task records
live in `finch-brain`; source identity and structural outlines live in `source_index`. This module
does not own those contracts or durable Brain state.

Two callers show how those pieces meet:

1. The [interactive REPL tool coordinator](../cli/repl_event/tool_execution.rs) receives an
   admitted call from its event-loop `ToolLoop`, asks `ToolExecutor` for the registered tool's
   execution timeout and result, then publishes one result back to that same round. The REPL
   owns query cancellation, approval presentation, and output timing; this module owns local
   tool dispatch and the registered implementations.
2. The [headless `finch agent` loop](../agent/mod.rs) builds a smaller registry of file, shell,
   and web tools with its explicit agent-mode permission rule, then calls `ToolExecutor`
   directly for each provider tool use and returns result blocks to the provider. This legacy
   path does not currently use `ToolLoop`; do not infer the REPL's round-admission guarantees
   for it from the shared executor alone.

The code-retrieval implementations are adapters over `source_index`. `code_outline` supplies one
path and returns its bounded structural envelope. `code_hop` refreshes the injected derived cache,
tries bounded fixed-string routing for paths/identifiers/literals, then walks sibling directory and
symbol menus with deterministic lexical scoring. It returns exact spans for a later `read`, never
source bodies. Ambiguous hops stay ranker-only with a warning today; a future local `compress`
adapter may receive only opaque candidate ids, labels, and the documented bounded `AGENTS.md` lead.
Corpus structure, source generations, parser selection, and cache format do not belong in the
executor.

Use the [agent contract](AGENTS.md) for authority and dependency rules, [`mod.rs`](mod.rs) for
the flat root-package facade, and the [MCP guide](mcp/README.md) for external connections.
Rustdoc gives signatures on re-exported types without a generated API catalog.
