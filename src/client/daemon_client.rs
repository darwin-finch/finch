// Daemon client implementation
//
// HTTP client that communicates with the Shammah daemon.
// Automatically spawns daemon if not running.

use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;
use tracing::{debug, error, info};

use crate::claude::{ContentBlock, Message};
use crate::daemon::ensure_daemon_running;
use crate::server::openai_types::{
    ChatCompletionRequest, ChatCompletionResponse, ChatMessage, Tool, FunctionDefinition,
};
use crate::tools::types::{ToolDefinition, ToolUse};
use crate::tools::executor::ToolExecutor;

/// Configuration for daemon connection
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Daemon bind address (e.g., "127.0.0.1:11434")
    pub bind_address: String,
    /// Whether to auto-spawn daemon if not running
    pub auto_spawn: bool,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:11435".to_string(), // Port 11435 (11434 is used by Ollama)
            auto_spawn: true,
            timeout_seconds: 120,
        }
    }
}

impl DaemonConfig {
    /// Create DaemonConfig from ClientConfig settings
    pub fn from_client_config(client_config: &crate::config::ClientConfig) -> Self {
        Self {
            bind_address: client_config.daemon_address.clone(),
            auto_spawn: client_config.auto_spawn,
            timeout_seconds: client_config.timeout_seconds,
        }
    }
}

/// HTTP client for communicating with Shammah daemon
pub struct DaemonClient {
    base_url: String,
    client: Client,
    config: DaemonConfig,
}

