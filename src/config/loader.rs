// Configuration loader
// Loads API key from ~/.finch/config.toml or environment variable

use anyhow::{bail, Context, Result};
use crossterm::style::Stylize as _;
use std::fs;
use std::path::{Path, PathBuf};

use super::provider::ProviderEntry;
use super::settings::Config;
use crate::errors;

/// The complete persisted schema accepted by the loader.
///
/// This lives at module scope so setup migration can validate the rewritten
/// TOML before replacing the user's file.
#[derive(serde::Deserialize)]
struct TomlConfig {
    #[serde(default)]
    streaming_enabled: bool,
    #[serde(default = "default_tui_enabled")]
    tui_enabled: bool,
    #[serde(default)]
    providers: Vec<ProviderEntry>,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    credentials: Vec<super::ProviderCredential>,
    #[serde(default)]
    backend: Option<super::backend::BackendConfig>,
    #[serde(default)]
    client: Option<super::settings::ClientConfig>,
    #[serde(default)]
    server: Option<super::settings::ServerConfig>,
    #[serde(default)]
    teachers: Vec<LegacyTeacherEntry>,
    #[serde(default)]
    colors: Option<crate::theme::ColorScheme>,
    #[serde(default)]
    features: Option<super::settings::FeaturesConfig>,
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
/// whose last retired local provider was removed by setup migration. Normal
/// startup continues to reject providerless configurations.
pub fn load_persisted_config_for_setup() -> Result<Option<Config>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    try_load_from_path_with_mode(&home.join(".finch/config.toml"), true)
}

/// Remove retired local-chat sections before the setup editor loads them.
///
/// This is deliberately separate from every ordinary startup path: only an
/// explicit setup invocation may rewrite configuration. The original bytes
/// are saved to a private adjacent backup before the migrated file is
/// atomically installed.
pub fn migrate_removed_local_chat_config_for_setup() -> Result<Option<PathBuf>> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    migrate_removed_local_chat_config_at_path(&home.join(".finch/config.toml"))
}

fn migrate_removed_local_chat_config_at_path(config_path: &Path) -> Result<Option<PathBuf>> {
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

    let original = fs::read_to_string(config_path).with_context(|| {
        format!(
            "Could not read existing Finch configuration at {}",
            config_path.display()
        )
    })?;
    let Some(migrated) = remove_retired_local_chat_sections(&original)? else {
        return Ok(None);
    };

    // Validate the full surviving schema before changing either directory
    // entry. This catches malformed unrelated settings that the retired-value
    // guard previously masked.
    toml::from_str::<TomlConfig>(&migrated)
        .map_err(|error| anyhow::anyhow!(errors::config_parse_error(&error.to_string())))
        .context("The configuration was not changed")?;

    let backup_path = next_migration_backup_path(config_path)?;
    super::atomic_write::atomic_write(&backup_path, original.as_bytes())
        .context("Could not create the pre-migration configuration backup")?;
    super::atomic_write::atomic_write(config_path, migrated.as_bytes()).with_context(|| {
        format!(
            "Could not install migrated configuration; the original is backed up at {}",
            backup_path.display()
        )
    })?;

    Ok(Some(backup_path))
}

fn next_migration_backup_path(config_path: &Path) -> Result<PathBuf> {
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow::anyhow!("Configuration path has no valid file name"))?;

    for suffix in 0..10_000 {
        let candidate_name = if suffix == 0 {
            format!("{file_name}.pre-gguf-migration.bak")
        } else {
            format!("{file_name}.pre-gguf-migration.bak.{suffix}")
        };
        let candidate = config_path.with_file_name(candidate_name);
        match fs::symlink_metadata(&candidate) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(candidate),
            Ok(_) => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Could not inspect backup path {}", candidate.display())
                });
            }
        }
    }

    bail!(
        "Could not choose an unused migration backup beside {}",
        config_path.display()
    )
}

#[derive(Debug)]
struct TomlTextSection {
    header: Option<String>,
    text: String,
}

fn remove_retired_local_chat_sections(contents: &str) -> Result<Option<String>> {
    let sections = split_toml_sections(contents);
    let mut removed_any = false;
    let mut removed_profile_names = std::collections::BTreeSet::new();

    for section in &sections {
        if !contains_removed_local_chat_value(&section.text) {
            continue;
        }
        match section.header.as_deref() {
            Some("[[providers]]") => {
                removed_any = true;
                if let Some(name) = removed_local_profile_name(&section.text) {
                    removed_profile_names.insert(name);
                }
            }
            Some("[backend]") | Some("[coreml]") => removed_any = true,
            Some(header) => bail!(
                "Found a removed ONNX/Candle setting in unsupported TOML section {header}; the configuration was not changed"
            ),
            None => bail!(
                "Found a removed ONNX/Candle setting outside a provider/backend section; the configuration was not changed"
            ),
        }
    }

    if !removed_any {
        return Ok(None);
    }

    let default_to_remove = top_level_string_value(contents, "default_provider")
        .filter(|name| removed_profile_names.contains(*name));
    let mut migrated = String::with_capacity(contents.len());
    for section in sections {
        let header = section.header.as_deref();
        let remove_section = (matches!(header, Some("[[providers]]"))
            && contains_removed_local_chat_value(&section.text))
            || (matches!(header, Some("[backend]"))
                && contains_removed_local_chat_value(&section.text))
            || header.is_some_and(is_coreml_section);

        if !remove_section {
            if header.is_none() && default_to_remove.is_some() {
                migrated.push_str(&remove_top_level_key(&section.text, "default_provider"));
            } else {
                migrated.push_str(&section.text);
            }
        }
    }

    Ok(Some(migrated))
}

