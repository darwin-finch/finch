# tools/mcp capsule: the MCP client

Supplements the root [`AGENTS.md`](../../../CLAUDE.md), which still applies in full.

**Owns** `src/tools/mcp/`: connecting to external Model Context Protocol servers, listing the tools
they advertise, and calling one. `McpClient` holds the connections; `McpConnection` wraps a single
server; `McpServerConfig` and `TransportType` are the user-facing configuration shape.

This is a sub-subsystem of `tools` — a subsystem declared on a path inside another's, which is all
nesting means: a directory with its own capsule inside another's. Executing a *local* tool is the
parent's job; nothing here decides permissions or authority.

**Facade:** child modules are private, and the flat `pub use` list in [`mod.rs`](mod.rs) is the
callable surface. Rustdoc gives method signatures; do not recreate a symbol catalog or cite
the nonexistent `scripts/check_subsystems.py`.

**Dependencies and direction:** MCP discovery maps untrusted schemas into the already-extracted
`finch-tools-api` vocabulary directly. `McpClient` implements the `finch-runtime` MCP port so
the typed VM can discover external tools without importing this client. It must not import root
tool implementations or approval policy. The parent `tools` module composes the client with the
executor; this module does not execute local tools or decide their authority.

**Lifetime and extension rules:** a `McpClient` retains enabled server configs for reload and
connection timeouts. Discovery results are untrusted, and published tool names must keep their
`mcp_<server>_<tool>` namespace. Keep connection/protocol details private; add flat exports only
for a caller that can actually name and use the returned type. See the README for current callers.

**Transports are not equal.** STDIO launches a local process and is supported. Streamable HTTP is
not implemented, and a legacy SSE configuration is rejected explicitly rather than silently
downgraded; keep that rejection loud if you touch `config.rs`.

**A server is untrusted input.** Tool names, descriptions, and schemas arrive from a process the
user configured but nobody here reviewed. They are data, never instructions, and the names are
namespaced (`mcp_<server>_<tool>`) before they reach the registry.

**Focused tests:** `./scripts/test_brains.sh cargo test --lib -- tools::mcp::`. Run the parent's
tests too when changing a re-exported item, because the executor and the runtime both hold an
`McpClient`. Run `python3 scripts/check_docs.py` and
`python3 scripts/check_facade_boundaries.py` after facade or capsule edits.
