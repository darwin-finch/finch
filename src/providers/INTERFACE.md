# providers — public interface

Generated from [`src/providers/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/providers/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Where Finch obtained a capability claim. Re-exported from `finch-providers`.
pub enum CapabilityProvenance { RuntimeDiscovery, StaticMetadata, Configuration, Unknown }
/// Whether a provider/model capability is known to be usable. Re-exported from `finch-providers`.
pub enum CapabilitySupport { Supported, Unsupported, Unknown }
/// Re-exported from `finch-providers`.
pub enum CatalogAuth { AnthropicApiKey, Bearer }
/// Re-exported from `finch-providers`.
pub enum CatalogSource { Discovered, Cache, StaticFallback }
/// Secret-free stage markers for actionable device-login diagnostics. Re-exported from `finch-providers`.
pub enum ChatGptAuthStageError { PollContract, TokenExchangeRejected, TokenExchangeContract, IdentityVerification, ClientBinding, AccountEntitlement }
/// Status-only ChatGPT device endpoint failures. Re-exported from `finch-providers`.
pub enum ChatGptDeviceEndpointError { StartDisabledOrUnsupported, StartRejected, PollRejected }
/// Re-exported from `finch-providers`.
pub struct ChatGptSubscriptionProvider { … }
/// Claude API provider  Implements the LlmProvider trait for Anthropic's Claude API. Re-exported from `finch-providers`.
pub struct ClaudeProvider { … }
/// Content block - supports text, image, tool_use, and tool_result Re-exported from `finch-providers`.
pub enum ContentBlock { Text, Image, ToolUse, ToolResult, OpaqueReasoning }
/// Context limits for a model, preserving the unit used by the boundary. Re-exported from `finch-providers`.
pub struct ContextWindowCapability { … }
/// Tracks the state of conversation with teacher Re-exported from `finch-providers`.
pub struct ConversationState { … }
/// Provider/model/event provenance retained with a stream event. Re-exported from `finch-providers`.
pub struct EventProvenance { … }
/// A chain of providers to try in order Re-exported from `finch-providers`.
pub struct FallbackChain { … }
/// Google Gemini API provider  Supports Gemini 2.0 Flash and other Gemini models. Re-exported from `finch-providers`.
pub struct GeminiProvider { … }
/// Source for an image content block Re-exported from `finch-providers`.
pub struct ImageSource { … }
/// Provider-neutral identity and accounting for one completed inference. Re-exported from `finch-providers`.
pub struct InvocationMetadata { … }
/// Re-exported from `finch-providers`.
pub struct Message { … }
/// Re-exported from `finch-providers`.
pub struct MessageRequest { … }
/// Re-exported from `finch-providers`.
pub struct MessageResponse { … }
/// Capabilities of one exact provider/model pair. Re-exported from `finch-providers`.
pub struct ModelCapabilities { … }
/// Re-exported from `finch-providers`.
pub struct ModelCatalog { … }
/// Re-exported from `finch-providers`.
pub struct ModelCatalogProfile { … }
/// A single yes/no/unknown model feature with its evidence. Re-exported from `finch-providers`.
pub struct ModelFeature { … }
/// Grant allowing a provider-native tool to be advertised. Re-exported from `finch-providers`.
pub struct NativeToolGrant { … }
/// OpenAI API provider  Supports both OpenAI and Grok APIs (they use the same format). Re-exported from `finch-providers`.
pub struct OpenAIProvider { … }
/// Strict OpenAI-specific dialect; reusable OAuth state remains in `oauth`. Re-exported from `finch-providers`.
pub struct OpenAiChatGptOAuthDialect<V> { … }
/// Bounded, single-flight verifier for the exact pinned OpenAI issuer. Re-exported from `finch-providers`.
pub struct OpenAiJwksVerifier { … }
/// Statistics about context optimization Re-exported from `finch-providers`.
pub struct OptimizationStats { … }
/// Maximum completion tokens accepted by the exact model/adapter pair. Re-exported from `finch-providers`.
pub struct OutputTokenLimitCapability { … }
/// Re-exported from `finch-providers`.
pub struct ProviderAllowance { … }
/// Re-exported from `finch-providers`.
pub struct ProviderEndpoints { … }
/// A cloud provider graph constructed exactly once from configuration.
pub struct ProviderGraph { … }
impl ProviderGraph {
    /// Shared primary/fallback provider used by compatibility clients.
    pub fn default_provider(&self) -> Arc<dyn LlmProvider>;
    /// Successfully constructed named profiles in configured fallback order.
    pub fn profiles(&self) -> &[ProviderProfile];
}
/// Production and test runtime handles. Re-exported from `finch-providers`.
pub struct ProviderPorts { … }
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
/// Unified request format for all LLM providers  This wraps the existing Message format and adds provider-agnostic options. Re-exported from `finch-providers`.
pub struct ProviderRequest { … }
/// Unified response format from LLM providers  This wraps the provider-specific response in a common format. Re-exported from `finch-providers`.
pub struct ProviderResponse { … }
/// Re-exported from `finch-providers`.
pub struct ProviderUsage { … }
/// Exact reasoning-effort values accepted by one provider/model adapter. Re-exported from `finch-providers`.
pub struct ReasoningCapability { … }
/// One semantic tool offered for compilation. Re-exported from `finch-providers`.
pub struct SemanticTool { … }
/// Streaming chunk (text delta, reasoning, tool call, or complete block). Re-exported from `finch-providers`.
pub enum StreamChunk { TextDelta, ThinkingDelta, ToolCallDelta, ToolCallComplete, ContentBlockComplete, ResponseMetadata, Usage, Allowance }
/// Configuration for teacher context management Re-exported from `finch-providers`.
pub struct TeacherContextConfig { … }
/// Teacher session with context tracking  Tracks what context has been sent to the teacher provider to enable: - Metrics on new vs repeated context - Optional t… Re-exported from `finch-providers`.
pub struct TeacherSession { … }
/// Authority class carried with a semantic tool. Re-exported from `finch-providers`.
pub enum ToolAuthority { Pure, VmRead, VmWrite, WorkspaceRead, ExternalRead, WorkspaceWrite, ExternalWrite, Destructive, Unclassified }
/// Why compilation or decode failed. Re-exported from `finch-providers`.
pub enum ToolBindingError { DuplicateLocalIdentity, DuplicateWireIdentity, ReservedNameCollision, CaseCollision, TruncationCollision, NameTooLong, InvalidIdentifier, LossySchemaConversion, UnsupportedSchemaFeature, UnknownWireCall, UnknownNamespace, UnknownSemanticIdentity, NativeToolWithoutHandler, NativeToolWithoutGrant, UnknownWireProtocol, TooManyTools }
/// Immutable bijective map from semantic identities to wire identities for one validated request. Re-exported from `finch-providers`.
pub struct ToolBindingTable { … }
/// Optional extras Finch attaches so compilation can record authority and native-tool grants. Re-exported from `finch-providers`.
pub struct ToolCompilePolicy { … }
/// Where a tool identity comes from. Re-exported from `finch-providers`.
pub enum ToolOrigin { Semantic, ProviderNative }
/// A request whose effective provider/model identity and optional capabilities were checked by Finch's non-overridable dispatch boundary. Re-exported from `finch-providers`.
pub struct ValidatedProviderRequest { … }
/// Signature-verified provider claims. Re-exported from `finch-providers`.
pub struct VerifiedOpenAiClaims { … }
/// Request/response protocol used by the provider adapter for this model. Re-exported from `finch-providers`.
pub enum WireProtocol { AnthropicMessages, OpenAiChatCompletions, OpenAiChatGptResponsesLite, GeminiGenerateContent }
/// Known wire protocol and the evidence for that binding. Re-exported from `finch-providers`.
pub struct WireProtocolCapability { … }
```

## Traits

```rust
/// Non-overridable validated dispatch API shared by every provider backend. Re-exported from `finch-providers`.
pub trait LlmProvider: ProviderBackend {
    fn supports_streaming(&self) -> bool;
    fn supports_tools(&self) -> bool;
}
/// Injected JWS/JWKS verification boundary. Re-exported from `finch-providers`.
pub trait OpenAiTokenVerifier: Send + Sync {
    fn preflight(&self) -> Result<()>;
}
/// Provider implementation hooks. Re-exported from `finch-providers`.
pub trait ProviderBackend: ProviderConcreteType + Send + Sync {
    fn name(&self) -> &str;
    fn default_model(&self) -> &str;
    fn capabilities(&self, model: &str) -> ModelCapabilities;
    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort>;
}
/// Non-overridable concrete type identity used by validated dispatch tokens. Re-exported from `finch-providers`.
pub trait ProviderConcreteType: Any {
    fn provider_concrete_type_id(&self) -> TypeId;
}
```

## Functions

```rust
/// Finch-local capability attached to a verified ChatGPT account credential. Re-exported from `finch-providers`.
pub fn chatgpt_required_scopes() -> BTreeSet<String> { … }
/// Compile `ToolDefinition`s plus an optional Finch policy into a table. Re-exported from `finch-providers`.
pub fn compile_from_definitions(protocol: WireProtocol, provider: &str, model: &str, definitions: &[ToolDefinition], policy: &ToolCompilePolicy) -> Result<ToolBindingTable, ToolBindingError> { … }
/// Compile semantic tools into an immutable bijective binding table. Re-exported from `finch-providers`.
pub fn compile_tool_bindings(protocol: WireProtocol, provider: impl Into<String>, model: impl Into<String>, tools: &[SemanticTool]) -> Result<ToolBindingTable, ToolBindingError> { … }
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
/// Re-exported from `finch-providers`.
pub fn default_cache_dir() -> Result<PathBuf> { … }
/// Re-exported from `finch-providers`.
pub fn fallback_catalog(provider: &str, models_url: &str) -> ModelCatalog { … }
/// Validate the complete provider/credential metadata graph and every named transport before secret resolution, provider construction, or selection shortcuts ca…
pub fn preflight_provider_config(config: &Config) -> Result<()> { … }
/// Opaque full-width cache/request identity. Re-exported from `finch-providers`.
pub fn profile_cache_identity(profile: &ModelCatalogProfile) -> String { … }
/// Re-exported from `finch-providers`.
pub fn read_cache(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<Option<ModelCatalog>> { … }
/// Fetch a catalogue and persist only endpoint/model metadata. Re-exported from `finch-providers`.
pub async fn refresh(profile: &ModelCatalogProfile, cache_dir: &Path) -> Result<ModelCatalog> { … }
/// Revalidate and resolve a named provider profile immediately before model discovery.
pub async fn refresh_from_config(config: &Config, profile_name: &str, resolver: &dyn CredentialResolver, cache_dir: &Path) -> Result<ModelCatalog> { … }
/// Use a successful live refresh, then the matching cache, then a visibly labelled static fallback. Re-exported from `finch-providers`.
pub async fn refresh_with_fallback(profile: &ModelCatalogProfile, cache_dir: &Path) -> (ModelCatalog, Option<String>) { … }
/// Re-exported from `finch-providers`.
pub fn static_fallback(provider: &str) -> Vec<String> { … }
/// Inject the alignment prompt into an existing system prompt, or return it standalone. Re-exported from `finch-providers`.
pub fn with_alignment(system: Option<&str>) -> String { … }
```

## Constants

```rust
/// Re-exported from `finch-providers`.
pub const CHATGPT_OAUTH_PROTOCOL_REVISION: &str = "openai-codex-public-client@94cbbddafc1776d5e377bca1b05932c697e82238+finch-binding-v2";
/// Default Claude model used when a transport does not override it. Re-exported from `finch-providers`.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5";
/// Re-exported from `finch-providers`.
pub const OPENAI_PUBLIC_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Re-exported from `finch-providers`.
pub const REQUIRED_TOKEN_ISSUER: &str = "https://auth.openai.com";
/// Date on which Finch's bundled, deliberately incomplete model fallback was reviewed. Re-exported from `finch-providers`.
pub const STATIC_FALLBACK_AS_OF: &str = "2026-08-26";
/// Prompt that normalizes output discipline across all LLM providers. Re-exported from `finch-providers`.
pub const UNIVERSAL_ALIGNMENT_PROMPT: &str = "\ ## Output Discipline These rules override any stylistic defaults: 1. When asked for JSON, return ONLY the JSON. No markdown code fences. No prose before \ or after. The first character of your response must be `[` or `{`. 2. When given a numbered format (1. Step one\n2. Step two), follow it exactly. 3. When given field names or schema, use them verbatim — no renaming, no extras. 4. Do not add unsolicited caveats, disclaimers, or explanations unless the instruction \ explicitly requests them. 5. Treat every instruction as binding, not advisory."; /// Inject the alignment prompt into an existing system prompt, or return it standalone. /// /// The alignment instructions are prepended so they take priority over any other /// stylistic context in the system prompt. pub fn with_alignment(system: Option<&str>) -> String { match system { Some(existing) if !existing.trim().is_empty() => { format!("{}\n\n{}", UNIVERSAL_ALIGNMENT_PROMPT.trim(), existing) } _ => UNIVERSAL_ALIGNMENT_PROMPT.trim().to_string(), } } #[cfg(test)] mod tests { use super::*; #[test] fn test_with_alignment_no_system() { let result = with_alignment(None); assert!(result.contains("Output Discipline")); assert!(result.starts_with("## Output Discipline")); } #[test] fn test_with_alignment_empty_system() { let result = with_alignment(Some("")); // Empty system treated same as None — just the alignment prompt, no extra suffix assert!(result.contains("Output Discipline")); assert_eq!(result, UNIVERSAL_ALIGNMENT_PROMPT.trim()); } #[test] fn test_with_alignment_prepends_to_existing() { let result = with_alignment(Some("Be a helpful assistant.")); assert!(result.starts_with("## Output Discipline")); assert!(result.contains("Be a helpful assistant.")); // Alignment comes first let align_pos = result.find("Output Discipline").unwrap(); let system_pos = result.find("Be a helpful").unwrap(); assert!(align_pos < system_pos); } #[test] fn test_with_alignment_whitespace_only_system() { let result = with_alignment(Some(" \n ")); // Whitespace-only treated same as None assert!(result.starts_with("## Output Discipline")); } #[test] fn test_universal_alignment_prompt_has_json_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("JSON")); assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("code fences")); } #[test] fn test_universal_alignment_prompt_has_numbered_format_rule() { assert!(UNIVERSAL_ALIGNMENT_PROMPT.contains("numbered format")); } }
```
