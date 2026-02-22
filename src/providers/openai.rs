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

/// Parse an API error body and return a human-friendly message with hints.
///
/// Most providers return `{"error": {"message": "...", "type": "...", "code": "..."}}`.
fn friendly_api_error(status: reqwest::StatusCode, body: &str) -> String {
    // Try to extract the inner message from standard JSON error format
    let extracted = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
        });

    let msg = extracted.as_deref().unwrap_or(body.trim());

    // Provide actionable hints based on status code
    let hint = match status.as_u16() {
        401 => " — Check that your API key is correct in ~/.finch/config.toml",
        403 => " — Your API key may lack permissions for this model",
        429 => " — You've hit a rate limit; wait a moment before retrying",
        400 => " — The request was malformed (this may be a finch bug; please report it)",
        404 => " — Model not found; check the model name in your config",
        500 | 502 | 503 => " — The provider is having issues; try again in a moment",
        _ => "",
    };

    format!("API error {}{}: {}", status, hint, msg)
}

// ─── Streaming tool-call helpers ─────────────────────────────────────────────
//
// OpenAI streams tool calls as *fragments* across multiple SSE deltas.
// We accumulate them into a Vec<(id, name, args_so_far)> and then convert
// them to ContentBlock::ToolUse when the [DONE] marker arrives.

/// Merge one streaming `OpenAIToolCallDelta` into the accumulator.
/// The accumulator is indexed by `delta.index` (default 0).
fn accumulate_tool_call_delta(
    acc: &mut Vec<(String, String, String)>,
    delta: &OpenAIToolCallDelta,
) {
    let idx = delta.index.unwrap_or(0);
    while acc.len() <= idx {
        acc.push((String::new(), String::new(), String::new()));
    }
    if let Some(id) = &delta.id {
        acc[idx].0.push_str(id);
    }
    if let Some(func) = &delta.function {
        if let Some(name) = &func.name {
            acc[idx].1.push_str(name);
        }
        if let Some(args) = &func.arguments {
            acc[idx].2.push_str(args);
        }
    }
}

