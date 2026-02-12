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

/// Widget for rendering the status area
pub struct StatusWidget<'a> {
    status_bar: &'a StatusBar,
}

impl<'a> StatusWidget<'a> {
    /// Create a new status widget
    pub fn new(status_bar: &'a StatusBar) -> Self {
        Self { status_bar }
    }

    /// Get the style for a status line based on its type
    fn get_line_style(line_type: &StatusLineType) -> Style {
        match line_type {
            StatusLineType::LiveStats => {
                // Live stats: bright green
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD)
            }
            StatusLineType::TrainingStats => {
                // Training stats: gray
                Style::default().fg(Color::DarkGray)
            }
            StatusLineType::DownloadProgress => {
                // Download progress: cyan
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            }
            StatusLineType::OperationStatus => {
                // Operation status: yellow
                Style::default().fg(Color::Yellow)
            }
            StatusLineType::Custom(_) => {
                // Custom status lines: readable dark gray
                Style::default().fg(Color::DarkGray)
            }
        }
    }

    /// Convert a status line to a styled Line
    fn status_line_to_line(line_type: &StatusLineType, content: &str) -> Line<'static> {
        let style = Self::get_line_style(line_type);
        Line::from(Span::styled(content.to_string(), style))
    }
}

impl<'a> Widget for StatusWidget<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        // Get all status lines
        let status_lines = self.status_bar.get_lines();

        // Convert to styled lines
        let lines: Vec<Line> = status_lines
            .iter()
            .map(|sl| Self::status_line_to_line(&sl.line_type, &sl.content))
            .collect();

        // If no status lines, show empty
        let lines = if lines.is_empty() {
            vec![Line::from(Span::styled(
                " ",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            lines
        };

        // Create paragraph with top border and "Status" title
        let paragraph = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::TOP)
                .title(" Status ")
                .title_alignment(Alignment::Right)
                .border_style(Style::default().fg(Color::Gray)),
        );

        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_stats_style() {
        let style = StatusWidget::get_line_style(&StatusLineType::TrainingStats);
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_download_progress_style() {
        let style = StatusWidget::get_line_style(&StatusLineType::DownloadProgress);
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_operation_status_style() {
        let style = StatusWidget::get_line_style(&StatusLineType::OperationStatus);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_custom_style() {
        let style = StatusWidget::get_line_style(&StatusLineType::Custom("test".to_string()));
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_status_line_conversion() {
        let line = StatusWidget::status_line_to_line(
            &StatusLineType::TrainingStats,
            "Training: 10 queries",
        );
        assert_eq!(line.spans.len(), 1);
    }

    #[test]
    fn test_widget_creation() {
        let status_bar = StatusBar::new();
        let widget = StatusWidget::new(&status_bar);
        // Just verify it creates without panic
        assert_eq!(widget.status_bar.len(), 0);
    }
}
