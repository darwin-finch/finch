# tools::mcp — public interface

Generated from [`src/tools/mcp/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/tools/mcp/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// MCP client that manages multiple server connections.
pub struct McpClient { … }
impl McpClient {
    /// Disconnect from a specific server
    pub async fn disconnect(&self, name: &str) -> Result<()>;
    /// Disconnect from all servers
    pub async fn disconnect_all(&self) -> Result<()>;
    /// Execute a tool on the appropriate server
    pub async fn execute_tool(&self, tool_name: &str, params: Value) -> Result<String>;
    /// Execute an MCP tool while preserving the structured JSON result for a typed VM or other non-chat embedder.
    pub async fn execute_tool_value(&self, tool_name: &str, params: Value) -> Result<Value>;
    /// Connect to MCP servers from configuration
    pub async fn from_config(servers: &HashMap<String, McpServerConfig>) -> Result<Self>;
    /// Check if a server is connected
    pub async fn is_connected(&self, name: &str) -> bool;
    /// Get list of connected server names
    pub async fn list_servers(&self) -> Vec<String>;
    /// List all available tools from all connected servers
    pub async fn list_tools(&self) -> Vec<ToolDefinition>;
    /// Refresh tools from all servers
    pub async fn refresh_all_tools(&self) -> Result<()>;
    /// Reconnect every enabled server using the configuration loaded at startup.
    pub async fn reload(&self) -> Result<()>;
    /// Return raw discovered descriptors with explicit server provenance.
    pub async fn tool_descriptors(&self) -> Vec<McpToolDescriptor>;
    /// Create a new MCP client
    pub fn new() -> Self;
    /// Configured timeout for a prefixed MCP tool, if its server is known.
    pub fn timeout_for_tool(&self, tool_name: &str) -> Option<std::time::Duration>;
}
/// A single MCP server connection over STDIO
pub struct McpConnection { … }
impl McpConnection {
    /// Call a tool on this server
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String>;
    /// Call a tool without flattening its structured result into prose.
    pub async fn call_tool_value(&self, tool_name: &str, arguments: Value) -> Result<Value>;
    /// Connect to an MCP server
    pub async fn connect(name: String, config: &McpServerConfig) -> Result<Self>;
    /// Refresh the list of available tools
    pub async fn refresh_tools(&mut self) -> Result<()>;
    /// Shutdown the connection
    pub async fn shutdown(&mut self) -> Result<()>;
    /// Check if connected
    pub fn is_connected(&self) -> bool;
    /// Get the list of available tools
    pub fn list_tools(&self) -> &[McpTool];
    /// Get the server name
    pub fn name(&self) -> &str;
    /// Get server info
    pub fn server_info(&self) -> Option<&McpServerInfo>;
}
/// MCP server configuration
pub struct McpServerConfig { … }
impl McpServerConfig {
    /// Validate the configuration
    pub fn validate(&self, name: &str) -> anyhow::Result<()>;
}
/// Untrusted discovery data retained with its server provenance.
pub struct McpToolDescriptor { … }
/// Transport type for MCP servers
pub enum TransportType { Stdio, Sse }
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `McpServerInfo`, `McpTool`
