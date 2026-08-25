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

/// Check `X-Finch-Token` header against this daemon's token.
/// Returns Ok(()) if valid, Err(StatusCode::FORBIDDEN) with a log entry if not.
fn check_peer_token(headers: &HeaderMap, peer_ip: &str, endpoint: &str) -> Result<(), Response> {
    let expected = &*crate::peer_token::TOKEN;
    match headers.get(crate::peer_token::HEADER) {
        Some(v) if v.as_bytes() == expected.as_bytes() => Ok(()),
        Some(_) => {
            tracing::warn!(ip = %peer_ip, endpoint, "rejected: wrong peer token");
            Err((
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({"error": "wrong peer token"})),
            )
                .into_response())
        }
        None => {
            tracing::warn!(ip = %peer_ip, endpoint, "rejected: no peer token");
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
        // Durable named Brain sessions
        .route("/v1/brains/named", get(list_named_brains))
        .route(
            "/v1/brains/named/:name",
            get(get_named_brain)
                .post(push_named_brain_event)
                .delete(archive_named_brain),
        )
        .route(
            "/v1/brains/named/:name/attachments",
            post(attach_named_brain),
        )
        .route(
            "/v1/brains/named/:name/credentials",
            post(issue_named_brain_credential),
        )
        .route(
            "/v1/brains/credentials/:credential_id",
            axum::routing::delete(revoke_named_brain_credential),
        )
        .route(
            "/v1/brains/named/:name/attachments/:attachment_id/ack",
            post(acknowledge_named_brain),
        )
        .route(
            "/v1/brains/named/:name/attachments/:attachment_id/connections/:connection_id",
            axum::routing::delete(detach_named_brain),
        )
        .route(
            "/v1/brains/named/:name/runner-lease",
            post(acquire_named_brain_runner),
        )
        .route(
            "/v1/brains/named/:name/runner-lease/:lease_id",
            axum::routing::delete(release_named_brain_runner),
        )
        .route("/v1/brains/named/:name/ws", get(watch_named_brain))
        .route(
            "/v1/brains/password",
            get(show_brain_password).put(change_brain_password),
        )
        // Note: node handlers load config independently (no AgentServer state needed)
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

const BRAIN_PASSWORD_HEADER: &str = "x-finch-brain-password";

async fn check_brain_bootstrap_access(
    server: &AgentServer,
    addr: SocketAddr,
    headers: &HeaderMap,
) -> Result<(), Response> {
    if addr.ip().is_loopback() {
        return Ok(());
    }
    let supplied = headers
        .get(BRAIN_PASSWORD_HEADER)
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.strip_prefix("Bearer "))
        })
        .unwrap_or_default();
    if !supplied.is_empty() && server.check_brain_password(supplied).await {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "brain password required"})),
        )
            .into_response())
    }
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn brain_auth_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({"error": message.into()}))).into_response()
}

fn authorize_named_brain(
    server: &AgentServer,
    addr: SocketAddr,
    headers: &HeaderMap,
    name: &str,
    scope: crate::brain::credential::BrainCredentialScope,
) -> Result<Option<crate::brain::credential::BrainCredentialClaims>, Response> {
    if addr.ip().is_loopback() {
        return Ok(None);
    }
    let token = bearer_token(headers).ok_or_else(|| {
        brain_auth_error(StatusCode::UNAUTHORIZED, "scoped Brain credential required")
    })?;
    let claims = server
        .brain_credentials()
        .verify(token, unix_epoch_millis())
        .map_err(|error| brain_auth_error(StatusCode::UNAUTHORIZED, error.to_string()))?;
    let snapshot = server
        .shared_brains()
        .snapshot(name)
        .map_err(|error| AppError(error).into_response())?;
    claims
        .require_audience(
            snapshot.brain_id,
            name,
            snapshot.environment.generation,
            scope,
        )
        .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
    Ok(Some(claims))
}

const DEFAULT_BRAIN_CREDENTIAL_TTL_MS: u64 = 8 * 60 * 60 * 1_000;
const MAX_BRAIN_CREDENTIAL_TTL_MS: u64 = 24 * 60 * 60 * 1_000;

#[derive(Debug, Deserialize)]
struct IssueNamedBrainCredentialRequest {
    subject: String,
    role: crate::brain::shared::AttachmentRole,
    ttl_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct IssueNamedBrainCredentialResponse {
    token: String,
    claims: crate::brain::credential::BrainCredentialClaims,
}

fn participant_scopes(
    role: crate::brain::shared::AttachmentRole,
) -> std::collections::BTreeSet<crate::brain::credential::BrainCredentialScope> {
    use crate::brain::credential::BrainCredentialScope;
    use crate::brain::shared::AttachmentRole;
    match role {
        AttachmentRole::Driver => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainSubmit,
            BrainCredentialScope::BrainApprove,
            BrainCredentialScope::BrainControl,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Consultant => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainSubmit,
            BrainCredentialScope::BrainApprove,
            BrainCredentialScope::BrainControl,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Observer => [
            BrainCredentialScope::BrainRead,
            BrainCredentialScope::BrainControl,
        ]
        .into_iter()
        .collect(),
        AttachmentRole::Runner => std::collections::BTreeSet::new(),
    }
}

fn claims_match_attachment(
    claims: Option<&crate::brain::credential::BrainCredentialClaims>,
    attachment: &crate::brain::shared::BrainAttachment,
) -> Result<(), Response> {
    if let Some(claims) = claims {
        claims
            .require_participant(&attachment.subject, attachment.role)
            .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
    }
    Ok(())
}

async fn issue_named_brain_credential(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<IssueNamedBrainCredentialRequest>,
) -> Result<Json<IssueNamedBrainCredentialResponse>, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    if request.role == crate::brain::shared::AttachmentRole::Runner {
        return Err(brain_auth_error(
            StatusCode::BAD_REQUEST,
            "runner authority cannot be minted as a participant credential",
        ));
    }
    let snapshot = server
        .shared_brains()
        .snapshot(&name)
        .map_err(|error| AppError(error).into_response())?;
    let ttl_ms = request
        .ttl_ms
        .unwrap_or(DEFAULT_BRAIN_CREDENTIAL_TTL_MS)
        .min(MAX_BRAIN_CREDENTIAL_TTL_MS);
    let token = server
        .brain_credentials()
        .issue(
            crate::brain::credential::BrainCredentialRequest {
                issuer: snapshot.environment.machine.clone(),
                subject: request.subject,
                brain_id: snapshot.brain_id,
                brain: name,
                environment_generation: snapshot.environment.generation,
                role: request.role,
                scopes: participant_scopes(request.role),
                ttl_ms,
            },
            unix_epoch_millis(),
        )
        .map_err(|error| AppError(error).into_response())?;
    let claims = server
        .brain_credentials()
        .verify(&token, unix_epoch_millis())
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(IssueNamedBrainCredentialResponse { token, claims }))
}

async fn revoke_named_brain_credential(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(credential_id): Path<uuid::Uuid>,
) -> Result<StatusCode, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    server
        .brain_credentials()
        .revoke(credential_id)
        .map_err(|error| AppError(error).into_response())?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
struct NamedBrainListEntry {
    name: String,
    environment: crate::brain::shared::BrainEnvironment,
    event_revision: u64,
    retained_programs: usize,
    runner: Option<crate::brain::shared::BrainRunnerLease>,
}

async fn list_named_brains(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Json<Vec<NamedBrainListEntry>>, Response> {
    check_brain_bootstrap_access(&server, addr, &headers).await?;
    let mut result = Vec::new();
    for name in server
        .shared_brains()
        .list()
        .map_err(|error| AppError(error).into_response())?
    {
        let snapshot = server
            .shared_brains()
            .snapshot(&name)
            .map_err(|error| AppError(error).into_response())?;
        result.push(NamedBrainListEntry {
            name,
            environment: snapshot.environment,
            event_revision: snapshot.revision,
            retained_programs: snapshot.program_stack.len(),
            runner: snapshot.runner_lease,
        });
    }
    Ok(Json(result))
}

async fn get_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<crate::brain::shared::BrainSnapshot>, Response> {
    authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainRead,
    )?;
    server
        .shared_brains()
        .snapshot(&name)
        .map(Json)
        .map_err(|error| AppError(error).into_response())
}

#[derive(Debug, Deserialize)]
struct AttachNamedBrainRequest {
    subject: String,
    role: crate::brain::shared::AttachmentRole,
    attachment_id: Option<crate::brain::shared::AttachmentId>,
}

const PENDING_BRAIN_ATTACHMENT_TTL: std::time::Duration = std::time::Duration::from_secs(15);

