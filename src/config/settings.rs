// Configuration structs

use super::backend::BackendConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    /// Path to crisis_keywords.json
    pub crisis_keywords_path: PathBuf,

    /// Directory for metrics storage
    pub metrics_dir: PathBuf,

    /// Enable streaming responses (default: true)
    pub streaming_enabled: bool,

    /// Enable TUI (Ratatui-based interface) (default: true)
    pub tui_enabled: bool,

    /// Path to constitutional guidelines for local LLM (optional)
    /// Only used for local inference, NOT sent to Claude API
    pub constitution_path: Option<PathBuf>,

    /// Backend configuration (device selection, model paths)
    pub backend: BackendConfig,

    /// Server configuration (daemon mode)
    pub server: ServerConfig,

    /// Teacher LLM provider configuration (array of teachers in priority order)
    pub teachers: Vec<TeacherEntry>,
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
    pub fn new(teachers: Vec<TeacherEntry>) -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let project_dir = std::env::current_dir().expect("Could not determine current directory");

        // Look for constitution in ~/.shammah/constitution.md
        let constitution_path = home.join(".shammah/constitution.md");
        let constitution_path = if constitution_path.exists() {
            Some(constitution_path)
        } else {
            None
        };

        Self {
            crisis_keywords_path: project_dir.join("data/crisis_keywords.json"),
            metrics_dir: home.join(".shammah/metrics"),
            streaming_enabled: true, // Enable by default
            tui_enabled: true,       // TUI is the default for interactive terminals
            constitution_path,
            backend: BackendConfig::default(),
            server: ServerConfig::default(),
            teachers,
        }
    }

    /// Get the active teacher (first in priority list)
    pub fn active_teacher(&self) -> Option<&TeacherEntry> {
        self.teachers.first()
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
            streaming_enabled: self.streaming_enabled,
            tui_enabled: self.tui_enabled,
            backend: self.backend.clone(),
            teachers: self.teachers.clone(),
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
    streaming_enabled: bool,
    tui_enabled: bool,
    backend: BackendConfig,
    teachers: Vec<TeacherEntry>,
}
