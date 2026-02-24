// TodoWrite / TodoRead tool implementations
//
// The LLM uses these tools to manage a session-scoped task list that is
// displayed in the TUI live area.  Both tools capture an
// Arc<RwLock<TodoList>> directly — no ToolContext fields needed.

use crate::tools::registry::Tool;
use crate::tools::todo::{TodoItem, TodoList};
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::RwLock;

// ─── TodoWriteTool ────────────────────────────────────────────────────────────

/// Replace the entire session task list atomically.
pub struct TodoWriteTool {
    todo_list: Arc<RwLock<TodoList>>,
}

impl TodoWriteTool {
    pub fn new(todo_list: Arc<RwLock<TodoList>>) -> Self {
        Self { todo_list }
    }
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "TodoWrite"
    }

    fn description(&self) -> &str {
        "Replace the entire session task list atomically. Use this to create, update, or mark tasks \
         as completed. Always include ALL tasks in each call — omitted tasks are deleted. \
         Call TodoRead first if you need to preserve existing tasks."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "todos": {
                    "type": "array",
                    "description": "The complete task list (replaces all existing todos). Omitting a task deletes it.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable task ID (keep it consistent across updates, e.g. \"1\", \"2\")"
                            },
                            "content": {
                                "type": "string",
                                "description": "Task description"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"],
                                "description": "Current lifecycle status"
                            },
                            "priority": {
                                "type": "string",
                                "enum": ["high", "medium", "low"],
                                "description": "Task priority (high items shown first)"
                            }
                        },
                        "required": ["id", "content", "status", "priority"]
                    }
                }
            }),
            required: vec!["todos".to_string()],
        }
    }

    async fn execute(&self, params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let raw_todos = params
            .get("todos")
            .ok_or_else(|| anyhow::anyhow!("Missing required parameter: todos"))?;

        let items: Vec<TodoItem> = serde_json::from_value(raw_todos.clone())
            .map_err(|e| anyhow::anyhow!("Invalid todos format: {}", e))?;

        let total = items.len();
        let in_progress = items
            .iter()
            .filter(|i| matches!(i.status, crate::tools::todo::TodoStatus::InProgress))
            .count();
        let pending = items
            .iter()
            .filter(|i| matches!(i.status, crate::tools::todo::TodoStatus::Pending))
            .count();
        let completed = items
            .iter()
            .filter(|i| matches!(i.status, crate::tools::todo::TodoStatus::Completed))
            .count();

        self.todo_list.write().await.replace_all(items);

        Ok(format!(
            "Todo list updated: {} task{} ({} in_progress, {} pending, {} completed)",
            total,
            if total == 1 { "" } else { "s" },
            in_progress,
            pending,
            completed,
        ))
    }
}

// ─── TodoReadTool ─────────────────────────────────────────────────────────────

/// Return the current session task list as JSON.
pub struct TodoReadTool {
    todo_list: Arc<RwLock<TodoList>>,
}

impl TodoReadTool {
    pub fn new(todo_list: Arc<RwLock<TodoList>>) -> Self {
        Self { todo_list }
    }
}

#[async_trait]
impl Tool for TodoReadTool {
    fn name(&self) -> &str {
        "TodoRead"
    }

