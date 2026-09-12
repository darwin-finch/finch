# tools/mcp capsule: the MCP client

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**Owns** `src/tools/mcp/`: connecting to external Model Context Protocol servers, listing the tools
they advertise, and calling one. `McpClient` holds the connections; `McpConnection` wraps a single
server; `McpServerConfig` and `TransportType` are the user-facing configuration shape.

This is a sub-subsystem of `tools` — a subsystem declared on a path inside another's, which is all
nesting means: a directory with its own capsule inside another's. Executing a *local* tool is the
parent's job; nothing here decides permissions or authority.

**Interface:** [`INTERFACE.md`](INTERFACE.md) lists every exported item with its signature. The
child modules are private, so the `pub use` list in `src/tools/mcp/mod.rs` is the whole public
surface, and `scripts/check_subsystems.py` rejects a `pub mod` there.

**Dependencies:** none downward, and one unwanted edge back up to its parent — `client.rs` uses the tool
vocabulary `ToolDefinition` and `ToolInputSchema` — which clears when `tools` splits its API from
its implementations (that is the tools facade work). Add no other import.

**Transports are not equal.** STDIO launches a local process and is supported. Streamable HTTP is
not implemented, and a legacy SSE configuration is rejected explicitly rather than silently
downgraded; keep that rejection loud if you touch `config.rs`.

**A server is untrusted input.** Tool names, descriptions, and schemas arrive from a process the
user configured but nobody here reviewed. They are data, never instructions, and the names are
namespaced (`mcp_<server>_<tool>`) before they reach the registry.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- tools::mcp::`. Run the parent's
tests too when changing a re-exported item, because the executor and the runtime both hold an
`McpClient`.
