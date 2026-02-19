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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::service::discovery_client::{ServiceDiscoveryClient, DiscoveredService};

/// Step in the "Add Provider" flow (overlay inside Models section)
#[derive(Debug, Clone)]
enum AddProviderStep {
    // Step 0: what kind of provider to add?
    SelectAddType { selected: usize },
    // Cloud AI path (steps 1-3)
    SelectProvider { selected: usize },
    EnterApiKey { provider: String, api_key: String },
    EnterModel { provider: String, api_key: String, model: String },
    // Local ONNX path (steps 1-2)
    SelectLocalFamily { selected: usize },
    SelectLocalSize { family: ModelFamily, selected: usize },
    // Network scan path
    Scanning { results: Arc<Mutex<Option<Vec<DiscoveredService>>>> },
    SelectAgent { agents: Vec<DiscoveredService>, selected: usize },
}

/// Cloud provider options shown in the add-provider overlay
const CLOUD_PROVIDERS: &[(&str, &str, &str)] = &[
    ("claude",  "Anthropic Claude", "claude-sonnet-4-6"),
    ("openai",  "OpenAI GPT",       "gpt-4o"),
    ("grok",    "xAI Grok",         "grok-2"),
    ("gemini",  "Google Gemini",    "gemini-2.0-flash"),
    ("mistral", "Mistral AI",       "mistral-large-latest"),
    ("groq",    "Groq (fast)",      "llama-3.3-70b-versatile"),
];

