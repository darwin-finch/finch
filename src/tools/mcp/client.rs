// MCP client coordinator - manages multiple server connections
//
// Uses direct JSON-RPC 2.0 implementation over STDIO/SSE transports.

use super::config::McpServerConfig;
use super::connection::McpConnection;
use crate::tools::types::{ToolDefinition, ToolInputSchema};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// MCP client that manages multiple server connections (stub implementation)
pub struct McpClient {
    /// Active server connections (name -> connection)
    connections: Arc<RwLock<HashMap<String, Arc<RwLock<McpConnection>>>>>,
}

impl Default for McpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl McpClient {
    /// Create a new MCP client
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Connect to MCP servers from configuration
    pub async fn from_config(servers: &HashMap<String, McpServerConfig>) -> Result<Self> {
        let client = Self::new();

        for (name, config) in servers {
            if !config.enabled {
                tracing::debug!("Skipping disabled MCP server '{}'", name);
                continue;
            }

            match McpConnection::connect(name.clone(), config).await {
                Ok(conn) => {
                    client
                        .connections
                        .write()
                        .await
                        .insert(name.clone(), Arc::new(RwLock::new(conn)));
                    tracing::info!("Connected to MCP server: {}", name);
                }
                Err(e) => {
                    tracing::warn!("Failed to connect to MCP server '{}': {}", name, e);
                    // Continue with other servers
                }
            }
        }

        Ok(client)
    }

    /// List all available tools from all connected servers
    pub async fn list_tools(&self) -> Vec<ToolDefinition> {
        let connections = self.connections.read().await;
        let mut tools = Vec::new();

        for (server_name, conn) in connections.iter() {
            let conn = conn.read().await;
            let server_tools = conn.list_tools();

            for tool in server_tools {
                // Convert MCP tool to our ToolDefinition format
                // Prefix tool name with "mcp_<server>_" to avoid conflicts
                let prefixed_name = format!("mcp_{}_{}", server_name, tool.name);

                // Convert MCP input schema to our format
                let input_schema = convert_mcp_schema(&tool.input_schema);

                tools.push(ToolDefinition {
                    name: prefixed_name,
                    description: tool
                        .description
                        .clone()
                        .unwrap_or_else(|| format!("Tool from MCP server '{}'", server_name)),
                    input_schema,
                });
            }
        }

        tools
    }

    /// Execute a tool on the appropriate server
    pub async fn execute_tool(&self, tool_name: &str, params: Value) -> Result<String> {
        // Parse prefixed name: "mcp_<server>_<tool>"
        let parts: Vec<&str> = tool_name.split('_').collect();
        if parts.len() < 3 || parts[0] != "mcp" {
            anyhow::bail!("Invalid MCP tool name: {}", tool_name);
        }

        let server_name = parts[1];
        let actual_tool_name = parts[2..].join("_");

        tracing::debug!(
            "Executing MCP tool '{}' on server '{}'",
            actual_tool_name,
            server_name
        );

        // Find the connection
        let connections = self.connections.read().await;
        let conn = connections
            .get(server_name)
            .with_context(|| format!("MCP server '{}' not found", server_name))?;

        // Call the tool
        let conn = conn.read().await;
        conn.call_tool(&actual_tool_name, params)
            .await
            .context("Failed to execute MCP tool")
    }

    /// Refresh tools from all servers
    pub async fn refresh_all_tools(&self) -> Result<()> {
        let connections = self.connections.read().await;

        for (name, conn) in connections.iter() {
            let mut conn = conn.write().await;
            if let Err(e) = conn.refresh_tools().await {
                tracing::warn!("Failed to refresh tools for MCP server '{}': {}", name, e);
            }
        }

        Ok(())
    }

    /// Get list of connected server names
    pub async fn list_servers(&self) -> Vec<String> {
        self.connections.read().await.keys().cloned().collect()
    }

    /// Check if a server is connected
    pub async fn is_connected(&self, name: &str) -> bool {
        self.connections.read().await.contains_key(name)
    }

    /// Disconnect from a specific server
    pub async fn disconnect(&self, name: &str) -> Result<()> {
        let mut connections = self.connections.write().await;

        if let Some(conn) = connections.remove(name) {
            let mut conn = conn.write().await;
            conn.shutdown().await.context("Failed to shutdown server")?;
            tracing::info!("Disconnected from MCP server: {}", name);
        }

        Ok(())
    }

    /// Disconnect from all servers
    pub async fn disconnect_all(&self) -> Result<()> {
        let mut connections = self.connections.write().await;
        let names: Vec<_> = connections.keys().cloned().collect();

        for name in names {
            if let Some(conn) = connections.remove(&name) {
                let mut conn = conn.write().await;
                if let Err(e) = conn.shutdown().await {
                    tracing::warn!("Failed to shutdown MCP server '{}': {}", name, e);
                }
            }
        }

        tracing::info!("Disconnected from all MCP servers");
        Ok(())
    }
}

/// Convert MCP input schema to our ToolInputSchema format
fn convert_mcp_schema(mcp_schema: &Value) -> ToolInputSchema {
    // MCP schemas are JSON Schema format
    // Extract properties and required fields
    let properties = mcp_schema
        .get("properties")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));

    let required = mcp_schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    ToolInputSchema {
        schema_type: "object".to_string(),
        properties,
        required,
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        tracing::debug!("Dropping MCP client");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::mcp::TransportType;

    #[tokio::test]
    async fn test_client_creation() {
        let client = McpClient::new();
        let servers = client.list_servers().await;
        assert_eq!(servers.len(), 0);
    }

    #[tokio::test]
    async fn test_from_config_empty() {
        let config: HashMap<String, McpServerConfig> = HashMap::new();
        let client = McpClient::from_config(&config).await.unwrap();
        assert_eq!(client.list_servers().await.len(), 0);
    }

    #[tokio::test]
    async fn test_from_config_with_servers() {
        let mut config = HashMap::new();
        config.insert(
            "test1".to_string(),
            McpServerConfig {
                transport: TransportType::Stdio,
                command: Some("nonexistent_command".to_string()),
                args: vec![],
                env: HashMap::new(),
                url: None,
                enabled: true,
            },
        );
        config.insert(
            "test2".to_string(),
            McpServerConfig {
                transport: TransportType::Stdio,
                command: Some("test2".to_string()),
                args: vec![],
                env: HashMap::new(),
                url: None,
                enabled: false, // Disabled
            },
        );

        // With real implementation, connection to nonexistent command will fail
        // but client should still be created (it logs warnings and continues)
        let client = McpClient::from_config(&config).await.unwrap();
        let servers = client.list_servers().await;

        // Connection to nonexistent command should fail, so 0 servers
        assert_eq!(servers.len(), 0);
    }

    #[tokio::test]
    async fn test_disconnect() {
        let mut config = HashMap::new();
        config.insert(
            "test".to_string(),
            McpServerConfig {
                transport: TransportType::Stdio,
                command: Some("nonexistent_command".to_string()),
                args: vec![],
                env: HashMap::new(),
                url: None,
                enabled: true,
            },
        );

        // Connection will fail but client creation succeeds
        let client = McpClient::from_config(&config).await.unwrap();
        assert!(!client.is_connected("test").await); // Not connected because command doesn't exist

        // Disconnect should succeed even if not connected
        client.disconnect("test").await.unwrap();
        assert!(!client.is_connected("test").await);
    }
}
