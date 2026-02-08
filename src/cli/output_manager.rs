// Output Manager - Buffers output AND writes to stdout for scrollback
//
// This module provides an abstraction layer that captures all output
// (user messages, Claude responses, tool output, status info, errors)
// into a structured buffer AND writes it to stdout immediately with ANSI colors.
// This enables terminal scrollback while maintaining TUI compatibility.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::sync::{Arc, RwLock};

/// Maximum number of messages to keep in the circular buffer
const MAX_BUFFER_SIZE: usize = 1000;

/// ANSI color codes for terminal output
mod colors {
    pub const CYAN: &str = "\x1b[36m";      // User prompts
    pub const RED: &str = "\x1b[31m";       // Errors
    pub const GREEN: &str = "\x1b[32m";     // Success messages
    pub const YELLOW: &str = "\x1b[33m";    // Progress/status
    pub const BLUE: &str = "\x1b[34m";      // Tool output
    pub const RESET: &str = "\x1b[0m";      // Reset to default
}

/// Types of messages that can be displayed
#[derive(Debug, Clone)]
pub enum OutputMessage {
    /// User input/query
    UserMessage { content: String },
    /// Claude's response
    ClaudeResponse { content: String },
    /// Tool execution output
    ToolOutput { tool_name: String, content: String },
    /// Status information (non-critical)
    StatusInfo { content: String },
    /// Error message
    Error { content: String },
    /// Progress update (for downloads, training, etc.)
    Progress { content: String },
}

impl OutputMessage {
    /// Get the raw content of the message (for rendering)
    pub fn content(&self) -> &str {
        match self {
            OutputMessage::UserMessage { content } => content,
            OutputMessage::ClaudeResponse { content } => content,
            OutputMessage::ToolOutput { content, .. } => content,
            OutputMessage::StatusInfo { content } => content,
            OutputMessage::Error { content } => content,
            OutputMessage::Progress { content } => content,
        }
    }

    /// Get the message type as a string (for debugging/logging)
    pub fn message_type(&self) -> &str {
        match self {
            OutputMessage::UserMessage { .. } => "user",
            OutputMessage::ClaudeResponse { .. } => "claude",
            OutputMessage::ToolOutput { .. } => "tool",
            OutputMessage::StatusInfo { .. } => "status",
            OutputMessage::Error { .. } => "error",
            OutputMessage::Progress { .. } => "progress",
        }
    }
}

/// Thread-safe output buffer manager
pub struct OutputManager {
    /// Circular buffer of messages (last 1000 lines)
    buffer: Arc<RwLock<VecDeque<OutputMessage>>>,
    /// Whether to write output to stdout immediately (for scrollback)
    write_to_stdout: Arc<RwLock<bool>>,
    /// Buffering mode - true = accumulate for batch flush, false = immediate write
    buffering_mode: Arc<RwLock<bool>>,
    /// Pending lines waiting to be flushed (used when buffering_mode = true)
    pending_flush: Arc<RwLock<Vec<String>>>,
}

