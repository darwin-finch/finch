# Interactive message lifecycle

`finch-messages` gives one interactive Finch session typed, mutable conversation entries. A
`WorkUnit` groups a generation turn with its program, tool, output, and child-agent rows; the
`Message` trait lets the output buffer and renderer treat that unit alongside user, progress,
and operation messages without downcasting. This is client-local conversation state, not the
durable named-Brain journal. It does not own provider dispatch, tool execution, terminal layout,
or the decision to persist a turn.

Two callers illustrate the boundary:

1. The [query processor](../../src/cli/repl_event/query_processor.rs) and [event loop](../../src/cli/repl_event/event_loop.rs)
   retain an `Arc<WorkUnit>` while provider and tool events arrive. The [output manager](../../src/cli/output_manager.rs)
   starts that unit, stores it as a `MessageRef`, and gives the renderer a stable identity while
   the same unit accumulates rows and reaches a terminal status. Query ordering and execution
   stay with the application; this module owns the message's lifecycle and snapshot.
2. The [TUI adapter](../../crates/finch-tui/src/view_model.rs) reads a `MessageRef` at blit time. It asks for the
   WorkUnit view when present and hands that plain snapshot to `finch-ui-model` for projection;
   migrated messages — say turns plus `StaticMessage`, `ProgressMessage`, `LiveToolMessage`,
   `OperationMessage` (#1120), and `MemoryRecalledMessage` — answer the generalized
   `component_view` accessor instead, so the renderer never matches on the message type. Ordinary
   un-migrated messages supply formatted
   lines. The renderer owns disclosure and painting, while the message's complete transcript
   remains the canonical text for scrollback and copying.

The presentation snapshot vocabulary and pure projection live in
[`finch-ui-model`](../finch-ui-model/AGENTS.md); this crate re-exports
some of those types for application compatibility. Read [AGENTS.md](AGENTS.md) for lifecycle and
dependency rules, then [`src/lib.rs`](src/lib.rs) for the callable facade. Rustdoc supplies signatures
without a checked-in symbol catalog.
