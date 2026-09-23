// Configuration structs

use super::backend::{BackendConfig, CoreMlConfig};
use super::diagnostics::DiagnosticsConfig;
use super::provider::ProviderEntry;
use super::ProviderCredential;
use crate::theme::ColorScheme;
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Feature flags configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeaturesConfig {
    /// Auto-approve all tools (skip confirmation dialogs)
    /// ⚠️  Use with caution - tools can modify files
    #[serde(default)]
    pub auto_approve_tools: bool,

    /// Enable streaming responses from cloud providers
    #[serde(default = "default_true")]
    pub streaming_enabled: bool,

    /// Enable debug logging for troubleshooting
    #[serde(default)]
    pub debug_logging: bool,

    /// Number of context-summary lines shown in the status strip.
    /// 1 = 🧠 stats only; 2 = stats + "now"; 3 = stats + overall + "now";
    /// 4 = stats + overall + mid + "now"; 5 (default) = same + extra mid; max 8.
    #[serde(default = "default_context_lines")]
    pub memory_context_lines: usize,

    /// Maximum number of recent messages sent verbatim to the provider.
    /// Older messages are accessible via MemTree semantic recall.
    /// Default: 20 (≈10 turns). Set to 0 to disable windowing.
    #[serde(default = "default_max_verbatim_messages")]
    pub max_verbatim_messages: usize,

    /// Number of MemTree results recalled and injected per query.
    /// Default: 2. Keep this low — injecting many memories on every turn pollutes
    /// the context and causes the model to over-rely on past sessions for simple tasks.
    #[serde(default = "default_context_recall_k")]
    pub context_recall_k: usize,

    /// Enable conversation summarization when messages slide off the window.
    /// When enabled, dropped messages are summarised via the active provider
    /// and injected as a `[Summary of earlier context: ...]` user+assistant
    /// prefix so that the LLM retains awareness of earlier context.
    /// Default: false (uses MemTree recall instead).
    #[serde(default)]
    pub enable_summarization: bool,

    /// Enable sliding-window context auto-compaction.
    /// Default: false. MemTree recall + summarization are the primary continuity
    /// mechanism; enable this only to also show the CompactionPercent status line.
    #[serde(default)]
    pub auto_compact_enabled: bool,

    /// Finch's explicit consent gate for GUI automation tools (macOS only).
    /// This does not represent or imply the separate macOS Accessibility grant.
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub gui_automation: bool,

    /// Whether Finch has explicitly invoked the native Accessibility prompt.
    /// This is UI history only, not evidence of an operating-system grant.
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub gui_automation_prompted: bool,

    /// Whether a wizard check previously observed Accessibility as available.
    /// Current native state always wins; this only lets the UI explain revocation.
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub gui_automation_last_known_available: bool,

    /// Executable/launcher context associated with the prompt/grant history.
    /// This is not a TCC identity and is used only to avoid cross-context claims.
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub gui_automation_permission_context: String,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            auto_approve_tools: false,
            streaming_enabled: true,
            debug_logging: false,
            memory_context_lines: 5,
            max_verbatim_messages: 20,
            context_recall_k: 2,
            enable_summarization: false,
            auto_compact_enabled: false,
            #[cfg(target_os = "macos")]
            gui_automation: false,
            #[cfg(target_os = "macos")]
            gui_automation_prompted: false,
            #[cfg(target_os = "macos")]
            gui_automation_last_known_available: false,
            #[cfg(target_os = "macos")]
            gui_automation_permission_context: String::new(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_context_lines() -> usize {
    5
}

fn default_max_verbatim_messages() -> usize {
    20
}

fn default_context_recall_k() -> usize {
    2
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Directory for metrics storage
    pub metrics_dir: PathBuf,

    /// Enable streaming responses (default: true)
    /// DEPRECATED: Use features.streaming_enabled instead
    #[deprecated(note = "Use features.streaming_enabled instead")]
    pub streaming_enabled: bool,

    /// Enable TUI (Ratatui-based interface) (default: true)
    pub tui_enabled: bool,

    /// Path to constitutional guidelines for local LLM (optional)
    /// Only used for local inference, NOT sent to Claude API
    pub constitution_path: Option<PathBuf>,

    /// Active persona name (e.g., "default", "expert-coder", "louis")
    pub active_persona: String,

    /// Active color theme (e.g., "dark", "light", "high-contrast", "solarized")
    pub active_theme: String,

    /// HuggingFace API token for model downloads (optional)
    pub huggingface_token: Option<String>,

    /// Backend configuration (device selection, model paths)
    pub backend: BackendConfig,

    /// Server configuration (daemon mode)
    pub server: ServerConfig,

    /// Client configuration (connecting to daemon)
    pub client: ClientConfig,

    /// Unified provider list — the sole source of truth for config I/O.
    /// Local providers are also mirrored in `backend`. Use `with_providers()`
    /// to construct from this list.
    pub providers: Vec<ProviderEntry>,

    /// Explicit global default provider profile name. New Brains inherit this
    /// once; changing it does not rewrite existing Brain overlays.
    pub default_provider: Option<String>,

    /// Secret-free named provider credential records. Secret material is
    /// resolved through an injected credential store only after graph validation.
    pub(crate) credentials: Vec<ProviderCredential>,

    /// TUI color scheme (customizable for accessibility)
    pub colors: ColorScheme,

    /// Feature flags (optional behaviors)
    pub features: FeaturesConfig,

    /// MCP (Model Context Protocol) server configurations
    pub mcp_servers: HashMap<String, crate::tools::McpServerConfig>,

    /// Memory system configuration (Phase 4: Hierarchical Memory)
    pub memory: finch_memory::MemoryConfig,

    /// License configuration (Noncommercial by default; Commercial with a valid key)
    pub license: LicenseConfig,

    /// Declared post-edit diagnostics sources (issue #757). Empty by default:
    /// no source is declared, nothing is inferred, and edit results are
    /// unchanged.
    pub diagnostics: DiagnosticsConfig,
}

/// Server configuration for daemon mode
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Enable daemon mode
    pub enabled: bool,
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// TLS-only remote Brain listener. It is opened only when service
    /// advertisement (and therefore LAN collaboration) is enabled.
    pub brain_bind_address: String,
    /// Enable API key authentication
    pub auth_enabled: bool,
    /// Valid API keys for authentication
    pub api_keys: Vec<String>,
    /// Operating mode: "full" (daemon + REPL) or "daemon-only" (no REPL)
    pub mode: String,
    /// Enable mDNS/Bonjour advertisement for service discovery
    pub advertise: bool,
    /// Service name for advertisement (defaults to "finch-{hostname}")
    pub service_name: String,
    /// Service description
    pub service_description: String,
    /// Password required to attach to or mutate a brain from another machine.
    /// Setup generates this value once and persists it in config.toml.
    pub brain_password: String,
}

/// Client configuration for connecting to daemon
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Use daemon client mode instead of loading model locally
    pub use_daemon: bool,
    /// Daemon bind address to connect to
    pub daemon_address: String,
    /// Auto-spawn daemon if not running
    pub auto_spawn: bool,
    /// Request timeout in seconds
    pub timeout_seconds: u64,
    /// Enable mDNS/Bonjour service discovery for remote daemons
    pub auto_discover: bool,
    /// Prefer local daemon over remote
    pub prefer_local: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_address: crate::config::constants::DEFAULT_HTTP_ADDR.to_string(),
            brain_bind_address: crate::config::constants::DEFAULT_BRAIN_TLS_ADDR.to_string(),
            auth_enabled: false,
            api_keys: vec![],
            mode: "full".to_string(), // "full" (daemon + REPL) or "daemon-only"
            advertise: false,         // Disabled by default
            service_name: String::new(), // Empty = auto-generate from hostname
            service_description: "Finch AI Assistant".to_string(),
            brain_password: default_brain_password(),
        }
    }
}