    fn description(&self) -> &str {
        "Return the current session task list as a JSON array. \
         Use this before TodoWrite when you want to update specific tasks \
         without losing the rest of the list."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![])
    }

    async fn execute(&self, _params: Value, _context: &ToolContext<'_>) -> Result<String> {
        let list = self.todo_list.read().await;
        let items = list.get_all();

        if items.is_empty() {
            return Ok("[]".to_string());
        }

        serde_json::to_string_pretty(items)
            .map_err(|e| anyhow::anyhow!("Failed to serialise todo list: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::todo::{TodoPriority, TodoStatus};

    fn make_list() -> Arc<RwLock<TodoList>> {
        Arc::new(RwLock::new(TodoList::default()))
    }

    fn dummy_context() -> ToolContext<'static> {
        crate::tools::types::ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        }
    }

    // ── TodoWriteTool ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_todo_write_empty_list() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let result = tool
            .execute(serde_json::json!({ "todos": [] }), &ctx)
            .await
            .unwrap();
        assert!(
            result.contains("0 tasks") || result.contains("0 task"),
            "{result}"
        );
        assert!(list.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_todo_write_valid_input() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "1", "content": "Write code", "status": "in_progress", "priority": "high" },
                        { "id": "2", "content": "Write tests", "status": "pending",     "priority": "medium" }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.contains("2 tasks"), "{result}");
        assert!(result.contains("1 in_progress"), "{result}");
        assert!(result.contains("1 pending"), "{result}");
        assert_eq!(list.read().await.len(), 2);
    }

    #[tokio::test]
    async fn test_todo_write_replaces_previous_list() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();

        tool.execute(
            serde_json::json!({ "todos": [{ "id": "1", "content": "old", "status": "pending", "priority": "low" }] }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(list.read().await.len(), 1);

        tool.execute(
            serde_json::json!({
                "todos": [
                    { "id": "2", "content": "new 1", "status": "pending", "priority": "high" },
                    { "id": "3", "content": "new 2", "status": "pending", "priority": "medium" },
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();
        assert_eq!(list.read().await.len(), 2);
        assert_eq!(list.read().await.get_all()[0].id, "2");
    }

    #[tokio::test]
    async fn test_todo_write_invalid_status_returns_error() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [{ "id": "1", "content": "x", "status": "done", "priority": "low" }]
                }),
                &ctx,
            )
            .await;
        assert!(result.is_err(), "Expected error for invalid status 'done'");
    }

    #[tokio::test]
    async fn test_todo_write_missing_required_field_returns_error() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        // missing "id"
        let result = tool
            .execute(
                serde_json::json!({
                    "todos": [{ "content": "x", "status": "pending", "priority": "low" }]
                }),
                &ctx,
            )
            .await;
        assert!(result.is_err(), "Expected error for missing 'id'");
    }

    #[tokio::test]
    async fn test_todo_write_missing_todos_param() {
        let list = make_list();
        let tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(result.is_err());
    }

    // ── TodoReadTool ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_todo_read_empty_returns_bracket_pair() {
        let list = make_list();
        let tool = TodoReadTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert_eq!(result.trim(), "[]");
    }

    #[tokio::test]
    async fn test_todo_read_roundtrip() {
        let list = make_list();
        // Write via the write tool
        let write_tool = TodoWriteTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        write_tool
            .execute(
                serde_json::json!({
                    "todos": [
                        { "id": "1", "content": "Alpha", "status": "in_progress", "priority": "high" },
                        { "id": "2", "content": "Beta",  "status": "pending",     "priority": "low"  }
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        // Read back
        let read_tool = TodoReadTool::new(Arc::clone(&list));
        let json = read_tool
            .execute(serde_json::json!({}), &ctx)
            .await
            .unwrap();
        let items: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "1");
        assert_eq!(items[1]["content"], "Beta");
    }

    #[tokio::test]
    async fn test_todo_read_includes_completed_items() {
        let list = make_list();
        {
            let mut l = list.write().await;
            l.replace_all(vec![
                TodoItem {
                    id: "1".to_string(),
                    content: "done".to_string(),
                    status: TodoStatus::Completed,
                    priority: TodoPriority::Low,
                },
                TodoItem {
                    id: "2".to_string(),
                    content: "todo".to_string(),
                    status: TodoStatus::Pending,
                    priority: TodoPriority::Medium,
                },
            ]);
        }
        let read_tool = TodoReadTool::new(Arc::clone(&list));
        let ctx = dummy_context();
        let json = read_tool
            .execute(serde_json::json!({}), &ctx)
            .await
            .unwrap();
        let items: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        // TodoRead returns ALL items including completed (full view for LLM)
        assert_eq!(items.len(), 2);
    }

    // ── Schema ────────────────────────────────────────────────────────────────

    #[test]
    fn test_todo_write_schema_requires_todos() {
        let list = make_list();
        let tool = TodoWriteTool::new(list);
        let schema = tool.input_schema();
        assert!(schema.required.contains(&"todos".to_string()));
    }

    #[test]
    fn test_todo_read_schema_has_no_required_params() {
        let list = make_list();
        let tool = TodoReadTool::new(list);
        let schema = tool.input_schema();
        assert!(schema.required.is_empty());
    }
}
