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
        • llama.cpp execution selection (automatic GPU offload or CPU only)\n\
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

/// Load existing settings for the setup editor, accepting a transitional file
/// whose last removed local provider has already been deleted by the operator.
/// Normal startup continues to reject providerless configurations.
pub fn load_persisted_config_for_setup() -> Result<Option<Config>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    try_load_from_path_with_mode(&home.join(".finch/config.toml"), true)
}

fn try_load_from_finch_config() -> Result<Option<Config>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    let config_path = home.join(".finch/config.toml");

    try_load_from_path(&config_path)
}

fn try_load_from_path(config_path: &std::path::Path) -> Result<Option<Config>> {
    try_load_from_path_with_mode(config_path, false)
}

fn try_load_from_path_with_mode(
    config_path: &std::path::Path,
    allow_empty_providers_for_setup: bool,
) -> Result<Option<Config>> {
    match fs::symlink_metadata(config_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "Existing Finch configuration at {} is a symbolic link; setup will not follow or overwrite it",
                config_path.display()
            );
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "Could not inspect existing Finch configuration at {}",
                    config_path.display()
                )
            });
        }
    }

    Ok(Some(load_config_from_path_with_factory_mode(
        config_path,
        Config::with_providers,
        allow_empty_providers_for_setup,
    )?))
}

pub(crate) fn load_config_from_path(config_path: &std::path::Path) -> Result<Config> {
    load_config_from_path_with_factory(config_path, Config::with_providers)
}

