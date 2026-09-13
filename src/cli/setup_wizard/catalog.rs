//! What Finch knows about providers and models before the user picks one.
//!
//! Cloud provider table, API-key detection, the model catalog profile, and the conversions
//! between a catalog entry and a `crate::config` provider entry. This is where the wizard's
//! `crate::models`, `crate::runtime` and most of its `crate::providers` references live.

use super::*;

/// Step in the "Add Provider" flow (overlay inside Models section)
#[derive(Debug, Clone)]
pub(super) enum AddProviderStep {
    // Step 0: what kind of provider to add?
    SelectAddType {
        selected: usize,
    },
    // Cloud AI path — provider-specific authentication input on one screen.
    ConfigureRemote {
        provider_idx: usize,        // index into CLOUD_PROVIDERS
        name: String,               // stable public name used by /model and API clients
        model: String,              // editable model name
        api_key: Option<String>,    // absent for device-auth subscription providers
        focused_field: usize,       // 0=Provider, 1=Name, 2=Model, 3=APIKey when present
        editing_idx: Option<usize>, // 0=primary, n=tool model index + 1
    },
    // Local model path — single dialog (backend, family, size, device on one screen)
    ConfigureLocal {
        inference_provider: InferenceProvider,
        family: ModelFamily,
        size: ModelSize,
        execution: ExecutionTarget,
        focused_field: usize, // 0=Backend, 1=Family, 2=Size, 3=Device
    },
    // Network scan path
    Scanning {
        results: Arc<Mutex<Option<Vec<DiscoveredService>>>>,
    },
    SelectAgent {
        agents: Vec<DiscoveredService>,
        selected: usize,
    },
}

/// Cloud provider options shown in the add-provider overlay
pub(super) const CLOUD_PROVIDERS: &[(&str, &str, &str, &str)] = &[
    (
        "chatgpt",
        "ChatGPT subscription",
        "gpt-5.6-sol",
        "Finch-native device sign-in starts after the wizard; not an OpenAI API key",
    ),
    (
        "grok",
        "Grok (xAI)",
        "",
        "get key at console.x.ai (X Premium+ included)",
    ),
    (
        "claude",
        "Claude (Anthropic)",
        "",
        "get key at console.anthropic.com",
    ),
    ("openai", "OpenAI API", "", "get key at platform.openai.com"),
    (
        "gemini",
        "Gemini (Google)",
        "gemini-2.5-flash",
        "get key at aistudio.google.com",
    ),
    ("mistral", "Mistral AI", "", "get key at console.mistral.ai"),
    (
        "groq",
        "Groq (fast cloud)",
        "openai/gpt-oss-120b",
        "get key at console.groq.com",
    ),
];

/// Whether a provider authenticates with an inline API key held in the config.
///
/// Three do not. A ChatGPT subscription authenticates through a named credential
/// binding, an Ollama server is unauthenticated, and a remote Finch daemon is
/// addressed rather than authenticated. For all three an empty `api_key` is the
/// normal, fully-configured state and says nothing about whether the provider is
/// set up — which is exactly the inference that destroyed providers in #419.
pub(super) fn provider_requires_inline_api_key(provider: &str) -> bool {
    !matches!(
        provider.to_ascii_lowercase().as_str(),
        "chatgpt" | "ollama" | "finch"
    )
}

pub(super) fn remote_api_key_input(provider: &str) -> Option<String> {
    provider_requires_inline_api_key(provider).then(String::new)
}

pub(super) type CatalogRefreshResult = Option<(ModelCatalog, Option<String>)>;

#[derive(Debug, Clone)]
pub(super) struct CatalogRefresh {
    pub(super) generation: u64,
    pub(super) selection_identity: String,
    pub(super) result: Arc<Mutex<CatalogRefreshResult>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ModelSelectionProvenance {
    Blank,
    DefaultGenerated,
    Cycled,
    Manual,
    Persisted,
}

/// Try to detect an existing Anthropic API key from the environment or Claude Code config.
pub(super) fn detect_anthropic_api_key() -> Option<String> {
    // 1. Check the standard environment variable first
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            return Some(key.trim().to_string());
        }
    }

    // 2. Check Claude Code's settings file (~/.claude/settings.json)
    if let Some(home) = dirs::home_dir() {
        let claude_settings = home.join(".claude").join("settings.json");
        if let Ok(contents) = std::fs::read_to_string(&claude_settings) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(key) = json.get("apiKey").and_then(|v| v.as_str()) {
                    if !key.trim().is_empty() {
                        return Some(key.trim().to_string());
                    }
                }
            }
        }
    }

    None
}

