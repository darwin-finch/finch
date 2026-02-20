// Memory system for Finch
//
// Hierarchical semantic memory using MemTree
// - Client-side storage (CLI, not daemon)
// - SQLite with WAL mode for concurrency
// - O(log N) insertion for real-time updates
// - Cross-session context recall

mod embeddings;
mod memtree;

pub use embeddings::{EmbeddingEngine, TfIdfEmbedding, cosine_similarity};
pub use memtree::{MemTree, TreeNode, NodeId};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
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
}

impl Default for MemoryConfig {
    fn default() -> Self {
        let db_path = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".finch")
            .join("memory.db");

        Self {
            db_path,
            enabled: true,
            max_context_items: 5,
            checkpoint_interval_secs: 300,  // 5 minutes
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
    /// Create new memory system
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

        // Load schema
        let schema = include_str!("schema.sql");
        conn.execute_batch(schema)?;

        tracing::info!("Memory system initialized: {}", config.db_path.display());

        // Create MemTree and rehydrate from existing conversations in SQLite.
        // This makes search_memory work across sessions: every stored conversation
        // is re-embedded and re-inserted into the in-memory tree at startup.
        let embedding_engine_init = TfIdfEmbedding::new();
        let mut tree = MemTree::new();

        {
            let mut stmt = conn.prepare(
                "SELECT content FROM conversations ORDER BY created_at ASC",
            )?;
            let rows: Vec<String> = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;

            let count = rows.len();
            for content in rows {
                if let Ok(embedding) = embedding_engine_init.embed(&content) {
                    let _ = tree.insert(content, embedding);
                }
            }
            if count > 0 {
                tracing::info!("Rehydrated MemTree with {} memories from database", count);
            }
        }

        Ok(Self {
            db: Arc::new(Mutex::new(conn)),
            tree: Arc::new(Mutex::new(tree)),
            embedding_engine: Arc::new(TfIdfEmbedding::new()),
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
        let timestamp = chrono::Utc::now().timestamp_nanos_opt()
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
                    None::<i32>,  // tokens (TODO: count)
                    model,
                    session_id,
                    timestamp,
                ],
            )?;
        }

        // Insert into MemTree
        let embedding = self.embedding_engine.embed(content)?;
        let mut tree = self.tree.lock().await;
        tree.insert(content.to_string(), embedding)?;

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
            .query_map([limit], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(conversations)
    }

    /// Get memory statistics
    pub async fn stats(&self) -> Result<MemoryStats> {
        let conn = self.db.lock().await;

        let conversation_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversations",
            [],
            |row| row.get(0),
        )?;

        let tree = self.tree.lock().await;
        let tree_size = tree.size();

        Ok(MemoryStats {
            conversation_count: conversation_count as usize,
            tree_node_count: tree_size,
        })
    }

    /// Checkpoint tree to database (for persistence)
    pub fn checkpoint(&self) -> Result<()> {
        // TODO: Implement tree serialization to SQLite
        // For now, tree is rebuilt on restart from conversations
        tracing::debug!("Memory checkpoint requested (not yet implemented)");
        Ok(())
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

        memory.insert_conversation(
            "user",
            "How do I use Rust lifetimes?",
            Some("local"),
            Some("test-session"),
        ).await?;

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
        memory.insert_conversation(
            "user",
            "How do I use Rust lifetimes?",
            Some("local"),
            None,
        ).await?;

        memory.insert_conversation(
            "user",
            "What is Python asyncio?",
            Some("local"),
            None,
        ).await?;

        // Query for Rust-related content
        let results = memory.query("Rust programming", Some(2)).await?;

        assert!(!results.is_empty());
        // Should return Rust-related conversation
        assert!(results.iter().any(|r| r.contains("Rust") || r.contains("lifetimes")));

        Ok(())
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
            memory.insert_conversation(
                "user",
                &format!("Message {}", i),
                Some("local"),
                None,
            ).await?;
        }

        // Get recent 3
        let recent = memory.get_recent_conversations(3).await?;

        assert_eq!(recent.len(), 3);
        // Should be in reverse chronological order
        assert!(recent[0].1.contains("Message 5"));

        Ok(())
    }
}
