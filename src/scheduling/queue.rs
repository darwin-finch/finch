// Task queue implementation using SQLite

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

/// A scheduled task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: Option<i64>,
    pub scheduled_time: DateTime<Utc>,
    pub task: String,
    pub context: String,
    pub recurring: Option<String>, // "hourly", "daily", "weekly"
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    pub last_run: Option<DateTime<Utc>>,
    pub retries: u32,
}

/// Task queue backed by SQLite
#[allow(dead_code)]
pub struct TaskQueue {
    db_path: PathBuf,
}

impl TaskQueue {
    /// Create new task queue
    ///
    /// The SQLite backend is not yet implemented (GitHub Issue #8).
    /// Tasks enqueued will not be persisted.
    pub fn new(db_path: PathBuf) -> Result<Self> {
        Ok(Self { db_path })
    }

    /// Enqueue a new task.
    ///
    /// **Not yet implemented** — the SQLite backend is missing.
    /// Returns an error so callers know the task was not stored,
    /// rather than silently discarding it.
    pub async fn enqueue(&self, _task: ScheduledTask) -> Result<i64> {
        anyhow::bail!(
            "Task scheduling not yet implemented (GitHub Issue #8). \
             The SQLite backend for the task queue has not been built. \
             Task was not persisted."
        )
    }

    /// Get ready tasks (scheduled_time <= now, status = Pending).
    ///
    /// Returns empty while the backend is unimplemented — the scheduler
    /// loop runs but finds no work to do, which is harmless.
    pub async fn get_ready_tasks(&self) -> Result<Vec<ScheduledTask>> {
        Ok(Vec::new())
    }

    /// Mark task as completed (no-op while backend is unimplemented).
    pub async fn mark_completed(&self, _task_id: i64) -> Result<()> {
        Ok(())
    }

    /// Mark task as failed (no-op while backend is unimplemented).
    pub async fn mark_failed(&self, _task_id: i64, _error: &str) -> Result<()> {
        Ok(())
    }

    /// Increment retry count (no-op while backend is unimplemented).
    pub async fn increment_retry(&self, _task_id: i64) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_task(task: &str) -> ScheduledTask {
        ScheduledTask {
            id: None,
            scheduled_time: Utc::now(),
            task: task.to_string(),
            context: "{}".to_string(),
            recurring: None,
            status: TaskStatus::Pending,
            created_at: Utc::now(),
            last_run: None,
            retries: 0,
        }
    }

    #[test]
    fn test_task_status_equality() {
        assert_eq!(TaskStatus::Pending, TaskStatus::Pending);
        assert_ne!(TaskStatus::Pending, TaskStatus::Completed);
        assert_ne!(TaskStatus::Running, TaskStatus::Failed);
    }

    #[test]
    fn test_task_status_serde_roundtrip() {
        for status in [TaskStatus::Pending, TaskStatus::Running, TaskStatus::Completed, TaskStatus::Failed] {
            let json = serde_json::to_string(&status).unwrap();
            let decoded: TaskStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, status);
        }
    }

    #[test]
    fn test_scheduled_task_creation() {
        let task = make_task("run_training");
        assert_eq!(task.task, "run_training");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.retries, 0);
        assert!(task.id.is_none());
        assert!(task.last_run.is_none());
        assert!(task.recurring.is_none());
    }

    #[test]
    fn test_scheduled_task_recurring() {
        let mut task = make_task("sync");
        task.recurring = Some("daily".to_string());
        task.id = Some(42);
        assert_eq!(task.id, Some(42));
        assert_eq!(task.recurring.as_deref(), Some("daily"));
    }

    #[test]
    fn test_task_queue_creation() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_queue.db")).unwrap();
        // Queue created — stub stores path but doesn't open DB
        drop(queue);
    }

    // --- Regression: enqueue must return an error, not silently discard tasks ---
    #[tokio::test]
    async fn test_enqueue_returns_error_not_implemented() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q2.db")).unwrap();
        let task = make_task("test_task");
        let result = queue.enqueue(task).await;
        assert!(result.is_err(), "enqueue should return an error until SQLite backend is implemented");
        assert!(result.unwrap_err().to_string().contains("not yet implemented"));
    }

    #[tokio::test]
    async fn test_get_ready_tasks_empty() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q3.db")).unwrap();
        let tasks = queue.get_ready_tasks().await.unwrap();
        assert!(tasks.is_empty()); // Stub returns empty
    }

    #[tokio::test]
    async fn test_mark_completed() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q4.db")).unwrap();
        assert!(queue.mark_completed(1).await.is_ok());
    }

    #[tokio::test]
    async fn test_mark_failed() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q5.db")).unwrap();
        assert!(queue.mark_failed(1, "timeout").await.is_ok());
    }

    #[tokio::test]
    async fn test_increment_retry() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q6.db")).unwrap();
        assert!(queue.increment_retry(1).await.is_ok());
    }

    #[test]
    fn test_scheduled_task_serde_roundtrip() {
        let task = make_task("train_lora");
        let json = serde_json::to_string(&task).unwrap();
        let decoded: ScheduledTask = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.task, task.task);
        assert_eq!(decoded.status, task.status);
        assert_eq!(decoded.retries, task.retries);
    }
}
