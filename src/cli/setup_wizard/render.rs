//! Wizard view props: what the widget host paints, one function per section.
//!
//! #812: this file no longer paints. The second painter is gone — each
//! function converts `WizardState` into plain styled lines and overlay-card
//! props ([`WizardView`], [`WizardCard`]), and `crate::cli::tui::wizard_host`
//! claims the frame, records the shadow buffer, and blits. The lines are the
//! speakable canonical form, so a GUI host (#808) can consume the same props.
//!
//! The windowing helpers here count physical rows with the same shadow-buffer
//! arithmetic the host's claiming pass uses, so the one list that must keep a
//! selected row visible (the old painter's `ListState` guarantee) is windowed
//! with the same budget the tree will claim.

use super::chatgpt_recovery::{chatgpt_setup_failure_cause, chatgpt_setup_failure_summary};
use super::grok_recovery::{grok_setup_failure_cause, grok_setup_failure_summary};
use super::*;
use crate::cli::tui::WizardColor as Color;
use crate::cli::tui::{
    wizard_bold, wizard_boxed, wizard_centered, wizard_line, wizard_paint, wizard_physical_rows,
    wizard_plain, WizardCard, WizardSectionContent, WizardView,
};

// ─── Small shared helpers ────────────────────────────────────────────────────

/// `sk-abcdef...wxyz` — the only part of a secret a screen needs to show.
pub(super) fn mask_secret(value: &str, keep_start: usize, keep_end: usize) -> String {
    let tail: String = value
        .chars()
        .rev()
        .take(keep_end)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{}...{}",
        value.chars().take(keep_start).collect::<String>(),
        tail
    )
}

/// Wrapped rows one logical line occupies at `width`.
///
/// The host's claiming pass and the view's windowing must count rows with the
/// same terminal-accurate arithmetic (#926): emoji-presentation characters
/// render two columns, and a one-column disagreement shifts a whole frame.
fn rows_of(line: &str, width: usize) -> usize {
    wizard_physical_rows(line, width)
}

/// `Auto`, `CPU`, or `CoreML (all)` — the execution-target display.
pub(super) fn execution_target_display(execution: ExecutionTarget, coreml: CoreMlConfig) -> String {
    #[cfg(target_os = "macos")]
    if execution == ExecutionTarget::CoreML {
        return format!("CoreML ({})", coreml.compute_units.name());
    }

    execution.name().to_string()
}

