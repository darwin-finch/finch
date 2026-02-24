//! Components not yet wired into the main render path.
#![allow(dead_code)]
// Status Widget - Renders the multi-line status bar
//
// Displays status lines from StatusBar in the bottom section of the TUI

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget},
};

use crate::cli::{StatusBar, StatusLineType};
use crate::config::ColorScheme;

/// Widget for rendering the status area
pub struct StatusWidget<'a> {
    status_bar: &'a StatusBar,
    colors: &'a ColorScheme,
}

impl<'a> StatusWidget<'a> {
    /// Create a new status widget
    pub fn new(status_bar: &'a StatusBar, colors: &'a ColorScheme) -> Self {
        Self { status_bar, colors }
    }

    /// Get the style for a status line based on its type
    fn get_line_style(&self, line_type: &StatusLineType) -> Style {
        match line_type {
            StatusLineType::SessionLabel => {
                // Session label: bold, prominent
                Style::default()
                    .fg(self.colors.ui.cursor.to_color())
                    .add_modifier(Modifier::BOLD)
            }
            StatusLineType::MemoryContext => {
                // Memory context: subtle teal/cyan
                Style::default().fg(Color::Cyan)
            }
            StatusLineType::ConversationTopic => {
                // Conversation topic: bright white so it reads easily
                Style::default().fg(Color::White)
            }
            StatusLineType::ConversationFocus => {
                // Current focus: de-emphasised relative to the topic line
                Style::default().fg(Color::DarkGray)
            }
            StatusLineType::LiveStats => {
                // Live stats: from color scheme
                Style::default()
                    .fg(self.colors.status.live_stats.to_color())
                    .add_modifier(Modifier::BOLD)
            }
            StatusLineType::TrainingStats => {
                // Training stats: from color scheme
                Style::default().fg(self.colors.status.training.to_color())
            }
            StatusLineType::DownloadProgress => {
                // Download progress: from color scheme
                Style::default()
                    .fg(self.colors.status.download.to_color())
                    .add_modifier(Modifier::BOLD)
            }
            StatusLineType::OperationStatus => {
                // Operation status: from color scheme
                Style::default().fg(self.colors.status.operation.to_color())
            }
            StatusLineType::Suggestions => {
                // Suggestions: cyan with subtle styling
                Style::default().fg(self.colors.ui.cursor.to_color())
            }
            StatusLineType::CompactionPercent => {
                // Compaction percentage: subtle gray
                Style::default().fg(Color::DarkGray)
            }
            StatusLineType::ContextLine(_) => {
                // Context line: de-emphasised like ConversationFocus
                Style::default().fg(Color::DarkGray)
            }
            StatusLineType::Custom(_) => {
                // Custom status lines: readable dark gray
                Style::default().fg(Color::DarkGray)
            }
        }
    }

    /// Convert a status line to a styled Line
    fn status_line_to_line(&self, line_type: &StatusLineType, content: &str) -> Line<'static> {
        let style = self.get_line_style(line_type);
        Line::from(Span::styled(content.to_string(), style))
    }
}

impl<'a> Widget for StatusWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Get all status lines
        let status_lines = self.status_bar.get_lines();

        // Check for compaction percentage line (displayed in title, not in content)
        let compaction_line = status_lines
            .iter()
            .find(|sl| sl.line_type == StatusLineType::CompactionPercent);

        // Convert to styled lines (excluding compaction line).
        // Insert a dim "── Memory ──" separator between MemoryContext and the
        // ConversationTopic/Focus block so the two sections are visually distinct.
        let separator_line = Line::from(Span::styled(
            "── Memory ──".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
        let mut lines: Vec<Line> = Vec::new();
        let mut prev_was_memory = false;
        for sl in status_lines
            .iter()
            .filter(|sl| sl.line_type != StatusLineType::CompactionPercent)
        {
            let is_memory = sl.line_type == StatusLineType::MemoryContext;
            let is_conversation = matches!(
                sl.line_type,
                StatusLineType::ConversationTopic
                    | StatusLineType::ConversationFocus
                    | StatusLineType::ContextLine(_)
            );
            // Insert separator at the boundary: memory → conversation
            if is_conversation && prev_was_memory {
                lines.push(separator_line.clone());
            }
            lines.push(self.status_line_to_line(&sl.line_type, &sl.content));
            prev_was_memory = is_memory;
        }

        // If no status lines, show empty
        let lines = if lines.is_empty() {
            vec![Line::from(Span::styled(
                " ",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            lines
        };

        // Build title with optional compaction percentage
        let title = if let Some(compaction) = compaction_line {
            format!(" Status • {} ", compaction.content)
        } else {
            " Status ".to_string()
        };

        // Create paragraph with top border and title
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .title(title)
                .title_alignment(Alignment::Right)
                .border_style(Style::default().fg(self.colors.status.border.to_color())),
        );

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_stats_style() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        let style = widget.get_line_style(&StatusLineType::TrainingStats);
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_download_progress_style() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        let style = widget.get_line_style(&StatusLineType::DownloadProgress);
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_operation_status_style() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        let style = widget.get_line_style(&StatusLineType::OperationStatus);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_custom_style() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        let style = widget.get_line_style(&StatusLineType::Custom("test".to_string()));
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_status_line_conversion() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        let line =
            widget.status_line_to_line(&StatusLineType::TrainingStats, "Training: 10 queries");
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn test_widget_creation() {
        let status_bar = StatusBar::new();
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);
        // Just verify it creates without panic
        assert_eq!(widget.status_bar.len(), 0);
    }

    /// Test that the "── Memory ──" separator is inserted when MemoryContext
    /// is immediately followed by a ConversationTopic line.
    #[test]
    fn test_memory_separator_inserted_between_memory_and_conversation() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let status_bar = StatusBar::new();
        status_bar.update_line(StatusLineType::MemoryContext, "🧠 neural · 10 memories");
        status_bar.update_line(StatusLineType::ConversationTopic, "📋 Rust lifetimes");
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);

        // Render into a buffer large enough to hold the lines
        let area = Rect::new(0, 0, 80, 10);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        // Collect all rendered text from the buffer
        let rendered: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            rendered.contains("Memory"),
            "Separator containing 'Memory' must appear between sections; got:\n{}",
            rendered
        );
    }

    /// Test that no separator is inserted when MemoryContext is absent.
    #[test]
    fn test_no_memory_separator_without_memory_context() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;

        let status_bar = StatusBar::new();
        status_bar.update_line(StatusLineType::ConversationTopic, "📋 Rust lifetimes");
        let colors = ColorScheme::default();
        let widget = StatusWidget::new(&status_bar, &colors);

        let area = Rect::new(0, 0, 80, 5);
        let mut buf = Buffer::empty(area);
        widget.render(area, &mut buf);

        let rendered: String = (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        // "── Memory ──" separator should NOT appear when only ConversationTopic is present
        assert!(
            !rendered.contains("── Memory ──"),
            "Separator must not appear without MemoryContext; got:\n{}",
            rendered
        );
    }
}
