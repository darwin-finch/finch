// TUI Renderer - Ratatui-based terminal user interface
//
// This module provides a Claude Code-like TUI with:
// - Scrollable output area (top)
// - Fixed input line (middle)
// - Multi-line status area (bottom)

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    text::{Line, Text},
    widgets::{Paragraph, Widget},
    Terminal, TerminalOptions, Viewport,
};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar};

mod async_input;
mod dialog;
mod dialog_widget;
mod input_widget;
mod output_widget;
mod status_widget;

pub use async_input::spawn_input_task;
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use input_widget::render_input_widget;
pub use output_widget::OutputWidget;
pub use status_widget::StatusWidget;

// Note: input_handler (TuiInputHandler) removed - we now use integrated tui-textarea

/// TUI renderer for Ratatui-based interface
pub struct TuiRenderer {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    output_manager: Arc<OutputManager>,
    status_bar: Arc<StatusBar>,
    /// Whether TUI is currently active (for suspend/resume)
    is_active: bool,
    /// Active dialog being displayed (if any)
    active_dialog: Option<Dialog>,
    /// Text input area for integrated TUI input
    input_textarea: TextArea<'static>,
    /// Command history for up/down arrow navigation
    command_history: Vec<String>,
    /// Current position in history (None = not navigating)
    history_index: Option<usize>,
}

impl TuiRenderer {
    /// Helper method to create a clean text area with no default styling
    fn create_clean_textarea() -> TextArea<'static> {
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Type your message...");

        use ratatui::style::{Modifier, Style};

        // Set style properties to remove unwanted defaults
        // TextArea defaults to underlined cursor line and blue selection
        let clean_style = Style::default();

