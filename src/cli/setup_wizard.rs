// Setup Wizard - First-run configuration

use crate::service::discovery_client::{DiscoveredService, ServiceDiscoveryClient};
use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap},
    Frame,
};
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Step in the "Add Provider" flow (overlay inside Models section)
#[derive(Debug, Clone)]
enum AddProviderStep {
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
const CLOUD_PROVIDERS: &[(&str, &str, &str, &str)] = &[
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
fn provider_requires_inline_api_key(provider: &str) -> bool {
    !matches!(
        provider.to_ascii_lowercase().as_str(),
        "chatgpt" | "ollama" | "finch"
    )
}

fn remote_api_key_input(provider: &str) -> Option<String> {
    provider_requires_inline_api_key(provider).then(String::new)
}

use crate::config::{CoreMlConfig, ExecutionTarget, ProviderEntry, TeacherEntry};
use crate::models::compatibility;
use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use crate::providers::endpoints::ProviderEndpoints;
use crate::providers::model_catalog::{
    self, CatalogAuth, CatalogSource, ModelCatalog, ModelCatalogProfile,
};
use chrono::{DateTime, Utc};

#[cfg(target_os = "macos")]
use crate::runtime::automation::{
    permission_context_key, permission_target_description, AutomationAvailability,
    AutomationBroker, AutomationPermissionResult, AutomationPromptContext,
    AutomationPromptDisposition, AutomationState,
};

type CatalogRefreshResult = Option<(ModelCatalog, Option<String>)>;

#[derive(Debug, Clone)]
struct CatalogRefresh {
    generation: u64,
    selection_identity: String,
    result: Arc<Mutex<CatalogRefreshResult>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelSelectionProvenance {
    Blank,
    DefaultGenerated,
    Cycled,
    Manual,
    Persisted,
}

/// Try to detect an existing Anthropic API key from the environment or Claude Code config.
fn detect_anthropic_api_key() -> Option<String> {
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
fn detect_xai_api_key() -> Option<String> {
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
fn known_models_for(provider: &str) -> Vec<String> {
    model_catalog::static_fallback(provider)
}

fn model_catalog_profile(
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
fn model_size_display(size: &ModelSize) -> &'static str {
    match size {
        ModelSize::Small => "Small (~1-3B)",
        ModelSize::Medium => "Medium (~3-9B)",
        ModelSize::Large => "Large (~7-14B)",
        ModelSize::XLarge => "XLarge (~14B+)",
    }
}

/// Main sections of the tabbed wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WizardSection {
    Themes,
    Models,
    Personas,
    Features,
    Review,
}

impl WizardSection {
    fn all() -> Vec<Self> {
        vec![
            Self::Themes,
            Self::Models,
            Self::Personas,
            Self::Features,
            Self::Review,
        ]
    }

    fn name(&self) -> &str {
        match self {
            Self::Themes => "Look & Feel",
            Self::Models => "Model Setup",
            Self::Personas => "Style",
            Self::Features => "Settings",
            Self::Review => "Finish",
        }
    }
}

/// State for each wizard section
#[derive(Debug, Clone)]
enum SectionState {
    Themes {
        selected_theme: usize,
    },
    Models {
        primary_model: ModelConfig,
        tool_models: Vec<ModelConfig>,
        selected_idx: usize, // 0 = primary, 1+ = tool models
        editing_mode: bool,
        editing_model_mode: bool, // editing model name for selected entry
        model_input: String,      // model name input buffer
        adding_provider: Option<AddProviderStep>,
        catalog_models: Vec<String>,
        catalog_model_provenance: ModelSelectionProvenance,
        catalog_source: CatalogSource,
        catalog_refresh: Option<CatalogRefresh>,
        catalog_generation: u64,
        catalog_refreshed_at: Option<DateTime<Utc>>,
        catalog_error: Option<String>,
        error: Option<String>,
    },
    Personas {
        available_personas: Vec<PersonaInfo>,
        selected_idx: usize,
        default_persona: String,
        editing_prompt: bool,
        prompt_input: String,
        /// Cursor position in chars within prompt_input (used in edit mode)
        cursor_pos: usize,
    },
    Features {
        auto_approve: bool,
        streaming: bool,
        debug: bool,
        hf_token: String,
        editing_hf_token: bool,
        finch_api_key: String,
        editing_finch_api_key: bool,
        #[cfg(target_os = "macos")]
        gui_automation: bool,
        #[cfg(target_os = "macos")]
        gui_automation_availability: AutomationAvailability,
        #[cfg(target_os = "macos")]
        gui_automation_prompt: AutomationPromptDisposition,
        #[cfg(target_os = "macos")]
        gui_automation_prompted: bool,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available: bool,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context: String,
        #[cfg(target_os = "macos")]
        gui_automation_settings_feedback: Option<GuiSettingsFeedback>,
        #[cfg(target_os = "macos")]
        gui_automation_details_expanded: bool,
        #[cfg(target_os = "macos")]
        gui_automation_details_scroll: u16,
        daemon_only_mode: bool,
        mdns_discovery: bool,
        auto_discover: bool,
        /// Total status-strip context lines (🧠 + summaries); range 1–8
        memory_context_lines: usize,
        selected_idx: usize, // For arrow key navigation
    },
    Review,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum GuiSettingsFeedback {
    OpenRequested,
    Suppressed,
    Failed(String),
}

#[cfg(target_os = "macos")]
impl GuiSettingsFeedback {
    fn compact_message(&self) -> &str {
        match self {
            Self::OpenRequested => "Open requested; R re-checks.",
            Self::Suppressed => "Not opened (SSH/headless).",
            Self::Failed(_) => "Open failed; D has the error.",
        }
    }

    fn full_message(&self) -> String {
        match self {
            Self::OpenRequested => {
                "System Settings open requested. Grant the app macOS identifies, then press R to re-check the current Finch process."
                    .to_string()
            }
            Self::Suppressed => {
                "System Settings was not opened in this SSH/headless session. From a local interactive session, press O, or open System Settings → Privacy & Security → Accessibility manually."
                    .to_string()
            }
            Self::Failed(error) => format!(
                "Could not open System Settings: {error}. Open System Settings → Privacy & Security → Accessibility manually."
            ),
        }
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
    fn enabled(&self) -> bool {
        match self {
            Self::Local { enabled, .. } => *enabled,
            Self::Remote { enabled, .. } => *enabled,
        }
    }

    fn set_enabled(&mut self, value: bool) {
        match self {
            Self::Local { enabled, .. } => *enabled = value,
            Self::Remote { enabled, .. } => *enabled = value,
        }
    }

    fn accepts_api_key(&self) -> bool {
        matches!(self, Self::Remote { provider, .. } if !provider.eq_ignore_ascii_case("chatgpt"))
    }

    #[allow(dead_code)]
    fn display_name(&self) -> String {
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
    fn is_configured(&self) -> bool {
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
fn model_config_from_provider(provider: &ProviderEntry) -> Option<ModelConfig> {
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
fn is_unconfigured_placeholder(model: &ModelConfig) -> bool {
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

fn provider_entry_from_remote_model(
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

fn named_catalog_refresh_config(
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

#[derive(Debug, Clone)]
struct PersonaInfo {
    slug: String, // Key used to load the persona (e.g. "expert-coder")
    name: String, // Display name (e.g. "Expert Coder")
    description: String,
    system_prompt: String,
}

/// Overall wizard state with tabbed navigation
struct WizardState {
    current_section: WizardSection,
    sections: HashMap<WizardSection, SectionState>,
    completed: HashSet<WizardSection>,
    confirming_cancel: bool,
    catalog_cache_dir: Option<std::path::PathBuf>,
    /// Typed CoreML policy provenance from the loaded configuration.
    coreml: CoreMlConfig,
    /// Named credential metadata is preserved unchanged by the compact model
    /// editor; it contains no secret material.
    credentials: Vec<crate::config::ProviderCredential>,
}

impl WizardState {
    fn new(existing_config: Option<&crate::config::Config>) -> Self {
        Self::new_with_catalog_cache_dir(existing_config, model_catalog::default_cache_dir().ok())
    }

    fn new_with_catalog_cache_dir(
        existing_config: Option<&crate::config::Config>,
        catalog_cache_dir: Option<std::path::PathBuf>,
    ) -> Self {
        use crate::config::Persona;
        use crate::theme::ColorTheme;

        let mut sections = HashMap::new();

        // Themes section
        let current_theme = existing_config
            .map(|c| c.active_theme.as_str())
            .unwrap_or("light"); // Default to Light theme for better initial visibility
        let themes = ColorTheme::all();
        let selected_theme = themes
            .iter()
            .position(|t| t.name().to_lowercase() == current_theme.to_lowercase())
            .unwrap_or(1); // Default to Light (index 1) if not found

        sections.insert(
            WizardSection::Themes,
            SectionState::Themes { selected_theme },
        );

        // The ordered unified provider list is authoritative and includes
        // local secondary models. The legacy teachers projection does not.
        let mut configured_models: Vec<ModelConfig> = existing_config
            .map(|config| {
                config
                    .providers
                    .iter()
                    .filter_map(model_config_from_provider)
                    .collect()
            })
            .unwrap_or_default();

        // Compatibility for Config values constructed from the old split
        // backend/teachers fields by tests or older callers.
        if configured_models.is_empty() {
            if let Some(config) = existing_config {
                if config.backend.enabled {
                    configured_models.push(ModelConfig::Local {
                        family: config.backend.model_family,
                        size: config.backend.model_size,
                        execution: config.backend.execution_target,
                        inference_provider: config.backend.inference_provider,
                        enabled: true,
                        persisted: None,
                    });
                }
                configured_models.extend(config.teachers.iter().map(|teacher| {
                    ModelConfig::Remote {
                        provider: teacher.provider.clone(),
                        name: teacher
                            .name
                            .clone()
                            .unwrap_or_else(|| teacher.provider.clone()),
                        api_key: teacher.api_key.clone(),
                        model: teacher.model.clone().unwrap_or_default(),
                        enabled: true,
                        persisted: None,
                    }
                }));
            }
        }

        let primary_model = if configured_models.is_empty() {
            // Default: remote Claude - try to auto-detect key
            let detected_key = detect_anthropic_api_key()
                .or_else(detect_xai_api_key)
                .unwrap_or_default();
            ModelConfig::Remote {
                provider: "claude".to_string(),
                name: "claude".to_string(),
                api_key: detected_key,
                model: String::new(),
                enabled: true,
                persisted: None,
            }
        } else {
            configured_models.remove(0)
        };

        let tool_models = configured_models;

        sections.insert(
            WizardSection::Models,
            SectionState::Models {
                primary_model,
                tool_models,
                selected_idx: 0,
                editing_mode: false,
                editing_model_mode: false,
                model_input: String::new(),
                adding_provider: None,
                catalog_models: Vec::new(),
                catalog_model_provenance: ModelSelectionProvenance::Blank,
                catalog_source: CatalogSource::StaticFallback,
                catalog_refresh: None,
                catalog_generation: 0,
                catalog_refreshed_at: None,
                catalog_error: None,
                error: None,
            },
        );

        // Personas section
        let builtin_personas: Vec<PersonaInfo> = Persona::list_builtins()
            .iter()
            .filter_map(|slug| {
                Persona::load_by_name(slug).ok().map(|p| PersonaInfo {
                    slug: slug.to_string(),
                    name: p.name().to_string(),
                    description: p.persona.description.clone(),
                    system_prompt: p.behavior.system_prompt.clone(),
                })
            })
            .collect();

        let default_persona = existing_config
            .map(|c| c.active_persona.clone())
            .unwrap_or_else(|| "default".to_string());

        let selected_idx = builtin_personas
            .iter()
            .position(|p| {
                p.slug == default_persona || p.name.to_lowercase() == default_persona.to_lowercase()
            })
            .unwrap_or(0);

        sections.insert(
            WizardSection::Personas,
            SectionState::Personas {
                available_personas: builtin_personas,
                selected_idx,
                default_persona,
                editing_prompt: false,
                prompt_input: String::new(),
                cursor_pos: 0,
            },
        );

        // Features section
        #[cfg(target_os = "macos")]
        let configured_gui_automation = existing_config
            .map(|c| c.features.gui_automation)
            .unwrap_or(false);
        #[cfg(target_os = "macos")]
        let current_gui_automation_context = permission_context_key();
        #[cfg(target_os = "macos")]
        let gui_automation_context_matches = existing_config.is_some_and(|config| {
            config.features.gui_automation_permission_context == current_gui_automation_context
        });
        #[cfg(target_os = "macos")]
        let native_gui_automation_available = AutomationBroker::new(configured_gui_automation)
            .availability()
            .state
            == AutomationState::Available;
        #[cfg(target_os = "macos")]
        let (gui_automation_prompted, gui_automation_last_known_available) =
            scoped_permission_history(
                existing_config
                    .map(|config| config.features.gui_automation_prompted)
                    .unwrap_or(false),
                existing_config
                    .map(|config| config.features.gui_automation_last_known_available)
                    .unwrap_or(false),
                gui_automation_context_matches,
                native_gui_automation_available,
            );

        sections.insert(
            WizardSection::Features,
            SectionState::Features {
                auto_approve: existing_config
                    .map(|c| c.features.auto_approve_tools)
                    .unwrap_or(false),
                streaming: existing_config
                    .map(|c| c.features.streaming_enabled)
                    .unwrap_or(true),
                debug: existing_config
                    .map(|c| c.features.debug_logging)
                    .unwrap_or(false),
                hf_token: existing_config
                    .and_then(|c| c.huggingface_token.clone())
                    .unwrap_or_default(),
                editing_hf_token: false,
                finch_api_key: existing_config
                    .and_then(|config| config.server.api_keys.first().cloned())
                    .unwrap_or_default(),
                editing_finch_api_key: false,
                #[cfg(target_os = "macos")]
                gui_automation: configured_gui_automation,
                #[cfg(target_os = "macos")]
                gui_automation_availability: AutomationBroker::new(configured_gui_automation)
                    .availability(),
                #[cfg(target_os = "macos")]
                gui_automation_prompt: AutomationPromptDisposition::NotNeeded,
                #[cfg(target_os = "macos")]
                gui_automation_prompted,
                #[cfg(target_os = "macos")]
                gui_automation_last_known_available,
                #[cfg(target_os = "macos")]
                gui_automation_permission_context: current_gui_automation_context,
                #[cfg(target_os = "macos")]
                gui_automation_settings_feedback: None,
                #[cfg(target_os = "macos")]
                gui_automation_details_expanded: false,
                #[cfg(target_os = "macos")]
                gui_automation_details_scroll: 0,
                daemon_only_mode: existing_config
                    .map(|c| c.server.mode == "daemon-only")
                    .unwrap_or(false),
                mdns_discovery: existing_config.map(|c| c.server.advertise).unwrap_or(false),
                auto_discover: existing_config
                    .map(|c| c.client.auto_discover)
                    .unwrap_or(true),
                memory_context_lines: existing_config
                    .map(|c| c.features.memory_context_lines)
                    .unwrap_or(4),
                selected_idx: 0,
            },
        );

        // Review section
        sections.insert(WizardSection::Review, SectionState::Review);

        Self {
            current_section: WizardSection::Themes,
            sections,
            completed: HashSet::new(),
            confirming_cancel: false,
            catalog_cache_dir,
            coreml: existing_config
                .map(|config| config.backend.coreml)
                .unwrap_or_default(),
            credentials: existing_config
                .map(|config| config.credentials().to_vec())
                .unwrap_or_default(),
        }
    }

    fn is_completed(&self, section: WizardSection) -> bool {
        self.completed.contains(&section)
    }

    fn mark_completed(&mut self, section: WizardSection) {
        self.completed.insert(section);
    }

    fn next_section(&mut self) {
        let all = WizardSection::all();
        if let Some(idx) = all.iter().position(|s| *s == self.current_section) {
            if idx < all.len() - 1 {
                self.current_section = all[idx + 1];
            }
        }
    }

    fn prev_section(&mut self) {
        let all = WizardSection::all();
        if let Some(idx) = all.iter().position(|s| *s == self.current_section) {
            if idx > 0 {
                self.current_section = all[idx - 1];
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn scoped_permission_history(
    prompted: bool,
    last_known_available: bool,
    context_matches: bool,
    native_available: bool,
) -> (bool, bool) {
    (
        prompted && context_matches,
        (last_known_available && context_matches) || native_available,
    )
}

/// Check if a model family is compatible with an execution target
/// Setup wizard result containing all collected configuration
#[derive(Clone)]
pub struct SetupResult {
    // Theme
    pub active_theme: String,

    // Models (primary + tools)
    pub primary_model: ModelConfig,
    pub tool_models: Vec<ModelConfig>,

    // Unified providers list (new format)
    pub providers: Vec<ProviderEntry>,

    /// Secret-free named credential records preserved by setup.
    pub credentials: Vec<crate::config::ProviderCredential>,

    // Backward compatibility fields (mapped from primary_model)
    pub claude_api_key: String,
    pub hf_token: Option<String>,
    pub backend_enabled: bool,
    pub inference_provider: InferenceProvider,
    pub execution_target: ExecutionTarget,
    /// CoreML policy loaded into this wizard, including an explicit Auto/All reset.
    pub coreml: CoreMlConfig,
    pub model_family: ModelFamily,
    pub model_size: ModelSize,
    pub custom_model_repo: Option<String>,
    pub teachers: Vec<TeacherEntry>,

    /// Single key accepted by the daemon's model API for every provider.
    pub finch_api_key: String,

    // Persona
    pub default_persona: String,
    /// Edited prompt for the selected persona when it differs from the
    /// compiled-in template.
    pub custom_system_prompt: Option<String>,

    // Feature flags
    pub auto_approve_tools: bool,
    pub streaming_enabled: bool,
    pub debug_logging: bool,
    #[cfg(target_os = "macos")]
    pub gui_automation: bool,
    #[cfg(target_os = "macos")]
    pub gui_automation_prompted: bool,
    #[cfg(target_os = "macos")]
    pub gui_automation_last_known_available: bool,
    #[cfg(target_os = "macos")]
    pub gui_automation_permission_context: String,
    pub daemon_only_mode: bool,
    pub mdns_discovery: bool,
    pub auto_discover: bool,
    pub memory_context_lines: usize,
}

impl SetupResult {
    /// Legacy field accessor for backward compatibility
    #[deprecated(note = "Use execution_target instead")]
    pub fn backend_device(&self) -> ExecutionTarget {
        self.execution_target
    }
}

/// Restore the terminal to normal state after the wizard exits.
fn cleanup_terminal(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let raw_result = crossterm::terminal::disable_raw_mode();
    let screen_result = crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    let cursor_result = terminal.show_cursor();
    raw_result?;
    screen_result?;
    cursor_result?;
    Ok(())
}

/// Show first-run setup wizard and return configuration
pub fn show_setup_wizard() -> Result<SetupResult> {
    // `None` is reserved for a genuinely absent file. Existing configuration
    // failures must stop setup before it can render or save an empty fallback.
    let existing_config = crate::config::load_persisted_config().context(
        "Existing Finch configuration could not be loaded; setup was not opened because saving an empty wizard would overwrite it",
    )?;
    if let Some(config) = existing_config.as_ref() {
        tracing::debug!(
            providers = config.providers.len(),
            credentials = config.credentials().len(),
            "Successfully loaded existing setup configuration"
        );
    }

    // Set up terminal
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;

    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend)?;

    // Run the NEW tabbed wizard
    let result = run_tabbed_wizard(&mut terminal, existing_config.as_ref());

    // ALWAYS restore terminal, even if wizard was cancelled or errored
    // Prioritize cleanup to ensure terminal is always restored
    cleanup_terminal(&mut terminal)?;

    // Return the wizard result after cleanup is guaranteed
    result
}

/// Public entry point used by `/setup` command — runs the wizard and returns
/// `Some(result)` on completion or `None` if the user cancelled.
pub fn run_setup_wizard() -> Result<Option<SetupResult>> {
    match show_setup_wizard() {
        Ok(result) => Ok(Some(result)),
        Err(e) if e.to_string().contains("Setup cancelled") => Ok(None),
        Err(e) => Err(e),
    }
}

/// Apply a `SetupResult` to a new `Config` and save it to disk.
///
/// Used both by `main.rs` (first-run) and by the `/setup` REPL command.
pub fn apply_and_save(result: &SetupResult) -> Result<()> {
    config_from_setup_result(result).save()?;
    if let Some(prompt) = result.custom_system_prompt.as_deref() {
        crate::config::Persona::save_system_prompt_override(&result.default_persona, prompt)?;
    }
    Ok(())
}

/// Result of the shared first-run, `finch setup`, and `/setup` commit ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupApplyOutcome {
    /// Authentication and model validation succeeded and configuration was saved.
    Saved,
    /// The user explicitly chose not to save the wizard changes.
    Cancelled,
}

/// Entry point invoking the shared post-wizard authentication and commit ceremony.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupInvocation {
    /// Automatic setup because no Finch configuration exists yet.
    FirstRun,
    /// Explicit `finch setup` command.
    Command,
    /// In-session `/setup` command.
    Repl,
}

/// Secret-free recovery state shown after a ChatGPT setup ceremony terminates.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatGptSetupRecovery {
    invocation: SetupInvocation,
    credential_ref: String,
    cause: ChatGptSetupFailureCause,
    summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatGptSetupFailureCause {
    Cancelled,
    Expired,
    Denied,
    StartDisabledOrUnsupported,
    ProviderRejected,
    PollContract,
    TokenExchangeRejected,
    TokenExchangeContract,
    IdentityVerification,
    ClientBinding,
    AccountEntitlement,
    Persistence,
    ProtocolOrOther,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatGptSetupRecoveryAction {
    RetrySignIn,
    ChangeNamedCredential(String),
    RemoveProvider,
    CancelSetup,
}

trait ChatGptSetupRecoveryEditor {
    fn choose(&mut self, recovery: &ChatGptSetupRecovery) -> Result<ChatGptSetupRecoveryAction>;
}

struct TerminalChatGptSetupRecoveryEditor;

const MAX_CHATGPT_EDITOR_INPUT_ATTEMPTS: usize = 4;

fn choose_chatgpt_setup_recovery_with_io(
    recovery: &ChatGptSetupRecovery,
    input: &mut impl std::io::BufRead,
    output: &mut impl std::io::Write,
) -> Result<ChatGptSetupRecoveryAction> {
    writeln!(output, "\nChatGPT provider/account editor")?;
    writeln!(output, "Credential: {}", recovery.credential_ref)?;
    writeln!(output, "{}", recovery.summary)?;
    for _ in 0..MAX_CHATGPT_EDITOR_INPUT_ATTEMPTS {
        writeln!(output, "  1. Retry sign-in")?;
        writeln!(output, "  2. Change named credential")?;
        writeln!(output, "  3. Remove provider")?;
        writeln!(output, "  4. Cancel setup")?;
        write!(output, "Choose an action [1-4]: ")?;
        output.flush()?;
        let mut choice = String::new();
        if input.read_line(&mut choice)? == 0 {
            return Ok(ChatGptSetupRecoveryAction::CancelSetup);
        }
        match choice.trim() {
            "1" => return Ok(ChatGptSetupRecoveryAction::RetrySignIn),
            "2" => {
                write!(output, "Named credential (for example chatgpt:work): ")?;
                output.flush()?;
                let mut reference = String::new();
                if input.read_line(&mut reference)? == 0 {
                    return Ok(ChatGptSetupRecoveryAction::CancelSetup);
                }
                let reference = reference.trim().to_string();
                if crate::oauth::validate_reference(&reference).is_ok() {
                    return Ok(ChatGptSetupRecoveryAction::ChangeNamedCredential(reference));
                }
                writeln!(
                    output,
                    "Named credential is invalid; choose an action again."
                )?;
            }
            "3" => return Ok(ChatGptSetupRecoveryAction::RemoveProvider),
            "4" => return Ok(ChatGptSetupRecoveryAction::CancelSetup),
            _ => writeln!(output, "Invalid selection; choose 1, 2, 3, or 4.")?,
        }
    }
    writeln!(
        output,
        "Too many invalid selections; setup was cancelled without saving."
    )?;
    Ok(ChatGptSetupRecoveryAction::CancelSetup)
}

impl ChatGptSetupRecoveryEditor for TerminalChatGptSetupRecoveryEditor {
    fn choose(&mut self, recovery: &ChatGptSetupRecovery) -> Result<ChatGptSetupRecoveryAction> {
        tracing::debug!(
            invocation = ?recovery.invocation,
            cause = ?recovery.cause,
            "ChatGPT setup entered secret-free recovery"
        );
        let stdin = io::stdin();
        let mut input = stdin.lock();
        let stderr = io::stderr();
        let mut output = stderr.lock();
        choose_chatgpt_setup_recovery_with_io(recovery, &mut input, &mut output)
    }
}

enum ChatGptSetupAttempt {
    Ready {
        config: crate::config::Config,
        compensations: Vec<crate::cli::chatgpt_auth::ChatGptCompensationHandle>,
    },
    Recoverable(ChatGptSetupRecovery),
}

/// Validate and save setup through the shared first-run/command/REPL boundary.
///
/// Unsupported legacy ChatGPT subscription profiles are rejected before any
/// provider, network, or process boundary is reached.
pub async fn validate_and_apply_for(
    invocation: SetupInvocation,
    result: &SetupResult,
) -> Result<SetupApplyOutcome> {
    tracing::debug!(?invocation, "Starting shared setup commit ceremony");
    if result
        .providers
        .iter()
        .any(|provider| matches!(provider, ProviderEntry::LegacyChatgptSubscription { .. }))
    {
        anyhow::bail!(
            "Legacy chatgpt_subscription profiles are unsupported because Finch no longer launches Codex app-server. Remove that profile and configure OpenAI Platform with an API key or another supported provider"
        );
    }
    if chatgpt_setup_references(result).is_empty() {
        apply_and_save(result)?;
        return Ok(SetupApplyOutcome::Saved);
    }
    let service = crate::cli::chatgpt_auth::ChatGptAuthService::production()?;
    let mut editor = TerminalChatGptSetupRecoveryEditor;
    let Some((config, committed_result, compensations)) =
        run_chatgpt_setup_recovery_loop(invocation, result, &service, &mut editor).await?
    else {
        return Ok(SetupApplyOutcome::Cancelled);
    };
    save_chatgpt_setup_config(&config, &compensations, &service, |config| config.save())?;
    if let Some(prompt) = committed_result.custom_system_prompt.as_deref() {
        crate::config::Persona::save_system_prompt_override(
            &committed_result.default_persona,
            prompt,
        )?;
    }
    Ok(SetupApplyOutcome::Saved)
}

fn save_chatgpt_setup_config<A, F>(
    config: &crate::config::Config,
    compensations: &[crate::cli::chatgpt_auth::ChatGptCompensationHandle],
    authenticator: &A,
    save: F,
) -> Result<()>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
    F: FnOnce(&crate::config::Config) -> Result<()>,
{
    if let Err(error) = save(config) {
        let failures = compensate_chatgpt_setup(authenticator, compensations);
        if !failures.is_empty() {
            anyhow::bail!("ChatGPT setup configuration could not be saved or rolled back safely. Run `finch auth status chatgpt` before trying again");
        }
        return Err(error).context(
            "ChatGPT setup configuration was not saved; newly issued credentials were rolled back",
        );
    }
    Ok(())
}

const MAX_CHATGPT_SETUP_ATTEMPTS: usize = 8;

async fn run_chatgpt_setup_recovery_loop<A, E>(
    invocation: SetupInvocation,
    result: &SetupResult,
    authenticator: &A,
    editor: &mut E,
) -> Result<
    Option<(
        crate::config::Config,
        SetupResult,
        Vec<crate::cli::chatgpt_auth::ChatGptCompensationHandle>,
    )>,
>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
    E: ChatGptSetupRecoveryEditor,
{
    let mut working = result.clone();
    let mut auth_attempts = 0;
    loop {
        let references = chatgpt_setup_references(&working);
        if references.is_empty() {
            return Ok(Some((
                config_from_setup_result(&working),
                working,
                Vec::new(),
            )));
        }
        if auth_attempts == MAX_CHATGPT_SETUP_ATTEMPTS {
            anyhow::bail!(
                "ChatGPT setup reached the retry limit; setup was not saved and no authorization remains pending"
            );
        }
        auth_attempts += 1;

        let cancel = tokio_util::sync::CancellationToken::new();
        let signal_cancel = cancel.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_cancel.cancel();
            }
        });
        let attempt =
            prepare_chatgpt_setup_attempt(invocation, &working, &references, authenticator, cancel)
                .await;
        signal.abort();
        let _ = signal.await;

        match attempt? {
            ChatGptSetupAttempt::Ready {
                config,
                compensations,
            } => return Ok(Some((config, working, compensations))),
            ChatGptSetupAttempt::Recoverable(recovery) => match editor.choose(&recovery)? {
                ChatGptSetupRecoveryAction::RetrySignIn => {}
                ChatGptSetupRecoveryAction::ChangeNamedCredential(replacement) => {
                    replace_chatgpt_setup_reference(
                        &mut working,
                        &recovery.credential_ref,
                        &replacement,
                    )?;
                }
                ChatGptSetupRecoveryAction::RemoveProvider => {
                    remove_chatgpt_setup_provider(&mut working, &recovery.credential_ref);
                }
                ChatGptSetupRecoveryAction::CancelSetup => return Ok(None),
            },
        }
    }
}

fn chatgpt_setup_references(result: &SetupResult) -> std::collections::BTreeSet<String> {
    result
        .providers
        .iter()
        .filter_map(|provider| match provider {
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                credential,
                ..
            } => Some(credential.credential_ref.clone()),
            _ => None,
        })
        .collect()
}

fn replace_chatgpt_setup_reference(
    result: &mut SetupResult,
    current: &str,
    replacement: &str,
) -> Result<()> {
    crate::oauth::validate_reference(replacement)?;
    let mut changed = false;
    for provider in &mut result.providers {
        let ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            credential,
            ..
        } = provider
        else {
            continue;
        };
        if credential.credential_ref == current {
            credential.credential_ref = replacement.to_string();
            credential.account = None;
            changed = true;
        }
    }
    if !changed {
        anyhow::bail!("ChatGPT recovery credential no longer matches the edited provider graph");
    }
    Ok(())
}

fn remove_chatgpt_setup_provider(result: &mut SetupResult, reference: &str) {
    result.providers.retain(|provider| {
        !matches!(
            provider,
            ProviderEntry::Credentialed {
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                credential,
                ..
            } if credential.credential_ref == reference
        )
    });
}

#[cfg(test)]
async fn prepare_chatgpt_setup_config<A>(
    result: &SetupResult,
    chatgpt_references: &std::collections::BTreeSet<String>,
    authenticator: &A,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<crate::config::Config>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    match prepare_chatgpt_setup_attempt(
        SetupInvocation::Command,
        result,
        chatgpt_references,
        authenticator,
        cancel,
    )
    .await?
    {
        ChatGptSetupAttempt::Ready { config, .. } => Ok(config),
        ChatGptSetupAttempt::Recoverable(recovery) => anyhow::bail!(recovery.summary),
    }
}

async fn prepare_chatgpt_setup_attempt<A>(
    invocation: SetupInvocation,
    result: &SetupResult,
    chatgpt_references: &std::collections::BTreeSet<String>,
    authenticator: &A,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<ChatGptSetupAttempt>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    // Prove the entire secret-free graph is structurally valid before opening
    // an OAuth endpoint. Account identity remains deliberately unknown until a
    // signed token is returned.
    let mut preflight_credentials = result.credentials.clone();
    for reference in chatgpt_references {
        let existing = preflight_credentials
            .iter()
            .find(|credential| credential.name == *reference);
        if let Some(existing) = existing {
            if !is_exact_chatgpt_setup_credential(existing, reference) {
                anyhow::bail!(
                    "Stored named credential '{reference}' does not satisfy the exact ChatGPT setup authority contract"
                );
            }
            // Existing records are static configuration authority. Preserve
            // valid leases exactly. A refreshable access lease may be expired
            // here; the provider refreshes it when inference uses it.
            if is_reusable_chatgpt_setup_credential(existing) {
                continue;
            }
            preflight_credentials.retain(|credential| credential.name != *reference);
        }
        preflight_credentials.push(crate::config::ProviderCredential {
            name: reference.clone(),
            kind: crate::config::CredentialKind::OauthDevice,
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            issuer: "openai-chatgpt".into(),
            audience: crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            ),
            tenant: None,
            project: None,
            account: None,
            scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
            secret_ref: format!("oauth-store:{reference}"),
            lifecycle: crate::config::CredentialLifecycle::Active {
                expires_at: None,
                refreshable: true,
            },
            revocation: Default::default(),
        });
    }
    config_from_setup_result(result)
        .with_credentials(preflight_credentials)
        .validate()
        .context("Setup provider graph is invalid; ChatGPT login was not started")?;

    let mut credentials = result.credentials.clone();
    let mut compensations = Vec::new();
    for reference in chatgpt_references {
        if credentials
            .iter()
            .find(|credential| credential.name == *reference)
            .is_some_and(|credential| {
                is_exact_chatgpt_setup_credential(credential, reference)
                    && is_reusable_chatgpt_setup_credential(credential)
            })
        {
            // Preflight already proved this persisted record satisfies the
            // exact ChatGPT authority contract. Setup is not a provider-use
            // boundary, so it must neither refresh nor rewrite the lease.
            continue;
        }
        let ensured = match authenticator
            .ensure_named_credential(
                &reference,
                crate::cli::chatgpt_auth::DeviceLoginPresentation::default(),
                cancel.clone(),
            )
            .await
        {
            Ok(ensured) => ensured,
            Err(error) => {
                let failures = compensate_chatgpt_setup(authenticator, &compensations);
                if !failures.is_empty() {
                    anyhow::bail!("ChatGPT sign-in could not be rolled back safely. Setup was not saved. Run `finch auth status chatgpt` before trying again");
                }
                let cause = chatgpt_setup_failure_cause(&error);
                let summary = chatgpt_setup_failure_summary(cause);
                return Ok(ChatGptSetupAttempt::Recoverable(ChatGptSetupRecovery {
                    invocation,
                    credential_ref: reference.clone(),
                    cause,
                    summary,
                }));
            }
        };
        if let Some(compensation) = ensured.compensation {
            compensations.push(compensation);
        }
        credentials.retain(|existing| existing.name != *reference);
        credentials.push(ensured.credential);
    }
    let config = config_from_setup_result(result).with_credentials(credentials);
    if let Err(error) = config.validate() {
        let failures = compensate_chatgpt_setup(authenticator, &compensations);
        if !failures.is_empty() {
            anyhow::bail!("ChatGPT sign-in could not be rolled back safely. Setup was not saved. Run `finch auth status chatgpt` before trying again");
        }
        let cause = chatgpt_setup_failure_cause(&error);
        return Ok(ChatGptSetupAttempt::Recoverable(ChatGptSetupRecovery {
            invocation,
            credential_ref: chatgpt_references
                .iter()
                .next()
                .cloned()
                .unwrap_or_else(|| "chatgpt:default".into()),
            cause,
            summary: chatgpt_setup_failure_summary(cause),
        }));
    }
    Ok(ChatGptSetupAttempt::Ready {
        config,
        compensations,
    })
}

fn is_exact_chatgpt_setup_credential(
    credential: &crate::config::ProviderCredential,
    reference: &str,
) -> bool {
    credential.kind == crate::config::CredentialKind::OauthDevice
        && credential.provider == crate::config::CredentialProvider::ChatgptSubscription
        && credential.issuer == "openai-chatgpt"
        && credential.audience
            == crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            )
        && credential.secret_ref == format!("oauth-store:{reference}")
        && credential
            .account
            .as_deref()
            .is_some_and(|account| !account.is_empty())
        && crate::providers::chatgpt_oauth::chatgpt_required_scopes().is_subset(&credential.scopes)
}

fn is_reusable_chatgpt_setup_credential(credential: &crate::config::ProviderCredential) -> bool {
    matches!(
        &credential.lifecycle,
        crate::config::CredentialLifecycle::Active {
            expires_at,
            refreshable,
        } if *refreshable || expires_at.as_ref().is_none_or(|expiry| expiry > &Utc::now())
    )
}

fn chatgpt_setup_failure_cause(error: &anyhow::Error) -> ChatGptSetupFailureCause {
    if let Some(terminal) = error.downcast_ref::<crate::oauth::OAuthDeviceAuthorizationError>() {
        return match terminal {
            crate::oauth::OAuthDeviceAuthorizationError::Cancelled => {
                ChatGptSetupFailureCause::Cancelled
            }
            crate::oauth::OAuthDeviceAuthorizationError::Expired => {
                ChatGptSetupFailureCause::Expired
            }
            crate::oauth::OAuthDeviceAuthorizationError::Denied => ChatGptSetupFailureCause::Denied,
        };
    }
    if let Some(endpoint) =
        error.downcast_ref::<crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError>()
    {
        return match endpoint {
            crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::StartDisabledOrUnsupported => {
                ChatGptSetupFailureCause::StartDisabledOrUnsupported
            }
            crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::StartRejected(_)
            | crate::providers::chatgpt_oauth::ChatGptDeviceEndpointError::PollRejected(_) => {
                ChatGptSetupFailureCause::ProviderRejected
            }
        };
    }
    if let Some(stage) =
        error.downcast_ref::<crate::providers::chatgpt_oauth::ChatGptAuthStageError>()
    {
        return match stage {
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::PollContract => {
                ChatGptSetupFailureCause::PollContract
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::TokenExchangeRejected(_) => {
                ChatGptSetupFailureCause::TokenExchangeRejected
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::TokenExchangeContract => {
                ChatGptSetupFailureCause::TokenExchangeContract
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::IdentityVerification => {
                ChatGptSetupFailureCause::IdentityVerification
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::ClientBinding => {
                ChatGptSetupFailureCause::ClientBinding
            }
            crate::providers::chatgpt_oauth::ChatGptAuthStageError::AccountEntitlement => {
                ChatGptSetupFailureCause::AccountEntitlement
            }
        };
    }
    if error
        .downcast_ref::<crate::oauth::OAuthCredentialPersistenceError>()
        .is_some()
    {
        return ChatGptSetupFailureCause::Persistence;
    }
    ChatGptSetupFailureCause::ProtocolOrOther
}

fn chatgpt_setup_failure_summary(cause: ChatGptSetupFailureCause) -> String {
    match cause {
        ChatGptSetupFailureCause::Cancelled => "ChatGPT sign-in was cancelled. No credential was saved. You can retry, change the named credential, remove the provider, or cancel setup.",
        ChatGptSetupFailureCause::Expired => "ChatGPT sign-in expired. No credential was saved. If device login is not enabled for this account or workspace, enable device-code authorization in ChatGPT Settings > Security, then Retry sign-in for a fresh one-time code.",
        ChatGptSetupFailureCause::Denied => "ChatGPT sign-in was denied. No credential was saved. Choose Retry sign-in, change the named credential, remove the provider, or cancel setup.",
        ChatGptSetupFailureCause::StartDisabledOrUnsupported => "ChatGPT device authorization is disabled or unsupported for this account. Check ChatGPT Settings > Security, then choose Retry sign-in. No credential was saved.",
        ChatGptSetupFailureCause::ProviderRejected => "ChatGPT rejected the sign-in request. No credential was saved. Retry sign-in or choose another provider/account action.",
        ChatGptSetupFailureCause::PollContract => "ChatGPT approved the browser sign-in, but Finch could not read the completed device response. No credential was saved. Update Finch or retry after checking for a newer release.",
        ChatGptSetupFailureCause::TokenExchangeRejected => "ChatGPT approved the browser sign-in, but rejected the authorization-code exchange. No credential was saved. Retry sign-in for a fresh code; if it repeats, update Finch.",
        ChatGptSetupFailureCause::TokenExchangeContract => "ChatGPT approved the browser sign-in, but its token response was incompatible with this Finch version. No credential was saved. Update Finch before retrying.",
        ChatGptSetupFailureCause::IdentityVerification => "ChatGPT approved the browser sign-in, but Finch could not verify the signed identity response. No credential was saved. Check connectivity to auth.openai.com and retry; update Finch if it repeats.",
        ChatGptSetupFailureCause::ClientBinding => "ChatGPT approved the browser sign-in, but the signed issuer, public client, or token lifetime did not match Finch's pinned ChatGPT contract. No credential was saved. Update Finch before retrying.",
        ChatGptSetupFailureCause::AccountEntitlement => "ChatGPT approved the browser sign-in, but the signed response did not contain a usable ChatGPT account identifier. No credential was saved. Retry with the intended account; update Finch if it repeats.",
        ChatGptSetupFailureCause::Persistence => "ChatGPT sign-in was validated, but Finch could not save the named credential. No active credential was committed. Check local credential-store permissions before retrying.",
        ChatGptSetupFailureCause::ProtocolOrOther => "ChatGPT sign-in failed. No credential was saved. Retry sign-in or choose another provider/account action.",
    }
    .into()
}

fn compensate_chatgpt_setup<A>(
    authenticator: &A,
    handles: &[crate::cli::chatgpt_auth::ChatGptCompensationHandle],
) -> Vec<String>
where
    A: crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator,
{
    let mut failed = Vec::new();
    for handle in handles.iter().rev() {
        if authenticator.compensate_with_tombstone(handle).is_err() {
            failed.push(handle.reference().to_string());
        }
    }
    failed
}

/// Apply setup through the shared ceremony without a specific UI entry-point label.
///
/// New interactive entry points should call [`validate_and_apply_for`] explicitly so tests and
/// diagnostics can prove that first-run, command, and REPL setup use the same boundary.
pub async fn validate_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Command, result).await
}

/// Run the shared ceremony for automatic first-run setup.
pub async fn validate_first_run_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::FirstRun, result).await
}

/// Run the shared ceremony for the explicit `finch setup` command.
pub async fn validate_command_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Command, result).await
}

/// Run the shared ceremony for the in-session `/setup` command.
pub async fn validate_repl_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> {
    validate_and_apply_for(SetupInvocation::Repl, result).await
}

/// Convert wizard output into the complete configuration written to disk.
///
/// Keep this mapping in one place so first-run setup, `finch setup`, and the
/// in-REPL `/setup` command cannot silently persist different subsets of the
/// settings shown by the wizard.
fn config_from_setup_result(result: &SetupResult) -> crate::config::Config {
    use crate::config::Config;

    let providers = result.providers.clone();
    apply_setup_result_to_config(
        result,
        Config::with_providers(providers).with_credentials(result.credentials.clone()),
    )
}

#[cfg(test)]
fn config_from_setup_result_with_paths(
    result: &SetupResult,
    metrics_dir: std::path::PathBuf,
    constitution_path: Option<std::path::PathBuf>,
) -> crate::config::Config {
    use crate::config::Config;

    let providers = result.providers.clone();
    apply_setup_result_to_config(
        result,
        Config::with_providers_and_paths(providers, metrics_dir, constitution_path)
            .with_credentials(result.credentials.clone()),
    )
}

fn apply_setup_result_to_config(
    result: &SetupResult,
    mut new_config: crate::config::Config,
) -> crate::config::Config {
    use crate::config::FeaturesConfig;

    apply_daemon_api_key(&mut new_config, &result.finch_api_key);
    new_config.backend.coreml = result.coreml;
    new_config.active_theme = result.active_theme.clone();
    new_config.active_persona = result.default_persona.clone();
    if let Some(ref hf_tok) = result.hf_token {
        if !hf_tok.is_empty() {
            new_config.huggingface_token = Some(hf_tok.clone());
        }
    }
    new_config.features = FeaturesConfig {
        auto_approve_tools: result.auto_approve_tools,
        streaming_enabled: result.streaming_enabled,
        debug_logging: result.debug_logging,
        #[cfg(target_os = "macos")]
        gui_automation: result.gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted: result.gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available: result.gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context: result.gui_automation_permission_context.clone(),
        memory_context_lines: result.memory_context_lines,
        max_verbatim_messages: new_config.features.max_verbatim_messages,
        context_recall_k: new_config.features.context_recall_k,
        enable_summarization: new_config.features.enable_summarization,
        auto_compact_enabled: new_config.features.auto_compact_enabled,
    };
    new_config.server.mode = if result.daemon_only_mode {
        "daemon-only".to_string()
    } else {
        "full".to_string()
    };
    new_config.server.advertise = result.mdns_discovery;
    new_config.client.auto_discover = result.auto_discover;
    #[allow(deprecated)]
    {
        new_config.streaming_enabled = new_config.features.streaming_enabled;
    }
    new_config
}

/// Apply the wizard's single client key to the existing server representation.
pub fn apply_daemon_api_key(config: &mut crate::config::Config, api_key: &str) {
    let api_key = api_key.trim();
    config.server.auth_enabled = !api_key.is_empty();
    config.server.api_keys = if api_key.is_empty() {
        Vec::new()
    } else {
        vec![api_key.to_string()]
    };
}

/// Returns true if the Models section is currently in the Scanning sub-step
fn is_scanning_state(state: &WizardState) -> bool {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        matches!(adding_provider, Some(AddProviderStep::Scanning { .. }))
            || matches!(
                state.sections.get(&WizardSection::Models),
                Some(SectionState::Models {
                    catalog_refresh: Some(_),
                    ..
                })
            )
    } else {
        false
    }
}

fn advance_catalog_refresh_if_done(state: &mut WizardState) {
    let Some(SectionState::Models {
        primary_model,
        tool_models,
        adding_provider,
        catalog_models,
        catalog_model_provenance,
        catalog_source,
        catalog_refresh,
        catalog_generation,
        catalog_refreshed_at,
        catalog_error,
        ..
    }) = state.sections.get_mut(&WizardSection::Models)
    else {
        return;
    };
    let completed = catalog_refresh.as_ref().and_then(|refresh| {
        refresh
            .result
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
            .map(|result| {
                (
                    refresh.generation,
                    refresh.selection_identity.clone(),
                    result,
                )
            })
    });
    let Some((generation, selection_identity, (catalog, refresh_error))) = completed else {
        return;
    };
    *catalog_refresh = None;

    let Some(AddProviderStep::ConfigureRemote {
        provider_idx,
        name,
        api_key,
        editing_idx,
        ..
    }) = adding_provider.as_ref()
    else {
        return;
    };
    let persisted = editing_idx.and_then(|index| {
        if index == 0 {
            match primary_model {
                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                ModelConfig::Local { .. } => None,
            }
        } else {
            tool_models.get(index - 1).and_then(|model| match model {
                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                ModelConfig::Local { .. } => None,
            })
        }
    });
    let provider_id = CLOUD_PROVIDERS[*provider_idx].0;
    let Some(current_profile) = model_catalog_profile(
        provider_id,
        name,
        api_key.as_deref().unwrap_or(""),
        persisted,
    ) else {
        return;
    };
    if generation != *catalog_generation
        || selection_identity != model_catalog::profile_cache_identity(&current_profile)
    {
        return;
    }

    if let Some(AddProviderStep::ConfigureRemote { model, .. }) = adding_provider.as_mut() {
        if catalog.source == CatalogSource::Discovered
            && matches!(
                catalog_model_provenance,
                ModelSelectionProvenance::Blank | ModelSelectionProvenance::DefaultGenerated
            )
        {
            if let Some(discovered) = catalog.models.first() {
                *model = discovered.clone();
                *catalog_model_provenance = ModelSelectionProvenance::DefaultGenerated;
            }
        }
    }
    *catalog_models = catalog.models;
    *catalog_source = catalog.source;
    *catalog_refreshed_at = Some(catalog.refreshed_at);
    *catalog_error = refresh_error;
}

/// Returns true if the Models section currently has any overlay open
fn is_overlay_active(state: &WizardState) -> bool {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get(&WizardSection::Models)
    {
        adding_provider.is_some()
    } else {
        false
    }
}

/// Returns true while a section owns keyboard input for a nested editor or overlay.
/// Global save/navigation shortcuts must not steal keys from these interactions.
fn is_nested_interaction_active(state: &WizardState) -> bool {
    match state.sections.get(&state.current_section) {
        Some(SectionState::Models {
            editing_mode,
            editing_model_mode,
            ..
        }) => *editing_mode || *editing_model_mode || is_overlay_active(state),
        Some(SectionState::Personas { editing_prompt, .. }) => *editing_prompt,
        Some(SectionState::Features {
            editing_hf_token,
            editing_finch_api_key,
            #[cfg(target_os = "macos")]
            gui_automation_details_expanded,
            ..
        }) => {
            let editing = *editing_hf_token || *editing_finch_api_key;
            #[cfg(target_os = "macos")]
            {
                editing || *gui_automation_details_expanded
            }
            #[cfg(not(target_os = "macos"))]
            {
                editing
            }
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WizardAction {
    Continue,
    Save,
    Cancel,
}

/// Apply one key event independently of terminal I/O so navigation behavior is
/// consistent and directly testable.
fn handle_wizard_key(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<WizardAction> {
    if state.confirming_cancel {
        return Ok(match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => WizardAction::Cancel,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                state.confirming_cancel = false;
                WizardAction::Continue
            }
            _ => WizardAction::Continue,
        });
    }

    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        state.confirming_cancel = true;
        return Ok(WizardAction::Continue);
    }

    let nested = is_nested_interaction_active(state);
    if !nested
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('s') | KeyCode::Char('S'))
    {
        return Ok(WizardAction::Save);
    }

    match key.code {
        KeyCode::Tab if !nested => {
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                state.prev_section();
            } else {
                state.next_section();
            }
        }
        KeyCode::Left | KeyCode::Right if !nested => {
            if key.code == KeyCode::Left {
                state.prev_section();
            } else {
                state.next_section();
            }
        }
        _ => {
            if handle_section_input(state, key)? {
                return Ok(WizardAction::Save);
            }
        }
    }

    Ok(WizardAction::Continue)
}

/// If a network scan has finished, advance to SelectAgent (or close overlay if no agents)
fn advance_scan_if_done(state: &mut WizardState) {
    if let Some(SectionState::Models {
        adding_provider, ..
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Check if results are ready without holding the lock across the reassignment
        let agents_opt =
            if let Some(AddProviderStep::Scanning { results }) = adding_provider.as_ref() {
                results.try_lock().ok().and_then(|g| g.as_ref().cloned())
            } else {
                return;
            };

        if let Some(agents) = agents_opt {
            *adding_provider = if agents.is_empty() {
                None // No agents found — close overlay
            } else {
                Some(AddProviderStep::SelectAgent {
                    agents,
                    selected: 0,
                })
            };
        }
    }
}

/// Run the NEW tabbed wizard with section navigation
fn run_tabbed_wizard(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    existing_config: Option<&crate::config::Config>,
) -> Result<SetupResult> {
    let mut state = WizardState::new(existing_config);

    loop {
        terminal.draw(|f| {
            render_tabbed_wizard(f, &state);
        })?;

        // When scanning for network agents, poll with a short timeout so we can check
        // the background thread's results without blocking on keyboard input.
        let key_opt: Option<crossterm::event::KeyEvent> = if is_scanning_state(&state) {
            advance_scan_if_done(&mut state);
            advance_catalog_refresh_if_done(&mut state);
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => Some(key),
                    _ => None,
                }
            } else {
                None
            }
        } else {
            match event::read()? {
                Event::Key(key) => Some(key),
                _ => None,
            }
        };

        let Some(key) = key_opt else {
            continue;
        };

        match handle_wizard_key(&mut state, key)? {
            WizardAction::Continue => {}
            WizardAction::Save => return build_setup_result(&state),
            WizardAction::Cancel => anyhow::bail!("Setup cancelled"),
        }
    }
}

/// Handle input for the current section
fn handle_section_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    match state.current_section {
        WizardSection::Themes => handle_themes_input(state, key),
        WizardSection::Models => handle_models_input(state, key),
        WizardSection::Personas => handle_personas_input(state, key),
        WizardSection::Features => handle_features_input(state, key),
        WizardSection::Review => handle_review_input(state, key),
    }
}

/// Handle input for Themes section
fn handle_themes_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get_mut(&WizardSection::Themes)
    {
        use crate::theme::ColorTheme;
        let themes = ColorTheme::all();

        match key.code {
            KeyCode::Up => {
                if *selected_theme > 0 {
                    *selected_theme -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_theme < themes.len() - 1 {
                    *selected_theme += 1;
                }
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Themes);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Models section (unified Backend + Teachers)
fn handle_models_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    let catalog_cache_dir = state.catalog_cache_dir.clone();
    let credentials = state.credentials.clone();
    if let Some(SectionState::Models {
        primary_model,
        tool_models,
        selected_idx,
        editing_mode,
        editing_model_mode,
        model_input,
        adding_provider,
        catalog_models,
        catalog_model_provenance,
        catalog_source,
        catalog_refresh,
        catalog_generation,
        catalog_refreshed_at,
        catalog_error,
        error,
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Clear error on any input
        *error = None;

        // Handle add-provider overlay first
        if adding_provider.is_some() {
            // Build option lists used by ConfigureLocal cycling
            let local_backends: Vec<InferenceProvider> = {
                let mut v = vec![InferenceProvider::Onnx];
                #[cfg(feature = "candle")]
                v.push(InferenceProvider::Candle);
                v
            };
            // When Candle is selected, only Qwen 2.5 is currently supported
            let candle_selected = {
                #[cfg(feature = "candle")]
                {
                    matches!(
                        adding_provider,
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider: InferenceProvider::Candle,
                            ..
                        })
                    )
                }
                #[cfg(not(feature = "candle"))]
                {
                    false
                }
            };
            let local_families: Vec<ModelFamily> = if candle_selected {
                vec![ModelFamily::Qwen2]
            } else {
                vec![
                    ModelFamily::Qwen2,
                    ModelFamily::Gemma2,
                    ModelFamily::Llama3,
                    ModelFamily::Mistral,
                    ModelFamily::Phi,
                    ModelFamily::DeepSeek,
                ]
            };
            let local_sizes = [
                ModelSize::Small,
                ModelSize::Medium,
                ModelSize::Large,
                ModelSize::XLarge,
            ];
            let local_devices: Vec<ExecutionTarget> = {
                let mut v = vec![ExecutionTarget::Auto];
                #[cfg(target_os = "macos")]
                v.push(ExecutionTarget::CoreML);
                v.push(ExecutionTarget::Cpu);
                #[cfg(feature = "cuda")]
                v.push(ExecutionTarget::Cuda);
                v
            };

            match key.code {
                KeyCode::Esc => {
                    *adding_provider = None;
                    *catalog_generation = catalog_generation.wrapping_add(1);
                    *catalog_refresh = None;
                }
                KeyCode::Up => match adding_provider {
                    Some(AddProviderStep::SelectAddType { selected }) => {
                        if *selected > 0 {
                            *selected -= 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureLocal { focused_field, .. }) => {
                        *focused_field = focused_field.saturating_sub(1);
                    }
                    Some(AddProviderStep::ConfigureRemote { focused_field, .. }) => {
                        *focused_field = focused_field.saturating_sub(1);
                    }
                    Some(AddProviderStep::SelectAgent { selected, .. }) => {
                        if *selected > 0 {
                            *selected -= 1;
                        }
                    }
                    _ => {}
                },
                KeyCode::Down => match adding_provider {
                    Some(AddProviderStep::SelectAddType { selected }) => {
                        if *selected < CLOUD_PROVIDERS.len() + 1 {
                            *selected += 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureLocal { focused_field, .. }) => {
                        if *focused_field < 3 {
                            *focused_field += 1;
                        }
                    }
                    Some(AddProviderStep::ConfigureRemote {
                        api_key,
                        focused_field,
                        ..
                    }) => {
                        let last_field = if api_key.is_some() { 3 } else { 2 };
                        if *focused_field < last_field {
                            *focused_field += 1;
                        }
                    }
                    Some(AddProviderStep::SelectAgent { agents, selected }) => {
                        if *selected + 1 < agents.len() {
                            *selected += 1;
                        }
                    }
                    _ => {}
                },
                KeyCode::Left => {
                    match adding_provider {
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            focused_field,
                        }) => {
                            match *focused_field {
                                0 => {
                                    if let Some(pos) = local_backends
                                        .iter()
                                        .position(|x| *x == *inference_provider)
                                    {
                                        *inference_provider =
                                            local_backends[(pos + local_backends.len() - 1)
                                                % local_backends.len()];
                                    }
                                    // Candle only supports Qwen 2.5; reset family if needed
                                    #[cfg(feature = "candle")]
                                    if *inference_provider == InferenceProvider::Candle {
                                        *family = ModelFamily::Qwen2;
                                    }
                                }
                                1 => {
                                    if let Some(pos) =
                                        local_families.iter().position(|x| *x == *family)
                                    {
                                        *family = local_families[(pos + local_families.len() - 1)
                                            % local_families.len()];
                                    }
                                }
                                2 => {
                                    if let Some(pos) = local_sizes.iter().position(|x| *x == *size)
                                    {
                                        *size = local_sizes
                                            [(pos + local_sizes.len() - 1) % local_sizes.len()];
                                    }
                                }
                                3 => {
                                    if let Some(pos) =
                                        local_devices.iter().position(|x| *x == *execution)
                                    {
                                        *execution = local_devices
                                            [(pos + local_devices.len() - 1) % local_devices.len()];
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            model,
                            api_key,
                            focused_field,
                            ..
                        }) => {
                            match *focused_field {
                                0 => {
                                    let new_idx = (*provider_idx + CLOUD_PROVIDERS.len() - 1)
                                        % CLOUD_PROVIDERS.len();
                                    *provider_idx = new_idx;
                                    *api_key = remote_api_key_input(CLOUD_PROVIDERS[new_idx].0);
                                    // Reset model to default for new provider
                                    let default = CLOUD_PROVIDERS[new_idx].2;
                                    *model = default.to_string();
                                    *catalog_model_provenance = if default.is_empty() {
                                        ModelSelectionProvenance::Blank
                                    } else {
                                        ModelSelectionProvenance::DefaultGenerated
                                    };
                                    *catalog_models = known_models_for(CLOUD_PROVIDERS[new_idx].0);
                                    *catalog_source = CatalogSource::StaticFallback;
                                    *catalog_refreshed_at = None;
                                    *catalog_error = None;
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                                2 => {
                                    if !catalog_models.is_empty() {
                                        let pos = catalog_models
                                            .iter()
                                            .position(|m| m == model)
                                            .unwrap_or(0);
                                        let new_pos =
                                            (pos + catalog_models.len() - 1) % catalog_models.len();
                                        *model = catalog_models[new_pos].clone();
                                        *catalog_model_provenance =
                                            ModelSelectionProvenance::Cycled;
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                KeyCode::Right => {
                    match adding_provider {
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            focused_field,
                        }) => {
                            match *focused_field {
                                0 => {
                                    if let Some(pos) = local_backends
                                        .iter()
                                        .position(|x| *x == *inference_provider)
                                    {
                                        *inference_provider =
                                            local_backends[(pos + 1) % local_backends.len()];
                                    }
                                    // Candle only supports Qwen 2.5; reset family if needed
                                    #[cfg(feature = "candle")]
                                    if *inference_provider == InferenceProvider::Candle {
                                        *family = ModelFamily::Qwen2;
                                    }
                                }
                                1 => {
                                    if let Some(pos) =
                                        local_families.iter().position(|x| *x == *family)
                                    {
                                        *family = local_families[(pos + 1) % local_families.len()];
                                    }
                                }
                                2 => {
                                    if let Some(pos) = local_sizes.iter().position(|x| *x == *size)
                                    {
                                        *size = local_sizes[(pos + 1) % local_sizes.len()];
                                    }
                                }
                                3 => {
                                    if let Some(pos) =
                                        local_devices.iter().position(|x| *x == *execution)
                                    {
                                        *execution = local_devices[(pos + 1) % local_devices.len()];
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            model,
                            api_key,
                            focused_field,
                            ..
                        }) => {
                            match *focused_field {
                                0 => {
                                    let new_idx = (*provider_idx + 1) % CLOUD_PROVIDERS.len();
                                    *provider_idx = new_idx;
                                    *api_key = remote_api_key_input(CLOUD_PROVIDERS[new_idx].0);
                                    // Reset model to default for new provider
                                    let default = CLOUD_PROVIDERS[new_idx].2;
                                    *model = default.to_string();
                                    *catalog_model_provenance = if default.is_empty() {
                                        ModelSelectionProvenance::Blank
                                    } else {
                                        ModelSelectionProvenance::DefaultGenerated
                                    };
                                    *catalog_models = known_models_for(CLOUD_PROVIDERS[new_idx].0);
                                    *catalog_source = CatalogSource::StaticFallback;
                                    *catalog_refreshed_at = None;
                                    *catalog_error = None;
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                                2 => {
                                    if !catalog_models.is_empty() {
                                        let pos = catalog_models
                                            .iter()
                                            .position(|m| m == model)
                                            .unwrap_or(0);
                                        *model = catalog_models[(pos + 1) % catalog_models.len()]
                                            .clone();
                                        *catalog_model_provenance =
                                            ModelSelectionProvenance::Cycled;
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        api_key,
                        model,
                        editing_idx,
                        ..
                    }) = adding_provider.as_ref()
                    else {
                        return Ok(false);
                    };
                    let persisted = editing_idx.and_then(|index| {
                        if index == 0 {
                            match primary_model {
                                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                                ModelConfig::Local { .. } => None,
                            }
                        } else {
                            tool_models.get(index - 1).and_then(|model| match model {
                                ModelConfig::Remote { persisted, .. } => persisted.as_ref(),
                                ModelConfig::Local { .. } => None,
                            })
                        }
                    });
                    let provider_id = CLOUD_PROVIDERS[*provider_idx].0;
                    let Some(profile) = model_catalog_profile(
                        provider_id,
                        name,
                        api_key.as_deref().unwrap_or(""),
                        persisted,
                    ) else {
                        *catalog_error = Some(format!(
                            "{} does not advertise model discovery; enter a model ID manually",
                            CLOUD_PROVIDERS[*provider_idx].1
                        ));
                        return Ok(false);
                    };
                    let selected_entry = provider_entry_from_remote_model(
                        provider_id,
                        name,
                        api_key.as_deref().unwrap_or(""),
                        model,
                        persisted,
                    );
                    let named_config = matches!(selected_entry, ProviderEntry::Credentialed { .. })
                        .then(|| {
                            named_catalog_refresh_config(
                                primary_model,
                                tool_models,
                                editing_idx.unwrap_or(0),
                                &selected_entry,
                                credentials.clone(),
                            )
                        });
                    let named_profile = selected_entry.profile_name();
                    let cache_dir = match catalog_cache_dir.clone() {
                        Some(path) => path,
                        None => {
                            *catalog_error = Some(
                                "Cannot locate home directory for model catalogue cache"
                                    .to_string(),
                            );
                            return Ok(false);
                        }
                    };
                    let result: Arc<Mutex<CatalogRefreshResult>> = Arc::new(Mutex::new(None));
                    let result_for_thread = Arc::clone(&result);
                    *catalog_generation = catalog_generation.wrapping_add(1);
                    let generation = *catalog_generation;
                    let selection_identity = model_catalog::profile_cache_identity(&profile);
                    std::thread::spawn(move || {
                        let refreshed = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .map_err(anyhow::Error::from)
                            .map(|runtime| {
                                if let Some(config) = named_config {
                                    runtime.block_on(async {
                                        match model_catalog::refresh_from_config(
                                            &config,
                                            &named_profile,
                                            &crate::config::EnvironmentCredentialResolver,
                                            &cache_dir,
                                        )
                                        .await
                                        {
                                            Ok(catalog) => (catalog, None),
                                            Err(error) => {
                                                let mut fallback = model_catalog::fallback_catalog(
                                                    &profile.provider,
                                                    &profile.endpoints.models_url,
                                                );
                                                fallback.profile_id = profile.profile_id.clone();
                                                (fallback, Some(error.to_string()))
                                            }
                                        }
                                    })
                                } else {
                                    runtime.block_on(model_catalog::refresh_with_fallback(
                                        &profile, &cache_dir,
                                    ))
                                }
                            });
                        *result_for_thread.lock().unwrap() = Some(match refreshed {
                            Ok(result) => result,
                            Err(_) => {
                                let mut fallback = model_catalog::fallback_catalog(
                                    &profile.provider,
                                    &profile.endpoints.models_url,
                                );
                                fallback.profile_id = profile.profile_id.clone();
                                (
                                    fallback,
                                    Some(
                                        "Could not initialize model catalogue refresh".to_string(),
                                    ),
                                )
                            }
                        });
                    });
                    *catalog_refresh = Some(CatalogRefresh {
                        generation,
                        selection_identity,
                        result,
                    });
                    *catalog_error = None;
                }
                KeyCode::Char(c) => {
                    if let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        model,
                        api_key,
                        focused_field,
                        ..
                    }) = adding_provider
                    {
                        match *focused_field {
                            1 => {
                                name.push(c);
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                            }
                            2 => {
                                model.push(c);
                                *catalog_model_provenance = ModelSelectionProvenance::Manual;
                            }
                            3 => {
                                if let Some(api_key) = api_key.as_mut() {
                                    api_key.push(c);
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(AddProviderStep::ConfigureRemote {
                        provider_idx,
                        name,
                        model,
                        api_key,
                        focused_field,
                        ..
                    }) = adding_provider
                    {
                        match *focused_field {
                            1 => {
                                name.pop();
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                            }
                            2 => {
                                model.pop();
                                *catalog_model_provenance = if model.is_empty() {
                                    ModelSelectionProvenance::Blank
                                } else {
                                    ModelSelectionProvenance::Manual
                                };
                            }
                            3 => {
                                if let Some(api_key) = api_key.as_mut() {
                                    api_key.pop();
                                    *catalog_generation = catalog_generation.wrapping_add(1);
                                    *catalog_refresh = None;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                KeyCode::Enter => {
                    // Ignore Enter while network scan is in progress
                    if matches!(adding_provider, Some(AddProviderStep::Scanning { .. })) {
                        return Ok(false);
                    }

                    let next_step = match adding_provider.take() {
                        // ── type selection ──────────────────────────────────────────────
                        Some(AddProviderStep::SelectAddType { selected }) => {
                            let n_cloud = CLOUD_PROVIDERS.len();
                            if selected < n_cloud {
                                // Open single-screen remote dialog pre-selected to this provider
                                let default_model = CLOUD_PROVIDERS[selected].2.to_string();
                                *catalog_model_provenance = if default_model.is_empty() {
                                    ModelSelectionProvenance::Blank
                                } else {
                                    ModelSelectionProvenance::DefaultGenerated
                                };
                                *catalog_models = known_models_for(CLOUD_PROVIDERS[selected].0);
                                *catalog_source = CatalogSource::StaticFallback;
                                *catalog_refreshed_at = None;
                                *catalog_error = None;
                                *catalog_generation = catalog_generation.wrapping_add(1);
                                *catalog_refresh = None;
                                if let (Some(profile), Some(cache_dir)) = (
                                    model_catalog_profile(
                                        CLOUD_PROVIDERS[selected].0,
                                        CLOUD_PROVIDERS[selected].0,
                                        "",
                                        None,
                                    ),
                                    catalog_cache_dir.clone(),
                                ) {
                                    if let Ok(Some(cached)) =
                                        model_catalog::read_cache(&profile, &cache_dir)
                                    {
                                        *catalog_models = cached.models;
                                        *catalog_source = CatalogSource::Cache;
                                        *catalog_refreshed_at = Some(cached.refreshed_at);
                                    }
                                }
                                Some(AddProviderStep::ConfigureRemote {
                                    provider_idx: selected,
                                    name: CLOUD_PROVIDERS[selected].0.to_string(),
                                    model: default_model,
                                    api_key: remote_api_key_input(CLOUD_PROVIDERS[selected].0),
                                    focused_field: if CLOUD_PROVIDERS[selected].0 == "chatgpt" {
                                        1
                                    } else {
                                        3
                                    },
                                    editing_idx: None,
                                })
                            } else if selected == n_cloud {
                                // Open single-screen local model dialog
                                Some(AddProviderStep::ConfigureLocal {
                                    inference_provider: InferenceProvider::Onnx,
                                    family: ModelFamily::Qwen2,
                                    size: ModelSize::Medium,
                                    execution: ExecutionTarget::Auto,
                                    focused_field: 0,
                                })
                            } else {
                                // Network scan
                                let results_arc: Arc<Mutex<Option<Vec<DiscoveredService>>>> =
                                    Arc::new(Mutex::new(None));
                                let arc_clone = Arc::clone(&results_arc);
                                std::thread::spawn(move || {
                                    if let Ok(client) = ServiceDiscoveryClient::new() {
                                        let found = client
                                            .discover(Duration::from_secs(5))
                                            .unwrap_or_default();
                                        *arc_clone.lock().unwrap() = Some(found);
                                    } else {
                                        *arc_clone.lock().unwrap() = Some(vec![]);
                                    }
                                });
                                Some(AddProviderStep::Scanning {
                                    results: results_arc,
                                })
                            }
                        }
                        // ── single-screen remote dialog — confirm ────────────────────
                        Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            name,
                            model,
                            api_key,
                            editing_idx,
                            ..
                        }) => {
                            let (provider_id, _, default_model, _) =
                                CLOUD_PROVIDERS[provider_idx.min(CLOUD_PROVIDERS.len() - 1)];
                            let resolved_model = if model.is_empty() {
                                default_model.to_string()
                            } else {
                                model
                            };
                            if resolved_model.trim().is_empty() {
                                *catalog_error = Some(
                                    "Refresh the authenticated catalogue with Ctrl+R or enter a model ID manually"
                                        .to_string(),
                                );
                                Some(AddProviderStep::ConfigureRemote {
                                    provider_idx,
                                    name,
                                    model: resolved_model,
                                    api_key,
                                    focused_field: 2,
                                    editing_idx,
                                })
                            } else {
                                let persisted = editing_idx
                                    .and_then(|index| {
                                        if index == 0 {
                                            match &*primary_model {
                                                ModelConfig::Remote { persisted, .. } => {
                                                    persisted.clone()
                                                }
                                                ModelConfig::Local { .. } => None,
                                            }
                                        } else {
                                            tool_models.get(index - 1).and_then(|model| match model
                                            {
                                                ModelConfig::Remote { persisted, .. } => {
                                                    persisted.clone()
                                                }
                                                ModelConfig::Local { .. } => None,
                                            })
                                        }
                                    })
                                    .filter(|entry| entry.provider_type() == provider_id);
                                let edited = ModelConfig::Remote {
                                    provider: provider_id.to_string(),
                                    name: if name.trim().is_empty() {
                                        provider_id.to_string()
                                    } else {
                                        name.trim().to_string()
                                    },
                                    api_key: api_key.unwrap_or_default(),
                                    model: resolved_model,
                                    enabled: true,
                                    persisted,
                                };
                                if let Some(index) = editing_idx {
                                    if index == 0 {
                                        *primary_model = edited;
                                    } else if let Some(slot) = tool_models.get_mut(index - 1) {
                                        let enabled = slot.enabled();
                                        *slot = edited;
                                        slot.set_enabled(enabled);
                                    }
                                    *selected_idx = index;
                                } else if tool_models.is_empty()
                                    && is_unconfigured_placeholder(primary_model)
                                {
                                    *primary_model = edited;
                                    *selected_idx = 0;
                                } else {
                                    tool_models.push(edited);
                                    *selected_idx = tool_models.len();
                                }
                                None
                            }
                        }
                        // ── single-screen local dialog — confirm ─────────────────────
                        Some(AddProviderStep::ConfigureLocal {
                            inference_provider,
                            family,
                            size,
                            execution,
                            ..
                        }) => {
                            let replace_primary = tool_models.is_empty()
                                && is_unconfigured_placeholder(primary_model);
                            if replace_primary {
                                *primary_model = ModelConfig::Local {
                                    family,
                                    size,
                                    execution,
                                    inference_provider,
                                    enabled: true,
                                    persisted: None,
                                };
                                *selected_idx = 0;
                            } else {
                                tool_models.push(ModelConfig::Local {
                                    family,
                                    size,
                                    execution,
                                    inference_provider,
                                    enabled: true,
                                    persisted: None,
                                });
                                *selected_idx = tool_models.len();
                            }
                            None
                        }
                        // ── network scan results ─────────────────────────────────────
                        Some(AddProviderStep::SelectAgent { agents, selected }) => {
                            if !agents.is_empty() {
                                let agent = &agents[selected.min(agents.len() - 1)];
                                tool_models.push(ModelConfig::Remote {
                                    provider: "finch".to_string(),
                                    name: agent.name.clone(),
                                    api_key: String::new(),
                                    model: format!("{}:{}", agent.host, agent.port),
                                    enabled: true,
                                    persisted: None,
                                });
                                *selected_idx = tool_models.len();
                            }
                            None
                        }
                        None => None,
                        // Scanning handled above with early return
                        Some(AddProviderStep::Scanning { .. }) => None,
                    };
                    *adding_provider = next_step;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_model_mode {
            // Editing model name for the selected entry
            match key.code {
                KeyCode::Char(c) => {
                    model_input.push(c);
                }
                KeyCode::Backspace => {
                    model_input.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    // Save model name
                    let mi = model_input.clone();
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { model, .. } = primary_model {
                            *model = mi;
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { model, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            *model = mi;
                        }
                    }
                    *editing_model_mode = false;
                    model_input.clear();
                }
                _ => {}
            }
        } else if *editing_mode {
            // In API key editing mode
            let accepts_api_key = if *selected_idx == 0 {
                primary_model.accepts_api_key()
            } else {
                tool_models
                    .get(*selected_idx - 1)
                    .is_some_and(ModelConfig::accepts_api_key)
            };
            if !accepts_api_key {
                *editing_mode = false;
                *error = Some(
                    "ChatGPT subscription uses a named Finch device credential, not an API key"
                        .into(),
                );
                return Ok(false);
            }
            match key.code {
                KeyCode::Char(c) => {
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { api_key, .. } = primary_model {
                            api_key.push(c);
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { api_key, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            api_key.push(c);
                        }
                    }
                }
                KeyCode::Backspace => {
                    if *selected_idx == 0 {
                        if let ModelConfig::Remote { api_key, .. } = primary_model {
                            api_key.pop();
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { api_key, .. }) =
                            tool_models.get_mut(tool_idx)
                        {
                            api_key.pop();
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_mode = false;
                }
                _ => {}
            }
        } else {
            // Navigation mode
            match key.code {
                KeyCode::Up => {
                    if *selected_idx > 0 {
                        *selected_idx -= 1;
                    }
                }
                KeyCode::Down => {
                    let total = 1 + tool_models.len();
                    if *selected_idx < total - 1 {
                        *selected_idx += 1;
                    }
                }
                KeyCode::Char(' ') => {
                    // Toggle enabled for tool models
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if let Some(model) = tool_models.get_mut(tool_idx) {
                            model.set_enabled(!model.enabled());
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char('e') | KeyCode::Char('E') => {
                    let selected = if *selected_idx == 0 {
                        Some(&*primary_model)
                    } else {
                        tool_models.get(*selected_idx - 1)
                    };
                    if let Some(ModelConfig::Remote {
                        provider,
                        name,
                        model,
                        api_key,
                        persisted,
                        ..
                    }) = selected
                    {
                        let provider_idx = CLOUD_PROVIDERS
                            .iter()
                            .position(|(id, ..)| *id == provider)
                            .unwrap_or(0);
                        *catalog_models = known_models_for(CLOUD_PROVIDERS[provider_idx].0);
                        *catalog_model_provenance = if model.trim().is_empty() {
                            ModelSelectionProvenance::Blank
                        } else {
                            ModelSelectionProvenance::Persisted
                        };
                        *catalog_source = CatalogSource::StaticFallback;
                        *catalog_refreshed_at = None;
                        *catalog_error = None;
                        *catalog_generation = catalog_generation.wrapping_add(1);
                        *catalog_refresh = None;
                        if let (Some(profile), Some(cache_dir)) = (
                            model_catalog_profile(provider, name, api_key, persisted.as_ref()),
                            catalog_cache_dir.clone(),
                        ) {
                            if let Ok(Some(cached)) =
                                model_catalog::read_cache(&profile, &cache_dir)
                            {
                                *catalog_models = cached.models;
                                *catalog_source = CatalogSource::Cache;
                                *catalog_refreshed_at = Some(cached.refreshed_at);
                            }
                        }
                        *adding_provider = Some(AddProviderStep::ConfigureRemote {
                            provider_idx,
                            name: name.clone(),
                            model: model.clone(),
                            api_key: if provider.eq_ignore_ascii_case("chatgpt") {
                                None
                            } else {
                                Some(api_key.clone())
                            },
                            focused_field: 1,
                            editing_idx: Some(*selected_idx),
                        });
                    } else {
                        *error = Some(
                            "Local-model editing is available when adding a replacement".into(),
                        );
                    }
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    // Open add-provider overlay (type selection first)
                    *adding_provider = Some(AddProviderStep::SelectAddType { selected: 0 });
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    // Delete selected tool model (cannot delete primary)
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if tool_idx < tool_models.len() {
                            tool_models.remove(tool_idx);
                            // Adjust selection
                            if *selected_idx > tool_models.len() {
                                *selected_idx = tool_models.len();
                            }
                        }
                    }
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    // Promote selected tool to primary (swap with current primary)
                    if *selected_idx > 0 {
                        let tool_idx = *selected_idx - 1;
                        if tool_idx < tool_models.len() {
                            std::mem::swap(primary_model, &mut tool_models[tool_idx]);
                        }
                    }
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    // Skip - just move to next section without validation
                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Tab => {
                    // Always allow advancing — no key format validation
                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Esc => {
                    state.prev_section();
                }
                _ => {}
            }
        }
    }
    Ok(false)
}

/// Handle input for Personas section
fn handle_personas_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        default_persona,
        editing_prompt,
        prompt_input,
        cursor_pos,
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
        if *editing_prompt {
            // Helper: convert char index to byte offset
            let char_to_byte = |s: &str, char_idx: usize| -> usize {
                s.char_indices()
                    .nth(char_idx)
                    .map(|(b, _)| b)
                    .unwrap_or(s.len())
            };

            match key.code {
                KeyCode::Char('s')
                    if key
                        .modifiers
                        .contains(crossterm::event::KeyModifiers::CONTROL) =>
                {
                    let new_prompt = prompt_input.clone();
                    if let Some(persona) = available_personas.get_mut(*selected_idx) {
                        persona.system_prompt = new_prompt;
                    }
                    *editing_prompt = false;
                }
                KeyCode::Esc => {
                    *editing_prompt = false;
                    prompt_input.clear();
                    *cursor_pos = 0;
                }
                KeyCode::Enter => {
                    let byte = char_to_byte(prompt_input, *cursor_pos);
                    prompt_input.insert(byte, '\n');
                    *cursor_pos += 1;
                }
                KeyCode::Backspace => {
                    if *cursor_pos > 0 {
                        *cursor_pos -= 1;
                        let byte = char_to_byte(prompt_input, *cursor_pos);
                        prompt_input.remove(byte);
                    }
                }
                KeyCode::Delete => {
                    let len = prompt_input.chars().count();
                    if *cursor_pos < len {
                        let byte = char_to_byte(prompt_input, *cursor_pos);
                        prompt_input.remove(byte);
                    }
                }
                KeyCode::Left => {
                    if *cursor_pos > 0 {
                        *cursor_pos -= 1;
                    }
                }
                KeyCode::Right => {
                    let len = prompt_input.chars().count();
                    if *cursor_pos < len {
                        *cursor_pos += 1;
                    }
                }
                KeyCode::Up => {
                    // Move cursor to the same column on the line above
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    let col = before
                        .rfind('\n')
                        .map(|i| before[i + 1..].chars().count())
                        .unwrap_or(before.chars().count());
                    if let Some(prev_nl) = before.rfind('\n') {
                        let line_before_prev = &before[..prev_nl];
                        let prev_line_len = line_before_prev
                            .rfind('\n')
                            .map(|i| line_before_prev[i + 1..].chars().count())
                            .unwrap_or(line_before_prev.chars().count());
                        let new_col = col.min(prev_line_len);
                        *cursor_pos = line_before_prev.chars().count() + 1 + new_col;
                    } else {
                        *cursor_pos = 0;
                    }
                }
                KeyCode::Down => {
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    let col = before
                        .rfind('\n')
                        .map(|i| before[i + 1..].chars().count())
                        .unwrap_or(before.chars().count());
                    let after: String = prompt_input.chars().skip(*cursor_pos).collect();
                    if let Some(next_nl) = after.find('\n') {
                        let before_count =
                            prompt_input.chars().take(*cursor_pos + next_nl + 1).count();
                        let next_line: String = prompt_input.chars().skip(before_count).collect();
                        let next_line_len = next_line
                            .find('\n')
                            .map(|i| next_line[..i].chars().count())
                            .unwrap_or(next_line.chars().count());
                        let new_col = col.min(next_line_len);
                        *cursor_pos = before_count + new_col;
                    } else {
                        *cursor_pos = prompt_input.chars().count();
                    }
                }
                KeyCode::Home => {
                    let before: String = prompt_input.chars().take(*cursor_pos).collect();
                    *cursor_pos = if let Some(last_nl) = before.rfind('\n') {
                        before[..last_nl].chars().count() + 1
                    } else {
                        0
                    };
                }
                KeyCode::End => {
                    let after: String = prompt_input.chars().skip(*cursor_pos).collect();
                    let to_eol = after.find('\n').unwrap_or(after.chars().count());
                    *cursor_pos += to_eol;
                }
                KeyCode::Char(c) => {
                    let byte = char_to_byte(prompt_input, *cursor_pos);
                    prompt_input.insert(byte, c);
                    *cursor_pos += 1;
                }
                _ => {}
            }
            return Ok(false);
        }

        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < available_personas.len() - 1 {
                    *selected_idx += 1;
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                // Enter system prompt editing mode; place cursor at end
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *prompt_input = persona.system_prompt.clone();
                    *cursor_pos = prompt_input.chars().count();
                    *editing_prompt = true;
                }
            }
            KeyCode::Enter => {
                // Save the slug (not display name) so it loads correctly
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *default_persona = persona.slug.clone();
                }
                state.mark_completed(WizardSection::Personas);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
const SETTINGS_FEATURE_COUNT: usize = 10;
#[cfg(not(target_os = "macos"))]
const SETTINGS_FEATURE_COUNT: usize = 9;
#[cfg(target_os = "macos")]
const SETTINGS_HF_TOKEN_IDX: usize = 4;
#[cfg(not(target_os = "macos"))]
const SETTINGS_HF_TOKEN_IDX: usize = 3;
#[cfg(target_os = "macos")]
const SETTINGS_FINCH_API_KEY_IDX: usize = 5;
#[cfg(not(target_os = "macos"))]
const SETTINGS_FINCH_API_KEY_IDX: usize = 4;
#[cfg(target_os = "macos")]
const SETTINGS_DAEMON_ONLY_IDX: usize = 6;
#[cfg(not(target_os = "macos"))]
const SETTINGS_DAEMON_ONLY_IDX: usize = 5;
#[cfg(target_os = "macos")]
const SETTINGS_MDNS_IDX: usize = 7;
#[cfg(not(target_os = "macos"))]
const SETTINGS_MDNS_IDX: usize = 6;
#[cfg(target_os = "macos")]
const SETTINGS_AUTO_DISCOVER_IDX: usize = 8;
#[cfg(not(target_os = "macos"))]
const SETTINGS_AUTO_DISCOVER_IDX: usize = 7;
#[cfg(target_os = "macos")]
const SETTINGS_CONTEXT_IDX: usize = 9;
#[cfg(not(target_os = "macos"))]
const SETTINGS_CONTEXT_IDX: usize = 8;

/// Handle input for Features section (with arrow key navigation)
fn handle_features_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        return handle_features_input_with_gui_actions(
            state,
            key,
            &mut || AutomationBroker::new(true).availability(),
            &mut || {
                AutomationBroker::new(true)
                    .request_permission(AutomationPromptContext::for_current_session(true))
            },
        );
    }
    #[cfg(not(target_os = "macos"))]
    {
        handle_features_input_impl(state, key)
    }
}

#[cfg(target_os = "macos")]
fn handle_features_input_with_gui_actions(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    passive_check: &mut dyn FnMut() -> AutomationAvailability,
    request_permission: &mut dyn FnMut() -> AutomationPermissionResult,
) -> Result<bool> {
    handle_features_input_impl(state, key, passive_check, request_permission)
}

fn handle_features_input_impl(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
    #[cfg(target_os = "macos")] passive_check: &mut dyn FnMut() -> AutomationAvailability,
    #[cfg(target_os = "macos")] request_permission: &mut dyn FnMut() -> AutomationPermissionResult,
) -> Result<bool> {
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        hf_token,
        editing_hf_token,
        finch_api_key,
        editing_finch_api_key,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_availability,
        #[cfg(target_os = "macos")]
        gui_automation_prompt,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        #[cfg(target_os = "macos")]
        gui_automation_settings_feedback,
        #[cfg(target_os = "macos")]
        gui_automation_details_expanded,
        #[cfg(target_os = "macos")]
        gui_automation_details_scroll,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        memory_context_lines,
        selected_idx,
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        #[cfg(target_os = "macos")]
        if *gui_automation_details_expanded {
            match key.code {
                KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Esc => {
                    *gui_automation_details_expanded = false;
                    *gui_automation_details_scroll = 0;
                }
                KeyCode::Up => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_sub(1);
                }
                KeyCode::Down => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_add(1);
                }
                KeyCode::PageUp => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_sub(5);
                }
                KeyCode::PageDown => {
                    *gui_automation_details_scroll =
                        gui_automation_details_scroll.saturating_add(5);
                }
                KeyCode::Home => {
                    *gui_automation_details_scroll = 0;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_hf_token {
            // In HF token editing mode
            match key.code {
                KeyCode::Char(c) => {
                    hf_token.push(c);
                }
                KeyCode::Backspace => {
                    hf_token.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_hf_token = false;
                }
                _ => {}
            }
            return Ok(false);
        }

        if *editing_finch_api_key {
            match key.code {
                KeyCode::Char(c) => finch_api_key.push(c),
                KeyCode::Backspace => {
                    finch_api_key.pop();
                }
                KeyCode::Enter | KeyCode::Esc => {
                    *editing_finch_api_key = false;
                }
                _ => {}
            }
            return Ok(false);
        }

        #[cfg(target_os = "macos")]
        if *selected_idx == 3
            && *gui_automation
            && handle_gui_permission_input_with(
                key.code,
                gui_automation_availability,
                gui_automation_prompt,
                gui_automation_prompted,
                gui_automation_last_known_available,
                gui_automation_permission_context,
                gui_automation_settings_feedback,
                gui_automation_details_scroll,
                passive_check,
                request_permission,
            )
        {
            return Ok(false);
        }

        // Text fields and toggle rows share these constants with the renderer so
        // keyboard focus and visual selection cannot drift apart.
        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < SETTINGS_FEATURE_COUNT - 1 {
                    *selected_idx += 1;
                }
            }
            KeyCode::Left => {
                // Decrement context_lines spinner (min 1)
                if *selected_idx == SETTINGS_CONTEXT_IDX && *memory_context_lines > 1 {
                    *memory_context_lines -= 1;
                }
            }
            KeyCode::Right => {
                // Increment context_lines spinner (max 8)
                if *selected_idx == SETTINGS_CONTEXT_IDX && *memory_context_lines < 8 {
                    *memory_context_lines += 1;
                }
            }
            KeyCode::Char(' ') => {
                // Toggle selected feature (all except hf_token and ctx_lines)
                #[cfg(target_os = "macos")]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    3 => {
                        *gui_automation_settings_feedback = None;
                        *gui_automation_details_scroll = 0;
                        toggle_gui_automation_with(
                            gui_automation,
                            gui_automation_availability,
                            gui_automation_prompt,
                            gui_automation_prompted,
                            gui_automation_last_known_available,
                            gui_automation_permission_context,
                            || {
                                AutomationBroker::new(true).request_permission(
                                    AutomationPromptContext::for_current_session(true),
                                )
                            },
                        )
                    }
                    SETTINGS_DAEMON_ONLY_IDX => *daemon_only_mode = !*daemon_only_mode,
                    SETTINGS_MDNS_IDX => *mdns_discovery = !*mdns_discovery,
                    SETTINGS_AUTO_DISCOVER_IDX => *auto_discover = !*auto_discover,
                    // index 8 = ctx_lines (use ◀/▶)
                    _ => {}
                }
                #[cfg(not(target_os = "macos"))]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    SETTINGS_DAEMON_ONLY_IDX => *daemon_only_mode = !*daemon_only_mode,
                    SETTINGS_MDNS_IDX => *mdns_discovery = !*mdns_discovery,
                    SETTINGS_AUTO_DISCOVER_IDX => *auto_discover = !*auto_discover,
                    // index 7 = ctx_lines (use ◀/▶)
                    _ => {}
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                if *selected_idx == SETTINGS_HF_TOKEN_IDX {
                    *editing_hf_token = true;
                } else if *selected_idx == SETTINGS_FINCH_API_KEY_IDX {
                    *editing_finch_api_key = true;
                }
            }
            #[cfg(target_os = "macos")]
            KeyCode::Char('o') | KeyCode::Char('O') if *selected_idx == 3 && *gui_automation => {
                open_gui_settings_with(gui_automation_settings_feedback, || {
                    AutomationBroker::new(true).open_permission_settings(
                        AutomationPromptContext::for_current_session(true),
                    )
                });
            }
            #[cfg(target_os = "macos")]
            KeyCode::Char('d') | KeyCode::Char('D') if *selected_idx == 3 && *gui_automation => {
                *gui_automation_details_expanded = true;
                *gui_automation_details_scroll = 0;
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Features);
                state.next_section();
            }
            KeyCode::Esc => {
                state.prev_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

#[cfg(target_os = "macos")]
#[allow(clippy::too_many_arguments)]
fn handle_gui_permission_input_with(
    key: KeyCode,
    availability: &mut AutomationAvailability,
    prompt: &mut AutomationPromptDisposition,
    prompted: &mut bool,
    last_known_available: &mut bool,
    permission_context: &mut String,
    settings_feedback: &mut Option<GuiSettingsFeedback>,
    details_scroll: &mut u16,
    passive_check: impl FnOnce() -> AutomationAvailability,
    request_permission: impl FnOnce() -> AutomationPermissionResult,
) -> bool {
    let result = match key {
        KeyCode::Char('r') | KeyCode::Char('R') => {
            let availability = passive_check();
            AutomationPermissionResult {
                availability,
                prompt: AutomationPromptDisposition::NotNeeded,
            }
        }
        KeyCode::Char('p') | KeyCode::Char('P') => request_permission(),
        _ => return false,
    };

    *settings_feedback = None;
    *details_scroll = 0;
    if result.prompt == AutomationPromptDisposition::Requested {
        *prompted = true;
    }
    if result.availability.state == AutomationState::Available {
        *last_known_available = true;
    }
    if result.prompt == AutomationPromptDisposition::Requested
        || result.availability.state == AutomationState::Available
    {
        *permission_context = permission_context_key();
    }
    *availability = result.availability;
    *prompt = result.prompt;
    true
}

#[cfg(target_os = "macos")]
fn toggle_gui_automation_with(
    configured: &mut bool,
    availability: &mut AutomationAvailability,
    prompt: &mut AutomationPromptDisposition,
    prompted: &mut bool,
    last_known_available: &mut bool,
    permission_context: &mut String,
    request_permission: impl FnOnce() -> AutomationPermissionResult,
) {
    if *configured {
        *configured = false;
        *availability = AutomationBroker::new(false).availability();
        *prompt = AutomationPromptDisposition::NotNeeded;
    } else {
        // Persist Finch's explicit capability consent independently from the
        // result of the native macOS permission request.
        *configured = true;
        let result = request_permission();
        if result.prompt == AutomationPromptDisposition::Requested {
            *prompted = true;
        }
        if result.availability.state == AutomationState::Available {
            *last_known_available = true;
        }
        if result.prompt == AutomationPromptDisposition::Requested
            || result.availability.state == AutomationState::Available
        {
            *permission_context = permission_context_key();
        }
        *availability = result.availability;
        *prompt = result.prompt;
    }
}

#[cfg(target_os = "macos")]
fn open_gui_settings_with(
    feedback: &mut Option<GuiSettingsFeedback>,
    open_settings: impl FnOnce() -> Result<bool>,
) {
    *feedback = Some(match open_settings() {
        Ok(true) => GuiSettingsFeedback::OpenRequested,
        Ok(false) => GuiSettingsFeedback::Suppressed,
        Err(error) => {
            tracing::warn!("Could not open macOS Accessibility settings: {error}");
            GuiSettingsFeedback::Failed(error.to_string())
        }
    });
}

/// Handle input for Review section
fn handle_review_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            // Confirm and exit
            Ok(true)
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            state.prev_section();
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Build the final SetupResult from wizard state
fn build_setup_result(state: &WizardState) -> Result<SetupResult> {
    use crate::theme::ColorTheme;

    // Extract theme
    let active_theme = if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get(&WizardSection::Themes)
    {
        let themes = ColorTheme::all();
        themes[*selected_theme].name().to_lowercase()
    } else {
        "dark".to_string()
    };

    // Extract models
    let (primary_model, tool_models) = if let Some(SectionState::Models {
        primary_model,
        tool_models,
        ..
    }) = state.sections.get(&WizardSection::Models)
    {
        (primary_model.clone(), tool_models.clone())
    } else {
        anyhow::bail!("Models not configured");
    };

    // Extract persona
    let (default_persona, custom_system_prompt) = if let Some(SectionState::Personas {
        available_personas,
        selected_idx,
        default_persona,
        ..
    }) =
        state.sections.get(&WizardSection::Personas)
    {
        let custom_prompt = available_personas.get(*selected_idx).and_then(|persona| {
            let builtin = crate::config::Persona::load_builtin(&persona.slug).ok()?;
            (persona.system_prompt != builtin.behavior.system_prompt)
                .then(|| persona.system_prompt.clone())
        });
        let selected_persona = available_personas
            .get(*selected_idx)
            .map(|persona| persona.slug.clone())
            .unwrap_or_else(|| default_persona.clone());
        (selected_persona, custom_prompt)
    } else {
        ("default".to_string(), None)
    };

    // Extract features
    let (
        auto_approve,
        streaming,
        debug,
        hf_token_val,
        finch_api_key_val,
        daemon_only,
        mdns,
        auto_disc,
        memory_ctx_lines,
    ) = if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        hf_token,
        finch_api_key,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        memory_context_lines,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (
            *auto_approve,
            *streaming,
            *debug,
            if hf_token.is_empty() {
                None
            } else {
                Some(hf_token.clone())
            },
            finch_api_key.trim().to_string(),
            *daemon_only_mode,
            *mdns_discovery,
            *auto_discover,
            *memory_context_lines,
        )
    } else {
        (
            false,
            true,
            false,
            None,
            String::new(),
            false,
            false,
            true,
            4,
        )
    };

    #[cfg(target_os = "macos")]
    let gui_automation = if let Some(SectionState::Features { gui_automation, .. }) =
        state.sections.get(&WizardSection::Features)
    {
        *gui_automation
    } else {
        false
    };

    #[cfg(target_os = "macos")]
    let (
        gui_automation_prompted,
        gui_automation_last_known_available,
        gui_automation_permission_context,
    ) = if let Some(SectionState::Features {
        gui_automation_prompted,
        gui_automation_last_known_available,
        gui_automation_permission_context,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (
            *gui_automation_prompted,
            *gui_automation_last_known_available,
            gui_automation_permission_context.clone(),
        )
    } else {
        (false, false, String::new())
    };

    // Map to backward-compatible fields
    let (
        claude_api_key,
        backend_enabled,
        inference_provider,
        execution_target,
        model_family,
        model_size,
    ) = match &primary_model {
        ModelConfig::Local {
            family,
            size,
            execution,
            inference_provider,
            ..
        } => (
            String::new(), // No API key for local
            true,
            *inference_provider,
            *execution,
            *family,
            *size,
        ),
        ModelConfig::Remote {
            provider: _,
            api_key,
            ..
        } => {
            // Remote API is primary - backend disabled
            (
                api_key.clone(),
                false,
                InferenceProvider::Onnx,
                ExecutionTarget::Cpu, // Placeholder
                ModelFamily::Qwen2,   // Placeholder
                ModelSize::Medium,    // Placeholder
            )
        }
    };

    // Build teachers list from primary + tool models
    let mut teachers: Vec<TeacherEntry> = Vec::new();

    // Primary model as first teacher (if remote)
    if let ModelConfig::Remote {
        provider,
        name,
        api_key,
        model,
        ..
    } = &primary_model
    {
        teachers.push(TeacherEntry {
            provider: provider.clone(),
            api_key: api_key.clone(),
            model: if model.is_empty() {
                None
            } else {
                Some(model.clone())
            },
            base_url: None,
            name: Some(name.clone()),
        });
    }

    // Tool models as additional teachers
    for tool_model in &tool_models {
        if let ModelConfig::Remote {
            provider,
            name,
            api_key,
            model,
            enabled,
            ..
        } = tool_model
        {
            if *enabled {
                teachers.push(TeacherEntry {
                    provider: provider.clone(),
                    api_key: api_key.clone(),
                    model: if model.is_empty() {
                        None
                    } else {
                        Some(model.clone())
                    },
                    base_url: None,
                    name: Some(name.clone()),
                });
            }
        }
    }

    // A profile name is the stable `/model <name>` selector. Keep generated
    // names unique even when the same provider/model is added more than once.
    let mut used_names: HashMap<String, usize> = HashMap::new();
    for teacher in &mut teachers {
        let base = teacher
            .name
            .clone()
            .unwrap_or_else(|| teacher.provider.clone());
        let count = used_names.entry(base.to_ascii_lowercase()).or_default();
        *count += 1;
        if *count > 1 {
            teacher.name = Some(format!("{}-{}", base, count));
        }
    }

    // Rebuild the unified provider list in the exact order shown. Remote
    // models have already been normalized in `teachers`; local models must be
    // emitted directly because they have no teacher representation.
    let providers: Vec<ProviderEntry> = std::iter::once(&primary_model)
        .chain(tool_models.iter())
        .enumerate()
        .filter_map(|(index, model)| match model {
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
            ModelConfig::Remote { .. } => None,
            ModelConfig::Local {
                family,
                size,
                execution,
                inference_provider,
                enabled,
                persisted,
            } => {
                let (name, model_repo, model_path) = match persisted {
                    Some(ProviderEntry::Local {
                        name,
                        model_repo,
                        model_path,
                        ..
                    }) => (name.clone(), model_repo.clone(), model_path.clone()),
                    _ => (
                        Some(format!(
                            "local-{}-{}",
                            family.name().to_ascii_lowercase().replace(' ', "-"),
                            size.to_size_string(*family)
                                .to_ascii_lowercase()
                                .replace(' ', "-")
                        )),
                        None,
                        None,
                    ),
                };
                Some(ProviderEntry::Local {
                    inference_provider: *inference_provider,
                    execution_target: *execution,
                    model_family: *family,
                    model_size: *size,
                    model_repo,
                    model_path,
                    enabled: *enabled,
                    name,
                })
            }
        })
        .collect();

    Ok(SetupResult {
        active_theme,
        primary_model,
        tool_models,
        providers,
        credentials: state.credentials.clone(),
        claude_api_key,
        hf_token: hf_token_val,
        backend_enabled,
        inference_provider,
        execution_target,
        coreml: state.coreml,
        model_family,
        model_size,
        custom_model_repo: None,
        teachers,
        finch_api_key: finch_api_key_val,
        default_persona,
        custom_system_prompt,
        auto_approve_tools: auto_approve,
        streaming_enabled: streaming,
        debug_logging: debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        #[cfg(target_os = "macos")]
        gui_automation_prompted,
        #[cfg(target_os = "macos")]
        gui_automation_last_known_available,
        #[cfg(target_os = "macos")]
        gui_automation_permission_context,
        daemon_only_mode: daemon_only,
        mdns_discovery: mdns,
        auto_discover: auto_disc,
        memory_context_lines: memory_ctx_lines,
    })
}

/// Render the tabbed wizard UI
fn render_tabbed_wizard(f: &mut Frame, state: &WizardState) {
    #[cfg(target_os = "macos")]
    let permission_target = permission_target_description();
    #[cfg(not(target_os = "macos"))]
    let permission_target = String::new();
    render_tabbed_wizard_with_permission_target(f, state, &permission_target);
}

fn render_tabbed_wizard_with_permission_target(
    f: &mut Frame,
    state: &WizardState,
    permission_target: &str,
) {
    let size = f.area();

    // Main layout: [Tab bar | Content | Help]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tab bar
            Constraint::Min(10),   // Content area
            Constraint::Length(2), // Help text
        ])
        .split(size);

    // Render tab bar
    let tab_titles: Vec<Line> = WizardSection::all()
        .iter()
        .map(|section| {
            let name = section.name();
            let indicator = if state.is_completed(*section) {
                " ✓"
            } else {
                ""
            };
            Line::from(format!("{}{}", name, indicator))
        })
        .collect();

    let selected_idx = WizardSection::all()
        .iter()
        .position(|s| *s == state.current_section)
        .unwrap_or(0);

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Finch Setup "),
        )
        .select(selected_idx)
        .style(Style::default().fg(Color::Blue))
        .highlight_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    // Render current section content
    render_section_content(f, chunks[1], state, permission_target);

    // Render help text
    let section_help = match state.current_section {
        WizardSection::Themes => "↑/↓: Choose theme | Enter: Next",
        WizardSection::Models => "Enter: Edit provider | A: Add | D: Remove",
        WizardSection::Personas => "↑/↓: Choose style | E: Edit prompt | Enter: Next",
        WizardSection::Features => "↑/↓: Navigate | Space: Toggle | Enter: Next",
        WizardSection::Review => "Enter: Save & start",
    };
    let help_text =
        format!("{section_help} | Ctrl+S: Save | Esc: Back | Tab: Next | Ctrl+C: Cancel");

    let help = Paragraph::new(help_text)
        .style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);

    if state.confirming_cancel {
        render_cancel_confirmation(f, size);
    }
}

fn render_cancel_confirmation(f: &mut Frame, area: Rect) {
    let width = 56.min(area.width);
    let height = 7.min(area.height);
    let popup = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let dialog = Paragraph::new(
        "Discard all setup changes and cancel?\n\nY / Enter: Discard    N / Esc: Keep editing",
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
            .title(" Cancel setup? "),
    )
    .style(Style::default().bg(Color::Black).fg(Color::White))
    .alignment(Alignment::Center)
    .wrap(Wrap { trim: false });
    f.render_widget(dialog, popup);
}

/// Render the content area for the current section
fn render_section_content(f: &mut Frame, area: Rect, state: &WizardState, permission_target: &str) {
    let section_state = state.sections.get(&state.current_section);

    match section_state {
        Some(SectionState::Themes { selected_theme }) => {
            render_themes_section(f, area, *selected_theme)
        }
        Some(SectionState::Models {
            primary_model,
            tool_models,
            selected_idx,
            editing_mode,
            editing_model_mode,
            model_input,
            adding_provider,
            catalog_source,
            catalog_refresh,
            catalog_refreshed_at,
            catalog_error,
            error,
            ..
        }) => render_models_section(
            f,
            area,
            state.coreml,
            primary_model,
            tool_models,
            *selected_idx,
            *editing_mode,
            *editing_model_mode,
            model_input,
            adding_provider.as_ref(),
            catalog_source,
            catalog_refresh.is_some(),
            catalog_refreshed_at.as_ref(),
            catalog_error.as_deref(),
            error.as_deref(),
        ),
        Some(SectionState::Personas {
            available_personas,
            selected_idx,
            default_persona,
            editing_prompt,
            prompt_input,
            cursor_pos,
        }) => render_personas_section(
            f,
            area,
            available_personas,
            *selected_idx,
            default_persona,
            *editing_prompt,
            prompt_input,
            *cursor_pos,
        ),
        Some(SectionState::Features {
            auto_approve,
            streaming,
            debug,
            hf_token,
            editing_hf_token,
            finch_api_key,
            editing_finch_api_key,
            #[cfg(target_os = "macos")]
            gui_automation,
            #[cfg(target_os = "macos")]
            gui_automation_availability,
            #[cfg(target_os = "macos")]
            gui_automation_prompt,
            #[cfg(target_os = "macos")]
            gui_automation_prompted,
            #[cfg(target_os = "macos")]
            gui_automation_last_known_available,
            #[cfg(target_os = "macos")]
                gui_automation_permission_context: _,
            #[cfg(target_os = "macos")]
            gui_automation_settings_feedback,
            #[cfg(target_os = "macos")]
            gui_automation_details_expanded,
            #[cfg(target_os = "macos")]
            gui_automation_details_scroll,
            daemon_only_mode,
            mdns_discovery,
            auto_discover,
            memory_context_lines,
            selected_idx,
        }) => render_features_section(
            f,
            area,
            *auto_approve,
            *streaming,
            *debug,
            hf_token,
            *editing_hf_token,
            finch_api_key,
            *editing_finch_api_key,
            #[cfg(target_os = "macos")]
            *gui_automation,
            #[cfg(target_os = "macos")]
            gui_automation_availability,
            #[cfg(target_os = "macos")]
            *gui_automation_prompt,
            #[cfg(target_os = "macos")]
            *gui_automation_prompted,
            #[cfg(target_os = "macos")]
            *gui_automation_last_known_available,
            #[cfg(target_os = "macos")]
            gui_automation_settings_feedback.as_ref(),
            #[cfg(target_os = "macos")]
            *gui_automation_details_expanded,
            #[cfg(target_os = "macos")]
            *gui_automation_details_scroll,
            #[cfg(target_os = "macos")]
            permission_target,
            *daemon_only_mode,
            *mdns_discovery,
            *auto_discover,
            *memory_context_lines,
            *selected_idx,
        ),
        Some(SectionState::Review) => render_review_section(f, area, state),
        None => {
            let error = Paragraph::new("Error: Section state not found")
                .style(Style::default().fg(Color::Red));
            f.render_widget(error, area);
        }
    }
}

/// Render Themes section
fn render_themes_section(f: &mut Frame, area: Rect, selected_theme: usize) {
    use crate::theme::ColorTheme;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(8),    // Theme list
            Constraint::Length(8), // Preview
            Constraint::Length(3), // Instructions
        ])
        .split(area);

    let title = Paragraph::new("Theme Selection")
        .style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Render theme options with VERY obvious selection indicator
    let themes = ColorTheme::all();
    let items: Vec<ListItem> = themes
        .iter()
        .enumerate()
        .map(|(i, theme)| {
            let is_selected = i == selected_theme;
            let (prefix, suffix, style) = if is_selected {
                (
                    ">>> ",
                    " <<<",
                    Style::default()
                        .bg(Color::Black)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("    ", "", Style::default().fg(Color::Blue))
            };

            let text = format!(
                "{}{} - {}{}",
                prefix,
                theme.name(),
                theme.description(),
                suffix
            );
            ListItem::new(text).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Available Themes"),
    );
    f.render_widget(list, chunks[1]);

    // Render preview of selected theme
    let preview_theme = themes[selected_theme].to_scheme();
    let preview_lines = vec![
        Line::from(vec![
            Span::styled(
                "User: ",
                Style::default().fg(preview_theme.messages.user.to_color()),
            ),
            Span::raw("What is 2+2?"),
        ]),
        Line::from(vec![
            Span::styled(
                "Assistant: ",
                Style::default().fg(preview_theme.messages.assistant.to_color()),
            ),
            Span::raw("The answer is 4."),
        ]),
        Line::from(vec![
            Span::styled(
                "🔧 Tool: ",
                Style::default().fg(preview_theme.messages.tool.to_color()),
            ),
            Span::raw("Reading file..."),
        ]),
        Line::from(vec![
            Span::styled(
                "❌ Error: ",
                Style::default().fg(preview_theme.messages.error.to_color()),
            ),
            Span::raw("File not found"),
        ]),
    ];

    let preview = Paragraph::new(preview_lines)
        .block(Block::default().borders(Borders::ALL).title("Preview"))
        .wrap(Wrap { trim: false });
    f.render_widget(preview, chunks[2]);

    let instructions = Paragraph::new(
        "Use ↑/↓ arrow keys to move selection (>>> theme <<<)\n\
         Selected theme shows with white background. Press Enter to confirm.",
    )
    .style(
        Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
    )
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[3]);
}

/// Render Models section (unified Backend + Teachers)
fn execution_target_display(execution: ExecutionTarget, coreml: CoreMlConfig) -> String {
    #[cfg(target_os = "macos")]
    if execution == ExecutionTarget::CoreML {
        return format!("CoreML ({})", coreml.compute_units.name());
    }

    execution.name().to_string()
}

#[allow(clippy::too_many_arguments)]
fn render_models_section(
    f: &mut Frame,
    area: Rect,
    coreml: CoreMlConfig,
    primary_model: &ModelConfig,
    tool_models: &[ModelConfig],
    selected_idx: usize,
    editing_mode: bool,
    editing_model_mode: bool,
    model_input: &str,
    adding_provider: Option<&AddProviderStep>,
    catalog_source: &CatalogSource,
    catalog_refreshing: bool,
    catalog_refreshed_at: Option<&DateTime<Utc>>,
    catalog_error: Option<&str>,
    error: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Length(4), // Description
            Constraint::Min(6),    // Primary model + tool models
            Constraint::Length(3), // Input panel (edit mode) or dim hint
            Constraint::Length(2), // Instructions
            Constraint::Length(2), // Error (if present)
        ])
        .split(area);

    let title = Paragraph::new("AI Providers")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Show helpful hint when no key is configured
    let has_key = match primary_model {
        ModelConfig::Remote {
            provider,
            api_key,
            persisted,
            ..
        } if provider.eq_ignore_ascii_case("chatgpt") => {
            matches!(persisted, Some(ProviderEntry::Credentialed { .. }))
        }
        ModelConfig::Remote { api_key, .. } => !api_key.is_empty(),
        ModelConfig::Local { .. } => true,
    };

    let description_text = if matches!(
        primary_model,
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("chatgpt")
    ) {
        "ChatGPT subscription uses a named Finch device credential; OpenAI Platform API keys are separate."
            .to_string()
    } else if has_key {
        format!(
            "Primary provider configured. Press A to add more providers ({} total).",
            1 + tool_models.len()
        )
    } else {
        "Paste your API key below (E), or add a provider with A.\n\
         No key yet? Get one at console.anthropic.com/keys"
            .to_string()
    };
    let description = Paragraph::new(description_text)
        .style(Style::default().fg(Color::Blue))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    f.render_widget(description, chunks[1]);

    // Build list items: primary model + tool models
    let mut items = vec![];

    // Primary model - make selection VERY obvious
    let is_selected = selected_idx == 0;
    let (prefix, suffix, primary_style) = if is_selected {
        (
            ">>> ",
            " <<<",
            Style::default()
                .bg(Color::Black)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("    ", "", Style::default().fg(Color::Blue))
    };

    let primary_display = match primary_model {
        ModelConfig::Local {
            family,
            size,
            execution,
            ..
        } => {
            format!(
                "{}★ Primary: Local {} {} ({}){}",
                prefix,
                family.name(),
                model_size_display(size),
                execution_target_display(*execution, coreml),
                suffix
            )
        }
        ModelConfig::Remote {
            provider,
            name,
            api_key,
            model,
            ..
        } => {
            let key_display = if provider.eq_ignore_ascii_case("chatgpt") {
                "Named device credential".to_string()
            } else if api_key.is_empty() {
                "[Not configured]".to_string()
            } else {
                format!(
                    "{}...{}",
                    &api_key.chars().take(10).collect::<String>(),
                    api_key
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                )
            };
            let model_display = if !model.is_empty() {
                format!(" - {}", model)
            } else {
                String::new()
            };
            format!(
                "{}★ Primary: {}{} [{}]{}",
                prefix, name, model_display, key_display, suffix
            )
        }
    };

    items.push(ListItem::new(primary_display).style(primary_style));

    // Tool models - make selection VERY obvious
    for (idx, tool_model) in tool_models.iter().enumerate() {
        let tool_idx = idx + 1;
        let is_tool_selected = selected_idx == tool_idx;

        let (prefix, suffix, style) = if is_tool_selected {
            (
                ">>> ",
                " <<<",
                Style::default()
                    .bg(Color::Black)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else if tool_model.enabled() {
            ("    ", "", Style::default())
        } else {
            ("    ", "", Style::default().fg(Color::DarkGray))
        };

        let checkbox = if tool_model.enabled() { "☑" } else { "☐" };

        let display = match tool_model {
            ModelConfig::Local { family, size, .. } => {
                format!(
                    "{}{} Tool: Local {} {}{}",
                    prefix,
                    checkbox,
                    family.name(),
                    model_size_display(size),
                    suffix
                )
            }
            ModelConfig::Remote { name, model, .. } => {
                let model_display = if !model.is_empty() {
                    format!(" - {}", model)
                } else {
                    String::new()
                };
                format!(
                    "{}{} Tool: {}{}{}",
                    prefix, checkbox, name, model_display, suffix
                )
            }
        };

        items.push(ListItem::new(display).style(style));
    }

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("AI Providers"));
    f.render_widget(list, chunks[2]);

    // Input panel (chunks[3]): bordered text box when in editing mode, dim hint otherwise
    let selected_accepts_api_key = if selected_idx == 0 {
        primary_model.accepts_api_key()
    } else {
        tool_models
            .get(selected_idx - 1)
            .is_some_and(ModelConfig::accepts_api_key)
    };
    if editing_mode && selected_accepts_api_key {
        // Show current API key in a bordered box so the user sees what they're typing
        let current_key = if selected_idx == 0 {
            match primary_model {
                ModelConfig::Remote { api_key, .. } => api_key.as_str(),
                _ => "",
            }
        } else {
            match tool_models.get(selected_idx - 1) {
                Some(ModelConfig::Remote { api_key, .. }) => api_key.as_str(),
                _ => "",
            }
        };
        let panel = Paragraph::new(format!("{}█", current_key)).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Edit API Key")
                .border_style(Style::default().fg(Color::Yellow)),
        );
        f.render_widget(panel, chunks[3]);
    } else if editing_mode {
        let panel = Paragraph::new("Named Finch device credential; no API key input").block(
            Block::default()
                .borders(Borders::ALL)
                .title("ChatGPT authentication")
                .border_style(Style::default().fg(Color::Yellow)),
        );
        f.render_widget(panel, chunks[3]);
    } else if editing_model_mode {
        let panel = Paragraph::new(format!("{}█", model_input)).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Edit Model")
                .border_style(Style::default().fg(Color::Yellow)),
        );
        f.render_widget(panel, chunks[3]);
    } else {
        let hint = Paragraph::new("Press Enter to edit the selected provider · P for primary")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(hint, chunks[3]);
    }

    // Instructions (chunks[4])
    let instructions_text = if editing_mode || editing_model_mode {
        "Type here | Enter/Esc: Save & return"
    } else {
        "Enter: Edit | P: Primary | A: Add | D: Remove | Tab: Next"
    };
    let instructions = Paragraph::new(instructions_text)
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[4]);

    // Error message (chunks[5], if present)
    if let Some(err) = error {
        let error_widget = Paragraph::new(err)
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Center);
        f.render_widget(error_widget, chunks[5]);
    }

    // Render add-provider overlay if active
    if let Some(step) = adding_provider {
        render_add_provider_overlay(
            f,
            area,
            coreml,
            step,
            catalog_source,
            catalog_refreshing,
            catalog_refreshed_at,
            catalog_error,
        );
    }
}

/// Render the add-provider overlay (centered box)
fn render_add_provider_overlay(
    f: &mut Frame,
    area: Rect,
    coreml: CoreMlConfig,
    step: &AddProviderStep,
    catalog_source: &CatalogSource,
    catalog_refreshing: bool,
    catalog_refreshed_at: Option<&DateTime<Utc>>,
    catalog_error: Option<&str>,
) {
    // Center a box that's 60% wide, 50% tall
    let overlay_width = (area.width * 6 / 10).max(50).min(area.width);
    let overlay_height = (area.height / 2).max(14).min(area.height);
    let overlay_x = area.x + (area.width.saturating_sub(overlay_width)) / 2;
    let overlay_y = area.y + (area.height.saturating_sub(overlay_height)) / 2;
    let overlay = Rect::new(overlay_x, overlay_y, overlay_width, overlay_height);

    // The wizard already knows which operation it is performing; say so rather
    // than telling someone editing a working provider that they are adding one
    // (#418). Only the remote form is ever reopened for an existing provider.
    let editing_existing_provider = matches!(
        step,
        AddProviderStep::ConfigureRemote {
            editing_idx: Some(_),
            ..
        }
    );

    // Clear the overlay area with a filled block
    let background = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(if editing_existing_provider {
            " Edit AI Provider "
        } else {
            " Add AI Provider "
        })
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(Color::Black));
    f.render_widget(background, overlay);

    let inner = Rect::new(
        overlay.x + 1,
        overlay.y + 1,
        overlay.width.saturating_sub(2),
        overlay.height.saturating_sub(2),
    );

    match step {
        // ── type selection — shows all providers directly ────────────────────────────
        AddProviderStep::SelectAddType { selected } => {
            let n_cloud = CLOUD_PROVIDERS.len();
            let mut items: Vec<ListItem> = CLOUD_PROVIDERS
                .iter()
                .enumerate()
                .map(|(i, (_, display_name, _, hint))| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (
                            ">>> ",
                            " <<<",
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let lines = vec![
                        Line::from(format!("{}{}{}", prefix, display_name, suffix)).style(style),
                        Line::from(format!("        {}", hint))
                            .style(Style::default().fg(Color::DarkGray)),
                    ];
                    ListItem::new(lines)
                })
                .collect();
            {
                let is_sel = *selected == n_cloud;
                let (prefix, suffix, style) = if is_sel {
                    (
                        ">>> ",
                        " <<<",
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("    ", "", Style::default().fg(Color::Cyan))
                };
                items.push(ListItem::new(vec![
                    Line::from(format!("{}Local model{}", prefix, suffix)).style(style),
                    Line::from("        Run a model on this machine (no internet after download)")
                        .style(Style::default().fg(Color::DarkGray)),
                ]));
            }
            {
                let is_sel = *selected == n_cloud + 1;
                let (prefix, suffix, style) = if is_sel {
                    (
                        ">>> ",
                        " <<<",
                        Style::default()
                            .fg(Color::White)
                            .bg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    ("    ", "", Style::default().fg(Color::DarkGray))
                };
                items.push(ListItem::new(vec![
                    Line::from(format!("{}Scan local network{}", prefix, suffix)).style(style),
                    Line::from("        Discover other Finch instances running on your LAN")
                        .style(Style::default().fg(Color::DarkGray)),
                ]));
            }
            let list = List::new(items).block(
                Block::default().title("Add AI Provider  ↑/↓: Move | Enter: Select | Esc: Cancel"),
            );
            f.render_widget(list, inner);
        }
        // ── single-screen cloud provider dialog ──────────────────────────────────────
        AddProviderStep::ConfigureRemote {
            provider_idx,
            name,
            model,
            api_key,
            focused_field,
            editing_idx,
        } => {
            render_configure_remote_overlay(
                f,
                inner,
                *provider_idx,
                name,
                model,
                api_key.as_deref(),
                *focused_field,
                editing_idx.is_some(),
                catalog_source,
                catalog_refreshing,
                catalog_refreshed_at,
                catalog_error,
            );
        }
        // ── single-screen local model dialog ─────────────────────────────────────────
        AddProviderStep::ConfigureLocal {
            inference_provider,
            family,
            size,
            execution,
            focused_field,
        } => {
            render_configure_local_overlay(
                f,
                inner,
                coreml,
                *inference_provider,
                *family,
                *size,
                *execution,
                *focused_field,
            );
        }
        // ── network scan path ─────────────────────────────────────────────────────────
        AddProviderStep::Scanning { .. } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Scanning for Finch agents on local network…",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "(this takes up to 5 seconds)",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Esc: Cancel",
                    Style::default().fg(Color::Yellow),
                )),
            ];
            let para = Paragraph::new(lines)
                .alignment(Alignment::Center)
                .wrap(Wrap { trim: false });
            f.render_widget(para, inner);
        }
        AddProviderStep::SelectAgent { agents, selected } => {
            let items: Vec<ListItem> = agents
                .iter()
                .enumerate()
                .map(|(i, agent)| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (
                            ">>> ",
                            " <<<",
                            Style::default()
                                .fg(Color::White)
                                .bg(Color::DarkGray)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let label = format!(
                        "{}{} @ {}:{}{}",
                        prefix, agent.name, agent.host, agent.port, suffix
                    );
                    ListItem::new(Line::from(label).style(style))
                })
                .collect();
            let list = List::new(items).block(
                Block::default().title("Discovered agents  ↑/↓: Move | Enter: Add | Esc: Cancel"),
            );
            f.render_widget(list, inner);
        }
    }
}

fn format_catalog_refresh_time(refreshed_at: &DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now
        .signed_duration_since(*refreshed_at)
        .num_seconds()
        .max(0);
    let age = if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    };
    format!("{} ({age})", refreshed_at.format("%Y-%m-%d %H:%M UTC"))
}

fn format_catalog_label(
    catalog_source: &CatalogSource,
    catalog_refreshing: bool,
    catalog_refreshed_at: Option<&DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    if catalog_refreshing {
        return "Refreshing authenticated model catalogue…".to_string();
    }

    let source = match catalog_source {
        CatalogSource::Discovered => "provider discovery".to_string(),
        CatalogSource::Cache => "local cache".to_string(),
        CatalogSource::StaticFallback => format!(
            "bundled fallback snapshot (as of {}; incomplete)",
            model_catalog::STATIC_FALLBACK_AS_OF
        ),
    };
    let refreshed = if *catalog_source == CatalogSource::StaticFallback {
        String::new()
    } else {
        catalog_refreshed_at
            .map(|refreshed| format!(" · {}", format_catalog_refresh_time(refreshed, now)))
            .unwrap_or_default()
    };
    format!("Models: {source}{refreshed} · Ctrl+R refresh · model ID remains editable")
}

/// Render single-screen cloud provider configuration dialog
fn render_configure_remote_overlay(
    f: &mut Frame,
    area: Rect,
    provider_idx: usize,
    name: &str,
    model: &str,
    api_key: Option<&str>,
    focused_field: usize,
    editing: bool,
    catalog_source: &CatalogSource,
    catalog_refreshing: bool,
    catalog_refreshed_at: Option<&DateTime<Utc>>,
    catalog_error: Option<&str>,
) {
    let (provider_id, provider_name, _default_model, key_hint) =
        CLOUD_PROVIDERS[provider_idx.min(CLOUD_PROVIDERS.len() - 1)];

    // Row rendering helper: label + bracketed value, highlighted when focused
    let make_row =
        |label: &str, value: &str, focused: bool, is_text_input: bool| -> Line<'static> {
            let label_str = format!("{:<10}", label);
            let value_str = if is_text_input && focused {
                format!("[ {}█ ]", value)
            } else if focused {
                format!("[◄ {:<34}►]", value)
            } else {
                format!("[  {:<34} ]", value)
            };
            let (label_style, value_style) = if focused {
                (
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (
                    Style::default().fg(Color::DarkGray),
                    Style::default().fg(Color::Cyan),
                )
            };
            Line::from(vec![
                Span::styled(label_str, label_style),
                Span::styled(value_str, value_style),
            ])
        };

    let provider_value = format!("{} ({})", provider_name, provider_id);
    let model_display = if model.is_empty() { "(default)" } else { model };
    let mut lines = vec![
        Line::from(""),
        make_row("Provider", &provider_value, focused_field == 0, false),
        make_row("Name", name, focused_field == 1, true),
        make_row("Model", model_display, focused_field == 2, true),
    ];
    if let Some(api_key) = api_key {
        let key_display = if api_key.is_empty() {
            String::new()
        } else {
            let visible: String = api_key.chars().take(12).collect();
            format!("{}…", visible)
        };
        lines.push(make_row("API Key", &key_display, focused_field == 3, true));
    } else {
        lines.push(make_row(
            "Auth",
            "Finch-native device sign-in after save",
            false,
            false,
        ));
    }
    lines.extend([
        Line::from(""),
        Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::DarkGray),
        )),
    ]);

    // Hint line
    lines.push(Line::from(Span::styled(
        key_hint,
        Style::default().fg(Color::DarkGray),
    )));

    let catalog_label = format_catalog_label(
        catalog_source,
        catalog_refreshing,
        catalog_refreshed_at,
        Utc::now(),
    );
    lines.push(Line::from(Span::styled(
        catalog_label,
        Style::default().fg(Color::Cyan),
    )));
    if let Some(error) = catalog_error {
        lines.push(Line::from(Span::styled(
            format!("Refresh warning: {error}"),
            Style::default().fg(Color::Yellow),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        if editing {
            "↑↓ navigate · type to edit · Ctrl+R refresh · Enter saves · Esc cancels"
        } else {
            "↑↓ navigate · ←→ change provider/model · Ctrl+R refresh · Enter adds · Esc back"
        },
        Style::default().fg(Color::Yellow),
    )));

    let para = Paragraph::new(lines)
        .block(Block::default().title(if editing {
            "Edit Provider"
        } else {
            "Add Cloud Provider"
        }))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// Render single-screen local model configuration dialog
fn render_configure_local_overlay(
    f: &mut Frame,
    area: Rect,
    coreml: CoreMlConfig,
    inference_provider: InferenceProvider,
    family: ModelFamily,
    size: ModelSize,
    execution: ExecutionTarget,
    focused_field: usize,
) {
    // Row rendering helper: label + bracketed value, highlighted when focused
    let make_row = |label: &str, value: &str, focused: bool| -> Line<'static> {
        let label_str = format!("{:<10}", label);
        let value_str = if focused {
            format!("[◄ {:<34}►]", value)
        } else {
            format!("[  {:<34} ]", value)
        };
        let (label_style, value_style) = if focused {
            (
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                Style::default().fg(Color::DarkGray),
                Style::default().fg(Color::Cyan),
            )
        };
        Line::from(vec![
            Span::styled(label_str, label_style),
            Span::styled(value_str, value_style),
        ])
    };

    let backend_name = match inference_provider {
        InferenceProvider::Onnx => "ONNX Runtime",
        #[cfg(feature = "candle")]
        InferenceProvider::Candle => "Candle",
    };
    // When Candle is selected, only Qwen 2.5 is supported — annotate the display
    let mut family_name = family.name().to_string();
    #[cfg(feature = "candle")]
    if inference_provider == InferenceProvider::Candle {
        family_name = format!("{} (only)", family.name());
    }
    let size_name = model_size_display(&size);
    let device_name = execution_target_display(execution, coreml);

    let mut lines = vec![
        Line::from(""),
        make_row("Backend", backend_name, focused_field == 0),
        make_row("Family", &family_name, focused_field == 1),
        make_row("Size", size_name, focused_field == 2),
        make_row("Device", &device_name, focused_field == 3),
        Line::from(""),
        Line::from(Span::styled(
            "─".repeat(area.width as usize),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    // Preview line: RAM estimate + resolved model repo
    let repo_preview = compatibility::get_repository(inference_provider, family, size)
        .map(|r| format!("→ {}", r))
        .unwrap_or_else(|| "(no model available for this combination)".to_string());

    let ram_estimate = match size {
        ModelSize::Small => "~2 GB RAM",
        ModelSize::Medium => "~4 GB RAM",
        ModelSize::Large => "~8 GB RAM",
        ModelSize::XLarge => "~16 GB RAM",
    };

    lines.push(Line::from(vec![
        Span::styled(
            format!("{}  ", ram_estimate),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(repo_preview, Style::default().fg(Color::DarkGray)),
    ]));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "↑↓ navigate · ←→ change · Enter to add · Esc back",
        Style::default().fg(Color::Yellow),
    )));

    let para = Paragraph::new(lines)
        .block(Block::default().title("Add Local Model"))
        .wrap(Wrap { trim: false });
    f.render_widget(para, area);
}

/// Render Personas section
#[allow(clippy::too_many_arguments)]
fn render_personas_section(
    f: &mut Frame,
    area: Rect,
    personas: &[PersonaInfo],
    selected_idx: usize,
    default_persona: &str,
    editing_prompt: bool,
    prompt_input: &str,
    cursor_pos: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Left: Persona list - make selection VERY obvious
    let items: Vec<ListItem> = personas
        .iter()
        .enumerate()
        .map(|(i, persona)| {
            let is_default = persona.name.to_lowercase() == default_persona.to_lowercase();
            let is_selected = i == selected_idx;

            let (prefix, suffix, style) = if is_selected {
                (
                    ">>> ",
                    " <<<",
                    Style::default()
                        .bg(Color::Black)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )
            } else if is_default {
                ("★   ", "", Style::default().fg(Color::Yellow))
            } else {
                ("    ", "", Style::default())
            };

            ListItem::new(format!("{}{}{}", prefix, persona.name, suffix)).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Choose a Style"),
    );

    f.render_widget(list, chunks[0]);

    // Right: Preview or edit
    if let Some(persona) = personas.get(selected_idx) {
        if editing_prompt {
            // Edit mode: block cursor (█) at cursor_pos; char under cursor is replaced by block
            let before: String = prompt_input.chars().take(cursor_pos).collect();
            let after: String = prompt_input.chars().skip(cursor_pos + 1).collect();
            let edit_text = format!("{}\u{2588}{}", before, after);
            let mut lines = vec![
                Line::from(Span::styled(
                    "Editing system prompt  (Ctrl+S: Save | Esc: Cancel)",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            for line in edit_text.lines() {
                lines.push(Line::from(line.to_string()));
            }
            let edit_area = Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title("Edit System Prompt")
                        .border_style(Style::default().fg(Color::Yellow)),
                )
                .wrap(Wrap { trim: false });
            f.render_widget(edit_area, chunks[1]);
        } else {
            let preview_lines = vec![
                Line::from(vec![
                    Span::styled("Name: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(&persona.name),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        "Description: ",
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(&persona.description),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "System Prompt:",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(persona.system_prompt.as_str()),
                Line::from(""),
                Line::from(Span::styled(
                    "E: Edit system prompt",
                    Style::default().fg(Color::DarkGray),
                )),
            ];

            let preview = Paragraph::new(preview_lines)
                .block(Block::default().borders(Borders::ALL).title("Preview"))
                .wrap(Wrap { trim: false });
            f.render_widget(preview, chunks[1]);
        }
    }
}

/// Render Features section (all settings visible)
#[cfg(target_os = "macos")]
fn gui_automation_status_lines(
    configured: bool,
    availability: &AutomationAvailability,
    prompt: AutomationPromptDisposition,
    prompted: bool,
    last_known_available: bool,
    target_description: &str,
    settings_feedback: Option<&GuiSettingsFeedback>,
) -> Vec<String> {
    let summary = match (availability.state, prompt) {
        (AutomationState::Disabled, _) => "Finch capability consent is disabled",
        (AutomationState::Unsupported, _) => "Configured, but unsupported on this launch",
        (AutomationState::Available, _) => {
            "Configured; macOS reports the current Finch process is Accessibility-trusted; Finch still approves each effect"
        }
        (AutomationState::PermissionRequired, AutomationPromptDisposition::SuppressedRemote) => {
            "Configured; current Finch process is not Accessibility-trusted (prompt suppressed over SSH); press P locally to request, or R to re-check"
        }
        (
            AutomationState::PermissionRequired,
            AutomationPromptDisposition::SuppressedNonInteractive,
        ) => {
            "Configured; current Finch process is not Accessibility-trusted (headless prompt suppressed); press P in an interactive session"
        }
        (AutomationState::PermissionRequired, _) if last_known_available => {
            "Configured; current Finch process is not Accessibility-trusted after a prior successful observation (access revoked or code identity changed); press R to re-check or P to request"
        }
        (AutomationState::PermissionRequired, AutomationPromptDisposition::Requested) => {
            "Configured; macOS prompt requested, but the current Finch process is not Accessibility-trusted yet; press R to verify or P to request again"
        }
        (AutomationState::PermissionRequired, _) if prompted => {
            "Configured; current Finch process remains untrusted after an earlier request; press R to re-check or P to request again"
        }
        (AutomationState::PermissionRequired, _) => {
            "Configured; current Finch process is not Accessibility-trusted; press R to check or P to request the macOS prompt"
        }
    };

    let mut lines = Vec::new();
    if let Some(feedback) = settings_feedback {
        lines.push(format!("Settings action: {}", feedback.full_message()));
    }
    lines.push(format!("Trust status: {summary}"));
    if configured
        && matches!(
            availability.state,
            AutomationState::PermissionRequired | AutomationState::Available
        )
    {
        lines.extend(
            target_description
                .lines()
                .map(|line| format!("Diagnostic only — {line}")),
        );
    }
    if availability.state == AutomationState::PermissionRequired {
        lines.push(
            "Recovery: a checkbox or prompt is not proof of access. Press P to request the macOS prompt, or open System Settings → Privacy & Security → Accessibility, then press R for a passive re-check of this live process. If it remains untrusted, relaunch the same executable/host context and check again."
                .to_string(),
        );
    }
    lines.push(
        "This full view is read/scroll only; clipboard copying is unavailable in the setup wizard."
            .to_string(),
    );
    lines
}

#[allow(clippy::too_many_arguments)]
fn render_features_section(
    f: &mut Frame,
    area: Rect,
    auto_approve: bool,
    streaming: bool,
    debug: bool,
    hf_token: &str,
    editing_hf_token: bool,
    finch_api_key: &str,
    editing_finch_api_key: bool,
    #[cfg(target_os = "macos")] gui_automation: bool,
    #[cfg(target_os = "macos")] gui_automation_availability: &AutomationAvailability,
    #[cfg(target_os = "macos")] gui_automation_prompt: AutomationPromptDisposition,
    #[cfg(target_os = "macos")] gui_automation_prompted: bool,
    #[cfg(target_os = "macos")] gui_automation_last_known_available: bool,
    #[cfg(target_os = "macos")] gui_automation_settings_feedback: Option<&GuiSettingsFeedback>,
    #[cfg(target_os = "macos")] gui_automation_details_expanded: bool,
    #[cfg(target_os = "macos")] gui_automation_details_scroll: u16,
    #[cfg(target_os = "macos")] gui_automation_target_description: &str,
    daemon_only_mode: bool,
    mdns_discovery: bool,
    auto_discover: bool,
    memory_context_lines: usize,
    selected_idx: usize,
) {
    #[cfg(target_os = "macos")]
    let show_gui_details = selected_idx == 3 && gui_automation;
    #[cfg(not(target_os = "macos"))]
    let show_gui_details = false;

    #[cfg(target_os = "macos")]
    let gui_automation_status = gui_automation_status_lines(
        gui_automation,
        gui_automation_availability,
        gui_automation_prompt,
        gui_automation_prompted,
        gui_automation_last_known_available,
        gui_automation_target_description,
        gui_automation_settings_feedback,
    );

    #[cfg(target_os = "macos")]
    let expanded_gui_details = show_gui_details && gui_automation_details_expanded;
    #[cfg(not(target_os = "macos"))]
    let expanded_gui_details = false;

    let condensed_layout = area.height < 18;
    let title_height = if condensed_layout { 1 } else { 3 };
    let instructions_height = if condensed_layout { 1 } else { 3 };
    let detail_height = if show_gui_details && !expanded_gui_details {
        let preferred = if area.width < 60 { 10 } else { 7 };
        let available = area
            .height
            .saturating_sub(title_height + instructions_height + 4);
        preferred.min(available)
    } else {
        0
    };
    let mut constraints = vec![Constraint::Length(title_height), Constraint::Min(4)];
    if detail_height > 0 {
        constraints.push(Constraint::Length(detail_height));
    }
    constraints.push(Constraint::Length(instructions_height));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    let title = Paragraph::new("Settings")
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    #[cfg(target_os = "macos")]
    if expanded_gui_details {
        let details = Paragraph::new(gui_automation_status.join("\n"))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Full GUI automation status (read/scroll only)"),
            )
            .wrap(Wrap { trim: false })
            .scroll((gui_automation_details_scroll, 0));
        f.render_widget(details, chunks[1]);
        let instructions =
            Paragraph::new("↑/↓ or PgUp/PgDn: Scroll | Home: Top | D/Esc: Back to settings")
                .style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .alignment(Alignment::Center);
        f.render_widget(instructions, chunks[chunks.len() - 1]);
        return;
    }

    // Build feature list: toggle-able booleans, editable credentials, and a spinner.
    #[cfg(target_os = "macos")]
    let gui_automation_description = gui_automation_status
        .iter()
        .find_map(|line| line.strip_prefix("Trust status: "))
        .unwrap_or("GUI automation status unavailable");

    #[cfg(not(target_os = "macos"))]
    let bool_features: Vec<(&str, bool, &str)> = vec![
        (
            "Live responses",
            streaming,
            "See Finch's answer as it types, word by word",
        ),
        (
            "Skip permission prompts",
            auto_approve,
            "Let Finch run tools without asking each time",
        ),
        (
            "Debug logging",
            debug,
            "Write verbose logs to ~/.finch/debug.log",
        ),
        // index 3 = HF token (handled separately below)
        (
            "Daemon-only mode",
            daemon_only_mode,
            "Run as background server, no interactive REPL",
        ),
        (
            "Advertise on network",
            mdns_discovery,
            "Broadcast this Finch instance via mDNS so others can discover it",
        ),
        (
            "Discover peers on LAN",
            auto_discover,
            "Find and connect to other Finch instances at startup",
        ),
    ];
    #[cfg(target_os = "macos")]
    let bool_features: Vec<(&str, bool, &str)> = vec![
        (
            "Live responses",
            streaming,
            "See Finch's answer as it types, word by word",
        ),
        (
            "Skip permission prompts",
            auto_approve,
            "Let Finch run tools without asking each time",
        ),
        (
            "Debug logging",
            debug,
            "Write verbose logs to ~/.finch/debug.log",
        ),
        ("GUI automation", gui_automation, gui_automation_description),
        // index 4 = HF token (handled separately)
        (
            "Daemon-only mode",
            daemon_only_mode,
            "Run as background server, no interactive REPL",
        ),
        (
            "Advertise on network",
            mdns_discovery,
            "Broadcast this Finch instance via mDNS so others can discover it",
        ),
        (
            "Discover peers on LAN",
            auto_discover,
            "Find and connect to other Finch instances at startup",
        ),
    ];

    // Build list items interleaving bool features with editable credential rows.
    let mut items: Vec<ListItem> = Vec::new();
    let mut list_idx = 0usize; // tracks which visual row we're building

    for (name, enabled, desc) in bool_features.iter() {
        // Insert HF token row before the appropriate bool feature
        if list_idx == SETTINGS_HF_TOKEN_IDX {
            let is_hf_selected = selected_idx == SETTINGS_HF_TOKEN_IDX;
            let (prefix, suffix, style) = if is_hf_selected {
                (
                    ">>> ",
                    " <<<",
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("    ", "", Style::default().fg(Color::Cyan))
            };
            let token_display = if editing_hf_token {
                format!("{}HF Token: {}|{}", prefix, hf_token, suffix)
            } else if hf_token.is_empty() {
                format!("{}HF Token: [not set — press E to enter]{}", prefix, suffix)
            } else {
                let masked = format!(
                    "{}...{}",
                    &hf_token.chars().take(4).collect::<String>(),
                    hf_token
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                );
                format!("{}HF Token: {}{}", prefix, masked, suffix)
            };
            let hf_lines = vec![
                Line::from(Span::styled(token_display, style)),
                Line::from(Span::styled(
                    "        For model downloads from HuggingFace",
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            items.push(ListItem::new(hf_lines));
            list_idx += 1;
        }

        if list_idx == SETTINGS_FINCH_API_KEY_IDX {
            let is_selected = selected_idx == SETTINGS_FINCH_API_KEY_IDX;
            let (prefix, suffix, style) = if is_selected {
                (
                    ">>> ",
                    " <<<",
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                ("    ", "", Style::default().fg(Color::Cyan))
            };
            let key_display = if editing_finch_api_key {
                format!("{}Finch client key: {}|{}", prefix, finch_api_key, suffix)
            } else if finch_api_key.is_empty() {
                format!(
                    "{}Finch client key: [not set — authentication disabled; press E to enter]{}",
                    prefix, suffix
                )
            } else {
                let masked = format!(
                    "{}...{}",
                    finch_api_key.chars().take(4).collect::<String>(),
                    finch_api_key
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                );
                format!("{}Finch client key: {}{}", prefix, masked, suffix)
            };
            items.push(ListItem::new(vec![
                Line::from(Span::styled(key_display, style)),
                Line::from(Span::styled(
                    "        Key OpenAI-compatible clients use to connect to Finch",
                    Style::default().fg(Color::DarkGray),
                )),
            ]));
            list_idx += 1;
        }

        let is_selected = list_idx == selected_idx;
        let checkbox = if *enabled { "✅" } else { "☐" };
        let (prefix, suffix, name_style) = if is_selected {
            (
                ">>> ",
                " <<<",
                Style::default()
                    .bg(Color::Black)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            (
                "    ",
                "",
                if *enabled {
                    Style::default().fg(Color::Blue)
                } else {
                    Style::default().fg(Color::DarkGray)
                },
            )
        };

        let feat_lines = vec![
            Line::from(vec![
                Span::raw(prefix),
                Span::raw(format!("{} ", checkbox)),
                Span::styled(*name, name_style),
                Span::styled(suffix, name_style),
            ]),
            Line::from(vec![
                Span::raw("        "),
                Span::styled(*desc, Style::default().fg(Color::DarkGray)),
            ]),
        ];
        items.push(ListItem::new(feat_lines));
        list_idx += 1;
    }

    // If hf_idx is after all bool features, append it at the end
    if SETTINGS_HF_TOKEN_IDX >= list_idx {
        let is_hf_selected = selected_idx == list_idx;
        let (prefix, suffix, style) = if is_hf_selected {
            (
                ">>> ",
                " <<<",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Black)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("    ", "", Style::default().fg(Color::Cyan))
        };
        let token_display = if editing_hf_token {
            format!("{}HF Token: {}|{}", prefix, hf_token, suffix)
        } else if hf_token.is_empty() {
            format!("{}HF Token: [not set — press E to enter]{}", prefix, suffix)
        } else {
            let masked = format!(
                "{}...{}",
                &hf_token.chars().take(4).collect::<String>(),
                hf_token
                    .chars()
                    .rev()
                    .take(4)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );
            format!("{}HF Token: {}{}", prefix, masked, suffix)
        };
        let hf_lines = vec![
            Line::from(Span::styled(token_display, style)),
            Line::from(Span::styled(
                "        For model downloads from HuggingFace",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        items.push(ListItem::new(hf_lines));
    }

    // Context-lines spinner row (always last)
    {
        let is_selected = selected_idx == SETTINGS_CONTEXT_IDX;
        let (prefix, suffix, label_style) = if is_selected {
            (
                ">>> ",
                " <<<",
                Style::default()
                    .bg(Color::Black)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("    ", "", Style::default().fg(Color::Blue))
        };
        let ctx_lines = vec![
            Line::from(vec![
                Span::raw(prefix),
                Span::styled(
                    format!("◀ Context lines: {} ▶", memory_context_lines),
                    label_style,
                ),
                Span::styled(suffix, label_style),
            ]),
            Line::from(vec![
                Span::raw("        "),
                Span::styled(
                    "Status-strip summary lines shown below the prompt (1–8)",
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];
        items.push(ListItem::new(ctx_lines));
    }

    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Options"));
    let mut list_state = ListState::default().with_selected(Some(selected_idx));
    f.render_stateful_widget(list, chunks[1], &mut list_state);

    #[cfg(target_os = "macos")]
    if show_gui_details {
        let mut compact_lines = Vec::new();
        if let Some(feedback) = gui_automation_settings_feedback {
            compact_lines.push(Line::from(feedback.compact_message()));
        } else {
            let compact_trust = if gui_automation_availability.state == AutomationState::Available {
                "Current Finch process: trusted."
            } else {
                "Current Finch process: untrusted."
            };
            compact_lines.push(Line::from(compact_trust));
        }
        compact_lines.push(Line::from(Span::styled(
            "R: Passive check | P: Request prompt",
            Style::default().fg(Color::Cyan),
        )));
        compact_lines.push(Line::from(Span::styled(
            "O: System Settings → Privacy & Security → Accessibility",
            Style::default().fg(Color::Cyan),
        )));
        compact_lines.push(Line::from(Span::styled(
            "D: Full process/host/status",
            Style::default().fg(Color::Cyan),
        )));
        let status = Paragraph::new(compact_lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("GUI automation status"),
            )
            .wrap(Wrap { trim: false });
        f.render_widget(status, chunks[2]);
    }

    let instructions_text = if editing_hf_token {
        "Type HuggingFace token | Enter/Esc: Done"
    } else if editing_finch_api_key {
        "Type Finch client key | Enter/Esc: Done"
    } else {
        #[cfg(target_os = "macos")]
        {
            if show_gui_details {
                "R: Check | P: Prompt | O/D: More"
            } else {
                "↑/↓: Move | Space: Toggle | E: Edit | Enter: Continue"
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            "↑/↓: Move | Space: Toggle | ◀/▶: Context lines | E: Edit selected key/token | Enter: Continue"
        }
    };
    let instructions = Paragraph::new(instructions_text)
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[chunks.len() - 1]);
}

/// Render Review section
fn render_review_section(f: &mut Frame, area: Rect, state: &WizardState) {
    use crate::theme::ColorTheme;

    let title = Paragraph::new("Ready to go!")
        .style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center);

    // Build summary text
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![Span::styled(
            "Here's what you set up:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
    ];

    // Theme
    if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get(&WizardSection::Themes)
    {
        let themes = ColorTheme::all();
        let theme_name = themes[*selected_theme].name().to_string();
        lines.push(Line::from(vec![
            Span::styled("Theme: ", Style::default().fg(Color::Yellow)),
            Span::raw(theme_name),
        ]));
    }

    // Models
    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get(&WizardSection::Models)
    {
        let ai_label = match primary_model {
            ModelConfig::Remote { api_key, .. } if !api_key.is_empty() => {
                "Claude (API key configured)"
            }
            ModelConfig::Remote { .. } => "Claude (no API key — will prompt on first use)",
            ModelConfig::Local { family, size, .. } => {
                // Use a static fallback — dynamic format not possible here
                let _ = (family, size);
                "Local model"
            }
        };
        lines.push(Line::from(vec![
            Span::styled("AI: ", Style::default().fg(Color::Yellow)),
            Span::raw(ai_label),
        ]));
    }

    // Persona
    if let Some(SectionState::Personas {
        default_persona, ..
    }) = state.sections.get(&WizardSection::Personas)
    {
        lines.push(Line::from(vec![
            Span::styled("Style: ", Style::default().fg(Color::Yellow)),
            Span::raw(default_persona),
        ]));
    }

    // Features (only show user-facing ones)
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        let mut settings = vec![];
        if *streaming {
            settings.push("Live responses");
        }
        if *auto_approve {
            settings.push("Skip permission prompts");
        }

        lines.push(Line::from(vec![
            Span::styled("Settings: ", Style::default().fg(Color::Yellow)),
            Span::raw(if settings.is_empty() {
                "Defaults".to_string()
            } else {
                settings.join(", ")
            }),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![Span::styled(
        "Press Enter or Ctrl+S to save & start chatting",
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    )]));
    lines.push(Line::from(vec![Span::styled(
        "Esc: Back to settings · Ctrl+C: Cancel setup",
        Style::default().fg(Color::Gray),
    )]));

    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    f.render_widget(title, chunks[0]);

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(para, chunks[1]);
}
// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests;
