// HTTP request handlers

use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use uuid::Uuid;

/// Check `X-Finch-Token` header against this daemon's token.
/// Returns Ok(()) if valid, Err(StatusCode::FORBIDDEN) with a log entry if not.
fn check_peer_token(headers: &HeaderMap, peer_ip: &str, endpoint: &str) -> Result<(), Response> {
    let expected = &*crate::peer_token::TOKEN;
    match headers.get(crate::peer_token::HEADER) {
        Some(v) if v.as_bytes() == expected.as_bytes() => Ok(()),
        Some(_) => {
            tracing::warn!(ip = %peer_ip, endpoint, "rejected: wrong peer token");
            let notice = format!("\x1b[33m{}\x1b[0m tried to get in (wrong key)", peer_ip);
            let _ = PUSH_INBOX.send(notice);
            Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "wrong peer token"})),
            )
                .into_response())
        }
        None => {
            tracing::warn!(ip = %peer_ip, endpoint, "rejected: no peer token");
            let notice = format!("\x1b[33m{}\x1b[0m knocked ({})", peer_ip, endpoint);
            let _ = PUSH_INBOX.send(notice);
            Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "peer token required — set X-Finch-Token"})),
            )
                .into_response())
        }
    }
}

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
        // Node identity and work stats (distributed worker network)
        .route("/v1/node/info", get(handle_node_info))
        .route("/v1/node/stats", get(handle_node_stats))
        // Brain sessions
        .route("/v1/brains", post(spawn_brain).get(list_brains))
        .route("/v1/brains/:id", get(get_brain).delete(cancel_brain))
        .route("/v1/brains/:id/answer", post(answer_brain_question))
        .route("/v1/brains/:id/plan", post(respond_to_brain_plan))
        .route("/v1/brains/shared", get(list_shared_brains))
        .route(
            "/v1/brains/shared/:name",
            get(get_shared_brain).post(contribute_shared_brain),
        )
        // Note: node handlers load config independently (no AgentServer state needed)
        // Co-Forth remote eval and direct exec
        .route("/v1/forth/eval", post(handle_forth_eval))
        .route("/v1/forth/resume", post(handle_forth_resume))
        .route("/v1/forth/define", post(handle_forth_define))
        .route("/v1/forth/vocab", get(handle_forth_vocab))
        .route("/v1/forth/push", post(handle_forth_push))
        .route(
            "/v1/forth/hash",
            post(handle_forth_hash_set).get(handle_forth_hash_get),
        )
        .route("/v1/exec", post(handle_exec))
        // Co-Forth mutual execution sessions
        .route("/v1/forth/coforth", post(handle_coforth_create))
        .route("/v1/forth/coforth/:id", get(handle_coforth_get))
        .route("/v1/forth/coforth/:id/yield", post(handle_coforth_yield))
        .route("/v1/forth/coforth/:id/agree", post(handle_coforth_agree))
        // Channel contribution stacks
        .route("/v1/forth/channel/:name/contribute", post(handle_channel_contribute))
        .route("/v1/forth/channel/:name", get(handle_channel_get))
        .route("/v1/forth/channel/:name/execute", post(handle_channel_execute))
        .route("/v1/file/get", get(handle_file_get))
        .route("/v1/file/put", post(handle_file_put))
        // Peer registry
        .route("/v1/registry/join", post(handle_registry_join))
        .route("/v1/registry/leave", post(handle_registry_leave))
        .route("/v1/registry/heartbeat", post(handle_registry_heartbeat))
        .route("/v1/registry/peers", get(handle_registry_peers))
        .route("/v1/registry/ledger/:addr", get(handle_registry_ledger))
        .route("/v1/registry/ledgers", get(handle_registry_all_ledgers))
        .route("/v1/registry/debit", post(handle_registry_debit))
        .route("/v1/settle", post(handle_settle))
        .route("/v1/gas/transfer", post(handle_gas_transfer))
        // Session WebSocket — bidirectional event bus between two finch nodes
        .route("/v1/session/ws", get(handle_session_ws))
        // Cross-machine peer relay
        .route("/v1/session/relay-drain", get(handle_relay_drain))
        .route("/v1/session/relay-broadcast", post(handle_relay_broadcast))
        .route("/v1/peer/announced", get(handle_announced_peers))
        // Named session registry
        .route("/v1/session/join", post(handle_session_join))
        .route("/v1/session/list", get(handle_session_list))
        // Health and metrics
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_endpoint))
        .with_state(server)
        // Merge feedback router
        .merge(feedback_router)
}

// ---------------------------------------------------------------------------
// Brain route handlers
// ---------------------------------------------------------------------------

/// POST /v1/brains — spawn a new brain session
#[derive(Debug, Deserialize)]
#[allow(dead_code)] // `name` is reserved for future named-brain support
struct SpawnBrainRequest {
    task: String,
    #[serde(default)]
    name: Option<String>,
}

async fn spawn_brain(
    State(server): State<Arc<AgentServer>>,
    Json(req): Json<SpawnBrainRequest>,
) -> Result<Json<crate::server::brain_registry::BrainSummary>, AppError> {
    use crate::brain::daemon_brain::run_daemon_brain_loop;

    let id = uuid::Uuid::new_v4();
    let registry = Arc::clone(server.brain_registry());

    // Choose a provider (first available)
    let provider = server
        .provider_for_name(None)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("No provider configured for daemon brains"))?;

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~".to_string());

    let name = registry.insert(id, req.task.clone()).await;
    let _ = name; // name embedded in registry

    let registry_clone = Arc::clone(&registry);
    let task_clone = req.task.clone();
    let cwd_clone = cwd.clone();

    tokio::spawn(async move {
        run_daemon_brain_loop(id, task_clone, registry_clone, provider, cwd_clone).await;
    });

    let brains = registry.get_detail(id).await;
    let summary = brains
        .map(|d| crate::server::brain_registry::BrainSummary {
            id: d.id,
            name: d.name,
            task: d.task,
            state: d.state,
            age_secs: d.age_secs,
        })
        .ok_or_else(|| anyhow::anyhow!("Brain not found after spawn"))?;

    Ok(Json(summary))
}

/// GET /v1/brains — list active brains
async fn list_brains(
    State(server): State<Arc<AgentServer>>,
) -> Result<Json<Vec<crate::server::brain_registry::BrainSummary>>, AppError> {
    let list = server.brain_registry().list_active().await;
    Ok(Json(list))
}

