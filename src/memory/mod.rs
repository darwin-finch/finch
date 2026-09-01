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
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

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

        // Load MemTree from persisted tree_nodes table.
        // Falls back gracefully to empty tree if table is empty or data is missing.
        {
            let node_count: i64 = conn
                .query_row("SELECT COUNT(*) FROM tree_nodes", [], |row| row.get(0))
                .unwrap_or(0);
            if node_count > 0 {
                if let Err(e) = Self::load_tree_from_db_conn(&conn, &mut tree) {
                    tracing::warn!("Failed to load MemTree from DB (will start fresh): {}", e);
                    tree = MemTree::new_with_dim(dim);
                } else {
                    tracing::debug!("Loaded MemTree with {} nodes from disk", tree.size());
                }
            }
        }

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            tree: Arc::new(Mutex::new(tree)),
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

        // Quality filter: classify and extract key content before indexing.
        // Low-signal content (acks, greetings) is skipped in MemTree but still
        // written to the conversations table above for raw history.
        let classifier = MemoryClassifier::new();
        if let Some((key_content, importance)) = classifier.process(role, content) {
            let embedding = self.embedding_engine.embed(&key_content)?;
            let effect = {
                let mut tree = self.tree.lock().await;
                tree.insert_with_effect(key_content, embedding, importance.as_u8())?
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
                self.reload_tree_from_db().await?;
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

    /// Persist all MemTree nodes to the tree_nodes table in a single transaction.
    ///
    /// Nodes are written sorted by node_id (root first) so that the self-referential
    /// FK constraint `parent_id → node_id` is satisfied for each INSERT.
    ///
    /// This replaces the old `save_node_to_db(leaf_id)` approach which only persisted
    /// the newly inserted leaf.  That missed two things:
    ///   1. The root node (id=0) was never written, causing FK violations because
    ///      libsqlite3-sys bundles SQLite compiled with SQLITE_DEFAULT_FOREIGN_KEYS=1.
    ///   2. Parent embeddings updated by `update_parent_aggregation` were never
    ///      persisted, so embeddings went stale across process restarts.
    async fn save_all_nodes_to_db(
        &self,
        source: Option<(NodeId, &str, i64)>,
        promotion: Option<(NodeId, NodeId)>,
    ) -> Result<()> {
        let mut nodes: Vec<TreeNode> = {
            let tree = self.tree.lock().await;
            tree.all_nodes().values().cloned().collect()
        };

        // Sort by node_id ascending so root (id=0) is written before its children.
        // SQLite enforces the self-referential FK immediately (IMMEDIATE mode),
        // so parent rows must exist before child rows within the transaction.
        nodes.sort_by_key(|n| n.id);

        let conn = self.db.lock().await;
        let tx = conn.unchecked_transaction()?;
        for node in &nodes {
            let embedding_bytes: Vec<u8> = node
                .embedding
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            // Upsert, never REPLACE. `INSERT OR REPLACE` deletes the existing
            // row before reinserting it, and `memory_sources.node_id` is
            // declared `ON DELETE CASCADE` — so a plain REPLACE here silently
            // cascades away the provenance of every node it rewrites. Since
            // this function rewrites the whole tree on every insert, that
            // destroyed every source row except the one written later in the
            // same transaction.
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
        *self.tree.lock().await = restored;
        Ok(())
    }

    /// Reconstruct MemTree from the tree_nodes table at startup.
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
}
