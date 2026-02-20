// Configuration structs

use super::backend::BackendConfig;
use super::colors::ColorScheme;
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

    /// Enable GUI automation tools (macOS only)
    #[cfg(target_os = "macos")]
    #[serde(default)]
    pub gui_automation: bool,
}

impl Default for FeaturesConfig {
    fn default() -> Self {
        Self {
            auto_approve_tools: false, // Safe default: require confirmations
            streaming_enabled: true,   // Enable by default for better UX
            debug_logging: false,       // Disabled by default
            #[cfg(target_os = "macos")]
            gui_automation: false,     // Disabled by default (requires permissions)
        }
    }
}

fn default_true() -> bool {
    true
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
}

/// Server configuration for daemon mode
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Enable daemon mode
    pub enabled: bool,
    /// Bind address (e.g., "127.0.0.1:8000")
    pub bind_address: String,
    /// Maximum number of concurrent sessions
    pub max_sessions: usize,
    /// Session timeout in minutes
    pub session_timeout_minutes: u64,
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
            bind_address: "127.0.0.1:8000".to_string(),
            max_sessions: 100,
            session_timeout_minutes: 30,
            auth_enabled: false,
            api_keys: vec![],
            mode: "full".to_string(),  // "full" (daemon + REPL) or "daemon-only"
            advertise: false,           // Disabled by default
            service_name: String::new(), // Empty = auto-generate from hostname
            service_description: "Finch AI Assistant".to_string(),
        }
    }
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            use_daemon: true, // Enabled by default (daemon-only mode)
            daemon_address: "127.0.0.1:11435".to_string(), // Port 11435 (11434 is used by Ollama)
            auto_spawn: true,
            timeout_seconds: 120,
            auto_discover: false, // Disabled by default (use explicit daemon_address)
            prefer_local: true,   // Try local daemon first before discovering remote
        }
    }
}


/// A single teacher entry with provider and settings
#[derive(Debug, Clone, Serialize, Deserialize)]
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


impl Config {
    /// Validate configuration and return helpful errors
    pub fn validate(&self) -> anyhow::Result<()> {
        use crate::errors;

        // Allow empty teachers — the app can start and will show an error
        // only when an actual API call is attempted (better UX than crashing on startup).

        // Validate each teacher entry
        for (idx, teacher) in self.teachers.iter().enumerate() {
            // Validate provider name
            let valid_providers = ["claude", "openai", "grok", "gemini", "mistral", "groq"];
            if !valid_providers.contains(&teacher.provider.as_str()) {
                anyhow::bail!(errors::wrap_error_with_suggestion(
                    format!("Invalid provider '{}' in teacher[{}]", teacher.provider, idx),
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
                            format!("{} API key has incorrect format (teacher[{}])", teacher.provider, idx),
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

        if !self.client.daemon_address.contains(':') {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!("Invalid daemon address: '{}'", self.client.daemon_address),
                "Daemon address should be in format 'IP:PORT'\n\
                 Example: 127.0.0.1:11435"
            ));
        }

        // Validate numeric ranges
        if self.server.max_sessions == 0 {
            anyhow::bail!("max_sessions must be greater than 0");
        }

        if self.server.max_sessions > 10000 {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!("max_sessions ({}) is unreasonably high", self.server.max_sessions),
                "Recommended range: 1-1000\n\
                 High values may cause memory issues"
            ));
        }

        if self.server.session_timeout_minutes == 0 {
            anyhow::bail!("session_timeout_minutes must be greater than 0");
        }

        if self.client.timeout_seconds == 0 {
            anyhow::bail!("timeout_seconds must be greater than 0");
        }

        if self.client.timeout_seconds > 3600 {
            anyhow::bail!(errors::wrap_error_with_suggestion(
                format!("timeout_seconds ({}) is very high", self.client.timeout_seconds),
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
        let home = dirs::home_dir().expect("Could not determine home directory");
        let _project_dir = std::env::current_dir().expect("Could not determine current directory");

        // Look for constitution in ~/.finch/constitution.md
        let constitution_path = home.join(".finch/constitution.md");
        let constitution_path = if constitution_path.exists() {
            Some(constitution_path)
        } else {
            None
        };

        let features = FeaturesConfig::default();

        Self {
            metrics_dir: home.join(".finch/metrics"),
            streaming_enabled: features.streaming_enabled, // Deprecated, maintained for compat
            tui_enabled: true,       // TUI is the default for interactive terminals
            constitution_path,
            active_persona: "default".to_string(),
            active_theme: "dark".to_string(), // Default to dark theme
            huggingface_token: None, // No HF token by default
            backend: BackendConfig::default(),
            server: ServerConfig::default(),
            client: ClientConfig::default(),
            colors: ColorScheme::default(),
            teachers,
            features,
            mcp_servers: HashMap::new(), // No MCP servers by default
            memory: crate::memory::MemoryConfig::default(), // Phase 4: Hierarchical Memory
        }
    }

    /// Get the active teacher (first in priority list)
    pub fn active_teacher(&self) -> Option<&TeacherEntry> {
        self.teachers.first()
    }

    /// Save configuration to TOML file at ~/.finch/config.toml
    pub fn save(&self) -> anyhow::Result<()> {
        use std::fs;

        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let config_dir = home.join(".finch");
        let config_path = config_dir.join("config.toml");

        // Create directory if it doesn't exist
        fs::create_dir_all(&config_dir)?;

        // Create serializable config
        let toml_config = TomlConfig {
            streaming_enabled: self.features.streaming_enabled, // Use features value
            tui_enabled: self.tui_enabled,
            active_theme: Some(self.active_theme.clone()),
            huggingface_token: self.huggingface_token.clone(),
            backend: self.backend.clone(),
            client: Some(self.client.clone()),
            teachers: self.teachers.clone(),
            colors: Some(self.colors.clone()),
            features: Some(self.features.clone()),
        };

        let toml_string = toml::to_string_pretty(&toml_config)?;
        fs::write(&config_path, toml_string)?;

        tracing::info!("Configuration saved to {:?}", config_path);
        Ok(())
    }
}

/// TOML-serializable config (subset of Config)
#[derive(Serialize, Deserialize)]
struct TomlConfig {
    streaming_enabled: bool, // Deprecated, kept for backward compat
    tui_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    huggingface_token: Option<String>,
    backend: BackendConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    client: Option<ClientConfig>,
    teachers: Vec<TeacherEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    colors: Option<ColorScheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    features: Option<FeaturesConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_features_config_safe_defaults() {
        let f = FeaturesConfig::default();
        // Safety-critical defaults
        assert!(!f.auto_approve_tools, "auto_approve_tools must default to false");
        assert!(f.streaming_enabled, "streaming should be on by default");
        assert!(!f.debug_logging, "debug logging should be off by default");
        #[cfg(target_os = "macos")]
        assert!(!f.gui_automation, "gui automation should be off by default");
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
    }

    #[test]
    fn test_config_active_teacher_none_when_empty() {
        let config = Config::new(vec![]);
        assert!(config.active_teacher().is_none());
    }
}
