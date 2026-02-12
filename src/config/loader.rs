// Configuration loader
// Loads API key from ~/.shammah/config.toml or environment variable

use anyhow::{bail, Context, Result};
use std::fs;

use super::settings::Config;
use crate::errors;

/// Load configuration from Shammah config file or environment
pub fn load_config() -> Result<Config> {
    // Try loading from ~/.shammah/config.toml first
    if let Some(config) = try_load_from_shammah_config()? {
        return Ok(config);
    }

    // Fall back to environment variable
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.is_empty() {
            let teachers = vec![super::TeacherEntry {
                provider: "claude".to_string(),
                api_key,
                model: None,
                base_url: None,
                name: Some("Claude (Environment)".to_string()),
            }];
            return Ok(Config::new(teachers));
        }
    }

    // No config found - prompt user to run setup
    bail!(
        "No configuration found. Please run the setup wizard:\n\n\
        \x1b[1;36mshammah setup\x1b[0m\n\n\
        This will guide you through:\n\
        • API key configuration (Claude, OpenAI, etc.)\n\
        • Local model selection (Qwen, Gemma, Llama, Mistral)\n\
        • Device selection (CoreML, Metal, CUDA, CPU)\n\
        • Model size selection based on your RAM\n\n\
        Alternatively, set environment variable:\n\
        export ANTHROPIC_API_KEY=\"sk-ant-...\""
    );
}

fn try_load_from_shammah_config() -> Result<Option<Config>> {
    use super::backend::BackendConfig;
    use super::settings::ClientConfig;
    use super::TeacherEntry;

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".shammah/config.toml");

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path)
        .map_err(|e| {
            anyhow::anyhow!(errors::file_not_found_error(
                &config_path.display().to_string(),
                "Configuration file"
            ))
        })?;

    // Parse TOML directly into a temp struct
    #[derive(serde::Deserialize)]
    struct TomlConfig {
        #[serde(default)]
        streaming_enabled: bool,
        #[serde(default = "default_tui_enabled")]
        tui_enabled: bool,
        #[serde(default)]
        backend: BackendConfig,
        #[serde(default)]
        client: Option<ClientConfig>,
        #[serde(default)]
        teachers: Vec<TeacherEntry>,
    }

    fn default_tui_enabled() -> bool {
        true
    }

    let toml_config: TomlConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!(errors::config_parse_error(&e.to_string())))?;

    if toml_config.teachers.is_empty() {
        bail!("Config is missing teachers array. Please run 'shammah setup' to configure.");
    }

    let mut config = Config::new(toml_config.teachers);
    config.streaming_enabled = toml_config.streaming_enabled;
    config.tui_enabled = toml_config.tui_enabled;
    config.backend = toml_config.backend;
    if let Some(client) = toml_config.client {
        config.client = client;
    }

    // Validate configuration
    config.validate()
        .context("Configuration validation failed")?;

    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_creation() {
        let config = Config::new("test-key".to_string());
        assert_eq!(config.api_key, "test-key");
        // similarity_threshold removed (pattern system removed)
    }
}
