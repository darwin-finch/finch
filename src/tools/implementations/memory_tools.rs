// Memory tools for LLM to explicitly manage memories
//
// Provides:
// - search_memory: Query past conversations by semantic similarity
// - inspect_memory: Resolve a search result to its complete canonical source
// - create_memory: Store important facts/notes explicitly
// - list_recent: Show recent conversation history

use crate::memory::MemorySystem;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Search memory for relevant past conversations
pub struct SearchMemoryTool {
    memory_system: Arc<MemorySystem>,
}

impl SearchMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self {
        Self { memory_system }
    }
}

#[async_trait]
impl Tool for SearchMemoryTool {
    fn name(&self) -> &str {
        "search_memory"
    }

    fn description(&self) -> &str {
        "Search your memory for relevant past conversations and context. Only call this when the user \
         explicitly asks you to recall something from a previous session, or when a task genuinely requires \
         information that is unlikely to be in the current conversation. Do NOT call this proactively at the \
         start of every turn or as a routine step before coding tasks."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "query": {
                    "type": "string",
                    "description": "What to search for (e.g., 'rust lifetimes discussion', 'bug fix we did yesterday', 'user's coding preferences')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default: 3, max: 10)",
                    "default": 3
                }
            }),
            required: vec!["query".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: query"))?;

        let limit = params["limit"].as_u64().unwrap_or(3).min(10) as usize;

        tracing::info!("Searching memory: query='{}', limit={}", query, limit);

        // Sample the index before the search as well as after.
        //
        // Hydration advances while the query runs, so an after-only sample can
        // read `Ready` for a search that covered a fraction of the store -- and
        // then the empty branch below prints plain absence, which is the exact
        // false claim this block exists to remove. Sampling both ways fails
        // closed (#275).
        let before = self.memory_system.hydration_status();
        let results = self
            .memory_system
            .query_with_sources(query, Some(limit))
            .await?;
        let index = crate::memory_status::observed(before, self.memory_system.hydration_status());
        let caveat = crate::memory_status::caveat(&index, !results.is_empty());

        if results.is_empty() {
            return Ok(match caveat {
                None => "No relevant memories found for this query.".to_string(),
                // An unusable index searched nothing, so there is no "among the
                // entries read" to speak of -- pairing the two produced the
                // self-contradicting "No matches among the memories searched.
                // The memory index is unavailable, so nothing could be read."
                Some(caveat) if crate::memory_status::read_nothing(&index) => caveat,
                // Deliberately not "no memories found": that would assert
                // absence on the strength of an index that was not read.
                Some(caveat) => format!("No matches among the entries that were read. {caveat}"),
            });
        }

        let formatted = format!(
            "Found {} relevant memor{}:\n\n{}",
            results.len(),
            if results.len() == 1 { "y" } else { "ies" },
            results
                .iter()
                .enumerate()
                .map(|(i, result)| {
                    // Truncate very long memories
                    let preview = if result.text.chars().count() > 500 {
                        format!("{}...", result.text.chars().take(500).collect::<String>())
                    } else {
                        result.text.clone()
                    };
                    let provenance = result.source.as_ref().map_or_else(
                        || "legacy source unavailable".to_string(),
                        |source| {
                            let mut fields = vec![format!("role={}", source.role)];
                            if let Some(brain_id) = &source.brain_id {
                                fields.push(format!("brain={brain_id}"));
                            }
                            if let Some(run_id) = &source.run_id {
                                fields.push(format!("run={run_id}"));
                            }
                            if let Some(request_seq) = source.request_seq {
                                fields.push(format!("request={request_seq}"));
                            }
                            fields.join(", ")
                        },
                    );
                    format!(
                        "{}. memory_id={} ({})\n{}",
                        i + 1,
                        result.memory_id,
                        provenance,
                        preview
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        );

        // The caveat rides with the hits too: "found 3" from a partial index is
        // a different claim from "found 3" out of everything stored.
        Ok(match caveat {
            None => formatted,
            Some(caveat) => format!("{formatted}\n\n{caveat}"),
        })
    }
}

/// Inspect the complete canonical turn behind a semantic search result.
pub struct InspectMemoryTool {
    memory_system: Arc<MemorySystem>,
}

impl InspectMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self {
        Self { memory_system }
    }
}

#[async_trait]
impl Tool for InspectMemoryTool {
    fn name(&self) -> &str {
        "inspect_memory"
    }

    fn description(&self) -> &str {
        "Inspect the full, untruncated canonical source of one result returned by search_memory. Pass the exact memory_id from that result."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "memory_id": {
                    "type": "string",
                    "description": "Exact stable memory_id returned by search_memory"
                }
            }),
            required: vec!["memory_id".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let memory_id = params["memory_id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: memory_id"))?;
        // Only the `node:<id>` form is answered from the index.
        //
        // That form goes through the in-memory tree, so an unhydrated node is
        // indistinguishable from a nonexistent one and a plain "not found"
        // tells a model the memory was never recorded (#275). Every other
        // `memory_id` -- including the conversation ids `query_with_sources`
        // attaches to attributed memories, which is the common case -- is a
        // direct SQLite read that hydration cannot affect. Qualifying those
        // manufactures doubt about a settled negative and sends a model back to
        // retry a definitive answer: this defect's mirror image, not its fix.
        //
        // `trim()` because `inspect_memory` trims before matching the prefix,
        // and a gate that disagreed with the lookup would caveat the wrong ids.
        let from_index = memory_id.trim().starts_with("node:");
        let before = self.memory_system.hydration_status();
        let found = self.memory_system.inspect_memory(memory_id).await?;
        let index = crate::memory_status::observed(before, self.memory_system.hydration_status());
        let Some(memory) = found else {
            let caveat = from_index
                .then(|| crate::memory_status::caveat(&index, false))
                .flatten();
            return Ok(match caveat {
                None => format!("No memory found for memory_id={memory_id}"),
                Some(caveat) => {
                    format!("No memory with memory_id={memory_id} is in the loaded index. {caveat}")
                }
            });
        };
        serde_json::to_string_pretty(&memory).map_err(Into::into)
    }
}

