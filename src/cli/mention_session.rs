//! Application adapter between project-context policy and the TUI mention port.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::cli::tui::{MentionAttachment, MentionCandidate, MentionPort, MentionSubmission};
use crate::context::{
    format_attachment_document, mention_query_at, parse_visible_mentions, snapshots_for_prompt,
    MentionCatalog, MentionError, MentionSnapshot,
};

#[derive(Default)]
struct MentionState {
    pending: Vec<MentionSnapshot>,
    prepared: Option<Vec<MentionSnapshot>>,
}

/// One interactive session's project catalog and selection-time snapshots.
pub(crate) struct MentionSession {
    catalog: MentionCatalog,
    state: Mutex<MentionState>,
}

impl MentionSession {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Arc<Self> {
        Arc::new(Self {
            catalog: MentionCatalog::new(root),
            state: Mutex::new(MentionState::default()),
        })
    }

    fn state(&self) -> MutexGuard<'_, MentionState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl MentionPort for MentionSession {
    fn query_at(&self, text: &str, cursor_chars: usize) -> Option<(usize, String)> {
        mention_query_at(text, cursor_chars)
    }

    fn candidates(&self, query: &str) -> Vec<MentionCandidate> {
        self.catalog
            .candidates(query)
            .into_iter()
            .map(|candidate| MentionCandidate {
                relative_path: candidate.relative_path.clone(),
                speakable_row: candidate.speakable_row(),
                insert_token: candidate.insert_token(),
            })
            .collect()
    }

    fn select(&self, relative_path: &str) -> Result<(), String> {
        match self.catalog.resolve_path(relative_path) {
            Ok(snapshot) => {
                let mut state = self.state();
                state
                    .pending
                    .retain(|existing| existing.relative_path != snapshot.relative_path);
                state.pending.push(snapshot);
                Ok(())
            }
            Err(error) => Err(error.speakable()),
        }
    }

    fn retain_visible(&self, input: &str) {
        let visible = parse_visible_mentions(input)
            .into_iter()
            .map(|parsed| parsed.relative_path)
            .collect::<std::collections::HashSet<_>>();
        self.state()
            .pending
            .retain(|snapshot| visible.contains(&snapshot.relative_path));
    }

    fn prepare_submission(&self, input: &str) -> Result<MentionSubmission, String> {
        let prior = {
            let mut state = self.state();
            debug_assert!(state.prepared.is_none());
            std::mem::take(&mut state.pending)
        };
        match snapshots_for_prompt(&self.catalog, input, &prior) {
            Ok(snapshots) => {
                let attachment_document = if snapshots.is_empty() {
                    None
                } else {
                    let bodies = snapshots
                        .iter()
                        .map(MentionSnapshot::as_body)
                        .collect::<Vec<_>>();
                    Some(format_attachment_document(&bodies))
                };
                let attachments = snapshots
                    .iter()
                    .map(|snapshot| MentionAttachment {
                        path: snapshot.relative_path.clone(),
                        kind: snapshot.kind.as_str().to_string(),
                        sha256: snapshot.sha256.clone(),
                        byte_len: snapshot.byte_len,
                        truncated: snapshot.truncated,
                        truncation_note: snapshot.truncation_note.clone(),
                        content: snapshot.content.clone(),
                    })
                    .collect();
                self.state().prepared = Some(snapshots);
                Ok(MentionSubmission {
                    attachment_document,
                    attachments,
                })
            }
            Err(errors) => {
                self.state().pending = prior;
                Err(speakable_errors(&errors))
            }
        }
    }

    fn commit_submission(&self) {
        self.state().prepared = None;
    }

    fn restore_submission(&self) {
        let mut state = self.state();
        if let Some(prepared) = state.prepared.take() {
            state.pending.extend(prepared);
        }
    }
}

fn speakable_errors(errors: &[MentionError]) -> String {
    errors
        .iter()
        .map(MentionError::speakable)
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_keeps_original_bytes_after_the_file_changes() {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(project.path().join("src")).unwrap();
        let path = project.path().join("src/selected.rs");
        std::fs::write(&path, "fn selected() {}\n").unwrap();
        let session = MentionSession::new(project.path());

        let candidate = session
            .candidates("selected")
            .into_iter()
            .next()
            .expect("selected file is listed");
        session.select(&candidate.relative_path).unwrap();
        std::fs::write(&path, "CHANGED ON DISK").unwrap();

        let submission = session
            .prepare_submission(&format!("explain {}", candidate.insert_token))
            .unwrap();
        let document = submission
            .attachment_document
            .expect("selected mention produces an attachment document");
        assert!(document.contains("fn selected() {}"));
        assert!(!document.contains("CHANGED ON DISK"));
    }

    #[test]
    fn failed_submit_restores_selection_for_a_corrected_prompt() {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::write(project.path().join("selected.rs"), "kept bytes\n").unwrap();
        let session = MentionSession::new(project.path());
        session.select("selected.rs").unwrap();

        let error = session
            .prepare_submission("@selected.rs @missing.rs")
            .expect_err("missing mention must reject the turn");
        assert!(error.contains("missing.rs"));

        std::fs::write(project.path().join("selected.rs"), "changed bytes\n").unwrap();
        let submission = session.prepare_submission("@selected.rs").unwrap();
        let document = submission
            .attachment_document
            .expect("selected mention produces an attachment document");
        assert!(document.contains("kept bytes"));
        assert!(!document.contains("changed bytes"));
    }

    #[test]
    fn rejected_checkpoint_restores_prepared_selection() {
        let project = tempfile::TempDir::new().unwrap();
        let path = project.path().join("selected.rs");
        std::fs::write(&path, "selection-time bytes\n").unwrap();
        let session = MentionSession::new(project.path());
        session.select("selected.rs").unwrap();

        session.prepare_submission("@selected.rs").unwrap();
        session.restore_submission();
        std::fs::write(&path, "changed after rejection\n").unwrap();

        let retried = session.prepare_submission("@selected.rs").unwrap();
        let document = retried
            .attachment_document
            .expect("restored mention produces an attachment document");
        assert!(document.contains("selection-time bytes"));
        assert!(!document.contains("changed after rejection"));
    }
}