/// GET /v1/brains/:id — full brain detail
async fn get_brain(
    State(server): State<Arc<AgentServer>>,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::server::brain_registry::BrainDetail>, AppError> {
    let detail = server
        .brain_registry()
        .get_detail(id)
        .await
        .ok_or_else(|| anyhow::anyhow!("Brain {} not found", id))?;
    Ok(Json(detail))
}

/// DELETE /v1/brains/:id — cancel a brain
async fn cancel_brain(
    State(server): State<Arc<AgentServer>>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    if server.brain_registry().cancel(id).await {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError(anyhow::anyhow!("Brain {} not found", id)))
    }
}

/// POST /v1/brains/:id/answer — answer a pending question
#[derive(Debug, Deserialize)]
struct AnswerRequest {
    answer: String,
}

async fn answer_brain_question(
    State(server): State<Arc<AgentServer>>,
    Path(id): Path<Uuid>,
    Json(req): Json<AnswerRequest>,
) -> Result<StatusCode, AppError> {
    server
        .brain_registry()
        .answer_question(id, req.answer)
        .await?;
    Ok(StatusCode::OK)
}

/// POST /v1/brains/:id/plan — respond to a pending plan
#[derive(Debug, Deserialize)]
struct PlanResponseRequest {
    action: String,
    #[serde(default)]
    feedback: Option<String>,
}

async fn respond_to_brain_plan(
    State(server): State<Arc<AgentServer>>,
    Path(id): Path<Uuid>,
    Json(req): Json<PlanResponseRequest>,
) -> Result<StatusCode, AppError> {
    use crate::server::brain_registry::PlanResponse;

    let response = match req.action.as_str() {
        "approve" => PlanResponse::Approve,
        "reject" => PlanResponse::Reject,
        "changes" | "changes_requested" => PlanResponse::ChangesRequested {
            feedback: req.feedback.unwrap_or_default(),
        },
        other => return Err(AppError(anyhow::anyhow!("Unknown plan action: {}", other))),
    };

    server
        .brain_registry()
        .respond_to_plan(id, response)
        .await?;
    Ok(StatusCode::OK)
}

/// GET /v1/brains/shared — list all shared brains (name, context, updated_at)
async fn list_shared_brains(
    State(server): State<Arc<AgentServer>>,
) -> Json<Vec<crate::server::brain_registry::SharedBrainEntry>> {
    Json(server.brain_registry().list_shared().await)
}

/// GET /v1/brains/shared/:name — return one shared brain
async fn get_shared_brain(
    State(server): State<Arc<AgentServer>>,
    Path(name): Path<String>,
) -> Result<Json<crate::server::brain_registry::SharedBrainEntry>, AppError> {
    server
        .brain_registry()
        .get_shared(&name)
        .await
        .map(Json)
        .ok_or_else(|| AppError(anyhow::anyhow!("Shared brain '{}' not found", name)))
}

/// POST /v1/brains/shared/:name — contribute context to a shared brain
#[derive(Debug, Deserialize)]
struct SharedBrainContribution {
    context: String,
}

async fn contribute_shared_brain(
    State(server): State<Arc<AgentServer>>,
    Path(name): Path<String>,
    Json(body): Json<SharedBrainContribution>,
) -> StatusCode {
    server
        .brain_registry()
        .contribute_shared(&name, &body.context)
        .await;
    StatusCode::OK
}

/// Request body for /v1/messages endpoint (Claude-compatible)
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
                        Ok(Some(response_text)) => (response_text, "local".to_string()),
                        Ok(None) => {
                            // Confidence too low, fall back to Claude
                            tracing::info!(
                                session_id = %session.id,
                                "Local confidence too low, falling back to Claude"
                            );
                            drop(generator); // Release lock

                            let claude_request =
                                ClaudeRequest::with_context(session.conversation.get_messages());
                            let response =
                                server.claude_client().send_message(&claude_request).await?;
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
                            let claude_request =
                                ClaudeRequest::with_context(session.conversation.get_messages());
                            let response =
                                server.claude_client().send_message(&claude_request).await?;
                            let text = response.text();

                            (text, "local_error_fallback".to_string())
                        }
                    }
                }
                GeneratorState::Initializing
                | GeneratorState::Downloading { .. }
                | GeneratorState::Loading { .. } => {
                    tracing::info!(
                        session_id = %session.id,
                        "Model still loading, forwarding to Claude"
                    );
                    drop(state); // Release lock

                    // Model not ready yet, forward to Claude
                    let claude_request =
                        ClaudeRequest::with_context(session.conversation.get_messages());
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
                    let claude_request =
                        ClaudeRequest::with_context(session.conversation.get_messages());
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
                    let claude_request =
                        ClaudeRequest::with_context(session.conversation.get_messages());
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

/// Handle GET /v1/node/info — return this node's identity and capabilities
pub async fn handle_node_info() -> Result<Json<serde_json::Value>, AppError> {
    use crate::config::load_config;
    use crate::node::NodeInfo;

    let has_teacher = load_config()
        .map(|c| c.active_teacher().is_some())
        .unwrap_or(false);
    let info = NodeInfo::load(has_teacher)?;
    Ok(Json(serde_json::to_value(&info)?))
}

/// Handle GET /v1/node/stats — return this node's work statistics
pub async fn handle_node_stats() -> Result<Json<serde_json::Value>, AppError> {
    use crate::node::WorkTracker;

    let stats = WorkTracker::load_persisted()?;
    Ok(Json(serde_json::to_value(&stats)?))
}

// ---------------------------------------------------------------------------
// Co-Forth remote eval endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ForthEvalRequest {
    code: String,
    /// Address of the requesting machine — used for ledger debit tracking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caller: Option<String>,
}

#[derive(Debug, Serialize)]
struct ForthEvalResponse {
    output: String,
    /// Data stack after execution (top of stack = last element).
    /// Allows the caller to push these values onto their own local stack.
    stack: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// Wall-clock milliseconds the execution took.  Used for ledger accounting.
    #[serde(default)]
    compute_ms: u64,
    /// Set when the caller's compute debt has crossed the threshold.
    /// The caller should arrange settlement before requesting more work.
    #[serde(skip_serializing_if = "Option::is_none")]
    debt_warning: Option<String>,
    /// Forth code the peer wants the caller to execute locally after this response.
    /// Set by `forth-back" <code>"` in the remote program.
    #[serde(skip_serializing_if = "Option::is_none")]
    forth_back: Option<String>,
    /// Suspended continuation captured at the last `yield` point.
    /// The caller can POST this to any peer's `/v1/forth/resume` to continue
    /// the computation there, or execute it locally with the `resume` builtin.
    #[serde(skip_serializing_if = "Option::is_none")]
    continuation: Option<crate::coforth::interpreter::Continuation>,
}

/// Global broadcast channel for incoming push messages.
/// The server writes to this when a peer calls POST /v1/forth/push.
/// The event loop subscribes and displays messages in the TUI.
pub static PUSH_INBOX: std::sync::LazyLock<tokio::sync::broadcast::Sender<String>> =
    std::sync::LazyLock::new(|| {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        tx
    });

