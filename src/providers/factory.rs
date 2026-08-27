// Provider factory
//
// Creates LLM providers based on teacher/provider configuration

use anyhow::{bail, Context, Result};

use super::claude::ClaudeProvider;
use super::gemini::GeminiProvider;
use super::openai::OpenAIProvider;
use super::LlmProvider;
use crate::config::{
    Config, CredentialProvider, CredentialResolver, EnvironmentCredentialResolver, ProviderEntry,
    ResolvedCredential, TeacherEntry,
};
use std::collections::BTreeMap;
use std::sync::Arc;

const LEGACY_CHATGPT_MIGRATION_ERROR: &str = "Legacy chatgpt_subscription profiles are unsupported because Finch no longer launches Codex app-server. Run `finch setup` and configure OpenAI Platform with an API key or another supported provider; subscription credentials are not API keys";

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
        ProviderEntry::Credentialed { .. } => {
            bail!("Named credential profiles must be created from the complete Config graph so their references can be validated; use create_provider_from_config")
        }
        ProviderEntry::LegacyChatgptSubscription { .. } => {
            bail!(LEGACY_CHATGPT_MIGRATION_ERROR)
        }
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

fn create_provider_from_resolved_entry(
    entry: &ProviderEntry,
    resolved: &ResolvedCredential,
) -> Result<Box<dyn LlmProvider>> {
    let ProviderEntry::Credentialed {
        provider,
        model,
        base_url,
        chat_path,
        models_path,
        reasoning_effort,
        ..
    } = entry
    else {
        return create_provider_from_entry(entry);
    };
    let secret = resolved.secret.expose().to_string();
    match provider {
        CredentialProvider::Anthropic => {
            let mut provider = ClaudeProvider::new_with_endpoints(
                secret,
                base_url.as_deref().unwrap_or("https://api.anthropic.com"),
                chat_path.as_deref().unwrap_or("/v1/messages"),
                models_path.as_deref().unwrap_or("/v1/models"),
            )?;
            if let Some(model) = model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }
        CredentialProvider::OpenaiPlatform
        | CredentialProvider::Xai
        | CredentialProvider::Mistral
        | CredentialProvider::Groq => {
            let (default_base, default_model, provider_name) = match provider {
                CredentialProvider::OpenaiPlatform => ("https://api.openai.com", "gpt-4o", "openai"),
                CredentialProvider::Xai => ("https://api.x.ai", "grok-4.6", "grok"),
                CredentialProvider::Mistral => ("https://api.mistral.ai", "mistral-large-2512", "mistral"),
                CredentialProvider::Groq => ("https://api.groq.com/openai", "openai/gpt-oss-120b", "groq"),
                _ => unreachable!("outer match limits provider"),
            };
            let mut provider = OpenAIProvider::new_compatible(
                secret,
                base_url.clone().unwrap_or_else(|| default_base.to_string()),
                chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                models_path.as_deref().unwrap_or("/v1/models"),
                default_model.to_string(),
                provider_name.to_string(),
            )?;
            if let Some(model) = model {
                provider = provider.with_model(model.clone());
            }
            if let Some(effort) = reasoning_effort {
                provider = provider.with_reasoning_effort(*effort);
            }
            Ok(Box::new(provider))
        }
        CredentialProvider::GeminiAiStudio => {
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() {
                bail!("Gemini AI Studio custom endpoints are not supported by this transport")
            }
            let mut provider = GeminiProvider::new(secret)?;
            if let Some(model) = model {
                provider = provider.with_model(model.clone());
            }
            Ok(Box::new(provider))
        }
        CredentialProvider::ChatgptSubscription => bail!(
            "ChatGPT subscription credentials are distinct from OpenAI Platform credentials, but no documented Finch-native subscription transport is currently available"
        ),
        CredentialProvider::GoogleVertex => bail!(
            "Google Vertex named credentials are modeled but its cloud-identity transport is not implemented"
        ),
    }
}

