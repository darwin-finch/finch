# providers — public interface

Generated from [`src/providers/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/providers/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Where Finch obtained a capability claim.
pub enum CapabilityProvenance { RuntimeDiscovery, StaticMetadata, Configuration, Unknown }
/// Whether a provider/model capability is known to be usable.
pub enum CapabilitySupport { Supported, Unsupported, Unknown }
pub enum CatalogAuth { AnthropicApiKey, Bearer }
pub enum CatalogSource { Discovered, Cache, StaticFallback }
/// Secret-free stage markers for actionable device-login diagnostics.
pub enum ChatGptAuthStageError { PollContract, TokenExchangeRejected, TokenExchangeContract, IdentityVerification, ClientBinding, AccountEntitlement }
/// Status-only ChatGPT device endpoint failures.
pub enum ChatGptDeviceEndpointError { StartDisabledOrUnsupported, StartRejected, PollRejected }
/// Claude API provider  Implements the LlmProvider trait for Anthropic's Claude API.
pub struct ClaudeProvider { … }
impl ClaudeProvider {
    /// Create a new Claude provider
    pub fn new(api_key: String) -> Result<Self>;
    /// Create a Claude provider with endpoint paths relative to `base_url` or complete endpoint URLs.
    pub fn new_with_endpoints(api_key: String, base_url: &str, chat_path: &str, models_path: &str) -> Result<Self>;
    /// Create with custom default model
    pub fn with_model(mut self, model: impl Into<String>) -> Self;
}
/// Context limits for a model, preserving the unit used by the boundary.
pub struct ContextWindowCapability { … }
impl ContextWindowCapability {
    /// Construct an unknown context window with no assumed default.
    pub fn unknown() -> Self;
}
/// Tracks the state of conversation with teacher
pub struct ConversationState { … }
/// A chain of providers to try in order
pub struct FallbackChain { … }
impl FallbackChain {
    /// Try streaming with automatic fallback  Tries providers in sequence until one succeeds at the send_message_stream level.
    pub async fn send_message_stream_with_fallback(&self, request: &ProviderRequest) -> Result<mpsc::Receiver<Result<StreamChunk>>>;
    /// Try sending message with automatic fallback
    pub async fn send_message_with_fallback(&self, request: &ProviderRequest) -> Result<ProviderResponse>;
    /// Create a fallback chain without reconstructing already validated providers.
    pub fn from_shared(providers: Vec<Arc<dyn LlmProvider>>) -> Self;
    /// Check if the chain is empty
    pub fn is_empty(&self) -> bool;
    /// Get the number of providers in the chain
    pub fn len(&self) -> usize;
    /// Create a new fallback chain with providers in priority order
    pub fn new(providers: Vec<Box<dyn LlmProvider>>) -> Self;
    /// Get the primary provider (first in chain)
    pub fn primary_provider(&self) -> Option<&dyn LlmProvider>;
}
/// Google Gemini API provider  Supports Gemini 2.0 Flash and other Gemini models.
pub struct GeminiProvider { … }
impl GeminiProvider {
    /// Create a new Gemini provider
    pub fn new(api_key: String) -> Result<Self>;
    /// Create with custom default model
    pub fn with_model(mut self, model: impl Into<String>) -> Self;
}
/// Provider-neutral identity and accounting for one completed inference.
pub struct InvocationMetadata { … }
impl InvocationMetadata {
    pub fn validate(&self) -> anyhow::Result<()>;
}
/// Capabilities of one exact provider/model pair.
pub struct ModelCapabilities { … }
impl ModelCapabilities {
    /// Build a descriptor from dated, provider-contract metadata.
    pub fn static_metadata(provider: impl Into<String>, model: impl Into<String>, tested_on: &str, source: &str, streaming: CapabilitySupport, tools: CapabilitySupport, continuation: CapabilitySupport, reasoning: ReasoningCapability, max_tokens: Option<usize>, max_output_tokens: Option<usize>, max_messages: Option<usize>) -> Self;
    /// A fail-closed descriptor for an unrecognized model.
    pub fn unknown(provider: impl Into<String>, model: impl Into<String>) -> Self;
    /// Validate all request-local requirements before provider work begins.
    pub fn validate_request(&self, request: &ProviderRequest, streaming: bool, reasoning: Option<ReasoningEffort>) -> anyhow::Result<()>;
    /// Bind this exact model descriptor to the adapter's configured wire protocol without implying any unverified optional model features.
    pub fn with_wire_protocol(mut self, protocol: WireProtocol, tested_on: &str, source: &str) -> Self;
}
pub struct ModelCatalog { … }
pub struct ModelCatalogProfile { … }
impl ModelCatalogProfile {
    pub fn new(provider: impl Into<String>, profile_id: impl Into<String>, api_key: impl Into<String>, endpoints: ProviderEndpoints, auth: CatalogAuth) -> Self;
}
/// A single yes/no/unknown model feature with its evidence.
pub struct ModelFeature { … }
impl ModelFeature {
    /// Return true only for explicit support, never for unknown status.
    pub fn is_supported(&self) -> bool;
    /// Construct a dated static feature claim.
    pub fn static_metadata(support: CapabilitySupport, tested_on: impl Into<String>, source: impl Into<String>) -> Self;
    /// Construct an unknown feature with no provenance.
    pub fn unknown() -> Self;
}
/// OpenAI API provider  Supports both OpenAI and Grok APIs (they use the same format).
pub struct OpenAIProvider { … }
impl OpenAIProvider {
    /// Create an OpenAI-compatible provider with explicit endpoint paths.
    pub fn new_compatible(api_key: String, base_url: String, chat_path: impl AsRef<str>, models_path: impl AsRef<str>, default_model: String, provider_name: String) -> Result<Self>;
    /// Create a new Grok provider (uses OpenAI-compatible API)
    pub fn new_grok(api_key: String) -> Result<Self>;
    /// Create a new Groq provider (fast inference, uses OpenAI-compatible API) Note: This is Groq (by Groq Inc), not Grok (by X.AI)
    pub fn new_groq(api_key: String) -> Result<Self>;
    /// Create a new Mistral provider (uses OpenAI-compatible API)
    pub fn new_mistral(api_key: String) -> Result<Self>;
    /// Create an Ollama provider using Ollama's OpenAI-compatible API.
    pub fn new_ollama(base_url: String, model: String) -> Result<Self>;
    /// Create a new OpenAI provider
    pub fn new_openai(api_key: String) -> Result<Self>;
    /// Create a provider that talks to a remote finch daemon's OpenAI-compatible endpoint.
    pub fn new_remote_daemon(address: String) -> Result<Self>;
    /// Set custom model for this provider
    pub fn with_model(mut self, model: impl Into<String>) -> Self;
    /// Set provider-side reasoning depth for models that support it.
    pub fn with_reasoning_effort(mut self, effort: ReasoningEffort) -> Self;
}
/// Strict OpenAI-specific dialect; reusable OAuth state remains in `oauth`.
pub struct OpenAiChatGptOAuthDialect<V> { … }
impl OpenAiChatGptOAuthDialect {
    pub fn production() -> Result<Self>;
}
/// Bounded, single-flight verifier for the exact pinned OpenAI issuer.
pub struct OpenAiJwksVerifier { … }
impl OpenAiJwksVerifier {
    /// Construct the production verifier for the exact pinned OpenAI authority.
    pub fn production() -> Result<Self>;
}
/// Statistics about context optimization
pub struct OptimizationStats { … }
/// Maximum completion tokens accepted by the exact model/adapter pair.
pub struct OutputTokenLimitCapability { … }
impl OutputTokenLimitCapability {
    pub fn unknown() -> Self;
}
pub struct ProviderAllowance { … }
pub struct ProviderEndpoints { … }
impl ProviderEndpoints {
    pub fn new(base_url: &str, chat_path: &str, models_path: &str) -> Self;
}
/// A cloud provider graph constructed exactly once from configuration.
pub struct ProviderGraph { … }
impl ProviderGraph {
    /// Shared primary/fallback provider used by compatibility clients.
    pub fn default_provider(&self) -> Arc<dyn LlmProvider>;
    /// Successfully constructed named profiles in configured fallback order.
    pub fn profiles(&self) -> &[ProviderProfile];
}
/// A successfully constructed cloud provider paired with its configured selector.
pub struct ProviderProfile { … }
impl ProviderProfile {
    /// Capabilities of this profile's configured model.
    pub fn capabilities(&self) -> Result<super::ModelCapabilities>;
    /// The stable configured selector for this provider.
    pub fn profile_name(&self) -> &str;
    /// The shared provider instance owned by this profile.
    pub fn provider(&self) -> &Arc<dyn LlmProvider>;
}
/// Unified request format for all LLM providers  This wraps the existing Message format and adds provider-agnostic options.
pub struct ProviderRequest { … }
impl ProviderRequest {
    /// Create a new request from messages
    pub fn new(messages: Vec<Message>) -> Self;
    /// Remove orphaned tool_use blocks from the end of the conversation.
    pub fn sanitize_messages(&mut self);
    /// Truncate conversation history to fit within a provider's context window.
    pub fn truncate_to_context_limit(&mut self, token_limit: usize) -> usize;
    pub fn with_cancellation_token(mut self, cancellation_token: tokio_util::sync::CancellationToken) -> Self;
    /// Set max tokens
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self;
    /// Set the model name
    pub fn with_model(mut self, model: impl Into<String>) -> Self;
    /// Enable streaming
    pub fn with_stream(mut self, stream: bool) -> Self;
    /// Set system prompt
    pub fn with_system(mut self, system: impl Into<String>) -> Self;
    /// Set temperature
    pub fn with_temperature(mut self, temperature: f32) -> Self;
    /// Add tools to the request
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self;
}
/// Unified response format from LLM providers  This wraps the provider-specific response in a common format.
pub struct ProviderResponse { … }
impl ProviderResponse {
    /// Check if response contains tool uses
    pub fn has_tool_uses(&self) -> bool;
    /// Extract text from the response
    pub fn text(&self) -> String;
    /// Convert to Message for conversation history
    pub fn to_message(&self) -> Message;
    /// Extract tool uses from response
    pub fn tool_uses(&self) -> Vec<crate::tools::ToolUse>;
}
pub struct ProviderUsage { … }
/// Exact reasoning-effort values accepted by one provider/model adapter.
pub struct ReasoningCapability { … }
impl ReasoningCapability {
    pub fn allowed(efforts: impl IntoIterator<Item = ReasoningEffort>, tested_on: &str, source: &str) -> Self;
    pub fn support(&self) -> CapabilitySupport;
    pub fn unknown() -> Self;
    pub fn unsupported(tested_on: &str, source: &str) -> Self;
}
/// Streaming chunk (text delta or complete block) Re-exported from `generators`.
pub enum StreamChunk { TextDelta, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Configuration for teacher context management
pub struct TeacherContextConfig { … }
/// Teacher session with context tracking  Tracks what context has been sent to the teacher provider to enable: - Metrics on new vs repeated context - Optional t…
pub struct TeacherSession { … }
impl TeacherSession {
    /// Send message with context tracking (Level 1: Minimal)  Tracks new vs repeated context for metrics and logging.
    pub async fn send_message(&mut self, request: &ProviderRequest) -> Result<ProviderResponse>;
    /// Send message with streaming response
    pub async fn send_message_stream(&mut self, request: &ProviderRequest) -> Result<mpsc::Receiver<Result<StreamChunk>>>;
    /// Send message with smart context optimization (Level 3: Full)  Applies multiple optimization strategies: - Preserves system prompts - Drops old tool results (…
    pub async fn send_message_with_optimization(&mut self, request: &ProviderRequest) -> Result<ProviderResponse>;
    /// Send message with optional context truncation (Level 2: Basic)  If max_context_turns is configured, only sends recent conversation history.
    pub async fn send_message_with_truncation(&mut self, request: &ProviderRequest) -> Result<ProviderResponse>;
    /// Create a new teacher session with default config
    pub fn new(provider: Box<dyn LlmProvider>) -> Self;
    /// Get optimization statistics
    pub fn optimization_stats(&self) -> OptimizationStats;
    /// Get teacher provider name
    pub fn provider_name(&self) -> &str;
    /// Reset conversation state (e.g., when starting new conversation)
    pub fn reset_state(&mut self);
    /// Get current conversation state (for metrics/debugging)
    pub fn state(&self) -> &ConversationState;
    /// Create a new teacher session with custom config
    pub fn with_config(provider: Box<dyn LlmProvider>, config: TeacherContextConfig) -> Self;
    /// Create a session from an already validated shared provider.
    pub fn with_shared_provider(provider: std::sync::Arc<dyn LlmProvider>, config: TeacherContextConfig) -> Self;
}
/// A request whose effective provider/model identity and optional capabilities were checked by Finch's non-overridable dispatch boundary.
pub struct ValidatedProviderRequest { … }
impl ValidatedProviderRequest {
    /// The exact descriptor used to validate this request.
    pub fn capabilities(&self) -> &ModelCapabilities;
    /// Consume this token at the exact provider instance for which it was validated and return the effective request.
    pub fn into_request_for(self, provider: &(impl ProviderBackend + ?Sized)) -> Result<ProviderRequest>;
}
/// Signature-verified provider claims.
pub struct VerifiedOpenAiClaims { … }
/// Request/response protocol used by the provider adapter for this model.
pub enum WireProtocol { AnthropicMessages, OpenAiChatCompletions, OpenAiChatGptResponsesLite, GeminiGenerateContent }
/// Known wire protocol and the evidence for that binding.
pub struct WireProtocolCapability { … }
```

## Traits

```rust
/// Non-overridable validated dispatch API shared by every provider backend.
pub trait LlmProvider: ProviderBackend {
    fn supports_streaming(&self) -> bool;
    fn supports_tools(&self) -> bool;
}
/// Injected JWS/JWKS verification boundary.
pub trait OpenAiTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
}
/// Provider implementation hooks.
pub trait ProviderBackend: ProviderConcreteType + Send + Sync {
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn capabilities(&self, model: &str) -> ModelCapabilities;
    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<crate::config::ReasoningEffort>;
}
/// Non-overridable concrete type identity used by validated dispatch tokens.
pub trait ProviderConcreteType: Any {
    fn provider_concrete_type_id(&self) -> TypeId;
}
```

## Functions

```rust
/// Finch-local capability attached to a verified ChatGPT account credential.
pub fn chatgpt_required_scopes() -> BTreeSet<String> { … }
/// Create a fallback chain with all teachers in priority order.
pub fn create_provider(teachers: &[TeacherEntry]) -> Result<Box<dyn LlmProvider>> { … }
/// Create the active provider or fallback chain from unified or legacy configuration.
pub fn create_provider_from_config(config: &Config) -> Result<Box<dyn LlmProvider>> { … }
/// Return a single `LlmProvider` from a slice of unified entries.
pub fn create_provider_from_entries(entries: &[ProviderEntry]) -> Result<Box<dyn LlmProvider>> { … }
/// Create a cloud `LlmProvider` from a unified `ProviderEntry`.
pub fn create_provider_from_entry(entry: &ProviderEntry) -> Result<Box<dyn LlmProvider>> { … }
/// Create a single provider from a `TeacherEntry`.
pub fn create_provider_from_teacher(entry: &TeacherEntry) -> Result<Box<dyn LlmProvider>> { … }
/// Construct the named cloud provider graph once.
pub fn create_provider_graph_from_config(config: &Config) -> Result<ProviderGraph> { … }
/// Construct a provider graph using an injected local credential resolver.
pub fn create_provider_graph_from_config_with_resolver(config: &Config, resolver: &dyn CredentialResolver) -> Result<ProviderGraph> { … }
/// Revalidate the complete graph and return one configured profile.
pub fn create_provider_profile_from_config(config: &Config, profile_name: &str) -> Result<Arc<dyn LlmProvider>> { … }
/// Revalidate the complete graph with an injected credential resolver and return one configured profile.
pub fn create_provider_profile_from_config_with_resolver(config: &Config, profile_name: &str, resolver: &dyn CredentialResolver) -> Result<Arc<dyn LlmProvider>> { … }
/// Create providers from teacher entries in priority order.
pub fn create_providers(teachers: &[TeacherEntry]) -> Result<Vec<Box<dyn LlmProvider>>> { … }
/// Create the ordered cloud provider pool from unified configuration, falling back to legacy teacher entries only when no cloud `[[providers]]` entries exist.
pub fn create_providers_from_config(config: &Config) -> Result<Vec<Box<dyn LlmProvider>>> { … }
/// Create providers from a slice of unified `ProviderEntry` values.
pub fn create_providers_from_entries(entries: &[ProviderEntry]) -> Result<Vec<Box<dyn LlmProvider>>> { … }
pub fn default_cache_dir() -> Result<PathBuf> { … }
pub fn fallback_catalog(provider: &str, models_url: &str) -> ModelCatalog { … }
/// Validate the complete provider/credential metadata graph and every named transport before secret resolution, provider construction, or selection shortcuts ca…
pub fn preflight_provider_config(config: &Config) -> Result<()> { … }
/// Opaque full-width cache/request identity.
pub fn profile_cache_identity(profile: &ModelCatalogProfile) -> String { … }
pub fn read_cache(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<Option<ModelCatalog>> { … }
/// Fetch a catalogue and persist only endpoint/model metadata.
pub async fn refresh(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<ModelCatalog> { … }
/// Revalidate and resolve a named provider profile immediately before model discovery.
pub async fn refresh_from_config(config: &Config, profile_name: &str, resolver: &dyn CredentialResolver, cache_dir: &Path) -> Result<ModelCatalog> { … }
/// Use a successful live refresh, then the matching cache, then a visibly labelled static fallback.
pub async fn refresh_with_fallback(profile: &ModelCatalogProfile, cache_dir: &Path) -> (ModelCatalog, Option<String>) { … }
pub(crate) fn resolve_effective_request(provider: &(impl ProviderBackend + ?Sized), request: &ProviderRequest) -> Result<(ProviderRequest, ModelCapabilities)> { … }
pub fn static_fallback(provider: &str) -> Vec<String> { … }
pub(crate) fn validate_provider_request(provider: &(impl ProviderBackend + ?Sized), request: &ProviderRequest, streaming: bool) -> Result<ValidatedProviderRequest> { … }
/// Inject the alignment prompt into an existing system prompt, or return it standalone.
pub fn with_alignment(system: Option<&str>) -> String { … }
```

## Constants

```rust
pub const CHATGPT_OAUTH_PROTOCOL_REVISION: &str = "openai-codex-public-client@94cbbddafc1776d5e377bca1b05932c697e82238+finch-binding-v2";
pub const OPENAI_PUBLIC_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REQUIRED_TOKEN_ISSUER: &str = "https://auth.openai.com";
/// Date on which Finch's bundled, deliberately incomplete model fallback was reviewed.
pub const STATIC_FALLBACK_AS_OF: &str = "2026-08-26";
/// Prompt that normalizes output discipline across all LLM providers.
pub const UNIVERSAL_ALIGNMENT_PROMPT: &str = "\ ## Output Discipline These rules override any stylistic defaults: 1. When asked for JSON, return ONLY the JSON. No markdown code fences. No prose before \ or after. The first character of your response must be `[` or `{`. 2. When given a numbered format (1. Step one\n2. Step two), follow it exactly. 3. When given field names or schema, use them verbatim — no renaming, no extras. 4. Do not add unsolicited caveats, disclaimers, or explanations unless the instruction \ explicitly requests them. 5. Treat every instruction as binding, not advisory."; /// Inject the alignment prompt into an existing system prompt, or return it standalone. /// /// The alignment instructions are prepended so they take priority over any other /// stylistic context in the system prompt. pub fn with_alignment(system: Option<&str>) -> String { match system { Some(existing) if !existing.trim().is_empty() => { format!("{}\n\n{}", UNIVERSAL_ALIGNMENT_PROMPT.trim(), existing) } _ => UNIVERSAL_ALIGNMENT_PROMPT.trim().to_string(), } } #[cfg(test)] mod tests { use super::*; #[test] fn test_with_alignment_no_system() { let result = with_alignment(None); assert!(result.contains("Output Discipline")); assert!(result.starts_with("## Output Discipline")); } #[test] fn test_with_alignment_empty_system() { let result = with_alignment(Some("")); // Empty system treated same as None — just the alignment prompt, no extra suffix assert!(result.contains("Output Discipline")); assert_eq!(result, UNIVERSAL_ALIGNMENT_PROMPT.trim()); } #[test] fn test_with_alignment_prepends_to_existing() { let result = with_alignment(Some("Be a helpful assistant.")); assert!(result.starts_with("## Output Discipline")); assert!(result.contains("Be a helpful assistant.")); // Alignment comes first let align_pos = result.find("Output Discipline").unwrap(); let system_pos = result.find("Be a helpful").unwrap(); assert!(align_pos < system_pos); } #[test] fn test_with_alignment_whitespace_only_system() { let result = with_alignment(Some(" \n ")); // Whitespace-only treated same as None assert!(result.starts_with("## Output Discipline")); } #[test] fn test_universal_alignment_prompt_has_json_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("JSON")); assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("code fences")); } #[test] fn test_universal_alignment_prompt_has_numbered_format_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("numbered format")); } }
```