/// Broadcast channel for incoming hash updates from peers.
/// Carries (room_id, key, value, from_name) tuples.
/// The event loop subscribes and applies updates to the local VM's rooms.
pub static HASH_INBOX: std::sync::LazyLock<
    tokio::sync::broadcast::Sender<(String, String, String, String)>,
> = std::sync::LazyLock::new(|| {
    let (tx, _) = tokio::sync::broadcast::channel(256);
    tx
});

/// Grammar-VM shared baseline: pre-compiled STDLIB + all grammar words, built once.
/// Each request clones this (O(dict size)) instead of recompiling from source.
static GRAMMAR_VM: std::sync::LazyLock<crate::coforth::Forth> = std::sync::LazyLock::new(|| {
    use crate::coforth::{Forth, Library};
    let mut vm = Forth::new();
    let lib = Library::load();
    vm.compile_library(&lib);
    vm
});

/// Monotonically increasing counter — incremented each time LIVE_VM vocabulary changes.
/// REPL sessions poll this to detect when another terminal defined new words.
static VOCAB_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Live VM — extends GRAMMAR_VM with published words from peers.
/// Persisted to ~/.finch/user_words.forth on every define call.
/// eval clones from here so scatter code can call published words.
static LIVE_VM: std::sync::LazyLock<std::sync::Arc<tokio::sync::RwLock<crate::coforth::Forth>>> =
    std::sync::LazyLock::new(|| {
        let mut vm = GRAMMAR_VM.clone_dict();
        // Load any words published in a prior session.
        if let Some(path) = user_words_path() {
            if let Ok(src) = std::fs::read_to_string(&path) {
                if !src.is_empty() {
                    let _ = vm.exec(&src);
                }
            }
        }
        std::sync::Arc::new(tokio::sync::RwLock::new(vm))
    });

fn user_words_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|mut p| {
        p.push(".finch");
        p.push("user_words.forth");
        p
    })
}

/// POST /v1/forth/eval — execute Forth code from a remote peer.
///
/// Clones from LIVE_VM so published words are available during scatter.
/// Any word in the vocabulary runs.  The VM is the boundary.
async fn handle_forth_eval(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ForthEvalRequest>,
) -> Result<Json<ForthEvalResponse>, Response> {
    let ip = addr.ip().to_string();
    if let Err(r) = check_peer_token(&headers, &ip, "/v1/forth/eval") {
        return Err(r);
    }
    handle_forth_eval_inner(req)
        .await
        .map_err(|e| AppError(e).into_response())
}

async fn handle_forth_eval_inner(req: ForthEvalRequest) -> anyhow::Result<Json<ForthEvalResponse>> {
    let base = LIVE_VM.read().await;
    let mut vm = base.clone_dict();
    drop(base);
    vm.remote_mode = true; // no dialogs, no AI calls on remote VMs
    let depth_before = vm.data_stack().len();
    let t0 = std::time::Instant::now();
    let result = vm.exec(&req.code);
    let compute_ms = t0.elapsed().as_millis() as u64;

    // Credit this machine for the work it just performed.
    if let Some(addr) = vm.registry_addr.clone() {
        REGISTRY.credit(&addr, compute_ms);
    }

    // Debit the caller and check if they've crossed the debt threshold.
    let debt_warning = if let Some(caller) = &req.caller {
        let (balance, crossed) = REGISTRY.debit(caller, compute_ms);
        if crossed {
            let threshold_s = REGISTRY.debt_threshold_ms as f64 / 1000.0;
            let balance_s = balance.abs() as f64 / 1000.0;
            Some(format!(
                "compute debt: {:.1}s owed (threshold {:.1}s) — please settle",
                balance_s, threshold_s
            ))
        } else if REGISTRY.is_in_debt(caller) {
            let balance_s = balance.abs() as f64 / 1000.0;
            Some(format!("compute debt: {:.1}s owed", balance_s))
        } else {
            None
        }
    } else {
        None
    };

    match result {
        Ok(output) => Ok(Json(ForthEvalResponse {
            output,
            stack: vm.data_stack()[depth_before..].to_vec(),
            error: None,
            compute_ms,
            debt_warning,
            forth_back: vm.forth_back.clone(),
            continuation: vm.pending_continuation.clone(),
        })),
        Err(e) => Ok(Json(ForthEvalResponse {
            output: vm.out.clone(),
            stack: vm.data_stack()[depth_before..].to_vec(),
            error: Some(e.to_string()),
            compute_ms,
            debt_warning,
            forth_back: vm.forth_back.clone(),
            continuation: vm.pending_continuation.clone(),
        })),
    }
}

// ── POST /v1/forth/resume ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ForthResumeRequest {
    continuation: crate::coforth::interpreter::Continuation,
    /// Address of the requesting machine — used for ledger debit tracking.
    #[serde(default)]
    caller: Option<String>,
}

/// POST /v1/forth/resume — resume a suspended Co-Forth continuation.
///
/// The caller supplies a `Continuation` (stack + code + string pool) that was
/// previously captured by a `yield` on any machine.  This machine restores the
/// stack, merges the string pool, and executes the remaining code under the
/// same constitutional constraints as a normal `/v1/forth/eval`.
///
/// **Danger**: the `code` field runs with the same authority as any peer eval.
/// Only resume continuations from peers you trust.
async fn handle_forth_resume(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ForthResumeRequest>,
) -> Result<Json<ForthEvalResponse>, Response> {
    let ip = addr.ip().to_string();
    if let Err(r) = check_peer_token(&headers, &ip, "/v1/forth/resume") {
        return Err(r);
    }
    handle_forth_resume_inner(req)
        .await
        .map_err(|e| AppError(e).into_response())
}

