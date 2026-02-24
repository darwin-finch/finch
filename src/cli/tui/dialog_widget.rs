// Dialog Widget - Ratatui Widget implementation for dialogs
//
// Renders dialogs inline with the TUI, matching the existing color scheme

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use super::dialog::{Dialog, DialogOption, DialogType};
use crate::config::ColorScheme;

/// Widget for rendering dialogs
pub struct DialogWidget<'a> {
    pub dialog: &'a Dialog,
    colors: &'a ColorScheme,
}

impl<'a> DialogWidget<'a> {
    /// Create a new dialog widget
    pub fn new(dialog: &'a Dialog, colors: &'a ColorScheme) -> Self {
        Self { dialog, colors }
    }

    /// Render a single-select dialog
    fn render_select(
        &self,
        options: &[DialogOption],
        selected_index: usize,
        allow_custom: bool,
        custom_input: &Option<String>,
        custom_mode_active: bool,
        custom_cursor_pos: usize,
        help: &Option<String>,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Add options with numbering
        for (idx, option) in options.iter().enumerate() {
            let is_selected = idx == selected_index;
            let number = idx + 1;

            // Format: "N. Label - Description"
            let prefix = if is_selected {
                Span::styled(
                    format!("{}. ", number),
                    Style::default()
                        .fg(self.colors.dialog.border.to_color())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!("{}. ", number),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                )
            };

            let label = if is_selected {
                Span::styled(
                    option.label.clone(),
                    Style::default()
                        .fg(self.colors.dialog.selected_fg.to_color())
                        .bg(self.colors.dialog.selected_bg.to_color())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    option.label.clone(),
                    Style::default().fg(self.colors.dialog.option.to_color()),
                )
            };

            let mut spans = vec![prefix, label];

            if let Some(desc) = &option.description {
                spans.push(Span::styled(
                    format!(" - {}", desc),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                ));
            }

            lines.push(Line::from(spans));
        }

        // Add "Other" option if enabled — numbered N+1 and highlighted when
        // the cursor has navigated to it (selected_index == options.len()) or
        // when the user is actively typing custom text (custom_mode_active).
        if allow_custom {
            lines.push(Line::from(""));
            let other_number = options.len() + 1;
            let other_label = format!("{}. Other (custom response)", other_number);
            let other_style = if custom_mode_active || selected_index == options.len() {
                Style::default()
                    .fg(self.colors.dialog.selected_fg.to_color())
                    .bg(self.colors.dialog.selected_bg.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.dialog.option.to_color())
            };
            lines.push(Line::from(Span::styled(other_label, other_style)));
        }

        // Show custom input field if active
        if custom_mode_active {
            if let Some(input_text) = custom_input {
                lines.push(Line::from(""));
                lines.push(Line::from(Self::render_custom_input_spans(
                    input_text,
                    custom_cursor_pos,
                    self.colors,
                )));
            }
        }

        // Add help message if present
        if let Some(help_text) = help {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                help_text.clone(),
                Style::default().fg(self.colors.status.operation.to_color()),
            )));
        }

        // Add keybindings hint
        lines.push(Line::from(""));
        if custom_mode_active {
            lines.push(Line::from(Span::styled(
                "Type | ←/→: Move cursor | Home/End | Del | Enter: Submit | Esc: Cancel",
                Style::default().fg(self.colors.ui.separator.to_color()),
            )));
        } else {
            let hint = if allow_custom {
                "↑/↓ or j/k: Navigate | 1-9: Select | o: Other | Enter: Confirm | Esc: Cancel"
            } else {
                "↑/↓ or j/k: Navigate | 1-9: Select directly | Enter: Confirm | Esc: Cancel"
            };
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(self.colors.ui.separator.to_color()),
            )));
        }

        lines
    }

    /// Render a multi-select dialog
    fn render_multiselect(
        &self,
        options: &[DialogOption],
        selected_indices: &std::collections::HashSet<usize>,
        cursor_index: usize,
        allow_custom: bool,
        custom_input: &Option<String>,
        custom_mode_active: bool,
        custom_cursor_pos: usize,
        help: &Option<String>,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Add options with checkboxes
        for (idx, option) in options.iter().enumerate() {
            let is_cursor = idx == cursor_index;
            let is_selected = selected_indices.contains(&idx);

            // Checkbox: [X] or [ ]
            let checkbox = if is_selected { "[X]" } else { "[ ]" };
            let checkbox_span = if is_cursor {
                Span::styled(
                    format!("{} ", checkbox),
                    Style::default()
                        .fg(self.colors.dialog.border.to_color())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!("{} ", checkbox),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                )
            };

            let label = if is_cursor {
                Span::styled(
                    option.label.clone(),
                    Style::default()
                        .fg(self.colors.dialog.selected_fg.to_color())
                        .bg(self.colors.dialog.selected_bg.to_color())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    option.label.clone(),
                    Style::default().fg(self.colors.dialog.option.to_color()),
                )
            };

            let mut spans = vec![checkbox_span, label];

            if let Some(desc) = &option.description {
                spans.push(Span::styled(
                    format!(" - {}", desc),
                    Style::default().fg(self.colors.ui.separator.to_color()),
                ));
            }

            lines.push(Line::from(spans));
        }

        // Add "Other" option if enabled — numbered N+1 and highlighted when
        // the cursor has navigated to it (cursor_index == options.len()) or
        // when the user is actively typing custom text (custom_mode_active).
        if allow_custom {
            lines.push(Line::from(""));
            let other_number = options.len() + 1;
            let other_label = format!("{}. Other (custom response)", other_number);
            let other_style = if custom_mode_active || cursor_index == options.len() {
                Style::default()
                    .fg(self.colors.dialog.selected_fg.to_color())
                    .bg(self.colors.dialog.selected_bg.to_color())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(self.colors.dialog.option.to_color())
            };
            lines.push(Line::from(Span::styled(other_label, other_style)));
        }

        // Show custom input field if active
        if custom_mode_active {
            if let Some(input_text) = custom_input {
                lines.push(Line::from(""));
                lines.push(Line::from(Self::render_custom_input_spans(
                    input_text,
                    custom_cursor_pos,
                    self.colors,
                )));
            }
        }

        // Add help message if present
        if let Some(help_text) = help {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                help_text.clone(),
                Style::default().fg(self.colors.status.operation.to_color()),
            )));
        }

        // Add keybindings hint
        lines.push(Line::from(""));
        if custom_mode_active {
            lines.push(Line::from(Span::styled(
                "Type | ←/→: Move cursor | Home/End | Del | Enter: Submit | Esc: Cancel",
                Style::default().fg(self.colors.ui.separator.to_color()),
            )));
        } else {
            let hint = if allow_custom {
                "↑/↓ or j/k: Navigate | Space: Toggle | o: Other | Enter: Confirm | Esc: Cancel"
            } else {
                "↑/↓ or j/k: Navigate | Space: Toggle | Enter: Confirm | Esc: Cancel"
            };
            lines.push(Line::from(Span::styled(
                hint,
                Style::default().fg(self.colors.ui.separator.to_color()),
            )));
        }

        lines
    }

    /// Build the spans for a custom "Other" input line, placing the block cursor
    /// at `cursor_pos` (char index).  Same 3-span approach as `render_text_input`.
    fn render_custom_input_spans(
        input: &str,
        cursor_pos: usize,
        colors: &ColorScheme,
    ) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(
            "> ",
            Style::default()
                .fg(colors.ui.cursor.to_color())
                .add_modifier(Modifier::BOLD),
        )];

        if cursor_pos > 0 {
            let before: String = input.chars().take(cursor_pos).collect();
            spans.push(Span::styled(
                before,
                Style::default().fg(colors.dialog.option.to_color()),
            ));
        }

        if let Some(cursor_ch) = input.chars().nth(cursor_pos) {
            spans.push(Span::styled(
                cursor_ch.to_string(),
                Style::default()
                    .fg(colors.dialog.selected_fg.to_color())
                    .bg(colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD),
            ));
            let after: String = input.chars().skip(cursor_pos + 1).collect();
            if !after.is_empty() {
                spans.push(Span::styled(
                    after,
                    Style::default().fg(colors.dialog.option.to_color()),
                ));
            }
        } else {
            // Cursor at end — show blank block
            spans.push(Span::styled(
                " ",
                Style::default()
                    .bg(colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        spans
    }

    /// Render a text input dialog
    fn render_text_input(
        &self,
        prompt: &str,
        input: &str,
        cursor_pos: usize,
        default: &Option<String>,
        help: &Option<String>,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Add prompt
        lines.push(Line::from(Span::styled(
            prompt.to_string(),
            Style::default().fg(self.colors.dialog.option.to_color()),
        )));

        // Show default if present
        if let Some(def) = default {
            lines.push(Line::from(Span::styled(
                format!("(default: {})", def),
                Style::default().fg(self.colors.ui.separator.to_color()),
            )));
        }

        lines.push(Line::from(""));

        // Render input field with cursor
        let mut input_spans = vec![Span::styled(
            "> ",
            Style::default()
                .fg(self.colors.ui.cursor.to_color())
                .add_modifier(Modifier::BOLD),
        )];

        // Add text before cursor (char-based to avoid byte-index panic on multi-byte chars)
        if cursor_pos > 0 {
            let before: String = input.chars().take(cursor_pos).collect();
            input_spans.push(Span::styled(
                before,
                Style::default().fg(self.colors.ui.input.to_color()),
            ));
        }

        // Add cursor (guard uses char count, not byte len, to avoid panic on multi-byte chars)
        if let Some(cursor_ch) = input.chars().nth(cursor_pos) {
            input_spans.push(Span::styled(
                cursor_ch.to_string(),
                Style::default()
                    .fg(self.colors.dialog.selected_fg.to_color())
                    .bg(self.colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD),
            ));

            // Add text after cursor (use char-based skip to avoid byte-index panic)
            let after: String = input.chars().skip(cursor_pos + 1).collect();
            if !after.is_empty() {
                input_spans.push(Span::styled(
                    after,
                    Style::default().fg(self.colors.ui.input.to_color()),
                ));
            }
        } else {
            // Cursor at end (show as block)
            input_spans.push(Span::styled(
                " ",
                Style::default()
                    .bg(self.colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD),
            ));
        }

        lines.push(Line::from(input_spans));

        // Add help message if present
        if let Some(help_text) = help {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                help_text.clone(),
                Style::default().fg(self.colors.status.operation.to_color()),
            )));
        }

        // Add keybindings hint
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Type to enter text | Backspace: Delete | ←/→: Move cursor | Enter: Confirm | Esc: Cancel",
            Style::default().fg(self.colors.ui.separator.to_color()),
        )));

        lines
    }

    /// Render a confirmation dialog
    fn render_confirm(
        &self,
        prompt: &str,
        default: bool,
        selected: bool,
        help: &Option<String>,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        // Add prompt
        lines.push(Line::from(Span::styled(
            prompt.to_string(),
            Style::default().fg(self.colors.dialog.option.to_color()),
        )));

        lines.push(Line::from(""));

        // Render Yes/No options
        let yes_style = if selected {
            Style::default()
                .fg(self.colors.dialog.selected_fg.to_color())
                .bg(self.colors.dialog.selected_bg.to_color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.colors.ui.separator.to_color())
        };

        let no_style = if !selected {
            Style::default()
                .fg(self.colors.dialog.selected_fg.to_color())
                .bg(self.colors.dialog.selected_bg.to_color())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.colors.ui.separator.to_color())
        };

        let yes_label = if default { "Yes (default)" } else { "Yes" };
        let no_label = if !default { "No (default)" } else { "No" };

        lines.push(Line::from(vec![
            Span::styled(format!("  {}  ", yes_label), yes_style),
            Span::raw("  "),
            Span::styled(format!("  {}  ", no_label), no_style),
        ]));

        // Add help message if present
        if let Some(help_text) = help {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                help_text.clone(),
                Style::default().fg(self.colors.status.operation.to_color()),
            )));
        }

        // Add keybindings hint
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "y/n: Select | ←/→: Toggle | Enter: Confirm | Esc: Cancel",
            Style::default().fg(self.colors.ui.separator.to_color()),
        )));

        lines
    }
}

