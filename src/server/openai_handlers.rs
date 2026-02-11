// OpenAI-compatible API handlers
//
// Implements /v1/chat/completions and /v1/models endpoints
// with format conversion between OpenAI and internal types.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use std::sync::Arc;
use std::time::Instant;
use tracing::{info, warn};

use super::openai_types::*;
use super::AgentServer;
use crate::claude::{ContentBlock, Message};
use crate::router::RouteDecision;
use crate::tools::types::ToolDefinition as InternalToolDefinition;
use crate::tools::types::ToolInputSchema;

/// Error response for OpenAI API
#[derive(Debug, serde::Serialize)]
struct ErrorResponse {
    error: ErrorDetail,
}

#[derive(Debug, serde::Serialize)]
struct ErrorDetail {
    message: String,
    #[serde(rename = "type")]
    error_type: String,
    code: Option<String>,
}

impl ErrorResponse {
    fn new(message: String, error_type: String) -> Self {
        Self {
            error: ErrorDetail {
                message,
                error_type,
                code: None,
            },
        }
    }
}

/// Handle POST /v1/chat/completions - OpenAI-compatible chat endpoint
pub async fn handle_chat_completions(
    State(server): State<Arc<AgentServer>>,
    Json(request): Json<ChatCompletionRequest>,
) -> Result<Json<ChatCompletionResponse>, Response> {
    let start_time = Instant::now();

    // Validate request
    if request.messages.is_empty() {
        return Err(error_response(
            "messages array cannot be empty",
            "invalid_request_error",
        ));
    }

    if request.stream {
        return Err(error_response(
            "streaming is not yet supported",
            "invalid_request_error",
        ));
    }

    // Convert OpenAI messages to internal format (now handles tool calls/results)
    let internal_messages = convert_messages_to_internal(&request.messages)
        .map_err(|e| error_response(&e.to_string(), "invalid_request_error"))?;

    // Convert OpenAI tools to internal format
    let internal_tools = request.tools.as_ref().map(|tools| convert_tools_to_internal(tools));

    // Extract user query for routing
    let user_query = request
        .messages
        .iter()
        .filter(|m| m.role == "user")
        .last()
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");

    // Route decision
    let router = server.router().read().await;
    let decision = router.route(user_query);
    drop(router);

    let (content_blocks, routing_decision) = match decision {
        RouteDecision::Forward { reason } => {
            info!(reason = ?reason, "Routing to Claude API");

            // Forward to Claude with tools
            let mut claude_request = crate::claude::MessageRequest::with_context(internal_messages.clone());
            if let Some(tools) = internal_tools.clone() {
                claude_request = claude_request.with_tools(tools);
            }

            let response = server
                .claude_client()
                .send_message(&claude_request)
                .await
                .map_err(|e| error_response(&e.to_string(), "api_error"))?;

            (response.content, "forward")
        }
        RouteDecision::Local { .. } => {
            info!("Routing to local model");

            // Check if model is ready
            use crate::models::GeneratorState;
            let state = server.generator_state().read().await;

            match &*state {
                GeneratorState::Ready { .. } => {
                    drop(state);

                    // Try local generation with tools
                    let mut generator = server.local_generator().write().await;
                    match generator.try_generate_with_tools(&internal_messages, internal_tools.clone()) {
                        Ok(Some(response)) => (response.content_blocks, "local"),
                        Ok(None) | Err(_) => {
                            // Fall back to Claude
                            drop(generator);
                            warn!("Local generation failed, falling back to Claude");

                            let mut claude_request =
                                crate::claude::MessageRequest::with_context(internal_messages.clone());
                            if let Some(tools) = internal_tools.clone() {
                                claude_request = claude_request.with_tools(tools);
                            }

                            let response = server
                                .claude_client()
                                .send_message(&claude_request)
                                .await
                                .map_err(|e| error_response(&e.to_string(), "api_error"))?;

                            (response.content, "fallback")
                        }
                    }
                }
                _ => {
                    // Model not ready, forward to Claude
                    drop(state);
                    info!("Model not ready, forwarding to Claude");

                    let mut claude_request =
                        crate::claude::MessageRequest::with_context(internal_messages.clone());
                    if let Some(tools) = internal_tools {
                        claude_request = claude_request.with_tools(tools);
                    }

                    let response = server
                        .claude_client()
                        .send_message(&claude_request)
                        .await
                        .map_err(|e| error_response(&e.to_string(), "api_error"))?;

                    (response.content, "forward")
                }
            }
        }
    };

    let elapsed = start_time.elapsed();
    info!(
        routing = routing_decision,
        elapsed_ms = elapsed.as_millis(),
        "Chat completion handled"
    );

    // Convert internal response to OpenAI format (handles tool_calls)
    let openai_response = convert_response_to_openai(content_blocks, &request.model)?;

    Ok(Json(openai_response))
}

