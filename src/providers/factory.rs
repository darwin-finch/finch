// Provider factory
//
// Creates LLM providers based on teacher/provider configuration

use anyhow::{bail, Context, Result};

use super::claude::ClaudeProvider;
use super::gemini::GeminiProvider;
use super::openai::OpenAIProvider;
use super::LlmProvider;
use crate::config::{Config, ProviderEntry, TeacherEntry};
use std::sync::Arc;

/// A successfully constructed cloud provider paired with its configured selector.
#[derive(Clone)]
pub struct ProviderProfile {
    profile_name: String,
    provider: Arc<dyn LlmProvider>,
}

impl ProviderProfile {
    /// The stable configured selector for this provider.
    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// The shared provider instance owned by this profile.
    pub fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    /// Capabilities of this profile's configured model.
    pub fn capabilities(&self) -> Result<super::ModelCapabilities> {
        let model = self.provider.default_model();
        let capabilities = self.provider.capabilities(model);
        if capabilities.provider != self.provider.name() || capabilities.model != model {
            bail!(
                "Capability descriptor identity mismatch for profile '{}': expected provider '{}' model '{}', got provider '{}' model '{}'",
                self.profile_name,
                self.provider.name(),
                model,
                capabilities.provider,
                capabilities.model
            );
        }
        Ok(capabilities)
    }
}

/// A cloud provider graph constructed exactly once from configuration.
#[derive(Clone)]
pub struct ProviderGraph {
    profiles: Vec<ProviderProfile>,
    default_provider: Arc<dyn LlmProvider>,
}

impl ProviderGraph {
    /// Successfully constructed named profiles in configured fallback order.
    pub fn profiles(&self) -> &[ProviderProfile] {
        &self.profiles
    }

    /// Shared primary/fallback provider used by compatibility clients.
    pub fn default_provider(&self) -> Arc<dyn LlmProvider> {
        Arc::clone(&self.default_provider)
    }
}

// ---------------------------------------------------------------------------
// New API: ProviderEntry-based (unified)
// ---------------------------------------------------------------------------

