// TUI Renderer - Ratatui-based terminal user interface
//
// This module provides a Claude Code-like TUI with:
// - Scrollable output area (top)
// - Fixed input line (middle)
// - Multi-line status area (bottom)

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyboardEnhancementFlags, PushKeyboardEnhancementFlags, PopKeyboardEnhancementFlags},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::{Paragraph, Widget},
    Terminal, TerminalOptions, Viewport,
};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar};
use crate::cli::messages::{MessageId, MessageRef};

mod async_input;
mod dialog;
mod dialog_widget;
mod input_widget;
mod scrollback;
mod shadow_buffer;
mod status_widget;

pub use async_input::spawn_input_task;
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use input_widget::render_input_widget;
pub use scrollback::ScrollbackBuffer;
pub use shadow_buffer::{ShadowBuffer, diff_buffers, visible_length, extract_visible_chars};
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
    pub active_dialog: Option<Dialog>,
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
    /// Current inline viewport size (1 + input_lines + 4)
    current_inline_viewport_size: u16,
    /// Shadow buffer for rendering (2D character array)
    shadow_buffer: ShadowBuffer,
    /// Previous frame buffer (for diff-based updates)
    prev_frame_buffer: ShadowBuffer,
    /// Whether full refresh is needed
    needs_full_refresh: bool,
    /// Pending feedback rating from user ('g' or 'b' key press)
    pub pending_feedback: Option<crate::feedback::FeedbackRating>,
    /// Pending cancellation request (Ctrl+C pressed)
    pub pending_cancellation: bool,
    /// Pending dialog result (completed but not yet processed)
    pub pending_dialog_result: Option<crate::cli::tui::DialogResult>,
    /// Last query-response pair for feedback
    last_interaction: Option<(String, String)>,
    /// Last refresh timestamp
    last_refresh: std::time::Instant,
    /// Refresh interval during streaming
    refresh_interval: Duration,
    /// Last blit timestamp (for rate limiting)
    last_blit: std::time::Instant,
    /// Blit rate limit (min interval between blits)
    blit_interval: Duration,
    /// Whether TUI needs to be redrawn (double buffering)
    needs_tui_render: bool,
    /// Previous input text (for change detection)
    prev_input_text: String,
    /// Previous cursor position (for change detection)
    prev_cursor_pos: (usize, usize),
    /// Previous status bar content (for change detection)
    prev_status_content: String,
    /// Color scheme for TUI elements
    colors: crate::config::ColorScheme,
    /// Suggestion manager for contextual prompts
    suggestions: crate::cli::SuggestionManager,
}

impl TuiRenderer {
    /// Helper method to create a clean text area with no default styling
    pub(super) fn create_clean_textarea() -> TextArea<'static> {
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
    pub(super) fn create_clean_textarea_with_text(text: &str) -> TextArea<'static> {
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

    /// Create a new TUI renderer with inline viewport
    pub fn new(
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        colors: crate::config::ColorScheme,
    ) -> Result<Self> {
        // Setup terminal with inline viewport - preserves terminal scrollback
        enable_raw_mode().context("Failed to enable raw mode")?;
        let mut stdout = io::stdout();

        // Enable keyboard enhancement flags for better modifier key support (Shift+Enter, etc.)
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        ).context("Failed to enable keyboard enhancements")?;

        // Ensure cursor is visible
        execute!(stdout, cursor::Show).context("Failed to show cursor")?;

        let backend = CrosstermBackend::new(stdout);

        // Use Inline viewport - DYNAMIC size based on input (starts at 6 lines minimum)
        // Messages will be written above this using insert_before()
        // Size: 1 separator + input (1-10 lines) + 4 status = 6-15 lines
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(6), // Start with minimum size
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

        // Calculate visible scrollback area (above inline viewport)
        // Inline viewport is dynamic: 1 (separator) + up to 10 (input) + 4 (status) = 15 lines max
        // Reserve space for maximum possible inline viewport height
        let max_inline_viewport_height = 15; // 1 + 10 + 4
        let visible_scrollback_rows = _term_height.saturating_sub(max_inline_viewport_height) as usize;

