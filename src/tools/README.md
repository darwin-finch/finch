# tools: tool execution, implementations, and composition-root wiring

Owns the tool executor, the concrete tool implementations, the event-loop's tool-round protocol
integration, the session task list, and post-edit diagnostics. The shared *vocabulary* tools use
(the `Tool` trait, permission policy, declared effects) lives in the dependency-free
[`finch-tools-api`](../../crates/finch-tools-api/README.md) crate; this directory is where that
vocabulary gets concrete implementations and application-bound wiring, which is why it may depend
on any composition-root subsystem while the API crate may depend on none of them.

Ownership, dependencies, invariants, and test commands are in [`AGENTS.md`](AGENTS.md); connecting
to external MCP servers is the nested [`mcp`](mcp/README.md) facade.

## Further documentation

- [`EXECUTION.md`](EXECUTION.md) — the permission model (Allow/AskUser/Deny) and effect
  declarations.
- [`../../docs/MACOS_GUI_AUTOMATION.md`](../../docs/MACOS_GUI_AUTOMATION.md) — macOS GUI
  automation tools; must follow the root
  [GUI accessibility invariants](../../CLAUDE.md#gui-accessibility).
