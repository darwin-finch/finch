// Setup Wizard - First-run configuration

use anyhow::Result;
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

use crate::config::{ExecutionTarget, FeaturesConfig, TeacherEntry};
use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use crate::models::compatibility;

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
            Self::Themes => "Themes",
            Self::Models => "Models",
            Self::Personas => "Personas",
            Self::Features => "Features",
            Self::Review => "Review",
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
        error: Option<String>,
    },
    Personas {
        available_personas: Vec<PersonaInfo>,
        selected_idx: usize,
        default_persona: String,
    },
    Features {
        auto_approve: bool,
        streaming: bool,
        debug: bool,
        #[cfg(target_os = "macos")]
        gui_automation: bool,
        daemon_only_mode: bool,
        mdns_discovery: bool,
        selected_idx: usize, // For arrow key navigation
    },
    Review,
}

#[derive(Debug, Clone)]
enum ModelConfig {
    Local {
        family: ModelFamily,
        size: ModelSize,
        execution: ExecutionTarget,
        enabled: bool,
    },
    Remote {
        provider: String,
        api_key: String,
        model: String,
        enabled: bool,
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

    fn display_name(&self) -> String {
        match self {
            Self::Local { family, size, .. } => {
                format!("Local {} {}", family.name(), model_size_display(size))
            }
            Self::Remote { provider, model, .. } => {
                if !model.is_empty() {
                    format!("{} - {}", provider, model)
                } else {
                    provider.clone()
                }
            }
        }
    }

    fn is_configured(&self) -> bool {
        match self {
            Self::Local { .. } => true, // Local models are always "configured"
            Self::Remote { api_key, .. } => !api_key.is_empty(),
        }
    }
}

#[derive(Debug, Clone)]
struct PersonaInfo {
    name: String,
    description: String,
    system_prompt: String,
    builtin: bool,
}

/// Overall wizard state with tabbed navigation
struct WizardState {
    current_section: WizardSection,
    sections: HashMap<WizardSection, SectionState>,
    completed: HashSet<WizardSection>,
    // Cached data for rendering
    inference_providers: Vec<InferenceProvider>,
    execution_targets: Vec<ExecutionTarget>,
    model_families: Vec<ModelFamily>,
    model_sizes: Vec<ModelSize>,
}

impl WizardState {
    fn new(existing_config: Option<&crate::config::Config>) -> Self {
        use crate::config::ColorTheme;
        use crate::config::persona::Persona;

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

        // Models section - unified Backend + Teachers
        let primary_model = if let Some(config) = existing_config {
            if config.backend.enabled {
                // Local model is primary
                ModelConfig::Local {
                    family: config.backend.model_family,
                    size: config.backend.model_size,
                    execution: config.backend.execution_target,
                    enabled: true,
                }
            } else if let Some(teacher) = config.active_teacher() {
                // Remote API is primary
                ModelConfig::Remote {
                    provider: teacher.provider.clone(),
                    api_key: teacher.api_key.clone(),
                    model: teacher.model.clone().unwrap_or_default(),
                    enabled: true,
                }
            } else {
                // Default: remote Claude
                ModelConfig::Remote {
                    provider: "claude".to_string(),
                    api_key: String::new(),
                    model: String::new(),
                    enabled: true,
                }
            }
        } else {
            // Default: remote Claude
            ModelConfig::Remote {
                provider: "claude".to_string(),
                api_key: String::new(),
                model: String::new(),
                enabled: true,
            }
        };

        let tool_models: Vec<ModelConfig> = existing_config
            .map(|c| {
                c.teachers
                    .iter()
                    .skip(1) // Skip first teacher (that's the primary)
                    .map(|t| ModelConfig::Remote {
                        provider: t.provider.clone(),
                        api_key: t.api_key.clone(),
                        model: t.model.clone().unwrap_or_default(),
                        enabled: true,
                    })
                    .collect()
            })
            .unwrap_or_default();

        sections.insert(
            WizardSection::Models,
            SectionState::Models {
                primary_model,
                tool_models,
                selected_idx: 0,
                editing_mode: false,
                error: None,
            },
        );

        // Personas section
        let builtin_personas: Vec<PersonaInfo> = Persona::list_builtins()
            .iter()
            .filter_map(|name| {
                Persona::load_builtin(name).ok().map(|p| PersonaInfo {
                    name: p.name().to_string(),
                    description: p.persona.description.clone(),
                    system_prompt: p.behavior.system_prompt.clone(),
                    builtin: true,
                })
            })
            .collect();

        let default_persona = existing_config
            .map(|c| c.active_persona.clone())
            .unwrap_or_else(|| "default".to_string());

        let selected_idx = builtin_personas
            .iter()
            .position(|p| p.name.to_lowercase() == default_persona.to_lowercase())
            .unwrap_or(0);

        sections.insert(
            WizardSection::Personas,
            SectionState::Personas {
                available_personas: builtin_personas,
                selected_idx,
                default_persona,
            },
        );

        // Features section (simplified, no memory toggle, accessibility moved here)
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
                #[cfg(target_os = "macos")]
                gui_automation: existing_config
                    .map(|c| c.features.gui_automation)
                    .unwrap_or(false),
                daemon_only_mode: existing_config
                    .map(|c| c.server.mode == "daemon-only")
                    .unwrap_or(false),
                mdns_discovery: existing_config
                    .map(|c| c.server.advertise || c.client.auto_discover)
                    .unwrap_or(false),
                selected_idx: 0,
            },
        );

        // Review section
        sections.insert(WizardSection::Review, SectionState::Review);

        // Cached data (kept for compatibility, though Models section doesn't use these the same way)
        let inference_providers = vec![InferenceProvider::Onnx];
        let execution_targets = ExecutionTarget::available_targets();
        let all_model_families = vec![
            ModelFamily::Qwen2,
            ModelFamily::Gemma2,
            ModelFamily::Llama3,
            ModelFamily::Mistral,
            ModelFamily::Phi,
            ModelFamily::DeepSeek,
        ];
        let model_sizes = vec![
            ModelSize::Small,
            ModelSize::Medium,
            ModelSize::Large,
            ModelSize::XLarge,
        ];

