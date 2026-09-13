//! The wizard's own state: which section is open and what each section holds.
//!
//! `WizardSection`, `SectionState`, `WizardState` and the `SetupResult` the wizard produces.
//! These types describe the wizard, not the configuration it writes.

use super::*;

/// Main sections of the tabbed wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum WizardSection {
    Themes,
    Models,
    Personas,
    Features,
    Review,
}

impl WizardSection {
    pub(super) fn all() -> Vec<Self> {
        vec![
            Self::Themes,
            Self::Models,
            Self::Personas,
            Self::Features,
            Self::Review,
        ]
    }

    pub(super) fn name(&self) -> &str {
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
pub(super) enum SectionState {
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
pub(super) enum GuiSettingsFeedback {
    OpenRequested,
    Suppressed,
    Failed(String),
}

#[cfg(target_os = "macos")]
impl GuiSettingsFeedback {
    pub(super) fn compact_message(&self) -> &str {
        match self {
            Self::OpenRequested => "Open requested; R re-checks.",
            Self::Suppressed => "Not opened (SSH/headless).",
            Self::Failed(_) => "Open failed; D has the error.",
        }
    }

    pub(super) fn full_message(&self) -> String {
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
pub(super) struct PersonaInfo {
    pub(super) slug: String, // Key used to load the persona (e.g. "expert-coder")
    pub(super) name: String, // Display name (e.g. "Expert Coder")
    pub(super) description: String,
    pub(super) system_prompt: String,
}

/// Overall wizard state with tabbed navigation
pub(super) struct WizardState {
    pub(super) current_section: WizardSection,
    pub(super) sections: HashMap<WizardSection, SectionState>,
    pub(super) completed: HashSet<WizardSection>,
    pub(super) confirming_cancel: bool,
    pub(super) catalog_cache_dir: Option<std::path::PathBuf>,
    /// Typed CoreML policy provenance from the loaded configuration.
    pub(super) coreml: CoreMlConfig,
    /// Named credential metadata is preserved unchanged by the compact model
    /// editor; it contains no secret material.
    pub(super) credentials: Vec<crate::config::ProviderCredential>,
}

impl WizardState {
    pub(super) fn new(existing_config: Option<&crate::config::Config>) -> Self {
        Self::new_with_catalog_cache_dir(existing_config, model_catalog::default_cache_dir().ok())
    }

    pub(super) fn new_with_catalog_cache_dir(
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

    pub(super) fn is_completed(&self, section: WizardSection) -> bool {
        self.completed.contains(&section)
    }

    pub(super) fn mark_completed(&mut self, section: WizardSection) {
        self.completed.insert(section);
    }

    pub(super) fn next_section(&mut self) {
        let all = WizardSection::all();
        if let Some(idx) = all.iter().position(|s| *s == self.current_section) {
            if idx < all.len() - 1 {
                self.current_section = all[idx + 1];
            }
        }
    }

    pub(super) fn prev_section(&mut self) {
        let all = WizardSection::all();
        if let Some(idx) = all.iter().position(|s| *s == self.current_section) {
            if idx > 0 {
                self.current_section = all[idx - 1];
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn scoped_permission_history(
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
