//! Daemon-owned Claude CLI Subscription session registry (issue #1354).
//!
//! Moves ownership of the `claude` subprocess and its MCP bridge socket from
//! the frontend into the daemon, keyed by Brain name, while leaving tool
//! execution and approval exactly where #1341/#1350 already put them: on
//! whichever frontend calls `BrainService.claudeCliRound`. This registry
//! reuses [`finch_providers::ClaudeCliProvider`] unchanged — the daemon
//! constructs and calls it directly instead of the frontend doing so. The
//! process-management logic (`spawn_running_turn`/`drive`/pause-resume) is
//! not reimplemented here or anywhere in the root crate; only *who calls it*
//! moved.
//!
//! **Lifecycle.** A session is created lazily on the first `claudeCliRound`
//! call for a Brain and kept alive, independent of any one frontend
//! connection, until [`ClaudeCliSessionRegistry::remove`] is called (Brain
//! archive/delete — see `src/server/handlers/lifecycle.rs` and
//! `src/main.rs`'s CLI archive path) or the daemon process itself exits.
//! Dropping the registry's entry drops the shared `ClaudeCliProvider`'s last
//! strong reference to any live `RunningTurn`/`ParkedTurn`; that type's own
//! `Drop` chain (`kill_on_drop` on the child, `SocketGuard` on the bridge
//! socket file) already performs the real cleanup — nothing here does that
//! directly. A daemon restart therefore terminates any live `claude` child
//! exactly like every other daemon-owned OS resource; only the underlying
//! `claude --resume <session-id>` conversation (recorded by the CLI's own
//! subscription-side session store, not by Finch) survives, and a later
//! round for the same Brain starts a fresh child that resumes it — any tool
//! call that was parked at the moment of the restart is lost and must be
//! re-requested by the model on the next round.
//!
//! One [`tokio::sync::Mutex`] per Brain serializes concurrent
//! `claudeCliRound` calls for the same Brain: a round is held for the exact
//! duration of one RPC call (spawn/resume through pause-or-completion), so
//! two frontends racing to drive the same Brain queue rather than each
//! spawning or resuming the process independently. Stage 1 (#1354) assumes
//! one frontend actively drives a Brain's session at a time; a second,
//! disconnected-and-reattached frontend queues behind whichever call is
//! in flight, but nothing here yet distinguishes "the right frontend to
//! answer a specific pending tool call" from "any frontend that calls next"
//! — see the issue for the deferred multi-frontend follow-up.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use finch_providers::ClaudeCliProvider;

/// One daemon-owned Claude CLI Subscription session per Brain name.
#[derive(Clone, Default)]
pub struct ClaudeCliSessionRegistry {
    sessions: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<ClaudeCliProvider>>>>>,
}

impl ClaudeCliSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The persistent session for `brain`, constructing one with `binary`
    /// and `model` if none exists yet. Both are only consulted on first
    /// construction: an already-live session keeps whatever binary/model it
    /// was created with, since switching either mid `claude --resume`
    /// conversation is not a supported operation.
    pub fn get_or_create(
        &self,
        brain: &str,
        binary: std::path::PathBuf,
        model: Option<String>,
    ) -> Arc<tokio::sync::Mutex<ClaudeCliProvider>> {
        let mut sessions = self
            .sessions
            .lock()
            .expect("claude cli session registry mutex poisoned");
        Arc::clone(sessions.entry(brain.to_string()).or_insert_with(|| {
            Arc::new(tokio::sync::Mutex::new(ClaudeCliProvider::with_binary(
                binary, model,
            )))
        }))
    }

    /// Drop this Brain's session, if any, returning whether one existed. The
    /// real cleanup (killing a live `claude` child, removing its bridge
    /// socket file) happens through the dropped `ClaudeCliProvider`'s own
    /// `Drop` chain once this was the last strong reference — never
    /// anything this function does directly.
    pub fn remove(&self, brain: &str) -> bool {
        let mut sessions = self
            .sessions
            .lock()
            .expect("claude cli session registry mutex poisoned");
        sessions.remove(brain).is_some()
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, brain: &str) -> bool {
        self.sessions
            .lock()
            .expect("claude cli session registry mutex poisoned")
            .contains_key(brain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_binary() -> std::path::PathBuf {
        std::path::PathBuf::from("claude")
    }

    #[test]
    fn get_or_create_reuses_the_same_session_for_the_same_brain() {
        let registry = ClaudeCliSessionRegistry::new();
        let first = registry.get_or_create("brain-a", fake_binary(), None);
        let second = registry.get_or_create("brain-a", fake_binary(), None);
        assert!(
            Arc::ptr_eq(&first, &second),
            "a second call for the same Brain must reuse the exact live session, not construct \
             a second `claude` process owner"
        );
    }

    #[test]
    fn get_or_create_gives_distinct_brains_distinct_sessions() {
        let registry = ClaudeCliSessionRegistry::new();
        let a = registry.get_or_create("brain-a", fake_binary(), None);
        let b = registry.get_or_create("brain-b", fake_binary(), None);
        assert!(
            !Arc::ptr_eq(&a, &b),
            "two different Brains must never share one daemon-owned claude process/session"
        );
    }

    #[test]
    fn remove_drops_the_session_so_a_later_round_starts_fresh() {
        let registry = ClaudeCliSessionRegistry::new();
        let first = registry.get_or_create("brain-a", fake_binary(), None);
        assert!(registry.contains("brain-a"));
        assert!(
            registry.remove("brain-a"),
            "remove must report that a live session existed for this Brain"
        );
        assert!(!registry.contains("brain-a"));
        let second = registry.get_or_create("brain-a", fake_binary(), None);
        assert!(
            !Arc::ptr_eq(&first, &second),
            "after remove, the next round for the same Brain name must get a brand-new session, \
             never the one that was just torn down (Brain archive/delete, issue #1354)"
        );
    }

    #[test]
    fn remove_on_an_unknown_brain_reports_nothing_existed() {
        let registry = ClaudeCliSessionRegistry::new();
        assert!(
            !registry.remove("never-created"),
            "removing a Brain with no live session must not be reported as a real teardown"
        );
    }
}
