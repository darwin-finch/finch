// Provider factory
//
// Creates LLM providers from the unified configuration

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::{
    LlmProvider, ProviderBackend, ProviderRequest, ProviderResponse, StreamChunk,
    ValidatedProviderRequest,
};
use crate::config::{
    Config, CredentialProvider, CredentialResolver, EnvironmentCredentialResolver, ProviderEntry,
    ResolvedCredential,
};
use finch_providers::{
    ChatGptSubscriptionProvider, ClaudeCliProvider, ClaudeProvider, ClaudeSubscriptionProvider,
    GeminiProvider, GrokSubscriptionProvider, OpenAIProvider,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;

const LEGACY_CHATGPT_MIGRATION_ERROR: &str = "Legacy chatgpt_subscription profiles are unsupported because Finch no longer launches Codex app-server. Run `finch setup` and configure OpenAI Platform with an API key or another supported provider; subscription credentials are not API keys";

/// Claude subscription is opt-in and disabled by default: it reuses Claude
/// Code's own OAuth client identity (Finch has no client id of its own
/// registered with Anthropic for this surface), which matches a pattern
/// Anthropic has a documented history of actively detecting and blocking for
/// other third-party tools. A credential can exist in the local OAuth store
/// (e.g. from an earlier opt-in, or a copied config) without the flag being
/// set now, so this is re-checked at every construction, not only at login.
fn require_claude_subscription_oauth_opt_in(config: &Config) -> Result<()> {
    if !config.features.claude_subscription_oauth_enabled {
        bail!(
            "Claude subscription is disabled by default; set claude_subscription_oauth_enabled = true under [features] in config.toml to opt in (see crates/finch-providers/AGENTS.md for why)"
        );
    }
    Ok(())
}

struct CredentialBoundProvider {
    inner: Box<dyn LlmProvider>,
    credential_name: String,
    expires_at: Option<DateTime<Utc>>,
    revocation: crate::config::LifecycleRevocation,
}

impl CredentialBoundProvider {
    fn new(inner: Box<dyn LlmProvider>, credential: &crate::config::ProviderCredential) -> Self {
        let expires_at = match &credential.lifecycle {
            crate::config::CredentialLifecycle::Active { expires_at, .. } => *expires_at,
            crate::config::CredentialLifecycle::Revoked
            | crate::config::CredentialLifecycle::LegacyAmbiguous => None,
        };
        Self {
            inner,
            credential_name: credential.name.clone(),
            expires_at,
            revocation: credential.revocation.clone(),
        }
    }

    fn validate_lifecycle(&self) -> Result<()> {
        if self.revocation.is_revoked() {
            bail!(
                "credential '{}' was revoked after provider construction; select another profile",
                self.credential_name
            );
        }
        if self
            .expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
        {
            bail!(
                "credential '{}' expired after provider construction; reconnect or reselect the profile after updating it",
                self.credential_name
            );
        }
        Ok(())
    }
}

#[async_trait]
impl ProviderBackend for CredentialBoundProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        self.validate_lifecycle()?;
        self.inner.send_message(&request).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>> {
        let (request, _bindings) = request.into_request_for(self)?;
        self.validate_lifecycle()?;
        self.inner.send_message_stream(&request).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn default_model(&self) -> &str {
        self.inner.default_model()
    }

    fn capabilities(&self, model: &str) -> super::ModelCapabilities {
        self.inner.capabilities(model)
    }

    fn requested_reasoning_effort(
        &self,
        request: &ProviderRequest,
    ) -> Option<crate::config::ReasoningEffort> {
        self.inner.requested_reasoning_effort(request)
    }
}

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
        ProviderEntry::OpenAiCompatible { .. } => {
            bail!("Generic OpenAI-compatible profiles must be created from the complete Config graph so their named credential can be validated; use create_provider_from_config")
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
        // Subscription subprocess backend: the `claude` CLI holds its own
        // OAuth login and this entry only exists when configured by hand.
        // The setup wizard never offers it; its ToS/enforcement risk is
        // documented on the config variant itself.
        ProviderEntry::ClaudeCliBackend { model, binary, .. } => {
            let binary = binary
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("claude"));
            Ok(Box::new(ClaudeCliProvider::with_binary(
                binary,
                model.clone(),
            )))
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

        ProviderEntry::Openrouter {
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
                    .unwrap_or_else(|| "https://openrouter.ai/api".to_string()),
                chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                models_path.as_deref().unwrap_or("/v1/models"),
                "z-ai/glm-5.3-flash".to_string(),
                "openrouter".to_string(),
            )?;
            if let Some(model) = model {
                provider = provider.with_model(model.clone());
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
    if let ProviderEntry::OpenAiCompatible {
        name,
        base_url,
        chat_path,
        models_path,
        model,
        capabilities,
        tool_choice,
        strict_tool_schemas,
        ..
    } = entry
    {
        let attested = super::ModelCapabilities::configured_openai_compatible(
            name.clone(),
            model.clone(),
            capabilities.streaming,
            capabilities.tools,
            capabilities.parallel_tool_calls,
            capabilities.image_input,
            capabilities
                .context_window_tokens
                .map(|value| value as usize),
            capabilities.max_output_tokens.map(|value| value as usize),
        );
        return Ok(Box::new(OpenAIProvider::new_configured_compatible(
            resolved.secret.expose().to_string(),
            base_url.clone(),
            chat_path.as_deref().unwrap_or("/chat/completions"),
            models_path.as_deref().unwrap_or("/models"),
            model.clone(),
            name.clone(),
            attested,
            matches!(tool_choice, crate::config::OpenAiCompatibleToolChoice::Auto),
            *strict_tool_schemas,
        )?));
    }
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
        | CredentialProvider::MetaModelApi
        | CredentialProvider::Xai
        | CredentialProvider::Mistral
        | CredentialProvider::Groq
        | CredentialProvider::Openrouter => {
            let (default_base, default_model, provider_name) = match provider {
                CredentialProvider::OpenaiPlatform => ("https://api.openai.com", "gpt-4o", "openai"),
                CredentialProvider::MetaModelApi => ("https://api.meta.ai", "muse-spark-1.3", "meta_model_api"),
                CredentialProvider::Xai => ("https://api.x.ai", "grok-4.6", "grok"),
                CredentialProvider::Mistral => ("https://api.mistral.ai", "mistral-large-2512", "mistral"),
                CredentialProvider::Groq => ("https://api.groq.com/openai", "openai/gpt-oss-120b", "groq"),
                CredentialProvider::Openrouter => ("https://openrouter.ai/api", "z-ai/glm-5.3-flash", "openrouter"),
                _ => unreachable!("outer match limits provider"),
            };
            let mut provider = if *provider == CredentialProvider::MetaModelApi {
                OpenAIProvider::new_meta_model_api(secret)?
            } else {
                OpenAIProvider::new_compatible(
                    secret,
                    base_url.clone().unwrap_or_else(|| default_base.to_string()),
                    chat_path.as_deref().unwrap_or("/v1/chat/completions"),
                    models_path.as_deref().unwrap_or("/v1/models"),
                    default_model.to_string(),
                    provider_name.to_string(),
                )?
            };
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
        CredentialProvider::ClaudeSubscription => bail!(
            "Claude subscription credentials are distinct from Anthropic API-key credentials and cannot be resolved as an environment secret"
        ),
        CredentialProvider::GrokSubscription => bail!(
            "Grok subscription credentials are distinct from xAI Console API-key credentials and cannot be resolved as an environment secret"
        ),
        CredentialProvider::GoogleVertex => bail!(
            "Google Vertex named credentials are modeled but its cloud-identity transport is not implemented"
        ),
        CredentialProvider::OpenaiCompatible => bail!(
            "generic OpenAI-compatible credentials require an openai_compatible profile"
        ),
    }
}

fn resolve_named_graph(
    config: &Config,
    resolver: &dyn CredentialResolver,
) -> Result<BTreeMap<String, ResolvedCredential>> {
    config.validate()?;
    let credentials = crate::config::credential_index(config.credentials())?;
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
        if credential.provider == CredentialProvider::ChatgptSubscription
            || credential.provider == CredentialProvider::ClaudeSubscription
            || credential.provider == CredentialProvider::GrokSubscription
        {
            continue;
        }
        let handle = resolve_named_credential(binding, credential, resolver)?;
        resolved.insert(binding.credential_ref.clone(), handle);
    }
    Ok(resolved)
}

fn resolve_named_credential(
    binding: &crate::config::CredentialBinding,
    credential: &crate::config::ProviderCredential,
    resolver: &dyn CredentialResolver,
) -> Result<ResolvedCredential> {
    let handle = resolver.resolve(credential).map_err(|_| {
        anyhow::anyhow!("Failed to resolve named credential '{}'; inspect the credential store without printing secret material", binding.credential_ref)
    })?;
    if handle.credential_name != binding.credential_ref {
        bail!(
            "credential resolver returned a handle for the wrong requested credential '{}'",
            binding.credential_ref
        );
    }
    Ok(handle)
}

fn preflight_named_transport(entry: &ProviderEntry) -> Result<()> {
    let ProviderEntry::Credentialed {
        provider,
        base_url,
        chat_path,
        models_path,
        model,
        ..
    } = entry
    else {
        return Ok(());
    };
    match provider {
        CredentialProvider::ChatgptSubscription
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() =>
        {
            bail!("ChatGPT subscription custom endpoints and paths are not supported")
        }
        CredentialProvider::ClaudeSubscription
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() =>
        {
            bail!("Claude subscription custom endpoints and paths are not supported")
        }
        CredentialProvider::GrokSubscription
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() =>
        {
            bail!("Grok subscription custom endpoints and paths are not supported")
        }
        CredentialProvider::GoogleVertex => bail!(
            "Google Vertex named credentials are modeled but its cloud-identity transport is not implemented"
        ),
        CredentialProvider::GeminiAiStudio
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() =>
        {
            bail!("Gemini AI Studio custom endpoints are not supported by this transport")
        }
        CredentialProvider::MetaModelApi
            if base_url.is_some() || chat_path.is_some() || models_path.is_some() =>
        {
            bail!("Meta Model API profiles are fixed to https://api.meta.ai/v1 and do not accept endpoint overrides")
        }
        CredentialProvider::MetaModelApi
            if model
                .as_deref()
                .is_some_and(|model| model != "muse-spark-1.3") =>
        {
            bail!("Meta Model API currently supports only muse-spark-1.3")
        }
        _ => Ok(()),
    }
}

/// Validate the complete provider/credential metadata graph and every named
/// transport before secret resolution, provider construction, or selection
/// shortcuts can perform external work.
pub fn preflight_provider_config(config: &Config) -> Result<()> {
    config.validate()?;
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
    for (index, entry) in config.providers.iter().enumerate() {
        preflight_named_transport(entry)
            .with_context(|| format!("Provider #{} is invalid", index + 1))?;
    }
    Ok(())
}

fn create_named_profiles_from_config_with_resolver(
    config: &Config,
    resolver: &dyn CredentialResolver,
    production_oauth: bool,
) -> Result<Vec<(String, Box<dyn LlmProvider>)>> {
    // Complete graph and transport validation happen before secret resolution
    // or the first provider constructor.
    preflight_provider_config(config)?;
    if !production_oauth
        && config.providers.iter().any(|entry| {
            matches!(
                entry,
                ProviderEntry::Credentialed {
                    provider: CredentialProvider::ChatgptSubscription
                        | CredentialProvider::ClaudeSubscription
                        | CredentialProvider::GrokSubscription,
                    ..
                }
            )
        })
    {
        bail!("Injected credential resolvers cannot fabricate a refreshable subscription lease")
    }
    let resolved = resolve_named_graph(config, resolver)?;
    let credentials = crate::config::credential_index(config.credentials())?;
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
            let provider = if let Some(binding) = entry.credential_binding() {
                let metadata = credentials
                    .get(binding.credential_ref.as_str())
                    .expect("validated credential index contains every profile reference");
                if metadata.provider == CredentialProvider::ChatgptSubscription {
                    if !production_oauth {
                        bail!("Injected credential resolvers cannot fabricate a refreshable ChatGPT subscription lease")
                    }
                    let ProviderEntry::Credentialed { model, reasoning_effort, .. } = entry else {
                        unreachable!("credential binding implies credentialed entry")
                    };
                    Ok(Box::new(ChatGptSubscriptionProvider::production(
                        metadata,
                        model.as_deref(),
                        *reasoning_effort,
                    )?) as Box<dyn LlmProvider>)
                } else if metadata.provider == CredentialProvider::ClaudeSubscription {
                    if !production_oauth {
                        bail!("Injected credential resolvers cannot fabricate a refreshable Claude subscription lease")
                    }
                    require_claude_subscription_oauth_opt_in(config)?;
                    let ProviderEntry::Credentialed { model, reasoning_effort, .. } = entry else {
                        unreachable!("credential binding implies credentialed entry")
                    };
                    Ok(Box::new(ClaudeSubscriptionProvider::production(
                        metadata,
                        model.as_deref(),
                        *reasoning_effort,
                    )?) as Box<dyn LlmProvider>)
                } else if metadata.provider == CredentialProvider::GrokSubscription {
                    if !production_oauth {
                        bail!("Injected credential resolvers cannot fabricate a refreshable Grok subscription lease")
                    }
                    let ProviderEntry::Credentialed { model, reasoning_effort, .. } = entry else {
                        unreachable!("credential binding implies credentialed entry")
                    };
                    Ok(Box::new(GrokSubscriptionProvider::production(
                        metadata,
                        model.as_deref(),
                        *reasoning_effort,
                    )?) as Box<dyn LlmProvider>)
                } else {
                    let handle = resolved
                        .get(&binding.credential_ref)
                        .expect("resolved graph contains every validated non-OAuth credential");
                    let inner = create_provider_from_resolved_entry(entry, handle)?;
                    Ok(Box::new(CredentialBoundProvider::new(inner, metadata)) as Box<dyn LlmProvider>)
                }
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
    allow_implicit_fallback: bool,
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
    let default_provider = if profiles.len() == 1 || !allow_implicit_fallback {
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
pub fn create_provider_graph_from_config(config: &Config) -> Result<ProviderGraph> {
    let profiles = create_named_profiles_from_config_with_resolver(
        config,
        &EnvironmentCredentialResolver,
        true,
    )?;
    let allow_implicit_fallback = !config
        .providers
        .iter()
        .any(|entry| matches!(entry, ProviderEntry::Credentialed { .. }));
    graph_from_boxed_profiles(profiles, allow_implicit_fallback)
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
    graph_from_boxed_profiles(
        create_named_profiles_from_config_with_resolver(config, resolver, false)?,
        false,
    )
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

/// Revalidate the complete graph with an injected credential resolver and
/// return one configured profile. Model switching and child-agent selection
/// use this same boundary as startup.
pub fn create_provider_profile_from_config_with_resolver(
    config: &Config,
    profile_name: &str,
    resolver: &dyn CredentialResolver,
) -> Result<Arc<dyn LlmProvider>> {
    if !config.providers.iter().any(|entry| !entry.is_local()) {
        let graph = create_provider_graph_from_config_with_resolver(config, resolver)?;
        return graph
            .profiles()
            .iter()
            .find(|profile| profile.profile_name() == profile_name)
            .map(|profile| Arc::clone(profile.provider()))
            .with_context(|| format!("Provider profile '{profile_name}' was not found"));
    }

    // Revalidate every profile and transport before resolving even the one
    // selected secret. Selection does not authorize reading other accounts.
    preflight_provider_config(config)?;
    let entry = config
        .providers
        .iter()
        .find(|entry| !entry.is_local() && entry.profile_name() == profile_name)
        .with_context(|| format!("Provider profile '{profile_name}' was not found"))?;
    let provider = if let Some(binding) = entry.credential_binding() {
        let credentials = crate::config::credential_index(config.credentials())?;
        let credential = credentials
            .get(binding.credential_ref.as_str())
            .expect("Config::validate checked the selected named credential reference");
        if credential.provider == CredentialProvider::ChatgptSubscription
            || credential.provider == CredentialProvider::ClaudeSubscription
            || credential.provider == CredentialProvider::GrokSubscription
        {
            bail!("Injected credential resolvers cannot fabricate a refreshable subscription lease")
        }
        let handle = resolve_named_credential(binding, credential, resolver)?;
        let inner = create_provider_from_resolved_entry(entry, &handle)?;
        Arc::new(CredentialBoundProvider::new(inner, credential)) as Arc<dyn LlmProvider>
    } else {
        Arc::from(create_provider_from_entry(entry)?)
    };
    Ok(provider)
}

/// Build one provider from an already-overlaid entry, reusing config credentials.
///
/// The clone may carry a Brain-local model or thinking overlay. Shared
/// `[[providers]]` rows are not written.
pub fn create_provider_from_overlaid_entry(
    config: &Config,
    entry: &ProviderEntry,
) -> Result<Arc<dyn LlmProvider>> {
    create_provider_from_overlaid_entry_with_resolver(config, entry, &EnvironmentCredentialResolver)
}

fn create_provider_from_overlaid_entry_with_resolver(
    config: &Config,
    entry: &ProviderEntry,
    resolver: &dyn CredentialResolver,
) -> Result<Arc<dyn LlmProvider>> {
    if entry.is_local() {
        bail!("Local providers are not constructed from the cloud provider factory");
    }
    preflight_provider_config(config)?;
    if let Some(binding) = entry.credential_binding() {
        let credentials = crate::config::credential_index(config.credentials())?;
        let credential = credentials
            .get(binding.credential_ref.as_str())
            .expect("Config::validate checked the selected named credential reference");
        if credential.provider == CredentialProvider::ChatgptSubscription {
            let ProviderEntry::Credentialed {
                model,
                reasoning_effort,
                ..
            } = entry
            else {
                unreachable!("credential binding implies credentialed entry")
            };
            return Ok(Arc::new(ChatGptSubscriptionProvider::production(
                credential,
                model.as_deref(),
                *reasoning_effort,
            )?) as Arc<dyn LlmProvider>);
        }
        if credential.provider == CredentialProvider::ClaudeSubscription {
            require_claude_subscription_oauth_opt_in(config)?;
            let ProviderEntry::Credentialed {
                model,
                reasoning_effort,
                ..
            } = entry
            else {
                unreachable!("credential binding implies credentialed entry")
            };
            return Ok(Arc::new(ClaudeSubscriptionProvider::production(
                credential,
                model.as_deref(),
                *reasoning_effort,
            )?) as Arc<dyn LlmProvider>);
        }
        if credential.provider == CredentialProvider::GrokSubscription {
            let ProviderEntry::Credentialed {
                model,
                reasoning_effort,
                ..
            } = entry
            else {
                unreachable!("credential binding implies credentialed entry")
            };
            return Ok(Arc::new(GrokSubscriptionProvider::production(
                credential,
                model.as_deref(),
                *reasoning_effort,
            )?) as Arc<dyn LlmProvider>);
        }
        let handle = resolve_named_credential(binding, credential, resolver)?;
        let inner = create_provider_from_resolved_entry(entry, &handle)?;
        Ok(Arc::new(CredentialBoundProvider::new(inner, credential)) as Arc<dyn LlmProvider>)
    } else {
        Ok(Arc::from(create_provider_from_entry(entry)?))
    }
}

/// Create the ordered cloud provider pool from unified configuration.
pub fn create_providers_from_config(config: &Config) -> Result<Vec<Box<dyn LlmProvider>>> {
    Ok(create_named_profiles_from_config_with_resolver(
        config,
        &EnvironmentCredentialResolver,
        true,
    )?
    .into_iter()
    .map(|(_, provider)| provider)
    .collect())
}

/// Create the active provider or fallback chain from unified configuration.
pub fn create_provider_from_config(config: &Config) -> Result<Box<dyn LlmProvider>> {
    let mut providers = create_providers_from_config(config)?;
    if providers.len() == 1
        || config
            .providers
            .iter()
            .any(|entry| matches!(entry, ProviderEntry::Credentialed { .. }))
    {
        return Ok(providers.remove(0));
    }
    Ok(Box::new(super::FallbackChain::new(providers)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderEntry;
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ExecutionTarget, OpenAiCompatibleCapabilities,
        OpenAiCompatibleToolChoice, ProviderCredential, ResolvedSecret,
    };
    use crate::models::{InferenceProvider, ModelFamily, ModelSize};
    use crate::providers::{ContentBlock, Message};
    use finch_providers::{ToolDefinition, ToolInputSchema};
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn entry(provider: &str, api_key: &str) -> ProviderEntry {
        match provider {
            "openai" => ProviderEntry::Openai {
                api_key: api_key.to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
                reasoning_effort: None,
            },
            "grok" => ProviderEntry::Grok {
                api_key: api_key.to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            },
            "gemini" => ProviderEntry::Gemini {
                api_key: api_key.to_string(),
                model: None,
                name: None,
            },
            "mistral" => ProviderEntry::Mistral {
                api_key: api_key.to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            },
            "groq" => ProviderEntry::Groq {
                api_key: api_key.to_string(),
                model: None,
                name: None,
            },
            _ => ProviderEntry::Claude {
                api_key: api_key.to_string(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: None,
            },
        }
    }

    fn entry_with_model(provider: &str, api_key: &str, model: &str) -> ProviderEntry {
        let mut e = entry(provider, api_key);
        match &mut e {
            ProviderEntry::Openai { model: m, .. }
            | ProviderEntry::Claude { model: m, .. }
            | ProviderEntry::Grok { model: m, .. }
            | ProviderEntry::Gemini { model: m, .. }
            | ProviderEntry::Mistral { model: m, .. }
            | ProviderEntry::Groq { model: m, .. } => *m = Some(model.to_string()),
            _ => unreachable!("entry() only builds simple cloud variants"),
        }
        e
    }

    fn pentry(variant: ProviderEntry) -> ProviderEntry {
        variant
    }

    struct CountingResolver {
        calls: AtomicUsize,
    }

    struct LeakyResolver;

    struct NamedAccountResolver;

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

    impl CredentialResolver for NamedAccountResolver {
        fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
            let secret = match credential.name.as_str() {
                "account-a" => "account-a-key",
                "account-b" => "account-b-key",
                other => anyhow::bail!("unexpected credential '{other}'"),
            };
            Ok(ResolvedCredential {
                credential_name: credential.name.clone(),
                secret: ResolvedSecret::new(secret)?,
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

    fn named_meta(profile_name: &str, credential_ref: &str) -> ProviderEntry {
        ProviderEntry::Credentialed {
            provider: CredentialProvider::MetaModelApi,
            credential: CredentialBinding {
                credential_ref: credential_ref.into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: BTreeSet::new(),
            },
            model: Some("muse-spark-1.3".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(profile_name.into()),
            reasoning_effort: Some(crate::config::ReasoningEffort::High),
        }
    }

    fn meta_credential(name: &str) -> ProviderCredential {
        ProviderCredential {
            name: name.into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::MetaModelApi,
            issuer: "meta-model-api".into(),
            audience: AudienceBinding::standard(EndpointFamily::MetaModelApi),
            tenant: None,
            project: None,
            account: None,
            scopes: BTreeSet::new(),
            secret_ref: format!("test:{name}"),
            lifecycle: CredentialLifecycle::Active {
                expires_at: None,
                refreshable: false,
            },
            revocation: Default::default(),
        }
    }

    #[test]
    fn test_meta_named_profile_constructs_truthful_model_identity_and_rejects_overrides() {
        let profile = named_meta("muse", "meta-work");
        let config = Config::with_providers(vec![profile.clone()])
            .with_credentials(vec![meta_credential("meta-work")]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let graph = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .expect("the origin-bound Meta profile must construct");
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        assert_eq!(graph.profiles()[0].profile_name(), "muse");
        assert_eq!(graph.profiles()[0].provider().name(), "meta_model_api");
        assert_eq!(
            graph.profiles()[0].provider().default_model(),
            "muse-spark-1.3"
        );
        assert!(graph.profiles()[0]
            .capabilities()
            .unwrap()
            .tools
            .is_supported());

        let mut overridden = profile;
        if let ProviderEntry::Credentialed { chat_path, .. } = &mut overridden {
            *chat_path = Some("/compatible-but-not-meta".into());
        }
        let invalid = Config::with_providers(vec![overridden])
            .with_credentials(vec![meta_credential("meta-work")]);
        let calls_before = resolver.calls.load(Ordering::SeqCst);
        let error = match create_provider_graph_from_config_with_resolver(&invalid, &resolver) {
            Ok(_) => panic!("Meta profiles must not silently redirect their credential"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("do not accept endpoint overrides"),
            "{error}"
        );
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            calls_before,
            "unsafe Meta endpoint overrides must fail before secret resolution"
        );

        let mut future_model = named_meta("future-muse", "meta-work");
        if let ProviderEntry::Credentialed { model, .. } = &mut future_model {
            *model = Some("muse-spark-future".into());
        }
        let invalid = Config::with_providers(vec![future_model])
            .with_credentials(vec![meta_credential("meta-work")]);
        let error = match create_provider_graph_from_config_with_resolver(&invalid, &resolver) {
            Ok(_) => panic!("undocumented future Muse models must remain unavailable"),
            Err(error) => format!("{error:#}"),
        };
        assert!(
            error.contains("currently supports only muse-spark-1.3"),
            "{error}"
        );
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            calls_before,
            "unknown Meta models must fail before secret resolution"
        );
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
            revocation: Default::default(),
        }
    }

    fn named_openai_at(
        profile_name: &str,
        credential_ref: &str,
        account: &str,
        endpoint: &str,
    ) -> ProviderEntry {
        let mut profile = named_openai(profile_name, credential_ref, "gpt-4o");
        if let ProviderEntry::Credentialed {
            credential,
            base_url,
            ..
        } = &mut profile
        {
            credential.account = Some(account.into());
            credential.audience = Some(AudienceBinding::custom(endpoint).unwrap());
            *base_url = Some(endpoint.into());
        }
        profile
    }

    fn named_openai_credential_at(name: &str, account: &str, endpoint: &str) -> ProviderCredential {
        let mut credential = named_openai_credential(name, account);
        credential.audience = AudienceBinding::custom(endpoint).unwrap();
        credential
    }

    fn generic_openai_compatible(
        name: &str,
        credential_ref: &str,
        endpoint: &str,
    ) -> ProviderEntry {
        ProviderEntry::OpenAiCompatible {
            name: name.into(),
            base_url: format!("{}/v1", endpoint.trim_end_matches('/')),
            chat_path: Some("/chat/completions".into()),
            models_path: Some("/models".into()),
            model: "main".into(),
            credential: CredentialBinding {
                credential_ref: credential_ref.into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: BTreeSet::new(),
            },
            capabilities: OpenAiCompatibleCapabilities {
                streaming: Some(true),
                tools: Some(true),
                parallel_tool_calls: Some(false),
                image_input: Some(false),
                context_window_tokens: Some(262_144),
                max_output_tokens: Some(65_536),
            },
            tool_choice: OpenAiCompatibleToolChoice::Auto,
            strict_tool_schemas: Some(false),
        }
    }

    fn generic_openai_compatible_credential(name: &str, endpoint: &str) -> ProviderCredential {
        ProviderCredential {
            name: name.into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiCompatible,
            issuer: "openai-compatible".into(),
            audience: AudienceBinding::custom(endpoint).unwrap(),
            tenant: None,
            project: None,
            account: None,
            scopes: BTreeSet::new(),
            secret_ref: format!("test:{name}"),
            lifecycle: CredentialLifecycle::default(),
            revocation: Default::default(),
        }
    }

    fn configured_compatible_provider_at(
        endpoint: &str,
    ) -> (Arc<dyn LlmProvider>, Arc<AtomicUsize>) {
        let config = Config::with_providers(vec![generic_openai_compatible(
            "configured-boundary",
            "configured-boundary-key",
            endpoint,
        )])
        .with_credentials(vec![generic_openai_compatible_credential(
            "configured-boundary-key",
            endpoint,
        )]);
        let calls = Arc::new(AtomicUsize::new(0));
        struct BoundaryResolver(Arc<AtomicUsize>);
        impl CredentialResolver for BoundaryResolver {
            fn resolve(&self, credential: &ProviderCredential) -> Result<ResolvedCredential> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ResolvedCredential {
                    credential_name: credential.name.clone(),
                    secret: ResolvedSecret::new("factory-boundary-sentinel-secret")?,
                })
            }
        }
        let graph = create_provider_graph_from_config_with_resolver(
            &config,
            &BoundaryResolver(Arc::clone(&calls)),
        )
        .expect("real Config and origin-bound resolver must construct the compatible profile");
        (graph.default_provider(), calls)
    }

    async fn configured_factory_stream_outcome(
        endpoint: &str,
        request: &ProviderRequest,
    ) -> (Vec<StreamChunk>, Vec<String>) {
        let (provider, _) = configured_compatible_provider_at(endpoint);
        let result = provider.send_message_stream(request).await;
        let mut chunks = Vec::new();
        let mut errors = Vec::new();
        match result {
            Ok(mut receiver) => {
                while let Some(item) = receiver.recv().await {
                    match item {
                        Ok(chunk) => chunks.push(chunk),
                        Err(error) => errors.push(format!("{error:#}")),
                    }
                }
            }
            Err(error) => errors.push(format!("{error:#}")),
        }
        (chunks, errors)
    }

    async fn factory_stalling_server(
        send_sse_headers: bool,
    ) -> (
        String,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("factory fixture must bind a kernel-assigned loopback port");
        let address = listener.local_addr().expect("fixture address must resolve");
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("fixture must accept");
            let mut request = vec![0u8; 16 * 1024];
            let _ = socket.read(&mut request).await;
            if send_sse_headers {
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                    )
                    .await
                    .expect("fixture must write SSE headers");
                socket.flush().await.expect("fixture headers must flush");
            }
            let _ = accepted_tx.send(());
            let mut byte = [0u8; 1];
            while matches!(socket.read(&mut byte).await, Ok(1)) {}
            let _ = closed_tx.send(());
        });
        (format!("http://{address}"), accepted_rx, closed_rx)
    }

    async fn factory_disconnect_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("disconnect fixture must bind a kernel-assigned loopback port");
        let address = listener
            .local_addr()
            .expect("disconnect fixture address must resolve");
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("fixture must accept");
            let mut request = vec![0u8; 16 * 1024];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 4096\r\nconnection: close\r\n\r\ndata: {\"id\":",
                )
                .await
                .expect("disconnect fixture must write its partial response");
            socket.flush().await.expect("partial response must flush");
        });
        format!("http://{address}")
    }

    // -----------------------------------------------------------------------
    // Single-entry construction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_create_claude_provider() {
        let provider = create_provider_from_entry(&entry("claude", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "claude");
    }

    #[test]
    fn test_create_openai_provider() {
        let provider = create_provider_from_entry(&entry("openai", "test-key"));
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
        let default = graph.default_provider();
        assert!(Arc::ptr_eq(&default, graph.profiles()[0].provider()));
    }

    #[test]
    fn test_generic_openai_compatible_profile_builds_from_endpoint_bound_credential() {
        let endpoint = "https://compatible.example";
        let config = Config::with_providers(vec![generic_openai_compatible(
            "ciru", "ciru-key", endpoint,
        )])
        .with_credentials(vec![generic_openai_compatible_credential(
            "ciru-key", endpoint,
        )]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };

        let graph = create_provider_graph_from_config_with_resolver(&config, &resolver).unwrap();
        let provider = graph.default_provider();
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
        assert_eq!(graph.profiles()[0].profile_name(), "ciru");
        assert_eq!(provider.name(), "ciru");
        assert_eq!(provider.default_model(), "main");
        let capabilities = provider.capabilities("main");
        assert!(capabilities.streaming.is_supported());
        assert!(capabilities.tools.is_supported());
        assert_eq!(capabilities.context_window.max_tokens, Some(262_144));
        assert_eq!(capabilities.output_token_limit.max_tokens, Some(65_536));
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_public_dispatch_enforces_strict_bounded_responses()
    {
        let mut nonstream_server = mockito::Server::new_async().await;
        let nonstream_mock = nonstream_server
            .mock("POST", "/v1/chat/completions")
            .match_header(
                "authorization",
                "Bearer factory-boundary-sentinel-secret",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"id":"chat-1","object":"chat.completion","model":"main","choices":[{"index":0,"message":{"role":"assistant","content":"bounded"},"finish_reason":"stop"}]}"#,
            )
            .create_async()
            .await;
        let (provider, resolutions) = configured_compatible_provider_at(&nonstream_server.url());
        let response = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("factory-built configured-compatible nonstream dispatch must succeed");
        assert_eq!(
            response.content,
            vec![ContentBlock::Text {
                text: "bounded".into()
            }],
            "factory-built compatible response changed successful content ordering"
        );
        assert_eq!(
            resolutions.load(Ordering::SeqCst),
            1,
            "factory must resolve the exact origin-bound credential once"
        );
        nonstream_mock.assert_async().await;

        let body = concat!(
            "data: {\"id\":\"chat-2\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"streamed\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-2\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut stream_server = mockito::Server::new_async().await;
        let stream_mock = stream_server
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer factory-boundary-sentinel-secret")
            .with_status(200)
            .with_header("content-type", "text/event-stream; charset=utf-8")
            .with_body(body)
            .create_async()
            .await;
        let (provider, _) = configured_compatible_provider_at(&stream_server.url());
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("factory-built configured-compatible stream must start");
        let mut completed = Vec::new();
        while let Some(item) = receiver.recv().await {
            if let StreamChunk::ContentBlockComplete(block) =
                item.expect("valid factory-built stream must not fail")
            {
                completed.push(block);
            }
        }
        assert_eq!(
            completed,
            vec![ContentBlock::Text {
                text: "streamed".into()
            }],
            "factory-built compatible stream changed successful completion ordering"
        );
        stream_mock.assert_async().await;

        let mut malformed_server = mockito::Server::new_async().await;
        malformed_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body("data: not-json\n\ndata: [DONE]\n\n")
            .create_async()
            .await;
        let (provider, _) = configured_compatible_provider_at(&malformed_server.url());
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("malformed body is diagnosed by the returned stream");
        let first = receiver
            .recv()
            .await
            .expect("malformed stream must emit its terminal error")
            .expect_err("malformed stream must not emit a successful chunk");
        assert!(
            format!("{first:#}").contains("malformed JSON"),
            "factory dispatch surfaced the wrong malformed-stream error: {first:#}"
        );
        assert!(
            receiver.recv().await.is_none(),
            "factory dispatch emitted a late outcome after its parse failure"
        );
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_rejects_adversarial_sse_framing_and_bounds() {
        const MIB: usize = 1024 * 1024;
        let terminal = concat!(
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let oversized_tool_arguments = "x".repeat(MIB + 1);
        let cases = vec![
            (
                "wrong content type",
                "application/json",
                terminal.to_string(),
            ),
            (
                "missing DONE",
                "text/event-stream",
                terminal.replace("data: [DONE]\n\n", ""),
            ),
            (
                "duplicate DONE",
                "text/event-stream",
                format!("{terminal}data: [DONE]\n\n"),
            ),
            (
                "late SSE data",
                "text/event-stream",
                format!(
                    "{terminal}data: {{\"id\":\"late\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[]}}\n\n"
                ),
            ),
            (
                "DONE without blank terminator",
                "text/event-stream",
                terminal
                    .strip_suffix('\n')
                    .expect("terminal fixture ends with a blank delimiter")
                    .to_string(),
            ),
            (
                "oversized SSE line",
                "text/event-stream",
                format!("data: {}\n\n", "x".repeat(MIB)),
            ),
            (
                "oversized SSE aggregate",
                "text/event-stream",
                format!(":{}\n", "x".repeat(MIB - 2)).repeat(5),
            ),
            (
                "oversized tool arguments",
                "text/event-stream",
                format!(
                    concat!(
                        "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{{\"name\":\"read\",\"arguments\":\"{}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
                        "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
                        "data: [DONE]\n\n"
                    ),
                    oversized_tool_arguments
                ),
            ),
        ];

        for (case, content_type, body) in cases {
            let mut server = mockito::Server::new_async().await;
            server
                .mock("POST", "/v1/chat/completions")
                .with_status(200)
                .with_header("content-type", content_type)
                .with_body(body)
                .create_async()
                .await;
            let request = ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![
                ToolDefinition {
                    name: "read".into(),
                    description: "read".into(),
                    input_schema: ToolInputSchema::simple(vec![]),
                },
            ]);
            let (chunks, errors) = configured_factory_stream_outcome(&server.url(), &request).await;
            assert!(
                !chunks
                    .iter()
                    .any(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_))),
                "factory-boundary {case} published a successful completion: {chunks:?}"
            );
            assert_eq!(
                errors.len(),
                1,
                "factory-boundary {case} must emit exactly one terminal error: chunks={chunks:?}, errors={errors:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_bounds_nonstream_success_body() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(vec![b'x'; 32 * 1024 * 1024 + 1])
            .create_async()
            .await;
        let (provider, _) = configured_compatible_provider_at(&server.url());
        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect_err("factory-built provider must reject an oversized success body");
        assert!(
            format!("{error:#}").contains("32 MiB"),
            "factory-built provider reported the wrong oversized-success diagnostic: {error:#}"
        );
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_redacts_reflected_credentials_and_releases_on_drop()
    {
        let secret = "factory-boundary-sentinel-secret";
        let request =
            ProviderRequest::new(vec![Message::user("hello")]).with_tools(vec![ToolDefinition {
                name: "read".into(),
                description: "read".into(),
                input_schema: ToolInputSchema::simple(vec![]),
            }]);
        let reflected_nonstream = serde_json::json!({
            "id": "chat-1",
            "object": "chat.completion",
            "model": "main",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-1",
                        "type": "function",
                        "function": { "name": secret, "arguments": "{}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut reflected_nonstream_server = mockito::Server::new_async().await;
        reflected_nonstream_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_vec(&reflected_nonstream).unwrap())
            .create_async()
            .await;
        let (provider, _) = configured_compatible_provider_at(&reflected_nonstream_server.url());
        let error = provider
            .send_message(&request)
            .await
            .expect_err("factory-built provider must reject an unadvertised reflected tool name");
        assert!(
            !format!("{error:#}").contains(secret),
            "factory nonstream schema diagnostic leaked the resolved credential: {error:#}"
        );

        let reflected_stream = format!(
            concat!(
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{{\"name\":\"{}\",\"arguments\":\"{{}}\"}}}}]}},\"finish_reason\":null}}]}}\n\n",
                "data: {{\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"model\":\"main\",\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"tool_calls\"}}]}}\n\n",
                "data: [DONE]\n\n"
            ),
            secret
        );
        let mut reflected_stream_server = mockito::Server::new_async().await;
        reflected_stream_server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(reflected_stream)
            .create_async()
            .await;
        let (chunks, errors) =
            configured_factory_stream_outcome(&reflected_stream_server.url(), &request).await;
        assert!(
            !chunks
                .iter()
                .any(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_))),
            "factory streaming schema failure published a completion: {chunks:?}"
        );
        assert_eq!(
            errors.len(),
            1,
            "factory streaming schema failure must emit exactly one error: {errors:?}"
        );
        assert!(
            !errors[0].contains(secret),
            "factory streaming schema diagnostic leaked the resolved credential: {}",
            errors[0]
        );

        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/v1/chat/completions")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(format!(
                r#"{{"error":{{"message":"Authorization: Bearer {secret} {}"}}}}"#,
                "x".repeat(65 * 1024)
            ))
            .create_async()
            .await;
        let (provider, _) = configured_compatible_provider_at(&server.url());
        let error = provider
            .send_message(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect_err("factory-built configured-compatible 400 must fail");
        let displayed = format!("{error:#}");
        assert!(displayed.contains("response body redacted"), "{displayed}");
        assert!(
            !displayed.contains(secret),
            "secret leaked in error: {displayed}"
        );
        assert!(
            displayed.len() < 1024,
            "factory error diagnostic was not bounded: {} bytes",
            displayed.len()
        );

        let (endpoint, accepted, closed) = factory_stalling_server(true).await;
        let (provider, _) = configured_compatible_provider_at(&endpoint);
        let receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("factory-built stream must return after SSE headers");
        accepted
            .await
            .expect("factory receiver-drop fixture did not accept the request");
        drop(receiver);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("factory receiver drop did not promptly close upstream")
            .expect("factory receiver-drop fixture lost its closure signal");
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_cancellation_releases_upstream_without_late_result()
    {
        let (endpoint, accepted, closed) = factory_stalling_server(false).await;
        let (provider, _) = configured_compatible_provider_at(&endpoint);
        let dispatch = tokio::spawn(async move {
            provider
                .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
                .await
        });
        accepted
            .await
            .expect("factory cancellation fixture did not accept the request");
        dispatch.abort();
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("factory cancellation did not promptly close upstream")
            .expect("factory cancellation fixture lost its closure signal");
        assert!(
            dispatch
                .await
                .expect_err("aborted dispatch must not complete")
                .is_cancelled(),
            "cancelled factory dispatch produced a late non-cancellation outcome"
        );
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_post_header_timeout_is_one_terminal_error() {
        let (endpoint, accepted, closed) = factory_stalling_server(true).await;
        let (provider, _) = configured_compatible_provider_at(&endpoint);
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("factory-built stream must return after SSE headers");
        accepted
            .await
            .expect("factory timeout fixture did not accept the request");
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        let error = receiver
            .recv()
            .await
            .expect("factory timeout stream ended without its terminal error")
            .expect_err("factory timeout must not emit a successful chunk");
        assert!(
            format!("{error:#}")
                .to_ascii_lowercase()
                .contains("timed out"),
            "factory timeout reported the wrong terminal diagnostic: {error:#}"
        );
        assert!(
            receiver.recv().await.is_none(),
            "factory timeout emitted more than one terminal outcome"
        );
        closed
            .await
            .expect("factory timeout did not release the upstream transport");
    }

    #[tokio::test]
    async fn test_configured_compatible_factory_disconnect_is_one_terminal_error_without_completion(
    ) {
        let endpoint = factory_disconnect_server().await;
        let (provider, _) = configured_compatible_provider_at(&endpoint);
        let mut receiver = provider
            .send_message_stream(&ProviderRequest::new(vec![Message::user("hello")]))
            .await
            .expect("disconnect fixture must return after SSE headers");
        let mut errors = Vec::new();
        let mut completions = Vec::new();
        while let Some(item) = receiver.recv().await {
            match item {
                Ok(StreamChunk::ContentBlockComplete(block)) => completions.push(block),
                Ok(_) => {}
                Err(error) => errors.push(format!("{error:#}")),
            }
        }
        assert!(
            completions.is_empty(),
            "transport disconnect produced a successful completion: {completions:?}"
        );
        assert_eq!(
            errors.len(),
            1,
            "transport disconnect must emit one terminal error: {errors:?}"
        );
    }

    #[test]
    fn test_generic_openai_compatible_rejects_cross_origin_path_before_resolution() {
        let endpoint = "https://compatible.example";
        let mut profile = generic_openai_compatible("ciru", "ciru-key", endpoint);
        if let ProviderEntry::OpenAiCompatible { chat_path, .. } = &mut profile {
            *chat_path = Some("https://attacker.example/v1/chat/completions".into());
        }
        let config = Config::with_providers(vec![profile]).with_credentials(vec![
            generic_openai_compatible_credential("ciru-key", endpoint),
        ]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };

        let error = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .err()
            .expect("cross-origin authenticated path must fail closed");
        assert!(format!("{error:#}").contains("unsafe endpoint override"));
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_startup_default_uses_first_named_account_without_implicit_fallback() {
        let mut server_a = mockito::Server::new_async().await;
        let mut server_b = mockito::Server::new_async().await;
        let account_a = server_a
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer account-a-key")
            .with_status(200)
            .with_body(r#"{"id":"chat-a","object":"chat.completion","created":1,"model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"account-a"},"finish_reason":"stop"}]}"#)
            .create_async()
            .await;
        let account_b = server_b
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer account-b-key")
            .expect(0)
            .create_async()
            .await;
        let config = Config::with_providers(vec![
            named_openai_at("profile-a", "account-a", "a", &server_a.url()),
            named_openai_at("profile-b", "account-b", "b", &server_b.url()),
        ])
        .with_credentials(vec![
            named_openai_credential_at("account-a", "a", &server_a.url()),
            named_openai_credential_at("account-b", "b", &server_b.url()),
        ]);

        create_provider_graph_from_config_with_resolver(&config, &NamedAccountResolver)
            .unwrap()
            .default_provider()
            .send_message(&ProviderRequest::new(vec![
                crate::providers::Message::user("startup"),
            ]))
            .await
            .unwrap();

        account_a.assert_async().await;
        account_b.assert_async().await;
    }

    #[tokio::test]
    async fn test_model_switch_selects_exact_named_account_without_fallback() {
        let mut server_a = mockito::Server::new_async().await;
        let mut server_b = mockito::Server::new_async().await;
        let account_a = server_a
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer account-a-key")
            .expect(0)
            .create_async()
            .await;
        let account_b = server_b
            .mock("POST", "/v1/chat/completions")
            .match_header("authorization", "Bearer account-b-key")
            .with_status(200)
            .with_body(r#"{"id":"chat-b","object":"chat.completion","created":1,"model":"gpt-4o","choices":[{"index":0,"message":{"role":"assistant","content":"account-b"},"finish_reason":"stop"}]}"#)
            .create_async()
            .await;
        let config = Config::with_providers(vec![
            named_openai_at("profile-a", "account-a", "a", &server_a.url()),
            named_openai_at("profile-b", "account-b", "b", &server_b.url()),
        ])
        .with_credentials(vec![
            named_openai_credential_at("account-a", "a", &server_a.url()),
            named_openai_credential_at("account-b", "b", &server_b.url()),
        ]);

        create_provider_profile_from_config_with_resolver(
            &config,
            "profile-b",
            &NamedAccountResolver,
        )
        .unwrap()
        .send_message(&ProviderRequest::new(vec![
            crate::providers::Message::user("switch"),
        ]))
        .await
        .unwrap();

        account_a.assert_async().await;
        account_b.assert_async().await;
    }

    #[test]
    fn test_unsupported_named_transports_reject_before_secret_resolution() {
        let cases = [
            (
                CredentialProvider::ChatgptSubscription,
                CredentialKind::Bearer,
                "openai-chatgpt",
                EndpointFamily::ChatgptSubscription,
                false,
            ),
            (
                CredentialProvider::ClaudeSubscription,
                CredentialKind::OauthBrowserPkce,
                "anthropic-claude",
                EndpointFamily::ClaudeSubscription,
                false,
            ),
            (
                CredentialProvider::GrokSubscription,
                CredentialKind::OauthDevice,
                "xai-grok",
                EndpointFamily::GrokSubscription,
                false,
            ),
            (
                CredentialProvider::GoogleVertex,
                CredentialKind::CloudIdentity,
                "google-cloud",
                EndpointFamily::GoogleVertex,
                false,
            ),
            (
                CredentialProvider::GeminiAiStudio,
                CredentialKind::ApiKey,
                "google-ai-studio",
                EndpointFamily::GeminiAiStudio,
                true,
            ),
        ];
        for (provider, kind, issuer, family, custom_gemini) in cases {
            let mut profile = ProviderEntry::Credentialed {
                provider,
                credential: CredentialBinding {
                    credential_ref: "work".into(),
                    audience: None,
                    tenant: None,
                    project: None,
                    account: None,
                    required_scopes: BTreeSet::new(),
                },
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("primary".into()),
                reasoning_effort: None,
            };
            if custom_gemini {
                if let ProviderEntry::Credentialed { base_url, .. } = &mut profile {
                    *base_url = Some("https://custom.example".into());
                }
            }
            let credential = ProviderCredential {
                name: "work".into(),
                kind,
                provider,
                issuer: issuer.into(),
                audience: if custom_gemini {
                    AudienceBinding::custom("https://custom.example").unwrap()
                } else {
                    AudienceBinding::standard(family)
                },
                tenant: None,
                project: None,
                account: None,
                scopes: BTreeSet::new(),
                secret_ref: "test:work".into(),
                lifecycle: CredentialLifecycle::default(),
                revocation: Default::default(),
            };
            let config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
            let resolver = CountingResolver {
                calls: AtomicUsize::new(0),
            };
            assert!(create_provider_graph_from_config_with_resolver(&config, &resolver).is_err());
            assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn test_claude_subscription_construction_refuses_without_explicit_opt_in() {
        let profile = ProviderEntry::Credentialed {
            provider: CredentialProvider::ClaudeSubscription,
            credential: CredentialBinding {
                credential_ref: "work".into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: BTreeSet::new(),
            },
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("claude".into()),
            reasoning_effort: None,
        };
        let credential = ProviderCredential {
            name: "work".into(),
            kind: CredentialKind::OauthBrowserPkce,
            provider: CredentialProvider::ClaudeSubscription,
            issuer: "anthropic-claude".into(),
            audience: AudienceBinding::standard(EndpointFamily::ClaudeSubscription),
            tenant: None,
            project: None,
            account: Some("acct-work".into()),
            scopes: BTreeSet::new(),
            secret_ref: "oauth-store:work".into(),
            lifecycle: CredentialLifecycle::default(),
            revocation: Default::default(),
        };
        let mut config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        assert!(
            !config.features.claude_subscription_oauth_enabled,
            "the opt-in flag must default to false for this to be a meaningful test"
        );

        // Disabled by default: refused before ever touching the OAuth store
        // or the network, even on the real `production_oauth = true` path.
        let error = create_providers_from_config(&config)
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("disabled by default"),
            "expected the opt-in refusal, got: {error}"
        );

        // Opting in removes the gate: construction only leases/refreshes the
        // credential lazily on first use, so it succeeds here without ever
        // touching the (possibly absent) on-disk OAuth store.
        config.features.claude_subscription_oauth_enabled = true;
        create_providers_from_config(&config)
            .expect("opting in must let a validly-shaped Claude subscription profile construct");
    }

    #[tokio::test]
    async fn test_live_config_revocation_invalidates_constructed_provider_before_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut profile = named_openai("primary", "work", "gpt-4o");
        if let ProviderEntry::Credentialed { base_url, .. } = &mut profile {
            *base_url = Some(origin.clone());
        }
        let mut credential = named_openai_credential("work", "account-1");
        credential.audience = AudienceBinding::custom(&origin).unwrap();
        let mut config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let provider = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .unwrap()
            .default_provider();
        config.revoke_credential("work").unwrap();

        let error = provider
            .send_message(&ProviderRequest::new(Vec::new()).with_model("gpt-4o"))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("revoked after provider construction"));
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn test_live_config_deletion_invalidates_constructed_provider_before_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut profile = named_openai("primary", "work", "gpt-4o");
        if let ProviderEntry::Credentialed { base_url, .. } = &mut profile {
            *base_url = Some(origin.clone());
        }
        let mut credential = named_openai_credential("work", "account-1");
        credential.audience = AudienceBinding::custom(&origin).unwrap();
        let mut config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let provider = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .unwrap()
            .default_provider();
        config.delete_credential("work").unwrap();

        assert!(provider
            .send_message(&ProviderRequest::new(Vec::new()).with_model("gpt-4o"))
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked after provider construction"));
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn test_public_credential_replacement_invalidates_live_provider_before_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut profile = named_openai("primary", "work", "gpt-4o");
        if let ProviderEntry::Credentialed { base_url, .. } = &mut profile {
            *base_url = Some(origin.clone());
        }
        let mut credential = named_openai_credential("work", "account-1");
        credential.audience = AudienceBinding::custom(&origin).unwrap();
        let config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let provider = create_provider_graph_from_config_with_resolver(&config, &resolver)
            .unwrap()
            .default_provider();
        let _replacement = config.with_credentials(Vec::new());

        assert!(provider
            .send_message(&ProviderRequest::new(Vec::new()).with_model("gpt-4o"))
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked after provider construction"));
        assert!(matches!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        ));
    }

    #[tokio::test]
    async fn test_reconnect_revalidates_revocation_before_resolution_or_socket_activity() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let profile = named_openai_at("primary", "work", "account-1", &origin);
        let credential = named_openai_credential_at("work", "account-1", &origin);
        let mut config = Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let resolver = CountingResolver {
            calls: AtomicUsize::new(0),
        };
        let connected =
            create_provider_profile_from_config_with_resolver(&config, "primary", &resolver)
                .unwrap();
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);

        config.revoke_credential("work").unwrap();
        let reconnect_error =
            create_provider_profile_from_config_with_resolver(&config, "primary", &resolver)
                .err()
                .expect("reconnect must revalidate lifecycle");
        assert!(format!("{reconnect_error:#}").contains("revoked"));
        assert_eq!(
            resolver.calls.load(Ordering::SeqCst),
            1,
            "reconnect resolved a revoked credential"
        );
        assert!(connected
            .send_message(&ProviderRequest::new(Vec::new()).with_model("gpt-4o"))
            .await
            .unwrap_err()
            .to_string()
            .contains("revoked after provider construction"));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
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
        let provider = create_provider_from_entry(&entry("grok", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "grok");
    }

    #[test]
    fn test_create_gemini_provider() {
        let provider = create_provider_from_entry(&entry("gemini", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "gemini");
    }

    #[test]
    fn test_create_mistral_provider() {
        let provider = create_provider_from_entry(&entry("mistral", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "mistral");
    }

    #[test]
    fn test_create_groq_provider() {
        let provider = create_provider_from_entry(&entry("groq", "test-key"));
        assert!(provider.is_ok());
        assert_eq!(provider.unwrap().name(), "groq");
    }

    #[test]
    fn test_create_openrouter_provider_uses_openrouter_defaults() {
        let provider = create_provider_from_entry(&ProviderEntry::Openrouter {
            api_key: "test-key".into(),
            model: None,
            base_url: None,
            chat_path: None,
            models_path: None,
            name: None,
        })
        .expect("OpenRouter profile must use the OpenAI-compatible transport");
        assert_eq!(provider.name(), "openrouter");
        assert_eq!(provider.default_model(), "z-ai/glm-5.3-flash");
    }

    #[test]
    fn test_empty_cloud_entries_returns_error() {
        let result = create_providers_from_entries(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_entries_preserve_priority_order() {
        let entries = vec![entry("openai", "key-1"), entry("claude", "key-2")];
        let providers = create_providers_from_entries(&entries).unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "openai");
        assert_eq!(providers[1].name(), "claude");
    }

    #[test]
    fn test_single_entry_returns_direct_provider_not_fallback() {
        let entries = vec![entry("claude", "key-1")];
        let provider = create_provider_from_entries(&entries).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    #[test]
    fn test_custom_model_is_applied() {
        let e = entry_with_model("openai", "key", "gpt-4o-mini");
        let provider = create_provider_from_entry(&e).unwrap();
        assert_eq!(provider.default_model(), "gpt-4o-mini");
    }

    #[test]
    fn test_same_provider_different_models() {
        let entries = vec![
            ProviderEntry::Openai {
                api_key: "test-key".to_string(),
                model: Some("gpt-4o".to_string()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("GPT-4o (best)".to_string()),
                reasoning_effort: None,
            },
            ProviderEntry::Openai {
                api_key: "test-key".to_string(),
                model: Some("gpt-5.6-sol".to_string()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("GPT-5.6 Sol".to_string()),
                reasoning_effort: None,
            },
        ];

        let providers = create_providers_from_entries(&entries).unwrap();
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
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
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
                inference_provider: InferenceProvider::LlamaCpp,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_path: None,
                managed_artifact: None,
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
            inference_provider: InferenceProvider::LlamaCpp,
            execution_target: ExecutionTarget::Auto,
            model_family: ModelFamily::Qwen2,
            model_size: ModelSize::Medium,
            model_path: None,
            managed_artifact: None,
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
        let error = create_provider_graph_from_config(&config)
            .err()
            .expect("legacy-only graph must be rejected");
        let message = error.to_string();
        assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));
        assert!(message.contains("finch setup"));
        assert!(message.contains("subscription credentials are not API keys"));
    }

    #[test]
    fn test_shared_startup_provider_constructs_from_provider_entries() {
        let config = Config::new(vec![entry("claude", "sk-ant-startup-key-1234567890")]);
        let provider = create_provider_from_config(&config).unwrap();
        assert_eq!(provider.name(), "claude");
    }

    /// No cloud `[[providers]]` entry must fail closed rather than silently
    /// forward to an unconfigured provider (the explicit-fallback rule).
    #[test]
    fn test_no_cloud_provider_entries_fail_closed() {
        let config = Config::new(vec![]);
        let error = create_provider_from_config(&config)
            .err()
            .expect("a configuration with no cloud providers must fail closed");
        assert!(
            error
                .to_string()
                .contains("No cloud provider entries configured"),
            "the failure must name the missing cloud provider list, got: {error:#}"
        );
    }
}