/// One legacy `[[teachers]]` row from a pre-`[[providers]]` config file.
///
/// This shim exists only so unmigrated config files still load; it is never
/// written or documented. Field mapping is 1:1 with the row's provider fields.
#[derive(serde::Deserialize)]
struct LegacyTeacherEntry {
    provider: String,
    api_key: String,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Map one legacy row onto a cloud `ProviderEntry` (1:1 field mapping;
/// unknown provider names fall back to Claude).
fn legacy_teacher_entry_to_provider(entry: &LegacyTeacherEntry) -> ProviderEntry {
    ProviderEntry::from_provider_fields(
        &entry.provider,
        entry.api_key.clone(),
        entry.model.clone(),
        entry.base_url.clone(),
        entry.name.clone(),
    )
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
    load_config_from_path_with_factory_mode(config_path, config_factory, false)
}

fn load_config_from_path_with_factory_mode<F>(
    config_path: &std::path::Path,
    config_factory: F,
    allow_empty_providers_for_setup: bool,
) -> Result<Config>
where
    F: FnOnce(Vec<ProviderEntry>) -> Config,
{
    use super::backend::BackendConfig;
    use super::settings::{ClientConfig, FeaturesConfig, ServerConfig};
    use crate::theme::ColorScheme;

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
        default_provider: Option<String>,
        #[serde(default)]
        credentials: Vec<super::ProviderCredential>,
        // Legacy fields — kept for reading old configs
        #[serde(default)]
        backend: Option<BackendConfig>,
        #[serde(default)]
        client: Option<ClientConfig>,
        #[serde(default)]
        server: Option<ServerConfig>,
        #[serde(default)]
        teachers: Vec<LegacyTeacherEntry>,
        #[serde(default)]
        colors: Option<ColorScheme>,
        #[serde(default)]
        features: Option<FeaturesConfig>,
        #[serde(default)]
        mcp_servers: Option<std::collections::HashMap<String, crate::tools::McpServerConfig>>,
        #[serde(default)]
        active_theme: Option<String>,
        #[serde(default)]
        active_persona: Option<String>,
        #[serde(default)]
        huggingface_token: Option<String>,
        #[serde(default)]
        license: super::settings::LicenseConfig,
        #[serde(default)]
        diagnostics: Option<super::DiagnosticsConfig>,
    }

    fn default_tui_enabled() -> bool {
        true
    }

    let mut current_section = "";
    let mut removed_provider_block = false;
    let mut removed_backend_block = false;
    for line in contents.lines() {
        let setting = line.split('#').next().unwrap_or_default().trim();
        if setting.starts_with('[') && setting.ends_with(']') {
            current_section = setting;
            continue;
        }
        let Some((key, value)) = setting.split_once('=') else {
            continue;
        };
        let removed = matches!(
            (key.trim(), value.trim()),
            ("inference_provider", "\"onnx\"")
                | ("inference_provider", "\"candle\"")
                | ("execution_target", "\"coreml\"")
                | ("execution_target", "\"cuda\"")
                | ("execution_target", "\"metal\"")
        );
        if removed {
            if current_section == "[backend]" {
                removed_backend_block = true;
            } else {
                removed_provider_block = true;
            }
        }
    }
    if removed_provider_block || removed_backend_block {
        let affected_blocks = match (removed_provider_block, removed_backend_block) {
            (true, true) => "the affected local [[providers]] block and legacy [backend] block",
            (true, false) => "the affected local [[providers]] block",
            (false, true) => "the legacy [backend] block",
            (false, false) => unreachable!(),
        };
        bail!(
            "Configuration {} contains a removed ONNX/Candle local-chat provider or execution target. Back up the file, remove only {affected_blocks} and any obsolete [coreml] block, then run `finch setup` to add a llama.cpp/GGUF local model. Other provider and credential entries can remain unchanged.",
            config_path.display(),
        );
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
            .map(legacy_teacher_entry_to_provider)
            .collect();
        if let Some(ref backend) = toml_config.backend {
            if backend.enabled {
                providers.push(ProviderEntry::from_backend_config(backend, None));
            }
        }
        providers
    } else if allow_empty_providers_for_setup {
        Vec::new()
    } else {
        bail!("Config has no providers configured. Please run 'finch setup' to configure.");
    };

    if providers.is_empty() && !allow_empty_providers_for_setup {
        bail!("Config has no providers configured. Please run 'finch setup' to configure.");
    }

    let providerless_setup = providers.is_empty();
    let mut config = config_factory(providers);
    config.default_provider = if providerless_setup {
        None
    } else {
        toml_config.default_provider
    };
    config.replace_loaded_credentials(toml_config.credentials);

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

    // Apply declared post-edit diagnostics sources (issue #757). Absent
    // section = no declared source = no behavior change.
    if let Some(diagnostics) = toml_config.diagnostics {
        config.diagnostics = diagnostics;
    }

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
        assert!(error.contains("Failed to parse config file"), "{error}");
        assert!(
            config_path.exists(),
            "a failed load must not remove the file"
        );

        let blocked_parent = directory.path().join("not-a-directory");
        std::fs::write(&blocked_parent, "sentinel").unwrap();
        let inaccessible = blocked_parent.join("config.toml");
        let error = try_load_from_path(&inaccessible)
            .expect_err("filesystem inspection errors must not look like first run")
            .to_string();
        assert!(error.contains("Could not inspect existing Finch configuration"));
    }

    #[test]
    fn removed_local_chat_config_reports_safe_manual_migration() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[[providers]]
type = "local"
inference_provider="onnx"
execution_target = "coreml" # old macOS target
model_family = "Qwen2"
model_size = "Medium"
"#,
        )
        .unwrap();

        let error = load_config_from_path(&config_path)
            .expect_err("removed local chat values must require explicit migration")
            .to_string();
        assert!(error.contains("Back up the file"), "{error}");
        assert!(error.contains("remove only the affected local"), "{error}");
        assert!(error.contains("[[providers]] block"), "{error}");
        assert!(error.contains("finch setup"), "{error}");
    }

    #[test]
    fn removed_legacy_backend_reports_backend_specific_migration() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[backend]
