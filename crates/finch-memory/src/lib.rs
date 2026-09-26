//! SQLite-backed MemTree storage and retrieval for Finch.
//!
//! The crate README traces REPL connection/embedding injection through
//! [`MemorySystem::open_connection`] and [`MemorySystem::new_with_connection`], and application
//! program-index composition through [`MemorySystem::index_program_record`]. Rustdoc renders
//! the method signatures without a generated interface catalog.

mod embeddings;
mod memory_status;
mod program_registry;
mod quality;
mod routing_memory;

pub use embeddings::{
    average_embeddings, cosine_similarity, EmbeddingEngine, HashedNgramEmbedding,
};
pub use memory_status::{caveat, count_qualifier, observed, Recall};
pub use program_registry::{ProgramIndexRecord, ProgramIndexRef};

/// A stable identity for one stored memory -- a `RoutingTree` point id
/// (`routing_memory.rs`'s own `PointId`) under its public name. Kept as a
/// plain `u64` alias (not re-exporting `routing_memory`'s own type) since
/// that module is crate-private; every public API that surfaces a memory's
/// identity (`RecalledMemory`, `MemorySearchResult`, `InspectedMemory`,
/// `query_with_sources`) uses this same type.
pub type NodeId = u64;
pub use quality::{MemoryClassifier, MemoryImportance};

use finch_routing_tree::{save_point, write_dirty_nodes_within};
use routing_memory::{LinkNextError, Occurrence, PointId, RoutingMemTree};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::Mutex;
use uuid::Uuid;

// Everything under `#[cfg(any(test, feature = "test-support"))]` from here
// down to `pause_in_projection_sweep` (and its call sites further below) is
// one seam: hydration completion/sweep test pauses that
// `crates/finch-runtime/src/tests.rs` drives to get a genuinely
// `Loading`/`Degraded` index. It is deliberately not plain `#[cfg(test)]` —
// see the `test-support` feature comment in Cargo.toml for why a bare
// `#[cfg(test)]` seam here would silently vanish from a dependent crate's own
// test build. Add any future cross-crate test seam the same way.
//
// A batch-hydration pause (freeze the loader after N nodes) lived here too
// until RoutingTree replaced MemTree. RoutingTree hydration is atomic --
// `hydrate_in_background` makes one `RoutingMemTree::load` call with no
// intermediate checkpoint -- so there is no batch boundary left to pause at;
// `MemorySystem::force_degraded_for_test` and a real, positive-duration seed
// are what `finch-runtime`'s integration tests use instead now.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
struct HydrationCompletionPause {
    reached: watch::Sender<bool>,
    release: watch::Receiver<bool>,
}

#[cfg(any(test, feature = "test-support"))]
impl HydrationCompletionPause {
    async fn after_completion(&self) {
        self.reached.send_replace(true);
        let mut release = self.release.clone();
        while !*release.borrow_and_update() {
            if release.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Holds the pending-projection sweep BETWEEN two rows of its backlog.
///
/// Distinct from `HydrationCompletionPause`, which sits at the end of
/// `hydrate_batches` — that is strictly *before* `hydrate_in_background` calls
/// the sweep at all, so a cancellation landing there cannot say anything about
/// the sweep. This one is inside the loop body, so a test that cancels here
/// cancels a sweep that has already committed `after_repaired` rows and is
/// about to start the next: the only place a partially-applied backlog could
/// exist.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
struct ProjectionSweepPause {
    /// Fires when the sweep is about to project the row at this zero-based
    /// index, i.e. once exactly this many rows have committed.
    after_repaired: usize,
    reached: watch::Sender<bool>,
    release: watch::Receiver<bool>,
}

#[cfg(any(test, feature = "test-support"))]
impl ProjectionSweepPause {
    async fn before_row(&self, repaired: usize) {
        if repaired != self.after_repaired {
            return;
        }
        self.reached.send_replace(true);
        let mut release = self.release.clone();
        while !*release.borrow_and_update() {
            if release.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
static HYDRATION_COMPLETION_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<HydrationCompletionPause>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(any(test, feature = "test-support"))]
static PROJECTION_SWEEP_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<ProjectionSweepPause>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(any(test, feature = "test-support"))]
struct HydrationCompletionPauseRegistration {
    path: PathBuf,
    pause: Arc<HydrationCompletionPause>,
}

#[cfg(any(test, feature = "test-support"))]
struct ProjectionSweepPauseRegistration {
    path: PathBuf,
    pause: Arc<ProjectionSweepPause>,
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for HydrationCompletionPauseRegistration {
    fn drop(&mut self) {
        let mut pauses = HYDRATION_COMPLETION_PAUSES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pauses
            .get(&self.path)
            .is_some_and(|registered| Arc::ptr_eq(registered, &self.pause))
        {
            pauses.remove(&self.path);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for ProjectionSweepPauseRegistration {
    fn drop(&mut self) {
        let mut pauses = PROJECTION_SWEEP_PAUSES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if pauses
            .get(&self.path)
            .is_some_and(|registered| Arc::ptr_eq(registered, &self.pause))
        {
            pauses.remove(&self.path);
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
fn register_hydration_completion_pause(
    path: PathBuf,
) -> (
    HydrationCompletionPauseRegistration,
    watch::Receiver<bool>,
    watch::Sender<bool>,
) {
    let (reached, reached_rx) = watch::channel(false);
    let (release, release_rx) = watch::channel(false);
    let pause = Arc::new(HydrationCompletionPause {
        reached,
        release: release_rx,
    });
    let mut pauses = HYDRATION_COMPLETION_PAUSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        pauses.insert(path.clone(), Arc::clone(&pause)).is_none(),
        "a hydration completion pause is already registered for {}",
        path.display()
    );
    drop(pauses);
    (
        HydrationCompletionPauseRegistration { path, pause },
        reached_rx,
        release,
    )
}

/// Hold the pending-projection sweep once `after_repaired` rows have committed,
/// so a test can cancel it with a backlog genuinely half-applied.
#[cfg(any(test, feature = "test-support"))]
fn register_projection_sweep_pause(
    path: PathBuf,
    after_repaired: usize,
) -> (
    ProjectionSweepPauseRegistration,
    watch::Receiver<bool>,
    watch::Sender<bool>,
) {
    let (reached, reached_rx) = watch::channel(false);
    let (release, release_rx) = watch::channel(false);
    let pause = Arc::new(ProjectionSweepPause {
        after_repaired,
        reached,
        release: release_rx,
    });
    let mut pauses = PROJECTION_SWEEP_PAUSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        pauses.insert(path.clone(), Arc::clone(&pause)).is_none(),
        "a projection sweep pause is already registered for {}",
        path.display()
    );
    drop(pauses);
    (
        ProjectionSweepPauseRegistration { path, pause },
        reached_rx,
        release,
    )
}

#[cfg(any(test, feature = "test-support"))]
fn take_hydration_completion_pause(
    path: &std::path::Path,
) -> Option<Arc<HydrationCompletionPause>> {
    HYDRATION_COMPLETION_PAUSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(path)
}

#[cfg(any(test, feature = "test-support"))]
fn take_projection_sweep_pause(path: &std::path::Path) -> Option<Arc<ProjectionSweepPause>> {
    PROJECTION_SWEEP_PAUSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(path)
}

/// Configuration for memory system
#[derive(Debug, Clone)]
pub struct MemoryConfig {
    /// Path to SQLite database
    pub db_path: PathBuf,
    /// Enable memory system
    pub enabled: bool,
    /// Maximum number of context items to retrieve
    pub max_context_items: usize,
    /// Checkpoint interval in seconds
    pub checkpoint_interval_secs: u64,
    /// Composition-root flag: prefer neural embeddings when the model is cached.
    /// `MemorySystem` constructors ignore this and use the injected engine.
    pub use_neural_embeddings: bool,
    /// Directory where the embedding model is cached / downloaded.
    /// Composition reads this; memory does not download or load models.
    pub embedding_cache_dir: PathBuf,
    /// Minimum weighted score (`cosine_similarity * importance_boost`,
    /// boost in {1.0, 1.2, 1.4} -- see `MemTree::retrieve`) a recall result
    /// must clear to be returned at all, independent of the `top_k` count
    /// cap. Below this, a result is dropped rather than injected.
    ///
    /// Default `0.15` is deliberately conservative: it only excludes a
    /// result that is weak even at the highest importance boost (cosine
    /// similarity below ~0.107 at boost 1.4, ~0.15 at boost 1.0), so only
    /// clearly-irrelevant matches are filtered. Revisit once retrieval
    /// quality work (#415) lands and scores become more trustworthy.
    ///
    /// This one number applies to whichever `EmbeddingEngine` the
    /// composition root injected -- the sparse hashed-n-gram fallback and a dense
    /// neural engine do not necessarily produce comparable cosine-similarity
    /// distributions for unrelated text, so a threshold reasoned about
    /// algebraically here has not been validated against either engine's
    /// actual score distribution. Treat `0.15` as a starting point to
    /// recalibrate per engine once real distributions are measured, not as
    /// an empirically-derived constant.
    pub min_relevance_score: f32,
    /// Turn-level injection gate (#1134): when `Some(t)`, a recall turn
    /// whose *best* retrieved weighted score (`cosine_similarity *
    /// importance_boost` -- the same quantity `min_relevance_score` is
    /// applied to) does not reach `t` injects nothing at all.
    /// `query_with_sources` returns an empty set before the per-result
    /// floor is applied.
    ///
    /// The per-result floor answers "is this one result strong enough to
    /// inject"; nearest-neighbour retrieval always returns `k` candidates,
    /// so that floor alone cannot keep a turn that needs no memory context
    /// from receiving `k` weak-but-individually-qualifying matches. This
    /// knob answers the turn-level question: if even the best memory
    /// retrieval holds for this query is weak, nothing about the turn is
    /// memory-relevant.
    ///
    /// Defaults to `None` (gate off): recall behaves exactly as it did
    /// before this knob existed. When set, the value only changes behavior
    /// if it is strictly greater than `min_relevance_score` -- a turn floor
    /// at or below the per-result floor can never skip a turn the
    /// per-result floor would not have emptied anyway.
    ///
    /// Like `min_relevance_score`, this is an uncalibrated starting point,
    /// not an empirically-derived constant, and is equally unvalidated
    /// against either engine's score distribution. Two disclosed
    /// consequences of enabling it: a skipped turn hands the committed
    /// recall set in the query processor an empty recall set, which counts
    /// toward `stale_after_turns` aging; and every skip is logged (`info`)
    /// with the turn floor, the best score, and the candidate count, so the
    /// decision is visible in the trace.
    pub min_turn_relevance_score: Option<f32>,
    /// Maximum number of memories the committed (Brain-persisted, byte-
    /// stable) recall set may hold at once. A newly-qualifying memory
    /// above this cap must out-score the current lowest-scoring committed
    /// entry to join, evicting it.
    pub max_committed_memories: usize,
    /// A committed memory is dropped once it goes this many consecutive
    /// turns without reappearing in that turn's fresh recall. Deliberately
    /// generous: premature eviction defeats the point of a stable,
    /// cacheable prefix, so this biases toward keeping a memory committed.
    pub stale_after_turns: u32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        Self {
            db_path: home.join(".finch").join("memory.db"),
            enabled: true,
            max_context_items: 5,
            checkpoint_interval_secs: 300, // 5 minutes
            use_neural_embeddings: true,
            embedding_cache_dir: home.join(".finch").join("embeddings"),
            min_relevance_score: 0.15,
            min_turn_relevance_score: None,
            max_committed_memories: 8,
            stale_after_turns: 20,
        }
    }
}

/// The turn-level injection decision for one recall turn (#1134).
///
/// Produced by [`turn_injection_decision`] from the configured turn floor
/// and this turn's retrieved scores, consumed and logged by
/// `query_with_sources` before the per-result floor is applied. The numbers
/// are retained so the log line discloses exactly what the decision was
/// made from: the tested decision and the emitted diagnostics cannot drift
/// apart.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TurnInjectionDecision {
    /// The configured turn floor; `None` when the gate is disabled.
    pub(crate) turn_floor: Option<f32>,
    /// The highest weighted score among this turn's retrieved candidates;
    /// `None` when retrieval returned nothing.
    pub(crate) best_score: Option<f32>,
}

impl TurnInjectionDecision {
    /// Whether this turn injects nothing: the gate is configured and even
    /// the best retrieved score falls strictly below it. An empty retrieval
    /// has nothing to gate, and a disabled gate never skips.
    pub(crate) fn skips(&self) -> bool {
        match (self.turn_floor, self.best_score) {
            (Some(turn_floor), Some(best_score)) => best_score < turn_floor,
            _ => false,
        }
    }
}

/// Decide whether anything at all should be injected for this turn, before
/// the per-result relevance floor is applied (#1134). `scores` are the
/// weighted scores of this turn's retrieved candidates in any order.
fn turn_injection_decision(turn_floor: Option<f32>, scores: &[f32]) -> TurnInjectionDecision {
    TurnInjectionDecision {
        turn_floor,
        best_score: scores.iter().copied().reduce(f32::max),
    }
}

/// Progress of the background MemTree hydration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HydrationStatus {
    /// Every persisted node is in memory.
    Ready { nodes: usize },
    /// Still loading. Retrieval sees `loaded` of `total` nodes.
    Loading { loaded: usize, total: usize },
    /// Hydration stopped early, but what loaded is coherent: children are
    /// linked, so the index is incomplete rather than wrong.
    ///
    /// Distinct from `Failed` because the consequences differ. A batch that
    /// could not be read leaves a smaller tree, which is the same situation as
    /// a read taken mid-hydration — reads already serve it, and a write placed
    /// into it is no worse than a write during hydration. Refusing every write
    /// for the rest of the process over one unreadable row was a far larger
    /// punishment than the defect warranted (#288).
    Degraded {
        loaded: usize,
        total: usize,
        reason: String,
    },
    /// Hydration ended without finishing and the tree may be structurally
    /// incomplete — children unlinked, so `MemTree::insert` cannot place
    /// anything safely. Writes are refused.
    Failed { reason: String },
}

/// Shared progress record for the background load.
#[derive(Debug)]
struct HydrationState {
    loaded: AtomicUsize,
    total: AtomicUsize,
    /// Completion as retained state rather than an edge.
    ///
    /// This was an `AtomicBool` plus a `Notify`, which has a lost wakeup:
    /// `Notified` captures the epoch when the future is *created*, and
    /// `notify_waiters` stores no permit, so a waiter that checks the flag,
    /// loses the race to `complete()`, and only then builds its future parks
    /// forever. `complete()` fires exactly once, so the turn wedges with no
    /// timeout and no error. Production runs a multi-threaded runtime, so that
    /// interleaving is reachable. A `watch` channel retains the value, which
    /// removes the window rather than narrowing it.
    done: watch::Sender<bool>,
    failure: std::sync::Mutex<Option<Failure>>,
    #[cfg(any(test, feature = "test-support"))]
    completion_pause: std::sync::Mutex<Option<Arc<HydrationCompletionPause>>>,
    /// Carried here rather than on `ProjectionContext` only because this is
    /// where the other two seams already live and `ProjectionContext` holds
    /// this `Arc`; the sweep is not part of hydration.
    #[cfg(any(test, feature = "test-support"))]
    sweep_pause: std::sync::Mutex<Option<Arc<ProjectionSweepPause>>>,
}

/// Opens the write gate if the loader ends without finishing.
///
/// Moved into the spawned future, so it is dropped however that future ends: a
/// panic unwinding through it, an `abort()`, or a runtime shutting down before
/// the task is ever polled. Without it those three cases left `done` false
/// forever — and because the `watch::Sender` lives inside the
/// `Arc<HydrationState>` that `MemorySystem` holds, it is never dropped either,
/// so `changed()` does not even return `Err`. Every subsequent write waited on
/// a completion that could not arrive, with no timeout and no error.
///
/// A guard rather than awaiting the `JoinHandle` in a supervisor task, because
/// a supervisor is one more thing that can itself be dropped. This cannot be
/// forgotten: it is owned by the future whose ending it reports.
struct HydrationGuard(Arc<HydrationState>);

impl Drop for HydrationGuard {
    fn drop(&mut self) {
        if !*self.0.done.borrow() {
            self.0.fail(
                "the MemTree loader ended without finishing: it panicked, was \
                 aborted, or its runtime shut down before it could run"
                    .to_string(),
            );
        }
    }
}

impl HydrationState {
    fn new(total: usize) -> Self {
        Self {
            loaded: AtomicUsize::new(0),
            total: AtomicUsize::new(total),
            done: watch::channel(total == 0).0,
            failure: std::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            completion_pause: std::sync::Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            sweep_pause: std::sync::Mutex::new(None),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn install_completion_pause(&self, pause: Option<Arc<HydrationCompletionPause>>) {
        *self
            .completion_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn pause_after_completion(&self) {
        let pause = self
            .completion_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause.after_completion().await;
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn install_sweep_pause(&self, pause: Option<Arc<ProjectionSweepPause>>) {
        *self
            .sweep_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    #[cfg(any(test, feature = "test-support"))]
    async fn pause_in_projection_sweep(&self, repaired: usize) {
        let pause = self
            .sweep_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause.before_row(repaired).await;
        }
    }

    fn complete(&self) {
        // `send_replace`, not `send`. `watch::Sender::send` returns `Err`
        // WITHOUT storing the value when the receiver count is zero, and the
        // count is zero here: `watch::channel` returns a `Receiver` that is
        // dropped immediately, and `ensure_hydrated` only subscribes on demand.
        // With `send` and a discarded error, completion silently never latched
        // unless a waiter happened to be parked already — so the first write
        // after hydration finished hung forever, which is the default sequence
        // rather than a race. `send_replace` stores the value regardless of
        // receivers.
        self.done.send_replace(true);
    }

    /// Record a failure that leaves the tree unusable for placement.
    fn fail(&self, reason: String) {
        self.record(Failure::Broken(reason));
    }

    /// Record a failure that stopped the load early but left what loaded
    /// coherent — the caller has linked children.
    fn degrade(&self, reason: String) {
        self.record(Failure::Degraded(reason));
    }

    fn record(&self, failure: Failure) {
        // `unwrap_or_else(PoisonError::into_inner)`, not a silent skip. The
        // critical section is one `Option` assignment, so poisoning is remote —
        // but swallowing it meant `complete()` still ran with no reason
        // recorded, `status` then reported `Ready` over a partial tree, and the
        // write the gate exists to stop went through. Liveness code has to fail
        // toward refusing.
        let mut slot = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = Some(failure);
        drop(slot);
        self.complete();
    }

    /// Forget a recorded failure.
    ///
    /// Without this a single unreadable row refused every memory write for the
    /// life of the process, and a restart re-read the same row and failed the
    /// same way — so it was permanent in practice, not merely for the session
    /// (#288). A store that has been reloaded successfully is working again and
    /// must be able to say so.
    fn clear_failure(&self, nodes: usize) {
        let mut slot = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *slot = None;
        drop(slot);
        // The counters describe the aborted loader, not the tree that replaced
        // it, so clearing only the reason would leave `status` reporting
        // `Ready { nodes: 0 }` over a fully restored store. Nothing reads it
        // outside this module yet (#275); it would be wrong the moment
        // something did.
        self.loaded.store(nodes, Ordering::SeqCst);
        self.total.store(nodes, Ordering::SeqCst);
    }

    fn status(&self) -> HydrationStatus {
        let recorded = self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let loaded = self.loaded.load(Ordering::SeqCst);
        match recorded {
            Some(Failure::Broken(reason)) => return HydrationStatus::Failed { reason },
            Some(Failure::Degraded(reason)) => {
                return HydrationStatus::Degraded {
                    loaded,
                    total: self.total.load(Ordering::SeqCst),
                    reason,
                }
            }
            None => {}
        }
        if *self.done.borrow() {
            HydrationStatus::Ready { nodes: loaded }
        } else {
            HydrationStatus::Loading {
                loaded,
                total: self.total.load(Ordering::SeqCst),
            }
        }
    }
}

/// How a hydration ended badly.
///
/// Two kinds, because they warrant different answers. See `HydrationStatus`.
#[derive(Debug, Clone)]
enum Failure {
    /// Stopped early with what loaded left coherent.
    Degraded(String),
    /// Ended without finishing; the tree may be unusable for placement.
    Broken(String),
}

/// Memory system with MemTree and SQLite storage
pub struct MemorySystem {
    db: Arc<Mutex<Connection>>,
    tree: Arc<Mutex<RoutingMemTree>>,
    /// Serialize conversation projection through semantic indexing. This keeps
    /// the SQLite identity, in-memory leaf, and durable leaf provenance from
    /// racing when the daemon retries one completed Brain run.
    insert_lock: Arc<Mutex<()>>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: MemoryConfig,
    hydration: Arc<HydrationState>,
    /// Whether a pending-projection sweep is still owed.
    ///
    /// Shared with the background loader, which runs the sweep the moment
    /// hydration completes — before `MemorySystem` itself exists. See
    /// `ProjectionContext`.
    needs_projection_sweep: Arc<AtomicBool>,
    /// Owned so the loader does not outlive the store it is loading.
    ///
    /// A discarded handle left the task decoding the whole database into a tree
    /// nobody would read, holding its connection and contending for the mutex
    /// with whatever opened the store next.
    hydration_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for MemorySystem {
    fn drop(&mut self) {
        if let Some(task) = self.hydration_task.take() {
            task.abort();
        }
    }
}

/// Everything the pending-projection sweep needs, without a `MemorySystem`.
///
/// The background loader is spawned from inside `new`, before `Self` is
/// constructed, so the sweep it runs on completion cannot be a `&self` method.
/// These are the same `Arc`s the store then holds, so the loader's sweep and a
/// later interactive one contend on one `insert_lock` and mutate one tree.
#[derive(Clone)]
struct ProjectionContext {
    db: Arc<Mutex<Connection>>,
    tree: Arc<Mutex<RoutingMemTree>>,
    hydration: Arc<HydrationState>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    insert_lock: Arc<Mutex<()>>,
    needs_projection_sweep: Arc<AtomicBool>,
}

/// A durably stored conversation with no `memory_sources` row: stored, never
/// projected. See `PENDING_PROJECTION_SQL`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingConversation {
    id: String,
    role: String,
    content: String,
    timestamp: i64,
    /// The session this turn belongs to, if any -- carried through so
    /// projection can resolve the occurrence chain's `prev` (the session's
    /// most recently projected occurrence) via `last_occurrence_uuid_for_session`.
    /// `None` for a turn stored with no session identity, which simply never
    /// gets a `prev`: there is nothing to chain it from.
    session_id: Option<String>,
}

/// A new occurrence's point content, bundled so [`MemorySystem::save_routing_occurrence`] stays
/// within clippy's default argument-count limit rather than taking each field loose.
struct NewOccurrenceContent {
    key_content: String,
    embedding: Vec<f32>,
    importance: u8,
    created_at: i64,
}

/// The pending-projection predicate.
///
/// A conversation is pending exactly when it has no `memory_sources` row at
/// all. That is unambiguous only because a classifier-discarded turn is
/// recorded with `node_id = NULL` rather than with no row (see `schema.sql`),
/// so "no row" means "projection never ran", never "projection ran and
/// declined". Any change that stops writing the NULL row silently converts
/// every discarded turn into a permanently pending one that this sweep would
/// re-classify on every startup.
///
/// Ordered by `timestamp`, so a backlog is replayed in the order the turns were
/// actually spoken; `MemTree` placement depends on what is already in the tree,
/// so an arbitrary order would place a backlog differently on every run.
///
/// `?1` excludes one conversation id, and NULL excludes nothing — `IS NOT`
/// rather than `<>` precisely so a NULL parameter keeps every row. The write
/// path binds the turn it is about to project.
///
/// What that earns is error attribution, not de-duplication. Double projection
/// is already impossible without it: an ordinary write mints a fresh UUID that
/// is not in `conversations` yet, so it cannot be in this set at all, and a
/// named-Brain retry whose row a sweep had just repaired would take the
/// `already_classified` short-circuit rather than project a second time.
///
/// The difference is what the caller is told when that projection FAILS.
/// `sweep_pending_projections_if_owed` deliberately swallows a sweep error into
/// a `tracing::warn!`, because a live turn must not be refused over an
/// unrelated older row. So without the exclusion, a named-Brain retry whose own
/// row failed to project *inside the sweep* would return `Ok(false)` — read as
/// "identical retry, nothing to do" — for a turn that was in fact never
/// indexed. With the exclusion the retry projects its own row and the failure
/// propagates as `Err`.
///
/// An earlier version of this comment claimed omitting `?1` would "silently
/// change the count reported at `event_loop.rs:5376`". That was wrong. The
/// count is summed there, but no consumer reads the number: one call site in
/// `server/handlers.rs` inspects only the `Err` arm, and the other counts runs
/// rather than rows. The boolean is a contract on a `pub` method, not a
/// quantity anything acts on today.
const PENDING_PROJECTION_SQL: &str = "SELECT c.id, c.role, c.content, c.timestamp, c.session_id
     FROM conversations c
     LEFT JOIN memory_sources ms ON ms.conversation_id = c.id
     WHERE ms.conversation_id IS NULL AND c.id IS NOT ?1
     ORDER BY c.timestamp ASC, c.id ASC";

/// Canonical source identity for a conversation pair projected from one
/// successful named-Brain run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainConversationProvenance {
    pub brain_id: String,
    pub run_id: String,
    pub request_seq: u64,
}

/// Stable metadata for the canonical conversation row behind one semantic
/// memory. The full content is returned only by explicit inspection.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MemorySourceMetadata {
    pub source_id: String,
    pub role: String,
    pub model: Option<String>,
    pub session_id: Option<String>,
    pub brain_id: Option<String>,
    pub run_id: Option<String>,
    pub request_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MemorySearchResult {
    /// Pass this exact value to `inspect_memory`.
    pub memory_id: String,
    pub node_id: NodeId,
    pub text: String,
    pub score: f32,
    pub source: Option<MemorySourceMetadata>,
}

/// One rendered, attributed recall result with its identity and weighted
/// score retained, so a caller can track it across turns (e.g. to decide
/// whether it should join a committed memory set).
#[derive(Debug, Clone, PartialEq)]
pub struct RecalledMemory {
    pub node_id: NodeId,
    /// Rendered, attributed text -- the same string `query()` returns.
    pub text: String,
    /// Weighted score (`cosine_similarity * importance_boost`) at recall
    /// time; already cleared `MemoryConfig::min_relevance_score`.
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InspectedMemory {
    pub memory_id: String,
    pub node_id: Option<NodeId>,
    pub source: Option<MemorySourceMetadata>,
    pub content: String,
}

fn optional_request_seq(value: Option<i64>) -> Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value)
                .with_context(|| format!("stored request sequence {value} is negative"))
        })
        .transpose()
}

fn source_metadata_for_node(
    conn: &Connection,
    node_id: NodeId,
) -> Result<Option<MemorySourceMetadata>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.role, c.model, c.session_id,
                c.brain_id, c.run_id, c.request_seq
         FROM memory_sources ms
         JOIN conversations c ON c.id = ms.conversation_id
         WHERE ms.node_id = ?1
         -- `node_id` is no longer unique: several conversations may share one
         -- deduplicated memory. Without an explicit order the row returned
         -- depends on the query plan, so the same memory could report a
         -- different origin after a VACUUM or an index change. Report the
         -- earliest occurrence, which is the conversation that first
         -- established the memory.
         ORDER BY ms.indexed_at ASC, ms.conversation_id ASC
         LIMIT 1",
    )?;
    let mut rows = stmt.query([node_id as i64])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(MemorySourceMetadata {
        source_id: row.get(0)?,
        role: row.get(1)?,
        model: row.get(2)?,
        session_id: row.get(3)?,
        brain_id: row.get(4)?,
        run_id: row.get(5)?,
        request_seq: optional_request_seq(row.get(6)?)?,
    }))
}

/// One stored turn with everything recall rendering needs beyond the public
/// source metadata: the raw content and the ordering timestamp.
#[derive(Debug)]
struct RecallTurn {
    source: MemorySourceMetadata,
    content: String,
    timestamp: i64,
}

fn recall_turn_from_row(row: &rusqlite::Row<'_>) -> Result<RecallTurn> {
    Ok(RecallTurn {
        source: MemorySourceMetadata {
            source_id: row.get(0)?,
            role: row.get(1)?,
            model: row.get(2)?,
            session_id: row.get(3)?,
            brain_id: row.get(4)?,
            run_id: row.get(5)?,
            request_seq: optional_request_seq(row.get(6)?)?,
        },
        content: row.get(7)?,
        timestamp: row.get(8)?,
    })
}

fn recall_turn_by_id(conn: &Connection, conversation_id: &str) -> Result<Option<RecallTurn>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.role, c.model, c.session_id,
                c.brain_id, c.run_id, c.request_seq, c.content, c.timestamp
         FROM conversations c
         WHERE c.id = ?1",
    )?;
    let mut rows = stmt.query([conversation_id])?;
    match rows.next()? {
        None => Ok(None),
        Some(row) => Ok(Some(recall_turn_from_row(row)?)),
    }
}

/// The stored turn this one replies to, or that replies to this one.
///
/// Resolved primarily through the retrieved turn's own occurrence-chain link
/// (`routing_occurrences`, PR #1213): a user turn's true reply is whatever
/// occurrence comes right after it in its session's chain (`next`), and an
/// assistant turn's true question is whatever occurrence comes right before
/// it (`prev`). This is a real request/response link, not a heuristic, so it
/// cannot be fooled by some other same-session turn of the opposite role
/// landing closer in wall-clock time than the actual reply.
///
/// Falls back to the previous nearest-timestamp heuristic
/// ([`counterpart_turn_by_nearest_timestamp`]) when the retrieved turn has no
/// occurrence row at all (a legacy leaf predating occurrence chains, or a
/// classifier-discarded turn), or when its occurrence exists but has no
/// `next`/`prev` set yet (the chain's own documented "first/last turn" case,
/// already handled the same way by `RoutingMemTree::retrieve`'s neighbor-
/// context tie-break -- see `crates/finch-memory/AGENTS.md`).
fn counterpart_turn(conn: &Connection, turn: &RecallTurn) -> Result<Option<RecallTurn>> {
    if turn.source.role != "user" && turn.source.role != "assistant" {
        return Ok(None);
    }
    if let Some(counterpart) = counterpart_turn_via_occurrence_chain(conn, turn)? {
        return Ok(Some(counterpart));
    }
    counterpart_turn_by_nearest_timestamp(conn, turn)
}

/// The real reply/question this turn's occurrence chain names, or `None` when
/// there is nothing to walk (see [`counterpart_turn`]'s doc for the exact
/// fallback cases).
fn counterpart_turn_via_occurrence_chain(
    conn: &Connection,
    turn: &RecallTurn,
) -> Result<Option<RecallTurn>> {
    let occurrence: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT ro.prev_uuid, ro.next_uuid
             FROM memory_sources ms
             JOIN routing_occurrences ro ON ro.point_id = ms.node_id
             WHERE ms.conversation_id = ?1",
            params![turn.source.source_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context(
            "counterpart_turn_via_occurrence_chain: query routing_occurrences by conversation",
        )?;
    let Some((prev_uuid, next_uuid)) = occurrence else {
        return Ok(None);
    };
    // A user turn's reply is whatever comes after it in the chain; an assistant turn's
    // question is whatever came before it.
    let neighbor_uuid = if turn.source.role == "user" {
        next_uuid
    } else {
        prev_uuid
    };
    let Some(neighbor_uuid) = neighbor_uuid else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT c.id, c.role, c.model, c.session_id,
                c.brain_id, c.run_id, c.request_seq, c.content, c.timestamp
         FROM routing_occurrences ro
         JOIN memory_sources ms ON ms.node_id = ro.point_id
         JOIN conversations c ON c.id = ms.conversation_id
         WHERE ro.uuid = ?1",
    )?;
    let mut rows = stmt.query(params![neighbor_uuid])?;
    match rows.next()? {
        None => Ok(None),
        Some(row) => Ok(Some(recall_turn_from_row(row)?)),
    }
}

/// The pre-#1213 pairing heuristic, kept only as [`counterpart_turn`]'s fallback for turns with
/// no occurrence-chain link to walk (see that function's doc).
///
/// Scoped to the session and to the two conversation roles: `session_id` is
/// what every captured turn carries, and a NULL session cannot bound "the
/// exchange" against interleaved other sessions, so such turns stay single.
/// Closest in time wins, which pairs adjacent turns and survives a follow-up
/// exchange after the first -- but, unlike the occurrence chain, can mis-pair
/// with any other same-session opposite-role turn that happens to land closer
/// in wall-clock time than the real reply.
fn counterpart_turn_by_nearest_timestamp(
    conn: &Connection,
    turn: &RecallTurn,
) -> Result<Option<RecallTurn>> {
    let Some(session_id) = turn.source.session_id.as_deref() else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT c.id, c.role, c.model, c.session_id,
                c.brain_id, c.run_id, c.request_seq, c.content, c.timestamp
         FROM conversations c
         WHERE c.session_id = ?1
           AND c.role IN ('user', 'assistant')
           AND c.role != ?2
           AND c.id != ?3
         ORDER BY ABS(c.timestamp - ?4) ASC, c.id ASC
         LIMIT 1",
    )?;
    let mut rows = stmt.query(params![
        session_id,
        turn.source.role,
        turn.source.source_id,
        turn.timestamp
    ])?;
    match rows.next()? {
        None => Ok(None),
        Some(row) => Ok(Some(recall_turn_from_row(row)?)),
    }
}

