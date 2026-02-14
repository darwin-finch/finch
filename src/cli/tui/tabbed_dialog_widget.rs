// Tabbed Dialog Widget - Ratatui renderer for tabbed dialogs
//
// Renders multiple questions with tab navigation, similar to Claude Code

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use super::tabbed_dialog::TabbedDialog;
use crate::config::ColorScheme;

/// Widget for rendering tabbed dialogs
pub struct TabbedDialogWidget<'a> {
    pub dialog: &'a TabbedDialog,
    colors: &'a ColorScheme,
}

impl<'a> TabbedDialogWidget<'a> {
    pub fn new(dialog: &'a TabbedDialog, colors: &'a ColorScheme) -> Self {
        Self { dialog, colors }
    }

    /// Render the content lines (tabs, question, options, help)
    fn render_content(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Render tabs at the top
        lines.extend(self.render_tabs());
        lines.push(Line::from(""));

        // Render current question
        let current_tab = self.dialog.current_tab();

        // Question text
        lines.push(Line::from(Span::styled(
            current_tab.question.question.clone(),
            Style::default()
                .fg(self.colors.dialog.title.to_color())
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        // Render options for current question
        if current_tab.custom_mode_active {
            // Show custom input field
            lines.extend(self.render_custom_input(current_tab));
        } else {
            lines.extend(self.render_options(current_tab));
        }

        // Render help/keybindings
        lines.push(Line::from(""));
        lines.extend(self.render_help(current_tab));

        lines
    }

    /// Render tab headers
    fn render_tabs(&self) -> Vec<Line<'static>> {
        let mut tab_spans = Vec::new();

        for (idx, tab_state) in self.dialog.tabs().iter().enumerate() {
            let is_current = idx == self.dialog.current_tab_index();
            let header = &tab_state.question.header;

            // Add visual indicator for answered tabs
            let label = if tab_state.answered {
                format!("✓ {}", header)
            } else {
                header.clone()
            };

            let style = if is_current {
                // Current tab: highlighted
                Style::default()
                    .fg(self.colors.dialog.selected_fg.to_color())
                    .bg(self.colors.dialog.selected_bg.to_color())
                    .add_modifier(Modifier::BOLD)
            } else if tab_state.answered {
                // Answered tab: dimmed with checkmark (use green-ish color)
                Style::default()
                    .fg(ratatui::style::Color::Green)
            } else {
                // Unanswered tab: normal
                Style::default().fg(self.colors.dialog.option.to_color())
            };

            tab_spans.push(Span::styled(format!(" {} ", label), style));

            // Add separator between tabs
            if idx < self.dialog.tabs().len() - 1 {
                tab_spans.push(Span::styled(" │ ", Style::default().fg(self.colors.ui.separator.to_color())));
            }
        }

        vec![Line::from(tab_spans)]
    }

    /// Render options for the current question
    fn render_options(&self, tab_state: &super::tabbed_dialog::TabState) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for (idx, option) in tab_state.question.options.iter().enumerate() {
            let is_selected = if tab_state.question.multi_select {
                // Multi-select: check if in selected set
                tab_state.selected_indices.contains(&idx)
            } else {
                // Single-select: check if cursor is here
                idx == tab_state.selected_index
            };

            let is_cursor = idx == tab_state.selected_index;

            // Build prefix as a String
            let prefix = if tab_state.question.multi_select {
                // Checkbox
                if is_selected {
                    "☑ ".to_string()
                } else {
                    "☐ ".to_string()
                }
            } else {
                // Number
                format!("{}. ", idx + 1)
            };

            let prefix_style = if is_cursor {
                Style::default()
                    .fg(self.colors.dialog.border.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.ui.separator.to_color())
            };

            let label_style = if is_cursor {
                Style::default()
                    .fg(self.colors.dialog.selected_fg.to_color())
                    .bg(self.colors.dialog.selected_bg.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.dialog.option.to_color())
            };

            let mut spans = vec![
                Span::styled(prefix, prefix_style),
                Span::styled(option.label.clone(), label_style),
            ];

            if !option.description.is_empty() {
                spans.push(Span::styled(
                    format!(" - {}", option.description),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                ));
            }

            lines.push(Line::from(spans));
        }

        // Add "Press 'o' for Other" option
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Press 'o' for Other (custom response)",
            Style::default().fg(self.colors.dialog.option.to_color()),
        )));

        lines
    }

    /// Render custom input field
    fn render_custom_input(&self, tab_state: &super::tabbed_dialog::TabState) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        if let Some(input_text) = &tab_state.custom_input {
            lines.push(Line::from(vec![
                Span::styled("> ", Style::default().fg(self.colors.ui.cursor.to_color()).add_modifier(Modifier::BOLD)),
                Span::styled(input_text.clone(), Style::default().fg(self.colors.dialog.option.to_color())),
                Span::styled("█", Style::default().fg(self.colors.ui.cursor.to_color())),
            ]));
        }

        lines
    }

    /// Render help text / keybindings
    fn render_help(&self, tab_state: &super::tabbed_dialog::TabState) -> Vec<Line<'static>> {
        let help_text = if tab_state.custom_mode_active {
            "Type response | Enter: Save & Next | Esc: Cancel custom input"
        } else if tab_state.question.multi_select {
            "←/→: Switch tabs | ↑/↓: Navigate | Space: Toggle | Enter: Save & Next | Esc: Cancel"
        } else {
            "←/→: Switch tabs | ↑/↓ or j/k: Navigate | 1-9: Select | Enter: Save & Next | Esc: Cancel"
        };

        vec![Line::from(Span::styled(
            help_text,
            Style::default().fg(self.colors.ui.separator.to_color()),
        ))]
    }
}

impl Widget for TabbedDialogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Create border block
        let title = self.dialog.title()
            .map(|t| format!(" {} ", t))
            .unwrap_or_else(|| " Questions ".to_string());

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.colors.dialog.border.to_color()))
            .title(title)
            .title_alignment(Alignment::Center);

        // Render content inside block
        let inner_area = block.inner(area);
        block.render(area, buf);

        // Render content lines
        let content = self.render_content();
        let paragraph = Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .alignment(Alignment::Left);

        paragraph.render(inner_area, buf);
    }
}
