//! Stable semantic messages for program source and emitted program output.

use std::sync::Mutex;

use super::{
    Message, MessageDisclosure, MessageId, MessageKind, MessageRef, MessageStatus, WorkUnit,
};
use crate::config::ColorScheme;

/// Outcome of reconciling a canonical artifact with its live projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonicalAdoption {
    Adopted,
    AlreadyAdopted,
    Conflict,
}

/// One inspectable provider/Brain program source artifact.
pub struct ProgramSourceMessage {
    unit: WorkUnit,
    canonical_source: Mutex<Option<String>>,
}

impl std::fmt::Debug for ProgramSourceMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgramSourceMessage")
            .field("id", &Message::id(self))
            .finish_non_exhaustive()
    }
}

impl ProgramSourceMessage {
    /// Create a source artifact with a fresh local identity.
    pub fn new(language: impl Into<String>) -> Self {
        Self::with_id(MessageId::new(), language)
    }

    /// Restore a source artifact with a canonical stable identity.
    pub fn with_id(id: MessageId, language: impl Into<String>) -> Self {
        let unit = WorkUnit::with_id(id, "program source");
        unit.set_program_source(language);
        Self {
            unit,
            canonical_source: Mutex::new(None),
        }
    }

    /// Replace provisional source while this artifact is live.
    pub fn replace_source(&self, source: impl Into<String>) -> bool {
        if self.status() != MessageStatus::InProgress {
            return false;
        }
        self.unit.set_response(source);
        true
    }

    /// Append provisional source while this artifact is live.
    pub fn append_source(&self, source: &str) -> bool {
        if self.status() != MessageStatus::InProgress {
            return false;
        }
        self.unit.append_response(source);
        true
    }

    /// Commit the provisional source successfully.
    pub fn set_complete(&self) {
        self.unit.set_complete();
    }

    /// Commit the provisional source as failed.
    pub fn set_failed(&self) {
        self.unit.set_failed();
    }

    /// Adopt matching canonical source exactly once. A late conflicting event
    /// cannot rewrite an already committed program message.
    pub fn adopt_canonical_source(&self, source: &str) -> CanonicalAdoption {
        let mut canonical = self
            .canonical_source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(adopted) = canonical.as_deref() {
            return if adopted == source {
                CanonicalAdoption::AlreadyAdopted
            } else {
                CanonicalAdoption::Conflict
            };
        }
        let current = self.content();
        if !current.is_empty() && current != source {
            return CanonicalAdoption::Conflict;
        }
        if current.is_empty() {
            self.unit.set_response(source);
        }
        *canonical = Some(source.to_string());
        self.unit.set_complete();
        CanonicalAdoption::Adopted
    }

    /// Whether a canonical event has finalized this source.
    pub fn canonical_adopted(&self) -> bool {
        self.canonical_source
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

/// One emitted program-output artifact, distinct from its source and activity.
pub struct ProgramOutputMessage {
    unit: WorkUnit,
    canonical_output: Mutex<Option<String>>,
}

impl std::fmt::Debug for ProgramOutputMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProgramOutputMessage")
            .field("id", &Message::id(self))
            .finish_non_exhaustive()
    }
}

impl ProgramOutputMessage {
    /// Create an output artifact with a fresh local identity.
    pub fn new() -> Self {
        Self::with_id(MessageId::new())
    }

    /// Restore an output artifact with a canonical stable identity.
    pub fn with_id(id: MessageId) -> Self {
        let unit = WorkUnit::with_id(id, "program output");
        unit.set_program_output();
        Self {
            unit,
            canonical_output: Mutex::new(None),
        }
    }

    /// Append provisional emitted output while this artifact is live.
    pub fn append_output(&self, output: &str) -> bool {
        if self.status() != MessageStatus::InProgress || self.canonical_adopted() {
            return false;
        }
        self.unit.append_response(output);
        true
    }

    /// Replace provisional output while this artifact is live.
    pub fn replace_output(&self, output: impl Into<String>) -> bool {
        if self.status() != MessageStatus::InProgress || self.canonical_adopted() {
            return false;
        }
        self.unit.set_response(output);
        true
    }

    /// Set the title of a live explicit output handle.
    pub fn set_output_handle(&self, title: impl Into<String>) -> bool {
        if self.status() != MessageStatus::InProgress || self.canonical_adopted() {
            return false;
        }
        self.unit.set_output_handle(title);
        true
    }

