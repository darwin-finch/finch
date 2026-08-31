//! Stable semantic message for one Brain/run activity hierarchy.

use std::sync::Arc;

use super::{Message, MessageId, MessageStatus, TranscriptRow, WorkUnit};
use crate::config::ColorScheme;

/// Lifecycle, approval, and genuine tool activity for one canonical run.
///
/// The wrapped `WorkUnit` supplies the mature row machinery, but the public
/// type exposes no presentation-changing operation. An activity message can
/// therefore never be retyped into program source or output after insertion.
pub struct ActivityMessage {
    unit: Arc<WorkUnit>,
}

impl std::fmt::Debug for ActivityMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActivityMessage")
            .field("id", &Message::id(self))
            .finish_non_exhaustive()
    }
}

impl ActivityMessage {
    /// Create an activity with a fresh local identity.
    pub fn new(title: impl Into<String>) -> Self {
        Self::with_id(MessageId::new(), title)
    }

    /// Restore an activity with a canonical stable identity.
    pub fn with_id(id: MessageId, title: impl Into<String>) -> Self {
        let title = title.into();
        let unit = Arc::new(WorkUnit::with_id(id, &title));
        unit.set_activity_presentation(title);
        Self { unit }
    }

    /// Row-mutation port used by the provider/tool loop. The `WorkUnit` is not
    /// separately registered as a message, so it cannot acquire another
    /// semantic identity in the transcript.
    pub(crate) fn work_unit(&self) -> Arc<WorkUnit> {
        Arc::clone(&self.unit)
    }

    /// Add one lifecycle activity row.
    pub fn add_activity(&self, label: impl Into<String>) -> usize {
        self.unit.add_activity_row(label)
    }

    /// Add one genuine tool-call row.
    pub fn add_tool(&self, label: impl Into<String>) -> usize {
        self.unit.add_row(label)
    }

    /// Repair a tool label when canonical call metadata arrives after its
    /// result during snapshot replay.
    pub fn set_row_label(&self, index: usize, label: impl Into<String>) {
        self.unit.set_row_label(index, label);
    }

    /// Append a live body line to an existing activity or tool row.
    pub fn append_row_body_line(&self, index: usize, line: String) {
        self.unit.append_row_body_line(index, line);
    }

    /// Complete an existing row with its compact summary.
    pub fn complete_row(&self, index: usize, summary: impl Into<String>) {
        self.unit.complete_row(index, summary);
    }

    /// Complete an existing row with a summary and retained body.
    pub fn complete_row_with_body(
        &self,
        index: usize,
        summary: impl Into<String>,
        body: Vec<String>,
    ) {
        self.unit.complete_row_with_body(index, summary, body);
    }

    /// Mark an existing row as failed.
    pub fn fail_row(&self, index: usize, error: impl Into<String>) {
        self.unit.fail_row(index, error);
    }

    /// Retain provider tokens while this activity remains live.
    pub fn add_tokens(&self, text: &str) {
        self.unit.add_tokens(text);
    }

    /// Commit this activity successfully.
    pub fn set_complete(&self) {
        self.unit.set_complete();
    }

    /// Commit this activity as failed.
    pub fn set_failed(&self) {
        self.unit.set_failed();
    }
}

impl Message for ActivityMessage {
    fn id(&self) -> MessageId {
        self.unit.id()
    }

    fn format(&self, colors: &ColorScheme) -> String {
        self.unit.format(colors)
    }

    fn status(&self) -> MessageStatus {
        self.unit.status()
    }

    fn content(&self) -> String {
        self.unit.content()
    }

    fn complete_transcript(&self, colors: &ColorScheme) -> String {
        self.unit.complete_transcript(colors)
    }

    fn transcript_row(&self, colors: &ColorScheme) -> Option<TranscriptRow> {
        self.unit.transcript_row(colors)
    }

    fn background_style(&self, colors: &ColorScheme) -> Option<ratatui::style::Style> {
        self.unit.background_style(colors)
    }

    fn background_style_for_line(
        &self,
        colors: &ColorScheme,
        line_index: usize,
        line_count: usize,
    ) -> Option<ratatui::style::Style> {
        self.unit
            .background_style_for_line(colors, line_index, line_count)
    }
}