async fn attach_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<AttachNamedBrainRequest>,
) -> Result<Json<crate::brain::shared::BrainAttachment>, Response> {
    let claims = authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainControl,
    )?;
    if let Some(claims) = claims {
        claims
            .require_participant(&request.subject, request.role)
            .map_err(|error| brain_auth_error(StatusCode::FORBIDDEN, error.to_string()))?;
    }
    if request.role == crate::brain::shared::AttachmentRole::Runner {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "runner authority requires a runner lease, not a client attachment"
            })),
        )
            .into_response());
    }
    let attachment = server
        .shared_brains()
        .attach(&name, &request.subject, request.role, request.attachment_id)
        .map_err(brain_state_conflict)?;
    let store = server.shared_brains().clone();
    let pending_name = name.clone();
    let attachment_id = attachment.attachment_id;
    let connection_id = attachment
        .connection_id
        .expect("new Brain attachment has a pending connection");
    tokio::spawn(async move {
        tokio::time::sleep(PENDING_BRAIN_ATTACHMENT_TTL).await;
        if store
            .expire_pending_connection(&pending_name, attachment_id, connection_id)
            .unwrap_or(false)
        {
            let _ = store.remove_if_unused(&pending_name);
        }
    });
    Ok(Json(attachment))
}

#[derive(Debug, Deserialize)]
struct AcknowledgeNamedBrainRequest {
    connection_id: crate::brain::shared::ConnectionId,
    seq: u64,
}

async fn acknowledge_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((name, attachment_id)): Path<(String, uuid::Uuid)>,
    Json(request): Json<AcknowledgeNamedBrainRequest>,
) -> Result<Json<crate::brain::shared::BrainAttachment>, Response> {
    let claims = authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainRead,
    )?;
    let attachment = server
        .shared_brains()
        .require_connection(
            &name,
            crate::brain::shared::AttachmentId(attachment_id),
            request.connection_id,
        )
        .map_err(brain_state_conflict)?;
    claims_match_attachment(claims.as_ref(), &attachment)?;
    server
        .shared_brains()
        .acknowledge(
            &name,
            crate::brain::shared::AttachmentId(attachment_id),
            request.connection_id,
            request.seq,
        )
        .map(Json)
        .map_err(brain_state_conflict)
}

async fn detach_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((name, attachment_id, connection_id)): Path<(String, uuid::Uuid, uuid::Uuid)>,
) -> Result<StatusCode, Response> {
    let claims = authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainControl,
    )?;
    let attachment_id = crate::brain::shared::AttachmentId(attachment_id);
    let connection_id = crate::brain::shared::ConnectionId(connection_id);
    let attachment = server
        .shared_brains()
        .require_connection(&name, attachment_id, connection_id)
        .map_err(brain_state_conflict)?;
    claims_match_attachment(claims.as_ref(), &attachment)?;
    let brain_id = server
        .shared_brains()
        .snapshot(&name)
        .map_err(brain_state_conflict)?
        .brain_id;
    server
        .shared_brains()
        .detach(&name, attachment_id, connection_id)
        .map_err(brain_state_conflict)?;
    server
        .brain_approvals()
        .cancel_attachment(brain_id, attachment_id);
    server
        .shared_brains()
        .remove_if_unused(&name)
        .map_err(brain_state_conflict)?;
    Ok(StatusCode::NO_CONTENT)
}

fn brain_state_conflict(error: anyhow::Error) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({ "error": error.to_string() })),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct AcquireNamedBrainRunnerRequest {
    subject: String,
    environment: crate::brain::shared::BrainEnvironment,
    lease_id: Option<crate::brain::shared::RunnerLeaseId>,
    ttl_ms: u64,
}

async fn acquire_named_brain_runner(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path(name): Path<String>,
    Json(request): Json<AcquireNamedBrainRunnerRequest>,
) -> Result<Json<crate::brain::shared::BrainRunnerLease>, Response> {
    if !addr.ip().is_loopback() {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "remote runner leases require scoped environment credentials"
            })),
        )
            .into_response());
    }
    if &request.environment != server.shared_brains().environment() {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "runner environment does not match the daemon Brain environment"
            })),
        )
            .into_response());
    }
    let lease = server
        .shared_brains()
        .acquire_runner_lease(
            &name,
            &request.subject,
            request.environment.generation,
            request.lease_id,
            request.ttl_ms,
        )
        .map_err(|error| AppError(error).into_response())?;
    let store = server.shared_brains().clone();
    let lease_id = lease.lease_id;
    let expires_ms = lease.expires_ms;
    tokio::spawn(async move {
        loop {
            let delay_ms = expires_ms.saturating_sub(unix_epoch_millis());
            if delay_ms == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        if store
            .expire_runner_lease(&name, lease_id, unix_epoch_millis())
            .is_ok_and(|expired| expired)
        {
            let _ = store.remove_if_unused(&name);
        }
    });
    Ok(Json(lease))
}

fn unix_epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

async fn release_named_brain_runner(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Path((name, lease_id)): Path<(String, uuid::Uuid)>,
) -> Result<StatusCode, Response> {
    if !addr.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    server
        .shared_brains()
        .release_runner_lease(&name, crate::brain::shared::RunnerLeaseId(lease_id))
        .map_err(|error| AppError(error).into_response())?;
    server
        .shared_brains()
        .remove_if_unused(&name)
        .map_err(|error| AppError(error).into_response())?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Serialize)]
struct ArchiveNamedBrainResponse {
    name: String,
    archived_to: Option<String>,
}

async fn archive_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Result<Json<ArchiveNamedBrainResponse>, Response> {
    authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::EnvironmentAdmin,
    )?;
    let execution_lock = server
        .shared_brains()
        .execution_lock(&name)
        .map_err(|error| AppError(error).into_response())?;
    let _turn = execution_lock.lock_owned().await;
    let archived_to = server
        .shared_brains()
        .archive(&name)
        .map_err(|error| AppError(error).into_response())?;
    Ok(Json(ArchiveNamedBrainResponse {
        name,
        archived_to: archived_to.map(|path| path.display().to_string()),
    }))
}

#[derive(Debug, Deserialize)]
struct PushNamedBrainEvent {
    attachment_id: crate::brain::shared::AttachmentId,
    connection_id: crate::brain::shared::ConnectionId,
    #[serde(flatten)]
    kind: crate::brain::shared::BrainEventKind,
}

#[derive(Debug, Serialize)]
struct PushNamedBrainResponse {
    accepted: crate::brain::shared::BrainEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    run: Option<crate::brain::shared::BrainRun>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<crate::brain::shared::BrainEvent>,
}

fn attachment_can_submit(
    role: crate::brain::shared::AttachmentRole,
    kind: &crate::brain::shared::BrainEventKind,
) -> bool {
    use crate::brain::shared::{AttachmentRole, BrainEventKind};
    match role {
        AttachmentRole::Driver => matches!(
            kind,
            BrainEventKind::Prompt { .. }
                | BrainEventKind::Program { .. }
                | BrainEventKind::ProgramPopped { .. }
                | BrainEventKind::ApprovalDecided { .. }
        ),
        AttachmentRole::Consultant => matches!(
            kind,
            BrainEventKind::Prompt { .. } | BrainEventKind::ApprovalDecided { .. }
        ),
        AttachmentRole::Observer | AttachmentRole::Runner => false,
    }
}

