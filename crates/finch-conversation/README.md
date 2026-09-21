# Finch conversation state

`finch-conversation` owns the provider-visible message history for one interactive session and
the ordered staging of a tool round before it becomes visible. It also owns JSON snapshots of
that history. It does not call a provider, execute tools, decide when to compact a request, or
own a named Brain's durable journal. The root application chooses those actions; the only Finch
crate this state depends on is `finch-providers` for its `Message` and `ContentBlock` wire types.

Two caller workflows show the boundary:

1. The [REPL](../../src/cli/repl.rs) creates a `ConversationHistory` for an interactive session
   and gives the same shared state to its event loop and query processor. The query processor
   reads committed messages for a provider request; the application-owned
   [summary adapter](../../src/cli/conversation_compactor.rs) may plan a stable summary prefix.
   This crate stores the messages, but does not choose a model or make a generation call.
2. The [event loop](../../src/cli/repl_event/event_loop.rs) stages a complete assistant tool-call
   payload, records results by tool id, and commits the assistant/result pair in declaration
   order only when every result is present. The application controls tool execution, continuation
   admission, and when to checkpoint; staged work is invisible to request readers and snapshots.

The [agent contract](AGENTS.md) records the ordering and persistence invariants. The flat
[crate facade](src/lib.rs) and rustdoc provide the callable surface; there is no generated API
catalog.
