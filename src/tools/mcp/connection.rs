// MCP connection wrapper for a single server

use super::config::{McpServerConfig, TransportType};
use anyhow::{Context, Result};
use rust_mcp_sdk::mcp_client::client_runtime::create_client;
use rust_mcp_sdk::mcp_client::{ClientHandler, McpClientOptions, ToMcpClientHandler};
use rust_mcp_sdk::schema::{
    CallToolRequestParams, ClientCapabilities, Implementation, InitializeRequestParams,
    ProtocolVersion, Tool,
};
use rust_mcp_sdk::task_store::InMemoryTaskStore;
use rust_mcp_sdk::{StdioTransport, TransportOptions};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Basic client handler (no custom behavior needed)
pub struct BasicClientHandler;

#[async_trait::async_trait]
impl ClientHandler for BasicClientHandler {}

/// A single MCP server connection
/// Note: We can't name the client type explicitly as ClientRuntime is private,
/// so we use a dynamic approach
pub struct McpConnection {
    /// Server name
    name: String,

    /// Available tools (cached)
    tools: Vec<Tool>,

    /// Server version info
    server_info: Option<Implementation>,
}

impl McpConnection {
    /// Connect to an MCP server
    /// Returns (connection, client handle) tuple
    /// The client handle must be kept alive for the duration of the connection
    pub async fn connect(
        name: String,
        config: &McpServerConfig,
    ) -> Result<(Self, Arc<dyn std::any::Any + Send + Sync>)> {
        // Validate config
        config
            .validate(&name)
            .context("Invalid MCP server configuration")?;

        // Create client runtime based on transport type
        match config.transport {
            TransportType::Stdio => Self::connect_stdio(name, config).await,
            TransportType::Sse => {
                // SSE support coming in next iteration
                anyhow::bail!("SSE transport not yet implemented");
            }
        }
    }

    /// Connect via STDIO transport
    async fn connect_stdio(
        name: String,
        config: &McpServerConfig,
    ) -> Result<(Self, Arc<dyn std::any::Any + Send + Sync>)> {
        let command = config
            .command
            .as_ref()
            .context("STDIO transport requires command")?;

        tracing::debug!(
            "Launching MCP server '{}': {} {}",
            name,
            command,
            config.args.join(" ")
        );

        // Create STDIO transport
        let transport = StdioTransport::create_with_server_launch(
            command,
            config.args.clone(),
            if config.env.is_empty() {
                None
            } else {
                Some(config.env.clone())
            },
            TransportOptions::default(),
        )
        .map_err(|e| anyhow::anyhow!("Failed to create STDIO transport: {:?}", e))?;

        // Create client capabilities
        let client_details = InitializeRequestParams {
            protocol_version: ProtocolVersion::V2025_11_25.into(),
            capabilities: ClientCapabilities::default(),
            client_info: Implementation {
                name: "shammah".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: Some("Local-first Constitutional AI proxy".to_string()),
                icons: vec![],
                title: None,
                website_url: None,
            },
            meta: None,
        };

        // Create and start client
        let handler = BasicClientHandler;
        let client = create_client(McpClientOptions {
            client_details,
            transport,
            handler: handler.to_mcp_client_handler(),
            task_store: Some(Arc::new(InMemoryTaskStore::new(None))),
            server_task_store: Some(Arc::new(InMemoryTaskStore::new(None))),
        });

        client
            .clone()
            .start()
            .await
            .context("Failed to start MCP client")?;

        // Get server info
        let server_info = client.server_version();

        // Discover tools
        let tools = client
            .request_tool_list(None)
            .await
            .context("Failed to list tools")?
            .tools;

        tracing::info!(
            "Connected to MCP server '{}' with {} tools",
            name,
            tools.len()
        );

        let conn = Self {
            name,
            tools,
            server_info,
        };

        // Return both connection and client (client must be kept alive)
        Ok((conn, Arc::new(client) as Arc<dyn std::any::Any + Send + Sync>))
    }

    /// Get the list of available tools
    pub fn list_tools(&self) -> &[Tool] {
        &self.tools
    }

    /// Get the server name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get server info
    pub fn server_info(&self) -> Option<&Implementation> {
        self.server_info.as_ref()
    }
}
