// Setup Wizard - First-run configuration

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use std::io;

use crate::config::{BackendDevice, TeacherEntry};
use crate::models::unified_loader::{ModelFamily, ModelSize};

/// Setup wizard result containing all collected configuration
pub struct SetupResult {
    pub claude_api_key: String,
    pub hf_token: Option<String>,
    pub backend_device: BackendDevice,
    pub model_family: ModelFamily,
    pub model_size: ModelSize,
    pub teachers: Vec<TeacherEntry>,
}

enum WizardStep {
    Welcome,
    ClaudeApiKey(String),
    HfToken(String),
    DeviceSelection(usize),
    ModelFamilySelection(usize),
    ModelSizeSelection(usize),
    TeacherConfig(Vec<TeacherEntry>, usize), // (teachers list, selected index)
    Confirm,
}

/// Show first-run setup wizard and return configuration
pub fn show_setup_wizard() -> Result<SetupResult> {
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

    // Wizard state
    let mut step = WizardStep::Welcome;
    let mut claude_key = String::new();
    let mut hf_token = String::new();
    let devices = BackendDevice::available_devices();
    let mut selected_device_idx = 0;

    let model_families = vec![
        ModelFamily::Qwen2,
        ModelFamily::Gemma2,
        ModelFamily::Llama3,
        ModelFamily::Mistral,
    ];
    let mut selected_family_idx = 0;

    let model_sizes = vec![
        ModelSize::Small,
        ModelSize::Medium,
        ModelSize::Large,
        ModelSize::XLarge,
    ];
    let mut selected_size_idx = 1; // Default to Medium

    let mut teachers: Vec<TeacherEntry> = vec![TeacherEntry {
        provider: "claude".to_string(),
        api_key: String::new(), // Will be filled from claude_key
        model: None,
        base_url: None,
        name: Some("Claude (Primary)".to_string()),
    }];
    let mut selected_teacher_idx = 0;

    let result = loop {
        terminal.draw(|f| {
            render_wizard_step(f, &step, &devices, &model_families, &model_sizes);
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
                            step = WizardStep::DeviceSelection(selected_device_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::DeviceSelection(selected) => {
                    match key.code {
                        KeyCode::Up => {
                            if *selected > 0 {
                                *selected -= 1;
                                selected_device_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < devices.len() - 1 {
                                *selected += 1;
                                selected_device_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            step = WizardStep::ModelFamilySelection(selected_family_idx);
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
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
                            if *selected > 0 {
                                *selected -= 1;
                                selected_teacher_idx = *selected;
                            }
                        }
                        KeyCode::Down => {
                            if *selected < teacher_list.len() - 1 {
                                *selected += 1;
                                selected_teacher_idx = *selected;
                            }
                        }
                        KeyCode::Enter => {
                            teachers = teacher_list.clone();
                            step = WizardStep::Confirm;
                        }
                        KeyCode::Char('a') => {
                            // Add new teacher (simplified - just show we can skip for now)
                            teachers = teacher_list.clone();
                            step = WizardStep::Confirm;
                        }
                        KeyCode::Esc => {
                            anyhow::bail!("Setup cancelled");
                        }
                        _ => {}
                    }
                }

                WizardStep::Confirm => {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Enter => {
                            break Ok(SetupResult {
                                claude_api_key: claude_key.clone(),
                                hf_token: if hf_token.is_empty() { None } else { Some(hf_token.clone()) },
                                backend_device: devices[selected_device_idx],
                                model_family: model_families[selected_family_idx],
                                model_size: model_sizes[selected_size_idx],
                                teachers: teachers.clone(),
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
    };

    // Restore terminal
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        terminal.backend_mut(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    )?;

    result
}

fn render_wizard_step(
    f: &mut Frame,
    step: &WizardStep,
    devices: &[BackendDevice],
    model_families: &[ModelFamily],
    model_sizes: &[ModelSize],
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
        WizardStep::DeviceSelection(selected) => render_device_selection(f, inner, devices, *selected),
        WizardStep::ModelFamilySelection(selected) => render_model_family_selection(f, inner, model_families, *selected),
        WizardStep::ModelSizeSelection(selected) => render_model_size_selection(f, inner, model_sizes, *selected),
        WizardStep::TeacherConfig(teachers, selected) => render_teacher_config(f, inner, teachers, *selected),
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
         • Inference device (CoreML/Metal/CPU)\n\n\
         Press Enter or Space to continue, Esc to cancel."
    )
    .style(Style::default().fg(Color::Reset))
    .alignment(Alignment::Left)
    .wrap(Wrap { trim: false });
    f.render_widget(message, chunks[1]);

    let help = Paragraph::new("Enter/Space: Continue  Esc: Cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_api_key_input(f: &mut Frame, area: Rect, input: &str) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(5),  // Instructions
            Constraint::Length(3),  // Input
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 1: Teacher Provider API Key")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Enter your API key for the TEACHER provider (Claude by default).\n\
         This is the AI model that will teach your local model.\n\n\
         Get Claude API key from: https://console.anthropic.com/\n\
         (starts with sk-ant-...)\n\n\
         You can configure other providers (OpenAI, Gemini, etc.) later."
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    // Mask the key for display (show only last 4 chars)
    let display_text = if input.len() > 8 {
        format!("{}...{}", &input[..7], &input[input.len()-4..])
    } else {
        input.to_string()
    };

    let input_widget = Paragraph::new(display_text)
        .block(Block::default().borders(Borders::ALL).title("Teacher API Key (Claude)"))
        .style(Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD));
    f.render_widget(input_widget, chunks[2]);

    let help = Paragraph::new("Type key then press Enter  Esc: Cancel")
        .style(Style::default().fg(Color::DarkGray))
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
    } else if input.len() > 8 {
        format!("{}...{}", &input[..7], &input[input.len()-4..])
    } else {
        input.to_string()
    };

    let input_widget = Paragraph::new(display_text)
        .block(Block::default().borders(Borders::ALL).title("HF Token"))
        .style(Style::default().fg(Color::LightGreen).add_modifier(Modifier::BOLD));
    f.render_widget(input_widget, chunks[2]);

    let help = Paragraph::new("Type token then press Enter (or Enter to skip)  Esc: Cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
}

fn render_device_selection(f: &mut Frame, area: Rect, devices: &[BackendDevice], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Min(8),     // Device list
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 3: Select Inference Device")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = devices
        .iter()
        .map(|device| {
            let description = device.description();
            let emoji = match device {
                #[cfg(target_os = "macos")]
                BackendDevice::CoreML => "⚡",
                #[cfg(target_os = "macos")]
                BackendDevice::Metal => "🚀",
                #[cfg(feature = "cuda")]
                BackendDevice::Cuda => "💨",
                BackendDevice::Cpu => "🐌",
                BackendDevice::Auto => "🤖",
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
        .style(Style::default().fg(Color::DarkGray))
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
        .style(Style::default().fg(Color::DarkGray))
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
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[2]);
}

fn render_teacher_config(f: &mut Frame, area: Rect, teachers: &[TeacherEntry], selected: usize) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // Title
            Constraint::Length(5),  // Instructions
            Constraint::Min(6),     // Teacher list
            Constraint::Length(2),  // Help
        ])
        .split(area);

    let title = Paragraph::new("Step 6: Teacher Configuration")
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    f.render_widget(title, chunks[0]);

    let instructions = Paragraph::new(
        "Configure teacher models for learning. The first teacher is primary.\n\
         You can add more teachers later by editing ~/.shammah/config.toml"
    )
    .style(Style::default().fg(Color::Reset))
    .wrap(Wrap { trim: false });
    f.render_widget(instructions, chunks[1]);

    let items: Vec<ListItem> = teachers
        .iter()
        .map(|teacher| {
            let name = teacher.name.as_deref().unwrap_or(&teacher.provider);
            ListItem::new(Line::from(vec![
                Span::styled(name, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                Span::raw(" ("),
                Span::styled(&teacher.provider, Style::default().fg(Color::Gray)),
                Span::raw(")"),
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

    f.render_stateful_widget(list, chunks[2], &mut list_state);

    let help = Paragraph::new("Enter: Continue  Esc: Cancel")
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(help, chunks[3]);
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
        "Configuration will be saved to: ~/.shammah/config.toml\n\n\
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
        .style(Style::default().fg(Color::DarkGray))
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
