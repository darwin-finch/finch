//! The view model for the live activity panel.
//!
//! The renderer draws indented status rows. It does not know what produces them — a task list, a
//! tree of child agents, or anything a future embedder invents — because a terminal framework that
//! knows what a `TodoList` is cannot be used by anything that has no todos.
//!
//! Callers translate their own types into [`ActivityRow`] at the boundary, which is the only place
//! the two vocabularies meet.

use std::sync::Arc;
use uuid::Uuid;

/// How far along a row's work is. The renderer chooses a glyph and colour from this and nothing
/// else, so a caller never decides how activity looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    Pending,
    Active,
    Done,
}

/// One row of the live activity panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityRow {
    /// Nesting level; each level is indented two columns.
    pub depth: usize,
    pub state: ActivityState,
    /// The row's own text, truncated by the renderer to fit the terminal.
    pub text: String,
    /// Draws an attention marker. What deserves one is the caller's judgement.
    pub urgent: bool,
    /// Trailing context shown after the text when there is room, such as a model or tool name.
    pub detail: Option<String>,
}

impl ActivityRow {
    pub fn new(text: impl Into<String>, state: ActivityState) -> Self {
        Self {
            depth: 0,
            state,
            text: text.into(),
            urgent: false,
            detail: None,
        }
    }
}

/// A source the renderer polls for rows when it redraws.
///
/// `rows` is called on the render path, so it must not block: a source behind a lock returns what
/// it has, or nothing, rather than making the terminal wait on another task.
pub trait ActivityRows: Send + Sync {
    fn rows(&self) -> Vec<ActivityRow>;
}

/// A change to the set of rows the renderer is tracking by identity.
///
/// Rows that arrive as a stream of events, rather than by polling a list, are applied through this.
#[derive(Debug, Clone)]
pub enum ActivityUpdate {
    /// Add the row, or replace the one already under this id.
    Upsert {
        id: Uuid,
        row: ActivityRow,
    },
    /// Replace a row's trailing detail without disturbing the rest of it.
    SetDetail {
        id: Uuid,
        detail: Option<String>,
    },
    Remove {
        id: Uuid,
    },
}

/// A polled source, held by the renderer for as long as it is attached.
pub type SharedActivityRows = Arc<dyn ActivityRows>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_row_defaults_to_the_top_level_and_no_decoration() {
        // Callers set only what they mean; everything else must be inert by default.
        let row = ActivityRow::new("build the thing", ActivityState::Active);
        assert_eq!(0, row.depth);
        assert!(!row.urgent);
        assert_eq!(None, row.detail);
    }
}
