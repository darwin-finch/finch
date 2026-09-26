//! Send-safe plumbing for the durably promoted (spliced-once) memory set
//! (#940).
//!
//! `process_query_with_tools` runs under `tokio::spawn` (`Send` required),
//! while `AttachedBrainClient`'s local transport is confined to the
//! frontend's `LocalSet` thread -- the same constraint `src/tools/todo.rs`
//! solves for `TodoWrite`. This module mirrors that split exactly:
//!
//! * [`MemoryCommitmentWriter`] is the `Send`, cloneable handle a query task
//!   holds to request a replacement of the promoted set.
//! * [`MemoryCommitmentTarget`] is the frontend-local (non-`Send`) selector
//!   for whichever Brain currently owns durable writes, set alongside
//!   `TodoJournalTarget` wherever a Brain is attached or detached.
//! * [`MemoryCommitmentReceiver`] is the `spawn_local` worker that owns the
//!   actual `AttachedBrainClient` push.
//!
//! Unlike the task-list journal, the local mirror
//! (`Arc<RwLock<Vec<CommittedMemoryRecord>>>`) updates even when no Brain is
//! attached: a standalone session still tracks which exchanges have been
//! promoted into real conversation history, even though nothing durable
//! backs that record. When a Brain *is* attached, the same replacement is
//! also journaled, which is what lets the record survive `finch attach` / a
//! daemon restart. The record itself no longer drives a per-turn
//! re-rendering (`query_processor.rs`'s `process_query_with_tools` splices a
//! promoted exchange into `ConversationHistory` exactly once instead) -- it
//! is now an audit trail of what has been promoted this session, and a dedup
//! key so the same exchange is not pushed as "newly promoted" twice.

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
/// the promoted (spliced-once) memory set: the local mirror (read
/// synchronously to fold in this turn's newly promoted exchanges) and the
/// writer (to request a durable replacement when the set grows). Bundled
/// into one value so a per-turn query task takes one extra parameter instead
/// of two.
///
/// There is no staleness clock any more (#940 follow-up): a promoted
/// exchange is spliced into real conversation history rather than
/// re-rendered every turn, so nothing here needs a decay policy -- once
/// promoted, an entry is a splice candidate for the rest of the session, and
/// `conversation_compactor.rs`'s summarization is what bounds actual request
/// size.
#[derive(Clone)]
pub struct MemoryCommitmentHandle {
    pub mirror: Arc<RwLock<Vec<CommittedMemoryRecord>>>,
    pub writer: MemoryCommitmentWriter,
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
        }
    }

    /// A handle pre-seeded with `memories` as the current promoted set, and
    /// no live worker (a push from this handle silently fails in the
    /// background, same as `inert()`). For tests that only need to observe
    /// how an already-promoted set behaves, not the push mechanism itself
    /// (covered separately by this module's own tests and the `BrainStore`
    /// restart tests).
    #[cfg(test)]
    pub fn with_committed(memories: Vec<CommittedMemoryRecord>) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Self {
            mirror: Arc::new(RwLock::new(memories)),
            writer: MemoryCommitmentWriter { tx },
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
