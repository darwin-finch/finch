# External MCP connections

This module connects to user-configured Model Context Protocol servers, discovers their
advertised tools, and sends calls over a server connection. It owns the connection lifecycle
and untrusted discovery data, not permission decisions, local tool execution, or the typed VM.
STDIO launches a local server process; streamable HTTP is not implemented, and legacy SSE
configuration is rejected explicitly.

Two callers show the boundary:

1. The [tool executor](../executor.rs) builds an `McpClient` from configured servers and keeps
   it for external tool calls. If an individual server fails to connect, the client continues
   with those that did; the executor handles tool approval and dispatch. Discovered names are
   prefixed with `mcp_<server>_` before joining the tool vocabulary.
2. The [interactive event loop](../../cli/repl_event/event_loop.rs) uses the executor's client
   to list, refresh, and reload the available MCP tools for `/mcp` commands. After refresh it
   asks the application runtime to rebind the typed VM vocabulary. The event loop owns what the
   user sees and when bindings change; this module supplies current server/tool data.

Read [AGENTS.md](AGENTS.md) for trust and dependency rules, and [`mod.rs`](mod.rs) for the flat
callable facade. `McpConnection::list_tools` and `McpConnection::server_info` used to return
`McpTool`/`McpServerInfo`, two child-defined types the facade never exported — a real gap only if
something outside this capsule could reach them (issue #1047). A workspace-wide reference audit
found neither: `list_tools` is called only from `McpClient` (a sibling module inside this
capsule), and `server_info` had no caller anywhere, including inside this capsule. Both documented
workflows above go through `McpClient::list_tools`, which returns the already-exported
`ToolDefinition` shape. `list_tools` is now `pub(super)` and `server_info` is removed; `McpTool`
and `McpServerInfo` stay unexported because nothing outside this capsule needs to name them.