async fn handle_forth_resume_inner(req: ForthResumeRequest) -> anyhow::Result<Json<ForthEvalResponse>> {
    let base = LIVE_VM.read().await;
    let mut vm = base.clone_dict();
    drop(base);
    vm.remote_mode = true;

    let depth_before = 0; // report entire resulting stack
    let t0 = std::time::Instant::now();
    let result = vm.apply_continuation(&req.continuation);
    let compute_ms = t0.elapsed().as_millis() as u64;

    if let Some(ref addr) = vm.registry_addr.clone() {
        REGISTRY.credit(addr, compute_ms);
    }
    let debt_warning = if let Some(caller) = &req.caller {
        let (balance, crossed) = REGISTRY.debit(caller, compute_ms);
        if crossed {
            let threshold_s = REGISTRY.debt_threshold_ms as f64 / 1000.0;
            let balance_s = balance.abs() as f64 / 1000.0;
            Some(format!(
                "compute debt: {:.1}s owed (threshold {:.1}s) — please settle",
                balance_s, threshold_s
            ))
        } else if REGISTRY.is_in_debt(caller) {
            let balance_s = balance.abs() as f64 / 1000.0;
            Some(format!("compute debt: {:.1}s owed", balance_s))
        } else {
            None
        }
    } else {
        None
    };

    match result {
        Ok(()) => Ok(Json(ForthEvalResponse {
            output: vm.out.clone(),
            stack: vm.data_stack()[depth_before..].to_vec(),
            error: None,
            compute_ms,
            debt_warning,
            forth_back: vm.forth_back.clone(),
            continuation: vm.pending_continuation.clone(),
        })),
        Err(e) => Ok(Json(ForthEvalResponse {
            output: vm.out.clone(),
            stack: vm.data_stack()[depth_before..].to_vec(),
            error: Some(e.to_string()),
            compute_ms,
            debt_warning,
            forth_back: vm.forth_back.clone(),
            continuation: vm.pending_continuation.clone(),
        })),
    }
}

#[derive(Debug, Deserialize)]
struct ForthDefineRequest {
    source: String,
}

/// POST /v1/forth/define — receive published word definitions from a peer.
///
/// Compiles the source into LIVE_VM and persists it to ~/.finch/user_words.forth
/// so the words survive daemon restarts and are available in all future eval requests.
async fn handle_forth_define(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ForthDefineRequest>,
) -> Result<Json<ForthEvalResponse>, Response> {
    let ip = addr.ip().to_string();
    if let Err(r) = check_peer_token(&headers, &ip, "/v1/forth/define") {
        return Err(r);
    }
    let mut live = LIVE_VM.write().await;
    match live.exec(&req.source) {
        Ok(output) => {
            // Persist accumulated vocabulary to disk.
            if let Some(path) = user_words_path() {
                let src = live.dump_source();
                if !src.is_empty() {
                    let _ = std::fs::create_dir_all(path.parent().unwrap());
                    let _ = std::fs::write(&path, src);
                }
            }
            // Signal all polling REPL sessions that the vocabulary changed.
            VOCAB_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Json(ForthEvalResponse {
                output,
                stack: Vec::new(),
                error: None,
                compute_ms: 0,
                debt_warning: None,
                forth_back: None,
                continuation: None,
            }))
        }
        Err(e) => Ok(Json(ForthEvalResponse {
            output: live.out.clone(),
            stack: Vec::new(),
            error: Some(e.to_string()),
            compute_ms: 0,
            debt_warning: None,
            forth_back: None,
            continuation: None,
        })),
    }
}

/// GET /v1/forth/vocab — return the current shared vocabulary and its version.
///
/// REPL sessions poll this to sync definitions made in other concurrent terminals.
/// `version` is a monotonic counter — if it matches the caller's last-seen value,
/// the vocabulary has not changed and there is nothing to compile.
async fn handle_forth_vocab() -> Json<serde_json::Value> {
    let live = LIVE_VM.read().await;
    let source = live.dump_source();
    let version = VOCAB_VERSION.load(std::sync::atomic::Ordering::Relaxed);
    Json(serde_json::json!({ "source": source, "version": version }))
}

/// POST /v1/forth/push — receive a plain-text push message from a peer.
/// Broadcasts it to the local TUI via PUSH_INBOX.
async fn handle_forth_push(Json(req): Json<ForthPushRequest>) -> StatusCode {
    let msg = match &req.from {
        Some(from) => format!("[{}] {}", from, req.text),
        None => req.text.clone(),
    };
    let _ = PUSH_INBOX.send(msg);
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
struct ForthPushRequest {
    text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    from: Option<String>,
}

/// POST /v1/forth/hash — receive a key-value update from a peer.
/// Broadcasts it to the local event loop via HASH_INBOX so the REPL VM is updated.
async fn handle_forth_hash_set(Json(req): Json<ForthHashSetRequest>) -> StatusCode {
    // Deletion sentinel — value "\x00del\x00" means remove the key
    let _ = HASH_INBOX.send((
        req.room_id,
        req.key,
        req.value,
        req.from.unwrap_or_default(),
    ));
    StatusCode::OK
}

/// GET /v1/forth/hash?room=<uuid> — return all key-value pairs for a room.
/// Useful for a newly connecting peer to bootstrap its hash state.
async fn handle_forth_hash_get(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let room_id = params.get("room").cloned().unwrap_or_default();
    let vm = LIVE_VM.read().await;
    let pairs: std::collections::HashMap<String, String> = vm
        .rooms
        .get(&room_id)
        .map(|r| r.hash.clone())
        .unwrap_or_default();
    Json(serde_json::json!({ "room_id": room_id, "hash": pairs }))
}

#[derive(Debug, Deserialize)]
struct ForthHashSetRequest {
    room_id: String,
    key: String,
    value: String,
    #[serde(default)]
    from: Option<String>,
}

// ---------------------------------------------------------------------------
// Peer registry endpoints
// ---------------------------------------------------------------------------

/// Global registry — shared across all requests, lives for the daemon lifetime.
pub static REGISTRY: std::sync::LazyLock<std::sync::Arc<crate::registry::Registry>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(crate::registry::Registry::new()));

#[derive(Debug, Serialize)]
struct RegistryJoinResponse {
    addr: String,
    ttl_secs: u64,
}

#[derive(Debug, Deserialize)]
struct RegistryLeaveRequest {
    addr: String,
}

#[derive(Debug, Deserialize)]
struct RegistryHeartbeatRequest {
    addr: String,
}

/// POST /v1/registry/join — register or refresh a peer.
async fn handle_registry_join(
    Json(entry): Json<crate::registry::PeerEntry>,
) -> Json<RegistryJoinResponse> {
    let addr = REGISTRY.join(entry);
    Json(RegistryJoinResponse { addr, ttl_secs: 90 })
}

/// POST /v1/registry/leave — deregister a peer immediately.
async fn handle_registry_leave(Json(req): Json<RegistryLeaveRequest>) -> StatusCode {
    REGISTRY.leave(&req.addr);
    StatusCode::OK
}

/// POST /v1/registry/heartbeat — refresh TTL for a peer.
async fn handle_registry_heartbeat(Json(req): Json<RegistryHeartbeatRequest>) -> StatusCode {
    REGISTRY.heartbeat(&req.addr);
    StatusCode::OK
}

#[derive(Debug, Deserialize, Default)]
struct RegistryPeersQuery {
    tag: Option<String>,
    region: Option<String>,
}

/// GET /v1/registry/peers — list live peers, optionally filtered.
async fn handle_registry_peers(
    axum::extract::Query(q): axum::extract::Query<RegistryPeersQuery>,
) -> Json<Vec<crate::registry::PeerEntry>> {
    Json(REGISTRY.peers(q.tag.as_deref(), q.region.as_deref()))
}