async fn push_named_brain_event(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(request): Json<PushNamedBrainEvent>,
) -> Result<Json<PushNamedBrainResponse>, Response> {
    use crate::brain::shared::BrainEventKind;

    let required_scope = if matches!(request.kind, BrainEventKind::ApprovalDecided { .. }) {
        crate::brain::credential::BrainCredentialScope::BrainApprove
    } else {
        crate::brain::credential::BrainCredentialScope::BrainSubmit
    };
    let claims = authorize_named_brain(&server, addr, &headers, &name, required_scope)?;
    let attachment = server
        .shared_brains()
        .require_connection(&name, request.attachment_id, request.connection_id)
        .map_err(|error| AppError(error).into_response())?;
    claims_match_attachment(claims.as_ref(), &attachment)?;
    if !matches!(
        request.kind,
        BrainEventKind::Prompt { .. }
            | BrainEventKind::Program { .. }
            | BrainEventKind::ProgramPopped { .. }
            | BrainEventKind::ApprovalDecided { .. }
    ) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "internal Brain events cannot be submitted through the program endpoint"
            })),
        )
            .into_response());
    }
    if !attachment_can_submit(attachment.role, &request.kind) {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "attachment role cannot submit this Brain event"
            })),
        )
            .into_response());
    }
    if let BrainEventKind::ApprovalDecided {
        request_seq,
        approval_id,
        decision,
    } = &request.kind
    {
        let accepted = commit_named_brain_approval_decision(
            server.shared_brains(),
            server.brain_approvals(),
            &name,
            &attachment,
            *request_seq,
            approval_id,
            decision.clone(),
        )
        .map_err(brain_state_conflict)?;
        return Ok(Json(PushNamedBrainResponse {
            accepted,
            run: None,
            result: None,
        }));
    }
    // A Brain is one ordered conversation and one authoritative VM revision.
    // Hold its lane from input acceptance through the corresponding result so
    // two attached consoles cannot race commits or interleave turn events.
    let execution_lock = server
        .shared_brains()
        .execution_lock(&name)
        .map_err(|error| AppError(error).into_response())?;
    let _turn = execution_lock.lock_owned().await;
    let accepted = server
        .shared_brains()
        .push(&name, &attachment.subject, request.kind.clone())
        .map_err(|error| AppError(error).into_response())?;

    let run = if matches!(
        request.kind,
        BrainEventKind::Program { .. } | BrainEventKind::Prompt { .. }
    ) {
        let status = if named_brain_runner_is_ready(
            server.shared_brains(),
            server.brain_runners(),
            &name,
        )
        .map_err(|error| AppError(error).into_response())?
        {
            crate::brain::shared::BrainRunStatus::Running
        } else {
            crate::brain::shared::BrainRunStatus::QueuedForEnvironment
        };
        Some(
            server
                .shared_brains()
                .start_run(
                    &name,
                    &attachment.subject,
                    crate::brain::shared::BrainRunKind::Interactive,
                    accepted.seq,
                    attachment.attachment_id,
                    status,
                )
                .map_err(|error| AppError(error).into_response())?,
        )
    } else {
        None
    };

    let result = match run.as_ref() {
        Some(run) if run.status == crate::brain::shared::BrainRunStatus::Running => Some(
            dispatch_named_brain_run(server.shared_brains(), server.brain_runners(), &name, run)
                .await,
        ),
        Some(_) => None,
        None => match request.kind {
            BrainEventKind::ProgramPopped { .. }
            | BrainEventKind::ToolCall { .. }
            | BrainEventKind::ToolResult { .. }
            | BrainEventKind::ApprovalRequested { .. }
            | BrainEventKind::ApprovalDecided { .. }
            | BrainEventKind::Result { .. }
            | BrainEventKind::RuntimeCommitted { .. }
            | BrainEventKind::RunnerLeaseAcquired { .. }
            | BrainEventKind::RunnerLeaseReleased { .. }
            | BrainEventKind::ClientAttached { .. }
            | BrainEventKind::ClientDetached { .. }
            | BrainEventKind::RunStarted { .. }
            | BrainEventKind::RunStatusChanged { .. } => None,
            BrainEventKind::Program { .. } | BrainEventKind::Prompt { .. } => {
                unreachable!("executable requests create a BrainRun")
            }
        },
    };
    let result = match result {
        Some(result) => result.map_err(|error| AppError(error).into_response())?,
        None => None,
    };

    Ok(Json(PushNamedBrainResponse {
        accepted,
        run,
        result,
    }))
}

fn named_brain_runner_is_ready(
    store: &crate::brain::shared::SharedBrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
) -> anyhow::Result<bool> {
    let snapshot = store.snapshot(name)?;
    ensure_named_brain_store_environment(store, &snapshot)?;
    Ok(snapshot.runner_lease.as_ref().is_some_and(|lease| {
        lease.environment_generation == snapshot.environment.generation
            && lease.expires_ms > crate::brain::shared::unix_millis()
            && runners.has_registration(name, lease.lease_id)
    }))
}

async fn dispatch_named_brain_run(
    store: &crate::brain::shared::SharedBrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    run: &crate::brain::shared::BrainRun,
) -> anyhow::Result<Option<crate::brain::shared::BrainEvent>> {
    use crate::brain::shared::{BrainEventKind, BrainRunStatus};

    let snapshot = store.snapshot(name)?;
    let request = match snapshot
        .events
        .iter()
        .find(|event| event.seq == run.request_seq)
        .cloned()
    {
        Some(request) => request,
        None => {
            let detail = format!("Brain run {} request event is missing", run.run_id.0);
            let result = push_named_brain_result(
                store,
                name,
                run.request_seq,
                Err(anyhow::anyhow!(detail.clone())),
            )?;
            store.transition_run(
                name,
                "daemon",
                run.run_id,
                BrainRunStatus::Failed,
                Some(detail),
            )?;
            return Ok(Some(result));
        }
    };
    let execution = match request.kind {
        BrainEventKind::Program { language, source } => {
            match dispatch_named_brain_program(
                store,
                runners,
                name,
                request.seq,
                language,
                &source,
            )
            .await
            {
                Ok(output) => push_named_brain_result(store, name, request.seq, Ok(output)),
                Err(error) => Err(error),
            }
        }
        BrainEventKind::Prompt { text } => {
            match snapshot
                .attachments
                .iter()
                .find(|attachment| attachment.attachment_id == run.initiating_attachment_id)
            {
                Some(requester) => {
                    dispatch_named_brain_turn(
                        store,
                        runners,
                        name,
                        request.seq,
                        &text,
                        requester,
                    )
                    .await
                }
                None => Err(anyhow::anyhow!(
                        "Brain run {} initiating attachment is missing",
                        run.run_id.0
                )),
            }
        }
        _ => Err(anyhow::anyhow!(
            "Brain run {} request event is not executable",
            run.run_id.0
        )),
    };

    match execution {
        Ok(result) => {
            store.transition_run(
                name,
                "daemon",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )?;
            Ok(Some(result))
        }
        Err(error) => {
            let detail = error.to_string();
            let result = push_named_brain_result(
                store,
                name,
                request.seq,
                Err(anyhow::anyhow!(detail.clone())),
            )?;
            store.transition_run(
                name,
                "daemon",
                run.run_id,
                BrainRunStatus::Failed,
                Some(detail),
            )?;
            Ok(Some(result))
        }
    }
}

/// Drain durable work that arrived while the environment runner was absent.
/// The exact lease that registered the callback must still be current before
/// each run begins; work that has not begun remains queued on disconnect.
pub(crate) async fn resume_queued_named_brain_runs(
    store: crate::brain::shared::SharedBrainStore,
    runners: crate::server::BrainRunnerBroker,
    name: String,
    lease_id: crate::brain::shared::RunnerLeaseId,
) -> anyhow::Result<usize> {
    use crate::brain::shared::BrainRunStatus;

    let execution_lock = store.execution_lock(&name)?;
    let _turn = execution_lock.lock_owned().await;
    let queued = store
        .snapshot(&name)?
        .runs
        .into_iter()
        .filter(|run| run.status == BrainRunStatus::QueuedForEnvironment)
        .collect::<Vec<_>>();
    let mut resumed = 0;
    for run in queued {
        let snapshot = store.snapshot(&name)?;
        let lease_is_current = snapshot.runner_lease.as_ref().is_some_and(|lease| {
            lease.lease_id == lease_id
                && lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::shared::unix_millis()
        });
        if !lease_is_current || !runners.has_registration(&name, lease_id) {
            break;
        }
        let running = store.transition_run(
            &name,
            "daemon",
            run.run_id,
            BrainRunStatus::Running,
            None,
        )?;
        dispatch_named_brain_run(&store, &runners, &name, &running).await?;
        resumed += 1;
    }
    Ok(resumed)
}

fn commit_named_brain_approval_decision(
    store: &crate::brain::shared::SharedBrainStore,
    approvals: &crate::server::BrainApprovalBroker,
    name: &str,
    attachment: &crate::brain::shared::BrainAttachment,
    request_seq: u64,
    approval_id: &str,
    decision: serde_json::Value,
) -> anyhow::Result<crate::brain::shared::BrainEvent> {
    let snapshot = store.snapshot(name)?;
    let claimed = approvals.claim(snapshot.brain_id, approval_id, attachment.attachment_id)?;
    if claimed.request_seq != request_seq
        || claimed.audience.brain_id != snapshot.brain_id
        || claimed.audience.brain != name
        || claimed.audience.attachment_id != attachment.attachment_id
        || claimed.audience.subject != attachment.subject
        || claimed.audience.role != attachment.role
        || claimed.audience.environment_generation != snapshot.environment.generation
    {
        claimed.fail("approval decision no longer matches its addressed attachment");
        anyhow::bail!("approval decision no longer matches its addressed attachment");
    }
    let accepted = match store.push(
        name,
        &attachment.subject,
        crate::brain::shared::BrainEventKind::ApprovalDecided {
            request_seq,
            approval_id: approval_id.to_string(),
            decision: decision.clone(),
        },
    ) {
        Ok(accepted) => accepted,
        Err(error) => {
            claimed.fail(error.to_string());
            return Err(error);
        }
    };
    // The canonical event is durable before the suspended runner sees the
    // decision, so reconnect/replay cannot observe an unaudited grant.
    claimed.complete(decision);
    Ok(accepted)
}

fn push_named_brain_result(
    store: &crate::brain::shared::SharedBrainStore,
    name: &str,
    request_seq: u64,
    result: anyhow::Result<String>,
) -> anyhow::Result<crate::brain::shared::BrainEvent> {
    let (output, error) = match result {
        Ok(output) => (output, None),
        Err(error) => (String::new(), Some(error.to_string())),
    };
    store.push(
        name,
        "daemon",
        crate::brain::shared::BrainEventKind::Result {
            request_seq,
            output,
            error,
        },
    )
}