/// Render one recalled memory the way a prompt consumes it: labelled with who
/// said it, and joined with the turn it replies to.
///
/// The exchange is rendered with the user's half first regardless of which
/// half was retrieved, so both halves collapse to one entry under the
/// rendered-text dedup in `query`.
fn render_recall_entry(primary: &RecallTurn, counterpart: Option<&RecallTurn>) -> String {
    let Some(other) = counterpart else {
        return format!("{}: {}", primary.source.role, primary.content);
    };
    if primary.source.role == "user" {
        format!("user: {}\nassistant: {}", primary.content, other.content)
    } else {
        format!("user: {}\nassistant: {}", other.content, primary.content)
    }
}

impl MemorySystem {
    /// Create a new memory system with the hashed-n-gram fallback engine.
    ///
    /// Model selection and download belong to the composition root. Inject a
    /// neural engine with [`Self::new_with_engine`].
    pub fn new(config: MemoryConfig) -> Result<Self> {
        Self::new_with_engine(config, Arc::new(HashedNgramEmbedding::new()))
    }

    /// Open (or create) `db_path` with WAL mode enabled.
    ///
    /// The shared open sequence behind [`Self::new_with_engine`]: create the
    /// parent directory if needed, open the file, enable WAL. Exposed so a
    /// composition root that wants to call [`Self::new_with_connection`]
    /// directly -- to actually exercise the injected path, rather than
    /// going through the path-based wrapper -- does not have to duplicate
    /// this sequence and risk it drifting (e.g. a future pragma change
    /// applied to one copy and not the other).
    pub fn open_connection(db_path: &std::path::Path) -> Result<Connection> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database: {}", db_path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        // WAL still serializes writers to one at a time: without a busy timeout, the second of
        // two concurrent writers (e.g. two `RoutingMemTree::link_next` callers racing against the
        // same db file) gets an immediate `SQLITE_BUSY` instead of waiting for the first writer's
        // transaction to finish. A few seconds is enough for a normal single-row UPDATE/INSERT to
        // clear without making a genuinely stuck writer hang the caller indefinitely.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(conn)
    }

    /// Create a memory system that embeds with the caller-supplied engine.
    ///
    /// The engine's dimension parameterizes the MemTree. Constructors never
    /// probe the HuggingFace cache or download a model.
    ///
    /// Opens `config.db_path` itself; a caller that already holds (or wants
    /// to control the lifetime of) the SQLite connection should call
    /// [`Self::new_with_connection`] instead.
    pub fn new_with_engine(
        config: MemoryConfig,
        embedding_engine: Arc<dyn EmbeddingEngine>,
    ) -> Result<Self> {
        let conn = Self::open_connection(&config.db_path)?;
        Self::new_with_connection(Arc::new(Mutex::new(conn)), config, embedding_engine)
    }

    /// Create a memory system against an already-open connection.
    ///
    /// This is the injectable primitive: it never calls [`Connection::open`]
    /// itself, so the caller controls how and where the SQLite connection is
    /// opened (a real file, an in-memory database for a test, or a connection
    /// whose lifetime the composition root already manages). It applies the
    /// same idempotent `schema.sql` initialization and migrations as
    /// [`Self::new_with_engine`] against whatever connection it is handed,
    /// then hydrates the `MemTree` from it.
    ///
    /// `db` must be uncontended when this is called -- the constructor holds
    /// its lock throughout schema initialization and the synchronous
    /// hydration path. A caller sharing the same `Arc<Mutex<Connection>>`
    /// with something else that might be holding the lock at this moment
    /// gets a clean `Err`, not a panic.
    pub fn new_with_connection(
        db: Arc<Mutex<Connection>>,
        config: MemoryConfig,
        embedding_engine: Arc<dyn EmbeddingEngine>,
    ) -> Result<Self> {
        let (node_count, max_node_id) = {
            let conn = db.try_lock().context(
                "new_with_connection requires the injected connection to be \
                 uncontended on entry",
            )?;

            // `db` and `config` are independent parameters here -- unlike
            // before this constructor took an injected connection, when both
            // always came from one `Connection::open(&config.db_path)` call.
            // A caller is free to pass a `db` that does not back
            // `config.db_path` at all (the regression test below does
            // exactly that). Ask the connection itself what file it has
            // open rather than assuming `config.db_path` is it, so the
            // remediation text below never names the wrong file. `path()`
            // is `None` for `:memory:` or an already-closed connection, in
            // which case there is no file to move aside at all.
            let db_file = conn.path().map(str::to_string);

            // Refuse a database created before `memory_sources.node_id UNIQUE` was
            // dropped. There is deliberately no migration — Finch has no users and
            // `schema.sql` is authoritative — but `CREATE TABLE IF NOT EXISTS`
            // silently leaves an old table in place, and the first repeated memory
            // then fails with `UNIQUE constraint failed: memory_sources.node_id`
            // from deep inside an insert. Fail at open, naming the remedy.
            {
                let stale: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM sqlite_master
                     WHERE type='table' AND name='memory_sources'
                       AND sql LIKE '%UNIQUE%'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);
                match (&db_file, stale == 0) {
                    (_, true) => {}
                    (Some(path), false) => anyhow::bail!(
                        "{path} predates the current memory schema and cannot be \
                         upgraded in place. Storing the same content twice would \
                         fail with a UNIQUE constraint error.\n\n\
                         Move it aside and Finch will create a fresh store, \
                         keeping the old one readable with any SQLite client:\n\
                         \x20\x20mv {path} {path}.pre-schema-change\n\n\
                         Do not delete it unless you are certain the history is \
                         not wanted — there is no export path yet.",
                    ),
                    (None, false) => anyhow::bail!(
                        "the injected connection predates the current memory \
                         schema and cannot be upgraded in place. Storing the \
                         same content twice would fail with a UNIQUE constraint \
                         error. This connection reports no backing file \
                         (in-memory or already closed), so there is nothing for \
                         Finch to move aside automatically -- the caller must \
                         supply a connection against a fresh database."
                    ),
                }
            }

            // A stale tree_nodes table (MemTree's storage, predating RoutingTree) may still be
            // present on disk from before RoutingTree replaced it outright. schema.sql no longer
            // declares that table, so it is never recreated; drop it here so it doesn't linger
            // as dead state in an existing database.
            conn.execute_batch("DROP TABLE IF EXISTS tree_nodes;")?;

            // Load schema (CREATE TABLE IF NOT EXISTS — safe to re-run)
            let schema = include_str!("schema.sql");
            conn.execute_batch(schema)?;

            // Migration C: executable vocabulary gained explicit effect declarations.
            // Unknown legacy definitions remain conservative and require approval.
            let _ = conn.execute(
            "ALTER TABLE program_registry ADD COLUMN effect TEXT NOT NULL DEFAULT 'unclassified'",
            [],
        );

            // Correlate projected memory with the authoritative Brain run. Existing
            // local history remains valid with NULL provenance.
            let _ = conn.execute("ALTER TABLE conversations ADD COLUMN brain_id TEXT", []);
            let _ = conn.execute("ALTER TABLE conversations ADD COLUMN run_id TEXT", []);
            let _ = conn.execute(
                "ALTER TABLE conversations ADD COLUMN request_seq INTEGER",
                [],
            );
            conn.execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_conversations_brain_run_role
             ON conversations(brain_id, run_id, role)
             WHERE brain_id IS NOT NULL AND run_id IS NOT NULL;",
            )?;

            tracing::debug!(
                "Memory system initialized: {}",
                db_file.as_deref().unwrap_or("<in-memory or unknown>")
            );

            // Hydrate the RoutingTree from `routing_points`/`routing_nodes`/`routing_leaf_membership`.
            //
            // Doing this synchronously blocked startup behind decoding every stored
            // embedding: on the dogfood host 16,782 nodes at 2048 f32 is 131 MiB,
            // and the frontend took 3.25 s to first prompt with about 308 MiB
            // resident (#242). When a Tokio runtime is available the load runs in
            // the background in bounded batches, so the prompt paints immediately
            // and memory fills in behind it.
            //
            // `next_id` is advanced past the highest stored id BEFORE any batch
            // lands. Otherwise a turn stored during hydration could be given an id
            // that a later batch then overwrites.
            //
            // These two propagate rather than `unwrap_or(0)`. A failed `COUNT`
            // silently skipped hydration entirely and reported `Ready { nodes: 0 }`
            // against a full store, with no log line at all; a failed `MAX` left
            // `next_id` at 1, so the first write upserted over persisted node 1.
            // Refusing to open the store is the honest outcome.
            let node_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM routing_points", [], |row| row.get(0))
                .context("Failed to count stored routing points")?;
            (node_count, 0i64)
        };

        // Parameterize RoutingTree dimension to match the injected engine. Unlike MemTree, there
        // is no separate next-id counter to advance past the highest stored id: a freshly loaded
        // RoutingTree's own point count (one Vec entry per `routing_points` row, tombstoned or
        // not) already determines the next assigned id, by construction.
        let dim = embedding_engine.dimension();
        let tree = RoutingMemTree::new_with_dim(dim);

        let hydration = Arc::new(HydrationState::new(node_count.max(0) as usize));
        #[cfg(any(test, feature = "test-support"))]
        {
            hydration.install_completion_pause(take_hydration_completion_pause(&config.db_path));
            hydration.install_sweep_pause(take_projection_sweep_pause(&config.db_path));
        }
        let tree = Arc::new(Mutex::new(tree));
        // Built before the loader is spawned, because the loader owns half of
        // it: the sweep it runs on completion has to serialize against
        // interactive writes on the SAME `insert_lock`, and clear the SAME
        // flag, as the store this function is about to return.
        let insert_lock = Arc::new(Mutex::new(()));
        let needs_projection_sweep = Arc::new(AtomicBool::new(true));
        let projection = ProjectionContext {
            db: Arc::clone(&db),
            tree: Arc::clone(&tree),
            hydration: Arc::clone(&hydration),
            embedding_engine: Arc::clone(&embedding_engine),
            insert_lock: Arc::clone(&insert_lock),
            needs_projection_sweep: Arc::clone(&needs_projection_sweep),
        };
        let mut hydration_task = None;

        if node_count > 0 {
            // Spawn ONLY on a multi-threaded runtime.
            //
            // `Handle::try_current()` succeeds on a current-thread runtime too,
            // and spawning there is what made `mem-store` deadlock:
            // `block_on_host` blocks the single scheduler thread waiting for a
            // write, the write waits for the loader, and the loader cannot be
            // polled because the thread it needs is the one blocking. Bounding
            // that wait does not help either — the blocked `Runtime::block_on`
            // owns the time driver, so a `tokio::time::timeout` around it never
            // fires. An earlier version of this change claimed the bound
            // covered this case; it does not, and the hang reproduces in over
            // 200 seconds.
            //
            // A current-thread runtime therefore loads synchronously, exactly
            // as a caller with no runtime does. There is no task to starve.
            // `try_lock` rather than `blocking_lock`, which panics inside an
            // async context: nothing else holds these `Arc`s yet, so it cannot
            // contend.
            let flavor = tokio::runtime::Handle::try_current()
                .map(|handle| handle.runtime_flavor())
                .ok();
            match flavor {
                Some(tokio::runtime::RuntimeFlavor::MultiThread) => {
                    let handle = tokio::runtime::Handle::current();
                    let state = Arc::clone(&hydration);
                    let projection = projection.clone();
                    // Constructed HERE, outside the async block, and moved in.
                    //
                    // Building it inside the block means it does not exist
                    // until the future is first polled — so a task aborted or
                    // dropped before it ever runs never creates the guard and
                    // never drops it, which is precisely the "never scheduled"
                    // case this is for. Capturing it makes it part of the
                    // future from the moment the future exists.
                    let guard = HydrationGuard(Arc::clone(&state));
                    hydration_task = Some(handle.spawn(async move {
                        let _guard = guard;
                        Self::hydrate_in_background(projection).await;
                    }));
                }
                _ => {
                    // A current-thread runtime, or no runtime at all.
                    let mut guard = tree
                        .try_lock()
                        .expect("a newly constructed RoutingMemTree cannot be contended");
                    let conn = db.try_lock().context(
                        "new_with_connection requires the injected connection to be \
                         uncontended during synchronous hydration",
                    )?;
                    match RoutingMemTree::load(&conn, dim) {
                        Err(error) => {
                            // Broken, not fresh. This arm is only entered when
                            // `node_count > 0`, so reaching the error means
                            // there ARE rows and none of them could be read —
                            // refusing writes rather than placing them against
                            // an empty index. A reload later clears the
                            // failure (#288 separates a partial index from an
                            // unusable one).
                            tracing::warn!(
                                %error,
                                "Failed to load RoutingTree; refusing writes rather than \
                                 placing them against an empty index"
                            );
                            hydration.fail(error.to_string());
                        }
                        Ok(loaded) => {
                            hydration
                                .loaded
                                .store(loaded.size(), std::sync::atomic::Ordering::SeqCst);
                            *guard = loaded;
                            hydration.complete();
                        }
                    }
                }
            }
        }

        Ok(Self {
            db,
            tree,
            hydration,
            needs_projection_sweep,
            hydration_task,
            insert_lock,
            embedding_engine,
            config,
        })
    }

    /// Insert a conversation turn into memory
    pub async fn insert_conversation(
        &self,
        role: &str,
        content: &str,
        model: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<()> {
        self.insert_conversation_record(role, content, model, session_id, None)
            .await
            .map(|_| ())
    }

    /// Insert one side of a successful named-Brain turn exactly once.
    /// Identical retries are no-ops; conflicting identity reuse is rejected.
    pub async fn insert_brain_conversation(
        &self,
        role: &str,
        content: &str,
        model: Option<&str>,
        session_id: Option<&str>,
        provenance: &BrainConversationProvenance,
    ) -> Result<bool> {
        self.insert_conversation_record(role, content, model, session_id, Some(provenance))
            .await
    }

    async fn insert_conversation_record(
        &self,
        role: &str,
        content: &str,
        model: Option<&str>,
        session_id: Option<&str>,
        provenance: Option<&BrainConversationProvenance>,
    ) -> Result<bool> {
        let _insert_guard = self.insert_lock.lock().await;
        let timestamp = chrono::Utc::now()
            .timestamp_nanos_opt()
            .ok_or_else(|| anyhow::anyhow!("Timestamp out of range"))?;
        let id = provenance.map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |source| {
                format!(
                    "brain:{}:run:{}:role:{role}",
                    source.brain_id, source.run_id
                )
            },
        );
        let brain_id = provenance.map(|source| source.brain_id.as_str());
        let run_id = provenance.map(|source| source.run_id.as_str());
        let request_seq = provenance
            .map(|source| i64::try_from(source.request_seq))
            .transpose()
            .context("Brain request sequence exceeds SQLite INTEGER range")?;

        // Repair the backlog BEFORE this turn's own `INSERT INTO
        // conversations`, and exclude this turn from it.
        //
        // Before the insert, because after it this turn is itself pending: a
        // sweep sitting below the `already_classified` query would project it
        // and then the tail of this function would project it again.
        //
        // Excluded, because a named-Brain retry of a turn whose first
        // projection failed already HAS its `conversations` row, so it is in
        // the pending set. Letting the sweep repair it changes nothing when the
        // repair succeeds and hides the failure when it does not: the sweep
        // logs its error and returns, and this function would then hit the
        // `already_classified` short-circuit and report `Ok(false)` for a turn
        // that was never indexed. See `PENDING_PROJECTION_SQL`.
        //
        // Here as well as in the loader because the current-thread arm of `new`
        // completes hydration synchronously and has no task to hang the sweep
        // on; the flag keeps this to a single scan per process rather than one
        // per turn. It never blocks a turn: a sweep is attempted only once
        // hydration has already settled `Ready`.
        Self::sweep_pending_projections_if_owed(&self.projection(), Some(id.as_str())).await;

        // Store in SQLite
        let inserted = {
            let conn = self.db.lock().await;
            let changed = conn.execute(
                "INSERT OR IGNORE INTO conversations
                 (id, timestamp, role, content, tokens, model, session_id, brain_id, run_id, request_seq, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    &id,
                    timestamp,
                    role,
                    content,
                    None::<i32>, // tokens (TODO: count)
                    model,
                    session_id,
                    brain_id,
                    run_id,
                    request_seq,
                    timestamp,
                ],
            )?;
            if changed == 0 {
                let existing: (String, String, Option<String>, Option<String>, Option<i64>) = conn
                    .query_row(
                        "SELECT role, content, brain_id, run_id, request_seq
                         FROM conversations WHERE id = ?1",
                        [&id],
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )?;
                anyhow::ensure!(
                    existing
                        == (
                            role.to_string(),
                            content.to_string(),
                            brain_id.map(str::to_owned),
                            run_id.map(str::to_owned),
                            request_seq,
                        ),
                    "named-Brain memory identity {id} was reused with conflicting content"
                );
            }
            changed != 0
        };

        let already_classified = {
            let conn = self.db.lock().await;
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_sources WHERE conversation_id = ?1)",
                [&id],
                |row| row.get::<_, bool>(0),
            )?
        };
        if already_classified {
            return Ok(inserted);
        }

        // The gate guards semantic placement, and only that.
        //
        // A memory placed against a half-loaded tree lands in the wrong part of
        // the structure, and that placement is persisted — so the tree insert
        // below has to wait. But it used to sit at the top of this function,
        // ahead of the `INSERT INTO conversations` above, so a refused write
        // dropped the raw turn as well as its index entry. That is worse than
        // the misplacement it was preventing: raw history is the thing a user
        // cannot reconstruct.
        //
        // Refusing here leaves the conversation row written and no
        // `memory_sources` row. That preserves the irreplaceable raw turn for
        // explicit inspection; the row is then *pending projection*, and
        // `sweep_pending_projections` repairs it the next time a hydration
        // completes cleanly (#339).
        self.ensure_hydrated().await?;

        Self::project_stored_conversation(
            &self.projection(),
            &PendingConversation {
                id: id.clone(),
                role: role.to_string(),
                content: content.to_string(),
                timestamp,
                session_id: session_id.map(str::to_owned),
            },
        )
        .await?;

        tracing::debug!("Inserted conversation into memory: {} chars", content.len());

        Ok(true)
    }

    /// The `Arc`s the pending-projection sweep needs, as the loader holds them.
    fn projection(&self) -> ProjectionContext {
        ProjectionContext {
            db: Arc::clone(&self.db),
            tree: Arc::clone(&self.tree),
            hydration: Arc::clone(&self.hydration),
            embedding_engine: Arc::clone(&self.embedding_engine),
            insert_lock: Arc::clone(&self.insert_lock),
            needs_projection_sweep: Arc::clone(&self.needs_projection_sweep),
        }
    }

    /// Project one already-stored conversation into the semantic index and
    /// record its `memory_sources` row.
    ///
    /// Shared by the ordinary write path and the pending-projection sweep, so
    /// a repaired turn is classified, placed, and attributed by exactly the
    /// same code as a turn projected on the first attempt. A second
    /// implementation would be free to drift on the one thing that makes the
    /// pending predicate work: the `node_id = NULL` row for a discarded turn.
    ///
    /// The caller holds `insert_lock` and has already established that
    /// hydration is usable.
    ///
    /// EVERY error exit re-arms the pending-projection sweep, which is the
    /// whole reason this wrapper exists rather than the flag being stored at
    /// each failure site. A failed projection leaves the conversation row with
    /// no `memory_sources` row — the exact durable shape #339 is about — so the
    /// sweep is owed again by definition, whatever the cause.
    ///
    /// Storing it only at the two failure sites that used to have it was a
    /// live defect. `sweep_pending_projections_locked` clears the flag when the
    /// pending set comes back empty, and the set can be empty *because the `?1`
    /// exclusion removed the caller's own row*. The caller then failed at
    /// `ctx.embedding_engine.embed()` — a neural engine returns `Err` on a
    /// tokenizer or ONNX fault — whose `?` returned above both of those sites.
    /// The row stayed pending with the flag disarmed, so no automatic sweep ran
    /// again for the life of the process. The same asymmetry hit an ordinary
    /// first-time write that failed at `embed`.
    async fn project_stored_conversation(
        ctx: &ProjectionContext,
        pending: &PendingConversation,
    ) -> Result<()> {
        let result = Self::project_stored_conversation_inner(ctx, pending).await;
        if result.is_err() {
            ctx.needs_projection_sweep.store(true, Ordering::SeqCst);
        }
        result
    }

    /// The body of `project_stored_conversation`. Call the wrapper: this one
    /// does not re-arm the sweep.
    async fn project_stored_conversation_inner(
        ctx: &ProjectionContext,
        pending: &PendingConversation,
    ) -> Result<()> {
        let PendingConversation {
            id,
            role,
            content,
            timestamp,
            session_id,
        } = pending;
        let timestamp = *timestamp;

        // Quality filter: classify and extract key content before indexing.
        // Low-signal content (acks, greetings) is skipped in MemTree but still
        // written to the conversations table above for raw history.
        let classifier = MemoryClassifier::new();
        if let Some((key_content, importance)) = classifier.process(role, content) {
            let embedding = ctx.embedding_engine.embed(&key_content)?;
            // The occurrence chain's `prev`, resolved durably rather than from an in-memory
            // cache: a plain `SELECT` over `conversations`/`memory_sources`/`routing_occurrences`
            // for this session's most recently projected occurrence, so the chain survives a
            // restart with no rebuild step (see `last_occurrence_uuid_for_session`). A stale read
            // under cross-process contention is expected and benign -- `link_next` below resolves
            // it (see `save_routing_occurrence`'s doc), it is never a correctness hazard.
            let prev = match session_id.as_deref() {
                Some(session_id) => {
                    let conn = ctx.db.lock().await;
                    Self::last_occurrence_uuid_for_session(&conn, session_id)?
                }
                None => None,
            };
            // Unlike `MemTree::insert_with_effect`, `RoutingTree::insert` (via `RoutingMemTree`)
            // cannot fail -- there is no aggregation pass with its own failure mode, and no
            // promotion for a partial failure to leave stranded. Nothing here mutates the tree
            // and then can error out before that mutation is accounted for.
            //
            // Persist the occurrence row, the point's content, changed tree structure, the
            // forward link from `prev`, and the provenance row all in one transaction, so the DB
            // stays consistent across process restarts and a retry cannot create a second
            // semantic point -- or a second occurrence -- for the same turn.
            if let Err(error) = Self::save_routing_occurrence(
                &ctx.db,
                &ctx.tree,
                NewOccurrenceContent {
                    key_content,
                    embedding,
                    importance: importance.as_u8(),
                    created_at: timestamp,
                },
                id.as_str(),
                prev,
            )
            .await
            {
                // The SQLite transaction rolled back, but insertion already
                // mutated the in-memory tree. Rebuild it from the durable
                // snapshot before returning so a retry cannot add a duplicate
                // semantic leaf for the same canonical turn.
                //
                // The reload's own failure is logged rather than returned, for
                // the same reason as the branch above: replacing the error the
                // caller actually needs with a second, coincidental one hides
                // what went wrong.
                //
                // The sweep is re-armed by the wrapper, not here.
                if let Err(reload_error) = Self::reload_tree(ctx).await {
                    tracing::error!(
                        ?reload_error,
                        "could not restore the MemTree after a failed save; the \
                         in-memory index is inconsistent until restart"
                    );
                }
                return Err(error);
            }
        } else {
            // A classifier-discarded turn is TERMINAL, not pending.
            //
            // This row, with its NULL `node_id`, is the only thing that lets
            // "no `memory_sources` row" mean "never projected". Drop it and
            // every low-signal turn becomes permanently pending, and the sweep
            // re-classifies the whole backlog on every startup.
            let conn = ctx.db.lock().await;
            conn.execute(
                "INSERT INTO memory_sources (conversation_id, node_id, indexed_at)
                 VALUES (?1, NULL, ?2)",
                params![id, timestamp],
            )?;
        }

        Ok(())
    }

    /// Every stored conversation that was never projected.
    ///
    /// Read in one shot rather than streamed: projecting mutates
    /// `memory_sources`, which is the table the predicate reads, so holding a
    /// cursor across the writes that invalidate it would be reading a moving
    /// set.
    async fn pending_projections(
        db: &Mutex<Connection>,
        exclude: Option<&str>,
    ) -> Result<Vec<PendingConversation>> {
        let conn = db.lock().await;
        let mut stmt = conn
            .prepare(PENDING_PROJECTION_SQL)
            .context("Failed to prepare the pending-projection query")?;
        let rows = stmt
            .query_map(params![exclude], |row| {
                Ok(PendingConversation {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    timestamp: row.get(3)?,
                    session_id: row.get(4)?,
                })
            })
            .context("Failed to read conversations pending projection")?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Project every pending conversation exactly once.
    ///
    /// Exactly-once is a property of the predicate, not of a counter: a row
    /// leaves the pending set precisely when its `memory_sources` row commits,
    /// and that row commits in the SAME SQLite transaction as the semantic leaf
    /// (`write_nodes`). So a repeated sweep re-reads the predicate and finds
    /// nothing, and a sweep interrupted between two rows resumes at the first
    /// row that has not committed. There is no window in which a leaf exists
    /// without its mapping, and therefore none in which a second sweep could
    /// place the same turn twice.
    ///
    /// The caller holds `insert_lock`.
    ///
    /// Stops at the first row that fails rather than continuing, and leaves the
    /// sweep flag armed. A failure here means the store itself is unwell —
    /// embedding, placement, or commit — and grinding the rest of a backlog
    /// through it would turn one diagnosable error into a log full of them.
    async fn sweep_pending_projections_locked(
        ctx: &ProjectionContext,
        exclude: Option<&str>,
    ) -> Result<usize> {
        // `Ready` only.
        //
        // `Degraded` accepts interactive writes (#288) because refusing a live
        // turn over one unreadable row is the larger harm. A backlog is not a
        // live turn: nothing is lost by waiting for a clean load, and placing
        // a whole backlog into a prefix of the tree would durably misplace
        // every row in it. `Failed` and `Loading` are refusals for the reasons
        // `ensure_hydrated` gives.
        if !matches!(ctx.hydration.status(), HydrationStatus::Ready { .. }) {
            return Ok(0);
        }
        let pending = Self::pending_projections(&ctx.db, exclude).await?;
        if pending.is_empty() {
            // Not "nothing is pending" when a row was excluded: the excluded
            // turn is about to be projected by its own caller, so the sweep is
            // still discharged either way.
            ctx.needs_projection_sweep.store(false, Ordering::SeqCst);
            return Ok(0);
        }
        tracing::info!(
            pending = pending.len(),
            "reprojecting conversations stranded by an earlier failed hydration"
        );
        let mut repaired = 0usize;
        for conversation in &pending {
            // The only point at which a backlog can be partially applied, and
            // therefore the only cancellation point worth testing. It is inside
            // the loop rather than around it precisely because a seam outside
            // the loop proves nothing about the loop.
            #[cfg(any(test, feature = "test-support"))]
            ctx.hydration.pause_in_projection_sweep(repaired).await;
            Self::project_stored_conversation(ctx, conversation)
                .await
                .with_context(|| {
                    format!(
                        "Failed to reproject stranded conversation {} \
                         ({repaired} of {} repaired)",
                        conversation.id,
                        pending.len()
                    )
                })?;
            repaired += 1;
        }
        ctx.needs_projection_sweep.store(false, Ordering::SeqCst);
        Ok(repaired)
    }

    /// Run the sweep if one is still owed, reporting failure to the log.
    ///
    /// Used by the hooks that must not fail their caller: the background loader
    /// has nobody to return to, and an interactive turn must not be refused
    /// because an unrelated older turn could not be repaired.
    async fn sweep_pending_projections(ctx: &ProjectionContext) {
        if !ctx.needs_projection_sweep.load(Ordering::SeqCst) {
            return;
        }
        let _guard = ctx.insert_lock.lock().await;
        Self::sweep_pending_projections_if_owed(ctx, None).await;
    }

    /// As `sweep_pending_projections`, for a caller that already holds
    /// `insert_lock`.
    async fn sweep_pending_projections_if_owed(ctx: &ProjectionContext, exclude: Option<&str>) {
        if !ctx.needs_projection_sweep.load(Ordering::SeqCst) {
            return;
        }
        if let Err(error) = Self::sweep_pending_projections_locked(ctx, exclude).await {
            // The flag stays armed, so the next completed hydration or the next
            // write tries again.
            tracing::warn!(
                ?error,
                "could not reproject conversations stranded by a failed hydration"
            );
        }
    }

    /// Project every conversation that was stored but never indexed, and report
    /// how many were repaired.
    ///
    /// Deliberately unconditional: it ignores the "sweep owed" flag the
    /// automatic hooks consult, so a caller asking for recovery gets a real
    /// pass over the predicate. Running it twice must report a second count of
    /// zero and change nothing — that is the exactly-once property, and a flag
    /// short-circuit would hide a violation of it rather than prevent one.
    pub async fn recover_pending_projections(&self) -> Result<usize> {
        let ctx = self.projection();
        let _guard = ctx.insert_lock.lock().await;
        Self::sweep_pending_projections_locked(&ctx, None).await
    }

    /// Structural summary of the semantic index: (leaf count, max depth,
    /// widest fan-out below the root).
    ///
    /// Deliberately scalars rather than a tree snapshot. Cloning the index
    /// copies every embedding while holding the lock — around 137 MB at the
    /// scale measured on the dogfood store, blocking every concurrent insert
    /// and query — and the callers only ever needed these three numbers.
    pub async fn index_shape(&self) -> (usize, usize, usize) {
        let tree = self.tree.lock().await;
        let leaves = tree.tree().leaf_count();
        // RoutingTree is strictly binary (module doc): every decision node has exactly two
        // children, unlike MemTree's variable fan-out. "Widest fan-out" is no longer a meaningful
        // structural signal here -- 2 once any split exists, 0 otherwise.
        let widest = if leaves > 1 { 2 } else { 0 };
        (leaves, tree.tree().max_depth(tree.tree().root()), widest)
    }

    /// Query memory for relevant context.
    ///
    /// Each entry is rendered for injection into a prompt: labelled with the
    /// role that produced it, joined with the turn it replies to, and free of
    /// the template noise older stores carry. Both halves of one exchange
    /// render as that one exchange, so a question never arrives severed from
    /// its answer and never occupies two entries.
    pub async fn query(&self, query_text: &str, top_k: Option<usize>) -> Result<Vec<String>> {
        Ok(self
            .query_recall(query_text, top_k)
            .await?
            .into_iter()
            .map(|recalled| recalled.text)
            .collect())
    }

    /// Query memory for relevant context, retaining each result's `node_id`
    /// and weighted score alongside its rendered, attributed text.
    ///
    /// This is what a caller needing to track recall identity across turns
    /// (the committed-memory-set decision in the query processor) should
    /// call instead of `query()`; `query()` is a thin wrapper over this for
    /// callers that only need rendered text.
    pub async fn query_recall(
        &self,
        query_text: &str,
        top_k: Option<usize>,
    ) -> Result<Vec<RecalledMemory>> {
        let results = self.query_with_sources(query_text, top_k).await?;
        let classifier = MemoryClassifier::new();
        let conn = self.db.lock().await;
        let mut rendered: Vec<RecalledMemory> = Vec::with_capacity(results.len());
        let mut seen: HashSet<String> = HashSet::with_capacity(results.len());
        for result in results {
            // The salience gate at recall, not only at insert (#415). A store
            // built before the classifier existed carries greetings and acks
            // in `routing_points`; nothing rewrites stored rows, so recall holds
            // the same line the classifier holds today.
            if classifier.is_recall_noise(result.text.trim()) {
                continue;
            }
            let entry = match result.source.as_ref() {
                // A legacy leaf with no conversation row cannot be attributed
                // or paired; its bare text is all that is known.
                None => result.text.clone(),
                Some(source) => {
                    let turn = recall_turn_by_id(&conn, &source.source_id)?.unwrap_or_else(|| {
                        RecallTurn {
                            source: source.clone(),
                            content: result.text.clone(),
                            timestamp: 0,
                        }
                    });
                    let counterpart = counterpart_turn(&conn, &turn)?;
                    render_recall_entry(&turn, counterpart.as_ref())
                }
            };
            // One entry per distinct rendered memory: duplicated leaf rows and
            // both halves of one exchange render the same string.
            if seen.insert(entry.clone()) {
                rendered.push(RecalledMemory {
                    node_id: result.node_id,
                    text: entry,
                    score: result.score,
                });
            }
        }
        drop(conn);

        tracing::debug!("Memory query returned {} results", rendered.len());

        Ok(rendered)
    }

    /// Query semantic memory while retaining a stable reference to the
    /// canonical stored turn behind every new-format leaf.
    pub async fn query_with_sources(
        &self,
        query_text: &str,
        top_k: Option<usize>,
    ) -> Result<Vec<MemorySearchResult>> {
        let k = top_k.unwrap_or(self.config.max_context_items);
        let query_embedding = self.embedding_engine.embed(query_text)?;
        let retrieved = {
            // Lock ordering matches `stats()`: db before tree. `retrieve`'s neighbor-context
            // tie-break needs both `routing_occurrences` (via `conn`) and the tree's point
            // embeddings, so both locks are held together here for the first time in this
            // function; keeping the same db-then-tree order everywhere in this file avoids a
            // lock-order deadlock against any other caller that takes both.
            let conn = self.db.lock().await;
            let tree = self.tree.lock().await;
            tree.retrieve(&conn, &query_embedding, k)?
        };
        let min_score = self.config.min_relevance_score;

        // The turn-level gate (#1134): decide whether anything at all is
        // injected this turn BEFORE the per-result floor is applied. The
        // per-result floor answers "is this one result strong enough";
        // nearest-neighbour retrieval always returns k candidates, so it
        // cannot answer "does this turn need memory context at all" -- a
        // query needing no memory context received k weak-but-qualifying
        // matches injected every turn.
        let retrieved_scores: Vec<f32> = retrieved.iter().map(|(_, _, score)| *score).collect();
        let decision =
            turn_injection_decision(self.config.min_turn_relevance_score, &retrieved_scores);
        if let Some(turn_floor) = decision.turn_floor {
            if decision.skips() {
                // `skips` is only true when `best_score` is `Some`.
                let best_score = decision.best_score.unwrap_or_default();
                tracing::info!(
                    turn_floor,
                    best_score,
                    candidates = retrieved.len(),
                    "memory turn-level injection gate skipped recall: best weighted \
                     score below the turn floor, injecting nothing this turn"
                );
                return Ok(Vec::new());
            }
            tracing::debug!(
                turn_floor,
                best_score = ?decision.best_score,
                "memory turn-level injection gate allowed recall: falling through \
                 to the per-result relevance floor"
            );
        }

        let conn = self.db.lock().await;
        let mut results = Vec::with_capacity(retrieved.len());
        let mut dropped_below_floor = 0usize;
        for (node_id, text, score) in retrieved {
            // Applied once here so every caller (rendered `query`/`query_recall`
            // and any direct `query_with_sources` caller) drops a weak match
            // instead of only ever capping by count (#940).
            if score < min_score {
                dropped_below_floor += 1;
                continue;
            }
            let source = source_metadata_for_node(&conn, node_id)?;
            let memory_id = source
                .as_ref()
                .map(|source| source.source_id.clone())
                .unwrap_or_else(|| format!("node:{node_id}"));
            results.push(MemorySearchResult {
                memory_id,
                node_id,
                text,
                score,
                source,
            });
        }
        tracing::debug!(
            "Memory query returned {} sourced results ({} dropped below the \
             per-result floor)",
            results.len(),
            dropped_below_floor
        );
        Ok(results)
    }

    /// Resolve the stable ID returned by `query_with_sources`. New memories
    /// return their complete conversation row; historical unattributed leaves
    /// remain inspectable by their explicit `node:<id>` fallback.
    pub async fn inspect_memory(&self, memory_id: &str) -> Result<Option<InspectedMemory>> {
        let memory_id = memory_id.trim();
        if let Some(node_id) = memory_id.strip_prefix("node:") {
            let node_id = node_id
                .parse::<NodeId>()
                .with_context(|| format!("invalid memory node reference '{memory_id}'"))?;
            let tree = self.tree.lock().await;
            return Ok(tree
                .get_point(node_id as PointId)
                .map(|point| InspectedMemory {
                    memory_id: memory_id.to_string(),
                    node_id: Some(node_id),
                    source: None,
                    content: point.text.clone(),
                }));
        }

        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT c.id, c.role, c.content, c.model, c.session_id,
                    c.brain_id, c.run_id, c.request_seq, ms.node_id
             FROM conversations c
             LEFT JOIN memory_sources ms ON ms.conversation_id = c.id
             WHERE c.id = ?1",
        )?;
        let mut rows = stmt.query([memory_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let request_seq = optional_request_seq(row.get::<_, Option<i64>>(7)?)?;
        let source = MemorySourceMetadata {
            source_id: row.get(0)?,
            role: row.get(1)?,
            model: row.get(3)?,
            session_id: row.get(4)?,
            brain_id: row.get(5)?,
            run_id: row.get(6)?,
            request_seq,
        };
        Ok(Some(InspectedMemory {
            memory_id: source.source_id.clone(),
            node_id: row.get::<_, Option<i64>>(8)?.map(|value| value as NodeId),
            source: Some(source),
            content: row.get(2)?,
        }))
    }

    /// Get recent conversations (for context window)
    pub async fn get_recent_conversations(&self, limit: usize) -> Result<Vec<(String, String)>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare(
            "SELECT role, content FROM conversations
             ORDER BY timestamp DESC
             LIMIT ?1",
        )?;

        let conversations: Vec<(String, String)> = stmt
            .query_map([limit], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(conversations)
    }

    /// Get memory statistics
    pub async fn stats(&self) -> Result<MemoryStats> {
        let conn = self.db.lock().await;

        let conversation_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;

        let tree = self.tree.lock().await;
        let tree_size = tree.size();

        Ok(MemoryStats {
            conversation_count: conversation_count as usize,
            tree_node_count: tree_size,
        })
    }

    /// Mint one conversational occurrence and persist it, its point's content, every routing
    /// node that changed as a result, an attempted forward link from `prev`, and its provenance
    /// row, all in a single transaction.
    ///
    /// Unlike `MemTree`, there is no promotion to fix up: a point's id is permanent from the
    /// moment `insert_occurrence` assigns it (`RoutingMemTree`'s own module doc), so a
    /// `memory_sources` row never needs to follow content to a different id after the fact.
    ///
    /// The occurrence's own row and its point are minted and persisted together -- unlike the
    /// retired `insert_with_effect`/`save_routing_insert` split this replaces, which mutated the
    /// tree first and persisted afterward, `insert_occurrence` is called *inside* this
    /// transaction (passed `&tx`, which derefs to `&Connection`) so a rollback here undoes the
    /// occurrence row exactly as it undoes the point row; nothing durable can name a point that
    /// was never persisted, or an occurrence whose point wasn't.
    ///
    /// `prev`'s forward link is attempted with the same transaction's connection. A
    /// [`LinkNextError::Conflict`] does NOT abort the transaction or fail this call: the new
    /// occurrence and its point are still real and still committed, they are just not linked
    /// from their predecessor -- a real, expected outcome of two callers racing to continue the
    /// same session (`link_next`'s own doc), not a reason to discard work that already
    /// succeeded. A [`LinkNextError::Sql`] is a genuine failure and aborts the transaction like
    /// any other error here.
    ///
    /// Takes the locks rather than `&self`: the pending-projection sweep runs inside the
    /// background loader, which is spawned before `MemorySystem` exists, so it holds these
    /// `Arc`s and has no `&self`. One implementation, same locks, same order.
    async fn save_routing_occurrence(
        db: &Mutex<Connection>,
        tree_lock: &Mutex<RoutingMemTree>,
        content: NewOccurrenceContent,
        conversation_id: &str,
        prev: Option<Uuid>,
    ) -> Result<Occurrence> {
        let NewOccurrenceContent {
            key_content,
            embedding,
            importance,
            created_at,
        } = content;

        // Both locks, held across the whole read-write-mark cycle, and taken **db before tree**
        // because `stats` nests them in that order -- inverting here would deadlock against a
        // concurrent `stats`. Same discipline `MemTree`'s own save path used (#313): no await
        // between reading the dirty set and committing, and the tree guard spans the commit so a
        // mutation landing mid-write cannot be marked and then silently unmarked.
        let conn = db.lock().await;
        let mut tree = tree_lock.lock().await;

        let tx = conn.unchecked_transaction()?;
        let occurrence = tree
            .insert_occurrence(&tx, key_content, embedding, importance, created_at, prev)
            .context("save_routing_occurrence: insert_occurrence")?;
        let point_id = occurrence.point_id;

        let meta = tree
            .get_point(point_id)
            .ok_or_else(|| {
                anyhow::anyhow!("memory: point {point_id} was just inserted but is not in the tree")
            })?
            .clone();
        let point_embedding = tree.tree().embedding_of(point_id as usize).to_vec();
        let dirty = tree.tree().dirty_node_ids();

        save_point(
            &tx,
            point_id as usize,
            &meta.text,
            &point_embedding,
            meta.importance,
            meta.created_at,
        )?;
        write_dirty_nodes_within(tree.tree(), &dirty, &tx)?;
        // `node_id` (`memory_sources`' own column name, unchanged) now holds a `routing_points`
        // point id. `insert_occurrence` never dedups, so every occurrence's point is unique to
        // it; `conversation_id` is still the primary key, so a retry of the same turn is still
        // idempotent.
        tx.execute(
            "INSERT OR REPLACE INTO memory_sources (conversation_id, node_id, indexed_at)
             VALUES (?1, ?2, ?3)",
            params![conversation_id, point_id as i64, created_at],
        )?;

        if let Some(prev_uuid) = prev {
            match RoutingMemTree::link_next(&tx, prev_uuid, occurrence.uuid) {
                Ok(()) => {}
                Err(LinkNextError::Conflict(_)) => {
                    tracing::warn!(
                        prev = %prev_uuid,
                        next = %occurrence.uuid,
                        conversation_id,
                        "occurrence chain link lost a race to a concurrent writer; the new \
                         occurrence and its point are still recorded, just not linked from \
                         their predecessor"
                    );
                }
                Err(LinkNextError::Sql(sql_error)) => {
                    return Err(sql_error).context("save_routing_occurrence: link_next");
                }
            }
        }

        tx.commit()?;
        tree.tree_mut().mark_persisted(&dirty);
        Ok(occurrence)
    }

    /// The uuid of `session_id`'s most recently projected occurrence, or `None` when the session
    /// has none yet -- a fresh session's first turn, or every prior turn in the session was
    /// classifier-discarded (never became an occurrence at all).
    ///
    /// Derived via `conversations`/`memory_sources`/`routing_occurrences` rather than a
    /// denormalized `session_id` column on `routing_occurrences` itself: `memory_sources.node_id`
    /// already holds the point id an occurrence was minted for, and `conversations.session_id`
    /// is the caller's real session identity, so this reuses columns and indexes
    /// (`idx_conversations_session`, `idx_memory_sources_node`) that already exist instead of
    /// keeping a second copy of session identity in sync. A durable query rather than an
    /// in-memory "last occurrence" cache for the same reason `RoutingMemTree::load` rebuilds the
    /// tree from `schema.sql` rows on every restart: nothing here needs its own recovery path,
    /// because there is nothing in memory that could go stale.
    fn last_occurrence_uuid_for_session(
        conn: &Connection,
        session_id: &str,
    ) -> Result<Option<Uuid>> {
        let raw: Option<String> = conn
            .query_row(
                "SELECT ro.uuid
                 FROM conversations c
                 JOIN memory_sources ms ON ms.conversation_id = c.id
                 JOIN routing_occurrences ro ON ro.point_id = ms.node_id
                 WHERE c.session_id = ?1
                 ORDER BY c.timestamp DESC, c.id DESC
                 LIMIT 1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()
            .context("last_occurrence_uuid_for_session: query conversations/memory_sources/routing_occurrences")?;
        raw.map(|uuid| {
            Uuid::parse_str(&uuid).with_context(|| {
                format!("last_occurrence_uuid_for_session: stored uuid {uuid:?} does not parse")
            })
        })
        .transpose()
    }

    async fn reload_tree_from_db(&self) -> Result<()> {
        // A store that has just been rebuilt may be a store whose failed
        // hydration stranded turns, so re-arm the sweep: `clear_failure` below
        // is exactly the transition from "writes refused" to "writes placed",
        // and #339 is about that transition never being acted on.
        self.needs_projection_sweep.store(true, Ordering::SeqCst);
        Self::reload_tree(&self.projection()).await
    }

    /// The body of `reload_tree_from_db`, over the `Arc`s rather than `self`.
    async fn reload_tree(ctx: &ProjectionContext) -> Result<()> {
        let restored = {
            let conn = ctx.db.lock().await;
            RoutingMemTree::load(&conn, ctx.embedding_engine.dimension())?
        };
        let nodes = restored.size();
        // `RoutingMemTree::load`/`load_routing_tree` clear the dirty set as part of installing
        // loaded state (`install_loaded_state`'s own doc comment) -- the tree was just built from
        // the durable rows, so nothing in it is pending.
        *ctx.tree.lock().await = restored;
        // The tree now matches the durable snapshot, so whatever the loader
        // recorded no longer describes it. Leaving the failure set meant a
        // store that had been rebuilt successfully still refused every write,
        // with no way back short of a restart that would re-read the same rows
        // and fail again (#288).
        ctx.hydration.clear_failure(nodes);
        Ok(())
    }

    /// Reconstruct the `RoutingTree` from `routing_points`/`routing_nodes`/
    /// `routing_leaf_membership` at startup.
    ///
    /// RoutingTree hydration, deliberately simpler than MemTree's own batch/link/partial-degrade
    /// machinery: `routing_nodes.is_leaf` is an explicit persisted column, never inferred from an
    /// empty child list, so there is no "unlinked node looks like a leaf" window for a concurrent
    /// read to fall into (the entire reason MemTree's own loader split into an edges-first pass, a
    /// batch loop, and a final linking pass). A `RoutingTree` node is unambiguous the instant its
    /// row lands.
    ///
    /// A real, disclosed simplification against MemTree's own resilience, not yet replicated:
    /// this loads all-or-nothing rather than in bounded batches, so a large store still blocks
    /// startup behind a full decode (the #242 cost this crate's own comments elsewhere describe
    /// paying down once already), and a failure partway is always `Failed`, never `Degraded` --
    /// there is no partial-batch state to preserve. Real follow-up work, not a silent regression:
    /// noted in `crates/finch-memory/AGENTS.md`.
    async fn hydrate_in_background(projection: ProjectionContext) {
        let restored = {
            let conn = projection.db.lock().await;
            RoutingMemTree::load(&conn, projection.embedding_engine.dimension())
        };
        match restored {
            Ok(restored) => {
                let nodes = restored.size();
                *projection.tree.lock().await = restored;
                projection.hydration.loaded.store(nodes, Ordering::SeqCst);
                projection.hydration.complete();
                #[cfg(any(test, feature = "test-support"))]
                projection.hydration.pause_after_completion().await;
            }
            Err(error) => {
                tracing::error!(%error, "RoutingTree hydration failed; the index is incomplete for the rest of this session");
                projection.hydration.fail(error.to_string());
            }
        }

        // The reopen half of #339.
        //
        // A turn stored while an earlier hydration was broken kept its
        // `conversations` row and got no `memory_sources` row, and nothing ever
        // looked at it again: ordinary retries mint a fresh UUID, so they
        // cannot repair the original row. Sweeping here is what makes the
        // stranding transient — the next successful load projects it.
        //
        // AFTER hydration settles, never during. The sweep takes
        // `insert_lock`, and a writer holds that lock while awaiting hydration
        // to finish; taking it before `complete()`/`fail()` fires would
        // deadlock the two against each other.
        Self::sweep_pending_projections(&projection).await;
    }

    /// Progress of the background hydration, for status surfaces.
    pub fn hydration_status(&self) -> HydrationStatus {
        self.hydration.status()
    }

    /// Force a `Degraded` status for cross-crate integration tests.
    ///
    /// RoutingTree hydration is atomic (see `hydrate_in_background`'s own
    /// doc): nothing in production reaches `Degraded` anymore, since there is
    /// no partial-batch load left to stop partway through with what loaded so
    /// far still coherent. The variant and its projection are still real and
    /// still worth testing (`mem-index-status`, `mem-recall`'s refusal of an
    /// incomplete index), so this exists purely to construct that state
    /// directly -- the same technique `crates/finch-memory/src/lib.rs`'s own
    /// `test_a_degraded_index_accepts_writes_and_a_broken_one_does_not` uses
    /// from inside this crate's `cfg(test)`, exposed here because a dependent
    /// crate's integration tests cannot reach `HydrationState` at all.
    #[cfg(any(test, feature = "test-support"))]
    pub fn force_degraded_for_test(&self, loaded: usize, total: usize, reason: String) {
        self.hydration.loaded.store(loaded, Ordering::SeqCst);
        self.hydration.total.store(total, Ordering::SeqCst);
        self.hydration.degrade(reason);
    }

    /// The configuration this instance was constructed with -- callers that
    /// need the relevance threshold, committed-set cap, or staleness grace
    /// period (#940) read it from here rather than duplicating it.
    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    /// Wait until every persisted node is in memory.
    ///
    /// Writes await this: placing a memory against a partially loaded tree
    /// would put it in the wrong part of the structure, and that placement is
    /// durable. Reads deliberately do not — serving the memories loaded so far
    /// is better than blocking a turn, and `hydration_status` reports when the
    /// view is partial.
    pub async fn ensure_hydrated(&self) -> Result<()> {
        let mut rx = self.hydration.done.subscribe();
        loop {
            if *rx.borrow_and_update() {
                break;
            }
            // The value is retained, so a completion between the check above
            // and this await is observed on the next iteration rather than
            // lost. An `Err` means the sender is gone, which cannot happen
            // while `self` holds the state, but returning is the safe read.
            if rx.changed().await.is_err() {
                break;
            }
        }

        // No timeout here, deliberately.
        //
        // An earlier version of this change wrapped the wait in
        // `tokio::time::timeout`, which was wrong twice. It could not fire in
        // the case it was written for — a current-thread runtime blocked in
        // `block_on_host` owns the time driver, so the `Sleep` never
        // advances — and where it *could* fire, it turned a latency signal
        // into a permanent verdict: a store that hydrated in 75 seconds
        // tripped the bound at 60, and because nothing clears `failure`, every
        // write for the rest of the process was refused, blaming a loader that
        // had in fact succeeded.
        //
        // The current-thread case is now structurally impossible: that flavor
        // loads synchronously in `new`, so there is no task to starve. What
        // remains is a multi-threaded loader that ends without finishing, and
        // `HydrationGuard` covers that by construction rather than by waiting
        // a guessed interval.
        // Only `Failed`.
        //
        // A `Degraded` index holds a prefix of the store with its children
        // linked, so `MemTree::insert` can descend and place a memory in a real
        // part of the structure — just a smaller one than a complete load would
        // have offered. That is a worse placement than the store deserves, and
        // it is the trade this makes: refusing every write for the rest of the
        // process over one unreadable row is a larger harm than an
        // imperfectly-placed memory (#288).
        //
        // Not "no worse than a write during hydration" — an earlier version of
        // this comment said that, and it is false. Writes during hydration are
        // not allowed; `ensure_hydrated` blocks them until the loader
        // finishes. There is no such precedent to appeal to.
        if let HydrationStatus::Failed { reason } = self.hydration.status() {
            anyhow::bail!("MemTree hydration failed, so this write cannot be placed: {reason}");
        }
        Ok(())
    }

    /// Persist a successful Lisp `(define ...)` expression for session replay.
    ///
    /// This writes `lisp_env` only. Composition projects authored definitions
    /// into the program index through the caller-owned adapter.
    pub async fn save_lisp_define(&self, expr: &str) -> Result<()> {
        let created_at = chrono::Utc::now().timestamp();
        {
            let conn = self.db.lock().await;
            conn.execute(
                "INSERT INTO lisp_env (expr, created_at) VALUES (?1, ?2)",
                rusqlite::params![expr, created_at],
            )?;
        }
        Ok(())
    }

    /// Load legacy persisted Lisp definitions for explicit migration tooling.
    ///
    /// The interactive runtime must not replay these into the native Lisp evaluator: authored
    /// definitions are projected into the shared typed program registry by the
    /// composition adapter. This reader remains temporarily available so older
    /// databases can be migrated without making their obsolete evaluator state
    /// authoritative again.
    pub async fn load_lisp_defines(&self) -> Result<Vec<String>> {
        let conn = self.db.lock().await;
        let mut stmt = conn.prepare("SELECT expr FROM lisp_env ORDER BY seq ASC")?;
        let exprs: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(exprs)
    }

    /// Derive a short topic summary without any LLM call.
    ///
    /// Uses centroid queries against the MemTree:
    /// - `overall` → representative turn for the whole session
    /// - `current` → representative turn among the 5 most recent turns
    ///
    /// Returns `depth` context-summary lines by querying the MemTree at
    /// increasingly fine-grained time windows (broadest → most recent).
    ///
    /// - `depth` = 0   → empty result
    /// - `depth` = 1   → one line: most-recent centroid
    /// - `depth` = 2   → \[overall, recent\]
    /// - `depth` = N   → overall + (N-2) intermediate windows + most-recent
    ///
    /// Returns an empty `lines` vec when no turns have been recorded yet.
    /// Consecutive identical lines are de-duplicated so a short session
    /// (few leaves) produces compact, non-redundant output.
    pub(crate) async fn conversation_summary(
        &self,
        depth: usize,
    ) -> Result<ConversationSummaryLines> {
        if depth == 0 {
            return Ok(ConversationSummaryLines::default());
        }

        // Lock ordering matches `stats()`/`query_with_sources`: db before tree. `retrieve` now
        // needs `conn` for its neighbor-context tie-break even at `top_k=1` here (a tie for the
        // single returned slot is still a tie).
        let conn = self.db.lock().await;
        let tree = self.tree.lock().await;

        // Collect every point's embedding and text -- unlike MemTree there is no root id=0 to
        // exclude (a `RoutingMemTree` point IS real content the moment it exists, never an
        // aggregate placeholder). Owned, not borrowed: `average_embeddings` below wants
        // `&[&Vec<f32>]`, easiest to satisfy from data this function already owns outright.
        let mut leaves: Vec<(i64, Vec<f32>, String)> = tree
            .iter_points()
            .map(|(_, m, e)| (m.created_at, e.to_vec(), m.text.clone()))
            .collect();

        if leaves.is_empty() {
            return Ok(ConversationSummaryLines::default());
        }

        // Sort most-recent first for window slicing
        leaves.sort_by(|a, b| b.0.cmp(&a.0));
        let num_leaves = leaves.len();

        // Compute the window sizes for the requested depth
        let windows = context_windows(depth, num_leaves);

        // The last window is always the "now" slot. Pin it to the most-recent
        // leaf's actual text so it is guaranteed to show something fresh and
        // distinct, even when all centroid queries converge on the same node.
        let now_text = truncate_str(&leaves[0].2, 70);

        let mut lines: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Centroid queries for all windows except the last ("now") slot.
        for window in windows.iter().take(windows.len().saturating_sub(1)) {
            let slice: Vec<&Vec<f32>> = leaves.iter().take(*window).map(|(_, e, _)| e).collect();
            let centroid = average_embeddings(&slice);
            if let Some((_, text, _)) = tree.retrieve(&conn, &centroid, 1)?.into_iter().next() {
                let s = truncate_str(&text, 70);
                if !s.trim().is_empty() && s != now_text && seen.insert(s.clone()) {
                    lines.push(s);
                }
            }
        }

        // "Now" slot: always the most-recent leaf — pinned, not a centroid query.
        if !now_text.trim().is_empty() {
            lines.push(now_text);
        }

        Ok(ConversationSummaryLines { lines })
    }

    /// Derive context-summary lines from one Finch session only.
    ///
    /// The MemTree is intentionally cross-session, so using its global leaves for
    /// the persistent footer can make one brain display another brain's topic.
    /// Footer identity must instead come from the conversations carrying the
    /// active session id. Cross-session MemTree results are still available to
    /// the model as recalled context, but they do not masquerade as this brain's
    /// current focus.
    pub async fn conversation_summary_for_session(
        &self,
        session_id: &str,
        depth: usize,
    ) -> Result<ConversationSummaryLines> {
        if depth == 0 || session_id.is_empty() {
            return Ok(ConversationSummaryLines::default());
        }

        let turns: Vec<String> = {
            let conn = self.db.lock().await;
            let mut stmt = conn.prepare(
                "SELECT content FROM conversations
                 WHERE session_id = ?1 AND TRIM(content) <> ''
                 ORDER BY timestamp DESC",
            )?;
            let rows = stmt
                .query_map([session_id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };

        if turns.is_empty() {
            return Ok(ConversationSummaryLines::default());
        }

        let embeddings = turns
            .iter()
            .map(|turn| self.embedding_engine.embed(turn))
            .collect::<Result<Vec<_>>>()?;
        let windows = context_windows(depth, turns.len());
        let now_text = truncate_str(&turns[0], 70);
        let mut lines = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for window in windows.iter().take(windows.len().saturating_sub(1)) {
            let count = (*window).min(embeddings.len());
            let window_embeddings: Vec<&Vec<f32>> = embeddings[..count].iter().collect();
            let centroid = average_embeddings(&window_embeddings);
            let representative = embeddings[..count]
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| {
                    cosine_similarity(a, &centroid)
                        .partial_cmp(&cosine_similarity(b, &centroid))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(index, _)| truncate_str(&turns[index], 70));
            if let Some(text) = representative {
                if text != now_text && seen.insert(text.clone()) {
                    lines.push(text);
                }
            }
        }

        if !now_text.trim().is_empty() {
            lines.push(now_text);
        }

        Ok(ConversationSummaryLines { lines })
    }
}

/// Summary of conversation topics derived from MemTree centroid queries.
#[derive(Debug, Clone, Default)]
pub struct ConversationSummaryLines {
    /// Context lines ordered from broadest (overall session) to most recent.
    /// Length equals the `depth` passed to `conversation_summary`, minus any
    /// de-duplicated consecutive matches.
    pub lines: Vec<String>,
}

/// Compute the leaf-count window sizes for the given display depth.
///
/// `depth` = number of context-summary lines requested (excluding the 🧠 stats line).
/// `num_leaves` caps window sizes so we never ask for more leaves than exist.
///
/// Window layout:
/// - depth 1  → \[3\]                                 (just "now")
/// - depth 2  → \[all, 3\]                            (overall + now)
/// - depth 3  → \[all, 5, 3\]
/// - depth 4  → \[all, 7, 5, 3\]
/// - depth 5  → \[all, 10, 7, 5, 3\]
/// - depth 6+ → \[all, 20, 10, 7, 5, 3\] (capped at 6 levels)
fn context_windows(depth: usize, num_leaves: usize) -> Vec<usize> {
    // Intermediate window sizes available between "all" and "now=3"
    const INTERMEDIATES: &[usize] = &[20, 10, 7, 5];
    let cap = |w: usize| w.min(num_leaves).max(1);

    match depth {
        0 => vec![],
        1 => vec![cap(3)],
        n => {
            let num_mid = n.saturating_sub(2);
            let avail = INTERMEDIATES.len().min(num_mid);
            let start = INTERMEDIATES.len().saturating_sub(avail);
            let mut ws = vec![cap(num_leaves)]; // overall = all leaves
            for &w in &INTERMEDIATES[start..] {
                ws.push(cap(w));
            }
            ws.push(cap(3)); // most recent
            ws
        }
    }
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_chars - 1).collect::<String>())
    }
}

/// Memory statistics
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub conversation_count: usize,
    pub tree_node_count: usize,
}

