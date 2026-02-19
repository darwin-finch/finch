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

/// Main sections of the tabbed wizard
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum WizardSection {
    ApiKeys,
    Backend,
    Teachers,
    Features,
    #[cfg(target_os = "macos")]
    Accessibility,
    Review,
}

impl WizardSection {
    fn all() -> Vec<Self> {
        vec![
            Self::ApiKeys,
            Self::Backend,
            Self::Teachers,
            Self::Features,
            #[cfg(target_os = "macos")]
            Self::Accessibility,
            Self::Review,
        ]
    }

    fn name(&self) -> &str {
        match self {
            Self::ApiKeys => "API Keys",
            Self::Backend => "Backend",
            Self::Teachers => "Teachers",
            Self::Features => "Features",
            #[cfg(target_os = "macos")]
            Self::Accessibility => "Accessibility",
            Self::Review => "Review",
        }
    }
}

/// State for each wizard section
#[derive(Debug, Clone)]
enum SectionState {
    ApiKeys {
        claude_key: String,
        hf_token: String,
        editing_field: ApiKeysField,
    },
    Backend {
        enabled: bool,
        provider_idx: usize,
        target_idx: usize,
        family_idx: usize,
        size_idx: usize,
        custom_repo: String,
        editing_field: BackendField,
    },
    Teachers {
        entries: Vec<TeacherEntry>,
        selected_idx: usize,
    },
    Features {
        auto_approve: bool,
        streaming: bool,
        debug: bool,
    },
    #[cfg(target_os = "macos")]
    Accessibility {
        gui_automation: bool,
    },
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ApiKeysField {
    ClaudeKey,
    HfToken,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BackendField {
    Enabled,
    Provider,
    Target,
    Family,
    Size,
    CustomRepo,
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
        // Initialize sections with default or pre-filled values
        let mut sections = HashMap::new();

        // API Keys section
        let claude_key = existing_config
            .and_then(|c| c.active_teacher())
            .map(|t| t.api_key.clone())
            .unwrap_or_default();
        sections.insert(
            WizardSection::ApiKeys,
            SectionState::ApiKeys {
                claude_key,
                hf_token: String::new(),
                editing_field: ApiKeysField::ClaudeKey,
            },
        );

        // Backend section
        let inference_providers = vec![
            InferenceProvider::Onnx,
            #[cfg(feature = "candle")]
            InferenceProvider::Candle,
        ];
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

        let provider_idx = existing_config
            .map(|c| {
                inference_providers
                    .iter()
                    .position(|p| *p == c.backend.inference_provider)
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let target_idx = existing_config
            .map(|c| {
                execution_targets
                    .iter()
                    .position(|t| *t == c.backend.execution_target)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        sections.insert(
            WizardSection::Backend,
            SectionState::Backend {
                enabled: existing_config.map(|c| c.backend.enabled).unwrap_or(true),
                provider_idx,
                target_idx,
                family_idx: 0,
                size_idx: 1, // Medium by default
                custom_repo: existing_config
                    .and_then(|c| c.backend.model_repo.clone())
                    .unwrap_or_default(),
                editing_field: BackendField::Enabled,
            },
        );

        // Teachers section
        let teachers = existing_config
            .map(|c| c.teachers.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| {
                vec![TeacherEntry {
                    provider: "claude".to_string(),
                    api_key: String::new(),
                    model: None,
                    base_url: None,
                    name: Some("Claude (Primary)".to_string()),
                }]
            });
        sections.insert(
            WizardSection::Teachers,
            SectionState::Teachers {
                entries: teachers,
                selected_idx: 0,
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
            },
        );

        // Accessibility section (macOS only)
        #[cfg(target_os = "macos")]
        sections.insert(
            WizardSection::Accessibility,
            SectionState::Accessibility {
                gui_automation: existing_config
                    .map(|c| c.features.gui_automation)
                    .unwrap_or(false),
            },
        );

        // Review section
        sections.insert(WizardSection::Review, SectionState::Review);

        Self {
            current_section: WizardSection::ApiKeys,
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
    pub claude_api_key: String,
    pub hf_token: Option<String>,
    pub backend_enabled: bool,
    pub inference_provider: InferenceProvider,
    pub execution_target: ExecutionTarget,
    pub model_family: ModelFamily,
    pub model_size: ModelSize,
    pub custom_model_repo: Option<String>,
    pub teachers: Vec<TeacherEntry>,
    // Feature flags
    pub auto_approve_tools: bool,
    pub streaming_enabled: bool,
    pub debug_logging: bool,
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
        WizardSection::ApiKeys => handle_api_keys_input(state, key),
        WizardSection::Backend => handle_backend_input(state, key),
        WizardSection::Teachers => handle_teachers_input(state, key),
        WizardSection::Features => handle_features_input(state, key),
        #[cfg(target_os = "macos")]
        WizardSection::Accessibility => handle_accessibility_input(state, key),
        WizardSection::Review => handle_review_input(state, key),
    }
}

/// Handle input for API Keys section
fn handle_api_keys_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::ApiKeys {
        claude_key,
        hf_token,
        editing_field,
    }) = state.sections.get_mut(&WizardSection::ApiKeys)
    {
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                // Toggle between fields
                *editing_field = match editing_field {
                    ApiKeysField::ClaudeKey => ApiKeysField::HfToken,
                    ApiKeysField::HfToken => ApiKeysField::ClaudeKey,
                };
            }
            KeyCode::Char(c) => {
                match editing_field {
                    ApiKeysField::ClaudeKey => claude_key.push(c),
                    ApiKeysField::HfToken => hf_token.push(c),
                }
            }
            KeyCode::Backspace => {
                match editing_field {
                    ApiKeysField::ClaudeKey => {
                        claude_key.pop();
                    }
                    ApiKeysField::HfToken => {
                        hf_token.pop();
                    }
                }
            }
            KeyCode::Enter => {
                // Mark as completed if Claude key is provided
                if !claude_key.is_empty() {
                    state.mark_completed(WizardSection::ApiKeys);
                    state.next_section();
                }
            }
            KeyCode::Esc => {
                anyhow::bail!("Setup cancelled");
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Backend section
fn handle_backend_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Backend {
        enabled,
        provider_idx,
        target_idx,
        family_idx,
        size_idx,
        custom_repo,
        editing_field,
    }) = state.sections.get_mut(&WizardSection::Backend)
    {
        match key.code {
            KeyCode::Up => {
                // Navigate fields up
                *editing_field = match editing_field {
                    BackendField::Enabled => BackendField::CustomRepo,
                    BackendField::Provider => BackendField::Enabled,
                    BackendField::Target => BackendField::Provider,
                    BackendField::Family => BackendField::Target,
                    BackendField::Size => BackendField::Family,
                    BackendField::CustomRepo => BackendField::Size,
                };
            }
            KeyCode::Down => {
                // Navigate fields down
                *editing_field = match editing_field {
                    BackendField::Enabled => BackendField::Provider,
                    BackendField::Provider => BackendField::Target,
                    BackendField::Target => BackendField::Family,
                    BackendField::Family => BackendField::Size,
                    BackendField::Size => BackendField::CustomRepo,
                    BackendField::CustomRepo => BackendField::Enabled,
                };
            }
            KeyCode::Left => {
                // Decrease selected index for current field
                match editing_field {
                    BackendField::Provider => {
                        if *provider_idx > 0 {
                            *provider_idx -= 1;
                        }
                    }
                    BackendField::Target => {
                        if *target_idx > 0 {
                            *target_idx -= 1;
                        }
                    }
                    BackendField::Family => {
                        if *family_idx > 0 {
                            *family_idx -= 1;
                        }
                    }
                    BackendField::Size => {
                        if *size_idx > 0 {
                            *size_idx -= 1;
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Right => {
                // Increase selected index for current field
                match editing_field {
                    BackendField::Provider => {
                        if *provider_idx < state.inference_providers.len() - 1 {
                            *provider_idx += 1;
                        }
                    }
                    BackendField::Target => {
                        if *target_idx < state.execution_targets.len() - 1 {
                            *target_idx += 1;
                        }
                    }
                    BackendField::Family => {
                        if *family_idx < state.model_families.len() - 1 {
                            *family_idx += 1;
                        }
                    }
                    BackendField::Size => {
                        if *size_idx < state.model_sizes.len() - 1 {
                            *size_idx += 1;
                        }
                    }
                    _ => {}
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('t') => {
                // Toggle enabled field
                if matches!(editing_field, BackendField::Enabled) {
                    *enabled = !*enabled;
                }
            }
            KeyCode::Char(c) => {
                // Edit custom repo
                if matches!(editing_field, BackendField::CustomRepo) {
                    custom_repo.push(c);
                }
            }
            KeyCode::Backspace => {
                if matches!(editing_field, BackendField::CustomRepo) {
                    custom_repo.pop();
                }
            }
            KeyCode::Enter => {
                // Mark as completed
                state.mark_completed(WizardSection::Backend);
                state.next_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Teachers section
fn handle_teachers_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Teachers {
        entries,
        selected_idx,
    }) = state.sections.get_mut(&WizardSection::Teachers)
    {
        match key.code {
            KeyCode::Up => {
                if *selected_idx > 0 {
                    *selected_idx -= 1;
                }
            }
            KeyCode::Down => {
                if *selected_idx < entries.len().saturating_sub(1) {
                    *selected_idx += 1;
                }
            }
            KeyCode::Enter => {
                // Mark as completed if at least one teacher configured
                if !entries.is_empty() && !entries[0].api_key.is_empty() {
                    state.mark_completed(WizardSection::Teachers);
                    state.next_section();
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Features section
fn handle_features_input(state: &mut WizardState, key: crossterm::event::KeyEvent) -> Result<bool> {
    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
    }) = state.sections.get_mut(&WizardSection::Features)
    {
        match key.code {
            KeyCode::Char('1') | KeyCode::Char(' ') => {
                *auto_approve = !*auto_approve;
            }
            KeyCode::Char('2') => {
                *streaming = !*streaming;
            }
            KeyCode::Char('3') => {
                *debug = !*debug;
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Features);
                state.next_section();
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Handle input for Accessibility section (macOS only)
#[cfg(target_os = "macos")]
fn handle_accessibility_input(
    state: &mut WizardState,
    key: crossterm::event::KeyEvent,
) -> Result<bool> {
    if let Some(SectionState::Accessibility { gui_automation }) =
        state.sections.get_mut(&WizardSection::Accessibility)
    {
        match key.code {
            KeyCode::Char(' ') | KeyCode::Char('1') => {
                *gui_automation = !*gui_automation;
            }
            KeyCode::Enter => {
                state.mark_completed(WizardSection::Accessibility);
                state.next_section();
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
    // Extract API keys
    let (claude_key, hf_token) = if let Some(SectionState::ApiKeys {
        claude_key,
        hf_token,
        ..
    }) = state.sections.get(&WizardSection::ApiKeys)
    {
        (claude_key.clone(), if hf_token.is_empty() { None } else { Some(hf_token.clone()) })
    } else {
        anyhow::bail!("API keys not configured");
    };

    // Extract backend config
    let (backend_enabled, provider_idx, target_idx, family_idx, size_idx, custom_repo) =
        if let Some(SectionState::Backend {
            enabled,
            provider_idx,
            target_idx,
            family_idx,
            size_idx,
            custom_repo,
            ..
        }) = state.sections.get(&WizardSection::Backend)
        {
            (
                *enabled,
                *provider_idx,
                *target_idx,
                *family_idx,
                *size_idx,
                if custom_repo.is_empty() {
                    None
                } else {
                    Some(custom_repo.clone())
                },
            )
        } else {
            anyhow::bail!("Backend not configured");
        };

    // Extract teachers
    let teachers = if let Some(SectionState::Teachers { entries, .. }) =
        state.sections.get(&WizardSection::Teachers)
    {
        // Fill first teacher's API key from claude_key if empty
        let mut teachers = entries.clone();
        if !teachers.is_empty() && teachers[0].api_key.is_empty() {
            teachers[0].api_key = claude_key.clone();
        }
        teachers
    } else {
        vec![TeacherEntry {
            provider: "claude".to_string(),
            api_key: claude_key.clone(),
            model: None,
            base_url: None,
            name: Some("Claude (Primary)".to_string()),
        }]
    };

    // Extract features
    let (auto_approve, streaming, debug) = if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
    }) = state.sections.get(&WizardSection::Features)
    {
        (*auto_approve, *streaming, *debug)
    } else {
        (false, true, false)
    };

    Ok(SetupResult {
        claude_api_key: claude_key,
        hf_token,
        backend_enabled,
        inference_provider: state.inference_providers[provider_idx],
        execution_target: state.execution_targets[target_idx],
        model_family: state.model_families[family_idx],
        model_size: state.model_sizes[size_idx],
        custom_model_repo: custom_repo,
        teachers,
        auto_approve_tools: auto_approve,
        streaming_enabled: streaming,
        debug_logging: debug,
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
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    // Render current section content
    render_section_content(f, chunks[1], state);

    // Render help text
    let help_text = match state.current_section {
        WizardSection::ApiKeys => "Tab/Arrows: Navigate sections | ↑/↓: Switch fields | Enter: Next | Esc: Cancel",
        WizardSection::Backend => "Tab/Arrows: Navigate sections | ↑/↓: Navigate fields | ←/→: Change values | Enter: Next",
        WizardSection::Teachers => "Tab/Arrows: Navigate sections | ↑/↓: Select teacher | Enter: Next",
        WizardSection::Features => "Tab/Arrows: Navigate sections | 1/2/3: Toggle features | Enter: Next",
        #[cfg(target_os = "macos")]
        WizardSection::Accessibility => "Tab/Arrows: Navigate sections | Space: Toggle | Enter: Next",
        WizardSection::Review => "Tab/Arrows: Navigate sections | Enter/y: Confirm | n/Esc: Cancel",
    };

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

/// Render the content area for the current section
fn render_section_content(f: &mut Frame, area: Rect, state: &WizardState) {
    let section_state = state.sections.get(&state.current_section);

    match section_state {
        Some(SectionState::ApiKeys {
            claude_key,
            hf_token,
            editing_field,
        }) => render_api_keys_section(f, area, claude_key, hf_token, *editing_field),
        Some(SectionState::Backend {
            enabled,
            provider_idx,
            target_idx,
            family_idx,
            size_idx,
            custom_repo,
            editing_field,
        }) => render_backend_section(
            f,
            area,
            *enabled,
            *provider_idx,
            *target_idx,
            *family_idx,
            *size_idx,
            custom_repo,
            *editing_field,
            state,
        ),
        Some(SectionState::Teachers {
            entries,
            selected_idx,
        }) => render_teachers_section(f, area, entries, *selected_idx),
        Some(SectionState::Features {
            auto_approve,
            streaming,
            debug,
        }) => render_features_section(f, area, *auto_approve, *streaming, *debug),
        #[cfg(target_os = "macos")]
        Some(SectionState::Accessibility { gui_automation }) => {
            render_accessibility_section(f, area, *gui_automation)
        }
        Some(SectionState::Review) => render_review_section(f, area, state),
        None => {
            let error = Paragraph::new("Error: Section state not found")
                .style(Style::default().fg(Color::Red));
            f.render_widget(error, area);
        }
    }
}

/// Render API Keys section
fn render_api_keys_section(
    f: &mut Frame,
    area: Rect,
    claude_key: &str,
    hf_token: &str,
    editing_field: ApiKeysField,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Length(4), // Claude key field
            Constraint::Length(4), // HF token field
            Constraint::Min(1),    // Instructions
        ])
        .split(area);

    let title = Paragraph::new("API Keys Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    // Claude API key
    let claude_style = if matches!(editing_field, ApiKeysField::ClaudeKey) {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let claude_display = if claude_key.len() > 60 {
        format!("{}...{} ({} chars)", &claude_key[..30], &claude_key[claude_key.len()-10..], claude_key.len())
    } else if !claude_key.is_empty() {
        claude_key.to_string()
    } else {
        "[Enter Claude API key]".to_string()
    };
    let claude_widget = Paragraph::new(claude_display)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Claude API Key (Required)")
                .border_style(claude_style),
        )
        .style(claude_style);
    f.render_widget(claude_widget, chunks[1]);

    // HuggingFace token
    let hf_style = if matches!(editing_field, ApiKeysField::HfToken) {
        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let hf_display = if !hf_token.is_empty() {
        hf_token.to_string()
    } else {
        "[Optional - for model downloads]".to_string()
    };
    let hf_widget = Paragraph::new(hf_display)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("HuggingFace Token (Optional)")
                .border_style(hf_style),
        )
        .style(hf_style);
    f.render_widget(hf_widget, chunks[2]);

    let instructions = Paragraph::new(
        "Enter your API keys. Press ↑/↓ to switch between fields.\n\
         Claude API key is required. HF token is optional but recommended for model downloads."
    )
    .style(Style::default().fg(Color::Yellow))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[3]);
}

/// Render Backend section
fn render_backend_section(
    f: &mut Frame,
    area: Rect,
    enabled: bool,
    provider_idx: usize,
    target_idx: usize,
    family_idx: usize,
    size_idx: usize,
    custom_repo: &str,
    editing_field: BackendField,
    state: &WizardState,
) {
    let title = Paragraph::new("Backend Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);

    let content = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::Enabled) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Enable local model: {}",
                if enabled { "Yes ✓" } else { "No" }
            )),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::Provider) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Provider: {}",
                state.inference_providers[provider_idx].name()
            )),
        ]),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::Target) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Target: {}",
                state.execution_targets[target_idx].name()
            )),
        ]),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::Family) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Model Family: {}",
                state.model_families[family_idx].name()
            )),
        ]),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::Size) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Model Size: {}",
                match state.model_sizes[size_idx] {
                    ModelSize::Small => "Small (~1-3B)",
                    ModelSize::Medium => "Medium (~3-9B) ★",
                    ModelSize::Large => "Large (~7-14B)",
                    ModelSize::XLarge => "XLarge (~14B+)",
                }
            )),
        ]),
        Line::from(vec![
            Span::styled(
                if matches!(editing_field, BackendField::CustomRepo) {
                    "▸ "
                } else {
                    "  "
                },
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(format!(
                "Custom repo: {}",
                if custom_repo.is_empty() {
                    "(auto)"
                } else {
                    custom_repo
                }
            )),
        ]),
    ];

    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    f.render_widget(title, chunks[0]);

    let para = Paragraph::new(content).wrap(Wrap { trim: false });
    f.render_widget(para, chunks[1]);
}