fn split_toml_sections(contents: &str) -> Vec<TomlTextSection> {
    let mut sections = vec![TomlTextSection {
        header: None,
        text: String::new(),
    }];

    for line in contents.split_inclusive('\n') {
        if let Some(header) = toml_section_header(line) {
            sections.push(TomlTextSection {
                header: Some(header.to_string()),
                text: line.to_string(),
            });
        } else {
            sections
                .last_mut()
                .expect("the prefix section always exists")
                .text
                .push_str(line);
        }
    }

    sections
}

fn toml_section_header(line: &str) -> Option<&str> {
    let setting = line.split('#').next().unwrap_or_default().trim();
    (setting.starts_with('[') && setting.ends_with(']')).then_some(setting)
}

fn is_coreml_section(header: &str) -> bool {
    header == "[coreml]" || header.starts_with("[coreml.")
}

fn contains_removed_local_chat_value(section: &str) -> bool {
    section.lines().any(|line| {
        let setting = line.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = setting.split_once('=') else {
            return false;
        };
        matches!(
            (key.trim(), value.trim()),
            ("inference_provider", "\"onnx\"")
                | ("inference_provider", "\"candle\"")
                | ("execution_target", "\"coreml\"")
                | ("execution_target", "\"cuda\"")
                | ("execution_target", "\"metal\"")
        )
    })
}

fn removed_local_profile_name(section: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct RemovedProviderDocument {
        providers: Vec<RemovedProviderIdentity>,
    }

    #[derive(serde::Deserialize)]
    struct RemovedProviderIdentity {
        #[serde(default)]
        name: Option<String>,
        #[serde(default = "default_removed_model_family")]
        model_family: crate::models::ModelFamily,
        #[serde(default = "default_removed_model_size")]
        model_size: crate::models::ModelSize,
    }

    fn default_removed_model_family() -> crate::models::ModelFamily {
        crate::models::ModelFamily::Qwen2
    }

    fn default_removed_model_size() -> crate::models::ModelSize {
        crate::models::ModelSize::Medium
    }

    let document = toml::from_str::<RemovedProviderDocument>(section).ok()?;
    let provider = document.providers.into_iter().next()?;
    if let Some(name) = provider.name.filter(|name| !name.trim().is_empty()) {
        return Some(name);
    }
    Some(format!(
        "local-{}-{}",
        provider
            .model_family
            .name()
            .to_ascii_lowercase()
            .replace(' ', "-"),
        provider
            .model_size
            .to_size_string(provider.model_family)
            .to_ascii_lowercase()
            .replace(' ', "-")
    ))
}

fn top_level_string_value<'a>(contents: &'a str, key_to_find: &str) -> Option<&'a str> {
    for line in contents.lines() {
        if toml_section_header(line).is_some() {
            break;
        }
        let setting = line.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = setting.split_once('=') else {
            continue;
        };
        if key.trim() == key_to_find {
            return value.trim().strip_prefix('"')?.strip_suffix('"');
        }
    }
    None
}