impl<'a> Widget for DialogWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Generate content based on dialog type
        let lines = match &self.dialog.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                allow_custom,
            } => self.render_select(
                options,
                *selected_index,
                *allow_custom,
                &self.dialog.custom_input,
                self.dialog.custom_mode_active,
                self.dialog.custom_cursor_pos,
                &self.dialog.help_message,
            ),

            DialogType::MultiSelect {
                options,
                selected_indices,
                cursor_index,
                allow_custom,
            } => self.render_multiselect(
                options,
                selected_indices,
                *cursor_index,
                *allow_custom,
                &self.dialog.custom_input,
                self.dialog.custom_mode_active,
                self.dialog.custom_cursor_pos,
                &self.dialog.help_message,
            ),

            DialogType::TextInput {
                prompt,
                input,
                cursor_pos,
                default,
            } => self.render_text_input(
                prompt,
                input,
                *cursor_pos,
                default,
                &self.dialog.help_message,
            ),

            DialogType::Confirm {
                prompt,
                default,
                selected,
            } => self.render_confirm(prompt, *default, *selected, &self.dialog.help_message),
        };

        // Create paragraph with top border for header
        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(format!(" {} ", self.dialog.title))
                    .title_alignment(Alignment::Center)
                    .style(Style::default().fg(self.colors.dialog.border.to_color())),
            )
            .wrap(Wrap { trim: false });

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::tui::dialog::DialogOption;

    #[test]
    fn test_widget_creation() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select(
            "Test",
            vec![DialogOption::new("Option 1"), DialogOption::new("Option 2")],
        );

        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);
        assert_eq!(widget.dialog.title, "Test");
    }

    #[test]
    fn test_select_render() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select(
            "Test",
            vec![
                DialogOption::new("Option 1"),
                DialogOption::with_description("Option 2", "With description"),
            ],
        );

        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);
        let lines = widget.render_select(
            &[
                DialogOption::new("Option 1"),
                DialogOption::with_description("Option 2", "With description"),
            ],
            0,
            false, // allow_custom
            &None, // custom_input
            false, // custom_mode_active
            0,     // custom_cursor_pos
            &None, // help
        );

        // Should have: 2 options + empty line + keybindings = 4 lines
        assert!(lines.len() >= 3);
    }

    #[test]
    fn test_multiselect_render() {
        use crate::config::ColorScheme;
        use std::collections::HashSet;

        let dialog = Dialog::select(
            "Test",
            vec![DialogOption::new("Option 1"), DialogOption::new("Option 2")],
        );

        let mut selected = HashSet::new();
        selected.insert(0);

        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);
        let lines = widget.render_multiselect(
            &[DialogOption::new("Option 1"), DialogOption::new("Option 2")],
            &selected,
            0,
            false, // allow_custom
            &None, // custom_input
            false, // custom_mode_active
            0,     // custom_cursor_pos
            &None, // help
        );

        // Should have: 2 options + empty line + keybindings = 4 lines
        assert!(lines.len() >= 3);
    }

    #[test]
    fn test_text_input_render() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select("Test", vec![DialogOption::new("Option 1")]);

        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);
        let lines = widget.render_text_input("Enter text", "hello", 3, &None, &None);

        // Should have: prompt + empty line + input + empty line + keybindings = 5 lines
        assert!(lines.len() >= 4);
    }

    #[test]
    fn test_confirm_render() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select("Test", vec![DialogOption::new("Option 1")]);

        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);
        let lines = widget.render_confirm("Are you sure?", true, true, &None);

        // Should have: prompt + empty line + options + empty line + keybindings = 5 lines
        assert!(lines.len() >= 4);
    }

    // --- Regression: cursor rendering must not panic on multi-byte chars ---
    //
    // Previously, input[cursor_pos + 1..] used cursor_pos as a byte index, which
    // panics on multi-byte characters. Also, `chars().nth(cursor_pos).unwrap()`
    // would panic if cursor_pos was at-or-past the char count.

    #[test]
    fn test_text_input_cursor_at_end_does_not_panic() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select("Test", vec![DialogOption::new("Option 1")]);
        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);

        // cursor_pos == input.chars().count() (end of string)
        let input = "hello";
        let lines = widget.render_text_input("Prompt", input, input.chars().count(), &None, &None);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_text_input_cursor_with_unicode_does_not_panic() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select("Test", vec![DialogOption::new("Option 1")]);
        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);

        // Multi-byte chars: "héllo" — 5 chars, 6 bytes
        let input = "héllo";
        // cursor in the middle (char index 2, which is byte index 3)
        let lines = widget.render_text_input("Prompt", input, 2, &None, &None);
        assert!(!lines.is_empty());

        // cursor at end
        let lines = widget.render_text_input("Prompt", input, input.chars().count(), &None, &None);
        assert!(!lines.is_empty());
    }

    // ─── "Other" row rendering ────────────────────────────────────────────────

    /// The "Other" row must be numbered N+1 (where N = options count).
    #[test]
    fn test_select_render_other_is_numbered() {
        use crate::config::ColorScheme;

        let dialog = Dialog::select_with_custom(
            "T",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
        );
        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);

        let lines = widget.render_select(
            &[
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
            0,     // selected_index
            true,  // allow_custom
            &Some(String::new()),
            false, // custom_mode_active
            0,
            &None,
        );

        // Collect all span text across all lines
        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");

        assert!(
            all_text.contains("4. Other"),
            "Other row must be numbered 4 for a 3-option list, got: {}",
            all_text
        );
    }

    /// The "Other" row must render with selection highlight when
    /// selected_index == options.len() and custom_mode_active is false.
    #[test]
    fn test_select_render_other_highlighted_when_navigated_to() {
        use crate::config::ColorScheme;
        use ratatui::style::Modifier;

        let dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        let colors = ColorScheme::default();
        let widget = DialogWidget::new(&dialog, &colors);

        // selected_index = 1 = options.len() → Other row should be highlighted
        let lines = widget.render_select(
            &[DialogOption::new("A")],
            1,     // selected_index == options.len()
            true,  // allow_custom
            &Some(String::new()),
            false, // custom_mode_active
            0,
            &None,
        );

        // Find the "Other" span
        let other_span = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains("Other"));

        assert!(other_span.is_some(), "Other span must exist in rendered lines");
        let span = other_span.unwrap();
        assert!(
            span.style.add_modifier.contains(Modifier::BOLD),
            "Other span must be BOLD when selected_index == options.len()"
        );
    }
}
