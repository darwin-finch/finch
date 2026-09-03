// Memory system for Finch
//
// Hierarchical semantic memory using MemTree
// - Client-side storage (CLI, not daemon)
// - SQLite with WAL mode for concurrency
// - O(log N) insertion for real-time updates
// - Cross-session context recall

mod embeddings;
mod memtree;
pub mod neural_embedding;
mod program_registry;
pub mod quality;

pub use embeddings::{average_embeddings, cosine_similarity, EmbeddingEngine, TfIdfEmbedding};
pub use memtree::{MemTree, NodeId, TreeNode};
pub use neural_embedding::NeuralEmbeddingEngine;
pub use quality::{MemoryClassifier, MemoryImportance};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::Mutex;

#[cfg(test)]
#[derive(Debug)]
struct HydrationBatchPause {
    after_loaded: usize,
    reached: watch::Sender<bool>,
    release: watch::Receiver<bool>,
}

#[cfg(test)]
impl HydrationBatchPause {
    async fn after_batch(&self, loaded: usize) {
        if loaded < self.after_loaded {
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

#[cfg(test)]
static HYDRATION_BATCH_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<HashMap<PathBuf, Arc<HydrationBatchPause>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
pub(crate) struct HydrationBatchPauseRegistration {
    path: PathBuf,
    pause: Arc<HydrationBatchPause>,
}

#[cfg(test)]
impl Drop for HydrationBatchPauseRegistration {
    fn drop(&mut self) {
        let mut pauses = HYDRATION_BATCH_PAUSES
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

/// Hold the production loader after `after_loaded` nodes so a test can
/// observe a genuinely `Loading` index. Visible to `src/runtime` so the typed
/// `mem-index-status` regression can drive the state #295 is about.
#[cfg(test)]
pub(crate) fn register_hydration_batch_pause(
    path: PathBuf,
    after_loaded: usize,
) -> (
    HydrationBatchPauseRegistration,
    watch::Receiver<bool>,
    watch::Sender<bool>,
) {
    let (reached, reached_rx) = watch::channel(false);
    let (release, release_rx) = watch::channel(false);
    let pause = Arc::new(HydrationBatchPause {
        after_loaded,
        reached,
        release: release_rx,
    });
    let mut pauses = HYDRATION_BATCH_PAUSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        pauses.insert(path.clone(), Arc::clone(&pause)).is_none(),
        "a hydration pause is already registered for {}",
        path.display()
    );
    drop(pauses);
    (
        HydrationBatchPauseRegistration { path, pause },
        reached_rx,
        release,
    )
}

#[cfg(test)]
fn take_hydration_batch_pause(path: &std::path::Path) -> Option<Arc<HydrationBatchPause>> {
    HYDRATION_BATCH_PAUSES
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
    /// Use neural ONNX embeddings when the model is cached (default: true).
    /// Falls back to TF-IDF if the model is not yet downloaded.
    pub use_neural_embeddings: bool,
    /// Directory where the embedding model is cached / downloaded.
    pub embedding_cache_dir: PathBuf,
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
        }
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
    #[cfg(test)]
    batch_pause: std::sync::Mutex<Option<Arc<HydrationBatchPause>>>,
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
            #[cfg(test)]
            batch_pause: std::sync::Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn install_batch_pause(&self, pause: Option<Arc<HydrationBatchPause>>) {
        *self
            .batch_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pause;
    }

    #[cfg(test)]
    async fn pause_after_batch(&self, loaded: usize) {
        let pause = self
            .batch_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(pause) = pause {
            pause.after_batch(loaded).await;
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

/// Nodes hydrated per batch. Small enough that the tree and database locks are
/// released frequently, so an interactive turn never waits on one long hold.
pub(crate) const HYDRATION_BATCH: usize = 512;

/// Memory system with MemTree and SQLite storage
pub struct MemorySystem {
    db: Arc<Mutex<Connection>>,
    tree: Arc<Mutex<MemTree>>,
    /// Serialize conversation projection through semantic indexing. This keeps
    /// the SQLite identity, in-memory leaf, and durable leaf provenance from
    /// racing when the daemon retries one completed Brain run.
    insert_lock: Arc<Mutex<()>>,
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: MemoryConfig,
    hydration: Arc<HydrationState>,
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

impl MemorySystem {
    /// Create new memory system (synchronous).
    ///
    /// If `config.use_neural_embeddings` is true and the model is already in
    /// the HuggingFace cache, a `NeuralEmbeddingEngine` is used; otherwise
    /// falls back to `TfIdfEmbedding`.  Call `new_async` to trigger a
    /// download on first run.
    pub fn new(config: MemoryConfig) -> Result<Self> {
        // Ensure directory exists
        if let Some(parent) = config.db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        // Open SQLite connection with WAL mode
        let conn = Connection::open(&config.db_path)
            .with_context(|| format!("Failed to open database: {}", config.db_path.display()))?;

        // Enable WAL mode for concurrency
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;

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
            anyhow::ensure!(
                stale == 0,
                "{} predates the current memory schema and cannot be upgraded \
                 in place. Storing the same content twice would fail with a \
                 UNIQUE constraint error.\n\n\
                 Move it aside and Finch will create a fresh store, keeping \
                 the old one readable with any SQLite client:\n\
                 \x20\x20mv {} {}.pre-schema-change\n\n\
                 Do not delete it unless you are certain the history is not \
                 wanted — there is no export path yet.",
                config.db_path.display(),
                config.db_path.display(),
                config.db_path.display()
            );
        }

        // Migration A: detect old tree_nodes schema (primary key was 'id AUTOINCREMENT',
        // not 'node_id').  The old table always had 0 rows because inserts failed with
        // FK violations, so dropping it is safe.  We detect by checking whether the
        // 'node_id' column is absent from an existing table.
        {
            let table_exists: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tree_nodes'",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            if table_exists > 0 {
                let has_node_id: i64 = conn
                    .query_row(
                        "SELECT COUNT(*) FROM pragma_table_info('tree_nodes') WHERE name='node_id'",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap_or(0);
                if has_node_id == 0 {
                    tracing::info!(
                        "Dropping stale tree_nodes table (old schema used 'id', not 'node_id')"
                    );
                    conn.execute_batch("DROP TABLE IF EXISTS tree_nodes;")?;
                }
            }
        }

        // Load schema (CREATE TABLE IF NOT EXISTS — safe to re-run)
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema)?;

        // Migration B: add importance column if the DB predates v0.7.15.
        // Silently ignored if the column already exists.
        let _ = conn.execute(
            "ALTER TABLE tree_nodes ADD COLUMN importance INTEGER NOT NULL DEFAULT 1",
            [],
        );

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

        tracing::debug!("Memory system initialized: {}", config.db_path.display());

        // Select embedding engine: try neural if enabled and cached, else TF-IDF.
        let embedding_engine: Arc<dyn EmbeddingEngine> = if config.use_neural_embeddings {
            match NeuralEmbeddingEngine::find_in_cache()
                .and_then(|dir| NeuralEmbeddingEngine::load(&dir).ok())
            {
                Some(neural) => {
                    tracing::debug!("Using neural ONNX embeddings (all-MiniLM-L6-v2)");
                    Arc::new(neural)
                }
                None => {
                    tracing::debug!(
                        "Neural embedding model not in cache — using TF-IDF fallback. \
                         Run `finch memory download` or call MemorySystem::new_async() \
                         to download."
                    );
                    Arc::new(TfIdfEmbedding::new())
                }
            }
        } else {
            Arc::new(TfIdfEmbedding::new())
        };

        // Parameterize MemTree dimension to match the chosen engine.
        let dim = embedding_engine.dimension();
        let mut tree = MemTree::new_with_dim(dim);

        // Hydrate the MemTree from `tree_nodes`.
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
            .query_row("SELECT COUNT(*) FROM tree_nodes", [], |row| row.get(0))
            .context("Failed to count stored MemTree nodes")?;
        let max_node_id: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(node_id), 0) FROM tree_nodes",
                [],
                |row| row.get(0),
            )
            .context("Failed to read the highest stored MemTree node id")?;
        tree.set_next_id(max_node_id as u64 + 1);

        let hydration = Arc::new(HydrationState::new(node_count.max(0) as usize));
        #[cfg(test)]
        hydration.install_batch_pause(take_hydration_batch_pause(&config.db_path));
        let db = Arc::new(Mutex::new(conn));
        let tree = Arc::new(Mutex::new(tree));
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
                    let db = Arc::clone(&db);
                    let tree = Arc::clone(&tree);
                    let state = Arc::clone(&hydration);
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
                        Self::hydrate_in_background(db, tree, state).await;
                    }));
                }
                _ => {
                    // A current-thread runtime, or no runtime at all.
                    let mut guard = tree
                        .try_lock()
                        .expect("a newly constructed MemTree cannot be contended");
                    let conn = db
                        .try_lock()
                        .expect("a newly opened connection cannot be contended");
                    if let Err(error) = Self::load_tree_from_db_conn(&conn, &mut guard) {
                        // Broken, not fresh.
                        //
                        // This arm is only entered when `node_count > 0`, so
                        // reaching the error means there ARE rows and none of
                        // them could be read — a full store behind an empty
                        // tree, which is the same `loaded == 0` case the
                        // batched arm now refuses. "Starting fresh" was the
                        // wrong framing: an empty tree over a populated store
                        // is not a new store, and treating it as one opened the
                        // write gate on nothing but the placeholder root.
                        // Memories then attached under a childless root and
                        // `save_all_nodes_to_db` rewrote node 0 with the
                        // placeholder's text and embedding.
                        //
                        // An earlier version of this comment argued the
                        // opposite, on the grounds that `fail()` then meant a
                        // permanent refusal. It no longer does: a reload clears
                        // it, and #288 separates a partial index from an
                        // unusable one.
                        tracing::warn!(
                            %error,
                            "Failed to load MemTree; refusing writes rather than \
                             placing them against an empty index"
                        );
                        hydration.fail(error.to_string());
                    } else {
                        // `size()` excludes the root; the background arm
                        // counts rows, which include it. Count rows on both so
                        // `Ready { nodes }` means one thing.
                        hydration
                            .loaded
                            .store(guard.all_nodes().len(), std::sync::atomic::Ordering::SeqCst);
                        hydration.complete();
                    }
                }
            }
        }

        Ok(Self {
            db,
            tree,
            hydration,
            hydration_task,
            insert_lock: Arc::new(Mutex::new(())),
            embedding_engine,
            config,
        })
    }

    /// Create a new memory system, downloading the neural model if needed.
    ///
    /// Same as `new()` but also triggers `NeuralEmbeddingEngine::ensure_downloaded()`
    /// before constructing, so the first run downloads the model rather than
    /// falling back to TF-IDF.
    pub async fn new_async(config: MemoryConfig) -> Result<Self> {
        if config.use_neural_embeddings {
            match NeuralEmbeddingEngine::ensure_downloaded().await {
                Ok(_) => tracing::info!("Neural embedding model ready"),
                Err(e) => tracing::warn!("Could not download neural model: {} — using TF-IDF", e),
            }
        }
        Self::new(config)
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
        // `memory_sources` row, which is exactly the state a later
        // re-projection repairs.
        self.ensure_hydrated().await?;

        // Quality filter: classify and extract key content before indexing.
        // Low-signal content (acks, greetings) is skipped in MemTree but still
        // written to the conversations table above for raw history.
        let classifier = MemoryClassifier::new();
        if let Some((key_content, importance)) = classifier.process(role, content) {
            let embedding = self.embedding_engine.embed(&key_content)?;
            let effect = {
                let mut tree = self.tree.lock().await;
                tree.insert_with_effect(key_content, embedding, importance.as_u8())
            };
            let effect = match effect {
                Ok(effect) => effect,
                Err(error) => {
                    // Same reason as the `save_all_nodes_to_db` failure below.
                    // `attach_child` and `promote_leaf` insert the new node and
                    // push it into its parent's `children` BEFORE aggregating,
                    // so an aggregation error leaves the tree mutated. Without
                    // this, an identical retry of the write that just hard
                    // failed succeeds — `find_leaf_by_text` dedups to the node
                    // the failed insert left behind and returns before
                    // aggregation, so the guard is never reached — and then
                    // persists. A caller cannot act on an error that behaves
                    // that way.
                    //
                    // The reload's own failure is logged, not returned: `error`
                    // is the cycle diagnostic this whole change exists to
                    // produce, and replacing it with a generic SQLite error
                    // would leave a structurally corrupt store looking like a
                    // transient I/O problem.
                    if let Err(reload_error) = self.reload_tree_from_db().await {
                        // `?`, not `%`: `Display` on an `anyhow::Error` prints
                        // only the outermost message and drops the source chain,
                        // which is where a restore failure's actual cause lives.
                        tracing::error!(
                            ?reload_error,
                            "could not restore the MemTree after a failed insert; \
                             the in-memory index is inconsistent until restart"
                        );
                    }
                    return Err(error);
                }
            };
            let node_id = effect.node;
            // Persist all nodes (root + ancestors + new leaf) so the DB stays
            // consistent across process restarts and FK constraints are satisfied.
            // The source mapping commits in the same SQLite transaction, so a
            // retry cannot create a second semantic leaf for the same turn.
            if let Err(error) = self
                .save_all_nodes_to_db(Some((node_id, &id, timestamp)), effect.promotion)
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
                if let Err(reload_error) = self.reload_tree_from_db().await {
                    tracing::error!(
                        ?reload_error,
                        "could not restore the MemTree after a failed save; the \
                         in-memory index is inconsistent until restart"
                    );
                }
                return Err(error);
            }
        } else {
            let conn = self.db.lock().await;
            conn.execute(
                "INSERT INTO memory_sources (conversation_id, node_id, indexed_at)
                 VALUES (?1, NULL, ?2)",
                params![&id, timestamp],
            )?;
        }

        tracing::debug!("Inserted conversation into memory: {} chars", content.len());

        Ok(true)
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
        let leaves = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0 && node.children.is_empty())
            .count();
        let widest = tree
            .all_nodes()
            .values()
            .filter(|node| node.id != 0)
            .map(|node| node.children.len())
            .max()
            .unwrap_or(0);
        (leaves, tree.max_depth(), widest)
    }

    /// Query memory for relevant context
    pub async fn query(&self, query_text: &str, top_k: Option<usize>) -> Result<Vec<String>> {
        let results = self.query_with_sources(query_text, top_k).await?;
        let texts: Vec<String> = results.into_iter().map(|result| result.text).collect();

        tracing::debug!("Memory query returned {} results", texts.len());

        Ok(texts)
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
            let tree = self.tree.lock().await;
            tree.retrieve(&query_embedding, k)
        };
        let conn = self.db.lock().await;
        let mut results = Vec::with_capacity(retrieved.len());
        for (node_id, text, score) in retrieved {
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
        tracing::debug!("Memory query returned {} sourced results", results.len());
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
            return Ok(tree.get_node(node_id).map(|node| InspectedMemory {
                memory_id: memory_id.to_string(),
                node_id: Some(node_id),
                source: None,
                content: node.text.clone(),
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

    /// Persist the MemTree nodes that changed, in a single transaction.
    ///
    /// Nodes are written in ascending node_id order (root first) so that the
    /// self-referential FK `parent_id → node_id` is satisfied for each INSERT.
    /// SQLite enforces it immediately, and a new node always takes a higher id
    /// than the parent it attaches to, so ascending order is sufficient.
    ///
    /// Two guarantees this must keep, both of which an earlier
    /// `save_node_to_db(leaf_id)` missed and which were the reason it was
    /// widened to write everything:
    ///   1. The root node (id=0) reaches the table, or children violate the FK
    ///      (libsqlite3-sys bundles SQLite with SQLITE_DEFAULT_FOREIGN_KEYS=1).
    ///      `MemTree::new_with_dim` marks the root dirty for exactly this.
    ///   2. Parent embeddings updated by `update_parent_aggregation` are
    ///      persisted, or they go stale across restarts. That walk marks every
    ///      node whose embedding it rewrites.
    ///
    /// Writing *everything* kept both guarantees at a cost proportional to the
    /// whole store: on the dogfood host, 16,782 rows and 131 MiB of embeddings
    /// rewritten per turn, which is why its write-ahead log matched its
    /// database at 142 MiB (#250).
    async fn save_all_nodes_to_db(
        &self,
        source: Option<(NodeId, &str, i64)>,
        promotion: Option<(NodeId, NodeId)>,
    ) -> Result<()> {
        let (changed, nodes): (Vec<NodeId>, Vec<TreeNode>) = {
            let tree = self.tree.lock().await;
            let changed = tree.dirty_nodes();
            let mut nodes = Vec::with_capacity(changed.len());
            for id in &changed {
                let node = tree.all_nodes().get(id).ok_or_else(|| {
                    // Unreachable while nothing removes nodes, and an error
                    // rather than a skip precisely because this change exists
                    // to bound silent loss: dropping a marked id here would be
                    // the failure it is trying to prevent.
                    anyhow::anyhow!(
                        "memory: node {id} was marked for persistence but is not in the tree"
                    )
                })?;
                nodes.push(node.clone());
            }
            (changed, nodes)
        };

        // The marks are *copied* above, not drained, and are cleared only once
        // the transaction has committed. So a save that fails, panics, or is
        // cancelled between here and the commit leaves them set, and the next
        // save writes them. Draining first and restoring on error -- the
        // obvious shape -- silently lost them to cancellation, which returns no
        // error to restore on.
        self.write_nodes(&nodes, source, promotion).await?;
        self.tree.lock().await.mark_persisted(&changed);
        Ok(())
    }

    /// Upsert exactly `nodes`, plus the provenance rows for this insert.
    async fn write_nodes(
        &self,
        nodes: &[TreeNode],
        source: Option<(NodeId, &str, i64)>,
        promotion: Option<(NodeId, NodeId)>,
    ) -> Result<()> {
        let conn = self.db.lock().await;
        let tx = conn.unchecked_transaction()?;
        for node in nodes {
            let embedding_bytes: Vec<u8> = node
                .embedding
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            // Upsert, never REPLACE. `INSERT OR REPLACE` deletes the existing
            // row before reinserting it, and `memory_sources.node_id` is
            // declared `ON DELETE CASCADE` — so a plain REPLACE here silently
            // cascades away the provenance of every node it rewrites. Since
            // this function used to rewrite the whole tree on every insert,
            // that destroyed every source row except the one written later in
            // the same transaction.
            //
            // (That rewrite is now scoped to the nodes that changed, so the
            // blast radius is smaller -- but REPLACE would still cascade away
            // the provenance of every node it did touch.)
            //
            // The dogfood store held 9 rows against 896 conversations. The
            // cascade alone accounts for all but one of those; the likely
            // explanation for the survivors is the `node_id IS NULL` rows
            // written for classifier-excluded turns, which a cascade through
            // `tree_nodes` cannot reach. That is a hypothesis, not a measured
            // fact.
            tx.execute(
                "INSERT INTO tree_nodes
                 (node_id, parent_id, text, embedding, level, created_at, importance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(node_id) DO UPDATE SET
                     parent_id = excluded.parent_id,
                     text = excluded.text,
                     embedding = excluded.embedding,
                     level = excluded.level,
                     created_at = excluded.created_at,
                     importance = excluded.importance",
                params![
                    node.id as i64,
                    node.parent.map(|p| p as i64),
                    &node.text,
                    &embedding_bytes,
                    node.level as i64,
                    node.created_at,
                    node.importance as i64,
                ],
            )?;
        }
        // A promotion moved the promoted node's content into a new leaf. Any
        // conversation attributed to that node was attributed to those words,
        // so its provenance follows them; leaving it behind would point the row
        // at an aggregate whose embedding is the mean of two memories, and the
        // leaf that actually holds the text would have no source at all.
        if let Some((promoted, moved)) = promotion {
            tx.execute(
                "UPDATE memory_sources SET node_id = ?1 WHERE node_id = ?2",
                params![moved as i64, promoted as i64],
            )?;
        }

        if let Some((node_id, conversation_id, indexed_at)) = source {
            // `node_id` is not unique: deduplicated content is one node with
            // several source conversations. `conversation_id` is the primary
            // key, so a retry of the same turn is still idempotent.
            tx.execute(
                "INSERT OR REPLACE INTO memory_sources (conversation_id, node_id, indexed_at)
                 VALUES (?1, ?2, ?3)",
                params![conversation_id, node_id as i64, indexed_at],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    async fn reload_tree_from_db(&self) -> Result<()> {
        let mut restored = MemTree::new_with_dim(self.embedding_engine.dimension());
        {
            let conn = self.db.lock().await;
            Self::load_tree_from_db_conn(&conn, &mut restored)?;
        }
        let nodes = restored.all_nodes().len();
        // The tree was just built from the durable rows, so nothing in it is
        // pending. `new_with_dim` marks the root dirty for the fresh-store
        // case; here that row already exists.
        restored.clear_dirty();
        *self.tree.lock().await = restored;
        // The tree now matches the durable snapshot, so whatever the loader
        // recorded no longer describes it. Leaving the failure set meant a
        // store that had been rebuilt successfully still refused every write,
        // with no way back short of a restart that would re-read the same rows
        // and fail again (#288).
        self.hydration.clear_failure(nodes);
        Ok(())
    }

    /// Reconstruct MemTree from the tree_nodes table at startup.
    /// Load persisted nodes in bounded batches, releasing both locks between
    /// each one so an interactive turn is never blocked for long.
    ///
    /// Batches are ordered by `node_id`, so a parent may arrive after its
    /// child, and the full child lists are only correct once every node is
    /// present — hence the final linking pass.
    ///
    /// But a node cannot be allowed to *look* like a leaf until then. Reads are
    /// deliberately not gated on hydration, and `MemTree::retrieve` identifies
    /// leaves as `children.is_empty()`; an unlinked tree therefore makes every
    /// internal node answer queries, surfacing provisional labels duplicated
    /// from a child and mean-of-subtree embeddings as if they were memories.
    /// That is the window this change creates, on the first turn, which is
    /// exactly when it would be seen.
    ///
    /// So the edge list is read first, in one query: two integers per row, a
    /// couple of megabytes resident against 131 MiB of embeddings on the
    /// dogfood store, so it does not reintroduce the startup cost this change
    /// exists to remove. Every node is linked as its batch lands. The final
    /// pass stays, both as a safety net and to cover rows written after the
    /// edge query.
    async fn hydrate_in_background(
        db: Arc<Mutex<Connection>>,
        tree: Arc<Mutex<MemTree>>,
        state: Arc<HydrationState>,
    ) {
        // Every parent's child list, before any node lands. See the doc
        // comment: an unlinked node is indistinguishable from a leaf, and reads
        // are not gated on hydration.
        let edges = {
            let conn = db.lock().await;
            Self::load_child_edges(&conn)
        };
        Self::hydrate_batches(db, tree, state, Self::edges_or_degraded(edges)).await;
    }

    /// Degrade, do not abandon hydration.
    ///
    /// The edge query only buys read purity during the window. Failing it used
    /// to call `fail()`, which opens the write gate — against a tree holding
    /// nothing but a fresh root, because this runs before the first batch. The
    /// next write would attach under that root and `save_all_nodes_to_db` would
    /// persist it: the durable misplacement the gate exists to prevent.
    ///
    /// This takes no `HydrationState`, so it *cannot* fail the hydration. An
    /// empty map reproduces exactly the pre-#242 behaviour — flat until the
    /// final pass, then a complete and correctly linked tree.
    fn edges_or_degraded(edges: Result<HashMap<u64, Vec<u64>>>) -> HashMap<u64, Vec<u64>> {
        edges.unwrap_or_else(|error| {
            tracing::warn!(
                %error,
                "MemTree hydration could not read child links; reads during \
                 hydration may return internal nodes until the final pass"
            );
            HashMap::new()
        })
    }

    /// The batch loop, taking the edge map rather than reading it.
    ///
    /// Split out so the degraded path is reachable from a test. Both queries
    /// read `tree_nodes`, so there is no way to fail the edge query and not the
    /// batch query against a real database — without this seam, "hydration
    /// still completes correctly when the edge list could not be read" would
    /// have no coverage at all, and re-adding the `fail()` that used to be
    /// there would be caught by nothing.
    async fn hydrate_batches(
        db: Arc<Mutex<Connection>>,
        tree: Arc<Mutex<MemTree>>,
        state: Arc<HydrationState>,
        edges: HashMap<u64, Vec<u64>>,
    ) {
        let mut cursor: Option<u64> = None;
        loop {
            // The database guard is scoped to the read and nothing else, so
            // it is not held across `link_loaded_children().await` on the
            // failure path below. (An earlier version of this comment claimed a
            // db -> tree lock ordering that the module does not actually have:
            // `stats` nests exactly that way, and nothing nests tree -> db, so
            // there was no deadlock either before or after. Not holding a tokio
            // guard across an await is the whole of the reason.)
            let batch = {
                let conn = db.lock().await;
                Self::load_batch(&conn, cursor, HYDRATION_BATCH)
            };
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    tracing::error!(
                        %error,
                        loaded = state.loaded.load(Ordering::SeqCst),
                        "MemTree hydration failed; the index is incomplete for \
                         the rest of this session"
                    );
                    // Link what did load, THEN fail.
                    //
                    // `fail` completes, and completing opens the write gate, so
                    // the order of these two lines is the fix. Failing first
                    // left every loaded node with an empty `children`, so
                    // `MemTree::insert` saw a childless root, attached every
                    // later memory directly under it, and
                    // `save_all_nodes_to_db` persisted that flattening — the
                    // exact durable misplacement the gate exists to prevent.
                    // `degrade` only if something actually loaded.
                    //
                    // Linking is what makes a partial index coherent, and that
                    // argument is vacuous when the FIRST batch fails: linking
                    // an empty node set leaves the tree holding nothing but the
                    // placeholder root `MemTree::new_with_dim` created. Calling
                    // that `Degraded` opened the write gate on it, so every
                    // memory for the session attached directly under a
                    // childless root and `save_all_nodes_to_db` persisted the
                    // flattening — the exact harm the paragraph above says the
                    // gate exists to prevent, reintroduced by the very
                    // reclassification meant to be safe. It also upserts node 0,
                    // overwriting the stored root row with the placeholder's.
                    //
                    // Nothing loaded is not a smaller index; it is no index.
                    let loaded = state.loaded.load(Ordering::SeqCst);
                    Self::link_loaded_children(&tree).await;
                    if loaded == 0 {
                        state.fail(error.to_string());
                    } else {
                        state.degrade(error.to_string());
                    }
                    return;
                }
            };
            if batch.is_empty() {
                break;
            }

            let count = batch.len();
            cursor = batch.last().map(|node| node.id);
            {
                let mut guard = tree.lock().await;
                let nodes = guard.all_nodes_mut();
                for mut node in batch {
                    if let Some(children) = edges.get(&node.id) {
                        node.children = children.clone();
                    }
                    nodes.insert(node.id, node);
                }
            }
            let loaded = state.loaded.fetch_add(count, Ordering::SeqCst) + count;
            #[cfg(test)]
            state.pause_after_batch(loaded).await;

            // Yield so a turn submitted mid-hydration is served promptly.
            tokio::task::yield_now().await;
        }

        Self::link_loaded_children(&tree).await;
        state.complete();
    }

    /// Rebuild every `children` list from the `parent` links of the nodes
    /// actually in the tree, discarding what was there.
    ///
    /// Rebuild rather than merge, because the batch loader seeds each node with
    /// its full stored child list — including children that have not loaded yet
    /// — so that an unlinked node is never mistaken for a leaf by a read taken
    /// mid-hydration. Those ids are dangling until their nodes arrive, and
    /// `MemTree::insert` rejects a dangling child id outright. Writes are gated
    /// on hydration finishing, so the only way one can reach a write is a
    /// hydration that *failed* partway: this pass runs there too, and the
    /// rebuild is what prunes them.
    ///
    /// Do not add an await after the guard is taken. Clearing before
    /// repopulating means a reader that observed the tree mid-pass would see
    /// every node in a fully loaded tree looking like a leaf — the defect this
    /// linking exists to prevent, at maximum blast radius. The pass is
    /// currently await-free from `tree.lock()` to the end, and that is what
    /// makes clearing safe rather than an accident worth preserving silently.
    ///
    /// It also means read purity is a success-path property. After a *failed*
    /// hydration an internal node whose children never loaded is pruned to an
    /// empty child list and does answer queries as a leaf for the rest of the
    /// session. That is the deliberate trade: a partial index that writes
    /// safely, over a pure one that corrupts.
    async fn link_loaded_children(tree: &Arc<Mutex<MemTree>>) {
        let mut guard = tree.lock().await;
        let parents: Vec<(u64, u64)> = guard
            .all_nodes()
            .values()
            .filter_map(|node| node.parent.map(|parent| (parent, node.id)))
            .collect();
        let nodes = guard.all_nodes_mut();
        for node in nodes.values_mut() {
            node.children.clear();
        }
        for (parent_id, child_id) in parents {
            if let Some(parent) = nodes.get_mut(&parent_id) {
                parent.children.push(child_id);
            }
        }
        for node in nodes.values_mut() {
            node.children.sort_unstable();
        }
    }

    /// Every parent's child list, as one compact query.
    ///
    /// Two integers per row and no embeddings, so this is cheap enough to run
    /// before the first batch: about 8 ms and a couple of megabytes resident
    /// for the dogfood store's 16,782 rows, against 131 MiB of embeddings and
    /// the 3.25 s this change exists to remove.
    ///
    /// No `ORDER BY`. Adding one on `node_id` turns this into a full table
    /// scan; without it `idx_tree_nodes_parent` covers the query outright. The
    /// order is not needed either way — every consumer of these lists reads
    /// only `is_empty()` or `len()`, and `link_loaded_children` sorts.
    fn load_child_edges(conn: &Connection) -> Result<HashMap<u64, Vec<u64>>> {
        let mut stmt =
            conn.prepare("SELECT parent_id, node_id FROM tree_nodes WHERE parent_id IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
        })?;
        let mut edges: HashMap<u64, Vec<u64>> = HashMap::new();
        for row in rows {
            let (parent, child) = row?;
            edges.entry(parent).or_default().push(child);
        }
        Ok(edges)
    }

    /// One page of stored nodes, keyed on the last id already read, without
    /// their child links.
    ///
    /// Keyset, not `LIMIT`/`OFFSET`. The database lock is released between
    /// batches, so the row set can change underneath an offset: a row deleted
    /// below the window shifts every later row back by one and the loader skips
    /// the boundary row for the rest of the session. A cursor on `node_id`
    /// cannot skip, and it is an O(1) primary-key seek rather than an
    /// O(offset) scan.
    ///
    /// The narrower claim, since an earlier version of this comment overstated
    /// it: Finch's own writers hand out ids monotonically from
    /// `set_next_id(MAX + 1)` and never `DELETE FROM tree_nodes`, so the
    /// skipping case is not reachable through Finch today. This removes the
    /// class rather than a demonstrated failure.
    fn load_batch(conn: &Connection, after: Option<u64>, limit: usize) -> Result<Vec<TreeNode>> {
        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, text, embedding, level, created_at, importance
             FROM tree_nodes WHERE node_id > ?1 ORDER BY node_id ASC LIMIT ?2",
        )?;
        // `-1` rather than `0`: node 0 is the root and must be included.
        let after = after.map_or(-1, |id| id as i64);
        let rows = stmt.query_map(params![after, limit as i64], |row| {
            let embedding: Vec<u8> = row.get(3)?;
            Ok(TreeNode {
                id: row.get::<_, i64>(0)? as u64,
                parent: row.get::<_, Option<i64>>(1)?.map(|value| value as u64),
                children: Vec::new(),
                text: row.get(2)?,
                embedding: embedding
                    .chunks_exact(4)
                    .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                    .collect(),
                level: row.get::<_, i64>(4)? as usize,
                created_at: row.get(5)?,
                importance: row.get::<_, i64>(6).unwrap_or(1).clamp(0, 3) as u8,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Progress of the background hydration, for status surfaces.
    pub fn hydration_status(&self) -> HydrationStatus {
        self.hydration.status()
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

    fn load_tree_from_db_conn(conn: &Connection, tree: &mut MemTree) -> Result<()> {
        struct Row {
            node_id: u64,
            parent_id: Option<u64>,
            text: String,
            embedding: Vec<f32>,
            level: usize,
            created_at: i64,
            importance: u8,
        }

        let mut stmt = conn.prepare(
            "SELECT node_id, parent_id, text, embedding, level, created_at, importance
             FROM tree_nodes ORDER BY node_id ASC",
        )?;

        let rows: Vec<Row> = stmt
            .query_map([], |row| {
                let node_id: i64 = row.get(0)?;
                let parent_id: Option<i64> = row.get(1)?;
                let text: String = row.get(2)?;
                let embedding_bytes: Vec<u8> = row.get(3)?;
                let level: i64 = row.get(4)?;
                let created_at: i64 = row.get(5)?;
                let importance: i64 = row.get(6).unwrap_or(1);
                Ok((
                    node_id,
                    parent_id,
                    text,
                    embedding_bytes,
                    level,
                    created_at,
                    importance,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(
                |(node_id, parent_id, text, embedding_bytes, level, created_at, importance)| Row {
                    node_id: node_id as u64,
                    parent_id: parent_id.map(|p| p as u64),
                    text,
                    embedding: embedding_bytes
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect(),
                    level: level as usize,
                    created_at,
                    importance: importance.clamp(0, 3) as u8,
                },
            )
            .collect();

        if rows.is_empty() {
            return Ok(());
        }

        let nodes = tree.all_nodes_mut();
        let mut max_id: u64 = 0;

        // First pass: insert all nodes
        for row in &rows {
            max_id = max_id.max(row.node_id);
            nodes.insert(
                row.node_id,
                TreeNode {
                    id: row.node_id,
                    parent: row.parent_id,
                    children: Vec::new(),
                    text: row.text.clone(),
                    embedding: row.embedding.clone(),
                    level: row.level,
                    created_at: row.created_at,
                    importance: row.importance,
                },
            );
        }

        // Second pass: rebuild children lists
        for row in &rows {
            if let Some(parent_id) = row.parent_id {
                if let Some(parent) = nodes.get_mut(&parent_id) {
                    if !parent.children.contains(&row.node_id) {
                        parent.children.push(row.node_id);
                    }
                }
            }
        }

        // Advance next_id past all loaded IDs
        tree.set_next_id(max_id + 1);

        Ok(())
    }

    /// Persist a successful Lisp `(define ...)` expression for session replay.
    pub async fn save_lisp_define(&self, expr: &str) -> Result<()> {
        let created_at = chrono::Utc::now().timestamp();
        {
            let conn = self.db.lock().await;
            conn.execute(
                "INSERT INTO lisp_env (expr, created_at) VALUES (?1, ?2)",
                rusqlite::params![expr, created_at],
            )?;
        }
        if let Some(definition) = crate::programs::ProgramDefinition::from_lisp_define(expr, None) {
            self.save_authored_program(definition).await?;
        }
        Ok(())
    }

    /// Load legacy persisted Lisp definitions for explicit migration tooling.
    ///
    /// The interactive runtime must not replay these into the native Lisp evaluator: authored
    /// definitions are projected into the shared typed program registry by `save_lisp_define`.
    /// This reader remains temporarily available so older databases can be migrated without
    /// making their obsolete evaluator state authoritative again.
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
    pub async fn conversation_summary(&self, depth: usize) -> Result<ConversationSummaryLines> {
        if depth == 0 {
            return Ok(ConversationSummaryLines::default());
        }

        let tree = self.tree.lock().await;
        let nodes = tree.all_nodes();

        // Collect leaf embeddings and texts (exclude root id=0)
        let mut leaves: Vec<(i64, &Vec<f32>, &str)> = nodes
            .values()
            .filter(|n| n.id != 0 && n.children.is_empty())
            .map(|n| (n.created_at, &n.embedding, n.text.as_str()))
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
        let now_text = truncate_str(leaves[0].2, 70);

        let mut lines: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Centroid queries for all windows except the last ("now") slot.
        for window in windows.iter().take(windows.len().saturating_sub(1)) {
            let slice: Vec<&Vec<f32>> = leaves.iter().take(*window).map(|(_, e, _)| *e).collect();
            let centroid = average_embeddings(&slice);
            if let Some((_, text, _)) = tree.retrieve(&centroid, 1).into_iter().next() {
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

    /// Count writes to `tree_nodes` made by one closure.
    ///
    /// A trigger rather than `Connection::total_changes`, because the store
    /// keeps its connection private: triggers belong to the database, so a
    /// probe installed from a second connection sees writes made through the
    /// first. `ON CONFLICT DO UPDATE` fires the update trigger even when every
    /// column is rewritten to the value it already held, which is exactly the
    /// work this measures.
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
             CREATE TRIGGER probe_tree_insert AFTER INSERT ON tree_nodes
                 BEGIN INSERT INTO write_probe VALUES (1); END;
             CREATE TRIGGER probe_tree_update AFTER UPDATE ON tree_nodes
                 BEGIN INSERT INTO write_probe VALUES (1); END;
             DELETE FROM write_probe;",
        )?;
        Ok(())
    }

    /// Build a store holding `turns` distinct memories and report how many
    /// `tree_nodes` rows one further insert writes.
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

    /// Every persisted column of every node reaches disk.
    ///
    /// This is the guard the incremental save needs and the old whole-tree
    /// write did not. Writing every node on every insert made a forgotten
    /// write impossible; writing only marked ones makes a forgotten *mark*
    /// silent data loss.
    ///
    /// It compares whole nodes, not texts. An earlier version compared sorted
    /// texts and review of #313 showed that was too weak to support the claim
    /// made for it: deleting the dedup importance mark left it green, and so
    /// did deleting the aggregation mark. Text alone cannot see a stale
    /// `importance` or a parent embedding that never got its aggregate. Every
    /// column the table stores is compared here, so a mark forgotten for any
    /// of them fails.
    ///
    /// The inputs drive all three mutation paths. Instrumented while writing
    /// this: 19 attaches, 61 promotions, 40 dedups.
    #[tokio::test]
    async fn test_every_persisted_column_survives_a_reload() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        };

        let expected: Vec<(NodeId, Option<NodeId>, String, usize, u8, Vec<f32>)> = {
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
                // Near-duplicate: close enough to descend to the same leaf and
                // promote it into a parent rather than attach beside it. That
                // is the path that creates aggregated embeddings.
                memory
                    .insert_conversation(
                        "user",
                        &format!("a distinct memory numbered {i} indeed"),
                        None,
                        None,
                    )
                    .await?;
                // Exact repeat: the dedup path, which creates no node.
                memory
                    .insert_conversation(
                        "user",
                        &format!("a distinct memory numbered {i}"),
                        None,
                        None,
                    )
                    .await?;
                // The same fact stored *explicitly*. This is the only way the
                // dedup importance bump fires in production: `process` gives
                // role "system" Critical and classifies everything else, and
                // `extract` is role-independent below 300 characters, so this
                // dedups onto the existing node and raises its importance.
                //
                // Without it the bump never triggers -- identical text from an
                // identical role classifies identically, so
                // `importance > node.importance` is false and there is nothing
                // to persist. Review of #313 caught exactly that: deleting the
                // mark left the suite green because no test reached the branch.
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
            "a node was lost entirely between memory and disk"
        );
        for (before, after) in expected.iter().zip(restored.iter()) {
            assert_eq!(
                before, after,
                "a persisted column did not survive the reload; a mutation that \
                 forgot to mark its node dirty is lost here and nowhere else"
            );
        }
        Ok(())
    }

    /// Every node's persisted columns, ordered by id.
    async fn snapshot(
        memory: &MemorySystem,
    ) -> Vec<(NodeId, Option<NodeId>, String, usize, u8, Vec<f32>)> {
        let tree = memory.tree.lock().await;
        let mut rows: Vec<_> = tree
            .all_nodes()
            .values()
            .map(|n| {
                (
                    n.id,
                    n.parent,
                    n.text.clone(),
                    n.level,
                    n.importance,
                    n.embedding.clone(),
                )
            })
            .collect();
        rows.sort_by_key(|row| row.0);
        rows
    }

    /// One new memory must not rewrite every node in the tree.
    ///
    /// `save_all_nodes_to_db` collects `tree.all_nodes()` and upserts each one
    /// on every insert, so the cost of storing one memory is proportional to
    /// everything stored before it. #250 measured the consequence on the
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
    use tempfile::NamedTempFile;

    /// Content long enough to survive the quality classifier's noise filter.
    fn substantive(tag: &str) -> String {
        format!(
            "The deploy key for the {tag} environment lives in the Employee \
             vault under the Finch signing item, not in the repository."
        )
    }

    #[tokio::test]
    async fn test_storing_identical_content_twice_succeeds() -> Result<()> {
        // Deduplication resolves repeated content to an existing node, which
        // the original `INSERT` into a UNIQUE `node_id` rejected outright.
        //
        // What this test now pins is the deduplication itself: on the base
        // revision three inserts of one text mint three nodes, so `matching`
        // is 3. The schema change is pinned separately by
        // `test_repeated_content_records_every_source_conversation` — with the
        // `INSERT OR REPLACE` used today, restoring the UNIQUE constraint
        // would not raise an error at all, it would silently delete the
        // earlier source row.
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
        let matching = results.iter().filter(|r| r.text == text).count();
        assert_eq!(
            matching, 1,
            "the repeated fact is one memory, not three; got {results:?}"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_repeated_content_records_every_source_conversation() -> Result<()> {
        // Many conversations map to one node, so all three sources are durable.
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

        Ok(())
    }

    #[tokio::test]
    async fn test_promotion_moves_provenance_to_the_leaf_holding_the_text() -> Result<()> {
        // When a matched leaf becomes an aggregate, the conversation attributed
        // to it was attributed to those words, so its row must follow them to
        // the moved child. Otherwise it points at an embedding that is the mean
        // of two different memories, and the leaf holding the text has no
        // source at all.
        //
        // The fixture pair must sit BELOW `NEAR_IDENTICAL_SIMILARITY`, or the
        // variant rule fires, promotion never happens, and this test silently
        // covers nothing. A previous version used "alpha" and "alpha two",
        // which measured 0.99275 and did exactly that. Swapping a single word
        // measures 0.9499 — below the cutoff, but by 0.040, not the 0.12
        // an earlier version of this comment implied.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        memory
            .insert_conversation("system", &substantive("production"), None, None)
            .await?;
        memory
            .insert_conversation("system", &substantive("staging"), None, None)
            .await?;

        // Guard: if no promotion occurred there is nothing to follow, and the
        // assertions below would pass vacuously.
        let (_, depth, _) = memory.index_shape().await;
        assert!(
            depth > 1,
            "the fixture must actually promote, or this test covers nothing"
        );

        let conn = memory.db.lock().await;

        // Every source row points at a leaf, never at an aggregate.
        let orphaned: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_sources s
             WHERE EXISTS (SELECT 1 FROM tree_nodes c WHERE c.parent_id = s.node_id)",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            orphaned, 0,
            "no conversation may be attributed to an internal aggregate node"
        );

        // And the row points at the leaf that actually holds its words.
        let mismatched: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_sources s
             JOIN conversations c ON c.id = s.conversation_id
             JOIN tree_nodes n ON n.node_id = s.node_id
             WHERE instr(c.content, n.text) = 0",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            mismatched, 0,
            "each source row must point at a node whose text came from that \
             conversation"
        );

        Ok(())
    }

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
        // Phase 1: every stored node counted.
        //
        // This alone does NOT mean the loader is done. `loaded` reaches
        // `total` on the last batch, and the loader then runs the child-linking
        // pass before calling `complete()`. Returning here would subscribe
        // *before* completion fired, which is precisely the sequence that hides
        // the bug — an earlier draft of this test did exactly that and passed
        // against the broken code.
        for i in 0..2_500 {
            match memory.hydration_status() {
                HydrationStatus::Ready { .. } => break,
                HydrationStatus::Loading { loaded, total } if total > 0 && loaded >= total => break,
                HydrationStatus::Loading { .. } => {}
                HydrationStatus::Failed { reason } => panic!("hydration failed: {reason}"),
                HydrationStatus::Degraded { reason, .. } => {
                    panic!("hydration stopped early: {reason}")
                }
            }
            assert!(i < 2_499, "hydration never counted every node");
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }

        // Phase 2: the child-linking pass has run. `complete()` is the next
        // statement after it with no await in between, so observing linked
        // children means the loader has either already completed or is a few
        // instructions away. The trailing sleep covers the second case on a
        // multi-threaded runtime.
        for i in 0..2_500 {
            let linked = {
                let tree = memory.tree.lock().await;
                tree.all_nodes()
                    .values()
                    .any(|node| !node.children.is_empty())
            };
            if linked {
                break;
            }
            assert!(i < 2_499, "the loader never linked children");
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    fn seed_tree_nodes(path: &std::path::Path, count: u64, overrides: &[(u64, u64)]) -> Result<()> {
        let conn = Connection::open(path)?;
        let embedding: Vec<u8> = 0.5f32.to_le_bytes().repeat(8);
        let tx = conn.unchecked_transaction()?;
        for id in 0..count {
            let parent = if id == 0 { None } else { Some(0i64) };
            tx.execute(
                "INSERT OR REPLACE INTO tree_nodes
                 (node_id, parent_id, text, embedding, level, created_at, importance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                params![
                    id as i64,
                    parent,
                    format!("seeded node {id}"),
                    embedding,
                    if id == 0 { 0i64 } else { 1i64 },
                    id as i64,
                ],
            )?;
        }
        for (child, parent) in overrides {
            tx.execute(
                "UPDATE tree_nodes SET parent_id = ?1 WHERE node_id = ?2",
                params![*parent as i64, *child as i64],
            )?;
        }
        tx.commit()?;
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
        // Asserted by comparison rather than against a fixed threshold, and
        // rather than against an immediate `Loading` status. The old assertion
        // relied on a current-thread runtime being unable to poll the loader
        // until the test awaited; under the flavor production actually runs,
        // the loader may well have started, so that assertion was pinned to a
        // configuration nothing ships. Timing the same store loaded both ways
        // measures the property directly and cannot be satisfied by a blocking
        // load.
        const NODES: u64 = 4 * HYDRATION_BATCH as u64;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_tree_nodes(temp.path(), NODES, &[])?;

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
        let started = std::time::Instant::now();
        let reopened = MemorySystem::new(config)?;
        let backgrounded = started.elapsed();

        assert!(
            backgrounded * 4 < blocking,
            "construction must return before the index is loaded: backgrounded \
             {backgrounded:?} against a blocking load of {blocking:?} for the \
             same {NODES}-node store"
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

        // The rebuilt structure must match what a blocking load produces:
        // children linked, not a flat list.
        let linked = {
            let tree = reopened.tree.lock().await;
            tree.all_nodes()
                .values()
                .filter(|node| !node.children.is_empty())
                .count()
        };
        assert!(
            linked > 0,
            "the final pass must link children; a flat tree means the parent \
             links were dropped"
        );

        Ok(())
    }

    // Multi-thread: the only flavor that spawns the background loader.
    // It exists for the paging loop, which a current-thread runtime never
    // enters.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_hydration_pages_across_batches_and_links_late_parents() -> Result<()> {
        // Every other hydration test seeds 30-40 conversations, so the paging
        // loop this PR introduces runs its body exactly once and stops. The
        // cursor advance, the per-batch lock release, and the case a parent
        // arrives in a later batch than its child were all unexercised, on a
        // change whose entire point is a 16,782-node store (#242).
        // Four non-empty pages plus the empty one that ends the loop. Written
        // in terms of HYDRATION_BATCH so it stays above the batch size if the
        // constant changes; asserting that would be a tautology.
        const NODES: u64 = 3 * HYDRATION_BATCH as u64 + 7;
        // Node 5 is in the first batch; its parent is in the third. Linking
        // therefore cannot be done as the batches land.
        let late_parent = 2 * HYDRATION_BATCH as u64 + 3;

        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?); // create the schema
        seed_tree_nodes(temp.path(), NODES, &[(5, late_parent)])?;

        let memory = MemorySystem::new(config)?;
        memory.ensure_hydrated().await?;

        match memory.hydration_status() {
            HydrationStatus::Ready { nodes } => assert_eq!(
                nodes as u64, NODES,
                "every node across every batch must load"
            ),
            other => panic!("hydration did not complete: {other:?}"),
        }

        let tree = memory.tree.lock().await;
        assert_eq!(
            tree.all_nodes().len() as u64,
            NODES,
            "a skipped or duplicated page loses nodes"
        );
        for id in 0..NODES {
            assert!(
                tree.all_nodes().contains_key(&id),
                "node {id} was skipped by the batch loader"
            );
        }
        assert_eq!(
            tree.all_nodes()
                .get(&late_parent)
                .expect("late parent must load")
                .children,
            vec![5],
            "a child in an earlier batch must be linked to its later parent"
        );
        Ok(())
    }

    #[test]
    fn test_an_unreadable_edge_list_degrades_instead_of_failing_hydration() {
        // The decision itself, separately from its consequence. `fail()` opens
        // the write gate, and this runs before the first batch, so failing here
        // released writes against a tree holding nothing but a fresh root.
        // `edges_or_degraded` takes no `HydrationState`, so it cannot fail the
        // hydration however it is edited — the type is the guarantee, and this
        // pins the empty-map result that goes with it.
        let degraded = MemorySystem::edges_or_degraded(Err(anyhow::anyhow!("database is locked")));
        assert!(
            degraded.is_empty(),
            "an unreadable edge list must degrade to no links, not propagate"
        );

        let mut edges = HashMap::new();
        edges.insert(0u64, vec![1u64, 2]);
        assert_eq!(
            MemorySystem::edges_or_degraded(Ok(edges.clone())),
            edges,
            "a readable edge list must pass through untouched"
        );
    }

    #[tokio::test]
    async fn test_hydration_completes_correctly_when_the_edge_list_is_unavailable() -> Result<()> {
        // The edge query only buys read purity during the window. Failing it
        // used to call `fail()`, which opens the write gate against a tree
        // holding nothing but a fresh root — so the next write would attach
        // under that root and `save_all_nodes_to_db` would persist it, which is
        // the durable misplacement the gate exists to prevent. Degrading must
        // still reach a complete, correctly linked tree, and must not open the
        // gate early.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        // 552 nodes: two batches, with node 5 (first batch) reparented under
        // node 520 (second batch), so the final linking pass has real work.
        const NODES: u64 = HYDRATION_BATCH as u64 + 40;
        const LATE_PARENT: u64 = HYDRATION_BATCH as u64 + 8;
        seed_tree_nodes(temp.path(), NODES, &[(5, LATE_PARENT)])?;

        // Construct without a runtime so nothing is spawned, then drive the
        // batch loop by hand with the map the fallback would produce.
        let memory = std::thread::spawn(move || MemorySystem::new(config))
            .join()
            .expect("constructor thread")?;
        let expected = memory.hydration_status();
        assert!(
            matches!(expected, HydrationStatus::Ready { .. }),
            "the no-runtime arm loads synchronously; got {expected:?}"
        );

        // Reset to the state the background arm starts from — except
        // `next_id`, which `new_with_dim` returns to 1. Nothing in
        // `hydrate_batches` or `link_loaded_children` reads it, and this test
        // asserts only node count, status, and linkage; a future edit that
        // consulted `next_id` would need the real value here.
        //
        // Then run the loop with no edges at all.
        {
            let mut tree = memory.tree.lock().await;
            *tree = MemTree::new_with_dim(memory.embedding_engine.dimension());
        }
        let state = Arc::new(HydrationState::new(NODES as usize));
        MemorySystem::hydrate_batches(
            Arc::clone(&memory.db),
            Arc::clone(&memory.tree),
            Arc::clone(&state),
            HashMap::new(),
        )
        .await;

        assert!(
            matches!(state.status(), HydrationStatus::Ready { .. }),
            "a degraded run must still complete; got {:?}",
            state.status()
        );
        let tree = memory.tree.lock().await;
        assert_eq!(
            tree.all_nodes().len(),
            NODES as usize,
            "every node must load without the edge list"
        );
        assert_eq!(
            tree.all_nodes()
                .get(&LATE_PARENT)
                .expect("the late parent must load")
                .children,
            vec![5],
            "the final pass must still link a child to a parent in a later \
             batch; degrading loses read purity during the window, not the \
             structure it ends with"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_child_edges_are_read_in_one_query_before_any_node_loads() -> Result<()> {
        // The seeding this depends on is what keeps an internal node from
        // looking like a leaf mid-hydration, so the query itself is worth
        // pinning: it must return every parent's full child list, including a
        // parent whose id is higher than its child's.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config)?);
        seed_tree_nodes(temp.path(), 6, &[(2, 5)])?;

        let conn = Connection::open(temp.path())?;
        let edges = MemorySystem::load_child_edges(&conn)?;

        let mut root_children = edges.get(&0).cloned().unwrap_or_default();
        root_children.sort_unstable();
        assert_eq!(
            root_children,
            vec![1, 3, 4, 5],
            "every child of the root must be listed"
        );
        assert_eq!(
            edges.get(&5).cloned().unwrap_or_default(),
            vec![2],
            "a parent whose id is higher than its child's must still be linked"
        );
        assert!(
            !edges.contains_key(&1),
            "a leaf must have no entry, so its `children` stays empty and \
             retrieval still treats it as a leaf"
        );

        // An empty store is not an error — that is the fallback the hydration
        // failure path relies on.
        let empty = NamedTempFile::new()?;
        drop(MemorySystem::new(MemoryConfig {
            db_path: empty.path().to_path_buf(),
            ..Default::default()
        })?);
        let conn = Connection::open(empty.path())?;
        assert!(MemorySystem::load_child_edges(&conn)?.is_empty());
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

    // Multi-thread, because that is the only flavor that spawns the
    // background loader now — a current-thread runtime loads
    // synchronously, so there is no task for this to observe. It is
    // also the flavor production runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_an_aborted_loader_releases_a_waiting_write() -> Result<()> {
        // Aborts the handle `MemorySystem::new` actually spawned, not a task
        // the test built for itself. An earlier version did the latter, and
        // removing the guard from the production spawn site then failed
        // nothing — the test proved the guard type worked while leaving the
        // one place it has to be installed uncovered.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        // Enough rows that the loader is still working when it is aborted.
        seed_tree_nodes(temp.path(), 4 * HYDRATION_BATCH as u64, &[])?;

        let memory = MemorySystem::new(config)?;
        memory
            .hydration_task
            .as_ref()
            .expect("a store with rows must spawn a loader")
            .abort();
        // Let the runtime process the cancellation and drop the future.
        for _ in 0..64 {
            tokio::task::yield_now().await;
            if !matches!(memory.hydration_status(), HydrationStatus::Loading { .. }) {
                break;
            }
        }

        let error = memory
            .ensure_hydrated()
            .await
            .expect_err("an aborted loader must fail the write, not hang it");
        assert!(
            error.to_string().contains("ended without finishing"),
            "the write must be refused for the reason the guard recorded, not \
             by the timeout; got {error}"
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
        seed_tree_nodes(temp.path(), 8, &[])?;

        // Break every row's `text`, so the whole load fails rather than one
        // batch of it.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE tree_nodes SET text = ?1",
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

    /// A first-batch failure loads nothing, so it is broken rather than
    /// degraded — and writes must stay refused.
    ///
    /// `Degraded` is justified by `link_loaded_children` having made what
    /// loaded coherent. That argument is vacuous when nothing loaded: linking
    /// an empty node set leaves only the placeholder root, and calling it
    /// degraded opened the gate on a tree with no real structure, so every
    /// memory attached directly under a childless root and was persisted that
    /// way. Removing the `loaded == 0` check makes this fail with the write
    /// accepted and a two-node tree — root plus the memory just written —
    /// against a store of 1024.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_a_first_batch_failure_is_broken_not_degraded() -> Result<()> {
        const NODES: u64 = 2 * HYDRATION_BATCH as u64;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_tree_nodes(temp.path(), NODES, &[])?;

        // Break a row in the FIRST batch, so the loader stops before anything
        // lands.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE tree_nodes SET text = ?1 WHERE node_id = ?2",
                params![vec![0xffu8, 0xfe], 4i64],
            )?;
        }

        let memory = MemorySystem::new(config)?;
        let refused = memory
            .insert_conversation(
                "system",
                "Nothing loaded, so there is no structure to place this in.",
                None,
                None,
            )
            .await
            .expect_err("an index that loaded nothing must refuse writes");
        assert!(
            refused.to_string().contains("cannot be placed"),
            "got {refused}"
        );
        assert!(
            matches!(memory.hydration_status(), HydrationStatus::Failed { .. }),
            "nothing loaded is not a smaller index, it is no index; got {:?}",
            memory.hydration_status()
        );

        // And the tree really is empty of stored nodes, so the refusal is not
        // incidental.
        let tree = memory.tree.lock().await;
        assert_eq!(
            tree.all_nodes().len(),
            1,
            "only the placeholder root should be present"
        );
        Ok(())
    }

    /// The two failure kinds must lead to different answers.
    ///
    /// Before #288 both were `Failed`, so a batch that could not be read
    /// refused every memory write for the rest of the process — the same
    /// verdict as a loader that died and may have left the tree unlinked. One
    /// unreadable row out of a 16,782-node store is not a reason to stop
    /// recording memory.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_a_degraded_index_accepts_writes_and_a_broken_one_does_not() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        // Reached through the loader, not by calling `degrade` directly.
        //
        // Calling it directly is what the first version of this test did, and
        // it made the test blind to the production call site: collapsing
        // `degrade` into `fail` at the batch-error arm left it passing. A test
        // that cannot observe the decision it names is not a regression for it.
        drop(MemorySystem::new(config.clone())?);
        seed_tree_nodes(temp.path(), 2 * HYDRATION_BATCH as u64, &[])?;
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE tree_nodes SET text = ?1 WHERE node_id = ?2",
                params![vec![0xffu8, 0xfe], HYDRATION_BATCH as i64 + 4],
            )?;
        }
        let degraded = MemorySystem::new(config.clone())?;
        degraded.ensure_hydrated().await?;
        assert!(
            matches!(
                degraded.hydration_status(),
                HydrationStatus::Degraded { .. }
            ),
            "a batch error after a batch loaded must degrade; got {:?}",
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
    /// reconstruct. It also leaves no `memory_sources` row, which is the state
    /// a later re-projection repairs.
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
            "and it must be left unindexed, so a re-projection can repair it"
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
    /// in `src/runtime`, which drives the real submit API.
    #[tokio::test]
    async fn test_a_current_thread_runtime_loads_without_spawning_a_loader() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        const NODES: u64 = 2 * HYDRATION_BATCH as u64;
        const LATE_PARENT: u64 = HYDRATION_BATCH as u64 + 8;
        seed_tree_nodes(temp.path(), NODES, &[(5, LATE_PARENT)])?;

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

        // And the structure is the one the batched loader would have built.
        let tree = memory.tree.lock().await;
        assert_eq!(
            tree.all_nodes()
                .get(&LATE_PARENT)
                .expect("the late parent must load")
                .children,
            vec![5],
            "the synchronous load must link children just as the batched one does"
        );
        Ok(())
    }

    // Multi-thread, because that is the only flavor that spawns the
    // background loader now — a current-thread runtime loads
    // synchronously, so there is no task for this to observe. It is
    // also the flavor production runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_a_read_during_hydration_returns_no_internal_aggregate_nodes() -> Result<()> {
        // Reads are deliberately not gated on hydration — serving what has
        // loaded beats blocking a turn — so the first turn of a session queries
        // a partially hydrated tree. `MemTree::retrieve` identifies leaves as
        // `children.is_empty()`, and linking children only in a final pass made
        // every internal node answer that predicate for the whole window: one
        // memory occupying several result slots, and mean-of-subtree embeddings
        // surfaced as if they were memories, on exactly the turn a user is most
        // likely to be typing during. Loading the edge list before the first
        // batch is what fixes it.
        const NODES: u64 = 4 * HYDRATION_BATCH as u64;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        // Node 2's parent becomes node 1, so node 1 is a genuine internal node
        // and both are in the first batch.
        seed_tree_nodes(temp.path(), NODES, &[(2, 1)])?;

        let (_pause_registration, mut batch_reached, release_batch) =
            register_hydration_batch_pause(temp.path().to_path_buf(), HYDRATION_BATCH);
        let memory = MemorySystem::new(config)?;

        // The production loader itself announces that it has committed the
        // first batch, then waits for this test to release it. Polling tree
        // size cannot establish this window: on a fast runner the loader can
        // commit all four batches before the polling task is scheduled once.
        if !*batch_reached.borrow_and_update() {
            let reached = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    batch_reached.changed().await.expect(
                        "the hydration pause sender disappeared before the first batch landed",
                    );
                    if *batch_reached.borrow_and_update() {
                        break;
                    }
                }
            })
            .await;
            assert!(
                reached.is_ok(),
                "the production loader did not reach its first batch within 2s; status={:?}",
                memory.hydration_status()
            );
        }
        assert_eq!(
            memory.hydration_status(),
            HydrationStatus::Loading {
                loaded: HYDRATION_BATCH,
                total: NODES as usize,
            },
            "the pause must hold the real loader after exactly one batch"
        );

        // Held across every assertion below, so the structure and the query
        // observe the same causally paused production state.
        {
            let tree = memory.tree.lock().await;
            assert_eq!(
                tree.all_nodes().len(),
                HYDRATION_BATCH,
                "the test pause must prevent a later batch from racing the read"
            );
            assert!(
                !tree
                    .all_nodes()
                    .get(&1)
                    .expect("node 1 must be present in the paused first batch")
                    .children
                    .is_empty(),
                "an internal node must not look like a leaf before its children \
                 are linked; retrieval would return it"
            );
        }

        // The production read boundary must not return the internal aggregate
        // while later hydration batches are provably unable to run.
        let results = memory.query("seeded node", Some(NODES as usize)).await?;
        assert!(
            !results.is_empty(),
            "the paused read must return memories from the first loaded batch"
        );
        assert!(
            !results.iter().any(|text| text == "seeded node 1"),
            "node 1 is an internal aggregate: its text is a label duplicated \
             from a child and its embedding is a mean, so it must never be a \
             query result"
        );

        release_batch.send_replace(true);
        match tokio::time::timeout(std::time::Duration::from_secs(5), memory.ensure_hydrated())
            .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => panic!("hydration failed after the test released it: {error:#}"),
            Err(_) => {
                panic!(
                    "hydration did not complete within 5s after release; status={:?}",
                    memory.hydration_status()
                );
            }
        }
        assert_eq!(
            memory.hydration_status(),
            HydrationStatus::Ready {
                nodes: NODES as usize,
            },
            "the released loader must finish and publish its terminal state"
        );
        Ok(())
    }

    // Multi-thread, because that is the only flavor that spawns the
    // background loader now — a current-thread runtime loads
    // synchronously, so there is no task for this to observe. It is
    // also the flavor production runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_failed_hydration_links_what_loaded_before_releasing_writes() -> Result<()> {
        // `fail()` completes, and completing opens the write gate. It used to
        // return before the child-linking pass, so every loaded node kept an
        // empty `children`: `MemTree::insert` saw a childless root, hung the
        // next memory directly off it, and `save_all_nodes_to_db` persisted
        // that flattening. A partial index is recoverable; a durably flattened
        // one is not.
        const NODES: u64 = 2 * HYDRATION_BATCH as u64;
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);
        seed_tree_nodes(temp.path(), NODES, &[])?;

        // Break one row in the SECOND batch: `text` becomes a BLOB, so
        // `row.get::<_, String>` fails. The first batch still loads.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute(
                "UPDATE tree_nodes SET text = ?1 WHERE node_id = ?2",
                params![vec![0xffu8, 0xfe], HYDRATION_BATCH as i64 + 4],
            )?;
        }

        let memory = MemorySystem::new(config)?;
        // Returns `Ok`, and that is the point of #288.
        //
        // #276 made a failed hydration refuse writes, which was right for a
        // tree that might be unusable — but it collapsed this case into the
        // same verdict, so one unreadable row refused every write for the rest
        // of the process. Linking below is exactly what makes this case
        // different: the index is smaller than the store, not incoherent.
        //
        // What matters for the ordering pinned below is that it returns at all,
        // which it can only do after the loader recorded and completed.
        memory
            .ensure_hydrated()
            .await
            .expect("a degraded index must still accept writes");

        match memory.hydration_status() {
            HydrationStatus::Degraded { loaded, total, .. } => {
                assert!(
                    loaded < total,
                    "a degraded index must report how much of the store it \
                     actually holds; got {loaded} of {total}"
                );
            }
            other => panic!("a broken row must surface as Degraded; got {other:?}"),
        }

        // And a write genuinely lands, rather than merely being permitted.
        memory
            .insert_conversation(
                "system",
                "A memory written into an index that stopped loading early.",
                None,
                None,
            )
            .await?;

        // The assertion below pins linking; it pins the *ordering* only because
        // `ensure_hydrated` returned above, and it can only return after the
        // loader recorded. Recording ahead of `link_loaded_children` would make
        // this a race with the hydration task for the tree lock rather than a
        // clean result, so the ordering is stated here as well as relied on.

        let tree = memory.tree.lock().await;
        let loaded = tree.all_nodes().len();
        assert!(
            loaded >= HYDRATION_BATCH,
            "the batches that did load must be kept; got {loaded}"
        );
        assert_eq!(
            tree.all_nodes()
                .get(&0)
                .expect("root must load")
                .children
                .len(),
            loaded - 1,
            "every loaded child must be linked to the root before writes are \
             released; an empty `children` makes the next memory flatten the \
             tree and persists it"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_batch_loading_cannot_skip_a_row_when_the_store_changes() -> Result<()> {
        // Keyset, not `LIMIT`/`OFFSET`: the database lock is released between
        // batches, so the row set can change underneath an offset. Deleting a
        // row below the window shifts every later row back by one, and an
        // offset loader skips the boundary row — permanently, for the rest of
        // the session. Reverting `load_batch` to `LIMIT ?2 OFFSET ?1` (keeping
        // this signature, with the offset derived from the cursor) makes this
        // fail: page two starts at 5 instead of 4.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config)?);
        seed_tree_nodes(temp.path(), 10, &[])?;

        let conn = Connection::open(temp.path())?;
        let first: Vec<u64> = MemorySystem::load_batch(&conn, None, 4)?
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(
            first,
            vec![0, 1, 2, 3],
            "the first page must start at the root"
        );

        // A row the loader has already read disappears between batches.
        conn.execute("DELETE FROM tree_nodes WHERE node_id = 1", [])?;

        let cursor = first.last().copied();
        let second: Vec<u64> = MemorySystem::load_batch(&conn, cursor, 4)?
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(
            second,
            vec![4, 5, 6, 7],
            "no stored node may be skipped when the row set shifts between \
             batches; an offset loader loses the boundary row"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_an_unreadable_node_census_refuses_to_open_the_store() -> Result<()> {
        // `unwrap_or(0)` on the two census queries turned a database error into
        // silent lies: a failed `COUNT` skipped hydration entirely and reported
        // `Ready { nodes: 0 }` against a full store with no log line, and a
        // failed `MAX` left `next_id` at 1 so the first write upserted over
        // persisted node 1. This drives the `MAX` half, which is the one that
        // destroys data; the `COUNT` above it is the same expression shape and
        // the assertion accepts either message. Restoring `unwrap_or(0)` on the
        // `MAX` query makes this fail.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        drop(MemorySystem::new(config.clone())?);

        // Same table name and columns, so every migration and
        // `CREATE ... IF NOT EXISTS` in `new` is a no-op — but `node_id` holds
        // text, so `MAX(node_id)` cannot be read as an `i64`.
        {
            let conn = Connection::open(temp.path())?;
            conn.execute_batch(
                "DROP TABLE memory_sources;
                 DROP TABLE tree_nodes;
                 CREATE TABLE tree_nodes (
                     node_id TEXT PRIMARY KEY,
                     parent_id INTEGER,
                     text TEXT NOT NULL,
                     embedding BLOB NOT NULL,
                     level INTEGER NOT NULL,
                     created_at INTEGER NOT NULL,
                     importance INTEGER NOT NULL DEFAULT 1
                 );
                 INSERT INTO tree_nodes
                 VALUES ('not-an-integer', NULL, 'x', x'00000000', 0, 0, 1);",
            )?;
        }

        let error = MemorySystem::new(config)
            .err()
            .expect("a store whose node census cannot be read must not open");
        let chain = format!("{error:#}");
        assert!(
            chain.contains("count stored MemTree nodes")
                || chain.contains("highest stored MemTree node id"),
            "the error must say which census query failed, so the operator can \
             tell it from an ordinary empty store; got {chain}"
        );
        Ok(())
    }

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
        // still match after `set_next_id` is deleted and one node is
        // overwritten — so it could not fail. Ranking cannot carry this
        // assertion either: `TfIdfEmbedding` drops tokens under two characters,
        // so the single-digit index that distinguishes these memories is not in
        // the embedding at all and `query` orders them arbitrarily. Scan the
        // hydrated node set instead.
        let texts: Vec<String> = {
            let tree = reopened.tree.lock().await;
            tree.all_nodes()
                .values()
                .map(|node| node.text.clone())
                .collect()
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
    // Its whole point is that the batched loader reproduces the blocking one.
    // On a current-thread runtime it compared `load_tree_from_db_conn` against
    // itself.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_background_hydration_reproduces_the_blocking_load_exactly() -> Result<()> {
        // The highest-value property: whatever the background loader builds
        // must be node-for-node what the blocking loader builds. Batching by
        // `node_id` means a parent can arrive after its child, so the child
        // links are rebuilt in a final pass; this is what proves that pass
        // reconstructs the same structure rather than merely a non-empty one.
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
        let background_nodes = {
            let tree = background.tree.lock().await;
            let mut nodes: Vec<(u64, Option<u64>, usize, Vec<u64>, String)> = tree
                .all_nodes()
                .values()
                .map(|node| {
                    let mut children = node.children.clone();
                    children.sort_unstable();
                    (
                        node.id,
                        node.parent,
                        node.level,
                        children,
                        node.text.clone(),
                    )
                })
                .collect();
            nodes.sort_by_key(|entry| entry.0);
            nodes
        };

        // Blocking path, built directly from the same database.
        let blocking_nodes = {
            let conn = Connection::open(temp.path())?;
            let mut tree = MemTree::new_with_dim(background.embedding_engine.dimension());
            MemorySystem::load_tree_from_db_conn(&conn, &mut tree)?;
            let mut nodes: Vec<(u64, Option<u64>, usize, Vec<u64>, String)> = tree
                .all_nodes()
                .values()
                .map(|node| {
                    let mut children = node.children.clone();
                    children.sort_unstable();
                    (
                        node.id,
                        node.parent,
                        node.level,
                        children,
                        node.text.clone(),
                    )
                })
                .collect();
            nodes.sort_by_key(|entry| entry.0);
            nodes
        };

        assert_eq!(
            background_nodes, blocking_nodes,
            "background hydration must reproduce the blocking load exactly"
        );
        assert!(
            background_nodes.len() > 1,
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
                "CREATE TRIGGER reject_memory_node BEFORE INSERT ON tree_nodes
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

    /// Regression: old production DBs had `id AUTOINCREMENT` as the tree_nodes PK
    /// instead of `node_id INTEGER PRIMARY KEY`.  MemorySystem::new() must detect
    /// this and drop/recreate the table so inserts don't fail with
    /// "no such column: node_id".
    #[tokio::test]
    async fn test_old_schema_migration_drops_and_recreates_tree_nodes() -> Result<()> {
        use rusqlite::Connection;

        let temp = NamedTempFile::new()?;

        // Set up a DB with the OLD tree_nodes schema (id AUTOINCREMENT)
        {
            let conn = Connection::open(temp.path())?;
            conn.execute_batch(
                "CREATE TABLE tree_nodes (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    parent_id INTEGER,
                    text TEXT NOT NULL,
                    embedding BLOB NOT NULL,
                    level INTEGER NOT NULL,
                    created_at INTEGER NOT NULL
                );",
            )?;
        }

        // Open via MemorySystem — migration should run automatically
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };
        let memory = MemorySystem::new(config)?;

        // Verify new schema: inserting a conversation must succeed
        memory
            .insert_conversation(
                "user",
                "We decided to always use anyhow for error handling.",
                Some("test"),
                None,
            )
            .await?;

        let stats = memory.stats().await?;
        assert_eq!(stats.conversation_count, 1);
        assert_eq!(
            stats.tree_node_count, 1,
            "node should be in MemTree after migration"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_a_corrupt_parent_chain_on_disk_errors_instead_of_aborting() -> Result<()> {
        // #274 at the persistence boundary. `parent` links are rebuilt from
        // `tree_nodes`, so a corrupt chain reaches `update_parent_aggregation`
        // through a real store open followed by a real write. Before the fix
        // that recursed until the thread overflowed its stack, which is SIGABRT
        // — the process dies and no `Result` is ever returned, so a caller
        // cannot log it, retry, or fall back.
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        // A real store with real nodes.
        {
            let memory = MemorySystem::new(config.clone())?;
            for i in 0..5 {
                memory
                    .insert_conversation(
                        "system",
                        &format!(
                            "Deployment note {i}: the signing key lives in the \
                             Employee vault, never in the repository."
                        ),
                        None,
                        None,
                    )
                    .await?;
            }
        }

        // Corrupt it: the root's parent points at one of its own descendants.
        // The foreign key holds because the target row already exists.
        {
            let conn = Connection::open(temp.path())?;
            let descendant: i64 = conn.query_row(
                "SELECT node_id FROM tree_nodes WHERE node_id != 0 ORDER BY node_id ASC LIMIT 1",
                [],
                |row| row.get(0),
            )?;
            conn.execute(
                "UPDATE tree_nodes SET parent_id = ?1 WHERE node_id = 0",
                params![descendant],
            )?;
        }

        let memory = MemorySystem::new(config)?;
        let result = memory
            .insert_conversation(
                "system",
                "A memory written against a store whose parent chain is corrupt.",
                None,
                None,
            )
            .await;

        let error = result.expect_err("a corrupt parent chain must surface as an error");
        assert!(
            error.to_string().contains("cycles:"),
            "the error must name the cycle so the corruption is diagnosable; \
             got {error}"
        );

        // The same write, again. `attach_child` and `promote_leaf` insert the
        // new node and push it into its parent's `children` BEFORE aggregating,
        // so an aggregation error leaves the tree mutated. Without restoring it
        // the retry hits `find_leaf_by_text`, dedups to the node the failed
        // insert left behind, returns before aggregation — and SUCCEEDS,
        // persisting a write that had just hard-failed. An error a caller
        // cannot retry deterministically is worse than no error.
        let retry = memory
            .insert_conversation(
                "system",
                "A memory written against a store whose parent chain is corrupt.",
                None,
                None,
            )
            .await;
        let retry_error =
            retry.expect_err("an identical retry of a write that hard-failed must fail too");
        assert!(
            retry_error.to_string().contains("cycles:"),
            "the retry must fail the SAME way, not on a foreign-key violation or \
             a masked reload error; got {retry_error}"
        );
        Ok(())
    }
}
