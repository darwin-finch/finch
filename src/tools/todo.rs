// Session-scoped task list for TodoWrite / TodoRead tools
//
// TodoList is never persisted to disk — it lives only for the duration of the
// REPL session.  The LLM writes to it via TodoWrite and reads from it via
// TodoRead.  The TUI renders the active (non-completed) items in the live area.

use serde::{Deserialize, Serialize};

/// Priority of a task item
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TodoPriority {
    High,
    #[default]
    Medium,
    Low,
}

/// Lifecycle status of a task item
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// A single task item in the session task list
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    /// Stable identifier across updates (e.g. "1", "2")
    pub id: String,
    /// Human-readable description
    pub content: String,
    pub status: TodoStatus,
    #[serde(default)]
    pub priority: TodoPriority,
}

/// Session-scoped task list.
///
/// Shared behind `Arc<RwLock<TodoList>>` between the tool implementations
/// and the TUI renderer.
#[derive(Debug, Default)]
pub struct TodoList {
    items: Vec<TodoItem>,
}

impl TodoList {
    /// Replace the entire list atomically (the semantics of TodoWrite).
    pub fn replace_all(&mut self, items: Vec<TodoItem>) {
        self.items = items;
    }

    /// Return all items (for TodoRead / serialisation).
    pub fn get_all(&self) -> &[TodoItem] {
        &self.items
    }

    /// Return items to display in the TUI: in_progress first, then pending.
    /// Completed items are excluded.  Within each group, high > medium > low.
    pub fn active_items(&self) -> Vec<&TodoItem> {
        let priority_ord = |p: &TodoPriority| match p {
            TodoPriority::High => 0,
            TodoPriority::Medium => 1,
            TodoPriority::Low => 2,
        };

        let mut in_progress: Vec<&TodoItem> = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::InProgress)
            .collect();
        let mut pending: Vec<&TodoItem> = self
            .items
            .iter()
            .filter(|i| i.status == TodoStatus::Pending)
            .collect();

        in_progress.sort_by_key(|i| priority_ord(&i.priority));
        pending.sort_by_key(|i| priority_ord(&i.priority));

        in_progress.extend(pending);
        in_progress
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: TodoStatus, priority: TodoPriority) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: format!("Task {}", id),
            status,
            priority,
        }
    }

    #[test]
    fn test_replace_all_empty() {
        let mut list = TodoList::default();
        list.replace_all(vec![]);
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn test_replace_all_nonempty() {
        let mut list = TodoList::default();
        list.replace_all(vec![
            item("1", TodoStatus::Pending, TodoPriority::High),
            item("2", TodoStatus::InProgress, TodoPriority::Medium),
            item("3", TodoStatus::Completed, TodoPriority::Low),
        ]);
        assert_eq!(list.len(), 3);
        assert_eq!(list.get_all().len(), 3);
    }

    #[test]
    fn test_replace_all_is_atomic() {
        let mut list = TodoList::default();
        list.replace_all(vec![item("1", TodoStatus::Pending, TodoPriority::High)]);
        list.replace_all(vec![item("2", TodoStatus::InProgress, TodoPriority::Low)]);
        assert_eq!(list.len(), 1);
        assert_eq!(list.get_all()[0].id, "2");
    }

    #[test]
    fn test_active_items_filters_completed() {
        let mut list = TodoList::default();
        list.replace_all(vec![
            item("1", TodoStatus::Pending, TodoPriority::Medium),
            item("2", TodoStatus::Completed, TodoPriority::High),
            item("3", TodoStatus::InProgress, TodoPriority::Low),
        ]);
        let active = list.active_items();
        assert_eq!(active.len(), 2);
        assert!(active.iter().all(|i| i.status != TodoStatus::Completed));
    }

    #[test]
    fn test_active_items_in_progress_before_pending() {
        let mut list = TodoList::default();
        list.replace_all(vec![
            item("1", TodoStatus::Pending, TodoPriority::High),
            item("2", TodoStatus::InProgress, TodoPriority::Low),
        ]);
        let active = list.active_items();
        assert_eq!(active[0].status, TodoStatus::InProgress);
        assert_eq!(active[1].status, TodoStatus::Pending);
    }

    #[test]
    fn test_active_items_priority_order_within_group() {
        let mut list = TodoList::default();
        list.replace_all(vec![
            item("1", TodoStatus::Pending, TodoPriority::Low),
            item("2", TodoStatus::Pending, TodoPriority::High),
            item("3", TodoStatus::Pending, TodoPriority::Medium),
        ]);
        let active = list.active_items();
        assert_eq!(active[0].priority, TodoPriority::High);
        assert_eq!(active[1].priority, TodoPriority::Medium);
        assert_eq!(active[2].priority, TodoPriority::Low);
    }

    #[test]
    fn test_active_items_empty_when_all_completed() {
        let mut list = TodoList::default();
        list.replace_all(vec![
            item("1", TodoStatus::Completed, TodoPriority::High),
            item("2", TodoStatus::Completed, TodoPriority::Medium),
        ]);
        assert!(list.active_items().is_empty());
    }

    #[test]
    fn test_serde_roundtrip() {
        let item = TodoItem {
            id: "42".to_string(),
            content: "Write tests".to_string(),
            status: TodoStatus::InProgress,
            priority: TodoPriority::High,
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: TodoItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "42");
        assert_eq!(back.status, TodoStatus::InProgress);
        assert_eq!(back.priority, TodoPriority::High);
    }

    #[test]
    fn test_status_serde_snake_case() {
        let s = serde_json::to_string(&TodoStatus::InProgress).unwrap();
        assert_eq!(s, "\"in_progress\"");
        let back: TodoStatus = serde_json::from_str("\"in_progress\"").unwrap();
        assert_eq!(back, TodoStatus::InProgress);
    }

    #[test]
    fn test_priority_serde_lowercase() {
        let s = serde_json::to_string(&TodoPriority::High).unwrap();
        assert_eq!(s, "\"high\"");
        let back: TodoPriority = serde_json::from_str("\"high\"").unwrap();
        assert_eq!(back, TodoPriority::High);
    }
}
