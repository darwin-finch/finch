// HTTP request handlers

use axum::{
    extract::{ConnectInfo, Path, State},
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
            Err((StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "wrong peer token"}))).into_response())
        }
        None => {
            tracing::warn!(ip = %peer_ip, endpoint, "rejected: no peer token");
            Err((StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "peer token required — set X-Finch-Token"}))).into_response())
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
        // Note: node handlers load config independently (no AgentServer state needed)
        // Co-Forth remote eval and direct exec
        .route("/v1/forth/eval", post(handle_forth_eval))
        .route("/v1/forth/define", post(handle_forth_define))
        .route("/v1/forth/push", post(handle_forth_push))
        .route("/v1/exec", post(handle_exec))
        // Peer registry
        .route("/v1/registry/join",      post(handle_registry_join))
        .route("/v1/registry/leave",     post(handle_registry_leave))
        .route("/v1/registry/heartbeat", post(handle_registry_heartbeat))
        .route("/v1/registry/peers",     get(handle_registry_peers))
        .route("/v1/registry/ledger/:addr", get(handle_registry_ledger))
        .route("/v1/registry/ledgers",   get(handle_registry_all_ledgers))
        .route("/v1/registry/debit",     post(handle_registry_debit))
        .route("/v1/settle",             post(handle_settle))
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
}

/// Global broadcast channel for incoming push messages.
/// The server writes to this when a peer calls POST /v1/forth/push.
/// The event loop subscribes and displays messages in the TUI.
pub static PUSH_INBOX: std::sync::LazyLock<tokio::sync::broadcast::Sender<String>> =
    std::sync::LazyLock::new(|| {
        let (tx, _) = tokio::sync::broadcast::channel(64);
        tx
    });

/// Grammar-VM shared baseline: pre-compiled STDLIB + all grammar words, built once.
/// Each request clones this (O(dict size)) instead of recompiling from source.
static GRAMMAR_VM: std::sync::LazyLock<crate::coforth::Forth> =
    std::sync::LazyLock::new(|| {
        use crate::coforth::{Forth, Library};
        let mut vm = Forth::new();
        let lib = Library::load();
        vm.compile_library(&lib);
        vm
    });

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
    dirs::home_dir().map(|mut p| { p.push(".finch"); p.push("user_words.forth"); p })
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
    handle_forth_eval_inner(req).await.map_err(|e| AppError(e).into_response())
}

async fn handle_forth_eval_inner(req: ForthEvalRequest) -> anyhow::Result<Json<ForthEvalResponse>> {
    let base = LIVE_VM.read().await;
    let mut vm = base.clone_dict();
    drop(base);
    vm.remote_mode = true; // no dialogs, no AI calls on remote VMs
    let t0 = std::time::Instant::now();
    let result = vm.exec(&req.code);
    let compute_ms = t0.elapsed().as_millis() as u64;

    // Credit this machine for the work it just performed.
    if let Some(addr) = vm.registry_addr.clone() {
        REGISTRY.credit(&addr, compute_ms).await;
    }

    // Debit the caller and check if they've crossed the debt threshold.
    let debt_warning = if let Some(caller) = &req.caller {
        let (balance, crossed) = REGISTRY.debit(caller, compute_ms).await;
        if crossed {
            let threshold_s = REGISTRY.debt_threshold_ms as f64 / 1000.0;
            let balance_s   = balance.abs() as f64 / 1000.0;
            Some(format!(
                "compute debt: {:.1}s owed (threshold {:.1}s) — please settle",
                balance_s, threshold_s
            ))
        } else if REGISTRY.is_in_debt(caller).await {
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
            stack:        vm.data_stack().to_vec(),
            error:        None,
            compute_ms,
            debt_warning,
            forth_back:   vm.forth_back.clone(),
        })),
        Err(e) => Ok(Json(ForthEvalResponse {
            output:       vm.out.clone(),
            stack:        vm.data_stack().to_vec(),
            error:        Some(e.to_string()),
            compute_ms,
            debt_warning,
            forth_back:   vm.forth_back.clone(),
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
            Ok(Json(ForthEvalResponse { output, stack: Vec::new(), error: None, compute_ms: 0, debt_warning: None, forth_back: None }))
        }
        Err(e) => Ok(Json(ForthEvalResponse {
            output:       live.out.clone(),
            stack:        Vec::new(),
            error:        Some(e.to_string()),
            compute_ms:   0,
            debt_warning: None,
            forth_back:   None,
        })),
    }
}