    /// Set live, noncanonical status text.
    pub fn set_transient_status(&self, status: Option<String>) -> bool {
        if self.status() != MessageStatus::InProgress || self.canonical_adopted() {
            return false;
        }
        self.unit.set_transient_status(status);
        true
    }

    /// Set live progress independently of durable output content.
    pub fn set_output_progress(&self, completed: u64, total: Option<u64>) -> bool {
        if self.status() != MessageStatus::InProgress || self.canonical_adopted() {
            return false;
        }
        self.unit.set_output_progress(completed, total);
        true
    }

    /// Commit this output successfully.
    pub fn set_complete(&self) {
        self.unit.set_complete();
    }

    /// Commit this output as failed.
    pub fn set_failed(&self) {
        self.unit.set_failed();
    }

    /// Clear one rejected provisional attempt before retrying on the same
    /// stable named-Brain output identity.
    pub(crate) fn reset_provisional_output(&self) -> bool {
        if self.canonical_adopted() || self.status() != MessageStatus::InProgress {
            return false;
        }
        self.unit.set_program_output();
        self.unit.set_response("");
        true
    }

    /// Adopt matching canonical output exactly once without removing the local
    /// message. This is the atomic local-to-canonical handoff.
    pub fn adopt_canonical_output(&self, output: &str) -> CanonicalAdoption {
        let mut canonical = self
            .canonical_output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(adopted) = canonical.as_deref() {
            return if adopted == output {
                CanonicalAdoption::AlreadyAdopted
            } else {
                CanonicalAdoption::Conflict
            };
        }
        let current = self.content();
        if !current.is_empty() && current != output {
            return CanonicalAdoption::Conflict;
        }
        if current.is_empty() {
            self.unit.set_response(output);
        }
        *canonical = Some(output.to_string());
        self.unit.set_transient_status(None);
        self.unit.set_complete();
        CanonicalAdoption::Adopted
    }

    /// Whether a canonical event has finalized this output.
    pub fn canonical_adopted(&self) -> bool {
        self.canonical_output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some()
    }
}

impl Default for ProgramOutputMessage {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! delegate_message {
    ($type:ty) => {
        impl Message for $type {
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

            fn kind(&self) -> MessageKind {
                self.unit.kind()
            }

            fn children(&self) -> Vec<MessageRef> {
                self.unit.children()
            }

            fn disclosure(&self, colors: &ColorScheme) -> Option<MessageDisclosure> {
                self.unit.disclosure(colors)
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
    };
}

delegate_message!(ProgramSourceMessage);
delegate_message!(ProgramOutputMessage);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_program_source_rejects_late_mutation_and_conflicting_canonical_event() {
        let source = ProgramSourceMessage::new("lisp");
        assert!(source.replace_source("(say \"first\")"));
        source.set_complete();

        assert!(!source.replace_source("(say \"late\")"));
        assert_eq!(
            source.adopt_canonical_source("(say \"first\")"),
            CanonicalAdoption::Adopted
        );
        assert_eq!(
            source.adopt_canonical_source("(say \"first\")"),
            CanonicalAdoption::AlreadyAdopted
        );
        assert_eq!(
            source.adopt_canonical_source("(say \"conflict\")"),
            CanonicalAdoption::Conflict
        );
        assert_eq!(source.content(), "(say \"first\")");
    }

    #[test]
    fn canonical_output_is_adopted_exactly_once_and_rejects_late_mutation() {
        let output = ProgramOutputMessage::new();
        assert!(output.append_output("answer"));
        output.set_complete();

        assert!(!output.append_output(" late"));
        assert_eq!(
            output.adopt_canonical_output("answer"),
            CanonicalAdoption::Adopted
        );
        assert_eq!(
            output.adopt_canonical_output("answer"),
            CanonicalAdoption::AlreadyAdopted
        );
        assert_eq!(
            output.adopt_canonical_output("different"),
            CanonicalAdoption::Conflict
        );
        assert_eq!(output.content(), "answer");
    }

    #[test]
    fn provisional_named_brain_output_retries_without_changing_identity() {
        let id = MessageId::new();
        let output = ProgramOutputMessage::with_id(id);
        assert!(output.append_output("rejected"));
        assert!(output.set_output_handle("VM program rejected"));

        assert!(output.reset_provisional_output());
        assert_eq!(output.id(), id);
        assert!(output.content().is_empty());
        assert!(output.append_output("repaired"));
        assert_eq!(output.content(), "repaired");
    }
}
