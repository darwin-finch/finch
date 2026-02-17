// Concrete Message Types
//
// Each message type has its own update interface appropriate for its use case.
// No need for downcasting - handlers receive concrete types directly.

use super::{Message, MessageId, MessageStatus};
use crate::config::{ColorScheme, ColorSpec};
use std::sync::{Arc, RwLock};

/// Helper to convert ColorSpec to ANSI escape code
fn color_to_ansi(color: &ColorSpec) -> String {
    use ratatui::style::Color;

    match color {
        ColorSpec::Named(name) => {
            // Map named colors to ANSI codes
            match name.to_lowercase().as_str() {
                "black" => "\x1b[30m",
                "red" => "\x1b[31m",
                "green" => "\x1b[32m",
                "yellow" => "\x1b[33m",
                "blue" => "\x1b[34m",
                "magenta" => "\x1b[35m",
                "cyan" => "\x1b[36m",
                "white" => "\x1b[37m",
                "gray" | "grey" => "\x1b[90m",
                "darkgray" | "darkgrey" => "\x1b[90m",
                "lightred" => "\x1b[91m",
                "lightgreen" => "\x1b[92m",
                "lightyellow" => "\x1b[93m",
                "lightblue" => "\x1b[94m",
                "lightmagenta" => "\x1b[95m",
                "lightcyan" => "\x1b[96m",
                _ => "\x1b[37m", // Default to white
            }.to_string()
        }
        ColorSpec::Rgb(r, g, b) => {
            // True color ANSI escape code
            format!("\x1b[38;2;{};{};{}m", r, g, b)
        }
    }
}

const RESET: &str = "\x1b[0m";

// ============================================================================
// UserQueryMessage - Immutable message for user input
// ============================================================================

/// User query message (immutable after creation)
pub struct UserQueryMessage {
    id: MessageId,
    content: String,
}

impl UserQueryMessage {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
        }
    }
}

impl Message for UserQueryMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        format!(
            "{} ❯ {}{}",
            color_to_ansi(&colors.messages.user),
            self.content,
            RESET
        )
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        self.content.clone()
    }

    fn background_style(&self) -> Option<ratatui::style::Style> {
        use ratatui::style::{Color, Style};
        // Grey background for user messages (like Claude Code)
        Some(Style::default()
            .bg(Color::Rgb(220, 220, 220))
            .fg(Color::Black))
    }
}

// ============================================================================
// StreamingResponseMessage - Mutable message for Claude/Qwen responses
// ============================================================================

/// Streaming response message (for Claude/Qwen)
pub struct StreamingResponseMessage {
    id: MessageId,
    content: Arc<RwLock<String>>,
    status: Arc<RwLock<MessageStatus>>,
    thinking: Arc<RwLock<bool>>,
}

impl StreamingResponseMessage {
    pub fn new() -> Self {
        Self {
            id: MessageId::new(),
            content: Arc::new(RwLock::new(String::new())),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
            thinking: Arc::new(RwLock::new(false)),
        }
    }

