# finch-generation — public interface

Generated from [`crates/finch-generation/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-generation/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Subscription allowance snapshot.
pub struct Allowance { … }
/// Kind of generation backend.
pub enum BackendKind { Cloud, Local, Test }
/// One provider/model pair as named by a caller, router, or completed run.
pub struct BackendRef { … }
impl BackendRef {
    /// Construct and validate a backend reference.
    pub fn new(provider: impl Into<String>, model: impl Into<String>, kind: BackendKind) -> Result<Self>;
}
/// Content block - supports text, image, tool_use, and tool_result Re-exported from `finch-providers`.
pub enum ContentBlock { Text, Image, ToolUse, ToolResult, OpaqueReasoning }
impl ContentBlock {
    /// Extract text from text block
    pub fn as_text(&self) -> Option<&str>;
    /// Create a base64 image content block
    pub fn image(media_type: impl Into<String>, base64_data: impl Into<String>) -> Self;
    /// Check if this is a text block
    pub fn is_text(&self) -> bool;
    /// Check if this is a tool use block
    pub fn is_tool_use(&self) -> bool;
    /// Create an opaque provider continuation block.
    pub fn opaque_reasoning(encrypted_content: impl Into<String>) -> Self;
    /// Create a text content block
    pub fn text(text: impl Into<String>) -> Self;
    /// Create a tool result content block
    pub fn tool_result(tool_use_id: String, content: String, is_error: Option<bool>) -> Self;
}
/// Sleeper that parks until [`ControllableSleeper::release`].
pub struct ControllableSleeper { … }
impl ControllableSleeper {
    /// Construct a parked sleeper.
    pub fn new() -> Self;
    /// Unblock one waiter.
    pub fn release(&self);
}
/// Empty cache.
pub struct EmptyCache;
/// Provider/model/event provenance retained with a stream event. Re-exported from `finch-providers`.
pub struct EventProvenance { … }
/// Deterministic clock tests can advance.
pub struct FrozenMonotonicClock { … }
impl FrozenMonotonicClock {
    /// Advance the clock by `delta_ms`.
    pub fn advance(&self, delta_ms: u64);
    /// Construct at `now_ms`.
    pub fn new(now_ms: u64) -> Self;
}
/// Features a backend is willing to claim.
pub struct GenerationCapabilities { … }
impl GenerationCapabilities {
    /// Construct capabilities for a scripted or unknown backend.
    pub fn for_strategy(strategy: GenerationStrategy) -> Self;
}
/// Normalized generation stream.
pub enum GenerationEvent { Started, Readiness, Loading, TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, Usage, Allowance, Identity, Route, Terminal }
impl GenerationEvent {
    /// True for the unique terminal event.
    pub fn is_terminal(&self) -> bool;
}
/// Stable id for one generate attempt.
pub struct GenerationId(Uuid);
impl GenerationId {
    /// Underlying UUID.
    pub fn as_uuid(self) -> Uuid;
    /// Mint a new attempt id.
    pub fn new() -> Self;
}
/// Requested versus resolved versus actual backend for one generation.
pub struct GenerationIdentity { … }
impl GenerationIdentity {
    /// Pin requested = resolved = actual at the start of an attempt.
    pub fn pinned(backend: BackendRef) -> Self;
    /// Record a serving-model correction without changing requested/resolved.
    pub fn with_actual_model(mut self, model: impl Into<String>) -> Result<Self>;
}
/// Secret-free timing and resource metadata for a completed attempt.
pub struct GenerationMetadata { … }
/// Injected environmental ports for generation.
pub struct GenerationPorts { … }
impl GenerationPorts {
    /// Production ports: wall monotonic clock, tokio sleep, tracing sinks.
    pub fn production() -> Self;
    /// Deterministic test ports with a frozen clock and instant sleeper.
    pub fn test() -> Self;
}
/// One generation attempt as seen by a backend.
pub struct GenerationRequest { … }
impl GenerationRequest {
    /// Construct a request with no tools, no timeout, and no fallback.
    pub fn new(messages: Vec<Message>, requested: BackendRef, strategy: GenerationStrategy) -> Self;
    /// Attach a budget.
    pub fn with_budget(mut self, budget: ResourceBudget) -> Self;
    /// Replace the cancellation token.
    pub fn with_cancellation(mut self, cancellation: CancellationToken) -> Self;
    /// Allow recorded fallback to another named candidate.
    pub fn with_fallback(mut self) -> Self;
    /// Attach tool schemas.
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self;
}
/// How a backend produces tokens over shared predictive state.
pub enum GenerationStrategy { CausalAutoregressive, MaskedRefinement, DirectPrediction, Hybrid }
/// Drives one generation attempt and drops events from superseded attempts.
pub struct GenerationSupervisor { … }
impl GenerationSupervisor {
    /// Run `backend` for `request`.
    pub async fn run(&self, backend: Arc<dyn GenerationBackend>, request: GenerationRequest, route: Option<RouteDecision>) -> Result<Receiver<Result<GenerationEvent>>>;
    /// Supersede the in-flight attempt and start `backend`.
    pub async fn switch(&self, backend: Arc<dyn GenerationBackend>, request: GenerationRequest, route: RouteDecision) -> Result<Receiver<Result<GenerationEvent>>>;
    /// Construct a supervisor with injected ports.
    pub fn new(ports: GenerationPorts) -> Self;
}
/// Hardware snapshot.
pub struct HardwareSnapshot { … }
/// Scheduler that runs work immediately.
pub struct InlineScheduler;
/// Sleeper that never waits.
pub struct InstantSleeper;
/// Explicit phase of a model load.
pub enum LoadPhase { DiscoveringHardware, Downloading, Caching, LoadingWeights, Warming, Ready }
/// Re-exported from `finch-providers`.
pub struct Message { … }
impl Message {
    /// Add a content block to this message
    pub fn add_content(mut self, block: ContentBlock) -> Self;
    /// Add tool result to this message
    pub fn add_tool_result(mut self, tool_use_id: String, result: String, is_error: bool) -> Self;
    /// Create an assistant message with text content
    pub fn assistant(content: impl Into<String>) -> Self;
    /// Check if message contains tool results
    pub fn has_tool_results(&self) -> bool;
    /// Check if message has no text content
    pub fn is_empty_text(&self) -> bool;
    /// Extract text from the message
    pub fn text(&self) -> String;
    /// Extract text content from this message
    pub fn text_content(&self) -> String;
    /// Create a user message with text content
    pub fn user(content: impl Into<String>) -> Self;
    /// Create a message with rich content blocks
    pub fn with_content(role: impl Into<String>, content: Vec<ContentBlock>) -> Self;
}
/// Generation backend over a provider transport.
pub struct ProviderGenerationBackend { … }
impl ProviderGenerationBackend {
    /// Wrap a provider using its default model.
    pub fn new(provider: Arc<dyn LlmProvider>) -> Result<Self>;
}
/// Whether a backend can accept a generate call.
pub enum Readiness { NotLoaded, Loading, Ready, Failed }
/// Readiness plus the load story that produced it.
pub struct ReadinessReport { … }
impl ReadinessReport {
    /// Construct a failed report.
    pub fn failed(cause: impl Into<String>, elapsed: Duration, resources: ResourceMetadata) -> Self;
    /// Construct a loading report for `phase`.
    pub fn loading(phase: LoadPhase, elapsed: Duration, resources: ResourceMetadata) -> Self;
    /// Construct a not-loaded report.
    pub fn not_loaded() -> Self;
    /// Construct a ready report with no load failure.
    pub fn ready(elapsed: Duration, resources: ResourceMetadata) -> Self;
}
/// Loader that reports ready without claiming a real model is loaded.
pub struct ReadyLoader;
/// How thinking/reasoning text should be labelled by a UI.
pub enum ReasoningKind { Summary, RawText, Opaque }
/// Why a candidate was not selected.
pub struct RejectedBackend { … }
/// Matched information and resource budget for comparing backends.
pub struct ResourceBudget { … }
impl ResourceBudget {
    /// True when two backends are being compared at the same budget.
    pub fn matches(&self, other: &Self) -> bool;
    /// Construct an unbounded budget.
    pub fn unlimited() -> Self;
}
/// Secret-free resource snapshot for a load or generate attempt.
pub struct ResourceMetadata { … }
/// Recorded routing decision.
pub struct RouteDecision { … }
/// Scripted backend used by production-boundary tests.
pub struct ScriptedBackend { … }
impl ScriptedBackend {
    /// Construct a ready backend that plays `script`.
    pub fn ready(identity: BackendRef, strategy: GenerationStrategy, script: Vec<ScriptedStep>) -> Self;
    /// Advance a loading backend through `phase`.
    pub fn set_phase(&self, phase: LoadPhase, elapsed_ms: u64);
    /// Replace the readiness report (background-load tests).
    pub fn set_readiness(&self, report: ReadinessReport);
    /// Construct a backend in `report` that plays `script` once ready.
    pub fn with_readiness(identity: BackendRef, strategy: GenerationStrategy, report: ReadinessReport, script: Vec<ScriptedStep>) -> Self;
}
/// One step in a scripted generation.
pub enum ScriptedStep { Event, Wait, Fail }
/// Streaming chunk (text delta, reasoning, tool call, or complete block). Re-exported from `finch-providers`.
pub enum StreamChunk { TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Production clock based on [`Instant`].
pub struct SystemMonotonicClock { … }
/// Exactly-once terminal state for an attempt.
pub enum TerminalOutcome { Completed, Cancelled, TimedOut, Disconnected, Failed }
impl TerminalOutcome {
    /// Identity recorded on this terminal.
    pub fn identity(&self) -> &GenerationIdentity;
}
/// Tokio sleeper.
pub struct TokioSleeper;
/// Validated semantic tool call.
pub struct ToolCall { … }
/// Tool definition (Claude API-compatible) Re-exported from `finch-providers`.
pub struct ToolDefinition { … }
/// Tool result the event loop feeds back into the next generation turn.
pub struct ToolResult { … }
impl ToolResult {
    /// Failed result.
    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self;
    /// Successful result.
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self;
}
/// Tool use request after adapter-level validation. Re-exported from `finch-providers`.
pub struct ToolUse { … }
impl ToolUse {
    /// Generate unique tool use ID
    pub fn generate_id() -> String;
    pub fn new(name: String, input: Value) -> Self;
    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock;
}
/// Tracing progress sink.
pub struct TracingProgress;
/// Tracing telemetry.
pub struct TracingTelemetry;
/// Unknown-hardware default.
pub struct UnknownHardware;
/// Token accounting for one attempt.
pub struct Usage { … }
```

## Traits

```rust
/// Artifact cache port.
pub trait ArtifactCache: Send + Sync {
    fn has(&self, key: &str) -> bool;
}
/// Blocking work scheduler.
pub trait BlockingScheduler: Send + Sync {
    fn spawn_blocking(&self, work: Box<dyn FnOnce() + Send>);
}
/// Shared generation contract for local, cloud, and test backends.
pub trait GenerationBackend: Send + Sync {
    fn identity(&self) -> BackendRef;
    fn capabilities(&self) -> GenerationCapabilities;
    fn strategy(&self) -> GenerationStrategy;
    fn readiness(&self) -> ReadinessReport;
}
/// Secret-free telemetry.
pub trait GenerationTelemetry: Send + Sync {
    fn event(&self, name: &str, fields: &[(&str, &str)]);
}
/// Hardware discovery port.
pub trait HardwareDiscovery: Send + Sync {
    fn snapshot(&self) -> HardwareSnapshot;
}
/// Model-load phase port.
pub trait ModelLoader: Send + Sync {
    fn phase(&self) -> LoadPhase;
}
/// Monotonic clock used for elapsed-time metadata.
pub trait MonotonicClock: Send + Sync {
    fn now_ms(&self) -> u64;
}
/// Secret-free progress sink for model loading.
pub trait ProgressSink: Send + Sync {
    fn report(&self, phase: LoadPhase, elapsed_ms: u64);
}
/// Sleeper used for timeouts.
pub trait Sleeper: Send + Sync { … }
```

## Functions

```rust
/// Select a backend for `request` among `candidates`.
pub fn select_backend(request: &GenerationRequest, candidates: &[Arc<dyn GenerationBackend>]) -> Result<(Arc<dyn GenerationBackend>, RouteDecision)> { … }
/// Translate one provider stream chunk into a generation event.
pub fn translate_provider_chunk(chunk: StreamChunk, identity: &GenerationIdentity, sequence: u64) -> Option<GenerationEvent> { … }
/// Reject empty, oversized, non-graphic, or non-ASCII identity strings without echoing the value into the error.
pub fn validate_model_id(value: &str) -> Result<()> { … }
```