#[cfg(test)]
mod tests {

    /// Count writes to `routing_nodes` made by one closure.
    ///
    /// A trigger rather than `Connection::total_changes`, because the store
    /// keeps its connection private: triggers belong to the database, so a
    /// probe installed from a second connection sees writes made through the
    /// first. `ON CONFLICT DO UPDATE` fires the update trigger even when every
    /// column is rewritten to the value it already held, which is exactly the
    /// work this measures. `routing_nodes` (structure: anchor/direction/
    /// real_centroid per tree node), not `routing_points` (content: one row
    /// per memory, always exactly one new row per insert regardless of tree
    /// size) -- the ancestors-whose-centroid-changed cost this test bounds.
    fn tree_node_writes(db_path: &std::path::Path) -> Result<i64> {
        let conn = Connection::open(db_path)?;
        conn.query_row("SELECT COUNT(*) FROM write_probe", [], |row| row.get(0))
            .map_err(Into::into)
    }

    fn install_write_probe(db_path: &std::path::Path) -> Result<()> {
        Connection::open(db_path)?.execute_batch(
            "CREATE TABLE IF NOT EXISTS write_probe (n INTEGER);
             DROP TRIGGER IF EXISTS probe_tree_insert;
             DROP TRIGGER IF EXISTS probe_tree_update;
             CREATE TRIGGER probe_tree_insert AFTER INSERT ON routing_nodes
                 BEGIN INSERT INTO write_probe VALUES (1); END;
             CREATE TRIGGER probe_tree_update AFTER UPDATE ON routing_nodes
                 BEGIN INSERT INTO write_probe VALUES (1); END;
             DELETE FROM write_probe;",
        )?;
        Ok(())
    }