use crate::config::{ExecutionTarget, TeacherEntry};
use crate::models::unified_loader::{InferenceProvider, ModelFamily, ModelSize};
use crate::models::compatibility;

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
            Self::Models => "API Key",
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
        error: Option<String>,
    },
    Personas {
        available_personas: Vec<PersonaInfo>,
        selected_idx: usize,
        default_persona: String,
        editing_prompt: bool,
        prompt_input: String,
    },
    Features {
        auto_approve: bool,
        streaming: bool,
        debug: bool,
        hf_token: String,
        editing_hf_token: bool,
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
    slug: String,       // Key used to load the persona (e.g. "expert-coder")
    name: String,       // Display name (e.g. "Expert Coder")
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
                // Default: remote Claude - try to auto-detect key
                let detected_key = detect_anthropic_api_key()
                    .or_else(detect_xai_api_key)
                    .unwrap_or_default();
                ModelConfig::Remote {
                    provider: "claude".to_string(),
                    api_key: detected_key,
                    model: String::new(),
                    enabled: true,
                }
            }
        } else {
            // Default: remote Claude - try to auto-detect key
            let detected_key = detect_anthropic_api_key()
                .or_else(detect_xai_api_key)
                .unwrap_or_default();
            ModelConfig::Remote {
                provider: "claude".to_string(),
                api_key: detected_key,
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
                editing_model_mode: false,
                model_input: String::new(),
                adding_provider: None,
                error: None,
            },
        );

        // Personas section
        let builtin_personas: Vec<PersonaInfo> = Persona::list_builtins()
            .iter()
            .filter_map(|slug| {
                Persona::load_builtin(slug).ok().map(|p| PersonaInfo {
                    slug: slug.to_string(),
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
            .position(|p| p.slug == default_persona || p.name.to_lowercase() == default_persona.to_lowercase())
            .unwrap_or(0);

        sections.insert(
            WizardSection::Personas,
            SectionState::Personas {
                available_personas: builtin_personas,
                selected_idx,
                default_persona,
                editing_prompt: false,
                prompt_input: String::new(),
            },
        );

        // Features section
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
                #[cfg(target_os = "macos")]
                gui_automation: existing_config
                    .map(|c| c.features.gui_automation)
                    .unwrap_or(false),
                daemon_only_mode: existing_config
                    .map(|c| c.server.mode == "daemon-only")
                    .unwrap_or(false),
                mdns_discovery: existing_config
                    .map(|c| c.server.advertise)
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

/// Returns true if the Models section is currently in the Scanning sub-step
fn is_scanning_state(state: &WizardState) -> bool {
    if let Some(SectionState::Models { adding_provider, .. }) =
        state.sections.get(&WizardSection::Models)
    {
        matches!(adding_provider, Some(AddProviderStep::Scanning { .. }))
    } else {
        false
    }
}

/// If a network scan has finished, advance to SelectAgent (or close overlay if no agents)
fn advance_scan_if_done(state: &mut WizardState) {
    if let Some(SectionState::Models { adding_provider, .. }) =
        state.sections.get_mut(&WizardSection::Models)
    {
        // Check if results are ready without holding the lock across the reassignment
        let agents_opt = if let Some(AddProviderStep::Scanning { results }) = adding_provider.as_ref() {
            results.try_lock().ok().and_then(|g| g.as_ref().cloned())
        } else {
            return;
        };

        if let Some(agents) = agents_opt {
            *adding_provider = if agents.is_empty() {
                None // No agents found — close overlay
            } else {
                Some(AddProviderStep::SelectAgent { agents, selected: 0 })
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

        let Some(key) = key_opt else { continue; };

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
        editing_model_mode,
        model_input,
        adding_provider,
        error,
    }) = state.sections.get_mut(&WizardSection::Models)
    {
        // Clear error on any input
        *error = None;

        // Handle add-provider overlay first
        if adding_provider.is_some() {
            match key.code {
                KeyCode::Esc => {
                    *adding_provider = None;
                }
                KeyCode::Up => {
                    match adding_provider {
                        Some(AddProviderStep::SelectAddType { selected }) => {
                            if *selected > 0 { *selected -= 1; }
                        }
                        Some(AddProviderStep::SelectProvider { selected }) => {
                            if *selected > 0 { *selected -= 1; }
                        }
                        Some(AddProviderStep::SelectLocalFamily { selected }) => {
                            if *selected > 0 { *selected -= 1; }
                        }
                        Some(AddProviderStep::SelectLocalSize { selected, .. }) => {
                            if *selected > 0 { *selected -= 1; }
                        }
                        Some(AddProviderStep::SelectAgent { selected, .. }) => {
                            if *selected > 0 { *selected -= 1; }
                        }
                        _ => {}
                    }
                }
                KeyCode::Down => {
                    match adding_provider {
                        Some(AddProviderStep::SelectAddType { selected }) => {
                            if *selected < 2 { *selected += 1; }
                        }
                        Some(AddProviderStep::SelectProvider { selected }) => {
                            if *selected < CLOUD_PROVIDERS.len() - 1 { *selected += 1; }
                        }
                        Some(AddProviderStep::SelectLocalFamily { selected }) => {
                            if *selected < 5 { *selected += 1; }
                        }
                        Some(AddProviderStep::SelectLocalSize { selected, .. }) => {
                            if *selected < 3 { *selected += 1; }
                        }
                        Some(AddProviderStep::SelectAgent { agents, selected }) => {
                            if *selected + 1 < agents.len() { *selected += 1; }
                        }
                        _ => {}
                    }
                }
                KeyCode::Char(c) => {
                    match adding_provider {
                        Some(AddProviderStep::EnterApiKey { api_key, .. }) => {
                            api_key.push(c);
                        }
                        Some(AddProviderStep::EnterModel { model, .. }) => {
                            model.push(c);
                        }
                        _ => {}
                    }
                }
                KeyCode::Backspace => {
                    match adding_provider {
                        Some(AddProviderStep::EnterApiKey { api_key, .. }) => {
                            api_key.pop();
                        }
                        Some(AddProviderStep::EnterModel { model, .. }) => {
                            model.pop();
                        }
                        _ => {}
                    }
                }
                KeyCode::Enter => {
                    // Ignore Enter while network scan is in progress
                    if matches!(adding_provider, Some(AddProviderStep::Scanning { .. })) {
                        return Ok(false);
                    }

                    let next_step = match adding_provider.take() {
                        // ── type selection ──────────────────────────────────────────────
                        Some(AddProviderStep::SelectAddType { selected }) => match selected {
                            0 => Some(AddProviderStep::SelectProvider { selected: 0 }),
                            1 => Some(AddProviderStep::SelectLocalFamily { selected: 0 }),
                            2 => {
                                // Start background network scan
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
                                Some(AddProviderStep::Scanning { results: results_arc })
                            }
                            _ => None,
                        },
                        // ── cloud AI path ────────────────────────────────────────────
                        Some(AddProviderStep::SelectProvider { selected }) => {
                            let (provider_id, _, _) = CLOUD_PROVIDERS[selected];
                            Some(AddProviderStep::EnterApiKey {
                                provider: provider_id.to_string(),
                                api_key: String::new(),
                            })
                        }
                        Some(AddProviderStep::EnterApiKey { provider, api_key }) => {
                            Some(AddProviderStep::EnterModel {
                                provider,
                                api_key,
                                model: String::new(),
                            })
                        }
                        Some(AddProviderStep::EnterModel { provider, api_key, model }) => {
                            let resolved_model = if model.is_empty() {
                                CLOUD_PROVIDERS
                                    .iter()
                                    .find(|(id, _, _)| *id == provider.as_str())
                                    .map(|(_, _, def)| def.to_string())
                                    .unwrap_or_default()
                            } else {
                                model
                            };
                            tool_models.push(ModelConfig::Remote {
                                provider,
                                api_key,
                                model: resolved_model,
                                enabled: true,
                            });
                            *selected_idx = tool_models.len();
                            None
                        }
                        // ── local ONNX path ──────────────────────────────────────────
                        Some(AddProviderStep::SelectLocalFamily { selected }) => {
                            let families = [
                                ModelFamily::Qwen2,
                                ModelFamily::Gemma2,
                                ModelFamily::Llama3,
                                ModelFamily::Mistral,
                                ModelFamily::Phi,
                                ModelFamily::DeepSeek,
                            ];
                            let family = families[selected.min(families.len() - 1)];
                            Some(AddProviderStep::SelectLocalSize { family, selected: 0 })
                        }
                        Some(AddProviderStep::SelectLocalSize { family, selected }) => {
                            let sizes = [
                                ModelSize::Small,
                                ModelSize::Medium,
                                ModelSize::Large,
                                ModelSize::XLarge,
                            ];
                            let size = sizes[selected.min(sizes.len() - 1)];
                            // Replace primary if it has no key and there are no tool models
                            let replace_primary = matches!(
                                primary_model,
                                ModelConfig::Remote { api_key, .. } if api_key.is_empty()
                            ) && tool_models.is_empty();
                            if replace_primary {
                                *primary_model = ModelConfig::Local {
                                    family,
                                    size,
                                    execution: ExecutionTarget::Cpu,
                                    enabled: true,
                                };
                                *selected_idx = 0;
                            } else {
                                tool_models.push(ModelConfig::Local {
                                    family,
                                    size,
                                    execution: ExecutionTarget::Cpu,
                                    enabled: true,
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
                                    api_key: String::new(),
                                    model: format!("{}:{}", agent.host, agent.port),
                                    enabled: true,
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
                KeyCode::Char('e') | KeyCode::Char('E') => {
                    // Enter API key edit mode
                    *editing_mode = true;
                }
                KeyCode::Char('m') | KeyCode::Char('M') => {
                    // Edit model name for selected entry
                    let current_model = if *selected_idx == 0 {
                        if let ModelConfig::Remote { model, .. } = primary_model {
                            model.clone()
                        } else {
                            String::new()
                        }
                    } else {
                        let tool_idx = *selected_idx - 1;
                        if let Some(ModelConfig::Remote { model, .. }) = tool_models.get(tool_idx) {
                            model.clone()
                        } else {
                            String::new()
                        }
                    };
                    *model_input = current_model;
                    *editing_model_mode = true;
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
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    // Skip - just move to next section without validation
                    state.mark_completed(WizardSection::Models);
                    state.next_section();
                }
                KeyCode::Enter | KeyCode::Tab => {
                    // Always allow advancing — no key format validation
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
        editing_prompt,
        prompt_input,
    }) = state.sections.get_mut(&WizardSection::Personas)
    {
        if *editing_prompt {
            // In system prompt editing mode
            match key.code {
                KeyCode::Char('s') if key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) => {
                    // Ctrl+S: save edited prompt
                    let new_prompt = prompt_input.clone();
                    if let Some(persona) = available_personas.get_mut(*selected_idx) {
                        persona.system_prompt = new_prompt;
                    }
                    *editing_prompt = false;
                }
                KeyCode::Esc => {
                    // Cancel edit, discard changes
                    *editing_prompt = false;
                    prompt_input.clear();
                }
                KeyCode::Enter => {
                    // Insert newline
                    prompt_input.push('\n');
                }
                KeyCode::Backspace => {
                    prompt_input.pop();
                }
                KeyCode::Char(c) => {
                    prompt_input.push(c);
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
                // Enter system prompt editing mode
                if let Some(persona) = available_personas.get(*selected_idx) {
                    *prompt_input = persona.system_prompt.clone();
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
        hf_token,
        editing_hf_token,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        selected_idx,
    }) = state.sections.get_mut(&WizardSection::Features)
    {
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

        // 6 features: 0=streaming, 1=auto_approve, 2=debug, 3=hf_token, 4=daemon_only, 5=mdns
        #[cfg(target_os = "macos")]
        let num_features = 7; // +1 for gui_automation at index 3, hf_token at 4, daemon at 5, mdns at 6
        #[cfg(not(target_os = "macos"))]
        let num_features = 6;

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
                // Toggle selected feature (all except hf_token index)
                #[cfg(target_os = "macos")]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    3 => *gui_automation = !*gui_automation,
                    // index 4 = hf_token (no toggle)
                    5 => *daemon_only_mode = !*daemon_only_mode,
                    6 => *mdns_discovery = !*mdns_discovery,
                    _ => {}
                }
                #[cfg(not(target_os = "macos"))]
                match *selected_idx {
                    0 => *streaming = !*streaming,
                    1 => *auto_approve = !*auto_approve,
                    2 => *debug = !*debug,
                    // index 3 = hf_token (no toggle)
                    4 => *daemon_only_mode = !*daemon_only_mode,
                    5 => *mdns_discovery = !*mdns_discovery,
                    _ => {}
                }
            }
            KeyCode::Char('e') | KeyCode::Char('E') => {
                // 'E' enters HF token edit mode when that row is selected
                #[cfg(target_os = "macos")]
                let hf_idx = 4;
                #[cfg(not(target_os = "macos"))]
                let hf_idx = 3;
                if *selected_idx == hf_idx {
                    *editing_hf_token = true;
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
    let (auto_approve, streaming, debug, hf_token_val, daemon_only, mdns) = if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        hf_token,
        daemon_only_mode,
        mdns_discovery,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        (
            *auto_approve,
            *streaming,
            *debug,
            if hf_token.is_empty() { None } else { Some(hf_token.clone()) },
            *daemon_only_mode,
            *mdns_discovery,
        )
    } else {
        (false, true, false, None, false, false)
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
        hf_token: hf_token_val,
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
        .block(Block::default().borders(Borders::ALL).title(" Finch Setup "))
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
        WizardSection::Themes => "↑/↓: Choose theme | Enter: Next | Tab: Jump to section",
        WizardSection::Models => "E: API key  M: Model name  A: Add  D: Remove  Tab: Next",
        WizardSection::Personas => "↑/↓: Choose style | E: Edit prompt | Enter: Next | Tab: Jump",
        WizardSection::Features => "↑/↓: Navigate | Space: Toggle | Enter: Save | Tab: Jump to section",
        WizardSection::Review => "Enter: Save & start | Tab: Go back to edit",
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
            editing_model_mode,
            model_input,
            adding_provider,
            error,
        }) => render_models_section(
            f,
            area,
            primary_model,
            tool_models,
            *selected_idx,
            *editing_mode,
            *editing_model_mode,
            model_input,
            adding_provider.as_ref(),
            error.as_deref(),
        ),
        Some(SectionState::Personas {
            available_personas,
            selected_idx,
            default_persona,
            editing_prompt,
            prompt_input,
        }) => render_personas_section(f, area, available_personas, *selected_idx, default_persona, *editing_prompt, prompt_input),
        Some(SectionState::Features {
            auto_approve,
            streaming,
            debug,
            hf_token,
            editing_hf_token,
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
            hf_token,
            *editing_hf_token,
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
                        .add_modifier(Modifier::BOLD)
                )
            } else {
                (
                    "    ",
                    "",
                    Style::default().fg(Color::Blue)
                )
            };

            let text = format!("{}{} - {}{}", prefix, theme.name(), theme.description(), suffix);
            ListItem::new(text).style(style)
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
        "Use ↑/↓ arrow keys to move selection (>>> theme <<<)\n\
         Selected theme shows with white background. Press Enter to confirm."
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
    editing_model_mode: bool,
    model_input: &str,
    adding_provider: Option<&AddProviderStep>,
    error: Option<&str>,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(4),  // Description
            Constraint::Min(6),     // Primary model + tool models
            Constraint::Length(3),  // Input panel (edit mode) or dim hint
            Constraint::Length(2),  // Instructions
            Constraint::Length(2),  // Error (if present)
        ])
        .split(area);

    let title = Paragraph::new("AI Providers")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Show helpful hint when no key is configured
    let has_key = match primary_model {
        ModelConfig::Remote { api_key, .. } => !api_key.is_empty(),
        ModelConfig::Local { .. } => true,
    };

    let description_text = if has_key {
        format!("Primary provider configured. Press A to add more providers ({} total).",
            1 + tool_models.len())
    } else {
        "Paste your API key below (E), or add a provider with A.\n\
         No key yet? Get one at console.anthropic.com/keys".to_string()
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
                .add_modifier(Modifier::BOLD)
        )
    } else {
        (
            "    ",
            "",
            Style::default().fg(Color::Blue)
        )
    };

    let primary_display = match primary_model {
        ModelConfig::Local { family, size, execution, .. } => {
            format!(
                "{}★ Primary: Local {} {} ({}){}",
                prefix,
                family.name(),
                model_size_display(size),
                execution.name(),
                suffix
            )
        }
        ModelConfig::Remote { provider, api_key, model, .. } => {
            let key_display = if api_key.is_empty() {
                "[Not configured]".to_string()
            } else {
                format!("{}...{}", &api_key.chars().take(10).collect::<String>(), api_key.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>())
            };
            let model_display = if !model.is_empty() {
                format!(" - {}", model)
            } else {
                String::new()
            };
            format!(
                "{}★ Primary: {}{} [{}]{}",
                prefix, provider, model_display, key_display, suffix
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
                    .add_modifier(Modifier::BOLD)
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
                    prefix, checkbox, family.name(), model_size_display(size), suffix
                )
            }
            ModelConfig::Remote { provider, model, .. } => {
                let model_display = if !model.is_empty() {
                    format!(" - {}", model)
                } else {
                    String::new()
                };
                format!(
                    "{}{} Tool: {}{}{}",
                    prefix, checkbox, provider, model_display, suffix
                )
            }
        };

        items.push(ListItem::new(display).style(style));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("AI Providers"),
        );
    f.render_widget(list, chunks[2]);

    // Input panel (chunks[3]): bordered text box when in editing mode, dim hint otherwise
    if editing_mode {
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
        let panel = Paragraph::new(format!("{}|", current_key))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Edit API Key")
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        f.render_widget(panel, chunks[3]);
    } else if editing_model_mode {
        let panel = Paragraph::new(format!("{}|", model_input))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Edit Model")
                    .border_style(Style::default().fg(Color::Yellow)),
            );
        f.render_widget(panel, chunks[3]);
    } else {
        let hint = Paragraph::new("Press E to edit API key · M to edit model name")
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center);
        f.render_widget(hint, chunks[3]);
    }

    // Instructions (chunks[4])
    let instructions_text = if editing_mode {
        "Type here | Enter/Esc: Save & return"
    } else if editing_model_mode {
        "Type here | Enter/Esc: Save & return"
    } else {
        "E: API key | M: Model name | A: Add provider | D: Remove | Tab: Next"
    };
    let instructions = Paragraph::new(instructions_text)
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
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
        render_add_provider_overlay(f, area, step);
    }
}

/// Render the add-provider overlay (centered box)
fn render_add_provider_overlay(f: &mut Frame, area: Rect, step: &AddProviderStep) {
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
        .title_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(Color::Black));
    f.render_widget(background, overlay);

    let inner = Rect::new(
        overlay.x + 1,
        overlay.y + 1,
        overlay.width.saturating_sub(2),
        overlay.height.saturating_sub(2),
    );

    match step {
        // ── type selection ───────────────────────────────────────────────────────────
        AddProviderStep::SelectAddType { selected } => {
            const ADD_TYPES: &[(&str, &str)] = &[
                ("Cloud AI provider", "Connect to Claude, GPT-4, Grok, Gemini, Mistral, or Groq"),
                ("Local ONNX model", "Run a model on this machine (no internet after download)"),
                ("Scan local network", "Discover other Finch instances running on your LAN"),
            ];
            let items: Vec<ListItem> = ADD_TYPES
                .iter()
                .enumerate()
                .map(|(i, (name, desc))| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let lines = vec![
                        Line::from(format!("{}{}{}", prefix, name, suffix)).style(style),
                        Line::from(format!("        {}", desc)).style(Style::default().fg(Color::DarkGray)),
                    ];
                    ListItem::new(lines)
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().title("What do you want to add?  ↑/↓: Move | Enter: Select | Esc: Cancel"));
            f.render_widget(list, inner);
        }
        // ── cloud AI path ─────────────────────────────────────────────────────────────
        AddProviderStep::SelectProvider { selected } => {
            let items: Vec<ListItem> = CLOUD_PROVIDERS
                .iter()
                .enumerate()
                .map(|(i, (_, display_name, default_model))| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    ListItem::new(format!("{}{} (e.g. {}){}", prefix, display_name, default_model, suffix)).style(style)
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().title("Select cloud provider  ↑/↓: Move | Enter: Select | Esc: Back"));
            f.render_widget(list, inner);
        }
        AddProviderStep::EnterApiKey { provider, api_key } => {
            let hint = CLOUD_PROVIDERS.iter()
                .find(|(id, _, _)| *id == provider.as_str())
                .map(|(_, name, _)| format!("{} API key", name))
                .unwrap_or_else(|| format!("{} API key", provider));
            let lines = vec![
                Line::from(vec![
                    Span::styled("Provider: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(provider.as_str(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(Span::styled(&hint, Style::default().fg(Color::DarkGray))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Key: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(if api_key.is_empty() { "[type here]" } else { api_key.as_str() }),
                    Span::styled("|", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(""),
                Line::from(Span::styled("Enter: Next  Esc: Cancel", Style::default().fg(Color::Yellow))),
            ];
            let para = Paragraph::new(lines).wrap(Wrap { trim: false });
            f.render_widget(para, inner);
        }
        AddProviderStep::EnterModel { provider, model, .. } => {
            let default_model = CLOUD_PROVIDERS.iter()
                .find(|(id, _, _)| *id == provider.as_str())
                .map(|(_, _, def)| *def)
                .unwrap_or("(default)");
            let lines = vec![
                Line::from(vec![
                    Span::styled("Provider: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(provider.as_str(), Style::default().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    format!("Model name (leave blank for '{}')", default_model),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Model: ", Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw(if model.is_empty() { "[type here or press Enter for default]" } else { model.as_str() }),
                    Span::styled("|", Style::default().fg(Color::Yellow)),
                ]),
                Line::from(""),
                Line::from(Span::styled("Enter: Add  Esc: Cancel", Style::default().fg(Color::Yellow))),
            ];
            let para = Paragraph::new(lines).wrap(Wrap { trim: false });
            f.render_widget(para, inner);
        }
        // ── local ONNX path ───────────────────────────────────────────────────────────
        AddProviderStep::SelectLocalFamily { selected } => {
            const LOCAL_FAMILIES: &[(&str, &str)] = &[
                ("Qwen 2.5",   "Alibaba's general-purpose model — good default choice"),
                ("Gemma 2",    "Google's efficient model — strong reasoning"),
                ("Llama 3",    "Meta's open model — broad capability"),
                ("Mistral",    "Mistral AI — fast and efficient"),
                ("Phi",        "Microsoft's small model — great on low RAM"),
                ("DeepSeek",   "DeepSeek's distilled reasoning model"),
            ];
            let items: Vec<ListItem> = LOCAL_FAMILIES
                .iter()
                .enumerate()
                .map(|(i, (name, desc))| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let lines = vec![
                        Line::from(format!("{}{}{}", prefix, name, suffix)).style(style),
                        Line::from(format!("        {}", desc)).style(Style::default().fg(Color::DarkGray)),
                    ];
                    ListItem::new(lines)
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().title("Choose model family  ↑/↓: Move | Enter: Next | Esc: Back"));
            f.render_widget(list, inner);
        }
        AddProviderStep::SelectLocalSize { family, selected } => {
            const LOCAL_SIZES: &[(&str, &str)] = &[
                ("Small  (~1-3B)", "Best for 8 GB RAM — fast, uses less memory"),
                ("Medium (~3-9B)", "Best for 16 GB RAM — balanced quality"),
                ("Large  (~7-14B)", "Best for 32 GB RAM — higher quality"),
                ("XLarge (14B+)",  "Best for 64 GB RAM — maximum quality"),
            ];
            let family_name = family.name();
            let items: Vec<ListItem> = LOCAL_SIZES
                .iter()
                .enumerate()
                .map(|(i, (size, desc))| {
                    let is_sel = i == *selected;
                    let (prefix, suffix, style) = if is_sel {
                        (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let lines = vec![
                        Line::from(format!("{}{}{}", prefix, size, suffix)).style(style),
                        Line::from(format!("        {}", desc)).style(Style::default().fg(Color::DarkGray)),
                    ];
                    ListItem::new(lines)
                })
                .collect();
            let title = format!("Choose size for {}  ↑/↓: Move | Enter: Add | Esc: Back", family_name);
            let list = List::new(items).block(Block::default().title(title));
            f.render_widget(list, inner);
        }
        // ── network scan path ─────────────────────────────────────────────────────────
        AddProviderStep::Scanning { .. } => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "Scanning for Finch agents on local network…",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "(this takes up to 5 seconds)",
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(""),
                Line::from(Span::styled("Esc: Cancel", Style::default().fg(Color::Yellow))),
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
                        (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    } else {
                        ("    ", "", Style::default().fg(Color::Cyan))
                    };
                    let label = format!("{}{} @ {}:{}{}", prefix, agent.name, agent.host, agent.port, suffix);
                    let model_line = format!("        model: {}", agent.model);
                    let lines = vec![
                        Line::from(label).style(style),
                        Line::from(model_line).style(Style::default().fg(Color::DarkGray)),
                    ];
                    ListItem::new(lines)
                })
                .collect();
            let list = List::new(items)
                .block(Block::default().title("Discovered agents  ↑/↓: Move | Enter: Add | Esc: Cancel"));
            f.render_widget(list, inner);
        }
    }
}

/// Render Personas section
fn render_personas_section(
    f: &mut Frame,
    area: Rect,
    personas: &[PersonaInfo],
    selected_idx: usize,
    default_persona: &str,
    editing_prompt: bool,
    prompt_input: &str,
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
                        .add_modifier(Modifier::BOLD)
                )
            } else if is_default {
                ("★   ", "", Style::default().fg(Color::Yellow))
            } else {
                ("    ", "", Style::default())
            };

            ListItem::new(format!("{}{}{}", prefix, persona.name, suffix)).style(style)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Choose a Style"));

    f.render_widget(list, chunks[0]);

    // Right: Preview or edit
    if let Some(persona) = personas.get(selected_idx) {
        if editing_prompt {
            // Edit mode: show editable text area
            let edit_text = format!("{}\u{2502}", prompt_input); // show cursor as │
            let mut lines = vec![
                Line::from(Span::styled(
                    "Editing system prompt  (Ctrl+S: Save | Esc: Cancel)",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
            ];
            for line in edit_text.lines() {
                lines.push(Line::from(line.to_string()));
            }
            let edit_area = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Edit System Prompt")
                    .border_style(Style::default().fg(Color::Yellow)))
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
fn render_features_section(
    f: &mut Frame,
    area: Rect,
    auto_approve: bool,
    streaming: bool,
    debug: bool,
    hf_token: &str,
    editing_hf_token: bool,
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

    let title = Paragraph::new("Settings")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Build feature list: toggle-able booleans + HF token text field
    // Index mapping (non-macOS): 0=streaming, 1=auto_approve, 2=debug, 3=hf_token, 4=daemon, 5=mdns
    // Index mapping (macOS):     0=streaming, 1=auto_approve, 2=debug, 3=gui_auto, 4=hf_token, 5=daemon, 6=mdns

    #[cfg(not(target_os = "macos"))]
    let bool_features: Vec<(&str, bool, &str)> = vec![
        ("Live responses",        streaming,        "See Finch's answer as it types, word by word"),
        ("Skip permission prompts", auto_approve,   "Let Finch run tools without asking each time"),
        ("Debug logging",         debug,            "Write verbose logs to ~/.finch/debug.log"),
        // index 3 = HF token (handled separately below)
        ("Daemon-only mode",      daemon_only_mode, "Run as background server, no interactive REPL"),
        ("Advertise on network",  mdns_discovery,   "Broadcast this Finch instance via mDNS so others can discover it"),
    ];
    #[cfg(target_os = "macos")]
    let bool_features: Vec<(&str, bool, &str)> = vec![
        ("Live responses",        streaming,        "See Finch's answer as it types, word by word"),
        ("Skip permission prompts", auto_approve,   "Let Finch run tools without asking each time"),
        ("Debug logging",         debug,            "Write verbose logs to ~/.finch/debug.log"),
        ("GUI automation",        gui_automation,   "Allow tools to click/type in macOS apps"),
        // index 4 = HF token (handled separately)
        ("Daemon-only mode",      daemon_only_mode, "Run as background server, no interactive REPL"),
        ("Advertise on network",  mdns_discovery,   "Broadcast this Finch instance via mDNS so others can discover it"),
    ];

    #[cfg(not(target_os = "macos"))]
    let hf_idx = 3usize;
    #[cfg(target_os = "macos")]
    let hf_idx = 4usize;

    // Build list items interleaving bool features with the HF token row
    let mut items: Vec<ListItem> = Vec::new();
    let mut list_idx = 0usize; // tracks which visual row we're building

    for (feat_idx, (name, enabled, desc)) in bool_features.iter().enumerate() {
        // Insert HF token row before the appropriate bool feature
        if list_idx == hf_idx {
            let is_hf_selected = selected_idx == hf_idx;
            let (prefix, suffix, style) = if is_hf_selected {
                (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::Black).add_modifier(Modifier::BOLD))
            } else {
                ("    ", "", Style::default().fg(Color::Cyan))
            };
            let token_display = if editing_hf_token {
                format!("{}HF Token: {}|{}", prefix, hf_token, suffix)
            } else if hf_token.is_empty() {
                format!("{}HF Token: [not set — press E to enter]{}", prefix, suffix)
            } else {
                let masked = format!("{}...{}", &hf_token.chars().take(4).collect::<String>(),
                    hf_token.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());
                format!("{}HF Token: {}{}", prefix, masked, suffix)
            };
            let hf_lines = vec![
                Line::from(Span::styled(token_display, style)),
                Line::from(Span::styled("        For model downloads from HuggingFace", Style::default().fg(Color::DarkGray))),
            ];
            items.push(ListItem::new(hf_lines));
            list_idx += 1;
        }

        let is_selected = list_idx == selected_idx;
        let checkbox = if *enabled { "✅" } else { "☐" };
        let (prefix, suffix, name_style) = if is_selected {
            (">>> ", " <<<", Style::default().bg(Color::Black).fg(Color::White).add_modifier(Modifier::BOLD))
        } else {
            ("    ", "", if *enabled { Style::default().fg(Color::Blue) } else { Style::default().fg(Color::DarkGray) })
        };

        let feat_lines = vec![
            Line::from(vec![
                Span::raw(prefix),
                Span::raw(format!("{} ", checkbox)),
                Span::styled(*name, name_style.clone()),
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
    if hf_idx >= list_idx {
        let is_hf_selected = selected_idx == list_idx;
        let (prefix, suffix, style) = if is_hf_selected {
            (">>> ", " <<<", Style::default().fg(Color::White).bg(Color::Black).add_modifier(Modifier::BOLD))
        } else {
            ("    ", "", Style::default().fg(Color::Cyan))
        };
        let token_display = if editing_hf_token {
            format!("{}HF Token: {}|{}", prefix, hf_token, suffix)
        } else if hf_token.is_empty() {
            format!("{}HF Token: [not set — press E to enter]{}", prefix, suffix)
        } else {
            let masked = format!("{}...{}", &hf_token.chars().take(4).collect::<String>(),
                hf_token.chars().rev().take(4).collect::<String>().chars().rev().collect::<String>());
            format!("{}HF Token: {}{}", prefix, masked, suffix)
        };
        let hf_lines = vec![
            Line::from(Span::styled(token_display, style)),
            Line::from(Span::styled("        For model downloads from HuggingFace", Style::default().fg(Color::DarkGray))),
        ];
        items.push(ListItem::new(hf_lines));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Options"));
    f.render_widget(list, chunks[1]);

    let instructions_text = if editing_hf_token {
        "Type HuggingFace token | Enter/Esc: Done"
    } else {
        "↑/↓: Move | Space: Toggle | E: Edit HF token | Enter: Continue"
    };
    let instructions = Paragraph::new(instructions_text)
        .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(instructions, chunks[2]);
}

/// Render Review section
fn render_review_section(f: &mut Frame, area: Rect, state: &WizardState) {
    use crate::config::ColorTheme;

    let title = Paragraph::new("Ready to go!")
        .style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);

    // Build summary text
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Here's what you set up:", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
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
    if let Some(SectionState::Models { primary_model, .. }) = state.sections.get(&WizardSection::Models) {
        let ai_label = match primary_model {
            ModelConfig::Remote { api_key, .. } if !api_key.is_empty() => "Claude (API key configured)",
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
    if let Some(SectionState::Personas { default_persona, .. }) = state.sections.get(&WizardSection::Personas) {
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
    lines.push(Line::from(vec![
        Span::styled("Press Enter to save & start chatting", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Tab: Go back to change anything", Style::default().fg(Color::Gray)),
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
                                // User wants local model - skip provider selection if only one option
                                if inference_providers.len() <= 1 {
                                    step = WizardStep::ExecutionTargetSelection(selected_target_idx);
                                } else {
                                    step = WizardStep::InferenceProviderSelection(selected_provider_idx);
                                }
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
                            // Go back to provider selection (or enable local if only one provider)
                            if inference_providers.len() <= 1 {
                                step = WizardStep::EnableLocalModel(backend_enabled);
                            } else {
                                step = WizardStep::InferenceProviderSelection(selected_provider_idx);
                            }
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
                .fg(Color::White)
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
                Span::styled(family.name(), Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
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
                .fg(Color::White)
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
                Style::default().fg(Color::Blue)
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
                .fg(Color::White)
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
                    Span::styled(display_name, Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)),
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
                .fg(Color::White)
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
