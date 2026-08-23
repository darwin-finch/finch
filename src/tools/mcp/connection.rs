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
use tokio::process::{
    Child as TokioChild, ChildStdin as TokioChildStdin, ChildStdout as TokioChildStdout,
};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use super::protocol::{
    select_protocol_version, LATEST_LEGACY_PROTOCOL_VERSION, LATEST_PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS,
};

const MODERN_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// MCP tool definition used by Finch's JSON-RPC client.
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

    /// Negotiated MCP revision. Modern revisions are carried on every request;
    /// legacy revisions are established by the initialize handshake.
    protocol_version: Option<&'static str>,

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

    /// A stdio transport is one ordered stream. Keep a complete request/response
    /// exchange atomic so concurrent tool calls cannot consume each other's replies.
    request_lock: Arc<Mutex<()>>,
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
                anyhow::bail!("SSE transport is not yet supported for MCP server '{}'; use stdio transport instead", name)
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
        let environment = resolve_environment(&config.env)
            .with_context(|| format!("Invalid environment for MCP server '{name}'"))?;
        cmd.args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // Show server logs
            .envs(environment);

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
            protocol_version: None,
            is_connected: false,
            child: Some(child),
            stdin: Some(stdin),
            stdout: Some(stdout),
            next_id: Arc::new(AtomicU64::new(1)),
            request_lock: Arc::new(Mutex::new(())),
        };

        // Initialize the connection
        conn.initialize().await?;

        // Discover available tools
        conn.refresh_tools().await?;

        conn.is_connected = true;
        tracing::info!(
            "Connected to MCP server '{}' with {} tools",
            name,
            conn.tools.len()
        );

        Ok(conn)
    }

    /// Initialize the MCP connection
    async fn initialize(&mut self) -> Result<()> {
        // MCP 2026-07-28 removed the initialize handshake. Probe first and use
        // per-request metadata when supported; any ordinary error or timeout is
        // the specified signal to fall back to a legacy initialize handshake.
        let discover_params = with_protocol_metadata(None, LATEST_PROTOCOL_VERSION);
        match self
            .send_request_with_timeout(
                "server/discover",
                Some(discover_params),
                MODERN_PROBE_TIMEOUT.min(Duration::from_secs(self.config.timeout_secs)),
            )
            .await
        {
            Ok(response) => {
                let offered = response
                    .get("supportedVersions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str);
                let selected = select_protocol_version(offered).with_context(|| {
                    format!(
                        "MCP server '{}' did not advertise a protocol version Finch supports",
                        self.name
                    )
                })?;
                if selected != LATEST_PROTOCOL_VERSION {
                    anyhow::bail!(
                        "MCP server '{}' returned a legacy version from server/discover; legacy versions must use initialize",
                        self.name
                    );
                }
                self.protocol_version = Some(selected);
                self.server_info = response
                    .pointer("/_meta/io.modelcontextprotocol~1serverInfo")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok());
                tracing::debug!(
                    "MCP server '{}' negotiated modern protocol {}",
                    self.name,
                    selected
                );
                Ok(())
            }
            Err(probe_error) => {
                tracing::debug!(
                    "MCP server '{}' did not accept modern discovery ({}); falling back to {}",
                    self.name,
                    probe_error,
                    LATEST_LEGACY_PROTOCOL_VERSION
                );
                self.initialize_legacy().await
            }
        }
    }

    async fn initialize_legacy(&mut self) -> Result<()> {
        let response = self
            .send_request(
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": LATEST_LEGACY_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "finch",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                })),
            )
            .await?;

        let negotiated = response
            .get("protocolVersion")
            .and_then(Value::as_str)
            .context("MCP initialize response omitted protocolVersion")?;
        let negotiated = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|version| *version == negotiated && *version != LATEST_PROTOCOL_VERSION)
            .with_context(|| {
                format!(
                    "MCP server '{}' selected unsupported protocol version '{}'",
                    self.name, negotiated
                )
            })?;
        self.protocol_version = Some(negotiated);
        self.server_info = response
            .get("serverInfo")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        self.send_notification("notifications/initialized", None)
            .await
    }

    /// Get the list of available tools
    pub fn list_tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Refresh the list of available tools
    pub async fn refresh_tools(&mut self) -> Result<()> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = cursor
                .as_ref()
                .map(|cursor| serde_json::json!({ "cursor": cursor }));
            let response = self.send_request("tools/list", params).await?;
            if let Some(tools_val) = response.get("tools") {
                let mut page: Vec<McpTool> = serde_json::from_value(tools_val.clone())
                    .context("Failed to parse tools list")?;
                tools.append(&mut page);
            }
            cursor = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        self.tools = tools;

        tracing::debug!(
            "Discovered {} tools from MCP server '{}'",
            self.tools.len(),
            self.name
        );

        Ok(())
    }

    /// Call a tool on this server
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> Result<String> {
        let response = self
            .send_request(
                "tools/call",
                Some(serde_json::json!({
                    "name": tool_name,
                    "arguments": arguments
                })),
            )
            .await?;

        let is_error = response
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // Preserve every content type. Text blocks remain plain text for the
        // model; non-text blocks and structured content remain JSON.
        let mut parts = Vec::new();
        if let Some(content) = response.get("content") {
            if let Some(arr) = content.as_array() {
                for item in arr {
                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    } else {
                        parts.push(serde_json::to_string_pretty(item)?);
                    }
                }
            }
        }
        if let Some(structured) = response.get("structuredContent") {
            parts.push(format!(
                "Structured content:\n{}",
                serde_json::to_string_pretty(structured)?
            ));
        }
        let result = parts.join("\n");
        if is_error {
            anyhow::bail!(
                "{}",
                if result.is_empty() {
                    "MCP tool failed"
                } else {
                    &result
                }
            );
        }
        if !result.is_empty() {
            return Ok(result);
        }

        // Fallback: return entire response as JSON
        Ok(serde_json::to_string_pretty(&response)?)
    }

    /// Send a JSON-RPC request and get response
    async fn send_request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let params = match self.protocol_version {
            Some(LATEST_PROTOCOL_VERSION) => {
                Some(with_protocol_metadata(params, LATEST_PROTOCOL_VERSION))
            }
            _ => params,
        };
        self.send_request_with_timeout(
            method,
            params,
            Duration::from_secs(self.config.timeout_secs),
        )
        .await
    }

    async fn send_request_with_timeout(
        &self,
        method: &str,
        params: Option<Value>,
        request_timeout: Duration,
    ) -> Result<Value> {
        let _request_guard = self.request_lock.lock().await;
        let stdin = self.stdin.as_ref().with_context(|| {
            format!(
                "MCP server '{}' is not connected (stdin unavailable)",
                self.name
            )
        })?;
        let stdout = self.stdout.as_ref().with_context(|| {
            format!(
                "MCP server '{}' is not connected (stdout unavailable)",
                self.name
            )
        })?;

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

        // A server may emit notifications before the response. Read until the
        // matching request ID arrives, while keeping the whole exchange locked.
        let response = timeout(request_timeout, async {
            loop {
                let mut line = String::new();
                let bytes = {
                    let mut stdout_guard = stdout.lock().await;
                    stdout_guard.read_line(&mut line).await?
                };
                if bytes == 0 {
                    anyhow::bail!("MCP server '{}' closed stdout", self.name);
                }

                tracing::debug!("MCP response: {}", line.trim());
                let value: Value = serde_json::from_str(&line)
                    .context("Failed to parse JSON-RPC message from MCP server")?;
                if value.get("id").is_none() {
                    tracing::debug!("Ignoring MCP notification while awaiting request {id}");
                    continue;
                }
                let response: JsonRpcResponse =
                    serde_json::from_value(value).context("Failed to parse JSON-RPC response")?;
                if response.id != id {
                    tracing::debug!(
                        "Ignoring MCP response {} while awaiting {} from '{}'",
                        response.id,
                        id,
                        self.name
                    );
                    continue;
                }
                break Ok::<JsonRpcResponse, anyhow::Error>(response);
            }
        })
        .await
        .with_context(|| {
            format!(
                "MCP server '{}' timed out after {} seconds calling '{}'",
                self.name,
                request_timeout.as_secs_f32(),
                method
            )
        })??;

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
        let _request_guard = self.request_lock.lock().await;
        let stdin = self.stdin.as_ref().with_context(|| {
            format!(
                "MCP server '{}' is not connected (stdin unavailable)",
                self.name
            )
        })?;

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

fn with_protocol_metadata(params: Option<Value>, version: &str) -> Value {
    let mut params = match params {
        Some(Value::Object(params)) => params,
        Some(value) => {
            let mut params = serde_json::Map::new();
            params.insert("value".to_string(), value);
            params
        }
        None => serde_json::Map::new(),
    };
    params.insert(
        "_meta".to_string(),
        serde_json::json!({
            "io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientInfo": {
                "name": "finch",
                "version": env!("CARGO_PKG_VERSION")
            },
            "io.modelcontextprotocol/clientCapabilities": {}
        }),
    );
    Value::Object(params)
}

/// Expand exact `$NAME` and `${NAME}` references without invoking a shell.
/// Literal values are passed through unchanged.
fn resolve_environment(
    configured: &std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>> {
    configured
        .iter()
        .map(|(key, value)| {
            let variable = value
                .strip_prefix("${")
                .and_then(|rest| rest.strip_suffix('}'))
                .or_else(|| {
                    value
                        .strip_prefix('$')
                        .filter(|name| !name.is_empty() && !name.contains('$'))
                });
            let resolved = match variable {
                Some(name) => std::env::var(name).with_context(|| {
                    format!("environment variable '{name}' referenced by '{key}' is not set")
                })?,
                None => value.clone(),
            };
            Ok((key.clone(), resolved))
        })
        .collect()
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
        let response_json =
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32600,"message":"Invalid request"}}"#;
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
            timeout_secs: 300,
        };

        let result = McpConnection::connect("test".to_string(), &config).await;
        assert!(result.is_err());
    }

    #[test]
    fn resolves_literal_environment_values() {
        let configured = HashMap::from([("TOKEN".to_string(), "literal".to_string())]);
        let resolved = resolve_environment(&configured).unwrap();
        assert_eq!(resolved.get("TOKEN").map(String::as_str), Some("literal"));
    }

    #[test]
    fn rejects_missing_environment_reference() {
        let configured = HashMap::from([(
            "TOKEN".to_string(),
            "$FINCH_TEST_MISSING_MCP_TOKEN_82B7".to_string(),
        )]);
        assert!(resolve_environment(&configured).is_err());
    }
}
