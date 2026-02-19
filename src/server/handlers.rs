// HTTP request handlers

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::AgentServer;
use crate::claude::{ContentBlock, Message};

/// Create the main application router
pub fn create_router(server: Arc<AgentServer>) -> Router {
    use super::feedback_handler::{handle_feedback, handle_training_status};
    use super::openai_handlers::{handle_chat_completions, handle_list_models};

    // Get training sender for feedback endpoint
    let training_tx = Arc::clone(server.training_tx());

    // Create feedback router with training_tx state
    let feedback_router = Router::new()
        .route("/v1/feedback", post(handle_feedback))
        .route("/v1/training/status", post(handle_training_status))
        .with_state(training_tx);

    // Create main router with server state
    Router::new()
        // Claude-compatible endpoints
        .route("/v1/messages", post(handle_message))
        .route("/v1/session/:id", get(get_session).delete(delete_session))
        .route("/v1/status", get(get_status))
        // OpenAI-compatible endpoints
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/v1/models", get(handle_list_models))
        // Health and metrics
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_endpoint))
        .with_state(server)
        // Merge feedback router
        .merge(feedback_router)
}

/// Request body for /v1/messages endpoint (Claude-compatible)
#[derive(Debug, Deserialize)]
pub struct MessageRequest {
    /// Model to use (e.g., "claude-sonnet-4-5-20250929")
    pub model: String,
    /// Messages in conversation
    pub messages: Vec<Message>,
    /// Maximum tokens to generate
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// System prompt
    #[serde(default)]
    pub system: Option<String>,
    /// Session ID for conversation continuity
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Response body for /v1/messages endpoint (Claude-compatible)
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub response_type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: String,
    pub session_id: String,
}

/// Handle POST /v1/messages - Main chat endpoint
async fn handle_message(
    State(server): State<Arc<AgentServer>>,
    Json(request): Json<MessageRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    use crate::claude::MessageRequest as ClaudeRequest;
    use crate::metrics::{RequestMetric, ResponseComparison};
    use crate::router::RouteDecision;
    use std::time::Instant;

    let start_time = Instant::now();

    // Get or create session
    let mut session = server
        .session_manager()
        .get_or_create(request.session_id.as_deref())?;

    // Extract user message (last message should be user role)
    let user_message = request
        .messages
        .last()
        .ok_or_else(|| anyhow::anyhow!("No messages in request"))?;

    // Extract text content from the user message for routing
    let user_text = user_message.text();

    // Add to conversation history
    session.conversation.add_message(user_message.clone());

    // Process query through router
    let router = server.router().read().await;
    let decision = router.route(&user_text);

    let (response_text, routing_decision) = match decision {
        RouteDecision::Forward { reason } => {
            let reason_str = format!("{:?}", reason);
            tracing::info!(
                session_id = %session.id,
                reason = %reason_str,
                "Forwarding to Claude API"
            );

            // Build Claude API request with full conversation context
            let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());

            // Forward to Claude
            let response = server.claude_client().send_message(&claude_request).await?;

            // Extract text from response
            let text = response.text();

            (text, "forward".to_string())
        }
        RouteDecision::Local { .. } => {
            tracing::info!(session_id = %session.id, "Handling locally");

            // Check if local generator is ready
            use crate::models::GeneratorState;
            let state = server.generator_state().read().await;

            match &*state {
                GeneratorState::Ready { .. } => {
                    drop(state); // Release lock before generating

                    tracing::info!(session_id = %session.id, "Using local Qwen model");

                    // Use local generator (need write lock for try_generate)
                    let mut generator = server.local_generator().write().await;

                    match generator.try_generate_from_pattern(&user_text) {
                        Ok(Some(response_text)) => {
                            (response_text, "local".to_string())
                        }
                        Ok(None) => {
                            // Confidence too low, fall back to Claude
                            tracing::info!(
                                session_id = %session.id,
                                "Local confidence too low, falling back to Claude"
                            );
                            drop(generator); // Release lock

                            let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());
                            let response = server.claude_client().send_message(&claude_request).await?;
                            let text = response.text();

                            (text, "confidence_fallback".to_string())
                        }
                        Err(e) => {
                            tracing::warn!(
                                session_id = %session.id,
                                error = %e,
                                "Local generation failed, falling back to Claude"
                            );
                            drop(generator); // Release lock

                            // Fall back to Claude on error
                            let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());
                            let response = server.claude_client().send_message(&claude_request).await?;
                            let text = response.text();

                            (text, "local_error_fallback".to_string())
                        }
                    }
                }
                GeneratorState::Initializing | GeneratorState::Downloading { .. } | GeneratorState::Loading { .. } => {
                    tracing::info!(
                        session_id = %session.id,
                        "Model still loading, forwarding to Claude"
                    );
                    drop(state); // Release lock

                    // Model not ready yet, forward to Claude
                    let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());
                    let response = server.claude_client().send_message(&claude_request).await?;
                    let text = response.text();

                    (text, "loading_fallback".to_string())
                }
                GeneratorState::Failed { error } => {
                    tracing::warn!(
                        session_id = %session.id,
                        error = %error,
                        "Model failed to load, forwarding to Claude"
                    );
                    drop(state); // Release lock

                    // Model failed to load, forward to Claude
                    let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());
                    let response = server.claude_client().send_message(&claude_request).await?;
                    let text = response.text();

                    (text, "failed_fallback".to_string())
                }
                GeneratorState::NotAvailable => {
                    tracing::info!(
                        session_id = %session.id,
                        "Model not available, forwarding to Claude"
                    );
                    drop(state); // Release lock

                    // No model available, forward to Claude
                    let claude_request = ClaudeRequest::with_context(session.conversation.get_messages());
                    let response = server.claude_client().send_message(&claude_request).await?;
                    let text = response.text();

                    (text, "unavailable_fallback".to_string())
                }
            }
        }
    };

    let elapsed_ms = start_time.elapsed().as_millis() as u64;

    // Log metrics
    let query_hash = crate::metrics::MetricsLogger::hash_query(&user_text);
    let metric = RequestMetric::new(
        query_hash,
        routing_decision,
        None, // pattern_id
        None, // confidence
        None, // forward_reason
        elapsed_ms,
        ResponseComparison {
            local_response: None,
            claude_response: response_text.clone(),
            quality_score: 1.0,
            similarity_score: None,
            divergence: None,
        },
        None, // router_confidence
        None, // validator_confidence
    );
    server.metrics_logger().log(&metric)?;

    // Create assistant response message
    let assistant_message = Message::assistant(&response_text);

    // Add response to conversation history
    session.conversation.add_message(assistant_message);

    // Update session
    session.touch();
    server
        .session_manager()
        .update(&session.id, session.clone())?;

    // Build Claude-compatible response
    let response = MessageResponse {
        id: format!("msg_{}", uuid::Uuid::new_v4()),
        response_type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![ContentBlock::text(&response_text)],
        model: request.model,
        stop_reason: "end_turn".to_string(),
        session_id: session.id,
    };

    Ok(Json(response))
}

