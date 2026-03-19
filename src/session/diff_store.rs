//! In-memory store of pending diff proposals in the room.
//!
//! Peers propose diffs via `SessionEvent::Diff`; humans argue back in chat;
//! the AI revises via `SessionEvent::DiffEdit`.  When the human is satisfied
//! they run `/accept [prefix]` or `/reject [reason]`.

use std::collections::HashMap;
use uuid::Uuid;

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffState {
    Pending,
    Accepted,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct PendingDiff {
    pub id: Uuid,
    /// Display name — file path, buffer name, or free label.
    pub label: String,
    /// Current unified diff text.
    pub patch: String,
    /// Optional prose description of what this diff does.
    pub description: Option<String>,
    /// Name of the peer who proposed this diff.
    pub proposed_by: String,
    pub state: DiffState,
}

// ── DiffStore ─────────────────────────────────────────────────────────────────

/// Ordered store of diff proposals, most recently proposed last.
pub struct DiffStore {
    /// Insertion-ordered map of id → diff.
    diffs: HashMap<Uuid, PendingDiff>,
    /// Insertion order (so we can iterate in order and find the most recent).
    order: Vec<Uuid>,
}

impl DiffStore {
    pub fn new() -> Self {
        Self {
            diffs: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Record a new proposed diff.
    pub fn propose(
        &mut self,
        id: Uuid,
        label: String,
        patch: String,
        description: Option<String>,
        proposed_by: String,
    ) {
        self.order.push(id);
        self.diffs.insert(
            id,
            PendingDiff {
                id,
                label,
                patch,
                description,
                proposed_by,
                state: DiffState::Pending,
            },
        );
    }

    /// Replace the patch (and optionally description) of an existing proposal.
    /// Returns `false` if the diff is not found.
    pub fn edit(&mut self, diff_id: Uuid, patch: String, description: Option<String>) -> bool {
        if let Some(d) = self.diffs.get_mut(&diff_id) {
            d.patch = patch;
            if description.is_some() {
                d.description = description;
            }
            true
        } else {
            false
        }
    }

    /// Mark a diff as accepted.  Returns a reference to it, or `None` if not found.
    pub fn accept(&mut self, diff_id: Uuid) -> Option<&PendingDiff> {
        if let Some(d) = self.diffs.get_mut(&diff_id) {
            d.state = DiffState::Accepted;
            Some(&*d)
        } else {
            None
        }
    }

    /// Mark a diff as rejected.  Returns `false` if not found.
    pub fn reject(&mut self, diff_id: Uuid, _reason: Option<String>) -> bool {
        if let Some(d) = self.diffs.get_mut(&diff_id) {
            d.state = DiffState::Rejected;
            true
        } else {
            false
        }
    }

    pub fn get(&self, diff_id: Uuid) -> Option<&PendingDiff> {
        self.diffs.get(&diff_id)
    }

    /// Iterate over all diffs that are still `Pending`, in insertion order.
    pub fn pending(&self) -> impl Iterator<Item = &PendingDiff> {
        self.order
            .iter()
            .filter_map(|id| self.diffs.get(id))
            .filter(|d| d.state == DiffState::Pending)
    }

    /// Find the most recent pending diff, or the first pending diff whose id
    /// starts with `prefix` (if provided).
    pub fn resolve_pending(&self, prefix: Option<&str>) -> Option<&PendingDiff> {
        match prefix {
            None => {
                // Most recent pending diff
                self.order
                    .iter()
                    .rev()
                    .filter_map(|id| self.diffs.get(id))
                    .find(|d| d.state == DiffState::Pending)
            }
            Some(p) => {
                // First pending diff whose id starts with the given prefix
                self.order
                    .iter()
                    .filter_map(|id| self.diffs.get(id))
                    .find(|d| d.state == DiffState::Pending && d.id.to_string().starts_with(p))
            }
        }
    }
}

impl Default for DiffStore {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store_with_one() -> (DiffStore, Uuid) {
        let mut store = DiffStore::new();
        let id = Uuid::new_v4();
        store.propose(
            id,
            "src/foo.rs".to_string(),
            "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            Some("fix the thing".to_string()),
            "peer-abc".to_string(),
        );
        (store, id)
    }

    #[test]
    fn propose_and_get() {
        let (store, id) = make_store_with_one();
        let d = store.get(id).unwrap();
        assert_eq!(d.label, "src/foo.rs");
        assert_eq!(d.state, DiffState::Pending);
    }

    #[test]
    fn pending_iterator() {
        let (store, _) = make_store_with_one();
        assert_eq!(store.pending().count(), 1);
    }

    #[test]
    fn edit_replaces_patch() {
        let (mut store, id) = make_store_with_one();
        let ok = store.edit(id, "new-patch".to_string(), None);
        assert!(ok);
        assert_eq!(store.get(id).unwrap().patch, "new-patch");
    }

    #[test]
    fn accept_changes_state() {
        let (mut store, id) = make_store_with_one();
        let d = store.accept(id).unwrap();
        assert_eq!(d.state, DiffState::Accepted);
        assert_eq!(store.pending().count(), 0);
    }

    #[test]
    fn reject_changes_state() {
        let (mut store, id) = make_store_with_one();
        assert!(store.reject(id, Some("not right".to_string())));
        assert_eq!(store.get(id).unwrap().state, DiffState::Rejected);
    }

    #[test]
    fn resolve_pending_most_recent() {
        let mut store = DiffStore::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        store.propose(id1, "a.rs".into(), "p1".into(), None, "peer".into());
        store.propose(id2, "b.rs".into(), "p2".into(), None, "peer".into());
        let resolved = store.resolve_pending(None).unwrap();
        assert_eq!(resolved.id, id2); // most recent
    }

    #[test]
    fn resolve_pending_by_prefix() {
        let (store, id) = make_store_with_one();
        let prefix = &id.to_string()[..8];
        let resolved = store.resolve_pending(Some(prefix)).unwrap();
        assert_eq!(resolved.id, id);
    }
}
