//! Components not yet wired into the main render path.
#![allow(dead_code)]
// Autocomplete Dropdown Widget - Beautiful command suggestions with descriptions
//
// Renders a dropdown menu above the input area showing matching commands
// with syntax hints, descriptions, and category grouping.

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use crate::cli::command_autocomplete::{CommandSpec, CommandCategory};
use crate::config::ColorScheme;

/// Maximum number of autocomplete suggestions to show at once
const MAX_VISIBLE_SUGGESTIONS: usize = 8;

/// Autocomplete state for TUI rendering
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct AutocompleteState {
    /// Matched commands from registry
    pub matches: Vec<CommandSpec>,
    /// Currently selected index (for up/down navigation)
    pub selected_index: usize,
    /// Whether the dropdown is visible
    pub visible: bool,
}


impl AutocompleteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update matches and show dropdown
    pub fn show_matches(&mut self, matches: Vec<CommandSpec>) {
        self.matches = matches;
        self.selected_index = 0;
        self.visible = !self.matches.is_empty();
    }

    /// Hide the dropdown
    pub fn hide(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.selected_index = 0;
    }

    /// Get the currently selected command (if any)
    pub fn get_selected(&self) -> Option<&CommandSpec> {
        if self.visible && self.selected_index < self.matches.len() {
            Some(&self.matches[self.selected_index])
        } else {
            None
        }
    }

    /// Move selection up (wraps around)
    pub fn select_previous(&mut self) {
        if !self.matches.is_empty() {
            if self.selected_index == 0 {
                self.selected_index = self.matches.len() - 1;
            } else {
                self.selected_index -= 1;
            }
        }
    }

    /// Move selection down (wraps around)
    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.matches.len();
        }
    }
}

/// Get category color for visual grouping
fn category_color(category: CommandCategory) -> Color {
    match category {
        CommandCategory::Basic => Color::Cyan,
        CommandCategory::Model => Color::Magenta,
        CommandCategory::Mcp => Color::Yellow,
        CommandCategory::Persona => Color::LightBlue,
        CommandCategory::Patterns => Color::LightGreen,
        CommandCategory::Feedback => Color::LightMagenta,
        CommandCategory::Memory => Color::LightYellow,
        CommandCategory::Discovery => Color::LightCyan,
    }
}

/// Render the autocomplete dropdown above the input area
pub fn render_autocomplete_dropdown(
    frame: &mut Frame,
    state: &AutocompleteState,
    area: Rect,
    _colors: &ColorScheme,
) {
    if !state.visible || state.matches.is_empty() {
        return;
    }

    // Calculate dropdown dimensions
    let num_items = state.matches.len().min(MAX_VISIBLE_SUGGESTIONS);
    let dropdown_height = (num_items as u16 + 2).min(area.height); // +2 for borders

    // Position dropdown above the input area
    let dropdown_area = Rect {
        x: area.x,
        y: area.y.saturating_sub(dropdown_height),
        width: area.width.min(80), // Max width 80 chars
        height: dropdown_height,
    };

    // Build list items with formatting
    let items: Vec<ListItem> = state
        .matches
        .iter()
        .take(MAX_VISIBLE_SUGGESTIONS)
        .enumerate()
        .map(|(idx, cmd)| {
            let is_selected = idx == state.selected_index;
            let cat_color = category_color(cmd.category);

            // Format: "❯ /command <params>  │  Description"
            let mut spans = Vec::new();

            // Selection indicator
            if is_selected {
                spans.push(Span::styled("❯ ", Style::default().fg(Color::Green)));
            } else {
                spans.push(Span::raw("  "));
            }

            // Command name (bold if selected)
            let cmd_style = if is_selected {
                Style::default()
                    .fg(cat_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(cat_color)
            };
            spans.push(Span::styled(cmd.name, cmd_style));

            // Parameters (if any)
            if let Some(params) = cmd.params {
                spans.push(Span::styled(
                    format!(" {}", params),
                    Style::default().fg(Color::DarkGray),
                ));
            }

            // Separator
            spans.push(Span::raw("  │  "));

            // Description
            let desc_style = if is_selected {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(cmd.description, desc_style));

            ListItem::new(Line::from(spans))
        })
        .collect();

    // Render block with title
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(
            format!(" Commands ({}) ", state.matches.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ));

    let list = List::new(items).block(block);

    frame.render_widget(list, dropdown_area);

    // Show navigation hint at bottom if truncated
    if state.matches.len() > MAX_VISIBLE_SUGGESTIONS {
        let hint_y = dropdown_area.y + dropdown_area.height - 1;
        let hint_area = Rect {
            x: dropdown_area.x + 2,
            y: hint_y,
            width: dropdown_area.width.saturating_sub(4),
            height: 1,
        };

        let hint = Paragraph::new(Span::styled(
            format!("↑↓ to navigate  •  {} more", state.matches.len() - MAX_VISIBLE_SUGGESTIONS),
            Style::default().fg(Color::DarkGray),
        ));

        frame.render_widget(hint, hint_area);
    }
}

/// Render minimal autocomplete hints inline (alternative to dropdown)
pub fn render_inline_hint(
    frame: &mut Frame,
    state: &AutocompleteState,
    area: Rect,
    _colors: &ColorScheme,
) {
    if !state.visible || state.matches.is_empty() {
        return;
    }

    // Show only the selected command's description
    if let Some(cmd) = state.get_selected() {
        let hint_text = format!("  {} — {}", cmd.full_syntax(), cmd.description);
        let hint = Paragraph::new(Span::styled(
            hint_text,
            Style::default().fg(Color::DarkGray),
        ));

        frame.render_widget(hint, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autocomplete_state() {
        let mut state = AutocompleteState::new();
        assert!(!state.visible);
        assert_eq!(state.matches.len(), 0);

        let matches = vec![
            CommandSpec {
                name: "/clear",
                params: None,
                description: "Clear history",
                category: CommandCategory::Basic,
            },
            CommandSpec {
                name: "/compact",
                params: Some("[instruction]"),
                description: "Compact history",
                category: CommandCategory::Basic,
            },
        ];

        state.show_matches(matches.clone());
        assert!(state.visible);
        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.selected_index, 0);

        state.select_next();
        assert_eq!(state.selected_index, 1);

        state.select_next();
        assert_eq!(state.selected_index, 0); // Wrapped around

        state.select_previous();
        assert_eq!(state.selected_index, 1); // Wrapped backward

        state.hide();
        assert!(!state.visible);
        assert_eq!(state.matches.len(), 0);
    }

    #[test]
    fn test_get_selected() {
        let mut state = AutocompleteState::new();

        let matches = vec![
            CommandSpec {
                name: "/help",
                params: None,
                description: "Show help",
                category: CommandCategory::Basic,
            },
        ];

        state.show_matches(matches);
        assert!(state.get_selected().is_some());
        assert_eq!(state.get_selected().unwrap().name, "/help");

        state.hide();
        assert!(state.get_selected().is_none());
    }
}