        // Initialize shadow buffers for diff-based rendering
        let shadow_buffer = ShadowBuffer::new(term_width as usize, visible_scrollback_rows);
        let prev_frame_buffer = ShadowBuffer::new(term_width as usize, visible_scrollback_rows);

        // Ensure stdout is disabled - we'll write via insert_before() instead
        // (Already disabled in main.rs, but double-check for safety)
        output_manager.disable_stdout();

        // Clear the visible scrollback area to prevent ghosting from previous output
        let mut stdout = io::stdout();
        use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
        execute!(stdout, BeginSynchronizedUpdate)?;
        for row in 0..visible_scrollback_rows {
            execute!(
                stdout,
                cursor::MoveTo(0, row as u16),
                Clear(ClearType::UntilNewLine)
            )?;
        }
        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        let mut renderer = Self {
            terminal,
            output_manager,
            status_bar,
            is_active: true,
            active_dialog: None,
            input_textarea: Self::create_clean_textarea(),
            command_history: Self::load_history(), // Load history from disk
            history_index: None,
            scrollback,
            viewport_height,
            current_inline_viewport_size: 6, // Initial: 1 separator + 1 input + 4 status
            shadow_buffer,
            prev_frame_buffer,
            needs_full_refresh: false,
            last_refresh: std::time::Instant::now(),
            refresh_interval: Duration::from_millis(100), // 10 FPS - blit to overwrite visible area
            last_blit: std::time::Instant::now(),
            blit_interval: Duration::from_millis(50), // 20 FPS max for blitting
            needs_tui_render: true, // Initial render needed
            prev_input_text: String::new(),
            prev_cursor_pos: (0, 0),
            prev_status_content: String::new(),
            pending_feedback: None,
            pending_cancellation: false,
            pending_dialog_result: None,
            last_interaction: None,
            colors,
            suggestions: crate::cli::SuggestionManager::new(),
        };

        // Initialize first-run suggestions
        renderer.update_suggestion_status();

