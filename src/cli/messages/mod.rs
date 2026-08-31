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

/// Semantic role of one retained Message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Response,
    Activity,
    Program,
    Output,
    ToolGroup,
    ToolCall,
    Input,
    ToolOutput,
}

/// Message-owned disclosure metadata. Semantic ancestry lives in the actual
/// `MessageRef` hierarchy returned by `Message::children`; this value is not a
/// parallel transcript node and deliberately contains no child collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageDisclosure {
    /// Human-readable row summary, independent of disclosure state.
    pub label: String,
    /// Complete body lines owned by this row.
    pub body: Vec<String>,
    /// Message-owned disclosure default for a new frontend.
    pub default_expanded: bool,
}

/// Test-only immutable inspection snapshot. Production rendering never builds
/// this parallel tree; it exists solely so lifecycle tests can assert a whole
/// Message hierarchy without coupling themselves to frontend primitives.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub kind: MessageKind,
    pub label: String,
    pub body: Vec<String>,
    pub children: Vec<TranscriptRow>,
    pub default_expanded: bool,
}

#[cfg(test)]
pub type TranscriptRowKind = MessageKind;

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

    pub(crate) fn as_uuid(self) -> Uuid {
        self.0
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

    /// Semantic role of this retained object.
    fn kind(&self) -> MessageKind {
        MessageKind::Response
    }

    /// Stable child objects owned by this composite message.
    fn children(&self) -> Vec<MessageRef> {
        Vec::new()
    }

    /// Optional disclosure metadata for this exact Message object.
    fn disclosure(&self, _colors: &crate::config::ColorScheme) -> Option<MessageDisclosure> {
        None
    }

    #[cfg(test)]
    fn transcript_row(&self, colors: &crate::config::ColorScheme) -> Option<TranscriptRow> {
        let disclosure = self.disclosure(colors)?;
        Some(TranscriptRow {
            kind: self.kind(),
            label: disclosure.label,
            body: disclosure.body,
            children: self
                .children()
                .into_iter()
                .filter_map(|child| child.transcript_row(colors))
                .collect(),
            default_expanded: disclosure.default_expanded,
        })
    }

    /// Render this concrete semantic message through frontend-neutral
    /// primitives. The immutable context is the only frontend capability
    /// surface available to a message.
    fn render(&self, context: &RenderContext<'_>) -> RenderedMessage {
        if let Some(disclosure) = self.disclosure(context.colors) {
            return render::render_message_tree(self.id(), &disclosure, self.children(), context);
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