    /// Append a chunk of streamed text
    pub fn append_chunk(&self, text: &str) {
        match self.content.write() {
            Ok(mut content) => content.push_str(text),
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage content lock poisoned in append_chunk, recovering");
                let mut content = poisoned.into_inner();
                content.push_str(text);
            }
        }
    }

    /// Set whether the model is thinking (for UI indicator)
    pub fn set_thinking(&self, thinking: bool) {
        match self.thinking.write() {
            Ok(mut t) => *t = thinking,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage thinking lock poisoned in set_thinking, recovering");
                *poisoned.into_inner() = thinking;
            }
        }
    }

    /// Mark this response as complete
    pub fn set_complete(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage status lock poisoned in set_complete, recovering");
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark this response as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage status lock poisoned in set_failed, recovering");
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for StreamingResponseMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully - recover with safe defaults
        let content = match self.content.read() {
            Ok(c) => c.clone(),
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage content lock poisoned, using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let status = match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage status lock poisoned, defaulting to InProgress");
                *poisoned.into_inner()
            }
        };

        let thinking = match self.thinking.read() {
            Ok(t) => *t,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage thinking lock poisoned, defaulting to false");
                *poisoned.into_inner()
            }
        };

        // No cleaning - already cleaned by daemon during streaming
        let text = content.clone();

        match status {
            MessageStatus::InProgress if thinking => {
                format!("🤔 [thinking...]\n{}", text)
            }
            MessageStatus::InProgress => {
                // Regular streaming (not thinking)
                if text.is_empty() {
                    "⏳ [streaming...]".to_string()
                } else {
                    format!("{}▸", text)  // Streaming indicator at end
                }
            }
            MessageStatus::Failed => {
                format!(
                    "{}❌ Response failed{}\n{}",
                    color_to_ansi(&colors.messages.error),
                    RESET,
                    text
                )
            }
            MessageStatus::Complete => text,
        }
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage status lock poisoned in status(), using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        match self.content.read() {
            Ok(c) => c.clone(),
            Err(poisoned) => {
                tracing::warn!("StreamingResponseMessage content lock poisoned in content(), using recovered data");
                poisoned.into_inner().clone()
            }
        }
    }
}

impl Default for StreamingResponseMessage {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// ToolExecutionMessage - Message for tool execution with stdout/stderr
// ============================================================================

/// Tool execution message with separate stdout/stderr
pub struct ToolExecutionMessage {
    id: MessageId,
    tool_name: String,
    stdout: Arc<RwLock<String>>,
    stderr: Arc<RwLock<String>>,
    exit_code: Arc<RwLock<Option<i32>>>,
    status: Arc<RwLock<MessageStatus>>,
}

impl ToolExecutionMessage {
    pub fn new(tool_name: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            tool_name: tool_name.into(),
            stdout: Arc::new(RwLock::new(String::new())),
            stderr: Arc::new(RwLock::new(String::new())),
            exit_code: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Append to stdout
    pub fn append_stdout(&self, text: &str) {
        match self.stdout.write() {
            Ok(mut stdout) => stdout.push_str(text),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stdout lock poisoned in append_stdout, recovering");
                let mut stdout = poisoned.into_inner();
                stdout.push_str(text);
            }
        }
    }

    /// Append to stderr
    pub fn append_stderr(&self, text: &str) {
        match self.stderr.write() {
            Ok(mut stderr) => stderr.push_str(text),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stderr lock poisoned in append_stderr, recovering");
                let mut stderr = poisoned.into_inner();
                stderr.push_str(text);
            }
        }
    }

    /// Set exit code (marks as complete)
    pub fn set_exit_code(&self, code: i32) {
        match self.exit_code.write() {
            Ok(mut e) => *e = Some(code),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage exit_code lock poisoned in set_exit_code, recovering");
                *poisoned.into_inner() = Some(code);
            }
        }
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage status lock poisoned in set_exit_code, recovering");
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage status lock poisoned in set_failed, recovering");
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for ToolExecutionMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully
        let stdout = match self.stdout.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stdout lock poisoned, using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let stderr = match self.stderr.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stderr lock poisoned, using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let exit_code = match self.exit_code.read() {
            Ok(e) => *e,
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage exit_code lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        };

        let mut result = format!(
            "{}[{}]{}",
            color_to_ansi(&colors.messages.tool),
            self.tool_name,
            RESET
        );

        if !stdout.is_empty() {
            result.push('\n');
            result.push_str(&stdout);
        }

        if !stderr.is_empty() {
            result.push('\n');
            result.push_str(&format!(
                "{}stderr: {}{}",
                color_to_ansi(&colors.messages.error),
                stderr,
                RESET
            ));
        }

        if let Some(code) = exit_code {
            result.push('\n');
            if code == 0 {
                result.push_str(&format!(
                    "{}✓ exit code: {}{}",
                    color_to_ansi(&colors.messages.system),
                    code,
                    RESET
                ));
            } else {
                result.push_str(&format!(
                    "{}✗ exit code: {}{}",
                    color_to_ansi(&colors.messages.error),
                    code,
                    RESET
                ));
            }
        }

