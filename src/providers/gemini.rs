// Google Gemini API provider implementation
//
// Gemini has a different message format and streaming protocol compared to
// Claude and OpenAI, requiring custom conversion logic.

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::types::{ProviderRequest, ProviderResponse, StreamChunk};
use super::LlmProvider;
use crate::claude::retry::with_retry;
use crate::claude::types::ContentBlock;

const REQUEST_TIMEOUT_SECS: u64 = 60;
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini API provider
///
/// Supports Gemini 2.0 Flash and other Gemini models.
#[derive(Clone)]
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl GeminiProvider {
    /// Create a new Gemini provider
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            client,
            api_key,
            default_model: "gemini-2.0-flash-exp".to_string(),
        })
    }

    /// Create with custom default model
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Convert ProviderRequest to Gemini API format
    fn to_gemini_request(&self, request: &ProviderRequest) -> GeminiRequest {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        // Convert messages to Gemini's contents format
        let contents: Vec<GeminiContent> = request
            .messages
            .iter()
            .map(|msg| {
                // Gemini uses "model" instead of "assistant"
                let role = if msg.role == "assistant" {
                    "model"
                } else {
                    &msg.role
                };

                // Convert all content blocks to Gemini parts
                let parts: Vec<GeminiPart> = msg
                    .content
                    .iter()
                    .map(|block| match block {
                        ContentBlock::Text { text } => GeminiPart::Text {
                            text: text.clone(),
                        },
                        ContentBlock::ToolUse { id: _, name, input } => GeminiPart::FunctionCall {
                            function_call: GeminiFunctionCall {
                                name: name.clone(),
                                args: input.clone(),
                            },
                        },
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            is_error,
                        } => GeminiPart::FunctionResponse {
                            function_response: GeminiFunctionResponse {
                                name: tool_use_id.clone(),
                                response: serde_json::json!({
                                    "content": content,
                                    "is_error": is_error.unwrap_or(false),
                                }),
                            },
                        },
                        ContentBlock::Image { .. } => GeminiPart::Text {
                            text: "[image content]".to_string(),
                        },
                    })
                    .collect();

                GeminiContent {
                    role: role.to_string(),
                    parts,
                }
            })
            .collect();

        // Convert tools to Gemini's function declarations format
        let tools = request.tools.as_ref().map(|tool_defs| {
            vec![GeminiTools {
                function_declarations: tool_defs
                    .iter()
                    .map(|tool| {
                        // Convert ToolInputSchema to parameters
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

                        GeminiFunctionDeclaration {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                            parameters,
                        }
                    })
                    .collect(),
            }]
        });

        let generation_config = GeminiGenerationConfig {
            temperature: request.temperature,
            max_output_tokens: Some(request.max_tokens as i32),
            ..Default::default()
        };

        GeminiRequest {
            model,
            contents,
            tools,
            generation_config: Some(generation_config),
        }
    }

    /// Convert Gemini response to ProviderResponse
    fn from_gemini_response(
        &self,
        response: GeminiResponse,
        model: String,
    ) -> Result<ProviderResponse> {
        let candidate = response
            .candidates
            .into_iter()
            .next()
            .context("Gemini returned no candidates in response")?;

        // Convert parts to ContentBlock
        let mut content = Vec::new();

        for part in candidate.content.parts {
            match part {
                GeminiPart::Text { text } => {
                    if !text.is_empty() {
                        content.push(ContentBlock::Text { text });
                    }
                }
                GeminiPart::FunctionCall { function_call } => {
                    // Generate unique ID since Gemini doesn't provide tool call IDs
                    let unique_id = format!("gemini_{}_{}", function_call.name, Uuid::new_v4());
                    content.push(ContentBlock::ToolUse {
                        id: unique_id,
                        name: function_call.name,
                        input: function_call.args,
                    });
                }
                GeminiPart::FunctionResponse { .. } => {
                    // Skip function responses in output
                }
            }
        }

        Ok(ProviderResponse {
            id: "gemini-response".to_string(), // Gemini doesn't provide response IDs
            model,
            content,
            stop_reason: candidate.finish_reason,
            role: "assistant".to_string(), // Convert "model" back to "assistant"
            provider: "gemini".to_string(),
        })
    }

    /// Send a single message request (no retry)
    async fn send_message_once(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let gemini_request = self.to_gemini_request(request);
        let model = gemini_request.model.clone();

        let url = format!(
            "{}/models/{}:generateContent?key={}",
            GEMINI_BASE_URL, model, self.api_key
        );

        tracing::debug!("Sending request to Gemini API: {:?}", gemini_request);

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&gemini_request)
            .send()
            .await
            .context("Failed to send request to Gemini API")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini API request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        let gemini_response: GeminiResponse = response
            .json()
            .await
            .context("Failed to parse Gemini API response")?;

        tracing::debug!("Received response: {:?}", gemini_response);

        self.from_gemini_response(gemini_response, model)
    }

    /// Send a message with streaming response (no retry)
    async fn send_message_stream_once(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::channel(100);

        let gemini_request = self.to_gemini_request(request);
        let model = gemini_request.model.clone();

        let url = format!(
            "{}/models/{}:streamGenerateContent?key={}&alt=sse",
            GEMINI_BASE_URL, model, self.api_key
        );

        tracing::debug!("Sending streaming request to Gemini API");

        let response = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .json(&gemini_request)
            .send()
            .await
            .context("Failed to send streaming request to Gemini API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Gemini API streaming request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        // Spawn task to parse streaming response
        tokio::spawn(async move {
            tracing::debug!("[STREAM] Gemini streaming task started");
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

                        // Parse line by line (Gemini uses SSE format)
                        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes);

                            // SSE format: "data: {...}\n"
                            if let Some(json_str) = line.strip_prefix("data: ") {
                                let json_str = json_str.trim();

                                // Skip [DONE] marker if present
                                if json_str == "[DONE]" {
                                    tracing::debug!("[STREAM] Received [DONE]");
                                    done = true;
                                    break;
                                }

                                // Parse streaming chunk
                                if let Ok(stream_response) =
                                    serde_json::from_str::<GeminiResponse>(json_str)
                                {
                                    if let Some(candidate) = stream_response.candidates.into_iter().next() {
                                        for part in candidate.content.parts {
                                            match part {
                                                GeminiPart::Text { text } => {
                                                    accumulated_text.push_str(&text);
                                                    // Send delta immediately
                                                    if tx.send(Ok(StreamChunk::TextDelta(text))).await.is_err() {
                                                        done = true;
                                                        break;
                                                    }
                                                }
                                                GeminiPart::FunctionCall { function_call } => {
                                                    // Generate unique ID for tool call
                                                    let unique_id = format!("gemini_{}_{}", function_call.name, Uuid::new_v4());
                                                    let tool_use = ContentBlock::ToolUse {
                                                        id: unique_id,
                                                        name: function_call.name,
                                                        input: function_call.args,
                                                    };
                                                    // Send complete tool use block
                                                    if tx.send(Ok(StreamChunk::ContentBlockComplete(tool_use))).await.is_err() {
                                                        done = true;
                                                        break;
                                                    }
                                                }
                                                GeminiPart::FunctionResponse { .. } => {
                                                    // Skip function responses
                                                }
                                            }
                                        }

                                        // Check for finish
                                        if candidate.finish_reason.is_some() {
                                            tracing::debug!("[STREAM] Stream completed");
                                            done = true;
                                            break;
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

            // Send final complete block if we have text
            if !accumulated_text.is_empty() {
                let block = ContentBlock::Text {
                    text: accumulated_text,
                };
                let _ = tx.send(Ok(StreamChunk::ContentBlockComplete(block))).await;
            }

            tracing::debug!("[STREAM] Gemini streaming task finished");
        });

        Ok(rx)
    }
}

#[async_trait]
impl LlmProvider for GeminiProvider {
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
        "gemini"
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

// Gemini API types

#[derive(Debug, Clone, Serialize)]
struct GeminiRequest {
    #[serde(skip)]
    model: String, // Used in URL, not in body
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTools>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiContent {
    role: String, // "user" or "model"
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum GeminiPart {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: GeminiFunctionCall,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: GeminiFunctionResponse,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiTools {
    #[serde(rename = "functionDeclarations")]
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "maxOutputTokens")]
    max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "topP")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "topK")]
    top_k: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct GeminiResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct GeminiCandidate {
    content: GeminiContent,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
    #[serde(rename = "safetyRatings")]
    safety_ratings: Option<Vec<GeminiSafetyRating>>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct GeminiSafetyRating {
    category: String,
    probability: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_provider_creation() {
        let provider = GeminiProvider::new("test-key".to_string());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_provider_name() {
        let provider = GeminiProvider::new("test-key".to_string()).unwrap();
        assert_eq!(provider.name(), "gemini");
    }

    #[test]
    fn test_default_model() {
        let provider = GeminiProvider::new("test-key".to_string()).unwrap();
        assert_eq!(provider.default_model(), "gemini-2.0-flash-exp");
    }

    #[test]
    fn test_custom_model() {
        let provider = GeminiProvider::new("test-key".to_string())
            .unwrap()
            .with_model("gemini-pro");
        assert_eq!(provider.default_model(), "gemini-pro");
    }
}