/// Render Teachers section
fn render_teachers_section(
    f: &mut Frame,
    area: Rect,
    entries: &[TeacherEntry],
    selected_idx: usize,
) {
    let title = Paragraph::new("Teacher Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(idx, teacher)| {
            let display_name = teacher.name.as_deref().unwrap_or(&teacher.provider);
            let priority = if idx == 0 { "PRIMARY" } else { "FALLBACK" };
            let style = if idx == 0 {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::Yellow)
            };

            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{}. ", idx + 1),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(display_name, Style::default().fg(Color::White)),
                    Span::raw("  "),
                    Span::styled(priority, style),
                ]),
                Line::from(vec![
                    Span::raw("   Provider: "),
                    Span::styled(&teacher.provider, Style::default().fg(Color::Gray)),
                ]),
            ])
        })
        .collect();

    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    f.render_widget(title, chunks[0]);

    let mut list_state = ListState::default();
    list_state.select(Some(selected_idx));

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");

    f.render_stateful_widget(list, chunks[1], &mut list_state);
}

/// Render Features section
fn render_features_section(
    f: &mut Frame,
    area: Rect,
    auto_approve: bool,
    streaming: bool,
    debug: bool,
) {
    // Reuse the existing render_features_config function
    render_features_config(f, area, auto_approve, streaming, debug);
}

