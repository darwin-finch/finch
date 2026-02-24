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

    /// Render the content lines (all questions visible simultaneously, with help)
    fn render_content(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for (idx, tab_state) in self.dialog.tabs().iter().enumerate() {
            let is_current = idx == self.dialog.current_tab_index();

            // Section header — bold/highlighted when active, dimmed otherwise
            let header_style = if is_current {
                Style::default()
                    .fg(self.colors.dialog.title.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.ui.separator.to_color())
            };
            let label = if tab_state.answered {
                format!("{}. {} ✓", idx + 1, tab_state.question.question)
            } else {
                format!("{}. {}", idx + 1, tab_state.question.question)
            };
            lines.push(Line::from(Span::styled(label, header_style)));
            lines.push(Line::from(""));

            // Full options for every question (not just the active one)
            lines.extend(self.render_options(tab_state));

            // Separator between questions (omit after the last one)
            if idx < self.dialog.tabs().len() - 1 {
                lines.push(Line::from(Span::styled(
                    "─".repeat(44),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                )));
                lines.push(Line::from(""));
            }
        }

        lines.push(Line::from(""));
        lines.extend(self.render_help(self.dialog.current_tab()));

        lines
    }

    /// Render options for the current question
    fn render_options(&self, tab_state: &super::tabbed_dialog::TabState) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Render regular options
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

        // Add blank line before custom input section
        lines.push(Line::from(""));

        // Render custom input field inline (always visible)
        if tab_state.custom_mode_active {
            // Show custom text input with cursor at custom_cursor_pos
            let input_text = tab_state.custom_input.as_deref().unwrap_or("");
            let cursor_pos = tab_state.custom_cursor_pos;
            let mut spans = vec![Span::styled(
                "❯ ",
                Style::default()
                    .fg(self.colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD),
            )];
            if cursor_pos > 0 {
                let before: String = input_text.chars().take(cursor_pos).collect();
                spans.push(Span::styled(
                    before,
                    Style::default().fg(self.colors.dialog.option.to_color()),
                ));
            }
            if let Some(cursor_ch) = input_text.chars().nth(cursor_pos) {
                spans.push(Span::styled(
                    cursor_ch.to_string(),
                    Style::default()
                        .fg(self.colors.dialog.selected_fg.to_color())
                        .bg(self.colors.ui.cursor.to_color())
                        .add_modifier(Modifier::BOLD),
                ));
                let after: String = input_text.chars().skip(cursor_pos + 1).collect();
                if !after.is_empty() {
                    spans.push(Span::styled(
                        after,
                        Style::default().fg(self.colors.dialog.option.to_color()),
                    ));
                }
            } else {
                spans.push(Span::styled(
                    " ",
                    Style::default()
                        .bg(self.colors.ui.cursor.to_color())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(spans));

            // Show "Submit" option if there's custom text
            if !input_text.trim().is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "Press Enter to submit",
                    Style::default().fg(self.colors.dialog.option.to_color()),
                )));
            }
        } else {
            // Show prompt to enter custom response
            lines.push(Line::from(Span::styled(
                "Press 'o' for Other (custom response)",
                Style::default().fg(self.colors.dialog.option.to_color()),
            )));
        }

        lines
    }

    /// Render help text / keybindings
    fn render_help(&self, tab_state: &super::tabbed_dialog::TabState) -> Vec<Line<'static>> {
        let help_text = if tab_state.custom_mode_active {
            "Type | ←/→: Move cursor | Home/End | Del | Enter: Submit | Esc: Cancel"
        } else if tab_state.question.multi_select {
            "←/→: Switch question | ↑/↓: Navigate | Space: Toggle | Enter: Submit | o: Other | Esc: Cancel"
        } else {
            "←/→: Switch question | ↑/↓ or j/k: Navigate | 1-9: Select | Enter: Submit | o: Other | Esc: Cancel"
        };

        vec![Line::from(Span::styled(
            help_text,
            Style::default().fg(self.colors.ui.separator.to_color()),
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::llm_dialogs::{Question, QuestionOption};
    use crate::cli::tui::tabbed_dialog::TabbedDialog;
    use crate::config::ColorScheme;

    fn make_q(text: &str, opts: &[&str]) -> Question {
        Question {
            question: text.to_string(),
            header: text[..text.len().min(12)].to_string(),
            options: opts
                .iter()
                .map(|&l| QuestionOption {
                    label: l.to_string(),
                    description: format!("{l} desc"),
                    markdown: None,
                })
                .collect(),
            multi_select: false,
        }
    }

    fn lines_to_text(lines: &[Line]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref().to_string()))
            .collect()
    }

    /// Regression #19: all question texts must appear in rendered content,
    /// not just the active tab.
    #[test]
    fn test_render_content_shows_all_question_texts() {
        let dialog = TabbedDialog::new(
            vec![
                make_q("First question?", &["A", "B"]),
                make_q("Second question?", &["X", "Y"]),
            ],
            None,
        );
        let colors = ColorScheme::default();
        let w = TabbedDialogWidget::new(&dialog, &colors);
        let text = lines_to_text(&w.render_content());

        assert!(
            text.contains("First question?"),
            "First question must be visible"
        );
        assert!(
            text.contains("Second question?"),
            "Second question must be visible: {text}"
        );
    }

    #[test]
    fn test_render_content_single_question_shows_options() {
        let dialog = TabbedDialog::new(vec![make_q("Pick one?", &["Alpha", "Beta"])], None);
        let colors = ColorScheme::default();
        let w = TabbedDialogWidget::new(&dialog, &colors);
        let text = lines_to_text(&w.render_content());

        assert!(
            text.contains("Alpha"),
            "Option Alpha must be visible: {text}"
        );
        assert!(text.contains("Beta"), "Option Beta must be visible: {text}");
    }

    /// Regression #19: separator must appear between questions.
    #[test]
    fn test_render_content_shows_separator_between_questions() {
        let dialog = TabbedDialog::new(vec![make_q("Q1?", &["A"]), make_q("Q2?", &["B"])], None);
        let colors = ColorScheme::default();
        let w = TabbedDialogWidget::new(&dialog, &colors);
        let text = lines_to_text(&w.render_content());

        assert!(
            text.contains("────"),
            "Separator must appear between questions: {text}"
        );
    }
}

impl Widget for TabbedDialogWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Create border block
        let title = self
            .dialog
            .title()
            .map(|t| format!(" {} ", t))
            .unwrap_or_else(|| " Questions ".to_string());

        let block = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
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
