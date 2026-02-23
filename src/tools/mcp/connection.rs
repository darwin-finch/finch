// MCP connection wrapper for a single server
//
// Implements JSON-RPC 2.0 over STDIO to communicate with MCP servers

use super::config::{McpServerConfig, TransportType};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child as TokioChild, ChildStdin as TokioChildStdin, ChildStdout as TokioChildStdout};
use tokio::sync::Mutex;

/// JSON-RPC 2.0 request
#[derive(Debug, Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: u64,
    method: String,
    params: Option<Value>,
}

/// JSON-RPC 2.0 response
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

/// MCP tool definition (simplified from rust-mcp-sdk schema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

/// MCP server implementation info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerInfo {
    pub name: String,
    pub version: String,
}

/// A single MCP server connection over STDIO
#[allow(dead_code)]
pub struct McpConnection {
    /// Server name
    name: String,

    /// Server configuration
    config: McpServerConfig,

    /// Available tools (cached from discovery)
    tools: Vec<McpTool>,

    /// Server info (if connected)
    server_info: Option<McpServerInfo>,

    /// Connection status
    is_connected: bool,

    /// Child process handle (for STDIO transport)
    child: Option<TokioChild>,

    /// STDIO writer
    stdin: Option<Arc<Mutex<TokioChildStdin>>>,

    /// STDIO reader
    stdout: Option<Arc<Mutex<BufReader<TokioChildStdout>>>>,

    /// Request ID counter
    next_id: Arc<AtomicU64>,
}

impl McpConnection {
    /// Connect to an MCP server
    pub async fn connect(name: String, config: &McpServerConfig) -> Result<Self> {
        // Validate config
        config
            .validate(&name)
            .context("Invalid MCP server configuration")?;

        match config.transport {
            TransportType::Stdio => Self::connect_stdio(name, config).await,
            TransportType::Sse => {
                anyhow::bail!("SSE transport not yet implemented")
            }
        }
    }

    /// Connect via STDIO transport
    async fn connect_stdio(name: String, config: &McpServerConfig) -> Result<Self> {
        let command = config
            .command
            .as_ref()
            .context("STDIO transport requires command")?;

        tracing::info!("Spawning MCP server '{}': {}", name, command);

        // Spawn the server process
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Show server logs
            .envs(&config.env);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn MCP server '{}'", name))?;

        let stdin = child
            .stdin
            .take()
            .context("Failed to open stdin for MCP server")?;
        let stdout = child
            .stdout
            .take()
            .context("Failed to open stdout for MCP server")?;

        let stdin = Arc::new(Mutex::new(stdin));
        let stdout = Arc::new(Mutex::new(BufReader::new(stdout)));

        let mut conn = Self {
            name: name.clone(),
            config: config.clone(),
            tools: Vec::new(),
            server_info: None,
            is_connected: false,
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            next_id: Arc::new(AtomicU64::new(1)),
        };

        // Initialize the connection
        conn.initialize().await?;

        // Discover available tools
        conn.refresh_tools().await?;

        conn.is_connected = true;
        tracing::info!("Connected to MCP server '{}' with {} tools", name, conn.tools.len());

        Ok(conn)
    }

