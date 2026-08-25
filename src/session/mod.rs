//! Transitional review events used by the local diff-proposal UI.
//!
//! Collaboration messages and lifecycle state belong to the canonical Brain
//! event log. This module deliberately contains no second chat/session bus or
//! network transport.

pub mod diff_store;
pub mod names;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A local reviewed-diff event. These values are currently carried over an
/// in-process channel; they are not a parallel collaboration protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// A proposed unified diff against a named file, buffer, or change set.
    Diff {
        id: Uuid,
        label: String,
        patch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Replace the patch and optionally the description of a proposal.
    DiffEdit {
        diff_id: Uuid,
        patch: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// Accept a proposed diff for application.
    DiffAccept { diff_id: Uuid },
    /// Reject a proposed diff without applying it.
    DiffReject {
        diff_id: Uuid,
        reason: Option<String>,
    },
}

impl SessionEvent {
    pub fn diff(
        label: impl Into<String>,
        patch: impl Into<String>,
        description: Option<String>,
    ) -> (Self, Uuid) {
        let id = Uuid::new_v4();
        (
            Self::Diff {
                id,
                label: label.into(),
                patch: patch.into(),
                description,
            },
            id,
        )
    }

    pub fn diff_edit(diff_id: Uuid, patch: impl Into<String>, description: Option<String>) -> Self {
        Self::DiffEdit {
            diff_id,
            patch: patch.into(),
            description,
        }
    }

    pub fn diff_accept(diff_id: Uuid) -> Self {
        Self::DiffAccept { diff_id }
    }

    pub fn diff_reject(diff_id: Uuid, reason: Option<String>) -> Self {
        Self::DiffReject { diff_id, reason }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_event_json_round_trip_preserves_identity() {
        let (event, id) =
            SessionEvent::diff("workspace", "--- old\n+++ new", Some("review".into()));
        let encoded = serde_json::to_string(&event).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&encoded).unwrap();
        assert!(matches!(decoded, SessionEvent::Diff { id: decoded_id, .. } if decoded_id == id));
    }
}
