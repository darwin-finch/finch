// OpenAI-compatible API handlers
//
// Implements /v1/chat/completions and /v1/models endpoints
// with format conversion between OpenAI and internal types.

use axum::{
    extract::State,
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Json, Response,
    },
};
use futures::stream::{self};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

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

/// Token buffer for accumulating and cleaning streamed tokens
struct TokenBuffer {
    /// Accumulated tokens since last flush
    tokens: Vec<String>,
    /// Characters already sent to client (for incremental cleaning)
    sent_prefix: String,
    /// Cached adapter for this generation session
    adapter: Option<Box<dyn crate::models::adapters::LocalModelAdapter>>,
    /// Partial marker being accumulated (e.g., "<|im_")
    partial_marker: String,
}

impl TokenBuffer {
    fn new() -> Self {
        Self {
            tokens: Vec::new(),
            sent_prefix: String::new(),
            adapter: None,
            partial_marker: String::new(),
        }
    }

    fn add_token(&mut self, token: &str) -> Option<String> {
        // Check for partial special token marker
        if self.is_start_of_special_marker(token) {
            self.partial_marker.push_str(token);
            return None; // Wait for more tokens
        }

        // If we have a partial marker, check if this completes it
        if !self.partial_marker.is_empty() {
            self.partial_marker.push_str(token);

            // Check if marker is complete (ends with |> or >)
            if self.partial_marker.ends_with("|>") || self.partial_marker.ends_with(">") {
                // Complete marker - discard it and continue
                self.partial_marker.clear();
                return None;
            } else {
                return None; // Still accumulating marker
            }
        }

        // Normal token - add to buffer
        self.tokens.push(token.to_string());

        // Flush every 10 tokens
        if self.tokens.len() >= 10 {
            Some(self.flush())
        } else {
            None
        }
    }

    fn is_start_of_special_marker(&self, token: &str) -> bool {
        const MARKER_STARTS: &[&str] = &[
            "<|",
            "<｜",
            "<think",
            "</think",
            "user\n",
            "system\n",
            "assistant\n",
        ];
        MARKER_STARTS.iter().any(|start| token.starts_with(start))
    }

    fn flush(&mut self) -> String {
        if self.tokens.is_empty() {
            return String::new();
        }

        // Concatenate accumulated tokens
        let accumulated: String = self.tokens.join("");
        let full_text = format!("{}{}", self.sent_prefix, accumulated);

        // Apply cleaning using adapter
        let cleaned = if let Some(adapter) = &self.adapter {
            // Check for tool XML - skip cleaning if present
            if full_text.contains("<tool_use>") || full_text.contains("<tool_result>") {
                full_text.clone() // Preserve tool XML
            } else {
                adapter.clean_output(&full_text)
            }
        } else {
            // No adapter - basic cleaning
            self.basic_clean(&full_text)
        };

        // Calculate what's new (incremental)
        let new_content = if cleaned.starts_with(&self.sent_prefix) {
            cleaned[self.sent_prefix.len()..].to_string()
        } else {
            // Cleaning changed earlier content - send full cleaned text
            self.sent_prefix.clear();
            cleaned.clone()
        };

        // Update state
        self.sent_prefix = cleaned;
        self.tokens.clear();

        new_content
    }

    fn basic_clean(&self, text: &str) -> String {
        // Fallback cleaning when no adapter available
        text.replace("<|im_end|>", "")
            .replace("<|endoftext|>", "")
            .replace("<｜end▁of▁sentence｜>", "")
            .trim()
            .to_string()
    }
}

/// Buffer and clean tokens before sending to SSE stream
async fn buffer_and_clean_tokens(
    mut token_rx: mpsc::Receiver<String>,
    cleaned_tx: mpsc::Sender<String>,
    adapter: Option<Box<dyn crate::models::adapters::LocalModelAdapter>>,
) {
    let mut buffer = TokenBuffer::new();
    buffer.adapter = adapter;

    while let Some(token) = token_rx.recv().await {
        if let Some(cleaned) = buffer.add_token(&token) {
            if cleaned_tx.send(cleaned).await.is_err() {
                break; // Client disconnected
            }
        }
    }

    // Final flush when generation ends
    let final_chunk = buffer.flush();
    if !final_chunk.is_empty() {
        let _ = cleaned_tx.send(final_chunk).await;
    }
}