fn default_brain_password() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..20].to_string()
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            use_daemon: true, // Enabled by default (daemon-only mode)
            daemon_address: crate::config::constants::DEFAULT_DAEMON_ADDR.to_string(),
            auto_spawn: true,
            timeout_seconds: 120,
            auto_discover: false, // Disabled by default (use explicit daemon_address)
            prefer_local: true,   // Try local daemon first before discovering remote
        }
    }
}

// ---------------------------------------------------------------------------
// License configuration
// ---------------------------------------------------------------------------

/// Whether this installation has a commercial license key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum LicenseType {
    #[default]
    Noncommercial,
    Commercial,
}

/// License state persisted in ~/.finch/config.toml `[license]`
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LicenseConfig {
    /// Raw commercial license key (FINCH-…)
    #[serde(default)]
    pub key: Option<String>,
    /// Derived license type — set after successful key validation
    #[serde(default)]
    pub license_type: LicenseType,
    /// ISO 8601 date when key was last validated
    #[serde(default)]
    pub verified_at: Option<String>,
    /// ISO 8601 expiry date from key payload
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Name from key payload (display only)
    #[serde(default)]
    pub licensee_name: Option<String>,
    /// Suppress startup notice until this ISO 8601 date
    #[serde(default)]
    pub notice_suppress_until: Option<String>,
}

