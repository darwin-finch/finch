// Configuration structs

use super::backend::{BackendConfig, CoreMlConfig};
use super::colors::ColorScheme;
use super::provider::ProviderEntry;
use super::ProviderCredential;
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

    /// Enable streaming responses from teacher models
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

    /// Unified provider list — source of truth for config I/O.
    /// Cloud providers here are also mirrored in `teachers`; local providers
    /// are also mirrored in `backend`. Use `with_providers()` to construct
    /// from this list, or `new()` to construct from the legacy fields.
    pub providers: Vec<ProviderEntry>,

    /// Secret-free named provider credential records. Secret material is
    /// resolved through an injected credential store only after graph validation.
    pub credentials: Vec<ProviderCredential>,

    /// Teacher LLM provider configuration (array of teachers in priority order)
    pub teachers: Vec<TeacherEntry>,

    /// TUI color scheme (customizable for accessibility)
    pub colors: ColorScheme,

    /// Feature flags (optional behaviors)
    pub features: FeaturesConfig,

    /// MCP (Model Context Protocol) server configurations
    pub mcp_servers: HashMap<String, crate::tools::mcp::McpServerConfig>,

    /// Memory system configuration (Phase 4: Hierarchical Memory)
    pub memory: crate::memory::MemoryConfig,

    /// License configuration (Noncommercial by default; Commercial with a valid key)
    pub license: LicenseConfig,
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

/// A single teacher entry with provider and settings
#[derive(Clone, Serialize, Deserialize)]
pub struct TeacherEntry {
    /// Provider name: "claude", "openai", "grok", "gemini", "mistral", "groq"
    pub provider: String,

    /// API key for this provider
    pub api_key: String,

    /// Optional model override (uses provider default if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Optional base URL (for custom endpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Optional name/label for this teacher (for UI/logging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl std::fmt::Debug for TeacherEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TeacherEntry")
            .field("provider", &self.provider)
            .field("api_key", &"[REDACTED]")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("name", &self.name)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Conversion helpers between ProviderEntry and legacy types
// (Defined here to avoid circular imports — TeacherEntry lives in this file)
// ---------------------------------------------------------------------------

impl ProviderEntry {
    /// Convert this cloud provider to a `TeacherEntry` for backward compat.
    /// Returns `None` for `Local` variants.
    pub fn to_teacher_entry(&self) -> Option<TeacherEntry> {
        match self {
            Self::Claude {
                api_key,
                model,
                base_url,
                name,
                ..
            } => Some(TeacherEntry {
                provider: "claude".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: base_url.clone(),
                name: name.clone(),
            }),
            Self::Openai {
                api_key,
                model,
                base_url,
                name,
                ..
            } => Some(TeacherEntry {
                provider: "openai".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: base_url.clone(),
                name: name.clone(),
            }),
            Self::Grok {
                api_key,
                model,
                base_url,
                name,
                ..
            } => Some(TeacherEntry {
                provider: "grok".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: base_url.clone(),
                name: name.clone(),
            }),
            Self::Gemini {
                api_key,
                model,
                name,
            } => Some(TeacherEntry {
                provider: "gemini".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: None,
                name: name.clone(),
            }),
            Self::Mistral {
                api_key,
                model,
                base_url,
                name,
                ..
            } => Some(TeacherEntry {
                provider: "mistral".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: base_url.clone(),
                name: name.clone(),
            }),
            Self::Groq {
                api_key,
                model,
                name,
            } => Some(TeacherEntry {
                provider: "groq".to_string(),
                api_key: api_key.clone(),
                model: model.clone(),
                base_url: None,
                name: name.clone(),
            }),
            Self::Credentialed { .. }
            | Self::LegacyChatgptSubscription { .. }
            | Self::Ollama { .. }
            | Self::RemoteDaemon { .. }
            | Self::Local { .. } => None,
        }
    }