/// Handle streaming chat completions (SSE)
async fn handle_chat_completions_streaming(
    server: Arc<AgentServer>,
    request: ChatCompletionRequest,
) -> Result<Response, Response> {
    // Validate request
    if request.messages.is_empty() {
        return Err(error_response(
            "messages array cannot be empty",
            "invalid_request_error",
        ));
    }

    // Check if local-only mode requested
    if !request.local_only.unwrap_or(false) {
        return Err(error_response(
            "streaming is only supported for local-only queries",
            "invalid_request_error",
        ));
    }

    // Convert OpenAI messages to internal format
    let internal_messages = convert_messages_to_internal(&request.messages)
        .map_err(|e| error_response(&e.to_string(), "invalid_request_error"))?;

    // Check generator state
    use crate::models::GeneratorState;
    let state = server.generator_state().read().await;

    match &*state {
        GeneratorState::Ready { .. } => {
            // Model ready, proceed
        }
        _ => {
            return Err(error_response(
                "Local model not ready for streaming",
                "model_not_ready",
            ));
        }
    }
    drop(state);

    // Create bounded channel for streaming tokens with backpressure
    // Buffer size of 2 allows one token to be consumed while another is being generated
    let (tx, rx) = mpsc::channel::<String>(2);

    // Create cleaned token channel
    let (cleaned_tx, cleaned_rx) = mpsc::channel::<String>(2);

    let model_name = request.model.clone();

    // Get model adapter for cleaning
    let model_adapter = {
        let gen = server.local_generator().read().await;
        Some(gen.get_adapter())
    };

    // Spawn buffering + cleaning task
    tokio::spawn(async move {
        buffer_and_clean_tokens(rx, cleaned_tx, model_adapter).await;
    });

    // Spawn generation task on blocking thread pool
    // ONNX generation is CPU-bound and synchronous, so we use spawn_blocking
    // to avoid blocking the async runtime. The bounded channel provides natural
    // backpressure - generation will pause if the HTTP stream can't keep up.
    let server_clone = server.clone();
    tokio::spawn(async move {
        // Run CPU-bound generation on blocking thread pool
        let result = tokio::task::spawn_blocking(move || {
            // Create runtime handle for async operations inside blocking context
            let handle = tokio::runtime::Handle::current();

            // Get generator (need to use block_on since we're in blocking context)
            let mut generator =
                handle.block_on(async { server_clone.local_generator().write().await });

            // Accumulate response for logging
            let accumulated_response = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let accumulated_clone = accumulated_response.clone();

            // Try to generate with streaming callback
            let result = generator.try_generate_from_pattern_streaming(
                &internal_messages,
                move |_token_id, token_text| {
                    tracing::debug!("[daemon] Sending token to SSE: {:?}", token_text);

                    // Accumulate for logging
                    if let Ok(mut acc) = accumulated_clone.lock() {
                        acc.push_str(token_text);
                    }

                    // Send token via bounded channel (blocking send)
                    // This provides backpressure - if the HTTP consumer is slow,
                    // generation will pause here until there's space in the channel
                    if tx.blocking_send(token_text.to_string()).is_err() {
                        // Channel closed (client disconnected), stop generating
                        return;
                    }

                    // Small sleep to pace token delivery and allow async runtime to process
                    // This helps prevent tokens from bunching up even with backpressure
                    std::thread::sleep(std::time::Duration::from_millis(10));
                },
            );

            // Log complete response
            if let Ok(acc) = accumulated_response.lock() {
                info!(
                    "[DAEMON_RESPONSE] Complete response ({} chars): {:?}",
                    acc.len(),
                    &acc
                );
            }

            result
        })
        .await;

        match result {
            Ok(Ok(Some(_response))) => {
                info!("✓ Streaming generation completed");
            }
            Ok(Ok(None)) => {
                warn!("❌ Streaming generation returned None");
            }
            Ok(Err(e)) => {
                warn!("❌ Streaming generation error: {}", e);
            }
            Err(e) => {
                warn!("❌ Blocking task error: {}", e);
            }
        }
    });

    // Create SSE stream from cleaned token receiver
    // State: (receiver, model_name, done_flag)
    let stream = stream::unfold(
        (cleaned_rx, model_name, false),
        |(mut rx, model_name, done)| async move {
            if done {
                // Already sent final chunk, terminate stream
                return None;
            }

            match rx.recv().await {
                Some(token_text) => {
                    // Format as OpenAI streaming chunk
                    let chunk = serde_json::json!({
                        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                        "object": "chat.completion.chunk",
                        "created": chrono::Utc::now().timestamp(),
                        "model": &model_name,
                        "choices": [{
                            "index": 0,
                            "delta": {
                                "content": token_text
                            },
                            "finish_reason": null
                        }]
                    });

                    Some((
                        Ok::<_, Infallible>(Event::default().json_data(chunk).unwrap()),
                        (rx, model_name, false), // Continue streaming
                    ))
                }
                None => {
                    // Send final chunk with finish_reason
                    let chunk = serde_json::json!({
                        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4()),
                        "object": "chat.completion.chunk",
                        "created": chrono::Utc::now().timestamp(),
                        "model": &model_name,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "stop"
                        }]
                    });

                    Some((
                        Ok::<_, Infallible>(Event::default().json_data(chunk).unwrap()),
                        (rx, model_name, true), // Mark done, will terminate on next call
                    ))
                }
            }
        },
    );

    Ok(Sse::new(stream).into_response())
}

