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

use std::sync::Arc;

pub use finch_ui_model::{
    AgentActivityView, AgentToolView, ComponentView, LiveToolView, MessageId, MessageStatus,
    OperationRowView, OperationView, OutputVm, ProgramSourceVm, ProgressView, SayTurnStatus,
    SayTurnView, StaticTextKind, StaticTextView, WorkRowPresentation, WorkRowStatus, WorkRowView,
    WorkUnitHead, WorkUnitPresentation, WorkUnitView, WorkUnitViewModel,
};

mod concrete;
mod work_unit;

pub use concrete::{
    BrainParticipantMessage, LiveToolMessage, OperationMessage, OperationRow, OperationRowStatus,
    ProgressMessage, StaticMessage, StaticMessageType, StreamingResponseMessage,
    ToolExecutionMessage, UserQueryMessage,
};
pub use work_unit::{random_spinner_verb, ComponentAction, ToggleProgram, WorkRow, WorkUnit};

/// Trait that all messages must implement
///
/// This is a minimal read-only interface. Each concrete message type
/// defines its own update methods appropriate for its use case.
pub trait Message: Send + Sync {
    /// Get the unique identifier for this message
    fn id(&self) -> MessageId;

    /// Format this message for display (with ANSI colors and styling)
    fn format(&self, colors: &finch_theme::ColorScheme) -> String;

    /// Get the current status of this message
    fn status(&self) -> MessageStatus;

    /// Get the raw content (without formatting, for change detection)
    fn content(&self) -> String;

    /// Complete canonical text for permanent terminal scrollback and copying.
    /// Presentation-only disclosure state must never affect this value.
    fn complete_transcript(&self, colors: &finch_theme::ColorScheme) -> String {
        self.format(colors)
    }

    /// Lightweight domain snapshot of this message when it is a WorkUnit run:
    /// presentation class, status, and body text, without row bodies. Filter
    /// and classify consumers use this instead of a full projection.
    fn work_unit_head(&self) -> Option<WorkUnitHead> {
        None
    }

    /// Full blit-time domain snapshot of this message when it is a WorkUnit
    /// run: plain domain data (labels, statuses, bodies) the renderer's
    /// ViewModel projects into widget props once per frame. A WorkUnit is
    /// domain data, never a widget kind (#805).
    fn work_unit_view(&self, _colors: &finch_theme::ColorScheme) -> Option<WorkUnitView> {
        None
    }

    /// The component-owned ViewModel snapshot of a migrated say turn (#882,
    /// stage 1 of docs/TUI_DESIGN.md), read under the message's own lock.
    /// `None` for rows that have not migrated to component-owned rendering;
    /// those keep the renderer's RowId-keyed disclosure maps. Kept since
    /// stage 3 for the consolidated-source pairing helper and the
    /// disclosure-direction read — the renderer's projection path asks
    /// [`Self::component_view`] instead.
    fn say_turn_view(&self) -> Option<SayTurnView> {
        None
    }

    /// The generalized component snapshot of this message (stage 3 of
    /// docs/TUI_DESIGN.md, #1120): the message constructs its component from
    /// its retained ViewModel under its own lock, and the renderer asks this
    /// instead of matching on the message type — the maintainer's original
    /// decision. `None` for rows that have not migrated to component-owned
    /// rendering; those keep the legacy projection path.
    fn component_view(&self) -> Option<ComponentView> {
        None
    }

    /// The component-defined action a click on the row at `path` produces.
    /// The engine resolves it and routes it to [`Self::handle_transcript_action`]
    /// without inspecting it — there is no central action enum.
    fn transcript_action(&self, _path: &[u32]) -> Option<ComponentAction> {
        None
    }

    /// Route a component action to the owning component's handle, which
    /// mutates the component ViewModel under the message's lock. True when
    /// handled.
    fn handle_transcript_action(&self, _action: &ComponentAction) -> bool {
        false
    }

    /// Get the background style for this message type (for TUI rendering)
    /// Returns None for default (no background)
    fn background_style(
        &self,
        _colors: &finch_theme::ColorScheme,
    ) -> Option<ratatui::style::Style> {
        None // Default: no background
    }

    /// Get the background style for one logical line of a formatted message.
    /// Most messages use one semantic band throughout; mixed messages can
    /// override this without embedding presentation codes in copied text.
    fn background_style_for_line(
        &self,
        colors: &finch_theme::ColorScheme,
        _line_index: usize,
        _line_count: usize,
    ) -> Option<ratatui::style::Style> {
        self.background_style(colors)
    }
}

/// Type alias for a shared message reference
pub type MessageRef = Arc<dyn Message>;