/// Handle GET /v1/session/:id - Retrieve session state
async fn get_session(
    State(server): State<Arc<AgentServer>>,
    Path(session_id): Path<String>,
) -> Result<Json<SessionInfo>, AppError> {
    let session = server.session_manager().get_or_create(Some(&session_id))?;

    let info = SessionInfo {
        id: session.id,
        created_at: session.created_at.to_rfc3339(),
        last_activity: session.last_activity.to_rfc3339(),
        message_count: session.conversation.message_count(),
    };

    Ok(Json(info))
}

/// Session information
#[derive(Debug, Serialize)]
pub struct SessionInfo {
    pub id: String,
    pub created_at: String,
    pub last_activity: String,
    pub message_count: usize,
}

/// Handle DELETE /v1/session/:id - Delete session
async fn delete_session(
    State(server): State<Arc<AgentServer>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, AppError> {
    if server.session_manager().delete(&session_id) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError(anyhow::anyhow!("Session not found")))
    }
}

/// Generator status information
#[derive(Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GeneratorStatus {
    Initializing,
    Downloading {
        model_size: String,
        file_name: String,
        current_file: usize,
        total_files: usize,
    },
    Loading {
        model_size: String,
    },
    Ready {
        model_size: String,
    },
    Failed {
        error: String,
    },
    NotAvailable,
}

/// Status response
#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub generator: GeneratorStatus,
    pub active_sessions: usize,
    pub training_enabled: bool,
}

/// Handle GET /v1/status - Get server and model status
async fn get_status(
    State(server): State<Arc<AgentServer>>,
) -> Result<Json<StatusResponse>, AppError> {
    use crate::models::GeneratorState;

    let state = server.generator_state().read().await;

    let generator_status = match &*state {
        GeneratorState::Initializing => GeneratorStatus::Initializing,
        GeneratorState::Downloading {
            model_name,
            progress,
        } => GeneratorStatus::Downloading {
            model_size: model_name.clone(),
            file_name: progress.file_name.clone(),
            current_file: progress.current_file,
            total_files: progress.total_files,
        },
        GeneratorState::Loading { model_name } => GeneratorStatus::Loading {
            model_size: model_name.clone(),
        },
        GeneratorState::Ready { model_name, .. } => GeneratorStatus::Ready {
            model_size: model_name.clone(),
        },
        GeneratorState::Failed { error } => GeneratorStatus::Failed {
            error: error.clone(),
        },
        GeneratorState::NotAvailable => GeneratorStatus::NotAvailable,
    };

    let response = StatusResponse {
        generator: generator_status,
        active_sessions: server.session_manager().active_count(),
        training_enabled: true, // LoRA training is always enabled
    };

    Ok(Json(response))
}

/// Health check response
#[derive(Debug, Serialize)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_seconds: u64,
    pub active_sessions: usize,
}

/// Handle GET /health - Health check endpoint
pub async fn health_check(
    State(server): State<Arc<AgentServer>>,
) -> Result<Json<HealthStatus>, AppError> {
    // TODO: Track actual uptime
    let status = HealthStatus {
        status: "healthy".to_string(),
        uptime_seconds: 0, // Placeholder
        active_sessions: server.session_manager().active_count(),
    };

    Ok(Json(status))
}

/// Handle GET /metrics - Prometheus metrics endpoint
pub async fn metrics_endpoint(
    State(_server): State<Arc<AgentServer>>,
) -> Result<Response, AppError> {
    // TODO: Implement Prometheus metrics
    let metrics = "# HELP finch_queries_total Total number of queries\n\
                   # TYPE finch_queries_total counter\n\
                   finch_queries_total 0\n";

    Ok((StatusCode::OK, metrics).into_response())
}

/// Application error wrapper for proper HTTP error responses
pub struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!(error = %self.0, "Request failed");

        let error_message = self.0.to_string();
        let body = serde_json::json!({
            "error": {
                "message": error_message,
                "type": "api_error"
            }
        });

        (StatusCode::INTERNAL_SERVER_ERROR, Json(body)).into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