/// Resolve the cloud provider to use, preferring the multi-provider pool.
///
/// Falls back to `claude_client` (wrapped in a throw-away `ClaudeClient`) when
/// `providers` is empty. Returns `None` only when the fallback also fails.
async fn forward_to_cloud(
    server: &AgentServer,
    provider_name: Option<&str>,
    messages: Vec<crate::claude::Message>,
    tools: Option<Vec<InternalToolDefinition>>,
) -> anyhow::Result<Vec<crate::claude::ContentBlock>> {
    if let Some(provider) = server.provider_for_name(provider_name) {
        let mut req = crate::providers::ProviderRequest::new(messages);
        if let Some(tools) = tools {
            req = req.with_tools(tools);
        }
        let resp = provider.send_message(&req).await?;
        Ok(resp.content)
    } else {
        // No providers configured — fall back to legacy ClaudeClient
        let mut claude_request = crate::claude::MessageRequest::with_context(messages);
        if let Some(tools) = tools {
            claude_request = claude_request.with_tools(tools);
        }
        let resp = server.claude_client().send_message(&claude_request).await?;
        Ok(resp.content)
    }
}

/// Handle POST /v1/chat/completions - OpenAI-compatible chat endpoint
pub async fn handle_chat_completions(
    State(server): State<Arc<AgentServer>>,
    headers: axum::http::HeaderMap,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let start_time = Instant::now();

    // Extract optional provider name from request header
    let provider_name: Option<String> = headers
        .get("x-finch-provider")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    // Validate request
    if request.messages.is_empty() {
        return error_response("messages array cannot be empty", "invalid_request_error");
    }

    // Handle streaming requests
    if request.stream {
        match handle_chat_completions_streaming(server, request).await {
            Ok(response) => return response,
            Err(error_resp) => return error_resp,
        }
    }

    // Check if local-only mode requested
    if request.local_only.unwrap_or(false) {
        match handle_local_only_query(server, request).await {
            Ok(json_resp) => return json_resp.into_response(),
            Err(error_resp) => return error_resp,
        }
    }

    // Convert OpenAI messages to internal format (now handles tool calls/results)
    let internal_messages = match convert_messages_to_internal(&request.messages) {
        Ok(messages) => messages,
        Err(e) => return error_response(&e.to_string(), "invalid_request_error"),
    };

    // Convert OpenAI tools to internal format
    let internal_tools = request
        .tools
        .as_ref()
        .map(|tools| convert_tools_to_internal(tools));

    // Extract user query for routing
    let user_query = request
        .messages
        .iter()
        .rfind(|m| m.role == "user")
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");

    // Route decision
    let router = server.router().read().await;
    let decision = router.route(user_query);
    drop(router);

    let (content_blocks, routing_decision) = match decision {
        RouteDecision::Forward { reason } => {
            info!(
                "☁️  ROUTING TO TEACHER API (reason: {:?}, provider: {:?})",
                reason, provider_name
            );

            match forward_to_cloud(
                &server,
                provider_name.as_deref(),
                internal_messages.clone(),
                internal_tools.clone(),
            )
            .await
            {
                Ok(blocks) => (blocks, "forward"),
                Err(e) => return error_response(&e.to_string(), "api_error"),
            }
        }
        RouteDecision::Local { .. } => {
            info!("🤖 ROUTING TO LOCAL MODEL");

            // Check if model is ready
            use crate::models::GeneratorState;
            let state = server.generator_state().read().await;

            match &*state {
                GeneratorState::Ready { .. } => {
                    drop(state);

                    // Try local generation with tools
                    let mut generator = server.local_generator().write().await;
                    match generator.try_generate_from_pattern_with_tools(
                        &internal_messages,
                        internal_tools.clone(),
                    ) {
                        Ok(Some(response)) => {
                            info!("✓ LOCAL MODEL RESPONDED");
                            (response.content_blocks, "local")
                        }
                        Ok(None) => {
                            drop(generator);
                            warn!("❌ Local generation returned None, falling back to teacher");
                            match forward_to_cloud(
                                &server,
                                provider_name.as_deref(),
                                internal_messages.clone(),
                                internal_tools.clone(),
                            )
                            .await
                            {
                                Ok(blocks) => (blocks, "fallback"),
                                Err(e) => return error_response(&e.to_string(), "api_error"),
                            }
                        }
                        Err(e) => {
                            drop(generator);
                            warn!("❌ Local generation error: {}, falling back to teacher", e);
                            match forward_to_cloud(
                                &server,
                                provider_name.as_deref(),
                                internal_messages.clone(),
                                internal_tools,
                            )
                            .await
                            {
                                Ok(blocks) => (blocks, "fallback"),
                                Err(e2) => return error_response(&e2.to_string(), "api_error"),
                            }
                        }
                    }
                }
                _ => {
                    drop(state);
                    info!("Model not ready, forwarding to cloud provider");
                    match forward_to_cloud(
                        &server,
                        provider_name.as_deref(),
                        internal_messages.clone(),
                        internal_tools,
                    )
                    .await
                    {
                        Ok(blocks) => (blocks, "forward"),
                        Err(e) => return error_response(&e.to_string(), "api_error"),
                    }
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

    // Automatically collect query/response for training (if not a tool call)
    if !has_tool_calls(&content_blocks) {
        let response_text = extract_text_from_blocks(&content_blocks);
        if !user_query.is_empty() && !response_text.is_empty() {
            // Send to training queue (non-blocking)
            let training_tx = server.training_tx();
            let example = crate::models::WeightedExample {
                query: user_query.to_string(),
                response: response_text,
                weight: 1.0,    // Normal weight for automatic collection
                feedback: None, // No explicit feedback for auto-collected examples
            };

            if let Err(e) = training_tx.send(example) {
                warn!("Failed to send example to training queue: {}", e);
            } else {
                debug!("Auto-collected query/response for training");
            }
        }
    }

    // Convert internal response to OpenAI format (handles tool_calls)
    let openai_response = match convert_response_to_openai(content_blocks, &request.model) {
        Ok(resp) => resp,
        Err(error_resp) => return error_resp,
    };

    Json(openai_response).into_response()
}

/// Handle local-only query (bypass routing, direct local model access)
async fn handle_local_only_query(
    server: Arc<AgentServer>,
    request: ChatCompletionRequest,
) -> Result<Json<ChatCompletionResponse>, Response> {
    use crate::models::GeneratorState;

    info!("Local-only query (bypassing routing)");

    // Check generator state
    let state = server.generator_state().read().await;

    match &*state {
        GeneratorState::Ready { .. } => {
            // Model ready, proceed (state will be dropped at end of scope)
        }
        GeneratorState::Initializing
        | GeneratorState::Downloading { .. }
        | GeneratorState::Loading { .. } => {
            warn!("Local model not ready: {:?}", &*state);
            return Err((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse::new(
                    "Local model not ready (initializing/downloading/loading)".to_string(),
                    "model_not_ready".to_string(),
                )),
            )
                .into_response());
        }
        GeneratorState::Failed { ref error } => {
            warn!("Local model failed: {}", error);
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse::new(
                    format!("Local model failed: {}", error),
                    "model_failed".to_string(),
                )),
            )
                .into_response());
        }
        GeneratorState::NotAvailable => {
            return Err((
                StatusCode::NOT_IMPLEMENTED,
                Json(ErrorResponse::new(
                    "Local model not available".to_string(),
                    "model_not_available".to_string(),
                )),
            )
                .into_response());
        }
    }
    // State dropped here automatically

    // Extract query from messages
    let internal_messages = convert_messages_to_internal(&request.messages)
        .map_err(|e| error_response(&e.to_string(), "invalid_request_error"))?;

    // Generate response (no tools for now - direct generation only)
    info!("Acquiring write lock on generator...");
    let mut generator = server.local_generator().write().await;
    info!("Write lock acquired, starting generation...");

    let content_blocks =
        match generator.try_generate_from_pattern_with_tools(&internal_messages, None) {
            Ok(Some(response)) => {
                info!(
                    "Generation successful, {} content blocks",
                    response.content_blocks.len()
                );
                response.content_blocks
            }
            Ok(None) => {
                warn!("Generation returned None");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new(
                        "Local model returned no response".to_string(),
                        "generation_failed".to_string(),
                    )),
                )
                    .into_response());
            }
            Err(e) => {
                warn!("Local generation failed: {}", e);
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(ErrorResponse::new(
                        format!("Local generation failed: {}", e),
                        "generation_failed".to_string(),
                    )),
                )
                    .into_response());
            }
        };
    drop(generator);
    info!("Write lock dropped");

    // Convert response to OpenAI format
    info!("Converting response to OpenAI format...");
    let openai_response = convert_response_to_openai(content_blocks, &request.model)?;
    info!("Response converted, sending back to client");

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
            let (schema_type, properties, required) =
                if let Some(obj) = t.function.parameters.as_object() {
                    let schema_type = obj
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("object")
                        .to_string();

                    let properties = obj
                        .get("properties")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({}));

                    let required = obj
                        .get("required")
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
#[allow(clippy::result_large_err)]
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

                tool_calls.get_or_insert_with(Vec::new).push(tool_call);

                finish_reason = "tool_calls";
            }
            ContentBlock::ToolResult { .. } => {
                // Tool results shouldn't appear in assistant responses
                // They're in user messages
            }
            ContentBlock::Image { .. } => {
                // Image blocks: not applicable in this context
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

/// Convert OpenAI messages to internal format.
///
/// Consecutive `role: "tool"` messages are batched into a single internal
/// `role: "user"` message containing all `ToolResult` blocks, as required by
/// the Claude API.
fn convert_messages_to_internal(messages: &[ChatMessage]) -> anyhow::Result<Vec<Message>> {
    let mut result: Vec<Message> = Vec::new();

    for msg in messages {
        // --- tool role: collect results ---
        if msg.role == "tool" {
            if let (Some(tool_call_id), Some(content)) = (&msg.tool_call_id, &msg.content) {
                let result_block = ContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: content.clone(),
                    is_error: None,
                };

                // Append to the previous user message if it already contains ToolResult
                // blocks (batch all results from one turn into a single user message).
                let appended = if let Some(last) = result.last_mut() {
                    if last.role == "user"
                        && last
                            .content
                            .iter()
                            .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
                    {
                        last.content.push(result_block.clone());
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                if !appended {
                    result.push(Message {
                        role: "user".to_string(),
                        content: vec![result_block],
                    });
                }
            }
            continue;
        }

        // --- all other roles ---
        let mut content_blocks = Vec::new();

        // Text content
        if let Some(text) = &msg.content {
            if !text.is_empty() {
                content_blocks.push(ContentBlock::Text { text: text.clone() });
            }
        }

        // Tool calls (assistant messages)
        if let Some(tool_calls) = &msg.tool_calls {
            for tool_call in tool_calls {
                if tool_call.tool_type == "function" {
                    let input: serde_json::Value =
                        serde_json::from_str(&tool_call.function.arguments)
                            .unwrap_or(serde_json::json!({}));

                    content_blocks.push(ContentBlock::ToolUse {
                        id: tool_call.id.clone(),
                        name: tool_call.function.name.clone(),
                        input,
                    });
                }
            }
        }

        if !content_blocks.is_empty() {
            result.push(Message {
                role: msg.role.clone(),
                content: content_blocks,
            });
        }
    }

    Ok(result)
}

/// Check if content blocks contain tool calls
fn has_tool_calls(blocks: &[ContentBlock]) -> bool {
    blocks
        .iter()
        .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
}

/// Extract text from content blocks
fn extract_text_from_blocks(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn test_token_buffer_basic() {
        let mut buffer = TokenBuffer::new();

        // Add 10 tokens to trigger flush
        for i in 0..9 {
            let result = buffer.add_token(&format!("token{} ", i));
            assert!(result.is_none(), "Should not flush before 10 tokens");
        }

        // 10th token should trigger flush
        let result = buffer.add_token("final");
        assert!(result.is_some(), "Should flush after 10 tokens");

        let flushed = result.unwrap();
        assert!(flushed.contains("token0"), "Should contain first token");
        assert!(flushed.contains("final"), "Should contain final token");
    }

    #[test]
    fn test_partial_marker_detection() {
        let mut buffer = TokenBuffer::new();

        // Start of ChatML marker
        assert!(
            buffer.add_token("<|").is_none(),
            "Should buffer start marker"
        );
        assert!(
            buffer.add_token("im_").is_none(),
            "Should continue buffering"
        );
        assert!(
            buffer.add_token("end|>").is_none(),
            "Should complete and discard marker"
        );

        // Normal token should be added
        buffer.add_token("normal");
        assert_eq!(buffer.tokens.len(), 1, "Should have 1 normal token");
    }

    #[test]
    fn test_basic_clean() {
        let buffer = TokenBuffer::new();

        let text = "<|im_end|>Hello world<|endoftext|>";
        let cleaned = buffer.basic_clean(text);

        assert_eq!(cleaned, "Hello world");
    }

    #[test]
    fn test_incremental_cleaning() {
        let mut buffer = TokenBuffer::new();

        // Add first batch
        for token in &["Hello", " ", "world", "!"] {
            buffer.add_token(token);
        }

        let first_flush = buffer.flush();
        assert_eq!(first_flush, "Hello world!");

        // Add second batch
        for token in &[" ", "How", " ", "are", " ", "you", "?"] {
            buffer.add_token(token);
        }

        let second_flush = buffer.flush();
        assert_eq!(
            second_flush, " How are you?",
            "Should only return new content"
        );
    }
}
