// Input Widget - Helper to render tui-textarea
//
// Note: This is not a proper Widget implementation due to tui-textarea's API.
// Instead, we provide a helper function to render the textarea.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::Span,
    widgets::Paragraph,
};
use tui_textarea::TextArea;

use crate::config::ColorScheme;

/// Render a TextArea with a colored prompt prefix and optional ghost text
pub fn render_input_widget<'a>(
    frame: &mut Frame,
    textarea: &'a TextArea<'a>,
    area: Rect,
    prompt: &str,
    colors: &ColorScheme,
    ghost_text: Option<&str>,
) {
    // Split area: prompt (3 chars) + textarea (rest)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(3),  // Prompt: " ❯ "
            Constraint::Min(1),     // Textarea: rest of line
        ])
        .split(area);

    // Render colored prompt
    let prompt_text = format!(" {} ", prompt);
    let prompt_widget = Paragraph::new(Span::styled(
        prompt_text,
        Style::default().fg(colors.ui.cursor.to_color()),
    ));
    frame.render_widget(prompt_widget, chunks[0]);

    // Render textarea
    frame.render_widget(textarea, chunks[1]);

    // Render ghost text if available (single-line only)
    if let Some(ghost) = ghost_text {
        if textarea.lines().len() == 1 {
            // Calculate cursor position in the rendered area
            let (_, cursor_col) = textarea.cursor();
            let ghost_x = chunks[1].x + cursor_col as u16;
            let ghost_y = chunks[1].y;

            // Render ghost text in gray after cursor
            let ghost_widget = Paragraph::new(Span::styled(
                ghost,
                Style::default().fg(Color::DarkGray),
            ));

            // Calculate available space for ghost text
            let available_width = chunks[1].width.saturating_sub(cursor_col as u16);
            if available_width > 0 {
                let ghost_area = Rect {
                    x: ghost_x,
                    y: ghost_y,
                    width: available_width.min(ghost.len() as u16),
                    height: 1,
                };
                frame.render_widget(ghost_widget, ghost_area);
            }
        }
    }
}
