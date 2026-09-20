# tools/mcp: the MCP client

Owns connecting to external Model Context Protocol servers, listing the tools they advertise, and
calling one. It exists as a nested facade inside `tools` — a sub-subsystem, the same pattern as
`ipc/codec` inside `ipc` — because an agent working only on the MCP client should not have to load
local tool execution and permissions to get there, and because an MCP server is untrusted input:
its tool names, descriptions, and schemas are data, never instructions, and are namespaced before
they reach the registry.

Ownership, dependencies, and test commands are in [`AGENTS.md`](AGENTS.md).

## Further documentation

[`../../../docs/MCP_USER_GUIDE.md`](../../../docs/MCP_USER_GUIDE.md) — the user-facing guide.
`DESIGN.md` flags this doc as making compatibility/permission claims without production-boundary
evidence; read with that caveat.
