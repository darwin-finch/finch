// Claude API provider implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use super::endpoints::ProviderEndpoints;
use super::types::{
    CapabilitySupport, ModelCapabilities, ProviderRequest, ProviderResponse, StreamChunk,
    WireProtocol,
};
use super::{LlmProvider, ProviderBackend, ReasoningCapability, ValidatedProviderRequest};
use crate::claude::retry::{with_retry, NonRetriableError};
use crate::claude::streaming::StreamEvent;
use crate::claude::types::{ContentBlock, MessageRequest};
use crate::config::constants::DEFAULT_CLAUDE_MODEL;

const CLAUDE_API_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT_SECS: u64 = 120;

/// Parse an Anthropic API error body and return a human-friendly message with hints.
fn friendly_api_error(status: reqwest::StatusCode, body: &str) -> String {
    // Anthropic errors look like: {"type":"error","error":{"type":"...","message":"..."}}
    let extracted = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        });

    let msg = extracted.as_deref().unwrap_or(body.trim());

    let hint = match status.as_u16() {
        401 => " — Check that your ANTHROPIC_API_KEY or api_key in ~/.finch/config.toml is correct",
        403 => " — Your API key may lack permissions",
        429 => " — You've hit a rate limit; wait a moment before retrying",
        400 => " — The request was malformed (this may be a finch bug; please report it)",
        404 => " — Model not found; check the model name in your config",
        500 | 502 | 503 => " — Anthropic is having issues; try again in a moment",
        _ => "",
    };

    format!("Claude API error {}{}: {}", status, hint, msg)
}

/// Helper struct for building blocks during streaming
struct BlockBuilder {
    block_type: String,
    id: Option<String>,
    name: Option<String>,
    accumulated: String,
}

/// Claude API provider
///
/// Implements the LlmProvider trait for Anthropic's Claude API.
#[derive(Clone)]
pub struct ClaudeProvider {
    client: Client,
    api_key: String,
    default_model: String,
    endpoints: ProviderEndpoints,
}

impl ClaudeProvider {
    /// Create a new Claude provider
    pub fn new(api_key: String) -> Result<Self> {
        Self::new_with_endpoints(api_key, CLAUDE_API_BASE_URL, "/v1/messages", "/v1/models")
    }

