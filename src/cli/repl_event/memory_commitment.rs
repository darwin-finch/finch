//! Send-safe plumbing for the committed (byte-stable) memory-recall set
//! (#940).
//!
//! `process_query_with_tools` runs under `tokio::spawn` (`Send` required),
//! while `AttachedBrainClient`'s local transport is confined to the
//! frontend's `LocalSet` thread -- the same constraint `src/tools/todo.rs`
//! solves for `TodoWrite`. This module mirrors that split exactly:
//!
//! * [`MemoryCommitmentWriter`] is the `Send`, cloneable handle a query task
//!   holds to request a replacement of the committed set.
//! * [`MemoryCommitmentTarget`] is the frontend-local (non-`Send`) selector
//!   for whichever Brain currently owns durable writes, set alongside
//!   `TodoJournalTarget` wherever a Brain is attached or detached.
//! * [`MemoryCommitmentReceiver`] is the `spawn_local` worker that owns the
//!   actual `AttachedBrainClient` push.
//!
//! Unlike the task-list journal, the local mirror
//! (`Arc<RwLock<Vec<CommittedMemoryRecord>>>`) updates even when no Brain is
//! attached: the committed set's job is turn-to-turn byte stability in the
//! request prefix (the `SummaryCache` invariant shape), which a standalone
//! session still benefits from even though nothing durable backs it. When a
//! Brain *is* attached, the same replacement is also journaled, which is
//! what lets the set survive `finch attach` / a daemon restart.

pub use crate::brain::CommittedMemoryRecord;
use anyhow::Result;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};

struct MemoryCommitmentRequest {
    memories: Vec<CommittedMemoryRecord>,
    reply: oneshot::Sender<Result<bool>>,
}

/// Send-safe handle a query task holds to request a committed-set
/// replacement. Cheap to clone; every clone shares one worker.
#[derive(Clone)]
pub struct MemoryCommitmentWriter {
    tx: mpsc::UnboundedSender<MemoryCommitmentRequest>,
}

impl MemoryCommitmentWriter {
    /// Replace the committed memory set. Returns `true` when a Brain was
    /// attached and the replacement was durably journaled; `false` means
    /// this is a standalone session -- the local mirror still updated (read
    /// it via the `Arc<RwLock<_>>` returned by [`memory_commitment_journal`]),
    /// but nothing durable changed.
    pub async fn replace(&self, memories: Vec<CommittedMemoryRecord>) -> Result<bool> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(MemoryCommitmentRequest { memories, reply })
            .map_err(|_| anyhow::anyhow!("Brain memory-commitment journal worker stopped"))?;
        response.await.map_err(|_| {
            anyhow::anyhow!("Brain memory-commitment journal worker dropped its response")
        })?
    }
}

/// Frontend-local selector for the Brain that owns the committed memory
/// set's durable writes. Set alongside `TodoJournalTarget` wherever a Brain
/// is attached or detached.
#[derive(Clone)]
pub struct MemoryCommitmentTarget {
    selected: Rc<RefCell<Option<crate::brain::AttachedBrainClient>>>,
}

impl MemoryCommitmentTarget {
    pub fn set(&self, selected: Option<crate::brain::AttachedBrainClient>) {
        *self.selected.borrow_mut() = selected;
    }

    /// True when a Brain client will receive the next replacement.
    #[cfg(test)]
    pub fn is_bound(&self) -> bool {
        self.selected.borrow().is_some()
    }
}

pub struct MemoryCommitmentReceiver {
    rx: mpsc::UnboundedReceiver<MemoryCommitmentRequest>,
    selected: Rc<RefCell<Option<crate::brain::AttachedBrainClient>>>,
    mirror: Arc<RwLock<Vec<CommittedMemoryRecord>>>,
}

impl MemoryCommitmentReceiver {
    /// Start the non-`Send` Cap'n Proto worker after the REPL enters its
    /// `LocalSet`.
    pub fn spawn(mut self) {
        let worker_target = Rc::clone(&self.selected);
        tokio::task::spawn_local(async move {
            while let Some(request) = self.rx.recv().await {
                let target = worker_target.borrow().clone();
                let result = match target {
                    Some(target) => {
                        let memories = request.memories;
                        match target
                            .push(crate::brain::BrainEventKind::CommittedMemoriesReplaced {
                                memories: memories.clone(),
                            })
                            .await
                        {
                            Ok(()) => {
                                *self.mirror.write().await = memories;
                                Ok(true)
                            }
                            Err(error) => Err(error),
                        }
                    }
                    // No Brain attached: still update the mirror so a
                    // standalone session keeps turn-to-turn byte stability,
                    // just without durability.
                    None => {
                        *self.mirror.write().await = request.memories;
                        Ok(false)
                    }
                };
                let _ = request.reply.send(result);
            }
        });
    }
}