/// POST /v1/forth/push — receive a plain-text push message from a peer.
/// Broadcasts it to the local TUI via PUSH_INBOX.
async fn handle_forth_push(
    Json(req): Json<ForthPushRequest>,
) -> StatusCode {
    let msg = match &req.from {
        Some(from) => format!("[{}] {}", from, req.text),
        None       => req.text.clone(),
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
    let addr = REGISTRY.join(entry).await;
    Json(RegistryJoinResponse { addr, ttl_secs: 90 })
}

/// POST /v1/registry/leave — deregister a peer immediately.
async fn handle_registry_leave(
    Json(req): Json<RegistryLeaveRequest>,
) -> StatusCode {
    REGISTRY.leave(&req.addr).await;
    StatusCode::OK
}

/// POST /v1/registry/heartbeat — refresh TTL for a peer.
async fn handle_registry_heartbeat(
    Json(req): Json<RegistryHeartbeatRequest>,
) -> StatusCode {
    REGISTRY.heartbeat(&req.addr).await;
    StatusCode::OK
}

#[derive(Debug, Deserialize, Default)]
struct RegistryPeersQuery {
    tag:    Option<String>,
    region: Option<String>,
}

/// GET /v1/registry/peers — list live peers, optionally filtered.
async fn handle_registry_peers(
    axum::extract::Query(q): axum::extract::Query<RegistryPeersQuery>,
) -> Json<Vec<crate::registry::PeerEntry>> {
    let peers = REGISTRY.peers(q.tag.as_deref(), q.region.as_deref()).await;
    Json(peers)
}

/// GET /v1/registry/ledger/:addr — get the ledger entry for one peer.
async fn handle_registry_ledger(
    axum::extract::Path(addr): axum::extract::Path<String>,
) -> Result<Json<crate::registry::LedgerEntry>, AppError> {
    let entry = REGISTRY.ledger(&addr).await.unwrap_or_default();
    Ok(Json(entry))
}

/// GET /v1/registry/ledgers — get ledger entries for all live peers.
async fn handle_registry_all_ledgers() -> Json<Vec<(String, crate::registry::LedgerEntry)>> {
    Json(REGISTRY.all_ledgers().await)
}

#[derive(Debug, Deserialize)]
struct RegistryDebitRequest {
    addr: String,
    compute_ms: u64,
}

/// POST /v1/registry/debit — record compute consumed from a peer.
///
/// Called by the machine that requested work to record its debt.
async fn handle_registry_debit(
    Json(req): Json<RegistryDebitRequest>,
) -> StatusCode {
    REGISTRY.debit(&req.addr, req.compute_ms).await;
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
async fn handle_settle(
    Json(req): Json<SettleRequest>,
) -> Result<Json<SettleResponse>, AppError> {
    let ledger = REGISTRY.ledger(&req.creditor).await.unwrap_or_default();
    let recorded_ms = ledger.credits_ms.saturating_sub(ledger.debits_ms.min(ledger.credits_ms));

    if recorded_ms == 0 {
        return Ok(Json(SettleResponse { cleared_ms: 0, message: "nothing owed".to_string() }));
    }

    // Accept if the debtor's stated amount is within 10% of what we recorded.
    // This allows for small clock drift between machines.
    let tolerance = (recorded_ms as f64 * 0.10) as u64 + 500;
    if req.amount_ms == 0 || req.amount_ms + tolerance < recorded_ms {
        return Err(anyhow::anyhow!(
            "settlement amount {}ms doesn't match recorded {}ms (tolerance ±{}ms)",
            req.amount_ms, recorded_ms, tolerance
        ).into());
    }

    REGISTRY.settle(&req.creditor).await;
    Ok(Json(SettleResponse {
        cleared_ms: recorded_ms,
        message: format!("settled: {}ms cleared", recorded_ms),
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

    let output = child.wait_with_output()
        .map_err(|e| ae(anyhow::anyhow!("failed to wait for '{}': {}", req.cmd, e)))?;

    Ok(Json(ExecResponse {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code().unwrap_or(-1),
        error: None,
    }))
}