impl ProviderEntry {
    /// The `(provider-name, api-key)` projection used by startup validation.
    /// Returns `None` for variants outside the simple API-key cloud set
    /// (`Credentialed`, `LegacyChatgptSubscription`, `Ollama`, `RemoteDaemon`,
    /// `Local`).
    ///
    /// This is validation input only — configuration I/O is `[[providers]]`.
    pub(crate) fn simple_cloud_key(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Claude { api_key, .. } => Some(("claude", api_key)),
            Self::Openai { api_key, .. } => Some(("openai", api_key)),
            Self::Grok { api_key, .. } => Some(("grok", api_key)),
            Self::Gemini { api_key, .. } => Some(("gemini", api_key)),
            Self::Mistral { api_key, .. } => Some(("mistral", api_key)),
            Self::Groq { api_key, .. } => Some(("groq", api_key)),
            Self::Openrouter { api_key, .. } => Some(("openrouter", api_key)),
            Self::Credentialed { .. }
            | Self::LegacyChatgptSubscription { .. }
            | Self::Ollama { .. }
            | Self::RemoteDaemon { .. }
            | Self::Local { .. } => None,
        }
    }

    /// Whether this entry is a simple API-key cloud provider — the set the
    /// removed legacy shadow carried. Local and credential-bound providers
    /// are excluded.
    pub(crate) fn is_simple_cloud(&self) -> bool {
        self.simple_cloud_key().is_some()
    }

    /// Map a provider-family name (for example `"claude"`, `"openai"`) plus
    /// raw fields onto a cloud `ProviderEntry`.
    ///
    /// This is the constructor the removed legacy `[[teachers]]` config rows
    /// and environment-driven setup use. Unknown provider names map to
    /// Claude — the same safest-fallback rule the removed conversion used,
    /// so old files keep loading identically.
    pub fn from_provider_fields(
        provider: &str,
        api_key: String,
        model: Option<String>,
        base_url: Option<String>,
        name: Option<String>,
    ) -> Self {
        match provider.to_lowercase().as_str() {
            "openai" => Self::Openai {
                api_key,
                model,
                base_url,
                chat_path: None,
                models_path: None,
                name,
                reasoning_effort: None,
            },
            "grok" => Self::Grok {
                api_key,
                model,
                base_url,
                chat_path: None,
                models_path: None,
                name,
            },
            "gemini" => Self::Gemini {
                api_key,
                model,
                name,
            },
            "mistral" => Self::Mistral {
                api_key,
                model,
                base_url,
                chat_path: None,
                models_path: None,
                name,
            },
            "groq" => Self::Groq {
                api_key,
                model,
                name,
            },
            "openrouter" => Self::Openrouter {
                api_key,
                model,
                base_url,
                chat_path: None,
                models_path: None,
                name,
            },
            // "claude" and any unknown provider — Claude is the safest fallback.
            _ => Self::Claude {
                api_key,
                model,
                base_url,
                chat_path: None,
                models_path: None,
                name,
            },
        }
    }

    /// Extract a `BackendConfig` from a `Local` variant. Returns `None` for
    /// cloud variants.
    pub fn to_backend_config(&self) -> Option<BackendConfig> {
        if let Self::Local {
            inference_provider,
            execution_target,
            model_family,
            model_size,
            model_repo,
            model_path,
            managed_artifact,
            enabled,
            ..
        } = self
        {
            Some(BackendConfig {
                enabled: *enabled,
                inference_provider: *inference_provider,
                execution_target: *execution_target,
                coreml: CoreMlConfig::default(),
                model_family: *model_family,
                model_size: *model_size,
                model_repo: model_repo.clone(),
                model_path: model_path.clone(),
                managed_artifact: managed_artifact.clone(),
                fallback_chain: BackendConfig::default().fallback_chain,
                #[allow(deprecated)]
                device: None,
            })
        } else {
            None
        }
    }

    /// Build a `Local` `ProviderEntry` from a `BackendConfig`.
    pub fn from_backend_config(cfg: &BackendConfig, name: Option<String>) -> Self {
        Self::Local {
            inference_provider: cfg.inference_provider,
            execution_target: cfg.execution_target,
            model_family: cfg.model_family,
            model_size: cfg.model_size,
            model_repo: cfg.model_repo.clone(),
            model_path: cfg.model_path.clone(),
            managed_artifact: cfg.managed_artifact.clone(),
            enabled: cfg.enabled,
            name,
        }
    }
}

pub(crate) struct ConfigPaths {
    metrics_dir: PathBuf,
    constitution_path: Option<PathBuf>,
}

fn resolve_default_config_paths() -> ConfigPaths {
    let home = dirs::home_dir().expect("Could not determine home directory");
    let constitution_path = home.join(".finch/constitution.md");
    let constitution_path = constitution_path.exists().then_some(constitution_path);
    ConfigPaths {
        metrics_dir: home.join(".finch/metrics"),
        constitution_path,
    }
}

fn credential_metadata_eq(left: &ProviderCredential, right: &ProviderCredential) -> bool {
    left.name == right.name
        && left.kind == right.kind
        && left.provider == right.provider
        && left.issuer == right.issuer
        && left.audience == right.audience
        && left.tenant == right.tenant
        && left.project == right.project
        && left.account == right.account
        && left.scopes == right.scopes
        && left.secret_ref == right.secret_ref
        && left.lifecycle == right.lifecycle
}

impl Config {
    /// Validate configuration and return helpful errors
    pub fn validate(&self) -> anyhow::Result<()> {
        use crate::errors;

        // Validate the complete named-credential graph before any provider,
        // fallback, catalogue, or transport object is constructed.
        let credentials = super::credential_index(&self.credentials)
            .context("Invalid named credential records")?;
        let mut profile_names = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let normalized = provider.profile_name().trim().to_lowercase();
            if !profile_names.insert(normalized) {
                anyhow::bail!("duplicate provider profile name '{}'; profile selectors must be unique across accounts", provider.profile_name());
            }
        }
        if let Some(name) = self.default_provider.as_deref() {
            if !self
                .providers
                .iter()
                .any(|entry| entry.profile_name() == name)
            {
                anyhow::bail!(
                    "default_provider '{name}' is not a configured [[providers]] profile name"
                );
            }
        }
        for provider in &self.providers {
            let Some(binding) = provider.credential_binding() else {
                continue;
            };
            let credential = credentials.get(binding.credential_ref.as_str()).ok_or_else(|| {
                anyhow::anyhow!(
                    "provider profile '{}' references missing credential '{}'; run `finch setup` to choose an existing named credential",
                    provider.profile_name(),
                    binding.credential_ref
                )
            })?;
            if let ProviderEntry::Credentialed {
                provider: credential_provider,
                base_url,
                chat_path,
                models_path,
                ..
            } = provider
            {
                super::validate_authenticated_endpoints(
                    *credential_provider,
                    base_url.as_deref(),
                    &[chat_path.as_deref(), models_path.as_deref()],
                )
                .with_context(|| {
                    format!(
                        "provider profile '{}' has unsafe endpoint override",
                        provider.profile_name()
                    )
                })?;
            }
            super::validate_binding(
                provider
                    .credential_provider()
                    .expect("credentialed profiles declare provider namespace"),
                provider.credential_base_url(),
                binding,
                credential,
                chrono::Utc::now(),
            )
            .with_context(|| {
                format!(
                    "provider profile '{}' has incompatible credential '{}'",
                    provider.profile_name(),
                    binding.credential_ref
                )
            })?;
        }

