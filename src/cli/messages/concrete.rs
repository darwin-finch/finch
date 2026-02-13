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
        let mut content = self.content.write().unwrap();
        content.push_str(text);
    }

    /// Set whether the model is thinking (for UI indicator)
    pub fn set_thinking(&self, thinking: bool) {
        *self.thinking.write().unwrap() = thinking;
    }

    /// Mark this response as complete
    pub fn set_complete(&self) {
        *self.status.write().unwrap() = MessageStatus::Complete;
    }

    /// Mark this response as failed
    pub fn set_failed(&self) {
        *self.status.write().unwrap() = MessageStatus::Failed;
    }
}

impl Message for StreamingResponseMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        let content = self.content.read().unwrap();
        let status = *self.status.read().unwrap();
        let thinking = *self.thinking.read().unwrap();

        match status {
            MessageStatus::InProgress if thinking => {
                format!("🤔 [thinking...]\n{}", content)
            }
            MessageStatus::InProgress => {
                // Regular streaming (not thinking)
                if content.is_empty() {
                    "⏳ [streaming...]".to_string()
                } else {
                    format!("{}▸", content)  // Streaming indicator at end
                }
            }
            MessageStatus::Failed => {
                format!(
                    "{}❌ Response failed{}\n{}",
                    color_to_ansi(&colors.messages.error),
                    RESET,
                    content
                )
            }
            MessageStatus::Complete => content.clone(),
        }
    }

    fn status(&self) -> MessageStatus {
        *self.status.read().unwrap()
    }

    fn content(&self) -> String {
        self.content.read().unwrap().clone()
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
        let mut stdout = self.stdout.write().unwrap();
        stdout.push_str(text);
    }

    /// Append to stderr
    pub fn append_stderr(&self, text: &str) {
        let mut stderr = self.stderr.write().unwrap();
        stderr.push_str(text);
    }

    /// Set exit code (marks as complete)
    pub fn set_exit_code(&self, code: i32) {
        *self.exit_code.write().unwrap() = Some(code);
        *self.status.write().unwrap() = MessageStatus::Complete;
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        *self.status.write().unwrap() = MessageStatus::Failed;
    }
}

impl Message for ToolExecutionMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        let stdout = self.stdout.read().unwrap();
        let stderr = self.stderr.read().unwrap();
        let exit_code = *self.exit_code.read().unwrap();

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
        *self.status.read().unwrap()
    }

    fn content(&self) -> String {
        let stdout = self.stdout.read().unwrap();
        let stderr = self.stderr.read().unwrap();
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
        *self.current.write().unwrap() = current;

        // Auto-complete when reaching 100%
        if current >= self.total {
            *self.status.write().unwrap() = MessageStatus::Complete;
        }
    }

    /// Mark as complete
    pub fn set_complete(&self) {
        *self.status.write().unwrap() = MessageStatus::Complete;
    }

    /// Mark as failed
    pub fn set_failed(&self) {
        *self.status.write().unwrap() = MessageStatus::Failed;
    }
}

impl Message for ProgressMessage {
    fn id(&self) -> MessageId {
        self.id
    }

    fn format(&self, colors: &ColorScheme) -> String {
        let current = *self.current.read().unwrap();
        let status = *self.status.read().unwrap();

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
        *self.status.read().unwrap()
    }

    fn content(&self) -> String {
        let current = *self.current.read().unwrap();
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
