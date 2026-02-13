// Output Manager - Buffers output AND writes to stdout for scrollback
//
// This module provides an abstraction layer that captures all output
// (user messages, Claude responses, tool output, status info, errors)
// into a structured buffer AND writes it to stdout immediately with ANSI colors.
// This enables terminal scrollback while maintaining TUI compatibility.

use std::io::{self, Write};
use std::sync::{Arc, RwLock};

use crate::cli::messages::{
    Message, MessageRef, UserQueryMessage, StreamingResponseMessage, StaticMessage,
};

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

// OutputMessage enum removed - now using trait-based messages only

/// Thread-safe output buffer manager
pub struct OutputManager {
    /// Whether to write output to stdout immediately (for scrollback)
    write_to_stdout: Arc<RwLock<bool>>,
    /// Buffering mode - true = accumulate for batch flush, false = immediate write
    buffering_mode: Arc<RwLock<bool>>,
    /// Pending lines waiting to be flushed (used when buffering_mode = true)
    pending_flush: Arc<RwLock<Vec<String>>>,
    /// Trait-based message storage (reactive updates)
    messages: Arc<RwLock<Vec<MessageRef>>>,
    /// Color scheme for message formatting
    colors: crate::config::ColorScheme,
}

impl OutputManager {
    /// Create a new OutputManager
    pub fn new(colors: crate::config::ColorScheme) -> Self {
        Self {
            write_to_stdout: Arc::new(RwLock::new(true)), // Enabled by default, but main.rs disables immediately for TUI
            buffering_mode: Arc::new(RwLock::new(false)), // Default: immediate write
            pending_flush: Arc::new(RwLock::new(Vec::new())),
            messages: Arc::new(RwLock::new(Vec::new())),
            colors,
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

    // Old enum-based methods removed - using trait-based messages only

    // ========================================================================
    // Trait-based message API (new reactive system)
    // ========================================================================

    /// Add a trait-based message to the buffer
    pub fn add_trait_message(&self, message: MessageRef) {
        // Write to terminal if not a status message
        self.write_trait_to_terminal(&message);

        // Add to messages vector
        let mut messages = self.messages.write().unwrap();

        // Simple circular buffer (no complex ring buffer needed here)
        if messages.len() >= MAX_BUFFER_SIZE {
            messages.remove(0); // Remove oldest
        }

        messages.push(message);
    }

    // get_trait_messages(), trait_message_count(), clear_trait_messages() removed
    // Use get_messages(), len(), clear() instead

    /// Write a trait-based message to terminal
    fn write_trait_to_terminal(&self, message: &MessageRef) {
        let formatted = message.format(&self.colors);

        let buffering = *self.buffering_mode.read().unwrap();
        let write_stdout = *self.write_to_stdout.read().unwrap();

        if buffering {
            // Buffering mode: accumulate for batch flush
            self.pending_flush.write().unwrap().push(formatted);
        } else if write_stdout {
            // Immediate mode: write to stdout
            let mut stdout = io::stdout();
            let _ = write!(stdout, "{}\r\n", formatted);
            let _ = stdout.flush();
        }
    }

    // ========================================================================
    // Legacy OutputMessage API (for backward compatibility)
    // ========================================================================

    /// Write a user message
    pub fn write_user(&self, content: impl Into<String>) {
        let msg = Arc::new(UserQueryMessage::new(content));
        self.add_trait_message(msg);
    }

    /// Write a provider response (can be called incrementally for streaming)
    pub fn write_response(&self, content: impl Into<String>) {
        let msg = StreamingResponseMessage::new();
        msg.append_chunk(&content.into());
        msg.set_complete();
        self.add_trait_message(Arc::new(msg));
    }

    /// Append to the last provider response (for streaming)
    pub fn append_response(&self, content: impl Into<String>) {
        // For now, just create a new message
        // TODO: In future, find last StreamingResponseMessage and append
        self.write_response(content);
    }

    /// Write tool execution output
    pub fn write_tool(&self, tool_name: impl Into<String>, content: impl Into<String>) {
        let formatted = format!("[{}] {}", tool_name.into(), content.into());
        let msg = Arc::new(StaticMessage::plain(formatted));
        self.add_trait_message(msg);
    }

    /// Write status information (deprecated - use write_progress or write_info)
    pub fn write_status(&self, content: impl Into<String>) {
        // Route to progress for backward compatibility
        self.write_progress(content);
    }

    /// Write error message
    pub fn write_error(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::error(content));
        self.add_trait_message(msg);
    }

    /// Write progress update
    pub fn write_progress(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::plain(content));
        self.add_trait_message(msg);
    }

    /// Write system information message (help, patterns, stats)
    pub fn write_info(&self, content: impl Into<String>) {
        let msg = Arc::new(StaticMessage::plain(content));
        self.add_trait_message(msg);
    }

    /// Get all messages (for rendering)
    pub fn get_messages(&self) -> Vec<MessageRef> {
        self.messages.read().unwrap().clone()
    }

    /// Get the last N messages
    pub fn get_last_messages(&self, n: usize) -> Vec<MessageRef> {
        let messages = self.messages.read().unwrap();
        let start = messages.len().saturating_sub(n);
        messages.iter().skip(start).cloned().collect()
    }

    /// Clear all messages
    pub fn clear(&self) {
        self.messages.write().unwrap().clear();
    }

    /// Get the number of messages in the buffer
    pub fn len(&self) -> usize {
        self.messages.read().unwrap().len()
    }

    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool {
        self.messages.read().unwrap().is_empty()
    }
}

impl Default for OutputManager {
    fn default() -> Self {
        Self::new(crate::config::ColorScheme::default())
    }
}

impl Clone for OutputManager {
    fn clone(&self) -> Self {
        Self {
            write_to_stdout: Arc::clone(&self.write_to_stdout),
            buffering_mode: Arc::clone(&self.buffering_mode),
            pending_flush: Arc::clone(&self.pending_flush),
            messages: Arc::clone(&self.messages),
            colors: self.colors.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_operations() {
        let manager = OutputManager::new(crate::config::ColorScheme::default());

        manager.write_user("Hello");
        manager.write_response("Hi there!");
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
        let manager = OutputManager::new(crate::config::ColorScheme::default());

        manager.write_response("Hello");
        manager.append_response(" world");
        manager.append_response("!");

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content(), "Hello world!");
    }

    #[test]
    fn test_circular_buffer() {
        let manager = OutputManager::new(crate::config::ColorScheme::default());

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
        let manager = OutputManager::new(crate::config::ColorScheme::default());

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
        let manager = OutputManager::new(crate::config::ColorScheme::default());

        manager.write_user("Test");
        assert_eq!(manager.len(), 1);

        manager.clear();
        assert_eq!(manager.len(), 0);
        assert!(manager.is_empty());
    }

    #[test]
    fn test_system_info_message() {
        let manager = OutputManager::new(crate::config::ColorScheme::default());

        manager.write_info("Help: Available commands...");

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 1);
        assert!(matches!(messages[0], OutputMessage::SystemInfo { .. }));
        assert_eq!(messages[0].content(), "Help: Available commands...");
        assert_eq!(messages[0].message_type(), "info");
    }
}
