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
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Text},
    widgets::{Clear as ClearWidget, Paragraph, Widget},
    Terminal, TerminalOptions, Viewport,
};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar};
use crate::cli::messages::{MessageId, MessageRef, MessageStatus, UserQueryMessage, StreamingResponseMessage, StaticMessage};

mod async_input;
mod dialog;
mod dialog_widget;
mod input_widget;
mod output_widget;
mod scrollback;
mod status_widget;

pub use async_input::spawn_input_task;
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use input_widget::render_input_widget;
pub use output_widget::OutputWidget;
pub use scrollback::ScrollbackBuffer;
pub use status_widget::StatusWidget;

// Import DialogType for internal use
use dialog::DialogType as DType;

// Note: input_handler (TuiInputHandler) removed - we now use integrated tui-textarea

/// Calculate viewport height dynamically based on terminal size
fn calculate_viewport_height(terminal_size: (u16, u16)) -> usize {
    let (_, term_height) = terminal_size;

    // Reserve space for TUI components:
    // - Separator: 1 line
    // - Input area: 1-3 lines (depends on content)
    // - Status bar: 1 line
    let tui_reserved = 3; // Minimum

    let viewport_height = term_height.saturating_sub(tui_reserved) as usize;
    viewport_height.max(5) // Minimum 5 lines
}

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
    /// Internal scrollback buffer with structured messages
    scrollback: ScrollbackBuffer,
    /// Dynamic viewport height (updated on resize)
    viewport_height: usize,
    /// Whether full refresh is needed
    needs_full_refresh: bool,
    /// Last refresh timestamp
    last_refresh: std::time::Instant,
    /// Refresh interval during streaming
    refresh_interval: Duration,
    /// Whether TUI needs to be redrawn (double buffering)
    needs_tui_render: bool,
    /// Previous input text (for change detection)
    prev_input_text: String,
    /// Previous status bar content (for change detection)
    prev_status_content: String,
    /// Track which messages have been permanently written to terminal scrollback
    written_message_ids: std::collections::HashSet<MessageId>,
    /// Track how many enum messages from output_manager we've processed
    last_enum_message_count: usize,
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

    /// Helper function to create a centered rect for dialog overlay
    ///
    /// # Arguments
    /// * `percent_width` - Width as percentage of parent area (e.g., 60 = 60%)
    /// * `percent_height` - Height as percentage of parent area (e.g., 80 = 80%)
    /// * `area` - The parent area to center within
    fn centered_rect(percent_width: u16, percent_height: u16, area: Rect) -> Rect {
        let popup_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage((100 - percent_height) / 2),
                Constraint::Percentage(percent_height),
                Constraint::Percentage((100 - percent_height) / 2),
            ])
            .split(area);

        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage((100 - percent_width) / 2),
                Constraint::Percentage(percent_width),
                Constraint::Percentage((100 - percent_width) / 2),
            ])
            .split(popup_layout[1])[1]
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

    /// Create a new TUI renderer with inline viewport
    pub fn new(output_manager: Arc<OutputManager>, status_bar: Arc<StatusBar>) -> Result<Self> {
        // Setup terminal with inline viewport - preserves terminal scrollback
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut stdout = io::stdout();

        // Ensure cursor is visible
        execute!(stdout, cursor::Show).context("Failed to show cursor")?;

        let backend = CrosstermBackend::new(stdout);

        // Use Inline viewport - 6 lines at bottom (separator + input + status)
        // Messages will be written above this using insert_before()
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(6),
            },
        ).context("Failed to create terminal with inline viewport")?;

        // Get terminal size for scrollback buffer
        let term_size = crossterm::terminal::size()
            .context("Failed to get terminal size")?;
        let (term_width, _term_height) = term_size;

        // Calculate dynamic viewport height
        let viewport_height = calculate_viewport_height(term_size);

        // ScrollbackBuffer tracks all messages (not for rendering, for structure)
        // We'll use insert_before() to write to terminal scrollback
        let scrollback = ScrollbackBuffer::new(viewport_height, term_width as usize);

        // Keep stdout disabled - we'll write via insert_before() instead
        output_manager.disable_stdout();

        Ok(Self {
            terminal,
            output_manager,
            status_bar,
            is_active: true,
            active_dialog: None,
            input_textarea: Self::create_clean_textarea(),
            command_history: Vec::new(),
            history_index: None,
            scrollback,
            viewport_height,
            needs_full_refresh: false,
            last_refresh: std::time::Instant::now(),
            refresh_interval: Duration::from_millis(100), // 10 FPS - blit to overwrite visible area
            needs_tui_render: true, // Initial render needed
            prev_input_text: String::new(),
            prev_status_content: String::new(),
            written_message_ids: std::collections::HashSet::new(),
            last_enum_message_count: 0,
        })
    }

    /// Render the TUI inline viewport (6 lines: separator + input + status)
    /// Messages are written to terminal scrollback via insert_before()
    pub fn render(&mut self) -> Result<()> {
        if !self.is_active {
            return Ok(());
        }

        // Double buffering: Check if anything changed
        let current_input_text = self.input_textarea.lines().join("\n");
        let current_status_content = self.status_bar.get_status();
        let has_dialog = self.active_dialog.is_some();

        let input_changed = current_input_text != self.prev_input_text;
        let status_changed = current_status_content != self.prev_status_content;
        let force_render = self.needs_tui_render;

        // Skip render if nothing changed
        if !input_changed && !status_changed && !force_render {
            return Ok(());
        }

        // Update previous state for next comparison
        self.prev_input_text = current_input_text;
        self.prev_status_content = current_status_content.clone();
        self.needs_tui_render = false;

        let active_dialog = self.active_dialog.clone();
        let status_bar = Arc::clone(&self.status_bar);
        let input_textarea = self.input_textarea.clone();

        // Wrap in synchronized update to prevent tearing
        use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
        execute!(io::stdout(), BeginSynchronizedUpdate)?;

        self.terminal
            .draw(|frame| {
                if has_dialog {
                    // Dialog mode: Show scrollback context + dialog at bottom
                    if let Some(dialog) = &active_dialog {
                        use ratatui::text::{Line, Span};
                        use ratatui::style::{Color, Style};
                        use ratatui::widgets::Paragraph;

                        let total_area = frame.area();

                        // Calculate dialog height (title + options + help + borders)
                        let num_options = match &dialog.dialog_type {
                            DType::Select { options, .. } => options.len(),
                            DType::MultiSelect { options, .. } => options.len(),
                            DType::Confirm { .. } => 2, // Yes/No
                            DType::TextInput { .. } => 1, // Single input line
                        };
                        let dialog_height = num_options as u16 + 4; // +4 for title, help, borders
                        let status_height = 4u16;
                        let separator_height = 1u16;

                        // Remaining space for scrollback
                        let scrollback_height = total_area.height
                            .saturating_sub(dialog_height)
                            .saturating_sub(status_height)
                            .saturating_sub(separator_height);

                        // Layout: scrollback (top) + separator + dialog (bottom) + status
                        let chunks = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([
                                Constraint::Length(scrollback_height), // Scrollback context
                                Constraint::Length(separator_height),   // Separator
                                Constraint::Length(dialog_height),      // Dialog
                                Constraint::Length(status_height),      // Status
                            ])
                            .split(total_area);

                        // Render recent scrollback messages for context
                        let scrollback_messages = self.scrollback.get_visible_messages();
                        let context_lines: Vec<Line> = scrollback_messages
                            .iter()
                            .rev()
                            .take(scrollback_height as usize)
                            .rev()
                            .flat_map(|msg| {
                                let formatted = msg.format();
                                formatted.lines().map(|line| Line::raw(line.to_string())).collect::<Vec<_>>()
                            })
                            .collect();

                        let scrollback_widget = Paragraph::new(context_lines);
                        frame.render_widget(scrollback_widget, chunks[0]);

                        // Render separator
                        let separator_char = '─';
                        let separator_line = separator_char.to_string().repeat(chunks[1].width as usize);
                        let separator_widget = Paragraph::new(Line::from(Span::styled(
                            separator_line,
                            Style::default().fg(Color::DarkGray),
                        )));
                        frame.render_widget(separator_widget, chunks[1]);

                        // Render dialog
                        let dialog_widget = DialogWidget::new(dialog);
                        frame.render_widget(dialog_widget, chunks[2]);

                        // Render status
                        let status_widget = StatusWidget::new(&status_bar);
                        frame.render_widget(status_widget, chunks[3]);
                    }
                } else {
                    // Normal mode: Render inline viewport (separator + input + status)
                    // Layout: 1 separator + 1 input + 4 status = 6 lines
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(1),   // Separator line
                            Constraint::Length(1),   // Input area
                            Constraint::Length(4),   // Status area
                        ])
                        .split(frame.area());

                    // Render separator line
                    use ratatui::text::{Line, Span};
                    use ratatui::widgets::Paragraph;
                    use ratatui::style::{Color, Style};

                    let separator_char = '─'; // Unicode box-drawing (U+2500)
                    let separator_line = separator_char.to_string().repeat(chunks[0].width as usize);
                    let separator_widget = Paragraph::new(Line::from(Span::styled(
                        separator_line,
                        Style::default().fg(Color::DarkGray),
                    )));
                    frame.render_widget(separator_widget, chunks[0]);

                    // Render input
                    render_input_widget(frame, &input_textarea, chunks[1], "❯");

                    // Render status
                    let status_widget = StatusWidget::new(&status_bar);
                    frame.render_widget(status_widget, chunks[2]);
                }
            })
            .context("Failed to draw frame")?;

        execute!(io::stdout(), EndSynchronizedUpdate)?;

        Ok(())
    }

    // Messages are rendered to terminal scrollback via insert_before() in flush_output_safe()

    /// Check if the TUI is currently active
    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Flush pending output to terminal scrollback via insert_before()
    /// Syncs OutputManager messages to ScrollbackBuffer, then renders new ones
    pub fn flush_output_safe(&mut self, output_manager: &OutputManager) -> Result<()> {
        use crate::cli::output_manager::OutputMessage as OldMessage;
        use std::sync::Arc;

        // Track new messages to render
        let mut new_messages_to_render: Vec<MessageRef> = Vec::new();

        // Handle old enum-based messages FIRST (so user input appears before streaming response)
        let messages = output_manager.get_messages();

        // Add only new messages (messages added since last flush)
        for msg in messages.iter().skip(self.last_enum_message_count) {
            // Skip status messages - they're shown in status bar
            if matches!(msg, OldMessage::StatusInfo { .. }) {
                continue;
            }

            // Create trait object from OutputMessage
            let trait_msg: MessageRef = match msg {
                OldMessage::UserMessage { content } => {
                    Arc::new(UserQueryMessage::new(content.clone()))
                }
                OldMessage::ClaudeResponse { content } => {
                    let msg = StreamingResponseMessage::new();
                    msg.append_chunk(content);
                    msg.set_complete();
                    Arc::new(msg)
                }
                OldMessage::ToolOutput { tool_name, content } => {
                    // Use StaticMessage for tool output (formatted)
                    let formatted = format!("[{}] {}", tool_name, content);
                    Arc::new(StaticMessage::info(formatted))
                }
                OldMessage::Error { content } => {
                    Arc::new(StaticMessage::error(content.clone()))
                }
                OldMessage::Progress { content } => {
                    // Use plain type - messages already have their own formatting/emojis
                    Arc::new(StaticMessage::plain(content.clone()))
                }
                OldMessage::SystemInfo { content } => {
                    // Use plain type - messages already have their own formatting/emojis
                    Arc::new(StaticMessage::plain(content.clone()))
                }
                OldMessage::StatusInfo { .. } => unreachable!(), // Already handled above
            };

            // Skip if this exact content was just added (prevent duplicates)
            if let Some(last_msg) = self.scrollback.get_last_message() {
                if last_msg.content() == trait_msg.content() {
                    // Duplicate detected, skip
                    continue;
                }
            }

            new_messages_to_render.push(trait_msg.clone());
            self.scrollback.add_message(trait_msg);
        }

        // Update count of processed enum messages
        self.last_enum_message_count = messages.len();

        // Handle trait-based messages AFTER enum messages (so they appear after user input)
        // Pure buffer comparison - add all messages to shadow buffer
        let trait_messages = output_manager.get_trait_messages();

        for msg in &trait_messages {
            let msg_id = msg.id();

            // Add to shadow buffer if not already there
            if self.scrollback.get_message(msg_id).is_none() {
                self.scrollback.add_message(msg.clone());
                self.needs_full_refresh = true; // New message = needs refresh
            }

            // Only write Complete messages to terminal scrollback permanently (once)
            // Status checking ONLY for permanent scrollback writes, not for rendering decisions
            if matches!(msg.status(), crate::cli::messages::MessageStatus::Complete) {
                if !self.written_message_ids.contains(&msg_id) {
                    new_messages_to_render.push(msg.clone());
                    self.written_message_ids.insert(msg_id);
                }
            }
        }

        // If there are any messages, trigger refresh to keep visible area updated
        // (Messages update in place via Arc<RwLock<>>, so periodic blit keeps display current)
        if !trait_messages.is_empty() {
            self.needs_full_refresh = true;
        }

        // Render new messages to terminal scrollback using insert_before()
        if !new_messages_to_render.is_empty() {
            // Get terminal width for wrapping
            let (term_width, _) = crossterm::terminal::size()?;
            let width = term_width.saturating_sub(2) as usize;

            // Calculate total lines needed
            let num_lines: usize = new_messages_to_render
                .iter()
                .map(|msg| {
                    // Count lines in formatted message
                    let formatted = msg.format();
                    formatted
                        .lines()
                        .map(|line| {
                            let visible_len = Self::visible_length(line);
                            ((visible_len + width - 1) / width.max(1)).max(1)
                        })
                        .sum::<usize>()
                        + 1 // Extra line for spacing
                })
                .sum();

            let num_lines_u16 = num_lines.min(u16::MAX as usize) as u16;

            // Use synchronized update to prevent flickering
            use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
            execute!(io::stdout(), BeginSynchronizedUpdate)?;

            // Use insert_before() to write to terminal scrollback
            self.terminal.insert_before(num_lines_u16, |buf| {
                let mut lines: Vec<Line> = Vec::new();

                for msg in &new_messages_to_render {
                    // Add formatted message content
                    let formatted = msg.format();
                    for line in formatted.lines() {
                        lines.push(Line::raw(line.to_string()));
                    }
                    // Add spacing
                    lines.push(Line::raw(""));
                }

                let text = Text::from(lines);
                let paragraph = Paragraph::new(text);
                paragraph.render(buf.area, buf);
            })?;

            execute!(io::stdout(), EndSynchronizedUpdate)?;

            // Update ring buffer to track rendered lines
            for msg in &new_messages_to_render {
                let formatted = msg.format();
                let num_content_lines = formatted.lines().count();
                for line_offset in 0..num_content_lines {
                    self.scrollback.push_line(msg.id(), line_offset);
                }
                // Add spacing line
                self.scrollback.push_line(msg.id(), num_content_lines);
            }

            // Mark TUI for render (separator might need to move up)
            self.needs_tui_render = true;
        }

        Ok(())
    }

    /// Add a trait-based message directly to scrollback (for live updates)
    pub fn add_trait_message(&mut self, message: MessageRef) -> MessageId {
        self.scrollback.add_message(message)
    }

    /// Get a message by ID from scrollback
    pub fn get_message(&self, id: MessageId) -> Option<MessageRef> {
        self.scrollback.get_message(id)
    }

    // Scrolling is handled by terminal (mouse wheel, shift+pgup/pgdn, etc.)
    // ScrollbackBuffer still tracks messages for structure, search, export

    /// Check if flush is needed
    pub fn should_flush(&self, output_manager: &OutputManager) -> bool {
        // Check if there are new messages to sync
        let messages = output_manager.get_messages();
        let current_count = self.scrollback.message_count();
        messages.len() > current_count
    }

    /// Show a dialog and block until the user responds
    pub fn show_dialog(&mut self, dialog: Dialog) -> Result<DialogResult> {
        if !self.is_active {
            anyhow::bail!("Cannot show dialog when TUI is not active");
        }

        // Validate terminal size
        let (_width, height) = crossterm::terminal::size()
            .context("Failed to get terminal size")?;

        if height < 15 {
            anyhow::bail!(
                "Terminal too small for dialog (need 15+ lines, have {}). Please resize terminal.",
                height
            );
        }

        // Set active dialog (will be rendered as overlay)
        self.active_dialog = Some(dialog);

        let result = loop {
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

                    if let Some(dialog_result) = dialog.handle_key_event(key) {
                        // Dialog returned a result, exit loop
                        break dialog_result;
                    }
                }
            }
        };

        // Clean up: remove dialog
        self.active_dialog = None;

        // Trigger a render to restore normal layout
        self.render()?;

        Ok(result)
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
                    _ => {
                        // Mouse events handled by terminal (scrolls terminal buffer)
                    }
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

    /// Full refresh of viewport from shadow buffer
    /// Uses MoveTo/Print to overwrite visible area with current state
    /// This prevents duplicates - insert_before() writes to scrollback,
    /// but blit overwrites visible area with latest content
    fn full_refresh_viewport(&mut self) -> Result<()> {
        use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
        use crossterm::style::Print;

        // Get terminal size
        let (term_width, term_height) = crossterm::terminal::size()?;

        // Calculate visible scrollback area (above inline viewport)
        // Inline viewport is 6 lines at bottom, so visible area is rows 0 to (height - 7)
        let visible_rows = term_height.saturating_sub(7); // -6 for inline, -1 for 0-indexing

        if visible_rows == 0 {
            return Ok(()); // Terminal too small
        }

        // Get all messages from shadow buffer (no status filtering!)
        let messages = self.scrollback.get_visible_messages();

        if messages.is_empty() {
            return Ok(());
        }

        // Format ALL messages from shadow buffer
        let mut all_lines: Vec<String> = Vec::new();
        for msg in &messages {
            let formatted = msg.format();
            for line in formatted.lines() {
                all_lines.push(line.to_string());
            }
            all_lines.push(String::new()); // Blank line between messages
        }

        if all_lines.is_empty() {
            return Ok(()); // Nothing to blit
        }

        // Calculate how many lines we can fit
        let num_lines = all_lines.len().min(visible_rows as usize);
        let lines_to_render = &all_lines[all_lines.len().saturating_sub(num_lines)..];

        // Render bottom-aligned (just above inline viewport)
        // Start at row (visible_rows - num_lines) so content appears at bottom
        let start_row = visible_rows.saturating_sub(num_lines as u16);

        let mut stdout = io::stdout();
        execute!(stdout, BeginSynchronizedUpdate)?;

        // Clear the entire visible area first
        for row in 0..visible_rows {
            execute!(
                stdout,
                cursor::MoveTo(0, row),
                Clear(ClearType::UntilNewLine)
            )?;
        }

        // Render lines bottom-aligned
        for (idx, line) in lines_to_render.iter().enumerate() {
            let row = start_row + idx as u16;
            execute!(
                stdout,
                cursor::MoveTo(0, row),
                Print(line)
            )?;
        }

        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        Ok(())
    }

    // Old approach (kept for reference, commented out):
    // fn full_refresh_viewport_old(&mut self) -> Result<()> {
    //     use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
    //     use crossterm::style::Print;
    //
    //     let mut stdout = io::stdout();
    //
    //     // Get lines to render from ring buffer
    //     let viewport_lines = self.scrollback.get_viewport_lines();
    //
    //     if viewport_lines.is_empty() {
    //         return Ok(()); // Nothing to render
    //     }
    //
    //     // Synchronized update for tear-free rendering
    //     execute!(stdout, BeginSynchronizedUpdate)?;
    //
    //     // Render each line in viewport
    //     for (line_idx, (message_id, line_offset)) in viewport_lines.iter().enumerate() {
    //         let row = line_idx as u16;
    //
    //         // Get message from scrollback
    //         if let Some(message) = self.scrollback.get_message(*message_id) {
    //             // Extract specific line from formatted message
    //             let formatted = message.format();
    //             let line_content = formatted
    //                 .lines()
    //                 .nth(*line_offset)
    //                 .unwrap_or("");
    //
    //             // Move cursor, clear line, write content
    //             execute!(
    //                 stdout,
    //                 cursor::MoveTo(0, row),
    //                 Clear(ClearType::UntilNewLine),
    //                 Print(line_content)
    //             )?;
    //         } else {
    //             // Message not found, clear line
    //             execute!(
    //                 stdout,
    //                 cursor::MoveTo(0, row),
    //                 Clear(ClearType::UntilNewLine)
    //             )?;
    //         }
    //     }
    //
    //     execute!(stdout, EndSynchronizedUpdate)?;
    //     stdout.flush()?;
    //
    //     Ok(())
    // }

    /// Update a streaming message (mark for refresh)
    // NOTE: With trait-based messages, updates happen directly through Arc<RwLock<>>
    // Example:
    //   let msg = Arc::new(StreamingResponseMessage::new());
    //   self.add_trait_message(msg.clone());
    //   msg.append_chunk("more text");  // Updates automatically
    //   msg.set_complete();
    //
    // The TUI will see the changes on next render cycle.

    /// Trigger a full refresh of the viewport (for reactive message updates)
    pub fn trigger_refresh(&mut self) {
        self.needs_full_refresh = true;
    }

    /// Check if full refresh is needed and perform it
    /// Blit overwrites visible area with current shadow buffer state
    pub fn check_and_refresh(&mut self) -> Result<()> {
        if self.needs_full_refresh {
            // Render current shadow buffer to visible area (overwrites existing content)
            self.full_refresh_viewport()?;
            self.needs_full_refresh = false;
        }

        Ok(())
    }

    /// Handle terminal resize event
    pub fn handle_resize(&mut self, width: u16, height: u16) -> Result<()> {
        // Update viewport dimensions
        let new_viewport_height = calculate_viewport_height((width, height));
        self.viewport_height = new_viewport_height;
        self.scrollback.update_viewport(new_viewport_height, width as usize);

        // Rebuild ring buffer with new line counts
        self.scrollback.rebuild_ring_buffer();

        // NOTE: No full refresh needed with inline viewport
        // Terminal scrollback content is already there
        // Just need to re-render TUI components (which happens on next render())
        self.needs_tui_render = true;

        Ok(())
    }

    /// Append a complete message to terminal scrollback
    // NOTE: The following methods are commented out during trait migration.
    // With the trait-based system, messages are added via add_trait_message()
    // and updated directly through Arc<RwLock<>>.
    //
    // pub fn append_message_to_scrollback(&mut self, message: &MessageRef) -> Result<()> { ... }
    // pub fn add_user_query(&mut self, query: String) -> Result<MessageId> { ... }
    // pub fn add_claude_response(&mut self, initial_content: String) -> MessageId { ... }
    // pub fn complete_claude_response(&mut self, message_id: MessageId) -> Result<()> { ... }

    /// Show dialog at bottom with scrollback context above
    pub fn show_centered_dialog(&mut self, dialog: Dialog) -> Result<()> {
        // Calculate how many extra lines we need for scrollback context
        let num_options = match &dialog.dialog_type {
            DType::Select { options, .. } => options.len(),
            DType::MultiSelect { options, .. } => options.len(),
            DType::Confirm { .. } => 2, // Yes/No
            DType::TextInput { .. } => 1, // Single input line
        };
        let dialog_height = num_options + 4; // title + options + help + borders
        let status_height = 4;
        let separator_height = 1;
        let scrollback_context_lines = 10; // Show last 10 lines of scrollback

        let total_needed = scrollback_context_lines + separator_height + dialog_height + status_height;
        let current_viewport = 6; // Our inline viewport size

        // If we need more space, insert blank lines to push viewport down
        if total_needed > current_viewport {
            let extra_lines = (total_needed - current_viewport) as u16;

            // Insert blank lines using insert_before to expand visible area
            self.terminal.insert_before(extra_lines, |buf| {
                // Render recent scrollback in these blank lines
                let scrollback_messages = self.scrollback.get_visible_messages();
                let context_lines: Vec<Line> = scrollback_messages
                    .iter()
                    .rev()
                    .take(extra_lines as usize)
                    .rev()
                    .flat_map(|msg| {
                        let formatted = msg.format();
                        formatted.lines().map(|line| Line::raw(line.to_string())).collect::<Vec<_>>()
                    })
                    .collect();

                let scrollback_paragraph = Paragraph::new(context_lines);
                scrollback_paragraph.render(buf.area, buf);
            })?;
        }

        // Store dialog
        self.active_dialog = Some(dialog);
        self.needs_tui_render = true; // Force render for dialog

        // Render dialog
        self.render()?;

        Ok(())
    }

    /// Hide dialog and return to normal mode
    pub fn hide_dialog(&mut self) -> Result<()> {
        // Clear dialog
        self.active_dialog = None;
        self.needs_tui_render = true; // Force render after dialog closes

        // Re-render TUI (will show normal mode now)
        self.render()?;

        Ok(())
    }
}

impl Drop for TuiRenderer {
    fn drop(&mut self) {
        // Ensure terminal is restored on drop
        if self.is_active {
            let mut stdout = io::stdout();

            // Show cursor
            let _ = execute!(stdout, cursor::Show);
            let _ = stdout.flush();

            // Disable raw mode
            let _ = disable_raw_mode();

            self.is_active = false;
        }
    }
}