        // Allow empty cloud providers — the app can start and will show an error
        // only when an actual API call is attempted (better UX than crashing on startup).

        // Validate each simple cloud provider entry (same accept/reject rules
        // as the removed legacy shadow, including the exact name set and
        // per-provider key format checks; entries the legacy shadow never
        // carried are skipped).
        for (idx, (provider_name, api_key)) in self
            .providers
            .iter()
            .filter_map(ProviderEntry::simple_cloud_key)
            .enumerate()
        {
            // Validate provider name
            let valid_providers = ["claude", "openai", "grok", "gemini", "mistral", "groq"];
            if !valid_providers.contains(&provider_name) {
                anyhow::bail!(errors::wrap_error_with_suggestion(
                    format!("Invalid provider '{}' in provider[{}]", provider_name, idx),
                    &format!(
                        "Valid providers: {}\n\n\
                         Update your config:\n  \
                         Edit ~/.finch/config.toml",
                        valid_providers.join(", ")
                    )
                ));
            }

            // Validate API key is not empty
            if api_key.trim().is_empty() {
                anyhow::bail!(errors::api_key_invalid_error(provider_name));
            }

            // Validate API key format based on provider
            match provider_name {
                "claude" => {
                    if !api_key.starts_with("sk-ant-") {
                        anyhow::bail!(errors::wrap_error_with_suggestion(
                            format!("Claude API key has incorrect format (provider[{}])", idx),
                            "Claude API keys start with 'sk-ant-'\n\n\
                             Get a valid key from:\n  \
                             https://console.anthropic.com/"
                        ));
                    }
                    if api_key.len() < 20 {
                        anyhow::bail!("Claude API key is too short (should be ~100+ characters)");
                    }
                }
                "openai" | "groq" => {
                    if !api_key.starts_with("sk-") {
                        anyhow::bail!(errors::wrap_error_with_suggestion(
                            format!(
                                "{} API key has incorrect format (provider[{}])",
                                provider_name, idx
                            ),
                            &format!(
                                "{} API keys start with 'sk-'\n\n\
                                 Get a valid key from:\n  \
                                 https://platform.openai.com/api-keys",
                                provider_name.to_uppercase()
                            )
                        ));
                    }
                }
                "gemini" => {
                    if api_key.len() < 30 {
                        anyhow::bail!("Gemini API key is too short");
                    }
                }
                _ => {} // Other providers - basic validation passed
            }
        }