        textarea.set_style(clean_style);                    // General text style
        textarea.set_cursor_line_style(clean_style);        // Remove underline on cursor line
        textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED)); // Block cursor
        textarea.set_selection_style(clean_style);          // Remove blue selection background
        textarea.set_placeholder_style(clean_style);        // Clean placeholder style

        textarea
    }

    /// Helper to create a clean text area with initial text
    fn create_clean_textarea_with_text(text: &str) -> TextArea<'static> {
        let mut textarea = TextArea::from([text]);

        use ratatui::style::{Modifier, Style};
        let clean_style = Style::default();

        textarea.set_style(clean_style);
        textarea.set_cursor_line_style(clean_style);
        textarea.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED)); // Block cursor
        textarea.set_selection_style(clean_style);
        textarea.set_placeholder_style(clean_style);

        textarea
    }

    /// Calculate visible length of string (excluding ANSI escape codes)
    fn visible_length(s: &str) -> usize {
        let mut len = 0;
        let mut chars = s.chars().peekable();

        while let Some(c) = chars.next() {
            match c {
                '\x1b' => {
                    // Handle escape sequences
                    if chars.peek() == Some(&'[') {
                        // CSI sequence: \x1b[...m (color codes, cursor movement)
                        chars.next(); // consume '['
                        while let Some(ch) = chars.next() {
                            if ch.is_ascii_alphabetic() {
                                break;  // Sequence terminator
                            }
                        }
                    } else if chars.peek() == Some(&']') {
                        // OSC sequence: \x1b]...\x07 or \x1b]...\x1b\\
                        chars.next(); // consume ']'
                        while let Some(ch) = chars.next() {
                            if ch == '\x07' || (ch == '\x1b' && chars.peek() == Some(&'\\')) {
                                if ch == '\x1b' {
                                    chars.next(); // consume '\\'
                                }
                                break;
                            }
                        }
                    } else {
                        // Other escape sequences, skip 1 char
                        chars.next();
                    }
                }
                '\r' | '\x08' | '\x7f' => {
                    // Control characters that don't add visible length
                    // \r = carriage return, \x08 = backspace, \x7f = delete
                }
                _ => {
                    len += 1;  // Regular visible character
                }
            }
        }

        len
    }

    /// Create a new TUI renderer
    pub fn new(output_manager: Arc<OutputManager>, status_bar: Arc<StatusBar>) -> Result<Self> {
        // Setup terminal on main screen (no alternate screen = scrollback enabled)
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut stdout = io::stdout();

        // Ensure cursor is visible
        execute!(stdout, cursor::Show).context("Failed to show cursor")?;

        let backend = CrosstermBackend::new(stdout);

        // Use Inline viewport - creates a 6-line window at the bottom
        // This matches the layout: 1 (input) + 1 (separator) + 4 (status with border) = 6
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(6),
            },
        ).context("Failed to create terminal with inline viewport")?;

        Ok(Self {
            terminal,
            output_manager,
            status_bar,
            is_active: true,
            active_dialog: None,
            input_textarea: Self::create_clean_textarea(),
            command_history: Vec::new(),
            history_index: None,
        })
    }

    /// Render the TUI with fixed layout
    pub fn render(&mut self) -> Result<()> {
        if !self.is_active {
            return Ok(());
        }

        let has_dialog = self.active_dialog.is_some();
        let active_dialog = self.active_dialog.clone();
        let status_bar = Arc::clone(&self.status_bar);
        let input_textarea = self.input_textarea.clone();

        self.terminal
            .draw(|frame| {
                if has_dialog {
                    // Dialog mode: Use full viewport for dialog - hide status temporarily
                    // Status isn't critical during brief dialog interactions
                    if let Some(dialog) = &active_dialog {
                        let dialog_widget = DialogWidget::new(dialog);
                        frame.render_widget(dialog_widget, frame.area());
                    }
                } else {
                    // Normal mode: Fixed 1+1+4 layout (1 for separator, 1 for input, 4 for status with border+title)
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(1), // Separator line (top of viewport)
                            Constraint::Length(1), // Input area (text only, no border)
                            Constraint::Length(4), // Status area (1 border+title + 3 content)
                        ])
                        .split(frame.area());

                    // Render separator line at top of viewport (above input)
                    use ratatui::widgets::{Block, Borders};
                    use ratatui::style::{Color, Style};
                    let separator = Block::default()
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(Color::DarkGray));
                    frame.render_widget(separator, chunks[0]);

                    render_input_widget(frame, &input_textarea, chunks[1], ">");

                    let status_widget = StatusWidget::new(&status_bar);
                    frame.render_widget(status_widget, chunks[2]);
                }
            })
            .context("Failed to draw frame")?;

        Ok(())
    }

    // Note: suspend() and resume() removed - no longer needed without alternate screen
    // TUI now stays on main screen continuously, enabling terminal scrollback

    /// Check if the TUI is currently active
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Flush pending output using Ratatui's official insert_before() API
    /// This properly inserts content above the inline viewport without manual cursor positioning
    pub fn flush_output_safe(&mut self, output_manager: &OutputManager) -> Result<()> {
        let pending_lines = output_manager.drain_pending();
        if pending_lines.is_empty() {
            return Ok(());
        }

        // Get terminal width for wrapping calculations
        let (term_width, _) = crossterm::terminal::size()?;
        let width = term_width.saturating_sub(2) as usize; // Account for borders/margins

        // Count total output lines INCLUDING terminal wrapping
        let num_lines: usize = pending_lines.iter()
            .map(|line| {
                // Split by explicit newlines first
                line.split('\n')
                    .filter(|segment| !segment.trim().is_empty())  // Skip empty lines (match rendering)
                    .map(|segment| {
                        // For each segment, calculate how many lines it wraps to
                        let visible_len = Self::visible_length(segment);
                        // Ceiling division for terminal wrapping
                        ((visible_len + width - 1) / width).max(1)
                    })
                    .sum::<usize>()
            })
            .sum();

        // No padding needed after counting fix
        let num_lines_with_padding = num_lines;

        // Convert to u16 (insert_before expects u16)
        let num_lines_u16 = num_lines_with_padding.min(u16::MAX as usize) as u16;

        // Use Ratatui's official API to insert content above the inline viewport
        // This handles all the positioning and scrolling automatically
        self.terminal.insert_before(num_lines_u16, |buf| {
            // Convert pending lines to Ratatui Text
            let lines: Vec<Line> = pending_lines
                .iter()
                .flat_map(|line| {
                    line.split('\n')
                        .filter(|part| !part.trim().is_empty())
                        .map(|part| {
                            // More aggressive cleaning to prevent cursor positioning issues
                            let clean = part
                                .trim_end_matches('\r')
                                .trim_end_matches('\n')
                                .trim_end_matches('\x00')  // Null bytes
                                .replace('\r', "");         // Remove all \r
                            Line::from(clean)
                        })
                })
                .collect();

            let text = Text::from(lines);
            let paragraph = Paragraph::new(text);

            // Render the output into the buffer using Widget trait
            // buf.area is automatically sized for num_lines
            Widget::render(paragraph, buf.area, buf);
        })?;

        // Now render the TUI normally
        // The inline viewport automatically repositions after insert_before()
        self.render()?;

        Ok(())
    }

    /// Check if flush is needed
    pub fn should_flush(&self, output_manager: &OutputManager) -> bool {
        output_manager.has_pending()
    }

    /// Show a dialog and block until the user responds
    pub fn show_dialog(&mut self, dialog: Dialog) -> Result<DialogResult> {
        if !self.is_active {
            anyhow::bail!("Cannot show dialog when TUI is not active");
        }

        self.active_dialog = Some(dialog);

        loop {
            // Render with dialog overlay
            self.render()?;

            // Poll for key events (100ms timeout)
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Handle the key event
                    let dialog = self
                        .active_dialog
                        .as_mut()
                        .expect("dialog should exist in show_dialog loop");

                    if let Some(result) = dialog.handle_key_event(key) {
                        // Dialog returned a result, close it
                        self.active_dialog = None;

                        // Redraw to clear dialog
                        self.render()?;

                        return Ok(result);
                    }
                }
            }
        }
    }

    /// Read a line of input from the integrated text area
    pub fn read_line(&mut self) -> Result<Option<String>> {
        use crossterm::event::{KeyCode, KeyModifiers};

        loop {
            // Check for pending output BEFORE rendering
            let output_mgr = self.output_manager.clone();
            if output_mgr.has_pending() {
                self.flush_output_safe(&output_mgr)?;
            }

            // Render current state
            self.render()?;

            // Poll for events (100ms timeout)
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) => {
                        match (key.code, key.modifiers) {
                            (KeyCode::Enter, KeyModifiers::SHIFT) => {
                                // Shift+Enter: Insert newline (multi-line input)
                                self.input_textarea.input(Event::Key(key));
                            }
                            (KeyCode::Enter, KeyModifiers::NONE) => {
                                // Enter: Submit input
                                let lines = self.input_textarea.lines();
                                let input = lines.join("\n");

                                if input.trim().is_empty() {
                                    continue; // Don't submit empty input
                                }

                                // Add to history
                                self.command_history.push(input.clone());
                                self.history_index = None;

                                // Clear input immediately before returning
                                self.input_textarea = Self::create_clean_textarea();

                                // Render once to show cleared input
                                self.render()?;

                                return Ok(Some(input));
                            }
                            (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                // Cancel (Ctrl+C or Esc)
                                return Ok(None);
                            }
                            (KeyCode::Up, KeyModifiers::NONE) => {
                                // Navigate history backwards
                                if let Some(idx) = self.history_index {
                                    if idx > 0 {
                                        self.history_index = Some(idx - 1);
                                        let cmd = &self.command_history[idx - 1];
                                        self.input_textarea = Self::create_clean_textarea_with_text(cmd);
                                    }
                                } else if !self.command_history.is_empty() {
                                    self.history_index = Some(self.command_history.len() - 1);
                                    let cmd = &self.command_history[self.command_history.len() - 1];
                                    self.input_textarea = Self::create_clean_textarea_with_text(cmd);
                                }
                            }
                            (KeyCode::Down, KeyModifiers::NONE) => {
                                // Navigate history forwards
                                if let Some(idx) = self.history_index {
                                    if idx < self.command_history.len() - 1 {
                                        self.history_index = Some(idx + 1);
                                        let cmd = &self.command_history[idx + 1];
                                        self.input_textarea = Self::create_clean_textarea_with_text(cmd);
                                    } else {
                                        self.history_index = None;
                                        self.input_textarea = Self::create_clean_textarea();
                                    }
                                }
                            }
                            _ => {
                                // Let tui-textarea handle the input
                                self.input_textarea.input(Event::Key(key));
                            }
                        }
                    }
                    _ => {}
                }
            }

            // Check again after polling
            let output_mgr = self.output_manager.clone();
            if output_mgr.has_pending() {
                self.flush_output_safe(&output_mgr)?;
            }
        }
    }

    /// Non-blocking input poll - shows typing to user without blocking
    /// Returns true if events were processed
    pub fn poll_input(&mut self) -> Result<bool> {
        use crossterm::event;

        let mut had_events = false;

        // Poll with very short timeout (10ms)
        while event::poll(std::time::Duration::from_millis(10))? {
            if let Ok(event_data) = event::read() {
                // Update textarea with keystrokes
                self.input_textarea.input(event_data);
                had_events = true;

                // Render to show typing immediately
                self.render()?;
            }
        }

        Ok(had_events)
    }

    /// Shutdown the TUI and restore terminal state
    pub fn shutdown(mut self) -> Result<()> {
        if self.is_active {
            // Re-enable direct stdout writes for non-TUI mode
            self.output_manager.disable_buffering();
            self.output_manager.enable_stdout();

            let mut stdout = io::stdout();

            // Clear the inline viewport area and restore cursor
            // Move to column 0, clear from cursor down, show cursor, add newline
            execute!(
                stdout,
                cursor::MoveToColumn(0),
                Clear(ClearType::FromCursorDown),
                cursor::Show
            ).context("Failed to clear terminal")?;

            // Add newline to move past cleared area
            writeln!(stdout).context("Failed to write newline")?;

            stdout.flush().context("Failed to flush stdout")?;

            // Disable raw mode
            disable_raw_mode().context("Failed to disable raw mode")?;

            self.is_active = false;
        }
        Ok(())
    }
}

impl Drop for TuiRenderer {
    fn drop(&mut self) {
        // Ensure terminal is restored on drop
        if self.is_active {
            let mut stdout = io::stdout();

            // Clear viewport and show cursor
            let _ = execute!(
                stdout,
                cursor::MoveToColumn(0),
                Clear(ClearType::FromCursorDown),
                cursor::Show
            );
            let _ = stdout.flush();

            // Disable raw mode
            let _ = disable_raw_mode();

            self.is_active = false;
        }
    }
}
