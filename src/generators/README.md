# Finch generator adapters

This root-package module adapts configured cloud and local backends to the older `Generator`
contract used by the interactive REPL. It owns Finch-specific `ClaudeGenerator`,
`DaemonLocalGenerator`, and `QwenGenerator` adapters and profile naming. It does not execute
tools, select application configuration, or own the provider-neutral development contract in
`finch-generation`. Provider transports and wire chunk parsing belong to `finch-providers`.

At REPL startup, `src/cli/repl.rs` selects a configured provider profile, constructs a cloud
adapter or daemon-local adapter, and passes an `Arc<dyn Generator>` to the event loop. The
application owns profile choice and daemon connection; this module owns the adapter behavior
and the compatibility trait.

During a query, `src/cli/repl_event/query_processor.rs` calls the generator's cancellable
stream method and consumes `StreamChunk` events. It accumulates text and usage, passes validated
tool-call events to the application `ToolLoop`, and updates a WorkUnit. Tool execution and
presentation stay in the event loop, not in an adapter.

Read [AGENTS.md](AGENTS.md) for dependencies and invariants, [mod.rs](mod.rs) for the facade, and
root-package rustdoc for methods. Some `finch-generation` types are re-exported here for
compatibility but are not yet the production REPL lifecycle.