/// Everything `process_query_with_tools` needs each turn to read and update
/// the committed memory set: the local mirror (read synchronously to render
/// the byte-stable block), the writer (to request a replacement when the
/// turn's decision changes the set), and the staleness clock. Bundled into
/// one value so a per-turn query task takes one extra parameter instead of
/// three.
///
/// `stale_counts` is deliberately not part of the durable
/// `CommittedMemoryRecord`/mirror: it is `LlmLoop`'s own process-local scratch
/// state (constructed fresh in `LlmLoop::new`, not threaded through
/// `memory_commitment_journal`), so it never needs installing alongside a
/// Brain target the way the writer/mirror do. A restart resets each
/// committed memory's decay clock rather than guessing at an elapsed-turn
/// count it never observed -- an accepted, conservative trade (#940).
#[derive(Clone)]
pub struct MemoryCommitmentHandle {
    pub mirror: Arc<RwLock<Vec<CommittedMemoryRecord>>>,
    pub writer: MemoryCommitmentWriter,
    pub stale_counts: Arc<RwLock<std::collections::HashMap<u64, u32>>>,
}

impl MemoryCommitmentHandle {
    /// A handle with no Brain ever attached and no live worker consuming
    /// its writer. Only for callers that need a value to pass but will
    /// never exercise it (e.g. a test harness for an unrelated code path);
    /// `writer.replace(..)` on this handle always errors because nothing
    /// is listening.
    #[cfg(test)]
    pub fn inert() -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            mirror: Arc::new(RwLock::new(Vec::new())),
            writer: MemoryCommitmentWriter { tx },
            stale_counts: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// A handle pre-seeded with `memories` as the current committed set, and
    /// no live worker (a push from this handle silently fails in the
    /// background, same as `inert()`). For tests that only need to observe
    /// how an already-committed set renders and persists turn-to-turn, not
    /// the push mechanism itself (covered separately by this module's own
    /// tests and the `BrainStore` restart tests).
    #[cfg(test)]
    pub fn with_committed(memories: Vec<CommittedMemoryRecord>) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            mirror: Arc::new(RwLock::new(memories)),
            writer: MemoryCommitmentWriter { tx },
            stale_counts: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }
}

/// Build the writer/target/receiver trio plus the shared local mirror the
/// query processor reads synchronously each turn.
pub fn memory_commitment_journal(
    mirror: Arc<RwLock<Vec<CommittedMemoryRecord>>>,
) -> (
    MemoryCommitmentWriter,
    MemoryCommitmentTarget,
    MemoryCommitmentReceiver,
) {
    let (tx, rx) = mpsc::unbounded_channel::<MemoryCommitmentRequest>();
    let selected = Rc::new(RefCell::new(None::<crate::brain::AttachedBrainClient>));
    (
        MemoryCommitmentWriter { tx },
        MemoryCommitmentTarget {
            selected: Rc::clone(&selected),
        },
        MemoryCommitmentReceiver {
            rx,
            selected,
            mirror,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(node_id: u64) -> CommittedMemoryRecord {
        CommittedMemoryRecord {
            node_id,
            text: format!("memory {node_id}"),
            score: 0.5,
        }
    }

    #[test]
    fn journal_worker_starts_only_inside_the_frontend_local_set() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let mirror = Arc::new(RwLock::new(Vec::new()));
            let (writer, _target, receiver) = memory_commitment_journal(Arc::clone(&mirror));
            receiver.spawn();
            let durable = writer.replace(vec![memory(1)]).await.unwrap();
            assert!(
                !durable,
                "no Brain attached, so the replacement must report non-durable"
            );
            assert_eq!(
                mirror.read().await.as_slice(),
                &[memory(1)],
                "the local mirror must still update without a Brain attached, \
                 so a standalone session keeps turn-to-turn byte stability"
            );
        }));
    }

    #[test]
    fn target_is_unbound_until_set() {
        let mirror = Arc::new(RwLock::new(Vec::new()));
        let (_writer, target, _receiver) = memory_commitment_journal(mirror);
        assert!(!target.is_bound());
    }
}
