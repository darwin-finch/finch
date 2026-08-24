// MCP (Model Context Protocol) integration
//
// Enables Shammah to connect to external MCP servers and use their tools.
//
// Architecture:
// - McpClient: Manages multiple server connections
// - McpConnection: Wraps a single MCP server connection
// - McpServerConfig: Configuration for MCP servers
//
// Supported transports:
// - STDIO: Launch local server processes (e.g., npx @modelcontextprotocol/server-*)
// - Streamable HTTP: not implemented; legacy SSE config is rejected explicitly
//
// Usage:
// ```rust
// let mcp_client = McpClient::from_config(&config.mcp_servers).await?;
// let tools = mcp_client.list_tools().await;
// let result = mcp_client.execute_tool("mcp_filesystem_read_file", params).await?;
// ```

pub mod client;
pub mod config;
pub mod connection;
pub mod protocol;

pub use client::{McpClient, McpToolDescriptor};
pub use config::{McpServerConfig, TransportType};
pub use connection::McpConnection;