    /// Create a Claude provider with endpoint paths relative to `base_url` or
    /// complete endpoint URLs.
    pub fn new_with_endpoints(
        api_key: String,
        base_url: &str,
        chat_path: &str,
        models_path: &str,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            api_key,
            default_model: DEFAULT_CLAUDE_MODEL.to_string(),
            endpoints: ProviderEndpoints::new(base_url, chat_path, models_path),
        })
    }

    /// Create with custom default model
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Convert ProviderRequest to Claude's MessageRequest format
    fn to_message_request(&self, request: &ProviderRequest) -> MessageRequest {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        MessageRequest {
            model,
            max_tokens: request.max_tokens,
            messages: request.messages.clone(),
            system: request.system.clone(),
            tools: request.tools.clone(),
        }
    }

    /// Send a single message request (no retry)
    async fn send_message_once(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let msg_request = self.to_message_request(request);

        tracing::debug!(
            model = %msg_request.model,
            messages = msg_request.messages.len(),
            tools = msg_request.tools.as_ref().map_or(0, Vec::len),
            "sending Claude request"
        );

        let response = self
            .client
            .post(&self.endpoints.chat_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&msg_request)
            .send()
            .await
            .context("Failed to send request to Claude API")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let msg = friendly_api_error(status, &error_body);
            if status.is_client_error() {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        let message_response: crate::claude::types::MessageResponse = response
            .json()
            .await
            .context("Failed to parse Claude API response")?;

        tracing::debug!(
            response_id = %message_response.id,
            model = %message_response.model,
            blocks = message_response.content.len(),
            stop_reason = ?message_response.stop_reason,
            "received Claude response"
        );

        // Convert to ProviderResponse
        Ok(ProviderResponse {
            id: message_response.id,
            model: message_response.model,
            content: message_response.content,
            stop_reason: message_response.stop_reason,
            role: message_response.role,
            provider: "claude".to_string(),
        })
    }

    /// Send a message with streaming response (no retry)
    async fn send_message_stream_once(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::channel(100);

        let msg_request = self.to_message_request(request);

        // Convert to JSON and add stream: true
        let mut request_json = serde_json::to_value(&msg_request)?;
        request_json["stream"] = serde_json::json!(true);

        tracing::debug!("Sending streaming request to Claude API");

        let response = self
            .client
            .post(&self.endpoints.chat_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&request_json)
            .send()
            .await
            .context("Failed to send streaming request to Claude API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            let msg = friendly_api_error(status, &error_body);
            if status.is_client_error() {
                return Err(anyhow::Error::new(NonRetriableError(msg)));
            }
            anyhow::bail!("{}", msg);
        }

        // Spawn task to parse SSE stream with block tracking
        tokio::spawn(async move {
            tracing::debug!("[STREAM] Streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();

            // Track blocks being built (index -> BlockBuilder)
            let mut blocks: HashMap<usize, BlockBuilder> = HashMap::new();
            let mut message_stop_seen = false;
            let mut transport_end_seen = false;
            let mut receiver_closed = false;
            let mut stream_failed = false;

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);

                        // Parse line by line
                        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes);

                            // SSE format: "data: {...}\n"
                            if let Some(json_str) = line.strip_prefix("data: ") {
                                let json_str = json_str.trim();

                                // Check for end marker
                                if json_str == "[DONE]" {
                                    if !blocks.is_empty() {
                                        let _ = tx
                                            .send(Err(anyhow::anyhow!(
                                                "Claude SSE terminated with open content blocks"
                                            )))
                                            .await;
                                        return;
                                    }
                                    if !message_stop_seen {
                                        let _ = tx
                                            .send(Err(anyhow::anyhow!(
                                                "Claude SSE [DONE] arrived before message_stop"
                                            )))
                                            .await;
                                        return;
                                    }
                                    transport_end_seen = true;
                                    continue;
                                }
                                if transport_end_seen || message_stop_seen {
                                    let _ = tx
                                        .send(Err(anyhow::anyhow!(
                                            "Claude SSE emitted data after its terminal event"
                                        )))
                                        .await;
                                    return;
                                }

                                // Parse event
                                if let Ok(event) = serde_json::from_str::<StreamEvent>(json_str) {
                                    tracing::debug!("Stream event: {}", event.event_type);
                                    match event.event_type.as_str() {
                                        "message_start" => {
                                            // Extract input_tokens from message.usage
                                            if let Ok(v) =
                                                serde_json::from_str::<serde_json::Value>(json_str)
                                            {
                                                if let Some(n) = v
                                                    .get("message")
                                                    .and_then(|m| m.get("usage"))
                                                    .and_then(|u| u.get("input_tokens"))
                                                    .and_then(|t| t.as_u64())
                                                {
                                                    let _ = tx
                                                        .send(Ok(StreamChunk::Usage {
                                                            input_tokens: n as u32,
                                                        }))
                                                        .await;
                                                }
                                            }
                                        }

                                        "content_block_start" => {
                                            if let Some(cb) = event.content_block {
                                                let index = event.index.unwrap_or(0);
                                                blocks.insert(
                                                    index,
                                                    BlockBuilder {
                                                        block_type: cb.block_type,
                                                        id: cb.id,
                                                        name: cb.name,
                                                        accumulated: String::new(),
                                                    },
                                                );
                                                tracing::debug!(
                                                    "Started block {} type {}",
                                                    index,
                                                    blocks[&index].block_type
                                                );
                                            }
                                        }

                                        "content_block_delta" => {
                                            let index = event.index.unwrap_or(0);
                                            if let Some(builder) = blocks.get_mut(&index) {
                                                if let Some(delta) = event.delta {
                                                    match delta.delta_type.as_str() {
                                                        "text_delta" => {
                                                            if let Some(text) = delta.text {
                                                                builder.accumulated.push_str(&text);
                                                                // Only stream visible text — skip
                                                                // extended thinking blocks so the
                                                                // model's chain-of-thought never
                                                                // leaks into the TUI output.
                                                                if builder.block_type != "thinking"
                                                                    && tx
                                                                        .send(Ok(
                                                                            StreamChunk::TextDelta(
                                                                                text,
                                                                            ),
                                                                        ))
                                                                        .await
                                                                        .is_err()
                                                                {
                                                                    // Receiver dropped, stop streaming
                                                                    receiver_closed = true;
                                                                    break;
                                                                }
                                                            }
                                                        }
                                                        "input_json_delta" => {
                                                            if let Some(json) = delta.partial_json {
                                                                builder.accumulated.push_str(&json);
                                                            }
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }

                                        "content_block_stop" => {
                                            let index = event.index.unwrap_or(0);
                                            if let Some(builder) = blocks.remove(&index) {
                                                let block = match builder.block_type.as_str() {
                                                    "text" => ContentBlock::Text {
                                                        text: builder.accumulated,
                                                    },
                                                    "tool_use" => {
                                                        let input = serde_json::from_str(
                                                            &builder.accumulated,
                                                        )
                                                        .unwrap_or(serde_json::json!({}));
                                                        ContentBlock::ToolUse {
                                                            id: builder.id.unwrap_or_default(),
                                                            name: builder.name.unwrap_or_default(),
                                                            input,
                                                        }
                                                    }
                                                    _ => continue,
                                                };

                                                tracing::debug!(
                                                    "Completed block {} type {}",
                                                    index,
                                                    builder.block_type
                                                );

                                                if tx
                                                    .send(Ok(StreamChunk::ContentBlockComplete(
                                                        block,
                                                    )))
                                                    .await
                                                    .is_err()
                                                {
                                                    // Receiver dropped, stop streaming
                                                    receiver_closed = true;
                                                    break;
                                                }
                                            }
                                        }

                                        "message_stop" => {
                                            if !blocks.is_empty() {
                                                let _ = tx
                                                    .send(Err(anyhow::anyhow!(
                                                        "Claude SSE message_stop arrived with open content blocks"
                                                    )))
                                                    .await;
                                                return;
                                            }
                                            message_stop_seen = true;
                                        }

                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Stream error: {}", e);
                        let _ = tx.send(Err(e.into())).await;
                        stream_failed = true;
                        break;
                    }
                }
                if receiver_closed {
                    break;
                }
            }

            if !receiver_closed && !stream_failed {
                if !buffer.iter().all(|byte| byte.is_ascii_whitespace()) {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "Claude SSE ended with an incomplete frame"
                        )))
                        .await;
                } else if !message_stop_seen {
                    let _ = tx
                        .send(Err(anyhow::anyhow!(
                            "Claude SSE ended before a terminal message_stop"
                        )))
                        .await;
                }
            }

            tracing::debug!("[STREAM] Exited chunk loop, task finishing");
        });

        Ok(rx)
    }
}

