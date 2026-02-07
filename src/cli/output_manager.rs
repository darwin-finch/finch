// Output Manager - Buffers all output for eventual TUI rendering
//
// This module provides an abstraction layer that captures all output
// (user messages, Claude responses, tool output, status info, errors)
// into a structured buffer. This allows us to later render it in a
// scrollable TUI without changing the rest of the codebase.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

/// Maximum number of messages to keep in the circular buffer
const MAX_BUFFER_SIZE: usize = 1000;

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
}

impl OutputManager {
    /// Create a new OutputManager
    pub fn new() -> Self {
        Self {
            buffer: Arc::new(RwLock::new(VecDeque::with_capacity(MAX_BUFFER_SIZE))),
        }
    }

    /// Add a message to the buffer (internal)
    fn add_message(&self, message: OutputMessage) {
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