/// GET /v1/registry/ledger/:addr — get the ledger entry for one peer.
async fn handle_registry_ledger(
    axum::extract::Path(addr): axum::extract::Path<String>,
) -> Json<crate::registry::LedgerEntry> {
    Json(REGISTRY.ledger(&addr).unwrap_or_default())
}

/// GET /v1/registry/ledgers — get ledger entries for all live peers.
async fn handle_registry_all_ledgers() -> Json<Vec<(String, crate::registry::LedgerEntry)>> {
    Json(REGISTRY.all_ledgers())
}

#[derive(Debug, Deserialize)]
struct RegistryDebitRequest {
    addr: String,
    compute_ms: u64,
}

/// POST /v1/registry/debit — record compute consumed from a peer.
///
/// Called by the machine that requested work to record its debt.
async fn handle_registry_debit(Json(req): Json<RegistryDebitRequest>) -> StatusCode {
    REGISTRY.debit(&req.addr, req.compute_ms);
    StatusCode::OK
}

#[derive(Debug, Deserialize)]
struct SettleRequest {
    /// The machine that did the work (creditor) — its ledger gets cleared.
    creditor: String,
    /// Acknowledged debt in milliseconds.
    amount_ms: u64,
}

#[derive(Debug, Serialize)]
struct SettleResponse {
    cleared_ms: u64,
    message: String,
}

/// POST /v1/settle — accept a settlement from a debtor machine.
///
/// The debtor POSTs here to acknowledge their debt and ask to clear the ledger.
/// We verify the amount is within 10% of what we recorded, then zero the entry.
async fn handle_settle(Json(req): Json<SettleRequest>) -> Result<Json<SettleResponse>, AppError> {
    let ledger = REGISTRY.ledger(&req.creditor).unwrap_or_default();
    let recorded_ms = ledger
        .credits_ms
        .saturating_sub(ledger.debits_ms.min(ledger.credits_ms));

    if recorded_ms == 0 {
        return Ok(Json(SettleResponse {
            cleared_ms: 0,
            message: "nothing owed".to_string(),
        }));
    }

    // Accept if the debtor's stated amount is within 10% of what we recorded.
    // This allows for small clock drift between machines.
    let tolerance = (recorded_ms as f64 * 0.10) as u64 + 500;
    if req.amount_ms == 0 || req.amount_ms + tolerance < recorded_ms {
        return Err(anyhow::anyhow!(
            "settlement amount {}ms doesn't match recorded {}ms (tolerance ±{}ms)",
            req.amount_ms,
            recorded_ms,
            tolerance
        )
        .into());
    }

    REGISTRY.settle(&req.creditor);
    Ok(Json(SettleResponse {
        cleared_ms: recorded_ms,
        message: format!("settled: {}ms cleared", recorded_ms),
    }))
}

// ---------------------------------------------------------------------------
// Gas transfer endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GasTransferRequest {
    /// Machine sending gas (will be debited).
    from: String,
    /// Machine receiving gas (will be credited).
    to: String,
    /// Amount of gas in milliseconds of compute credit.
    amount_ms: u64,
}

#[derive(Debug, Serialize)]
struct GasTransferResponse {
    message: String,
}

/// POST /v1/gas/transfer — send gas from one machine to another.
///
/// Debits `from` and credits `to` by `amount_ms`.
/// Both machines must be registered.
async fn handle_gas_transfer(
    Json(req): Json<GasTransferRequest>,
) -> Result<Json<GasTransferResponse>, AppError> {
    REGISTRY.transfer(&req.from, &req.to, req.amount_ms)?;
    let amount_s = req.amount_ms as f64 / 1000.0;
    Ok(Json(GasTransferResponse {
        message: format!(
            "transferred {:.1}s from {} to {}",
            amount_s, req.from, req.to
        ),
    }))
}

// ---------------------------------------------------------------------------
// Direct exec endpoint
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ExecRequest {
    cmd: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    stdin: Option<String>,
}

#[derive(Debug, Serialize)]
struct ExecResponse {
    stdout: String,
    stderr: String,
    exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// POST /v1/exec — run a command on this machine and return its output.
///
/// Body: { "cmd": "hostname" }
///   or: { "cmd": "grep", "args": ["-r", "TODO", "."] }
///   or: { "cmd": "bash", "args": ["-c", "echo hello && ls"] }
async fn handle_exec(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, Response> {
    let ip = addr.ip().to_string();
    if let Err(r) = check_peer_token(&headers, &ip, "/v1/exec") {
        return Err(r);
    }
    tracing::info!(ip = %ip, cmd = %req.cmd, "exec request");
    use std::io::Write;

    let ae = |e: anyhow::Error| AppError(e).into_response();

    let mut child = std::process::Command::new(&req.cmd)
        .args(&req.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| ae(anyhow::anyhow!("failed to spawn '{}': {}", req.cmd, e)))?;

    if let (Some(mut stdin), Some(input)) = (child.stdin.take(), &req.stdin) {
        let _ = stdin.write_all(input.as_bytes());
    }

    let output = child
        .wait_with_output()
        .map_err(|e| ae(anyhow::anyhow!("failed to wait for '{}': {}", req.cmd, e)))?;

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        error: None,
    }))
}

// ── Session WebSocket ──────────────────────────────────────────────────────────
//
// GET /v1/session/ws — upgrade to a bidirectional session with this node.
//
// After the upgrade, each side can send SessionEvent JSON frames at any time.
// The server side emits the connected bus into SESSION_BUS_TX so the local
// AI loop can pick it up and participate in the conversation.

use once_cell::sync::Lazy;
use tokio::sync::{broadcast, Mutex};

// ── Named session registry ────────────────────────────────────────────────────

/// Daemon-wide named session registry.
/// Clients POST /v1/session/join to get a broadcast sender for a named session.
pub static SESSION_REGISTRY: Lazy<
    std::sync::Arc<tokio::sync::Mutex<crate::server::session_registry::SessionRegistry>>,
> = Lazy::new(|| {
    std::sync::Arc::new(tokio::sync::Mutex::new(
        crate::server::session_registry::SessionRegistry::new(),
    ))
});

/// Broadcast channel that delivers new SessionBus handles to whoever is
/// listening (typically the AI event loop).
static SESSION_BUS_TX: Lazy<broadcast::Sender<()>> = Lazy::new(|| {
    let (tx, _) = broadcast::channel(16);
    tx
});

/// Queue of newly created SessionBus handles waiting to be claimed.
static PENDING_BUSES: Lazy<Mutex<Vec<crate::session::SessionBus>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