        // Validate bind address format
        if !self.server.bind_address.contains(':') {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!("Invalid bind address: '{}'", self.server.bind_address),
                "Bind address should be in format 'IP:PORT'\n\
                 Examples:\n  \
                 • 127.0.0.1:8000\n  \
                 • 0.0.0.0:11435\n  \
                 • localhost:8080"
            ));
        }

        if self.server.advertise && !self.server.brain_bind_address.contains(':') {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!(
                    "Invalid Brain TLS bind address: '{}'",
                    self.server.brain_bind_address
                ),
                "Brain TLS bind address should be in IP:PORT form\n\
                 Example: 0.0.0.0:11436"
            ));
        }

        if !self.client.daemon_address.contains(':') {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!("Invalid daemon address: '{}'", self.client.daemon_address),
                "Daemon address should be in format 'IP:PORT'\n\
                 Example: 127.0.0.1:11435"
            ));
        }

        if self.server.auth_enabled
            && self
                .server
                .api_keys
                .iter()
                .filter(|key| !key.trim().is_empty())
                .count()
                != 1
        {
            anyhow::bail!("server authentication requires exactly one non-empty Finch API key");
        }

        if self.client.timeout_seconds == 0 {
            anyhow::bail!("timeout_seconds must be greater than 0");
        }

        if self.client.timeout_seconds > 3600 {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!(
                    "timeout_seconds ({}) is very high",
                    self.client.timeout_seconds
                ),
                "Recommended range: 30-600 seconds\n\
                 High values may cause requests to hang"
            ));
        }

        // Validate paths exist if specified
        if let Some(ref path) = self.constitution_path {
            if !path.exists() {
                anyhow::bail!(errors::file_not_found_error(
                    &path.display().to_string(),
                    "Constitution file"
                ));
            }
        }

        // Declared post-edit diagnostics sources (issue #757): fail closed on
        // declarations this build must not mis-execute.
        self.diagnostics
            .validate()
            .context("Invalid [diagnostics] configuration")?;

        Ok(())
    }

    /// Construct from a unified providers list.
    pub fn new(providers: Vec<ProviderEntry>) -> Self {
        Self::with_providers(providers)
    }

    /// Construct from a unified providers list.
    ///
    /// Automatically derives the legacy `backend` field so existing code
    /// continues to work without changes.
    pub fn with_providers(providers: Vec<ProviderEntry>) -> Self {
        Self::with_providers_from_paths_or_else(providers, None, resolve_default_config_paths)
    }

    /// Construct from providers with every filesystem path supplied explicitly.
    ///
    /// This internal constructor avoids probing the user's home directory and is
    /// intended for callers, such as hermetic tests, that own their path roots.
    pub(crate) fn with_providers_and_paths(
        providers: Vec<ProviderEntry>,
        metrics_dir: PathBuf,
        constitution_path: Option<PathBuf>,
    ) -> Self {
        Self::with_providers_and_paths_using_resolver(
            providers,
            metrics_dir,
            constitution_path,
            resolve_default_config_paths,
        )
    }

    pub(crate) fn with_providers_and_paths_using_resolver<F>(
        providers: Vec<ProviderEntry>,
        metrics_dir: PathBuf,
        constitution_path: Option<PathBuf>,
        resolve_default_paths: F,
    ) -> Self
    where
        F: FnOnce() -> ConfigPaths,
    {
        Self::with_providers_from_paths_or_else(
            providers,
            Some(ConfigPaths {
                metrics_dir,
                constitution_path,
            }),
            resolve_default_paths,
        )
    }

    fn with_providers_from_paths_or_else<F>(
        providers: Vec<ProviderEntry>,
        paths: Option<ConfigPaths>,
        resolve_default_paths: F,
    ) -> Self
    where
        F: FnOnce() -> ConfigPaths,
    {
        let paths = paths.unwrap_or_else(resolve_default_paths);
        let backend = providers
            .iter()
            .find_map(ProviderEntry::to_backend_config)
            .unwrap_or_else(|| BackendConfig {
                enabled: false,
                ..BackendConfig::default()
            });
        Self::new_with_all_and_paths(
            backend,
            providers,
            paths.metrics_dir,
            paths.constitution_path,
        )
    }

    fn new_with_all_and_paths(
        backend: BackendConfig,
        providers: Vec<ProviderEntry>,
        metrics_dir: PathBuf,
        constitution_path: Option<PathBuf>,
    ) -> Self {
        let features = FeaturesConfig::default();

        Self {
            metrics_dir,
            streaming_enabled: features.streaming_enabled,
            tui_enabled: true,
            constitution_path,
            active_persona: "default".to_string(),
            active_theme: "dark".to_string(),
            huggingface_token: None,
            backend,
            server: ServerConfig::default(),
            client: ClientConfig::default(),
            colors: ColorScheme::default(),
            providers,
            default_provider: None,
            credentials: Vec::new(),
            features,
            mcp_servers: HashMap::new(),
            memory: finch_memory::MemoryConfig::default(),
            license: LicenseConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
        }
    }

    /// Get the active provider (named global default, else first in the list).
    pub fn active_provider(&self) -> Option<&ProviderEntry> {
        if let Some(name) = self.default_provider.as_deref() {
            if let Some(entry) = self
                .providers
                .iter()
                .find(|entry| entry.profile_name() == name)
            {
                return Some(entry);
            }
        }
        self.providers.first()
    }

    /// Profile name new Brains inherit when they have no overlay yet.
    pub fn default_provider_name(&self) -> Option<String> {
        self.default_provider
            .clone()
            .or_else(|| self.active_provider().map(|entry| entry.profile_name()))
    }

    /// Attach secret-free named credential metadata to this configuration.
    pub fn with_credentials(mut self, mut credentials: Vec<ProviderCredential>) -> Self {
        self.replace_credentials_preserving_authority(&mut credentials);
        self
    }

    /// Secret-free named credential records. Mutations must use the lifecycle
    /// methods below so live providers receive invalidation signals.
    pub fn credentials(&self) -> &[ProviderCredential] {
        &self.credentials
    }

    pub(crate) fn replace_loaded_credentials(&mut self, mut credentials: Vec<ProviderCredential>) {
        self.replace_credentials_preserving_authority(&mut credentials);
    }

    fn replace_credentials_preserving_authority(
        &mut self,
        credentials: &mut Vec<ProviderCredential>,
    ) {
        for old in &self.credentials {
            match credentials.iter_mut().find(|new| new.name == old.name) {
                Some(new) if credential_metadata_eq(old, new) => {
                    new.revocation = old.revocation.clone();
                }
                Some(_) | None => old.revocation.revoke(),
            }
        }
        self.credentials = std::mem::take(credentials);
    }

    /// Profiles that reference a named credential, for dependency-aware UX.
    pub fn credential_dependents(&self, credential_name: &str) -> Vec<String> {
        super::credential_dependencies(
            credential_name,
            self.providers
                .iter()
                .map(|profile| (profile.profile_name(), profile.credential_binding())),
        )
    }

    /// Revoke a named credential and return every invalidated dependent profile.
    pub fn revoke_credential(&mut self, credential_name: &str) -> anyhow::Result<Vec<String>> {
        let dependents = self.credential_dependents(credential_name);
        let credential = self
            .credentials
            .iter_mut()
            .find(|credential| credential.name == credential_name)
            .ok_or_else(|| anyhow::anyhow!("credential '{}' was not found", credential_name))?;
        credential.revocation.revoke();
        credential.lifecycle = super::CredentialLifecycle::Revoked;
        Ok(dependents)
    }

    /// Delete a credential after invalidating every already-constructed
    /// provider that shares its authoritative lifecycle signal.
    pub fn delete_credential(&mut self, credential_name: &str) -> anyhow::Result<Vec<String>> {
        let dependents = self.credential_dependents(credential_name);
        let index = self
            .credentials
            .iter()
            .position(|credential| credential.name == credential_name)
            .ok_or_else(|| anyhow::anyhow!("credential '{}' was not found", credential_name))?;
        self.credentials[index].revocation.revoke();
        self.credentials.remove(index);
        Ok(dependents)
    }

    /// All cloud providers (excludes Local entries).
    pub fn cloud_providers(&self) -> Vec<&ProviderEntry> {
        self.providers.iter().filter(|p| !p.is_local()).collect()
    }

    /// All local providers (only Local entries).
    pub fn local_providers(&self) -> Vec<&ProviderEntry> {
        self.providers.iter().filter(|p| p.is_local()).collect()
    }

    /// Save configuration to TOML file at ~/.finch/config.toml
    pub fn save(&self) -> anyhow::Result<()> {
        let home = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let config_dir = home.join(".finch");
        let config_path = config_dir.join("config.toml");

        self.save_to(&config_path)
    }

    /// Save configuration to an explicit path owned by the caller.
    ///
    /// The write is atomic — private temporary beside the target, fsync,
    /// rename — so a save that fails partway leaves the previous configuration
    /// intact. Only intentional changes reach this path; ordinary startup
    /// saves nothing (#76).
    pub(crate) fn save_to(&self, config_path: &std::path::Path) -> anyhow::Result<()> {
        self.validate()
            .context("Configuration validation failed before save")?;

        // Build the providers list — prefer the explicit providers field; fall
        // back to deriving the local backend entry for configs whose provider
        // list was not populated.
        let providers = if !self.providers.is_empty() {
            self.providers.clone()
        } else if self.backend.enabled {
            vec![ProviderEntry::from_backend_config(&self.backend, None)]
        } else {
            Vec::new()
        };

        // Create serializable config (new [[providers]] format)
        let toml_config = TomlConfig {
            streaming_enabled: self.features.streaming_enabled,
            tui_enabled: self.tui_enabled,
            active_theme: Some(self.active_theme.clone()),
            active_persona: Some(self.active_persona.clone()),
            huggingface_token: self.huggingface_token.clone(),
            client: Some(self.client.clone()),
            server: Some(self.server.clone()),
            providers,
            default_provider: self.default_provider.clone(),
            credentials: self.credentials.clone(),
            coreml: Some(self.backend.coreml),
            colors: Some(self.colors.clone()),
            features: Some(self.features.clone()),
            license: self.license.clone(),
            diagnostics: Some(self.diagnostics.clone()),
        };

        let toml_string = toml::to_string_pretty(&toml_config)?;
        super::atomic_write::atomic_write(config_path, toml_string.as_bytes())?;

        tracing::info!("Configuration saved to {:?}", config_path);
        Ok(())
    }
}

