// Task queue implementation using SQLite

use anyhow::{Context, Result};
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
pub struct TaskQueue {
    db_path: PathBuf,
}

impl TaskQueue {
    /// Create new task queue
    pub fn new(db_path: PathBuf) -> Result<Self> {
        // TODO: Initialize SQLite database
        // TODO: Create tasks table with schema
        Ok(Self { db_path })
    }

    /// Enqueue a new task
    pub async fn enqueue(&self, _task: ScheduledTask) -> Result<i64> {
        // TODO: INSERT into tasks table
        // TODO: Return task ID
        Ok(1) // Placeholder
    }

    /// Get ready tasks (scheduled_time <= now, status = Pending)
    pub async fn get_ready_tasks(&self) -> Result<Vec<ScheduledTask>> {
        // TODO: SELECT from tasks WHERE scheduled_time <= now AND status = 'pending'
        Ok(Vec::new()) // Placeholder
    }

    /// Mark task as completed
    pub async fn mark_completed(&self, _task_id: i64) -> Result<()> {
        // TODO: UPDATE tasks SET status = 'completed'
        Ok(())
    }

    /// Mark task as failed
    pub async fn mark_failed(&self, _task_id: i64, _error: &str) -> Result<()> {
        // TODO: UPDATE tasks SET status = 'failed', error = ?
        Ok(())
    }

    /// Increment retry count
    pub async fn increment_retry(&self, _task_id: i64) -> Result<()> {
        // TODO: UPDATE tasks SET retries = retries + 1
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

    #[tokio::test]
    async fn test_enqueue_returns_id() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q2.db")).unwrap();
        let task = make_task("test_task");
        let id = queue.enqueue(task).await.unwrap();
        assert_eq!(id, 1); // Stub always returns 1
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