fn resolve_named_graph(
    config: &Config,
    resolver: &dyn CredentialResolver,
) -> Result<BTreeMap<String, ResolvedCredential>> {
    config.validate()?;
    let credentials = crate::config::credential::credential_index(&config.credentials)?;
    let mut resolved = BTreeMap::new();
    for entry in &config.providers {
        let Some(binding) = entry.credential_binding() else {
            continue;
        };
        if resolved.contains_key(&binding.credential_ref) {
            continue;
        }
        let credential = credentials
            .get(binding.credential_ref.as_str())
            .expect("Config::validate checked every named credential reference");
        let handle = resolver.resolve(credential).map_err(|_| {
            anyhow::anyhow!("Failed to resolve named credential '{}'; inspect the credential store without printing secret material", binding.credential_ref)
        })?;
        if handle.credential_name != binding.credential_ref {
            bail!(
                "credential resolver returned a handle for the wrong requested credential '{}'",
                binding.credential_ref
            );
        }
        resolved.insert(binding.credential_ref.clone(), handle);
    }
    Ok(resolved)
}

fn create_named_profiles_from_config_with_resolver(
    config: &Config,
    resolver: &dyn CredentialResolver,
) -> Result<Vec<(String, Box<dyn LlmProvider>)>> {
    // Complete graph validation and local secret resolution happen before the
    // first provider constructor is called.
    let resolved = resolve_named_graph(config, resolver)?;
    if let Some((index, _)) = config
        .providers
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, ProviderEntry::LegacyChatgptSubscription { .. }))
    {
        bail!(
            "Provider #{} is invalid: {}",
            index + 1,
            LEGACY_CHATGPT_MIGRATION_ERROR
        );
    }
    let cloud: Vec<_> = config
        .providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| !entry.is_local())
        .collect();
    if cloud.is_empty() {
        bail!("No cloud provider entries configured");
    }
    cloud
        .into_iter()
        .map(|(index, entry)| {
            let provider = if let Some(handle) = entry
                .credential_binding()
                .and_then(|binding| resolved.get(&binding.credential_ref))
            {
                create_provider_from_resolved_entry(entry, handle)
            } else {
                create_provider_from_entry(entry)
            }
            .with_context(|| format!("Failed to create provider #{}", index + 1))?;
            Ok((entry.profile_name(), provider))
        })
        .collect()
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
    if let Some((idx, _)) = entries
        .iter()
        .enumerate()
        .find(|(_, entry)| matches!(entry, ProviderEntry::LegacyChatgptSubscription { .. }))
    {
        bail!(
            "Provider #{} is invalid: {}",
            idx + 1,
            LEGACY_CHATGPT_MIGRATION_ERROR
        );
    }

    let cloud: Vec<_> = entries.iter().filter(|e| !e.is_local()).collect();
    if cloud.is_empty() {
        bail!("No cloud provider entries configured");
    }
    let mut providers = Vec::with_capacity(cloud.len());
    for (idx, entry) in cloud.into_iter().enumerate() {
        match create(entry) {
            Ok(provider) => providers.push((entry.profile_name(), provider)),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to create provider #{}", idx + 1));
            }
        }
    }
    if providers.is_empty() {
        bail!("No usable cloud provider entries configured");
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

/// Construct the named cloud provider graph once. Invalid legacy subscription
/// entries reject the complete graph before any provider is constructed.
/// Legacy teacher-only configuration remains supported.
pub fn create_provider_graph_from_config(config: &Config) -> Result<ProviderGraph> {
    let profiles = if config.providers.iter().any(|entry| !entry.is_local()) {
        create_named_profiles_from_config_with_resolver(config, &EnvironmentCredentialResolver)?
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

/// Construct a provider graph using an injected local credential resolver.
/// Tests and alternate secret stores use this exact production validation path.
pub fn create_provider_graph_from_config_with_resolver(
    config: &Config,
    resolver: &dyn CredentialResolver,
) -> Result<ProviderGraph> {
    if !config.providers.iter().any(|entry| !entry.is_local()) {
        return create_provider_graph_from_config(config);
    }
    graph_from_boxed_profiles(create_named_profiles_from_config_with_resolver(
        config, resolver,
    )?)
}

/// Revalidate the complete graph and return one configured profile.
pub fn create_provider_profile_from_config(
    config: &Config,
    profile_name: &str,
) -> Result<Arc<dyn LlmProvider>> {
    let graph = create_provider_graph_from_config(config)?;
    graph
        .profiles()
        .iter()
        .find(|profile| profile.profile_name() == profile_name)
        .map(|profile| Arc::clone(profile.provider()))
        .with_context(|| format!("Provider profile '{profile_name}' was not found"))
}

/// Create the ordered cloud provider pool from unified configuration, falling back to legacy
/// teacher entries only when no cloud `[[providers]]` entries exist.
pub fn create_providers_from_config(config: &Config) -> Result<Vec<Box<dyn LlmProvider>>> {
    if config.providers.iter().any(|entry| !entry.is_local()) {
        return Ok(create_named_profiles_from_config_with_resolver(
            config,
            &EnvironmentCredentialResolver,
        )?
        .into_iter()
        .map(|(_, provider)| provider)
        .collect());
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
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ExecutionTarget, ProviderCredential, ResolvedSecret,
    };
    use crate::config::{ProviderEntry, TeacherEntry};
    use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    struct CountingResolver {
        calls: AtomicUsize,
    }

    struct LeakyResolver;

    impl CredentialResolver for LeakyResolver {
        fn resolve(&self, _credential: &ProviderCredential) -> Result<ResolvedCredential> {
            anyhow::bail!("resolver accidentally included sentinel-secret")
        }
    }

    impl CredentialResolver for CountingResolver {
        fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: ResolvedSecret::new("test-only-secret")?,
            })
        }
    }

    fn named_openai(profile_name: &str, credential_ref: &str, model: &str) -> ProviderEntry {
        ProviderEntry::Credentialed {
            provider: CredentialProvider::OpenaiPlatform,
            credential: CredentialBinding {
                credential_ref: credential_ref.into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: BTreeSet::new(),
            },
            model: Some(model.into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(profile_name.into()),
            reasoning_effort: None,
        }
    }

    fn named_openai_credential(name: &str, account: &str) -> ProviderCredential {
        ProviderCredential {
            name: name.into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiPlatform,
            issuer: "openai-platform".into(),
            audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
            tenant: None,
            project: None,
            account: Some(account.into()),
            scopes: BTreeSet::new(),
            secret_ref: format!("test:{name}"),
            lifecycle: CredentialLifecycle::default(),
        }
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
    fn test_complete_graph_rejects_before_secret_resolution_or_provider_construction() {
        let mut invalid = named_openai("primary", "work", "gpt-4o");
        if let ProviderEntry::Credentialed { credential, .. } = &mut invalid {
            credential.account = Some("different-account".into());
        }
        let config = Config::with_providers(vec![invalid])
            .with_credentials(vec![named_openai_credential("work", "account-1")]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };

        let error = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .err()
            .expect("account mismatch must reject the graph");
        assert!(error.to_string().contains("incompatible credential"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_absolute_chat_origin_rejects_before_resolution() {
        for hostile in [
            "http://127.0.0.1:9/steal",
            "HTTPS://evil.example/steal",
            "//evil.example/steal",
            r"\\evil.example\steal",
            "https://user:password@api.openai.com/steal",
            "https://évil.example/steal",
        ] {
            let mut profile = named_openai("primary", "work", "gpt-4o");
            if let ProviderEntry::Credentialed { chat_path, .. } = &mut profile {
                *chat_path = Some(hostile.into());
            }
            let config = Config::with_providers(vec![profile])
                .with_credentials(vec![named_openai_credential("work", "account-1")]);
            let resolver = CountingResolver {
                calls: AtomicUsize::new(0),
            };
            assert!(create_provider_graph_from_config_with_resolver(&config, &resolver).is_err());
            assert_eq!(
                resolver.calls.load(Ordering::SeqCst),
                0,
                "resolved {hostile}"
            );
        }
    }

    #[test]
    fn test_resolver_errors_are_sanitized_at_factory_boundary() {
        let config = Config::with_providers(vec![named_openai("primary", "work", "gpt-4o")])
            .with_credentials(vec![named_openai_credential("work", "account-1")]);
        let error = create_provider_graph_from_config_with_resolver(&config, &LeakyResolver)
            .err()
            .unwrap();
        let displayed = format!("{error:#}");
        assert!(!displayed.contains("sentinel-secret"));
        assert!(displayed.contains("Failed to resolve named credential 'work'"));
    }

    #[test]
    fn test_shared_compatible_credential_constructs_multiple_model_profiles_once_each() {
        let config = Config::with_providers(vec![
            named_openai("fast", "work", "gpt-4o"),
            named_openai("reasoning", "work", "gpt-5.6-sol"),
        ])
        .with_credentials(vec![named_openai_credential("work", "account-1")]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let graph = create_provider_graph_from_config_with_resolver(&config, &resolver).unwrap();
        assert_eq!(graph.profiles().len(), 2);
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        assert_eq!(graph.profiles()[0].profile_name(), "fast");
        assert_eq!(graph.profiles()[1].profile_name(), "reasoning");
    }

    #[test]
    fn test_missing_named_account_never_falls_back_to_another_credential() {
        let config = Config::with_providers(vec![named_openai("primary", "missing", "gpt-4o")])
            .with_credentials(vec![
                named_openai_credential("personal", "account-1"),
                named_openai_credential("work", "account-2"),
            ]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let error = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("missing credential 'missing'"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_revoke_reports_all_dependents_and_invalidates_complete_graph() {
        let mut config = Config::with_providers(vec![
            named_openai("primary", "work", "gpt-4o"),
            named_openai("tools", "work", "gpt-5.6-sol"),
        ])
        .with_credentials(vec![named_openai_credential("work", "account-1")]);
        assert_eq!(
            config.revoke_credential("work").unwrap(),
            vec!["primary", "tools"]
        );
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let error = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("revoked"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_duplicate_profile_names_cannot_ambiguously_select_accounts() {
        let mut second = named_openai("WORK", "account-two", "gpt-5.6-sol");
        if let ProviderEntry::Credentialed { credential, .. } = &mut second {
            credential.account = Some("account-2".into());
        }
        let config =
            Config::with_providers(vec![named_openai("work", "account-one", "gpt-4o"), second])
                .with_credentials(vec![
                    named_openai_credential("account-one", "account-1"),
                    named_openai_credential("account-two", "account-2"),
                ]);
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("duplicate provider profile name"));
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
    fn test_mixed_legacy_and_grok_rejects_before_provider_construction_or_http() {
        let directory = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let entries = vec![
            ProviderEntry::LegacyChatgptSubscription {
                credential_ref: "codex-app-server:managed".into(),
                model: Some("gpt-5.6-sol".into()),
                name: Some("subscription".into()),
            },
            ProviderEntry::Grok {
                api_key: "xai-test".into(),
                model: Some("grok-code-fast-1".into()),
                base_url: Some(endpoint.clone()),
                chat_path: None,
                models_path: None,
                name: Some("fallback".into()),
            },
        ];
        let path = directory.path().join("config.toml");
        Config::with_providers(entries).save_to(&path).unwrap();
        let config = crate::config::load_config_from_path(&path).unwrap();
        let mut construction_attempts = 0;
        let error = create_named_providers_from_entries_with(&config.providers, |entry| {
            construction_attempts += 1;
            let mut stream = std::net::TcpStream::connect(&endpoint)?;
            std::io::Write::write_all(&mut stream, b"GET /unexpected HTTP/1.0\r\n\r\n")?;
            create_provider_from_entry(entry)
        })
        .err()
        .expect("mixed legacy configuration must fail as a complete graph");

        assert_eq!(construction_attempts, 0);
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        let message = error.to_string();
        assert!(message.contains("Provider #1 is invalid"));
        assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));

        let error = create_provider_graph_from_config(&config)
            .err()
            .expect("production provider graph must reject mixed legacy configuration");
        assert!(error
            .to_string()
            .contains("Legacy chatgpt_subscription profiles are unsupported"));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
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