/// TOML-serializable config (new [[providers]] format).
#[derive(Serialize, Deserialize)]
struct TomlConfig {
    streaming_enabled: bool,
    tui_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_persona: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    huggingface_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<ClientConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<ServerConfig>,
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    credentials: Vec<ProviderCredential>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    coreml: Option<CoreMlConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colors: Option<ColorScheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    features: Option<FeaturesConfig>,
    #[serde(default)]
    license: LicenseConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    diagnostics: Option<super::DiagnosticsConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_persists_single_finch_api_key() {
        let mut server = ServerConfig::default();
        server.auth_enabled = true;
        server.api_keys = vec!["custom-secret".to_string()];

        let encoded = toml::to_string(&server).unwrap();
        let decoded: ServerConfig = toml::from_str(&encoded).unwrap();

        assert!(decoded.auth_enabled);
        assert_eq!(decoded.api_keys, vec!["custom-secret"]);
    }

    #[test]
    fn test_serialized_config_persists_active_persona() {
        let encoded = toml::to_string(&TomlConfig {
            streaming_enabled: true,
            tui_enabled: true,
            active_theme: Some("dark".to_string()),
            active_persona: Some("expert-coder".to_string()),
            huggingface_token: None,
            client: None,
            server: None,
            providers: Vec::new(),
            default_provider: None,
            credentials: Vec::new(),
            coreml: None,
            colors: None,
            features: None,
            license: LicenseConfig::default(),
            diagnostics: None,
        })
        .unwrap();
        let decoded: TomlConfig = toml::from_str(&encoded).unwrap();

        assert_eq!(decoded.active_persona.as_deref(), Some("expert-coder"));
    }

    #[test]
    fn test_serialized_config_persists_declared_diagnostics_sources() {
        let mut config = Config::new(vec![]);
        config.diagnostics = super::super::DiagnosticsConfig {
            check: vec![super::super::CheckCommandSource {
                extensions: vec!["rs".to_string()],
                command: "cargo check --message-format=json".to_string(),
            }],
            timeout_secs: 42,
            max_output_chars: 512,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        config.save_to(&path).unwrap();

        let round = toml::from_str::<TomlConfig>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let saved = round.diagnostics.expect("declared sources must persist");
        assert_eq!(
            saved, config.diagnostics,
            "a saved [diagnostics] table must round-trip declared sources and bounds"
        );
    }

    #[test]
    fn test_server_config_persists_brain_password() {
        let mut server = ServerConfig::default();
        server.brain_password = "correct horse battery staple".to_string();
        let encoded = toml::to_string(&server).unwrap();
        let decoded: ServerConfig = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded.brain_password, "correct horse battery staple");
    }

    #[test]
    fn test_server_config_generates_a_nonempty_brain_password() {
        let server = ServerConfig::default();
        assert!(server.brain_password.len() >= 16);
    }