enabled = true
inference_provider = "candle"
execution_target = "cpu"
model_family = "Qwen2"
model_size = "Medium"
"#,
        )
        .unwrap();

        let error = load_config_from_path(&config_path)
            .expect_err("removed legacy backend must require explicit migration")
            .to_string();
        assert!(
            error.contains("remove only the legacy [backend] block"),
            "{error}"
        );
        assert!(
            !error.contains("affected local [[providers]] block"),
            "{error}"
        );
        assert!(error.contains("finch setup"), "{error}");

        // Simulate following the instruction: the provider is gone, while
        // unrelated settings remain available to the setup editor.
        std::fs::write(
            &config_path,
            r#"
active_theme = "solarized"

[features]
streaming_enabled = false
"#,
        )
        .unwrap();
        assert!(
            try_load_from_path(&config_path).is_err(),
            "normal startup must continue rejecting a providerless config"
        );
        let setup_config = try_load_from_path_with_mode(&config_path, true)
            .expect("setup loader must accept the transitional providerless file")
            .expect("the existing file must remain distinguishable from absence");
        assert!(setup_config.providers.is_empty());
        assert_eq!(setup_config.active_theme, "solarized");
        assert!(!setup_config.features.streaming_enabled);
    }

    #[cfg(unix)]
    #[test]
    fn test_setup_loader_rejects_dangling_config_symlink_as_existing_state() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        symlink(directory.path().join("missing-target.toml"), &config_path).unwrap();

        let error = try_load_from_path(&config_path)
            .expect_err("a dangling config symlink must not look like first run")
            .to_string();
        assert!(error.contains("symbolic link"), "{error}");
        assert!(
            std::fs::symlink_metadata(&config_path).is_ok(),
            "failed inspection must preserve the link"
        );
    }

    /// A config written before `[[providers]]` existed still loads: the
    /// `[[teachers]]` rows migrate 1:1 onto cloud provider entries and an
    /// enabled `[backend]` becomes the local entry.
    #[test]
    fn test_legacy_teachers_config_still_loads_and_migrates_to_providers() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[[teachers]]
provider = "claude"
api_key = "sk-ant-legacy-key-1234567890"
model = "claude-sonnet-4-5"
name = "Legacy Claude"

[[teachers]]
provider = "openai"
api_key = "sk-legacy-openai"
base_url = "https://example.invalid/v1"

[[teachers]]
provider = "some_removed_provider"
api_key = "sk-ant-unknown-name-1234567890"
"#,
        )
        .unwrap();

        let loaded = load_config_from_path_with_factory(&config_path, |providers| {
            Config::with_providers_and_paths(providers, directory.path().join("metrics"), None)
        })
        .expect("a legacy [[teachers]] config must still load");

        assert_eq!(
            loaded.providers.len(),
            3,
            "every legacy teacher row must become one provider entry: {:?}",
            loaded.providers
        );
        assert!(
            loaded.providers.iter().all(|entry| !entry.is_local()),
            "no local backend was enabled, so every migrated entry is cloud: {:?}",
            loaded.providers
        );

        let first = &loaded.providers[0];
        assert_eq!(first.provider_type(), "claude", "{first:?}");
        assert_eq!(first.api_key(), Some("sk-ant-legacy-key-1234567890"));
        assert_eq!(first.profile_name(), "Legacy Claude");
        assert_eq!(first.model(), Some("claude-sonnet-4-5"));

        let second = &loaded.providers[1];
        assert_eq!(second.provider_type(), "openai", "{second:?}");

        // Unknown provider names map to Claude — the safest fallback the
        // removed conversion used — instead of failing the load.
        let third = &loaded.providers[2];
        assert_eq!(
            third.provider_type(),
            "claude",
            "unknown legacy provider names must fall back to Claude: {third:?}"
        );
        assert_eq!(third.api_key(), Some("sk-ant-unknown-name-1234567890"));
    }

    /// Saving a loaded legacy config writes the unified `[[providers]]` format
    /// and no longer mentions the removed table.
    #[test]
    fn test_legacy_teachers_config_saves_as_providers_only() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[[teachers]]
provider = "claude"
api_key = "sk-ant-legacy-key-1234567890"
"#,
        )
        .unwrap();

        let loaded = load_config_from_path_with_factory(&config_path, |providers| {
            Config::with_providers_and_paths(providers, directory.path().join("metrics"), None)
        })
        .expect("a legacy [[teachers]] config must still load");

        let saved_path = directory.path().join("saved.toml");
        loaded.save_to(&saved_path).unwrap();
        let saved = std::fs::read_to_string(&saved_path).unwrap();
        assert!(
            saved.contains("[[providers]]"),
            "the save must use the unified provider format, file was:\n{saved}"
        );
        assert!(
            !saved.contains("teachers"),
            "the removed table must not be written back, file was:\n{saved}"
        );
    }
}
