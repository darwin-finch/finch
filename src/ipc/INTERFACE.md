# ipc — public interface

Generated from [`src/ipc/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/ipc/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Re-exported from `ipc::codec`.
pub(crate) struct BrainRemoteCommand { … }
/// Re-exported from `ipc::codec`.
pub(crate) enum BrainRemoteCommandKind { Submit, Acknowledge, Detach, RequestRunnerHandoff, CancelRunnerHandoff, CancelRun, CreateSchedule, CancelSchedule, ScheduleInitialization }
/// Re-exported from `ipc::codec`.
pub(crate) enum BrainRemoteEnvelope { Projection, Command, Reply }
/// Re-exported from `ipc::codec`.
pub(crate) struct BrainRemoteMutation { … }
/// Re-exported from `ipc::codec`.
pub(crate) enum BrainRemoteReply { Submitted, Acknowledged, Detached, HandoffRequested, HandoffCancelled, RunCancelled, ScheduleCreated, ScheduleCancelled, InitializationScheduled, Error }
/// Named async event bus with continuation support.
pub struct EventBus { … }
impl EventBus {
    /// Process all currently queued events (non-blocking once the queue is empty).
    pub async fn flush(&mut self);
    /// Drain the queue until it is empty or the channel is closed.
    pub async fn run(&mut self);
    pub fn new() -> Self;
    /// Register an async handler for events with the given name.
    pub fn register<F, Fut>(&mut self, name: impl Into<String>, handler: F) where F: Fn(QueuedEvent) -> Fut + Send + Sync + 'static, Fut: Future<Output = Option<QueuedEvent>> + Send + 'static,;
    /// Enqueue an event.
    pub fn send(&self, event: QueuedEvent);
    /// Return a sender that can enqueue events from other tasks.
    pub fn sender(&self) -> mpsc::UnboundedSender<QueuedEvent>;
}
/// Async client for the daemon IPC socket.
pub struct IpcClient { … }
impl IpcClient {
    pub async fn brain_accept_runner_handoff(&self, brain: &str, target_subject: &str, handoff_id: crate::brain::RunnerHandoffId, environment: &crate::brain::BrainEnvironment, ttl_ms: u64) -> Result<crate::brain::BrainRunnerLease>;
    pub async fn brain_acknowledge(&self, brain: &str, attachment: &crate::brain::BrainAttachment, seq: u64) -> Result<crate::brain::BrainAttachment>;
    pub async fn brain_acknowledge_effect_delivery(&self, brain: &str, client_id: uuid::Uuid, cursor: crate::runtime::DeliveryCursor) -> Result<bool>;
    pub async fn brain_acquire_runner(&self, brain: &str, subject: &str, environment: &crate::brain::BrainEnvironment, lease_id: Option<crate::brain::RunnerLeaseId>, ttl_ms: u64) -> Result<crate::brain::BrainRunnerLease>;
    pub async fn brain_attach(&self, brain: &str, subject: &str, role: crate::brain::AttachmentRole, attachment_id: Option<crate::brain::AttachmentId>) -> Result<crate::brain::BrainAttachment>;
    pub async fn brain_cancel_run(&self, brain: &str, attachment: &crate::brain::BrainAttachment, run_id: crate::brain::RunId) -> Result<crate::brain::BrainRun>;
    pub async fn brain_cancel_runner_handoff(&self, brain: &str, handoff_id: crate::brain::RunnerHandoffId, sender: &str) -> Result<()>;
    pub async fn brain_cancel_schedule(&self, brain: &str, attachment: &crate::brain::BrainAttachment, schedule_id: crate::brain::ScheduleId) -> Result<bool>;
    pub async fn brain_claim_runner_identity(&self, subject: &str) -> Result<()>;
    pub async fn brain_create_schedule(&self, brain: &str, attachment: &crate::brain::BrainAttachment, language: crate::brain::ProgramLanguage, source: &str, grant_ceiling: &crate::vm::EffectSet, next_due_ms: u64, interval_ms: Option<u64>, delivery_policy: &crate::brain::BrainScheduleDeliveryPolicy) -> Result<crate::brain::BrainSchedule>;
    pub async fn brain_detach(&self, brain: &str, attachment: &crate::brain::BrainAttachment) -> Result<()>;
    pub async fn brain_inspect_run(&self, brain: &str, run_id: crate::brain::RunId) -> Result<crate::brain::BrainRun>;
    pub async fn brain_inspect_schedule(&self, brain: &str, schedule_id: crate::brain::ScheduleId) -> Result<Option<crate::brain::BrainSchedule>>;
    pub async fn brain_pending_effect_delivery(&self, brain: &str, client_id: uuid::Uuid) -> Result<Vec<crate::runtime::RuntimeApplicationMessage>>;
    pub async fn brain_release_runner(&self, brain: &str, lease_id: crate::brain::RunnerLeaseId) -> Result<()>;
    pub async fn brain_request_runner_handoff(&self, brain: &str, requested_by: &str, target_subject: &str, expected_lease_id: crate::brain::RunnerLeaseId, environment: &crate::brain::BrainEnvironment, ttl_ms: u64) -> Result<crate::brain::BrainRunnerHandoff>;
    pub async fn brain_schedule_initialization(&self, brain: &str, attachment: &crate::brain::BrainAttachment, next_due_ms: u64) -> Result<crate::brain::BrainSchedule>;
    pub async fn brain_snapshot(&self, brain: &str) -> Result<crate::brain::BrainSnapshot>;
    pub async fn brain_start_speculative(&self, brain: &str, attachment: &crate::brain::BrainAttachment, prompt: String) -> Result<crate::brain::BrainRun>;
    pub async fn brain_submit(&self, brain: &str, attachment: &crate::brain::BrainAttachment, kind: crate::brain::BrainEventKind) -> Result<BrainSubmissionResult>;
    pub async fn brain_watch(&self, brain: &str, attachment: &crate::brain::BrainAttachment) -> Result<mpsc::UnboundedReceiver<Result<crate::brain::BrainWireMessage>>>;
    /// Connect to the daemon's Unix socket.
    pub async fn connect() -> Result<Self>;
    /// Send a Forth program to the daemon; get back the full data stack + output.
    pub async fn eval_forth(&self, program: &str) -> Result<(Vec<i64>, String)>;
    pub async fn ping(&self) -> Result<String>;
    /// Non-streaming query — returns the full response.
    pub async fn query(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> Result<QueryResponse>;
    /// Streaming query — returns a channel of `StreamChunk`s.
    pub async fn query_stream(&self, messages: Vec<Message>, tools: Vec<ToolDefinition>) -> Result<mpsc::UnboundedReceiver<Result<StreamChunk>>>;
    /// Register this frontend as the callback for its current named-Brain runner lease.
    pub async fn register_brain_runner(&self, brain: &str, lease_id: crate::brain::RunnerLeaseId, event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>) -> Result<BrainRunnerBootstrap>;
}
/// A single event on the bus.
pub struct QueuedEvent { … }
impl QueuedEvent {
    /// Produce a continuation event: same `id`, new `name` and `payload`.
    pub fn continue_as(&self, name: impl Into<String>, payload: serde_json::Value) -> Self;
    pub fn new(name: impl Into<String>, payload: serde_json::Value) -> Self;
}
```

## Functions

```rust
/// Re-exported from `ipc::codec`.
pub(crate) fn brain_remote_command_fingerprint(kind: &BrainRemoteCommandKind) -> anyhow::Result<String> { … }
/// Re-exported from `ipc::codec`.
pub(crate) fn decode_brain_remote_envelope(bytes: &[u8]) -> anyhow::Result<BrainRemoteEnvelope> { … }
/// Decode one durable typed-runtime checkpoint. Re-exported from `ipc::codec`.
pub(crate) fn decode_checkpoint_bytes(encoded: &[u8]) -> Result<TypedRuntimeCheckpoint> { … }
/// Re-exported from `ipc::codec`.
pub(crate) fn encode_brain_remote_envelope(envelope: &BrainRemoteEnvelope) -> anyhow::Result<Vec<u8>> { … }
/// Encode one durable typed-runtime checkpoint using the same closed native schema used by runner registration and result transport. Re-exported from `ipc::codec`.
pub(crate) fn encode_checkpoint_bytes(value: &TypedRuntimeCheckpoint) -> Result<Vec<u8>> { … }
/// Compact packed Cap'n Proto frame for the Runtime/Application ABI. Re-exported from `ipc::codec`.
pub(crate) fn encode_runtime_application_message_packed(value: &RuntimeApplicationMessage) -> Result<Vec<u8>> { … }
/// Bind the Unix socket and accept Cap'n Proto connections in a `LocalSet`.
pub async fn start_ipc_server(server: Arc<AgentServer>, shutdown: tokio_util::sync::CancellationToken) -> Result<()> { … }
```

## Constants

```rust
/// Default path for the IPC Unix domain socket.
pub const DAEMON_SOCK_PATH: &str = "~/.finch/daemon.sock";
/// Compatibility generation for the frontend/daemon Cap'n Proto contract.
pub const IPC_PROTOCOL_VERSION: u32 = 9;
```

## Modules

```rust
pub mod client;
pub mod events;
pub mod schema;
pub mod server;
pub mod transport;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `BrainRunnerBootstrap`, `BrainSubmissionResult`, `QueryResponse`
