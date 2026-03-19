/// Daemon-held named session registry.
///
/// The daemon keeps one `SessionRegistry` wrapped in `Arc<Mutex<...>>`.
/// Any client that knows the session name can join the same broadcast channel
/// and see every message published to that session.
///
/// Session lifecycle:
///   1. First peer calls `get_or_create("quiet-hill")` → channel created, peer_count = 1.
///   2. Second peer calls `get_or_create("quiet-hill")` → same channel, peer_count = 2.
///   3. Either peer calls `remove_if_empty(id)` when leaving — entry removed only when
///      peer_count reaches zero.

use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::broadcast;
use uuid::Uuid;

use crate::session::SessionEvent;

/// Broadcast channel capacity: 256 events.
/// Bursty enough for active sessions; excess senders block until consumers drain.
const BCAST_CAPACITY: usize = 256;

/// One entry in the registry — one named session.
pub struct SessionEntry {
    pub name: String,
    pub id: Uuid,
    pub bcast_tx: broadcast::Sender<SessionEvent>,
    pub created_at: Instant,
    pub peer_count: usize,
}

/// Named session registry — held by the daemon inside `Arc<Mutex<SessionRegistry>>`.
pub struct SessionRegistry {
    sessions: HashMap<Uuid, SessionEntry>,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
        }
    }

    /// Look up a session by name, creating it if it doesn't exist yet.
    ///
    /// Increments `peer_count` each time a peer joins.
    /// Returns `(session_uuid, broadcast_sender)`.
    pub fn get_or_create(&mut self, name: &str) -> (Uuid, broadcast::Sender<SessionEvent>) {
        let id = crate::session::names::to_uuid(name);
        if let Some(entry) = self.sessions.get_mut(&id) {
            entry.peer_count += 1;
            return (id, entry.bcast_tx.clone());
        }
        let (tx, _) = broadcast::channel(BCAST_CAPACITY);
        let entry = SessionEntry {
            name: name.to_string(),
            id,
            bcast_tx: tx.clone(),
            created_at: Instant::now(),
            peer_count: 1,
        };
        self.sessions.insert(id, entry);
        (id, tx)
    }

    /// Join an existing session by name.
    ///
    /// Returns `None` if no session with that name exists.
    /// Increments `peer_count` on success.
    pub fn join(&mut self, name: &str) -> Option<broadcast::Sender<SessionEvent>> {
        let id = crate::session::names::to_uuid(name);
        self.sessions.get_mut(&id).map(|entry| {
            entry.peer_count += 1;
            entry.bcast_tx.clone()
        })
    }

    /// List active sessions as `(name, id, peer_count)` tuples.
    pub fn list(&self) -> Vec<(String, Uuid, usize)> {
        self.sessions
            .values()
            .map(|e| (e.name.clone(), e.id, e.peer_count))
            .collect()
    }

    /// Decrement peer count and remove the session entry if no peers remain.
    pub fn remove_if_empty(&mut self, id: Uuid) {
        if let Some(entry) = self.sessions.get_mut(&id) {
            entry.peer_count = entry.peer_count.saturating_sub(1);
            if entry.peer_count == 0 {
                self.sessions.remove(&id);
            }
        }
    }
}

impl Default for SessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_create_same_name_same_id() {
        let mut reg = SessionRegistry::new();
        let (id1, _) = reg.get_or_create("quiet-hill");
        let (id2, _) = reg.get_or_create("quiet-hill");
        assert_eq!(id1, id2);
    }

    #[test]
    fn peer_count_increments() {
        let mut reg = SessionRegistry::new();
        reg.get_or_create("quiet-hill");
        reg.get_or_create("quiet-hill");
        let list = reg.list();
        assert_eq!(list[0].2, 2);
    }

    #[test]
    fn remove_if_empty_cleans_up() {
        let mut reg = SessionRegistry::new();
        let (id, _) = reg.get_or_create("golden-path");
        reg.remove_if_empty(id);
        assert!(reg.list().is_empty());
    }

    #[test]
    fn remove_if_empty_does_not_remove_when_peers_remain() {
        let mut reg = SessionRegistry::new();
        let (id, _) = reg.get_or_create("golden-path");
        reg.get_or_create("golden-path"); // peer_count = 2
        reg.remove_if_empty(id);          // peer_count = 1 — still alive
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn broadcast_reaches_subscriber() {
        let mut reg = SessionRegistry::new();
        let (_, tx) = reg.get_or_create("amber-cove");
        let mut rx = tx.subscribe();
        tx.send(crate::session::SessionEvent::chat("hello")).unwrap();
        let ev = rx.try_recv().unwrap();
        assert!(matches!(ev, crate::session::SessionEvent::Chat { text } if text == "hello"));
    }
}
