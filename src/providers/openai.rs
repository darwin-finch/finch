// OpenAI API provider implementation
//
// This provider works for both OpenAI (GPT-4, etc.) and Grok (X.AI)
// since they use compatible API formats.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;

use super::types::{ProviderRequest, ProviderResponse, StreamChunk};
use super::LlmProvider;
use crate::claude::retry::with_retry;
use crate::claude::types::ContentBlock;

const REQUEST_TIMEOUT_SECS: u64 = 60;

/// OpenAI API provider
///
/// Supports both OpenAI and Grok APIs (they use the same format).
#[derive(Clone)]
pub struct OpenAIProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_model: String,
    provider_name: String,
}

impl OpenAIProvider {
    /// Create a new OpenAI provider
    pub fn new_openai(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.openai.com".to_string(),
            "gpt-4o".to_string(),
            "openai".to_string(),
        )
    }

    /// Create a new Grok provider (uses OpenAI-compatible API)
    pub fn new_grok(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.x.ai".to_string(),
            "grok-2".to_string(),
            "grok".to_string(),
        )
    }

    /// Create a new Mistral provider (uses OpenAI-compatible API)
    pub fn new_mistral(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.mistral.ai".to_string(),
            "mistral-large-latest".to_string(),
            "mistral".to_string(),
        )
    }

    /// Create a new Groq provider (fast inference, uses OpenAI-compatible API)
    /// Note: This is Groq (by Groq Inc), not Grok (by X.AI)
    pub fn new_groq(api_key: String) -> Result<Self> {
        Self::new(
            api_key,
            "https://api.groq.com".to_string(),
            "llama-3.1-70b-versatile".to_string(),
            "groq".to_string(),
        )
    }

    /// Set custom model for this provider
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Create a provider with custom settings
    fn new(api_key: String, base_url: String, default_model: String, provider_name: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            api_key,
            base_url,
            default_model,
            provider_name,
        })
    }

    /// Convert ProviderRequest to OpenAI API format
    fn to_openai_request(&self, request: &ProviderRequest) -> OpenAIRequest {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        let mut messages: Vec<OpenAIMessage> = Vec::new();

        // Prepend system prompt as a {"role":"system"} message (OpenAI convention)
        if let Some(system) = &request.system {
            messages.push(OpenAIMessage::Regular {
                role: "system".to_string(),
                content: system.clone(),
            });
        }

        for msg in &request.messages {
            match msg.role.as_str() {
                "assistant" => {
                    // Collect text and tool_calls into a single assistant message.
                    // The OpenAI API requires tool_calls to be in the assistant message
                    // (not silently dropped), otherwise subsequent tool results are orphaned.
                    let text: String = msg.content.iter()
                        .filter_map(|b| b.as_text())
                        .collect::<Vec<_>>()
                        .join("");

                    let tool_calls: Vec<OpenAIRequestToolCall> = msg.content.iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => {
                                let arguments = serde_json::to_string(input)
                                    .unwrap_or_else(|_| "{}".to_string());
                                Some(OpenAIRequestToolCall {
                                    id: id.clone(),
                                    tool_type: "function".to_string(),
                                    function: OpenAIRequestFunction {
                                        name: name.clone(),
                                        arguments,
                                    },
                                })
                            }
                            _ => None,
                        })
                        .collect();

                    messages.push(OpenAIMessage::Assistant {
                        role: "assistant".to_string(),
                        content: if text.is_empty() { None } else { Some(text) },
                        tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                    });
                }
                _ => {
                    // user / system messages: separate text from tool results
                    let mut text_parts: Vec<&str> = Vec::new();
                    let mut tool_results: Vec<(String, String)> = Vec::new();

                    for block in &msg.content {
                        match block {
                            ContentBlock::Text { text } => {
                                text_parts.push(text.as_str());
                            }
                            ContentBlock::ToolResult { tool_use_id, content, .. } => {
                                tool_results.push((tool_use_id.clone(), content.clone()));
                            }
                            ContentBlock::Image { .. } => {
                                text_parts.push("[image]");
                            }
                            ContentBlock::ToolUse { .. } => {}
                        }
                    }

                    if !text_parts.is_empty() {
                        let content = text_parts.join("\n");
                        if !content.trim().is_empty() {
                            messages.push(OpenAIMessage::Regular {
                                role: msg.role.clone(),
                                content,
                            });
                        }
                    }

                    // One tool message per result (OpenAI requires separate messages)
                    for (tool_call_id, content) in tool_results {
                        messages.push(OpenAIMessage::Tool {
                            role: "tool".to_string(),
                            content: if content.trim().is_empty() {
                                "(no output)".to_string()
                            } else {
                                content
                            },
                            tool_call_id,
                        });
                    }
                }
            }
        }

        // Convert tools to OpenAI format if present
        let tools = request.tools.as_ref().map(|tool_defs| {
            tool_defs
                .iter()
                .map(|tool| {
                    // Convert ToolInputSchema to Value
                    let parameters = match serde_json::to_value(&tool.input_schema) {
                        Ok(value) => value,
                        Err(e) => {
                            tracing::warn!(
                                "Failed to convert tool schema for '{}': {}",
                                tool.name,
                                e
                            );
                            serde_json::json!({})
                        }
                    };

                    OpenAITool {
                        tool_type: "function".to_string(),
                        function: OpenAIFunction {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            parameters,
                        },
                    }
                })
                .collect()
        });

        OpenAIRequest {
            model,
            messages,
            max_tokens: Some(request.max_tokens),
            temperature: request.temperature,
            tools,
            stream: request.stream,
        }
    }

    /// Convert OpenAI response to ProviderResponse
    fn from_openai_response(&self, response: OpenAIResponse) -> Result<ProviderResponse> {
        let choice = response
            .choices
            .into_iter()
            .next()
            .context("OpenAI returned no choices in response")?;

        // Convert message content to ContentBlock
        let mut content = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentBlock::Text { text });
            }
        }

        // Convert tool calls to ContentBlock::ToolUse
        if let Some(tool_calls) = choice.message.tool_calls {
            for tool_call in tool_calls {
                if tool_call.tool_type == "function" {
                    let input = serde_json::from_str(&tool_call.function.arguments)
                        .unwrap_or(serde_json::json!({}));
                    content.push(ContentBlock::ToolUse {
                        id: tool_call.id,
                        name: tool_call.function.name,
                        input,
                    });
                }
            }
        }

        Ok(ProviderResponse {
            id: response.id,
            model: response.model,
            content,
            stop_reason: choice.finish_reason,
            role: choice.message.role,
            provider: self.provider_name.clone(),
        })
    }

    /// Send a single message request (no retry)
    async fn send_message_once(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let openai_request = self.to_openai_request(request);
        let url = format!("{}/v1/chat/completions", self.base_url);

        tracing::debug!("Sending request to OpenAI API: {:?}", openai_request);

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
            .context("Failed to send request to OpenAI API")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "OpenAI API request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        let openai_response: OpenAIResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI API response")?;

        tracing::debug!("Received response: {:?}", openai_response);

        self.from_openai_response(openai_response)
    }

    /// Send a message with streaming response (no retry)
    async fn send_message_stream_once(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::channel(100);

        let mut openai_request = self.to_openai_request(request);
        openai_request.stream = true;

        let url = format!("{}/v1/chat/completions", self.base_url);

        tracing::debug!("Sending streaming request to OpenAI API");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&openai_request)
            .send()
            .await
            .context("Failed to send streaming request to OpenAI API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "OpenAI API streaming request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        // Spawn task to parse SSE stream
        tokio::spawn(async move {
            tracing::debug!("[STREAM] OpenAI streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut accumulated_text = String::new();
            #[allow(unused_assignments)]
            let mut done = false;

            while let Some(chunk) = stream.next().await {
                if done {
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
                                    tracing::debug!("[STREAM] Received [DONE]");

                                    // Send accumulated text as final block
                                    if !accumulated_text.is_empty() {
                                        let block = ContentBlock::Text {
                                            text: accumulated_text.clone(),
                                        };
                                        if tx
                                            .send(Ok(StreamChunk::ContentBlockComplete(block)))
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }

                                    done = true;
                                    break;
                                }

                                // Parse streaming chunk
                                if let Ok(stream_chunk) =
                                    serde_json::from_str::<OpenAIStreamChunk>(json_str)
                                {
                                    if let Some(choice) = stream_chunk.choices.into_iter().next() {
                                        if let Some(content) = choice.delta.content {
                                            accumulated_text.push_str(&content);
                                            // Send delta immediately
                                            if tx.send(Ok(StreamChunk::TextDelta(content))).await.is_err() {
                                                done = true;
                                                break;
                                            }
                                        }

                                        // Check for tool calls in delta
                                        if let Some(tool_calls) = choice.delta.tool_calls {
                                            for tool_call in tool_calls {
                                                if let Some(function) = tool_call.function {
                                                    // For now, we'll accumulate tool calls and send them complete
                                                    // A more sophisticated implementation would stream tool arguments
                                                    tracing::debug!("Tool call: {:?}", function.name);
                                                }
                                            }
                                        }
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

            tracing::debug!("[STREAM] OpenAI streaming task finished");
        });

        Ok(rx)
    }
}

#[async_trait]
impl LlmProvider for OpenAIProvider {
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
        &self.provider_name
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

// OpenAI API types

#[derive(Debug, Clone, Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<OpenAITool>>,
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// OpenAI message format — request side only (we never deserialize this)
///
/// The untagged variants are ordered so serde tries the most-specific first:
/// Tool (has tool_call_id), Assistant (has optional tool_calls), then Regular.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
enum OpenAIMessage {
    /// Tool result message (one per tool invocation)
    Tool {
        role: String, // "tool"
        content: String,
        tool_call_id: String,
    },
    /// Assistant message — may contain text, tool_calls, or both
    Assistant {
        role: String, // "assistant"
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<OpenAIRequestToolCall>>,
    },
    /// Plain user / system message
    Regular {
        role: String,
        content: String,
    },
}

/// Tool call entry inside an assistant message (request format)
#[derive(Debug, Clone, Serialize)]
struct OpenAIRequestToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIRequestFunction,
}

#[derive(Debug, Clone, Serialize)]
struct OpenAIRequestFunction {
    name: String,
    arguments: String, // JSON-encoded string
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAITool {
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAIFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIResponse {
    id: String,
    model: String,
    choices: Vec<OpenAIChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIChoice {
    index: usize,
    message: OpenAIResponseMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIResponseMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    tool_type: String,
    function: OpenAIToolFunction,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIToolFunction {
    name: String,
    arguments: String, // JSON string
}

// Streaming types

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamChunk {
    id: String,
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIStreamChoice {
    index: usize,
    delta: OpenAIDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIDelta {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCallDelta>>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    #[serde(rename = "type")]
    tool_type: Option<String>,
    function: Option<OpenAIFunctionDelta>,
}

#[derive(Debug, Clone, Deserialize)]
struct OpenAIFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_provider_creation() {
        let provider = OpenAIProvider::new_openai("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_grok_provider_creation() {
        let provider = OpenAIProvider::new_grok("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_provider_names() {
        let openai = OpenAIProvider::new_openai("test-key".to_string()).unwrap();
        assert_eq!(openai.name(), "openai");

        let grok = OpenAIProvider::new_grok("test-key".to_string()).unwrap();
        assert_eq!(grok.name(), "grok");
    }
}