// ── Cross-machine peer relay ─────────────────────────────────────────────────
//
// Remote finch instances connect via GET /v1/session/ws?from=<their-addr>.
// Their messages land in REMOTE_TO_LOCAL for the local REPL to drain.
// The local REPL can broadcast back via POST /v1/session/relay-broadcast, which
// fans out to all connected remote WS clients through LOCAL_TO_REMOTE.

use std::sync::atomic::{AtomicU32, Ordering};

/// Monotonic counter for labelling incoming remote peers ("remote-1", "remote-2", …).
static REMOTE_PEER_COUNTER: AtomicU32 = AtomicU32::new(1);

/// Broadcast to all connected remote peer WS clients.
/// The local REPL sends here; every remote WS subscriber receives.
pub static LOCAL_TO_REMOTE: Lazy<broadcast::Sender<crate::session::SessionEvent>> =
    Lazy::new(|| broadcast::channel::<crate::session::SessionEvent>(256).0);

/// Queue of (label, text) pairs received FROM remote peers.
/// The local REPL polls GET /v1/session/relay-drain to consume this.
pub static REMOTE_TO_LOCAL: Lazy<Mutex<Vec<(String, String)>>> =
    Lazy::new(|| Mutex::new(Vec::new()));

/// Addresses of remote peers that announced themselves via ?from=<addr>.
/// The local REPL polls GET /v1/peer/announced to discover peers to connect back to.
pub static ANNOUNCED_PEERS: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

#[derive(Deserialize, Default)]
struct SessionWsQuery {
    /// The daemon address (host:port) of the connecting peer, so we can connect back.
    from: Option<String>,
}

/// Upgrade a GET /v1/session/ws request to a WebSocket session.
///
/// Optional query param `?from=host:port` causes the connecting peer's address to be
/// stored in ANNOUNCED_PEERS so the local REPL can connect back to them.
async fn handle_session_ws(
    ws: axum::extract::WebSocketUpgrade,
    Query(params): Query<SessionWsQuery>,
) -> impl IntoResponse {
    let from_addr = params.from;
    ws.on_upgrade(|socket| async move {
        if let Some(addr) = from_addr {
            ANNOUNCED_PEERS.lock().await.push(addr);
        }

        let n = REMOTE_PEER_COUNTER.fetch_add(1, Ordering::Relaxed);
        let label = format!("remote-{n}");

        let crate::session::SessionBus {
            tx: bus_tx,
            rx: mut bus_rx,
        } = crate::session::transport::serve(socket);

        // Remote peer → REMOTE_TO_LOCAL queue (local REPL drains via relay-drain).
        let lbl = label;
        tokio::spawn(async move {
            while let Some(ev) = bus_rx.recv().await {
                let entry = match ev {
                    crate::session::SessionEvent::Chat { text } => {
                        Some((lbl.clone(), text))
                    }
                    crate::session::SessionEvent::ChannelMessage { channel, sender, bundle } => {
                        let primary = bundle.primary();
                        let comment = if bundle.comments.is_empty() {
                            String::new()
                        } else {
                            format!("  \\ {}", bundle.comments.join("; "))
                        };
                        Some((lbl.clone(), format!("{channel} {sender}: {}{comment}", primary.code)))
                    }
                    _ => None,
                };
                if let Some(pair) = entry {
                    REMOTE_TO_LOCAL.lock().await.push(pair);
                }
            }
        });

        // LOCAL_TO_REMOTE broadcast → this remote peer's WS.
        let mut local_rx = LOCAL_TO_REMOTE.subscribe();
        tokio::spawn(async move {
            while let Ok(ev) = local_rx.recv().await {
                if bus_tx.send(ev).await.is_err() {
                    break;
                }
            }
        });
    })
}

/// GET /v1/session/relay-drain — return and clear all queued remote peer messages.
async fn handle_relay_drain() -> Json<Vec<(String, String)>> {
    Json(std::mem::take(&mut *REMOTE_TO_LOCAL.lock().await))
}

/// POST /v1/session/relay-broadcast — broadcast a Chat event to all connected remote peers.
///
/// Body: `{"text": "..."}`
async fn handle_relay_broadcast(Json(body): Json<serde_json::Value>) -> StatusCode {
    if let Some(text) = body["text"].as_str() {
        let _ = LOCAL_TO_REMOTE.send(crate::session::SessionEvent::chat(text));
    }
    StatusCode::OK
}

/// GET /v1/peer/announced — list remote peer addresses that connected with `?from=`.
async fn handle_announced_peers() -> Json<Vec<String>> {
    Json(ANNOUNCED_PEERS.lock().await.clone())
}

/// Accept the next inbound session bus (called by the AI loop).
/// Returns `None` immediately if no pending connection.
pub async fn accept_session_bus() -> Option<crate::session::SessionBus> {
    PENDING_BUSES.lock().await.pop()
}

/// Subscribe to new-session notifications.
pub fn session_bus_notifications() -> broadcast::Receiver<()> {
    SESSION_BUS_TX.subscribe()
}

// ── Named session HTTP endpoints ──────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct SessionJoinRequest {
    name: String,
}

#[derive(Debug, Serialize)]
struct SessionJoinResponse {
    id: String,
    name: String,
    /// WebSocket URL to use for this session's broadcast channel.
    ws_url: String,
}

#[derive(Debug, Serialize)]
struct SessionListEntry {
    name: String,
    id: String,
    peers: usize,
}

/// POST /v1/session/join — join (or create) a named session.
///
/// Body: `{"name": "quiet-hill"}`
///
/// Returns the session UUID and a WebSocket URL for the broadcast channel.
/// Two clients that POST the same name will get the same UUID and can therefore
/// connect to the same WebSocket session.
async fn handle_session_join(
    State(server): State<Arc<AgentServer>>,
    Json(req): Json<SessionJoinRequest>,
) -> Result<Json<SessionJoinResponse>, AppError> {
    if req.name.trim().is_empty() {
        return Err(AppError(anyhow::anyhow!("session name must not be empty")));
    }

    let (id, _tx) = SESSION_REGISTRY.lock().await.get_or_create(&req.name);

    // Build a ws:// URL pointing to this daemon's session WebSocket endpoint.
    let ws_url = format!(
        "ws://{}/v1/session/ws?session={}",
        server.config().bind_address,
        id
    );

    Ok(Json(SessionJoinResponse {
        id: id.to_string(),
        name: req.name,
        ws_url,
    }))
}

/// GET /v1/session/list — list all active named sessions.
async fn handle_session_list() -> Json<Vec<SessionListEntry>> {
    let entries = SESSION_REGISTRY
        .lock()
        .await
        .list()
        .into_iter()
        .map(|(name, id, peers)| SessionListEntry {
            name,
            id: id.to_string(),
            peers,
        })
        .collect();
    Json(entries)
}

