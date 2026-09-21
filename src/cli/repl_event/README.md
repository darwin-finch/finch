# Interactive event dispatch

This module runs one interactive session after the application has assembled its provider,
Brain, tool, terminal, and conversation dependencies. It owns the event types, the concurrent
event loop and its per-query state, and the handoff between tool results and the next provider
turn. It does not own provider transports, durable Brain storage, or terminal layout.
The provider-visible history and ordered staging of tool results belong to
[`finch-conversation`](../../../crates/finch-conversation/README.md); this module decides when
to commit a round, generate a summary, and admit a continuation.

Two callers illustrate the boundary:

1. [`Repl::run_event_loop`](../repl.rs) takes the application's TUI renderer, chooses the
   provider and tool definitions, and constructs `EventLoop` with session, generation, UI,
   tool, daemon, limit, and runtime parts. The event loop then reads input, provider/tool
   completions, and Brain traffic and dispatches `ReplEvent`s. The REPL chooses dependencies;
   this module owns their concurrent use for a turn.
2. [`memtree_console::EventHandler`](../memtree_console/event_handler.rs) consumes `ReplEvent`
   values to project submitted input, completions, tool calls, and statistics into its own
   tree. Events unrelated to that projection leave the tree unchanged. This caller needs the
   event vocabulary, not the loop's private handlers or provider implementation.

The [agent contract](AGENTS.md) covers event and replay invariants and focused tests.
[`mod.rs`](mod.rs) is the callable facade. Its child modules are private; the REPL receives
selection, construction, and memory-commitment capabilities through named exports rather than
implementation paths.