/// Try to detect an existing xAI/Grok API key from the environment.
pub(super) fn detect_xai_api_key() -> Option<String> {
    for var in &["XAI_API_KEY", "GROK_API_KEY"] {
        if let Ok(key) = std::env::var(var) {
            if !key.trim().is_empty() {
                return Some(key.trim().to_string());
            }
        }
    }
    None
}

/// Known model names for cloud providers (used for cycling in ConfigureRemote dialog)
pub(super) fn known_models_for(provider: &str) -> Vec<String> {
    model_catalog::static_fallback(provider)
}

pub(super) fn model_catalog_profile(
    provider: &str,
    profile_id: &str,
    api_key: &str,
    persisted: Option<&ProviderEntry>,
) -> Option<ModelCatalogProfile> {
    let (base_url, chat_path, models_path, auth) = match (provider, persisted) {
        (
            "claude",
            Some(ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::Anthropic,
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => (
            base_url.as_deref().unwrap_or("https://api.anthropic.com"),
            chat_path.as_deref().unwrap_or("/v1/messages"),
            models_path.as_deref().unwrap_or("/v1/models"),
            CatalogAuth::AnthropicApiKey,
        ),
        (
            "claude",
            Some(ProviderEntry::Claude {
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => (
            base_url.as_deref().unwrap_or("https://api.anthropic.com"),
            chat_path.as_deref().unwrap_or("/v1/messages"),
            models_path.as_deref().unwrap_or("/v1/models"),
            CatalogAuth::AnthropicApiKey,
        ),
        ("claude", _) => (
            "https://api.anthropic.com",
            "/v1/messages",
            "/v1/models",
            CatalogAuth::AnthropicApiKey,
        ),
        (
            "openai",
            Some(ProviderEntry::Openai {
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => (
            base_url.as_deref().unwrap_or("https://api.openai.com"),
            chat_path.as_deref().unwrap_or("/v1/chat/completions"),
            models_path.as_deref().unwrap_or("/v1/models"),
            CatalogAuth::Bearer,
        ),
        ("openai", persisted) if !matches!(persisted, Some(ProviderEntry::Credentialed { .. })) => {
            (
                "https://api.openai.com",
                "/v1/chat/completions",
                "/v1/models",
                CatalogAuth::Bearer,
            )
        }
        (
            "openai" | "grok" | "mistral" | "groq",
            Some(ProviderEntry::Credentialed {
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => {
            let (default_base, default_chat, default_models) = match provider {
                "openai" => (
                    "https://api.openai.com",
                    "/v1/chat/completions",
                    "/v1/models",
                ),
                "grok" => ("https://api.x.ai", "/v1/chat/completions", "/v1/models"),
                "mistral" => (
                    "https://api.mistral.ai",
                    "/v1/chat/completions",
                    "/v1/models",
                ),
                "groq" => (
                    "https://api.groq.com/openai",
                    "/v1/chat/completions",
                    "/v1/models",
                ),
                _ => unreachable!("match pattern limits provider"),
            };
            (
                base_url.as_deref().unwrap_or(default_base),
                chat_path.as_deref().unwrap_or(default_chat),
                models_path.as_deref().unwrap_or(default_models),
                CatalogAuth::Bearer,
            )
        }
        (
            "grok",
            Some(ProviderEntry::Grok {
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => (
            base_url.as_deref().unwrap_or("https://api.x.ai"),
            chat_path.as_deref().unwrap_or("/v1/chat/completions"),
            models_path.as_deref().unwrap_or("/v1/models"),
            CatalogAuth::Bearer,
        ),
        ("grok", _) => (
            "https://api.x.ai",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
        ),
        (
            "mistral",
            Some(ProviderEntry::Mistral {
                base_url,
                chat_path,
                models_path,
                ..
            }),
        ) => (
            base_url.as_deref().unwrap_or("https://api.mistral.ai"),
            chat_path.as_deref().unwrap_or("/v1/chat/completions"),
            models_path.as_deref().unwrap_or("/v1/models"),
            CatalogAuth::Bearer,
        ),
        ("mistral", _) => (
            "https://api.mistral.ai",
            "/v1/chat/completions",
            "/v1/models",
            CatalogAuth::Bearer,
        ),
        _ => return None,
    };
    Some(ModelCatalogProfile::new(
        provider,
        profile_id,
        api_key,
        ProviderEndpoints::new(base_url, chat_path, models_path),
        auth,
    ))
}

/// Helper function to display ModelSize
pub(super) fn model_size_display(size: &ModelSize) -> &'static str {
    match size {
        ModelSize::Small => "Small (~1-3B)",
        ModelSize::Medium => "Medium (~3-9B)",
        ModelSize::Large => "Large (~7-14B)",
        ModelSize::XLarge => "XLarge (~14B+)",
    }
}

#[derive(Debug, Clone)]
pub enum ModelConfig {
    Local {
        family: ModelFamily,
        size: ModelSize,
        execution: ExecutionTarget,
        inference_provider: InferenceProvider,
        enabled: bool,
        /// Original profile metadata that the local-model editor does not
        /// expose yet (stable name, repository, and resolved model path).
        persisted: Option<ProviderEntry>,
    },
    Remote {
        provider: String,
        name: String,
        api_key: String,
        model: String,
        enabled: bool,
        /// Exact configured profile, retaining endpoint/auth/capability fields
        /// the compact wizard editor does not expose.
        persisted: Option<ProviderEntry>,
    },
}

impl ModelConfig {
    pub(super) fn enabled(&self) -> bool {
        match self {
            Self::Local { enabled, .. } => *enabled,
            Self::Remote { enabled, .. } => *enabled,
        }
    }

    pub(super) fn set_enabled(&mut self, value: bool) {
        match self {
            Self::Local { enabled, .. } => *enabled = value,
            Self::Remote { enabled, .. } => *enabled = value,
        }
    }

    pub(super) fn accepts_api_key(&self) -> bool {
        matches!(self, Self::Remote { provider, .. } if !provider.eq_ignore_ascii_case("chatgpt"))
    }

    #[allow(dead_code)]
    pub(super) fn display_name(&self) -> String {
        match self {
            Self::Local { family, size, .. } => {
                format!("Local {} {}", family.name(), model_size_display(size))
            }
            Self::Remote { name, model, .. } => {
                if !model.is_empty() {
                    format!("{} - {}", name, model)
                } else {
                    name.clone()
                }
            }
        }
    }

    #[allow(dead_code)]
    pub(super) fn is_configured(&self) -> bool {
        match self {
            Self::Local { .. } => true, // Local models are always "configured"
            Self::Remote {
                api_key, persisted, ..
            } => {
                !api_key.is_empty() || matches!(persisted, Some(ProviderEntry::Credentialed { .. }))
            }
        }
    }
}

/// Convert a persisted provider profile into the wizard's editable model form.
/// Local providers live only in the unified provider list, not the legacy
/// `teachers` projection.
pub(super) fn model_config_from_provider(provider: &ProviderEntry) -> Option<ModelConfig> {
    match provider {
        ProviderEntry::Credentialed {
            provider: credential_provider,
            model,
            name,
            ..
        } => Some(ModelConfig::Remote {
            provider: match credential_provider {
                crate::config::CredentialProvider::Anthropic => "claude",
                crate::config::CredentialProvider::OpenaiPlatform => "openai",
                crate::config::CredentialProvider::ChatgptSubscription => "chatgpt",
                crate::config::CredentialProvider::Xai => "grok",
                crate::config::CredentialProvider::GeminiAiStudio => "gemini",
                crate::config::CredentialProvider::Mistral => "mistral",
                crate::config::CredentialProvider::Groq => "groq",
                _ => credential_provider.as_str(),
            }
            .to_string(),
            name: name
                .clone()
                .unwrap_or_else(|| credential_provider.as_str().to_string()),
            api_key: String::new(),
            model: model.clone().unwrap_or_default(),
            enabled: true,
            persisted: Some(provider.clone()),
        }),
        ProviderEntry::LegacyChatgptSubscription { .. } => None,
        ProviderEntry::Local {
            inference_provider,
            execution_target,
            model_family,
            model_size,
            enabled,
            ..
        } => Some(ModelConfig::Local {
            family: *model_family,
            size: *model_size,
            execution: *execution_target,
            inference_provider: *inference_provider,
            enabled: *enabled,
            persisted: Some(provider.clone()),
        }),
        ProviderEntry::Ollama { model, name, .. } => Some(ModelConfig::Remote {
            provider: "ollama".to_string(),
            name: name.clone().unwrap_or_else(|| "ollama".to_string()),
            api_key: String::new(),
            model: model.clone(),
            enabled: true,
            persisted: Some(provider.clone()),
        }),
        ProviderEntry::RemoteDaemon { address, name } => Some(ModelConfig::Remote {
            provider: "finch".to_string(),
            name: name.clone().unwrap_or_else(|| "remote-daemon".to_string()),
            api_key: String::new(),
            model: address.clone(),
            enabled: true,
            persisted: Some(provider.clone()),
        }),
        _ => provider
            .to_teacher_entry()
            .map(|teacher| ModelConfig::Remote {
                provider: teacher.provider.clone(),
                name: teacher
                    .name
                    .clone()
                    .unwrap_or_else(|| teacher.provider.clone()),
                api_key: teacher.api_key,
                model: teacher.model.unwrap_or_default(),
                enabled: true,
                persisted: Some(provider.clone()),
            }),
    }
}

/// True only for the placeholder the wizard shows when a provider slot has not
/// been configured yet.
///
/// The question this has to answer is not "is anything filled in" but "does this
/// provider take an inline API key, and is it missing?" Emptiness alone cannot
/// tell a genuinely unconfigured Claude row from a ChatGPT subscription, whose
/// `api_key` is empty precisely because it authenticates through a named
/// credential binding. Mistaking the second for the first is #419: a provider
/// destroyed because it authenticated the more secure way.
///
/// `remote_api_key_input` already encodes which providers take an inline key, so
/// ask it rather than inferring from emptiness.
pub(super) fn is_unconfigured_placeholder(model: &ModelConfig) -> bool {
    let ModelConfig::Remote {
        provider,
        api_key,
        persisted,
        ..
    } = model
    else {
        // A local model is always a real, usable provider.
        return false;
    };
    if !api_key.is_empty() {
        return false;
    }
    match persisted {
        // Never saved. Only a provider that takes an inline key and has none is
        // the unconfigured placeholder; one that never takes a key — a ChatGPT
        // subscription, an Ollama server, a discovered Finch daemon — is real.
        None => provider_requires_inline_api_key(provider),
        // Persisted. `to_teacher_entry` returns `None` for credential-backed,
        // Ollama, remote-daemon and local entries, each of which authenticates or
        // addresses itself without an inline key; only a legacy key-based entry
        // holding no key is the "[Not configured]" empty state.
        Some(entry) => entry
            .to_teacher_entry()
            .is_some_and(|teacher| teacher.api_key.is_empty()),
    }
}

pub(super) fn provider_entry_from_remote_model(
    provider: &str,
    name: &str,
    api_key: &str,
    model: &str,
    persisted: Option<&ProviderEntry>,
) -> ProviderEntry {
    let model = (!model.is_empty()).then(|| model.to_string());
    let name = Some(name.to_string());
    match persisted {
        Some(ProviderEntry::Credentialed {
            provider,
            credential,
            base_url,
            chat_path,
            models_path,
            reasoning_effort,
            ..
        }) => ProviderEntry::Credentialed {
            provider: *provider,
            credential: credential.clone(),
            model,
            base_url: base_url.clone(),
            chat_path: chat_path.clone(),
            models_path: models_path.clone(),
            name,
            reasoning_effort: *reasoning_effort,
        },
        Some(ProviderEntry::Claude {
            base_url,
            chat_path,
            models_path,
            ..
        }) => ProviderEntry::Claude {
            api_key: api_key.to_string(),
            model,
            base_url: base_url.clone(),
            chat_path: chat_path.clone(),
            models_path: models_path.clone(),
            name,
        },
        Some(ProviderEntry::Openai {
            base_url,
            chat_path,
            models_path,
            reasoning_effort,
            ..
        }) => ProviderEntry::Openai {
            api_key: api_key.to_string(),
            model,
            base_url: base_url.clone(),
            chat_path: chat_path.clone(),
            models_path: models_path.clone(),
            name,
            reasoning_effort: *reasoning_effort,
        },
        Some(ProviderEntry::Grok {
            base_url,
            chat_path,
            models_path,
            ..
        }) => ProviderEntry::Grok {
            api_key: api_key.to_string(),
            model,
            base_url: base_url.clone(),
            chat_path: chat_path.clone(),
            models_path: models_path.clone(),
            name,
        },
        Some(ProviderEntry::Gemini { .. }) => ProviderEntry::Gemini {
            api_key: api_key.to_string(),
            model,
            name,
        },
        Some(ProviderEntry::Mistral {
            base_url,
            chat_path,
            models_path,
            ..
        }) => ProviderEntry::Mistral {
            api_key: api_key.to_string(),
            model,
            base_url: base_url.clone(),
            chat_path: chat_path.clone(),
            models_path: models_path.clone(),
            name,
        },
        Some(ProviderEntry::Groq { .. }) => ProviderEntry::Groq {
            api_key: api_key.to_string(),
            model,
            name,
        },
        Some(ProviderEntry::Ollama { base_url, .. }) => ProviderEntry::Ollama {
            model: model.unwrap_or_default(),
            base_url: base_url.clone(),
            name,
        },
        Some(ProviderEntry::RemoteDaemon { .. }) => ProviderEntry::RemoteDaemon {
            address: model.unwrap_or_default(),
            name,
        },
        _ if provider.eq_ignore_ascii_case("finch") => ProviderEntry::RemoteDaemon {
            address: model.unwrap_or_default(),
            name,
        },
        _ if provider.eq_ignore_ascii_case("chatgpt") => ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            credential: crate::config::CredentialBinding {
                credential_ref: "chatgpt:default".into(),
                audience: Some(crate::config::AudienceBinding::standard(
                    crate::config::EndpointFamily::ChatgptSubscription,
                )),
                tenant: None,
                project: None,
                account: None,
                required_scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
            },
            model,
            base_url: None,
            chat_path: None,
            models_path: None,
            name,
            reasoning_effort: None,
        },
        _ => ProviderEntry::from_teacher_entry(&TeacherEntry {
            provider: provider.to_string(),
            api_key: api_key.to_string(),
            model,
            base_url: None,
            name,
        }),
    }
}

pub(super) fn named_catalog_refresh_config(
    primary_model: &ModelConfig,
    tool_models: &[ModelConfig],
    editing_idx: usize,
    selected_entry: &ProviderEntry,
    credentials: Vec<crate::config::ProviderCredential>,
) -> crate::config::Config {
    let providers = std::iter::once(primary_model)
        .chain(tool_models.iter())
        .enumerate()
        .filter_map(|(index, configured)| {
            if index == editing_idx {
                return Some(selected_entry.clone());
            }
            match configured {
                ModelConfig::Remote {
                    provider,
                    name,
                    api_key,
                    model,
                    enabled,
                    persisted,
                } if index == 0 || *enabled => Some(provider_entry_from_remote_model(
                    provider,
                    name,
                    api_key,
                    model,
                    persisted.as_ref(),
                )),
                ModelConfig::Local {
                    persisted, enabled, ..
                } if index == 0 || *enabled => persisted.clone(),
                _ => None,
            }
        })
        .collect();
    crate::config::Config::with_providers(providers).with_credentials(credentials)
}
