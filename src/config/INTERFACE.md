# config — public interface

Generated from [`src/config/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/config/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)
- **May depend on:** nothing. Debt: `memory`, `models`, `tools`.

Everything below is what callers outside this subsystem can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Audience or endpoint-family binding persisted with a credential/profile.
pub struct AudienceBinding { … }
/// Backend configuration for model inference
pub struct BackendConfig { … }
/// Legacy alias for compatibility during migration
pub type BackendDevice = ExecutionTarget;
/// Client configuration for connecting to daemon
pub struct ClientConfig { … }
/// Color scheme for TUI elements
pub struct ColorScheme { … }
/// Color specification - supports named colors and RGB
pub enum ColorSpec { Named, Rgb }
/// Predefined color themes for different terminal backgrounds
pub enum ColorTheme { Dark, Light, HighContrast, Solarized }
pub struct Config { … }
/// Compute units Finch asks CoreML to consider.
pub enum CoreMlComputeUnits { All, CpuAndNeuralEngine, CpuAndGpu, CpuOnly }
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
/// Dialog color configuration
pub struct DialogColors { … }
/// Normalized service family.
pub enum EndpointFamily { AnthropicApi, OpenaiPlatform, ChatgptSubscription, XaiApi, GeminiAiStudio, GoogleVertex, MistralApi, GroqApi, Custom }
/// Production resolver for explicit `env:VARIABLE_NAME` opaque references.
pub struct EnvironmentCredentialResolver;
/// Execution target for inference (hardware where code runs)  All targets use ONNX Runtime as the inference provider.
pub enum ExecutionTarget { CoreML, Cuda, Cpu, Auto }
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
/// Secret-free, named provider credential metadata.
pub struct ProviderCredential { … }
/// A single provider entry — either a cloud API or a local inference backend.
pub enum ProviderEntry { Credentialed, LegacyChatgptSubscription, Claude, Openai, Grok, Gemini, Mistral, Groq, Ollama, RemoteDaemon, Local }
/// Provider-controlled reasoning depth.
pub enum ReasoningEffort { None, Minimal, Low, Medium, High, Xhigh, Max }
/// Immutable, redaction-safe resolved credential handle.
pub struct ResolvedCredential { … }
/// Secret bytes returned by an injected local credential resolver.
pub struct ResolvedSecret(String);
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
