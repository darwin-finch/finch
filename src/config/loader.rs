// Configuration loader
// Loads API key from ~/.shammah/config.toml or environment variable

use anyhow::{bail, Context, Result};
use std::fs;

use super::settings::Config;

/// Load configuration from Shammah config file or environment
pub fn load_config() -> Result<Config> {
    // Try loading from ~/.shammah/config.toml first
    if let Some(mut config) = try_load_from_shammah_config()? {
        return Ok(config);
    }

    // Fall back to environment variable
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.is_empty() {
            return Ok(Config::new(api_key));
        }
    }

    // No API key found
    bail!(
        "Claude API key not found\n\n\
        Checked locations:\n\
        1. ~/.shammah/config.toml\n\
        2. Environment variable: $ANTHROPIC_API_KEY\n\n\
        Please set your API key in one of these locations.\n\n\
        Quick setup:\n\
        mkdir -p ~/.shammah\n\
        echo 'api_key = \"sk-ant-...\"' > ~/.shammah/config.toml\n\n\
        Or use environment variable:\n\
        export ANTHROPIC_API_KEY=\"sk-ant-...\""
    );
}

fn try_load_from_shammah_config() -> Result<Option<Config>> {
    use super::backend::BackendConfig;
    use super::settings::TeacherConfig;

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".shammah/config.toml");

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    // Parse TOML
    let toml_config: toml::Value =
        toml::from_str(&contents).context("Failed to parse config.toml")?;

    // Extract api_key (for backwards compatibility)
    let api_key = toml_config
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if let Some(api_key) = api_key {
        let mut config = Config::new(api_key);

        // Override tui_enabled if specified in config
        if let Some(tui_enabled) = toml_config.get("tui_enabled").and_then(|v| v.as_bool()) {
            config.tui_enabled = tui_enabled;
        }

        // Load backend config if present
        if let Some(backend_value) = toml_config.get("backend") {
            if let Ok(backend_config) = toml::from_str::<BackendConfig>(&backend_value.to_string()) {
                config.backend = backend_config;
            }
        }

        // Load teacher config if present (check "teacher" first, then "fallback" for backwards compat)
        if let Some(teacher_value) = toml_config.get("teacher").or_else(|| toml_config.get("fallback")) {
            if let Ok(teacher_config) = toml::from_str::<TeacherConfig>(&teacher_value.to_string()) {
                config.teacher = teacher_config;
            }
        }

        Ok(Some(config))
    } else {
        Ok(None)
    }
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