    /// Build a store holding `turns` distinct memories and report how many
    /// `routing_nodes` rows one further insert writes.
    async fn writes_for_one_more_insert(turns: usize) -> Result<(i64, usize)> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        for i in 0..turns {
            memory
                .insert_conversation(
                    "user",
                    &format!("a distinct memory numbered {i}"),
                    None,
                    None,
                )
                .await?;
        }
        let nodes = memory.stats().await?.tree_node_count;

        install_write_probe(temp.path())?;
        memory
            .insert_conversation("user", "one more entirely distinct memory", None, None)
            .await?;
        Ok((tree_node_writes(temp.path())?, nodes))
    }

    /// A save and a concurrent reader both finish; neither waits on the other
    /// forever.
    ///
    /// This module has a db -> tree lock ordering. `stats` has always nested
    /// that way, and `save_all_nodes_to_db` now does too -- holding db and
    /// waiting for tree across a whole transaction, on the write path, every
    /// turn. A reader that took tree and then wanted db would deadlock against
    /// it.
    ///
    /// Nothing enforced that. Review of #313 showed the hazard is one deleted
    /// pair of braces away: hoisting `query_with_sources`'s tree guard out of
    /// its block so it is still held when the db lock is taken hangs both
    /// tasks. Before this branch that inversion collided only with `stats`;
    /// now it collides with every insert.
    ///
    /// A deadlock cannot be asserted directly, so this asserts its observable
    /// consequence: with writes and reads interleaved, everything completes
    /// well inside a deadline. An inversion fails it by timing out rather than
    /// by hanging the suite.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn test_a_save_and_a_concurrent_read_both_finish() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = Arc::new(MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })?);
        for i in 0..20 {
            memory
                .insert_conversation("user", &format!("a seeded memory numbered {i}"), None, None)
                .await?;
        }

        let writer = {
            let memory = Arc::clone(&memory);
            tokio::spawn(async move {
                for i in 0..20 {
                    memory
                        .insert_conversation(
                            "user",
                            &format!("a concurrently written memory numbered {i}"),
                            None,
                            None,
                        )
                        .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        };
        let reader = {
            let memory = Arc::clone(&memory);
            tokio::spawn(async move {
                for _ in 0..20 {
                    // Both orderings of interest: `stats` nests db -> tree,
                    // `query` takes tree alone and then db.
                    let _ = memory.stats().await?;
                    let _ = memory.query("a seeded memory", None).await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        };

        let deadline = std::time::Duration::from_secs(30);
        let both = tokio::time::timeout(deadline, async { tokio::try_join!(writer, reader) })
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "a save and a concurrent read did not both finish within 30s, which is \
                     what a tree -> db lock inversion looks like from outside"
                )
            })?;
        let (wrote, read) = both?;
        wrote?;
        read?;
        Ok(())
    }

    /// Every persisted column of every point reaches disk, including the dedup
    /// importance bump.
    ///
    /// This is the guard the incremental save needs and a whole-tree write did
    /// not. Writing every point on every insert made a forgotten write
    /// impossible; writing only marked ones makes a forgotten *mark* silent
    /// data loss. Unlike `MemTree`'s equivalent, there is no parent/level/
    /// promotion/aggregation column to lose: `RoutingMemTree`'s point content
    /// (text, embedding, importance, created_at) is the entire persisted
    /// surface for a point (`routing_memory.rs`'s own module doc -- no
    /// promotion, since a point's id is permanent).
    #[tokio::test]
    async fn test_every_persisted_point_survives_a_reload() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };

        let expected: Vec<(PointId, String, u8, i64, Vec<f32>)> = {
            let memory = MemorySystem::new(config.clone())?;
            for i in 0..40 {
                memory
                    .insert_conversation(
                        "user",
                        &format!("a distinct memory numbered {i}"),
                        None,
                        None,
                    )
                    .await?;
                // A genuinely different point -- real content, not a duplicate.
                memory
                    .insert_conversation(
                        "user",
                        &format!("a distinct memory numbered {i} indeed"),
                        None,
                        None,
                    )
                    .await?;
                // Exact repeat: the dedup path, which creates no new point.
                memory
                    .insert_conversation(
                        "user",
                        &format!("a distinct memory numbered {i}"),
                        None,
                        None,
                    )
                    .await?;
                // The same fact stored *explicitly* -- the only way the dedup
                // importance bump fires in production (`process` gives role
                // "system" Critical; `extract` is role-independent below 300
                // characters, so this dedups onto the existing point and
                // raises its importance).
                memory
                    .insert_conversation(
                        "system",
                        &format!("a distinct memory numbered {i}"),
                        None,
                        None,
                    )
                    .await?;
            }
            snapshot(&memory).await
        };

        // Reopen from the same file: `MemorySystem::new` hydrates from disk, so
        // this is the restart path.
        let reopened = MemorySystem::new(config)?;
        reopened.ensure_hydrated().await?;
        let restored = snapshot(&reopened).await;

        assert_eq!(
            restored.len(),
            expected.len(),
            "a point was lost entirely between memory and disk"
        );
        for (before, after) in expected.iter().zip(restored.iter()) {
            if before == after {
                continue;
            }
            let column = if before.1 != after.1 {
                format!("text {:?} != {:?}", before.1, after.1)
            } else if before.2 != after.2 {
                format!("importance {} != {}", before.2, after.2)
            } else if before.3 != after.3 {
                format!("created_at {} != {}", before.3, after.3)
            } else {
                format!(
                    "embedding differs ({} floats; first divergence at {:?})",
                    before.4.len(),
                    before
                        .4
                        .iter()
                        .zip(after.4.iter())
                        .position(|(a, b)| a != b)
                )
            };
            panic!("point {} did not survive the reload: {column}. A mutation that forgot to mark its node dirty is lost here and nowhere else.", before.0);
        }
        Ok(())
    }

    /// Every point's persisted columns, ordered by id.
    async fn snapshot(memory: &MemorySystem) -> Vec<(PointId, String, u8, i64, Vec<f32>)> {
        let tree = memory.tree.lock().await;
        let mut rows: Vec<_> = tree
            .iter_points()
            .map(|(pid, m, e)| (pid, m.text.clone(), m.importance, m.created_at, e.to_vec()))
            .collect();
        rows.sort_by_key(|row| row.0);
        rows
    }

    /// One new memory must not rewrite every node in the tree.
    ///
    /// `save_all_nodes_to_db` used to collect `tree.all_nodes()` and upsert
    /// each one on every insert, so the cost of storing one memory was
    /// proportional to everything stored before it. #250 measured the consequence on the
    /// dogfood host: a 142 MiB store with a 142 MiB write-ahead log, because
    /// each save rewrote all 16,782 rows and 131 MiB of embeddings.
    ///
    /// Asserted as size-independence rather than a fixed ceiling. What a
    /// correct save writes is the new leaf and the ancestors whose aggregate
    /// embedding it changed -- bounded by depth, which grows with the log of
    /// the content, not with the row count. So the test grows the tree
    /// fourfold and requires the per-insert write count not to follow. A fixed
    /// ceiling would either be loose enough to pass the defect at small sizes
    /// or tight enough to break when the aggregation path legitimately
    /// lengthens.
    #[tokio::test]
    async fn test_one_new_memory_does_not_rewrite_the_whole_tree() -> Result<()> {
        let (small_writes, small_nodes) = writes_for_one_more_insert(30).await?;
        let (large_writes, large_nodes) = writes_for_one_more_insert(120).await?;

        assert!(
            large_nodes >= small_nodes * 3,
            "the fixture must actually grow the tree, or this test cannot fail \
             for the reason it exists: {small_nodes} -> {large_nodes}"
        );
        assert!(
            small_writes > 0,
            "the probe must observe the save at all, or the comparison below \
             is between two zeroes"
        );
        assert!(
            large_writes <= small_writes + 8,
            "storing one memory wrote {small_writes} rows into a {small_nodes}-node \
             tree and {large_writes} into a {large_nodes}-node one, so the cost of \
             remembering something is proportional to everything remembered before \
             it -- which is what filled the dogfood store's write-ahead log (#250)"
        );
        Ok(())
    }

    use super::*;
    use finch_routing_tree::{load_routing_tree, RoutingConfig, RoutingTree};
    use tempfile::NamedTempFile;

    struct FixedDimensionEngine {
        dimension: usize,
    }

    impl EmbeddingEngine for FixedDimensionEngine {
        fn embed(&self, _text: &str) -> Result<Vec<f32>> {
            let mut embedding = vec![0.0; self.dimension];
            if !embedding.is_empty() {
                embedding[0] = 1.0;
            }
            Ok(embedding)
        }

        fn dimension(&self) -> usize {
            self.dimension
        }
    }

    #[test]
    fn test_new_uses_hashed_ngram_and_ignores_neural_selection_flag() {
        let temp = NamedTempFile::new().unwrap();
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: true,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            memory.embedding_engine.dimension(),
            HashedNgramEmbedding::new().dimension(),
            "MemorySystem::new must not select or load a neural model; the \
             composition root injects the engine. flag=true, dim={}",
            memory.embedding_engine.dimension()
        );
    }

    #[test]
    fn test_new_with_engine_uses_injected_dimension() {
        let temp = NamedTempFile::new().unwrap();
        let memory = MemorySystem::new_with_engine(
            MemoryConfig {
                db_path: temp.path().to_path_buf(),
                use_neural_embeddings: true,
                ..Default::default()
            },
            Arc::new(FixedDimensionEngine { dimension: 4 }),
        )
        .unwrap();
        assert_eq!(
            memory.embedding_engine.dimension(),
            4,
            "injected engine dimension must parameterize the store; got {}",
            memory.embedding_engine.dimension()
        );
    }

    /// Proves `new_with_connection` builds against the connection it is
    /// handed rather than silently opening one of its own.
    ///
    /// `config.db_path` names a directory that does not exist, so
    /// `Connection::open(&config.db_path)` would fail outright -- if
    /// `new_with_connection` fell back to opening it internally, this test
    /// would fail at construction. The connection actually used is an
    /// in-memory one built here and passed in directly; a write made through
    /// the returned `MemorySystem` is then read back off that SAME
    /// `Arc<Mutex<Connection>>` from outside `MemorySystem` entirely, which a
    /// silently-self-opened connection could never produce because it would
    /// stay empty.
    #[tokio::test]
    async fn test_new_with_connection_uses_the_injected_connection_not_its_own() -> Result<()> {
        let db = Arc::new(Mutex::new(Connection::open_in_memory()?));
        let config = MemoryConfig {
            db_path: std::path::PathBuf::from("/nonexistent-dir-for-di-regression-test/memory.db"),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let memory = MemorySystem::new_with_connection(
            Arc::clone(&db),
            config,
            Arc::new(HashedNgramEmbedding::new()),
        )
        .context(
            "new_with_connection must initialize schema and build against the \
             injected connection without ever calling Connection::open itself",
        )?;

        let text = "new_with_connection round-trips through the connection it was given";
        memory.insert_conversation("user", text, None, None).await?;

        let results = memory.query_with_sources(text, Some(5)).await?;
        assert!(
            results.iter().any(|r| r.text == text),
            "the MemorySystem returned by new_with_connection could not read back \
             its own write; results={results:?}"
        );

        let stored: i64 = {
            let conn = db.lock().await;
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?
        };
        assert_eq!(
            stored, 1,
            "the conversation must be visible on the exact connection instance \
             passed into new_with_connection -- a self-opened fallback \
             connection would leave this count at 0"
        );

        Ok(())
    }

    /// Content long enough to survive the quality classifier's noise filter.
    fn substantive(tag: &str) -> String {
        format!(
            "The deploy key for the {tag} environment lives in the Employee \
             vault under the Finch signing item, not in the repository."
        )
    }

    #[tokio::test]
    async fn test_storing_identical_content_repeatedly_creates_distinct_occurrences() -> Result<()>
    {
        // Historical note: on an older revision (`insert_with_effect`'s text dedup, still a
        // tested primitive in `routing_memory.rs` but no longer reachable from this production
        // path -- see `project_stored_conversation_inner`), three inserts of one text minted one
        // shared node and this test asserted `matching == 1`. `project_stored_conversation_inner`
        // now calls `insert_occurrence` instead, which never dedups by text on purpose: two
        // occurrences of identical text are two distinct conversational moments, not the same
        // fact restated (`RoutingMemTree::insert_occurrence`'s own doc, `AGENTS.md`). This test
        // now pins the opposite: `matching` is 3, not 1, and storing identical content repeatedly
        // still never fails -- the original UNIQUE-`node_id` failure mode this test was first
        // written against is doubly gone now (`INSERT OR REPLACE` already lifted it, and every
        // occurrence's point is unique to it regardless).
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        let text = substantive("production");
        memory
            .insert_conversation("system", &text, None, None)
            .await?;

        // The same fact again, in a different conversation.
        memory
            .insert_conversation("system", &text, None, None)
            .await
            .expect("storing identical content a second time must not fail");

        // And a third time, to catch a fix that only handles the first repeat.
        memory
            .insert_conversation("system", &text, None, None)
            .await?;

        let stats = memory.stats().await?;
        assert_eq!(
            stats.conversation_count, 3,
            "every turn is still recorded in raw history"
        );

        let results = memory.query_with_sources(&text, Some(5)).await?;
        let matching: Vec<&MemorySearchResult> =
            results.iter().filter(|r| r.text == text).collect();
        assert_eq!(
            matching.len(),
            3,
            "each occurrence of the repeated fact is its own memory now, not collapsed onto one \
             (insert_occurrence never dedups by text); got {results:?}"
        );
        let distinct_nodes: std::collections::HashSet<NodeId> =
            matching.iter().map(|r| r.node_id).collect();
        assert_eq!(
            distinct_nodes.len(),
            3,
            "each occurrence must have its own distinct point/node id, not share one; \
             matching={matching:?}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_repeated_content_records_every_source_conversation() -> Result<()> {
        // Each of the three conversations now maps to its OWN node (`insert_occurrence` never
        // dedups by text -- see `test_storing_identical_content_repeatedly_creates_distinct_occurrences`),
        // so all three sources are durable regardless.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        let text = substantive("staging");
        for _ in 0..3 {
            memory
                .insert_conversation("system", &text, None, None)
                .await?;
        }

        let sources: i64 = {
            let conn = memory.db.lock().await;
            conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |row| row.get(0))?
        };
        assert_eq!(
            sources, 3,
            "each conversation that produced the memory keeps its own source row"
        );

        let distinct_nodes: i64 = {
            let conn = memory.db.lock().await;
            conn.query_row(
                "SELECT COUNT(DISTINCT node_id) FROM memory_sources",
                [],
                |row| row.get(0),
            )?
        };
        assert_eq!(
            distinct_nodes, 3,
            "each source row now points at its own node, not a shared one -- \
             insert_occurrence never dedups by text"
        );

        Ok(())
    }

    // A test previously lived here (`test_promotion_moves_provenance_to_the_
    // leaf_holding_the_text`) verifying that when a MemTree leaf was promoted
    // into a new parent, `memory_sources` followed the moved content to its
    // new node id. RoutingTree has no promotion at all -- a point's id is
    // permanent from the moment `insert` assigns it, regardless of how the
    // tree restructures around it later (`routing_memory.rs`'s own module
    // doc; `schema.sql`'s `memory_sources` comment). There is no "provenance
    // must follow a move" property left to test, since nothing ever moves.
    #[tokio::test]
    async fn test_stale_schema_is_refused_and_a_fresh_one_reopens() -> Result<()> {
        use rusqlite::Connection;

        // A database carrying the historical `memory_sources.node_id UNIQUE`
        // must be refused at open, naming the file. `CREATE TABLE IF NOT
        // EXISTS` would otherwise leave it in place and the first repeated
        // memory would fail with an opaque constraint error from inside an
        // insert.
        let temp = NamedTempFile::new()?;
        {
            let conn = Connection::open(temp.path())?;
            conn.execute_batch(
                "CREATE TABLE conversations (id TEXT PRIMARY KEY);
                 CREATE TABLE tree_nodes (node_id INTEGER PRIMARY KEY);
                 CREATE TABLE memory_sources (
                     conversation_id TEXT PRIMARY KEY,
                     node_id INTEGER UNIQUE,
                     indexed_at INTEGER NOT NULL
                 );",
            )?;
        }

        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        // `expect_err` would require `MemorySystem: Debug`, which it is not.
        let error = match MemorySystem::new(config.clone()) {
            Ok(_) => panic!("a database predating the schema change must be refused"),
            Err(error) => error,
        };
        let message = format!("{error:#}");
        assert!(
            message.contains("predates the current memory schema"),
            "the refusal must say why: {message}"
        );
        assert!(
            message.contains("mv "),
            "and must advise moving the file aside rather than deleting it: {message}"
        );

        // The guard must not fire on a database this build created. Opening a
        // fresh store twice is the case a bare substring probe would break.
        let fresh = NamedTempFile::new()?;
        let fresh_config = MemoryConfig {
            db_path: fresh.path().to_path_buf(),
            ..Default::default()
        };
        MemorySystem::new(fresh_config.clone())?;
        assert!(
            MemorySystem::new(fresh_config).is_ok(),
            "a store created by this build must reopen without tripping the guard"
        );

        Ok(())
    }

    async fn await_hydration_without_subscribing(memory: &MemorySystem) {
        // Unlike MemTree's loader (counted, THEN a separate child-linking pass,
        // THEN `complete()`), RoutingTree hydration is atomic: `loaded` is set
        // and `complete()` is called together, in the same step, with the full
        // tree already installed (`hydrate_in_background`'s own doc comment).
        // There is no intermediate "counted but not yet usable" state left to
        // wait through -- waiting for `Ready` (or a terminal failure) is the
        // whole wait.
        for i in 0..2_500 {
            match memory.hydration_status() {
                HydrationStatus::Ready { .. } => break,
                HydrationStatus::Loading { .. } => {}
                HydrationStatus::Failed { reason } => panic!("hydration failed: {reason}"),
                HydrationStatus::Degraded { reason, .. } => {
                    panic!("hydration stopped early: {reason}")
                }
            }
            assert!(i < 2_499, "hydration never reached Ready");
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    async fn await_registered_hydration_pause(
        reached: &mut watch::Receiver<bool>,
        memory: &MemorySystem,
    ) {
        if *reached.borrow_and_update() {
            return;
        }

        let wait = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                reached.changed().await.expect(
                    "the registered hydration pause disappeared before the production loader reached it",
                );
                if *reached.borrow_and_update() {
                    break;
                }
            }
        })
        .await;
        assert!(
            wait.is_ok(),
            "the production loader did not reach its registered pause within 2s; \
             status={:?}, gate_receivers={}",
            memory.hydration_status(),
            memory.hydration.done.receiver_count()
        );
    }

    /// Builds a REAL `RoutingTree` of `count` points (deterministic pseudo-random
    /// embeddings, `dim` matching whatever engine the caller's `MemoryConfig` will
    /// later reopen with) via genuine `insert()` calls, then persists it -- unlike
    /// MemTree's old raw-SQL `tree_nodes` seeding, `RoutingTree`'s structure has
    /// real geometric consistency requirements (frozen split axes, membership that
    /// must actually replay-descend correctly) that hand-crafted rows cannot
    /// safely fake. Slower than a raw INSERT loop, but the only way to produce a
    /// store `load_routing_tree` can hydrate correctly.
    /// A "real, non-trivial store" size for hydration/reload tests. Used to be
    /// sized off `HYDRATION_BATCH` (multiples of 512) to exercise MemTree's own
    /// batch-boundary behavior -- meaningless now that RoutingTree hydration is
    /// atomic, and at HashedNgramEmbedding's real 2048-dim, building hundreds+ of
    /// points via genuine `RoutingTree::insert()` in an unoptimized debug test
    /// binary is genuinely slow (confirmed live: 1024 points hung past several
    /// minutes; the same test passes in ~8s at this size). Large enough to
    /// force several real splits, small enough to stay fast.
    const SEEDED_STORE_SIZE: u64 = 50;

    /// Real cluster structure, not pure noise -- pure uniform-random points in a
    /// high-dim space have no real axis for `fraction_explained` to find, so
    /// `discrimination_gate_threshold` keeps rejecting every candidate split.
    /// `try_split` fires on every insert once a leaf exceeds `leaf_capacity`
    /// (`insertInto`'s own doc comment, inherited unmodified from the D
    /// reference), so a leaf that never successfully splits gets a full
    /// candidate-generation attempt -- O(bucket size x dim x iterations) --
    /// retried on EVERY subsequent insert into it, against an ever-growing
    /// bucket: a real O(n^2)-ish cost cliff, confirmed live (a first version of
    /// this helper used pure noise and a 1024-point seed never finished in over
    /// 30 minutes). Clustered synthetic data gives splits a real signal to find
    /// quickly, the same way real embeddings would.
    fn seed_routing_points(path: &std::path::Path, count: u64, dim: usize) -> Result<()> {
        let mut conn = Connection::open(path)?;
        // WAL mode (`MemorySystem::open_connection`'s own setup) doesn't apply
        // to this raw connection, and 2000+ individual autocommit inserts each
        // fsync separately -- genuinely slow, confirmed live (this was the
        // actual remaining bottleneck after fixing the cluster-structure issue
        // above; one wrapping transaction turned minutes into well under a
        // second). One transaction for the whole seed, matching how a real
        // caller would batch a bulk load anyway.
        let mut tree = RoutingTree::new(RoutingConfig::default(), dim, 7);
        let mut state = 0xC0FFEE_u64;
        let clusters = 8usize;
        let tx = conn.transaction()?;
        for id in 0..count {
            let mut embedding = vec![0.0f32; dim];
            embedding[(id as usize % clusters) % dim] = 3.0;
            for slot in embedding.iter_mut() {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let jitter = ((state >> 33) as f64 / u32::MAX as f64) as f32 - 0.5;
                *slot += jitter * 0.3;
            }
            let point_id = tree.insert(embedding.clone());
            save_point(
                &tx,
                point_id,
                &format!("seeded point {id}"),
                &embedding,
                1,
                id as i64,
            )?;
        }
        // `save_dirty_nodes` opens its own transaction, which would nest inside
        // `tx` (SQLite rejects that) -- `write_dirty_nodes_within` is the
        // lower-level piece meant for exactly this composition, same as
        // `save_routing_insert` above uses it.
        let dirty = tree.dirty_node_ids();
        write_dirty_nodes_within(&tree, &dirty, &tx)?;
        tx.commit()?;
        tree.mark_persisted(&dirty);
        Ok(())
    }

    async fn write_after_hydration_completes(config: MemoryConfig) -> Result<()> {
        {
            let memory = MemorySystem::new(config.clone())?;
            memory.ensure_hydrated().await?;
            for i in 0..40 {
                memory
                    .insert_conversation(
                        "system",
                        &format!(
                            "Rollout note {i}: drain the queue before restarting \
                             the worker or in-flight jobs are lost."
                        ),
                        None,
                        None,
                    )
                    .await?;
            }
        }

        let reopened = MemorySystem::new(config)?;
        await_hydration_without_subscribing(&reopened).await;

        // The write goes through `ensure_hydrated`. With `send` and a
        // discarded error the completion above stored nothing, so this awaits
        // a `changed()` that can never fire.
        let wrote = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            reopened.insert_conversation(
                "system",
                "Written after hydration finished with nobody waiting on it.",
                None,
                None,
            ),
        )
        .await;

        assert!(
            wrote.is_ok(),
            "the first write after hydration completed hung: completion did \
             not latch, so every later turn waits forever"
        );
        wrote.expect("timeout checked above")?;

        assert!(
            matches!(reopened.hydration_status(), HydrationStatus::Ready { .. }),
            "completion must be retained for a waiter that arrives after it; \
             got {:?}",
            reopened.hydration_status()
        );

        Ok(())
    }

    // Multi-thread: the only flavor that backgrounds the load. A
    // current-thread runtime now loads synchronously, so there is nothing here
    // to observe — and production runs multi-thread.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_startup_does_not_block_on_hydration() -> Result<()> {
        // #242: `MemorySystem::new` decoded every stored embedding before
        // returning — 131 MiB on the dogfood store, 3.25 s to first prompt.
        // Construction must return before the index is loaded.
        //
        // Asserted against hydration state, not against a wall-clock ratio.
        //
        // The ratio form (`backgrounded * 4 < blocking`) compared two timings
        // taken on a runner executing ~2970 tests in parallel, so both terms
        // were noisy and the quotient doubly so. It failed on an unrelated PR
        // at 11.5ms against 41.6ms -- backgrounding was 3.6x faster and needed
        // 4x, so the property held and the threshold did not.
        //
        // Asserted structurally first: did construction *delegate* the load,
        // or perform it? `hydration_task` answers that with no clock in it,
        // which is what the neighbouring
        // `test_a_current_thread_runtime_loads_without_spawning_a_loader`
        // already relies on.
        //
        // Three timing-based forms were tried here and each failed for its own
        // reason. `backgrounded * 4 < blocking` is a quotient of two wall-clock
        // measurements on a loaded runner; it failed in CI at 3.6x against a 4x
        // threshold, with the property intact. `Loading { loaded: 0, .. }` --
        // #273's original -- looks deterministic and is not: run alone, with
        // both worker threads idle, the loader reliably lands two batches
        // before this line, and it fails 5 times in 5 reporting
        // `loaded: 1024`. It survives only inside the full suite, where
        // contention starves the loader. That is a trap, not a margin.
        //
        // What remains uncovered, honestly: a construction that spawns the
        // loader and then waits on it anyway reports a spawned task and a
        // non-`Ready` status, and passes. That is a timing property with no
        // structural signature, and the ratio only caught it 48 times in 50.
        // Detecting it deterministically needs a gate the loader must pass,
        // which is a larger change than this flake fix.
        const NODES: u64 = SEEDED_STORE_SIZE;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_routing_points(temp.path(), NODES, HashedNgramEmbedding::new().dimension())?;

        // The blocking load, on a thread with no runtime.
        let blocking_config = config.clone();
        let blocking = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let memory = MemorySystem::new(blocking_config).expect("blocking construction");
            let elapsed = started.elapsed();
            assert!(
                matches!(memory.hydration_status(), HydrationStatus::Ready { .. }),
                "the no-runtime arm must finish loading before it returns"
            );
            elapsed
        })
        .join()
        .expect("blocking construction thread");

        // The backgrounded one.
        let reopened = MemorySystem::new(config)?;
        let status = reopened.hydration_status();

        assert!(
            reopened.hydration_task.is_some(),
            "construction must delegate the load to a spawned task rather than \
             perform it: no loader was spawned, and the no-runtime arm shows \
             this store takes {blocking:?} to load synchronously"
        );
        assert!(
            !matches!(status, HydrationStatus::Ready { .. }),
            "construction must return before the load finishes; it was already \
             complete when `new` returned: status {status:?} for {NODES} nodes"
        );

        // And it must still complete, with every node present.
        reopened.ensure_hydrated().await?;
        match reopened.hydration_status() {
            HydrationStatus::Ready { nodes } => assert_eq!(
                nodes as u64, NODES,
                "every persisted node must load; got {nodes}"
            ),
            other => panic!("hydration did not complete: {other:?}"),
        }

        // Unlike MemTree, there is no separate "children linked" pass to
        // verify here: `routing_nodes.is_leaf`/`left_id`/`right_id` are
        // explicit persisted columns, loaded whole by `load_routing_tree`, so
        // a `RoutingTree` node is never ambiguous the way an unlinked MemTree
        // node briefly was mid-hydration (module doc on `hydrate_in_background`).
        // The `Ready { nodes }` count above already covers the substantive
        // claim: every persisted point reloaded.
        Ok(())
    }

    /// #276. A loader that ends without finishing must open the write gate
    /// with a diagnosis, not leave it shut forever.
    ///
    /// The `watch::Sender` lives inside the `Arc<HydrationState>` that
    /// `MemorySystem` holds, so it is never dropped and `changed()` never
    /// returns `Err`. Nothing else would ever have woken a waiter.
    #[test]
    fn test_a_loader_that_ends_without_finishing_opens_the_gate_as_failed() {
        let state = Arc::new(HydrationState::new(100));
        assert!(
            matches!(state.status(), HydrationStatus::Loading { .. }),
            "precondition: the gate starts shut"
        );

        // However the loader's future ends — panic, abort, runtime shutdown —
        // the guard it owns is dropped.
        drop(HydrationGuard(Arc::clone(&state)));

        match state.status() {
            HydrationStatus::Failed { reason } => assert!(
                reason.contains("ended without finishing"),
                "the diagnosis must say the loader stopped early; got {reason:?}"
            ),
            other => panic!("the gate must open as failed; got {other:?}"),
        }
        assert!(
            *state.done.borrow(),
            "a waiter must be released, not left parked on a completion that \
             cannot arrive"
        );
    }

    /// A loader that finished is not overwritten by its own guard dropping.
    #[test]
    fn test_the_guard_does_not_disturb_a_loader_that_finished() {
        let state = Arc::new(HydrationState::new(1));
        state.loaded.store(1, Ordering::SeqCst);
        state.complete();
        drop(HydrationGuard(Arc::clone(&state)));
        assert!(
            matches!(state.status(), HydrationStatus::Ready { .. }),
            "got {:?}",
            state.status()
        );
    }

    const ABORTED_HYDRATION_REASON: &str = "the MemTree loader ended without finishing: it \
        panicked, was aborted, or its runtime shut down before it could run";
    const ABORTED_HYDRATION_WRITE_ERROR: &str = "MemTree hydration failed, so this write cannot \
        be placed: the MemTree loader ended without finishing: it panicked, was aborted, or its \
        runtime shut down before it could run";
    const PRE_POLL_ABORT_WRITE: &str = "A write after pre-poll cancellation retains raw history \
        without entering the incomplete semantic tree.";
    const MID_HYDRATION_ABORT_WRITE: &str = "A waiting write must retain raw history but never \
        enter the incomplete semantic tree.";
    const POST_READY_ABORT_WRITE: &str = "The deployment audit record remains writable after a \
        completed hydration loader is cancelled during cleanup.";

    /// #333. Cancellation before the production loader's first poll must
    /// still drop its guard and release the write gate as failed.
    ///
    /// The existing cancellation regressions deliberately stop the loader
    /// after a batch or after completion. Neither can distinguish a guard
    /// captured by the spawned future from one constructed inside it: in both
    /// cases the future has already run. Occupying the runtime's only worker
    /// makes the pre-poll ordering causal rather than scheduler-dependent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn test_loader_abort_before_first_poll_releases_write_without_semantic_commit(
    ) -> Result<()> {
        const NODES: u64 = SEEDED_STORE_SIZE;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_routing_points(temp.path(), NODES, HashedNgramEmbedding::new().dimension())?;

        // This task blocks the sole Tokio worker. The test future itself is
        // driven by Runtime::block_on on the caller thread, so it can create
        // and cancel the loader while no worker exists that could poll it.
        // Both channel waits are bounded; dropping the release sender also
        // wakes the blocker if an assertion below panics.
        let (worker_started_tx, worker_started_rx) = std::sync::mpsc::sync_channel(0);
        let (release_worker_tx, release_worker_rx) = std::sync::mpsc::channel();
        let blocker = tokio::spawn(async move {
            worker_started_tx
                .send(())
                .expect("the test thread must remain alive for the worker handshake");
            release_worker_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("the test must release the occupied Tokio worker within 5s");
        });
        worker_started_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the sole Tokio worker did not enter the blocker within 2s");

        let mut memory = MemorySystem::new(config)?;
        assert_eq!(
            memory.hydration.loaded.load(Ordering::SeqCst),
            0,
            "the occupied sole worker must prevent the production loader's first poll"
        );
        let loader = memory
            .hydration_task
            .take()
            .expect("a nonempty store on a multi-thread runtime must retain its loader handle");
        loader.abort();
        release_worker_tx
            .send(())
            .expect("the occupied Tokio worker must remain available for release");

        tokio::time::timeout(std::time::Duration::from_secs(2), blocker)
            .await
            .expect("the occupied Tokio worker did not exit within 2s after release")
            .context("the occupied Tokio worker task panicked")?;
        let loader_result = tokio::time::timeout(std::time::Duration::from_secs(2), loader)
            .await
            .expect("the pre-poll loader cancellation did not finish within 2s");
        let join_error = loader_result
            .expect_err("the pre-poll loader must terminate through the requested cancellation");
        assert!(
            join_error.is_cancelled(),
            "the pre-poll loader must report cancellation; join_error={join_error}"
        );

        match memory.hydration_status() {
            HydrationStatus::Failed { reason } => assert_eq!(
                reason, ABORTED_HYDRATION_REASON,
                "pre-poll cancellation must retain the exact guard diagnosis"
            ),
            other => panic!(
                "pre-poll cancellation must release the gate as Failed, not leave it shut; \
                 status={other:?}, loaded={}",
                memory.hydration.loaded.load(Ordering::SeqCst)
            ),
        }
        assert!(
            *memory.hydration.done.borrow(),
            "pre-poll cancellation must publish a terminal gate state"
        );

        let write_result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            memory.insert_conversation("user", PRE_POLL_ABORT_WRITE, None, None),
        )
        .await
        .expect("the production write remained stuck on the hydration gate for more than 2s");
        let error = write_result.expect_err(
            "a write after pre-poll cancellation must be refused rather than semantically committed",
        );
        assert_eq!(
            error.to_string(),
            ABORTED_HYDRATION_WRITE_ERROR,
            "the write must report the exact actionable cancellation diagnosis; status={:?}",
            memory.hydration_status()
        );
        let live_results = memory
            .query(PRE_POLL_ABORT_WRITE, Some(NODES as usize + 1))
            .await?;
        assert!(
            live_results
                .iter()
                .all(|result| result != PRE_POLL_ABORT_WRITE),
            "a refused write must not remain searchable in the live MemTree; \
             status={:?}, results={live_results:?}",
            memory.hydration_status()
        );

        let conn = Connection::open(temp.path())?;
        let raw_conversations: i64 =
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
        let semantic_sources: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |row| row.get(0))?;
        let persisted_nodes: i64 =
            conn.query_row("SELECT COUNT(*) FROM routing_points", [], |row| row.get(0))?;
        assert_eq!(
            raw_conversations, 1,
            "the refused semantic write must retain raw history for explicit \
             inspection and the pending-projection recovery tracked in #339; \
             semantic_sources={semantic_sources}, persisted_nodes={persisted_nodes}"
        );
        assert_eq!(
            semantic_sources, 0,
            "pre-poll cancellation must prevent semantic provenance from committing; \
             raw_conversations={raw_conversations}, persisted_nodes={persisted_nodes}"
        );
        assert_eq!(
            persisted_nodes, NODES as i64,
            "pre-poll cancellation must not alter the stored semantic tree; \
             raw_conversations={raw_conversations}, semantic_sources={semantic_sources}"
        );
        assert_eq!(
            memory.hydration.done.receiver_count(),
            0,
            "the completed write must leave no waiter subscribed to the terminal gate"
        );
        Ok(())
    }

    // A test previously lived here (`test_loader_abort_before_completion_
    // releases_waiter_without_semantic_commit`) that paused the production
    // loader mid-batch (`register_hydration_batch_pause`) to abort it
    // strictly between "some rows loaded" and "complete" -- a real, distinct
    // window under MemTree's batched loader. RoutingTree's
    // `hydrate_in_background` has no batch loop to pause partway through (a
    // disclosed simplification, `AGENTS.md`): it either hasn't started, or
    // has fully loaded and is about to call `complete()` (covered by
    // `test_loader_abort_after_completion_preserves_ready_state` below), or
    // hasn't been polled at all (covered by
    // `test_loader_abort_before_first_poll_releases_write_without_semantic_commit`
    // above) -- there is no longer a third, meaningfully distinct window to
    // abort within. Deleted rather than adapted, since a mid-load pause point
    // doesn't exist to adapt onto.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_loader_abort_after_completion_preserves_ready_state() -> Result<()> {
        const NODES: u64 = SEEDED_STORE_SIZE;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_routing_points(temp.path(), NODES, HashedNgramEmbedding::new().dimension())?;

        // Hold the real loader after it publishes Ready but before its future
        // can return and drop HydrationGuard. Aborting at that exact point
        // causally exercises cancellation-driven guard drop after completion.
        let (_pause_registration, mut completion_reached, _release_completion) =
            register_hydration_completion_pause(temp.path().to_path_buf());
        let mut memory = MemorySystem::new(config)?;
        let reached = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !*completion_reached.borrow_and_update() {
                completion_reached
                    .changed()
                    .await
                    .expect("the production loader dropped its completion signal");
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "the production loader did not reach its post-completion pause within 2s; \
             status={:?}",
            memory.hydration_status()
        );
        match tokio::time::timeout(std::time::Duration::from_secs(2), memory.ensure_hydrated())
            .await
        {
            Ok(result) => result.context("hydration failed at its post-completion pause")?,
            Err(_) => panic!(
                "hydration did not expose Ready within 2s at its completion pause; \
                 status={:?}, gate_receivers={}",
                memory.hydration_status(),
                memory.hydration.done.receiver_count()
            ),
        }
        assert_eq!(
            memory.hydration_status(),
            HydrationStatus::Ready {
                nodes: NODES as usize,
            },
            "the loader must publish the complete row count before the late abort"
        );

        let loader = memory
            .hydration_task
            .take()
            .expect("a store with rows must retain its paused loader handle");
        loader.abort();
        let loader_result = tokio::time::timeout(std::time::Duration::from_secs(2), loader)
            .await
            .expect("the post-completion loader cancellation must finish within 2s");
        let join_error = loader_result
            .expect_err("the paused loader must terminate through the requested cancellation");
        assert!(
            join_error.is_cancelled(),
            "the post-completion loader must report cancellation; join_error={join_error}"
        );
        assert_eq!(
            memory.hydration_status(),
            HydrationStatus::Ready {
                nodes: NODES as usize,
            },
            "a late abort must not overwrite an already completed hydration"
        );
        tokio::time::timeout(std::time::Duration::from_secs(2), memory.ensure_hydrated())
            .await
            .expect("the completed write gate must remain immediately available")
            .context("the completed write gate must remain successful after a late abort")?;

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            memory.insert_conversation("user", POST_READY_ABORT_WRITE, None, None),
        )
        .await
        .expect("the first production write after a late abort did not finish within 2s")
        .context("the first production write after a late abort must succeed")?;
        let live_results = memory
            .query(POST_READY_ABORT_WRITE, Some(NODES as usize + 2))
            .await?;
        assert!(
            live_results
                .iter()
                .any(|result| result.ends_with(POST_READY_ABORT_WRITE)),
            "the successful post-abort write must be searchable in the live MemTree; \
             status={:?}, results={live_results:?}",
            memory.hydration_status()
        );

        let conn = Connection::open(temp.path())?;
        let raw_conversations: i64 =
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
        let semantic_sources: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_sources WHERE node_id IS NOT NULL",
            [],
            |row| row.get(0),
        )?;
        let persisted_nodes: i64 =
            conn.query_row("SELECT COUNT(*) FROM routing_points", [], |row| row.get(0))?;
        assert_eq!(
            raw_conversations, 1,
            "the successful post-abort write must persist exactly one raw turn; \
             semantic_sources={semantic_sources}, persisted_nodes={persisted_nodes}"
        );
        assert_eq!(
            semantic_sources, 1,
            "the successful post-abort write must persist exactly one semantic source; \
             raw_conversations={raw_conversations}, persisted_nodes={persisted_nodes}"
        );
        assert!(
            persisted_nodes > NODES as i64,
            "the successful post-abort write must persist a semantic tree placement; \
             seeded_nodes={NODES}, persisted_nodes={persisted_nodes}, \
             raw_conversations={raw_conversations}, semantic_sources={semantic_sources}"
        );
        Ok(())
    }

    /// The synchronous arm has the same rule: a store it cannot read is
    /// broken, not fresh.
    ///
    /// A current-thread runtime and a caller with no runtime both load
    /// all-or-nothing, so a failure there is exactly the `loaded == 0` case.
    /// It used to log "starting with an empty index" and complete cleanly, so
    /// the gate opened over a full store whose tree held only the placeholder
    /// root — memories attached under a childless root and
    /// `save_all_nodes_to_db` rewrote node 0 with the placeholder's text.
    #[tokio::test]
    async fn test_an_unreadable_store_on_the_synchronous_arm_refuses_writes() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_routing_points(temp.path(), 8, HashedNgramEmbedding::new().dimension())?;

        // Break every row's `text`, so the whole load fails rather than one
        // batch of it.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE routing_points SET text = ?1",
                params![vec![0xffu8, 0xfe]],
            )?;
        }

        // `#[tokio::test]` is current-thread, which loads synchronously.
        let memory = MemorySystem::new(config)?;
        assert!(
            memory.hydration_task.is_none(),
            "precondition: this arm does not spawn a loader"
        );
        assert!(
            matches!(memory.hydration_status(), HydrationStatus::Failed { .. }),
            "a store that could not be read is broken, not empty; got {:?}",
            memory.hydration_status()
        );

        let refused = memory
            .insert_conversation(
                "system",
                "There is no structure to place this in, so it must be refused.",
                None,
                None,
            )
            .await
            .expect_err("an unreadable store must refuse writes");
        assert!(
            refused.to_string().contains("cannot be placed"),
            "got {refused}"
        );
        Ok(())
    }

    /// The two failure kinds must lead to different answers.
    ///
    /// Unlike `MemTree`'s own loader, nothing in `RoutingTree`'s hydration
    /// currently reaches `Degraded` -- a full `load_routing_tree` either
    /// succeeds outright or fails outright, a disclosed simplification
    /// (`hydrate_in_background`'s own doc comment). `Degraded` remains a real,
    /// reachable `HydrationStatus` variant in the state machine itself (a
    /// future batched RoutingTree loader could reintroduce a caller of
    /// `degrade()`), so this keeps covering ITS contract -- accepts writes,
    /// unlike `Failed` -- triggered directly, the same way the broken half
    /// below already has to be (the guard path fires when a loader is
    /// dropped, which a test cannot induce through a real load either).
    #[tokio::test]
    async fn test_a_degraded_index_accepts_writes_and_a_broken_one_does_not() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let degraded = MemorySystem::new(config.clone())?;
        degraded.ensure_hydrated().await?;
        degraded
            .hydration
            .degrade("simulated partial load".to_string());
        assert!(
            matches!(
                degraded.hydration_status(),
                HydrationStatus::Degraded { .. }
            ),
            "got {:?}",
            degraded.hydration_status()
        );
        degraded
            .insert_conversation(
                "system",
                "An index that stopped loading early is smaller than the store, \
                 not incoherent, so this must land.",
                None,
                None,
            )
            .await
            .expect("a degraded index must accept writes");

        // The broken half stays direct: the guard path fires when a loader is
        // dropped, which a test cannot induce through the batch loop.
        let clean = NamedTempFile::new()?;
        let broken = MemorySystem::new(MemoryConfig {
            db_path: clean.path().to_path_buf(),
            ..Default::default()
        })?;
        broken
            .hydration
            .fail("the loader ended without finishing".to_string());
        let refused = broken
            .insert_conversation(
                "system",
                "A tree that may be unlinked cannot place this safely.",
                None,
                None,
            )
            .await
            .expect_err("a broken index must still refuse writes");
        assert!(
            refused.to_string().contains("cannot be placed"),
            "got {refused}"
        );
        Ok(())
    }

    /// A store that has been rebuilt is working again, and must be able to say
    /// so without a restart.
    ///
    /// `failure` was write-once, so the refusal outlived whatever caused it —
    /// and a restart re-read the same rows and recorded it again, making it
    /// permanent in practice rather than merely for the session.
    #[tokio::test]
    async fn test_a_successful_reload_clears_a_recorded_failure() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        memory
            .hydration
            .fail("the loader ended without finishing".to_string());
        memory
            .insert_conversation("system", "Refused while the index is broken.", None, None)
            .await
            .expect_err("precondition: writes are refused");

        memory.reload_tree_from_db().await?;

        assert!(
            !matches!(memory.hydration_status(), HydrationStatus::Failed { .. }),
            "a rebuilt tree must not still be reported as failed; got {:?}",
            memory.hydration_status()
        );
        memory
            .insert_conversation(
                "system",
                "And a write must land once the index has been rebuilt.",
                None,
                None,
            )
            .await
            .expect("a reloaded store must accept writes again");
        Ok(())
    }

    /// A refused index write must still keep the raw turn.
    ///
    /// The gate used to sit at the top of `insert_conversation_record`, ahead
    /// of the `INSERT INTO conversations`, so refusing dropped the raw turn as
    /// well as its index entry — worse than the misplacement it was
    /// preventing, because raw history is the thing a user cannot
    /// reconstruct. It also leaves no `memory_sources` row, which is the
    /// pending-projection state
    /// `test_a_stranded_conversation_is_reprojected_when_hydration_next_succeeds`
    /// starts from.
    #[tokio::test]
    async fn test_a_refused_index_write_still_keeps_the_raw_conversation() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        // Fail hydration the way a loader that ended without finishing does.
        memory
            .hydration
            .fail("loader ended without finishing".to_string());

        let error = memory
            .insert_conversation(
                "user",
                "A turn written while the index was unusable. It is long enough \
                 for the quality classifier to want to index it.",
                None,
                Some("session-1"),
            )
            .await
            .expect_err("a failed index must refuse the placement");
        assert!(
            error.to_string().contains("cannot be placed"),
            "got {error}"
        );

        let conn = memory.db.lock().await;
        let rows: i64 =
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?;
        assert_eq!(
            rows, 1,
            "the raw turn must survive: refusing to place a memory is not a \
             reason to lose the conversation"
        );
        let sources: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |row| row.get(0))?;
        assert_eq!(
            sources, 0,
            "and it must be left unindexed, which is what makes it pending \
             projection: no memory_sources row at all, as distinct from the \
             NULL-node_id row a classifier-discarded turn gets; \
             raw_conversations={rows}"
        );
        Ok(())
    }

    // ── #339: conversations stranded by a failed hydration ───────────────────

    /// Projected successfully before hydration is broken, so the reopened store
    /// has persisted nodes to hydrate and therefore spawns a loader.
    const SEED_TURN: &str = "The Linux release runner must be ubuntu-24.04 or \
                             newer, because the ort crate needs glibc 2.38.";

    /// Stored raw while the index was unusable, and never projected.
    const STRANDED_TURN: &str = "The hydration loader ended without finishing, \
                                 so this turn was kept as raw history and never \
                                 placed in the semantic index.";

    /// `(conversations, memory_sources, routing_points)` read through a fresh
    /// connection, so it reports what is durable rather than what some
    /// in-memory tree believes.
    fn store_counts(db_path: &std::path::Path) -> Result<(i64, i64, i64)> {
        let conn = Connection::open(db_path)?;
        Ok((
            conn.query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))?,
            conn.query_row("SELECT COUNT(*) FROM memory_sources", [], |row| row.get(0))?,
            conn.query_row("SELECT COUNT(*) FROM routing_points", [], |row| row.get(0))?,
        ))
    }

    /// The three states a stored conversation can be in, as the durable rows
    /// report them.
    ///
    /// `None` — no `memory_sources` row at all: pending projection.
    /// `Some(None)` — the classifier discarded it: terminal, never pending.
    /// `Some(Some(node))` — projected onto that semantic leaf.
    fn source_row(db_path: &std::path::Path, conversation_id: &str) -> Result<Option<Option<i64>>> {
        let conn = Connection::open(db_path)?;
        let mut stmt =
            conn.prepare("SELECT node_id FROM memory_sources WHERE conversation_id = ?1")?;
        let mut rows = stmt.query([conversation_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(row.get::<_, Option<i64>>(0)?)),
            None => Ok(None),
        }
    }

    fn conversation_id_for_session(db_path: &std::path::Path, session: &str) -> Result<String> {
        let conn = Connection::open(db_path)?;
        Ok(conn.query_row(
            "SELECT id FROM conversations WHERE session_id = ?1",
            [session],
            |row| row.get(0),
        )?)
    }

    /// The conversation id whose content matches exactly -- used by the occurrence-chain tests
    /// below, which need to name a specific turn within a session that has more than one, where
    /// `conversation_id_for_session` above (unordered over however many rows match) cannot.
    fn conversation_id_for_content(db_path: &std::path::Path, content: &str) -> Result<String> {
        let conn = Connection::open(db_path)?;
        Ok(conn.query_row(
            "SELECT id FROM conversations WHERE content = ?1",
            [content],
            |row| row.get(0),
        )?)
    }

    /// (uuid, prev_uuid, next_uuid) -- named so the occurrence-chain tests below don't repeat
    /// clippy's `type_complexity`-triggering nested tuple type at every call site.
    type OccurrenceRow = (String, Option<String>, Option<String>);

    /// The `routing_occurrences` row for the point this conversation was projected onto, read
    /// through a fresh connection so it reports what is durable. `None` when the conversation has
    /// no `memory_sources` row yet, was classifier-discarded (NULL `node_id`), or -- should never
    /// happen for anything projected through `project_stored_conversation_inner`'s production
    /// path -- its point has no occurrence row.
    fn occurrence_row_for_conversation(
        db_path: &std::path::Path,
        conversation_id: &str,
    ) -> Result<Option<OccurrenceRow>> {
        let conn = Connection::open(db_path)?;
        conn.query_row(
            "SELECT ro.uuid, ro.prev_uuid, ro.next_uuid
             FROM memory_sources ms
             JOIN routing_occurrences ro ON ro.point_id = ms.node_id
             WHERE ms.conversation_id = ?1",
            [conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
    }

    /// Reproduce the #339 state and leave the store closed.
    ///
    /// One turn projected normally — which is what gives the reopened store
    /// persisted nodes to hydrate — then hydration is failed the way a loader
    /// that ended without finishing fails it, and a second substantive turn is
    /// written. That second write keeps its raw `conversations` row (#276) and
    /// gets no `memory_sources` row: stored, never projected, and before this
    /// change nothing ever looked at it again.
    async fn strand_a_conversation(config: &MemoryConfig) -> Result<()> {
        let memory = MemorySystem::new(config.clone())?;
        memory
            .insert_conversation("user", SEED_TURN, None, Some("seed"))
            .await
            .context("the seeding turn must project normally")?;
        memory
            .hydration
            .fail("loader ended without finishing".to_string());
        let refused = memory
            .insert_conversation("user", STRANDED_TURN, None, Some("stranded"))
            .await
            .expect_err("precondition: a broken index must refuse the placement");
        assert!(
            refused.to_string().contains("cannot be placed"),
            "precondition: the refusal must come from the hydration gate; got {refused}"
        );

        let counts = store_counts(&config.db_path)?;
        let stranded = conversation_id_for_session(&config.db_path, "stranded")?;
        assert_eq!(
            (counts.0, counts.1),
            (2, 1),
            "precondition: both raw turns stored, only the seeded one projected; \
             counts=(conversations, memory_sources, routing_points)={counts:?}"
        );
        assert_eq!(
            source_row(&config.db_path, &stranded)?,
            None,
            "precondition: the stranded turn must have NO memory_sources row, \
             which is what distinguishes it from a discarded one; \
             stranded={stranded}, counts={counts:?}"
        );
        Ok(())
    }

    /// The defect: a turn stored while hydration was broken stayed unprojected
    /// forever.
    ///
    /// Nothing scanned for `conversations` rows without a `memory_sources` row,
    /// and an ordinary retry mints a fresh UUID, so it cannot repair the
    /// original row. Reopening the store is the moment the repair is possible,
    /// and this asserts it now happens there.
    ///
    /// The loader's `JoinHandle` is the synchronisation: the sweep runs inside
    /// the loader future, so the handle resolving means the sweep has finished.
    /// No polling and no sleep.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_a_stranded_conversation_is_reprojected_when_hydration_next_succeeds() -> Result<()>
    {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        strand_a_conversation(&config).await?;
        let stranded = conversation_id_for_session(temp.path(), "stranded")?;
        let before = store_counts(temp.path())?;

        let mut reopened = MemorySystem::new(config)?;
        let loader = reopened.hydration_task.take().expect(
            "a reopened store with persisted nodes must spawn a loader on a \
             multi-threaded runtime",
        );
        loader
            .await
            .expect("the loader must finish rather than panic or be cancelled");

        let after = store_counts(temp.path())?;
        let projected = source_row(temp.path(), &stranded)?;
        assert!(
            matches!(projected, Some(Some(_))),
            "the stranded turn must be projected onto a semantic leaf when \
             hydration next succeeds; source_row={projected:?}, \
             status={:?}, before=(conv, sources, nodes)={before:?}, after={after:?}, \
             stranded={stranded}",
            reopened.hydration_status()
        );
        assert_eq!(
            after.0, before.0,
            "recovery must not duplicate raw conversations; \
             before={before:?}, after={after:?}, stranded={stranded}"
        );
        assert_eq!(
            after.1,
            before.1 + 1,
            "exactly one source mapping must be added, for the one pending turn; \
             before={before:?}, after={after:?}, stranded={stranded}"
        );

        let results = reopened.query(STRANDED_TURN, Some(8)).await?;
        assert!(
            results.iter().any(|result| result.ends_with(STRANDED_TURN)),
            "the repaired turn must be reachable by semantic recall, not merely \
             carry a database row; results={results:?}, source_row={projected:?}, \
             status={:?}",
            reopened.hydration_status()
        );

        let again = reopened.recover_pending_projections().await?;
        assert_eq!(
            again,
            0,
            "the repaired turn must have left the pending set, so a second sweep \
             finds nothing; after={after:?}, now={:?}, stranded={stranded}",
            store_counts(temp.path())?
        );
        Ok(())
    }

    /// Repair is exactly-once, and that is a property of the predicate rather
    /// than of a flag.
    ///
    /// `recover_pending_projections` deliberately ignores the "sweep owed"
    /// bookkeeping the automatic hooks consult, so this really re-runs the
    /// query. A row leaves the pending set only when its `memory_sources` row
    /// commits, and that commits in the same SQLite transaction as its semantic
    /// leaf — so a second pass can neither re-place the turn nor mint a second
    /// leaf for it.
    ///
    /// Current-thread on purpose: `new` hydrates synchronously and spawns no
    /// loader, so every sweep here is one this test asked for.
    #[tokio::test]
    async fn test_reprojecting_a_stranded_conversation_happens_exactly_once() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        strand_a_conversation(&config).await?;
        let stranded = conversation_id_for_session(temp.path(), "stranded")?;

        let reopened = MemorySystem::new(config)?;
        assert!(
            reopened.hydration_task.is_none(),
            "precondition: the current-thread arm loads synchronously and spawns \
             no loader, so no sweep can run behind this test's back"
        );
        assert!(
            matches!(reopened.hydration_status(), HydrationStatus::Ready { .. }),
            "precondition: the reopened store must hydrate cleanly, or there is \
             nothing to repair into; got {:?}",
            reopened.hydration_status()
        );

        let first = reopened.recover_pending_projections().await?;
        let after_first = store_counts(temp.path())?;
        let node_after_first = source_row(temp.path(), &stranded)?;
        assert_eq!(
            first, 1,
            "the one pending turn must be repaired; counts=(conv, sources, nodes)\
             ={after_first:?}, source_row={node_after_first:?}, stranded={stranded}"
        );
        assert!(
            matches!(node_after_first, Some(Some(_))),
            "and repaired means attributed to a leaf; source_row={node_after_first:?}, \
             counts={after_first:?}, stranded={stranded}"
        );

        let second = reopened.recover_pending_projections().await?;
        let after_second = store_counts(temp.path())?;
        let node_after_second = source_row(temp.path(), &stranded)?;
        assert_eq!(
            second, 0,
            "a repeated sweep must find nothing pending; \
             after_first={after_first:?}, after_second={after_second:?}, \
             stranded={stranded}"
        );
        assert_eq!(
            after_second, after_first,
            "and must change no row counts: (conversations, memory_sources, \
             routing_points); stranded={stranded}"
        );
        assert_eq!(
            node_after_second, node_after_first,
            "and must not re-point the repaired turn at a second leaf; \
             counts={after_second:?}, stranded={stranded}"
        );
        Ok(())
    }

    /// A turn the quality classifier discarded is terminal, not pending.
    ///
    /// This is what makes the pending predicate unambiguous: a discarded turn
    /// gets a `memory_sources` row with `node_id = NULL`, so "no row at all"
    /// can only mean "projection never ran". Stop writing that row and every
    /// low-signal turn becomes permanently pending, and the sweep re-classifies
    /// the whole backlog on every startup.
    #[tokio::test]
    async fn test_a_classifier_discarded_conversation_is_not_pending_projection() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        // Under the classifier's 20-character floor, so it is discarded.
        memory
            .insert_conversation("user", "ok", None, Some("noise"))
            .await?;
        let discarded = conversation_id_for_session(temp.path(), "noise")?;

        let row = source_row(temp.path(), &discarded)?;
        let terminal: Option<Option<i64>> = Some(None);
        assert_eq!(
            row, terminal,
            "a discarded turn must be recorded terminal, as a memory_sources row \
             with a NULL node_id; got {row:?} for {discarded}"
        );

        let pending = MemorySystem::pending_projections(&memory.db, None).await?;
        assert!(
            pending.is_empty(),
            "a discarded turn must not look pending, or every low-signal turn is \
             reprojected on every startup; pending={pending:?}, discarded={discarded}"
        );

        let before = store_counts(temp.path())?;
        let swept = memory.recover_pending_projections().await?;
        let after = store_counts(temp.path())?;
        assert_eq!(
            swept, 0,
            "and a sweep must repair nothing; before=(conv, sources, nodes)\
             ={before:?}, after={after:?}, discarded={discarded}"
        );
        assert_eq!(
            after, before,
            "nor add a semantic leaf for content the classifier rejected; \
             discarded={discarded}"
        );
        Ok(())
    }

    /// Written straight into `conversations` with a timestamp this test picks,
    /// so a backlog's replay order is observable rather than incidental.
    const BACKLOG_EARLY_ID: &str = "zzz-spoken-first";
    const BACKLOG_MIDDLE_ID: &str = "mmm-spoken-second";
    const BACKLOG_LATE_ID: &str = "aaa-spoken-third";
    /// Deliberately the reverse of the id ordering above.
    const BACKLOG_EARLY_AT: i64 = 1_000;
    const BACKLOG_MIDDLE_AT: i64 = 2_000;
    const BACKLOG_LATE_AT: i64 = 3_000;

    const BACKLOG_EARLY_TURN: &str = "Intel macOS is not a supported release \
                                      target because the ort crate ships no \
                                      prebuilt binaries for it.";
    const BACKLOG_MIDDLE_TURN: &str = "Dialogs in the terminal interface are \
                                       drawn full width with no side borders, \
                                       and that keeps regressing.";
    const BACKLOG_LATE_TURN: &str = "Feedback is private durable data: a rating \
                                     is never consent to train anything on it.";

    /// A second stranded turn for the cancellation case, timestamped after the
    /// one `strand_a_conversation` leaves behind so the sweep's ordering fixes
    /// which of the two it repairs first.
    const SECOND_STRANDED_ID: &str = "second-stranded-turn";
    const SECOND_STRANDED_AT: i64 = 9_000_000_000_000_000_000;
    const SECOND_STRANDED_TURN: &str = "This turn was still waiting in the \
                                        backlog when the loader was cancelled \
                                        part way through repairing it.";

    /// A live turn written after the store reopened, so a repair and an
    /// ordinary projection can be told apart.
    const FRESH_TURN: &str = "Every bug fix needs a regression test that fails \
                              before the fix and passes after it.";

    /// The turn used for the named-Brain cases, which have a deterministic
    /// conversation id and can therefore address the row a previous attempt
    /// stranded.
    const BRAIN_TURN: &str = "The named-Brain runner retried this turn after a \
                              broken semantic index refused its first placement.";

    fn brain_provenance() -> BrainConversationProvenance {
        BrainConversationProvenance {
            brain_id: "reproject-brain".to_string(),
            run_id: "run-339".to_string(),
            request_seq: 1,
        }
    }

    /// The text of one semantic leaf, read durably.
    /// The text of the node a mapping points at, asserting that node is a leaf.
    ///
    /// The leaf check is load-bearing, not decoration. An internal node carries
    /// a provisional label duplicated from one of its children, so a mapping
    /// repointed to a parent that inherited the label would satisfy a bare text
    /// comparison while the turn was no longer placed as its own memory. The
    /// assertions that call this say "resolves to a leaf holding its own text";
    /// without this check they would only be asserting the second half.
    /// `node_id` here is a `routing_points.point_id` (`memory_sources.node_id`'s
    /// real meaning now, schema.sql's own comment). No "is this a leaf, not an
    /// internal aggregate" check is needed the way MemTree's version needed
    /// one: a `RoutingTree` point is never an internal/aggregate anything --
    /// `routing_points` only ever holds real, individually-inserted content.
    fn node_text(db_path: &std::path::Path, node_id: i64) -> Result<String> {
        let conn = Connection::open(db_path)?;
        Ok(conn.query_row(
            "SELECT text FROM routing_points WHERE point_id = ?1",
            [node_id],
            |row| row.get(0),
        )?)
    }

    /// Write raw `conversations` rows with no `memory_sources` row — the exact
    /// durable shape a failed hydration leaves behind — with ids and timestamps
    /// this test chooses.
    ///
    /// `insert_conversation` cannot produce that shape to order: it mints a
    /// UUID and stamps `Utc::now()`, so id order and timestamp order always
    /// agree and neither can be made to disagree with the other. Pinning
    /// `ORDER BY c.timestamp ASC` needs them to disagree.
    fn strand_rows_directly(db_path: &std::path::Path, rows: &[(&str, i64, &str)]) -> Result<()> {
        let conn = Connection::open(db_path)?;
        for &(id, timestamp, content) in rows {
            conn.execute(
                "INSERT INTO conversations
                 (id, timestamp, role, content, tokens, model, session_id,
                  brain_id, run_id, request_seq, created_at)
                 VALUES (?1, ?2, 'user', ?3, NULL, NULL, NULL, NULL, NULL, NULL, ?2)",
                params![id, timestamp, content],
            )?;
            let mapped: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_sources WHERE conversation_id = ?1",
                [id],
                |row| row.get(0),
            )?;
            assert_eq!(
                mapped, 0,
                "a directly stranded row must carry no memory_sources row, or it \
                 is not pending projection at all and this fixture proves nothing; \
                 id={id}, timestamp={timestamp}"
            );
        }
        Ok(())
    }

    /// As `strand_a_conversation`, for a turn with a DETERMINISTIC identity.
    ///
    /// An ordinary retry mints a fresh UUID and so can never address the row a
    /// previous attempt stranded. A named-Brain retry can, and that is the only
    /// case in which the `?1` exclusion in `PENDING_PROJECTION_SQL` does
    /// anything at all. Returns the stranded conversation id.
    async fn strand_a_brain_conversation(config: &MemoryConfig) -> Result<String> {
        let memory = MemorySystem::new(config.clone())?;
        memory
            .insert_conversation("user", SEED_TURN, None, Some("seed"))
            .await
            .context("the seeding turn must project normally")?;
        memory
            .hydration
            .fail("loader ended without finishing".to_string());
        let provenance = brain_provenance();
        let refused = memory
            .insert_brain_conversation("user", BRAIN_TURN, None, Some("brain"), &provenance)
            .await
            .expect_err("precondition: a broken index must refuse the placement");
        assert!(
            refused.to_string().contains("cannot be placed"),
            "precondition: the refusal must come from the hydration gate; got {refused}"
        );
        let id = format!(
            "brain:{}:run:{}:role:user",
            provenance.brain_id, provenance.run_id
        );
        let counts = store_counts(&config.db_path)?;
        assert_eq!(
            source_row(&config.db_path, &id)?,
            None,
            "precondition: the stranded Brain turn must have NO memory_sources \
             row, which is what makes it pending; id={id}, \
             counts=(conversations, memory_sources, routing_points)={counts:?}"
        );
        assert_eq!(
            (counts.0, counts.1),
            (2, 1),
            "precondition: both raw turns stored, only the seeded one projected; \
             id={id}, counts={counts:?}"
        );
        Ok(id)
    }

    /// An embedding engine that can be switched to failing.
    ///
    /// A neural engine's `embed` returns `Err` on a tokenizer or ONNX fault, so
    /// the `?` at the top of `project_stored_conversation` is a reachable
    /// production exit. Nothing else in this suite can reach it.
    struct FailableEmbedding {
        inner: HashedNgramEmbedding,
        fail: AtomicBool,
    }

    impl EmbeddingEngine for FailableEmbedding {
        fn embed(&self, text: &str) -> Result<Vec<f32>> {
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("embedding engine unavailable");
            }
            self.inner.embed(text)
        }

        fn dimension(&self) -> usize {
            self.inner.dimension()
        }
    }

    /// Cancelling the loader WHILE THE SWEEP IS RUNNING must leave the backlog
    /// exactly-once: what committed stays committed, nothing is half-applied,
    /// and what had not run is still pending for the next start.
    ///
    /// The seam is inside the sweep loop, and that is the point of this
    /// version. The previous one aborted at `pause_after_completion`, which is
    /// the last statement of `hydrate_batches` — strictly before
    /// `hydrate_in_background` calls the sweep at all. Its three cancellation
    /// assertions therefore held for every possible implementation, including
    /// one with no sweep: deleting `Self::sweep_pending_projections(&projection)`
    /// left the test passing. It now hangs the loader between the first and
    /// second rows of a two-row backlog, so a missing sweep, or one that stops
    /// after a single row, never reaches the pause.
    ///
    /// What cancellation can and cannot split. `write_nodes` is synchronous, so
    /// a semantic leaf and its `memory_sources` row are one SQLite transaction
    /// and cannot be split from each other. `save_routing_occurrence` now
    /// acquires both locks (`db.lock()`, `tree_lock.lock()`) BEFORE calling
    /// `tree.insert_occurrence` — the tree mutation that used to happen
    /// separately, ahead of those two awaits — so there is no longer an await
    /// point between the mutation and the transaction it is committed in
    /// inside that one function. A cancellation can still land between the two
    /// lock acquisitions themselves, before any mutation has happened at all,
    /// which is a no-op to cancel. The conclusion survives for a reason
    /// outside `project_stored_conversation`:
    /// the only cancellation of the sweep in production is `Drop for
    /// MemorySystem` aborting the loader, and that drop tears the tree down
    /// along with the task, so the orphan leaf dies with it and nothing durable
    /// was written. Anyone adding an await between those two points for some
    /// other caller has to revisit that.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_cancelling_the_loader_mid_sweep_leaves_no_partial_projection() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        strand_a_conversation(&config).await?;
        let first_stranded = conversation_id_for_session(temp.path(), "stranded")?;
        strand_rows_directly(
            temp.path(),
            &[(SECOND_STRANDED_ID, SECOND_STRANDED_AT, SECOND_STRANDED_TURN)],
        )?;
        let before = store_counts(temp.path())?;
        assert_eq!(
            (before.0, before.1),
            (3, 1),
            "precondition: three raw turns with only the seeded one projected, so \
             the sweep has a two-row backlog to be interrupted in the middle of; \
             counts=(conversations, memory_sources, routing_points)={before:?}, \
             first_stranded={first_stranded}"
        );

        // Hang the sweep once exactly one backlog row has committed.
        let (_sweep_registration, mut sweep_reached, release_sweep) =
            register_projection_sweep_pause(temp.path().to_path_buf(), 1);
        let mut reopened = MemorySystem::new(config)?;
        let reached = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !*sweep_reached.borrow_and_update() {
                sweep_reached
                    .changed()
                    .await
                    .expect("the production sweep dropped its pause signal");
            }
        })
        .await;
        assert!(
            reached.is_ok(),
            "the loader's sweep never began a second backlog row, so nothing below \
             is a statement about cancelling a sweep: either the loader does not \
             sweep at all, or the sweep stops after one row; status={:?}, \
             before={before:?}, counts={:?}, first_stranded={first_stranded}",
            reopened.hydration_status(),
            store_counts(temp.path())?
        );

        let loader = reopened.hydration_task.take().expect(
            "a reopened store with persisted nodes must spawn a loader on a \
             multi-threaded runtime",
        );
        loader.abort();
        let join_error = tokio::time::timeout(std::time::Duration::from_secs(10), loader)
            .await
            .expect("the cancelled loader must finish")
            .expect_err("the paused loader must terminate through the cancellation");
        assert!(
            join_error.is_cancelled(),
            "the loader must report cancellation rather than a panic; \
             join_error={join_error}"
        );
        // Nothing is parked on the pause any more. Release it so the resumed
        // sweep below cannot block on a seam this test is finished with.
        release_sweep.send_replace(true);

        let after_abort = store_counts(temp.path())?;
        let first_row = source_row(temp.path(), &first_stranded)?;
        let second_row = source_row(temp.path(), SECOND_STRANDED_ID)?;
        assert!(
            matches!(first_row, Some(Some(_))),
            "the backlog row the sweep committed BEFORE the cancellation must stay \
             committed — its leaf and its mapping are one transaction, so a \
             cancellation cannot unpick it; first_row={first_row:?}, \
             second_row={second_row:?}, before={before:?}, after_abort={after_abort:?}"
        );
        assert_eq!(
            second_row, None,
            "and the row the sweep had not reached must be left wholly pending, \
             not half-attributed; second_row={second_row:?}, \
             first_row={first_row:?}, before={before:?}, after_abort={after_abort:?}"
        );
        assert_eq!(
            (after_abort.0, after_abort.1),
            (before.0, before.1 + 1),
            "exactly one mapping may exist for the one row the sweep finished, and \
             no raw conversation may be duplicated: (conversations, \
             memory_sources); before={before:?}, after_abort={after_abort:?}, \
             first_row={first_row:?}, second_row={second_row:?}"
        );

        // Restart. The repair the cancellation interrupted must resume, exactly
        // once, without redoing the row that already committed.
        let repaired = reopened.recover_pending_projections().await?;
        let after_repair = store_counts(temp.path())?;
        assert_eq!(
            repaired, 1,
            "the restart must repair exactly the row the cancellation left pending; \
             after_abort={after_abort:?}, after_repair={after_repair:?}, \
             first_row={first_row:?}"
        );
        let second_after_repair = source_row(temp.path(), SECOND_STRANDED_ID)?;
        assert!(
            matches!(second_after_repair, Some(Some(_))),
            "and must attribute it to a leaf; second_row={second_after_repair:?}, \
             after_repair={after_repair:?}"
        );
        // Not "the node id is unchanged": inserting the second turn may promote
        // the first one's leaf, and `write_nodes` deliberately carries the
        // mapping to the leaf that still holds the words
        // (`test_promotion_moves_provenance_to_the_leaf_holding_the_text`). What
        // must hold is that the turn still resolves to its own text through
        // exactly one mapping — the count assertion below covers the "exactly
        // one" half.
        let first_after_repair = source_row(temp.path(), &first_stranded)?;
        let first_leaf_text = first_after_repair
            .flatten()
            .map(|node| node_text(temp.path(), node))
            .transpose()?;
        assert_eq!(
            first_leaf_text.as_deref(),
            Some(STRANDED_TURN),
            "and the already-committed row must still resolve to the leaf holding \
             its own text, rather than being re-placed as a second memory; \
             at_abort={first_row:?}, after_repair_row={first_after_repair:?}, \
             after_repair={after_repair:?}, first_stranded={first_stranded}"
        );
        assert_eq!(
            (after_repair.0, after_repair.1),
            (before.0, before.1 + 2),
            "with one mapping per stranded turn and no duplicate raw rows; \
             before={before:?}, after_abort={after_abort:?}, \
             after_repair={after_repair:?}"
        );

        let again = reopened.recover_pending_projections().await?;
        assert_eq!(
            again,
            0,
            "with no post-terminal effect from running the repair once more; \
             after_repair={after_repair:?}, now={:?}",
            store_counts(temp.path())?
        );
        assert_eq!(
            store_counts(temp.path())?,
            after_repair,
            "and no rows added by that second pass; after_repair={after_repair:?}"
        );
        Ok(())
    }

    /// The sweep walks the WHOLE backlog, in the order the turns were spoken,
    /// mapping each row onto its own leaf.
    ///
    /// Nothing pinned any of that. `strand_a_conversation` strands exactly one
    /// row, so the loop body never ran twice: `ORDER BY c.timestamp ASC`, the
    /// `repaired` counter, and per-row mapping were all free to be anything.
    /// Replacing the loop with `pending.iter().take(1)` changed no test.
    ///
    /// Three rows whose id order is deliberately the reverse of their timestamp
    /// order, so ordering by timestamp is observable rather than incidental.
    /// The sweep is held between its first and second rows and what has
    /// committed at that instant is read through a fresh connection.
    #[tokio::test]
    async fn test_a_backlog_is_reprojected_in_timestamp_order_row_by_row() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let (_sweep_registration, mut sweep_reached, release_sweep) =
            register_projection_sweep_pause(temp.path().to_path_buf(), 1);
        let memory = MemorySystem::new(config)?;
        assert!(
            memory.hydration_task.is_none(),
            "precondition: the current-thread arm spawns no loader, so every sweep \
             here is one this test asked for"
        );
        memory
            .insert_conversation("user", SEED_TURN, None, Some("seed"))
            .await
            .context("the seeding turn must project normally")?;
        strand_rows_directly(
            temp.path(),
            &[
                (BACKLOG_LATE_ID, BACKLOG_LATE_AT, BACKLOG_LATE_TURN),
                (BACKLOG_EARLY_ID, BACKLOG_EARLY_AT, BACKLOG_EARLY_TURN),
                (BACKLOG_MIDDLE_ID, BACKLOG_MIDDLE_AT, BACKLOG_MIDDLE_TURN),
            ],
        )?;
        let before = store_counts(temp.path())?;
        assert_eq!(
            (before.0, before.1),
            (4, 1),
            "precondition: four raw turns with only the seeded one projected; \
             counts=(conversations, memory_sources, routing_points)={before:?}"
        );

        // The sweep and an observer of it, on one task: `join!` polls the probe
        // while the sweep is parked at its seam. The timeout is a liveness
        // bound on reaching a second row, never an assertion about how long
        // anything takes.
        let sweep = memory.recover_pending_projections();
        let probe = async {
            let reached = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !*sweep_reached.borrow_and_update() {
                    sweep_reached
                        .changed()
                        .await
                        .expect("the production sweep dropped its pause signal");
                }
            })
            .await
            .is_ok();
            let midpoint = if reached {
                Some((
                    source_row(temp.path(), BACKLOG_EARLY_ID),
                    source_row(temp.path(), BACKLOG_MIDDLE_ID),
                    source_row(temp.path(), BACKLOG_LATE_ID),
                ))
            } else {
                None
            };
            release_sweep.send_replace(true);
            (reached, midpoint)
        };
        let (repaired, (reached, midpoint)) = tokio::join!(sweep, probe);
        assert!(
            reached,
            "the sweep never began a second backlog row, so it does not walk a \
             backlog at all; repaired={repaired:?}, before={before:?}, counts={:?}",
            store_counts(temp.path())?
        );
        let (early_mid, middle_mid, late_mid) =
            midpoint.expect("the midpoint snapshot is taken whenever the pause is reached");
        let (early_mid, middle_mid, late_mid) = (early_mid?, middle_mid?, late_mid?);
        assert!(
            matches!(early_mid, Some(Some(_))),
            "the EARLIEST-spoken turn must be the row the sweep projects first: \
             `ORDER BY c.timestamp ASC` is what replays a backlog in the order it \
             was actually spoken, and the ids here sort the other way round; \
             early={early_mid:?}, middle={middle_mid:?}, late={late_mid:?}, \
             before={before:?}"
        );
        assert_eq!(
            (middle_mid, late_mid),
            (None, None),
            "and it must be the only row committed at that instant, or the sweep \
             is not committing row by row; early={early_mid:?}, \
             middle={middle_mid:?}, late={late_mid:?}, before={before:?}"
        );

        let repaired = repaired?;
        let after = store_counts(temp.path())?;
        assert_eq!(
            repaired, 3,
            "every stranded row must be repaired and counted, not just the first; \
             before={before:?}, after=(conversations, memory_sources, \
             routing_points)={after:?}"
        );
        assert_eq!(
            (after.0, after.1),
            (before.0, before.1 + 3),
            "one new mapping per stranded turn and no duplicated raw rows; \
             repaired={repaired}, before={before:?}, after={after:?}"
        );

        let mut placed: Vec<(&str, i64)> = Vec::new();
        for (id, content) in [
            (BACKLOG_EARLY_ID, BACKLOG_EARLY_TURN),
            (BACKLOG_MIDDLE_ID, BACKLOG_MIDDLE_TURN),
            (BACKLOG_LATE_ID, BACKLOG_LATE_TURN),
        ] {
            let row = source_row(temp.path(), id)?;
            let Some(Some(node)) = row else {
                panic!(
                    "every stranded row must end mapped to a semantic leaf, not \
                     only the first one the loop reached; id={id}, \
                     source_row={row:?}, repaired={repaired}, after={after:?}, \
                     placed={placed:?}"
                );
            };
            assert_eq!(
                node_text(temp.path(), node)?,
                content,
                "and each row must be mapped to the leaf holding ITS OWN text, so \
                 the loop carries its row through rather than reusing one; \
                 id={id}, node={node}, repaired={repaired}, after={after:?}, \
                 placed={placed:?}"
            );
            placed.push((id, node));
        }
        assert_eq!(
            placed
                .iter()
                .map(|(_, node)| *node)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "three distinct turns must not collapse onto one leaf; placed={placed:?}, \
             after={after:?}"
        );

        let again = memory.recover_pending_projections().await?;
        assert_eq!(
            again,
            0,
            "and the whole repaired backlog must have left the pending set; \
             after={after:?}, now={:?}, placed={placed:?}",
            store_counts(temp.path())?
        );
        Ok(())
    }

    /// The write path's own hook repairs the backlog.
    ///
    /// The loader's hook covers reopening the store. This covers the other one,
    /// which is the only hook the current-thread arm of `new` has at all: that
    /// arm hydrates synchronously and spawns no task to hang a sweep on. No
    /// test reached `sweep_pending_projections_if_owed` through
    /// `insert_conversation` before this one — deleting the call from
    /// `insert_conversation_record` changed nothing.
    #[tokio::test]
    async fn test_the_next_ordinary_write_reprojects_a_stranded_conversation() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        strand_a_conversation(&config).await?;
        let stranded = conversation_id_for_session(temp.path(), "stranded")?;
        let before = store_counts(temp.path())?;

        let reopened = MemorySystem::new(config)?;
        assert!(
            reopened.hydration_task.is_none(),
            "precondition: the current-thread arm spawns no loader, so the only \
             sweep that can run here is the write path's own"
        );
        reopened
            .insert_conversation("user", FRESH_TURN, None, Some("fresh"))
            .await
            .context("the live turn must be accepted")?;

        let after = store_counts(temp.path())?;
        let repaired = source_row(temp.path(), &stranded)?;
        let fresh = conversation_id_for_session(temp.path(), "fresh")?;
        let fresh_row = source_row(temp.path(), &fresh)?;
        assert!(
            matches!(repaired, Some(Some(_))),
            "an ordinary write must repair the turns stranded before it — this is \
             the only repair a process that never reopens its store ever gets; \
             stranded_row={repaired:?}, fresh_row={fresh_row:?}, before={before:?}, \
             after=(conversations, memory_sources, routing_points)={after:?}, \
             stranded={stranded}"
        );
        assert!(
            matches!(fresh_row, Some(Some(_))),
            "and must still project its own turn; fresh_row={fresh_row:?}, \
             stranded_row={repaired:?}, after={after:?}, fresh={fresh}"
        );
        assert_ne!(
            repaired.flatten(),
            fresh_row.flatten(),
            "onto a leaf of its own rather than the repaired turn's; \
             after={after:?}, stranded={stranded}, fresh={fresh}"
        );
        assert_eq!(
            (after.0, after.1),
            (before.0 + 1, before.1 + 2),
            "one new raw turn, and two new mappings — the repair and the live \
             turn; before={before:?}, after={after:?}"
        );
        let again = reopened.recover_pending_projections().await?;
        assert_eq!(
            again,
            0,
            "with nothing left pending afterwards; after={after:?}, now={:?}",
            store_counts(temp.path())?
        );
        Ok(())
    }

    /// A named-Brain retry projects its OWN stranded row rather than letting
    /// the sweep do it, and reports that it did.
    ///
    /// This is the only thing the `?1` exclusion in `PENDING_PROJECTION_SQL`
    /// buys, and nothing pinned it. Without the exclusion the sweep repairs the
    /// row first, the retry then takes the `already_classified` short-circuit,
    /// and `insert_brain_conversation` returns `Ok(false)` — read as "identical
    /// retry, nothing to do" — for the call that actually caused the turn to be
    /// indexed. The consequence that matters is the failing case: the sweep
    /// swallows its errors into a `tracing::warn!`, so without the exclusion a
    /// projection that FAILED would reach the caller as `Ok(false)` rather than
    /// as an error.
    #[tokio::test]
    async fn test_a_brain_retry_projects_its_own_stranded_turn() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let brain_turn_id = strand_a_brain_conversation(&config).await?;
        let before = store_counts(temp.path())?;

        let reopened = MemorySystem::new(config)?;
        assert!(
            reopened.hydration_task.is_none(),
            "precondition: the current-thread arm spawns no loader, so the retry's \
             own sweep is the only one that can run"
        );
        let performed = reopened
            .insert_brain_conversation("user", BRAIN_TURN, None, Some("brain"), &brain_provenance())
            .await
            .context("the retry must be accepted once the index is usable")?;

        let after = store_counts(temp.path())?;
        let row = source_row(temp.path(), &brain_turn_id)?;
        assert!(
            performed,
            "the retry must report that IT projected the turn: the sweep it runs \
             first has to exclude the very row the caller is about to project, or \
             a projection that failed inside that sweep is reported to the caller \
             as a successful no-op; source_row={row:?}, before={before:?}, \
             after=(conversations, memory_sources, routing_points)={after:?}, \
             id={brain_turn_id}"
        );
        assert!(
            matches!(row, Some(Some(_))),
            "and the stranded turn must actually be indexed, whichever path did \
             it; source_row={row:?}, after={after:?}, id={brain_turn_id}"
        );
        assert_eq!(
            (after.0, after.1),
            (before.0, before.1 + 1),
            "with no duplicate raw turn and exactly one new mapping; \
             before={before:?}, after={after:?}, id={brain_turn_id}"
        );

        let second = reopened
            .insert_brain_conversation("user", BRAIN_TURN, None, Some("brain"), &brain_provenance())
            .await?;
        let after_second = store_counts(temp.path())?;
        assert!(
            !second,
            "a second identical retry, with nothing left to project, must report \
             no work; after={after:?}, after_second={after_second:?}, \
             id={brain_turn_id}"
        );
        assert_eq!(
            after_second, after,
            "and must change no row counts; id={brain_turn_id}"
        );
        assert_eq!(
            source_row(temp.path(), &brain_turn_id)?,
            row,
            "nor re-point the turn at a second leaf; after_second={after_second:?}, \
             id={brain_turn_id}"
        );
        Ok(())
    }

    /// Two consecutive turns in the same session must be linked: the second turn's own
    /// occurrence records `prev` as the first turn's uuid, and the first turn's occurrence gets
    /// `next` closed forward to the second by `link_next` -- both directions of the same edge,
    /// not just one. A third turn in an UNRELATED session, inserted in between, must not affect
    /// either: `prev` resolution is scoped by `session_id`, not "whatever occurrence was created
    /// most recently in the whole store".
    #[tokio::test]
    async fn test_two_consecutive_turns_in_one_session_link_prev_and_next() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        memory
            .insert_conversation(
                "user",
                &substantive("chain-turn-one"),
                None,
                Some("sess-chain"),
            )
            .await?;
        let turn_one_id = conversation_id_for_content(temp.path(), &substantive("chain-turn-one"))?;
        let turn_one = occurrence_row_for_conversation(temp.path(), &turn_one_id)?
            .expect("turn one must be projected as an occurrence");
        assert_eq!(
            turn_one.1, None,
            "a fresh session's first occurrence must have prev=None; got {turn_one:?}"
        );
        assert_eq!(
            turn_one.2, None,
            "turn one has no successor yet; got {turn_one:?}"
        );

        // An unrelated session's first turn, inserted in between, must not be treated as this
        // session's predecessor by either side.
        memory
            .insert_conversation(
                "user",
                &substantive("other-session-turn"),
                None,
                Some("sess-other"),
            )
            .await?;
        let other_id =
            conversation_id_for_content(temp.path(), &substantive("other-session-turn"))?;
        let other = occurrence_row_for_conversation(temp.path(), &other_id)?
            .expect("the other session's own first turn must also be projected");
        assert_eq!(
            other.1, None,
            "a different session's first occurrence must ALSO have prev=None -- prev resolution \
             is scoped by session_id, not by store-wide recency; got {other:?}"
        );

        memory
            .insert_conversation(
                "assistant",
                &substantive("chain-turn-two"),
                None,
                Some("sess-chain"),
            )
            .await?;
        let turn_two_id = conversation_id_for_content(temp.path(), &substantive("chain-turn-two"))?;
        let turn_two = occurrence_row_for_conversation(temp.path(), &turn_two_id)?
            .expect("turn two must be projected as an occurrence");
        assert_eq!(
            turn_two.1,
            Some(turn_one.0.clone()),
            "turn two's own occurrence must record prev=turn one's uuid, not the unrelated \
             session's turn; turn_one={turn_one:?}, turn_two={turn_two:?}"
        );

        let turn_one_after = occurrence_row_for_conversation(temp.path(), &turn_one_id)?
            .expect("turn one's occurrence row must still exist");
        assert_eq!(
            turn_one_after.2,
            Some(turn_two.0.clone()),
            "turn one's occurrence must be linked forward to turn two by link_next; \
             turn_one_after={turn_one_after:?}, turn_two={turn_two:?}"
        );

        Ok(())
    }

    /// The whole reason to resolve `prev` durably (`MemorySystem::last_occurrence_uuid_for_session`)
    /// rather than from an in-memory "last occurrence" cache: the occurrence chain must survive a
    /// process restart. Two turns are stored, the `MemorySystem` is dropped and a fresh one
    /// rebuilt from the same durable database (mirroring how every other restart-survival test in
    /// this module reopens over the same `db_path`), and a third turn in the SAME session must
    /// link to the second turn's occurrence -- not come back with `prev=None` as it would if
    /// chain state had lived only in memory and been silently lost at restart.
    #[tokio::test]
    async fn test_occurrence_chain_survives_a_process_restart() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        {
            let memory = MemorySystem::new(config.clone())?;
            memory
                .insert_conversation(
                    "user",
                    &substantive("restart-turn-one"),
                    None,
                    Some("sess-restart"),
                )
                .await?;
            memory
                .insert_conversation(
                    "assistant",
                    &substantive("restart-turn-two"),
                    None,
                    Some("sess-restart"),
                )
                .await?;
        } // `memory` dropped here -- nothing about the chain lives past this point except what
          // was durably committed.

        let turn_two_id =
            conversation_id_for_content(temp.path(), &substantive("restart-turn-two"))?;
        let turn_two_before_restart = occurrence_row_for_conversation(temp.path(), &turn_two_id)?
            .expect("turn two must have been projected before the restart");

        let reopened = MemorySystem::new(config)?;
        reopened
            .insert_conversation(
                "user",
                &substantive("restart-turn-three"),
                None,
                Some("sess-restart"),
            )
            .await?;

        let turn_three_id =
            conversation_id_for_content(temp.path(), &substantive("restart-turn-three"))?;
        let turn_three = occurrence_row_for_conversation(temp.path(), &turn_three_id)?
            .expect("turn three must be projected as an occurrence after reopening");
        assert_eq!(
            turn_three.1,
            Some(turn_two_before_restart.0.clone()),
            "turn three, inserted after a fresh MemorySystem was rebuilt from the same durable \
             database, must link to turn two's occurrence (prev=turn two's uuid) -- not come \
             back orphaned with prev=None, which is what an in-memory-only 'last occurrence' \
             cache would have produced after a restart; turn_two={turn_two_before_restart:?}, \
             turn_three={turn_three:?}"
        );

        let turn_two_after_restart = occurrence_row_for_conversation(temp.path(), &turn_two_id)?
            .expect("turn two's occurrence row must still exist after the restart");
        assert_eq!(
            turn_two_after_restart.2,
            Some(turn_three.0.clone()),
            "turn two must be linked forward to turn three by the post-restart link_next call; \
             turn_two_after_restart={turn_two_after_restart:?}, turn_three={turn_three:?}"
        );

        Ok(())
    }

    /// `LinkNextError::Conflict` is a real, expected outcome (two writers racing to continue the
    /// same session), not corruption -- `save_routing_occurrence` must not fail the turn that
    /// lost the race, and must not disturb the winner's link. The race itself is proven exact-once
    /// at the SQL layer by `RoutingMemTree::link_next`'s own hostile-concurrency test
    /// (`routing_memory/tests.rs`); this test proves what THIS caller does with a losing result,
    /// arranged deterministically (a rival link is written directly) rather than raced on wall
    /// clock, per this crate's own rule against timing as a correctness oracle.
    #[tokio::test]
    async fn test_a_link_conflict_does_not_fail_or_corrupt_the_losing_turn() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        memory
            .insert_conversation(
                "user",
                &substantive("race-turn-zero"),
                None,
                Some("sess-race"),
            )
            .await?;
        let turn_zero_id =
            conversation_id_for_content(temp.path(), &substantive("race-turn-zero"))?;
        let turn_zero = occurrence_row_for_conversation(temp.path(), &turn_zero_id)?
            .expect("turn zero must be projected as an occurrence");

        // Arrange the conflict this test is about: link turn zero forward to a rival uuid before
        // this process's own next turn gets a chance to. Its own `prev` resolution still finds
        // turn zero (the durable lookup has no way to know about the rival), so it will attempt
        // to link from turn zero via `link_next` and lose.
        let rival_uuid = Uuid::new_v4();
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE routing_occurrences SET next_uuid = ?1 WHERE uuid = ?2",
                params![rival_uuid.to_string(), turn_zero.0],
            )?;
        }

        memory
            .insert_conversation(
                "assistant",
                &substantive("race-turn-one"),
                None,
                Some("sess-race"),
            )
            .await
            .expect("a losing link_next must not fail the turn that lost it");

        let turn_one_id = conversation_id_for_content(temp.path(), &substantive("race-turn-one"))?;
        let turn_one = occurrence_row_for_conversation(temp.path(), &turn_one_id)?.expect(
            "the losing turn's own occurrence and point must still be recorded, not \
                     silently dropped because its link lost",
        );
        assert_eq!(
            turn_one.1,
            Some(turn_zero.0.clone()),
            "the losing turn's own occurrence must still record prev=turn zero's uuid even \
             though the forward link from turn zero did not win; turn_one={turn_one:?}"
        );

        let turn_zero_after = occurrence_row_for_conversation(temp.path(), &turn_zero_id)?
            .expect("turn zero's occurrence row must still exist");
        assert_eq!(
            turn_zero_after.2,
            Some(rival_uuid.to_string()),
            "the losing link_next call must not overwrite the rival's next_uuid on turn zero; \
             turn_zero_after={turn_zero_after:?}"
        );

        let results = memory
            .query_with_sources(&substantive("race-turn-one"), Some(5))
            .await?;
        assert!(
            results
                .iter()
                .any(|r| r.text == substantive("race-turn-one")),
            "the losing turn must still be a real, queryable memory, not corrupted or dropped; \
             results={results:?}"
        );

        Ok(())
    }

    /// Regression for the nearest-wall-clock-timestamp mis-pairing bug: `counterpart_turn` must
    /// pair a retrieved turn with its REAL reply via the occurrence chain (PR #1213's
    /// `prev`/`next` links), not with whatever other same-session, opposite-role turn happens to
    /// land closer in wall-clock time.
    ///
    /// Arranged deterministically per this crate's rule against timing as a correctness oracle:
    /// three turns are inserted in the real chain order (question, true reply, unrelated
    /// assistant turn) so the occurrence chain records question -> true reply -> unrelated turn
    /// regardless of clock behavior, and then `conversations.timestamp` is overwritten directly
    /// so the unrelated turn is numerically closest to the question -- reproducing exactly the
    /// reported case (a real reply that loses to a nearer-in-time impostor under the old
    /// heuristic) without depending on real elapsed time.
    #[tokio::test]
    async fn test_counterpart_turn_follows_occurrence_chain_not_nearest_timestamp() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        memory
            .insert_conversation(
                "user",
                &substantive("pairing-question"),
                None,
                Some("sess-pairing"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                &substantive("pairing-true-reply"),
                None,
                Some("sess-pairing"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                &substantive("pairing-unrelated-turn"),
                None,
                Some("sess-pairing"),
            )
            .await?;

        let question_id =
            conversation_id_for_content(temp.path(), &substantive("pairing-question"))?;
        let true_reply_id =
            conversation_id_for_content(temp.path(), &substantive("pairing-true-reply"))?;
        let unrelated_id =
            conversation_id_for_content(temp.path(), &substantive("pairing-unrelated-turn"))?;

        let question_occurrence = occurrence_row_for_conversation(temp.path(), &question_id)?
            .expect("the question must be projected as an occurrence");
        assert_eq!(
            question_occurrence.2,
            Some(
                occurrence_row_for_conversation(temp.path(), &true_reply_id)?
                    .expect("the true reply must be projected as an occurrence")
                    .0
            ),
            "the question's occurrence must chain forward to the TRUE reply's occurrence \
             (insertion order), not to the unrelated turn inserted afterward; \
             question_occurrence={question_occurrence:?}"
        );

        // Overwrite timestamps directly so the unrelated turn is numerically nearest to the
        // question -- the exact condition that fooled the old nearest-timestamp heuristic --
        // while the occurrence chain above (set at insert time, independent of this column)
        // still names the true reply.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE conversations SET timestamp = 1000 WHERE id = ?1",
                params![question_id],
            )?;
            conn.execute(
                "UPDATE conversations SET timestamp = 1001 WHERE id = ?1",
                params![unrelated_id],
            )?;
            conn.execute(
                "UPDATE conversations SET timestamp = 50000 WHERE id = ?1",
                params![true_reply_id],
            )?;
        }

        let conn = Connection::open(temp.path())?;
        let question_turn = recall_turn_by_id(&conn, &question_id)?
            .expect("the question's conversation row must still exist");
        let counterpart = counterpart_turn(&conn, &question_turn)?.expect(
            "the question has a real reply via the occurrence chain and must not return None",
        );
        assert_eq!(
            counterpart.source.source_id, true_reply_id,
            "counterpart_turn must return the TRUE reply (linked via the occurrence chain), not \
             the unrelated turn that was made numerically closer in timestamp \
             (question_ts=1000, unrelated_ts=1001, true_reply_ts=50000); got \
             counterpart={counterpart:?}, true_reply_id={true_reply_id}, unrelated_id={unrelated_id}"
        );

        let rendered = render_recall_entry(&question_turn, Some(&counterpart));
        assert_eq!(
            rendered,
            format!(
                "user: {}\nassistant: {}",
                substantive("pairing-question"),
                substantive("pairing-true-reply")
            ),
            "the rendered recall entry must display the TRUE reply, not the unrelated turn; \
             rendered={rendered:?}"
        );

        Ok(())
    }

    /// A projection that FAILS must leave the pending-projection sweep armed.
    ///
    /// The asymmetry this pins is #339 reintroduced by its own fix.
    /// `sweep_pending_projections_locked` clears the flag when the pending set
    /// comes back empty — and with one pending row and a named-Brain retry for
    /// that same identity, the set is empty only because the `?1` exclusion
    /// removed it. The caller then reaches `project_stored_conversation`, where
    /// `ctx.embedding_engine.embed(&key_content)?` returns above BOTH sites
    /// that used to re-arm the flag (the `insert_with_effect` and
    /// `save_all_nodes_to_db` error paths). The row stayed pending with the
    /// sweep disarmed, so no automatic sweep ran again for the life of the
    /// process. The same asymmetry hit an ordinary first-time write that failed
    /// at `embed`.
    #[tokio::test]
    async fn test_a_failed_projection_rearms_the_pending_sweep() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };
        let brain_turn_id = strand_a_brain_conversation(&config).await?;

        let engine = Arc::new(FailableEmbedding {
            inner: HashedNgramEmbedding::new(),
            fail: AtomicBool::new(true),
        });
        let reopened = MemorySystem::new_with_engine(config, Arc::clone(&engine) as _)?;
        assert!(
            reopened.hydration_task.is_none(),
            "precondition: the current-thread arm spawns no loader, so no sweep \
             runs behind this test's back"
        );

        let error = reopened
            .insert_brain_conversation("user", BRAIN_TURN, None, Some("brain"), &brain_provenance())
            .await
            .expect_err("precondition: a failing embedder must refuse the projection");
        assert!(
            error.to_string().contains("embedding engine unavailable"),
            "precondition: the failure must be the embed call itself, not \
             something earlier; got {error}"
        );
        let row = source_row(temp.path(), &brain_turn_id)?;
        assert_eq!(
            row,
            None,
            "precondition: the failed projection must leave the turn stranded, \
             with its raw row and no memory_sources row; source_row={row:?}, \
             id={brain_turn_id}, counts={:?}",
            store_counts(temp.path())?
        );
        assert!(
            reopened.needs_projection_sweep.load(Ordering::SeqCst),
            "a failed projection must leave the sweep OWED. Its own sweep found \
             an empty pending set — empty only because the `?1` exclusion removed \
             this very row — and cleared the flag, so with the flag left down \
             nothing automatic ever looks at this row again; id={brain_turn_id}, \
             counts=(conversations, memory_sources, routing_points)={:?}",
            store_counts(temp.path())?
        );

        // The production consequence, not merely the flag: the next ordinary
        // write must repair the turn the failure stranded.
        engine.fail.store(false, Ordering::SeqCst);
        reopened
            .insert_conversation("user", FRESH_TURN, None, Some("fresh"))
            .await
            .context("the next live turn must be accepted")?;
        let repaired = source_row(temp.path(), &brain_turn_id)?;
        assert!(
            matches!(repaired, Some(Some(_))),
            "and the next write's automatic sweep must repair it; a disarmed flag \
             makes the turn permanently invisible to semantic recall, which is \
             exactly the loss #339 is about; source_row={repaired:?}, \
             id={brain_turn_id}, counts={:?}",
            store_counts(temp.path())?
        );
        Ok(())
    }

    /// A current-thread runtime does not spawn a loader at all, so there is
    /// nothing to starve and nothing to wait on.
    ///
    /// Spawning there was a real hazard: `Handle::try_current()` succeeds on a
    /// current-thread runtime, and a loader spawned onto it cannot be polled
    /// while any synchronous caller blocks the single scheduler thread. A
    /// timeout could not have rescued it either — that blocked thread owns the
    /// time driver, so the `Sleep` never fires, which is why an earlier
    /// attempt at bounding the wait hung for over 200 seconds while claiming
    /// to be bounded.
    ///
    /// The multi-threaded counterpart is `typed_mem_store_completes_on_a_single_worker_runtime`
    /// in `finch-runtime`, which drives the real submit API.
    #[tokio::test]
    async fn test_a_current_thread_runtime_loads_without_spawning_a_loader() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        const NODES: u64 = SEEDED_STORE_SIZE;
        seed_routing_points(temp.path(), NODES, HashedNgramEmbedding::new().dimension())?;

        let memory = MemorySystem::new(config)?;
        assert!(
            memory.hydration_task.is_none(),
            "a current-thread runtime must not spawn a loader it cannot poll"
        );
        match memory.hydration_status() {
            HydrationStatus::Ready { nodes } => assert_eq!(nodes as u64, NODES),
            other => panic!("construction must finish the load here; got {other:?}"),
        }

        // The write that used to deadlock. It completes because the gate is
        // already open, not because anything timed out.
        memory
            .insert_conversation(
                "system",
                "A memory written from a current-thread runtime, which is how \
                 mem-store reaches this through block_on_host.",
                None,
                None,
            )
            .await?;

        // The structure loaded correctly: real, undeflated centroids exist on
        // every node (RoutingTree has no separate "children linked" pass to
        // verify -- `routing_nodes.is_leaf`/`left_id`/`right_id` are explicit
        // persisted columns, unambiguous the instant a row loads).
        let tree = memory.tree.lock().await;
        assert!(tree.tree().node_count() > 1, "a store with {NODES} points must have real tree structure, not just a placeholder root");
        Ok(())
    }

    // MemTree's own version of this test corrupted `tree_nodes.node_id`'s type
    // to make its `MAX(node_id)` census query fail (the query that fed
    // `set_next_id`). RoutingTree needs no such counter -- a freshly loaded
    // tree's own point count already determines the next assigned id
    // (`new_with_connection`'s own comment) -- so there is no longer a MAX
    // query, and the remaining `COUNT(*) FROM routing_points` census has no
    // realistic single-column corruption that makes it fail: `COUNT(*)` never
    // decodes a row's values. A genuinely unreadable store is still covered,
    // by `test_an_unreadable_store_on_the_synchronous_arm_refuses_writes`
    // below, via a different (still-real) mechanism.

    #[tokio::test]
    async fn test_write_after_hydration_completes_does_not_hang() -> Result<()> {
        let temp = NamedTempFile::new()?;
        write_after_hydration_completes(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_write_after_hydration_completes_does_not_hang_multi_thread() -> Result<()> {
        // The current-thread sibling above pins the deterministic case; this
        // one runs the scheduler production actually uses, where the loader
        // task and the writing turn are genuinely concurrent.
        let temp = NamedTempFile::new()?;
        write_after_hydration_completes(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })
        .await
    }

    // Multi-thread: the only flavor that spawns the background loader.
    // Its "stored while the index was still loading" premise needs a loader
    // that is actually running.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_hydration_does_not_lose_or_overwrite_stored_memories() -> Result<()> {
        // `next_id` is advanced past `MAX(node_id)` before any batch lands.
        // Without it a write takes id 1 and `nodes.insert(1, ..)` overwrites
        // the node already there.
        //
        // An earlier version of this test asserted only that the NEW memory was
        // retrievable afterwards — which passes with `set_next_id` deleted,
        // because the query finds the new node and the destroyed victim is
        // never asserted on. Count the nodes instead, and check a specific
        // earlier memory survives.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let before = {
            let memory = MemorySystem::new(config.clone())?;
            memory.ensure_hydrated().await?;
            for i in 0..30 {
                memory
                    .insert_conversation(
                        "system",
                        &format!(
                            "Runbook step {i}: restart the daemon and confirm the \
                             health endpoint answers before proceeding."
                        ),
                        None,
                        None,
                    )
                    .await?;
            }
            memory.stats().await?.tree_node_count
        };

        let reopened = MemorySystem::new(config)?;
        reopened
            .insert_conversation(
                "system",
                "A memory stored while the index was still loading from disk.",
                None,
                None,
            )
            .await?;
        reopened.ensure_hydrated().await?;

        assert_eq!(
            reopened.stats().await?.tree_node_count,
            before + 1,
            "the new memory must be added, not written over an existing node"
        );

        // Every seeded memory, not "a memory matching `Runbook step`". An
        // earlier version searched for the shared prefix, which 29 survivors
        // still match even if one point is overwritten — so it could not fail.
        // Ranking cannot carry this assertion either: `HashedNgramEmbedding` drops
        // tokens under two characters, so the single-digit index that
        // distinguishes these memories is not in the embedding at all and
        // `query` orders them arbitrarily. Scan the hydrated point set instead.
        let texts: Vec<String> = {
            let tree = reopened.tree.lock().await;
            tree.iter_points().map(|(_, m, _)| m.text.clone()).collect()
        };
        for i in 0..30 {
            let needle = format!("Runbook step {i}:");
            assert!(
                texts.iter().any(|text| text.contains(&needle)),
                "every memory stored before the restart must survive it; \
                 {needle:?} was overwritten"
            );
        }

        Ok(())
    }

    // Multi-thread: the only flavor that spawns the background loader.
    //
    // Unlike MemTree (two independently-written loaders -- a batched
    // background one and a direct blocking one -- that this test cross-checked
    // against each other), RoutingTree's constructor and `hydrate_in_background`
    // both call the SAME `RoutingMemTree::load`, so there are no longer two
    // independent implementations to compare. What's still real to check: the
    // background (spawned) path must produce exactly what a direct call to
    // that same function produces on the same database -- catching a bug like
    // the background path passing the wrong `dim` or the wrong connection,
    // which a spawned task's own type signature can't rule out at compile time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_background_hydration_matches_a_direct_load() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        {
            let memory = MemorySystem::new(config.clone())?;
            memory.ensure_hydrated().await?;
            for i in 0..40 {
                memory
                    .insert_conversation(
                        "system",
                        &format!(
                            "Incident {i}: the provider returned a malformed \
                             program and the runner recovered without a restart."
                        ),
                        None,
                        None,
                    )
                    .await?;
            }
        }

        // Background path.
        let background = MemorySystem::new(config.clone())?;
        background.ensure_hydrated().await?;
        let dim = background.embedding_engine.dimension();
        let background_points = {
            let tree = background.tree.lock().await;
            let mut points: Vec<(PointId, String, u8)> = tree
                .iter_points()
                .map(|(pid, m, _)| (pid, m.text.clone(), m.importance))
                .collect();
            points.sort_by_key(|entry| entry.0);
            points
        };

        // Direct path, built straight from the same database.
        let direct_points = {
            let conn = Connection::open(temp.path())?;
            let (_, metadata) = load_routing_tree(
                &conn,
                RoutingConfig::default(),
                dim,
                routing_memory::FIXED_SEED,
            )?;
            let mut points: Vec<(PointId, String, u8)> = metadata
                .into_iter()
                .map(|(pid, text, importance)| (pid as PointId, text, importance))
                .collect();
            points.sort_by_key(|entry| entry.0);
            points
        };

        assert_eq!(
            background_points, direct_points,
            "the background loader must reproduce a direct load exactly"
        );
        assert!(
            background_points.len() > 1,
            "the comparison is only meaningful on a populated store"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_memory_system_creation() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            enabled: true,
            max_context_items: 5,
            checkpoint_interval_secs: 300,
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;
        let stats = memory.stats().await?;

        assert_eq!(stats.conversation_count, 0);
        assert_eq!(stats.tree_node_count, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_insert_conversation() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;

        memory
            .insert_conversation(
                "user",
                "How do I use Rust lifetimes?",
                Some("local"),
                Some("test-session"),
            )
            .await?;

        let stats = memory.stats().await?;
        assert_eq!(stats.conversation_count, 1);
        assert_eq!(stats.tree_node_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn named_brain_conversation_is_idempotent_and_conflict_safe() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })?;
        let provenance = BrainConversationProvenance {
            brain_id: "brain-1".into(),
            run_id: "run-1".into(),
            request_seq: 17,
        };

        assert!(
            memory
                .insert_brain_conversation(
                    "user",
                    "inspect the scheduler cancellation path",
                    Some("test-model"),
                    Some("test-brain"),
                    &provenance,
                )
                .await?
        );
        assert!(
            !memory
                .insert_brain_conversation(
                    "user",
                    "inspect the scheduler cancellation path",
                    Some("test-model"),
                    Some("test-brain"),
                    &provenance,
                )
                .await?
        );
        assert!(memory
            .insert_brain_conversation(
                "user",
                "different content for the same run",
                Some("test-model"),
                Some("test-brain"),
                &provenance,
            )
            .await
            .is_err());

        assert_eq!(memory.stats().await?.conversation_count, 1);
        assert_eq!(memory.stats().await?.tree_node_count, 1);
        let stored: (String, String, i64) = {
            let conn = memory.db.lock().await;
            conn.query_row(
                "SELECT brain_id, run_id, request_seq FROM conversations",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?
        };
        assert_eq!(stored, ("brain-1".into(), "run-1".into(), 17));

        let results = memory
            .query_with_sources("scheduler cancellation", Some(1))
            .await?;
        assert_eq!(results.len(), 1);
        let result = &results[0];
        let source = result.source.as_ref().expect("new leaf has provenance");
        assert_eq!(source.brain_id.as_deref(), Some("brain-1"));
        assert_eq!(source.run_id.as_deref(), Some("run-1"));
        assert_eq!(source.request_seq, Some(17));

        let inspected = memory
            .inspect_memory(&result.memory_id)
            .await?
            .expect("stable memory id resolves");
        assert_eq!(inspected.content, "inspect the scheduler cancellation path");

        drop(memory);
        let reopened = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })?;
        let reopened_memory = reopened
            .inspect_memory(&result.memory_id)
            .await?
            .expect("source mapping survives restart");
        assert_eq!(reopened_memory, inspected);
        Ok(())
    }

    #[tokio::test]
    async fn failed_memory_projection_restores_tree_before_retry() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })?;
        let provenance = BrainConversationProvenance {
            brain_id: "brain-retry".into(),
            run_id: "run-retry".into(),
            request_seq: 3,
        };
        {
            let conn = memory.db.lock().await;
            conn.execute_batch(
                "CREATE TRIGGER reject_memory_node BEFORE INSERT ON routing_points
                 BEGIN SELECT RAISE(FAIL, 'injected projection failure'); END;",
            )?;
        }
        assert!(memory
            .insert_brain_conversation(
                "assistant",
                "The durable scheduler retry must create exactly one semantic memory.",
                Some("test-model"),
                Some("test-brain"),
                &provenance,
            )
            .await
            .is_err());
        assert_eq!(memory.stats().await?.conversation_count, 1);
        assert_eq!(memory.stats().await?.tree_node_count, 0);

        {
            let conn = memory.db.lock().await;
            conn.execute_batch("DROP TRIGGER reject_memory_node;")?;
        }
        memory
            .insert_brain_conversation(
                "assistant",
                "The durable scheduler retry must create exactly one semantic memory.",
                Some("test-model"),
                Some("test-brain"),
                &provenance,
            )
            .await?;
        assert_eq!(memory.stats().await?.conversation_count, 1);
        assert_eq!(memory.stats().await?.tree_node_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn test_query_memory() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;

        // Insert conversations
        memory
            .insert_conversation("user", "How do I use Rust lifetimes?", Some("local"), None)
            .await?;

        memory
            .insert_conversation("user", "What is Python asyncio?", Some("local"), None)
            .await?;

        // Query for Rust-related content
        let results = memory.query("Rust programming", Some(2)).await?;

        assert!(!results.is_empty());
        // Should return Rust-related conversation
        assert!(results
            .iter()
            .any(|r| r.contains("Rust") || r.contains("lifetimes")));

        Ok(())
    }

    #[tokio::test]
    async fn test_conversation_summary_empty() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        let summary = memory.conversation_summary(3).await?;
        assert!(summary.lines.is_empty(), "empty tree → no context lines");
        Ok(())
    }

    /// Regression: a single turn must produce at least one non-empty line so
    /// the status strip populates after the first assistant response.
    #[tokio::test]
    async fn test_conversation_summary_single_turn_shows_content() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        memory
            .insert_conversation("user", "How do Rust lifetimes work?", Some("local"), None)
            .await?;
        let summary = memory.conversation_summary(3).await?;
        assert!(
            !summary.lines.is_empty(),
            "single turn should produce at least one context line"
        );
        assert!(
            !summary.lines[0].is_empty(),
            "context line text must not be empty"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_conversation_summary_multiple_turns() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;
        for content in &[
            "How do Rust lifetimes work?",
            "What is async await in Rust?",
            "Explain Rust ownership and borrowing",
        ] {
            memory
                .insert_conversation("user", content, Some("local"), None)
                .await?;
        }
        let summary = memory.conversation_summary(3).await?;
        assert!(
            !summary.lines.is_empty(),
            "should have context lines with 3 turns"
        );
        assert!(
            summary.lines.iter().all(|l| !l.is_empty()),
            "all lines must be non-empty"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_session_summary_does_not_leak_another_brains_context() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory
            .insert_conversation(
                "user",
                "Beelzebub and unrelated mythology",
                Some("local"),
                Some("other-brain"),
            )
            .await?;
        memory
            .insert_conversation(
                "user",
                "Fix Finch shadow buffer row accounting",
                Some("local"),
                Some("active-brain"),
            )
            .await?;

        let summary = memory
            .conversation_summary_for_session("active-brain", 3)
            .await?;
        assert!(summary
            .lines
            .iter()
            .any(|line| line.contains("shadow buffer")));
        assert!(summary.lines.iter().all(|line| !line.contains("Beelzebub")));
        Ok(())
    }

    #[test]
    fn test_context_windows_depth_zero_is_empty() {
        assert!(context_windows(0, 10).is_empty());
    }

    #[test]
    fn test_context_windows_depth_one_is_single_window() {
        let ws = context_windows(1, 10);
        assert_eq!(ws.len(), 1);
        assert_eq!(ws[0], 3); // capped at 3
    }

    #[test]
    fn test_context_windows_depth_two_has_overall_and_recent() {
        let ws = context_windows(2, 100);
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0], 100); // all leaves = overall
        assert_eq!(ws[1], 3); // most recent
    }

    #[test]
    fn test_context_windows_depth_four_has_four_slots() {
        let ws = context_windows(4, 100);
        assert_eq!(ws.len(), 4);
        assert_eq!(ws[0], 100); // overall
        assert_eq!(*ws.last().unwrap(), 3); // most recent always last
    }

    #[test]
    fn test_context_windows_caps_to_num_leaves() {
        // Only 2 leaves — all windows should be capped at 2
        let ws = context_windows(4, 2);
        for w in &ws {
            assert!(*w <= 2, "window {} > num_leaves 2", w);
        }
    }

    #[tokio::test]
    async fn test_get_recent_conversations() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = MemorySystem::new(config)?;

        // Insert multiple conversations
        for i in 1..=5 {
            memory
                .insert_conversation("user", &format!("Message {}", i), Some("local"), None)
                .await?;
        }

        // Get recent 3
        let recent = memory.get_recent_conversations(3).await?;

        assert_eq!(recent.len(), 3);
        // Should be in reverse chronological order
        assert!(recent[0].1.contains("Message 5"));

        Ok(())
    }

    // A test previously lived here (`test_old_schema_migration_drops_and_recreates_
    // tree_nodes`) verifying that MemorySystem::new() detected and dropped a stale
    // `tree_nodes` table left by an old `id AUTOINCREMENT` schema (predating even
    // MemTree's own `node_id INTEGER PRIMARY KEY` fix) before recreating it. That
    // migration and its table are gone: `schema.sql` no longer declares `tree_nodes`
    // at all now that RoutingTree is the only index, so the constructor
    // unconditionally drops any stale `tree_nodes` table on open (see the
    // `DROP TABLE IF EXISTS tree_nodes;` ahead of `include_str!("schema.sql")`)
    // rather than detecting a specific old shape of it.

    // A test previously lived here (`test_a_corrupt_parent_chain_on_disk_
    // errors_instead_of_aborting`, #274) verifying that a corrupt `parent_id`
    // cycle in `tree_nodes` surfaced as a named "cycles:" error from
    // `update_parent_aggregation`'s recursive walk, rather than recursing
    // until the thread overflowed its stack (a SIGABRT, no `Result` a caller
    // could act on). RoutingTree's own real_centroid maintenance walks parent
    // pointers too (`remove_point`'s downdate, `insert_into`'s update), but
    // iteratively, never recursively -- the specific stack-overflow failure
    // mode #274 fixed does not exist here by construction. A REAL, disclosed
    // gap this doesn't cover: those iterative walks currently use `.expect()`
    // on a missing parent, which still panics rather than returning a
    // diagnosable `Result` if `routing_nodes.parent_id` were ever corrupted on
    // disk the same way. Hardening that (mirroring #274's own fix: detect and
    // name the problem, return `Err`, never panic or infinite-loop) is real,
    // not-yet-done follow-up work, not something this deletion silently
    // resolves.

    // ── #415: MemTree recall defects ─────────────────────────────────────────

    /// The question and the answer of one stored exchange, used by the
    /// recall tests below. Both are long enough to clear the classifier's
    /// 20-character floor and carry no ack or greeting shape.
    const PAIRED_QUESTION: &str = "What is the deployment token for the staging cluster?";
    const PAIRED_ANSWER: &str =
        "The staging deployment token is stored in the Employee vault, not in \
         the repository.";

    /// Seed a store created through the real schema with real `routing_points`
    /// -- one real `RoutingTree` point per entry (real embedding via
    /// `HashedNgramEmbedding`, so duplicate TEXT across entries is real duplicate
    /// content on genuinely distinct points, bypassing `RoutingMemTree`'s own
    /// insert-time text dedup the same way a store built by an older/different
    /// process would). A reopen then hydrates exactly these rows.
    ///
    /// Was `seed_legacy_tree_rows`, writing directly to `tree_nodes` -- nothing
    /// reads that table anymore, and RoutingTree cannot read an old MemTree
    /// store at all (a different representation, no migration path was ever
    /// built or promised, same "no users yet, start fresh" precedent
    /// `schema.sql`'s own comment already establishes for the schema itself).
    /// The properties these tests protect (duplicate leaf content collapses to
    /// one recall result; recall excludes noise regardless of when it was
    /// stored) are still real for RoutingTree's own format, just seeded
    /// against it directly instead of simulating an incompatible old one.
    fn seed_legacy_tree_rows(db_path: &std::path::Path, leaves: &[(&str, u8)]) -> Result<()> {
        let mut conn = Connection::open(db_path)?;
        let engine = HashedNgramEmbedding::new();
        let dim = engine.dimension();
        let mut tree = RoutingTree::new(RoutingConfig::default(), dim, routing_memory::FIXED_SEED);
        let tx = conn.transaction()?;
        for (index, (text, importance)) in leaves.iter().enumerate() {
            let embedding = engine.embed(text)?;
            let point_id = tree.insert(embedding.clone());
            save_point(&tx, point_id, text, &embedding, *importance, index as i64)?;
        }
        let dirty = tree.dirty_node_ids();
        write_dirty_nodes_within(&tree, &dirty, &tx)?;
        tx.commit()?;
        tree.mark_persisted(&dirty);
        Ok(())
    }

    /// #415 defect 1 at the persistence boundary. Leaves-only retrieval stops
    /// an internal node's provisional label from surfacing, but a store built
    /// before insertion was fixed already holds the same text on several
    /// leaves — the reference host measured 5 distinct texts across 11 of 27
    /// nodes — and hydration loads every row, so recall spent its top-k
    /// budget on duplicates. Replayed here through a real store reopen.
    #[tokio::test]
    async fn test_recall_from_a_legacy_store_with_duplicate_leaf_rows_returns_each_memory_once(
    ) -> Result<()> {
        let temp = NamedTempFile::new()?;
        {
            // Create the schema, then drop the handle so the rows below are
            // the only content the reopen hydrates.
            let _warm = MemorySystem::new(MemoryConfig {
                db_path: temp.path().to_path_buf(),
                ..Default::default()
            })?;
        }
        let text = "The signing key lives in the Employee vault, never in the \
                    repository.";
        seed_legacy_tree_rows(temp.path(), &[(text, 1), (text, 1)])?;

        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory.ensure_hydrated().await?;

        let results = memory.query(text, Some(5)).await?;
        let copies = results.iter().filter(|r| r.contains(text)).count();
        assert_eq!(
            copies, 1,
            "the same memory must be recalled once even when a legacy store \
             holds it on several leaves; got {results:?}"
        );
        Ok(())
    }

    /// #415 defect 4 at the persistence boundary. The reference store's tree
    /// was built before the quality classifier existed, so greetings and acks
    /// sit in `routing_points` at importance 1 and recall handed them back as
    /// memories. Nothing rewrites stored rows, so the salience gate must hold
    /// at recall, not only at insert.
    #[tokio::test]
    async fn test_recall_excludes_noise_stored_by_older_builds() -> Result<()> {
        let temp = NamedTempFile::new()?;
        {
            let _warm = MemorySystem::new(MemoryConfig {
                db_path: temp.path().to_path_buf(),
                ..Default::default()
            })?;
        }
        seed_legacy_tree_rows(
            temp.path(),
            &[
                ("You're welcome, Shammah!", 1),
                (
                    "The signing key lives in the Employee vault, never in \
                     the repository.",
                    1,
                ),
            ],
        )?;

        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory.ensure_hydrated().await?;

        let results = memory
            .query("The signing key lives in the Employee vault", Some(5))
            .await?;
        assert!(
            results.iter().all(|r| !r.contains("You're welcome")),
            "recall must not hand back an ack a pre-classifier build stored; \
             got {results:?}"
        );
        assert!(
            results
                .iter()
                .any(|r| r.contains("signing key lives in the Employee vault")),
            "the substantive memory must still be recalled; got {results:?}"
        );
        Ok(())
    }

    /// #415 defect 2. Recall stripped the role, so the model could not tell
    /// which turns were the user's and which were its own — it was handed its
    /// own past greeting as "relevant context". The role lives in the
    /// `conversations` row every new-format leaf points at, so recall can
    /// attribute without changing what is stored.
    #[tokio::test]
    async fn test_recall_labels_the_role_of_the_stored_turn() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        // Separate sessions, so neither turn has a counterpart to pair with
        // and the role label is asserted on its own.
        memory
            .insert_conversation(
                "user",
                PAIRED_QUESTION,
                Some("test-model"),
                Some("session-question"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                PAIRED_ANSWER,
                Some("test-model"),
                Some("session-answer"),
            )
            .await?;

        let for_question = memory
            .query("deployment token for the staging cluster", Some(5))
            .await?;
        assert!(
            for_question
                .iter()
                .any(|r| r.starts_with("user: ") && r.contains("staging cluster")),
            "a recalled user turn must be labelled as the user's; got \
             {for_question:?}"
        );

        let for_answer = memory
            .query("staging deployment token Employee vault", Some(5))
            .await?;
        assert!(
            for_answer
                .iter()
                .any(|r| r.starts_with("assistant: ") && r.contains("Employee vault")),
            "a recalled assistant turn must be labelled as the assistant's; \
             got {for_answer:?}"
        );
        Ok(())
    }

    /// #415 defect 3. The exchange is split across separate leaves, so
    /// retrieving one half returned a question the model cannot see the
    /// answer to — worse than recalling nothing.
    #[tokio::test]
    async fn test_a_recalled_question_brings_its_answer() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory
            .insert_conversation(
                "user",
                PAIRED_QUESTION,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                PAIRED_ANSWER,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;

        let results = memory
            .query("deployment token for the staging cluster", Some(5))
            .await?;
        assert!(
            results.iter().any(|r| r.starts_with("user: ")
                && r.contains(PAIRED_QUESTION)
                && r.contains("assistant: ")
                && r.contains(PAIRED_ANSWER)),
            "a recalled question must arrive with its answer; got {results:?}"
        );
        Ok(())
    }

    /// #415 defect 3 from the other side: recalling the answer half of an
    /// exchange must bring the question it replied to.
    #[tokio::test]
    async fn test_a_recalled_answer_brings_its_question() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory
            .insert_conversation(
                "user",
                PAIRED_QUESTION,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                PAIRED_ANSWER,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;

        let results = memory
            .query("where is the staging deployment token stored", Some(5))
            .await?;
        assert!(
            results.iter().any(|r| r.starts_with("user: ")
                && r.contains(PAIRED_QUESTION)
                && r.contains(PAIRED_ANSWER)),
            "a recalled answer must arrive with the question it replies to; \
             got {results:?}"
        );
        Ok(())
    }

    /// #415 defect 3, dedup complement. Both halves of one exchange are
    /// separate leaves and a query that matches both used to return the
    /// question and the answer as two entries; now each half renders the
    /// whole exchange, so recall must collapse them to one.
    #[tokio::test]
    async fn test_recalling_both_halves_of_one_exchange_injects_it_once() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        })?;
        memory
            .insert_conversation(
                "user",
                PAIRED_QUESTION,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;
        memory
            .insert_conversation(
                "assistant",
                PAIRED_ANSWER,
                Some("test-model"),
                Some("session-pair"),
            )
            .await?;

        let results = memory
            .query("the staging deployment token", Some(5))
            .await?;
        assert_eq!(
            results.len(),
            1,
            "both halves of one exchange render the same exchange and must be \
             injected once; got {results:?}"
        );
        assert!(
            results
                .first()
                .is_some_and(|r| r.contains(PAIRED_QUESTION) && r.contains(PAIRED_ANSWER)),
            "the single injected entry must carry the whole exchange; got \
             {results:?}"
        );
        Ok(())
    }

    // --- turn-level injection gate (#1134) ---

    /// A stored fact and a probe over part of its vocabulary: the probe
    /// retrieves the fact at a weighted score that clears the per-result
    /// floor (0.15) -- the leak the turn-level gate exists for -- while
    /// sitting strictly below self-similarity.
    const GATE_SEED: &str = "The deploy key for the production environment lives \
         in the Employee vault under the Finch signing item, not in the repository.";
    const GATE_PROBE: &str = "The deploy key for the production environment lives \
         in the Employee vault";
    /// A deliberately weak-but-not-sub-floor memory. The hashed-n-gram fallback
    /// embeds character n-grams, so even a topic-disjoint English sentence
    /// shares enough letter pairs to clear the 0.15 default floor -- which
    /// is exactly the leak family the turn-level gate addresses. Weak
    /// entries are measured, never assumed: tests set floors just above a
    /// baseline-measured score instead of guessing one.
    const GATE_WEAK_MEMORY: &str =
        "Zebra herds migrate across vast savannah plains during seasonal rains.";

    /// Seed one fresh store with [`GATE_SEED`] and recall [`GATE_PROBE`],
    /// returning the recalled best weighted score and result count. The
    /// hashed-n-gram engine is deterministic over identical content, so two
    /// identically-seeded stores produce identical scores -- the only
    /// difference between two such recalls is the knob's value.
    async fn seeded_recall_probe(min_turn: Option<f32>) -> Result<(f32, usize)> {
        let temp = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            min_turn_relevance_score: min_turn,
            ..Default::default()
        })?;
        memory
            .insert_conversation("user", GATE_SEED, None, None)
            .await?;
        let results = memory.query_with_sources(GATE_PROBE, Some(5)).await?;
        let best = results.iter().map(|r| r.score).fold(0.0_f32, f32::max);
        Ok((best, results.len()))
    }

    /// The leak precondition every behavior test below reasons from: under
    /// the default configuration the probe's best result clears the 0.15
    /// per-result floor and is injected. If this ever fails, the turn-gate
    /// tests no longer exercise the ticket's scenario and must be re-based
    /// on a probe that does.
    async fn assert_leak_precondition_holds() -> Result<f32> {
        let (best, count) = seeded_recall_probe(None).await?;
        assert!(
            count >= 1 && best > 0.15,
            "leak precondition failed: the probe must recall the seeded memory \
             at a best score above the 0.15 per-result floor (else the \
             per-result filter alone already empties the turn and there is \
             nothing for the turn gate to skip); observed count {count}, best \
             score {best}"
        );
        Ok(best)
    }

    #[tokio::test]
    async fn test_turn_gate_skips_injection_when_best_score_below_turn_floor() -> Result<()> {
        let best = assert_leak_precondition_holds().await?;
        let turn_floor = best + 0.02;
        let (_, count) = seeded_recall_probe(Some(turn_floor)).await?;
        assert_eq!(
            count, 0,
            "the turn-level gate must skip this turn: the probe's best score \
             {best} cleared the 0.15 per-result floor (so the per-result \
             filter alone would have injected it) but sits below the turn \
             floor {turn_floor}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_turn_gate_allows_strong_results_and_keeps_per_result_floor() -> Result<()> {
        // Baseline (gate off, default floors): both memories recall against
        // the seed query. The weak memory clears the 0.15 default floor --
        // which is why it leaks into turns today -- while sitting far below
        // the seed's self-similarity. Its score is measured, not assumed,
        // so the gated store's per-result floor can be set just above it.
        let baseline_store = NamedTempFile::new()?;
        let baseline = MemorySystem::new(MemoryConfig {
            db_path: baseline_store.path().to_path_buf(),
            ..Default::default()
        })?;
        baseline
            .insert_conversation("user", GATE_SEED, None, None)
            .await?;
        baseline
            .insert_conversation("user", GATE_WEAK_MEMORY, None, None)
            .await?;
        let base_results = baseline.query_with_sources(GATE_SEED, Some(5)).await?;
        let weak_score = base_results
            .iter()
            .find(|r| r.text.contains("Zebra"))
            .map(|r| r.score)
            .with_context(|| {
                format!(
                    "the weak memory must recall in the baseline so its score \
                     can be measured; got {base_results:?}"
                )
            })?;
        assert!(
            weak_score >= 0.15,
            "the weak memory must clear the 0.15 default per-result floor for \
             this test to exercise the leak family (a sub-floor entry would \
             be dropped before the turn gate matters); measured {weak_score}"
        );

        // Gated store, identically seeded: the turn floor sits below the
        // seed's self-similarity (the turn is allowed) while the per-result
        // floor sits just above the measured weak score (the weak entry
        // must still drop).
        let gated_store = NamedTempFile::new()?;
        let memory = MemorySystem::new(MemoryConfig {
            db_path: gated_store.path().to_path_buf(),
            min_relevance_score: weak_score + 0.02,
            min_turn_relevance_score: Some(0.5),
            ..Default::default()
        })?;
        memory
            .insert_conversation("user", GATE_SEED, None, None)
            .await?;
        memory
            .insert_conversation("user", GATE_WEAK_MEMORY, None, None)
            .await?;

        let results = memory.query_with_sources(GATE_SEED, Some(5)).await?;
        assert!(
            !results.is_empty(),
            "the gate must allow this turn: the seed's self-similar best score \
             (~1.2 with the conversation boost) clears the 0.5 turn floor; got \
             {results:?}"
        );
        assert!(
            results.iter().all(|r| !r.text.contains("Zebra")),
            "on an allowed turn the per-result floor must still drop the weak \
             entry (measured score {weak_score} vs floor {}); got texts {:?}",
            weak_score + 0.02,
            results.iter().map(|r| &r.text).collect::<Vec<_>>()
        );
        assert!(
            results.iter().all(|r| r.score >= weak_score + 0.02),
            "every allowed injection must clear the configured per-result \
             floor; got {:?}",
            results
                .iter()
                .map(|r| (&r.text, r.score))
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_turn_gate_default_off_preserves_current_injection() -> Result<()> {
        assert!(
            MemoryConfig::default().min_turn_relevance_score.is_none(),
            "the turn-level gate must ship disabled: MemoryConfig::default() \
             carries min_turn_relevance_score = None so existing behavior is \
             unchanged until the knob is set"
        );
        let (best, count) = seeded_recall_probe(None).await?;
        assert!(
            count >= 1 && best > 0.15,
            "with the gate off (default), recall must behave exactly as before \
             the gate existed: the probe recalls its seed (count {count}, best \
             score {best})"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_turn_gate_knob_flips_the_injection_decision() -> Result<()> {
        // Identically-seeded stores; hashed-n-gram scoring is deterministic over
        // identical content, so the only difference between the two recalls
        // below is the knob's value. This is the flip itself.
        let (best_off, count_off) = seeded_recall_probe(None).await?;
        assert!(
            count_off >= 1,
            "the off-half of the flip must inject (count {count_off}, best \
             score {best_off}) or the flip proves nothing"
        );
        let (_, count_on) = seeded_recall_probe(Some(best_off + 0.02)).await?;
        assert_eq!(
            count_on,
            0,
            "the same query on the same content must inject with the knob off \
             (count {count_off}) and inject nothing with the knob at {} \
             (count {count_on})",
            best_off + 0.02
        );
        Ok(())
    }

    #[test]
    fn test_turn_injection_decision_no_gate_never_skips() {
        let empty = turn_injection_decision(None, &[]);
        assert!(
            !empty.skips(),
            "a disabled gate must never skip a turn; decision {empty:?}"
        );
        let strong = turn_injection_decision(None, &[0.99]);
        assert!(
            !strong.skips(),
            "a disabled gate must never skip a turn; decision {strong:?}"
        );
        let mixed = turn_injection_decision(None, &[0.1, 0.9, 0.3]);
        assert!(
            !mixed.skips(),
            "a disabled gate must never skip a turn; decision {mixed:?}"
        );
    }

    #[test]
    fn test_turn_injection_decision_empty_retrieval_never_skips() {
        let decision = turn_injection_decision(Some(0.01), &[]);
        assert!(
            !decision.skips(),
            "an empty retrieval has nothing to gate, so even a low floor must \
             not skip; decision {decision:?}"
        );
        assert_eq!(
            decision.best_score, None,
            "an empty retrieval must report no best score, not a fabricated one"
        );
    }

    #[test]
    fn test_turn_injection_decision_best_strictly_below_floor_skips() {
        let decision = turn_injection_decision(Some(0.6), &[0.4, 0.2, 0.55]);
        assert!(
            decision.skips(),
            "the best score 0.55 sits strictly below the turn floor 0.6, so \
             the turn must skip; decision {decision:?}"
        );
        assert_eq!(
            decision,
            TurnInjectionDecision {
                turn_floor: Some(0.6),
                best_score: Some(0.55),
            },
            "the decision must carry the exact numbers the log line discloses, \
             computed over all candidates regardless of order"
        );
    }

    #[test]
    fn test_turn_injection_decision_best_at_floor_allows() {
        let decision = turn_injection_decision(Some(0.6), &[0.6, 0.2]);
        assert!(
            !decision.skips(),
            "the gate skips strictly below the floor: a best score equal to \
             the turn floor must allow; decision {decision:?}"
        );
    }

    // The gate's log-line observability proof lives in
    // `tests/gate_observability_test.rs`: tracing caches per-callsite
    // interest globally within a process, so when sibling lib tests that
    // exercise the gate run in parallel with a `with_default` capture, a
    // concurrent no-dispatcher evaluation can re-cache a gate callsite as
    // disabled mid-capture and silently drop the very line being asserted.
    // A dedicated integration binary gives the capture window the process
    // to itself, making the proof deterministic.
}
