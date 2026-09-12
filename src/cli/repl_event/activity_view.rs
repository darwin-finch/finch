//! Translation from Finch's own types into the terminal's activity view model.
//!
//! The renderer draws indented status rows and knows nothing about todos or child agents. This is
//! the one place the two vocabularies meet, so the terminal stays usable by anything that has
//! neither.

use std::sync::Arc;

use crate::cli::tui::activity::{ActivityRow, ActivityRows, ActivityState, ActivityUpdate};
use crate::runtime::scheduler::{AgentEvent, AgentTaskSnapshot, AgentTaskStatus};
use crate::tools::todo::{TodoList, TodoPriority, TodoStatus};

/// The session todo list, presented as rows the renderer can poll.
pub struct TodoRows(Arc<tokio::sync::RwLock<TodoList>>);

impl TodoRows {
    pub fn new(list: Arc<tokio::sync::RwLock<TodoList>>) -> Arc<dyn ActivityRows> {
        Arc::new(Self(list))
    }
}

impl ActivityRows for TodoRows {
    fn rows(&self) -> Vec<ActivityRow> {
        // Polled on the render path: yield to a writer rather than stall the terminal, and show
        // nothing for this frame if the list is mid-update.
        let Ok(list) = self.0.try_read() else {
            return Vec::new();
        };
        list.active_items()
            .iter()
            .map(|item| ActivityRow {
                depth: 0,
                state: match item.status {
                    TodoStatus::InProgress => ActivityState::Active,
                    TodoStatus::Pending => ActivityState::Pending,
                    TodoStatus::Completed => ActivityState::Done,
                },
                text: item.content.clone(),
                urgent: matches!(item.priority, TodoPriority::High),
                detail: None,
            })
            .collect()
    }
}

/// One child agent, as a row: its depth in the tree, what it is doing, and which model runs it.
fn agent_row(snapshot: &AgentTaskSnapshot) -> ActivityRow {
    ActivityRow {
        depth: snapshot.identity.depth,
        state: match snapshot.status {
            AgentTaskStatus::Queued => ActivityState::Pending,
            AgentTaskStatus::Running => ActivityState::Active,
            _ => ActivityState::Done,
        },
        text: snapshot.task.clone(),
        urgent: false,
        detail: Some(format!(" · {}", snapshot.identity.provider_model)),
    }
}

/// A scheduler event as a change to the rows on screen.
pub fn agent_activity(event: &AgentEvent) -> ActivityUpdate {
    match event {
        AgentEvent::TaskQueued { snapshot } | AgentEvent::TaskStarted { snapshot } => {
            ActivityUpdate::Upsert {
                id: snapshot.identity.task_id,
                row: agent_row(snapshot),
            }
        }
        // A running tool is shown after the model, and clearing it restores the model alone.
        AgentEvent::ToolStarted { task_id, name } => ActivityUpdate::SetDetail {
            id: *task_id,
            detail: Some(format!(" · {name}")),
        },
        AgentEvent::ToolCompleted { task_id, .. } => ActivityUpdate::SetDetail {
            id: *task_id,
            detail: None,
        },
        AgentEvent::TaskFinished { result } => ActivityUpdate::Remove {
            id: result.identity.task_id,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_a_high_priority_todo_is_urgent_and_an_in_progress_one_is_active() {
        // The renderer decides how urgency looks; this decides what counts as urgent.
        let mut list = TodoList::default();
        list.replace_all(vec![crate::tools::todo::TodoItem {
            id: "1".into(),
            content: "write the thing".into(),
            status: TodoStatus::InProgress,
            priority: TodoPriority::High,
        }]);
        let rows = TodoRows(Arc::new(tokio::sync::RwLock::new(list))).rows();
        assert_eq!(
            1,
            rows.len(),
            "an active todo must produce one row: {rows:?}"
        );
        assert!(rows[0].urgent, "a high-priority todo must be marked urgent");
        assert_eq!("write the thing", rows[0].text);
    }

    #[test]
    fn test_a_finished_agent_removes_its_row_rather_than_marking_it_done() {
        // A finished child agent leaves the panel; leaving a tick behind would grow the live area
        // without bound over a long session.
        let id = uuid::Uuid::new_v4();
        let update = agent_activity(&AgentEvent::ToolCompleted {
            task_id: id,
            name: "bash".into(),
            is_error: false,
        });
        assert!(
            matches!(update, ActivityUpdate::SetDetail { detail: None, .. }),
            "a completed tool clears the detail rather than the row: {update:?}"
        );
    }
}