        Self {
            current_section: WizardSection::Themes,
            sections,
            completed: HashSet::new(),
            inference_providers,
            execution_targets,
            model_families: all_model_families,
            model_sizes,
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

/// Check if a model family is compatible with an execution target
///
/// Uses the compatibility matrix for single source of truth
fn is_model_available(family: ModelFamily, target: ExecutionTarget) -> bool {
    compatibility::is_compatible(family, target)
}

/// Get error message for incompatible model/target combination
///
/// NOTE: With ONNX Runtime, all models support all execution targets.
/// This function is kept for future edge cases but should rarely trigger.
fn get_compatibility_error(family: ModelFamily, target: ExecutionTarget) -> String {
    format!(
        "⚠️  {} models are not available for {} execution target.\n\n\
         Please select a different target or model family.\n\n\
         Press 't' to change target, or 'b' to change model family.",
        family.name(),
        target.name()
    )
}

/// Setup wizard result containing all collected configuration
pub struct SetupResult {
    // Theme
    pub active_theme: String,

    // Models (primary + tools)
    pub primary_model: ModelConfig,
    pub tool_models: Vec<ModelConfig>,

    // Backward compatibility fields (mapped from primary_model)
    pub claude_api_key: String,
    pub hf_token: Option<String>,
    pub backend_enabled: bool,
    pub inference_provider: InferenceProvider,
    pub execution_target: ExecutionTarget,
    pub model_family: ModelFamily,
    pub model_size: ModelSize,
    pub custom_model_repo: Option<String>,
    pub teachers: Vec<TeacherEntry>,

    // Persona
    pub default_persona: String,

    // Feature flags
    pub auto_approve_tools: bool,
    pub streaming_enabled: bool,
    pub debug_logging: bool,
    #[cfg(target_os = "macos")]
    pub gui_automation: bool,
    pub daemon_only_mode: bool,
    pub mdns_discovery: bool,
}

impl SetupResult {
    /// Legacy field accessor for backward compatibility
    #[deprecated(note = "Use execution_target instead")]
    pub fn backend_device(&self) -> ExecutionTarget {
        self.execution_target
    }
}

enum WizardStep {
    Welcome,
    ClaudeApiKey(String),
    HfToken(String),
    EnableLocalModel(bool), // Ask if user wants local model (true = yes, false = proxy-only)
    InferenceProviderSelection(usize), // Select inference provider (ONNX/Candle)
    ExecutionTargetSelection(usize), // Select hardware target (CoreML/CPU/CUDA)
    ModelFamilySelection(usize),
    ModelSizeSelection(usize),
    IncompatibleCombination(String), // Error message for incompatible target/family
    ModelPreview, // Show resolved model info before proceeding
    CustomModelRepo(String, ExecutionTarget), // (repo input, selected target)
    TeacherConfig(Vec<TeacherEntry>, usize), // (teachers list, selected index)
    AddTeacherProviderSelection(Vec<TeacherEntry>, usize), // (existing teachers, selected provider idx)
    AddTeacherApiKey(Vec<TeacherEntry>, String, String), // (existing teachers, provider, api_key input)
    AddTeacherModel(Vec<TeacherEntry>, String, String, String), // (existing teachers, provider, api_key, model input)
    EditTeacher(Vec<TeacherEntry>, usize, String, String), // (teachers, teacher_idx, model_input, name_input)
    FeaturesConfig(bool, bool, bool), // (auto_approve_tools, streaming_enabled, debug_logging)
    Confirm,
}

/// Show first-run setup wizard and return configuration
pub fn show_setup_wizard() -> Result<SetupResult> {
    // Try to load existing config to pre-fill values
    let existing_config = match crate::config::load_config() {
        Ok(config) => {
            let debug_msg = format!("Successfully loaded existing config with {} teachers\n", config.teachers.len());
            if let Some(teacher) = config.active_teacher() {
                let debug_msg = format!("{}Active teacher: provider={}, key_len={}\n",
                    debug_msg, teacher.provider, teacher.api_key.len());
                let _ = std::fs::write("/tmp/wizard_debug.log", debug_msg);
            }
            tracing::debug!("Successfully loaded existing config with {} teachers", config.teachers.len());
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

        // Handle input
        if let Event::Key(key) = event::read()? {
            // Global navigation (works in any section)
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                anyhow::bail!("Setup cancelled");
            }

            match key.code {
                // Tab navigation
                KeyCode::Tab => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        state.prev_section();
                    } else {
                        state.next_section();
                    }
                }
                KeyCode::Left => state.prev_section(),
                KeyCode::Right => state.next_section(),

                // Section-specific handling
                _ => {
                    let should_exit = handle_section_input(&mut state, key)?;
                    if should_exit {
                        // User confirmed in Review section - build result
                        return build_setup_result(&state);
                    }
                }
            }
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
                anyhow::bail!("Setup cancelled");
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Models section (unified Backend + Teachers)
fn handle_models_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Models {
        primary_model,
        tool_models,
        selected_idx,
        editing_mode,
        error,
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Clear error on any input
        *error = None;

        if *editing_mode {
            // In editing mode - simple text input for API keys
            match key.code {
                KeyCode::Char(c) => {
                    if *selected_idx == 0 {
                        // Editing primary model
                        if let ModelConfig::Remote { api_key, .. } = primary_model {
                            api_key.push(c);
                        }
                    } else {
                        // Editing tool model
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
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    // Enter edit mode
                    *editing_mode = true;
                }
                KeyCode::Enter => {
                    // Validate and proceed
                    if !primary_model.is_configured() {
                        *error = Some("Primary model must be configured with an API key".to_string());
                        return Ok(false);
                    }

                    // Validate API key format
                    if let ModelConfig::Remote { provider, api_key, .. } = primary_model {
                        if !api_key.starts_with("sk-") && provider == "claude" {
                            *error = Some("Claude API key must start with 'sk-ant-'".to_string());
                            return Ok(false);
                        }
                    }

                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Esc => {
                    anyhow::bail!("Setup cancelled");
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
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
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
            KeyCode::Enter => {
                // Set selected persona as default
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *default_persona = persona.name.clone();
                }
                state.mark_completed(WizardSection::Personas);
                state.next_section();
            }
            KeyCode::Esc => {
                anyhow::bail!("Setup cancelled");
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Features section (with arrow key navigation)
fn handle_features_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        selected_idx,
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        // Calculate number of features (platform-dependent)
        #[cfg(target_os = "macos")]
        let num_features = 6;
        #[cfg(not(target_os = "macos"))]
        let num_features = 5;

        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < num_features - 1 {
                    *selected_idx += 1;
                }
            }
            KeyCode::Char(' ') => {
                // Toggle selected feature
                match *selected_idx {
                    0 => *auto_approve = !*auto_approve,
                    1 => *streaming = !*streaming,
                    2 => *debug = !*debug,
                    #[cfg(target_os = "macos")]
                    3 => *gui_automation = !*gui_automation,
                    #[cfg(target_os = "macos")]
                    4 => *daemon_only_mode = !*daemon_only_mode,
                    #[cfg(target_os = "macos")]
                    5 => *mdns_discovery = !*mdns_discovery,
                    #[cfg(not(target_os = "macos"))]
                    3 => *daemon_only_mode = !*daemon_only_mode,
                    #[cfg(not(target_os = "macos"))]
                    4 => *mdns_discovery = !*mdns_discovery,
                    _ => {}
                }
            }
            KeyCode::Char(c @ '1'..='6') => {
                // Number keys also work (backwards compatibility)
                let idx = (c as u8 - b'1') as usize;
                if idx < num_features {
                    *selected_idx = idx;
                    // Auto-toggle
                    match idx {
                        0 => *auto_approve = !*auto_approve,
                        1 => *streaming = !*streaming,
                        2 => *debug = !*debug,
                        #[cfg(target_os = "macos")]
                        3 => *gui_automation = !*gui_automation,
                        #[cfg(target_os = "macos")]
                        4 => *daemon_only_mode = !*daemon_only_mode,
                        #[cfg(target_os = "macos")]
                        5 => *mdns_discovery = !*mdns_discovery,
                        #[cfg(not(target_os = "macos"))]
                        3 => *daemon_only_mode = !*daemon_only_mode,
                        #[cfg(not(target_os = "macos"))]
                        4 => *mdns_discovery = !*mdns_discovery,
                        _ => {}
                    }
                }
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Features);
                state.next_section();
            }
            KeyCode::Esc => {
                anyhow::bail!("Setup cancelled");
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Review section
fn handle_review_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
            // Confirm and exit
            Ok(true)
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            anyhow::bail!("Setup cancelled");
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
    let default_persona = if let Some(SectionState::Personas { default_persona, .. }) =
        state.sections.get(&WizardSection::Personas)
    {
        default_persona.clone()
    } else {
        "default".to_string()
    };

    // Extract features
    let (auto_approve, streaming, debug, daemon_only, mdns) = if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        daemon_only_mode,
        mdns_discovery,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (*auto_approve, *streaming, *debug, *daemon_only_mode, *mdns_discovery)
    } else {
        (false, true, false, false, false)
    };

    #[cfg(target_os = "macos")]
    let gui_automation = if let Some(SectionState::Features { gui_automation, .. }) =
        state.sections.get(&WizardSection::Features)
    {
        *gui_automation
    } else {
        false
    };

    // Map to backward-compatible fields
    let (claude_api_key, backend_enabled, inference_provider, execution_target, model_family, model_size) =
        match &primary_model {
            ModelConfig::Local { family, size, execution, .. } => (
                String::new(), // No API key for local
                true,
                InferenceProvider::Onnx,
                *execution,
                *family,
                *size,
            ),
            ModelConfig::Remote { provider, api_key, .. } => {
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
    if let ModelConfig::Remote { provider, api_key, model, .. } = &primary_model {
        teachers.push(TeacherEntry {
            provider: provider.clone(),
            api_key: api_key.clone(),
            model: if model.is_empty() { None } else { Some(model.clone()) },
            base_url: None,
            name: Some(format!("{} (Primary)", provider)),
        });
    }

    // Tool models as additional teachers
    for tool_model in &tool_models {
        if let ModelConfig::Remote { provider, api_key, model, enabled } = tool_model {
            if *enabled {
                teachers.push(TeacherEntry {
                    provider: provider.clone(),
                    api_key: api_key.clone(),
                    model: if model.is_empty() { None } else { Some(model.clone()) },
                    base_url: None,
                    name: Some(provider.clone()),
                });
            }
        }
    }

    Ok(SetupResult {
        active_theme,
        primary_model,
        tool_models,
        claude_api_key,
        hf_token: None, // TODO: Add HF token input to Models section if needed
        backend_enabled,
        inference_provider,
        execution_target,
        model_family,
        model_size,
        custom_model_repo: None,
        teachers,
        default_persona,
        auto_approve_tools: auto_approve,
        streaming_enabled: streaming,
        debug_logging: debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode: daemon_only,
        mdns_discovery: mdns,
    })
}

/// Render the tabbed wizard UI
fn render_tabbed_wizard(f: &mut Frame, state: &WizardState) {
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
        .block(Block::default().borders(Borders::ALL).title("Shammah Setup"))
        .select(selected_idx)
        .style(Style::default().fg(Color::Blue))
        .highlight_style(
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    // Render current section content
    render_section_content(f, chunks[1], state);

    // Render help text
    let help_text = match state.current_section {
        WizardSection::Themes => "Tab/Arrows: Navigate sections | ↑/↓: Select theme | Enter: Next | Esc: Cancel",
        WizardSection::Models => "Tab/Arrows: Navigate sections | ↑/↓: Navigate | E: Edit | Space: Toggle | Enter: Next",
        WizardSection::Personas => "Tab/Arrows: Navigate sections | ↑/↓: Select persona | Enter: Set as default",
        WizardSection::Features => "Tab/Arrows: Navigate sections | ↑/↓: Navigate | Space: Toggle | Enter: Next",
        WizardSection::Review => "Tab/Arrows: Navigate sections | Enter/y: Confirm | n/Esc: Cancel",
    };

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

/// Render the content area for the current section
fn render_section_content(f: &mut Frame, area: Rect, state: &WizardState) {
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
            error,
        }) => render_models_section(
            f,
            area,
            primary_model,
            tool_models,
            *selected_idx,
            *editing_mode,
            error.as_deref(),
        ),
        Some(SectionState::Personas {
            available_personas,
            selected_idx,
            default_persona,
        }) => render_personas_section(f, area, available_personas, *selected_idx, default_persona),
        Some(SectionState::Features {
            auto_approve,
            streaming,
            debug,
            #[cfg(target_os = "macos")]
            gui_automation,
            daemon_only_mode,
            mdns_discovery,
            selected_idx,
        }) => render_features_section(
            f,
            area,
            *auto_approve,
            *streaming,
            *debug,
            #[cfg(target_os = "macos")]
            *gui_automation,
            *daemon_only_mode,
            *mdns_discovery,
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
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Theme list
            Constraint::Length(8),  // Preview
            Constraint::Length(3),  // Instructions
        ])
        .split(area);

    let title = Paragraph::new("Theme Selection")
        .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Render theme options with colors visible on both light and dark backgrounds
    let themes = ColorTheme::all();
    let items: Vec<ListItem> = themes
        .iter()
        .enumerate()
        .map(|(i, theme)| {
            let marker = if i == selected_theme { "▶ " } else { "  " };
            let style = if i == selected_theme {
                Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Blue)
            };
            ListItem::new(format!("{}{} - {}", marker, theme.name(), theme.description()))
                .style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Available Themes"));
    f.render_widget(list, chunks[1]);

    // Render preview of selected theme
    let preview_theme = themes[selected_theme].to_scheme();
    let preview_lines = vec![
        Line::from(vec![
            Span::styled("User: ", Style::default().fg(preview_theme.messages.user.to_color())),
            Span::raw("What is 2+2?"),
        ]),
        Line::from(vec![
            Span::styled("Assistant: ", Style::default().fg(preview_theme.messages.assistant.to_color())),
            Span::raw("The answer is 4."),
        ]),
        Line::from(vec![
            Span::styled("🔧 Tool: ", Style::default().fg(preview_theme.messages.tool.to_color())),
            Span::raw("Reading file..."),
        ]),
        Line::from(vec![
            Span::styled("❌ Error: ", Style::default().fg(preview_theme.messages.error.to_color())),
            Span::raw("File not found"),
        ]),
    ];

    let preview = Paragraph::new(preview_lines)
        .block(Block::default().borders(Borders::ALL).title("Preview"))
        .wrap(Wrap { trim: false });
    f.render_widget(preview, chunks[2]);

    let instructions = Paragraph::new(
        "Select a color theme that works well with your terminal background.\n\
         Use ↑/↓ to navigate, Enter to confirm. If you can't see this text, try pressing ↓ then Enter."
    )
    .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[3]);
}

/// Render Models section (unified Backend + Teachers)
fn render_models_section(
    f: &mut Frame,
    area: Rect,
    primary_model: &ModelConfig,
    tool_models: &[ModelConfig],
    selected_idx: usize,
    editing_mode: bool,
    error: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Primary model + tool models
            Constraint::Length(3),  // Instructions
            Constraint::Length(2),  // Error (if present)
        ])
        .split(area);

    let title = Paragraph::new("Model Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Build list items: primary model + tool models
    let mut items = vec![];

    // Primary model
    let primary_marker = if selected_idx == 0 { "▶ " } else { "  " };
    let primary_style = if selected_idx == 0 {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Green)
    };

    let primary_display = match primary_model {
        ModelConfig::Local { family, size, execution, .. } => {
            format!(
                "{}★ PRIMARY: Local {} {} ({})",
                primary_marker,
                family.name(),
                model_size_display(size),
                execution.name()
            )
        }
        ModelConfig::Remote { provider, api_key, model, .. } => {
            let key_display = if api_key.is_empty() {
                "[Not configured]".to_string()
            } else if editing_mode && selected_idx == 0 {
                api_key.clone()
            } else {
                format!("{}...{}", &api_key.chars().take(10).collect::<String>(), api_key.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>())
            };
            let model_part = if !model.is_empty() {
                format!(" - {}", model)
            } else {
                String::new()
            };
            format!(
                "{}★ PRIMARY: {}{} [{}]",
                primary_marker, provider, model_part, key_display
            )
        }
    };

    items.push(ListItem::new(primary_display).style(primary_style));

    // Tool models
    for (idx, tool_model) in tool_models.iter().enumerate() {
        let tool_idx = idx + 1;
        let marker = if selected_idx == tool_idx { "▶ " } else { "  " };
        let checkbox = if tool_model.enabled() { "☑" } else { "☐" };
        let style = if selected_idx == tool_idx {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else if tool_model.enabled() {
            Style::default()
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let display = match tool_model {
            ModelConfig::Local { family, size, .. } => {
                format!(
                    "{}{} Tool: Local {} {}",
                    marker, checkbox, family.name(), model_size_display(size)
                )
            }
            ModelConfig::Remote { provider, model, .. } => {
                let model_part = if !model.is_empty() {
                    format!(" - {}", model)
                } else {
                    String::new()
                };
                format!(
                    "{}{} Tool: {}{}",
                    marker, checkbox, provider, model_part
                )
            }
        };

        items.push(ListItem::new(display).style(style));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Models (Primary + Tools)"),
        );
    f.render_widget(list, chunks[1]);

    // Instructions
    let instructions_text = if editing_mode {
        "Editing API key | Type to edit | Enter/Esc: Done"
    } else {
        "↑/↓: Navigate | E: Edit | Space: Toggle (tools) | Enter: Next"
    };
    let instructions = Paragraph::new(instructions_text)
        .style(Style::default().fg(Color::Yellow))
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[2]);

    // Error message (if present)
    if let Some(err) = error {
        let error_widget = Paragraph::new(err)
            .style(Style::default().fg(Color::Red))
            .alignment(Alignment::Center);
        f.render_widget(error_widget, chunks[3]);
    }
}

/// Render Personas section
fn render_personas_section(
    f: &mut Frame,
    area: Rect,
    personas: &[PersonaInfo],
    selected_idx: usize,
    default_persona: &str,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Left: Persona list
    let items: Vec<ListItem> = personas
        .iter()
        .enumerate()
        .map(|(i, persona)| {
            let is_default = persona.name.to_lowercase() == default_persona.to_lowercase();
            let marker = if is_default { "★ " } else { "  " };
            let style = if i == selected_idx {
                Style::default().bg(Color::Cyan).fg(Color::Black)
            } else if is_default {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            ListItem::new(format!("{}{}", marker, persona.name)).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Personas"))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    list_state.select(Some(selected_idx));
    f.render_stateful_widget(list, chunks[0], &mut list_state);

    // Right: Preview
    if let Some(persona) = personas.get(selected_idx) {
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
        ];

        let preview = Paragraph::new(preview_lines)
            .block(Block::default().borders(Borders::ALL).title("Preview"))
            .wrap(Wrap { trim: false });
        f.render_widget(preview, chunks[1]);
    }
}

/// Render Features section (simplified, with arrow key navigation)
fn render_features_section(
    f: &mut Frame,
    area: Rect,
    auto_approve: bool,
    streaming: bool,
    debug: bool,
    #[cfg(target_os = "macos")] gui_automation: bool,
    daemon_only_mode: bool,
    mdns_discovery: bool,
    selected_idx: usize,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Feature list
            Constraint::Length(3),  // Instructions
        ])
        .split(area);

    let title = Paragraph::new("Feature Flags")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Build feature list
    let mut features: Vec<(&str, bool, Option<&str>)> = vec![
        ("Auto-approve tools", auto_approve, Some("Skip confirmation dialogs (⚠️  use with caution)")),
        ("Streaming responses", streaming, Some("Enable real-time response streaming")),
        ("Debug logging", debug, Some("Enable detailed logs for troubleshooting")),
    ];

    #[cfg(target_os = "macos")]
    features.push((
        "GUI automation (macOS)",
        gui_automation,
        Some("Enable GuiClick, GuiType, GuiInspect tools"),
    ));

    features.push((
        "Daemon-only mode",
        daemon_only_mode,
        Some("Run as daemon without REPL (server mode)"),
    ));

    features.push((
        "mDNS service discovery",
        mdns_discovery,
        Some("Advertise and discover remote daemons via Bonjour"),
    ));

    let items: Vec<ListItem> = features
        .iter()
        .enumerate()
        .map(|(i, (name, enabled, desc))| {
            let checkbox = if *enabled { "☑" } else { "☐" };
            let marker = if i == selected_idx { "▶ " } else { "  " };
            let style = if i == selected_idx {
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
            } else if *enabled {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Gray)
            };

            let mut lines = vec![Line::from(vec![
                Span::raw(marker),
                Span::styled(format!("{} {}", checkbox, name), style),
            ])];

            if let Some(description) = desc {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(*description, Style::default().fg(Color::DarkGray)),
                ]));
            }

            ListItem::new(lines)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Features"));
    f.render_widget(list, chunks[1]);

    let instructions = Paragraph::new(
        "↑/↓: Navigate | Space: Toggle | 1-6: Quick toggle | Enter: Next"
    )
    .style(Style::default().fg(Color::Yellow))
    .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[2]);
}

/// Render Review section
fn render_review_section(f: &mut Frame, area: Rect, state: &WizardState) {
    use crate::config::ColorTheme;

    let title = Paragraph::new("Review Configuration")
        .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);

    // Build summary text
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Configuration Summary:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
    ];

    // Theme
    if let Some(SectionState::Themes { selected_theme }) = state.sections.get(&WizardSection::Themes) {
        let themes = ColorTheme::all();
        let theme_name = themes[*selected_theme].name().to_string();
        lines.push(Line::from(vec![
            Span::styled("Theme: ", Style::default().fg(Color::Yellow)),
            Span::raw(theme_name),
        ]));
    }

    // Models
    if let Some(SectionState::Models { primary_model, tool_models, .. }) = state.sections.get(&WizardSection::Models) {
        lines.push(Line::from(vec![
            Span::styled("Primary Model: ", Style::default().fg(Color::Yellow)),
            Span::raw(primary_model.display_name()),
        ]));
        if !tool_models.is_empty() {
            let enabled_tools: Vec<_> = tool_models
                .iter()
                .filter(|m| m.enabled())
                .map(|m| m.display_name())
                .collect();
            if !enabled_tools.is_empty() {
                lines.push(Line::from(vec![
                    Span::styled("Tool Models: ", Style::default().fg(Color::Yellow)),
                    Span::raw(enabled_tools.join(", ")),
                ]));
            }
        }
    }

    // Persona
    if let Some(SectionState::Personas { default_persona, .. }) = state.sections.get(&WizardSection::Personas) {
        lines.push(Line::from(vec![
            Span::styled("Default Persona: ", Style::default().fg(Color::Yellow)),
            Span::raw(default_persona),
        ]));
    }

    // Features
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        let mut feature_flags = vec![];
        if *auto_approve {
            feature_flags.push("Auto-approve");
        }
        if *streaming {
            feature_flags.push("Streaming");
        }
        if *debug {
            feature_flags.push("Debug");
        }
        #[cfg(target_os = "macos")]
        if *gui_automation {
            feature_flags.push("GUI automation");
        }
        if *daemon_only_mode {
            feature_flags.push("Daemon-only");
        }
        if *mdns_discovery {
            feature_flags.push("mDNS discovery");
        }

        lines.push(Line::from(vec![
            Span::styled("Features: ", Style::default().fg(Color::Yellow)),
            Span::raw(if feature_flags.is_empty() {
                "None enabled".to_string()
            } else {
                feature_flags.join(", ")
            }),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("Press Enter or 'y' to save and continue", Style::default().fg(Color::Green)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Press 'n' or Esc to cancel", Style::default().fg(Color::Red)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Use Tab/Arrows to go back and edit any section", Style::default().fg(Color::Gray)),
    ]));

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

/// Run the wizard interaction loop (OLD - kept for reference, will be removed)
fn run_wizard_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>,
    existing_config: Option<&crate::config::Config>,
) -> Result<SetupResult> {
    // Pre-fill from existing config if available
    let mut claude_key = existing_config
        .and_then(|c| {
            let msg = format!("Loading from existing config, teachers: {}\n", c.teachers.len());
            let _ = std::fs::OpenOptions::new().append(true).create(true).open("/tmp/wizard_debug.log")
                .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
            tracing::debug!("Loading from existing config");
            c.active_teacher()
        })
        .map(|t| {
            let msg = format!("Found active teacher: provider={}, key_len={}\n", t.provider, t.api_key.len());
            let _ = std::fs::OpenOptions::new().append(true).create(true).open("/tmp/wizard_debug.log")
                .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
            tracing::debug!("Found active teacher: provider={}, key_len={}", t.provider, t.api_key.len());
            t.api_key.clone()
        })
        .unwrap_or_else(|| {
            let msg = "No existing config or teacher found, starting with empty key\n";
            let _ = std::fs::OpenOptions::new().append(true).create(true).open("/tmp/wizard_debug.log")
                .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
            tracing::debug!("No existing config or teacher found, starting with empty key");
            String::new()
        });

    let mut hf_token = String::new(); // TODO: Add HF token to config

    // Inference providers available
    let inference_providers = vec![
        InferenceProvider::Onnx,
        #[cfg(feature = "candle")]
        InferenceProvider::Candle,
    ];
    let mut selected_provider_idx = existing_config
        .map(|c| {
            inference_providers
                .iter()
                .position(|p| *p == c.backend.inference_provider)
                .unwrap_or(0)
        })
        .unwrap_or(0);

    let execution_targets = ExecutionTarget::available_targets();
    let mut selected_target_idx = existing_config
        .map(|c| {
            execution_targets
                .iter()
                .position(|t| *t == c.backend.execution_target)
                .unwrap_or(0)
        })
        .unwrap_or(0);

    // Model families will be filtered based on selected provider + target
    // Start with all families, will be filtered dynamically
    let all_model_families = vec![
        ModelFamily::Qwen2,
        ModelFamily::Gemma2,
        ModelFamily::Llama3,
        ModelFamily::Mistral,
        ModelFamily::Phi,
        ModelFamily::DeepSeek,
    ];
    let mut model_families = all_model_families.clone();
    let mut selected_family_idx = existing_config
        .map(|c| {
            model_families
                .iter()
                .position(|f| *f == c.backend.model_family)
                .unwrap_or(0)
        })
        .unwrap_or(0);

    let model_sizes = vec![
        ModelSize::Small,
        ModelSize::Medium,
        ModelSize::Large,
        ModelSize::XLarge,
    ];
    let mut selected_size_idx = existing_config
        .map(|c| {
            model_sizes
                .iter()
                .position(|s| *s == c.backend.model_size)
                .unwrap_or(1)
        })
        .unwrap_or(1); // Default to Medium

    let mut teachers: Vec<TeacherEntry> = existing_config
        .map(|c| c.teachers.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| {
            vec![TeacherEntry {
                provider: "claude".to_string(),
                api_key: String::new(), // Will be filled from claude_key
                model: None,
                base_url: None,
                name: Some("Claude (Primary)".to_string()),
            }]
        });

    let mut selected_teacher_idx = 0;

    let mut custom_model_repo = existing_config
        .and_then(|c| c.backend.model_repo.clone())
        .unwrap_or_default();

    // Track whether user wants local model enabled
    let mut backend_enabled = existing_config
        .map(|c| c.backend.enabled)
        .unwrap_or(true); // Default to enabled

    // Feature flags
    let mut auto_approve_tools = existing_config
        .as_ref()
        .map(|c| c.features.auto_approve_tools)
        .unwrap_or(false); // Default to false (safe)
    let mut streaming_enabled = existing_config
        .as_ref()
        .map(|c| c.features.streaming_enabled)
        .unwrap_or(true); // Default to true (better UX)
    let mut debug_logging = existing_config
        .as_ref()
        .map(|c| c.features.debug_logging)
        .unwrap_or(false); // Default to false

    // Wizard state - start at Welcome
    let mut step = WizardStep::Welcome;

    loop {
        terminal.draw(|f| {
            render_wizard_step(
                f,
                &step,
                &inference_providers,
                &execution_targets,
                &model_families,
                &model_sizes,
                &custom_model_repo,
                selected_provider_idx,
                selected_target_idx,
                selected_family_idx,
                selected_size_idx,
            );
        })?;

        // Handle input
        if let Event::Key(key) = event::read()? {
            match &mut step {
                WizardStep::Welcome => {
                    if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                        step = WizardStep::ClaudeApiKey(claude_key.clone());
                    } else if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                        anyhow::bail!("Setup cancelled");
                    }
                }

                WizardStep::ClaudeApiKey(input) => {
                    match key.code {
                        KeyCode::Char(c) => {
                            input.push(c);
                            claude_key = input.clone();
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            claude_key = input.clone();
                        }
                        KeyCode::Enter => {
                            if !input.is_empty() {
                                step = WizardStep::HfToken(hf_token.clone());
                            }
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::HfToken(input) => {
                    match key.code {
                        KeyCode::Char(c) => {
                            input.push(c);
                            hf_token = input.clone();
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            hf_token = input.clone();
                        }
                        KeyCode::Enter => {
                            // Continue even if empty (optional)
                            step = WizardStep::EnableLocalModel(true); // Default to yes
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::EnableLocalModel(enable) => {
                    match key.code {
                        KeyCode::Up | KeyCode::Down => {
                            // Toggle between yes/no
                            *enable = !*enable;
                        }
                        KeyCode::Enter => {
                            backend_enabled = *enable; // Save user's choice
                            if *enable {
                                // User wants local model - continue to provider selection
                                step = WizardStep::InferenceProviderSelection(selected_provider_idx);
                            } else {
                                // User wants proxy-only - skip to teacher config
                                teachers[0].api_key = claude_key.clone();
                                step = WizardStep::TeacherConfig(teachers.clone(), selected_teacher_idx);
                            }
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::InferenceProviderSelection(selected) => {
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                                selected_provider_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < inference_providers.len() - 1 {
                                *selected += 1;
                                selected_provider_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            // Proceed to execution target selection
                            step = WizardStep::ExecutionTargetSelection(selected_target_idx);
                        }
                        KeyCode::Esc => {
                            // Go back to enable local model
                            step = WizardStep::EnableLocalModel(backend_enabled);
                        }
                        _ => {}
                    }
                }

                WizardStep::ExecutionTargetSelection(selected) => {
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                                selected_target_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < execution_targets.len() - 1 {
                                *selected += 1;
                                selected_target_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            // Filter model families based on selected provider + target
                            use crate::models::compatibility::get_compatible_families_for_provider;
                            let selected_provider = inference_providers[selected_provider_idx];
                            let selected_target = execution_targets[selected_target_idx];
                            model_families = get_compatible_families_for_provider(selected_provider, selected_target);

                            if model_families.is_empty() {
                                // No compatible models for this combination
                                let error_msg = format!(
                                    "No models available for {} on {}",
                                    selected_provider.name(),
                                    selected_target.name()
                                );
                                step = WizardStep::IncompatibleCombination(error_msg);
                            } else {
                                // Reset family selection to first compatible model
                                selected_family_idx = 0;
                                step = WizardStep::ModelFamilySelection(selected_family_idx);
                            }
                        }
                        KeyCode::Esc => {
                            // Go back to provider selection
                            step = WizardStep::InferenceProviderSelection(selected_provider_idx);
                        }
                        _ => {}
                    }
                }

                WizardStep::ModelFamilySelection(selected) => {
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                                selected_family_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < model_families.len() - 1 {
                                *selected += 1;
                                selected_family_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            step = WizardStep::ModelSizeSelection(selected_size_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::ModelSizeSelection(selected) => {
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                                selected_size_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < model_sizes.len() - 1 {
                                *selected += 1;
                                selected_size_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            // Check if selected target + model family is compatible
                            let selected_target = execution_targets[selected_target_idx];
                            let selected_family = model_families[selected_family_idx];

                            if !is_model_available(selected_family, selected_target) {
                                // Show error and go back to family selection
                                let error_msg = get_compatibility_error(selected_family, selected_target);
                                step = WizardStep::IncompatibleCombination(error_msg);
                            } else {
                                step = WizardStep::ModelPreview;
                            }
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::IncompatibleCombination(_error_msg) => {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('b') => {
                            // Go back to model family selection to choose a compatible family
                            step = WizardStep::ModelFamilySelection(selected_family_idx);
                        }
                        KeyCode::Char('t') => {
                            // Go back to execution target selection to choose a compatible target
                            step = WizardStep::ExecutionTargetSelection(selected_target_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::ModelPreview => {
                    match key.code {
                        KeyCode::Enter | KeyCode::Char('y') => {
                            // User confirmed - proceed to custom model repo input
                            step = WizardStep::CustomModelRepo(
                                custom_model_repo.clone(),
                                execution_targets[selected_target_idx]
                            );
                        }
                        KeyCode::Char('b') | KeyCode::Backspace => {
                            // Go back to model size selection
                            step = WizardStep::ModelSizeSelection(selected_size_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::CustomModelRepo(input, selected_device) => {
                    match key.code {
                        KeyCode::Char(c) => {
                            input.push(c);
                            custom_model_repo = input.clone();
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            custom_model_repo = input.clone();
                        }
                        KeyCode::Enter => {
                            // Continue even if empty (optional)
                            // Fill teacher's API key from claude_key
                            teachers[0].api_key = claude_key.clone();
                            step = WizardStep::TeacherConfig(teachers.clone(), selected_teacher_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::TeacherConfig(teacher_list, selected) => {
                    match key.code {
                        KeyCode::Up => {
                            // Shift+Up or Ctrl+Up: Move teacher up (increase priority)
                            if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) ||
                               key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                                if *selected > 0 {
                                    let mut new_teachers = teacher_list.clone();
                                    new_teachers.swap(*selected, *selected - 1);
                                    step = WizardStep::TeacherConfig(new_teachers, *selected - 1);
                                }
                            } else {
                                // Normal Up: Navigate selection
                                if *selected > 0 {
                                    *selected -= 1;
                                    selected_teacher_idx = *selected;
                                }
                            }
                        }
                        KeyCode::Down => {
                            // Shift+Down or Ctrl+Down: Move teacher down (decrease priority)
                            if key.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) ||
                               key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) {
                                if *selected < teacher_list.len() - 1 {
                                    let mut new_teachers = teacher_list.clone();
                                    new_teachers.swap(*selected, *selected + 1);
                                    step = WizardStep::TeacherConfig(new_teachers, *selected + 1);
                                }
                            } else {
                                // Normal Down: Navigate selection
                                if *selected < teacher_list.len() - 1 {
                                    *selected += 1;
                                    selected_teacher_idx = *selected;
                                }
                            }
                        }
                        KeyCode::Enter => {
                            teachers = teacher_list.clone();
                            step = WizardStep::FeaturesConfig(auto_approve_tools, streaming_enabled, debug_logging);
                        }
                        KeyCode::Char('a') => {
                            // Add new teacher - go to provider selection
                            step = WizardStep::AddTeacherProviderSelection(teacher_list.clone(), 0);
                        }
                        KeyCode::Char('e') => {
                            // Edit selected teacher
                            if *selected < teacher_list.len() {
                                let teacher = &teacher_list[*selected];
                                let model_input = teacher.model.clone().unwrap_or_default();
                                let name_input = teacher.name.clone().unwrap_or_default();
                                step = WizardStep::EditTeacher(
                                    teacher_list.clone(),
                                    *selected,
                                    model_input,
                                    name_input,
                                );
                            }
                        }
                        KeyCode::Char('d') | KeyCode::Char('r') => {
                            // Delete/Remove selected teacher (if not the only one)
                            if teacher_list.len() > 1 && *selected < teacher_list.len() {
                                let mut new_teachers = teacher_list.clone();
                                new_teachers.remove(*selected);
                                let new_selected = if *selected >= new_teachers.len() {
                                    new_teachers.len().saturating_sub(1)
                                } else {
                                    *selected
                                };
                                step = WizardStep::TeacherConfig(new_teachers, new_selected);
                            }
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::AddTeacherProviderSelection(teacher_list, selected) => {
                    let providers = vec!["claude", "openai", "gemini", "grok", "mistral", "groq"];
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < providers.len() - 1 {
                                *selected += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let provider = providers[*selected].to_string();
                            step = WizardStep::AddTeacherApiKey(teacher_list.clone(), provider, String::new());
                        }
                        KeyCode::Esc => {
                            // Go back to teacher config
                            step = WizardStep::TeacherConfig(teacher_list.clone(), 0);
                        }
                        _ => {}
                    }
                }

                WizardStep::AddTeacherApiKey(teacher_list, provider, api_key_input) => {
                    match key.code {
                        KeyCode::Enter => {
                            if !api_key_input.is_empty() {
                                // Go to model name input (optional)
                                step = WizardStep::AddTeacherModel(teacher_list.clone(), provider.clone(), api_key_input.clone(), String::new());
                            }
                        }
                        KeyCode::Backspace => {
                            api_key_input.pop();
                        }
                        KeyCode::Char(c) => {
                            api_key_input.push(c);
                        }
                        KeyCode::Esc => {
                            // Go back to provider selection
                            step = WizardStep::AddTeacherProviderSelection(teacher_list.clone(), 0);
                        }
                        _ => {}
                    }
                }

                WizardStep::AddTeacherModel(teacher_list, provider, api_key, model_input) => {
                    match key.code {
                        KeyCode::Enter => {
                            // Create new teacher and add to list
                            let mut new_teachers = teacher_list.clone();
                            new_teachers.push(TeacherEntry {
                                provider: provider.clone(),
                                api_key: api_key.clone(),
                                model: if model_input.is_empty() { None } else { Some(model_input.clone()) },
                                base_url: None,
                                name: None,
                            });
                            step = WizardStep::TeacherConfig(new_teachers, teacher_list.len());
                        }
                        KeyCode::Backspace => {
                            model_input.pop();
                        }
                        KeyCode::Char(c) => {
                            model_input.push(c);
                        }
                        KeyCode::Esc => {
                            // Skip model input and add teacher anyway
                            let mut new_teachers = teacher_list.clone();
                            new_teachers.push(TeacherEntry {
                                provider: provider.clone(),
                                api_key: api_key.clone(),
                                model: None,
                                base_url: None,
                                name: None,
                            });
                            step = WizardStep::TeacherConfig(new_teachers, teacher_list.len());
                        }
                        _ => {}
                    }
                }

                WizardStep::EditTeacher(teacher_list, teacher_idx, model_input, name_input) => {
                    match key.code {
                        KeyCode::Tab => {
                            // Tab to switch between model and name fields
                            // For now, we'll use Enter to save
                        }
                        KeyCode::Enter => {
                            // Save edited teacher
                            let mut new_teachers = teacher_list.clone();
                            if *teacher_idx < new_teachers.len() {
                                new_teachers[*teacher_idx].model = if model_input.is_empty() {
                                    None
                                } else {
                                    Some(model_input.clone())
                                };
                                new_teachers[*teacher_idx].name = if name_input.is_empty() {
                                    None
                                } else {
                                    Some(name_input.clone())
                                };
                            }
                            step = WizardStep::TeacherConfig(new_teachers, *teacher_idx);
                        }
                        KeyCode::Backspace => {
                            // For simplicity, only edit model field for now
                            // In a real implementation, we'd track which field is active
                            model_input.pop();
                        }
                        KeyCode::Char(c) => {
                            model_input.push(c);
                        }
                        KeyCode::Esc => {
                            // Cancel edit, go back
                            step = WizardStep::TeacherConfig(teacher_list.clone(), *teacher_idx);
                        }
                        _ => {}
                    }
                }

                WizardStep::FeaturesConfig(auto_approve, streaming, debug) => {
                    match key.code {
                        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                            // Cycle through the three checkboxes
                            // For simplicity, we'll use a single index
                            // Toggle on space
                        }
                        KeyCode::Char(' ') | KeyCode::Char('1') => {
                            // Toggle auto_approve_tools
                            *auto_approve = !*auto_approve;
                            auto_approve_tools = *auto_approve;
                        }
                        KeyCode::Char('2') => {
                            // Toggle streaming_enabled
                            *streaming = !*streaming;
                            streaming_enabled = *streaming;
                        }
                        KeyCode::Char('3') => {
                            // Toggle debug_logging
                            *debug = !*debug;
                            debug_logging = *debug;
                        }
                        KeyCode::Enter => {
                            // Save feature flags and proceed to confirm
                            auto_approve_tools = *auto_approve;
                            streaming_enabled = *streaming;
                            debug_logging = *debug;
                            step = WizardStep::Confirm;
                        }
                        KeyCode::Esc => {
                            // Go back to teacher config
                            step = WizardStep::TeacherConfig(teachers.clone(), selected_teacher_idx);
                        }
                        _ => {}
                    }
                }

                WizardStep::Confirm => {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            // Build primary model config from backend settings
                            let primary_model = if backend_enabled {
                                ModelConfig::Local {
                                    family: model_families[selected_family_idx],
                                    size: model_sizes[selected_size_idx],
                                    execution: execution_targets[selected_target_idx],
                                    enabled: true,
                                }
                            } else {
                                ModelConfig::Remote {
                                    provider: "claude".to_string(),
                                    api_key: claude_key.clone(),
                                    model: String::new(),
                                    enabled: true,
                                }
                            };

                            return Ok(SetupResult {
                                active_theme: "dark".to_string(),
                                primary_model,
                                tool_models: vec![],
                                claude_api_key: claude_key.clone(),
                                hf_token: if hf_token.is_empty() { None } else { Some(hf_token.clone()) },
                                backend_enabled,
                                inference_provider: inference_providers[selected_provider_idx],
                                execution_target: execution_targets[selected_target_idx],
                                model_family: model_families[selected_family_idx],
                                model_size: model_sizes[selected_size_idx],
                                custom_model_repo: if custom_model_repo.is_empty() {
                                    None
                                } else {
                                    Some(custom_model_repo.clone())
                                },
                                teachers: teachers.clone(),
                                default_persona: "default".to_string(),
                                auto_approve_tools,
                                streaming_enabled,
                                debug_logging,
                                #[cfg(target_os = "macos")]
                                gui_automation: false,
                                daemon_only_mode: false,
                                mdns_discovery: false,
                            });
                        }
                        KeyCode::Char('n') | KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Clean up terminal state
fn cleanup_terminal(terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>) -> Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;
    Ok(())
}

fn render_wizard_step(
    f: &mut Frame,
    step: &WizardStep,
    inference_providers: &[InferenceProvider],
    execution_targets: &[ExecutionTarget],
    model_families: &[ModelFamily],
    model_sizes: &[ModelSize],
    _custom_repo: &str,
    selected_provider_idx: usize,
    selected_target_idx: usize,
    selected_family_idx: usize,
    selected_size_idx: usize,
) {
    let size = f.area();
    let dialog_area = centered_rect(70, 70, size);

    // Outer border
    let border = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title("Shammah Setup Wizard");
    f.render_widget(border, dialog_area);

    let inner = dialog_area.inner(ratatui::layout::Margin { horizontal: 2, vertical: 2 });

    match step {
        WizardStep::Welcome => render_welcome(f, inner),
        WizardStep::ClaudeApiKey(input) => render_api_key_input(f, inner, input),
        WizardStep::HfToken(input) => render_hf_token_input(f, inner, input),
        WizardStep::EnableLocalModel(enable) => render_enable_local_model(f, inner, *enable),
        WizardStep::InferenceProviderSelection(selected) => render_inference_provider_selection(f, inner, inference_providers, *selected),
        WizardStep::ExecutionTargetSelection(selected) => render_execution_target_selection(f, inner, execution_targets, *selected),
        WizardStep::ModelFamilySelection(selected) => render_model_family_selection(f, inner, model_families, *selected),
        WizardStep::ModelSizeSelection(selected) => render_model_size_selection(f, inner, model_sizes, *selected),
        WizardStep::IncompatibleCombination(error_msg) => render_incompatible_combination(f, inner, error_msg),
        WizardStep::ModelPreview => render_model_preview(f, inner, execution_targets[selected_target_idx], model_families[selected_family_idx], model_sizes[selected_size_idx]),
        WizardStep::CustomModelRepo(input, target) => render_custom_model_repo(f, inner, input, *target),
        WizardStep::TeacherConfig(teachers, selected) => render_teacher_config(f, inner, teachers, *selected),
        WizardStep::AddTeacherProviderSelection(_, selected) => render_provider_selection(f, inner, *selected),
        WizardStep::AddTeacherApiKey(_, provider, input) => render_teacher_api_key_input(f, inner, provider, input),
        WizardStep::AddTeacherModel(_, provider, _, input) => render_teacher_model_input(f, inner, provider, input),
        WizardStep::EditTeacher(teachers, idx, model_input, name_input) => render_edit_teacher(f, inner, teachers, *idx, model_input, name_input),
        WizardStep::FeaturesConfig(auto_approve, streaming, debug) => render_features_config(f, inner, *auto_approve, *streaming, *debug),
        WizardStep::Confirm => render_confirm(f, inner),
    }
}

fn render_welcome(f: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(5),     // Message
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("🚀 Welcome to Shammah!")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let message = Paragraph::new(
        "Shammah is a local-first AI coding assistant with continuous improvement.\n\n\
         This wizard will help you set up:\n\
         • Claude API key (for remote assistance)\n\
         • HuggingFace token (for model downloads)\n\
         • Inference device (uses ONNX Runtime)\n\n\
         Press Enter or Space to continue, Esc to cancel."
    )
    .style(Style::default().fg(Color::Reset))
    .alignment(Alignment::Left)
    .wrap(Wrap { trim: false });
    f.render_widget(message, chunks[1]);

    let help = Paragraph::new("Enter/Space: Continue  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_api_key_input(f: &mut Frame, area: Rect, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(5),  // Instructions
            Constraint::Length(4),  // Input (increased to 4 for better visibility)
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 1: Claude API Key")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Enter your Claude API key (required).\n\n\
         Get your key from: https://console.anthropic.com/\n\
         (starts with sk-ant-...)"
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    // For long API keys (>60 chars), show truncated version with indication
    let display_text = if input.len() > 60 {
        format!("{}...{} ({}characters) _",
            &input[..40],
            &input[input.len()-10..],
            input.len())
    } else if !input.is_empty() {
        format!("{}_", input)
    } else {
        "_".to_string()
    };

    let title_suffix = if !input.is_empty() {
        " (Pre-filled - press Backspace to clear)"
    } else {
        ""
    };

    let input_widget = Paragraph::new(display_text)
        .block(Block::default()
            .borders(Borders::ALL)
            .title(format!("API Key{}", title_suffix)))
        .style(Style::default().fg(if !input.is_empty() { Color::Green } else { Color::Reset }))
        .wrap(Wrap { trim: false });
    f.render_widget(input_widget, chunks[2]);

    let help = Paragraph::new("Type key then press Enter  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}

fn render_hf_token_input(f: &mut Frame, area: Rect, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(5),  // Instructions
            Constraint::Length(3),  // Input
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 2: HuggingFace Token (Optional)")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Enter your HuggingFace token (optional but recommended).\n\n\
         Required for downloading some models.\n\
         Get token from: https://huggingface.co/settings/tokens\n\
         (Press Enter to skip)"
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    let display_text = if input.is_empty() {
        "[Optional - press Enter to skip]".to_string()
    } else {
        input.to_string()
    };

    let input_widget = Paragraph::new(display_text)
        .block(Block::default().borders(Borders::ALL).title("HF Token"))
        .style(Style::default().fg(Color::Reset));
    f.render_widget(input_widget, chunks[2]);

    let help = Paragraph::new("Type token then press Enter (or Enter to skip)  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}

fn render_enable_local_model(f: &mut Frame, area: Rect, enable: bool) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Instructions + options
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 3: Enable Local Model?")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Would you like to enable local model inference?\n\n\
         ✓ Local Model: Download and run AI models on your device\n\
         • Works offline after initial download\n\
         • Privacy-first (code stays on your machine)\n\
         • Requires 8-64GB RAM depending on model size\n\
         • 5-30 minute download on first run\n\n\
         ✗ Proxy-Only: Use Shammah like Claude Code (no local model)\n\
         • REPL + tool execution (Read, Bash, etc.)\n\
         • Always forwards to teacher APIs (Claude/GPT-4)\n\
         • Faster startup, no downloads\n\
         • Requires internet connection\n\n"
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    // Show selected option with visual indicator
    let yes_style = if enable {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let no_style = if !enable {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    let options_text = vec![
        Line::from(vec![
            Span::styled(if enable { "▸ " } else { "  " }, yes_style),
            Span::styled("✓ Yes - Enable local model", yes_style),
        ]),
        Line::from(vec![
            Span::styled(if !enable { "▸ " } else { "  " }, no_style),
            Span::styled("✗ No - Proxy-only mode", no_style),
        ]),
    ];

    let options = Paragraph::new(options_text)
        .alignment(Alignment::Center);
    f.render_widget(options, Rect::new(chunks[1].x, chunks[1].y + chunks[1].height - 3, chunks[1].width, 3));

    let help = Paragraph::new("↑/↓: Toggle  Enter: Confirm  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_inference_provider_selection(f: &mut Frame, area: Rect, providers: &[InferenceProvider], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Provider options
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 4: Select Inference Provider")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let mut provider_lines = vec![
        Line::from(Span::styled(
            "Choose the inference engine for running models locally:\n",
            Style::default().fg(Color::Yellow),
        )),
    ];

    for (i, provider) in providers.iter().enumerate() {
        let is_selected = i == selected;
        let style = if is_selected {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Reset)
        };

        let indicator = if is_selected { "▸ " } else { "  " };

        match provider {
            InferenceProvider::Onnx => {
                provider_lines.push(Line::from(""));
                provider_lines.push(Line::from(vec![
                    Span::styled(indicator, style),
                    Span::styled("ONNX Runtime (Recommended)", style),
                ]));
                provider_lines.push(Line::from(
                    "  • Cross-platform, optimized inference engine"
                ));
                provider_lines.push(Line::from(
                    "  • CoreML/ANE acceleration on Mac (best performance)"
                ));
                provider_lines.push(Line::from(
                    "  • CUDA acceleration on NVIDIA GPUs"
                ));
                provider_lines.push(Line::from(
                    "  • Community-converted ONNX models"
                ));
            }
            #[cfg(feature = "candle")]
            InferenceProvider::Candle => {
                provider_lines.push(Line::from(""));
                provider_lines.push(Line::from(vec![
                    Span::styled(indicator, style),
                    Span::styled("Candle (Alternative)", style),
                ]));
                provider_lines.push(Line::from(
                    "  • Native Rust ML framework"
                ));
                provider_lines.push(Line::from(
                    "  • Metal/CUDA/CPU support"
                ));
                provider_lines.push(Line::from(
                    "  • Access to larger models (8B Llama, 27B Gemma)"
                ));
                provider_lines.push(Line::from(
                    "  • Original model repositories"
                ));
                provider_lines.push(Line::from(vec![
                    Span::styled("  ⚠ Note: ", Style::default().fg(Color::Yellow)),
                    Span::raw("ANE/CoreML works best with ONNX Runtime"),
                ]));
            }
        }
    }

    let provider_list = Paragraph::new(provider_lines)
        .wrap(Wrap { trim: false });
    f.render_widget(provider_list, chunks[1]);

    let help = Paragraph::new("↑/↓: Select  Enter: Confirm  Esc: Back")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_execution_target_selection(f: &mut Frame, area: Rect, targets: &[ExecutionTarget], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Target list
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 5: Select Execution Target")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = targets
        .iter()
        .map(|target| {
            let description = target.description();
            let emoji = match target {
                #[cfg(target_os = "macos")]
                ExecutionTarget::CoreML => "⚡",
                #[cfg(feature = "cuda")]
                ExecutionTarget::Cuda => "💨",
                ExecutionTarget::Cpu => "🔄",
                ExecutionTarget::Auto => "🤖",
            };

            ListItem::new(Line::from(vec![
                Span::raw(emoji),
                Span::raw(" "),
                Span::styled(description, Style::default().fg(Color::Reset).add_modifier(Modifier::BOLD)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Where should inference run? "))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = Paragraph::new("↑/↓: Navigate  Enter: Select  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_model_family_selection(f: &mut Frame, area: Rect, families: &[ModelFamily], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Family list
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 4: Select Model Family")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = families
        .iter()
        .map(|family| {
            ListItem::new(Line::from(vec![
                Span::styled(family.name(), Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                Span::raw(" - "),
                Span::styled(family.description(), Style::default().fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = Paragraph::new("↑/↓: Navigate  Enter: Select  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_model_size_selection(f: &mut Frame, area: Rect, sizes: &[ModelSize], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Size list
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 5: Select Model Size")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = sizes
        .iter()
        .enumerate()
        .map(|(idx, size)| {
            let (desc, ram) = match size {
                ModelSize::Small => ("Small (~1-3B params)", "8-16GB RAM"),
                ModelSize::Medium => ("Medium (~3-9B params)", "16-32GB RAM (Recommended)"),
                ModelSize::Large => ("Large (~7-14B params)", "32-64GB RAM"),
                ModelSize::XLarge => ("XLarge (~14B+ params)", "64GB+ RAM"),
            };
            let is_recommended = idx == 1; // Medium
            let style = if is_recommended {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(Line::from(vec![
                Span::styled(desc, style.add_modifier(Modifier::BOLD)),
                Span::raw(" - "),
                Span::styled(ram, Style::default().fg(Color::Gray)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut list_state);

    let help = Paragraph::new("↑/↓: Navigate  Enter: Select  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_incompatible_combination(f: &mut Frame, area: Rect, error_msg: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Error message
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("⚠️  Incompatible Configuration")
        .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let error = Paragraph::new(error_msg)
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: false })
        .alignment(Alignment::Left);
    f.render_widget(error, chunks[1]);

    let help = Paragraph::new("Enter/b: Change Model Family  d: Change Device  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_model_preview(f: &mut Frame, area: Rect, target: ExecutionTarget, family: ModelFamily, size: ModelSize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(10),    // Model info
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 7: Model Preview")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Use compatibility matrix to resolve repository (ONNX provider by default)
    use crate::models::unified_loader::InferenceProvider;
    let repo = compatibility::get_repository(InferenceProvider::Onnx, family, size)
        .unwrap_or_else(|| format!("onnx-community/{}-{}-Instruct", family.name(), size.to_size_string(family)));

    // Estimate parameters, download size, and RAM based on size
    let (params, download_size, ram_req) = match size {
        ModelSize::Small => ("~1-3B parameters", "~2-4 GB", "8-16 GB"),
        ModelSize::Medium => ("~3-9B parameters", "~6-12 GB", "16-32 GB"),
        ModelSize::Large => ("~7-14B parameters", "~14-28 GB", "32-64 GB"),
        ModelSize::XLarge => ("~14B+ parameters", "~28-56 GB", "64+ GB"),
    };

    let info_text = format!(
        "The following model will be downloaded:\n\n\
         📦 Repository: {}\n\
         🧠 Size: {}\n\
         💾 Download: {}\n\
         ⚡ Execution Target: {}\n\
         💻 RAM Required: {}\n\n\
         This model will be cached in ~/.cache/huggingface/hub/\n\
         for offline use. First download may take 5-30 minutes.\n\n\
         All models use ONNX Runtime. Your selection determines which\n\
         execution provider is used (CoreML/CPU/CUDA).\n\n\
         Press Enter to continue, 'b' to go back, Esc to cancel.",
        repo, params, download_size, target.name(), ram_req
    );

    let info = Paragraph::new(info_text)
        .style(Style::default().fg(Color::Reset))
        .wrap(Wrap { trim: false });
    f.render_widget(info, chunks[1]);

    let help = Paragraph::new("Enter: Continue  b: Back  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_custom_model_repo(f: &mut Frame, area: Rect, input: &str, _target: ExecutionTarget) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(8),  // Instructions
            Constraint::Length(3),  // Input
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 6: Custom Model Repository (Optional)")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // ONNX-focused instructions (device selection only affects execution provider)
    let instructions_text = "Specify a custom HuggingFace model repository in ONNX format (optional).\n\n\
         All models must be in ONNX format. Your device selection (CoreML/Metal/CPU)\n\
         only affects which ONNX Runtime execution provider is used.\n\n\
         Examples of ONNX model repositories:\n\
         • onnx-community/Qwen2.5-1.5B-Instruct (Qwen, recommended)\n\
         • microsoft/Phi-3.5-mini-instruct-onnx (Phi)\n\
         • onnx-community/DeepSeek-R1-Distill-Qwen-1.5B-ONNX (DeepSeek)\n\n\
         Leave empty to use recommended defaults. Press Enter to continue.";

    let instructions = Paragraph::new(instructions_text)
        .style(Style::default().fg(Color::Reset))
        .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    let display_text = if input.is_empty() {
        "[Optional - press Enter to skip]".to_string()
    } else {
        input.to_string()
    };

    let input_widget = Paragraph::new(display_text)
        .block(Block::default()
            .borders(Borders::ALL)
            .title("HuggingFace Repo"))
        .style(Style::default().fg(Color::Reset));
    f.render_widget(input_widget, chunks[2]);

    let help = Paragraph::new("Type repo then press Enter (or Enter to skip)  Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}

fn render_teacher_config(f: &mut Frame, area: Rect, teachers: &[TeacherEntry], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(4),  // Instructions
            Constraint::Min(8),     // Teacher list (more space for details)
            Constraint::Length(3),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 6: Teacher Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Teachers are tried in order. First teacher is primary.\n\
         Use Shift+↑/↓ to reorder, e to edit, d to remove, a to add."
    )
    .style(Style::default().fg(Color::Yellow))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    // Build detailed teacher list with priority indicators
    let items: Vec<ListItem> = teachers
        .iter()
        .enumerate()
        .map(|(idx, teacher)| {
            let priority_label = if idx == 0 {
                "PRIMARY"
            } else {
                "FALLBACK"
            };

            let display_name = teacher.name.as_deref().unwrap_or(&teacher.provider);
            let model_display = teacher.model.as_deref().unwrap_or("(default)");

            let priority_style = if idx == 0 {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Yellow)
            };

            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{}. ", idx + 1),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                    ),
                    Span::styled(display_name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::styled(priority_label, priority_style),
                ]),
                Line::from(vec![
                    Span::raw("   Provider: "),
                    Span::styled(&teacher.provider, Style::default().fg(Color::Gray)),
                    Span::raw("  Model: "),
                    Span::styled(model_display, Style::default().fg(Color::Gray)),
                ]),
            ])
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(selected));

    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .title(" Teachers (in priority order) ")
        )
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[2], &mut list_state);

    let help = Paragraph::new(
        "↑/↓: Navigate  Shift+↑/↓: Reorder  e: Edit  d: Remove  a: Add\n\
         Enter: Continue  Esc: Cancel"
    )
    .style(Style::default().fg(Color::Gray))
    .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}

fn render_edit_teacher(
    f: &mut Frame,
    area: Rect,
    teachers: &[TeacherEntry],
    teacher_idx: usize,
    model_input: &str,
    name_input: &str,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(6),  // Current info
            Constraint::Length(5),  // Model input
            Constraint::Length(5),  // Name input (future)
            Constraint::Min(2),     // Examples
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Edit Teacher")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Show current teacher info
    if teacher_idx < teachers.len() {
        let teacher = &teachers[teacher_idx];
        let current_info = Paragraph::new(format!(
            "Provider: {}\n\
             Current Model: {}\n\
             Current Name: {}",
            teacher.provider,
            teacher.model.as_deref().unwrap_or("(default)"),
            teacher.name.as_deref().unwrap_or("(none)")
        ))
        .style(Style::default().fg(Color::Gray))
        .block(Block::default().borders(Borders::ALL).title(" Current Settings "));
        f.render_widget(current_info, chunks[1]);
    }

    // Model input
    let model_prompt = Paragraph::new("API Model Name (leave empty for provider default):")
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(model_prompt, chunks[2]);

    let model_widget = Paragraph::new(model_input)
        .style(Style::default().fg(Color::Green))
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green))
            .title(" Model ")
        );
    f.render_widget(model_widget, chunks[3]);

    // Examples based on provider
    let examples = if teacher_idx < teachers.len() {
        let teacher = &teachers[teacher_idx];
        match teacher.provider.as_str() {
            "claude" => "Examples: claude-opus-4-6 | claude-sonnet-4-20250514 | claude-haiku-4-5",
            "openai" => "Examples: gpt-4o | gpt-4o-mini | gpt-4-turbo | o1",
            "gemini" => "Examples: gemini-2.0-flash-exp | gemini-1.5-pro | gemini-1.5-flash",
            "grok" => "Examples: grok-2-1212 | grok-beta",
            "mistral" => "Examples: mistral-large-latest | mistral-small-latest",
            "groq" => "Examples: llama-3.1-70b-versatile | mixtral-8x7b | gemma-7b",
            _ => "Leave empty to use provider's default model"
        }
    } else {
        ""
    };

    let examples_widget = Paragraph::new(examples)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: false });
    f.render_widget(examples_widget, chunks[4]);

    let help = Paragraph::new("Type model name | Enter: Save | Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[5]);
}

fn render_confirm(f: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(5),     // Summary
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("✓ Setup Complete!")
        .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let summary = Paragraph::new(
        "Configuration will be saved to: ~/.finch/config.toml\n\n\
         ✓ Claude API key configured\n\
         ✓ HuggingFace token configured (or skipped)\n\
         ✓ Inference device selected\n\
         ✓ Model family and size selected\n\
         ✓ Teacher configuration set\n\n\
         Press 'y' or Enter to confirm and start Shammah.\n\
         Press 'n' or Esc to cancel."
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(summary, chunks[1]);

    let help = Paragraph::new("y/Enter: Confirm  n/Esc: Cancel")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_provider_selection(f: &mut Frame, area: Rect, selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new("Select Provider")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let providers = vec!["claude", "openai", "gemini", "grok", "mistral", "groq"];
    let items: Vec<ListItem> = providers
        .iter()
        .enumerate()
        .map(|(idx, provider)| {
            let style = if idx == selected {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Reset)
            };
            ListItem::new(Line::from(Span::styled(*provider, style)))
        })
        .collect();

    let list = List::new(items);
    f.render_widget(list, chunks[1]);

    let instructions = Paragraph::new("↑/↓: Navigate | Enter: Select | Esc: Back")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[2]);
}

fn render_teacher_api_key_input(f: &mut Frame, area: Rect, provider: &str, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(1),
        ])
        .split(area);

    let title = Paragraph::new(format!("Configure {}", provider.to_uppercase()))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let prompt = Paragraph::new(format!("Enter API key for {}:", provider))
        .style(Style::default().fg(Color::Yellow));
    f.render_widget(prompt, chunks[1]);

    // Mask API key for security (show asterisks)
    let masked = "*".repeat(input.len());
    let input_widget = Paragraph::new(masked)
        .style(Style::default().fg(Color::Green))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
    f.render_widget(input_widget, chunks[2]);

    let instructions = Paragraph::new("Type API key | Enter: Continue | Esc: Back")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[3]);
}

fn render_teacher_model_input(f: &mut Frame, area: Rect, provider: &str, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new(format!("Configure {}", provider.to_uppercase()))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let prompt = Paragraph::new(
        format!("Enter model name for {} (optional):\nLeave empty to use default model", provider)
    )
        .style(Style::default().fg(Color::Yellow))
        .wrap(Wrap { trim: true });
    f.render_widget(prompt, chunks[1]);

    let input_widget = Paragraph::new(input)
        .style(Style::default().fg(Color::Green))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(Color::Green)));
    f.render_widget(input_widget, chunks[3]);

    let instructions = Paragraph::new("Type model name | Enter: Add Teacher | Esc: Skip")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[4]);
}

fn render_features_config(f: &mut Frame, area: Rect, auto_approve: bool, streaming: bool, debug: bool) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(4),  // Instructions
            Constraint::Min(12),     // Feature checkboxes
            Constraint::Length(3),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 7: Feature Flags")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Configure optional features:\n\
         Press 1/2/3 to toggle, Space for first option, Enter to continue"
    )
    .style(Style::default().fg(Color::Yellow))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    // Feature checkboxes
    let auto_approve_checkbox = if auto_approve { "☑" } else { "☐" };
    let streaming_checkbox = if streaming { "☑" } else { "☐" };
    let debug_checkbox = if debug { "☑" } else { "☐" };

    let features_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("1. ", Style::default().fg(Color::Cyan)),
            Span::styled(format!("{} Auto-approve all tools", auto_approve_checkbox),
                if auto_approve { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Gray) }),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("Skip confirmation dialogs when AI uses tools", Style::default().fg(Color::Gray)),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("⚠️  Use with caution - tools can modify files", Style::default().fg(Color::Yellow)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("2. ", Style::default().fg(Color::Cyan)),
            Span::styled(format!("{} Streaming responses", streaming_checkbox),
                if streaming { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Gray) }),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("Stream tokens in real-time from teacher models", Style::default().fg(Color::Gray)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("3. ", Style::default().fg(Color::Cyan)),
            Span::styled(format!("{} Debug logging", debug_checkbox),
                if debug { Style::default().fg(Color::Green) } else { Style::default().fg(Color::Gray) }),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled("Enable verbose logging for troubleshooting", Style::default().fg(Color::Gray)),
        ]),
    ];

    let features = Paragraph::new(features_text)
        .wrap(Wrap { trim: false });
    f.render_widget(features, chunks[2]);

    let help = Paragraph::new("1/2/3: Toggle  Space: Toggle first  Enter: Continue  Esc: Back")
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}