/// Create a cloud `LlmProvider` from a unified `ProviderEntry`.
///
/// Returns an error for `Local` variants — those use a different code path
/// (`create_local_generator`).
pub fn create_provider_from_entry(entry: &ProviderEntry) -> Result<Box<dyn LlmProvider>> {
    match entry {
        ProviderEntry::LegacyChatgptSubscription { .. } => bail!(
            "Legacy chatgpt_subscription profiles are unsupported because Finch no longer launches Codex app-server. Run `finch setup` and configure OpenAI Platform with an API key or another supported provider; subscription credentials are not API keys"
        ),
        ProviderEntry::Claude {
            api_key,
            model,
            base_url,
            chat_path,
            models_path,
            ..
        } => {
            let mut provider = ClaudeProvider::new_with_endpoints(
                api_key.clone(),
                base_url.as_deref().unwrap_or("https://api.anthropic.com"),
                chat_path.as_deref().unwrap_or("/v1/messages"),
                models_path.as_deref().unwrap_or("/v1/models"),
            )?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Openai {
            api_key,
            model,
            base_url,
            chat_path,
            models_path,
            reasoning_effort,
            ..
        } => {
            let api_key = if api_key.trim().is_empty() {
                std::env::var("OPENAI_API_KEY").context(
                    "OpenAI provider needs api_key in config or OPENAI_API_KEY in the environment",
                )?
            } else {
                api_key.clone()
            };
            let mut provider = OpenAIProvider::new_compatible(
                api_key,
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com".to_string()),
                chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                models_path.as_deref().unwrap_or("/v1/models"),
                "gpt-4o".to_string(),
                "openai".to_string(),
            )?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            if let Some(effort) = reasoning_effort {
                provider = provider.with_reasoning_effort(*effort);
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Grok {
            api_key,
            model,
            base_url,
            chat_path,
            models_path,
            ..
        } => {
            let mut provider = OpenAIProvider::new_compatible(
                api_key.clone(),
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.x.ai".to_string()),
                chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                models_path.as_deref().unwrap_or("/v1/models"),
                "grok-4.6".to_string(),
                "grok".to_string(),
            )?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Gemini { api_key, model, .. } => {
            let mut provider = GeminiProvider::new(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Mistral {
            api_key,
            model,
            base_url,
            chat_path,
            models_path,
            ..
        } => {
            let mut provider = OpenAIProvider::new_compatible(
                api_key.clone(),
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.mistral.ai".to_string()),
                chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                models_path.as_deref().unwrap_or("/v1/models"),
                "mistral-large-2512".to_string(),
                "mistral".to_string(),
            )?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Groq { api_key, model, .. } => {
            let mut provider = OpenAIProvider::new_groq(api_key.clone())?;
            if let Some(m) = model {
                provider = provider.with_model(m.clone());
            }
            Ok(Box::new(provider))
        }

        ProviderEntry::Ollama {
            base_url, model, ..
        } => Ok(Box::new(OpenAIProvider::new_ollama(
            base_url.clone(),
            model.clone(),
        )?)),

        ProviderEntry::RemoteDaemon { address, .. } => Ok(Box::new(
            OpenAIProvider::new_remote_daemon(address.clone())?,
        )),

        ProviderEntry::Local { .. } => {
            bail!("Local providers use a local generator — call create_local_generator() instead")
        }
    }
}

/// Create providers from a slice of unified `ProviderEntry` values.
/// Only cloud entries are included; `Local` variants are silently skipped.
pub fn create_providers_from_entries(
    entries: &[ProviderEntry],
) -> Result<Vec<Box<dyn LlmProvider>>> {
    Ok(
        create_named_providers_from_entries_with(entries, create_provider_from_entry)?
            .into_iter()
            .map(|(_, provider)| provider)
            .collect(),
    )
}

fn create_named_providers_from_entries_with<F>(
    entries: &[ProviderEntry],
    mut create: F,
) -> Result<Vec<(String, Box<dyn LlmProvider>)>>
where
    F: FnMut(&ProviderEntry) -> Result<Box<dyn LlmProvider>>,
{
    let cloud: Vec<_> = entries.iter().filter(|e| !e.is_local()).collect();
    if cloud.is_empty() {
        bail!("No cloud provider entries configured");
    }
    let mut providers = Vec::with_capacity(cloud.len());
    let mut skipped_chatgpt = Vec::new();
    for (idx, entry) in cloud.into_iter().enumerate() {
        match create(entry) {
            Ok(provider) => providers.push((entry.profile_name(), provider)),
            Err(error) if matches!(entry, ProviderEntry::LegacyChatgptSubscription { .. }) => {
                tracing::warn!(
                    provider_index = idx + 1,
                    error = %error,
                    "Skipping unsupported legacy ChatGPT subscription provider; configured fallbacks remain available"
                );
                skipped_chatgpt.push(format!("provider #{}: {error}", idx + 1));
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to create provider #{}", idx + 1));
            }
        }
    }
    if providers.is_empty() {
        if skipped_chatgpt.is_empty() {
            bail!("No usable cloud provider entries configured");
        }
        bail!(
            "No usable cloud provider entries configured; {}",
            skipped_chatgpt.join("; ")
        );
    }
    Ok(providers)
}

/// Return a single `LlmProvider` from a slice of unified entries.
/// Multiple cloud providers are wrapped in a `FallbackChain`.
pub fn create_provider_from_entries(entries: &[ProviderEntry]) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers_from_entries(entries)?;
    if providers.len() == 1 {
        Ok(providers
            .into_iter()
            .next()
            .expect("len == 1 checked above"))
    } else {
        use super::FallbackChain;
        Ok(Box::new(FallbackChain::new(providers)))
    }
}

fn graph_from_boxed_profiles(
    profiles: Vec<(String, Box<dyn LlmProvider>)>,
) -> Result<ProviderGraph> {
    if profiles.is_empty() {
        bail!("No usable cloud provider entries configured");
    }
    let profiles: Vec<_> = profiles
        .into_iter()
        .map(|(profile_name, provider)| ProviderProfile {
            profile_name,
            provider: Arc::from(provider),
        })
        .collect();
    let default_provider = if profiles.len() == 1 {
        Arc::clone(profiles[0].provider())
    } else {
        Arc::new(super::FallbackChain::from_shared(
            profiles
                .iter()
                .map(|profile| Arc::clone(profile.provider()))
                .collect(),
        ))
    };
    Ok(ProviderGraph {
        profiles,
        default_provider,
    })
}

/// Construct the named cloud provider graph once, preserving profile/provider identity when an
/// unusable ChatGPT entry is skipped. Legacy teacher-only configuration remains supported.
pub fn create_provider_graph_from_config(config: &Config) -> Result<ProviderGraph> {
    let profiles = if config.providers.iter().any(|entry| !entry.is_local()) {
        create_named_providers_from_entries_with(&config.providers, create_provider_from_entry)?
    } else {
        if config.teachers.is_empty() {
            bail!("No teacher providers configured");
        }
        config
            .teachers
            .iter()
            .enumerate()
            .map(|(idx, teacher)| {
                let profile_name = teacher
                    .name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .or_else(|| {
                        teacher
                            .model
                            .clone()
                            .filter(|model| !model.trim().is_empty())
                    })
                    .unwrap_or_else(|| teacher.provider.clone());
                create_provider_from_teacher(teacher)
                    .map(|provider| (profile_name, provider))
                    .with_context(|| format!("Failed to create teacher provider #{}", idx + 1))
            })
            .collect::<Result<Vec<_>>>()?
    };
    graph_from_boxed_profiles(profiles)
}

/// Create the ordered cloud provider pool from unified configuration, falling back to legacy
/// teacher entries only when no cloud `[[providers]]` entries exist.
pub fn create_providers_from_config(config: &Config) -> Result<Vec<Box<dyn LlmProvider>>> {
    if config.providers.iter().any(|entry| !entry.is_local()) {
        return create_providers_from_entries(&config.providers);
    }
    create_providers(&config.teachers)
}

/// Create the active provider or fallback chain from unified or legacy configuration.
pub fn create_provider_from_config(config: &Config) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers_from_config(config)?;
    if providers.len() == 1 {
        return Ok(providers
            .into_iter()
            .next()
            .expect("len == 1 checked above"));
    }
    Ok(Box::new(super::FallbackChain::new(providers)))
}

// ---------------------------------------------------------------------------
// Legacy API: TeacherEntry-based (kept for backward compat)
// ---------------------------------------------------------------------------

/// Create providers from teacher entries in priority order.
pub fn create_providers(teachers: &[TeacherEntry]) -> Result<Vec<Box<dyn LlmProvider>>> {
    if teachers.is_empty() {
        bail!("No teacher providers configured");
    }

    teachers
        .iter()
        .enumerate()
        .map(|(idx, entry)| {
            create_provider_from_teacher(entry)
                .with_context(|| format!("Failed to create teacher provider #{}", idx + 1))
        })
        .collect()
}

/// Create a single provider from a `TeacherEntry`.
pub fn create_provider_from_teacher(entry: &TeacherEntry) -> Result<Box<dyn LlmProvider>> {
    match entry.provider.to_lowercase().as_str() {
        "claude" => {
            let mut provider = ClaudeProvider::new_with_endpoints(
                entry.api_key.clone(),
                entry
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com"),
                "/v1/messages",
                "/v1/models",
            )?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "openai" => {
            let mut provider = OpenAIProvider::new_compatible(
                entry.api_key.clone(),
                entry
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com".to_string()),
                "/v1/chat/completions",
                "/v1/models",
                "gpt-4o".to_string(),
                "openai".to_string(),
            )?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "grok" => {
            let mut provider = OpenAIProvider::new_compatible(
                entry.api_key.clone(),
                entry
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.x.ai".to_string()),
                "/v1/chat/completions",
                "/v1/models",
                "grok-4.6".to_string(),
                "grok".to_string(),
            )?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "gemini" => {
            let mut provider = GeminiProvider::new(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "mistral" => {
            let mut provider = OpenAIProvider::new_compatible(
                entry.api_key.clone(),
                entry
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.mistral.ai".to_string()),
                "/v1/chat/completions",
                "/v1/models",
                "mistral-large-2512".to_string(),
                "mistral".to_string(),
            )?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        "groq" => {
            let mut provider = OpenAIProvider::new_groq(entry.api_key.clone())?;
            if let Some(model) = &entry.model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }

        _ => bail!(
            "Unknown provider '{}'. Supported: claude, openai, grok, gemini, mistral, groq",
            entry.provider
        ),
    }
}

/// Create a fallback chain with all teachers in priority order.
pub fn create_provider(teachers: &[TeacherEntry]) -> Result<Box<dyn LlmProvider>> {
    let providers = create_providers(teachers)?;

    if providers.len() == 1 {
        Ok(providers
            .into_iter()
            .next()
            .expect("len == 1 checked above"))
    } else {
        use super::FallbackChain;
        Ok(Box::new(FallbackChain::new(providers)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionTarget;
    use crate::config::{ProviderEntry, TeacherEntry};
    use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn entry(provider: &str, api_key: &str) -> TeacherEntry {
        TeacherEntry {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: None,
            base_url: None,
            name: None,
        }
    }

    fn entry_with_model(provider: &str, api_key: &str, model: &str) -> TeacherEntry {
        TeacherEntry {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model: Some(model.to_string()),
            base_url: None,
            name: None,
        }
    }

    fn pentry(variant: ProviderEntry) -> ProviderEntry {
        variant
    }

    // -----------------------------------------------------------------------
    // TeacherEntry-based tests (legacy API)
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_claude_provider() {
        let provider = create_provider_from_teacher(&entry("claude", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_create_openai_provider() {
        let provider = create_provider_from_teacher(&entry("openai", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "openai");
    }

    #[test]
    fn test_openai_platform_api_key_entry_remains_direct_and_unchanged() {
        let provider = create_provider_from_entry(&ProviderEntry::Openai {
            api_key: "sk-platform-test".into(),
            model: Some("gpt-4o-mini".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("platform".into()),
            reasoning_effort: None,
        })
        .unwrap();

        assert_eq!(provider.name(), "openai");
        assert_eq!(provider.default_model(), "gpt-4o-mini");
    }

    #[test]
    fn test_legacy_subscription_never_accepts_platform_or_app_server_credentials() {
        for credential_ref in ["", "codex-app-server:managed", "openai-platform:api-key"] {
            let error = create_provider_from_entry(&ProviderEntry::LegacyChatgptSubscription {
                credential_ref: credential_ref.into(),
                model: Some("gpt-5.6-sol".into()),
                name: None,
            })
            .err()
            .expect("legacy subscription must be rejected");
            let message = error.to_string();
            assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));
            assert!(message.contains("subscription credentials are not API keys"));
        }
    }

    #[test]
    fn test_create_grok_provider() {
        let provider = create_provider_from_teacher(&entry("grok", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "grok");
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider_from_teacher(&entry("gemini", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_create_mistral_provider() {
        let provider = create_provider_from_teacher(&entry("mistral", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "mistral");
    }

    #[test]
    fn test_create_groq_provider() {
        let provider = create_provider_from_teacher(&entry("groq", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "groq");
    }

    #[test]
    fn test_unknown_provider_returns_error() {
        let result = create_provider_from_teacher(&entry("unknown_provider_xyz", "test-key"));
        assert!(result.is_err());
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("Unknown provider"));
    }

    #[test]
    fn test_empty_teachers_returns_error() {
        let result = create_providers(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_teachers() {
        let teachers = vec![entry("openai", "key-1"), entry("claude", "key-2")];
        let providers = create_providers(&teachers).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "claude");
    }

    #[test]
    fn test_single_teacher_returns_direct_provider_not_fallback() {
        let teachers = vec![entry("claude", "key-1")];
        let provider = create_provider(&teachers).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_custom_model_is_applied() {
        let e = entry_with_model("openai", "key", "gpt-4o-mini");
        let provider = create_provider_from_teacher(&e).unwrap();
        assert_eq!(provider.default_model(), "gpt-4o-mini");
    }

    #[test]
    fn test_case_insensitive_provider_name() {
        let mut e = entry("claude", "key");
        e.provider = "Claude".to_string();
        let provider = create_provider_from_teacher(&e);
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_same_provider_different_models() {
        let teachers = vec![
            TeacherEntry {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                model: Some("gpt-4o".to_string()),
                base_url: None,
                name: Some("GPT-4o (best)".to_string()),
            },
            TeacherEntry {
                provider: "openai".to_string(),
                api_key: "test-key".to_string(),
                model: Some("gpt-5.6-sol".to_string()),
                base_url: None,
                name: Some("GPT-5.6 Sol".to_string()),
            },
        ];

        let providers = create_providers(&teachers).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "openai");
        assert_eq!(providers[0].default_model(), "gpt-4o");
        assert_eq!(providers[1].default_model(), "gpt-5.6-sol");
        assert_eq!(
            providers[0]
                .capabilities(providers[0].default_model())
                .reasoning
                .support(),
            crate::providers::CapabilitySupport::Unsupported
        );
        assert_eq!(
            providers[1]
                .capabilities(providers[1].default_model())
                .reasoning
                .support(),
            crate::providers::CapabilitySupport::Supported
        );
    }

    // -----------------------------------------------------------------------
    // ProviderEntry-based tests (new API)
    // -----------------------------------------------------------------------

    #[test]
    fn test_provider_entry_claude() {
        let p = pentry(ProviderEntry::Claude {
            api_key: "sk-ant-test".to_string(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        });
        let provider = create_provider_from_entry(&p).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_provider_entry_grok() {
        let p = pentry(ProviderEntry::Grok {
            api_key: "xai-test".to_string(),
            model: Some("grok-code-fast-1".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        });
        let provider = create_provider_from_entry(&p).unwrap();
        assert_eq!(provider.name(), "grok");
        assert_eq!(provider.default_model(), "grok-code-fast-1");
    }

    #[test]
    fn test_provider_entry_local_returns_error() {
        let p = pentry(ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        });
        let result = create_provider_from_entry(&p);
        assert!(result.is_err());
    }

    #[test]
    fn test_create_providers_from_entries_skips_local() {
        let entries = vec![
            ProviderEntry::Claude {
                api_key: "key".to_string(),
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
        let providers = create_providers_from_entries(&entries).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].name(), "claude");
    }

    #[test]
    fn test_create_providers_from_entries_empty_cloud_errors() {
        let entries = vec![ProviderEntry::Local {
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_repo: None,
            model_path: None,
            enabled: true,
            name: None,
        }];
        let result = create_providers_from_entries(&entries);
        assert!(result.is_err());
    }

    #[test]
    fn test_unsupported_legacy_chatgpt_preserves_later_configured_grok_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let entries = vec![
            ProviderEntry::LegacyChatgptSubscription {
                credential_ref: "codex-app-server:managed".into(),
                model: Some("gpt-5.6-sol".into()),
                name: Some("subscription".into()),
            },
            ProviderEntry::Grok {
                api_key: "xai-test".into(),
                model: Some("grok-code-fast-1".into()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("fallback".into()),
            },
        ];
        let path = directory.path().join("config.toml");
        Config::with_providers(entries).save_to(&path).unwrap();
        let config = crate::config::load_config_from_path(&path).unwrap();
        let graph = create_provider_graph_from_config(&config).unwrap();
        assert_eq!(graph.profiles().len(), 1);
        assert_eq!(graph.profiles()[0].profile_name(), "fallback");
        assert_eq!(graph.profiles()[0].provider().name(), "grok");
        assert_eq!(
            graph.profiles()[0].provider().default_model(),
            "grok-code-fast-1"
        );
        assert!(graph
            .profiles()
            .iter()
            .all(|profile| profile.profile_name() != "subscription"));
    }

    #[test]
    fn test_saved_legacy_chatgpt_only_config_fails_with_actionable_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        Config::with_providers(vec![ProviderEntry::LegacyChatgptSubscription {
            credential_ref: "codex-app-server:managed".into(),
            model: Some("gpt-5.6-sol".into()),
            name: Some("subscription".into()),
        }])
        .save_to(&path)
        .unwrap();

        let config = crate::config::load_config_from_path(&path).unwrap();
        assert!(config.teachers.is_empty());
        let error = create_provider_graph_from_config(&config)
            .err()
            .expect("legacy-only graph must be rejected");
        let message = error.to_string();
        assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));
        assert!(message.contains("finch setup"));
        assert!(message.contains("subscription credentials are not API keys"));
    }

    #[test]
    fn test_shared_startup_provider_preserves_legacy_teacher_config() {
        let mut config = Config::new(vec![entry("claude", "legacy-key")]);
        config.providers.clear();
        let provider = create_provider_from_config(&config).unwrap();
        assert_eq!(provider.name(), "claude");
    }
}