/// Create a memory explicitly (store important facts/notes)
pub struct CreateMemoryTool {
    memory_system: Arc<MemorySystem>,
}

impl CreateMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self {
        Self { memory_system }
    }
}

#[async_trait]
impl Tool for CreateMemoryTool {
    fn name(&self) -> &str {
        "create_memory"
    }

    fn description(&self) -> &str {
        "Store an explicit fact, preference, or decision in memory. Only call this when the user \
         explicitly asks you to remember something, or when a specific non-obvious fact should persist \
         across sessions (e.g. 'always use bun', 'never auto-commit'). Do NOT call this proactively \
         after routine tasks — conversations are already stored automatically."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "content": {
                    "type": "string",
                    "description": "The fact, note, or preference to remember (e.g., 'User prefers early-exit code style', 'Project uses MemTree for hierarchical memory')"
                },
                "context": {
                    "type": "string",
                    "description": "Optional context or category (e.g., 'code-style', 'project-architecture', 'user-preference')"
                }
            }),
            required: vec!["content".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let content = params["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: content"))?;

        let context = params["context"].as_str().map(|s| s.to_string());

        // Store as a system note (not attributed to user or assistant)
        let role = "system";
        let full_content = if let Some(ctx) = context {
            format!("[{}] {}", ctx, content)
        } else {
            content.to_string()
        };

        tracing::info!("Creating explicit memory: {}", full_content);

        self.memory_system
            .insert_conversation(role, &full_content, Some("memory-tool"), None)
            .await?;

        Ok(format!(
            "Memory created: {}",
            if full_content.len() > 100 {
                format!("{}...", full_content.chars().take(100).collect::<String>())
            } else {
                full_content
            }
        ))
    }
}

/// List recent conversations from memory
pub struct ListRecentTool {
    memory_system: Arc<MemorySystem>,
}

impl ListRecentTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self {
        Self { memory_system }
    }
}

#[async_trait]
impl Tool for ListRecentTool {
    fn name(&self) -> &str {
        "list_recent_memories"
    }

