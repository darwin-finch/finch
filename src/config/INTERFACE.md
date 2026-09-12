# config — public interface

Generated from [`src/config/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/config/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)
- **May depend on:** nothing. Debt: `memory`, `models`, `tools-mcp`.

Everything below is what callers outside this subsystem can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Audience or endpoint-family binding persisted with a credential/profile.
pub struct AudienceBinding { … }
impl AudienceBinding {
    pub fn custom(endpoint: &str) -> Result<Self>;
    pub fn standard(family: EndpointFamily) -> Self;
}
/// Backend configuration for model inference
pub struct BackendConfig { … }
impl BackendConfig {
    /// Legacy alias for effective_target()
    pub fn effective_device(&self) -> ExecutionTarget;
    /// Get the effective execution target (resolve Auto to concrete target)
    pub fn effective_target(&self) -> ExecutionTarget;
    /// Get execution target (for backward compatibility, returns execution_target)
    pub fn get_device(&self) -> ExecutionTarget;
    /// Get the model repository for the selected target and model size  Uses compatibility matrix to resolve repository automatically
    pub fn get_model_repo(&self, _model_size: &str) -> String;
    /// Describe the requested target policy without implying observed placement.
    pub fn requested_target_name(&self) -> String;
    /// Legacy alias for with_target()
    pub fn with_device(target: ExecutionTarget) -> Self;
    /// Create new backend config with model family and size
    pub fn with_model(target: ExecutionTarget, family: ModelFamily, size: ModelSize) -> Self;
    /// Create new backend config with execution target
    pub fn with_target(target: ExecutionTarget) -> Self;
}
/// Legacy alias for compatibility during migration
pub type BackendDevice = ExecutionTarget;
/// Client configuration for connecting to daemon
pub struct ClientConfig { … }
/// Color scheme for TUI elements
pub struct ColorScheme { … }
impl ColorScheme {
    /// Return a subtle, contrast-safe full-row style for a transcript role.
    pub fn message_band_style(&self, band: MessageBand) -> Style;
}
/// Color specification - supports named colors and RGB
pub enum ColorSpec { Named, Rgb }
impl ColorSpec {
    /// Convert to ratatui Color
    pub fn to_color(&self) -> Color;
}
/// Predefined color themes for different terminal backgrounds
pub enum ColorTheme { Dark, Light, HighContrast, Solarized }
impl ColorTheme {
    /// Get all available themes
    pub fn all() -> Vec<Self>;
    /// Get theme description
    pub fn description(&self) -> &str;
    /// Get theme name for display
    pub fn name(&self) -> &str;
    /// Convert theme to color scheme
    pub fn to_scheme(&self) -> ColorScheme;
}
pub struct Config { … }
impl Config {
    /// Get the active provider (first in the unified providers list).
    pub fn active_provider(&self) -> Option<&ProviderEntry>;
    /// Get the active teacher (first cloud provider in priority list).
    pub fn active_teacher(&self) -> Option<&TeacherEntry>;
    /// All cloud providers (excludes Local entries).
    pub fn cloud_providers(&self) -> Vec<&ProviderEntry>;
    /// Profiles that reference a named credential, for dependency-aware UX.
    pub fn credential_dependents(&self, credential_name: &str) -> Vec<String>;
    /// Secret-free named credential records.
    pub fn credentials(&self) -> &[ProviderCredential];
    /// Delete a credential after invalidating every already-constructed provider that shares its authoritative lifecycle signal.
    pub fn delete_credential(&mut self, credential_name: &str) -> anyhow::Result<Vec<String>>;
    /// All local providers (only Local entries).
    pub fn local_providers(&self) -> Vec<&ProviderEntry>;
    pub fn new(teachers: Vec<TeacherEntry>) -> Self;
    /// Revoke a named credential and return every invalidated dependent profile.
    pub fn revoke_credential(&mut self, credential_name: &str) -> anyhow::Result<Vec<String>>;
    /// Save configuration to TOML file at ~/.finch/config.toml
    pub fn save(&self) -> anyhow::Result<()>;
    /// Validate configuration and return helpful errors
    pub fn validate(&self) -> anyhow::Result<()>;
    /// Attach secret-free named credential metadata to this configuration.
    pub fn with_credentials(mut self, mut credentials: Vec<ProviderCredential>) -> Self;
    /// Construct from a unified providers list.
    pub fn with_providers(providers: Vec<ProviderEntry>) -> Self;
}
/// Compute units Finch asks CoreML to consider.
pub enum CoreMlComputeUnits { All, CpuAndNeuralEngine, CpuAndGpu, CpuOnly }
impl CoreMlComputeUnits {
    /// Human-readable requested policy.
    pub fn name(self) -> &'static str;
}
/// CoreML execution-provider options.
pub struct CoreMlConfig { … }
/// Profile-side authentication contract.
pub struct CredentialBinding { … }
/// Authentication mechanism represented by a named credential.
pub enum CredentialKind { ApiKey, Bearer, OauthDevice, OauthBrowserPkce, CloudIdentity, LocalSocket, None }
/// Persisted lifecycle metadata.
pub enum CredentialLifecycle { Active, Revoked, LegacyAmbiguous }
/// Provider/account namespace.
pub enum CredentialProvider { Anthropic, OpenaiPlatform, ChatgptSubscription, Xai, GeminiAiStudio, GoogleVertex, Mistral, Groq }
impl CredentialProvider {
    pub fn as_str(self) -> &'static str;
}
/// Dialog color configuration
pub struct DialogColors { … }
/// Normalized service family.
pub enum EndpointFamily { AnthropicApi, OpenaiPlatform, ChatgptSubscription, XaiApi, GeminiAiStudio, GoogleVertex, MistralApi, GroqApi, Custom }
/// Production resolver for explicit `env:VARIABLE_NAME` opaque references.
pub struct EnvironmentCredentialResolver;
/// Execution target for inference (hardware where code runs)  All targets use ONNX Runtime as the inference provider.
pub enum ExecutionTarget { CoreML, Cuda, Cpu, Auto }
impl ExecutionTarget {
    /// Select best available execution target automatically
    pub fn auto_select() -> ExecutionTarget;
    /// Legacy alias for available_targets()
    pub fn available_devices() -> Vec<ExecutionTarget>;
    /// Get list of available execution targets on this system
    pub fn available_targets() -> Vec<ExecutionTarget>;
    /// Get human-readable description
    pub fn description(&self) -> &'static str;
    /// Check if this execution target is available on the current system  Simplified: assumes platform support = availability ONNX Runtime will handle actual device…
    pub fn is_available(&self) -> bool;
    /// Get short name for logging
    pub fn name(&self) -> &'static str;
}
/// Feature flags configuration
pub struct FeaturesConfig { … }
/// License state persisted in ~/.finch/config.toml `[license]`
pub struct LicenseConfig { … }
/// Whether this installation has a commercial license key
pub enum LicenseType { Noncommercial, Commercial }
pub(crate) struct LifecycleRevocation(Arc<AtomicBool>);
/// Semantic full-row bands used by the transcript renderer.
pub enum MessageBand { LocalUser, Participant, Assistant, ProgramSource, Tool, ProgramOutput }
/// Message display colors
pub struct MessageColors { … }
/// A persona defines how the AI should behave
pub struct Persona { … }
impl Persona {
    /// Get persona focus
    pub fn focus(&self) -> &str;
    /// List available builtin personas
    pub fn list_builtins() -> Vec<&'static str>;
    /// Load persona from TOML file
    pub fn load(path: &Path) -> Result<Self>;
    /// Load built-in persona by name
    pub fn load_builtin(name: &str) -> Result<Self>;
    /// Load persona by name: checks ~/.finch/personas/<name>.toml first, then builtins
    pub fn load_by_name(name: &str) -> Result<Self>;
    /// Get persona name
    pub fn name(&self) -> &str;
    /// Persist a user override for a persona's system prompt.
    pub fn save_system_prompt_override(name: &str, system_prompt: &str) -> Result<PathBuf>;
    /// Get system prompt formatted for injection
    pub fn to_system_message(&self) -> String;
    /// Get persona tone
    pub fn tone(&self) -> &str;
    /// Get persona verbosity
    pub fn verbosity(&self) -> &str;
}
/// Secret-free, named provider credential metadata.
pub struct ProviderCredential { … }
/// A single provider entry — either a cloud API or a local inference backend.
pub enum ProviderEntry { Credentialed, LegacyChatgptSubscription, Claude, Openai, Grok, Gemini, Mistral, Groq, Ollama, RemoteDaemon, Local }
impl ProviderEntry {
    /// API key for cloud variants; `None` for Local, Ollama, and RemoteDaemon.
    pub fn api_key(&self) -> Option<&str>;
    /// Configured base endpoint for credential audience validation.
    pub fn credential_base_url(&self) -> Option<&str>;
    /// Named provider credential binding, if this is a credentialed profile.
    pub fn credential_binding(&self) -> Option<&CredentialBinding>;
    /// Named credential provider namespace, if applicable.
    pub fn credential_provider(&self) -> Option<CredentialProvider>;
    /// Human-readable name for UI display.
    pub fn display_name(&self) -> &str;
    /// Build a `Local` `ProviderEntry` from a `BackendConfig`.
    pub fn from_backend_config(cfg: &BackendConfig, name: Option<String>) -> Self;
    /// Build a `ProviderEntry` from a `TeacherEntry`.
    pub fn from_teacher_entry(entry: &TeacherEntry) -> Self;
    /// True for `Local` variants.
    pub fn is_local(&self) -> bool;
    /// Optional model override (cloud providers only).
    pub fn model(&self) -> Option<&str>;
    /// Stable, user-facing selector for this configured provider profile.
    pub fn profile_name(&self) -> String;
    /// Short provider-type tag (e.g.
    pub fn provider_type(&self) -> &'static str;
    /// Extract a `BackendConfig` from a `Local` variant.
    pub fn to_backend_config(&self) -> Option<BackendConfig>;
    /// Convert this cloud provider to a `TeacherEntry` for backward compat.
    pub fn to_teacher_entry(&self) -> Option<TeacherEntry>;
}
/// Provider-controlled reasoning depth.
pub enum ReasoningEffort { None, Minimal, Low, Medium, High, Xhigh, Max }
impl ReasoningEffort {
    pub fn as_str(self) -> &'static str;
}
/// Immutable, redaction-safe resolved credential handle.
pub struct ResolvedCredential { … }
/// Secret bytes returned by an injected local credential resolver.
pub struct ResolvedSecret(String);
impl ResolvedSecret {
    pub fn new(secret: impl Into<String>) -> Result<Self>;
}
/// Server configuration for daemon mode
pub struct ServerConfig { … }
/// Status bar color configuration
pub struct StatusColors { … }
/// A single teacher entry with provider and settings
pub struct TeacherEntry { … }
/// UI element colors
pub struct UiColors { … }
```

## Traits

```rust
/// Injected secret store boundary.
pub trait CredentialResolver: Send + Sync {
    fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential>;
}
```

## Functions

```rust
/// Decide whether the licence notice is due, recording the decision in the runtime-state file rather than in `config.toml` (#76).
pub fn claim_notice_showing_now(legacy_suppress_until: Option<&str>, today: chrono::NaiveDate) -> bool { … }
/// Validate all credential metadata and return a stable name index.
pub fn credential_index(credentials: &[ProviderCredential]) -> Result<BTreeMap<&str, &ProviderCredential>> { … }
/// Forget any recorded notice suppression, so the next start shows it.
pub fn forget_notice_suppression() { … }
/// Load configuration from Shammah config file or environment
pub fn load_config() -> Result<Config> { … }
pub(crate) fn load_config_from_path(config_path: &std::path::Path) -> Result<Config> { … }
pub(crate) fn load_config_from_path_with_paths(config_path: &std::path::Path, metrics_dir: std::path::PathBuf, constitution_path: Option<std::path::PathBuf>) -> Result<Config> { … }
/// Load the persisted configuration without substituting environment or empty state when an existing file is invalid.
pub fn load_persisted_config() -> Result<Option<Config>> { … }
/// Validate a profile reference against one named credential without resolving secret material or performing external activity.
pub fn validate_binding(provider: CredentialProvider, endpoint: Option<&str>, binding: &CredentialBinding, credential: &ProviderCredential, now: DateTime<Utc>) -> Result<()> { … }
```

## Constants

```rust
pub const DEFAULT_BRAIN_TLS_PORT: u16 = 11436;
/// Default Claude model used when no model is specified in config.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5";
/// Default bind address for the finch daemon.
pub const DEFAULT_DAEMON_ADDR: &str = "127.0.0.1:11435";
/// Default bind address for the HTTP daemon (localhost only).
pub const DEFAULT_HTTP_ADDR: &str = "127.0.0.1:8000";
/// Default maximum tokens for teacher API requests.
pub const DEFAULT_MAX_TOKENS: u32 = 8000;
/// Default bind address for the network worker (all interfaces).
pub const DEFAULT_WORKER_ADDR: &str = "0.0.0.0:8000";
```