        result
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        let stdout = match self.stdout.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stdout lock poisoned in content(), using recovered data");
                poisoned.into_inner().clone()
            }
        };

        let stderr = match self.stderr.read() {
            Ok(s) => s.clone(),
            Err(poisoned) => {
                tracing::warn!("ToolExecutionMessage stderr lock poisoned in content(), using recovered data");
                poisoned.into_inner().clone()
            }
        };

        format!("{}\n{}", stdout, stderr)
    }
}

// ============================================================================
// ProgressMessage - Message for download/upload progress
// ============================================================================

/// Progress message for downloads, uploads, etc.
pub struct ProgressMessage {
    id: MessageId,
    label: String,
    current: Arc<RwLock<u64>>,
    total: u64,
    status: Arc<RwLock<MessageStatus>>,
}

impl ProgressMessage {
    pub fn new(label: impl Into<String>, total: u64) -> Self {
        Self {
            id: MessageId::new(),
            label: label.into(),
            current: Arc::new(RwLock::new(0)),
            total,
            status: Arc::new(RwLock::new(MessageStatus::InProgress)),
        }
    }

    /// Update progress
    pub fn update_progress(&self, current: u64) {
        match self.current.write() {
            Ok(mut c) => *c = current,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage current lock poisoned in update_progress, recovering");
                *poisoned.into_inner() = current;
            }
        }

        // Auto-complete when reaching 100%
        if current >= self.total {
            match self.status.write() {
                Ok(mut s) => *s = MessageStatus::Complete,
                Err(poisoned) => {
                    tracing::warn!("ProgressMessage status lock poisoned in update_progress, recovering");
                    *poisoned.into_inner() = MessageStatus::Complete;
                }
            }
        }
    }

    /// Mark as complete
    pub fn set_complete(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Complete,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned in set_complete, recovering");
                *poisoned.into_inner() = MessageStatus::Complete;
            }
        }
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        match self.status.write() {
            Ok(mut s) => *s = MessageStatus::Failed,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned in set_failed, recovering");
                *poisoned.into_inner() = MessageStatus::Failed;
            }
        }
    }
}

impl Message for ProgressMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        // Handle poisoned locks gracefully
        let current = match self.current.read() {
            Ok(c) => *c,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage current lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        };

        let status = match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        };

        let percentage = if self.total > 0 {
            (current as f64 / self.total as f64 * 100.0) as u8
        } else {
            0
        };

        // Progress bar: [████████░░] 80%
        let filled = (percentage / 10).min(10) as usize;
        let empty = 10 - filled;
        let bar = format!("[{}{}]", "█".repeat(filled), "░".repeat(empty));

        match status {
            MessageStatus::Complete => {
                format!(
                    "{}{} {} 100% ✓{}",
                    color_to_ansi(&colors.status.download),
                    self.label,
                    bar,
                    RESET
                )
            }
            MessageStatus::Failed => {
                format!(
                    "{}{} {} {}% ✗{}",
                    color_to_ansi(&colors.messages.error),
                    self.label,
                    bar,
                    percentage,
                    RESET
                )
            }
            MessageStatus::InProgress => {
                format!(
                    "{}{} {} {}%{}",
                    color_to_ansi(&colors.status.operation),
                    self.label,
                    bar,
                    percentage,
                    RESET
                )
            }
        }
    }

    fn status(&self) -> MessageStatus {
        match self.status.read() {
            Ok(s) => *s,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage status lock poisoned, using recovered data");
                *poisoned.into_inner()
            }
        }
    }

    fn content(&self) -> String {
        let current = match self.current.read() {
            Ok(c) => *c,
            Err(poisoned) => {
                tracing::warn!("ProgressMessage current lock poisoned in content(), using recovered data");
                *poisoned.into_inner()
            }
        };

        format!("{}: {}/{}", self.label, current, self.total)
    }
}