        Ok(renderer)
    }

    /// Record the last query-response pair for feedback
    pub fn record_interaction(&mut self, query: String, response: String) {
        self.last_interaction = Some((query, response));
        // Update suggestion context: query just completed
        self.suggestions.set_context(crate::cli::SuggestionContext::QueryComplete);
        self.suggestions.increment_query_count();
        self.update_suggestion_status();
    }

    /// Update the status bar with current suggestions
    fn update_suggestion_status(&self) {
        if let Some(suggestion_line) = self.suggestions.get_suggestion_line() {
            self.status_bar.update_line(
                crate::cli::StatusLineType::Suggestions,
                suggestion_line,
            );
        } else {
            self.status_bar.remove_line(&crate::cli::StatusLineType::Suggestions);
        }
    }

    /// Update suggestion context based on TUI state
    pub fn update_suggestion_context(&mut self, context: crate::cli::SuggestionContext) {
        self.suggestions.set_context(context);
        self.update_suggestion_status();
    }

    /// Check and process pending feedback
    pub fn process_pending_feedback(&mut self) -> Result<()> {
        if let Some(rating) = self.pending_feedback.take() {
            if let Some((query, response)) = &self.last_interaction {
                // Create feedback logger
                let logger = crate::feedback::FeedbackLogger::new()
                    .context("Failed to create feedback logger")?;

                // Create feedback entry
                let entry = crate::feedback::FeedbackEntry::new(
                    query.clone(),
                    response.clone(),
                    rating,
                );

                // Log feedback
                logger.log(&entry)
                    .context("Failed to log feedback")?;

                // Show confirmation message
                let confirmation = format!(
                    "\n{} Feedback recorded: {} (weight: {:.1}x)\n",
                    match rating {
                        crate::feedback::FeedbackRating::Good => "✓",
                        crate::feedback::FeedbackRating::Bad => "⚠",
                    },
                    rating.display_str(),
                    rating.training_weight()
                );

                self.output_manager.write_info(&confirmation);
            } else {
                self.output_manager.write_info("\n⚠ No recent interaction to provide feedback on\n");
            }
        }

        Ok(())
    }

    /// Load command history from disk
    fn load_history() -> Vec<String> {
        use std::io::BufRead;

        let history_file = match dirs::home_dir() {
            Some(home) => home.join(".shammah").join("history.txt"),
            None => return Vec::new(),
        };

        if !history_file.exists() {
            return Vec::new();
        }

        match std::fs::File::open(&history_file) {
            Ok(file) => {
                let reader = std::io::BufReader::new(file);
                reader
                    .lines()
                    .filter_map(|line| line.ok())
                    .filter(|line| !line.trim().is_empty())
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Save command history to disk
    pub fn save_history(&self) -> Result<()> {
        use std::io::Write;

        let history_file = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".shammah")
            .join("history.txt");

        // Ensure parent directory exists
        if let Some(parent) = history_file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        // Write history (limit to last 1000 commands)
        let history_to_save: Vec<&String> = self
            .command_history
            .iter()
            .rev()
            .take(1000)
            .rev()
            .collect();

        let mut file = std::fs::File::create(&history_file)
            .with_context(|| format!("Failed to create history file: {}", history_file.display()))?;

        for cmd in history_to_save {
            writeln!(file, "{}", cmd)
                .context("Failed to write to history file")?;
        }

        Ok(())
    }

    /// Render the TUI inline viewport (6 lines: separator + input + status)
    /// Messages are written to terminal scrollback via insert_before()
    pub fn render(&mut self) -> Result<()> {
        if !self.is_active {
            return Ok(());
        }

        // Double buffering: Check if anything changed
        let current_input_text = self.input_textarea.lines().join("\n");
        let current_cursor = self.input_textarea.cursor();
        let current_status_content = self.status_bar.get_status();
        let has_dialog = self.active_dialog.is_some();

        let input_changed = current_input_text != self.prev_input_text;
        let cursor_changed = current_cursor != self.prev_cursor_pos;
        let status_changed = current_status_content != self.prev_status_content;
        let force_render = self.needs_tui_render;

        // Skip render if nothing changed (including cursor position)
        if !input_changed && !cursor_changed && !status_changed && !force_render {
            return Ok(());
        }

        // Update previous state for next comparison
        self.prev_input_text = current_input_text;
        self.prev_cursor_pos = current_cursor;
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
                                let formatted = msg.format(&self.colors);
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
                        let dialog_widget = DialogWidget::new(dialog, &self.colors);
                        frame.render_widget(dialog_widget, chunks[2]);

                        // Render status
                        let status_widget = StatusWidget::new(&status_bar, &self.colors);
                        frame.render_widget(status_widget, chunks[3]);
                    }
                } else {
                    // Normal mode: Render inline viewport (separator + input + status)
                    // Calculate dynamic input height based on textarea lines (min 1, max 10)
                    let input_lines = input_textarea.lines().len().max(1).min(10) as u16;

                    // Layout: separator + input + status (NO SPACER)
                    // The viewport size itself should match this dynamically
                    let chunks = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(1),           // Separator line
                            Constraint::Length(input_lines), // Input area (dynamic)
                            Constraint::Length(4),           // Status area
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
                    render_input_widget(frame, &input_textarea, chunks[1], "❯", &self.colors);

                    // Render status
                    let status_widget = StatusWidget::new(&status_bar, &self.colors);
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

    /// Flush pending output to terminal scrollback using insert_before() for complete messages
    /// and shadow buffer for visible area updates
    pub fn flush_output_safe(&mut self, output_manager: &OutputManager) -> Result<()> {
        // Track new messages to write to terminal scrollback
        let mut new_messages: Vec<MessageRef> = Vec::new();

        // Get all trait-based messages from OutputManager
        let messages = output_manager.get_messages();

        for msg in &messages {
            let msg_id = msg.id();

            // If message not in scrollback yet, it's NEW - add and write to terminal
            if self.scrollback.get_message(msg_id).is_none() {
                self.scrollback.add_message(msg.clone());
                new_messages.push(msg.clone());
                self.needs_full_refresh = true;
            }
            // Otherwise it's an UPDATE - message already in scrollback
            // Updates happen via Arc<RwLock<>>, shadow buffer sees them automatically
        }

        // If there are any messages, trigger refresh to keep visible area updated
        if !messages.is_empty() {
            self.needs_full_refresh = true;
        }

        // Write new messages to terminal scrollback using insert_before()
        // This pushes content up above the inline viewport (permanent, scrollable)
        if !new_messages.is_empty() {
            let (term_width, _) = crossterm::terminal::size()?;

            // Format messages with proper wrapping using shadow buffer
            let mut wrapped_lines = Vec::new();
            for msg in &new_messages {
                let formatted = msg.format(&self.colors);
                for line in formatted.lines() {
                    // Use shadow buffer's wrapping logic
                    let visible_len = visible_length(line);
                    if visible_len <= term_width as usize {
                        wrapped_lines.push(line.to_string());
                    } else {
                        // Wrap long lines
                        let chars_per_row = term_width as usize;
                        let (visible_chars, _) = extract_visible_chars(line);
                        let num_rows = (visible_chars.len() + chars_per_row - 1) / chars_per_row.max(1);

                        for row_idx in 0..num_rows {
                            let start = row_idx * chars_per_row;
                            let end = (start + chars_per_row).min(visible_chars.len());
                            let chunk: String = visible_chars[start..end].iter().collect();
                            wrapped_lines.push(chunk);
                        }
                    }
                }
                wrapped_lines.push(String::new()); // Blank line between messages
            }

            let num_lines = wrapped_lines.len().min(u16::MAX as usize) as u16;

            // Use insert_before to write to terminal scrollback (pushes content up)
            use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
            execute!(io::stdout(), BeginSynchronizedUpdate)?;

            self.terminal.insert_before(num_lines, |buf| {
                for (i, line) in wrapped_lines.iter().enumerate() {
                    if i < buf.area.height as usize {
                        buf.set_string(0, i as u16, line, ratatui::style::Style::default());
                    }
                }
            })?;

            execute!(io::stdout(), EndSynchronizedUpdate)?;

            // Mark TUI for render (separator might need to move)
            self.needs_tui_render = true;
        }

        // Blit updates to visible area with rate limiting
        // diff_buffers() will return empty if nothing changed (fast early return)
        if !messages.is_empty() && self.last_blit.elapsed() >= self.blit_interval {
            self.blit_visible_area()?;
            self.last_blit = std::time::Instant::now();
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

    /// Show LLM-prompted questions and collect answers
    ///
    /// Displays 1-4 questions sequentially using DialogWidget and collects
    /// user's answers. Returns AskUserQuestionOutput with all answers.
    pub fn show_llm_question(
        &mut self,
        input: &crate::cli::AskUserQuestionInput,
    ) -> Result<crate::cli::AskUserQuestionOutput> {
        use crate::cli::llm_dialogs;
        use std::collections::HashMap;

        // Validate input
        llm_dialogs::validate_input(input)
            .context("Invalid AskUserQuestion input")?;

        let mut answers = HashMap::new();

        // Show each question sequentially
        for question in &input.questions {
            // Convert to Dialog
            let dialog = llm_dialogs::question_to_dialog(question);

            // Show dialog and wait for answer
            let result = self.show_dialog(dialog)
                .with_context(|| format!("Failed to show question: {}", question.question))?;

            // Check for cancellation
            if result.is_cancelled() {
                anyhow::bail!("User cancelled dialog");
            }

            // Extract answer
            if let Some(answer) = llm_dialogs::extract_answer(question, &result) {
                answers.insert(question.question.clone(), answer);
            } else {
                anyhow::bail!("Failed to extract answer from dialog result");
            }
        }

        Ok(crate::cli::AskUserQuestionOutput {
            questions: input.questions.clone(),
            answers,
        })
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

            // Save command history to disk
            if let Err(e) = self.save_history() {
                eprintln!("Warning: Failed to save command history: {}", e);
            }

            // Disable keyboard enhancement flags
            execute!(stdout, PopKeyboardEnhancementFlags)
                .context("Failed to disable keyboard enhancements")?;

            // Disable raw mode
            disable_raw_mode().context("Failed to disable raw mode")?;

            self.is_active = false;
        }
        Ok(())
    }

    /// Full refresh of viewport using shadow buffer
    /// Renders all messages to shadow buffer, then updates terminal
    fn full_refresh_viewport(&mut self) -> Result<()> {
        use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
        use crossterm::style::Print;

        // Get terminal size
        let (_term_width, term_height) = crossterm::terminal::size()?;

        // Calculate visible scrollback area (above inline viewport)
        let visible_rows = term_height.saturating_sub(6); // -6 for inline viewport

        if visible_rows == 0 {
            return Ok(()); // Terminal too small
        }

        // Render all messages to shadow buffer (with proper wrapping)
        let all_messages = self.scrollback.get_visible_messages();
        self.shadow_buffer.render_messages(&all_messages, &self.colors);

        // Clear terminal and render entire shadow buffer
        let mut stdout = io::stdout();
        execute!(stdout, BeginSynchronizedUpdate)?;

        // Clear the entire visible area
        for row in 0..visible_rows {
            execute!(
                stdout,
                cursor::MoveTo(0, row),
                Clear(ClearType::UntilNewLine)
            )?;
        }

        // Render shadow buffer to terminal (row by row for efficiency)
        for y in 0..self.shadow_buffer.height {
            let mut line_content = String::new();
            for x in 0..self.shadow_buffer.width {
                if let Some(cell) = self.shadow_buffer.get(x, y) {
                    line_content.push(cell.ch);
                }
            }

            // Write entire line at once (already cleared above)
            if !line_content.trim().is_empty() {
                execute!(
                    stdout,
                    cursor::MoveTo(0, y as u16),
                    Print(line_content)
                )?;
            }
        }

        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        // Update previous frame buffer
        self.prev_frame_buffer = self.shadow_buffer.clone_buffer();

        Ok(())
    }

    /// Blit only changed cells to visible area using diff-based updates
    /// More efficient than full_refresh_viewport() for streaming updates
    fn blit_visible_area(&mut self) -> Result<()> {
        use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
        use crossterm::style::Print;
        use std::collections::HashMap;

        // Get terminal size
        let (_term_width, term_height) = crossterm::terminal::size()?;
        let visible_rows = term_height.saturating_sub(6); // -6 for inline viewport

        if visible_rows == 0 {
            return Ok(()); // Terminal too small
        }

        // Render all visible messages to shadow buffer
        let all_messages = self.scrollback.get_visible_messages();
        self.shadow_buffer.render_messages(&all_messages, &self.colors);

        // Diff with previous frame to find changes
        let changes = diff_buffers(&self.shadow_buffer, &self.prev_frame_buffer);

        if changes.is_empty() {
            return Ok(()); // No changes to apply
        }

        // Group changes by row for efficient line-based clearing
        let mut changes_by_row: HashMap<usize, Vec<(usize, char)>> = HashMap::new();

        for (x, y, cell) in changes {
            if (y as u16) < visible_rows {
                changes_by_row.entry(y).or_insert_with(Vec::new).push((x, cell.ch));
            }
        }

        // Apply changes to terminal
        let mut stdout = io::stdout();
        execute!(stdout, BeginSynchronizedUpdate)?;

        for (row, _cells) in changes_by_row {
            // Clear line and write entire row (more efficient than per-cell updates)
            execute!(stdout, cursor::MoveTo(0, row as u16), Clear(ClearType::UntilNewLine))?;

            // Build full line content from shadow buffer
            let mut line_content = String::new();
            for x in 0..self.shadow_buffer.width {
                if let Some(cell) = self.shadow_buffer.get(x, row) {
                    line_content.push(cell.ch);
                }
            }

            // Write entire line at once
            if !line_content.is_empty() {
                execute!(stdout, cursor::MoveTo(0, row as u16), Print(line_content))?;
            }
        }

        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        // Update previous frame buffer
        self.prev_frame_buffer = self.shadow_buffer.clone_buffer();

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

        // Resize shadow buffers
        let visible_rows = height.saturating_sub(6) as usize; // -6 for inline viewport
        self.shadow_buffer.resize(width as usize, visible_rows);
        self.prev_frame_buffer.resize(width as usize, visible_rows);

        // Rebuild ring buffer with new line counts
        self.scrollback.rebuild_ring_buffer();

        // Force full refresh after resize
        self.needs_full_refresh = true;
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
                        let formatted = msg.format(&self.colors);
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

            // Save command history (best effort)
            let _ = self.save_history();

            // Show cursor
            let _ = execute!(stdout, cursor::Show);
            let _ = stdout.flush();

            // Disable keyboard enhancements
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);

            // Disable raw mode
            let _ = disable_raw_mode();

            self.is_active = false;
        }
    }
}