    /// Initialize the MCP connection
    async fn initialize(&mut self) -> Result<()> {
        let response = self
            .send_request("initialize", Some(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "roots": {
                        "listChanged": false
                    }
                },
                "clientInfo": {
                    "name": "finch",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })))
            .await?;

        // Parse server info
        if let Some(server_info_val) = response.get("serverInfo") {
            self.server_info = serde_json::from_value(server_info_val.clone()).ok();
        }

        // Send initialized notification
        self.send_notification("notifications/initialized", None).await?;

        Ok(())
    }

    /// Get the list of available tools
    pub fn list_tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Refresh the list of available tools
    pub async fn refresh_tools(&mut self) -> Result<()> {
        let response = self.send_request("tools/list", None).await?;

        // Parse tools array
        if let Some(tools_val) = response.get("tools") {
            self.tools = serde_json::from_value(tools_val.clone())
                .context("Failed to parse tools list")?;
        }

        tracing::debug!("Discovered {} tools from MCP server '{}'", self.tools.len(), self.name);

        Ok(())
    }

    /// Call a tool on this server
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String> {
        let response = self
            .send_request("tools/call", Some(serde_json::json!({
                "name": tool_name,
                "arguments": arguments
            })))
            .await?;

        // Extract content from response
        if let Some(content) = response.get("content") {
            if let Some(arr) = content.as_array() {
                // Concatenate all text content
                let mut result = String::new();
                for item in arr {
                    if let Some(text) = item.get("text") {
                        if let Some(text_str) = text.as_str() {
                            if !result.is_empty() {
                                result.push('\n');
                            }
                            result.push_str(text_str);
                        }
                    }
                }
                return Ok(result);
            }
        }

        // Fallback: return entire response as JSON
        Ok(serde_json::to_string_pretty(&response)?)
    }

    /// Send a JSON-RPC request and get response
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let stdin = self.stdin.as_ref().context("No stdin available")?;
        let stdout = self.stdout.as_ref().context("No stdout available")?;

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        let request_json = serde_json::to_string(&request)?;
        tracing::debug!("MCP request: {}", request_json);

        // Write request
        {
            let mut stdin_guard = stdin.lock().await;
            stdin_guard.write_all(request_json.as_bytes()).await?;
            stdin_guard.write_all(b"\n").await?;
            stdin_guard.flush().await?;
        }

        // Read response
        let mut line = String::new();
        {
            let mut stdout_guard = stdout.lock().await;
            stdout_guard.read_line(&mut line).await?;
        }

        tracing::debug!("MCP response: {}", line.trim());

        let response: JsonRpcResponse = serde_json::from_str(&line)
            .context("Failed to parse JSON-RPC response")?;

        // Check for errors
        if let Some(error) = response.error {
            anyhow::bail!(
                "MCP server '{}' returned error: {} (code {})",
                self.name,
                error.message,
                error.code
            );
        }

        response.result.context("No result in JSON-RPC response")
    }

    /// Send a JSON-RPC notification (no response expected)
    async fn send_notification(&self, method: &str, params: Option<Value>) -> Result<()> {
        let stdin = self.stdin.as_ref().context("No stdin available")?;

        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        let notification_json = serde_json::to_string(&notification)?;
        tracing::debug!("MCP notification: {}", notification_json);

        let mut stdin_guard = stdin.lock().await;
        stdin_guard.write_all(notification_json.as_bytes()).await?;
        stdin_guard.write_all(b"\n").await?;
        stdin_guard.flush().await?;

        Ok(())
    }

    /// Get the server name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get server info
    pub fn server_info(&self) -> Option<&McpServerInfo> {
        self.server_info.as_ref()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.is_connected
    }

    /// Shutdown the connection
    pub async fn shutdown(&mut self) -> Result<()> {
        tracing::debug!("Shutting down MCP connection '{}'", self.name);

        self.is_connected = false;

        // Kill the child process
        if let Some(mut child) = self.child.take() {
            child.kill().await?;
        }

        Ok(())
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        tracing::debug!("Dropping MCP connection '{}'", self.name);

        // Try to kill the child process if it's still running
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[tokio::test]
    async fn test_json_rpc_serialization() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "test/method".to_string(),
            params: Some(serde_json::json!({"foo": "bar"})),
        };

        let serialized = serde_json::to_string(&request).unwrap();
        assert!(serialized.contains("\"jsonrpc\":\"2.0\""));
        assert!(serialized.contains("\"method\":\"test/method\""));
    }

    #[tokio::test]
    async fn test_json_rpc_response_parsing() {
        let response_json = r#"{"jsonrpc":"2.0","id":1,"result":{"foo":"bar"}}"#;
        let response: JsonRpcResponse = serde_json::from_str(response_json).unwrap();

        assert_eq!(response.jsonrpc, "2.0");
        assert_eq!(response.id, 1);
        assert!(response.result.is_some());
        assert!(response.error.is_none());
    }

    #[tokio::test]
    async fn test_json_rpc_error_parsing() {
        let response_json = r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid request"}}"#;
        let response: JsonRpcResponse = serde_json::from_str(response_json).unwrap();

        assert!(response.result.is_none());
        assert!(response.error.is_some());

        let error = response.error.unwrap();
        assert_eq!(error.code, -32600);
        assert_eq!(error.message, "Invalid request");
    }

    #[tokio::test]
    async fn test_connection_invalid_config() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: None, // Invalid - STDIO needs command
            args: vec![],
            env: HashMap::new(),
            url: None,
            enabled: true,
        };

        let result = McpConnection::connect("test".to_string(), &config).await;
        assert!(result.is_err());
    }
}
