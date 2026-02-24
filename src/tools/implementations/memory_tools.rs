// Memory tools for LLM to explicitly manage memories
//
// Provides:
// - search_memory: Query past conversations by semantic similarity
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

        let results = self.memory_system.query(query, Some(limit)).await?;

        if results.is_empty() {
            return Ok("No relevant memories found for this query.".to_string());
        }

        let formatted = format!(
            "Found {} relevant memor{}:\n\n{}",
            results.len(),
            if results.len() == 1 { "y" } else { "ies" },
            results
                .iter()
                .enumerate()
                .map(|(i, text)| {
                    // Truncate very long memories
                    let preview = if text.len() > 500 {
                        format!("{}...", &text[..500])
                    } else {
                        text.clone()
                    };
                    format!("{}. {}", i + 1, preview)
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        );

        Ok(formatted)
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
                format!("{}...", &full_content[..100])
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
                        format!("{}...", &content[..200])
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
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };
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

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

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
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };
        let result = tool
            .execute(serde_json::json!({"limit": 3}), &context)
            .await?;

        assert!(result.contains("Recent 3"));
        assert!(result.contains("Message 5")); // Most recent

        Ok(())
    }
}
