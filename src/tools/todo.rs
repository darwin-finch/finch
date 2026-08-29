// Local projection for TodoWrite / TodoRead tools
//
// Named-Brain sessions journal replacements before updating this local TUI
// projection. Standalone tool tests may still use it without a Brain target.

pub use crate::brain::tasks::{
    BrainTask as TodoItem, BrainTaskPriority as TodoPriority, BrainTaskStatus as TodoStatus,
};
use anyhow::Result;
use std::cell::RefCell;
use std::rc::Rc;
use tokio::sync::{mpsc, oneshot};

struct TodoJournalRequest {
    tasks: Vec<TodoItem>,
    reply: oneshot::Sender<Result<bool>>,
}

/// Send-safe model-tool endpoint for the frontend-local Brain journal worker.
#[derive(Clone)]
pub struct TodoJournalWriter {
    tx: mpsc::UnboundedSender<TodoJournalRequest>,
}

impl TodoJournalWriter {
    /// Persist a replacement when a Brain is selected. `false` means this is
    /// a standalone session and the caller may retain session-local behavior.
    pub async fn replace(&self, tasks: Vec<TodoItem>) -> Result<bool> {
        let (reply, response) = oneshot::channel();
        self.tx
            .send(TodoJournalRequest { tasks, reply })
            .map_err(|_| anyhow::anyhow!("Brain task journal worker stopped"))?;
        response
            .await
            .map_err(|_| anyhow::anyhow!("Brain task journal worker dropped its response"))?
    }
}

/// Frontend-local selector for the Brain that owns model-facing task writes.
/// The Cap'n Proto client is deliberately confined to this LocalSet thread;
/// tools communicate with it through the send-safe writer above.
#[derive(Clone)]
pub struct TodoJournalTarget {
    selected: Rc<RefCell<Option<crate::brain::remote::AttachedBrainClient>>>,
}

impl TodoJournalTarget {
    pub fn set(&self, selected: Option<crate::brain::remote::AttachedBrainClient>) {
        *self.selected.borrow_mut() = selected;
    }
}

pub struct TodoJournalReceiver {
    rx: mpsc::UnboundedReceiver<TodoJournalRequest>,
    selected: Rc<RefCell<Option<crate::brain::remote::AttachedBrainClient>>>,
    projection: std::sync::Arc<tokio::sync::RwLock<TodoList>>,
}

impl TodoJournalReceiver {
    /// Start the non-Send Cap'n Proto worker after the REPL enters its LocalSet.
    pub fn spawn(mut self) {
        let worker_target = Rc::clone(&self.selected);
        tokio::task::spawn_local(async move {
            while let Some(request) = self.rx.recv().await {
                let target = worker_target.borrow().clone();
                let result = match target {
                    Some(target) => {
                        let tasks = request.tasks;
                        match target
                            .push(crate::brain::store::BrainEventKind::TaskListReplaced {
                                tasks: tasks.clone(),
                            })
                            .await
                        {
                            Ok(()) => {
                                self.projection.write().await.replace_all(tasks);
                                Ok(true)
                            }
                            Err(error) => Err(error),
                        }
                    }
                    None => Ok(false),
                };
                let _ = request.reply.send(result);
            }
        });
    }
}

pub fn todo_journal(
    projection: std::sync::Arc<tokio::sync::RwLock<TodoList>>,
) -> (TodoJournalWriter, TodoJournalTarget, TodoJournalReceiver) {
    let (tx, rx) = mpsc::unbounded_channel::<TodoJournalRequest>();
    let selected = Rc::new(RefCell::new(
        None::<crate::brain::remote::AttachedBrainClient>,
    ));
    (
        TodoJournalWriter { tx },
        TodoJournalTarget {
            selected: Rc::clone(&selected),
        },
        TodoJournalReceiver {
            rx,
            selected,
            projection,
        },
    )
}

/// Local projection of the selected Brain's durable task list.
///
/// Shared behind `Arc<RwLock<TodoList>>` between the tool implementations
/// and the TUI renderer. Standalone tool instances retain this projection as
/// their session-local authority when no Brain journal target is installed.
#[derive(Default)]
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
    fn journal_worker_starts_only_inside_the_frontend_local_set() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let projection = std::sync::Arc::new(tokio::sync::RwLock::new(TodoList::default()));
            let (writer, _target, receiver) = todo_journal(projection);
            receiver.spawn();
            assert!(!writer
                .replace(vec![item("1", TodoStatus::InProgress, TodoPriority::High,)])
                .await
                .unwrap());
        }));
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