// ── Co-Forth session endpoints ────────────────────────────────────────────────
//
// Minimal message protocol:
//   POST /v1/forth/coforth                 — create session between two peers
//   POST /v1/forth/coforth/:id/yield       — yield a program fragment
//   POST /v1/forth/coforth/:id/agree       — signal readiness to execute
//   GET  /v1/forth/coforth/:id             — poll session state

static CO_SESSION_STORE: std::sync::LazyLock<crate::coforth::co_session::SessionStore> =
    std::sync::LazyLock::new(crate::coforth::co_session::new_session_store);

/// Global channel registry — unified state for Forth words, HTTP endpoints, and TCP IRC peers.
/// `join"`, `yield-to"`, `execute-channel"`, and remote TCP connections all share this store.
pub static CHANNEL_REGISTRY: std::sync::LazyLock<crate::coforth::irc_proto::ChannelRegistry> =
    std::sync::LazyLock::new(crate::coforth::irc_proto::new_channel_registry);

#[derive(Debug, Deserialize)]
struct CoForthCreateRequest {
    peer_a: String,
    peer_b: String,
}

#[derive(Debug, Serialize)]
struct CoForthCreateResponse {
    session_id: Uuid,
}

/// POST /v1/forth/coforth — create a new co-forth session between two peers.
async fn handle_coforth_create(Json(req): Json<CoForthCreateRequest>) -> Json<CoForthCreateResponse> {
    let session = crate::coforth::co_session::CoForthSession::new(req.peer_a, req.peer_b);
    let id = session.id;
    CO_SESSION_STORE.lock().unwrap().insert(id, session);
    Json(CoForthCreateResponse { session_id: id })
}

#[derive(Debug, Deserialize)]
struct CoForthYieldRequest {
    from: String,
    program: String,
}

#[derive(Debug, Serialize)]
struct CoForthStateResponse {
    session_id: Uuid,
    stack_a: Vec<String>,
    stack_b: Vec<String>,
    agreed_a: bool,
    agreed_b: bool,
    consensus: bool,
    /// When consensus is true: program peer_b should run.
    program_for_b: Option<String>,
    /// When consensus is true: program peer_a should run.
    program_for_a: Option<String>,
}

/// POST /v1/forth/coforth/:id/yield — yield a program fragment from one peer.
async fn handle_coforth_yield(
    Path(id): Path<Uuid>,
    Json(req): Json<CoForthYieldRequest>,
) -> Result<Json<CoForthStateResponse>, (StatusCode, Json<serde_json::Value>)> {
    let mut store = CO_SESSION_STORE.lock().unwrap();
    let session = store.get_mut(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )
    })?;
    session.push_yield(&req.from, req.program);
    let consensus = session.consensus();
    let (pfb, pfa) = if consensus {
        (Some(session.program_for_b()), Some(session.program_for_a()))
    } else {
        (None, None)
    };
    Ok(Json(CoForthStateResponse {
        session_id: session.id,
        stack_a: session.stack_a.clone(),
        stack_b: session.stack_b.clone(),
        agreed_a: session.agreed_a,
        agreed_b: session.agreed_b,
        consensus,
        program_for_b: pfb,
        program_for_a: pfa,
    }))
}

#[derive(Debug, Deserialize)]
struct CoForthAgreeRequest {
    from: String,
}

/// POST /v1/forth/coforth/:id/agree — signal agreement; returns state (consensus fires execution).
async fn handle_coforth_agree(
    Path(id): Path<Uuid>,
    Json(req): Json<CoForthAgreeRequest>,
) -> Result<Json<CoForthStateResponse>, (StatusCode, Json<serde_json::Value>)> {
    let mut store = CO_SESSION_STORE.lock().unwrap();
    let session = store.get_mut(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )
    })?;
    session.agree(&req.from);
    let consensus = session.consensus();
    let (pfb, pfa) = if consensus {
        (Some(session.program_for_b()), Some(session.program_for_a()))
    } else {
        (None, None)
    };
    Ok(Json(CoForthStateResponse {
        session_id: session.id,
        stack_a: session.stack_a.clone(),
        stack_b: session.stack_b.clone(),
        agreed_a: session.agreed_a,
        agreed_b: session.agreed_b,
        consensus,
        program_for_b: pfb,
        program_for_a: pfa,
    }))
}

/// GET /v1/forth/coforth/:id — poll session state.
async fn handle_coforth_get(
    Path(id): Path<Uuid>,
) -> Result<Json<CoForthStateResponse>, (StatusCode, Json<serde_json::Value>)> {
    let store = CO_SESSION_STORE.lock().unwrap();
    let session = store.get(&id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "session not found"})),
        )
    })?;
    let consensus = session.consensus();
    let (pfb, pfa) = if consensus {
        (Some(session.program_for_b()), Some(session.program_for_a()))
    } else {
        (None, None)
    };
    Ok(Json(CoForthStateResponse {
        session_id: session.id,
        stack_a: session.stack_a.clone(),
        stack_b: session.stack_b.clone(),
        agreed_a: session.agreed_a,
        agreed_b: session.agreed_b,
        consensus,
        program_for_b: pfb,
        program_for_a: pfa,
    }))
}

// ── Channel contribution endpoints ───────────────────────────────────────────
//
// Every peer on the LAN can push a Forth program onto a named channel's stack.
// The stack is visible to all; any peer can trigger execution.

#[derive(Debug, Deserialize)]
struct ChannelContributeRequest {
    from: String,
    program: String,
}

#[derive(Debug, Serialize)]
struct ChannelEntry {
    from: String,
    program: String,
}

#[derive(Debug, Serialize)]
struct ChannelStateResponse {
    channel: String,
    contributions: Vec<ChannelEntry>,
}

/// POST /v1/forth/channel/:name/contribute — receive a contribution from a peer.
async fn handle_channel_contribute(
    Path(name): Path<String>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<ChannelContributeRequest>,
) -> StatusCode {
    let ip = addr.ip().to_string();
    if check_peer_token(&headers, &ip, "/v1/forth/channel/contribute").is_err() {
        return StatusCode::FORBIDDEN;
    }
    let chan = if name.starts_with('#') { name } else { format!("#{name}") };
    use crate::coforth::irc_proto::{IrcMessage, OP_YIELD};
    crate::coforth::irc_proto::process_message(
        IrcMessage::new(OP_YIELD, &req.from, &chan, req.program.as_bytes().to_vec()),
        &CHANNEL_REGISTRY,
    );
    StatusCode::OK
}

