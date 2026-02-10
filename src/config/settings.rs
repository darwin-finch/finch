// Configuration structs

use super::backend::BackendConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Claude API key (deprecated - use fallback config instead)
    pub api_key: String,

    /// Path to crisis_keywords.json
    pub crisis_keywords_path: PathBuf,

    /// Directory for metrics storage
    pub metrics_dir: PathBuf,

    /// Enable streaming responses (default: true)
    pub streaming_enabled: bool,

    /// Enable TUI (Ratatui-based interface) (default: false for Phase 2)
    pub tui_enabled: bool,

    /// Path to constitutional guidelines for local LLM (optional)
    /// Only used for local inference, NOT sent to Claude API
    pub constitution_path: Option<PathBuf>,

    /// Backend configuration (device selection, model paths)
    pub backend: BackendConfig,

    /// Server configuration (daemon mode)
    pub server: ServerConfig,

    /// Teacher LLM provider configuration
    pub teacher: TeacherConfig,
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
        }
    }
}

/// Teacher LLM provider configuration
///
/// The local model (student) learns from teacher providers.
/// Configure multiple teachers, priority = first in array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeacherConfig {
    /// Legacy single provider (deprecated, use teachers array)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Legacy provider settings (deprecated, use teachers array)
    #[serde(flatten, skip_serializing_if = "HashMap::is_empty", default)]
    pub settings: HashMap<String, ProviderSettings>,

    /// Array of teacher configurations in priority order
    /// The first teacher in the array is the active one.
    #[serde(default)]
    pub teachers: Vec<TeacherEntry>,
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

/// Provider-specific settings (legacy)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderSettings {
    /// API key for this provider
    pub api_key: String,

    /// Optional model override (uses provider default if not specified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Optional base URL (for custom endpoints)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Default for TeacherConfig {
    fn default() -> Self {
        Self {
            provider: Some("claude".to_string()),
            settings: HashMap::new(),
            teachers: vec![],
        }
    }
}

impl TeacherConfig {
    /// Get teacher entries in priority order
    /// Supports both legacy (single provider) and new (array) formats
    pub fn get_teachers(&self) -> Vec<TeacherEntry> {
        if !self.teachers.is_empty() {
            // Use new format
            self.teachers.clone()
        } else if let Some(provider) = &self.provider {
            // Convert legacy format to single entry
            if let Some(settings) = self.settings.get(provider) {
                vec![TeacherEntry {
                    provider: provider.clone(),
                    api_key: settings.api_key.clone(),
                    model: settings.model.clone(),
                    base_url: settings.base_url.clone(),
                    name: None,
                }]
            } else {
                vec![]
            }
        } else {
            vec![]
        }
    }

    /// Get the active teacher (first in priority list)
    pub fn active_teacher(&self) -> Option<&TeacherEntry> {
        self.get_teachers().first()
    }

    /// Get the settings for a specific provider (legacy method)
    pub fn get_provider_settings(&self, provider: &str) -> Option<&ProviderSettings> {
        self.settings.get(provider)
    }

    /// Get the settings for the currently selected provider (legacy method)
    pub fn get_current_settings(&self) -> Option<&ProviderSettings> {
        if let Some(provider) = &self.provider {
            self.get_provider_settings(provider)
        } else {
            None
        }
    }
}

// Backwards compatibility alias
pub type FallbackConfig = TeacherConfig;
pub type FallbackEntry = TeacherEntry;

impl Config {
    pub fn new(api_key: String) -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let project_dir = std::env::current_dir().expect("Could not determine current directory");

        // Look for constitution in ~/.shammah/constitution.md
        let constitution_path = home.join(".shammah/constitution.md");
        let constitution_path = if constitution_path.exists() {
            Some(constitution_path)
        } else {
            None
        };

        // Create default teacher config with Claude (legacy format)
        let mut teacher = TeacherConfig::default();
        teacher.provider = Some("claude".to_string());
        teacher.settings.insert(
            "claude".to_string(),
            ProviderSettings {
                api_key: api_key.clone(),
                model: None,
                base_url: None,
            },
        );

        Self {
            api_key,
            crisis_keywords_path: project_dir.join("data/crisis_keywords.json"),
            metrics_dir: home.join(".shammah/metrics"),
            streaming_enabled: true, // Enable by default
            tui_enabled: true,       // TUI is the default for interactive terminals
            constitution_path,
            backend: BackendConfig::default(),
            server: ServerConfig::default(),
            teacher,
        }
    }

    /// Save configuration to TOML file at ~/.shammah/config.toml
    pub fn save(&self) -> anyhow::Result<()> {
        use std::fs;

        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
        let config_dir = home.join(".shammah");
        let config_path = config_dir.join("config.toml");

        // Create directory if it doesn't exist
        fs::create_dir_all(&config_dir)?;

        // Create serializable config
        let toml_config = TomlConfig {
            api_key: self.api_key.clone(),
            streaming_enabled: self.streaming_enabled,
            backend: self.backend.clone(),
            teacher: self.teacher.clone(),
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
    api_key: String,
    streaming_enabled: bool,
    backend: BackendConfig,
    teacher: TeacherConfig,
}