impl OutputManager {
    /// Create a new OutputManager
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_BUFFER_SIZE))),
            write_to_stdout: Arc::new(RwLock::new(true)), // Enable by default for TUI mode
            buffering_mode: Arc::new(RwLock::new(false)), // Default: immediate write
            pending_flush: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Enable writing to stdout (for TUI mode with scrollback)
    pub fn enable_stdout(&self) {
        *self.write_to_stdout.write().unwrap() = true;
    }

    /// Disable writing to stdout (for testing or special modes)
    pub fn disable_stdout(&self) {
        *self.write_to_stdout.write().unwrap() = false;
    }

    /// Enable buffering mode - accumulate writes for batch flush
    pub fn enable_buffering(&self) {
        *self.buffering_mode.write().unwrap() = true;
    }

    /// Disable buffering mode - writes go to stdout immediately
    pub fn disable_buffering(&self) {
        *self.buffering_mode.write().unwrap() = false;
    }

    /// Drain all pending output lines for flushing
    pub fn drain_pending(&self) -> Vec<String> {
        let mut pending = self.pending_flush.write().unwrap();
        std::mem::take(&mut *pending)
    }

    /// Check if there are pending lines to flush
    pub fn has_pending(&self) -> bool {
        !self.pending_flush.read().unwrap().is_empty()
    }

    /// Write a message to stdout with ANSI colors (internal)
    fn write_to_terminal(&self, message: &OutputMessage) {
        let formatted = self.format_message(message);

        if *self.buffering_mode.read().unwrap() {
            // Buffering mode: always accumulate for batch flush (TUI will render)
            self.pending_flush.write().unwrap().push(formatted);
        } else if *self.write_to_stdout.read().unwrap() {
            // Immediate mode: write to stdout only if enabled
            let mut stdout = io::stdout();
            // Always use \r\n for raw mode compatibility (harmless in normal mode)
            let _ = write!(stdout, "{}\r\n", formatted);
            let _ = stdout.flush();
        }
        // If neither buffering nor stdout writing, output is discarded (testing mode)
    }

    /// Format a message with ANSI colors (internal)
    fn format_message(&self, message: &OutputMessage) -> String {
        match message {
            OutputMessage::UserMessage { content } => {
                // Cyan prompt ">" with content on same line
                format!("{}> {}{}", colors::CYAN, content, colors::RESET)
            }
            OutputMessage::ClaudeResponse { content } => {
                // Default color for responses
                format!("{}{}{}", colors::RESET, content, colors::RESET)
            }
            OutputMessage::ToolOutput { tool_name, content } => {
                // Blue for tool output
                format!("{}[{}] {}{}", colors::BLUE, tool_name, content, colors::RESET)
            }
            OutputMessage::StatusInfo { content } => {
                // Default color for status (not yellow - too bright)
                content.to_string()
            }
            OutputMessage::Error { content } => {
                // Red for errors
                format!("{}❌ {}{}", colors::RED, content, colors::RESET)
            }
            OutputMessage::Progress { content } => {
                // Yellow for progress
                format!("{}{}{}", colors::YELLOW, content, colors::RESET)
            }
        }
    }

    /// Add a message to the buffer (internal)
    fn add_message(&self, message: OutputMessage) {
        // In TUI mode, skip writing StatusInfo to terminal
        // (StatusInfo should only appear in StatusBar widget)
        let should_write = match message {
            OutputMessage::StatusInfo { .. } => {
                // Don't write status to stdout - use StatusBar widget instead
                false
            }
            _ => {
                // Write all other message types to stdout normally
                true
            }
        };

        if should_write {
            // Write to terminal immediately (for scrollback)
            self.write_to_terminal(&message);
        }

        // Always buffer for TUI rendering
        let mut buffer = self.buffer.write().unwrap();

        // If buffer is full, remove oldest message
        if buffer.len() >= MAX_BUFFER_SIZE {
            buffer.pop_front();
        }

        buffer.push_back(message);
    }

    /// Write a user message
    pub fn write_user(&self, content: impl Into<String>) {
        self.add_message(OutputMessage::UserMessage {
            content: content.into(),
        });
    }

    /// Write a Claude response (can be called incrementally for streaming)
    pub fn write_claude(&self, content: impl Into<String>) {
        self.add_message(OutputMessage::ClaudeResponse {
            content: content.into(),
        });
    }

    /// Append to the last Claude response (for streaming)
    pub fn append_claude(&self, content: impl Into<String>) {
        let mut buffer = self.buffer.write().unwrap();

        // Find the last Claude response and append to it
        if let Some(last) = buffer.back_mut() {
            if let OutputMessage::ClaudeResponse { content: existing } = last {
                existing.push_str(&content.into());
                return;
            }
        }

        // If no existing Claude response, create a new one
        drop(buffer);
        self.write_claude(content);
    }

    /// Write tool execution output
    pub fn write_tool(&self, tool_name: impl Into<String>, content: impl Into<String>) {
        self.add_message(OutputMessage::ToolOutput {
            tool_name: tool_name.into(),
            content: content.into(),
        });
    }

    /// Write status information
    pub fn write_status(&self, content: impl Into<String>) {
        self.add_message(OutputMessage::StatusInfo {
            content: content.into(),
        });
    }

    /// Write error message
    pub fn write_error(&self, content: impl Into<String>) {
        self.add_message(OutputMessage::Error {
            content: content.into(),
        });
    }

    /// Write progress update
    pub fn write_progress(&self, content: impl Into<String>) {
        self.add_message(OutputMessage::Progress {
            content: content.into(),
        });
    }

    /// Get all messages (for rendering)
    pub fn get_messages(&self) -> Vec<OutputMessage> {
        self.buffer.read().unwrap().iter().cloned().collect()
    }

    /// Get the last N messages
    pub fn get_last_messages(&self, n: usize) -> Vec<OutputMessage> {
        let buffer = self.buffer.read().unwrap();
        let start = buffer.len().saturating_sub(n);
        buffer.iter().skip(start).cloned().collect()
    }

    /// Clear all messages
    pub fn clear(&self) {
        self.buffer.write().unwrap().clear();
    }

    /// Get the number of messages in the buffer
    pub fn len(&self) -> usize {
        self.buffer.read().unwrap().len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.buffer.read().unwrap().is_empty()
    }
}

impl Default for OutputManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for OutputManager {
    fn clone(&self) -> Self {
        Self {
            buffer: Arc::clone(&self.buffer),
            write_to_stdout: Arc::clone(&self.write_to_stdout),
            buffering_mode: Arc::clone(&self.buffering_mode),
            pending_flush: Arc::clone(&self.pending_flush),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let manager = OutputManager::new();

        manager.write_user("Hello");
        manager.write_claude("Hi there!");
        manager.write_tool("read", "File contents...");

        assert_eq!(manager.len(), 3);

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 3);
        assert!(matches!(messages[0], OutputMessage::UserMessage { .. }));
        assert!(matches!(messages[1], OutputMessage::ClaudeResponse { .. }));
        assert!(matches!(messages[2], OutputMessage::ToolOutput { .. }));
    }

    #[test]
    fn test_streaming_append() {
        let manager = OutputManager::new();

        manager.write_claude("Hello");
        manager.append_claude(" world");
        manager.append_claude("!");

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content(), "Hello world!");
    }

    #[test]
    fn test_circular_buffer() {
        let manager = OutputManager::new();

        // Add more than MAX_BUFFER_SIZE messages
        for i in 0..1100 {
            manager.write_user(format!("Message {}", i));
        }

        // Should only keep last 1000
        assert_eq!(manager.len(), MAX_BUFFER_SIZE);

        // First message should be "Message 100" (0-99 were dropped)
        let messages = manager.get_messages();
        assert_eq!(messages[0].content(), "Message 100");
    }

    #[test]
    fn test_get_last_messages() {
        let manager = OutputManager::new();

        for i in 0..10 {
            manager.write_user(format!("Message {}", i));
        }

        let last_3 = manager.get_last_messages(3);
        assert_eq!(last_3.len(), 3);
        assert_eq!(last_3[0].content(), "Message 7");
        assert_eq!(last_3[1].content(), "Message 8");
        assert_eq!(last_3[2].content(), "Message 9");
    }

    #[test]
    fn test_clear() {
        let manager = OutputManager::new();

        manager.write_user("Test");
        assert_eq!(manager.len(), 1);

        manager.clear();
        assert_eq!(manager.len(), 0);
        assert!(manager.is_empty());
    }
}
