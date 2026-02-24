// Integration tests for TodoWrite / TodoRead tools
//
// Verifies the cross-module interaction: both tools share the same
// Arc<RwLock<TodoList>> and the list's active_items ordering is correct
// for TUI display.

use finch::tools::implementations::{TodoReadTool, TodoWriteTool};
use finch::tools::registry::Tool;
use finch::tools::todo::{TodoItem, TodoList, TodoPriority, TodoStatus};
use finch::tools::types::ToolContext;
use std::sync::Arc;
use tokio::sync::RwLock;

fn make_list() -> Arc<RwLock<TodoList>> {
    Arc::new(RwLock::new(TodoList::default()))
}

fn dummy_ctx() -> ToolContext<'static> {
    ToolContext {
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

// ── Tool name / schema ─────────────────────────────────────────────────────────

#[test]
fn test_tool_names() {
    let list = make_list();
    assert_eq!(TodoWriteTool::new(Arc::clone(&list)).name(), "TodoWrite");
    assert_eq!(TodoReadTool::new(Arc::clone(&list)).name(), "TodoRead");
}

#[test]
fn test_write_schema_requires_todos_param() {
    let schema = TodoWriteTool::new(make_list()).input_schema();
    assert!(schema.required.contains(&"todos".to_string()));
}

#[test]
fn test_read_schema_requires_no_params() {
    let schema = TodoReadTool::new(make_list()).input_schema();
    assert!(schema.required.is_empty());
}

// ── Shared Arc state ─────────────────────────────────────────────────────────

/// Write tool and read tool share the same underlying list.
#[tokio::test]
async fn test_write_and_read_share_state() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let read = TodoReadTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({
                "todos": [
                    { "id": "1", "content": "Integrate tools", "status": "in_progress", "priority": "high" }
                ]
            }),
            &ctx,
        )
        .await
        .unwrap();

    let json = read.execute(serde_json::json!({}), &ctx).await.unwrap();
    let items: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], "1");
    assert_eq!(items[0]["content"], "Integrate tools");
}

/// Atomicity: the second write replaces the list entirely.
#[tokio::test]
async fn test_write_is_atomic_replacement() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "1", "content": "First", "status": "pending", "priority": "low" }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "2", "content": "Second A", "status": "pending", "priority": "high" },
                { "id": "3", "content": "Second B", "status": "pending", "priority": "medium" }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let items = list.read().await;
    assert_eq!(items.len(), 2, "old item must be gone");
    assert_eq!(items.get_all()[0].id, "2");
}

// ── JSON serialization ────────────────────────────────────────────────────────

/// TodoRead output is valid JSON that round-trips through serde_json.
#[tokio::test]
async fn test_read_output_is_valid_json_roundtrip() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let read = TodoReadTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "a", "content": "Alpha", "status": "in_progress", "priority": "high" },
                { "id": "b", "content": "Beta",  "status": "completed",   "priority": "low"  }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let raw = read.execute(serde_json::json!({}), &ctx).await.unwrap();
    // Must parse as a JSON array
    let parsed: Vec<TodoItem> =
        serde_json::from_str(&raw).expect("TodoRead output must be valid JSON array of TodoItem");

    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].id, "a");
    assert_eq!(parsed[0].status, TodoStatus::InProgress);
    assert_eq!(parsed[0].priority, TodoPriority::High);
    assert_eq!(parsed[1].id, "b");
    assert_eq!(parsed[1].status, TodoStatus::Completed);
}

/// Status values serialise with the correct snake_case strings.
#[tokio::test]
async fn test_status_serialization_snake_case() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let read = TodoReadTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "1", "content": "x", "status": "in_progress", "priority": "medium" }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let raw = read.execute(serde_json::json!({}), &ctx).await.unwrap();
    // The serialised JSON must use "in_progress" (snake_case), not "InProgress"
    assert!(
        raw.contains("\"in_progress\""),
        "status must be snake_case in JSON: {raw}"
    );
}

// ── TUI list rendering (active_items ordering) ─────────────────────────────────

/// TodoList::active_items returns in_progress before pending, completed excluded.
/// This is the ordering used by the TUI live-area rendering.
#[tokio::test]
async fn test_active_items_ordering_for_tui_display() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "1", "content": "Pending low",    "status": "pending",     "priority": "low"    },
                { "id": "2", "content": "Completed",      "status": "completed",   "priority": "high"   },
                { "id": "3", "content": "InProgress med", "status": "in_progress", "priority": "medium" },
                { "id": "4", "content": "Pending high",   "status": "pending",     "priority": "high"   }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let guard = list.read().await;
    let active = guard.active_items();

    // Completed item must be excluded
    assert_eq!(active.len(), 3, "completed item should be excluded");

    // in_progress must come before pending items
    assert_eq!(
        active[0].status,
        TodoStatus::InProgress,
        "in_progress must be first"
    );

    // Within the pending group, high priority must precede low
    let pending: Vec<_> = active
        .iter()
        .filter(|i| i.status == TodoStatus::Pending)
        .collect();
    assert_eq!(pending[0].priority, TodoPriority::High);
    assert_eq!(pending[1].priority, TodoPriority::Low);
}

/// When all items are completed, active_items is empty (TUI hides the panel).
#[tokio::test]
async fn test_active_items_empty_when_all_completed() {
    let list = make_list();
    let write = TodoWriteTool::new(Arc::clone(&list));
    let ctx = dummy_ctx();

    write
        .execute(
            serde_json::json!({ "todos": [
                { "id": "1", "content": "Done", "status": "completed", "priority": "high" }
            ]}),
            &ctx,
        )
        .await
        .unwrap();

    let guard = list.read().await;
    assert!(
        guard.active_items().is_empty(),
        "TUI should see no active items when all are completed"
    );
}
