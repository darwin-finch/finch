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
pub use work_unit::{WorkRow, WorkRowStatus, WorkUnit};

/// Unique identifier for messages
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(Uuid);

impl MessageId {
    /// Generate a new unique message ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
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

    /// Get the background style for this message type (for TUI rendering)
    /// Returns None for default (no background)
    fn background_style(&self) -> Option<ratatui::style::Style> {
        None // Default: no background
    }
}

/// Type alias for a shared message reference
pub type MessageRef = Arc<dyn Message>;
