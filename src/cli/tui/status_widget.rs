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

        // Convert to styled lines (excluding compaction line)
        let lines: Vec<Line> = status_lines
            .iter()
            .filter(|sl| sl.line_type != StatusLineType::CompactionPercent)
            .map(|sl| self.status_line_to_line(&sl.line_type, &sl.content))
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
}
