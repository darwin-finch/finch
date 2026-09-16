# generators — public interface

Generated from [`src/generators/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/generators/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Subscription allowance snapshot. Re-exported from `finch-generation`.
pub struct Allowance { … }
/// Kind of generation backend. Re-exported from `finch-generation`.
pub enum BackendKind { Cloud, Local, Test }
/// One provider/model pair as named by a caller, router, or completed run. Re-exported from `finch-generation`.
pub struct BackendRef { … }
/// Claude API generator implementation
pub struct ClaudeGenerator { … }
impl ClaudeGenerator {
    /// The instruction files found at construction and what happened to each.
    pub fn instruction_sources(&self) -> &InstructionSources;
    pub fn new(client: Arc<ClaudeClient>) -> Self;
    /// Build a generator whose instructions are collected from `cwd`, reading user-level files under `home`.
    pub fn new_in(client: Arc<ClaudeClient>, cwd: Option<PathBuf>, home: Option<PathBuf>) -> Self;
}
/// Presents the daemon-owned local model through the same interface used by cloud providers.
pub struct DaemonLocalGenerator { … }
impl DaemonLocalGenerator {
    pub fn new(client: Arc<DaemonClient>, profile_name: impl Into<String>) -> Self;
}
/// Normalized generation stream. Re-exported from `finch-generation`.
pub enum GenerationEvent { Started, Readiness, Loading, TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, Usage, Allowance, Identity, Route, Terminal }
/// Stable id for one generate attempt. Re-exported from `finch-generation`.
pub struct GenerationId(Uuid);
/// Requested versus resolved versus actual backend for one generation. Re-exported from `finch-generation`.
pub struct GenerationIdentity { … }
/// Injected environmental ports for generation. Re-exported from `finch-generation`.
pub struct GenerationPorts { … }
/// One generation attempt as seen by a backend. Re-exported from `finch-generation`.
pub struct GenerationRequest { … }
/// How a backend produces tokens over shared predictive state. Re-exported from `finch-generation`.
pub enum GenerationStrategy { CausalAutoregressive, MaskedRefinement, DirectPrediction, Hybrid }
/// Drives one generation attempt and drops events from superseded attempts. Re-exported from `finch-generation`.
pub struct GenerationSupervisor { … }
/// Tool result the event loop feeds back into the next generation turn. Re-exported from `finch-generation`. Exported as `GenerationToolResult`.
pub struct ToolResult { … }
/// Generator capabilities (what features are supported)
pub struct GeneratorCapabilities { … }
/// Unified response format
pub struct GeneratorResponse { … }
/// Explicit phase of a model load. Re-exported from `finch-generation`.
pub enum LoadPhase { DiscoveringHardware, Downloading, Caching, LoadingWeights, Warming, Ready }
/// Associates a configured profile name with a generator without changing its provider-specific response metadata.
pub struct ProfiledGenerator { … }
impl ProfiledGenerator {
    pub fn new(profile_name: impl Into<String>, inner: std::sync::Arc<dyn Generator>) -> Self;
}
/// Generation backend over a provider transport. Re-exported from `finch-generation`.
pub struct ProviderGenerationBackend { … }
/// Qwen local generator implementation.
pub struct QwenGenerator { … }
impl QwenGenerator {
    pub fn new(local_generator: Arc<RwLock<LocalGenerator>>) -> Self;
}
/// Whether a backend can accept a generate call. Re-exported from `finch-generation`.
pub enum Readiness { NotLoaded, Loading, Ready, Failed }
/// Readiness plus the load story that produced it. Re-exported from `finch-generation`.
pub struct ReadinessReport { … }
/// How thinking/reasoning text should be labelled by a UI. Re-exported from `finch-generation`.
pub enum ReasoningKind { Summary, RawText, Opaque }
/// Matched information and resource budget for comparing backends. Re-exported from `finch-generation`.
pub struct ResourceBudget { … }
pub struct ResponseMetadata { … }
/// Recorded routing decision. Re-exported from `finch-generation`.
pub struct RouteDecision { … }
/// Scripted backend used by production-boundary tests. Re-exported from `finch-generation`.
pub struct ScriptedBackend { … }
/// Streaming chunk (text delta, reasoning, tool call, or complete block). Re-exported from `finch-providers`.
pub enum StreamChunk { TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Exactly-once terminal state for an attempt. Re-exported from `finch-generation`.
pub enum TerminalOutcome { Completed, Cancelled, TimedOut, Disconnected, Failed }
/// Validated semantic tool call. Re-exported from `finch-generation`.
pub struct ToolCall { … }
/// Tool use request after adapter-level validation. Re-exported from `finch-providers`.
pub struct ToolUse { … }
```

## Traits

```rust
/// Shared generation contract for local, cloud, and test backends. Re-exported from `finch-generation`.
pub trait GenerationBackend: Send + Sync {
    fn identity(&self) -> BackendRef;
    fn capabilities(&self) -> GenerationCapabilities;
    fn strategy(&self) -> GenerationStrategy;
    fn readiness(&self) -> ReadinessReport;
    async fn generate(&self, request: GenerationRequest) -> Result<Receiver<Result<GenerationEvent>>>;
}
/// Unified generator interface for Claude, Qwen, and future generators
pub trait Generator: Send + Sync {
    async fn generate(&self, messages: Vec<Message>, tools: Option<Vec<ToolDefinition>>) -> Result<GeneratorResponse>;
    async fn generate_stream(&self, messages: Vec<Message>, tools: Option<Vec<ToolDefinition>>) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>>;
    async fn generate_stream_cancellable(&self, messages: Vec<Message>, tools: Option<Vec<ToolDefinition>>, _cancellation_token: tokio_util::sync::CancellationToken) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>>;
    fn capabilities(&self) -> &GeneratorCapabilities;
    fn name(&self) -> &str;
    fn model_name(&self) -> &str;
}
```

## Functions

```rust
/// Translate one provider stream chunk into a generation event. Re-exported from `finch-generation`.
pub fn translate_provider_chunk(chunk: StreamChunk, identity: &GenerationIdentity, sequence: u64) -> Option<GenerationEvent> { … }
pub(crate) fn validate_response_model(model: &str) -> Result<()> { … }
```

## Constants

```rust
pub const CODING_SYSTEM_PROMPT: &str = "You are the software-engineering reasoning provider inside \ Finch. You are not the Finch application or terminal UI, and you do not impersonate either one. \ Use the host tools Finch exposes to inspect and modify the user's codebase autonomously, like a \ senior engineer pairing at the terminal. A transport-specific execution/output contract may follow \ this coding policy;
pub(crate) const MAX_RESPONSE_MODEL_BYTES: usize = 256;
```
