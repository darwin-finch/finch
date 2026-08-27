// Messages Module - Trait-based polymorphic message system
//
// Provides a flexible message system where different message types can have
// completely different update interfaces while sharing a common display trait.
//
// Design:
// - Message trait: Minimal read-only interface (id, format, status)
// - Concrete types: Each has type-specific update methods
// - Thread-safe: Arc<RwLock<>> for interior mutability
// - No downcasting: Handlers receive concrete types

use std::fmt;
use std::sync::Arc;
use uuid::Uuid;

pub mod concrete;
pub mod work_unit;

pub use concrete::*;
pub use work_unit::{random_spinner_verb, WorkRow, WorkRowStatus, WorkUnit};

/// Stable identity for one expandable row within a retained message.
///
/// `path` is append-only semantic ancestry (unit, call index, input/output), so
/// streamed appends and terminal reflow never change an existing row's key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TranscriptRowId {
    pub message_id: MessageId,
    pub path: Vec<u32>,
}

/// Semantic defaults used by the transcript disclosure renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptRowKind {
    Response,
    Program,
    Output,
    ToolGroup,
    ToolCall,
    Input,
    ToolOutput,
}

/// A presentation-only tree projected from canonical message data.
#[derive(Debug, Clone)]
pub struct TranscriptRow {
    pub id: TranscriptRowId,
    pub kind: TranscriptRowKind,
    pub label: String,
    pub body: Vec<String>,
    pub children: Vec<TranscriptRow>,
    pub default_expanded: bool,
}

/// Unique identifier for messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(Uuid);

impl MessageId {
    /// Generate a new unique message ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Restore a stable ID supplied by a canonical transcript event.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Status of a message
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageStatus {
    /// Message is being updated (streaming, downloading, etc.)
    InProgress,
    /// Message is complete and won't change
    Complete,
    /// Message represents a failed operation
    Failed,
}

/// Trait that all messages must implement
///
/// This is a minimal read-only interface. Each concrete message type
/// defines its own update methods appropriate for its use case.
pub trait Message: Send + Sync {
    /// Get the unique identifier for this message
    fn id(&self) -> MessageId;

    /// Format this message for display (with ANSI colors and styling)
    fn format(&self, colors: &crate::config::ColorScheme) -> String;

    /// Get the current status of this message
    fn status(&self) -> MessageStatus;

    /// Get the raw content (without formatting, for change detection)
    fn content(&self) -> String;

    /// Complete canonical text for permanent terminal scrollback and copying.
    /// Presentation-only disclosure state must never affect this value.
    fn complete_transcript(&self, colors: &crate::config::ColorScheme) -> String {
        self.format(colors)
    }

    /// Optional semantic retained-row projection for interactive disclosure.
    fn transcript_row(&self, _colors: &crate::config::ColorScheme) -> Option<TranscriptRow> {
        None
    }

    /// Get the background style for this message type (for TUI rendering)
    /// Returns None for default (no background)
    fn background_style(
        &self,
        _colors: &crate::config::ColorScheme,
    ) -> Option<ratatui::style::Style> {
        None // Default: no background
    }

    /// Get the background style for one logical line of a formatted message.
    /// Most messages use one semantic band throughout; mixed messages can
    /// override this without embedding presentation codes in copied text.
    fn background_style_for_line(
        &self,
        colors: &crate::config::ColorScheme,
        _line_index: usize,
        _line_count: usize,
    ) -> Option<ratatui::style::Style> {
        self.background_style(colors)
    }
}

/// Type alias for a shared message reference
pub type MessageRef = Arc<dyn Message>;
