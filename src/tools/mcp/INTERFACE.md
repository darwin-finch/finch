# tools-mcp — public interface

Generated from [`src/tools/mcp/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/tools/mcp/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)
- **May depend on:** nothing. Debt: `tools`.

Everything below is what callers outside this subsystem can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// MCP client that manages multiple server connections.
pub struct McpClient { … }
/// A single MCP server connection over STDIO
pub struct McpConnection { … }
/// MCP server configuration
pub struct McpServerConfig { … }
/// Untrusted discovery data retained with its server provenance.
pub struct McpToolDescriptor { … }
/// Transport type for MCP servers
pub enum TransportType { Stdio, Sse }
```