pub(super) fn format_catalog_refresh_time(
    refreshed_at: &DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
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

pub(super) fn format_catalog_label(
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
            STATIC_FALLBACK_AS_OF
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

/// Skip the first `skip` wrapped rows of `lines`, by whole logical lines.
fn skip_wrapped_rows(lines: &[String], skip: usize, width: usize) -> Vec<String> {
    let mut used = 0usize;
    let mut start = 0usize;
    for line in lines {
        let rows = rows_of(line, width);
        if used + rows <= skip {
            used += rows;
            start += 1;
        } else {
            break;
        }
    }
    lines[start.min(lines.len())..].to_vec()
}

/// Keep the head of `lines` that fits `budget` wrapped rows.
fn head_fitting_rows(lines: &[String], budget: usize, width: usize) -> Vec<String> {
    let mut used = 0usize;
    let mut out = Vec::new();
    for line in lines {
        let rows = rows_of(line, width);
        if used + rows > budget {
            break;
        }
        used += rows;
        out.push(line.clone());
    }
    out
}

// ─── Tab row and help ────────────────────────────────────────────────────────

fn tab_titles(state: &WizardState) -> Vec<String> {
    WizardSection::all()
        .iter()
        .map(|section| {
            if state.is_completed(*section) {
                format!("{} ✓", section.name())
            } else {
                section.name().to_string()
            }
        })
        .collect()
}

fn help_line(state: &WizardState, width: usize) -> String {
    let section_help = match state.current_section {
        WizardSection::Themes => "↑/↓: Choose theme | Enter: Next",
        WizardSection::Models => "Enter: Edit provider | A: Add | D: Remove",
        WizardSection::LocalHelpers => "Space: Toggle | Enter: Next",
        WizardSection::Personas => "↑/↓: Choose style | E: Edit prompt | Enter: Next",
        WizardSection::Features => "↑/↓: Navigate | Space: Toggle | Enter: Next",
        WizardSection::Review => "Enter: Save & start",
    };
    let text = format!("{section_help} | Ctrl+S: Save | Esc: Back | Tab: Next | Ctrl+C: Cancel");
    wizard_centered(&wizard_bold(&text, Color::Blue), width)
}

// ─── Section content ─────────────────────────────────────────────────────────

/// Themes section: list, preview, instructions.
fn themes_section_lines(selected_theme: usize, width: usize) -> Vec<String> {
    use crate::theme::ColorTheme;

    let mut lines = vec![wizard_centered(
        &wizard_bold("Theme Selection", Color::Blue),
        width,
    )];

    let themes = ColorTheme::all();
    let items: Vec<String> = themes
        .iter()
        .enumerate()
        .map(|(index, theme)| {
            if index == selected_theme {
                wizard_bold(
                    &format!(">>> {} - {} <<<", theme.name(), theme.description()),
                    Color::White,
                )
            } else {
                wizard_line(
                    &format!("    {} - {}", theme.name(), theme.description()),
                    Color::Blue,
                )
            }
        })
        .collect();
    lines.extend(wizard_boxed("Available Themes", &items, Color::Blue, width));

    // Preview of the selected theme, in the theme's own colours.
    let preview_theme = themes[selected_theme].to_scheme();
    let preview = vec![
        format!(
            "{}{}",
            wizard_line("User: ", preview_theme.messages.user.to_color().into()),
            wizard_plain("What is 2+2?")
        ),
        format!(
            "{}{}",
            wizard_line(
                "Assistant: ",
                preview_theme.messages.assistant.to_color().into()
            ),
            wizard_plain("The answer is 4.")
        ),
        format!(
            "{}{}",
            wizard_line("🔧 Tool: ", preview_theme.messages.tool.to_color().into()),
            wizard_plain("Reading file...")
        ),
        format!(
            "{}{}",
            wizard_line("❌ Error: ", preview_theme.messages.error.to_color().into()),
            wizard_plain("File not found")
        ),
    ];
    lines.extend(wizard_boxed("Preview", &preview, Color::Blue, width));

    lines.push(wizard_bold(
        "Use ↑/↓ arrow keys to move selection (>>> theme <<<)",
        Color::Blue,
    ));
    lines.push(wizard_bold(
        "Selected theme shows with white background. Press Enter to confirm.",
        Color::Blue,
    ));
    lines
}

/// Local Helpers section: the separate local-only model choice for
/// finch-builtin functions (memory embeddings today), distinct from the
/// "Model Setup" tab's chat-provider configuration.
fn local_helpers_section_lines(use_neural_embeddings: bool, width: usize) -> Vec<String> {
    let mut lines = vec![wizard_centered(
        &wizard_bold("Local Helper Models", Color::Blue),
        width,
    )];
    lines.push(wizard_line(
        "Separate from the chat model above: these are the local-only models \
         finch's own built-in features use for themselves.",
        Color::DarkGray,
    ));

    let checkbox = if use_neural_embeddings { "[x]" } else { "[ ]" };
    let item = wizard_bold(
        &format!(">>> {checkbox} Memory embeddings: use the neural model <<<"),
        Color::White,
    );
    lines.extend(wizard_boxed("Memory", &[item], Color::Blue, width));

    let detail = if use_neural_embeddings {
        "On: all-MiniLM-L6-v2 (ONNX), downloaded once from Hugging Face on \
         first use, then runs locally with no further network calls. Better \
         recall quality than the fallback below."
    } else {
        "Off: built-in TF-IDF embeddings. No download, no network access, \
         ever -- at lower recall quality than the neural model."
    };
    lines.push(String::new());
    lines.push(wizard_line(detail, Color::DarkGray));
    lines
}

/// The display text for one provider row: primary marker, tool checkbox, and
/// masked key state — the exact shapes the old painter rendered.
fn provider_row_display(model: &ModelConfig, coreml: CoreMlConfig, primary: bool) -> String {
    if primary {
        return match model {
            ModelConfig::Local {
                family,
                size,
                execution,
                ..
            } => format!(
                "★ Primary: Local {} {} ({})",
                family.name(),
                model_size_display(size),
                execution_target_display(*execution, coreml)
            ),
            ModelConfig::Remote {
                provider,
                name,
                api_key,
                model,
                ..
            } => {
                let key_display = if provider.eq_ignore_ascii_case("chatgpt")
                    || provider.eq_ignore_ascii_case("grok-sub")
                {
                    "Named device credential".to_string()
                } else if api_key.is_empty() {
                    "[Not configured]".to_string()
                } else {
                    mask_secret(api_key, 10, 4)
                };
                let model_display = if model.is_empty() {
                    String::new()
                } else {
                    format!(" - {model}")
                };
                format!("★ Primary: {name}{model_display} [{key_display}]")
            }
        };
    }
    let checkbox = if model.enabled() { "☑" } else { "☐" };
    match model {
        ModelConfig::Local { family, size, .. } => format!(
            "{checkbox} Tool: Local {} {}",
            family.name(),
            model_size_display(size)
        ),
        ModelConfig::Remote { name, model, .. } => {
            let model_display = if model.is_empty() {
                String::new()
            } else {
                format!(" - {model}")
            };
            format!("{checkbox} Tool: {name}{model_display}")
        }
    }
}

fn marked_row(display: &str, selected: bool, enabled: bool) -> String {
    if selected {
        wizard_bold(&format!(">>> {display} <<<"), Color::White)
    } else if enabled {
        wizard_plain(&format!("    {display}"))
    } else {
        wizard_line(&format!("    {display}"), Color::DarkGray)
    }
}

/// Models section: description, provider list, edit panel, instructions, error.
#[allow(clippy::too_many_arguments)]
fn models_section_lines(
    coreml: CoreMlConfig,
    primary_model: &ModelConfig,
    tool_models: &[ModelConfig],
    selected_idx: usize,
    editing_mode: bool,
    editing_model_mode: bool,
    model_input: &str,
    error: Option<&str>,
    width: usize,
) -> Vec<String> {
    let mut lines = vec![wizard_centered(
        &wizard_bold("AI Providers", Color::Cyan),
        width,
    )];

    let has_key = match primary_model {
        ModelConfig::Remote {
            provider,
            api_key,
            persisted,
            ..
        } if provider.eq_ignore_ascii_case("chatgpt")
            || provider.eq_ignore_ascii_case("grok-sub") =>
        {
            matches!(persisted, Some(ProviderEntry::Credentialed { .. }))
        }
        ModelConfig::Remote { api_key, .. } => !api_key.is_empty(),
        ModelConfig::Local { .. } => true,
    };
    let description_text = match primary_model {
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("grok-sub") => {
            "Grok subscription uses SuperGrok entitlement via device sign-in; xAI Console API keys are a separate provider and are never used automatically."
                .to_string()
        }
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("chatgpt") => {
            "ChatGPT subscription uses a named Finch device credential; OpenAI Platform API keys are separate."
                .to_string()
        }
        _ if has_key => format!(
            "Primary provider configured. Press A to add more providers ({} total).",
            1 + tool_models.len()
        ),
        _ => "Paste your API key below (E), or add a provider with A.\n\
              No key yet? Get one at console.anthropic.com/keys"
            .to_string(),
    };
    for text in description_text.split('\n') {
        lines.push(wizard_centered(
            &wizard_line(text.trim(), Color::Blue),
            width,
        ));
    }

    let mut list_rows = Vec::new();
    for (index, model) in std::iter::once(primary_model)
        .chain(tool_models.iter())
        .enumerate()
    {
        let display = provider_row_display(model, coreml, index == 0);
        list_rows.push(marked_row(&display, selected_idx == index, model.enabled()));
    }
    lines.extend(wizard_boxed("AI Providers", &list_rows, Color::Blue, width));

    let selected_accepts_api_key = if selected_idx == 0 {
        primary_model.accepts_api_key()
    } else {
        tool_models
            .get(selected_idx.saturating_sub(1))
            .is_some_and(ModelConfig::accepts_api_key)
    };
    if editing_mode && selected_accepts_api_key {
        let current_key = if selected_idx == 0 {
            match primary_model {
                ModelConfig::Remote { api_key, .. } => api_key.as_str(),
                ModelConfig::Local { .. } => "",
            }
        } else {
            match tool_models.get(selected_idx.saturating_sub(1)) {
                Some(ModelConfig::Remote { api_key, .. }) => api_key.as_str(),
                _ => "",
            }
        };
        lines.extend(wizard_boxed(
            "Edit API Key",
            &[wizard_plain(&format!("{current_key}█"))],
            Color::Yellow,
            width,
        ));
    } else if editing_mode {
        lines.extend(wizard_boxed(
            "Subscription authentication",
            &[wizard_plain(
                "Named Finch device credential; no API key input",
            )],
            Color::Yellow,
            width,
        ));
    } else if editing_model_mode {
        lines.extend(wizard_boxed(
            "Edit Model",
            &[wizard_plain(&format!("{model_input}█"))],
            Color::Yellow,
            width,
        ));
    } else {
        lines.push(wizard_centered(
            &wizard_line(
                "Press Enter to edit the selected provider · P for primary",
                Color::DarkGray,
            ),
            width,
        ));
    }

    let instructions_text = if editing_mode || editing_model_mode {
        "Type here | Enter/Esc: Save & return"
    } else {
        "Enter: Edit | P: Primary | A: Add | D: Remove | Tab: Next"
    };
    lines.push(wizard_centered(
        &wizard_bold(instructions_text, Color::Yellow),
        width,
    ));
    if let Some(error) = error {
        lines.push(wizard_centered(&wizard_line(error, Color::Red), width));
    }
    lines
}

/// Personas section: style list, then preview or the prompt editor. The old
/// painter placed list and preview side by side; the claiming tree hosts one
/// column, so the preview reads below the list — the same order a screen
/// reader speaks them.
#[allow(clippy::too_many_arguments)]
fn personas_section_lines(
    personas: &[PersonaInfo],
    selected_idx: usize,
    default_persona: &str,
    editing_prompt: bool,
    prompt_input: &str,
    cursor_pos: usize,
    width: usize,
) -> Vec<String> {
    let mut lines = Vec::new();
    let rows: Vec<String> = personas
        .iter()
        .enumerate()
        .map(|(index, persona)| {
            let is_default = persona.name.to_lowercase() == default_persona.to_lowercase();
            if index == selected_idx {
                wizard_bold(&format!(">>> {} <<<", persona.name), Color::White)
            } else if is_default {
                wizard_line(&format!("★   {}", persona.name), Color::Yellow)
            } else {
                wizard_plain(&format!("    {}", persona.name))
            }
        })
        .collect();
    lines.extend(wizard_boxed("Choose a Style", &rows, Color::Blue, width));

    let Some(persona) = personas.get(selected_idx) else {
        return lines;
    };
    if editing_prompt {
        let before: String = prompt_input.chars().take(cursor_pos).collect();
        let after: String = prompt_input.chars().skip(cursor_pos + 1).collect();
        let mut body = vec![wizard_bold(
            "Editing system prompt  (Ctrl+S: Save | Esc: Cancel)",
            Color::Yellow,
        )];
        body.push(String::new());
        for text in format!("{before}\u{2588}{after}").split('\n') {
            body.push(wizard_plain(text));
        }
        lines.extend(wizard_boxed(
            "Edit System Prompt",
            &body,
            Color::Yellow,
            width,
        ));
    } else {
        let preview = vec![
            format!(
                "{}{}",
                wizard_paint("Name: ", None, true),
                wizard_plain(&persona.name)
            ),
            String::new(),
            format!(
                "{}{}",
                wizard_paint("Description: ", None, true),
                wizard_plain(&persona.description)
            ),
            String::new(),
            wizard_paint("System Prompt:", None, true),
            String::new(),
            wizard_plain(&persona.system_prompt),
            String::new(),
            wizard_line("E: Edit system prompt", Color::DarkGray),
        ];
        lines.extend(wizard_boxed("Preview", &preview, Color::Blue, width));
    }
    lines
}

/// The status lines the GUI-automation surfaces show. Speakable by contract
/// (Key Principle 5): every outcome names the key that fixes it.
#[cfg(target_os = "macos")]
pub(super) fn gui_automation_status_lines(
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

/// One settings row group: the toggle/edit line and its dim description.
fn feature_group(
    selected: bool,
    enabled: Option<bool>,
    name: &str,
    description: &str,
) -> Vec<String> {
    let checkbox = match enabled {
        Some(true) => "✅ ",
        Some(false) => "☐ ",
        None => "",
    };
    let name_line = if selected {
        wizard_bold(&format!(">>> {checkbox}{name} <<<"), Color::White)
    } else {
        match enabled {
            Some(true) => wizard_line(&format!("    {checkbox}{name}"), Color::Blue),
            Some(false) => wizard_line(&format!("    {checkbox}{name}"), Color::DarkGray),
            None => wizard_line(&format!("    {name}"), Color::Cyan),
        }
    };
    vec![
        name_line,
        wizard_line(&format!("        {description}"), Color::DarkGray),
    ]
}

/// Features section: the settings list with the selected row kept visible,
/// plus the macOS GUI-automation compact/expanded status surfaces.
#[allow(clippy::too_many_arguments)]
fn features_section_content(
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
    help_rows: usize,
    #[cfg(target_os = "macos")] gui_automation_target_description: &str,
    daemon_only_mode: bool,
    mdns_discovery: bool,
    auto_discover: bool,
    memory_context_lines: usize,
    selected_idx: usize,
    width: usize,
    height: usize,
) -> WizardSectionContent {
    // macOS-only: the expanded-details early return below reads these, and
    // the GUI-automation section itself is a macOS surface.
    #[cfg(target_os = "macos")]
    let show_gui_details = selected_idx == 3 && gui_automation;

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

    // Rows the frame reserves outside the section: the 3-row tab block and
    // the wrapped help line (#926: the help wraps when it exceeds the frame,
    // so the budget counts its actual extent, not a fixed one row) — the same
    // arithmetic the host's claiming pass runs.
    let section_rows = height.saturating_sub(3).saturating_sub(help_rows);

    // The expanded status owns the section: its scroll offset skips whole
    // wrapped rows, and the instructions stay visible beneath the box. The
    // whole block is macOS-only: it reads the macOS-only automation status
    // lines and scroll offset, and `expanded_gui_details` can only be true
    // there. Compiling it on other platforms would name values that do not
    // exist (E0425 on the Linux CI lane).
    #[cfg(target_os = "macos")]
    if expanded_gui_details {
        let inner_width = width.saturating_sub(4);
        let body_budget = section_rows
            .saturating_sub(1)
            .saturating_sub(2)
            .saturating_sub(1);
        let scrolled = skip_wrapped_rows(
            &gui_automation_status,
            gui_automation_details_scroll as usize,
            inner_width,
        );
        let body = head_fitting_rows(&scrolled, body_budget, inner_width);
        let mut lines = vec![wizard_centered(
            &wizard_bold("Settings", Color::Cyan),
            width,
        )];
        lines.extend(wizard_boxed(
            "Full GUI automation status (read/scroll only)",
            &body,
            Color::Blue,
            width,
        ));
        lines.push(wizard_centered(
            &wizard_bold(
                "↑/↓ or PgUp/PgDn: Scroll | Home: Top | D/Esc: Back to settings",
                Color::Yellow,
            ),
            width,
        ));
        return WizardSectionContent::plain(lines);
    }

    #[cfg(target_os = "macos")]
    let gui_automation_description = gui_automation_status
        .iter()
        .find_map(|line| line.strip_prefix("Trust status: "))
        .unwrap_or("GUI automation status unavailable")
        .to_string();
    #[cfg(not(target_os = "macos"))]
    let gui_automation_description = String::new();

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
        (
            "GUI automation",
            gui_automation,
            &gui_automation_description,
        ),
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

    let hf_group = |selected: bool| -> Vec<String> {
        let (prefix, suffix) = if selected {
            (">>> ", " <<<")
        } else {
            ("    ", "")
        };
        let line = if editing_hf_token {
            format!("{prefix}HF Token: {hf_token}{suffix}")
        } else if hf_token.is_empty() {
            format!("{prefix}HF Token: [not set — press E to enter]{suffix}")
        } else {
            format!("{prefix}HF Token: {}{suffix}", mask_secret(hf_token, 4, 4))
        };
        vec![
            wizard_line(&line, Color::Cyan),
            wizard_line(
                "        For model downloads from HuggingFace",
                Color::DarkGray,
            ),
        ]
    };
    let finch_key_group = |selected: bool| -> Vec<String> {
        let (prefix, suffix) = if selected {
            (">>> ", " <<<")
        } else {
            ("    ", "")
        };
        let line = if editing_finch_api_key {
            format!("{prefix}Finch client key: {finch_api_key}{suffix}")
        } else if finch_api_key.is_empty() {
            format!("{prefix}Finch client key: [not set — authentication disabled; press E to enter]{suffix}")
        } else {
            format!(
                "{prefix}Finch client key: {}{suffix}",
                mask_secret(finch_api_key, 4, 4)
            )
        };
        vec![
            wizard_line(&line, Color::Cyan),
            wizard_line(
                "        Key OpenAI-compatible clients use to connect to Finch",
                Color::DarkGray,
            ),
        ]
    };

    // Build the groups in the exact order the input handler indexes them.
    let mut groups: Vec<Vec<String>> = Vec::new();
    let mut list_idx = 0usize;
    for (name, enabled, description) in bool_features.iter() {
        if list_idx == SETTINGS_HF_TOKEN_IDX {
            groups.push(hf_group(selected_idx == list_idx));
            list_idx += 1;
        }
        if list_idx == SETTINGS_FINCH_API_KEY_IDX {
            groups.push(finch_key_group(selected_idx == list_idx));
            list_idx += 1;
        }
        groups.push(feature_group(
            list_idx == selected_idx,
            Some(*enabled),
            name,
            description,
        ));
        list_idx += 1;
    }
    if SETTINGS_HF_TOKEN_IDX >= list_idx {
        groups.push(hf_group(selected_idx == list_idx));
    }
    // Context-lines spinner row (always last).
    {
        let selected = selected_idx == SETTINGS_CONTEXT_IDX;
        let (prefix, suffix) = if selected {
            (">>> ", " <<<")
        } else {
            ("    ", "")
        };
        groups.push(vec![
            format!(
                "{}{}{}",
                wizard_plain(prefix),
                wizard_bold(
                    &format!("◀ Context lines: {} ▶", memory_context_lines),
                    if selected { Color::White } else { Color::Blue },
                ),
                if selected {
                    wizard_bold(suffix, Color::White)
                } else {
                    String::new()
                },
            ),
            wizard_line(
                "        Status-strip summary lines shown below the prompt (1–8)",
                Color::DarkGray,
            ),
        ]);
    }

    // Compact GUI status box claims rows under the list when its row is active.
    #[cfg(target_os = "macos")]
    let compact_rows: Vec<String> = if show_gui_details {
        let mut rows = Vec::new();
        if let Some(feedback) = gui_automation_settings_feedback {
            rows.push(wizard_plain(feedback.compact_message()));
        } else {
            rows.push(wizard_plain(
                if gui_automation_availability.state == AutomationState::Available {
                    "Current Finch process: trusted."
                } else {
                    "Current Finch process: untrusted."
                },
            ));
        }
        rows.push(wizard_line(
            "R: Passive check | P: Request prompt",
            Color::Cyan,
        ));
        rows.push(wizard_line(
            "O: System Settings → Privacy & Security → Accessibility",
            Color::Cyan,
        ));
        rows.push(wizard_line("D: Full process/host/status", Color::Cyan));
        rows
    } else {
        Vec::new()
    };
    #[cfg(not(target_os = "macos"))]
    let compact_rows: Vec<String> = Vec::new();

    // Window the groups so the selected row stays visible with the same
    // minimal-scroll guarantee the old painter's list state gave. The budget
    // counts the compact status box's wrapped rows, so the status and the
    // instructions the accessibility contract pins stay visible next to it.
    // `start` only ever advances toward the selection, so a selected group
    // taller than the whole budget converges (its name line shows at the
    // window's head) instead of oscillating.
    // macOS-only: `show_gui_details` and the compact status box exist only
    // there; elsewhere the list budget simply keeps the extra rows.
    #[cfg(target_os = "macos")]
    let compact_extra: usize = if show_gui_details {
        2 + compact_rows
            .iter()
            .map(|line| rows_of(line, width))
            .sum::<usize>()
    } else {
        0
    };
    #[cfg(not(target_os = "macos"))]
    let compact_extra: usize = 0;
    let list_budget = section_rows
        .saturating_sub(1) // title
        .saturating_sub(2) // options box borders
        .saturating_sub(1) // instructions
        .saturating_sub(compact_extra);
    let group_rows: Vec<usize> = groups
        .iter()
        .map(|group| group.iter().map(|line| rows_of(line, width)).sum())
        .collect();
    let mut start = 0usize;
    while start < selected_idx && start < groups.len() {
        let mut used = 0usize;
        let mut end = start;
        while end < groups.len() && used + group_rows[end] <= list_budget {
            used += group_rows[end];
            end += 1;
        }
        if selected_idx < end {
            break;
        }
        start += 1;
    }
    let start = start.min(selected_idx);
    let mut windowed: Vec<String> = Vec::new();
    {
        let mut used = 0usize;
        for (index, group) in groups.iter().enumerate().skip(start) {
            let rows = group_rows[index];
            if used + rows <= list_budget {
                used += rows;
                windowed.extend(group.iter().cloned());
                continue;
            }
            if index == selected_idx {
                // The selected group alone overflows the budget: show its
                // head lines (the toggle name first) and clip the rest.
                for line in group {
                    let line_rows = rows_of(line, width);
                    if used + line_rows > list_budget {
                        break;
                    }
                    used += line_rows;
                    windowed.push(line.clone());
                }
            }
            break;
        }
    }

    let mut lines = vec![wizard_centered(
        &wizard_bold("Settings", Color::Cyan),
        width,
    )];
    lines.extend(wizard_boxed("Options", &windowed, Color::Blue, width));
    #[cfg(target_os = "macos")]
    if show_gui_details {
        lines.extend(wizard_boxed(
            "GUI automation status",
            &compact_rows,
            Color::Blue,
            width,
        ));
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
    lines.push(wizard_centered(
        &wizard_bold(instructions_text, Color::Yellow),
        width,
    ));
    WizardSectionContent::plain(lines)
}

/// Review section: the summary the user confirms before saving.
fn review_section_lines(state: &WizardState, width: usize) -> Vec<String> {
    use crate::theme::ColorTheme;

    let mut body = vec![
        wizard_centered(&wizard_bold("Ready to go!", Color::Green), width),
        wizard_centered(&wizard_bold("Here's what you set up:", Color::Cyan), width),
    ];

    if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get(&WizardSection::Themes)
    {
        let themes = ColorTheme::all();
        let theme_name = themes[*selected_theme].name().to_string();
        body.push(format!(
            "{}{}",
            wizard_line("Theme: ", Color::Yellow),
            wizard_plain(&theme_name)
        ));
    }

    if let Some(SectionState::Models { primary_model, .. }) =
        state.sections.get(&WizardSection::Models)
    {
        let ai_label = match primary_model {
            ModelConfig::Remote { api_key, .. } if !api_key.is_empty() => {
                "Claude (API key configured)"
            }
            ModelConfig::Remote { .. } => "Claude (no API key — will prompt on first use)",
            ModelConfig::Local { .. } => "Local model",
        };
        body.push(format!(
            "{}{}",
            wizard_line("AI: ", Color::Yellow),
            wizard_plain(ai_label)
        ));
    }

    if let Some(SectionState::Personas {
        default_persona, ..
    }) = state.sections.get(&WizardSection::Personas)
    {
        body.push(format!(
            "{}{}",
            wizard_line("Style: ", Color::Yellow),
            wizard_plain(default_persona)
        ));
    }

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
        let settings_text = if settings.is_empty() {
            "Defaults".to_string()
        } else {
            settings.join(", ")
        };
        body.push(format!(
            "{}{}",
            wizard_line("Settings: ", Color::Yellow),
            wizard_plain(&settings_text)
        ));
    }

    body.push(String::new());
    body.push(wizard_bold(
        "Press Enter or Ctrl+S to save & start chatting",
        Color::Green,
    ));
    body.push(wizard_line(
        "Esc: Back to settings · Ctrl+C: Cancel setup",
        Color::Gray,
    ));
    wizard_boxed("Ready", &body, Color::Green, width)
}

// ─── Overlay cards (#807) ────────────────────────────────────────────────────

/// The discard-confirmation card the user gets on Ctrl+C.
pub(super) fn cancel_confirm_card() -> WizardCard {
    WizardCard {
        title: "Cancel setup?".to_string(),
        body: vec![
            wizard_plain("Discard all setup changes and cancel?"),
            String::new(),
        ],
        controls: Some("Y / Enter: Discard    N / Esc: Keep editing".to_string()),
        accent: Color::Yellow,
    }
}

/// Split a failure summary at sentence boundaries so each line stays whole
/// on the card — a wrap inside "Console API-key billing" would read as a
/// different sentence to a screen reader.
fn failure_summary_sentences(summary: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    for word in summary.split_whitespace() {
        current.push_str(word);
        current.push(' ');
        if word.ends_with('.') && word.len() > 1 {
            sentences.push(current.trim().to_string());
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        sentences.push(current.trim().to_string());
    }
    if sentences.is_empty() {
        sentences.push(summary.to_string());
    }
    sentences
}

fn cloud_provider_id(provider_idx: usize) -> &'static str {
    CLOUD_PROVIDERS[provider_idx.min(CLOUD_PROVIDERS.len() - 1)].0
}

/// The add-time device sign-in card (#424/#705). Every state is plain,
/// speakable text: starting, the one-time code with its verification URL, the
/// authenticated account, or the terminal failure cause with recovery keys.
fn device_auth_card(
    provider_idx: usize,
    provider_name: &str,
    pending: &std::sync::Arc<std::sync::Mutex<Option<DeviceAuthPresentation>>>,
    outcome: &DeviceAuthOutcome,
    editing_existing_provider: bool,
) -> WizardCard {
    let is_grok_sub = cloud_provider_id(provider_idx).eq_ignore_ascii_case("grok-sub");
    let title_text = if is_grok_sub {
        format!("Grok subscription device sign-in for {provider_name}")
    } else {
        format!("ChatGPT device sign-in for {provider_name}")
    };
    let mut body = vec![wizard_bold(&title_text, Color::Cyan), String::new()];
    let controls;
    match outcome.lock().unwrap().as_ref() {
        Some(Ok(ensured)) => {
            let account = ensured
                .account
                .as_deref()
                .unwrap_or("the authorized account");
            body.push(wizard_plain(&format!("Signed in as {account}.")));
            body.push(wizard_plain(&format!(
                "{provider_name} is authenticated. Press Enter to return to the provider list."
            )));
            controls = "Enter: Continue".to_string();
        }
        Some(Err(failure)) => {
            let summary = if is_grok_sub {
                grok_setup_failure_summary(grok_setup_failure_cause(failure))
            } else {
                chatgpt_setup_failure_summary(chatgpt_setup_failure_cause(failure))
            };
            for sentence in failure_summary_sentences(&summary) {
                body.push(wizard_plain(&sentence));
            }
            controls = "Enter: Retry sign-in | Esc: Back to provider details".to_string();
        }
        None => match pending.lock().unwrap().as_ref() {
            Some(presentation) => {
                body.push(wizard_plain(&format!(
                    "Open: {}",
                    presentation.verification_uri
                )));
                body.push(wizard_line(
                    &format!("One-time code: {}", presentation.user_code),
                    Color::White,
                ));
                body.push(String::new());
                body.push(wizard_plain(
                    "Approve the code in your browser; this dialog finishes automatically.",
                ));
                body.push(wizard_plain(&format!(
                    "The code expires in {} minutes.",
                    presentation.expires_in.as_secs().div_ceil(60)
                )));
                controls = "Esc: Cancel".to_string();
            }
            None => {
                body.push(wizard_plain("Starting the device sign-in…"));
                controls = "Esc: Cancel".to_string();
            }
        },
    }
    let mut card = WizardCard::new(
        if editing_existing_provider {
            "Edit AI Provider"
        } else {
            "Add AI Provider"
        },
        body,
        Some(controls),
    );
    card.accent = Color::Cyan;
    card
}

/// One bracketed form row; the exact shapes the compact editors always showed.
fn remote_form_row(label: &str, value: &str, focused: bool, is_text_input: bool) -> String {
    let label_text = if focused {
        wizard_bold(&format!("{:<10}", label), Color::White)
    } else {
        wizard_line(&format!("{:<10}", label), Color::DarkGray)
    };
    let value_text = if is_text_input && focused {
        format!("[ {}█ ]", value)
    } else if focused {
        format!("[◄ {:<34}►]", value)
    } else {
        format!("[  {:<34} ]", value)
    };
    let value_painted = if focused {
        wizard_bold(&value_text, Color::White)
    } else {
        wizard_line(&value_text, Color::Cyan)
    };
    format!("{label_text}{value_painted}")
}

/// The add-provider overlay as one claimed card: type selection, the
/// single-screen remote/local forms, the network scan, and the device
/// ceremony all render as body lines whose controls stay pinned inside the
/// card (#807) — no floating second painter.
pub(super) fn add_provider_card(
    coreml: CoreMlConfig,
    step: &AddProviderStep,
    catalog_source: &CatalogSource,
    catalog_refreshing: bool,
    catalog_refreshed_at: Option<&DateTime<Utc>>,
    catalog_error: Option<&str>,
) -> WizardCard {
    match step {
        // ── type selection — shows all providers directly ────────────────────
        AddProviderStep::SelectAddType { selected } => {
            let n_cloud = CLOUD_PROVIDERS.len();
            let mut body = Vec::new();
            for (index, (_, display_name, _, hint)) in CLOUD_PROVIDERS.iter().enumerate() {
                body.push(if index == *selected {
                    wizard_bold(&format!(">>> {display_name} <<<"), Color::White)
                } else {
                    wizard_line(&format!("    {display_name}"), Color::Cyan)
                });
                body.push(wizard_line(&format!("        {hint}"), Color::DarkGray));
            }
            let (local_line, scan_line) = if *selected == n_cloud {
                (
                    wizard_bold(">>> Local model <<<", Color::White),
                    wizard_line("    Scan local network", Color::DarkGray),
                )
            } else if *selected == n_cloud + 1 {
                (
                    wizard_line("    Local model", Color::Cyan),
                    wizard_bold(">>> Scan local network <<<", Color::White),
                )
            } else {
                (
                    wizard_line("    Local model", Color::Cyan),
                    wizard_line("    Scan local network", Color::DarkGray),
                )
            };
            body.push(local_line);
            body.push(wizard_line(
                "        Run a model on this machine (no internet after download)",
                Color::DarkGray,
            ));
            body.push(scan_line);
            body.push(wizard_line(
                "        Discover other Finch instances running on your LAN",
                Color::DarkGray,
            ));
            WizardCard::new(
                "Add AI Provider",
                body,
                Some("↑/↓: Move | Enter: Select | Esc: Cancel".to_string()),
            )
        }
        // ── single-screen cloud provider form ────────────────────────────────
        AddProviderStep::ConfigureRemote {
            provider_idx,
            name,
            model,
            api_key,
            focused_field,
            editing_idx,
        } => {
            let remote_idx = (*provider_idx).min(CLOUD_PROVIDERS.len() - 1);
            let provider_name = CLOUD_PROVIDERS[remote_idx].1;
            let key_hint = CLOUD_PROVIDERS[remote_idx].3;
            let editing = editing_idx.is_some();
            let provider_value =
                format!("{} ({})", provider_name, cloud_provider_id(*provider_idx));
            let model_display = if model.is_empty() { "(default)" } else { model };
            let mut body = vec![
                String::new(),
                remote_form_row("Provider", &provider_value, *focused_field == 0, false),
                remote_form_row("Name", name, *focused_field == 1, true),
                remote_form_row("Model", model_display, *focused_field == 2, true),
            ];
            if let Some(api_key) = api_key {
                let key_display = if api_key.is_empty() {
                    String::new()
                } else {
                    format!("{}…", api_key.chars().take(12).collect::<String>())
                };
                body.push(remote_form_row(
                    "API Key",
                    &key_display,
                    *focused_field == 3,
                    true,
                ));
            } else {
                body.push(remote_form_row(
                    "Auth",
                    "Finch-native device sign-in after save",
                    false,
                    false,
                ));
            }
            body.push(String::new());
            body.push(wizard_line(key_hint, Color::DarkGray));
            body.push(wizard_line(
                &format_catalog_label(
                    catalog_source,
                    catalog_refreshing,
                    catalog_refreshed_at,
                    Utc::now(),
                ),
                Color::Cyan,
            ));
            if let Some(error) = catalog_error {
                body.push(wizard_line(
                    &format!("Refresh warning: {error}"),
                    Color::Yellow,
                ));
            }
            let controls = if editing {
                "↑↓ navigate · type to edit · Ctrl+R refresh · Enter saves · Esc cancels"
            } else {
                "↑↓ navigate · ←→ change provider/model · Ctrl+R refresh · Enter adds · Esc back"
            };
            WizardCard::new(
                if editing {
                    "Edit AI Provider"
                } else {
                    "Add Cloud Provider"
                },
                body,
                Some(controls.to_string()),
            )
        }
        // ── single-screen local model form ───────────────────────────────────
        AddProviderStep::ConfigureLocal {
            inference_provider: _,
            family,
            size,
            quantization,
            execution,
            model_path,
            focused_field,
            editing_idx,
        } => {
            let backend_name = "llama.cpp (GGUF)";
            let family_name = family.name().to_string();
            let size_name = model_size_display(size);
            let quantization_name = quantization.name();
            let device_name = execution_target_display(*execution, coreml);
            let row = |label: &str, value: &str, focused: bool| {
                remote_form_row(label, value, focused, false)
            };
            let managed = managed_gguf_artifact(*family, *size, *quantization);
            let repo_preview = managed.as_ref().map_or_else(
                || "No managed artifact for this combination".to_string(),
                |artifact| format!("Managed: {}", artifact.repository),
            );
            let ram_estimate = managed.as_ref().map_or_else(
                || "RAM depends on GGUF file".to_string(),
                |artifact| format!("Download {:.1} GB", artifact.expected_size as f64 / 1e9),
            );
            let mut body = vec![
                String::new(),
                row("Backend", backend_name, *focused_field == 0),
                row("Family", &family_name, *focused_field == 1),
                row("Size", size_name, *focused_field == 2),
                row("Quantization", quantization_name, *focused_field == 3),
                row("Device", &device_name, *focused_field == 4),
            ];
            let path_display = if model_path.chars().count() > 34 {
                let suffix: String = model_path.chars().rev().take(33).collect();
                format!("…{}", suffix.chars().rev().collect::<String>())
            } else {
                model_path.clone()
            };
            body.push(remote_form_row(
                "GGUF file (optional)",
                &path_display,
                *focused_field == 5,
                true,
            ));
            body.extend([
                String::new(),
                format!(
                    "{}  {}",
                    wizard_line(&format!("{ram_estimate}  "), Color::Cyan),
                    wizard_line(&repo_preview, Color::DarkGray)
                ),
            ]);
            WizardCard::new(
                if editing_idx.is_some() {
                    "Edit Local Model"
                } else {
                    "Add Local Model"
                },
                body,
                Some(if editing_idx.is_some() {
                    "↑↓ navigate · ←→ change · leave path blank to download · Enter to save · Esc back".to_string()
                } else {
                    "↑↓ navigate · ←→ change · leave path blank to download · Enter to add · Esc back".to_string()
                }),
            )
        }
        // ── network scan path ────────────────────────────────────────────────
        AddProviderStep::Scanning { .. } => WizardCard::new(
            "Add AI Provider",
            vec![
                String::new(),
                wizard_bold("Scanning for Finch agents on local network…", Color::Cyan),
                String::new(),
                wizard_line("(this takes up to 5 seconds)", Color::DarkGray),
            ],
            Some("Esc: Cancel".to_string()),
        ),
        AddProviderStep::SelectAgent { agents, selected } => {
            let body: Vec<String> = agents
                .iter()
                .enumerate()
                .map(|(index, agent)| {
                    let label = format!("{} @ {}:{}", agent.name, agent.host, agent.port);
                    if index == *selected {
                        wizard_bold(&format!(">>> {label} <<<"), Color::White)
                    } else {
                        wizard_line(&format!("    {label}"), Color::Cyan)
                    }
                })
                .collect();
            WizardCard::new(
                "Discovered agents",
                body,
                Some("↑/↓: Move | Enter: Add | Esc: Cancel".to_string()),
            )
        }
        // ── add-time device ceremony (#424) ──────────────────────────────────
        AddProviderStep::DeviceAuth {
            provider_idx,
            name,
            pending,
            outcome,
            editing_idx,
            ..
        } => device_auth_card(*provider_idx, name, pending, outcome, editing_idx.is_some()),
    }
}

// ─── The view ────────────────────────────────────────────────────────────────

/// Assemble the whole wizard view for one frame. This is the only conversion
/// the widget host needs; `permission_target` feeds the macOS GUI-automation
/// surfaces.
pub(super) fn wizard_view_with_permission_target(
    state: &WizardState,
    permission_target: &str,
    width: usize,
    height: usize,
) -> WizardView {
    // The help line's wrapped extent (#926): the host wraps the help to the
    // frame width and claims exactly these rows, so the sections' own window
    // budgets must reserve the same count.
    let help_rows = crate::cli::tui::wizard_wrap(&help_line(state, width), width).len();
    let section = match state.sections.get(&state.current_section) {
        Some(SectionState::Themes { selected_theme }) => {
            WizardSectionContent::plain(themes_section_lines(*selected_theme, width))
        }
        Some(SectionState::LocalHelpers {
            use_neural_embeddings,
        }) => {
            WizardSectionContent::plain(local_helpers_section_lines(*use_neural_embeddings, width))
        }
        Some(SectionState::Models {
            primary_model,
            tool_models,
            selected_idx,
            editing_mode,
            editing_model_mode,
            model_input,
            error,
            ..
        }) => WizardSectionContent::plain(models_section_lines(
            state.coreml,
            primary_model,
            tool_models,
            *selected_idx,
            *editing_mode,
            *editing_model_mode,
            model_input,
            error.as_deref(),
            width,
        )),
        Some(SectionState::Personas {
            available_personas,
            selected_idx,
            default_persona,
            editing_prompt,
            prompt_input,
            cursor_pos,
            ..
        }) => WizardSectionContent::plain(personas_section_lines(
            available_personas,
            *selected_idx,
            default_persona,
            *editing_prompt,
            prompt_input,
            *cursor_pos,
            width,
        )),
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
            ..
        }) => features_section_content(
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
            help_rows,
            #[cfg(target_os = "macos")]
            permission_target,
            *daemon_only_mode,
            *mdns_discovery,
            *auto_discover,
            *memory_context_lines,
            *selected_idx,
            width,
            height,
        ),
        Some(SectionState::Review) => {
            WizardSectionContent::plain(review_section_lines(state, width))
        }
        None => WizardSectionContent::plain(vec![wizard_line(
            "Error: Section state not found",
            Color::Red,
        )]),
    };

    // The open overlay is a claimed card; the help yields while it owns keys.
    let card = if state.confirming_cancel {
        Some(cancel_confirm_card())
    } else {
        match state.sections.get(&WizardSection::Models) {
            Some(SectionState::Models {
                adding_provider: Some(step),
                catalog_source,
                catalog_refresh,
                catalog_refreshed_at,
                catalog_error,
                ..
            }) => Some(add_provider_card(
                state.coreml,
                step,
                catalog_source,
                catalog_refresh.is_some(),
                catalog_refreshed_at.as_ref(),
                catalog_error.as_deref(),
            )),
            _ => None,
        }
    };

    let selected_tab = WizardSection::all()
        .iter()
        .position(|section| *section == state.current_section)
        .unwrap_or(0);

    WizardView {
        title: " Finch Setup ".to_string(),
        tab_titles: tab_titles(state),
        selected_tab,
        section,
        help: if card.is_none() {
            Some(help_line(state, width))
        } else {
            None
        },
        card,
    }
}

/// The frame's view, with the macOS permission target refreshed per frame the
/// way the old painter refreshed it.
pub(super) fn wizard_view(state: &WizardState, width: usize, height: usize) -> WizardView {
    #[cfg(target_os = "macos")]
    let permission_target = permission_target_description();
    #[cfg(not(target_os = "macos"))]
    let permission_target = String::new();
    wizard_view_with_permission_target(state, &permission_target, width, height)
}
