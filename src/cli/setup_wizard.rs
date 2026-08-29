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
    (
        "zai",
        "Z.ai",
        "glm-5.3-flash",
        "enter the environment variable containing your Z.ai API key",
    ),
];

fn remote_api_key_input(provider: &str) -> Option<String> {
    (!provider.eq_ignore_ascii_case("chatgpt")).then(|| default_credential_input(provider))
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

fn default_credential_input(provider: &str) -> String {
    if provider == "zai" {
        "ZAI_API_KEY".to_string()
    } else {
        String::new()
    }
}

fn zai_named_setup_entries(
    profile_name: &str,
    model: &str,
    environment_variable: &str,
) -> Result<(crate::config::ProviderCredential, ProviderEntry)> {
    use crate::config::{
        AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
        CredentialProvider, EndpointFamily, ProviderCredential, ReasoningEffort,
    };

    let credential_name = format!("{}-credential", profile_name.trim());
    let environment_variable = environment_variable.trim();
    let valid_environment_variable = !environment_variable.is_empty()
        && environment_variable
            .bytes()
            .enumerate()
            .all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_uppercase() || (index > 0 && byte.is_ascii_digit())
            });
    if !valid_environment_variable {
        anyhow::bail!(
            "Z.ai setup requires an environment-variable name such as ZAI_API_KEY; secret values are not stored in config.toml"
        );
    }

    let credential = ProviderCredential {
        name: credential_name.clone(),
        kind: CredentialKind::ApiKey,
        provider: CredentialProvider::Zai,
        issuer: "zai".into(),
        audience: AudienceBinding::standard(EndpointFamily::ZaiApi),
        tenant: None,
        project: None,
        account: None,
        scopes: std::collections::BTreeSet::new(),
        secret_ref: format!("env:{environment_variable}"),
        lifecycle: CredentialLifecycle::default(),
        revocation: Default::default(),
    };
    let profile = ProviderEntry::Credentialed {
        provider: CredentialProvider::Zai,
        credential: CredentialBinding {
            credential_ref: credential_name,
            audience: None,
            tenant: None,
            project: None,
            account: None,
            required_scopes: std::collections::BTreeSet::new(),
        },
        model: Some(model.to_string()),
        base_url: None,
        chat_path: None,
        models_path: None,
        name: Some(profile_name.to_string()),
        reasoning_effort: Some(ReasoningEffort::Max),
    };
    Ok((credential, profile))
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
            "openai" | "grok" | "mistral" | "groq" | "zai",
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
                "zai" => (
                    "https://api.z.ai/api/paas/v4",
                    "/chat/completions",
                    "/models",
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
        matches!(self, Self::Remote { provider, .. }
            if !provider.eq_ignore_ascii_case("chatgpt")
                && !provider.eq_ignore_ascii_case("zai"))
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
fn model_config_from_provider(
    provider: &ProviderEntry,
    credentials: &[crate::config::ProviderCredential],
) -> Option<ModelConfig> {
    match provider {
        ProviderEntry::Credentialed {
            provider: credential_provider,
            credential,
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
                crate::config::CredentialProvider::Zai => "zai",
                _ => credential_provider.as_str(),
            }
            .to_string(),
            name: name
                .clone()
                .unwrap_or_else(|| credential_provider.as_str().to_string()),
            api_key: if *credential_provider == crate::config::CredentialProvider::Zai {
                credentials
                    .iter()
                    .find(|metadata| metadata.name == credential.credential_ref)
                    .and_then(|metadata| metadata.secret_ref.strip_prefix("env:"))
                    .unwrap_or_default()
                    .to_string()
            } else {
                String::new()
            },
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
        _ if provider.eq_ignore_ascii_case("zai") => ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::Zai,
            credential: crate::config::CredentialBinding {
                credential_ref: format!("{}-credential", name.as_deref().unwrap_or("zai")),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: std::collections::BTreeSet::new(),
            },
            model,
            base_url: None,
            chat_path: None,
            models_path: None,
            name,
            reasoning_effort: Some(crate::config::ReasoningEffort::Max),
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
        use crate::config::persona::Persona;
        use crate::config::ColorTheme;

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
                    .filter_map(|provider| {
                        model_config_from_provider(provider, config.credentials())
                    })
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
    // Try to load existing config to pre-fill values
    let existing_config = match crate::config::load_config() {
        Ok(config) => {
            let debug_msg = format!(
                "Successfully loaded existing config with {} teachers\n",
                config.teachers.len()
            );
            if let Some(teacher) = config.active_teacher() {
                let debug_msg = format!(
                    "{}Active teacher: provider={}, key_len={}\n",
                    debug_msg,
                    teacher.provider,
                    teacher.api_key.len()
                );
                let _ = std::fs::write("/tmp/wizard_debug.log", debug_msg);
            }
            tracing::debug!(
                "Successfully loaded existing config with {} teachers",
                config.teachers.len()
            );
            Some(config)
        }
        Err(e) => {
            let debug_msg = format!("Could not load existing config: {}\n", e);
            let _ = std::fs::write("/tmp/wizard_debug.log", debug_msg);
            tracing::debug!("Could not load existing config: {}", e);
            None
        }
    };

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
    let chatgpt_references = result
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
        .collect::<std::collections::BTreeSet<_>>();
    if chatgpt_references.is_empty() {
        apply_and_save(result)?;
        return Ok(SetupApplyOutcome::Saved);
    }

    let service = crate::cli::chatgpt_auth::ChatGptAuthService::production()?;
    let config =
        prepare_chatgpt_setup_config(result, &chatgpt_references, &service, setup_cancellation())
            .await?;
    config.save()?;
    if let Some(prompt) = result.custom_system_prompt.as_deref() {
        crate::config::Persona::save_system_prompt_override(&result.default_persona, prompt)?;
    }
    Ok(SetupApplyOutcome::Saved)
}

async fn prepare_chatgpt_setup_config<A>(
    result: &SetupResult,
    chatgpt_references: &std::collections::BTreeSet<String>,
    authenticator: &A,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<crate::config::Config>
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
            let exact_authority = existing.kind == crate::config::CredentialKind::OauthDevice
                && existing.provider == crate::config::CredentialProvider::ChatgptSubscription
                && existing.issuer == "openai-chatgpt"
                && existing.audience
                    == crate::config::AudienceBinding::standard(
                        crate::config::EndpointFamily::ChatgptSubscription,
                    )
                && existing.secret_ref == format!("oauth-store:{reference}")
                && crate::providers::chatgpt_oauth::chatgpt_required_scopes()
                    .is_subset(&existing.scopes);
            if !exact_authority {
                // Preserve the hostile record so graph validation rejects it
                // before the authenticator or any OAuth socket is reached.
                continue;
            }
            let active = matches!(
                existing.lifecycle,
                crate::config::CredentialLifecycle::Active { expires_at, .. }
                    if expires_at.is_none_or(|expiry| expiry > Utc::now())
            );
            if active {
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
                let original = error.to_string();
                let context = if cancel.is_cancelled() && failures.is_empty() {
                    format!(
                        "ChatGPT login for named credential '{reference}' was cancelled; setup was not saved ({original})"
                    )
                } else if cancel.is_cancelled() {
                    format!(
                        "ChatGPT login for named credential '{reference}' was cancelled; setup was not saved ({original}); compensation conflicts for {} require local status review",
                        failures.join(",")
                    )
                } else if failures.is_empty() {
                    format!("ChatGPT login for named credential '{reference}' failed: {original}")
                } else {
                    format!(
                        "ChatGPT login for named credential '{reference}' failed ({original}); compensation conflicts for {} require local status review",
                        failures.join(",")
                    )
                };
                return Err(error).context(context);
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
        // Config validation layers profile context over the exact authority
        // mismatch. Preserve the complete, secret-free validation chain so the
        // user can repair the signed credential instead of seeing only the
        // generic profile incompatibility.
        let original = format!("{error:#}");
        let context = if failures.is_empty() {
            format!("Signed ChatGPT credential does not match the setup provider graph: {original}")
        } else {
            format!(
                "Signed ChatGPT credential does not match the setup provider graph ({original}); compensation conflicts for {} require local status review",
                failures.join(",")
            )
        };
        return Err(error).context(context);
    }
    Ok(config)
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

fn setup_cancellation() -> tokio_util::sync::CancellationToken {
    let cancel = tokio_util::sync::CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            signal.cancel();
        }
    });
    cancel
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
        use crate::config::ColorTheme;
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
    let mut credential_to_store = None;
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
                    let mut refresh_credentials = credentials.clone();
                    let selected_entry = if provider_id == "zai" && persisted.is_none() {
                        let (credential, entry) = match zai_named_setup_entries(
                            name,
                            model,
                            api_key.as_deref().unwrap_or_default(),
                        ) {
                            Ok(entries) => entries,
                            Err(error) => {
                                *catalog_error = Some(error.to_string());
                                return Ok(false);
                            }
                        };
                        if refresh_credentials
                            .iter()
                            .any(|existing| existing.name == credential.name)
                        {
                            *catalog_error = Some(format!(
                                "credential '{}' already exists; choose a unique profile name",
                                credential.name
                            ));
                            return Ok(false);
                        }
                        refresh_credentials.push(credential);
                        entry
                    } else {
                        provider_entry_from_remote_model(
                            provider_id,
                            name,
                            api_key.as_deref().unwrap_or(""),
                            model,
                            persisted,
                        )
                    };
                    let Some(profile) = model_catalog_profile(
                        provider_id,
                        name,
                        if provider_id == "zai" {
                            ""
                        } else {
                            api_key.as_deref().unwrap_or("")
                        },
                        Some(&selected_entry),
                    ) else {
                        *catalog_error = Some(format!(
                            "{} does not advertise model discovery; enter a model ID manually",
                            CLOUD_PROVIDERS[*provider_idx].1
                        ));
                        return Ok(false);
                    };
                    let named_config = matches!(selected_entry, ProviderEntry::Credentialed { .. })
                        .then(|| {
                            named_catalog_refresh_config(
                                primary_model,
                                tool_models,
                                editing_idx.unwrap_or(0),
                                &selected_entry,
                                refresh_credentials,
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
                                let mut persisted = editing_idx
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
                                let configured_name = if name.trim().is_empty() {
                                    provider_id.to_string()
                                } else {
                                    name.trim().to_string()
                                };
                                if provider_id == "zai" && persisted.is_none() {
                                    match zai_named_setup_entries(
                                        &configured_name,
                                        &resolved_model,
                                        api_key.as_deref().unwrap_or_default(),
                                    ) {
                                        Ok((credential, profile)) => {
                                            if credentials
                                                .iter()
                                                .any(|existing| existing.name == credential.name)
                                            {
                                                *catalog_error = Some(format!(
                                                    "credential '{}' already exists; choose a unique profile name",
                                                    credential.name
                                                ));
                                                return Ok(false);
                                            }
                                            credential_to_store = Some(credential);
                                            persisted = Some(profile);
                                        }
                                        Err(error) => {
                                            *catalog_error = Some(error.to_string());
                                            return Ok(false);
                                        }
                                    }
                                }
                                let edited = ModelConfig::Remote {
                                    provider: provider_id.to_string(),
                                    name: configured_name,
                                    api_key: if provider_id == "zai" {
                                        String::new()
                                    } else {
                                        api_key.unwrap_or_default()
                                    },
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
                                } else if matches!(
                                    primary_model,
                                    ModelConfig::Remote { api_key: ref k, .. } if k.is_empty()
                                ) && tool_models.is_empty()
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
                            let replace_primary = matches!(
                                primary_model,
                                ModelConfig::Remote { api_key, .. } if api_key.is_empty()
                            ) && tool_models.is_empty();
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
            if let Some(credential) = credential_to_store {
                state.credentials.push(credential);
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
                let provider = if *selected_idx == 0 {
                    match primary_model {
                        ModelConfig::Remote { provider, .. } => provider.as_str(),
                        ModelConfig::Local { .. } => "local",
                    }
                } else {
                    tool_models
                        .get(*selected_idx - 1)
                        .and_then(|model| match model {
                            ModelConfig::Remote { provider, .. } => Some(provider.as_str()),
                            ModelConfig::Local { .. } => None,
                        })
                        .unwrap_or("local")
                };
                *error = Some(if provider.eq_ignore_ascii_case("zai") {
                    "Z.ai uses a named environment reference; edit the Key env field in the provider dialog, not an API key"
                        .into()
                } else {
                    "ChatGPT subscription uses a named Finch device credential, not an API key"
                        .into()
                });
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
    use crate::config::ColorTheme;

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
    use crate::config::ColorTheme;

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
        } if provider.eq_ignore_ascii_case("chatgpt") || provider.eq_ignore_ascii_case("zai") => {
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
    } else if matches!(
        primary_model,
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("zai")
    ) {
        "Z.ai uses a named environment reference; secret values are never entered or stored in Finch configuration."
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
            } else if provider.eq_ignore_ascii_case("zai") {
                "Named environment credential".to_string()
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
        let selected_provider = if selected_idx == 0 {
            match primary_model {
                ModelConfig::Remote { provider, .. } => provider.as_str(),
                ModelConfig::Local { .. } => "local",
            }
        } else {
            tool_models
                .get(selected_idx - 1)
                .and_then(|model| match model {
                    ModelConfig::Remote { provider, .. } => Some(provider.as_str()),
                    ModelConfig::Local { .. } => None,
                })
                .unwrap_or("local")
        };
        let (message, title) = if selected_provider.eq_ignore_ascii_case("zai") {
            (
                "Named environment reference; use the provider dialog to edit Key env",
                "Z.ai authentication",
            )
        } else {
            (
                "Named Finch device credential; no API key input",
                "ChatGPT authentication",
            )
        };
        let panel = Paragraph::new(message).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
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

    // Clear the overlay area with a filled block
    let background = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Add AI Provider ")
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
                Block::default().title("Add AI provider  ↑/↓: Move | Enter: Select | Esc: Cancel"),
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
        lines.push(make_row(
            if provider_id == "zai" {
                "Key env"
            } else {
                "API Key"
            },
            &key_display,
            focused_field == 3,
            true,
        ));
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
    use crate::config::ColorTheme;

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
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn models_tab_describes_model_setup() {
        assert_eq!(WizardSection::Models.name(), "Model Setup");
    }

    #[test]
    fn global_save_is_available_from_every_top_level_screen() {
        for section in WizardSection::all() {
            let mut state = WizardState::new(None);
            state.current_section = section;
            assert_eq!(
                handle_wizard_key(
                    &mut state,
                    modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
                )
                .unwrap(),
                WizardAction::Save,
                "Ctrl+S should save from {}",
                section.name()
            );
        }
    }

    #[test]
    fn editor_local_ctrl_s_is_not_stolen_by_global_save() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Personas;
        handle_personas_input(&mut state, key(KeyCode::Char('e'))).unwrap();

        assert_eq!(
            handle_wizard_key(
                &mut state,
                modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            )
            .unwrap(),
            WizardAction::Continue
        );
        assert!(!is_nested_interaction_active(&state));
    }

    #[test]
    fn escape_closes_nested_overlay_before_leaving_screen() {
        let mut state = state_with_step(default_configure_remote(0));
        state.current_section = WizardSection::Models;
        assert_eq!(state.current_section, WizardSection::Models);

        assert_eq!(
            handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
            WizardAction::Continue
        );
        assert!(get_step(&state).is_none());
        assert_eq!(state.current_section, WizardSection::Models);
    }

    #[test]
    fn escape_closes_nested_editor_before_leaving_screen() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Personas;
        handle_personas_input(&mut state, key(KeyCode::Char('e'))).unwrap();
        assert!(is_nested_interaction_active(&state));

        assert_eq!(
            handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
            WizardAction::Continue
        );
        assert!(!is_nested_interaction_active(&state));
        assert_eq!(state.current_section, WizardSection::Personas);
    }

    #[test]
    fn escape_moves_back_one_top_level_screen_without_cancelling() {
        let expected = [
            (WizardSection::Themes, WizardSection::Themes),
            (WizardSection::Models, WizardSection::Themes),
            (WizardSection::Personas, WizardSection::Models),
            (WizardSection::Features, WizardSection::Personas),
            (WizardSection::Review, WizardSection::Features),
        ];

        for (current, previous) in expected {
            let mut state = WizardState::new(None);
            state.current_section = current;
            assert_eq!(
                handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
                WizardAction::Continue
            );
            assert_eq!(state.current_section, previous);
            assert!(!state.confirming_cancel);
        }
    }

    #[test]
    fn cancellation_requires_explicit_confirmation() {
        let mut state = WizardState::new(None);
        assert_eq!(
            handle_wizard_key(
                &mut state,
                modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            )
            .unwrap(),
            WizardAction::Continue
        );
        assert!(state.confirming_cancel);

        assert_eq!(
            handle_wizard_key(&mut state, key(KeyCode::Esc)).unwrap(),
            WizardAction::Continue
        );
        assert!(!state.confirming_cancel);

        handle_wizard_key(
            &mut state,
            modified_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        )
        .unwrap();
        assert_eq!(
            handle_wizard_key(&mut state, key(KeyCode::Char('y'))).unwrap(),
            WizardAction::Cancel
        );
    }

    #[test]
    fn save_from_non_review_uses_accumulated_selection() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Personas;
        let expected_slug = if let Some(SectionState::Personas {
            available_personas,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Personas)
        {
            *selected_idx = 1;
            available_personas[1].slug.clone()
        } else {
            panic!("expected personas section");
        };

        assert_eq!(
            handle_wizard_key(
                &mut state,
                modified_key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            )
            .unwrap(),
            WizardAction::Save
        );
        assert_eq!(
            build_setup_result(&state).unwrap().default_persona,
            expected_slug
        );
    }

    #[test]
    fn enter_opens_provider_editor_and_saves_public_name() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Models;

        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote {
            name,
            model,
            editing_idx,
            ..
        }) = state
            .sections
            .get_mut(&WizardSection::Models)
            .and_then(|section| match section {
                SectionState::Models {
                    adding_provider, ..
                } => adding_provider.as_mut(),
                _ => None,
            })
        {
            *name = "work-claude".to_string();
            *model = "manually-selected-claude".to_string();
            assert_eq!(*editing_idx, Some(0));
        } else {
            panic!("expected provider editor");
        }

        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        if let Some(ModelConfig::Remote { name, .. }) = get_primary(&state) {
            assert_eq!(name, "work-claude");
        } else {
            panic!("expected remote primary");
        }
    }

    #[test]
    fn peer_discovery_and_context_lines_have_distinct_rows() {
        assert_ne!(SETTINGS_AUTO_DISCOVER_IDX, SETTINGS_CONTEXT_IDX);
        assert_ne!(SETTINGS_FINCH_API_KEY_IDX, SETTINGS_AUTO_DISCOVER_IDX);
        assert_eq!(SETTINGS_AUTO_DISCOVER_IDX + 1, SETTINGS_CONTEXT_IDX);
        assert_eq!(SETTINGS_CONTEXT_IDX, SETTINGS_FEATURE_COUNT - 1);
    }

    #[test]
    fn wizard_settings_survive_config_mapping_and_reopen() {
        let mut state = WizardState::new(None);
        if let Some(SectionState::Features {
            auto_approve,
            #[cfg(target_os = "macos")]
            gui_automation,
            #[cfg(target_os = "macos")]
            gui_automation_prompted,
            #[cfg(target_os = "macos")]
            gui_automation_last_known_available,
            #[cfg(target_os = "macos")]
            gui_automation_permission_context,
            daemon_only_mode,
            mdns_discovery,
            auto_discover,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *auto_approve = true;
            #[cfg(target_os = "macos")]
            {
                *gui_automation = true;
                *gui_automation_prompted = true;
                *gui_automation_last_known_available = true;
                *gui_automation_permission_context = permission_context_key();
            }
            *daemon_only_mode = true;
            *mdns_discovery = true;
            *auto_discover = true;
        } else {
            panic!("expected settings section");
        }

        let result = build_setup_result(&state).unwrap();
        let config = config_from_setup_result(&result);
        assert!(config.features.auto_approve_tools);
        #[cfg(target_os = "macos")]
        assert!(config.features.gui_automation);
        #[cfg(target_os = "macos")]
        assert!(config.features.gui_automation_prompted);
        #[cfg(target_os = "macos")]
        assert!(config.features.gui_automation_last_known_available);
        #[cfg(target_os = "macos")]
        assert_eq!(
            config.features.gui_automation_permission_context,
            permission_context_key()
        );
        assert_eq!(config.server.mode, "daemon-only");
        assert!(config.server.advertise);
        assert!(config.client.auto_discover);

        let reopened = WizardState::new(Some(&config));
        if let Some(SectionState::Features {
            auto_approve,
            #[cfg(target_os = "macos")]
            gui_automation,
            #[cfg(target_os = "macos")]
            gui_automation_prompted,
            #[cfg(target_os = "macos")]
            gui_automation_last_known_available,
            #[cfg(target_os = "macos")]
            gui_automation_permission_context,
            daemon_only_mode,
            mdns_discovery,
            auto_discover,
            ..
        }) = reopened.sections.get(&WizardSection::Features)
        {
            assert!(*auto_approve);
            #[cfg(target_os = "macos")]
            assert!(*gui_automation);
            #[cfg(target_os = "macos")]
            assert!(*gui_automation_prompted);
            #[cfg(target_os = "macos")]
            assert!(*gui_automation_last_known_available);
            #[cfg(target_os = "macos")]
            assert_eq!(gui_automation_permission_context, &permission_context_key());
            assert!(*daemon_only_mode);
            assert!(*mdns_discovery);
            assert!(*auto_discover);
        } else {
            panic!("expected settings section");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_permission_keys_separate_passive_check_from_prompt_request() {
        use std::cell::Cell;

        let passive_checks = Cell::new(0);
        let prompt_requests = Cell::new(0);
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features {
            gui_automation,
            gui_automation_prompt,
            gui_automation_prompted,
            gui_automation_settings_feedback,
            gui_automation_details_scroll,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            *gui_automation_prompt = AutomationPromptDisposition::Requested;
            *gui_automation_prompted = true;
            *gui_automation_settings_feedback = Some(GuiSettingsFeedback::OpenRequested);
            *gui_automation_details_scroll = 8;
            *selected_idx = 3;
        }

        handle_features_input_with_gui_actions(
            &mut state,
            key(KeyCode::Char('r')),
            &mut || {
                passive_checks.set(passive_checks.get() + 1);
                AutomationAvailability {
                    state: AutomationState::PermissionRequired,
                    backend: "test-native",
                    operations: vec!["click", "type"],
                }
            },
            &mut || {
                prompt_requests.set(prompt_requests.get() + 1);
                panic!("passive R must never invoke the native prompt callback")
            },
        )
        .unwrap();
        assert_eq!(passive_checks.get(), 1);
        assert_eq!(prompt_requests.get(), 0);
        if let Some(SectionState::Features {
            gui_automation_prompt,
            gui_automation_settings_feedback,
            gui_automation_details_scroll,
            ..
        }) = state.sections.get(&WizardSection::Features)
        {
            assert_eq!(
                *gui_automation_prompt,
                AutomationPromptDisposition::NotNeeded
            );
            assert!(gui_automation_settings_feedback.is_none());
            assert_eq!(*gui_automation_details_scroll, 0);
        }

        handle_features_input_with_gui_actions(
            &mut state,
            key(KeyCode::Char('p')),
            &mut || panic!("explicit P must use the prompt callback"),
            &mut || {
                prompt_requests.set(prompt_requests.get() + 1);
                AutomationPermissionResult {
                    availability: AutomationAvailability {
                        state: AutomationState::PermissionRequired,
                        backend: "test-native",
                        operations: vec!["click", "type"],
                    },
                    prompt: AutomationPromptDisposition::Requested,
                }
            },
        )
        .unwrap();
        assert_eq!(passive_checks.get(), 1);
        assert_eq!(prompt_requests.get(), 1);
        if let Some(SectionState::Features {
            gui_automation_prompt,
            gui_automation_prompted,
            gui_automation_permission_context,
            ..
        }) = state.sections.get(&WizardSection::Features)
        {
            assert_eq!(
                *gui_automation_prompt,
                AutomationPromptDisposition::Requested
            );
            assert!(*gui_automation_prompted);
            assert_eq!(gui_automation_permission_context, &permission_context_key());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gui_toggle_persists_consent_without_claiming_prompt_granted_access() {
        use crate::runtime::automation::{
            AutomationAvailability, AutomationPermissionResult, AutomationPromptDisposition,
            AutomationState,
        };

        let mut configured = false;
        let mut availability = AutomationBroker::new(false).availability();
        let mut prompt = AutomationPromptDisposition::NotNeeded;
        let mut prompted = false;
        let mut last_known_available = false;
        let mut permission_context = String::new();

        toggle_gui_automation_with(
            &mut configured,
            &mut availability,
            &mut prompt,
            &mut prompted,
            &mut last_known_available,
            &mut permission_context,
            || AutomationPermissionResult {
                availability: AutomationAvailability {
                    state: AutomationState::PermissionRequired,
                    backend: "test-native",
                    operations: vec!["click", "type"],
                },
                prompt: AutomationPromptDisposition::Requested,
            },
        );

        assert!(configured, "Finch consent should be persisted separately");
        assert_eq!(availability.state, AutomationState::PermissionRequired);
        assert_eq!(prompt, AutomationPromptDisposition::Requested);
        assert!(prompted);
        assert!(!last_known_available);
        assert_eq!(permission_context, permission_context_key());

        toggle_gui_automation_with(
            &mut configured,
            &mut availability,
            &mut prompt,
            &mut prompted,
            &mut last_known_available,
            &mut permission_context,
            || panic!("disabling must not invoke the native prompt"),
        );
        assert!(!configured);
        assert_eq!(availability.state, AutomationState::Disabled);
        assert_eq!(prompt, AutomationPromptDisposition::NotNeeded);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gui_permission_history_is_scoped_and_current_native_state_wins() {
        assert_eq!(
            scoped_permission_history(true, true, true, false),
            (true, true)
        );
        assert_eq!(
            scoped_permission_history(true, true, false, false),
            (false, false),
            "history from a different executable/launcher context must not imply denial or revocation"
        );
        assert_eq!(
            scoped_permission_history(false, false, false, true),
            (false, true),
            "a current native grant must be reported regardless of stale history"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gui_permission_required_status_includes_non_authoritative_process_diagnostics() {
        let availability = AutomationAvailability {
            state: AutomationState::PermissionRequired,
            backend: "test-native",
            operations: vec!["click", "type"],
        };
        let target = "executable: /tmp/target/debug/finch; launcher hint: Apple_Terminal (diagnostic only, not the macOS TCC identity)";
        let lines = gui_automation_status_lines(
            true,
            &availability,
            AutomationPromptDisposition::SuppressedNonInteractive,
            false,
            false,
            target,
            None,
        );
        let output = lines.join("\n");

        assert!(output.contains("current Finch process is not Accessibility-trusted"));
        assert!(output.contains("headless prompt suppressed"));
        assert!(output.contains("Diagnostic only — executable: /tmp/target/debug/finch"));
        assert!(output.contains("launcher hint: Apple_Terminal"));
        assert!(!output.contains("Accessibility is not granted to"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_o_outcomes_visible_at_80x24() {
        use ratatui::backend::TestBackend;

        for (feedback, needle) in [
            (GuiSettingsFeedback::OpenRequested, "Open requested"),
            (GuiSettingsFeedback::Suppressed, "Not opened (SSH/headless)"),
            (
                GuiSettingsFeedback::Failed("test opener failure".to_string()),
                "Open failed",
            ),
        ] {
            let mut state = WizardState::new(None);
            state.current_section = WizardSection::Features;
            if let Some(SectionState::Features {
                gui_automation,
                gui_automation_availability,
                gui_automation_prompt,
                gui_automation_settings_feedback,
                selected_idx,
                ..
            }) = state.sections.get_mut(&WizardSection::Features)
            {
                *gui_automation = true;
                gui_automation_availability.state = AutomationState::PermissionRequired;
                *gui_automation_prompt = AutomationPromptDisposition::SuppressedNonInteractive;
                *gui_automation_settings_feedback = Some(feedback);
                *selected_idx = 3;
            }

            for (width, height) in [(80, 24), (40, 18)] {
                let backend = TestBackend::new(width, height);
                let mut terminal = ratatui::Terminal::new(backend).unwrap();
                terminal
                    .draw(|frame| {
                        render_tabbed_wizard_with_permission_target(
                            frame,
                            &state,
                            "current Finch process: PID 42\nexecutable: /tmp/finch\nlauncher hint: Terminal",
                        );
                    })
                    .unwrap();
                let rendered = test_buffer_text(terminal.backend().buffer());
                assert!(rendered.contains("GUI automation"));
                assert!(rendered.contains(needle), "missing {needle}: {rendered}");
                assert!(rendered.contains("D: Full"));
                if !matches!(
                    state.sections.get(&WizardSection::Features),
                    Some(SectionState::Features {
                        gui_automation_settings_feedback: Some(GuiSettingsFeedback::OpenRequested),
                        ..
                    })
                ) {
                    assert!(rendered.contains("System Settings"));
                    assert!(rendered.contains("Privacy"));
                    assert!(rendered.contains("Security"));
                    assert!(rendered.contains("Accessibility"));
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_fresh_40x18_shows_exact_recovery_keys_and_path() {
        use ratatui::backend::TestBackend;

        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features {
            gui_automation,
            gui_automation_availability,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            gui_automation_availability.state = AutomationState::PermissionRequired;
            *selected_idx = 3;
        }
        let backend = TestBackend::new(40, 18);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_tabbed_wizard_with_permission_target(
                    frame,
                    &state,
                    "current Finch process: PID 42\nexecutable: /tmp/finch\nlauncher hint: Terminal",
                );
            })
            .unwrap();
        let rendered = test_buffer_text(terminal.backend().buffer());

        assert!(rendered.contains("Current Finch process"));
        assert!(rendered.contains("R: Passive check"));
        assert!(rendered.contains("P: Request prompt"));
        assert!(rendered.contains("O: System Settings"));
        assert!(rendered.contains("Privacy"));
        assert!(rendered.contains("Security"));
        assert!(rendered.contains("Accessibility"));
        assert!(rendered.contains("D: Full"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_expanded_details_preserve_long_identity_hints() {
        use ratatui::backend::TestBackend;

        let long_path = format!(
            "/private/tmp/{}/finch-ad-hoc-build",
            "long-development-directory/".repeat(4)
        );
        let target = format!(
            "executable: {long_path}\nlauncher hint: VeryLongLauncherNameForAccessibilityDiagnostics\nuse the app name macOS shows"
        );
        let feedback = GuiSettingsFeedback::Failed("test opener failure".to_string());
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features {
            gui_automation,
            gui_automation_availability,
            gui_automation_prompt,
            gui_automation_prompted,
            gui_automation_settings_feedback,
            gui_automation_details_expanded,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            gui_automation_availability.state = AutomationState::PermissionRequired;
            *gui_automation_prompt = AutomationPromptDisposition::Requested;
            *gui_automation_prompted = true;
            *gui_automation_settings_feedback = Some(feedback);
            *gui_automation_details_expanded = true;
            *selected_idx = 3;
        }

        let mut render = |width, height, scroll| {
            if let Some(SectionState::Features {
                gui_automation_details_scroll,
                ..
            }) = state.sections.get_mut(&WizardSection::Features)
            {
                *gui_automation_details_scroll = scroll;
            }
            let backend = TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_tabbed_wizard_with_permission_target(frame, &state, &target);
                })
                .unwrap();
            test_buffer_text(terminal.backend().buffer())
        };

        let full_status = gui_automation_status_lines(
            true,
            &AutomationAvailability {
                state: AutomationState::PermissionRequired,
                backend: "test-native",
                operations: vec!["click", "type"],
            },
            AutomationPromptDisposition::Requested,
            true,
            false,
            &target,
            Some(&GuiSettingsFeedback::Failed(
                "test opener failure".to_string(),
            )),
        )
        .join("\n");
        assert!(full_status.contains(&long_path));

        let full_size_pages = (0..16)
            .map(|scroll| render(80, 24, scroll))
            .collect::<Vec<_>>();
        assert!(full_size_pages
            .iter()
            .any(|page| page.contains("/private/tmp/long-development-directory")));
        assert!(full_size_pages
            .iter()
            .any(|page| page.contains("finch-ad-hoc-build")));
        assert!(full_size_pages
            .iter()
            .any(|page| page.contains("test opener failure")));
        assert!(full_size_pages
            .iter()
            .any(|page| page.contains("VeryLongLauncherNameForAccessibilityDiagnostics")));
        assert!(full_size_pages
            .iter()
            .any(|page| page.contains("clipboard copying is unavailable")));

        let narrow_pages = (0..24)
            .map(|scroll| render(40, 18, scroll))
            .collect::<Vec<_>>();
        let narrow_page_text = |page: &str| {
            page.chars()
                .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
                .collect::<String>()
        };
        assert!(narrow_pages
            .iter()
            .any(|page| narrow_page_text(page).contains("finch-ad-hoc-build")));
        assert!(narrow_pages.iter().any(|page| narrow_page_text(page)
            .contains("VeryLongLauncherNameForAccessibilityDiagnostics")));
        assert!(narrow_pages.iter().any(|page| page.contains("PgUp/PgDn")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_navigation_and_resize_keep_selected_row_visible() {
        use ratatui::backend::TestBackend;

        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        for _ in 0..SETTINGS_CONTEXT_IDX {
            handle_features_input(&mut state, key(KeyCode::Down)).unwrap();
        }
        for (width, height) in [(80, 24), (40, 18)] {
            let backend = TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| render_tabbed_wizard(frame, &state))
                .unwrap();
            let rendered = test_buffer_text(terminal.backend().buffer());
            assert!(
                rendered.contains("Context lines: 4"),
                "selected row clipped after resize to {width}x{height}: {rendered}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_full_status_scrolls_and_closes_without_native_actions() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features {
            gui_automation,
            selected_idx,
            ..
        }) = state.sections.get_mut(&WizardSection::Features)
        {
            *gui_automation = true;
            *selected_idx = 3;
        }

        handle_features_input(&mut state, key(KeyCode::Char('d'))).unwrap();
        handle_wizard_key(&mut state, key(KeyCode::Right)).unwrap();
        assert_eq!(state.current_section, WizardSection::Features);
        handle_features_input(&mut state, key(KeyCode::Down)).unwrap();
        handle_features_input(&mut state, key(KeyCode::PageDown)).unwrap();
        if let Some(SectionState::Features {
            gui_automation_details_expanded,
            gui_automation_details_scroll,
            selected_idx,
            ..
        }) = state.sections.get(&WizardSection::Features)
        {
            assert!(*gui_automation_details_expanded);
            assert_eq!(*gui_automation_details_scroll, 6);
            assert_eq!(
                *selected_idx, 3,
                "detail scrolling must not move the feature row"
            );
        }

        handle_features_input(&mut state, key(KeyCode::Home)).unwrap();
        if let Some(SectionState::Features {
            gui_automation_details_scroll,
            ..
        }) = state.sections.get(&WizardSection::Features)
        {
            assert_eq!(*gui_automation_details_scroll, 0);
        }

        handle_features_input(&mut state, key(KeyCode::Esc)).unwrap();
        if let Some(SectionState::Features {
            gui_automation_details_expanded,
            gui_automation_details_scroll,
            ..
        }) = state.sections.get(&WizardSection::Features)
        {
            assert!(!*gui_automation_details_expanded);
            assert_eq!(*gui_automation_details_scroll, 0);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_gui_accessibility_settings_open_outcomes_are_visible_and_actionable() {
        let mut feedback = None;
        open_gui_settings_with(&mut feedback, || Ok(false));
        let suppressed = feedback.as_ref().unwrap().full_message();
        assert!(suppressed.contains("was not opened"));
        assert!(suppressed.contains("local interactive session"));
        assert!(suppressed.contains("open System Settings"));

        open_gui_settings_with(&mut feedback, || {
            Err(anyhow::anyhow!("test opener failure"))
        });
        let failed = feedback.as_ref().unwrap().full_message();
        assert!(failed.contains("Could not open System Settings: test opener failure"));
        assert!(failed.contains("Privacy & Security → Accessibility"));

        open_gui_settings_with(&mut feedback, || Ok(true));
        let opened = feedback.as_ref().unwrap().full_message();
        assert!(opened.contains("open requested"));
        assert!(opened.contains("press R"));
    }

    #[test]
    fn finch_client_key_can_be_entered_and_applied() {
        let mut state = WizardState::new(None);
        state.current_section = WizardSection::Features;
        if let Some(SectionState::Features { selected_idx, .. }) =
            state.sections.get_mut(&WizardSection::Features)
        {
            *selected_idx = SETTINGS_FINCH_API_KEY_IDX;
        }

        handle_features_input(&mut state, key(KeyCode::Char('e'))).unwrap();
        for c in "custom-secret".chars() {
            handle_features_input(&mut state, key(KeyCode::Char(c))).unwrap();
        }
        handle_features_input(&mut state, key(KeyCode::Enter)).unwrap();

        let result = build_setup_result(&state).unwrap();
        assert_eq!(result.finch_api_key, "custom-secret");

        let mut config = crate::config::Config::with_providers(result.providers);
        apply_daemon_api_key(&mut config, &result.finch_api_key);
        assert!(config.server.auth_enabled);
        assert_eq!(config.server.api_keys, vec!["custom-secret"]);
    }

    // ── helpers ──────────────────────────────────────────────────────────────

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn modified_key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn test_buffer_text(buffer: &ratatui::buffer::Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .fold(String::new(), |mut line, symbol| {
                        line.push_str(symbol);
                        line
                    })
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn state_with_step(step: AddProviderStep) -> WizardState {
        let hermetic_config = crate::config::Config::with_providers_and_paths(
            vec![ProviderEntry::Claude {
                api_key: String::new(),
                model: None,
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("claude".to_string()),
            }],
            std::path::PathBuf::from("unused-test-metrics"),
            None,
        );
        let mut state = WizardState::new_with_catalog_cache_dir(Some(&hermetic_config), None);
        if let Some(SectionState::Models {
            adding_provider,
            catalog_models,
            catalog_model_provenance,
            ..
        }) = state.sections.get_mut(&WizardSection::Models)
        {
            if let AddProviderStep::ConfigureRemote {
                provider_idx,
                model,
                ..
            } = &step
            {
                *catalog_models = known_models_for(CLOUD_PROVIDERS[*provider_idx].0);
                *catalog_model_provenance = if model.is_empty() {
                    ModelSelectionProvenance::Blank
                } else {
                    ModelSelectionProvenance::Manual
                };
            }
            *adding_provider = Some(step);
        }
        state
    }

    fn set_catalog_model_provenance(state: &mut WizardState, provenance: ModelSelectionProvenance) {
        if let Some(SectionState::Models {
            catalog_model_provenance,
            ..
        }) = state.sections.get_mut(&WizardSection::Models)
        {
            *catalog_model_provenance = provenance;
        }
    }

    fn get_step(state: &WizardState) -> Option<&AddProviderStep> {
        if let Some(SectionState::Models {
            adding_provider, ..
        }) = state.sections.get(&WizardSection::Models)
        {
            adding_provider.as_ref()
        } else {
            None
        }
    }

    fn install_completed_catalog_refresh(
        state: &mut WizardState,
        profile: &ModelCatalogProfile,
        catalog: ModelCatalog,
    ) {
        install_completed_catalog_refresh_result(state, profile, catalog, None);
    }

    fn install_completed_catalog_refresh_result(
        state: &mut WizardState,
        profile: &ModelCatalogProfile,
        catalog: ModelCatalog,
        error: Option<String>,
    ) {
        if let Some(SectionState::Models {
            catalog_refresh,
            catalog_generation,
            ..
        }) = state.sections.get_mut(&WizardSection::Models)
        {
            *catalog_generation = catalog_generation.wrapping_add(1);
            *catalog_refresh = Some(CatalogRefresh {
                generation: *catalog_generation,
                selection_identity: model_catalog::profile_cache_identity(profile),
                result: Arc::new(Mutex::new(Some((catalog, error)))),
            });
        }
    }

    fn render_wizard_text(state: &WizardState) -> String {
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(180, 50);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render_tabbed_wizard(frame, state))
            .unwrap();
        test_buffer_text(terminal.backend().buffer())
    }

    fn discovered_catalog(profile: &ModelCatalogProfile, models: &[&str]) -> ModelCatalog {
        ModelCatalog {
            provider: profile.provider.clone(),
            profile_id: profile.profile_id.clone(),
            models_url: profile.endpoints.models_url.clone(),
            models: models.iter().map(|model| (*model).to_string()).collect(),
            source: CatalogSource::Discovered,
            refreshed_at: Utc::now(),
        }
    }

    fn catalog_model_provenance(state: &WizardState) -> ModelSelectionProvenance {
        match state.sections.get(&WizardSection::Models) {
            Some(SectionState::Models {
                catalog_model_provenance,
                ..
            }) => *catalog_model_provenance,
            _ => panic!("expected models section"),
        }
    }

    fn edit_primary_remote(state: &mut WizardState, name: &str, model: &str, api_key: &str) {
        handle_models_input(state, key(KeyCode::Enter)).unwrap();
        let Some(SectionState::Models {
            adding_provider:
                Some(AddProviderStep::ConfigureRemote {
                    name: editing_name,
                    model: editing_model,
                    api_key: editing_key,
                    ..
                }),
            ..
        }) = state.sections.get_mut(&WizardSection::Models)
        else {
            panic!("expected remote editor");
        };
        *editing_name = name.to_string();
        *editing_model = model.to_string();
        *editing_key = Some(api_key.to_string());
        handle_models_input(state, key(KeyCode::Enter)).unwrap();
    }

    fn get_primary(state: &WizardState) -> Option<&ModelConfig> {
        if let Some(SectionState::Models { primary_model, .. }) =
            state.sections.get(&WizardSection::Models)
        {
            Some(primary_model)
        } else {
            None
        }
    }

    fn get_tool_models(state: &WizardState) -> Vec<ModelConfig> {
        if let Some(SectionState::Models { tool_models, .. }) =
            state.sections.get(&WizardSection::Models)
        {
            tool_models.clone()
        } else {
            vec![]
        }
    }

    fn default_configure_local(focused_field: usize) -> AddProviderStep {
        AddProviderStep::ConfigureLocal {
            inference_provider: InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Medium,
            execution: ExecutionTarget::Auto,
            focused_field,
        }
    }

    fn default_configure_remote(focused_field: usize) -> AddProviderStep {
        let provider_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "grok")
            .unwrap();
        AddProviderStep::ConfigureRemote {
            provider_idx,
            name: "grok".to_string(),
            model: CLOUD_PROVIDERS[provider_idx].2.to_string(),
            api_key: Some(String::new()),
            focused_field,
            editing_idx: None,
        }
    }

    // ── known_models_for ──────────────────────────────────────────────────────

    #[test]
    fn test_known_models_for_returns_list_for_all_providers() {
        for (id, _, default_model, _) in CLOUD_PROVIDERS {
            let models = known_models_for(id);
            assert!(!models.is_empty(), "provider '{}' has no known models", id);
            // Discovery-capable providers intentionally start blank so the
            // wizard cannot silently select a stale compile-time identifier.
            assert!(
                default_model.is_empty() || models.iter().any(|model| model == default_model),
                "provider '{}': default model '{}' not in known_models_for list {:?}",
                id,
                default_model,
                models
            );
        }
    }

    #[test]
    fn test_known_models_for_unknown_provider_returns_empty() {
        assert!(known_models_for("nonexistent").is_empty());
    }

    #[test]
    fn discovery_capable_providers_do_not_start_with_compile_time_model_ids() {
        for provider in ["claude", "openai", "grok", "mistral"] {
            let (_, _, default_model, _) = CLOUD_PROVIDERS
                .iter()
                .find(|(id, ..)| *id == provider)
                .unwrap();
            assert!(
                default_model.is_empty(),
                "{provider} should start editable and blank"
            );
        }
    }

    #[test]
    fn catalog_profile_preserves_configured_full_paths() {
        let persisted = ProviderEntry::Openai {
            api_key: "old-key".to_string(),
            model: Some("manual-model".to_string()),
            base_url: Some("https://compatible.example/v1".to_string()),
            chat_path: Some("https://chat.example/exact/completions?preview=1".to_string()),
            models_path: Some("https://models.example/exact/catalog?account=work".to_string()),
            name: Some("compatible".to_string()),
            reasoning_effort: None,
        };
        let profile =
            model_catalog_profile("openai", "compatible", "new-key", Some(&persisted)).unwrap();
        assert_eq!(profile.api_key, "new-key");
        assert_eq!(
            profile.endpoints.chat_url,
            "https://chat.example/exact/completions?preview=1"
        );
        assert_eq!(
            profile.endpoints.models_url,
            "https://models.example/exact/catalog?account=work"
        );
    }

    #[test]
    fn named_catalog_profile_preserves_bound_custom_endpoints_without_inline_secret() {
        let persisted = ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::OpenaiPlatform,
            credential: crate::config::CredentialBinding {
                credential_ref: "work".into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: std::collections::BTreeSet::new(),
            },
            model: Some("manual-model".into()),
            base_url: Some("https://compatible.example/v1".into()),
            chat_path: Some("/v1/chat/completions".into()),
            models_path: Some("/v1/models".into()),
            name: Some("work-profile".into()),
            reasoning_effort: None,
        };
        let profile =
            model_catalog_profile("openai", "work-profile", "", Some(&persisted)).unwrap();
        assert!(profile.api_key.is_empty());
        assert_eq!(
            profile.endpoints.models_url,
            "https://compatible.example/v1/models"
        );
    }

    #[test]
    fn named_catalog_refresh_config_uses_edited_name_and_validates_siblings() {
        use crate::config::{
            AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
            CredentialProvider, EndpointFamily, ProviderCredential,
        };
        let credential = ProviderCredential {
            name: "work".into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiPlatform,
            issuer: "openai-platform".into(),
            audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
            tenant: None,
            project: None,
            account: None,
            scopes: std::collections::BTreeSet::new(),
            secret_ref: "env:OPENAI_WORK_API_KEY".into(),
            lifecycle: CredentialLifecycle::default(),
            revocation: Default::default(),
        };
        let named = |name: &str, credential_ref: &str| ProviderEntry::Credentialed {
            provider: CredentialProvider::OpenaiPlatform,
            credential: CredentialBinding {
                credential_ref: credential_ref.into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: std::collections::BTreeSet::new(),
            },
            model: Some("gpt-4o".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(name.into()),
            reasoning_effort: None,
        };
        let primary = model_config_from_provider(&named("old-name", "work"), &[]).unwrap();
        let sibling = model_config_from_provider(&named("broken", "missing"), &[]).unwrap();
        let selected = named("renamed", "work");
        let config = named_catalog_refresh_config(
            &primary,
            &[sibling],
            0,
            &selected,
            vec![credential.clone()],
        );
        assert!(config
            .validate()
            .unwrap_err()
            .to_string()
            .contains("missing"));

        let valid = named_catalog_refresh_config(&primary, &[], 0, &selected, vec![credential]);
        valid.validate().unwrap();
        assert_eq!(valid.providers[0].profile_name(), "renamed");
    }

    #[test]
    fn builtin_discovery_profiles_use_provider_specific_urls_and_auth() {
        let claude = model_catalog_profile("claude", "claude-work", "key", None).unwrap();
        assert_eq!(claude.auth, CatalogAuth::AnthropicApiKey);
        assert_eq!(
            claude.endpoints.models_url,
            "https://api.anthropic.com/v1/models"
        );

        let openai = model_catalog_profile("openai", "openai-work", "key", None).unwrap();
        assert_eq!(openai.auth, CatalogAuth::Bearer);
        assert_eq!(
            openai.endpoints.models_url,
            "https://api.openai.com/v1/models"
        );

        let xai = model_catalog_profile("grok", "xai-work", "key", None).unwrap();
        assert_eq!(xai.auth, CatalogAuth::Bearer);
        assert_eq!(xai.endpoints.models_url, "https://api.x.ai/v1/models");

        let mistral = model_catalog_profile("mistral", "mistral-work", "key", None).unwrap();
        assert_eq!(mistral.auth, CatalogAuth::Bearer);
        assert_eq!(
            mistral.endpoints.models_url,
            "https://api.mistral.ai/v1/models"
        );
    }

    #[test]
    fn stale_cross_provider_refresh_is_discarded() {
        let claude_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "claude")
            .unwrap();
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: claude_idx,
            name: "claude-work".to_string(),
            model: String::new(),
            api_key: Some("claude-key".to_string()),
            focused_field: 0,
            editing_idx: None,
        });
        set_catalog_model_provenance(&mut state, ModelSelectionProvenance::DefaultGenerated);
        let claude = model_catalog_profile("claude", "claude-work", "claude-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &claude,
            ModelCatalog {
                provider: "claude".to_string(),
                profile_id: "claude-work".to_string(),
                models_url: claude.endpoints.models_url.clone(),
                models: vec!["claude-account-model".to_string()],
                source: CatalogSource::Discovered,
                refreshed_at: Utc::now(),
            },
        );
        if let Some(SectionState::Models {
            adding_provider,
            catalog_generation,
            ..
        }) = state.sections.get_mut(&WizardSection::Models)
        {
            *catalog_generation = catalog_generation.wrapping_add(1);
            *adding_provider = Some(AddProviderStep::ConfigureRemote {
                provider_idx: openai_idx,
                name: "openai-work".to_string(),
                model: String::new(),
                api_key: Some("openai-key".to_string()),
                focused_field: 0,
                editing_idx: None,
            });
        }

        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { provider_idx, model, .. })
                if *provider_idx == openai_idx && model.is_empty()
        ));
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::DefaultGenerated
        );
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::StaticFallback,
                ..
            })
        ));
    }

    #[test]
    fn only_latest_same_profile_refresh_is_applied() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            ModelCatalog {
                provider: "openai".to_string(),
                profile_id: "openai-work".to_string(),
                models_url: profile.endpoints.models_url.clone(),
                models: vec!["old-result".to_string()],
                source: CatalogSource::Discovered,
                refreshed_at: Utc::now(),
            },
        );
        if let Some(SectionState::Models {
            catalog_generation, ..
        }) = state.sections.get_mut(&WizardSection::Models)
        {
            *catalog_generation = catalog_generation.wrapping_add(1);
        }
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model.is_empty()
        ));

        let refreshed_at = Utc::now() - chrono::Duration::minutes(7);
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            ModelCatalog {
                provider: "openai".to_string(),
                profile_id: "openai-work".to_string(),
                models_url: profile.endpoints.models_url.clone(),
                models: vec!["latest-result".to_string()],
                source: CatalogSource::Discovered,
                refreshed_at,
            },
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "latest-result"
        ));
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::Discovered,
                catalog_refreshed_at: Some(actual),
                ..
            }) if *actual == refreshed_at
        ));

        let cached_at = refreshed_at - chrono::Duration::hours(2);
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            ModelCatalog {
                provider: "openai".to_string(),
                profile_id: "openai-work".to_string(),
                models_url: profile.endpoints.models_url.clone(),
                models: vec!["cached-result".to_string()],
                source: CatalogSource::Cache,
                refreshed_at: cached_at,
            },
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::Cache,
                catalog_refreshed_at: Some(actual),
                ..
            }) if *actual == cached_at
        ));
    }

    #[test]
    fn catalog_refresh_time_displays_timestamp_and_age() {
        let refreshed_at = DateTime::parse_from_rfc3339("2026-08-25T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2026-08-25T10:07:01Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            format_catalog_refresh_time(&refreshed_at, now),
            "2026-08-25 10:00 UTC (7m ago)"
        );
    }

    #[test]
    fn chooser_keeps_chatgpt_subscription_distinct_from_openai_platform() {
        use ratatui::backend::TestBackend;

        let openai = CLOUD_PROVIDERS
            .iter()
            .find(|(id, ..)| *id == "openai")
            .unwrap();
        assert_eq!(openai.1, "OpenAI API");
        let chatgpt = CLOUD_PROVIDERS
            .iter()
            .find(|(id, ..)| *id == "chatgpt")
            .unwrap();
        assert_eq!(chatgpt.1, "ChatGPT subscription");
        assert_eq!(chatgpt.2, "gpt-5.6-sol");
        assert!(CLOUD_PROVIDERS
            .iter()
            .all(|(id, ..)| *id != "chatgpt_subscription"));

        let step = AddProviderStep::SelectAddType { selected: 0 };
        let backend = TestBackend::new(160, 50);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_add_provider_overlay(
                    frame,
                    area,
                    CoreMlConfig::default(),
                    &step,
                    &CatalogSource::StaticFallback,
                    false,
                    None,
                    None,
                );
            })
            .unwrap();
        let rendered = test_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("OpenAI API"), "{rendered}");
        assert!(rendered.contains("ChatGPT subscription"), "{rendered}");
        assert!(
            rendered.contains("Finch-native device sign-in"),
            "{rendered}"
        );
        assert!(!rendered.contains("Codex"), "{rendered}");
        assert!(rendered.contains("platform.openai.com"), "{rendered}");
        assert!(!rendered.contains("GPT-4 (OpenAI)"), "{rendered}");
    }

    #[test]
    fn chatgpt_configuration_has_no_api_key_input_buffer_or_render_path() {
        use ratatui::backend::TestBackend;

        let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                provider_idx: 0,
                api_key: None,
                focused_field: 1,
                ..
            })
        ));

        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                api_key: None,
                focused_field: 2,
                ..
            })
        ));

        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        for character in "sk-platform-must-not-cross".chars() {
            handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
            handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
            handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
            handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
            handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
            handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
            handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        }
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                api_key: Some(key),
                ..
            }) if key == "sk-platform-must-not-cross"
        ));
        handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
        let step = get_step(&state).unwrap();
        assert!(matches!(
            step,
            AddProviderStep::ConfigureRemote {
                provider_idx: 0,
                api_key: None,
                ..
            }
        ));

        let backend = TestBackend::new(180, 50);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_add_provider_overlay(
                    frame,
                    frame.area(),
                    CoreMlConfig::default(),
                    step,
                    &CatalogSource::StaticFallback,
                    false,
                    None,
                    None,
                );
            })
            .unwrap();
        let rendered = test_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("Finch-native device sign-in after save"));
        assert!(!rendered.contains("API Key"), "{rendered}");
        assert!(
            !rendered.contains("sk-platform-must-not-cross"),
            "{rendered}"
        );

        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            get_primary(&state),
            Some(ModelConfig::Remote {
                provider,
                api_key,
                ..
            }) if provider == "chatgpt" && api_key.is_empty()
        ));
        state.current_section = WizardSection::Models;
        let rendered = render_wizard_text(&state);
        assert!(rendered.contains("Named device credential"), "{rendered}");
        assert!(!rendered.contains("Paste your API key"), "{rendered}");
        assert!(
            !rendered.contains("sk-platform-must-not-cross"),
            "{rendered}"
        );

        if let Some(SectionState::Models { editing_mode, .. }) =
            state.sections.get_mut(&WizardSection::Models)
        {
            *editing_mode = true;
        }
        handle_models_input(&mut state, key(KeyCode::Char('x'))).unwrap();
        assert!(matches!(
            get_primary(&state),
            Some(ModelConfig::Remote {
                provider,
                api_key,
                ..
            }) if provider == "chatgpt" && api_key.is_empty()
        ));
        let rendered = render_wizard_text(&state);
        assert!(rendered.contains("not an API key"), "{rendered}");
        assert!(!rendered.contains("Edit API Key"), "{rendered}");
    }

    #[test]
    fn static_fallback_ui_is_dated_incomplete_and_never_presented_as_fresh() {
        use ratatui::backend::TestBackend;

        let misleading_runtime_time = Utc::now();
        let label = format_catalog_label(
            &CatalogSource::StaticFallback,
            false,
            Some(&misleading_runtime_time),
            misleading_runtime_time,
        );

        assert!(label.contains("bundled fallback snapshot"), "{label}");
        assert!(
            label.contains(model_catalog::STATIC_FALLBACK_AS_OF),
            "{label}"
        );
        assert!(label.contains("incomplete"), "{label}");
        assert!(label.contains("model ID remains editable"), "{label}");
        assert!(!label.contains("provider discovery"), "{label}");
        assert!(!label.contains("local cache"), "{label}");
        assert!(!label.contains("UTC"), "{label}");
        assert!(!label.contains("ago"), "{label}");
        assert!(!label.contains("current"), "{label}");
        assert!(!label.contains("live"), "{label}");

        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let step = AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: "gateway-preview-model".to_string(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        };
        let backend = TestBackend::new(180, 50);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_add_provider_overlay(
                    frame,
                    area,
                    CoreMlConfig::default(),
                    &step,
                    &CatalogSource::StaticFallback,
                    false,
                    Some(&misleading_runtime_time),
                    None,
                );
            })
            .unwrap();
        let rendered = test_buffer_text(terminal.backend().buffer());
        assert!(rendered.contains("bundled fallback snapshot"), "{rendered}");
        assert!(
            rendered.contains(model_catalog::STATIC_FALLBACK_AS_OF),
            "{rendered}"
        );
        assert!(rendered.contains("incomplete"), "{rendered}");
        assert!(!rendered.contains("provider discovery"), "{rendered}");
        assert!(!rendered.contains("local cache"), "{rendered}");
        assert!(!rendered.contains("UTC"), "{rendered}");
    }

    #[test]
    fn manual_openai_id_survives_save_reopen_and_fallback_installation() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let cache_dir = directory.path().join("model-catalog-cache");
        let metrics_dir = directory.path().join("metrics");
        let constitution_path = directory.path().join("constitution.md");
        std::fs::write(&constitution_path, "test-only constitution").unwrap();
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let manual_model = "gateway-preview-model";
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("sk-openai-test-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        state.catalog_cache_dir = Some(cache_dir.clone());
        for character in manual_model.chars() {
            handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
        }
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();

        let first_save = build_setup_result(&state).unwrap();
        let config = config_from_setup_result_with_paths(
            &first_save,
            metrics_dir.clone(),
            Some(constitution_path.clone()),
        );
        config.save_to(&config_path).unwrap();
        let loaded = crate::config::load_config_from_path_with_paths(
            &config_path,
            metrics_dir.clone(),
            Some(constitution_path.clone()),
        )
        .unwrap();
        assert_eq!(loaded.metrics_dir, metrics_dir);
        assert_eq!(loaded.constitution_path, Some(constitution_path.clone()));
        assert!(matches!(
            loaded.providers.first(),
            Some(ProviderEntry::Openai { model: Some(model), .. }) if model == manual_model
        ));

        let mut reopened =
            WizardState::new_with_catalog_cache_dir(Some(&loaded), Some(cache_dir.clone()));
        handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            get_step(&reopened),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
        ));
        assert_eq!(
            catalog_model_provenance(&reopened),
            ModelSelectionProvenance::Persisted
        );

        let persisted = loaded.providers.first().unwrap();
        let profile = model_catalog_profile(
            "openai",
            "openai-work",
            "sk-openai-test-key",
            Some(persisted),
        )
        .unwrap();
        let mut fallback =
            model_catalog::fallback_catalog(&profile.provider, &profile.endpoints.models_url);
        fallback.profile_id = profile.profile_id.clone();
        install_completed_catalog_refresh_result(
            &mut reopened,
            &profile,
            fallback,
            Some("fake authenticated catalogue failure".to_string()),
        );
        advance_catalog_refresh_if_done(&mut reopened);
        assert!(matches!(
            get_step(&reopened),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
        ));
        assert!(matches!(
            reopened.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::StaticFallback,
                catalog_error: Some(error),
                ..
            }) if error == "fake authenticated catalogue failure"
        ));

        handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
        let second_save = build_setup_result(&reopened).unwrap();
        config_from_setup_result_with_paths(
            &second_save,
            metrics_dir.clone(),
            Some(constitution_path.clone()),
        )
        .save_to(&config_path)
        .unwrap();
        let reloaded = crate::config::load_config_from_path_with_paths(
            &config_path,
            metrics_dir.clone(),
            Some(constitution_path.clone()),
        )
        .unwrap();
        assert_eq!(reloaded.metrics_dir, metrics_dir);
        assert_eq!(reloaded.constitution_path, Some(constitution_path));
        assert!(matches!(
            reloaded.providers.first(),
            Some(ProviderEntry::Openai { model: Some(model), .. }) if model == manual_model
        ));
        assert!(
            !cache_dir.exists(),
            "the hermetic cache should remain empty unless the test writes it"
        );
    }

    #[test]
    fn failed_refresh_with_stale_cache_renders_age_warning_and_preserves_manual_model() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let manual_model = "gateway-preview-model";
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: manual_model.to_string(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        state.current_section = WizardSection::Models;
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        let refreshed_at = Utc::now() - chrono::Duration::hours(2);
        install_completed_catalog_refresh_result(
            &mut state,
            &profile,
            ModelCatalog {
                provider: profile.provider.clone(),
                profile_id: profile.profile_id.clone(),
                models_url: profile.endpoints.models_url.clone(),
                models: vec!["cached-account-model".to_string()],
                source: CatalogSource::Cache,
                refreshed_at,
            },
            Some("fake provider unavailable".to_string()),
        );
        advance_catalog_refresh_if_done(&mut state);

        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
        ));
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::Cache,
                catalog_error: Some(error),
                ..
            }) if error == "fake provider unavailable"
        ));
        let rendered = render_wizard_text(&state);
        assert!(rendered.contains("local cache"), "{rendered}");
        assert!(
            rendered.contains(&refreshed_at.format("%Y-%m-%d %H:%M UTC").to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("2h ago"), "{rendered}");
        assert!(
            rendered.contains("Refresh warning: fake provider unavailable"),
            "{rendered}"
        );
        assert!(rendered.contains(manual_model), "{rendered}");
    }

    #[test]
    fn failed_refresh_with_static_fallback_renders_snapshot_warning_and_preserves_manual_model() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let manual_model = "restricted-account-model";
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: manual_model.to_string(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        state.current_section = WizardSection::Models;
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        let mut fallback =
            model_catalog::fallback_catalog(&profile.provider, &profile.endpoints.models_url);
        fallback.profile_id = profile.profile_id.clone();
        install_completed_catalog_refresh_result(
            &mut state,
            &profile,
            fallback,
            Some("fake provider unavailable".to_string()),
        );
        advance_catalog_refresh_if_done(&mut state);

        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == manual_model
        ));
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_source: CatalogSource::StaticFallback,
                catalog_error: Some(error),
                ..
            }) if error == "fake provider unavailable"
        ));
        let rendered = render_wizard_text(&state);
        assert!(rendered.contains("bundled fallback snapshot"), "{rendered}");
        assert!(
            rendered.contains(model_catalog::STATIC_FALLBACK_AS_OF),
            "{rendered}"
        );
        assert!(rendered.contains("incomplete"), "{rendered}");
        assert!(
            rendered.contains("Refresh warning: fake provider unavailable"),
            "{rendered}"
        );
        assert!(rendered.contains(manual_model), "{rendered}");
        assert!(!rendered.contains("provider discovery"), "{rendered}");
        assert!(!rendered.contains("local cache"), "{rendered}");
    }

    #[test]
    fn persisted_fallback_id_is_not_replaced_by_refresh() {
        let persisted = ProviderEntry::Openai {
            api_key: "openai-key".to_string(),
            model: Some("gpt-4o".to_string()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("openai-work".to_string()),
            reasoning_effort: None,
        };
        let config = crate::config::Config::with_providers_and_paths(
            vec![persisted.clone()],
            std::path::PathBuf::from("unused-test-metrics"),
            None,
        );
        let mut state = WizardState::new_with_catalog_cache_dir(Some(&config), None);
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::Persisted
        );
        let profile =
            model_catalog_profile("openai", "openai-work", "openai-key", Some(&persisted)).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
        ));
    }

    #[test]
    fn manually_typed_fallback_id_is_not_replaced_by_refresh() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        for character in "gpt-4o".chars() {
            handle_models_input(&mut state, key(KeyCode::Char(character))).unwrap();
        }
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::Manual
        );
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
        ));
    }

    #[test]
    fn discovered_default_may_update_on_later_refresh() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            discovered_catalog(&profile, &["gpt-4o"]),
        );
        advance_catalog_refresh_if_done(&mut state);
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::DefaultGenerated
        );

        install_completed_catalog_refresh(
            &mut state,
            &profile,
            discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "aaa-new-default"
        ));
    }

    #[test]
    fn cycled_selection_is_not_replaced_by_refresh() {
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "openai")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai-work".to_string(),
            model: String::new(),
            api_key: Some("openai-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        if let Some(SectionState::Models { catalog_models, .. }) =
            state.sections.get_mut(&WizardSection::Models)
        {
            *catalog_models = vec!["other-model".to_string(), "gpt-4o".to_string()];
        }
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        assert_eq!(
            catalog_model_provenance(&state),
            ModelSelectionProvenance::Cycled
        );
        let profile = model_catalog_profile("openai", "openai-work", "openai-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            discovered_catalog(&profile, &["aaa-new-default", "gpt-4o"]),
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. }) if model == "gpt-4o"
        ));
    }

    #[test]
    fn completed_discovery_replaces_default_generated_but_not_manual_model() {
        let claude_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "claude")
            .unwrap();
        let fallback = known_models_for("claude")[0].clone();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: claude_idx,
            name: "claude".to_string(),
            model: fallback,
            api_key: Some("test-key".to_string()),
            focused_field: 2,
            editing_idx: None,
        });
        set_catalog_model_provenance(&mut state, ModelSelectionProvenance::DefaultGenerated);
        let profile = model_catalog_profile("claude", "claude", "test-key", None).unwrap();
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            ModelCatalog {
                provider: "claude".to_string(),
                profile_id: "claude".to_string(),
                models_url: "https://api.anthropic.com/v1/models".to_string(),
                models: vec!["account-visible-model".to_string()],
                source: CatalogSource::Discovered,
                refreshed_at: chrono::Utc::now(),
            },
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. })
                if model == "account-visible-model"
        ));

        if let Some(AddProviderStep::ConfigureRemote { model, .. }) = state
            .sections
            .get_mut(&WizardSection::Models)
            .and_then(|section| match section {
                SectionState::Models {
                    adding_provider, ..
                } => adding_provider.as_mut(),
                _ => None,
            })
        {
            *model = "manually-entered-model".to_string();
        }
        set_catalog_model_provenance(&mut state, ModelSelectionProvenance::Manual);
        install_completed_catalog_refresh(
            &mut state,
            &profile,
            ModelCatalog {
                provider: "claude".to_string(),
                profile_id: "claude".to_string(),
                models_url: "https://api.anthropic.com/v1/models".to_string(),
                models: vec!["newer-visible-model".to_string()],
                source: CatalogSource::Discovered,
                refreshed_at: chrono::Utc::now(),
            },
        );
        advance_catalog_refresh_if_done(&mut state);
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote { model, .. })
                if model == "manually-entered-model"
        ));
    }

    // ── is_overlay_active ─────────────────────────────────────────────────────

    #[test]
    fn test_is_overlay_active_false_by_default() {
        let state = WizardState::new(None);
        assert!(!is_overlay_active(&state));
    }

    #[test]
    fn test_is_overlay_active_true_when_configure_local() {
        let state = state_with_step(default_configure_local(0));
        assert!(is_overlay_active(&state));
    }

    #[test]
    fn test_is_overlay_active_true_when_configure_remote() {
        let state = state_with_step(default_configure_remote(0));
        assert!(is_overlay_active(&state));
    }

    #[test]
    fn test_is_overlay_active_true_when_select_add_type() {
        let state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
        assert!(is_overlay_active(&state));
    }

    // ── ConfigureLocal: focus navigation ─────────────────────────────────────

    #[test]
    fn test_configure_local_down_advances_focused_field() {
        let mut state = state_with_step(default_configure_local(0));
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 1);
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_up_decrements_focused_field() {
        let mut state = state_with_step(default_configure_local(2));
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 1);
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_up_clamps_at_zero() {
        let mut state = state_with_step(default_configure_local(0));
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 0, "should not go below 0");
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_down_clamps_at_three() {
        let mut state = state_with_step(default_configure_local(3));
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 3, "should not go past 3 (Device)");
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    // ── ConfigureLocal: option cycling ───────────────────────────────────────

    #[test]
    fn test_configure_local_right_cycles_family_forward() {
        let mut state = state_with_step(default_configure_local(1)); // focused on Family
                                                                     // Qwen2 → Gemma2
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { family, .. }) = get_step(&state) {
            assert_eq!(*family, ModelFamily::Gemma2);
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_left_cycles_family_backward() {
        let mut state = state_with_step(default_configure_local(1)); // Qwen2, focused Family
                                                                     // Qwen2 → wraps to last family (DeepSeek)
        handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { family, .. }) = get_step(&state) {
            assert_eq!(*family, ModelFamily::DeepSeek);
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_right_cycles_size_forward() {
        let mut state = state_with_step(default_configure_local(2)); // focused on Size (Medium)
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { size, .. }) = get_step(&state) {
            assert_eq!(*size, ModelSize::Large);
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_right_on_device_field_cycles() {
        let mut state = state_with_step(AddProviderStep::ConfigureLocal {
            inference_provider: InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Medium,
            execution: ExecutionTarget::Auto,
            focused_field: 3, // Device
        });
        // Auto is first in the list; right should cycle to next (Cpu on non-macOS, CoreML on macOS)
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal { execution, .. }) = get_step(&state) {
            assert_ne!(
                *execution,
                ExecutionTarget::Auto,
                "should have cycled off Auto"
            );
        } else {
            panic!("expected ConfigureLocal");
        }
    }

    #[test]
    fn test_configure_local_right_on_non_focused_field_does_not_affect_others() {
        let mut state = state_with_step(default_configure_local(2)); // focused Size
        let before_family;
        let before_execution;
        if let Some(AddProviderStep::ConfigureLocal {
            family, execution, ..
        }) = get_step(&state)
        {
            before_family = *family;
            before_execution = *execution;
        } else {
            panic!();
        }
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureLocal {
            family, execution, ..
        }) = get_step(&state)
        {
            assert_eq!(
                *family, before_family,
                "family should not change when Size is focused"
            );
            assert_eq!(*execution, before_execution, "execution should not change");
        }
    }

    // ── ConfigureLocal: Enter commits ─────────────────────────────────────────

    #[test]
    fn test_configure_local_enter_replaces_empty_primary() {
        // Default state has remote claude with empty key — Enter should replace primary
        let mut state = state_with_step(AddProviderStep::ConfigureLocal {
            inference_provider: InferenceProvider::Onnx,
            family: ModelFamily::Phi,
            size: ModelSize::Small,
            execution: ExecutionTarget::Cpu,
            focused_field: 0,
        });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        // overlay should be gone
        assert!(get_step(&state).is_none());
        // primary should now be local
        if let Some(ModelConfig::Local {
            family,
            size,
            execution,
            inference_provider,
            ..
        }) = get_primary(&state)
        {
            assert_eq!(*family, ModelFamily::Phi);
            assert_eq!(*size, ModelSize::Small);
            assert_eq!(*execution, ExecutionTarget::Cpu);
            assert_eq!(*inference_provider, InferenceProvider::Onnx);
        } else {
            panic!("expected Local primary model");
        }
    }

    #[test]
    fn test_configure_local_enter_adds_tool_model_when_primary_is_configured() {
        let mut state = state_with_step(default_configure_local(0));
        // Give primary a real API key so it won't be replaced
        if let Some(SectionState::Models { primary_model, .. }) =
            state.sections.get_mut(&WizardSection::Models)
        {
            *primary_model = ModelConfig::Remote {
                provider: "claude".to_string(),
                name: "claude".to_string(),
                api_key: "sk-ant-abc123".to_string(),
                model: String::new(),
                enabled: true,
                persisted: None,
            };
        }
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        let tools = get_tool_models(&state);
        assert_eq!(tools.len(), 1);
        if let ModelConfig::Local { family, size, .. } = &tools[0] {
            assert_eq!(*family, ModelFamily::Qwen2);
            assert_eq!(*size, ModelSize::Medium);
        } else {
            panic!("expected Local tool model");
        }
    }

    // ── ConfigureLocal: Esc goes back ─────────────────────────────────────────

    #[test]
    fn test_configure_local_esc_closes_overlay() {
        let mut state = state_with_step(default_configure_local(0));
        handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
        assert!(get_step(&state).is_none());
    }

    // ── ConfigureRemote: focus navigation ────────────────────────────────────

    #[test]
    fn test_configure_remote_down_advances_focused_field() {
        let mut state = state_with_step(default_configure_remote(0));
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 1);
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_up_clamps_at_zero() {
        let mut state = state_with_step(default_configure_remote(0));
        handle_models_input(&mut state, key(KeyCode::Up)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 0);
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_down_clamps_at_three() {
        let mut state = state_with_step(default_configure_remote(3));
        handle_models_input(&mut state, key(KeyCode::Down)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { focused_field, .. }) = get_step(&state) {
            assert_eq!(*focused_field, 3);
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    // ── ConfigureRemote: provider cycling ────────────────────────────────────

    #[test]
    fn test_configure_remote_right_cycles_provider_forward() {
        let mut state = state_with_step(default_configure_remote(0)); // focused Provider
        let initial_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "grok")
            .unwrap();
        let expected_idx = (initial_idx + 1) % CLOUD_PROVIDERS.len();
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote {
            provider_idx,
            model,
            ..
        }) = get_step(&state)
        {
            assert_eq!(*provider_idx, expected_idx);
            // model should reset to default for new provider
            let expected_model = CLOUD_PROVIDERS[expected_idx].2;
            assert_eq!(model.as_str(), expected_model);
            assert_eq!(
                catalog_model_provenance(&state),
                ModelSelectionProvenance::Blank
            );
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_left_wraps_provider_to_last() {
        let mut state = state_with_step(default_configure_remote(0));
        let initial_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "grok")
            .unwrap();
        let expected_idx = (initial_idx + CLOUD_PROVIDERS.len() - 1) % CLOUD_PROVIDERS.len();
        handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote {
            provider_idx,
            model,
            ..
        }) = get_step(&state)
        {
            assert_eq!(*provider_idx, expected_idx);
            let expected_model = CLOUD_PROVIDERS[expected_idx].2;
            assert_eq!(model.as_str(), expected_model);
            assert_eq!(
                catalog_model_provenance(&state),
                ModelSelectionProvenance::DefaultGenerated
            );
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    // ── ConfigureRemote: model cycling ───────────────────────────────────────

    #[test]
    fn test_configure_remote_right_on_model_field_cycles_to_next_known_model() {
        // OpenAI's deliberately small fallback has multiple choices to cycle.
        let openai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "openai")
            .unwrap();
        let first_model = known_models_for("openai")[0].clone();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: openai_idx,
            name: "openai".to_string(),
            model: first_model,
            api_key: Some(String::new()),
            focused_field: 2, // Model field
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Right)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
            let models = known_models_for("openai");
            assert_eq!(model, &models[1]);
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_left_on_model_field_cycles_backward() {
        let claude_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "claude")
            .unwrap();
        let first_model = known_models_for("claude")[0].clone();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: claude_idx,
            name: "claude".to_string(),
            model: first_model,
            api_key: Some(String::new()),
            focused_field: 2,
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Left)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
            let models = known_models_for("claude");
            // wraps from first to last
            assert_eq!(model, &models[models.len() - 1]);
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    // ── ConfigureRemote: text input on API key / model ────────────────────────

    #[test]
    fn test_configure_remote_typing_appends_to_api_key_field() {
        let mut state = state_with_step(default_configure_remote(3)); // focused APIKey
        handle_models_input(&mut state, key(KeyCode::Char('s'))).unwrap();
        handle_models_input(&mut state, key(KeyCode::Char('k'))).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
            assert_eq!(api_key.as_deref(), Some("sk"));
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_typing_appends_to_model_field() {
        let mut state = state_with_step(default_configure_remote(2)); // focused Model
        handle_models_input(&mut state, key(KeyCode::Char('m'))).unwrap();
        handle_models_input(&mut state, key(KeyCode::Char('y'))).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { model, .. }) = get_step(&state) {
            // starts with default model then appends
            assert!(model.ends_with("my"));
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_backspace_removes_from_api_key() {
        let grok_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "grok")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: grok_idx,
            name: "grok".to_string(),
            model: "grok-code-fast-1".to_string(),
            api_key: Some("abc".to_string()),
            focused_field: 3,
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Backspace)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
            assert_eq!(api_key.as_deref(), Some("ab"));
        } else {
            panic!("expected ConfigureRemote");
        }
    }

    #[test]
    fn test_configure_remote_typing_on_provider_field_is_ignored() {
        let mut state = state_with_step(default_configure_remote(0)); // focused Provider (field 0)
        let before_key;
        if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
            before_key = api_key.clone();
        } else {
            panic!();
        }
        handle_models_input(&mut state, key(KeyCode::Char('x'))).unwrap();
        if let Some(AddProviderStep::ConfigureRemote { api_key, .. }) = get_step(&state) {
            assert_eq!(
                api_key, &before_key,
                "typing on Provider field should not modify api_key"
            );
        }
    }

    // ── ConfigureRemote: Enter commits ────────────────────────────────────────

    #[test]
    fn test_configure_remote_enter_replaces_empty_primary() {
        let grok_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "grok")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: grok_idx,
            name: "grok".to_string(),
            model: "grok-code-fast-1".to_string(),
            api_key: Some("xai-test-key".to_string()),
            focused_field: 3,
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(get_step(&state).is_none());
        if let Some(ModelConfig::Remote {
            provider,
            api_key,
            model,
            ..
        }) = get_primary(&state)
        {
            assert_eq!(provider.as_str(), "grok");
            assert_eq!(api_key.as_str(), "xai-test-key");
            assert_eq!(model.as_str(), "grok-code-fast-1");
        } else {
            panic!("expected Remote primary model");
        }
    }

    #[test]
    fn gemini_25_default_survives_save_load_and_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let config_path = directory.path().join("config.toml");
        let metrics_dir = directory.path().join("metrics");
        let gemini_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "gemini")
            .unwrap();
        let canonical_model = "gemini-2.5-flash";
        assert_eq!(CLOUD_PROVIDERS[gemini_idx].2, canonical_model);
        assert_eq!(known_models_for("gemini"), vec![canonical_model]);

        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: gemini_idx,
            name: "gemini".to_string(),
            model: canonical_model.to_string(),
            api_key: Some("gemini-test-key-that-is-long-enough-123456".to_string()),
            focused_field: 3,
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        let result = build_setup_result(&state).unwrap();
        config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
            .save_to(&config_path)
            .unwrap();

        let loaded =
            crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None)
                .unwrap();
        assert!(matches!(
            loaded.providers.first(),
            Some(ProviderEntry::Gemini { model: Some(model), .. }) if model == canonical_model
        ));
        let reopened = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
        assert!(matches!(
            get_primary(&reopened),
            Some(ModelConfig::Remote { provider, model, .. })
                if provider == "gemini" && model == canonical_model
        ));
    }

    #[test]
    fn test_configure_remote_requires_refresh_or_manual_id_when_model_empty() {
        let claude_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, _, _, _)| *id == "claude")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: claude_idx,
            name: "claude".to_string(),
            model: String::new(),
            api_key: Some("sk-ant-key".to_string()),
            focused_field: 3,
            editing_idx: None,
        });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            get_step(&state),
            Some(AddProviderStep::ConfigureRemote {
                model,
                focused_field: 2,
                ..
            }) if model.is_empty()
        ));
        assert!(matches!(
            state.sections.get(&WizardSection::Models),
            Some(SectionState::Models {
                catalog_error: Some(error),
                ..
            }) if error.contains("Ctrl+R")
        ));
    }

    // ── ConfigureRemote: Esc goes back ────────────────────────────────────────

    #[test]
    fn test_configure_remote_esc_closes_overlay() {
        let mut state = state_with_step(default_configure_remote(0));
        handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
        assert!(get_step(&state).is_none());
    }

    // ── SelectAddType routing ─────────────────────────────────────────────────

    #[test]
    fn test_select_add_type_enter_on_cloud_opens_configure_remote() {
        let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(
            matches!(
                get_step(&state),
                Some(AddProviderStep::ConfigureRemote {
                    provider_idx: 0,
                    ..
                })
            ),
            "selecting first cloud provider should open ConfigureRemote at index 0"
        );
    }

    #[test]
    fn test_select_add_type_enter_on_local_opens_configure_local() {
        let n_cloud = CLOUD_PROVIDERS.len();
        let mut state = state_with_step(AddProviderStep::SelectAddType { selected: n_cloud });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        assert!(
            matches!(
                get_step(&state),
                Some(AddProviderStep::ConfigureLocal { .. })
            ),
            "selecting 'Local model' should open ConfigureLocal"
        );
    }

    #[test]
    fn test_api_key_provider_starts_on_api_key_field() {
        let grok_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "grok")
            .unwrap();
        let mut state = state_with_step(AddProviderStep::SelectAddType { selected: grok_idx });
        handle_models_input(&mut state, key(KeyCode::Enter)).unwrap();
        if let Some(AddProviderStep::ConfigureRemote {
            provider_idx,
            api_key,
            focused_field,
            ..
        }) = get_step(&state)
        {
            assert_eq!(*provider_idx, grok_idx);
            assert_eq!(api_key.as_deref(), Some(""));
            assert_eq!(
                *focused_field, 3,
                "API-key providers should open focused on the API key field"
            );
        } else {
            panic!(
                "expected ConfigureRemote, got {:?}",
                get_step(&state).map(|s| format!("{:?}", s))
            );
        }
    }

    #[test]
    fn test_select_add_type_esc_closes_overlay() {
        let mut state = state_with_step(AddProviderStep::SelectAddType { selected: 0 });
        handle_models_input(&mut state, key(KeyCode::Esc)).unwrap();
        assert!(
            get_step(&state).is_none(),
            "Esc on SelectAddType should close overlay"
        );
    }

    // ── build_setup_result: inference_provider propagation ───────────────────

    #[test]
    fn test_build_setup_result_uses_inference_provider_from_local_model() {
        let mut state = WizardState::new(None);
        // Set primary to a local model with ONNX provider
        if let Some(SectionState::Models { primary_model, .. }) =
            state.sections.get_mut(&WizardSection::Models)
        {
            *primary_model = ModelConfig::Local {
                family: ModelFamily::Llama3,
                size: ModelSize::Large,
                execution: ExecutionTarget::Cpu,
                inference_provider: InferenceProvider::Onnx,
                enabled: true,
                persisted: None,
            };
        }
        let result = build_setup_result(&state).unwrap();
        assert!(result.backend_enabled);
        assert_eq!(result.inference_provider, InferenceProvider::Onnx);
        assert_eq!(result.model_family, ModelFamily::Llama3);
        assert_eq!(result.model_size, ModelSize::Large);
        assert_eq!(result.execution_target, ExecutionTarget::Cpu);
    }

    #[test]
    fn test_build_setup_result_remote_primary_disables_backend() {
        let mut state = WizardState::new(None);
        if let Some(SectionState::Models { primary_model, .. }) =
            state.sections.get_mut(&WizardSection::Models)
        {
            *primary_model = ModelConfig::Remote {
                provider: "claude".to_string(),
                name: "claude".to_string(),
                api_key: "sk-ant-test".to_string(),
                model: "claude-sonnet-4-6".to_string(),
                enabled: true,
                persisted: None,
            };
        }
        let result = build_setup_result(&state).unwrap();
        assert!(!result.backend_enabled);
        assert_eq!(result.claude_api_key, "sk-ant-test");
    }

    // ── ModelConfig::Local inference_provider field ───────────────────────────

    #[test]
    fn test_model_config_local_stores_inference_provider() {
        let config = ModelConfig::Local {
            family: ModelFamily::Gemma2,
            size: ModelSize::XLarge,
            execution: ExecutionTarget::Cpu,
            inference_provider: InferenceProvider::Onnx,
            enabled: true,
            persisted: None,
        };
        if let ModelConfig::Local {
            inference_provider, ..
        } = config
        {
            assert_eq!(inference_provider, InferenceProvider::Onnx);
        } else {
            panic!("unexpected variant");
        }
    }

    #[test]
    fn test_wizard_state_new_loads_inference_provider_from_existing_config() {
        use crate::config::{BackendConfig, Config};
        let mut config = Config::with_providers(vec![]);
        config.backend = BackendConfig {
            enabled: true,
            inference_provider: InferenceProvider::Onnx,
            execution_target: ExecutionTarget::Cpu,
            model_family: ModelFamily::DeepSeek,
            model_size: ModelSize::Large,
            ..Default::default()
        };
        let state = WizardState::new(Some(&config));
        if let Some(ModelConfig::Local {
            inference_provider,
            family,
            ..
        }) = get_primary(&state)
        {
            assert_eq!(*inference_provider, InferenceProvider::Onnx);
            assert_eq!(*family, ModelFamily::DeepSeek);
        } else {
            panic!("expected Local primary when backend is enabled");
        }
    }

    #[test]
    fn test_coreml_policy_survives_wizard_mapping_save_and_reload_for_every_compute_unit() {
        use crate::config::{Config, CoreMlComputeUnits};

        for compute_units in [
            CoreMlComputeUnits::All,
            CoreMlComputeUnits::CpuAndNeuralEngine,
            CoreMlComputeUnits::CpuAndGpu,
            CoreMlComputeUnits::CpuOnly,
        ] {
            let directory = tempfile::tempdir().unwrap();
            let config_path = directory.path().join("config.toml");
            let metrics_dir = directory.path().join("metrics");
            let providers = vec![ProviderEntry::Local {
                inference_provider: InferenceProvider::Onnx,
                execution_target: ExecutionTarget::Auto,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                enabled: true,
                name: Some("local-coreml-policy-test".to_string()),
            }];
            let mut existing =
                Config::with_providers_and_paths(providers, metrics_dir.clone(), None);
            existing.backend.coreml = CoreMlConfig {
                compute_units,
                profile_compute_plan: true,
                enable_subgraphs: true,
            };

            let state = WizardState::new_with_catalog_cache_dir(Some(&existing), None);
            let result = build_setup_result(&state).unwrap();
            assert_eq!(result.coreml, existing.backend.coreml);

            config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
                .save_to(&config_path)
                .unwrap();
            let reloaded =
                crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None)
                    .unwrap();
            assert_eq!(reloaded.backend.coreml, existing.backend.coreml);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_setup_coreml_auto_label_is_dispatcher_not_ane_only_or_fastest() {
        let label = ExecutionTarget::CoreML.name();
        let description = ExecutionTarget::CoreML.description();

        assert_eq!(label, "CoreML (Auto: ANE/GPU/CPU)");
        assert!(description.contains("automatic compute-unit selection"));
        assert!(!description.to_ascii_lowercase().contains("fastest"));
        assert!(!description.contains("ANE only"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_reopened_coreml_policy_renders_requested_units_for_every_policy() {
        use crate::config::{Config, CoreMlComputeUnits};

        for (compute_units, expected) in [
            (CoreMlComputeUnits::All, "CoreML (Auto: ANE/GPU/CPU)"),
            (CoreMlComputeUnits::CpuAndNeuralEngine, "CoreML (CPU + ANE)"),
            (CoreMlComputeUnits::CpuAndGpu, "CoreML (CPU + GPU)"),
            (CoreMlComputeUnits::CpuOnly, "CoreML (CPU only)"),
        ] {
            let coreml = CoreMlConfig {
                compute_units,
                ..CoreMlConfig::default()
            };
            let mut reopened = Config::with_providers(vec![ProviderEntry::Local {
                inference_provider: InferenceProvider::Onnx,
                execution_target: ExecutionTarget::CoreML,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Medium,
                model_repo: None,
                model_path: None,
                enabled: true,
                name: Some("reopened-coreml".to_string()),
            }]);
            reopened.backend.coreml = coreml;
            let state = WizardState::new_with_catalog_cache_dir(Some(&reopened), None);
            assert_eq!(state.coreml, coreml);
            assert_eq!(
                execution_target_display(ExecutionTarget::CoreML, state.coreml),
                expected
            );
        }
    }

    #[test]
    fn test_cloud_primary_keeps_local_qwen_as_tool_model_on_reopen() {
        use crate::config::{Config, ProviderEntry};

        #[cfg(target_os = "macos")]
        let execution_target = ExecutionTarget::CoreML;
        #[cfg(not(target_os = "macos"))]
        let execution_target = ExecutionTarget::Cpu;

        let original = Config::with_providers(vec![
            ProviderEntry::Grok {
                api_key: "xai-test-key".to_string(),
                model: Some("grok-code-fast-1".to_string()),
                base_url: None,
                chat_path: None,
                models_path: None,
                name: Some("grok-code-fast-1".to_string()),
            },
            ProviderEntry::Local {
                inference_provider: InferenceProvider::Onnx,
                execution_target,
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Small,
                model_repo: Some("onnx-community/Qwen2.5-Coder-3B-Instruct".to_string()),
                model_path: Some("/models/qwen-coder".into()),
                enabled: true,
                name: Some("local-qwen".to_string()),
            },
        ]);

        let state = WizardState::new(Some(&original));
        assert!(matches!(
            get_primary(&state),
            Some(ModelConfig::Remote { provider, .. }) if provider == "grok"
        ));
        assert!(matches!(
            get_tool_models(&state).as_slice(),
            [ModelConfig::Local {
                family: ModelFamily::Qwen2,
                size: ModelSize::Small,
                execution,
                ..
            }] if *execution == execution_target
        ));

        let saved = build_setup_result(&state).unwrap();
        assert_eq!(saved.providers.len(), 2);
        assert!(matches!(saved.providers[0], ProviderEntry::Grok { .. }));
        assert!(matches!(
            saved.providers[1],
            ProviderEntry::Local {
                model_family: ModelFamily::Qwen2,
                model_size: ModelSize::Small,
                execution_target: saved_execution_target,
                model_repo: Some(ref repo),
                model_path: Some(ref path),
                name: Some(ref name),
                ..
            } if saved_execution_target == execution_target
                && repo == "onnx-community/Qwen2.5-Coder-3B-Instruct"
                && path == &std::path::PathBuf::from("/models/qwen-coder")
                && name == "local-qwen"
        ));

        let reopened = WizardState::new(Some(&Config::with_providers(saved.providers)));
        assert!(matches!(
            get_tool_models(&reopened).as_slice(),
            [ModelConfig::Local {
                family: ModelFamily::Qwen2,
                ..
            }]
        ));
    }

    #[test]
    fn test_wizard_round_trip_preserves_non_teacher_provider_metadata() {
        use crate::config::{Config, ProviderEntry, ReasoningEffort};

        let providers = vec![
            ProviderEntry::Openai {
                api_key: "openai-key".to_string(),
                model: Some("gpt-test".to_string()),
                base_url: Some("https://compatible.example/api".to_string()),
                chat_path: Some("/custom/chat".to_string()),
                models_path: Some("/custom/models".to_string()),
                name: Some("reasoning-profile".to_string()),
                reasoning_effort: Some(ReasoningEffort::High),
            },
            ProviderEntry::Ollama {
                model: "qwen2.5:7b".to_string(),
                base_url: "http://model-host:11434".to_string(),
                name: Some("office-qwen".to_string()),
            },
            ProviderEntry::RemoteDaemon {
                address: "https://finch-host:11435".to_string(),
                name: Some("build-machine".to_string()),
            },
        ];
        let state = WizardState::new(Some(&Config::with_providers(providers.clone())));
        let saved = build_setup_result(&state).unwrap();

        assert_eq!(saved.providers, providers);
    }

    #[test]
    fn ordinary_remote_edits_preserve_exact_connection_profile_metadata() {
        use crate::config::{Config, ReasoningEffort};

        let cases = vec![
            ProviderEntry::Claude {
                api_key: "old-key".to_string(),
                model: Some("old-model".to_string()),
                base_url: Some("https://claude-compatible.example/v1".to_string()),
                chat_path: Some("https://chat.example/claude?preview=1".to_string()),
                models_path: Some("https://models.example/claude?account=a".to_string()),
                name: Some("claude-old".to_string()),
            },
            ProviderEntry::Openai {
                api_key: "old-key".to_string(),
                model: Some("old-model".to_string()),
                base_url: Some("https://openai-compatible.example/v1".to_string()),
                chat_path: Some("https://chat.example/openai?preview=1".to_string()),
                models_path: Some("https://models.example/openai?account=b".to_string()),
                name: Some("openai-old".to_string()),
                reasoning_effort: Some(ReasoningEffort::High),
            },
            ProviderEntry::Grok {
                api_key: "old-key".to_string(),
                model: Some("old-model".to_string()),
                base_url: Some("https://xai-compatible.example/v1".to_string()),
                chat_path: Some("https://chat.example/xai?preview=1".to_string()),
                models_path: Some("https://models.example/xai?account=c".to_string()),
                name: Some("xai-old".to_string()),
            },
            ProviderEntry::Mistral {
                api_key: "old-key".to_string(),
                model: Some("old-model".to_string()),
                base_url: Some("https://mistral-compatible.example/v1".to_string()),
                chat_path: Some("https://chat.example/mistral?preview=1".to_string()),
                models_path: Some("https://models.example/mistral?account=d".to_string()),
                name: Some("mistral-old".to_string()),
            },
        ];

        for original in cases {
            let expected_type = original.provider_type().to_string();
            let mut state = WizardState::new(Some(&Config::with_providers(vec![original.clone()])));
            edit_primary_remote(&mut state, "renamed", "new-model", "new-key");
            let saved = build_setup_result(&state).unwrap();
            let edited = &saved.providers[0];
            assert_eq!(edited.provider_type(), expected_type);
            match (&original, edited) {
                (
                    ProviderEntry::Claude {
                        base_url,
                        chat_path,
                        models_path,
                        ..
                    },
                    ProviderEntry::Claude {
                        api_key,
                        model,
                        base_url: actual_base,
                        chat_path: actual_chat,
                        models_path: actual_models,
                        name,
                    },
                )
                | (
                    ProviderEntry::Grok {
                        base_url,
                        chat_path,
                        models_path,
                        ..
                    },
                    ProviderEntry::Grok {
                        api_key,
                        model,
                        base_url: actual_base,
                        chat_path: actual_chat,
                        models_path: actual_models,
                        name,
                    },
                )
                | (
                    ProviderEntry::Mistral {
                        base_url,
                        chat_path,
                        models_path,
                        ..
                    },
                    ProviderEntry::Mistral {
                        api_key,
                        model,
                        base_url: actual_base,
                        chat_path: actual_chat,
                        models_path: actual_models,
                        name,
                    },
                ) => {
                    assert_eq!(
                        (actual_base, actual_chat, actual_models),
                        (base_url, chat_path, models_path)
                    );
                    assert_eq!(
                        (api_key.as_str(), model.as_deref(), name.as_deref()),
                        ("new-key", Some("new-model"), Some("renamed"))
                    );
                }
                (
                    ProviderEntry::Openai {
                        base_url,
                        chat_path,
                        models_path,
                        reasoning_effort,
                        ..
                    },
                    ProviderEntry::Openai {
                        api_key,
                        model,
                        base_url: actual_base,
                        chat_path: actual_chat,
                        models_path: actual_models,
                        name,
                        reasoning_effort: actual_reasoning,
                    },
                ) => {
                    assert_eq!(
                        (actual_base, actual_chat, actual_models),
                        (base_url, chat_path, models_path)
                    );
                    assert_eq!(actual_reasoning, reasoning_effort);
                    assert_eq!(
                        (api_key.as_str(), model.as_deref(), name.as_deref()),
                        ("new-key", Some("new-model"), Some("renamed"))
                    );
                }
                _ => panic!("provider type changed during edit"),
            }
        }
    }

    #[test]
    fn test_edited_system_prompt_is_included_in_setup_result() {
        let mut state = WizardState::new(None);
        if let Some(SectionState::Personas {
            available_personas,
            selected_idx,
            default_persona,
            ..
        }) = state.sections.get_mut(&WizardSection::Personas)
        {
            *selected_idx = available_personas
                .iter()
                .position(|persona| persona.slug == "default")
                .unwrap();
            *default_persona = "default".to_string();
            available_personas[*selected_idx].system_prompt =
                "Say hello once you've loaded.".to_string();
        } else {
            panic!("expected persona section");
        }

        let result = build_setup_result(&state).unwrap();
        assert_eq!(
            result.custom_system_prompt.as_deref(),
            Some("Say hello once you've loaded.")
        );
        let config = config_from_setup_result(&result);
        assert_eq!(config.active_persona, "default");
    }

    #[test]
    fn chooser_keeps_platform_api_key_distinct_from_native_chatgpt_subscription() {
        assert!(CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "openai"));
        assert!(CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "chatgpt"));
        assert!(!CLOUD_PROVIDERS
            .iter()
            .any(|(id, ..)| *id == "chatgpt_subscription"));
    }

    #[test]
    fn zai_setup_creates_a_secret_free_named_profile_with_max_reasoning() {
        use crate::config::{CredentialProvider, EndpointFamily, ProviderEntry, ReasoningEffort};

        assert!(CLOUD_PROVIDERS.iter().any(|(id, ..)| *id == "zai"));
        assert_eq!(default_credential_input("zai"), "ZAI_API_KEY");
        let (credential, profile) =
            zai_named_setup_entries("zai-flash", "glm-5.3-flash", "ZAI_API_KEY").unwrap();
        assert_eq!(credential.provider, CredentialProvider::Zai);
        assert_eq!(credential.audience.family, EndpointFamily::ZaiApi);
        assert_eq!(credential.secret_ref, "env:ZAI_API_KEY");
        assert!(matches!(
            profile,
            ProviderEntry::Credentialed {
                provider: CredentialProvider::Zai,
                model: Some(ref model),
                reasoning_effort: Some(ReasoningEffort::Max),
                ref credential,
                ..
            } if model == "glm-5.3-flash"
                && credential.credential_ref == "zai-flash-credential"
        ));
        // Exercise the same credential record shape persisted under
        // `[[credentials]]`. TOML cannot represent a synthetic top-level
        // tuple of arrays, and Finch never writes that shape.
        let encoded_credential = toml::to_string(&credential).unwrap();
        assert!(encoded_credential.contains("env:ZAI_API_KEY"));
        assert!(!encoded_credential.contains("api_key ="));
        let encoded_profile = serde_json::to_string(&profile).unwrap();
        assert!(!encoded_profile.contains("ZAI_API_KEY"));
    }

    #[test]
    fn zai_setup_rejects_secret_material_instead_of_persisting_it() {
        for invalid in ["", "zai-key", "sk-secret-value", "ZAI API KEY", "9ZAI_KEY"] {
            assert!(zai_named_setup_entries("zai", "glm-5.3-flash", invalid).is_err());
        }
    }

    #[test]
    fn zai_first_refresh_uses_named_resolution_and_fails_before_http_when_env_is_missing() {
        let missing = format!("FINCH_ZAI_MISSING_REFRESH_{}", std::process::id());
        assert!(std::env::var_os(&missing).is_none());
        let zai_idx = CLOUD_PROVIDERS
            .iter()
            .position(|(id, ..)| *id == "zai")
            .unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let mut state = state_with_step(AddProviderStep::ConfigureRemote {
            provider_idx: zai_idx,
            name: "zai-refresh".into(),
            model: "glm-5.3-flash".into(),
            api_key: Some(missing),
            focused_field: 3,
            editing_idx: None,
        });
        state.catalog_cache_dir = Some(temporary.path().join("catalog"));

        handle_models_input(
            &mut state,
            modified_key(KeyCode::Char('r'), KeyModifiers::CONTROL),
        )
        .unwrap();
        for _ in 0..200 {
            advance_catalog_refresh_if_done(&mut state);
            let pending = matches!(
                state.sections.get(&WizardSection::Models),
                Some(SectionState::Models {
                    catalog_refresh: Some(_),
                    ..
                })
            );
            if !pending {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let error = match state.sections.get(&WizardSection::Models) {
            Some(SectionState::Models {
                catalog_refresh: None,
                catalog_error: Some(error),
                ..
            }) => error,
            other => panic!("expected completed local credential rejection, got {other:?}"),
        };
        assert!(
            error.contains("Failed to resolve named credential"),
            "{error}"
        );
        assert!(!error.contains("Bearer"), "{error}");
        assert!(!temporary.path().join("catalog").exists());
    }

    #[test]
    fn zai_save_reopen_and_edit_preserve_environment_reference_only_ui() {
        let temporary = tempfile::tempdir().unwrap();
        let config_path = temporary.path().join("config.toml");
        let metrics_dir = temporary.path().join("metrics");
        let (credential, profile) =
            zai_named_setup_entries("zai-work", "glm-5.3-flash", "ZAI_API_KEY").unwrap();
        let initial =
            crate::config::Config::with_providers(vec![profile]).with_credentials(vec![credential]);
        let state = WizardState::new_with_catalog_cache_dir(Some(&initial), None);
        let result = build_setup_result(&state).unwrap();
        config_from_setup_result_with_paths(&result, metrics_dir.clone(), None)
            .save_to(&config_path)
            .unwrap();
        let loaded =
            crate::config::load_config_from_path_with_paths(&config_path, metrics_dir, None)
                .unwrap();
        let mut reopened = WizardState::new_with_catalog_cache_dir(Some(&loaded), None);
        assert!(matches!(
            get_primary(&reopened),
            Some(ModelConfig::Remote {
                provider,
                api_key,
                persisted: Some(ProviderEntry::Credentialed { .. }),
                ..
            }) if provider == "zai" && api_key == "ZAI_API_KEY"
        ));
        let main = render_wizard_text(&reopened);
        assert!(main.contains("Named environment credential"), "{main}");
        assert!(!main.contains("Paste your API key"), "{main}");
        assert!(!main.contains("ZAI_API_KEY"), "{main}");

        handle_models_input(&mut reopened, key(KeyCode::Enter)).unwrap();
        assert!(matches!(
            get_step(&reopened),
            Some(AddProviderStep::ConfigureRemote {
                api_key: Some(environment),
                ..
            }) if environment == "ZAI_API_KEY"
        ));
        let overlay = render_wizard_text(&reopened);
        assert!(overlay.contains("Key env"), "{overlay}");
        assert!(overlay.contains("ZAI_API_KEY"), "{overlay}");
        assert!(!overlay.contains("Edit API Key"), "{overlay}");

        handle_models_input(&mut reopened, key(KeyCode::Esc)).unwrap();
        if let Some(SectionState::Models { editing_mode, .. }) =
            reopened.sections.get_mut(&WizardSection::Models)
        {
            *editing_mode = true;
        }
        handle_models_input(&mut reopened, key(KeyCode::Char('s'))).unwrap();
        assert!(matches!(
            get_primary(&reopened),
            Some(ModelConfig::Remote { api_key, .. }) if api_key == "ZAI_API_KEY"
        ));
        let rejected = render_wizard_text(&reopened);
        assert!(
            rejected.contains("named environment reference"),
            "{rejected}"
        );
        assert!(!rejected.contains("Edit API Key"), "{rejected}");
    }

    #[tokio::test]
    async fn legacy_chatgpt_setup_fails_before_save() {
        let mut result = build_setup_result(&WizardState::new(None)).unwrap();
        result.providers = vec![ProviderEntry::LegacyChatgptSubscription {
            credential_ref: "codex-app-server:managed".into(),
            model: Some("gpt-5.6-sol".into()),
            name: Some("legacy".into()),
        }];

        let error = validate_and_apply_for(SetupInvocation::Command, &result)
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("Legacy chatgpt_subscription profiles are unsupported"));
        assert!(message.contains("finch setup") || message.contains("configure OpenAI Platform"));
    }

    #[test]
    fn named_credential_setup_reopen_and_save_preserves_reference_without_secret() {
        use crate::config::{
            AudienceBinding, CredentialBinding, CredentialKind, CredentialLifecycle,
            CredentialProvider, EndpointFamily, ProviderCredential,
        };
        let credential = ProviderCredential {
            name: "openai-work".into(),
            kind: CredentialKind::ApiKey,
            provider: CredentialProvider::OpenaiPlatform,
            issuer: "openai-platform".into(),
            audience: AudienceBinding::standard(EndpointFamily::OpenaiPlatform),
            tenant: None,
            project: None,
            account: Some("work".into()),
            scopes: std::collections::BTreeSet::new(),
            secret_ref: "env:OPENAI_WORK_API_KEY".into(),
            lifecycle: CredentialLifecycle::default(),
            revocation: Default::default(),
        };
        let profile = ProviderEntry::Credentialed {
            provider: CredentialProvider::OpenaiPlatform,
            credential: CredentialBinding {
                credential_ref: "openai-work".into(),
                audience: None,
                tenant: None,
                project: None,
                account: Some("work".into()),
                required_scopes: std::collections::BTreeSet::new(),
            },
            model: Some("gpt-5.6-sol".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("work-reasoning".into()),
            reasoning_effort: Some(crate::config::ReasoningEffort::High),
        };
        let existing = crate::config::Config::with_providers(vec![profile.clone()])
            .with_credentials(vec![credential.clone()]);

        let result = build_setup_result(&WizardState::new(Some(&existing))).unwrap();
        assert_eq!(result.providers, vec![profile]);
        assert_eq!(result.credentials, vec![credential]);
        let saved = config_from_setup_result(&result);
        saved.validate().unwrap();
        let serialized = toml::to_string(&saved.credentials()[0]).unwrap();
        assert!(serialized.contains("env:OPENAI_WORK_API_KEY"));
        assert!(!serialized.contains("sk-"));
    }

    #[derive(Default)]
    struct FakeChatGptSetupAuthenticator {
        calls: std::sync::Mutex<Vec<String>>,
        fail: std::sync::Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for FakeChatGptSetupAuthenticator {
        async fn ensure_named_credential(
            &self,
            reference: &str,
            _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
            cancel: tokio_util::sync::CancellationToken,
        ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
            self.calls.lock().unwrap().push(reference.to_string());
            if cancel.is_cancelled() {
                anyhow::bail!("ChatGPT setup login was cancelled");
            }
            if let Some(message) = self.fail.lock().unwrap().clone() {
                anyhow::bail!(message);
            }
            Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
                credential: chatgpt_setup_credential(
                    reference,
                    &format!("acct-{}", reference.rsplit(':').next().unwrap()),
                ),
                compensation: None,
            })
        }
    }

    #[cfg(unix)]
    struct DurableSetupAuthenticator {
        root: std::path::PathBuf,
        fail_reference: Option<String>,
        invalid_final_reference: Option<String>,
        replace_before_compensation: Option<String>,
    }

    #[cfg(unix)]
    impl DurableSetupAuthenticator {
        fn store(&self) -> crate::oauth::file_store::FileOAuthCredentialStore {
            crate::oauth::file_store::FileOAuthCredentialStore::new(self.root.clone())
        }

        fn token_record(reference: &str) -> crate::oauth::OAuthTokenRecord {
            crate::oauth::OAuthTokenRecord {
                dialect_id: "openai_chatgpt_subscription".into(),
                protocol_revision: crate::providers::chatgpt_oauth::CHATGPT_OAUTH_PROTOCOL_REVISION
                    .into(),
                provider: crate::config::CredentialProvider::ChatgptSubscription,
                kind: crate::config::CredentialKind::OauthDevice,
                issuer: "openai-chatgpt".into(),
                audience: crate::config::AudienceBinding::standard(
                    crate::config::EndpointFamily::ChatgptSubscription,
                ),
                client_id: crate::providers::chatgpt_oauth::OPENAI_PUBLIC_CLIENT_ID.into(),
                account: format!("acct-{}", reference.rsplit(':').next().unwrap()),
                tenant: None,
                project: None,
                scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
                access_token: format!("secret-access-{reference}"),
                refresh_token: Some(format!("secret-refresh-{reference}")),
                id_token: Some(format!("secret-identity-{reference}")),
                expires_at: Utc::now() + chrono::TimeDelta::hours(1),
                generation: uuid::Uuid::new_v4().to_string(),
                revoked: false,
                mutation_pending: false,
            }
        }
    }

    #[cfg(unix)]
    #[async_trait::async_trait]
    impl crate::cli::chatgpt_auth::ChatGptCredentialAuthenticator for DurableSetupAuthenticator {
        fn compensate_with_tombstone(
            &self,
            handle: &crate::cli::chatgpt_auth::ChatGptCompensationHandle,
        ) -> Result<()> {
            use crate::oauth::OAuthCredentialStore;
            let store = self.store();
            if self.replace_before_compensation.as_deref() == Some(handle.reference()) {
                let current = store
                    .load(handle.reference())?
                    .context("missing staged credential")?;
                let mut external = current.clone();
                external.generation = uuid::Uuid::new_v4().to_string();
                external.access_token = "external-replacement-sentinel".into();
                store.compare_and_swap(handle.reference(), Some(&current.generation), &external)?;
            }
            let current = store
                .load(handle.reference())?
                .context("missing staged credential")?;
            if current.generation != handle.generation() {
                anyhow::bail!("compensation generation changed; current record left untouched");
            }
            let mut tombstone = current.clone();
            tombstone.access_token.clear();
            tombstone.refresh_token = None;
            tombstone.id_token = None;
            tombstone.generation = uuid::Uuid::new_v4().to_string();
            tombstone.revoked = true;
            tombstone.mutation_pending = false;
            store.compare_and_swap(handle.reference(), Some(handle.generation()), &tombstone)
        }

        async fn ensure_named_credential(
            &self,
            reference: &str,
            _presentation: crate::cli::chatgpt_auth::DeviceLoginPresentation,
            _cancel: tokio_util::sync::CancellationToken,
        ) -> Result<crate::cli::chatgpt_auth::EnsuredChatGptCredential> {
            use crate::oauth::OAuthCredentialStore;
            if self.fail_reference.as_deref() == Some(reference) {
                anyhow::bail!("terminal second-account denial");
            }
            let store = self.store();
            let existing = store.load(reference)?;
            let expected = existing.as_ref().map(|record| record.generation.as_str());
            let replacement = Self::token_record(reference);
            let generation = replacement.generation.clone();
            store.compare_and_swap(reference, expected, &replacement)?;
            let mut credential = replacement.provider_credential(reference);
            if self.invalid_final_reference.as_deref() == Some(reference) {
                credential.issuer = "wrong-issuer".into();
            }
            Ok(crate::cli::chatgpt_auth::EnsuredChatGptCredential {
                credential,
                compensation: Some(crate::cli::chatgpt_auth::ChatGptCompensationHandle::issued(
                    reference, generation,
                )),
            })
        }
    }

    fn chatgpt_setup_credential(
        reference: &str,
        account: &str,
    ) -> crate::config::ProviderCredential {
        crate::config::ProviderCredential {
            name: reference.into(),
            kind: crate::config::CredentialKind::OauthDevice,
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            issuer: "openai-chatgpt".into(),
            audience: crate::config::AudienceBinding::standard(
                crate::config::EndpointFamily::ChatgptSubscription,
            ),
            tenant: None,
            project: None,
            account: Some(account.into()),
            scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
            secret_ref: format!("oauth-store:{reference}"),
            lifecycle: crate::config::CredentialLifecycle::Active {
                expires_at: Some(Utc::now() + chrono::TimeDelta::hours(1)),
                refreshable: true,
            },
            revocation: Default::default(),
        }
    }

    fn chatgpt_setup_profile(reference: &str, name: &str, model: &str) -> ProviderEntry {
        ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::ChatgptSubscription,
            credential: crate::config::CredentialBinding {
                credential_ref: reference.into(),
                audience: Some(crate::config::AudienceBinding::standard(
                    crate::config::EndpointFamily::ChatgptSubscription,
                )),
                tenant: None,
                project: None,
                account: None,
                required_scopes: crate::providers::chatgpt_oauth::chatgpt_required_scopes(),
            },
            model: Some(model.into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some(name.into()),
            reasoning_effort: None,
        }
    }

    fn setup_result_with_profiles(profiles: Vec<ProviderEntry>) -> SetupResult {
        build_setup_result(&WizardState::new(Some(
            &crate::config::Config::with_providers(profiles),
        )))
        .unwrap()
    }

    #[tokio::test]
    async fn first_run_command_and_repl_share_one_account_multi_model_setup_boundary() {
        for invocation in [
            SetupInvocation::FirstRun,
            SetupInvocation::Command,
            SetupInvocation::Repl,
        ] {
            let result = setup_result_with_profiles(vec![
                chatgpt_setup_profile("chatgpt:work", "work-fast", "gpt-5.6-sol"),
                chatgpt_setup_profile("chatgpt:work", "work-deep", "gpt-5.6-sol"),
            ]);
            let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
            let authenticator = FakeChatGptSetupAuthenticator::default();
            let config = prepare_chatgpt_setup_config(
                &result,
                &references,
                &authenticator,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(
                authenticator.calls.lock().unwrap().as_slice(),
                ["chatgpt:work"]
            );
            assert_eq!(config.providers.len(), 2, "{invocation:?}");
            assert_eq!(config.credentials().len(), 1, "{invocation:?}");
            config.validate().unwrap();
        }
    }

    #[tokio::test]
    async fn setup_keeps_two_named_chatgpt_accounts_distinct_without_fallback() {
        let result = setup_result_with_profiles(vec![
            chatgpt_setup_profile("chatgpt:work", "work", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:personal", "personal", "gpt-5.6-sol"),
        ]);
        let references = std::collections::BTreeSet::from([
            "chatgpt:personal".to_string(),
            "chatgpt:work".to_string(),
        ]);
        let authenticator = FakeChatGptSetupAuthenticator::default();
        let config = prepare_chatgpt_setup_config(
            &result,
            &references,
            &authenticator,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(config.credentials().len(), 2);
        assert_eq!(
            config.credentials()[0].secret_ref,
            "oauth-store:chatgpt:personal"
        );
        assert_eq!(
            config.credentials()[1].secret_ref,
            "oauth-store:chatgpt:work"
        );
    }

    #[tokio::test]
    async fn cancelled_or_unusable_setup_never_returns_a_config_or_tries_another_account() {
        let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
            "chatgpt:work",
            "work",
            "gpt-5.6-sol",
        )]);
        let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
        let authenticator = FakeChatGptSetupAuthenticator::default();
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let save_root = tempfile::tempdir().unwrap();
        let save_path = save_root.path().join("config.toml");
        std::fs::write(&save_path, "unchanged-config-sentinel").unwrap();
        let error = prepare_chatgpt_setup_config(&result, &references, &authenticator, cancel)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("was cancelled; setup was not saved"),
            "{error}"
        );
        assert!(
            error.contains("ChatGPT setup login was cancelled"),
            "{error}"
        );
        assert!(!error.contains("access_token"), "{error}");
        assert!(!error.contains("refresh_token"), "{error}");
        assert_eq!(
            std::fs::read_to_string(save_path).unwrap(),
            "unchanged-config-sentinel"
        );
        assert_eq!(
            authenticator.calls.lock().unwrap().as_slice(),
            ["chatgpt:work"]
        );

        let mut unusable = result;
        unusable.providers.push(ProviderEntry::Credentialed {
            provider: crate::config::CredentialProvider::OpenaiPlatform,
            credential: crate::config::CredentialBinding {
                credential_ref: "missing-platform".into(),
                audience: None,
                tenant: None,
                project: None,
                account: None,
                required_scopes: Default::default(),
            },
            model: Some("gpt-5.6-sol".into()),
            base_url: None,
            chat_path: None,
            models_path: None,
            name: Some("broken-platform".into()),
            reasoning_effort: None,
        });
        let before = authenticator.calls.lock().unwrap().len();
        assert!(prepare_chatgpt_setup_config(
            &unusable,
            &references,
            &authenticator,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .is_err());
        assert_eq!(authenticator.calls.lock().unwrap().len(), before);
    }

    #[tokio::test]
    async fn setup_denial_and_expiry_are_terminal_without_config_or_account_fallback() {
        let result = setup_result_with_profiles(vec![chatgpt_setup_profile(
            "chatgpt:work",
            "work",
            "gpt-5.6-sol",
        )]);
        let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
        for terminal in [
            "device authorization was denied",
            "device authorization expired",
        ] {
            let authenticator = FakeChatGptSetupAuthenticator::default();
            *authenticator.fail.lock().unwrap() = Some(terminal.into());
            let error = prepare_chatgpt_setup_config(
                &result,
                &references,
                &authenticator,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();
            assert!(error.contains("ChatGPT login for named credential 'chatgpt:work' failed"));
            assert!(error.contains(terminal), "{error}");
            assert_eq!(
                authenticator.calls.lock().unwrap().as_slice(),
                ["chatgpt:work"]
            );
        }
    }

    #[tokio::test]
    async fn revoked_and_expired_same_name_metadata_are_replaced_only_by_exact_account_result() {
        for lifecycle in [
            crate::config::CredentialLifecycle::Revoked,
            crate::config::CredentialLifecycle::Active {
                expires_at: Some(Utc::now() - chrono::TimeDelta::minutes(1)),
                refreshable: true,
            },
        ] {
            let mut result = setup_result_with_profiles(vec![chatgpt_setup_profile(
                "chatgpt:work",
                "work",
                "gpt-5.6-sol",
            )]);
            let mut stale = chatgpt_setup_credential("chatgpt:work", "acct-old");
            stale.lifecycle = lifecycle;
            result.credentials = vec![stale];
            let references = std::collections::BTreeSet::from(["chatgpt:work".to_string()]);
            let authenticator = FakeChatGptSetupAuthenticator::default();
            let config = prepare_chatgpt_setup_config(
                &result,
                &references,
                &authenticator,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(
                authenticator.calls.lock().unwrap().as_slice(),
                ["chatgpt:work"]
            );
            assert_eq!(config.credentials().len(), 1);
            assert_eq!(
                config.credentials()[0].account.as_deref(),
                Some("acct-work")
            );
            assert!(matches!(
                config.credentials()[0].lifecycle,
                crate::config::CredentialLifecycle::Active { .. }
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_multi_account_terminal_failure_tombstones_prior_issue_and_restart_can_resume() {
        use crate::oauth::OAuthCredentialStore;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let result = setup_result_with_profiles(vec![
            chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
        ]);
        let references =
            std::collections::BTreeSet::from(["chatgpt:a".to_string(), "chatgpt:b".to_string()]);
        let failing = DurableSetupAuthenticator {
            root: root.clone(),
            fail_reference: Some("chatgpt:b".into()),
            invalid_final_reference: None,
            replace_before_compensation: None,
        };
        assert!(prepare_chatgpt_setup_config(
            &result,
            &references,
            &failing,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("named credential 'chatgpt:b' failed"));

        let reopened = crate::oauth::file_store::FileOAuthCredentialStore::new(root.clone());
        let first = reopened.load("chatgpt:a").unwrap().unwrap();
        assert!(first.revoked && !first.mutation_pending);
        assert!(first.access_token.is_empty());
        assert!(first.refresh_token.is_none() && first.id_token.is_none());
        assert!(reopened.load("chatgpt:b").unwrap().is_none());

        let resumed = DurableSetupAuthenticator {
            root: root.clone(),
            fail_reference: None,
            invalid_final_reference: None,
            replace_before_compensation: None,
        };
        let config = prepare_chatgpt_setup_config(
            &result,
            &references,
            &resumed,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.credentials().len(), 2);
        for reference in ["chatgpt:a", "chatgpt:b"] {
            let record = reopened.load(reference).unwrap().unwrap();
            assert!(!record.revoked && !record.mutation_pending);
            assert!(record.access_token.starts_with("secret-access-"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_compensation_generation_race_leaves_concurrent_replacement_untouched() {
        use crate::oauth::OAuthCredentialStore;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let result = setup_result_with_profiles(vec![
            chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:c", "account-c", "gpt-5.6-sol"),
        ]);
        let references = std::collections::BTreeSet::from([
            "chatgpt:a".to_string(),
            "chatgpt:b".to_string(),
            "chatgpt:c".to_string(),
        ]);
        let racing = DurableSetupAuthenticator {
            root: root.clone(),
            fail_reference: Some("chatgpt:c".into()),
            invalid_final_reference: None,
            replace_before_compensation: Some("chatgpt:b".into()),
        };
        let error = prepare_chatgpt_setup_config(
            &result,
            &references,
            &racing,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(error.contains("named credential 'chatgpt:c' failed"));
        assert!(error.contains("terminal second-account denial"));
        assert!(error.contains("compensation conflicts for chatgpt:b"));
        let reopened = crate::oauth::file_store::FileOAuthCredentialStore::new(root);
        let external = reopened.load("chatgpt:b").unwrap().unwrap();
        assert!(!external.revoked && !external.mutation_pending);
        assert_eq!(external.access_token, "external-replacement-sentinel");
        let owned = reopened.load("chatgpt:a").unwrap().unwrap();
        assert!(owned.revoked && !owned.mutation_pending);
        assert!(owned.access_token.is_empty());
        assert!(owned.refresh_token.is_none() && owned.id_token.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn setup_final_validation_preserves_cause_and_compensates_every_owned_generation() {
        use crate::oauth::OAuthCredentialStore;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("oauth");
        let result = setup_result_with_profiles(vec![
            chatgpt_setup_profile("chatgpt:a", "account-a", "gpt-5.6-sol"),
            chatgpt_setup_profile("chatgpt:b", "account-b", "gpt-5.6-sol"),
        ]);
        let references =
            std::collections::BTreeSet::from(["chatgpt:a".to_string(), "chatgpt:b".to_string()]);
        let authenticator = DurableSetupAuthenticator {
            root: root.clone(),
            fail_reference: None,
            invalid_final_reference: Some("chatgpt:b".into()),
            replace_before_compensation: None,
        };

        let error = prepare_chatgpt_setup_config(
            &result,
            &references,
            &authenticator,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("Signed ChatGPT credential does not match the setup provider graph"),
            "{error}"
        );
        assert!(
            error.contains("credential 'chatgpt:b' issuer mismatch"),
            "{error}"
        );
        assert!(
            error.contains("provider profile 'account-b' has incompatible credential 'chatgpt:b'"),
            "{error}"
        );
        assert!(!error.contains("compensation conflicts"), "{error}");
        assert!(!error.contains("secret-access"), "{error}");
        assert!(!error.contains("secret-refresh"), "{error}");

        let reopened = crate::oauth::file_store::FileOAuthCredentialStore::new(root);
        for reference in ["chatgpt:a", "chatgpt:b"] {
            let owned = reopened.load(reference).unwrap().unwrap();
            assert!(owned.revoked && !owned.mutation_pending);
            assert!(owned.access_token.is_empty());
            assert!(owned.refresh_token.is_none() && owned.id_token.is_none());
        }
    }
}
