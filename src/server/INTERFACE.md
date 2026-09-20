# server — public interface

Generated from [`src/server/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/server/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Main agent server structure
pub struct AgentServer { … }
impl AgentServer {
    pub async fn brain_password(&self) -> String;
    pub async fn check_brain_password(&self, candidate: &str) -> bool;
    /// Return the daemon-owned MCP transport, connecting it on first use.
    pub async fn mcp_client(&self) -> Result<Option<Arc<crate::tools::McpClient>>>;
    /// Start the HTTP server.
    pub async fn serve(self: Arc<Self>) -> Result<()>;
    pub async fn set_brain_password(&self, password: String);
    /// Get reference to bootstrap loader
    pub fn bootstrap_loader(&self) -> &Arc<BootstrapLoader>;
    pub fn brain_approvals(&self) -> &BrainApprovalBroker;
    pub fn brain_credentials(&self) -> &crate::brain::BrainCredentialAuthority;
    pub fn brain_runners(&self) -> &BrainRunnerBroker;
    pub fn brain_store(&self) -> &crate::brain::BrainStore;
    /// Get reference to Claude client
    pub fn claude_client(&self) -> &Arc<ClaudeClient>;
    /// Get server configuration
    pub fn config(&self) -> &ServerConfig;
    /// Get the append-only explicit feedback store.
    pub fn feedback_store(&self) -> &Arc<FeedbackLogger>;
    /// Get reference to generator state
    pub fn generator_state(&self) -> &Arc<RwLock<GeneratorState>>;
    /// Whether named cloud provider profiles are configured.
    pub fn has_provider_profiles(&self) -> bool;
    /// Get reference to local generator
    pub fn local_generator(&self) -> &Arc<RwLock<LocalGenerator>>;
    /// Get reference to metrics logger
    pub fn metrics_logger(&self) -> &Arc<MetricsLogger>;
    /// Create a new agent server.
    pub fn new(config: Config, mut server_config: ServerConfig, claude_client: ClaudeClient, router: Router, metrics_logger: MetricsLogger, local_generator: Arc<RwLock<LocalGenerator>>, bootstrap_loader: Arc<BootstrapLoader>, generator_state: Arc<RwLock<GeneratorState>>, provider_graph: ProviderGraph) -> Result<Self>;
    /// Return the primary cloud provider (first in the configured list, if any).
    pub fn primary_provider(&self) -> Option<Arc<dyn crate::providers::LlmProvider>>;
    /// Resolve the cloud provider to use for a given request.
    pub fn provider_for_name(&self, name: Option<&str>) -> Option<&Arc<dyn LlmProvider>>;
    /// Get reference to router
    pub fn router(&self) -> &Arc<RwLock<Router>>;
    /// How long this server has been running.
    pub fn uptime(&self) -> std::time::Duration;
}
pub struct BrainApprovalBroker { … }
impl BrainApprovalBroker {
    pub fn cancel_attachment(&self, brain_id: BrainId, attachment_id: AttachmentId) -> usize;
    pub fn cancel_connection(&self, brain_id: BrainId, attachment_id: AttachmentId, connection_id: ConnectionId) -> usize;
    pub fn claim(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId) -> Result<ClaimedApproval>;
    pub fn claim_connection(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<ClaimedApproval>;
    pub fn deliver(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId, decision: serde_json::Value) -> Result<()>;
    pub fn deliver_connection(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId, connection_id: ConnectionId, decision: serde_json::Value) -> Result<()>;
    pub fn inspect(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId) -> Result<BrainApprovalAudience>;
    pub fn inspect_connection(&self, brain_id: BrainId, request_seq: u64, approval_id: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainApprovalAudience>;
    /// Serialize durable decisions for one approval without taking the Brain's turn lane.
    pub fn mutation_lock(&self, brain_id: BrainId, request_seq: u64, approval_id: &str) -> Arc<AsyncMutex<()>>;
    pub fn register(&self, request_seq: u64, approval_id: impl Into<String>, audience: BrainApprovalAudience) -> Result<ApprovalRegistration>;
    pub fn register_for_connection(&self, request_seq: u64, approval_id: impl Into<String>, audience: BrainApprovalAudience, connection_id: ConnectionId) -> Result<ApprovalRegistration>;
}
/// The canonical in-process named-Brain lifecycle boundary.
pub struct BrainLifecycleService { … }
impl BrainLifecycleService {
    pub async fn cancel_run(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, run_id: RunId) -> Result<BrainRun>;
    pub async fn cancel_run_with_receipt(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, run_id: RunId, receipt: Option<crate::brain::BrainMutationReceipt>) -> Result<BrainRun>;
    /// Create one empty Brain in this daemon's indivisible environment.
    pub async fn create(&self, brain: &str) -> Result<BrainSnapshot>;
    pub async fn replay_committed_memory(&self, brain: String, lease_id: RunnerLeaseId) -> Result<usize>;
    pub async fn resume_queued_runs(&self, brain: String, lease_id: RunnerLeaseId) -> Result<usize>;
    /// Explicitly start one cancellable speculative helper through the same authoritative submission and runner path as every other Brain turn.
    pub async fn start_speculative(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, prompt: String) -> Result<BrainSubmissionOutcome, BrainSubmissionError>;
    pub async fn submit(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, kind: BrainEventKind) -> Result<BrainSubmissionOutcome, BrainSubmissionError>;
    pub fn accept_runner_handoff(&self, brain: &str, target_subject: &str, handoff_id: RunnerHandoffId, environment: &BrainEnvironment, ttl_ms: u64) -> Result<BrainRunnerLease>;
    pub fn acknowledge(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, seq: u64) -> Result<BrainAttachment>;
    pub fn acknowledge_effect_delivery(&self, brain: &str, client_id: uuid::Uuid, cursor: crate::runtime::DeliveryCursor) -> Result<bool>;
    pub fn acquire_runner(&self, brain: &str, subject: &str, environment: &BrainEnvironment, lease_id: Option<RunnerLeaseId>, ttl_ms: u64) -> Result<BrainRunnerLease>;
    pub fn attach(&self, brain: &str, subject: &str, role: AttachmentRole, attachment_id: Option<AttachmentId>) -> Result<BrainAttachment>;
    pub fn cancel_runner_handoff(&self, brain: &str, handoff_id: RunnerHandoffId, sender: &str) -> Result<()>;
    pub fn cancel_runner_handoff_with_receipt(&self, brain: &str, handoff_id: RunnerHandoffId, sender: &str, receipt: Option<crate::brain::BrainMutationReceipt>) -> Result<()>;
    pub fn cancel_schedule(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, schedule_id: ScheduleId) -> Result<bool>;
    pub fn cancel_schedule_for_run(&self, brain: &str, run_id: RunId, request_seq: u64, schedule_id: ScheduleId) -> Result<bool>;
    pub fn cancel_schedule_with_receipt(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, schedule_id: ScheduleId, receipt: Option<crate::brain::BrainMutationReceipt>) -> Result<bool>;
    pub fn connection(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainAttachment>;
    pub fn create_schedule(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, language: ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: BrainScheduleDeliveryPolicy) -> Result<BrainSchedule>;
    pub fn create_schedule_for_run(&self, brain: &str, run_id: RunId, request_seq: u64, maximum_grant_ceiling: Option<&crate::vm::EffectSet>, language: ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: BrainScheduleDeliveryPolicy) -> Result<BrainSchedule>;
    pub fn create_schedule_with_receipt(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, language: ProgramLanguage, source: String, grant_ceiling: crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: BrainScheduleDeliveryPolicy, mutation: Option<crate::brain::BrainMutationReceipt>) -> Result<BrainSchedule>;
    pub fn detach(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<()>;
    pub fn from_server(server: &AgentServer) -> Self;
    pub fn initialization(&self, brain: &str) -> Result<crate::brain::BrainInitialization>;
    pub fn inspect_run(&self, brain: &str, run_id: RunId) -> Result<BrainRun>;
    pub fn inspect_schedule(&self, brain: &str, schedule_id: ScheduleId) -> Result<Option<BrainSchedule>>;
    pub fn inspect_schedule_for_run(&self, brain: &str, run_id: RunId, request_seq: u64, schedule_id: ScheduleId) -> Result<Option<BrainSchedule>>;
    pub fn list(&self) -> Result<Vec<String>>;
    /// Resolve an exact, not-yet-activated attachment reservation.
    pub fn pending_attachment(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainAttachment>;
    pub fn pending_effect_delivery(&self, brain: &str, client_id: uuid::Uuid) -> Result<Vec<crate::runtime::RuntimeApplicationMessage>>;
    pub fn release_runner(&self, brain: &str, lease_id: RunnerLeaseId) -> Result<()>;
    pub fn request_runner_handoff(&self, brain: &str, requested_by: &str, target_subject: &str, expected_lease_id: RunnerLeaseId, environment: &BrainEnvironment, ttl_ms: u64) -> Result<BrainRunnerHandoff>;
    pub fn request_runner_handoff_with_receipt(&self, brain: &str, requested_by: &str, target_subject: &str, expected_lease_id: RunnerLeaseId, environment: &BrainEnvironment, ttl_ms: u64, mutation: Option<crate::brain::BrainMutationReceipt>) -> Result<BrainRunnerHandoff>;
    /// Journal the reviewed initialization module as a one-shot schedule.
    pub fn schedule_initialization(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, next_due_ms: u64) -> Result<BrainSchedule>;
    pub fn schedule_initialization_with_receipt(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId, next_due_ms: u64, receipt: Option<crate::brain::BrainMutationReceipt>) -> Result<BrainSchedule>;
    pub fn snapshot(&self, brain: &str) -> Result<BrainSnapshot>;
    pub fn start_run_with_parent(&self, brain: &str, sender: &str, kind: BrainRunKind, request_seq: u64, initiating_attachment_id: AttachmentId, status: BrainRunStatus, parent_run_id: Option<RunId>) -> Result<BrainRun>;
    /// Register a frontend-owned child agent as a canonical run beneath the active parent.
    pub fn start_subagent_for_run(&self, brain: &str, parent_run_id: RunId, task_id: uuid::Uuid, detail: Option<String>) -> Result<BrainRun>;
    pub fn transition_subagent_run(&self, brain: &str, run_id: RunId, status: BrainRunStatus, detail: Option<String>) -> Result<BrainRun>;
    /// Activate and subscribe without a snapshot/event race.
    pub fn watch(&self, brain: &str, attachment_id: AttachmentId, connection_id: ConnectionId) -> Result<BrainWatch>;
}
/// Registrations contain only Tokio channels and portable values.
pub struct BrainRunnerBroker { … }
impl BrainRunnerBroker {
    pub async fn cancel_run(&self, brain: &str, lease_id: RunnerLeaseId, run_id: RunId) -> Result<bool>;
    pub async fn dispatch_program(&self, brain: &str, lease_id: RunnerLeaseId, run_id: RunId, request_seq: u64, language: ProgramLanguage, source: String, interaction: RunnerProgramInteraction, grant_ceiling: Option<crate::vm::EffectSet>) -> Result<RunnerProgramResult>;
    pub async fn dispatch_turn(&self, brain: &str, lease_id: RunnerLeaseId, run_id: RunId, request_seq: u64, prompt: String, context: Vec<crate::providers::Message>, approval_audience: crate::brain::BrainApprovalAudience, approval_connection_id: Option<crate::brain::ConnectionId>) -> Result<RunnerTurnResult>;
    /// Ask the exact leased environment runner to project one already committed Brain turn into its semantic-memory store.
    pub async fn project_memory(&self, brain: &str, lease_id: RunnerLeaseId, brain_id: crate::brain::BrainId, run_id: RunId, request_seq: u64, prompt: String, rendered: String) -> Result<usize>;
    /// `project_memory`, distinguishing a failure that will repeat for every later run from one that is specific to this turn.
    pub async fn try_project_memory(&self, brain: &str, lease_id: RunnerLeaseId, brain_id: crate::brain::BrainId, run_id: RunId, request_seq: u64, prompt: String, rendered: String) -> std::result::Result<usize, RunnerProjectionError>;
    pub fn has_registration(&self, brain: &str, lease_id: RunnerLeaseId) -> bool;
    pub fn register(&self, brain: impl Into<String>, lease_id: RunnerLeaseId, tx: mpsc::UnboundedSender<RunnerRequest>) -> RunnerRegistrationId;
    /// Remove a registration only if it is still the connection that created it.
    pub fn unregister(&self, brain: &str, id: RunnerRegistrationId);
}
pub enum BrainSubmissionError { Invalid, Forbidden, State }
pub struct BrainSubmissionOutcome { … }
/// Snapshot plus the already-subscribed event receiver for a newly activated attachment.
pub struct BrainWatch { … }
/// Request body for /v1/chat/completions endpoint
pub struct ChatCompletionRequest { … }
/// Response body for /v1/chat/completions endpoint
pub struct ChatCompletionResponse { … }
/// Chat message in OpenAI format
pub struct ChatMessage { … }
impl ChatMessage {
    /// Create an assistant message
    pub fn assistant(content: impl Into<String>) -> Self;
    /// Create a new message
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self;
    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self;
    /// Create a user message
    pub fn user(content: impl Into<String>) -> Self;
}
/// Completion choice
pub struct Choice { … }
/// Model-API client authentication shared by every provider profile.
pub struct DaemonAuth { … }
impl DaemonAuth {
    pub fn new(enabled: bool, api_keys: Vec<String>) -> Self;
}
/// Function call details
pub struct FunctionCall { … }
/// Function definition
pub struct FunctionDefinition { … }
/// Model information
pub struct Model { … }
/// Response for /v1/models endpoint
pub struct ModelsResponse { … }
/// Shared rate limiter state — clone freely (it's an Arc inside)
pub struct RateLimiter { … }
impl RateLimiter {
    /// Returns true if the request from `ip` is within rate limits.
    pub fn check(&self, ip: IpAddr) -> bool;
    /// Create a rate limiter.
    pub fn new(requests_per_second: f64, burst: f64) -> Self;
    /// Purge buckets that have been idle for more than `idle_secs`.
    pub fn purge_idle(&self, idle_secs: u64);
    /// Number of currently tracked IPs.
    pub fn tracked_ips(&self) -> usize;
}
pub struct RunnerApprovalRequest { … }
pub struct RunnerCancelRequest { … }
/// Send-safe proxy for the daemon-owned run-scoped effect audit capability. Re-exported from `finch-runtime`.
pub struct RunnerEffectAuditControl { … }
/// Re-exported from `finch-runtime`.
pub(crate) enum RunnerEffectAuditControlRequest { Reserve }
/// One accepted intent. Re-exported from `finch-runtime`.
pub struct RunnerEffectAuditReservation { … }
/// Re-exported from `finch-runtime`.
pub(crate) enum RunnerEffectAuditReservationRequest { Begin, NotApplied }
pub struct RunnerEffectRecord { … }
/// Re-exported from `finch-runtime`.
pub(crate) struct RunnerHostEffectFinishRequest { … }
/// Re-exported from `finch-runtime`.
pub enum RunnerHostEffectOutcome { Acknowledged, NotApplied, FailedPartial }
/// Opaque proof that the daemon fsynced `AwaitingHostResult`. Re-exported from `finch-runtime`.
pub struct RunnerHostEffectPermit { … }
pub struct RunnerMemoryProjectionRequest { … }
pub enum RunnerProgramControlRequest { CreateSchedule, InspectSchedule, CancelSchedule }
pub struct RunnerProgramError { … }
pub enum RunnerProgramInteraction { Interactive, Noninteractive }
pub struct RunnerProgramRequest { … }
pub struct RunnerProgramResult { … }
/// Why one memory projection did not happen.
pub enum RunnerProjectionError { Unavailable, Rejected }
impl RunnerProjectionError {
    /// Flatten back to the untyped error the ordinary callers expect.
    pub fn into_error(self) -> anyhow::Error;
}
pub struct RunnerRegistrationId(uuid::Uuid);
pub enum RunnerRequest { Program, Turn, ProjectMemory, Cancel }
/// Send-safe proxy for a frontend-owned post-commit continuation.
pub struct RunnerTurnCommitAck { … }
impl RunnerTurnCommitAck {
    pub fn acknowledge(&self, status: crate::brain::BrainRunStatus, detail: impl Into<String>) -> Result<(), String>;
    pub fn new(tx: mpsc::UnboundedSender<RunnerTurnCommitNotice>) -> Self;
}
pub struct RunnerTurnCommitNotice { … }
pub struct RunnerTurnError { … }
pub enum RunnerTurnEvent { Call, Result, ApprovalRequested, ApprovalDecided }
pub struct RunnerTurnRequest { … }
pub struct RunnerTurnResult { … }
/// Configuration for the HTTP server
pub struct ServerConfig { … }
/// Tool definition in OpenAI format
pub struct Tool { … }
/// Tool call in OpenAI format
pub struct ToolCall { … }
/// Token usage statistics
pub struct Usage { … }
```

## Functions

```rust
/// Require the shared API key on model endpoints using the standard OpenAI bearer format.
pub async fn auth_middleware(State(auth): State<DaemonAuth>, request: Request<Body>, next: Next) -> Response { … }
pub(crate) fn authorize_pending_remote_attachment(lifecycle: &crate::server::BrainLifecycleService, credentials: &crate::brain::BrainCredentialAuthority, headers: &HeaderMap, name: &str, attachment_id: crate::brain::AttachmentId, connection_id: crate::brain::ConnectionId) -> Result<crate::brain::BrainCredentialClaims, Response> { … }
/// The TLS listener deliberately exposes only the collaboration protocol.
pub fn create_remote_brain_router(server: Arc<AgentServer>) -> Router { … }
/// Create the main application router
pub fn create_router(server: Arc<AgentServer>) -> Router { … }
#[cfg(test)]
pub(crate) fn drop_next_remote_brain_reply_after_commit() { … }
pub(crate) fn execute_authorized_remote_initialization(lifecycle: &crate::server::BrainLifecycleService, claims: &crate::brain::BrainCredentialClaims, name: &str, attachment_id: crate::brain::AttachmentId, connection_id: crate::brain::ConnectionId, request_id: u64, next_due_ms: u64, mutation: Option<crate::brain::BrainMutationReceipt>) -> crate::brain::ipc_codec::BrainRemoteReply { … }
/// Handle POST /v1/chat/completions - OpenAI-compatible chat endpoint
pub async fn handle_chat_completions(State(server): State<Arc<AgentServer>>, Json(request): Json<ChatCompletionRequest>) -> Response { … }
/// Handle POST /v1/feedback - durably retain explicit feedback
pub async fn handle_feedback(State(feedback_store): State<Arc<FeedbackLogger>>, Json(request): Json<FeedbackRequest>) -> Result<Json<FeedbackResponse>, Response> { … }
/// Handle GET /v1/models - List available models
pub async fn handle_list_models(State(server): State<Arc<AgentServer>>) -> Json<ModelsResponse> { … }
/// Handle GET /v1/node/info — return this node's identity and capabilities
pub async fn handle_node_info() -> Result<Json<serde_json::Value>, AppError> { … }
/// Test seam for the production node-info response with explicit state.
#[cfg(unix)]
pub async fn handle_node_info_from_state_directory(state: crate::node::IsolatedNodeTestState, capabilities: crate::node::NodeCapabilities) -> Result<Json<serde_json::Value>, AppError> { … }
/// Handle GET /v1/node/stats — return this node's work statistics
pub async fn handle_node_stats() -> Result<Json<serde_json::Value>, AppError> { … }
/// Test seam for the production node-stats response with explicit state.
#[cfg(unix)]
pub async fn handle_node_stats_from_state_directory(state: crate::node::IsolatedNodeTestState) -> Result<Json<serde_json::Value>, AppError> { … }
/// Handle GET /v1/training/status - Get training queue status
pub async fn handle_training_status() -> Json<TrainingStatusResponse> { … }
/// Handle GET /health - Health check endpoint
pub async fn health_check(State(server): State<Arc<AgentServer>>) -> Result<Json<HealthStatus>, AppError> { … }
/// Handle GET /metrics - Prometheus metrics endpoint
pub async fn metrics_endpoint(State(server): State<Arc<AgentServer>>) -> Result<Response, AppError> { … }
/// Bind the Unix socket and accept Cap'n Proto connections in a `LocalSet`.
pub async fn start_ipc_server(server: Arc<AgentServer>, shutdown: tokio_util::sync::CancellationToken) -> Result<()> { … }
```

## Constants

```rust
/// Marks a runner reply as a condition that will repeat for every later run.
pub const RUNNER_UNAVAILABLE_PREFIX: &str = "runner-unavailable: ";
```

## Modules

```rust
pub(crate) mod ipc;
/// The two timing decisions the schedule delivery loop makes.
pub(crate) mod schedule_delivery { … }
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `AppError`, `ApprovalRegistration`, `ClaimedApproval`, `FeedbackRequest`, `FeedbackResponse`, `HealthStatus`, `TrainingStatusResponse`