async fn dispatch_named_brain_program(
    store: &crate::brain::shared::SharedBrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    request_seq: u64,
    language: crate::brain::shared::ProgramLanguage,
    source: &str,
) -> anyhow::Result<String> {
    let snapshot = store.snapshot(name)?;
    ensure_named_brain_store_environment(store, &snapshot)?;
    let lease_id = snapshot
        .runner_lease
        .as_ref()
        .filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::shared::unix_millis()
        })
        .map(|lease| lease.lease_id)
        .ok_or_else(|| anyhow::anyhow!("named Brain '{name}' has no live environment runner"))?;
    let outcome = runners
        .dispatch_program(name, lease_id, request_seq, language, source.to_string())
        .await?;
    store.commit_runner_runtime(
        name,
        request_seq,
        outcome.runtime_revision,
        outcome.checkpoint,
    )?;
    Ok(outcome.output)
}

async fn dispatch_named_brain_turn(
    store: &crate::brain::shared::SharedBrainStore,
    runners: &crate::server::BrainRunnerBroker,
    name: &str,
    request_seq: u64,
    prompt: &str,
    requester: &crate::brain::shared::BrainAttachment,
) -> anyhow::Result<crate::brain::shared::BrainEvent> {
    let snapshot = store.snapshot(name)?;
    ensure_named_brain_store_environment(store, &snapshot)?;
    let lease = snapshot
        .runner_lease
        .as_ref()
        .filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::shared::unix_millis()
        })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("named Brain '{name}' has no live environment runner"))?;
    let lease_id = lease.lease_id;
    let approval_audience = crate::brain::shared::BrainApprovalAudience {
        brain_id: snapshot.brain_id,
        brain: name.to_string(),
        attachment_id: requester.attachment_id,
        subject: requester.subject.clone(),
        role: requester.role,
        environment_generation: snapshot.environment.generation,
    };
    let outcome = match runners
        .dispatch_turn(
            name,
            lease_id,
            request_seq,
            prompt.to_string(),
            named_brain_provider_messages(&snapshot),
            approval_audience.clone(),
        )
        .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<crate::server::RunnerTurnError>() {
                persist_named_brain_turn_events(
                    store,
                    name,
                    request_seq,
                    &lease.subject,
                    &approval_audience,
                    failure.turn_events.clone(),
                )?;
            }
            return Err(error);
        }
    };
    persist_named_brain_turn_events(
        store,
        name,
        request_seq,
        &lease.subject,
        &approval_audience,
        outcome.turn_events,
    )?;
    let program = store.push(
        name,
        "provider",
        crate::brain::shared::BrainEventKind::Program {
            language: outcome.language,
            source: outcome.source,
        },
    )?;
    store.commit_runner_runtime(
        name,
        program.seq,
        outcome.runtime_revision,
        outcome.checkpoint,
    )?;
    push_named_brain_result(store, name, program.seq, Ok(outcome.output))
}

fn persist_named_brain_turn_events(
    store: &crate::brain::shared::SharedBrainStore,
    name: &str,
    request_seq: u64,
    runner_subject: &str,
    expected_approval_audience: &crate::brain::shared::BrainApprovalAudience,
    turn_events: Vec<crate::server::RunnerTurnEvent>,
) -> anyhow::Result<()> {
    let mut persisted = store
        .snapshot(name)?
        .events
        .into_iter()
        .filter_map(|event| match event.kind {
            crate::brain::shared::BrainEventKind::ToolCall {
                request_seq: event_request,
                tool_id,
                ..
            } if event_request == request_seq => Some(format!("call:{tool_id}")),
            crate::brain::shared::BrainEventKind::ToolResult {
                request_seq: event_request,
                tool_id,
                ..
            } if event_request == request_seq => Some(format!("result:{tool_id}")),
            crate::brain::shared::BrainEventKind::ApprovalRequested {
                request_seq: event_request,
                approval_id,
                ..
            } if event_request == request_seq => Some(format!("approval:{approval_id}")),
            crate::brain::shared::BrainEventKind::ApprovalDecided {
                request_seq: event_request,
                approval_id,
                ..
            } if event_request == request_seq => Some(format!("decision:{approval_id}")),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    for turn_event in turn_events {
        match turn_event {
            crate::server::RunnerTurnEvent::Call {
                tool_id,
                name: tool_name,
                input,
            } => {
                if !persisted.insert(format!("call:{tool_id}")) {
                    continue;
                }
                store.push(
                    name,
                    "provider",
                    crate::brain::shared::BrainEventKind::ToolCall {
                        request_seq,
                        tool_id,
                        name: tool_name,
                        input,
                    },
                )?;
            }
            crate::server::RunnerTurnEvent::Result {
                tool_id,
                output,
                is_error,
            } => {
                if !persisted.insert(format!("result:{tool_id}")) {
                    continue;
                }
                store.push(
                    name,
                    "runner",
                    crate::brain::shared::BrainEventKind::ToolResult {
                        request_seq,
                        tool_id,
                        output,
                        is_error,
                    },
                )?;
            }
            crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id,
                approval_kind,
                subject,
                audience,
                detail,
            } => {
                anyhow::ensure!(
                    audience == *expected_approval_audience,
                    "runner substituted the approval audience for request {request_seq}"
                );
                if !persisted.insert(format!("approval:{approval_id}")) {
                    continue;
                }
                store.push(
                    name,
                    "runner",
                    crate::brain::shared::BrainEventKind::ApprovalRequested {
                        request_seq,
                        approval_id,
                        approval_kind,
                        subject,
                        audience: Some(expected_approval_audience.clone()),
                        detail,
                    },
                )?;
            }
            crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id,
                decision,
            } => {
                if !persisted.insert(format!("decision:{approval_id}")) {
                    continue;
                }
                store.push(
                    name,
                    runner_subject,
                    crate::brain::shared::BrainEventKind::ApprovalDecided {
                        request_seq,
                        approval_id,
                        decision,
                    },
                )?;
            }
        }
    }
    Ok(())
}

fn named_brain_provider_messages(snapshot: &crate::brain::shared::BrainSnapshot) -> Vec<Message> {
    use crate::brain::shared::BrainEventKind;

    let events = snapshot
        .events
        .iter()
        .rev()
        .filter(|event| {
            !matches!(
                event.kind,
                BrainEventKind::RuntimeCommitted { .. }
                    | BrainEventKind::ApprovalRequested { .. }
                    | BrainEventKind::ApprovalDecided { .. }
                    | BrainEventKind::RunnerLeaseAcquired { .. }
                    | BrainEventKind::RunnerLeaseReleased { .. }
                    | BrainEventKind::ClientAttached { .. }
                    | BrainEventKind::ClientDetached { .. }
                    | BrainEventKind::RunStarted { .. }
                    | BrainEventKind::RunStatusChanged { .. }
            )
        })
        .take(80)
        .collect::<Vec<_>>();
    let projected = events
        .into_iter()
        .rev()
        .map(|event| match &event.kind {
            BrainEventKind::Prompt { text } => Message::user(format!("[{}]\n{text}", event.sender)),
            BrainEventKind::ToolCall {
                tool_id,
                name,
                input,
                ..
            } => Message::with_content(
                "assistant",
                vec![crate::claude::ContentBlock::ToolUse {
                    id: tool_id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }],
            ),
            BrainEventKind::ToolResult {
                tool_id,
                output,
                is_error,
                ..
            } => Message::with_content(
                "user",
                vec![crate::claude::ContentBlock::ToolResult {
                    tool_use_id: tool_id.clone(),
                    content: output.clone(),
                    is_error: is_error.then_some(true),
                }],
            ),
            BrainEventKind::Program {
                language: _,
                source,
            } if event.sender == "provider" => Message::assistant(source.clone()),
            BrainEventKind::Program { language, source } => Message::user(format!(
                "[{} submitted a Finch {} program as event #{}]\n{}",
                event.sender,
                match language {
                    crate::brain::shared::ProgramLanguage::Forth => "Co-Forth",
                    crate::brain::shared::ProgramLanguage::Lisp => "Lisp",
                },
                event.seq,
                source,
            )),
            BrainEventKind::ProgramPopped { program_seq } => Message::user(format!(
                "[{} removed program event #{} from the visible Brain projection]",
                event.sender, program_seq,
            )),
            BrainEventKind::Result {
                request_seq,
                output,
                error,
            } => {
                let result = error
                    .as_ref()
                    .map(|error| format!("error: {error}"))
                    .unwrap_or_else(|| output.clone());
                Message::user(format!(
                    "[Finch VM result for program event #{request_seq}]\n{result}"
                ))
            }
            BrainEventKind::RuntimeCommitted { .. }
            | BrainEventKind::ApprovalRequested { .. }
            | BrainEventKind::ApprovalDecided { .. }
            | BrainEventKind::RunnerLeaseAcquired { .. }
            | BrainEventKind::RunnerLeaseReleased { .. }
            | BrainEventKind::ClientAttached { .. }
            | BrainEventKind::ClientDetached { .. }
            | BrainEventKind::RunStarted { .. }
            | BrainEventKind::RunStatusChanged { .. } => unreachable!("filtered above"),
        })
        .collect::<Vec<_>>();

    // Parallel provider calls are one assistant message, followed by one user
    // message containing their results. The event log deliberately stores one
    // lifecycle item per event; rebuild the provider protocol grouping here
    // instead of emitting invalid consecutive assistant/user messages.
    let mut messages: Vec<Message> = Vec::with_capacity(projected.len());
    for mut message in projected {
        let block_kind = message.content.first().map(|block| match block {
            crate::claude::ContentBlock::ToolUse { .. } => 1,
            crate::claude::ContentBlock::ToolResult { .. } => 2,
            _ => 0,
        });
        let merge = messages.last().is_some_and(|previous| {
            previous.role == message.role
                && block_kind.is_some_and(|kind| kind != 0)
                && previous.content.iter().all(|block| {
                    matches!(
                        (block_kind, block),
                        (Some(1), crate::claude::ContentBlock::ToolUse { .. })
                            | (Some(2), crate::claude::ContentBlock::ToolResult { .. })
                    )
                })
        });
        if merge {
            messages
                .last_mut()
                .expect("merge requires a preceding message")
                .content
                .append(&mut message.content);
        } else {
            messages.push(message);
        }
    }
    messages
}