/// Render Accessibility section (macOS only)
#[cfg(target_os = "macos")]
fn render_accessibility_section(f: &mut Frame, area: Rect, gui_automation: bool) {
    let title = Paragraph::new("Accessibility (macOS)")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);

    let checkbox = if gui_automation { "☑" } else { "☐" };
    let content = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("1. ", Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{} Enable GUI automation tools", checkbox),
                if gui_automation {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Gray)
                },
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "Enables GuiClick, GuiType, GuiInspect tools",
                Style::default().fg(Color::Gray),
            ),
        ]),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "⚠️  Requires Accessibility permissions",
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "System Preferences > Security & Privacy > Privacy > Accessibility",
                Style::default().fg(Color::Gray).add_modifier(Modifier::ITALIC),
            ),
        ]),
    ];

    let block = Block::default().borders(Borders::ALL);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(inner);

    f.render_widget(title, chunks[0]);

    let para = Paragraph::new(content).wrap(Wrap { trim: false });
    f.render_widget(para, chunks[1]);
}

/// Render Review section
fn render_review_section(f: &mut Frame, area: Rect, state: &WizardState) {
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

    // API Keys
    if let Some(SectionState::ApiKeys { claude_key, hf_token, .. }) = state.sections.get(&WizardSection::ApiKeys) {
        lines.push(Line::from(vec![
            Span::styled("API Keys: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("Claude key configured ({}), HF token: {}",
                if claude_key.is_empty() { "missing" } else { "set" },
                if hf_token.is_empty() { "not set" } else { "set" }
            )),
        ]));
    }

    // Backend
    if let Some(SectionState::Backend { enabled, provider_idx, target_idx, family_idx, size_idx, .. }) = state.sections.get(&WizardSection::Backend) {
        lines.push(Line::from(vec![
            Span::styled("Backend: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("{}, {}, {}, {}",
                if *enabled { "Enabled" } else { "Disabled" },
                state.inference_providers[*provider_idx].name(),
                state.execution_targets[*target_idx].name(),
                state.model_families[*family_idx].name(),
            )),
        ]));
    }

    // Teachers
    if let Some(SectionState::Teachers { entries, .. }) = state.sections.get(&WizardSection::Teachers) {
        lines.push(Line::from(vec![
            Span::styled("Teachers: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!("{} configured", entries.len())),
        ]));
    }

    // Features
    if let Some(SectionState::Features { auto_approve, streaming, debug }) = state.sections.get(&WizardSection::Features) {
        lines.push(Line::from(vec![
            Span::styled("Features: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!(
                "Auto-approve: {}, Streaming: {}, Debug: {}",
                if *auto_approve { "Yes" } else { "No" },
                if *streaming { "Yes" } else { "No" },
                if *debug { "Yes" } else { "No" }
            )),
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
                            return Ok(SetupResult {
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
                                auto_approve_tools,
                                streaming_enabled,
                                debug_logging,
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