fn remove_top_level_key(prefix: &str, key_to_remove: &str) -> String {
    prefix
        .split_inclusive('\n')
        .filter(|line| {
            let setting = line.split('#').next().unwrap_or_default().trim();
            let Some((key, _)) = setting.split_once('=') else {
                return true;
            };
            key.trim() != key_to_remove
        })
        .collect()
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
    let contents = fs::read_to_string(config_path).map_err(|_e| {
        anyhow::anyhow!(errors::file_not_found_error(
            &config_path.display().to_string(),
            "Configuration file"
        ))
    })?;

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
            "Configuration {} contains a removed ONNX/Candle local-chat provider or execution target. Run `finch setup`: it will back up the file, remove only {affected_blocks} and any obsolete [coreml] block, preserve other providers and credentials, and open the wizard to add a llama.cpp/GGUF local model.",
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
    fn removed_local_chat_config_reports_safe_setup_migration() {
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
        assert!(error.contains("Run `finch setup`"), "{error}");
        assert!(error.contains("back up the file"), "{error}");
        assert!(error.contains("remove only the affected local"), "{error}");
        assert!(error.contains("[[providers]] block"), "{error}");
        assert!(error.contains("llama.cpp/GGUF"), "{error}");
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
        assert!(error.contains("Run `finch setup`"), "{error}");

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

    #[test]
    fn setup_migration_backs_up_and_removes_only_retired_sections() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let original = r#"# keep this hand-written comment
default_provider = "Old Local"
active_theme = "solarized"

[[providers]]
type = "local"
name = "Old Local"
inference_provider = "onnx"
execution_target = "coreml"
model_family = "Qwen2"
model_size = "Medium"

[coreml]
compute_units = "all"

[coreml.cache]
enabled = true

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-test"
name = "Cloud"

[providers.credential]
credential_ref = "openai-work"
required_scopes = []

[[credentials]]
name = "openai-work"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
secret_ref = "env:OPENAI_WORK_API_KEY"
scopes = []

[credentials.audience]
family = "openai_platform"

[credentials.lifecycle]
state = "active"
refreshable = false

[features]
streaming_enabled = false
"#;
        std::fs::write(&config_path, original).unwrap();

        assert!(
            load_config_from_path(&config_path).is_err(),
            "normal startup must reject the legacy file without mutating it"
        );
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);

        let backup = migrate_removed_local_chat_config_at_path(&config_path)
            .expect("setup migration must succeed")
            .expect("a retired provider must produce a backup");
        assert_eq!(
            backup.file_name().unwrap(),
            "config.toml.pre-gguf-migration.bak"
        );
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&backup).unwrap().permissions().mode() & 0o077,
                0,
                "the backup contains credentials and must be owner-only"
            );
        }

        let migrated = std::fs::read_to_string(&config_path).unwrap();
        assert!(migrated.contains("# keep this hand-written comment"));
        assert!(migrated.contains("credential_ref = \"openai-work\""));
        assert!(migrated.contains("secret_ref = \"env:OPENAI_WORK_API_KEY\""));
        assert!(migrated.contains("active_theme = \"solarized\""));
        assert!(!migrated.contains("Old Local"), "{migrated}");
        assert!(!migrated.contains("[coreml"), "{migrated}");
        assert!(!migrated.contains("inference_provider = \"onnx\""));

        let loaded = try_load_from_path_with_mode(&config_path, true)
            .expect("the setup loader must accept the migrated file")
            .expect("the migrated file still exists");
        assert_eq!(loaded.providers.len(), 1);
        assert_eq!(loaded.providers[0].profile_name(), "Cloud");
        assert_eq!(loaded.credentials().len(), 1);
        assert_eq!(loaded.default_provider, None);
        assert_eq!(loaded.active_theme, "solarized");
        assert!(!loaded.features.streaming_enabled);
    }

    #[test]
    fn setup_migration_allows_a_providerless_legacy_backend() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"active_theme = "light"

[backend]
enabled = true
inference_provider = "candle"
execution_target = "cpu"
model_family = "Qwen2"
model_size = "Medium"
"#,
        )
        .unwrap();

        migrate_removed_local_chat_config_at_path(&config_path)
            .unwrap()
            .expect("the original must be backed up");

        assert!(
            try_load_from_path(&config_path).is_err(),
            "normal startup still requires a provider"
        );
        let loaded = try_load_from_path_with_mode(&config_path, true)
            .unwrap()
            .unwrap();
        assert!(loaded.providers.is_empty());
        assert_eq!(loaded.active_theme, "light");
    }

    #[test]
    fn setup_migration_is_a_noop_for_current_or_absent_configuration() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        assert!(migrate_removed_local_chat_config_at_path(&config_path)
            .unwrap()
            .is_none());

        let current = r#"[[providers]]
type = "claude"
api_key = "sk-ant-current-test-key-1234567890"
"#;
        std::fs::write(&config_path, current).unwrap();
        assert!(migrate_removed_local_chat_config_at_path(&config_path)
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), current);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn setup_migration_leaves_invalid_surviving_configuration_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let original = r#"[[providers]]
type = "local"
inference_provider = "onnx"
execution_target = "coreml"

[features]
streaming_enabled = [
"#;
        std::fs::write(&config_path, original).unwrap();

        let error = migrate_removed_local_chat_config_at_path(&config_path)
            .expect_err("invalid surviving TOML must prevent every write")
            .to_string();
        assert!(error.contains("configuration was not changed"), "{error}");
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn setup_migration_never_overwrites_an_existing_backup() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let first_backup = directory.path().join("config.toml.pre-gguf-migration.bak");
        std::fs::write(&first_backup, "older backup").unwrap();
        std::fs::write(
            &config_path,
            "[backend]\ninference_provider = \"onnx\"\nexecution_target = \"cpu\"\n",
        )
        .unwrap();

        let backup = migrate_removed_local_chat_config_at_path(&config_path)
            .unwrap()
            .unwrap();
        assert_eq!(
            backup.file_name().unwrap(),
            "config.toml.pre-gguf-migration.bak.1"
        );
        assert_eq!(
            std::fs::read_to_string(first_backup).unwrap(),
            "older backup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_setup_loader_rejects_dangling_config_symlink_as_existing_state() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        symlink(directory.path().join("missing-target.toml"), &config_path).unwrap();

        let migration_error = migrate_removed_local_chat_config_at_path(&config_path)
            .expect_err("setup migration must refuse a dangling config symlink")
            .to_string();
        assert!(
            migration_error.contains("symbolic link"),
            "{migration_error}"
        );

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