fn ensure_named_brain_store_environment(
    store: &crate::brain::shared::SharedBrainStore,
    snapshot: &crate::brain::shared::BrainSnapshot,
) -> anyhow::Result<()> {
    let configured = store.environment();
    if &snapshot.environment != configured {
        anyhow::bail!("brain environment generation does not match this execution host");
    }
    let process_workspace = std::env::current_dir()?;
    let process_workspace = process_workspace
        .canonicalize()
        .unwrap_or(process_workspace);
    if process_workspace != configured.workspace {
        anyhow::bail!(
            "brain workspace {} is not active on this execution host",
            configured.workspace.display()
        );
    }
    Ok(())
}

async fn watch_named_brain(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Query(connection): Query<WatchNamedBrainQuery>,
    ws: axum::extract::WebSocketUpgrade,
) -> Result<Response, Response> {
    let claims = authorize_named_brain(
        &server,
        addr,
        &headers,
        &name,
        crate::brain::credential::BrainCredentialScope::BrainRead,
    )?;
    let attachment_id = crate::brain::shared::AttachmentId(connection.attachment_id);
    let connection_id = crate::brain::shared::ConnectionId(connection.connection_id);
    let attachment = server
        .shared_brains()
        .require_connection(&name, attachment_id, connection_id)
        .map_err(brain_state_conflict)?;
    claims_match_attachment(claims.as_ref(), &attachment)?;
    server
        .shared_brains()
        .activate_connection(&name, attachment_id, connection_id)
        .map_err(|error| AppError(error).into_response())?;
    // Subscribe before taking the snapshot. Events appended after the
    // snapshot revision then wait in this receiver and are sent immediately
    // afterward, so an attaching console cannot miss the gap between two
    // independent HTTP/WebSocket requests.
    let mut events = server
        .shared_brains()
        .subscribe(&name)
        .map_err(|error| AppError(error).into_response())?;
    let snapshot = server
        .shared_brains()
        .snapshot(&name)
        .map_err(|error| AppError(error).into_response())?;
    let brain_id = snapshot.brain_id;
    let store = server.shared_brains().clone();
    let approvals = server.brain_approvals().clone();
    Ok(ws
        .on_upgrade(move |mut socket| async move {
            use axum::extract::ws::Message as WsMessage;
            let initial = crate::brain::shared::BrainWireMessage::Snapshot { brain: snapshot };
            if let Ok(encoded) = crate::ipc::brain_codec::encode_brain_wire_message(&initial) {
                if socket
                    .send(WsMessage::Binary(encoded.into()))
                    .await
                    .is_err()
                {
                    let _ = store.detach(&name, attachment_id, connection_id);
                    approvals.cancel_attachment(brain_id, attachment_id);
                    let _ = store.remove_if_unused(&name);
                    return;
                }
            }
            loop {
                let wire = tokio::select! {
                    incoming = socket.recv() => match incoming {
                        Some(Ok(WsMessage::Ping(payload))) => {
                            if socket.send(WsMessage::Pong(payload)).await.is_err() {
                                break;
                            }
                            continue;
                        }
                        Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => continue,
                    },
                    event = events.recv() => match event {
                        Ok(event) => crate::brain::shared::BrainWireMessage::Event { event },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let Ok(brain) = store.snapshot(&name) else {
                                break;
                            };
                            crate::brain::shared::BrainWireMessage::Snapshot { brain }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                };
                if store
                    .require_connection(&name, attachment_id, connection_id)
                    .is_err()
                {
                    break;
                }
                let Ok(encoded) = crate::ipc::brain_codec::encode_brain_wire_message(&wire) else {
                    continue;
                };
                if socket
                    .send(WsMessage::Binary(encoded.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            let _ = store.detach(&name, attachment_id, connection_id);
            approvals.cancel_attachment(brain_id, attachment_id);
            let _ = store.remove_if_unused(&name);
        })
        .into_response())
}

#[derive(Debug, Deserialize)]
struct WatchNamedBrainQuery {
    attachment_id: uuid::Uuid,
    connection_id: uuid::Uuid,
}

async fn show_brain_password(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    if !addr.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(Json(serde_json::json!({
        "password": server.brain_password().await
    })))
}

#[derive(Debug, Deserialize)]
struct ChangeBrainPassword {
    password: String,
}

async fn change_brain_password(
    State(server): State<Arc<AgentServer>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(request): Json<ChangeBrainPassword>,
) -> Result<StatusCode, Response> {
    if !addr.ip().is_loopback() {
        return Err(StatusCode::FORBIDDEN.into_response());
    }
    if request.password.trim().len() < 12 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "brain password must be at least 12 characters"})),
        )
            .into_response());
    }
    let mut config =
        crate::config::load_config().map_err(|error| AppError(error).into_response())?;
    config.server.brain_password = request.password.clone();
    config
        .save()
        .map_err(|error| AppError(error).into_response())?;
    server.set_brain_password(request.password).await;
    Ok(StatusCode::NO_CONTENT)
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
                    crate::session::SessionEvent::Chat { text } => Some((lbl.clone(), text)),
                    crate::session::SessionEvent::ChannelMessage {
                        channel,
                        sender,
                        bundle,
                    } => {
                        let primary = bundle.primary();
                        let comment = if bundle.comments.is_empty() {
                            String::new()
                        } else {
                            format!("  \\ {}", bundle.comments.join("; "))
                        };
                        Some((
                            lbl.clone(),
                            format!("{channel} {sender}: {}{comment}", primary.code),
                        ))
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
        let rel = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
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
                    let rel = p
                        .strip_prefix(base)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .to_string();
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
async fn handle_file_get(
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
async fn handle_file_put(
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
            let out_norm = out_path
                .components()
                .fold(std::path::PathBuf::new(), |mut acc, c| match c {
                    std::path::Component::ParentDir => {
                        acc.pop();
                        acc
                    }
                    _ => {
                        acc.push(c);
                        acc
                    }
                });
            if !out_norm.starts_with(&dest_canon) && !out_norm.starts_with(dest_p) {
                anyhow::bail!("zip-slip detected in entry '{}'", entry.name());
            }
            if entry.is_dir() {
                std::fs::create_dir_all(&out_path)?;
            } else {
                if let Some(p) = out_path.parent() {
                    std::fs::create_dir_all(p)?;
                }
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

#[cfg(test)]
mod named_brain_provider_context_tests {
    use super::*;
    use crate::brain::shared::{
        AttachmentId, AttachmentRole, BrainApprovalAudience, BrainAttachment, BrainEnvironment,
        BrainEvent, BrainEventKind, BrainId, BrainSnapshot, ProgramLanguage,
    };

    fn driver_attachment(subject: &str) -> BrainAttachment {
        BrainAttachment {
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: subject.into(),
            role: AttachmentRole::Driver,
            acknowledged_seq: 0,
            connected: true,
            connection_id: Some(crate::brain::shared::ConnectionId(uuid::Uuid::new_v4())),
        }
    }

    #[test]
    fn participant_credentials_are_least_privilege_by_role() {
        use crate::brain::credential::BrainCredentialScope;

        let driver = participant_scopes(AttachmentRole::Driver);
        assert!(driver.contains(&BrainCredentialScope::BrainRead));
        assert!(driver.contains(&BrainCredentialScope::BrainSubmit));
        assert!(driver.contains(&BrainCredentialScope::BrainApprove));
        assert!(driver.contains(&BrainCredentialScope::BrainControl));
        assert!(!driver.contains(&BrainCredentialScope::EnvironmentExecute));
        assert!(!driver.contains(&BrainCredentialScope::EnvironmentAdmin));
        assert!(!driver.contains(&BrainCredentialScope::ComputeSubmit));

        let observer = participant_scopes(AttachmentRole::Observer);
        assert!(observer.contains(&BrainCredentialScope::BrainRead));
        assert!(observer.contains(&BrainCredentialScope::BrainControl));
        assert!(!observer.contains(&BrainCredentialScope::BrainSubmit));
        assert!(!observer.contains(&BrainCredentialScope::BrainApprove));
    }

    fn event(seq: u64, sender: &str, kind: BrainEventKind) -> BrainEvent {
        BrainEvent {
            schema_version: 2,
            brain_id: BrainId(uuid::Uuid::nil()),
            seq,
            environment_generation: 1,
            sender: sender.into(),
            created_ms: 0,
            kind,
        }
    }

    #[test]
    fn brain_history_remains_conversation_data_not_system_text() {
        let snapshot = BrainSnapshot {
            brain_id: BrainId(uuid::Uuid::nil()),
            name: "shared".into(),
            environment: BrainEnvironment {
                machine: "box.local".into(),
                workspace: "/workspace".into(),
                generation: 1,
            },
            revision: 4,
            events: vec![
                event(
                    1,
                    "driver",
                    BrainEventKind::Prompt {
                        text: "compute it".into(),
                    },
                ),
                event(
                    2,
                    "provider",
                    BrainEventKind::Program {
                        language: ProgramLanguage::Lisp,
                        source: "(say \"answer\")".into(),
                    },
                ),
                event(
                    3,
                    "daemon",
                    BrainEventKind::RuntimeCommitted {
                        request_seq: 2,
                        runtime_revision: 1,
                        checkpoint_sha256: "hash".into(),
                    },
                ),
                event(
                    4,
                    "daemon",
                    BrainEventKind::Result {
                        request_seq: 2,
                        output: "answer".into(),
                        error: None,
                    },
                ),
            ],
            program_stack: Vec::new(),
            attachments: Vec::new(),
            runner_lease: None,
            runs: Vec::new(),
        };

        let messages = named_brain_provider_messages(&snapshot);
        assert_eq!(messages.len(), 3, "internal checkpoint events stay hidden");
        assert_eq!(messages[0].role, "user");
        assert!(messages[0].text_content().contains("[driver]"));
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text_content(), "(say \"answer\")");
        assert_eq!(messages[2].role, "user");
        assert!(messages[2].text_content().contains("program event #2"));
    }

    #[test]
    fn brain_history_reconstructs_provider_tool_protocol() {
        let snapshot = BrainSnapshot {
            brain_id: BrainId(uuid::Uuid::nil()),
            name: "shared".into(),
            environment: BrainEnvironment {
                machine: "box.local".into(),
                workspace: "/workspace".into(),
                generation: 1,
            },
            revision: 6,
            events: vec![
                event(
                    1,
                    "driver",
                    BrainEventKind::Prompt {
                        text: "inspect fib".into(),
                    },
                ),
                event(
                    2,
                    "provider",
                    BrainEventKind::ToolCall {
                        request_seq: 1,
                        tool_id: "tool-1".into(),
                        name: "search_word".into(),
                        input: serde_json::json!({"query": "fib"}),
                    },
                ),
                event(
                    3,
                    "provider",
                    BrainEventKind::ToolCall {
                        request_seq: 1,
                        tool_id: "tool-2".into(),
                        name: "get_vm_state".into(),
                        input: serde_json::json!({}),
                    },
                ),
                event(
                    4,
                    "runner",
                    BrainEventKind::ToolResult {
                        request_seq: 1,
                        tool_id: "tool-1".into(),
                        output: "found fib".into(),
                        is_error: false,
                    },
                ),
                event(
                    5,
                    "runner",
                    BrainEventKind::ToolResult {
                        request_seq: 1,
                        tool_id: "tool-2".into(),
                        output: "revision 7".into(),
                        is_error: false,
                    },
                ),
                event(
                    6,
                    "provider",
                    BrainEventKind::Program {
                        language: ProgramLanguage::Lisp,
                        source: "(say \"done\")".into(),
                    },
                ),
            ],
            program_stack: Vec::new(),
            attachments: Vec::new(),
            runner_lease: None,
            runs: Vec::new(),
        };

        let messages = named_brain_provider_messages(&snapshot);
        assert_eq!(messages.len(), 4);
        assert!(matches!(
            &messages[1].content[..],
            [
                crate::claude::ContentBlock::ToolUse { id, name, .. },
                crate::claude::ContentBlock::ToolUse { id: id2, name: name2, .. }
            ] if id == "tool-1" && name == "search_word"
                && id2 == "tool-2" && name2 == "get_vm_state"
        ));
        assert!(matches!(
            &messages[2].content[..],
            [
                crate::claude::ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error: None,
                },
                crate::claude::ContentBlock::ToolResult {
                    tool_use_id: tool_use_id2,
                    content: content2,
                    is_error: None,
                }
            ] if tool_use_id == "tool-1" && content == "found fib"
                && tool_use_id2 == "tool-2" && content2 == "revision 7"
        ));
    }

    #[test]
    fn named_brain_list_fields_expose_their_actual_semantics() {
        let entry = NamedBrainListEntry {
            name: "shared".into(),
            environment: BrainEnvironment {
                machine: "box.local".into(),
                workspace: "/workspace".into(),
                generation: 1,
            },
            event_revision: 21,
            retained_programs: 7,
            runner: None,
        };

        let value = serde_json::to_value(entry).unwrap();
        assert_eq!(value["event_revision"], 21);
        assert_eq!(value["retained_programs"], 7);
        assert!(value.get("revision").is_none());
        assert!(value.get("programs").is_none());
    }

    #[test]
    fn attachment_roles_bound_which_events_the_client_may_submit() {
        use crate::brain::shared::AttachmentRole;

        let prompt = BrainEventKind::Prompt {
            text: "hello".into(),
        };
        let program = BrainEventKind::Program {
            language: ProgramLanguage::Lisp,
            source: "(say \"hello\")".into(),
        };
        let decision = BrainEventKind::ApprovalDecided {
            request_seq: 1,
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "deny"}),
        };
        assert!(attachment_can_submit(AttachmentRole::Driver, &prompt));
        assert!(attachment_can_submit(AttachmentRole::Driver, &program));
        assert!(attachment_can_submit(AttachmentRole::Driver, &decision));
        assert!(attachment_can_submit(AttachmentRole::Consultant, &prompt));
        assert!(attachment_can_submit(AttachmentRole::Consultant, &decision));
        assert!(!attachment_can_submit(AttachmentRole::Consultant, &program));
        assert!(!attachment_can_submit(AttachmentRole::Observer, &prompt));
        assert!(!attachment_can_submit(AttachmentRole::Runner, &program));
    }

    #[tokio::test]
    async fn approval_decision_is_durable_before_the_runner_resumes() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let request_seq = store
            .push(
                "shared",
                "alice@box.local",
                BrainEventKind::Prompt {
                    text: "read it".into(),
                },
            )
            .unwrap()
            .seq;
        let snapshot = store.snapshot("shared").unwrap();
        let attachment = driver_attachment("alice@box.local");
        let audience = BrainApprovalAudience {
            brain_id: snapshot.brain_id,
            brain: snapshot.name,
            attachment_id: attachment.attachment_id,
            subject: attachment.subject.clone(),
            role: attachment.role,
            environment_generation: snapshot.environment.generation,
        };
        let approvals = crate::server::BrainApprovalBroker::default();
        let registration = approvals
            .register(request_seq, "approval-1", audience)
            .unwrap();
        let decision = serde_json::json!({"choice": "approve_once"});

        let accepted = commit_named_brain_approval_decision(
            &store,
            &approvals,
            "shared",
            &attachment,
            request_seq,
            "approval-1",
            decision.clone(),
        )
        .unwrap();
        assert!(matches!(
            accepted.kind,
            BrainEventKind::ApprovalDecided { .. }
        ));
        assert!(store
            .snapshot("shared")
            .unwrap()
            .events
            .iter()
            .any(|event| {
                event.seq == accepted.seq
                    && matches!(
                        &event.kind,
                        BrainEventKind::ApprovalDecided { approval_id, .. }
                            if approval_id == "approval-1"
                    )
            }));
        assert_eq!(registration.wait().await.unwrap(), decision);
    }

    #[tokio::test]
    async fn wrong_attachment_cannot_consume_an_approval_decision() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let request_seq = store
            .push(
                "shared",
                "alice@box.local",
                BrainEventKind::Prompt {
                    text: "read it".into(),
                },
            )
            .unwrap()
            .seq;
        let snapshot = store.snapshot("shared").unwrap();
        let attachment = driver_attachment("alice@box.local");
        let audience = BrainApprovalAudience {
            brain_id: snapshot.brain_id,
            brain: snapshot.name,
            attachment_id: attachment.attachment_id,
            subject: attachment.subject.clone(),
            role: attachment.role,
            environment_generation: snapshot.environment.generation,
        };
        let approvals = crate::server::BrainApprovalBroker::default();
        let registration = approvals
            .register(request_seq, "approval-1", audience)
            .unwrap();
        let intruder = driver_attachment("mallory@box.local");

        assert!(commit_named_brain_approval_decision(
            &store,
            &approvals,
            "shared",
            &intruder,
            request_seq,
            "approval-1",
            serde_json::json!({"choice": "approve_once"}),
        )
        .is_err());
        let claimed = approvals
            .claim(snapshot.brain_id, "approval-1", attachment.attachment_id)
            .unwrap();
        claimed.complete(serde_json::json!({"choice": "deny"}));
        assert_eq!(registration.wait().await.unwrap()["choice"], "deny");
    }

    #[test]
    fn final_turn_flush_deduplicates_live_approval_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let request_seq = store
            .push(
                "shared",
                "alice@box.local",
                BrainEventKind::Prompt { text: "search".into() },
            )
            .unwrap()
            .seq;
        let snapshot = store.snapshot("shared").unwrap();
        let attachment = driver_attachment("alice@box.local");
        let audience = BrainApprovalAudience {
            brain_id: snapshot.brain_id,
            brain: snapshot.name,
            attachment_id: attachment.attachment_id,
            subject: attachment.subject,
            role: attachment.role,
            environment_generation: snapshot.environment.generation,
        };
        store
            .push(
                "shared",
                "provider",
                BrainEventKind::ToolCall {
                    request_seq,
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
            )
            .unwrap();
        store
            .push(
                "shared",
                "runner",
                BrainEventKind::ApprovalRequested {
                    request_seq,
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: Some(audience.clone()),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
            )
            .unwrap();
        store
            .push(
                "shared",
                "alice@box.local",
                BrainEventKind::ApprovalDecided {
                    request_seq,
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
            )
            .unwrap();

        persist_named_brain_turn_events(
            &store,
            "shared",
            request_seq,
            "runner@box.local",
            &audience,
            vec![
                crate::server::RunnerTurnEvent::Call {
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
                crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: audience.clone(),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
                crate::server::RunnerTurnEvent::ApprovalDecided {
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
                crate::server::RunnerTurnEvent::Result {
                    tool_id: "tool-1".into(),
                    output: "found".into(),
                    is_error: false,
                },
            ],
        )
        .unwrap();

        let events = store.snapshot("shared").unwrap().events;
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ToolCall { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ApprovalRequested { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ApprovalDecided { .. }))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ToolResult { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn runner_cannot_substitute_the_daemon_selected_approval_audience() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let snapshot = store.snapshot("shared").unwrap();
        let requester = driver_attachment("alice@box.local");
        let expected = BrainApprovalAudience {
            brain_id: snapshot.brain_id,
            brain: snapshot.name,
            attachment_id: requester.attachment_id,
            subject: requester.subject,
            role: requester.role,
            environment_generation: snapshot.environment.generation,
        };
        let mut substituted = expected.clone();
        substituted.subject = "mallory@box.local".into();

        let error = persist_named_brain_turn_events(
            &store,
            "shared",
            1,
            "runner@box.local",
            &expected,
            vec![crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: "approval-1".into(),
                approval_kind: "tool".into(),
                subject: "bash".into(),
                audience: substituted,
                detail: serde_json::json!({}),
            }],
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("substituted the approval audience"));
        assert!(!store
            .snapshot("shared")
            .unwrap()
            .events
            .iter()
            .any(|event| { matches!(event.kind, BrainEventKind::ApprovalRequested { .. }) }));
    }

    #[tokio::test]
    async fn named_brain_program_runs_on_registered_frontend_and_commits_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let generation = store.environment().generation;
        let lease = store
            .acquire_runner_lease("shared", "console", generation, None, 60_000)
            .unwrap();
        let runners = crate::server::BrainRunnerBroker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runners.register("shared", lease.lease_id, tx);
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
                panic!("expected program request")
            };
            assert_eq!(request.source, "(define (double (n : int)) : int (* n 2))");
            let runtime = crate::runtime::ProgramRuntime::new();
            let outcome = runtime
                .submit_typed_only(crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("frontend-test".into()),
                    source: request.source,
                    intent: "frontend runner test".into(),
                    effect: crate::programs::ExecutionEffect::Pure,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: Some(runtime.revision()),
                    budget: None,
                })
                .await
                .unwrap();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.revision == outcome.output_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .unwrap();
            request
                .response_tx
                .send(Ok(crate::server::RunnerProgramResult {
                    output: "frontend completed".into(),
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                }))
                .unwrap();
        });

        let output = dispatch_named_brain_program(
            &store,
            &runners,
            "shared",
            41,
            ProgramLanguage::Lisp,
            "(define (double (n : int)) : int (* n 2))",
        )
        .await
        .unwrap();
        assert_eq!(output, "frontend completed");

        let restored = store.program_runtime("shared").unwrap();
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("daemon-check".into()),
                source: "21 double".into(),
                intent: "verify committed runner checkpoint".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert!(matches!(
            called.values.as_slice(),
            [crate::programs::ProgramValue::Int(42)]
        ));
        assert!(store
            .snapshot("shared")
            .unwrap()
            .events
            .iter()
            .any(|event| {
                matches!(
                    event.kind,
                    BrainEventKind::RuntimeCommitted {
                        request_seq: 41,
                        ..
                    }
                )
            }));
    }

    #[tokio::test]
    async fn named_brain_prompt_runs_the_full_turn_on_the_registered_frontend() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let prompt = store
            .push(
                "shared",
                "driver@box.local",
                BrainEventKind::Prompt {
                    text: "define triple".into(),
                },
            )
            .unwrap();
        let prompt_seq = prompt.seq;
        let requester = driver_attachment("driver@box.local");
        let generation = store.environment().generation;
        let lease = store
            .acquire_runner_lease("shared", "runner@box.local", generation, None, 60_000)
            .unwrap();
        let runners = crate::server::BrainRunnerBroker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runners.register("shared", lease.lease_id, tx);
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Turn(request) = rx.recv().await.unwrap() else {
                panic!("expected full turn request")
            };
            assert_eq!(request.request_seq, prompt_seq);
            assert_eq!(request.prompt, "define triple");
            assert!(request
                .context
                .iter()
                .any(|message| message.text_content().contains("define triple")));
            let runtime = crate::runtime::ProgramRuntime::new();
            let source = "(define (triple (n : int)) : int (* n 3))";
            let outcome = runtime
                .submit_typed_only(crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("frontend-turn-test".into()),
                    source: source.into(),
                    intent: "frontend full turn test".into(),
                    effect: crate::programs::ExecutionEffect::Pure,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: Some(runtime.revision()),
                    budget: None,
                })
                .await
                .unwrap();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.revision == outcome.output_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .unwrap();
            let approval_audience = request.approval_audience.clone();
            request
                .response_tx
                .send(Ok(crate::server::RunnerTurnResult {
                    source: source.into(),
                    language: ProgramLanguage::Lisp,
                    output: "triple defined".into(),
                    turn_events: vec![
                        crate::server::RunnerTurnEvent::Call {
                            tool_id: "tool-1".into(),
                            name: "search_word".into(),
                            input: serde_json::json!({"query": "triple"}),
                        },
                        crate::server::RunnerTurnEvent::ApprovalRequested {
                            approval_id: "tool-1".into(),
                            approval_kind: "tool".into(),
                            subject: "search_word".into(),
                            audience: approval_audience,
                            detail: serde_json::json!({"input": {"query": "triple"}}),
                        },
                        crate::server::RunnerTurnEvent::ApprovalDecided {
                            approval_id: "tool-1".into(),
                            decision: serde_json::json!({"choice": "approve_once"}),
                        },
                        crate::server::RunnerTurnEvent::Result {
                            tool_id: "tool-1".into(),
                            output: "no matches".into(),
                            is_error: false,
                        },
                    ],
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                }))
                .unwrap();
        });

        let result = dispatch_named_brain_turn(
            &store,
            &runners,
            "shared",
            prompt_seq,
            "define triple",
            &requester,
        )
        .await
        .unwrap();
        let BrainEventKind::Result {
            request_seq,
            output,
            error,
        } = result.kind
        else {
            panic!("expected result event")
        };
        assert_eq!(output, "triple defined");
        assert!(error.is_none());

        let snapshot = store.snapshot("shared").unwrap();
        let kinds = snapshot
            .events
            .iter()
            .filter_map(|event| match &event.kind {
                BrainEventKind::ToolCall { tool_id, .. } => Some(("call", tool_id.as_str())),
                BrainEventKind::ToolResult { tool_id, .. } => Some(("result", tool_id.as_str())),
                BrainEventKind::ApprovalRequested {
                    approval_id,
                    audience: Some(audience),
                    ..
                } => {
                    assert_eq!(audience.brain, "shared");
                    assert_eq!(audience.attachment_id, requester.attachment_id);
                    assert_eq!(audience.subject, "driver@box.local");
                    assert_eq!(audience.role, AttachmentRole::Driver);
                    assert_eq!(
                        audience.environment_generation,
                        store.environment().generation
                    );
                    Some(("approval_requested", approval_id.as_str()))
                }
                BrainEventKind::ApprovalDecided { approval_id, .. } => {
                    assert_eq!(event.sender, "runner@box.local");
                    Some(("approval_decided", approval_id.as_str()))
                }
                BrainEventKind::Program { .. } if event.sender == "provider" => {
                    Some(("program", ""))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                ("call", "tool-1"),
                ("approval_requested", "tool-1"),
                ("approval_decided", "tool-1"),
                ("result", "tool-1"),
                ("program", ""),
            ],
            "turn lifecycle must precede the final provider program in the canonical log"
        );
        assert!(snapshot.events.iter().any(|event| {
            event.seq == request_seq
                && matches!(
                    &event.kind,
                    BrainEventKind::Program { source, .. } if source.contains("triple")
                )
        }));
        assert!(snapshot.events.iter().any(|event| {
            matches!(
                event.kind,
                BrainEventKind::RuntimeCommitted {
                    request_seq: committed,
                    ..
                } if committed == request_seq
            )
        }));
    }

    #[tokio::test]
    async fn failed_named_brain_turn_persists_partial_approval_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let prompt_seq = store
            .push(
                "shared",
                "driver@box.local",
                BrainEventKind::Prompt {
                    text: "try an effect".into(),
                },
            )
            .unwrap()
            .seq;
        let requester = driver_attachment("driver@box.local");
        let lease = store
            .acquire_runner_lease(
                "shared",
                "runner@box.local",
                store.environment().generation,
                None,
                60_000,
            )
            .unwrap();
        let runners = crate::server::BrainRunnerBroker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runners.register("shared", lease.lease_id, tx);
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Turn(request) = rx.recv().await.unwrap() else {
                panic!("expected full turn request")
            };
            let approval_audience = request.approval_audience.clone();
            request
                .response_tx
                .send(Err(crate::server::RunnerTurnError {
                    message: "provider failed after approval".into(),
                    turn_events: vec![
                        crate::server::RunnerTurnEvent::ApprovalRequested {
                            approval_id: "approval-1".into(),
                            approval_kind: "vm_capability".into(),
                            subject: "FileRead".into(),
                            audience: approval_audience,
                            detail: serde_json::json!({"reason": "read manifest"}),
                        },
                        crate::server::RunnerTurnEvent::ApprovalDecided {
                            approval_id: "approval-1".into(),
                            decision: serde_json::json!({"choice": "allow_once"}),
                        },
                    ],
                }))
                .unwrap();
        });

        let error = dispatch_named_brain_turn(
            &store,
            &runners,
            "shared",
            prompt_seq,
            "try an effect",
            &requester,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("provider failed after approval"));
        let snapshot = store.snapshot("shared").unwrap();
        assert!(matches!(
            &snapshot.events[snapshot.events.len() - 2].kind,
            BrainEventKind::ApprovalRequested {
                approval_id,
                audience: Some(audience),
                ..
            }
                if approval_id == "approval-1"
                    && audience.attachment_id == requester.attachment_id
                    && audience.subject == "driver@box.local"
                    && audience.role == AttachmentRole::Driver
        ));
        assert!(matches!(
            &snapshot.events[snapshot.events.len() - 1].kind,
            BrainEventKind::ApprovalDecided { approval_id, decision, .. }
                if approval_id == "approval-1" && decision["choice"] == "allow_once"
        ));
    }

    #[tokio::test]
    async fn named_brain_program_requires_callback_for_the_live_lease() {
        let store = crate::brain::shared::SharedBrainStore::with_root("box.local", None);
        let generation = store.environment().generation;
        store
            .acquire_runner_lease("shared", "console", generation, None, 60_000)
            .unwrap();
        let error = dispatch_named_brain_program(
            &store,
            &crate::server::BrainRunnerBroker::default(),
            "shared",
            1,
            ProgramLanguage::Forth,
            "21 2 *",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("no connected runner callback"));
        assert!(!store
            .snapshot("shared")
            .unwrap()
            .events
            .iter()
            .any(|event| { matches!(event.kind, BrainEventKind::RuntimeCommitted { .. }) }));
    }

    #[tokio::test]
    async fn queued_brain_run_resumes_on_runner_registration_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        let pending = store
            .attach("shared", "alice@box.local", AttachmentRole::Driver, None)
            .unwrap();
        let attachment = store
            .activate_connection(
                "shared",
                pending.attachment_id,
                pending.connection_id.unwrap(),
            )
            .unwrap();
        let request = store
            .push(
                "shared",
                &attachment.subject,
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "(define (double (n : int)) : int (* n 2))".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                &attachment.subject,
                crate::brain::shared::BrainRunKind::Interactive,
                request.seq,
                attachment.attachment_id,
                crate::brain::shared::BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        drop(store);
        let store = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        assert_eq!(
            store.snapshot("shared").unwrap().runs[0].status,
            crate::brain::shared::BrainRunStatus::QueuedForEnvironment
        );
        let lease = store
            .acquire_runner_lease(
                "shared",
                "runner@box.local",
                store.environment().generation,
                None,
                60_000,
            )
            .unwrap();
        let runners = crate::server::BrainRunnerBroker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runners.register("shared", lease.lease_id, tx);
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
                panic!("expected queued program request")
            };
            let runtime = crate::runtime::ProgramRuntime::new();
            let outcome = runtime
                .submit_typed_only(crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source_id: Some("queued-run-test".into()),
                    source: request.source,
                    intent: "resume queued Brain run".into(),
                    effect: crate::programs::ExecutionEffect::Pure,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: Some(runtime.revision()),
                    budget: None,
                })
                .await
                .unwrap();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .into_iter()
                .find(|snapshot| snapshot.revision == outcome.output_revision)
                .and_then(|snapshot| snapshot.checkpoint)
                .unwrap();
            request
                .response_tx
                .send(Ok(crate::server::RunnerProgramResult {
                    output: "definition committed".into(),
                    runtime_revision: outcome.output_revision,
                    checkpoint,
                }))
                .unwrap();
        });

        assert_eq!(
            resume_queued_named_brain_runs(
                store.clone(),
                runners,
                "shared".into(),
                lease.lease_id,
            )
            .await
            .unwrap(),
            1
        );
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.runs[0].run_id, run.run_id);
        assert_eq!(
            snapshot.runs[0].status,
            crate::brain::shared::BrainRunStatus::Completed
        );
        assert!(snapshot.events.iter().any(|event| {
            matches!(
                &event.kind,
                BrainEventKind::Result {
                    request_seq,
                    output,
                    error: None,
                } if *request_seq == request.seq && output == "definition committed"
            )
        }));

        drop(store);
        let restarted = crate::brain::shared::SharedBrainStore::with_root(
            "box.local",
            Some(temp.path().into()),
        );
        assert_eq!(
            restarted.snapshot("shared").unwrap().runs[0].status,
            crate::brain::shared::BrainRunStatus::Completed
        );
    }

    #[tokio::test]
    async fn queued_brain_run_stays_queued_without_the_registered_lease() {
        let store = crate::brain::shared::SharedBrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice@box.local", AttachmentRole::Driver, None)
            .unwrap();
        let request = store
            .push(
                "shared",
                &attachment.subject,
                BrainEventKind::Program {
                    language: ProgramLanguage::Forth,
                    source: "21 2 *".into(),
                },
            )
            .unwrap();
        store
            .start_run(
                "shared",
                &attachment.subject,
                crate::brain::shared::BrainRunKind::Interactive,
                request.seq,
                attachment.attachment_id,
                crate::brain::shared::BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease(
                "shared",
                "runner@box.local",
                store.environment().generation,
                None,
                60_000,
            )
            .unwrap();

        assert_eq!(
            resume_queued_named_brain_runs(
                store.clone(),
                crate::server::BrainRunnerBroker::default(),
                "shared".into(),
                lease.lease_id,
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(
            store.snapshot("shared").unwrap().runs[0].status,
            crate::brain::shared::BrainRunStatus::QueuedForEnvironment
        );
        assert!(!store.snapshot("shared").unwrap().events.iter().any(|event| {
            matches!(event.kind, BrainEventKind::Result { .. })
        }));
    }

    #[tokio::test]
    async fn runner_failure_is_a_durable_failed_run_and_correlated_result() {
        let store = crate::brain::shared::SharedBrainStore::with_root("box.local", None);
        let attachment = store
            .attach("shared", "alice@box.local", AttachmentRole::Driver, None)
            .unwrap();
        let request = store
            .push(
                "shared",
                &attachment.subject,
                BrainEventKind::Program {
                    language: ProgramLanguage::Forth,
                    source: "21 2 *".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                &attachment.subject,
                crate::brain::shared::BrainRunKind::Interactive,
                request.seq,
                attachment.attachment_id,
                crate::brain::shared::BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease(
                "shared",
                "runner@box.local",
                store.environment().generation,
                None,
                60_000,
            )
            .unwrap();
        let runners = crate::server::BrainRunnerBroker::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        runners.register("shared", lease.lease_id, tx);
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
                panic!("expected program request")
            };
            request
                .response_tx
                .send(Err("frontend execution failed".into()))
                .unwrap();
        });

        let result = dispatch_named_brain_run(&store, &runners, "shared", &run)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            result.kind,
            BrainEventKind::Result {
                request_seq,
                ref error,
                ..
            } if request_seq == request.seq
                && error.as_deref() == Some("frontend execution failed")
        ));
        let failed = &store.snapshot("shared").unwrap().runs[0];
        assert_eq!(
            failed.status,
            crate::brain::shared::BrainRunStatus::Failed
        );
        assert_eq!(failed.detail.as_deref(), Some("frontend execution failed"));
    }
}