    fn description(&self) -> &str {
        "List recent conversations from memory in chronological order. Only call this when the user \
         explicitly asks to review history (e.g. 'what did we work on last time?'). Do NOT call this \
         proactively or as a startup check before handling a coding task."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "limit": {
                    "type": "integer",
                    "description": "Number of recent conversations to show (default: 5, max: 20)",
                    "default": 5
                }
            }),
            required: vec![],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let limit = params["limit"].as_u64().unwrap_or(5).min(20) as usize;

        tracing::info!("Listing recent memories: limit={}", limit);

        let recent = self.memory_system.get_recent_conversations(limit).await?;

        if recent.is_empty() {
            return Ok("No recent memories found.".to_string());
        }

        let formatted = format!(
            "Recent {} conversation{}:\n\n{}",
            recent.len(),
            if recent.len() == 1 { "" } else { "s" },
            recent
                .iter()
                .enumerate()
                .map(|(i, (role, content))| {
                    // Truncate very long messages
                    let preview = if content.len() > 200 {
                        format!("{}...", content.chars().take(200).collect::<String>())
                    } else {
                        content.clone()
                    };
                    format!("{}. {}: {}", i + 1, role, preview)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        );

        Ok(formatted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryConfig;
    use tempfile::NamedTempFile;

    fn test_context<'a>() -> ToolContext<'a> {
        ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            poset: None,
        }
    }

    #[tokio::test]
    async fn test_search_memory_tool() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = Arc::new(MemorySystem::new(config)?);

        // Insert test data
        memory
            .insert_conversation("user", "How do I use Rust lifetimes?", Some("test"), None)
            .await?;
        memory
            .insert_conversation(
                "assistant",
                "Lifetimes in Rust ensure references are valid...",
                Some("test"),
                None,
            )
            .await?;

        // Create tool and search
        let tool = SearchMemoryTool::new(memory);
        let context = test_context();
        let result = tool
            .execute(
                serde_json::json!({
                    "query": "rust lifetimes",
                    "limit": 2
                }),
                &context,
            )
            .await?;

        assert!(result.contains("relevant"));
        assert!(result.contains("Rust") || result.contains("lifetimes"));
        assert!(result.contains("memory_id="));

        Ok(())
    }

    /// Corrupt a store so hydration cannot read it.
    ///
    /// `level` is read as an i64, and TEXT that does not look numeric keeps its
    /// type under INTEGER affinity, so every row fails to parse. The migrations
    /// leave it alone: migration A only drops `tree_nodes` when `node_id` is
    /// absent, and `schema.sql` is `CREATE TABLE IF NOT EXISTS`.
    fn break_hydration(db_path: &std::path::Path) -> Result<()> {
        let conn = rusqlite::Connection::open(db_path)?;
        conn.execute("UPDATE tree_nodes SET level = 'unreadable'", [])?;
        Ok(())
    }

    /// Wait for the background loader to stop, rather than racing it.
    async fn settled_hydration(memory: &MemorySystem) -> crate::memory::HydrationStatus {
        for _ in 0..200 {
            let status = memory.hydration_status();
            if !matches!(status, crate::memory::HydrationStatus::Loading { .. }) {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("hydration never reached a terminal state");
    }

    /// A search that read nothing must not report that nothing exists.
    ///
    /// The tool is the sharper half of #275. A human sees the status line next
    /// to the answer; a model sees only the sentence, and
    /// "No relevant memories found for this query." is a claim about the store,
    /// not about the search. Made against an index that never loaded, it is
    /// false, and it is the kind of false a model acts on -- it concludes the
    /// user never said the thing and moves on.
    ///
    /// This covers the synchronous loader: `#[tokio::test]` is a current-thread
    /// runtime, so `MemorySystem::new` loads in line and never spawns a batched
    /// loader at all. Production is `#[tokio::main]`, i.e. multi-threaded and
    /// batched -- covered separately below, because the two failure paths reach
    /// `Failed` through different code.
    #[tokio::test]
    async fn test_search_does_not_report_absence_from_an_index_it_could_not_read() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let db_path = temp.path().to_path_buf();

        {
            let memory = MemorySystem::new(MemoryConfig {
                db_path: db_path.clone(),
                use_neural_embeddings: false,
                ..Default::default()
            })?;
            memory
                .insert_conversation("user", "my deploy key lives in 1Password", Some("t"), None)
                .await?;
        }

        // Nothing loads, so the index is Failed -- the memory above is on disk
        // and unreachable, which is exactly the state where an absence claim
        // does damage.
        break_hydration(&db_path)?;

        let memory = Arc::new(MemorySystem::new(MemoryConfig {
            db_path,
            use_neural_embeddings: false,
            ..Default::default()
        })?);
        assert!(
            matches!(
                memory.hydration_status(),
                crate::memory::HydrationStatus::Failed { .. }
            ),
            "fixture must actually break hydration, or this test cannot fail: {:?}",
            memory.hydration_status()
        );

        let result = SearchMemoryTool::new(memory)
            .execute(
                serde_json::json!({ "query": "deploy key", "limit": 5 }),
                &test_context(),
            )
            .await?;

        assert!(
            !result.contains("No relevant memories found"),
            "asserted absence on an index it never read: {result}"
        );
        assert!(
            result.contains("unavailable"),
            "did not tell the caller the index was unreadable: {result}"
        );

        Ok(())
    }

    /// The same guarantee on the loader production actually runs.
    ///
    /// `MemorySystem::new` only spawns the batched background loader on a
    /// multi-threaded runtime; a current-thread runtime loads synchronously.
    /// So the test above, despite its subject, never executes `load_batch` --
    /// and the batched path is where `Failed` is raised from a different place,
    /// where hydration is still in flight when `new` returns, and where #242
    /// made a partial index a normal part of startup rather than an edge case.
    ///
    /// Waits for a settled status rather than asserting immediately: `new`
    /// returns while the loader is still running, so an immediate assert would
    /// be a race that passes or fails by timing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_search_qualifies_its_answer_when_the_batched_loader_fails() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let db_path = temp.path().to_path_buf();

        {
            let memory = MemorySystem::new(MemoryConfig {
                db_path: db_path.clone(),
                use_neural_embeddings: false,
                ..Default::default()
            })?;
            memory
                .insert_conversation("user", "my deploy key lives in 1Password", Some("t"), None)
                .await?;
        }
        break_hydration(&db_path)?;

        let memory = Arc::new(MemorySystem::new(MemoryConfig {
            db_path,
            use_neural_embeddings: false,
            ..Default::default()
        })?);
        // Pin the path, not just the outcome. `HydrationState::new` seeds
        // `done` only when the store is empty, and this fixture is not, so on
        // the spawned path the status still reads `Loading` when `new` returns
        // -- the main thread has a few instructions' head start on the worker
        // -- while the synchronous path has already finished. Cut the
        // `MultiThread` arm out of `MemorySystem::new` and this assertion
        // fails, which is what makes the test cover the batched loader rather
        // than merely coexist with it.
        assert!(
            matches!(
                memory.hydration_status(),
                crate::memory::HydrationStatus::Loading { .. }
            ),
            "no background loader was spawned, so this is the synchronous path: {:?}",
            memory.hydration_status()
        );
        let settled = settled_hydration(&memory).await;
        assert!(
            matches!(settled, crate::memory::HydrationStatus::Failed { .. }),
            "the batched loader must actually fail, or this test cannot fail: {settled:?}"
        );

        let result = SearchMemoryTool::new(memory)
            .execute(
                serde_json::json!({ "query": "deploy key", "limit": 5 }),
                &test_context(),
            )
            .await?;

        assert!(
            !result.contains("No relevant memories found"),
            "asserted absence on an index the batched loader never read: {result}"
        );
        assert!(
            result.contains("unavailable"),
            "did not tell the caller the index was unreadable: {result}"
        );

        Ok(())
    }

    /// Only the index-backed lookup form gets an index caveat.
    ///
    /// `node:<id>` resolves through the MemTree, so an unhydrated node reads as
    /// a nonexistent one and a bare "not found" is a false absence claim. Every
    /// other memory_id -- including the conversation ids attached to attributed
    /// memories, which is the common case -- is a direct SQLite read that
    /// hydration cannot affect. Caveating those manufactures doubt about a
    /// settled negative and sends a model back to retry a definitive answer,
    /// which is this defect's mirror image rather than its fix (#275).
    ///
    /// Both halves are asserted here because the gate was twice described in a
    /// comment without being present in the code.
    #[tokio::test]
    async fn test_inspect_qualifies_only_the_lookups_the_index_answers() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let db_path = temp.path().to_path_buf();
        {
            let memory = MemorySystem::new(MemoryConfig {
                db_path: db_path.clone(),
                use_neural_embeddings: false,
                ..Default::default()
            })?;
            memory
                .insert_conversation("user", "a memory that will be stranded", Some("t"), None)
                .await?;
        }
        break_hydration(&db_path)?;

        let memory = Arc::new(MemorySystem::new(MemoryConfig {
            db_path,
            use_neural_embeddings: false,
            ..Default::default()
        })?);
        assert!(
            matches!(
                memory.hydration_status(),
                crate::memory::HydrationStatus::Failed { .. }
            ),
            "fixture must break hydration, or neither half of this test can fail"
        );
        let tool = InspectMemoryTool::new(memory);

        let from_index = tool
            .execute(
                serde_json::json!({ "memory_id": "node:7" }),
                &test_context(),
            )
            .await?;
        assert!(
            from_index.contains("index is unavailable"),
            "a node lookup against an unusable index must not read as absence: {from_index}"
        );

        let from_sqlite = tool
            .execute(
                serde_json::json!({ "memory_id": "a-conversation-id-that-does-not-exist" }),
                &test_context(),
            )
            .await?;
        assert!(
            !from_sqlite.contains("index"),
            "qualified a settled SQLite negative, which sends a model back to \
             retry an answer that will never change: {from_sqlite}"
        );
        assert!(from_sqlite.contains("No memory found"), "{from_sqlite}");

        Ok(())
    }

    #[tokio::test]
    async fn inspect_memory_returns_complete_source() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let memory = Arc::new(MemorySystem::new(MemoryConfig {
            db_path: temp.path().to_path_buf(),
            use_neural_embeddings: false,
            ..Default::default()
        })?);
        let content = format!(
            "Remember this complete source: {}",
            "long provenance payload ".repeat(30)
        );
        memory
            .insert_conversation("user", &content, Some("test"), Some("session-1"))
            .await?;
        let result = memory
            .query_with_sources("complete source provenance", Some(1))
            .await?
            .pop()
            .expect("memory search result");
        let tool = InspectMemoryTool::new(memory);
        let context = test_context();
        let output = tool
            .execute(serde_json::json!({"memory_id": result.memory_id}), &context)
            .await?;
        assert!(output.contains(&content));
        assert!(output.contains("session-1"));
        Ok(())
    }

    #[tokio::test]
    async fn test_create_memory_tool() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = Arc::new(MemorySystem::new(config)?);
        let tool = CreateMemoryTool::new(memory.clone());

        let context = test_context();

        let result = tool
            .execute(
                serde_json::json!({
                    "content": "User prefers early-exit code style",
                    "context": "code-style"
                }),
                &context,
            )
            .await?;

        assert!(result.contains("Memory created"));

        // Verify it was stored
        let stats = memory.stats().await?;
        assert_eq!(stats.conversation_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_list_recent_tool() -> Result<()> {
        let temp = NamedTempFile::new()?;
        let config = MemoryConfig {
            db_path: temp.path().to_path_buf(),
            ..Default::default()
        };

        let memory = Arc::new(MemorySystem::new(config)?);

        // Insert test data
        for i in 1..=5 {
            memory
                .insert_conversation("user", &format!("Message {}", i), Some("test"), None)
                .await?;
        }

        let tool = ListRecentTool::new(memory);
        let context = test_context();
        let result = tool
            .execute(serde_json::json!({"limit": 3}), &context)
            .await?;

        assert!(result.contains("Recent 3"));
        assert!(result.contains("Message 5")); // Most recent

        Ok(())
    }
}
