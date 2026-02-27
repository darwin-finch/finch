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
    embedding_engine: Arc<dyn EmbeddingEngine>,
    config: MemoryConfig,
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

        // Load schema (CREATE TABLE IF NOT EXISTS — safe to re-run)
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema)?;

        // Migration: add importance column if the DB predates v0.7.15.
        // Silently ignored if the column already exists.
        let _ = conn.execute(
            "ALTER TABLE tree_nodes ADD COLUMN importance INTEGER NOT NULL DEFAULT 1",
            [],
        );

        tracing::info!("Memory system initialized: {}", config.db_path.display());

        // Select embedding engine: try neural if enabled and cached, else TF-IDF.
        let embedding_engine: Arc<dyn EmbeddingEngine> = if config.use_neural_embeddings {
            match NeuralEmbeddingEngine::find_in_cache()
                .and_then(|dir| NeuralEmbeddingEngine::load(&dir).ok())
            {
                Some(neural) => {
                    tracing::info!("Using neural ONNX embeddings (all-MiniLM-L6-v2)");
                    Arc::new(neural)
                }
                None => {
                    tracing::warn!(
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
            let node_count: i64 =
                conn.query_row("SELECT COUNT(*) FROM tree_nodes", [], |row| row.get(0))
                    .unwrap_or(0);
            if node_count > 0 {
                if let Err(e) = Self::load_tree_from_db_conn(&conn, &mut tree) {
                    tracing::warn!("Failed to load MemTree from DB (will start fresh): {}", e);
                    tree = MemTree::new_with_dim(dim);
                } else {
                    tracing::info!("Loaded MemTree with {} nodes from disk", tree.size());
                }
            }
        }

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            tree: Arc::new(Mutex::new(tree)),
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
        let timestamp = chrono::Utc::now()
            .timestamp_nanos_opt()
            .ok_or_else(|| anyhow::anyhow!("Timestamp out of range"))?;
        let id = uuid::Uuid::new_v4().to_string();

        // Store in SQLite
        {
            let conn = self.db.lock().await;
            conn.execute(
                "INSERT INTO conversations (id, timestamp, role, content, tokens, model, session_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &id,
                    timestamp,
                    role,
                    content,
                    None::<i32>, // tokens (TODO: count)
                    model,
                    session_id,
                    timestamp,
                ],
            )?;
        }

        // Quality filter: classify and extract key content before indexing.
        // Low-signal content (acks, greetings) is skipped in MemTree but still
        // written to the conversations table above for raw history.
        let classifier = MemoryClassifier::new();
        if let Some((key_content, importance)) = classifier.process(role, content) {
            let embedding = self.embedding_engine.embed(&key_content)?;
            {
                let mut tree = self.tree.lock().await;
                tree.insert(key_content, embedding, importance.as_u8())?;
            }
            // Persist all nodes (root + ancestors + new leaf) so the DB stays
            // consistent across process restarts and FK constraints are satisfied.
            self.save_all_nodes_to_db().await?;
        }

        tracing::debug!("Inserted conversation into memory: {} chars", content.len());

        Ok(())
    }

    /// Query memory for relevant context
    pub async fn query(&self, query_text: &str, top_k: Option<usize>) -> Result<Vec<String>> {
        let k = top_k.unwrap_or(self.config.max_context_items);

        // Generate query embedding
        let query_embedding = self.embedding_engine.embed(query_text)?;

        // Retrieve from MemTree
        let tree = self.tree.lock().await;
        let results = tree.retrieve(&query_embedding, k);

        // Extract texts
        let texts: Vec<String> = results.into_iter().map(|(_, text, _)| text).collect();

        tracing::debug!("Memory query returned {} results", texts.len());

        Ok(texts)
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
    async fn save_all_nodes_to_db(&self) -> Result<()> {
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
            tx.execute(
                "INSERT OR REPLACE INTO tree_nodes
                 (node_id, parent_id, text, embedding, level, created_at, importance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
        tx.commit()?;
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
                Ok((node_id, parent_id, text, embedding_bytes, level, created_at, importance))
            })?
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .map(|(node_id, parent_id, text, embedding_bytes, level, created_at, importance)| Row {
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
            })
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
}
