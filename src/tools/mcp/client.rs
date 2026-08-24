// MCP client coordinator - manages multiple server connections
//
// Uses a direct JSON-RPC 2.0 implementation over stdio.

use super::config::McpServerConfig;
use super::connection::McpConnection;
use crate::tools::types::{ToolDefinition, ToolInputSchema};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Untrusted discovery data retained with its server provenance. Consumers
/// must validate the schema and render the prose as data before publishing a
/// model-facing binding.
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolDescriptor {
    pub server: String,
    pub tool: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}

/// MCP client that manages multiple server connections.
pub struct McpClient {
    /// Original configuration, retained for reloads and timeout lookup.
    configs: HashMap<String, McpServerConfig>,
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
            configs: HashMap::new(),
            connections: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Connect to MCP servers from configuration
    pub async fn from_config(servers: &HashMap<String, McpServerConfig>) -> Result<Self> {
        let client = Self {
            configs: servers.clone(),
            connections: Arc::new(RwLock::new(HashMap::new())),
        };

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

        // HashMap iteration order is intentionally unstable. Stable tool order
        // improves reproducible model requests and upstream prompt caching.
        tools.sort_by(|left, right| left.name.cmp(&right.name));
        tools
    }

    /// Return raw discovered descriptors with explicit server provenance.
    /// This is the input to the typed VM adapter; unlike `list_tools`, it does
    /// not flatten JSON Schema into the legacy provider-tool shape.
    pub async fn tool_descriptors(&self) -> Vec<McpToolDescriptor> {
        let connections = self.connections.read().await;
        let mut descriptors = Vec::new();
        for (server, connection) in connections.iter() {
            for tool in connection.read().await.list_tools() {
                descriptors.push(McpToolDescriptor {
                    server: server.clone(),
                    tool: tool.name.clone(),
                    description: tool.description.clone(),
                    input_schema: tool.input_schema.clone(),
                    output_schema: tool.output_schema.clone(),
                });
            }
        }
        descriptors.sort_by(|left, right| {
            (&left.server, &left.tool).cmp(&(&right.server, &right.tool))
        });
        descriptors
    }

    /// Execute a tool on the appropriate server
    pub async fn execute_tool(&self, tool_name: &str, params: Value) -> Result<String> {
        let (server_name, actual_tool_name, conn) = self.resolve_tool(tool_name).await?;

        tracing::debug!(
            "Executing MCP tool '{}' on server '{}'",
            actual_tool_name,
            server_name
        );

        let conn = conn.read().await;
        conn.call_tool(&actual_tool_name, params)
            .await
            .with_context(|| {
                format!(
                    "Failed to execute tool '{}' on MCP server '{}'",
                    actual_tool_name, server_name
                )
            })
    }

    /// Execute an MCP tool while preserving the structured JSON result for a
    /// typed VM or other non-chat embedder.
    pub async fn execute_tool_value(&self, tool_name: &str, params: Value) -> Result<Value> {
        let (server_name, actual_tool_name, conn) = self.resolve_tool(tool_name).await?;
        let conn = conn.read().await;
        conn.call_tool_value(&actual_tool_name, params)
            .await
            .with_context(|| {
                format!(
                    "Failed to execute tool '{}' on MCP server '{}'",
                    actual_tool_name, server_name
                )
            })
    }

    async fn resolve_tool(
        &self,
        tool_name: &str,
    ) -> Result<(String, String, Arc<RwLock<McpConnection>>)> {
        let connections = self.connections.read().await;
        let unprefixed = tool_name
            .strip_prefix("mcp_")
            .with_context(|| format!("Invalid MCP tool name: {tool_name}"))?;
        // Match configured names rather than splitting on `_`; both server and
        // tool names may legitimately contain underscores. Prefer the longest
        // matching server name when one name prefixes another.
        let mut candidates: Vec<_> = connections
            .keys()
            .filter_map(|name| {
                unprefixed
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('_'))
                    .map(|tool| (name.as_str(), tool.to_owned()))
            })
            .collect();
        candidates.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));

        let mut owner = None;
        for (server_name, actual_tool_name) in candidates {
            let conn = connections
                .get(server_name)
                .expect("candidate server must exist");
            if conn
                .read()
                .await
                .list_tools()
                .iter()
                .any(|tool| tool.name == actual_tool_name)
            {
                owner = Some((server_name, actual_tool_name, Arc::clone(conn)));
                break;
            }
        }
        let (server_name, actual_tool_name, conn) =
            owner.with_context(|| format!("No connected MCP server owns tool '{tool_name}'"))?;
        Ok((server_name.to_owned(), actual_tool_name, Arc::clone(&conn)))
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

    /// Reconnect every enabled server using the configuration loaded at startup.
    pub async fn reload(&self) -> Result<()> {
        self.disconnect_all().await?;
        for (name, config) in &self.configs {
            if !config.enabled {
                continue;
            }
            match McpConnection::connect(name.clone(), config).await {
                Ok(connection) => {
                    self.connections
                        .write()
                        .await
                        .insert(name.clone(), Arc::new(RwLock::new(connection)));
                }
                Err(error) => {
                    tracing::warn!("Failed to reconnect to MCP server '{}': {}", name, error);
                }
            }
        }
        Ok(())
    }

    /// Configured timeout for a prefixed MCP tool, if its server is known.
    pub fn timeout_for_tool(&self, tool_name: &str) -> Option<std::time::Duration> {
        let unprefixed = tool_name.strip_prefix("mcp_")?;
        self.configs
            .iter()
            .filter_map(|(name, config)| {
                unprefixed
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('_'))
                    .map(|_| (name.len(), config.timeout_secs))
            })
            .max_by_key(|(name_len, _)| *name_len)
            .map(|(_, seconds)| std::time::Duration::from_secs(seconds))
    }

    /// Get list of connected server names
    pub async fn list_servers(&self) -> Vec<String> {
        let mut servers: Vec<_> = self.connections.read().await.keys().cloned().collect();
        servers.sort();
        servers
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
                timeout_secs: 300,
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
                timeout_secs: 300,
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
                timeout_secs: 300,
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