    #[test]
    fn test_coreml_policy_persistence_round_trip_uses_isolated_path() {
        use crate::config::{CoreMlComputeUnits, ExecutionTarget, ProviderEntry};
        use crate::models::{InferenceProvider, ModelFamily, ModelSize};

        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("config.toml");
        let second_path = directory.path().join("reloaded.toml");
        let mut config = Config::with_providers(vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        }]);
        config.backend.coreml = CoreMlConfig {
            compute_units: CoreMlComputeUnits::CpuAndGpu,
            profile_compute_plan: true,
            enable_subgraphs: true,
        };

        config.save_to(&first_path).unwrap();
        let reloaded = crate::config::load_config_from_path(&first_path).unwrap();
        assert_eq!(reloaded.backend.coreml, config.backend.coreml);

        reloaded.save_to(&second_path).unwrap();
        let reloaded_again = crate::config::load_config_from_path(&second_path).unwrap();
        assert_eq!(reloaded_again.backend.coreml, config.backend.coreml);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_legacy_onnx_entry_reloads_only_for_setup_migration() {
        use crate::config::ExecutionTarget;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                [[providers]]
                type = "local"
                inference_provider = "onnx"
                execution_target = "coreml"
                model_family = "Qwen2"
                model_size = "Medium"
                enabled = true
            "#,
        )
        .unwrap();

        let loaded = crate::config::load_config_from_path(&path).unwrap();
        assert_eq!(
            loaded.backend.inference_provider,
            crate::models::InferenceProvider::LegacyOnnx,
            "legacy ONNX identity must survive config load so setup can migrate it"
        );
        assert_eq!(loaded.backend.execution_target, ExecutionTarget::CoreML);
        assert_eq!(loaded.backend.coreml, CoreMlConfig::default());
    }

    #[test]
    fn test_features_config_safe_defaults() {
        let f = FeaturesConfig::default();
        // Safety-critical defaults
        assert!(
            !f.auto_approve_tools,
            "auto_approve_tools must default to false"
        );
        assert!(f.streaming_enabled, "streaming should be on by default");
        assert!(!f.debug_logging, "debug logging should be off by default");
        assert!(
            !f.auto_compact_enabled,
            "auto_compact_enabled must default to false (MemTree + summarization are primary)"
        );
        #[cfg(target_os = "macos")]
        {
            assert!(!f.gui_automation, "gui automation should be off by default");
            assert!(!f.gui_automation_prompted);
            assert!(!f.gui_automation_last_known_available);
            assert!(f.gui_automation_permission_context.is_empty());
        }
    }

    #[test]
    fn test_features_config_serde_roundtrip() {
        let original = FeaturesConfig::default();
        let json = serde_json::to_string(&original).unwrap();
        let decoded: FeaturesConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.auto_approve_tools, original.auto_approve_tools);
        assert_eq!(decoded.streaming_enabled, original.streaming_enabled);
        assert_eq!(decoded.debug_logging, original.debug_logging);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_automation_consent_history_roundtrip() {
        let original = FeaturesConfig {
            gui_automation: true,
            gui_automation_prompted: true,
            gui_automation_last_known_available: true,
            gui_automation_permission_context: "test-context".to_string(),
            ..FeaturesConfig::default()
        };
        let encoded = toml::to_string(&original).unwrap();
        let decoded: FeaturesConfig = toml::from_str(&encoded).unwrap();
        assert!(decoded.gui_automation);
        assert!(decoded.gui_automation_prompted);
        assert!(decoded.gui_automation_last_known_available);
        assert_eq!(decoded.gui_automation_permission_context, "test-context");
    }

    #[test]
    fn test_features_config_streaming_default_from_json_empty() {
        // streaming_enabled has default = "default_true"
        // When key is absent in JSON, it should default to true
        let json = r#"{"auto_approve_tools": false, "debug_logging": false}"#;
        let f: FeaturesConfig = serde_json::from_str(json).unwrap();
        assert!(f.streaming_enabled);
    }

    #[test]
    fn test_config_new_has_no_providers_when_empty() {
        let config = Config::new(vec![]);
        assert!(config.active_provider().is_none());
        assert!(config.cloud_providers().is_empty());
    }

    #[test]
    fn test_with_providers_keeps_cloud_entries_as_sole_truth() {
        use crate::config::ProviderEntry;
        let providers = vec![ProviderEntry::Claude {
            api_key: "sk-ant-test".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("Claude".to_string()),
        }];
        let config = Config::with_providers(providers);
        assert_eq!(config.providers.len(), 1);
        assert_eq!(config.cloud_providers().len(), 1);
        assert!(config.active_provider().is_some());
        assert!(
            !config.backend.enabled,
            "cloud-only provider lists must not start a local model"
        );
    }

    #[test]
    fn test_explicit_config_paths_bypass_default_resolver() {
        use std::cell::Cell;

        let directory = tempfile::tempdir().unwrap();
        let metrics_dir = directory.path().join("metrics-not-created");
        let constitution_path = directory.path().join("constitution-not-created.md");
        let resolver_calls = Cell::new(0);
        assert!(!metrics_dir.exists());
        assert!(!constitution_path.exists());

        let config = Config::with_providers_and_paths_using_resolver(
            vec![],
            metrics_dir.clone(),
            Some(constitution_path.clone()),
            || {
                resolver_calls.set(resolver_calls.get() + 1);
                panic!("explicit config paths must bypass ambient default resolution");
            },
        );

        assert_eq!(resolver_calls.get(), 0);
        assert_eq!(config.metrics_dir, metrics_dir);
        assert_eq!(config.constitution_path, Some(constitution_path));
    }

    #[test]
    fn test_with_providers_derives_backend_from_local() {
        use crate::config::ExecutionTarget;
        use crate::config::ProviderEntry;
        use crate::models::{InferenceProvider, ModelFamily, ModelSize};
        let providers = vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            managed_artifact: None,
            enabled: true,
            name: None,
        }];
        let config = Config::with_providers(providers);
        assert!(config.cloud_providers().is_empty()); // no cloud providers
        assert!(config.backend.enabled);
        assert_eq!(config.providers.len(), 1);
        assert!(config.active_provider().is_some());
        assert!(config.active_provider().unwrap().is_local());
    }

    #[test]
    fn test_cloud_providers_filters_local() {
        use crate::config::ExecutionTarget;
        use crate::config::ProviderEntry;
        use crate::models::{InferenceProvider, ModelFamily, ModelSize};
        let providers = vec![
            ProviderEntry::Grok {
                api_key: "xai-key".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            },
            ProviderEntry::Local {
                inference_provider: InferenceProvider::LlamaCpp,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                managed_artifact: None,
                enabled: true,
                name: None,
            },
        ];
        let config = Config::with_providers(providers);
        assert_eq!(config.cloud_providers().len(), 1);
        assert_eq!(config.local_providers().len(), 1);
    }

    /// A config the tests can actually save: one provider, valid enough to
    /// pass `validate()` on the save path.
    fn savable_config() -> Config {
        Config::with_providers(vec![ProviderEntry::Claude {
            api_key: "sk-ant-test-key-1234567890".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("claude".to_string()),
        }])
    }

    /// An intentional save must be able to replace a file it cannot open for
    /// writing — that is what "atomic" buys.
    ///
    /// A mode the owner can read but not write is exactly the case where a
    /// plain `fs::write` fails (EACCES on open) and a temporary-then-rename
    /// succeeds (the rename needs only the directory), so this test fails
    /// under the pre-#76 direct write and discriminates the two.
    #[cfg(unix)]
    #[test]
    fn test_an_intentional_save_replaces_a_read_only_config() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(&config_path, b"# user's own bytes\n").unwrap();
        let mut perms = std::fs::metadata(&config_path).unwrap().permissions();
        perms.set_mode(0o444);
        std::fs::set_permissions(&config_path, perms).unwrap();

        savable_config()
            .save_to(&config_path)
            .expect("an atomic save replaces a read-only target");

        let saved = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            saved.contains("providers"),
            "the new configuration must be in place; file was:\n{saved}"
        );
        assert_eq!(
            std::fs::metadata(&config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o444,
            "and the save must not silently change the permissions the user had"
        );
    }

    /// A save that cannot complete leaves the previous configuration exactly
    /// as it was — the data-loss half of #76's atomic-write clause.
    #[cfg(unix)]
    #[test]
    fn test_a_failed_intentional_save_preserves_the_previous_config_bytes() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        savable_config().save_to(&config_path).unwrap();
        let previous = std::fs::read(&config_path).unwrap();
        let previous_mtime = std::fs::metadata(&config_path).unwrap().modified().unwrap();

        let mut perms = std::fs::metadata(directory.path()).unwrap().permissions();
        perms.set_mode(0o555);
        std::fs::set_permissions(directory.path(), perms).unwrap();
        let outcome = savable_config().save_to(&config_path);
        let mut perms = std::fs::metadata(directory.path()).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(directory.path(), perms).unwrap();

        assert!(
            outcome.is_err(),
            "a save that cannot create its temporary must report failure"
        );
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            previous,
            "the previous configuration must survive a failed save byte-for-byte"
        );
        assert_eq!(
            std::fs::metadata(&config_path).unwrap().modified().unwrap(),
            previous_mtime,
            "and a failed save must not move its mtime"
        );
    }

    /// Setup must not follow a symlink planted at the config path: the loader
    /// refuses one on read, and the save path refuses to write through one.
    #[cfg(unix)]
    #[test]
    fn test_an_intentional_save_refuses_to_write_through_a_config_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let real = directory.path().join("elsewhere.toml");
        std::fs::write(&real, b"sentinel\n").unwrap();
        let config_path = directory.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &config_path).unwrap();

        let outcome = savable_config().save_to(&config_path);

        assert!(
            outcome.is_err(),
            "a symlinked config must be refused, not written through"
        );
        assert_eq!(
            std::fs::read(&real).unwrap(),
            b"sentinel\n",
            "nothing may be written through the link"
        );
        assert!(
            std::fs::symlink_metadata(&config_path)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must survive the refused save"
        );
    }

    /// No temporary may survive a completed save.
    #[test]
    fn test_a_completed_save_leaves_only_the_config_file_behind() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        savable_config().save_to(&config_path).unwrap();

        let entries: Vec<String> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["config.toml"],
            "a completed save must leave no temporary behind; directory held {entries:?}"
        );
    }

    /// A freshly written config holds API keys, so it must be owner-only.
    #[cfg(unix)]
    #[test]
    fn test_a_freshly_saved_config_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        savable_config().save_to(&config_path).unwrap();

        let mode = std::fs::metadata(&config_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "a fresh config.toml must not be readable by group or other (mode was {mode:o})"
        );
    }
}