// ============================================================================
// StaticMessage - Immutable message for errors, info, etc.
// ============================================================================

/// Static message (immutable, for errors, system info, etc.)
pub struct StaticMessage {
    id: MessageId,
    content: String,
    message_type: StaticMessageType,
}

#[derive(Debug, Clone, Copy)]
pub enum StaticMessageType {
    Info,
    Error,
    Success,
    Warning,
    Plain, // For messages that already have their own formatting
}

impl StaticMessage {
    pub fn info(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Info,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Error,
        }
    }

    pub fn success(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Success,
        }
    }

    pub fn warning(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Warning,
        }
    }

    pub fn plain(content: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            content: content.into(),
            message_type: StaticMessageType::Plain,
        }
    }
}

impl Message for StaticMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        match self.message_type {
            StaticMessageType::Info => {
                format!(
                    "{}ℹ️  {}{}",
                    color_to_ansi(&colors.messages.system),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Error => {
                format!(
                    "{}❌ {}{}",
                    color_to_ansi(&colors.messages.error),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Success => {
                format!(
                    "{}✓ {}{}",
                    color_to_ansi(&colors.messages.system),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Warning => {
                format!(
                    "{}⚠️  {}{}",
                    color_to_ansi(&colors.status.operation),
                    self.content,
                    RESET
                )
            }
            StaticMessageType::Plain => {
                // No prefix - content already formatted
                self.content.clone()
            }
        }
    }

    fn status(&self) -> MessageStatus {
        MessageStatus::Complete
    }

    fn content(&self) -> String {
        self.content.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_streaming_message_handles_poisoned_lock() {
        let msg = StreamingResponseMessage::new();

        // Poison the content lock by panicking while holding it
        let content_clone = Arc::clone(&msg.content);
        let handle = std::thread::spawn(move || {
            let _guard = content_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join(); // Let thread panic

        // Now the lock is poisoned - format() should NOT panic
        let colors = crate::config::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and return some string (not panic)
        assert!(!result.is_empty());
        // Should either be empty or show streaming indicator
        assert!(result.contains("[streaming...]") || result.is_empty() || result.contains("▸"));
    }

    #[test]
    fn test_streaming_message_concurrent_access() {
        let msg = Arc::new(StreamingResponseMessage::new());
        let mut handles = vec![];

        // Spawn 10 threads reading/writing concurrently
        for i in 0..10 {
            let msg_clone = Arc::clone(&msg);
            handles.push(std::thread::spawn(move || {
                if i % 2 == 0 {
                    msg_clone.append_chunk(&format!("chunk {}", i));
                } else {
                    let colors = crate::config::ColorScheme::default();
                    let _ = msg_clone.format(&colors);
                }
            }));
        }

        // All threads should complete without deadlock or panic
        for handle in handles {
            handle.join().unwrap();
        }

        // Message should contain some content
        let content = msg.content();
        assert!(content.contains("chunk"));
    }

    #[test]
    fn test_tool_message_handles_poisoned_lock() {
        let msg = ToolExecutionMessage::new("test_tool");

        // Poison the stdout lock
        let stdout_clone = Arc::clone(&msg.stdout);
        let handle = std::thread::spawn(move || {
            let _guard = stdout_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join();

        // format() should NOT panic
        let colors = crate::config::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and return formatted output
        assert!(result.contains("test_tool"));
    }

    #[test]
    fn test_progress_message_handles_poisoned_lock() {
        let msg = ProgressMessage::new("Download", 100);

        // Poison the current lock
        let current_clone = Arc::clone(&msg.current);
        let handle = std::thread::spawn(move || {
            let _guard = current_clone.write().unwrap();
            panic!("Intentional panic to poison lock");
        });
        let _ = handle.join();

        // format() should NOT panic
        let colors = crate::config::ColorScheme::default();
        let result = msg.format(&colors);

        // Should recover and show progress bar
        assert!(result.contains("Download"));
        assert!(result.contains("["));
        assert!(result.contains("]"));
    }
}
