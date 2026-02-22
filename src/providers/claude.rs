// Claude API provider implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::mpsc;

use super::types::{ProviderRequest, ProviderResponse, StreamChunk};
use super::LlmProvider;
use crate::claude::retry::with_retry;
use crate::claude::streaming::StreamEvent;
use crate::claude::types::{ContentBlock, MessageRequest};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT_SECS: u64 = 60;

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
}

impl ClaudeProvider {
    /// Create a new Claude provider
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            api_key,
            default_model: "claude-sonnet-4-20250514".to_string(),
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

        tracing::debug!("Sending request to Claude API: {:?}", msg_request);

        let response = self
            .client
            .post(CLAUDE_API_URL)
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
            anyhow::bail!("{}", friendly_api_error(status, &error_body));
        }

        let message_response: crate::claude::types::MessageResponse = response
            .json()
            .await
            .context("Failed to parse Claude API response")?;

        tracing::debug!("Received response: {:?}", message_response);

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
            .post(CLAUDE_API_URL)
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
            anyhow::bail!("{}", friendly_api_error(status, &error_body));
        }

        // Spawn task to parse SSE stream with block tracking
        tokio::spawn(async move {
            tracing::debug!("[STREAM] Streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();

            // Track blocks being built (index -> BlockBuilder)
            let mut blocks: HashMap<usize, BlockBuilder> = HashMap::new();
            #[allow(unused_assignments)]
            let mut done = false;

            while let Some(chunk) = stream.next().await {
                if done {
                    tracing::debug!("[STREAM] Done flag set, breaking from chunk loop");
                    break;
                }

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
                                    tracing::debug!("[STREAM] Received [DONE], marking stream as complete");
                                    done = true;
                                    break;
                                }

                                // Parse event
                                if let Ok(event) = serde_json::from_str::<StreamEvent>(json_str) {
                                    tracing::debug!("Stream event: {}", event.event_type);
                                    match event.event_type.as_str() {
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
                                                                // Send delta immediately
                                                                if tx
                                                                    .send(Ok(StreamChunk::TextDelta(
                                                                        text,
                                                                    )))
                                                                    .await
                                                                    .is_err()
                                                                {
                                                                    // Receiver dropped, stop streaming
                                                                    done = true;
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
                                                    done = true;
                                                    break;
                                                }
                                            }
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
                        break;
                    }
                }
            }

            tracing::debug!("[STREAM] Exited chunk loop, task finishing");
        });

        Ok(rx)
    }
}

#[async_trait]
impl LlmProvider for ClaudeProvider {
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        with_retry(|| self.send_message_once(request)).await
    }

    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        with_retry(|| self.send_message_stream_once(request)).await
    }

    fn name(&self) -> &str {
        "claude"
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
