//! Wizard view props: what the widget host paints, one function per section.
//!
//! #812: this file no longer paints. The second painter is gone — each
//! function converts `WizardState` into styled span lines and overlay-card
//! props ([`WizardView`], [`WizardCard`]), and `crate::cli::tui::wizard_host`
//! claims the frame, lowers the spans to SGR, records the shadow buffer, and
//! blits. The spans are the speakable canonical form, so a GUI host (#808)
//! can consume the same props without any terminal bytes.
//!
//! The windowing helpers here count physical rows with the same shadow-buffer
//! arithmetic the host's claiming pass uses, so the one list that must keep a
//! selected row visible (the old painter's `ListState` guarantee) is windowed
//! with the same budget the tree will claim.

use super::chatgpt_recovery::{chatgpt_setup_failure_cause, chatgpt_setup_failure_summary};
use super::gemini_recovery::{gemini_setup_failure_cause, gemini_setup_failure_summary};
use super::grok_recovery::{grok_setup_failure_cause, grok_setup_failure_summary};
use super::*;
use crate::cli::tui::WizardColor as Color;
use crate::cli::tui::{
    wizard_bold, wizard_boxed, wizard_centered, wizard_line, wizard_markdown, wizard_paint,
    wizard_plain, wizard_selected, wizard_url, wizard_wrap, WizardCard, WizardLine,
    WizardSectionContent, WizardView,
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
fn rows_of(line: &WizardLine, width: usize) -> usize {
    line.physical_rows(width)
}

/// `Auto` or `CPU` — the llama.cpp execution-target display.
pub(super) fn execution_target_display(execution: ExecutionTarget) -> String {
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
            "built-in list (as of {}; incomplete)",
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
    format!("Models: {source}{refreshed} · Ctrl+R refresh · model name remains editable")
}

/// Skip the first `skip` wrapped rows of `lines`, by whole logical lines.
fn skip_wrapped_rows(lines: &[WizardLine], skip: usize, width: usize) -> Vec<WizardLine> {
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
fn head_fitting_rows(lines: &[WizardLine], budget: usize, width: usize) -> Vec<WizardLine> {
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

fn help_line(state: &WizardState, width: usize) -> WizardLine {
    let section_help = match state.current_section {
        WizardSection::Themes => "↑/↓: Choose theme | Enter: Next",
        WizardSection::Models => "Enter: Edit provider | A: Add | D: Remove",
        WizardSection::LocalHelpers => "Space: Toggle | Enter: Next",
        WizardSection::Personas => "↑/↓: Choose style | E: Edit prompt | Enter: Next",
        WizardSection::Features => "↑/↓: Navigate | Space: Toggle | Enter: Next",
        WizardSection::Review => "Enter: Save & start",
    };
    let text = format!("{section_help} | Ctrl+S: Save | Esc: Back | Tab: Next | Ctrl+C: Cancel");
    wizard_centered(wizard_bold(&text, Color::Blue), width)
}

// ─── Section content ─────────────────────────────────────────────────────────

/// Themes section: list, preview, instructions. The selected theme's row is
/// pinned, so a window too short for the whole section scrolls to it (#1651).
fn themes_section_content(selected_theme: usize, width: usize) -> WizardSectionContent {
    use crate::theme::ColorTheme;

    let mut lines = vec![wizard_centered(
        wizard_bold("Theme Selection", Color::Blue),
        width,
    )];

    let themes = ColorTheme::all();
    let items: Vec<WizardLine> = themes
        .iter()
        .enumerate()
        .map(|(index, theme)| {
            if index == selected_theme {
                wizard_selected(&format!(
                    ">>> {} - {} <<<",
                    theme.name(),
                    theme.description()
                ))
            } else {
                wizard_line(
                    &format!("    {} - {}", theme.name(), theme.description()),
                    Color::Blue,
                )
            }
        })
        .collect();
    // The box wraps each item to its inner width, one line per wrapped row;
    // the title and the box's top border precede the first item.
    let item_rows = |item: &WizardLine| wizard_wrap(item, width.max(4) - 4).len();
    let pin_start = 2 + items[..selected_theme].iter().map(item_rows).sum::<usize>();
    let pin_end = pin_start + item_rows(&items[selected_theme]);
    lines.extend(wizard_boxed("Available Themes", &items, Color::Blue, width));

    // Preview of the selected theme, in the theme's own colours.
    let preview_theme = themes[selected_theme].to_scheme();
    let preview = vec![
        WizardLine::concat(&[
            wizard_line("User: ", preview_theme.messages.user.to_color().into()),
            wizard_plain("What is 2+2?"),
        ]),
        WizardLine::concat(&[
            wizard_line(
                "Assistant: ",
                preview_theme.messages.assistant.to_color().into(),
            ),
            wizard_plain("The answer is 4."),
        ]),
        WizardLine::concat(&[
            wizard_line("🔧 Tool: ", preview_theme.messages.tool.to_color().into()),
            wizard_plain("Reading file..."),
        ]),
        WizardLine::concat(&[
            wizard_line("❌ Error: ", preview_theme.messages.error.to_color().into()),
            wizard_plain("File not found"),
        ]),
    ];
    lines.extend(wizard_boxed("Preview", &preview, Color::Blue, width));

    lines.push(wizard_bold(
        "Use ↑/↓ arrow keys to move selection (>>> theme <<<)",
        Color::Blue,
    ));
    lines.push(wizard_bold(
        "This whole screen previews the selected theme. Press Enter to confirm.",
        Color::Blue,
    ));
    WizardSectionContent {
        lines,
        scroll_rows: 0,
        pin_visible: Some((pin_start, pin_end)),
    }
}

/// Wrap `text` at `width` into one [`WizardLine`] per physical row, then pad
/// with blank rows up to `min_rows`.
///
/// A toggle-driven description that prints as a single un-wrapped
/// `WizardLine` relies on the terminal's own line wrap: the widget host's
/// row-diff blit (`WizardHost::paint` in `crates/finch-tui`) clears only the
/// row it repaints, and skips a logical line whose *content* is unchanged
/// without checking whether that line's physical row moved. When a shorter
/// variant makes this line (or anything after it) occupy fewer rows than the
/// previous render, everything past the shrink point silently shifts up by
/// one absolute terminal row, and a downstream line whose text happens to
/// read the same in both frames (most often a blank pad row) is skipped even
/// though it now sits one row higher — leaving the longer variant's last
/// wrapped row stranded on screen (#1297). Exploding the text into one entry
/// per physical row, then padding every variant to the *same* declared
/// height, keeps the row count constant across toggle states so nothing
/// downstream ever shifts.
fn wrapped_and_padded(text: &str, color: Color, width: usize, min_rows: usize) -> Vec<WizardLine> {
    let mut rows = wizard_wrap(&wizard_line(text, color), width);
    while rows.len() < min_rows {
        rows.push(WizardLine::blank());
    }
    rows
}

/// Local Helpers section: the separate local-only model choice for
/// finch-builtin functions (memory embeddings today), distinct from the
/// "Model Setup" tab's chat-provider configuration.
fn local_helpers_section_lines(use_neural_embeddings: bool, width: usize) -> Vec<WizardLine> {
    let mut lines = vec![wizard_centered(
        wizard_bold("Local Helper Models", Color::Blue),
        width,
    )];
    lines.push(wizard_line(
        "Separate from the chat model above: these are the local-only models \
         finch's own built-in features use for themselves.",
        Color::DarkGray,
    ));

    let checkbox = if use_neural_embeddings { "☑" } else { "☐" };
    let item = wizard_selected(format!(
        ">>> {checkbox} Smart memory: enable enhanced search <<<"
    ));
    lines.extend(wizard_boxed("Memory", &[item], Color::Blue, width));

    const ON_DETAIL: &str = "On: Downloaded once on first use, then runs locally on your computer \
         with no further internet access needed. Understands related concepts \
         and finds past context more accurately.";
    const OFF_DETAIL: &str =
        "Off: Uses basic keyword matching. No download and no internet access \
         required, but search results may be less accurate.";
    let detail = if use_neural_embeddings {
        ON_DETAIL
    } else {
        OFF_DETAIL
    };

    // #1297: declare the fixed height from both variants so switching the
    // toggle never changes this block's total row count.
    let fixed_rows = wizard_wrap(&wizard_line(ON_DETAIL, Color::DarkGray), width)
        .len()
        .max(wizard_wrap(&wizard_line(OFF_DETAIL, Color::DarkGray), width).len());
    lines.push(WizardLine::blank());
    lines.extend(wrapped_and_padded(
        detail,
        Color::DarkGray,
        width,
        fixed_rows,
    ));
    lines
}

/// The display text for one provider row: primary marker, tool checkbox, and
/// masked key state — the exact shapes the old painter rendered.
fn provider_row_display(model: &ModelConfig, primary: bool) -> String {
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
                execution_target_display(*execution)
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
                    || provider.eq_ignore_ascii_case("gemini-sub")
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

fn marked_row(display: &str, selected: bool, enabled: bool) -> WizardLine {
    if selected {
        wizard_selected(&format!(">>> {display} <<<"))
    } else if enabled {
        wizard_plain(&format!("    {display}"))
    } else {
        wizard_line(&format!("    {display}"), Color::DarkGray)
    }
}

/// Models section: description, provider list, edit panel, instructions, error.
#[allow(clippy::too_many_arguments)]
fn models_section_lines(
    primary_model: &ModelConfig,
    tool_models: &[ModelConfig],
    selected_idx: usize,
    editing_mode: bool,
    editing_model_mode: bool,
    model_input: &str,
    error: Option<&str>,
    width: usize,
) -> Vec<WizardLine> {
    let mut lines = vec![wizard_centered(
        wizard_bold("AI Providers", Color::Cyan),
        width,
    )];

    let has_key = match primary_model {
        ModelConfig::Remote {
            provider,
            api_key,
            persisted,
            ..
        } if provider.eq_ignore_ascii_case("chatgpt")
            || provider.eq_ignore_ascii_case("grok-sub")
            || provider.eq_ignore_ascii_case("gemini-sub") =>
        {
            matches!(persisted, Some(ProviderEntry::Credentialed { .. }))
        }
        ModelConfig::Remote { api_key, .. } => !api_key.is_empty(),
        ModelConfig::Local { .. } => true,
    };
    const GEMINI_SUB_DETAIL: &str = "Gemini subscription uses Google Gemini via device sign-in; AI Studio API keys are a separate provider and are never used automatically.";
    const GROK_SUB_DETAIL: &str = "Grok subscription uses SuperGrok entitlement via device sign-in; xAI Console API keys are a separate provider and are never used automatically.";
    const CHATGPT_DETAIL: &str = "ChatGPT subscription uses a named Finch device credential; OpenAI Platform API keys are separate.";
    const NO_KEY_DETAIL: &str =
        "Press Enter or E to Paste your API key, or add a provider with A.\n\
         No key yet? Get one at console.anthropic.com/keys";
    let has_key_detail = format!(
        "Primary provider configured. Press A to add more providers ({} total).",
        1 + tool_models.len()
    );
    let description_text = match primary_model {
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("gemini-sub") => {
            GEMINI_SUB_DETAIL.to_string()
        }
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("grok-sub") => {
            GROK_SUB_DETAIL.to_string()
        }
        ModelConfig::Remote { provider, .. } if provider.eq_ignore_ascii_case("chatgpt") => {
            CHATGPT_DETAIL.to_string()
        }
        _ if has_key => has_key_detail.clone(),
        _ => NO_KEY_DETAIL.to_string(),
    };

    // #1305: same class of bug as #1297 -- the no-key variant is two
    // sentences (two logical rows) while the other three variants are one,
    // so switching primary providers or adding/removing an API key changes
    // this block's row count and, with nothing but `wizard_boxed` right
    // after it, strands a stale row from the longer variant under the box's
    // top border. Declare the fixed height across all four candidate texts
    // up front and pad every variant to it.
    let rows_for = |text: &str| -> usize {
        text.split('\n')
            .map(|segment| wizard_wrap(&wizard_line(segment.trim(), Color::Blue), width).len())
            .sum()
    };
    let fixed_rows = [
        GROK_SUB_DETAIL,
        CHATGPT_DETAIL,
        has_key_detail.as_str(),
        NO_KEY_DETAIL,
    ]
    .into_iter()
    .map(rows_for)
    .max()
    .unwrap_or(1);

    let mut printed_rows = 0;
    for text in description_text.split('\n') {
        for row in wizard_wrap(&wizard_line(text.trim(), Color::Blue), width) {
            lines.push(wizard_centered(row, width));
            printed_rows += 1;
        }
    }
    while printed_rows < fixed_rows {
        lines.push(WizardLine::blank());
        printed_rows += 1;
    }

    let mut list_rows: Vec<WizardLine> = Vec::new();
    for (index, model) in std::iter::once(primary_model)
        .chain(tool_models.iter())
        .enumerate()
    {
        let display = provider_row_display(model, index == 0);
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
            wizard_line(
                "Press Enter to edit the selected provider · P for primary",
                Color::DarkGray,
            ),
            width,
        ));
    }

    let instructions_text = if editing_mode || editing_model_mode {
        "Type here | Enter/Esc: Save & return"
    } else {
        "Enter: Edit | P: Primary | Shift+↑/↓: Reorder | A: Add | D: Remove | Tab: Next"
    };
    lines.push(wizard_centered(
        wizard_bold(instructions_text, Color::Yellow),
        width,
    ));
    if let Some(error) = error {
        lines.push(wizard_centered(wizard_line(error, Color::Red), width));
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
) -> Vec<WizardLine> {
    let mut lines = Vec::new();
    let rows: Vec<WizardLine> = personas
        .iter()
        .enumerate()
        .map(|(index, persona)| {
            let is_default = persona.name.to_lowercase() == default_persona.to_lowercase();
            if index == selected_idx {
                wizard_selected(&format!(">>> {} <<<", persona.name))
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
        body.push(WizardLine::blank());
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
            WizardLine::concat(&[
                wizard_paint("Name: ", None, true),
                wizard_plain(&persona.name),
            ]),
            WizardLine::blank(),
            WizardLine::concat(&[
                wizard_paint("Description: ", None, true),
                wizard_plain(&persona.description),
            ]),
            WizardLine::blank(),
            wizard_paint("System Prompt:", None, true),
            WizardLine::blank(),
            wizard_plain(&persona.system_prompt),
            WizardLine::blank(),
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
) -> Vec<WizardLine> {
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

    let mut lines: Vec<WizardLine> = Vec::new();
    if let Some(feedback) = settings_feedback {
        lines.push(WizardLine::plain(format!(
            "Settings action: {}",
            feedback.full_message()
        )));
    }
    lines.push(wizard_plain(&summary));
    if configured
        && matches!(
            availability.state,
            AutomationState::PermissionRequired | AutomationState::Available
        )
    {
        lines.extend(
            target_description
                .lines()
                .map(|line| WizardLine::plain(format!("Diagnostic only — {line}"))),
        );
    }
    if availability.state == AutomationState::PermissionRequired {
        lines.push(wizard_plain(
            "Recovery: a checkbox or prompt is not proof of access. Press P to request the macOS prompt, or open System Settings → Privacy & Security → Accessibility, then press R for a passive re-check of this live process. If it remains untrusted, relaunch the same executable/host context and check again.",
        ));
    }
    lines.push(wizard_plain(
        "This full view is read/scroll only; clipboard copying is unavailable in the setup wizard.",
    ));
    lines
}

/// The Settings tab's boolean toggles, in list order: `(label, enabled)`.
///
/// This is the single source of truth for which toggles exist. Both the
/// Settings tab (`features_section_content`, which zips descriptions onto
/// this same list) and the Finish screen's "Ready to go!" summary
/// (`review_section_lines`) build from it, so adding a toggle here is enough
/// for it to show up in both places — the Finish screen previously hardcoded
/// its own two-item allowlist and silently dropped every setting added since,
/// including the two with real network effect (#1299).
#[allow(clippy::too_many_arguments)]
fn feature_toggle_states(
    streaming: bool,
    auto_approve: bool,
    debug: bool,
    #[cfg(target_os = "macos")] gui_automation: bool,
    daemon_only_mode: bool,
    mdns_discovery: bool,
    auto_discover: bool,
) -> Vec<(&'static str, bool)> {
    #[cfg(target_os = "macos")]
    {
        vec![
            ("Live responses", streaming),
            ("Skip permission prompts", auto_approve),
            ("Debug logging", debug),
            ("GUI automation", gui_automation),
            ("Background mode only", daemon_only_mode),
            ("Advertise on network", mdns_discovery),
            ("Discover peers on LAN", auto_discover),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![
            ("Live responses", streaming),
            ("Skip permission prompts", auto_approve),
            ("Debug logging", debug),
            ("Background mode only", daemon_only_mode),
            ("Advertise on network", mdns_discovery),
            ("Discover peers on LAN", auto_discover),
        ]
    }
}

/// One settings row group: the toggle/edit line and its dim description.
fn feature_group(
    selected: bool,
    enabled: Option<bool>,
    name: &str,
    description: &str,
) -> Vec<WizardLine> {
    let checkbox = match enabled {
        Some(true) => "☑ ",
        Some(false) => "☐ ",
        None => "",
    };
    let name_line = if selected {
        wizard_selected(&format!(">>> {checkbox}{name} <<<"))
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
        let mut lines = vec![wizard_centered(wizard_bold("Settings", Color::Cyan), width)];
        lines.extend(wizard_boxed(
            "Full GUI automation status (read/scroll only)",
            &body,
            Color::Blue,
            width,
        ));
        lines.push(wizard_centered(
            wizard_bold(
                "↑/↓ or PgUp/PgDn: Scroll | Home: Top | D/Esc: Back to settings",
                Color::Yellow,
            ),
            width,
        ));
        return WizardSectionContent::plain(lines);
    }

    // #1298: `gui_automation_status_lines` never emits a "Trust status: "
    // prefix — every real outcome starts with "Configured; ..." or a plain
    // "Finch capability consent is disabled" / "Configured, but unsupported"
    // sentence — so a `strip_prefix("Trust status: ")` match can never
    // succeed and the row always fell back to the generic placeholder,
    // regardless of whether the process was trusted, untrusted, or in error.
    // The real trust-status sentence is the first line `gui_automation_status_lines`
    // returns, except when a transient "Settings action: ..." banner (from a
    // just-pressed P/R) is queued ahead of it; skip that one line to reach
    // the real status instead of parsing a prefix the source never writes.
    #[cfg(target_os = "macos")]
    let gui_automation_description = gui_automation_status
        .iter()
        .map(|line| line.plain_text())
        .find(|text| !text.starts_with("Settings action: "))
        .unwrap_or_else(|| "GUI automation status unavailable".to_string());
    #[cfg(not(target_os = "macos"))]
    let gui_automation_description = String::new();

    // The label/enabled pairs come from `feature_toggle_states`, the same
    // list the Finish screen's summary reads (#1299): a toggle added there is
    // automatically a row here too, so the two views cannot drift apart.
    #[cfg(target_os = "macos")]
    let descriptions: [&str; 7] = [
        "See Finch's answer as it types, word by word",
        "Let Finch run tools without asking each time",
        "Write detailed diagnostic logs to help troubleshoot issues",
        &gui_automation_description,
        "Run silently in the background without opening the chat window",
        "Broadcast this Finch instance via mDNS so others can discover it",
        "Find and connect to other Finch instances at startup",
    ];
    #[cfg(not(target_os = "macos"))]
    let descriptions: [&str; 6] = [
        "See Finch's answer as it types, word by word",
        "Let Finch run tools without asking each time",
        "Write detailed diagnostic logs to help troubleshoot issues",
        "Run silently in the background without opening the chat window",
        "Broadcast this Finch instance via mDNS so others can discover it",
        "Find and connect to other Finch instances at startup",
    ];
    let bool_features: Vec<(&str, bool, &str)> = feature_toggle_states(
        streaming,
        auto_approve,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
    )
    .into_iter()
    .zip(descriptions)
    .map(|((name, enabled), description)| (name, enabled, description))
    .collect();

    let hf_group = |selected: bool| -> Vec<WizardLine> {
        let (prefix, suffix) = if selected {
            (">>> ", " <<<")
        } else {
            ("    ", "")
        };
        let line = if editing_hf_token {
            format!("{prefix}Community model token: {hf_token}{suffix}")
        } else if hf_token.is_empty() {
            format!("{prefix}Community model token: [not set — press E to enter]{suffix}")
        } else {
            format!(
                "{prefix}Community model token: {}{suffix}",
                mask_secret(hf_token, 4, 4)
            )
        };
        if selected {
            vec![
                wizard_selected(&line),
                wizard_line(
                    "        Optional access token for downloading community models",
                    Color::DarkGray,
                ),
            ]
        } else {
            vec![
                wizard_line(&line, Color::Cyan),
                wizard_line(
                    "        Optional access token for downloading community models",
                    Color::DarkGray,
                ),
            ]
        }
    };
    let finch_key_group = |selected: bool| -> Vec<WizardLine> {
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
        if selected {
            vec![
                wizard_selected(&line),
                wizard_line(
                    "        Key OpenAI-compatible clients use to connect to Finch",
                    Color::DarkGray,
                ),
            ]
        } else {
            vec![
                wizard_line(&line, Color::Cyan),
                wizard_line(
                    "        Key OpenAI-compatible clients use to connect to Finch",
                    Color::DarkGray,
                ),
            ]
        }
    };

    // Build the groups in the exact order the input handler indexes them.
    let mut groups: Vec<Vec<WizardLine>> = Vec::new();
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
    // Context-lines spinner row (always last). The value is the row's point:
    // it renders in the selection contrast when selected (bg Black, #1140)
    // and stays bold blue otherwise, so the number is always readable and
    // the ◀/▶ affordance is visible.
    {
        let selected = selected_idx == SETTINGS_CONTEXT_IDX;
        let (prefix, suffix) = if selected {
            (">>> ", " <<<")
        } else {
            ("    ", "")
        };
        let spinner_line = if selected {
            wizard_selected(&format!(
                "{prefix}◀ Context lines: {memory_context_lines} ▶{suffix}"
            ))
        } else {
            WizardLine::concat(&[
                wizard_plain(prefix),
                wizard_bold(
                    &format!("◀ Context lines: {memory_context_lines} ▶"),
                    Color::Blue,
                ),
                wizard_plain(suffix),
            ])
        };
        groups.push(vec![
            spinner_line,
            wizard_line(
                "        Status-strip summary lines shown below the prompt (1–8)",
                Color::DarkGray,
            ),
        ]);
    }

    // Compact GUI status box claims rows under the list when its row is active.
    #[cfg(target_os = "macos")]
    let compact_rows: Vec<WizardLine> = if show_gui_details {
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
    let compact_rows: Vec<WizardLine> = Vec::new();

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
    let mut windowed: Vec<WizardLine> = Vec::new();
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

    let mut lines = vec![wizard_centered(wizard_bold("Settings", Color::Cyan), width)];
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
        "Type model download token | Enter/Esc: Done"
    } else if editing_finch_api_key {
        "Type Finch client key | Enter/Esc: Done"
    } else {
        #[cfg(target_os = "macos")]
        {
            if show_gui_details {
                "R: Check | P: Prompt | O/D: More"
            } else {
                // The ◀/▶ affordance is advertised on every platform (#1140:
                // the context-lines spinner was undiscoverable on macOS).
                "↑/↓: Move | Space: Toggle | ◀/▶: Context lines | E: Edit selected key/token | Enter: Continue"
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            "↑/↓: Move | Space: Toggle | ◀/▶: Context lines | E: Edit selected key/token | Enter: Continue"
        }
    };
    lines.push(wizard_centered(
        wizard_bold(instructions_text, Color::Yellow),
        width,
    ));
    WizardSectionContent::plain(lines)
}

/// Review section: the summary the user confirms before saving.
fn review_section_lines(state: &WizardState, width: usize) -> Vec<WizardLine> {
    use crate::theme::ColorTheme;

    let mut body = vec![
        wizard_centered(wizard_bold("Ready to go!", Color::Green), width),
        wizard_centered(wizard_bold("Here's what you set up:", Color::Cyan), width),
    ];

    if let Some(SectionState::Themes { selected_theme }) =
        state.sections.get(&WizardSection::Themes)
    {
        let themes = ColorTheme::all();
        let theme_name = themes[*selected_theme].name().to_string();
        body.push(WizardLine::concat(&[
            wizard_line("Theme: ", Color::Yellow),
            wizard_plain(&theme_name),
        ]));
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
        body.push(WizardLine::concat(&[
            wizard_line("AI: ", Color::Yellow),
            wizard_plain(ai_label),
        ]));
    }

    if let Some(SectionState::Personas {
        default_persona, ..
    }) = state.sections.get(&WizardSection::Personas)
    {
        body.push(WizardLine::concat(&[
            wizard_line("Style: ", Color::Yellow),
            wizard_plain(default_persona),
        ]));
    }

    if let Some(SectionState::Features {
        auto_approve,
        streaming,
        debug,
        #[cfg(target_os = "macos")]
        gui_automation,
        daemon_only_mode,
        mdns_discovery,
        auto_discover,
        ..
    }) = state.sections.get(&WizardSection::Features)
    {
        // #1299: every Features-section toggle that's on, not a hardcoded
        // two-item allowlist — this used to silently drop settings with real
        // network effect (mDNS advertise, LAN peer discovery) from the last
        // review screen before saving. `feature_toggle_states` is the same
        // list the Settings tab renders, so a future toggle can't repeat this.
        let settings: Vec<&str> = feature_toggle_states(
            *streaming,
            *auto_approve,
            *debug,
            #[cfg(target_os = "macos")]
            *gui_automation,
            *daemon_only_mode,
            *mdns_discovery,
            *auto_discover,
        )
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect();
        let settings_text = if settings.is_empty() {
            "Defaults".to_string()
        } else {
            settings.join(", ")
        };
        body.push(WizardLine::concat(&[
            wizard_line("Settings: ", Color::Yellow),
            wizard_plain(&settings_text),
        ]));
    }

    body.push(WizardLine::blank());
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
            WizardLine::blank(),
        ],
        controls: Some(wizard_line(
            "Y / Enter: Discard    N / Esc: Keep editing",
            Color::Yellow,
        )),
        accent: Color::Yellow,
        pin_visible: None,
    }
}

pub(super) fn validation_error_card(error: &str) -> WizardCard {
    let clean_error = finch_ui_model::strip_ansi(error);
    WizardCard {
        title: "Validation Error".to_string(),
        body: vec![wizard_plain(&clean_error), WizardLine::blank()],
        controls: Some(wizard_line("Enter / Esc: Back to setup", Color::Yellow)),
        accent: Color::Red,
        pin_visible: None,
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
    let is_gemini_sub = cloud_provider_id(provider_idx).eq_ignore_ascii_case("gemini-sub");
    let title_text = if is_grok_sub {
        format!("Grok subscription device sign-in for {provider_name}")
    } else if is_gemini_sub {
        format!("Gemini subscription device sign-in for {provider_name}")
    } else {
        format!("ChatGPT device sign-in for {provider_name}")
    };
    let mut body: Vec<WizardLine> =
        vec![wizard_bold(&title_text, Color::Cyan), WizardLine::blank()];
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
            controls = wizard_line("Enter: Continue", Color::Yellow);
        }
        Some(Err(failure)) => {
            let summary = if is_grok_sub {
                grok_setup_failure_summary(grok_setup_failure_cause(failure))
            } else if is_gemini_sub {
                gemini_setup_failure_summary(gemini_setup_failure_cause(failure))
            } else {
                chatgpt_setup_failure_summary(chatgpt_setup_failure_cause(failure))
            };
            for sentence in failure_summary_sentences(&summary) {
                body.push(wizard_plain(&sentence));
            }
            controls = wizard_line(
                "Enter: Retry sign-in | Esc: Back to provider details",
                Color::Yellow,
            );
        }
        None => match pending.lock().unwrap().as_ref() {
            Some(presentation) => {
                // Both rows are links: clicking the first opens the sign-in
                // page, clicking the second copies the code. The address and
                // the code stay in the visible text for anyone not using a
                // mouse, and the O / Enter keys do both at once.
                let uri = &presentation.verification_uri;
                // A short address is shown inside the link. A long one (a
                // full OAuth authorize URL runs to several wrapped rows) is
                // not worth reading on screen: the link opens it, and a
                // second link copies it for a browser on another machine.
                const SHOWN_ADDRESS_MAX: usize = 60;
                if uri.chars().count() <= SHOWN_ADDRESS_MAX {
                    body.push(wizard_markdown(
                        &format!("[Click to open the device sign-in page: {uri}]({uri})"),
                        Color::Cyan,
                    ));
                } else {
                    body.push(wizard_markdown(
                        &format!("[Click to open the device sign-in page]({uri})"),
                        Color::Cyan,
                    ));
                    body.push(wizard_markdown(
                        &format!("[Click to copy the sign-in address](copy:{uri})"),
                        Color::Cyan,
                    ));
                }
                if !presentation.user_code.is_empty() {
                    let code = presentation.user_code.replace("-", "");
                    body.push(wizard_markdown(
                        &format!("[Click to copy the verification code ({code})](copy:{code})"),
                        Color::Cyan,
                    ));
                    body.push(WizardLine::blank());
                    body.push(wizard_plain(
                        "Approve the code in your browser; this dialog finishes automatically.",
                    ));
                    body.push(wizard_plain(&format!(
                        "The code expires in {} minutes.",
                        presentation.expires_in.as_secs().div_ceil(60)
                    )));
                } else {
                    body.push(WizardLine::blank());
                    body.push(wizard_plain(
                        "Complete sign-in in your browser; this dialog finishes automatically.",
                    ));
                }
                controls = wizard_line("O / Enter: Open & Copy Code | Esc: Cancel", Color::Yellow);
            }
            None => {
                body.push(wizard_plain("Starting the device sign-in…"));
                controls = wizard_line("Esc: Cancel", Color::Yellow);
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
fn remote_form_row(label: &str, value: &str, focused: bool, is_text_input: bool) -> WizardLine {
    let label_text = if focused {
        wizard_selected(format!("{:<10}", label))
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
        wizard_selected(value_text)
    } else {
        wizard_line(&value_text, Color::Cyan)
    };
    WizardLine::concat(&[label_text, value_painted])
}

/// How many model rows the provider form shows at once.
pub(super) const MODEL_CHOICE_WINDOW: usize = 6;

/// Rows the model list always occupies in the provider form: a heading, the
/// window of choices, and one note row. The line count is fixed whatever the
/// catalogue holds, so a list that grows from two built-in entries to a
/// hundred discovered ones, or a note that appears and clears, never moves
/// the rows beneath it (at a width the note fits on one row).
pub(super) const MODEL_CHOICE_ROWS: usize = MODEL_CHOICE_WINDOW + 2;

/// Longest model identifier shown in full; a longer one is cut with `…`.
const MODEL_CHOICE_MAX_CHARS: usize = 60;

fn model_choice_display(model: &str) -> String {
    if model.chars().count() <= MODEL_CHOICE_MAX_CHARS {
        return model.to_string();
    }
    let head: String = model.chars().take(MODEL_CHOICE_MAX_CHARS - 1).collect();
    format!("{head}…")
}

/// The visible model list of the provider form.
///
/// Shows what ←→ on the Model row moves through, with the selected entry
/// marked in words as well as by colour, so nobody has to know an identifier
/// from memory or cycle blind. A typed identifier the list does not contain
/// is called out here, before saving, rather than at the first query.
pub(super) fn model_choice_lines(
    models: &[String],
    current: &str,
    source: &CatalogSource,
    subscription: bool,
) -> Vec<WizardLine> {
    let mut lines = Vec::with_capacity(MODEL_CHOICE_ROWS);
    let selected = models.iter().position(|model| model == current);
    if models.is_empty() {
        lines.push(wizard_line(
            "No model list yet · type a model ID on the Model row",
            Color::DarkGray,
        ));
    } else if models.len() <= MODEL_CHOICE_WINDOW {
        lines.push(wizard_line(
            &format!(
                "Model choices ({}) · ←→ on the Model row picks one",
                models.len()
            ),
            Color::DarkGray,
        ));
    }
    // Keep the selected entry inside the window; with nothing selected the
    // window starts at the top of the list.
    let start = selected
        .map(|index| {
            index
                .saturating_sub(MODEL_CHOICE_WINDOW / 2)
                .min(models.len().saturating_sub(MODEL_CHOICE_WINDOW))
        })
        .unwrap_or(0);
    let end = (start + MODEL_CHOICE_WINDOW).min(models.len());
    if models.len() > MODEL_CHOICE_WINDOW {
        lines.push(wizard_line(
            &format!(
                "Model choices ({} to {} of {}) · ←→ on the Model row moves through all",
                start + 1,
                end,
                models.len()
            ),
            Color::DarkGray,
        ));
    }
    for (index, model) in models.iter().enumerate().take(end).skip(start) {
        let shown = model_choice_display(model);
        lines.push(if Some(index) == selected {
            wizard_line(&format!("  → {shown}  (selected)"), Color::Green)
        } else {
            wizard_line(&format!("    {shown}"), Color::Cyan)
        });
    }
    while lines.len() < MODEL_CHOICE_ROWS - 1 {
        lines.push(WizardLine::blank());
    }
    let typed = current.trim();
    let listed = *source != CatalogSource::StaticFallback;
    let note = if !typed.is_empty() && selected.is_none() && !models.is_empty() {
        let shown = model_choice_display(typed);
        Some(if listed {
            format!("'{shown}' is not in this list · the provider may reject it")
        } else {
            format!("'{shown}' is not in the built-in list · unchecked until the list is fetched")
        })
    } else if subscription && !listed {
        Some(
            "Built-in choices · this account's own list needs a signed-in subscription".to_string(),
        )
    } else {
        None
    };
    lines.push(match note {
        Some(note) => wizard_line(&note, Color::Yellow),
        None => WizardLine::blank(),
    });
    lines
}

/// The add-provider overlay as one claimed card: type selection, the
/// single-screen remote/local forms, the network scan, and the device
/// ceremony all render as body lines whose controls stay pinned inside the
/// card (#807) — no floating second painter.
pub(super) fn add_provider_card(
    step: &AddProviderStep,
    catalog_models: &[String],
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
                    wizard_selected(&format!(">>> {display_name} <<<"))
                } else {
                    wizard_line(&format!("    {display_name}"), Color::Cyan)
                });
                body.push(wizard_line(&format!("        {hint}"), Color::DarkGray));
            }
            let (local_line, scan_line) = if *selected == n_cloud {
                (
                    wizard_selected(">>> Local model <<<"),
                    wizard_line("    Scan local network", Color::DarkGray),
                )
            } else if *selected == n_cloud + 1 {
                (
                    wizard_line("    Local model", Color::Cyan),
                    wizard_selected(">>> Scan local network <<<"),
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
            // Every entry is a name line and a hint line; the card scrolls
            // to keep the selected pair on screen (#1651).
            let pinned = (*selected).min(n_cloud + 1) * 2;
            WizardCard::new(
                "Add AI Provider",
                body,
                Some(wizard_line(
                    "↑/↓: Move | Enter: Select | Esc: Cancel",
                    Color::Yellow,
                )),
            )
            .with_pin_visible(pinned, pinned + 2)
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
            let default_model = CLOUD_PROVIDERS[remote_idx].2;
            let key_hint = CLOUD_PROVIDERS[remote_idx].3;
            let editing = editing_idx.is_some();
            let provider_value =
                format!("{} ({})", provider_name, cloud_provider_id(*provider_idx));
            let model_display = if !model.is_empty() {
                model.as_str()
            } else if !default_model.is_empty() {
                "(default)"
            } else {
                "(choose a model)"
            };
            let mut body: Vec<WizardLine> = vec![
                WizardLine::blank(),
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
            body.push(WizardLine::blank());
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
                let msg = if error.starts_with("Model required:") {
                    error.to_string()
                } else if error.contains("required") {
                    format!("Model required: {error}")
                } else {
                    format!("Refresh warning: {error}")
                };
                body.push(wizard_line(&msg, Color::Yellow));
            }
            body.extend(model_choice_lines(
                catalog_models,
                model,
                catalog_source,
                CLOUD_PROVIDERS[remote_idx].0 == "chatgpt",
            ));
            let can_save = !model.is_empty() || !default_model.is_empty();
            let controls = if editing {
                if can_save {
                    "↑↓ navigate · ←→ pick model · type to edit · Ctrl+R refresh · Enter saves · Esc cancels"
                } else {
                    "↑↓ navigate · ←→ pick model · type to edit · Ctrl+R refresh · Enter chooses model · Esc cancels"
                }
            } else {
                if can_save {
                    "↑↓ navigate · ←→ change provider/model · Ctrl+R refresh · Enter adds · Esc back"
                } else {
                    "↑↓ navigate · ←→ change provider/model · Ctrl+R refresh · Enter chooses model · Esc back"
                }
            };
            WizardCard::new(
                if editing {
                    "Edit AI Provider"
                } else {
                    "Add Cloud Provider"
                },
                body,
                Some(wizard_line(controls, Color::Yellow)),
            )
        }
        AddProviderStep::ConfigureCompatibleConnection {
            draft,
            focused_field,
            editing_idx,
        } => {
            let kind = match draft.credential_kind {
                crate::config::CredentialKind::Bearer => "Bearer token",
                _ => "API key",
            };
            let body = vec![
                wizard_line(
                    "Protocol compatibility does not attest model capabilities.",
                    Color::Yellow,
                ),
                remote_form_row("Profile", &draft.name, *focused_field == 0, true),
                remote_form_row("Base URL", &draft.base_url, *focused_field == 1, true),
                remote_form_row("Chat path", &draft.chat_path, *focused_field == 2, true),
                remote_form_row("Models path", &draft.models_path, *focused_field == 3, true),
                remote_form_row("Model", &draft.model, *focused_field == 4, true),
                remote_form_row(
                    "Credential",
                    &draft.credential_ref,
                    *focused_field == 5,
                    true,
                ),
                remote_form_row("Secret env", &draft.secret_env, *focused_field == 6, true),
                remote_form_row("Auth", kind, *focused_field == 7, false),
                wizard_line(
                    "Only env:VARIABLE is saved; Finch never stores or displays the secret.",
                    Color::DarkGray,
                ),
            ];
            WizardCard::new(
                if editing_idx.is_some() {
                    "Edit Compatible Connection (1/2)"
                } else {
                    "Add Compatible Connection (1/2)"
                },
                body,
                Some(wizard_line(
                    "↑↓ navigate · type to edit · ←→ auth · Enter capabilities · Esc back",
                    Color::Yellow,
                )),
            )
        }
        AddProviderStep::ConfigureCompatibleCapabilities {
            draft,
            focused_field,
            editing_idx,
        } => {
            let attestation = |value: Option<bool>| match value {
                Some(true) => "supported",
                Some(false) => "unsupported",
                None => "unknown",
            };
            let tool_choice = match draft.tool_choice {
                crate::config::OpenAiCompatibleToolChoice::Omit => "omit",
                crate::config::OpenAiCompatibleToolChoice::Auto => "auto",
            };
            let body = vec![
                wizard_line(
                    "Declare only capabilities verified for this exact endpoint and model.",
                    Color::Yellow,
                ),
                remote_form_row(
                    "Streaming",
                    attestation(draft.streaming),
                    *focused_field == 0,
                    false,
                ),
                remote_form_row(
                    "Tools",
                    attestation(draft.tools),
                    *focused_field == 1,
                    false,
                ),
                remote_form_row(
                    "Parallel tools",
                    attestation(draft.parallel_tool_calls),
                    *focused_field == 2,
                    false,
                ),
                remote_form_row(
                    "Image input",
                    attestation(draft.image_input),
                    *focused_field == 3,
                    false,
                ),
                remote_form_row(
                    "Context tokens",
                    if draft.context_window_tokens.is_empty() {
                        "unknown"
                    } else {
                        &draft.context_window_tokens
                    },
                    *focused_field == 4,
                    true,
                ),
                remote_form_row(
                    "Max output",
                    if draft.max_output_tokens.is_empty() {
                        "unknown"
                    } else {
                        &draft.max_output_tokens
                    },
                    *focused_field == 5,
                    true,
                ),
                remote_form_row("Tool choice", tool_choice, *focused_field == 6, false),
                remote_form_row(
                    "Strict schemas",
                    attestation(draft.strict_tool_schemas),
                    *focused_field == 7,
                    false,
                ),
            ];
            WizardCard::new(
                if editing_idx.is_some() {
                    "Edit Compatible Capabilities (2/2)"
                } else {
                    "Add Compatible Capabilities (2/2)"
                },
                body,
                Some(wizard_line(
                    "↑↓ navigate · ←→ change/attest · type token limits · Enter saves · Esc back",
                    Color::Yellow,
                )),
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
            let device_name = execution_target_display(*execution);
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
            let mut body: Vec<WizardLine> = vec![
                WizardLine::blank(),
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
                WizardLine::blank(),
                WizardLine::concat(&[
                    wizard_line(&format!("{ram_estimate}  "), Color::Cyan),
                    wizard_line(&repo_preview, Color::DarkGray),
                ]),
            ]);
            WizardCard::new(
                if editing_idx.is_some() {
                    "Edit Local Model"
                } else {
                    "Add Local Model"
                },
                body,
                Some(wizard_line(
                    if editing_idx.is_some() {
                        "↑↓ navigate · ←→ change · leave path blank to download · Enter to save · Esc back"
                    } else {
                        "↑↓ navigate · ←→ change · leave path blank to download · Enter to add · Esc back"
                    },
                    Color::Yellow,
                )),
            )
        }
        // ── network scan path ────────────────────────────────────────────────
        AddProviderStep::Scanning { .. } => WizardCard::new(
            "Add AI Provider",
            vec![
                WizardLine::blank(),
                wizard_bold("Scanning for Finch agents on local network…", Color::Cyan),
                WizardLine::blank(),
                wizard_line("(this takes up to 5 seconds)", Color::DarkGray),
            ],
            Some(wizard_line("Esc: Cancel", Color::Yellow)),
        ),
        AddProviderStep::SelectAgent { agents, selected } => {
            let body: Vec<WizardLine> = agents
                .iter()
                .enumerate()
                .map(|(index, agent)| {
                    let label = format!("{} @ {}:{}", agent.name, agent.host, agent.port);
                    if index == *selected {
                        wizard_selected(&format!(">>> {label} <<<"))
                    } else {
                        wizard_line(&format!("    {label}"), Color::Cyan)
                    }
                })
                .collect();
            WizardCard::new(
                "Discovered agents",
                body,
                Some(wizard_line(
                    "↑/↓: Move | Enter: Add | Esc: Cancel",
                    Color::Yellow,
                )),
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
            themes_section_content(*selected_theme, width)
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
    let card = if let Some(error) = &state.save_error {
        Some(validation_error_card(error))
    } else if state.confirming_cancel {
        Some(cancel_confirm_card())
    } else {
        match state.sections.get(&WizardSection::Models) {
            Some(SectionState::Models {
                adding_provider: Some(step),
                catalog_models,
                catalog_source,
                catalog_refresh,
                catalog_refreshed_at,
                catalog_error,
                ..
            }) => Some(add_provider_card(
                step,
                catalog_models,
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
