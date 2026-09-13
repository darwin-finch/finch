//! Drawing, one function per wizard section and overlay.
//!
//! The wizard's largest region and its least coupled: it references no subsystem outside
//! `cli` except `crate::theme`.

use super::*;

/// Render the tabbed wizard UI
pub(super) fn render_tabbed_wizard(f: &mut Frame, state: &WizardState) {
    #[cfg(target_os = "macos")]
    let permission_target = permission_target_description();
    #[cfg(not(target_os = "macos"))]
    let permission_target = String::new();
    render_tabbed_wizard_with_permission_target(f, state, &permission_target);
}

pub(super) fn render_tabbed_wizard_with_permission_target(
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

pub(super) fn render_cancel_confirmation(f: &mut Frame, area: Rect) {
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
pub(super) fn render_section_content(
    f: &mut Frame,
    area: Rect,
    state: &WizardState,
    permission_target: &str,
) {
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
pub(super) fn render_themes_section(f: &mut Frame, area: Rect, selected_theme: usize) {
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
pub(super) fn execution_target_display(execution: ExecutionTarget, coreml: CoreMlConfig) -> String {
    #[cfg(target_os = "macos")]
    if execution == ExecutionTarget::CoreML {
        return format!("CoreML ({})", coreml.compute_units.name());
    }

    execution.name().to_string()
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_models_section(
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
pub(super) fn render_add_provider_overlay(
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
pub(super) fn render_configure_remote_overlay(
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
pub(super) fn render_configure_local_overlay(
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
pub(super) fn render_personas_section(
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

#[allow(clippy::too_many_arguments)]
pub(super) fn render_features_section(
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
pub(super) fn render_review_section(f: &mut Frame, area: Rect, state: &WizardState) {
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