    /// Build a `ProviderEntry` from a `TeacherEntry`.
    pub fn from_teacher_entry(entry: &TeacherEntry) -> Self {
        match entry.provider.to_lowercase().as_str() {
            "claude" => Self::Claude {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                base_url: entry.base_url.clone(),
                chat_path: None,
                models_path: None,
                name: entry.name.clone(),
            },
            "openai" => Self::Openai {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                base_url: entry.base_url.clone(),
                chat_path: None,
                models_path: None,
                name: entry.name.clone(),
                reasoning_effort: None,
            },
            "grok" => Self::Grok {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                base_url: entry.base_url.clone(),
                chat_path: None,
                models_path: None,
                name: entry.name.clone(),
            },
            "gemini" => Self::Gemini {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                name: entry.name.clone(),
            },
            "mistral" => Self::Mistral {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                base_url: entry.base_url.clone(),
                chat_path: None,
                models_path: None,
                name: entry.name.clone(),
            },
            "groq" => Self::Groq {
                api_key: entry.api_key.clone(),
                model: entry.model.clone(),
                name: entry.name.clone(),
            },
            _ => {
                // Unknown provider — treat as Claude (safest fallback)
                Self::Claude {
                    api_key: entry.api_key.clone(),
                    model: entry.model.clone(),
                    base_url: entry.base_url.clone(),
                    chat_path: None,
                    models_path: None,
                    name: entry.name.clone(),
                }
            }
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

impl Config {
    /// Validate configuration and return helpful errors
    pub fn validate(&self) -> anyhow::Result<()> {
        use crate::errors;

        // Validate the complete named-credential graph before any provider,
        // fallback, catalogue, or transport object is constructed.
        let credentials = super::credential::credential_index(&self.credentials)
            .context("Invalid named credential records")?;
        let mut profile_names = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let normalized = provider.profile_name().trim().to_lowercase();
            if !profile_names.insert(normalized) {
                anyhow::bail!("duplicate provider profile name '{}'; profile selectors must be unique across accounts", provider.profile_name());
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
                super::credential::validate_authenticated_endpoints(
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
            super::credential::validate_binding(
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

        // Allow empty teachers — the app can start and will show an error
        // only when an actual API call is attempted (better UX than crashing on startup).

        // Validate each teacher entry
        for (idx, teacher) in self.teachers.iter().enumerate() {
            // Validate provider name
            let valid_providers = ["claude", "openai", "grok", "gemini", "mistral", "groq"];
            if !valid_providers.contains(&teacher.provider.as_str()) {
                anyhow::bail!(errors::wrap_error_with_suggestion(
                    format!(
                        "Invalid provider '{}' in teacher[{}]",
                        teacher.provider, idx
                    ),
                    &format!(
                        "Valid providers: {}\n\n\
                         Update your config:\n  \
                         Edit ~/.finch/config.toml",
                        valid_providers.join(", ")
                    )
                ));
            }

            // Validate API key is not empty
            if teacher.api_key.trim().is_empty() {
                anyhow::bail!(errors::api_key_invalid_error(&teacher.provider));
            }

            // Validate API key format based on provider
            match teacher.provider.as_str() {
                "claude" => {
                    if !teacher.api_key.starts_with("sk-ant-") {
                        anyhow::bail!(errors::wrap_error_with_suggestion(
                            format!("Claude API key has incorrect format (teacher[{}])", idx),
                            "Claude API keys start with 'sk-ant-'\n\n\
                             Get a valid key from:\n  \
                             https://console.anthropic.com/"
                        ));
                    }
                    if teacher.api_key.len() < 20 {
                        anyhow::bail!("Claude API key is too short (should be ~100+ characters)");
                    }
                }
                "openai" | "groq" => {
                    if !teacher.api_key.starts_with("sk-") {
                        anyhow::bail!(errors::wrap_error_with_suggestion(
                            format!(
                                "{} API key has incorrect format (teacher[{}])",
                                teacher.provider, idx
                            ),
                            &format!(
                                "{} API keys start with 'sk-'\n\n\
                                 Get a valid key from:\n  \
                                 https://platform.openai.com/api-keys",
                                teacher.provider.to_uppercase()
                            )
                        ));
                    }
                }
                "gemini" => {
                    if teacher.api_key.len() < 30 {
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

        Ok(())
    }

    pub fn new(teachers: Vec<TeacherEntry>) -> Self {
        // Derive providers from teachers (no local backend by default)
        let providers: Vec<ProviderEntry> = teachers
            .iter()
            .map(ProviderEntry::from_teacher_entry)
            .collect();
        Self::new_with_all(teachers, BackendConfig::default(), providers)
    }

    /// Construct from a unified providers list.
    ///
    /// Automatically derives the legacy `teachers` and `backend` fields so
    /// existing code continues to work without changes.
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
        let teachers: Vec<TeacherEntry> = providers
            .iter()
            .filter_map(ProviderEntry::to_teacher_entry)
            .collect();
        let backend = providers
            .iter()
            .find_map(ProviderEntry::to_backend_config)
            .unwrap_or_else(|| BackendConfig {
                enabled: false,
                ..BackendConfig::default()
            });
        Self::new_with_all_and_paths(
            teachers,
            backend,
            providers,
            paths.metrics_dir,
            paths.constitution_path,
        )
    }

    #[allow(deprecated)]
    fn new_with_all(
        teachers: Vec<TeacherEntry>,
        backend: BackendConfig,
        providers: Vec<ProviderEntry>,
    ) -> Self {
        let paths = resolve_default_config_paths();

        Self::new_with_all_and_paths(
            teachers,
            backend,
            providers,
            paths.metrics_dir,
            paths.constitution_path,
        )
    }

    #[allow(deprecated)]
    fn new_with_all_and_paths(
        teachers: Vec<TeacherEntry>,
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
            teachers,
            providers,
            credentials: Vec::new(),
            features,
            mcp_servers: HashMap::new(),
            memory: crate::memory::MemoryConfig::default(),
            license: LicenseConfig::default(),
        }
    }

    /// Get the active provider (first in the unified providers list).
    pub fn active_provider(&self) -> Option<&ProviderEntry> {
        self.providers.first()
    }

    /// Attach secret-free named credential metadata to this configuration.
    pub fn with_credentials(mut self, credentials: Vec<ProviderCredential>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Profiles that reference a named credential, for dependency-aware UX.
    pub fn credential_dependents(&self, credential_name: &str) -> Vec<String> {
        super::credential::credential_dependencies(
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

    /// Get the active teacher (first cloud provider in priority list).
    ///
    /// Deprecated: prefer `active_provider()` for new code.
    pub fn active_teacher(&self) -> Option<&TeacherEntry> {
        self.teachers.first()
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
    pub(crate) fn save_to(&self, config_path: &std::path::Path) -> anyhow::Result<()> {
        use std::fs;

        self.validate()
            .context("Configuration validation failed before save")?;

        let config_dir = config_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Configuration path has no parent directory"))?;

        // Create directory if it doesn't exist
        fs::create_dir_all(&config_dir)?;

        // Build the providers list — prefer the explicit providers field; fall
        // back to deriving from teachers+backend for configs constructed via
        // the legacy Config::new(teachers) path.
        let providers = if !self.providers.is_empty() {
            self.providers.clone()
        } else {
            // Derive from legacy fields
            let mut p: Vec<ProviderEntry> = self
                .teachers
                .iter()
                .map(ProviderEntry::from_teacher_entry)
                .collect();
            if self.backend.enabled {
                p.push(ProviderEntry::from_backend_config(&self.backend, None));
            }
            p
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
            credentials: self.credentials.clone(),
            coreml: Some(self.backend.coreml),
            colors: Some(self.colors.clone()),
            features: Some(self.features.clone()),
            license: self.license.clone(),
        };

        let toml_string = toml::to_string_pretty(&toml_config)?;
        fs::write(&config_path, toml_string)?;

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
            credentials: Vec::new(),
            coreml: None,
            colors: None,
            features: None,
            license: LicenseConfig::default(),
        })
        .unwrap();
        let decoded: TomlConfig = toml::from_str(&encoded).unwrap();

        assert_eq!(decoded.active_persona.as_deref(), Some("expert-coder"));
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
        use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};

        let directory = tempfile::tempdir().unwrap();
        let first_path = directory.path().join("config.toml");
        let second_path = directory.path().join("reloaded.toml");
        let mut config = Config::with_providers(vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
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
    fn test_legacy_coreml_provider_reloads_with_compatible_default_policy() {
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
    fn test_config_new_has_no_teachers() {
        let config = Config::new(vec![]);
        assert!(config.active_teacher().is_none());
        assert!(config.active_provider().is_none());
    }

    #[test]
    fn test_config_active_teacher_none_when_empty() {
        let config = Config::new(vec![]);
        assert!(config.active_teacher().is_none());
    }

    #[test]
    fn test_with_providers_derives_teachers() {
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
        assert_eq!(config.teachers.len(), 1);
        assert_eq!(config.teachers[0].provider, "claude");
        assert_eq!(config.providers.len(), 1);
        assert!(config.active_provider().is_some());
        assert!(config.active_teacher().is_some());
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
        use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
        let providers = vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        }];
        let config = Config::with_providers(providers);
        assert!(config.teachers.is_empty()); // no cloud providers
        assert!(config.backend.enabled);
        assert_eq!(config.providers.len(), 1);
        assert!(config.active_provider().is_some());
        assert!(config.active_provider().unwrap().is_local());
    }

    #[test]
    fn test_cloud_providers_filters_local() {
        use crate::config::ExecutionTarget;
        use crate::config::ProviderEntry;
        use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
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
                inference_provider: InferenceProvider::Onnx,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                enabled: true,
                name: None,
            },
        ];
        let config = Config::with_providers(providers);
        assert_eq!(config.cloud_providers().len(), 1);
        assert_eq!(config.local_providers().len(), 1);
    }
}