#[async_trait]
impl ProviderBackend for ClaudeProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let request = request.into_request_for(self)?;
        with_retry(|| self.send_message_once(&request)).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let request = request.into_request_for(self)?;
        with_retry(|| self.send_message_stream_once(&request)).await
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        if self.endpoints.chat_url != "https://api.anthropic.com/v1/messages"
            || self.endpoints.models_url != "https://api.anthropic.com/v1/models"
        {
            return ModelCapabilities::unknown(self.name(), model);
        }
        if model != DEFAULT_CLAUDE_MODEL {
            return ModelCapabilities::unknown(self.name(), model);
        }
        ModelCapabilities::static_metadata(
            self.name(),
            model,
            "2026-08-26",
            "https://platform.claude.com/docs/en/models/sonnet-5/whats-new-sonnet-5; https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview",
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-08-26", "Finch Claude adapter"),
            Some(1_000_000),
            Some(128_000),
            None,
        )
        .with_wire_protocol(
            WireProtocol::AnthropicMessages,
            "2026-08-26",
            "Finch Anthropic Messages adapter",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn raw_sse_eof_after_tool_block_is_an_error_not_a_completion() {
        let mut server = mockito::Server::new_async().await;
        let body = concat!(
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-A\",\"name\":\"Read\"}}\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{}\"}}\n",
            "data: {\"type\":\"content_block_stop\",\"index\":0}\n",
        );
        let mock = server
            .mock("POST", "/v1/messages")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(body)
            .create_async()
            .await;
        let provider = ClaudeProvider::new_with_endpoints(
            "test-key".to_string(),
            &server.url(),
            "/v1/messages",
            "/v1/models",
        )
        .unwrap();
        let mut stream = provider
            .send_message_stream_once(&ProviderRequest::new(vec![crate::claude::Message::user(
                "inspect",
            )]))
            .await
            .unwrap();
        let mut tool_blocks = 0;
        let mut terminal_error = None;
        while let Some(item) = stream.recv().await {
            match item {
                Ok(StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { .. })) => {
                    tool_blocks += 1;
                }
                Err(error) => terminal_error = Some(error.to_string()),
                _ => {}
            }
        }

        assert_eq!(
            tool_blocks, 1,
            "fixture crosses the production tool boundary"
        );
        assert_eq!(
            terminal_error.as_deref(),
            Some("Claude SSE ended before a terminal message_stop")
        );
        mock.assert_async().await;
    }

    #[test]
    fn provider_request_boundary_observes_only_complete_tool_pairs() {
        use crate::claude::{ContentBlock, Message};
        use crate::cli::ConversationHistory;

        let provider = ClaudeProvider::new("test-key".to_string()).unwrap();
        let query_id = uuid::Uuid::new_v4();
        let mut history = ConversationHistory::new();
        history.add_user_message("inspect".to_string());
        let token = history
            .stage_assistant(
                query_id,
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::ToolUse {
                        id: "call-A".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"path": "A"}),
                    }],
                },
            )
            .unwrap();

        let staged = provider.to_message_request(&ProviderRequest::new(history.get_messages()));
        assert_eq!(staged.messages.len(), 1);
        assert!(staged.messages.iter().all(|message| message
            .content
            .iter()
            .all(|block| !matches!(block, ContentBlock::ToolUse { .. }))));

        history
            .record_tool_result(query_id, token, "call-A", &Ok("value".to_string()))
            .unwrap();
        history.commit_tool_round(query_id, token).unwrap();
        let committed = provider.to_message_request(&ProviderRequest::new(history.get_messages()));
        assert_eq!(committed.messages.len(), 3);
        assert!(matches!(
            committed.messages[1].content[0],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            committed.messages[2].content[0],
            ContentBlock::ToolResult { .. }
        ));
    }

    #[tokio::test]
    async fn configured_claude_endpoint_and_auth_are_honored() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/gateway/anthropic/messages")
            .match_header("x-api-key", "endpoint-secret")
            .match_header("anthropic-version", ANTHROPIC_VERSION)
            .with_status(200)
            .with_body(r#"{"id":"msg-1","type":"message","role":"assistant","content":[{"type":"text","text":"ok"}],"model":"claude-test","stop_reason":"end_turn"}"#)
            .create_async()
            .await;
        let provider = ClaudeProvider::new_with_endpoints(
            "endpoint-secret".to_string(),
            &server.url(),
            "/gateway/anthropic/messages",
            "/gateway/anthropic/models",
        )
        .unwrap()
        .with_model(DEFAULT_CLAUDE_MODEL);

        provider
            .send_message(&ProviderRequest::new(vec![
                crate::claude::types::Message::user("hello"),
            ]))
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn custom_claude_endpoint_cannot_claim_anthropic_streaming_capability() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/gateway/anthropic/messages")
            .expect(0)
            .with_status(500)
            .create_async()
            .await;
        let provider = ClaudeProvider::new_with_endpoints(
            "endpoint-secret".to_string(),
            &server.url(),
            "/gateway/anthropic/messages",
            "/gateway/anthropic/models",
        )
        .unwrap()
        .with_model(DEFAULT_CLAUDE_MODEL);

        let capabilities = provider.capabilities(DEFAULT_CLAUDE_MODEL);
        assert_eq!(capabilities.streaming.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.tools.support, CapabilitySupport::Unknown);
        assert_eq!(capabilities.context_window.max_tokens, None);

        let error = provider
            .send_message_stream(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Provider 'claude' model 'claude-sonnet-5' has unknown streaming capability; refusing to assume support"
        );
        mock.assert_async().await;
    }

    #[test]
    fn test_provider_creation() {
        let provider = ClaudeProvider::new("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_provider_name() {
        let provider = ClaudeProvider::new("test-key".to_string()).unwrap();
        assert_eq!(provider.name(), "claude");
    }
}