/// GET /v1/forth/channel/:name — list all contributions in a channel.
async fn handle_channel_get(Path(name): Path<String>) -> Json<ChannelStateResponse> {
    let chan = if name.starts_with('#') { name.clone() } else { format!("#{name}") };
    let reg = CHANNEL_REGISTRY.lock().unwrap();
    let contributions = reg
        .get(&chan)
        .map(|s| {
            s.stack
                .iter()
                .map(|(f, p)| ChannelEntry { from: f.clone(), program: p.clone() })
                .collect()
        })
        .unwrap_or_default();
    Json(ChannelStateResponse { channel: chan, contributions })
}

/// POST /v1/forth/channel/:name/execute — run all contributions in a channel on this peer.
async fn handle_channel_execute(Path(name): Path<String>) -> Json<serde_json::Value> {
    let chan = if name.starts_with('#') { name } else { format!("#{name}") };
    use crate::coforth::irc_proto::{IrcMessage, OP_EXEC};
    let reply = crate::coforth::irc_proto::process_message(
        IrcMessage::new(OP_EXEC, "http", &chan, vec![]),
        &CHANNEL_REGISTRY,
    );
    let combined = reply
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .unwrap_or_default();
    if combined.is_empty() {
        return Json(serde_json::json!({ "output": "", "error": "no contributions" }));
    }
    let base = LIVE_VM.read().await;
    let mut vm = base.clone_dict();
    drop(base);
    vm.remote_mode = true;
    match vm.exec(&combined) {
        Ok(out) => Json(serde_json::json!({ "output": out, "stack": vm.data_stack() })),
        Err(e) => Json(serde_json::json!({ "output": vm.out, "error": e.to_string() })),
    }
}

// ── File transfer: zip/unzip over the peer protocol ───────────────────────────

/// Recursively add a file or directory into a ZipWriter.
/// Paths inside the archive are relative to `base`.
fn zip_add_to_writer<W: std::io::Write + std::io::Seek>(
    zw: &mut zip::ZipWriter<W>,
    path: &std::path::Path,
    base: &std::path::Path,
    opts: zip::write::SimpleFileOptions,
) -> anyhow::Result<()> {
    use std::io::Write as _;
    if path.is_file() {
        let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy().to_string();
        zw.start_file(&rel, opts)?;
        zw.write_all(&std::fs::read(path)?)?;
    } else if path.is_dir() {
        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let p = entry?.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(base).unwrap_or(&p).to_string_lossy().to_string();
                    zw.start_file(&rel, opts)?;
                    zw.write_all(&std::fs::read(&p)?)?;
                }
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct FileGetParams {
    path: String,
}

/// GET /v1/file/get?path=<localpath>
///
/// Zips `path` and returns the raw bytes as `application/zip`.
/// The zip is created relative to the parent of `path` so the top-level entry
/// inside the archive is just the basename, not the full absolute path.
pub async fn handle_file_get(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(params): Query<FileGetParams>,
) -> Result<Response, AppError> {
    let ip = addr.ip().to_string();
    check_peer_token(&headers, &ip, "/v1/file/get")
        .map_err(|r| AppError(anyhow::anyhow!("{:?}", r)))?;

    let src = std::path::Path::new(&params.path);
    if !src.exists() {
        return Err(AppError(anyhow::anyhow!("path not found: {}", params.path)));
    }

    let parent = src.parent().unwrap_or(std::path::Path::new("."));
    let name = src
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("archive");

    // Build zip in memory using the zip crate (no external binary required).
    let bytes = tokio::task::spawn_blocking({
        let src = src.to_path_buf();
        let parent = parent.to_path_buf();
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        move || -> anyhow::Result<Vec<u8>> {
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut zw = zip::ZipWriter::new(&mut buf);
            zip_add_to_writer(&mut zw, &src, &parent, opts)?;
            zw.finish()?;
            Ok(buf.into_inner())
        }
    })
    .await
    .map_err(|e| AppError(anyhow::anyhow!("zip task: {}", e)))?
    .map_err(AppError)?;

    let filename = format!("{}.zip", name);

    use axum::http::header;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        bytes,
    )
        .into_response())
}

#[derive(Deserialize)]
struct FilePutParams {
    /// Filename to save the zip as (default: random UUID + .zip).
    #[serde(default)]
    name: Option<String>,
    /// Directory to unzip into (default: ~/.finch/received/).
    #[serde(default)]
    dest: Option<String>,
}

/// POST /v1/file/put?name=archive.zip&dest=/some/dir
///
/// Accepts raw zip bytes, saves to `dest` (default `~/.finch/received/`),
/// and immediately unzips in place.
pub async fn handle_file_put(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(params): Query<FilePutParams>,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, AppError> {
    let ip = addr.ip().to_string();
    check_peer_token(&headers, &ip, "/v1/file/put")
        .map_err(|r| AppError(anyhow::anyhow!("{:?}", r)))?;

    let dest = params.dest.unwrap_or_else(|| {
        dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
            .join(".finch/received")
            .to_string_lossy()
            .into_owned()
    });
    std::fs::create_dir_all(&dest)?;

    let name = params
        .name
        .unwrap_or_else(|| format!("{}.zip", uuid::Uuid::new_v4()));
    let zip_path = std::path::Path::new(&dest).join(&name);
    std::fs::write(&zip_path, &body)?;

    // Unzip using the zip crate (no external binary required).
    let dest_clone = dest.clone();
    let zip_path_clone = zip_path.clone();
    let count = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        let file = std::fs::File::open(&zip_path_clone)?;
        let mut archive = zip::ZipArchive::new(file)?;
        let dest_p = std::path::Path::new(&dest_clone);
        let dest_canon = std::fs::canonicalize(dest_p).unwrap_or_else(|_| dest_p.to_path_buf());
        let count = archive.len();
        for i in 0..count {
            let mut entry = archive.by_index(i)?;
            let out_path = dest_p.join(entry.name());
            // Zip-slip guard.
            let out_norm = out_path.components().fold(
                std::path::PathBuf::new(),
                |mut acc, c| match c {
                    std::path::Component::ParentDir => { acc.pop(); acc }
                    _ => { acc.push(c); acc }
                },
            );
            if !out_norm.starts_with(&dest_canon) && !out_norm.starts_with(dest_p) {
                anyhow::bail!("zip-slip detected in entry '{}'", entry.name());
            }
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(p) = out_path.parent() { std::fs::create_dir_all(p)?; }
                let mut out = std::fs::File::create(&out_path)?;
                std::io::copy(&mut entry, &mut out)?;
            }
        }
        Ok(count)
    })
    .await
    .map_err(|e| AppError(anyhow::anyhow!("unzip task: {}", e)))?
    .map_err(AppError)?;

    Ok(Json(serde_json::json!({
        "zip": zip_path.to_string_lossy(),
        "dest": dest,
        "entries": count,
    })))
}