/// Convert the final accumulator into `ContentBlock::ToolUse` blocks.
///
/// Each entry is `(id, name, json_arguments_string)`.
/// Invalid JSON in the arguments is replaced with an empty object.
fn finalize_tool_calls(acc: &[(String, String, String)]) -> Vec<ContentBlock> {
    acc.iter()
        .filter(|(id, name, _)| !id.is_empty() || !name.is_empty())
        .map(|(id, name, args_str)| {
            let input = serde_json::from_str::<serde_json::Value>(args_str)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
            ContentBlock::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input,
            }
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────

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

                    // Grok (and strict OpenAI) require at least one of content or tool_calls.
                    // If both are absent, use a single space so the message is not empty.
                    let content = match (text.is_empty(), tool_calls.is_empty()) {
                        (false, _) => Some(text),
                        (true, false) => None, // tool_calls present — content optional
                        (true, true) => Some(" ".to_string()), // guard: never emit bare {"role":"assistant"}
                    };
                    messages.push(OpenAIMessage::Assistant {
                        role: "assistant".to_string(),
                        content,
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
            anyhow::bail!("{}", friendly_api_error(status, &error_body));
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
            anyhow::bail!("{}", friendly_api_error(status, &error_body));
        }

        // Spawn task to parse SSE stream
        tokio::spawn(async move {
            tracing::debug!("[STREAM] OpenAI streaming task started");
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();
            let mut accumulated_text = String::new();
            // Tool call accumulator: indexed by tool_call.index.
            // Each entry: (call_id, function_name, arguments_so_far).
            // Converted to ContentBlock::ToolUse when [DONE] arrives.
            let mut tool_call_acc: Vec<(String, String, String)> = Vec::new();
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

                                    // Convert accumulated tool call deltas to ToolUse blocks
                                    for block in finalize_tool_calls(&tool_call_acc) {
                                        if let ContentBlock::ToolUse { ref name, ref id, .. } = block {
                                            tracing::debug!("[STREAM] Sending tool call: {} ({})", name, id);
                                        }
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

                                        // Accumulate tool call deltas — OpenAI sends them piecemeal.
                                        // Each delta may contain partial id/name/arguments fragments
                                        // for each tool call (identified by index).
                                        if let Some(tc_deltas) = choice.delta.tool_calls {
                                            for tc in tc_deltas {
                                                accumulate_tool_call_delta(&mut tool_call_acc, &tc);
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

    #[test]
    fn test_default_models() {
        let openai = OpenAIProvider::new_openai("key".to_string()).unwrap();
        assert!(!openai.default_model().is_empty());

        let grok = OpenAIProvider::new_grok("key".to_string()).unwrap();
        assert!(grok.default_model().contains("grok"));
    }

    #[test]
    fn test_to_openai_request_system_prompt() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::Message;
        let req = ProviderRequest::new(vec![Message::user("hello")])
            .with_system("You are helpful.");
        let openai_req = provider.to_openai_request(&req);
        // System message should be first
        assert!(matches!(&openai_req.messages[0], OpenAIMessage::Regular { role, .. } if role == "system"));
        if let OpenAIMessage::Regular { content, .. } = &openai_req.messages[0] {
            assert_eq!(content, "You are helpful.");
        }
    }

    #[test]
    fn test_to_openai_request_no_system_prompt() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::Message;
        let req = ProviderRequest::new(vec![Message::user("hello")]);
        let openai_req = provider.to_openai_request(&req);
        // No system message — first message is user
        assert!(matches!(&openai_req.messages[0], OpenAIMessage::Regular { role, .. } if role == "user"));
    }

    #[test]
    fn test_to_openai_request_tool_calls_included() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::{Message, ContentBlock};
        let req = ProviderRequest::new(vec![
            Message::user("run ls"),
            Message::with_content("assistant", vec![
                ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ]),
        ]);
        let openai_req = provider.to_openai_request(&req);
        // Assistant message should have tool_calls
        let assistant_msg = openai_req.messages.iter().find(|m| matches!(m, OpenAIMessage::Assistant { .. }));
        assert!(assistant_msg.is_some());
        if let Some(OpenAIMessage::Assistant { tool_calls, .. }) = assistant_msg {
            let calls = tool_calls.as_ref().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].id, "call_1");
            assert_eq!(calls[0].function.name, "bash");
        }
    }

    #[test]
    fn test_to_openai_request_tool_result_becomes_tool_role() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::{Message, ContentBlock};
        let req = ProviderRequest::new(vec![
            Message::user("run ls"),
            Message::with_content("assistant", vec![
                ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "bash".to_string(),
                    input: serde_json::json!({}),
                },
            ]),
            Message::with_content("user", vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "file.txt".to_string(),
                    is_error: None,
                },
            ]),
        ]);
        let openai_req = provider.to_openai_request(&req);
        // There should be a "tool" role message
        let tool_msg = openai_req.messages.iter().find(|m| matches!(m, OpenAIMessage::Tool { .. }));
        assert!(tool_msg.is_some());
        if let Some(OpenAIMessage::Tool { tool_call_id, content, .. }) = tool_msg {
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(content, "file.txt");
        }
    }

    #[test]
    fn test_empty_tool_result_gets_placeholder() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::{Message, ContentBlock};
        let req = ProviderRequest::new(vec![
            Message::with_content("user", vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "  ".to_string(), // whitespace-only
                    is_error: None,
                },
            ]),
        ]);
        let openai_req = provider.to_openai_request(&req);
        if let Some(OpenAIMessage::Tool { content, .. }) = openai_req.messages.iter().find(|m| matches!(m, OpenAIMessage::Tool { .. })) {
            assert_eq!(content, "(no output)");
        } else {
            panic!("Expected a tool message");
        }
    }

    #[test]
    fn test_to_openai_request_empty_user_text_skipped() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        use crate::claude::types::{Message, ContentBlock};
        // A user message with only whitespace text should not generate a "user" message
        let req = ProviderRequest::new(vec![
            Message::with_content("user", vec![
                ContentBlock::Text { text: "   ".to_string() },
            ]),
        ]);
        let openai_req = provider.to_openai_request(&req);
        assert!(openai_req.messages.is_empty());
    }

    #[test]
    fn test_to_openai_request_uses_fallback_model() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        use crate::providers::types::ProviderRequest;
        // Request with empty model — should fall back to provider default
        let req = ProviderRequest::new(vec![]);
        let openai_req = provider.to_openai_request(&req);
        assert!(!openai_req.model.is_empty());
    }

    #[test]
    fn test_provider_supports_streaming() {
        let provider = OpenAIProvider::new_openai("key".to_string()).unwrap();
        assert!(provider.supports_streaming());
    }

    #[test]
    fn test_provider_supports_tools() {
        let provider = OpenAIProvider::new_grok("key".to_string()).unwrap();
        assert!(provider.supports_tools());
    }

    // ── Streaming tool-call accumulation ─────────────────────────────────────

    #[test]
    fn test_accumulate_single_complete_delta() {
        // A single delta that has the full id, name, and arguments.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        let delta = OpenAIToolCallDelta {
            index: Some(0),
            id: Some("call_abc".to_string()),
            tool_type: Some("function".to_string()),
            function: Some(OpenAIFunctionDelta {
                name: Some("bash".to_string()),
                arguments: Some(r#"{"command":"echo hi"}"#.to_string()),
            }),
        };
        accumulate_tool_call_delta(&mut acc, &delta);
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_abc");
        assert_eq!(acc[0].1, "bash");
        assert_eq!(acc[0].2, r#"{"command":"echo hi"}"#);
    }

    #[test]
    fn test_accumulate_fragmented_arguments() {
        // OpenAI often sends the arguments JSON in multiple fragments.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        // First delta: has id and name
        accumulate_tool_call_delta(&mut acc, &OpenAIToolCallDelta {
            index: Some(0),
            id: Some("call_1".to_string()),
            tool_type: None,
            function: Some(OpenAIFunctionDelta {
                name: Some("read".to_string()),
                arguments: Some(r#"{"file_"#.to_string()),
            }),
        });
        // Second delta: continues arguments
        accumulate_tool_call_delta(&mut acc, &OpenAIToolCallDelta {
            index: Some(0),
            id: None,
            tool_type: None,
            function: Some(OpenAIFunctionDelta {
                name: None,
                arguments: Some(r#"path":"src/main.rs"}"#.to_string()),
            }),
        });
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_1");
        assert_eq!(acc[0].1, "read");
        assert_eq!(acc[0].2, r#"{"file_path":"src/main.rs"}"#);
    }

    #[test]
    fn test_accumulate_multiple_tool_calls() {
        // Two tool calls with different indices.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        accumulate_tool_call_delta(&mut acc, &OpenAIToolCallDelta {
            index: Some(0),
            id: Some("call_0".to_string()),
            tool_type: None,
            function: Some(OpenAIFunctionDelta {
                name: Some("bash".to_string()),
                arguments: Some(r#"{}"#.to_string()),
            }),
        });
        accumulate_tool_call_delta(&mut acc, &OpenAIToolCallDelta {
            index: Some(1),
            id: Some("call_1".to_string()),
            tool_type: None,
            function: Some(OpenAIFunctionDelta {
                name: Some("read".to_string()),
                arguments: Some(r#"{"file_path":"x"}"#.to_string()),
            }),
        });
        assert_eq!(acc.len(), 2);
        assert_eq!(acc[0].1, "bash");
        assert_eq!(acc[1].1, "read");
    }

    #[test]
    fn test_finalize_tool_calls_parses_json() {
        let acc = vec![
            ("call_1".to_string(), "bash".to_string(), r#"{"command":"ls"}"#.to_string()),
        ];
        let blocks = finalize_tool_calls(&acc);
        assert_eq!(blocks.len(), 1);
        if let crate::claude::types::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
            assert_eq!(id, "call_1");
            assert_eq!(name, "bash");
            assert_eq!(input["command"].as_str().unwrap(), "ls");
        } else {
            panic!("Expected ToolUse block");
        }
    }

    #[test]
    fn test_finalize_tool_calls_invalid_json_falls_back_to_empty_object() {
        let acc = vec![
            ("call_x".to_string(), "glob".to_string(), "NOT_VALID_JSON".to_string()),
        ];
        let blocks = finalize_tool_calls(&acc);
        assert_eq!(blocks.len(), 1);
        if let crate::claude::types::ContentBlock::ToolUse { input, .. } = &blocks[0] {
            assert!(input.is_object(), "Invalid JSON should yield empty object");
        } else {
            panic!("Expected ToolUse block");
        }
    }

    #[test]
    fn test_finalize_tool_calls_empty_acc() {
        let acc: Vec<(String, String, String)> = Vec::new();
        let blocks = finalize_tool_calls(&acc);
        assert!(blocks.is_empty());
    }

    #[test]
    fn test_accumulate_default_index_zero() {
        // Delta without an explicit index should go to slot 0.
        let mut acc: Vec<(String, String, String)> = Vec::new();
        accumulate_tool_call_delta(&mut acc, &OpenAIToolCallDelta {
            index: None, // no index — should default to 0
            id: Some("call_no_idx".to_string()),
            tool_type: None,
            function: Some(OpenAIFunctionDelta {
                name: Some("grep".to_string()),
                arguments: Some(r#"{"pattern":"TODO"}"#.to_string()),
            }),
        });
        assert_eq!(acc.len(), 1);
        assert_eq!(acc[0].0, "call_no_idx");
        assert_eq!(acc[0].1, "grep");
    }

    #[test]
    fn test_streaming_tool_calls_end_to_end_simulation() {
        // Simulate a full streaming sequence: two deltas for one tool call followed by finalize.
        // This replicates the exact pattern Grok/OpenAI uses in the wild.
        let mut acc: Vec<(String, String, String)> = Vec::new();

        // Delta 1: id + function name + start of arguments
        let delta1_json = r#"{"index":0,"id":"call_xyz","type":"function","function":{"name":"bash","arguments":"{\"comm"}}"#;
        let delta1: OpenAIToolCallDelta = serde_json::from_str(delta1_json).unwrap();
        accumulate_tool_call_delta(&mut acc, &delta1);

        // Delta 2: continuation of arguments only
        let delta2_json = r#"{"index":0,"function":{"arguments":"and\": \"echo test\"}"}}"#;
        let delta2: OpenAIToolCallDelta = serde_json::from_str(delta2_json).unwrap();
        accumulate_tool_call_delta(&mut acc, &delta2);

        // Finalize
        let blocks = finalize_tool_calls(&acc);
        assert_eq!(blocks.len(), 1);
        if let crate::claude::types::ContentBlock::ToolUse { id, name, input } = &blocks[0] {
            assert_eq!(id, "call_xyz");
            assert_eq!(name, "bash");
            assert_eq!(input["command"].as_str().unwrap(), "echo test");
        } else {
            panic!("Expected ToolUse block, got {:?}", blocks[0]);
        }
    }
}