/// Handle GET /v1/models - List available models
pub async fn handle_list_models() -> Json<ModelsResponse> {
    Json(ModelsResponse {
        object: "list".to_string(),
        data: vec![Model {
            id: "qwen-local".to_string(),
            object: "model".to_string(),
            created: 1672531200, // Arbitrary timestamp
            owned_by: "local".to_string(),
        }],
    })
}

/// Convert OpenAI tools to internal format
fn convert_tools_to_internal(tools: &[Tool]) -> Vec<InternalToolDefinition> {
    tools
        .iter()
        .map(|t| {
            // Extract schema components from parameters
            let (schema_type, properties, required) = if let Some(obj) = t.function.parameters.as_object() {
                let schema_type = obj.get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("object")
                    .to_string();

                let properties = obj.get("properties")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));

                let required = obj.get("required")
                    .and_then(|r| r.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                (schema_type, properties, required)
            } else {
                ("object".to_string(), serde_json::json!({}), vec![])
            };

            InternalToolDefinition {
                name: t.function.name.clone(),
                description: t.function.description.clone().unwrap_or_default(),
                input_schema: ToolInputSchema {
                    schema_type,
                    properties,
                    required,
                },
            }
        })
        .collect()
}

/// Convert internal GeneratorResponse to OpenAI format
fn convert_response_to_openai(
    content_blocks: Vec<ContentBlock>,
    model: &str,
) -> Result<ChatCompletionResponse, Response> {
    let mut message_content: Option<String> = None;
    let mut tool_calls: Option<Vec<ToolCall>> = None;
    let mut finish_reason = "stop";

    // Process content blocks
    for block in content_blocks {
        match block {
            ContentBlock::Text { text } => {
                message_content = Some(text);
            }
            ContentBlock::ToolUse { id, name, input } => {
                // Convert to OpenAI ToolCall
                let tool_call = ToolCall {
                    id: id.clone(),
                    tool_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: serde_json::to_string(&input)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                };

                tool_calls
                    .get_or_insert_with(Vec::new)
                    .push(tool_call);

                finish_reason = "tool_calls";
            }
            ContentBlock::ToolResult { .. } => {
                // Tool results shouldn't appear in assistant responses
                // They're in user messages
            }
        }
    }

    let response = ChatCompletionResponse {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
        object: "chat.completion".to_string(),
        created: chrono::Utc::now().timestamp(),
        model: model.to_string(),
        choices: vec![Choice {
            index: 0,
            message: ChatMessage {
                role: "assistant".to_string(),
                content: message_content,
                tool_calls,
                tool_call_id: None,
                name: None,
            },
            finish_reason: finish_reason.to_string(),
        }],
        usage: Usage {
            prompt_tokens: 0, // TODO: Calculate actual counts
            completion_tokens: 0,
            total_tokens: 0,
        },
    };

    Ok(response)
}

/// Convert OpenAI messages to internal format
/// Handles text content, tool calls, and tool results
fn convert_messages_to_internal(messages: &[ChatMessage]) -> anyhow::Result<Vec<Message>> {
    let mut result = Vec::new();

    for msg in messages {
        let mut content_blocks = Vec::new();

        // Handle text content
        if let Some(text) = &msg.content {
            if !text.is_empty() {
                content_blocks.push(ContentBlock::Text {
                    text: text.clone(),
                });
            }
        }

        // Handle tool calls (assistant messages)
        if let Some(tool_calls) = &msg.tool_calls {
            for tool_call in tool_calls {
                if tool_call.tool_type == "function" {
                    let input: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
                        .unwrap_or(serde_json::json!({}));

                    content_blocks.push(ContentBlock::ToolUse {
                        id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        input,
                    });
                }
            }
        }

        // Handle tool results (tool role messages)
        // In Claude API, tool results MUST be in user messages, not separate "tool" role
        if msg.role == "tool" {
            if let (Some(tool_call_id), Some(content)) = (&msg.tool_call_id, &msg.content) {
                content_blocks.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                    is_error: None,
                });
            }
        }

        // Only add message if it has content
        if !content_blocks.is_empty() {
            // Convert "tool" role to "user" for Claude API compatibility
            let role = if msg.role == "tool" {
                "user".to_string()
            } else {
                msg.role.clone()
            };

            result.push(Message {
                role,
                content: content_blocks,
            });
        }
    }

    Ok(result)
}

/// Create error response
fn error_response(message: &str, error_type: &str) -> Response {
    let error = ErrorResponse::new(message.to_string(), error_type.to_string());
    (StatusCode::BAD_REQUEST, Json(error)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_messages() {
        let openai_messages = vec![
            ChatMessage::system("You are a helpful assistant"),
            ChatMessage::user("Hello"),
        ];

        let internal = convert_messages_to_internal(&openai_messages).unwrap();
        assert_eq!(internal.len(), 2);
        assert_eq!(internal[0].role, "system");
        assert_eq!(internal[1].role, "user");
    }
}
