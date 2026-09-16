# finch-providers — public interface

Generated from [`crates/finch-providers/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-providers/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Audience or endpoint-family binding persisted with a credential/profile.
pub struct AudienceBinding { … }
impl AudienceBinding {
    pub fn custom(endpoint: &str) -> Result<Self>;
    pub fn standard(family: EndpointFamily) -> Self;
}
/// One compiled row of a binding table.
pub struct BoundTool { … }
impl BoundTool {
    /// Anthropic Messages tool definition using the compiled wire name.
    pub fn anthropic_tool(&self) -> ToolDefinition;
    /// ChatGPT Responses-Lite function tool inside the `functions` namespace.
    pub fn chatgpt_function(&self) -> Value;
    /// Gemini `functionDeclarations` entry.
    pub fn gemini_declaration(&self) -> Value;
    /// OpenAI chat-completions `tools[]` function entry.
    pub fn openai_tool(&self) -> Value;
}
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
pub struct ChatGptSubscriptionProvider { … }
impl ChatGptSubscriptionProvider {
    pub fn production(credential: &ProviderCredential, model: Option<&str>, reasoning_effort: Option<ReasoningEffort>) -> Result<Self>;
}
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
/// Content block - supports text, image, tool_use, and tool_result
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
/// Context limits for a model, preserving the unit used by the boundary.
pub struct ContextWindowCapability { … }
impl ContextWindowCapability {
    /// Construct an unknown context window with no assumed default.
    pub fn unknown() -> Self;
}
/// Tracks the state of conversation with teacher
pub struct ConversationState { … }
/// Profile-side authentication contract.
pub struct CredentialBinding { … }
/// Authentication mechanism represented by a named credential.
pub enum CredentialKind { ApiKey, Bearer, OauthDevice, OauthBrowserPkce, CloudIdentity, LocalSocket, None }
/// Persisted lifecycle metadata.
pub enum CredentialLifecycle { Active, Revoked, LegacyAmbiguous }
/// Provider/account namespace.
pub enum CredentialProvider { Anthropic, OpenaiPlatform, ChatgptSubscription, Xai, GeminiAiStudio, GoogleVertex, Mistral, Groq, Openrouter }
impl CredentialProvider {
    pub fn as_str(self) -> &'static str;
}
/// Normalized service family.
pub enum EndpointFamily { AnthropicApi, OpenaiPlatform, ChatgptSubscription, XaiApi, GeminiAiStudio, GoogleVertex, MistralApi, GroqApi, OpenrouterApi, Custom }
/// Production resolver for explicit `env:VARIABLE_NAME` opaque references.
pub struct EnvironmentCredentialResolver;
/// Provider/model/event provenance retained with a stream event.
pub struct EventProvenance { … }
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
/// In-memory clock for deterministic tests.
pub struct FrozenClock { … }
impl FrozenClock {
    pub fn new(now: DateTime<Utc>) -> Self;
}
/// Google Gemini API provider  Supports Gemini 2.0 Flash and other Gemini models.
pub struct GeminiProvider { … }
impl GeminiProvider {
    /// Create a new Gemini provider
    pub fn new(api_key: String) -> Result<Self>;
    /// Create with custom default model
    pub fn with_model(mut self, model: impl Into<String>) -> Self;
}
/// Source for an image content block
pub struct ImageSource { … }
/// Instant sleeper for deterministic tests.
pub struct InstantSleeper;
/// Provider-neutral identity and accounting for one completed inference.
pub struct InvocationMetadata { … }
impl InvocationMetadata {
    pub fn validate(&self) -> anyhow::Result<()>;
}
pub struct LifecycleRevocation(Arc<AtomicBool>);
impl LifecycleRevocation {
    pub fn is_revoked(&self) -> bool;
    pub fn revoke(&self);
}
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
pub struct MessageRequest { … }
impl MessageRequest {
    /// Append a user message to existing conversation
    pub fn append_user_message(mut self, content: String) -> Self;
    pub fn new(user_query: &str) -> Self;
    /// Create request with full conversation context
    pub fn with_context(messages: Vec<Message>) -> Self;
    /// Set a system prompt for the request
    pub fn with_system(mut self, system: impl Into<String>) -> Self;
    /// Add tools to the request
    pub fn with_tools(mut self, tools: Vec<ToolDefinition>) -> Self;
}
pub struct MessageResponse { … }
impl MessageResponse {
    /// Check if response contains tool uses
    pub fn has_tool_uses(&self) -> bool;
    /// Extract text from the response
    pub fn text(&self) -> String;
    /// Convert response to a Message for conversation history
    pub fn to_message(&self) -> Message;
    /// Extract tool uses from response
    pub fn tool_uses(&self) -> Vec<crate::ToolUse>;
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
/// Grant allowing a provider-native tool to be advertised.
pub struct NativeToolGrant { … }
/// A marker error type that tells `with_retry` not to retry the request.
pub struct NonRetriableError(pub String);
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
    pub fn for_test(auth_origin: &str, verifier: Arc<V>) -> Result<Self>;
    pub fn production() -> Result<Self>;
}
/// Bounded, single-flight verifier for the exact pinned OpenAI issuer.
pub struct OpenAiJwksVerifier { … }
impl OpenAiJwksVerifier {
    pub fn for_test(authority_origin: &str, expected_issuer: &str, client_id: &str, timeout: Duration) -> Result<Self>;
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
/// Secret-free, named provider credential metadata.
pub struct ProviderCredential { … }
pub struct ProviderEndpoints { … }
impl ProviderEndpoints {
    pub fn new(base_url: &str, chat_path: &str, models_path: &str) -> Self;
}
/// Production and test runtime handles.
pub struct ProviderPorts { … }
impl ProviderPorts {
    /// Production ports: reqwest, wall clock, tokio sleep, fail-closed UX.
    pub fn production() -> Result<Self>;
}
/// Unified request format for all LLM providers  This wraps the existing Message format and adds provider-agnostic options.
pub struct ProviderRequest { … }
impl ProviderRequest {
    /// Create a new request from messages
    pub fn new(messages: Vec<Message>) -> Self;
    /// Remove orphaned tool_use blocks from the end of the conversation.
    pub fn sanitize_messages(&mut self);
    /// Policy used when compiling this request's tool bindings.
    pub fn tool_policy(&self) -> &ToolCompilePolicy;
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
    /// Attach Finch authority metadata and native-tool grants for compilation.
    pub fn with_tool_policy(mut self, policy: ToolCompilePolicy) -> Self;
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
    pub fn tool_uses(&self) -> Vec<crate::ToolUse>;
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
/// Provider-controlled reasoning depth.
pub enum ReasoningEffort { None, Minimal, Low, Medium, High, Xhigh, Max }
impl ReasoningEffort {
    pub fn as_str(self) -> &'static str;
}
/// Default reqwest transport.
pub struct ReqwestTransport { … }
impl ReqwestTransport {
    pub fn new() -> Result<Self>;
}
/// Immutable, redaction-safe resolved credential handle.
pub struct ResolvedCredential { … }
/// Secret bytes returned by an injected local credential resolver.
pub struct ResolvedSecret(String);
impl ResolvedSecret {
    /// Expose secret bytes to a trusted resolver/transport boundary.
    pub fn expose(&self) -> &str;
    pub fn new(secret: impl Into<String>) -> Result<Self>;
}
/// How a tool result is written back on this protocol.
pub enum ResultEncoding { AnthropicToolResult, OpenAiToolMessage, ChatGptFunctionCallOutput, GeminiFunctionResponse }
/// One semantic tool offered for compilation.
pub struct SemanticTool { … }
impl SemanticTool {
    /// Finch-owned semantic tool with unclassified authority.
    pub fn finch(identity: impl Into<String>, description: impl Into<String>, schema: ToolInputSchema) -> Self;
    /// Mark this as a provider-native tool that still needs handler+grant.
    pub fn provider_native(mut self, wire_name: impl Into<String>, namespace: Option<String>) -> Self;
    /// Attach declared authority.
    pub fn with_authority(mut self, authority: ToolAuthority) -> Self;
}
/// Streaming chunk (text delta, reasoning, tool call, or complete block).
pub enum StreamChunk { TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Delta within a streaming event
pub struct StreamDelta { … }
/// Server-Sent Event from Claude API
pub struct StreamEvent { … }
impl StreamEvent {
    /// Check if this event contains a text delta
    pub fn is_text_delta(&self) -> bool;
    /// Check if this event signals a tool_use block starting
    pub fn is_tool_use_start(&self) -> bool;
    /// Extract text from the event if available
    pub fn text(&self) -> Option<&str>;
}
/// Wall-clock UTC.
pub struct SystemClock;
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
/// Tokio sleeper.
pub struct TokioSleeper;
/// Authority class carried with a semantic tool.
pub enum ToolAuthority { Pure, VmRead, VmWrite, WorkspaceRead, ExternalRead, WorkspaceWrite, ExternalWrite, Destructive, Unclassified }
impl ToolAuthority {
    /// Snake-case name used in diagnostics.
    pub fn as_str(self) -> &'static str;
}
/// Why compilation or decode failed.
pub enum ToolBindingError { DuplicateLocalIdentity, DuplicateWireIdentity, ReservedNameCollision, CaseCollision, TruncationCollision, NameTooLong, InvalidIdentifier, LossySchemaConversion, UnsupportedSchemaFeature, UnknownWireCall, UnknownNamespace, UnknownSemanticIdentity, NativeToolWithoutHandler, NativeToolWithoutGrant, UnknownWireProtocol, TooManyTools }
/// Immutable bijective map from semantic identities to wire identities for one validated request.
pub struct ToolBindingTable { … }
impl ToolBindingTable {
    /// Decode a provider wire call into the semantic binding for this table.
    pub fn decode_wire_call(&self, name: &str, namespace: Option<&str>) -> Result<&BoundTool, ToolBindingError>;
    /// Empty table for a request that advertised no tools.
    pub fn empty(protocol: WireProtocol, provider: impl Into<String>, model: impl Into<String>) -> Self;
    /// Look up the binding for a semantic Finch identity.
    pub fn encode_semantic(&self, identity: &str) -> Result<&BoundTool, ToolBindingError>;
    /// Compiled rows in advertisement order.
    pub fn entries(&self) -> &[BoundTool];
    /// True when no tools were advertised.
    pub fn is_empty(&self) -> bool;
    /// Number of advertised tools.
    pub fn len(&self) -> usize;
    /// Model identity recorded at compile time.
    pub fn model(&self) -> &str;
    /// Wire protocol this table was compiled for.
    pub fn protocol(&self) -> WireProtocol;
    /// Provider identity recorded at compile time.
    pub fn provider(&self) -> &str;
}
/// Optional extras Finch attaches so compilation can record authority and native-tool grants.
pub struct ToolCompilePolicy { … }
impl ToolCompilePolicy {
    /// Empty policy: unclassified authority, no native grants.
    pub fn new() -> Self;
    /// Record authority for one semantic identity.
    pub fn with_authority(mut self, identity: impl Into<String>, authority: ToolAuthority) -> Self;
    /// Allow one provider-native tool to be advertised.
    pub fn with_native_grant(mut self, grant: NativeToolGrant) -> Self;
}
/// Tool definition (Claude API-compatible)
pub struct ToolDefinition { … }
/// JSON Schema for tool input parameters
pub struct ToolInputSchema { … }
impl ToolInputSchema {
    /// Create a simple schema with required string parameters
    pub fn simple(params: Vec<(&str, &str)>) -> Self;
}
/// Where a tool identity comes from.
pub enum ToolOrigin { Semantic, ProviderNative }
/// Tool use request after adapter-level validation.
pub struct ToolUse { … }
impl ToolUse {
    /// Generate unique tool use ID
    pub fn generate_id() -> String;
    pub fn new(name: String, input: Value) -> Self;
    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock;
}
/// A request whose effective provider/model identity and optional capabilities were checked by Finch's non-overridable dispatch boundary.
pub struct ValidatedProviderRequest { … }
impl ValidatedProviderRequest {
    /// The exact descriptor used to validate this request.
    pub fn capabilities(&self) -> &ModelCapabilities;
    /// Consume this token at the exact provider instance for which it was validated and return the effective request plus the immutable tool-binding table compiled…
    pub fn into_request_for(self, provider: &(impl ProviderBackend + ?Sized)) -> Result<(ProviderRequest, Arc<ToolBindingTable>)>;
    /// Immutable tool-binding table compiled for this validated request.
    pub fn tool_bindings(&self) -> &Arc<ToolBindingTable>;
}
/// Signature-verified provider claims.
pub struct VerifiedOpenAiClaims { … }
/// Request/response protocol used by the provider adapter for this model.
pub enum WireProtocol { AnthropicMessages, OpenAiChatCompletions, OpenAiChatGptResponsesLite, GeminiGenerateContent }
/// Known wire protocol and the evidence for that binding.
pub struct WireProtocolCapability { … }
/// Collision-free identity on one provider wire.
pub struct WireToolIdentity { … }
/// Wire-level kind advertised to the provider.
pub enum WireToolKind { Function, ProviderNative }
```

## Traits

```rust
/// Optional presenter for device/browser authorization UX.
pub trait AuthorizationPresenter: Send + Sync {
    fn present_device_login(&self, verification_uri: &str, user_code: &str) -> Result<()>;
    fn present_browser_login(&self, authorization_url: &str) -> Result<()>;
}
/// Billing-action confirmation.
pub trait BillingActionConfirmer: Send + Sync {
    fn confirm(&self, action: &str, provider: &str) -> Result<bool>;
}
/// Clock used for credential expiry and lease checks.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}
/// Injected secret store boundary.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential>;
}
/// Bounded HTTP POST used by OAuth and catalog transports.
pub trait HttpTransport: Send + Sync {
    async fn post(&self, request: OAuthHttpRequest, timeout: Duration, cancel: &CancellationToken) -> Result<(StatusCode, Vec<u8>)>;
}
/// Non-overridable validated dispatch API shared by every provider backend.
pub trait LlmProvider: ProviderBackend {
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse>;
    async fn send_message_stream(&self, request: &ProviderRequest) -> Result<Receiver<Result<StreamChunk>>>;
    fn supports_streaming(&self) -> bool;
    fn supports_tools(&self) -> bool;
}
/// Injected JWS/JWKS verification boundary.
pub trait OpenAiTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
    async fn verify(&self, id_token: Option<&str>, access_token: &str, cancel: &CancellationToken) -> Result<VerifiedOpenAiClaims>;
}
/// Provider implementation hooks.
pub trait ProviderBackend: ProviderConcreteType + Send + Sync {
    async fn send_message_validated(&self, request: ValidatedProviderRequest) -> Result<ProviderResponse>;
    async fn send_message_stream_validated(&self, request: ValidatedProviderRequest) -> Result<Receiver<Result<StreamChunk>>>;
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn capabilities(&self, model: &str) -> ModelCapabilities;
    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort>;
}
/// Non-overridable concrete type identity used by validated dispatch tokens.
pub trait ProviderConcreteType: Any {
    fn provider_concrete_type_id(&self) -> TypeId;
}
/// Secret-free log/telemetry sink.
pub trait ProviderTelemetry: Send + Sync {
    fn event(&self, name: &str, fields: &[(&str, &str)]);
}
/// Backoff/sleeper used by OAuth polling and HTTP retry.
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}
```

## Functions

```rust
/// Finch-local capability attached to a verified ChatGPT account credential.
pub fn chatgpt_required_scopes() -> BTreeSet<String> { … }
/// Compile `ToolDefinition`s plus an optional Finch policy into a table.
pub fn compile_from_definitions(protocol: WireProtocol, provider: &str, model: &str, definitions: &[ToolDefinition], policy: &ToolCompilePolicy) -> Result<ToolBindingTable, ToolBindingError> { … }
/// Compile semantic tools into an immutable bijective binding table.
pub fn compile_tool_bindings(protocol: WireProtocol, provider: impl Into<String>, model: impl Into<String>, tools: &[SemanticTool]) -> Result<ToolBindingTable, ToolBindingError> { … }
/// Names of profiles that depend on a credential, for revoke/delete UX.
pub fn credential_dependencies<'a>(credential_name: &str, profiles: impl IntoIterator<Item = (String, Option<&'a CredentialBinding>)>) -> Vec<String> { … }
/// Validate all credential metadata and return a stable name index.
pub fn credential_index(credentials: &[ProviderCredential]) -> Result<BTreeMap<&str, &ProviderCredential>> { … }
pub fn default_cache_dir() -> Result<PathBuf> { … }
pub fn fallback_catalog(provider: &str, models_url: &str) -> ModelCatalog { … }
/// Normalize a configured base URL to its lowercase scheme/host/port origin.
pub fn normalize_origin(endpoint: &str) -> Result<String> { … }
/// Opaque full-width cache/request identity.
pub fn profile_cache_identity(profile: &ModelCatalogProfile) -> String { … }
pub fn read_cache(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<Option<ModelCatalog>> { … }
/// Fetch a catalogue and persist only endpoint/model metadata.
pub async fn refresh(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<ModelCatalog> { … }
/// Use a successful live refresh, then the matching cache, then a visibly labelled static fallback.
pub async fn refresh_with_fallback(profile: &ModelCatalogProfile, cache_dir: &Path) -> (ModelCatalog, Option<String>) { … }
/// Determine the binding required by a provider profile and endpoint.
pub fn required_audience(provider: CredentialProvider, endpoint: Option<&str>) -> Result<AudienceBinding> { … }
pub(crate) fn resolve_effective_request(provider: &(impl ProviderBackend + ?Sized), request: &ProviderRequest) -> Result<(ProviderRequest, ModelCapabilities)> { … }
pub fn static_fallback(provider: &str) -> Vec<String> { … }
/// Reject absolute authenticated path overrides that leave the bound origin.
pub fn validate_authenticated_endpoints(provider: CredentialProvider, base_url: Option<&str>, overrides: &[Option<&str>]) -> Result<()> { … }
/// Validate a profile reference against one named credential without resolving secret material or performing external activity.
pub fn validate_binding(provider: CredentialProvider, endpoint: Option<&str>, binding: &CredentialBinding, credential: &ProviderCredential, now: DateTime<Utc>) -> Result<()> { … }
pub(crate) fn validate_provider_request(provider: &(impl ProviderBackend + ?Sized), request: &ProviderRequest, streaming: bool) -> Result<ValidatedProviderRequest> { … }
pub(crate) fn validate_response_model(model: &str) -> Result<()> { … }
/// Inject the alignment prompt into an existing system prompt, or return it standalone.
pub fn with_alignment(system: Option<&str>) -> String { … }
/// Execute a function with exponential backoff retry logic.
pub async fn with_retry<F, Fut, T>(f: F) -> Result<T> where F: Fn() -> Fut, Fut: std::future::Future<Output = Result<T>>, { … }
```

## Constants

```rust
pub const CHATGPT_OAUTH_PROTOCOL_REVISION: &str = "openai-codex-public-client@94cbbddafc1776d5e377bca1b05932c697e82238+finch-binding-v2";
/// Default Claude model used when a transport does not override it.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5";
/// Default completion budget used by Anthropic request envelopes.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8000;
/// Upper bound on advertised tools for every current wire protocol.
pub const MAX_ADVERTISED_TOOLS: usize = 256;
pub const OPENAI_PUBLIC_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const REQUIRED_TOKEN_ISSUER: &str = "https://auth.openai.com";
/// Date on which Finch's bundled, deliberately incomplete model fallback was reviewed.
pub const STATIC_FALLBACK_AS_OF: &str = "2026-08-26";
/// Prompt that normalizes output discipline across all LLM providers.
pub const UNIVERSAL_ALIGNMENT_PROMPT: &str = "\ ## Output Discipline These rules override any stylistic defaults: 1. When asked for JSON, return ONLY the JSON. No markdown code fences. No prose before \ or after. The first character of your response must be `[` or `{`. 2. When given a numbered format (1. Step one\n2. Step two), follow it exactly. 3. When given field names or schema, use them verbatim — no renaming, no extras. 4. Do not add unsolicited caveats, disclaimers, or explanations unless the instruction \ explicitly requests them. 5. Treat every instruction as binding, not advisory."; /// Inject the alignment prompt into an existing system prompt, or return it standalone. /// /// The alignment instructions are prepended so they take priority over any other /// stylistic context in the system prompt. pub fn with_alignment(system: Option<&str>) -> String { match system { Some(existing) if !existing.trim().is_empty() => { format!("{}\n\n{}", UNIVERSAL_ALIGNMENT_PROMPT.trim(), existing) } _ => UNIVERSAL_ALIGNMENT_PROMPT.trim().to_string(), } } #[cfg(test)] mod tests { use super::*; #[test] fn test_with_alignment_no_system() { let result = with_alignment(None); assert!(result.contains("Output Discipline")); assert!(result.starts_with("## Output Discipline")); } #[test] fn test_with_alignment_empty_system() { let result = with_alignment(Some("")); // Empty system treated same as None — just the alignment prompt, no extra suffix assert!(result.contains("Output Discipline")); assert_eq!(result, UNIVERSAL_ALIGNMENT_PROMPT.trim()); } #[test] fn test_with_alignment_prepends_to_existing() { let result = with_alignment(Some("Be a helpful assistant.")); assert!(result.starts_with("## Output Discipline")); assert!(result.contains("Be a helpful assistant.")); // Alignment comes first let align_pos = result.find("Output Discipline").unwrap(); let system_pos = result.find("Be a helpful").unwrap(); assert!(align_pos < system_pos); } #[test] fn test_with_alignment_whitespace_only_system() { let result = with_alignment(Some(" \n ")); // Whitespace-only treated same as None assert!(result.starts_with("## Output Discipline")); } #[test] fn test_universal_alignment_prompt_has_json_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("JSON")); assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("code fences")); } #[test] fn test_universal_alignment_prompt_has_numbered_format_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("numbered format")); } }
```

## Modules

```rust
pub mod oauth;
```