impl DaemonClient {
    /// Create a new daemon client and ensure daemon is running
    pub async fn connect(config: DaemonConfig) -> Result<Self> {
        let base_url = format!("http://{}", config.bind_address);

        // Ensure daemon is running (auto-spawn if enabled)
        if config.auto_spawn {
            ensure_daemon_running(Some(&config.bind_address))
                .await
                .context("Failed to ensure daemon is running")?;
        } else {
            // Just check if daemon is reachable
            Self::check_health(&base_url).await?;
        }

        let client = Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds))
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(0)  // Disable connection pooling
            .build()
            .context("Failed to build HTTP client")?;

        info!(base_url = %base_url, "Connected to daemon");

        Ok(Self {
            base_url,
            client,
            config,
        })
    }

    /// Create a client with default configuration
    pub async fn connect_default() -> Result<Self> {
        Self::connect(DaemonConfig::default()).await
    }

    /// Create config from ClientConfig settings (convenience method)
    pub fn config_from_settings(client_config: &crate::config::ClientConfig) -> DaemonConfig {
        DaemonConfig::from_client_config(client_config)
    }

    /// Send a query to the daemon using OpenAI-compatible API
    ///
    /// This is the main method for CLI to send queries.
    pub async fn query(&self, messages: Vec<Message>) -> Result<String> {
        // Convert internal messages to OpenAI format
        let openai_messages: Vec<ChatMessage> = messages
            .into_iter()
            .map(|m| {
                let content = m
                    .content
                    .into_iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");

                ChatMessage {
                    role: m.role,
                    content: Some(content),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                }
            })
            .collect();

        // Build request
        let request = ChatCompletionRequest {
            model: "qwen-local".to_string(),
            messages: openai_messages,
            max_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stream: false,
            stop: None,
            tools: None,
            local_only: None,
        };

        // Send to daemon
        let url = format!("{}/v1/chat/completions", self.base_url);
        debug!(url = %url, "Sending chat completion request");

        let response: ChatCompletionResponse = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                // Log detailed error info
                error!("HTTP request failed: {}", e);
                if e.is_timeout() {
                    error!("  → Error type: TIMEOUT");
                } else if e.is_connect() {
                    error!("  → Error type: CONNECTION");
                } else if e.is_request() {
                    error!("  → Error type: REQUEST");
                } else if e.is_body() {
                    error!("  → Error type: BODY");
                } else {
                    error!("  → Error type: OTHER");
                }
                anyhow::anyhow!("Failed to send request to daemon: {}", e)
            })?
            .json()
            .await
            .context("Failed to parse response from daemon")?;

        // Extract response text
        let response_text = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.clone())
            .unwrap_or_else(|| "No response from model".to_string());

        Ok(response_text)
    }

    /// Send a simple text query (convenience method)
    pub async fn query_text(&self, query: &str) -> Result<String> {
        let messages = vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: query.to_string(),
            }],
        }];

        self.query(messages).await
    }

    /// Query with tool execution loop
    ///
    /// Handles the full tool execution flow:
    /// 1. Send query with tools
    /// 2. If response has tool_calls, execute them locally
    /// 3. Send tool results back
    /// 4. Repeat until final answer
    pub async fn query_with_tools(
        &self,
        initial_query: &str,
        tools: Vec<ToolDefinition>,
        tool_executor: &ToolExecutor,
    ) -> Result<String> {
        let mut messages = vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: initial_query.to_string(),
            }],
        }];

        const MAX_TURNS: usize = 10;
        let mut turn = 0;

        loop {
            if turn >= MAX_TURNS {
                anyhow::bail!("Max tool execution turns reached ({})", MAX_TURNS);
            }
            turn += 1;

            // Convert to OpenAI format
            let openai_messages = Self::convert_to_openai_messages(&messages);
            let openai_tools = Self::convert_to_openai_tools(&tools);

            // Send request
            let request = ChatCompletionRequest {
                model: "qwen-local".to_string(),
                messages: openai_messages,
                tools: Some(openai_tools),
                max_tokens: None,
                temperature: None,
                top_p: None,
                n: None,
                stream: false,
                stop: None,
                local_only: None,
            };

            let url = format!("{}/v1/chat/completions", self.base_url);
            debug!(url = %url, turn, "Sending chat completion request with tools");

            let response: ChatCompletionResponse = self
                .client
                .post(&url)
                .json(&request)
                .send()
                .await
                .context("Failed to send request to daemon")?
                .json()
                .await
                .context("Failed to parse response from daemon")?;

            let choice = response
                .choices
                .first()
                .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;

            // Check if tools were called
            if let Some(tool_calls) = &choice.message.tool_calls {
                info!("Received {} tool call(s) from daemon", tool_calls.len());

                // Add assistant message with tool calls
                let mut tool_use_blocks = Vec::new();
                for tc in tool_calls {
                    let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)?;
                    tool_use_blocks.push(ContentBlock::ToolUse {
                        id: tc.id.clone(),
                        name: tc.function.name.clone(),
                        input,
                    });
                }

                messages.push(Message {
                    role: "assistant".to_string(),
                    content: tool_use_blocks.clone(),
                });

                // Execute tools locally
                let mut tool_result_blocks = Vec::new();
                for block in tool_use_blocks {
                    if let ContentBlock::ToolUse { id, name, input } = block {
                        info!("Executing tool locally: {} ({})", name, id);

                        // Execute tool
                        let tool_use = ToolUse { id: id.clone(), name, input };

                        let result = tool_executor
                            .execute_tool::<fn() -> Result<()>>(&tool_use, None, None, None, None, None, None, None)
                            .await?;

                        tool_result_blocks.push(ContentBlock::ToolResult {
                            tool_use_id: id,
                            content: result.content,
                            is_error: Some(result.is_error),
                        });
                    }
                }

                // Add tool results to conversation
                messages.push(Message {
                    role: "user".to_string(),
                    content: tool_result_blocks,
                });

                // Continue loop with updated conversation
                continue;
            }

            // No tool calls, return final answer
            if let Some(content) = &choice.message.content {
                return Ok(content.clone());
            }

            anyhow::bail!("Response has no content and no tool calls");
        }
    }

    /// Convert internal messages to OpenAI format
    fn convert_to_openai_messages(messages: &[Message]) -> Vec<ChatMessage> {
        messages
            .iter()
            .map(|m| {
                let mut openai_msg = ChatMessage {
                    role: m.role.clone(),
                    content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                };

                let mut text_parts = Vec::new();
                let mut tool_calls = Vec::new();

                for block in &m.content {
                    match block {
                        ContentBlock::Text { text } => {
                            text_parts.push(text.clone());
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(crate::server::openai_types::ToolCall {
                                id: id.clone(),
                                tool_type: "function".to_string(),
                                function: crate::server::openai_types::FunctionCall {
                                    name: name.clone(),
                                    arguments: serde_json::to_string(input).unwrap_or_default(),
                                },
                            });
                        }
                        ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                            // For tool results, create a separate message with role "tool"
                            // But for simplicity in batching, we'll encode it as text for now
                            // This is a limitation - proper implementation would split messages
                            text_parts.push(format!("[Tool Result for {}]: {}", tool_use_id, content));
                        }
                        ContentBlock::Image { .. } => {
                            // Image blocks: not applicable in this context
                        }
                    }
                }

                if !text_parts.is_empty() {
                    openai_msg.content = Some(text_parts.join("\n"));
                }

                if !tool_calls.is_empty() {
                    openai_msg.tool_calls = Some(tool_calls);
                }

                openai_msg
            })
            .collect()
    }

    /// Convert internal tools to OpenAI format
    fn convert_to_openai_tools(tools: &[ToolDefinition]) -> Vec<Tool> {
        tools
            .iter()
            .map(|t| Tool {
                tool_type: "function".to_string(),
                function: FunctionDefinition {
                    name: t.name.clone(),
                    description: Some(t.description.clone()),
                    parameters: t.input_schema.properties.clone(),
                },
            })
            .collect()
    }

    /// Check daemon health
    pub async fn check_health_status(&self) -> Result<serde_json::Value> {
        let url = format!("{}/health", self.base_url);
        let response = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(30))  // Increased from 5 to 30 seconds
            .send()
            .await
            .context("Failed to check daemon health")?
            .json()
            .await
            .context("Failed to parse health response")?;

        Ok(response)
    }

    /// Internal health check (used during connection)
    async fn check_health(base_url: &str) -> Result<()> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))  // Increased from 5 to 30 seconds
            .build()
            .context("Failed to build HTTP client")?;

        let url = format!("{}/health", base_url);
        let response = client
            .get(&url)
            .send()
            .await
            .context("Daemon is not reachable")?;

        if !response.status().is_success() {
            anyhow::bail!("Daemon health check failed: {}", response.status());
        }

        Ok(())
    }

    /// Get base URL
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Get configuration
    pub fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// Query local model directly, bypassing routing
    ///
    /// This sends a request with local_only=true to bypass crisis detection
    /// and threshold routing, going directly to the local model.
    /// Returns an error if the model is not ready or generation fails.
    pub async fn query_local_only(&self, query: &str) -> Result<String> {
        use reqwest::StatusCode;

        let request = ChatCompletionRequest {
            model: "qwen-local".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(query.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            max_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stream: false,
            stop: None,
            tools: None,
            local_only: Some(true), // KEY: Bypass routing
        };

        let url = format!("{}/v1/chat/completions", self.base_url);
        debug!(url = %url, "Sending local-only query");

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to send request to daemon")?;

        match response.status() {
            StatusCode::OK => {
                let completion: ChatCompletionResponse = response
                    .json()
                    .await
                    .context("Failed to parse response from daemon")?;

                let content = completion
                    .choices
                    .first()
                    .and_then(|choice| choice.message.content.clone())
                    .unwrap_or_else(|| "No response from model".to_string());

                Ok(content)
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                anyhow::bail!("Local model not ready (initializing/downloading/loading)")
            }
            StatusCode::NOT_IMPLEMENTED => {
                anyhow::bail!("Local model not available")
            }
            StatusCode::INTERNAL_SERVER_ERROR => {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!("Local model generation failed: {}", error_text)
            }
            status => {
                let error_text = response.text().await.unwrap_or_default();
                anyhow::bail!("Daemon error ({}): {}", status, error_text)
            }
        }
    }

    /// Query with automatic crash recovery
    ///
    /// Attempts to send query, and if connection fails (daemon crashed),
    /// automatically restarts the daemon and retries once.
    ///
    /// # Arguments
    /// * `messages` - Conversation messages to send
    ///
    /// # Returns
    /// * `Ok(String)` - Response text from model
    /// * `Err` - Error if both attempts fail
    pub async fn query_with_recovery(&self, messages: Vec<Message>) -> Result<String> {
        // First attempt
        match self.query(messages.clone()).await {
            Ok(response) => Ok(response),
            Err(e) => {
                // Check if error is connection-related (daemon crash)
                let error_str = e.to_string().to_lowercase();
                let is_connection_error = error_str.contains("connection refused")
                    || error_str.contains("connection reset")
                    || error_str.contains("broken pipe")
                    || error_str.contains("failed to send request");

                if is_connection_error && self.config.auto_spawn {
                    info!("Daemon connection failed, attempting auto-restart...");

                    // Try to restart daemon
                    match ensure_daemon_running(Some(&self.config.bind_address)).await {
                        Ok(_) => {
                            info!("Daemon restarted successfully, retrying query...");

                            // Wait a moment for daemon to fully initialize
                            tokio::time::sleep(Duration::from_millis(500)).await;

                            // Retry query once
                            self.query(messages).await.context(
                                "Query failed after daemon restart. Original error: {}",
                            )
                        }
                        Err(restart_err) => {
                            anyhow::bail!(
                                "Failed to restart daemon: {}. Original query error: {}",
                                restart_err,
                                e
                            )
                        }
                    }
                } else {
                    // Not a connection error, or auto-spawn disabled
                    Err(e)
                }
            }
        }
    }

    /// Query local model with streaming and callback for UI updates
    ///
    /// Similar to query_local_only_streaming but calls a callback for each token
    /// as it arrives, enabling real-time UI updates.
    ///
    /// # Arguments
    /// * `query` - Text query to send
    /// * `token_callback` - Called for each token as it arrives
    ///
    /// # Returns
    /// * `Ok(String)` - Complete response text from local model
    /// * `Err` - Error if model not ready or generation fails
    pub async fn query_local_only_streaming_with_callback<F>(
        &self,
        query: &str,
        mut token_callback: F,
    ) -> Result<String>
    where
        F: FnMut(&str) + Send,
    {
        use futures::StreamExt;
        use reqwest::StatusCode;

        let request = ChatCompletionRequest {
            model: "qwen-local".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(query.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            max_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stream: true, // Enable streaming
            stop: None,
            tools: None,
            local_only: Some(true), // Bypass routing
        };

        let url = format!("{}/v1/chat/completions", self.base_url);
        debug!(url = %url, "Sending streaming local-only query with callback");

        let response = self
            .client
            .post(&url)
            .json(&request)
            .timeout(Duration::from_secs(300))  // 5 minute timeout for streaming
            .send()
            .await
            .context("Failed to send streaming request to daemon")?;

        match response.status() {
            StatusCode::OK => {
                // Parse SSE stream
                let mut stream = response.bytes_stream();
                let mut accumulated_content = String::new();
                let mut buffer = Vec::new();

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result.context("Failed to read streaming chunk")?;
                    buffer.extend_from_slice(&chunk);

                    // Process complete SSE events
                    // SSE format: "data: {...}\n\n"
                    // Look for double newline separator
                    loop {
                        let separator_pos = buffer.windows(2)
                            .position(|w| w == b"\n\n")
                            .or_else(|| buffer.windows(4).position(|w| w == b"\r\n\r\n"));

                        if let Some(pos) = separator_pos {
                            // Extract event (including separator for proper draining)
                            let drain_len = if buffer[pos..].starts_with(b"\r\n\r\n") {
                                pos + 4
                            } else {
                                pos + 2
                            };

                            let event_bytes: Vec<u8> = buffer.drain(..drain_len).collect();
                            let event_str = String::from_utf8_lossy(&event_bytes);

                            for line in event_str.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data.trim() == "[DONE]" {
                                        break;
                                    }

                                    // Parse JSON chunk
                                    if let Ok(chunk_json) = serde_json::from_str::<serde_json::Value>(data) {
                                        if let Some(choices) = chunk_json["choices"].as_array() {
                                            if let Some(first_choice) = choices.first() {
                                                if let Some(delta) = first_choice["delta"].as_object() {
                                                    // Safely check for "content" key (final chunk has empty delta)
                                                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                                        accumulated_content.push_str(content);
                                                        // Call callback for UI update
                                                        token_callback(content);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // No complete event yet, wait for more data
                            break;
                        }
                    }
                }

                if accumulated_content.is_empty() {
                    anyhow::bail!("No content received from streaming response")
                } else {
                    Ok(accumulated_content)
                }
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                anyhow::bail!("Local model not ready (initializing/downloading/loading)")
            }
            StatusCode::INTERNAL_SERVER_ERROR => {
                let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                anyhow::bail!("Local generation failed: {}", error_body)
            }
            StatusCode::NOT_IMPLEMENTED => {
                anyhow::bail!("Local model not available")
            }
            status => {
                let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                anyhow::bail!("Unexpected status {}: {}", status, error_body)
            }
        }
    }

    /// Query local model with streaming (SSE)
    ///
    /// This sends a streaming request with local_only=true to bypass routing.
    /// Tokens are received via Server-Sent Events and accumulated into the final response.
    /// This keeps the HTTP connection alive during long generations, preventing timeouts.
    ///
    /// # Arguments
    /// * `query` - Text query to send
    ///
    /// # Returns
    /// * `Ok(String)` - Complete response text from local model
    /// * `Err` - Error if model not ready or generation fails
    pub async fn query_local_only_streaming(&self, query: &str) -> Result<String> {
        use futures::StreamExt;
        use reqwest::StatusCode;

        let request = ChatCompletionRequest {
            model: "qwen-local".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: Some(query.to_string()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            max_tokens: None,
            temperature: None,
            top_p: None,
            n: None,
            stream: true, // Enable streaming
            stop: None,
            tools: None,
            local_only: Some(true), // Bypass routing
        };

        let url = format!("{}/v1/chat/completions", self.base_url);
        debug!(url = %url, "Sending streaming local-only query");

        let response = self
            .client
            .post(&url)
            .json(&request)
            .timeout(Duration::from_secs(300))  // 5 minute timeout for streaming (per chunk, not total)
            .send()
            .await
            .context("Failed to send streaming request to daemon")?;

        match response.status() {
            StatusCode::OK => {
                // Parse SSE stream
                let mut stream = response.bytes_stream();
                let mut accumulated_content = String::new();
                let mut buffer = Vec::new();

                while let Some(chunk_result) = stream.next().await {
                    let chunk = chunk_result.context("Failed to read streaming chunk")?;
                    buffer.extend_from_slice(&chunk);

                    // Process complete SSE events
                    // SSE format: "data: {...}\n\n"
                    // Look for double newline separator
                    loop {
                        let separator_pos = buffer.windows(2)
                            .position(|w| w == b"\n\n")
                            .or_else(|| buffer.windows(4).position(|w| w == b"\r\n\r\n"));

                        if let Some(pos) = separator_pos {
                            // Extract event (including separator for proper draining)
                            let drain_len = if buffer[pos..].starts_with(b"\r\n\r\n") {
                                pos + 4
                            } else {
                                pos + 2
                            };

                            let event_bytes: Vec<u8> = buffer.drain(..drain_len).collect();
                            let event_str = String::from_utf8_lossy(&event_bytes);

                            for line in event_str.lines() {
                                if let Some(data) = line.strip_prefix("data: ") {
                                    if data.trim() == "[DONE]" {
                                        break;
                                    }

                                    // Parse JSON chunk
                                    if let Ok(chunk_json) = serde_json::from_str::<serde_json::Value>(data) {
                                        if let Some(choices) = chunk_json["choices"].as_array() {
                                            if let Some(first_choice) = choices.first() {
                                                if let Some(delta) = first_choice["delta"].as_object() {
                                                    // Safely check for "content" key (final chunk has empty delta)
                                                    if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                                                        accumulated_content.push_str(content);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        } else {
                            // No complete event yet, wait for more data
                            break;
                        }
                    }
                }

                if accumulated_content.is_empty() {
                    anyhow::bail!("No content received from streaming response")
                } else {
                    Ok(accumulated_content)
                }
            }
            StatusCode::SERVICE_UNAVAILABLE => {
                anyhow::bail!("Local model not ready (initializing/downloading/loading)")
            }
            StatusCode::INTERNAL_SERVER_ERROR => {
                let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                anyhow::bail!("Local generation failed: {}", error_body)
            }
            StatusCode::NOT_IMPLEMENTED => {
                anyhow::bail!("Local model not available")
            }
            status => {
                let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
                anyhow::bail!("Unexpected status {}: {}", status, error_body)
            }
        }
    }

    /// Query local model only with automatic crash recovery
    ///
    /// Same as query_local_only but with automatic daemon restart on connection failure.
    ///
    /// # Arguments
    /// * `query` - Text query to send
    ///
    /// # Returns
    /// * `Ok(String)` - Response text from local model
    /// * `Err` - Error if both attempts fail or model not ready
    pub async fn query_local_only_with_recovery(&self, query: &str) -> Result<String> {
        // First attempt (use streaming to avoid timeouts)
        match self.query_local_only_streaming(query).await {
            Ok(response) => Ok(response),
            Err(e) => {
                // Check if error is connection-related (daemon crash)
                let error_str = e.to_string().to_lowercase();
                let is_connection_error = error_str.contains("connection refused")
                    || error_str.contains("connection reset")
                    || error_str.contains("broken pipe")
                    || error_str.contains("failed to send request");

                if is_connection_error && self.config.auto_spawn {
                    info!("Daemon connection failed, attempting auto-restart...");

                    // Try to restart daemon
                    match ensure_daemon_running(Some(&self.config.bind_address)).await {
                        Ok(_) => {
                            info!("Daemon restarted successfully, retrying query...");

                            // Wait a moment for daemon to fully initialize
                            tokio::time::sleep(Duration::from_millis(500)).await;

                            // Retry query once (with streaming)
                            self.query_local_only_streaming(query).await.context(
                                "Streaming query failed after daemon restart",
                            )
                        }
                        Err(restart_err) => {
                            anyhow::bail!(
                                "Failed to restart daemon: {}. Original query error: {}",
                                restart_err,
                                e
                            )
                        }
                    }
                } else {
                    // Not a connection error, or auto-spawn disabled
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.bind_address, "127.0.0.1:11435"); // Port 11435 to avoid Ollama conflict
        assert!(config.auto_spawn);
        assert_eq!(config.timeout_seconds, 120);
    }
}
