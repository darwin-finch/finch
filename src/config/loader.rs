// Configuration loader
// Loads API key from ~/.finch/config.toml or environment variable

use anyhow::{bail, Context, Result};
use crossterm::style::Stylize as _;
use std::fs;

use super::provider::ProviderEntry;
use super::settings::Config;
use crate::errors;

/// Load configuration from Shammah config file or environment
pub fn load_config() -> Result<Config> {
    // Try loading from ~/.finch/config.toml first
    if let Some(config) = load_persisted_config()? {
        return Ok(config);
    }

    // Fall back to environment variable
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.is_empty() {
            let providers = vec![ProviderEntry::Claude {
                api_key,
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("Claude (Environment)".to_string()),
            }];
            return Ok(Config::with_providers(providers));
        }
    }

    // No config found - prompt user to run setup
    bail!(
        "No configuration found. Please run the setup wizard:\n\n{}\n\n\
        This will guide you through:\n\
        • API key configuration (Claude, OpenAI, etc.)\n\
        • Local model selection (Qwen, Gemma, Llama, Mistral)\n\
        • Device selection (CoreML, Metal, CUDA, CPU)\n\
        • Model size selection based on your RAM\n\n\
        Alternatively, set environment variable:\n\
        export ANTHROPIC_API_KEY=\"sk-ant-...\"",
        "finch setup".cyan().bold()
    );
}

/// Load the persisted configuration without substituting environment or empty
/// state when an existing file is invalid.
///
/// `None` means the file is genuinely absent. Any read, parse, or validation
/// failure is returned so setup cannot overwrite a provider graph it failed to
/// understand.
pub fn load_persisted_config() -> Result<Option<Config>> {
    try_load_from_finch_config()
}

fn try_load_from_finch_config() -> Result<Option<Config>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".finch/config.toml");

    try_load_from_path(&config_path)
}

fn try_load_from_path(config_path: &std::path::Path) -> Result<Option<Config>> {
    if !config_path.exists() {
        return Ok(None);
    }

    Ok(Some(load_config_from_path(&config_path)?))
}

pub(crate) fn load_config_from_path(config_path: &std::path::Path) -> Result<Config> {
    load_config_from_path_with_factory(config_path, Config::with_providers)
}

#[cfg(test)]
pub(crate) fn load_config_from_path_with_paths(
    config_path: &std::path::Path,
    metrics_dir: std::path::PathBuf,
    constitution_path: Option<std::path::PathBuf>,
) -> Result<Config> {
    load_config_from_path_with_factory(config_path, move |providers| {
        Config::with_providers_and_paths(providers, metrics_dir, constitution_path)
    })
}

fn load_config_from_path_with_factory<F>(
    config_path: &std::path::Path,
    config_factory: F,
) -> Result<Config>
where
    F: FnOnce(Vec<ProviderEntry>) -> Config,
{
    use super::backend::BackendConfig;
    use super::colors::ColorScheme;
    use super::settings::{ClientConfig, FeaturesConfig, ServerConfig, TeacherEntry};

    let contents = fs::read_to_string(config_path).map_err(|_e| {
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
        #[serde(default)]
        credentials: Vec<super::ProviderCredential>,
        // Legacy fields — kept for reading old configs
        #[serde(default)]
        backend: Option<BackendConfig>,
        #[serde(default)]
        coreml: Option<super::backend::CoreMlConfig>,
        #[serde(default)]
        client: Option<ClientConfig>,
        #[serde(default)]
        server: Option<ServerConfig>,
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
        active_persona: Option<String>,
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
    let legacy_coreml = toml_config.backend.as_ref().map(|backend| backend.coreml);

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

    let mut config = config_factory(providers);
    config.replace_loaded_credentials(toml_config.credentials);

    if let Some(coreml) = toml_config.coreml.or(legacy_coreml) {
        config.backend.coreml = coreml;
    }

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
    if let Some(server) = toml_config.server {
        config.server = server;
    }
    if let Some(colors) = toml_config.colors {
        config.colors = colors;
    }
    if let Some(theme) = toml_config.active_theme {
        config.active_theme = theme;
    }
    if let Some(persona) = toml_config.active_persona {
        config.active_persona = persona;
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

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn test_explicit_config_path_loader_bypasses_default_resolver() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let metrics_dir = directory.path().join("metrics");
        let resolver_calls = Cell::new(0);
        let source = Config::with_providers_and_paths(
            vec![ProviderEntry::Claude {
                api_key: "sk-ant-test-key-1234567890".to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("claude".to_string()),
            }],
            metrics_dir.clone(),
            None,
        );
        source.save_to(&config_path).unwrap();

        let loaded = load_config_from_path_with_factory(&config_path, |providers| {
            Config::with_providers_and_paths_using_resolver(
                providers,
                metrics_dir.clone(),
                None,
                || {
                    resolver_calls.set(resolver_calls.get() + 1);
                    panic!("explicit config loader must bypass ambient default resolution");
                },
            )
        })
        .unwrap();

        assert_eq!(resolver_calls.get(), 0);
        assert_eq!(loaded.metrics_dir, metrics_dir);
        assert_eq!(loaded.constitution_path, None);
    }

    #[test]
    fn test_setup_loader_distinguishes_absent_from_broken_existing_config() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");

        assert!(try_load_from_path(&config_path).unwrap().is_none());

        std::fs::write(&config_path, "this is not valid Finch TOML = [").unwrap();
        let error = try_load_from_path(&config_path).unwrap_err().to_string();
        assert!(
            error.contains("Failed to parse configuration") || error.contains("configuration"),
            "unexpected load error: {error}"
        );
        assert!(
            config_path.exists(),
            "a failed load must not remove the file"
        );
    }
}
