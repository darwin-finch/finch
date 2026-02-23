// Configuration loader
// Loads API key from ~/.finch/config.toml or environment variable

use anyhow::{bail, Context, Result};
use std::fs;

use super::provider::ProviderEntry;
use super::settings::Config;
use crate::errors;

/// Load configuration from Shammah config file or environment
pub fn load_config() -> Result<Config> {
    // Try loading from ~/.finch/config.toml first
    if let Some(config) = try_load_from_finch_config()? {
        return Ok(config);
    }

    // Fall back to environment variable
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.is_empty() {
            let providers = vec![ProviderEntry::Claude {
                api_key,
                model: None,
                base_url: None,
                name: Some("Claude (Environment)".to_string()),
            }];
            return Ok(Config::with_providers(providers));
        }
    }

    // No config found - prompt user to run setup
    bail!(
        "No configuration found. Please run the setup wizard:\n\n\
        \x1b[1;36mfinch setup\x1b[0m\n\n\
        This will guide you through:\n\
        • API key configuration (Claude, OpenAI, etc.)\n\
        • Local model selection (Qwen, Gemma, Llama, Mistral)\n\
        • Device selection (CoreML, Metal, CUDA, CPU)\n\
        • Model size selection based on your RAM\n\n\
        Alternatively, set environment variable:\n\
        export ANTHROPIC_API_KEY=\"sk-ant-...\""
    );
}

fn try_load_from_finch_config() -> Result<Option<Config>> {
    use super::backend::BackendConfig;
    use super::colors::ColorScheme;
    use super::settings::{ClientConfig, FeaturesConfig, TeacherEntry};

    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".finch/config.toml");

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path)
        .map_err(|_e| {
            anyhow::anyhow!(errors::file_not_found_error(
                &config_path.display().to_string(),
                "Configuration file"
            ))
        })?;

    // Parse TOML into a struct that accepts both the old and new formats.
    #[derive(serde::Deserialize)]
    struct TomlConfig {
        #[serde(default)]
        streaming_enabled: bool,
        #[serde(default = "default_tui_enabled")]
        tui_enabled: bool,
        // New unified format
        #[serde(default)]
        providers: Vec<ProviderEntry>,
        // Legacy fields — kept for reading old configs
        #[serde(default)]
        backend: Option<BackendConfig>,
        #[serde(default)]
        client: Option<ClientConfig>,
        #[serde(default)]
        teachers: Vec<TeacherEntry>,
        #[serde(default)]
        colors: Option<ColorScheme>,
        #[serde(default)]
        features: Option<FeaturesConfig>,
        #[serde(default)]
        mcp_servers: Option<std::collections::HashMap<String, crate::tools::mcp::McpServerConfig>>,
        #[serde(default)]
        active_theme: Option<String>,
        #[serde(default)]
        huggingface_token: Option<String>,
        #[serde(default)]
        license: super::settings::LicenseConfig,
    }

    fn default_tui_enabled() -> bool {
        true
    }

    let toml_config: TomlConfig = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!(errors::config_parse_error(&e.to_string())))?;

    // Determine providers: prefer new format; fall back to legacy teachers/backend.
    let providers = if !toml_config.providers.is_empty() {
        toml_config.providers
    } else if !toml_config.teachers.is_empty() || toml_config.backend.is_some() {
        // Legacy format: convert to providers
        let mut providers: Vec<ProviderEntry> = toml_config
            .teachers
            .iter()
            .map(ProviderEntry::from_teacher_entry)
            .collect();
        if let Some(ref backend) = toml_config.backend {
            if backend.enabled {
                providers.push(ProviderEntry::from_backend_config(backend, None));
            }
        }
        providers
    } else {
        bail!("Config has no providers or teachers. Please run 'finch setup' to configure.");
    };

    if providers.is_empty() {
        bail!("Config has no providers configured. Please run 'finch setup' to configure.");
    }

    let mut config = Config::with_providers(providers);

    // Apply scalar overrides
    if let Some(features) = toml_config.features {
        config.features = features;
    } else {
        config.features.streaming_enabled = toml_config.streaming_enabled;
    }
    #[allow(deprecated)]
    {
        config.streaming_enabled = config.features.streaming_enabled;
    }
    config.tui_enabled = toml_config.tui_enabled;

    if let Some(client) = toml_config.client {
        config.client = client;
    }
    if let Some(colors) = toml_config.colors {
        config.colors = colors;
    }
    if let Some(theme) = toml_config.active_theme {
        config.active_theme = theme;
    }
    if let Some(token) = toml_config.huggingface_token {
        config.huggingface_token = Some(token);
    }
    if let Some(mcp_servers) = toml_config.mcp_servers {
        config.mcp_servers = mcp_servers;
    }

    // Apply license config (default = Noncommercial when section is absent)
    config.license = toml_config.license;

    // Validate configuration
    config
        .validate()
        .context("Configuration validation failed")?;

    Ok(Some(config))
}

#[cfg(test)]
mod tests {
    // Config loading tests rely on filesystem state; see integration tests.
}
