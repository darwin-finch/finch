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

pub mod activity;
pub mod concrete;
pub mod program;
pub mod render;
pub mod work_unit;

pub use activity::ActivityMessage;
pub use concrete::*;
pub use program::{CanonicalAdoption, ProgramOutputMessage, ProgramSourceMessage};
pub use render::{
    ColorDepth, DisclosureLookup, FrontendKind, RenderAction, RenderCapabilities, RenderContext,
    RenderedLine, RenderedMessage,
};
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
    Activity,
    Program,
    Output,
    ToolGroup,
    ToolCall,
    Input,
    ToolOutput,
}

/// A frontend-neutral semantic tree projected from canonical message data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    /// Stable identity used by disclosure state and viewport reconstruction.
    pub id: TranscriptRowId,
    /// Semantic role of this row.
    pub kind: TranscriptRowKind,
    /// Human-readable row summary, independent of disclosure state.
    pub label: String,
    /// Complete body lines owned by this row.
    pub body: Vec<String>,
    /// Append-stable semantic descendants.
    pub children: Vec<TranscriptRow>,
    /// Message-owned disclosure default for a new frontend.
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

    /// Derive a stable semantic child identity from a canonical namespace.
    pub fn from_namespace(namespace: Uuid, semantic_key: &str) -> Self {
        Self(Uuid::new_v5(&namespace, semantic_key.as_bytes()))
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

    /// Render this concrete semantic message through frontend-neutral
    /// primitives. The immutable context is the only frontend capability
    /// surface available to a message.
    fn render(&self, context: &RenderContext<'_>) -> RenderedMessage {
        if let Some(root) = self.transcript_row(context.colors) {
            return render::render_transcript_tree(self.id(), &root, context);
        }
        RenderedMessage {
            message_id: self.id(),
            lines: self
                .format(context.colors)
                .split('\n')
                .map(RenderedLine::plain)
                .collect(),
        }
    }

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
