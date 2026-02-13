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

/// Render a TextArea with a colored prompt prefix
pub fn render_input_widget<'a>(
    frame: &mut Frame,
    textarea: &'a TextArea<'a>,
    area: Rect,
    prompt: &str,
    colors: &ColorScheme,
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
}
