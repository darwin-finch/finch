// Task queue implementation using SQLite

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
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
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let queue = Self { db_path };
        queue.connection()?.execute_batch(
            "CREATE TABLE IF NOT EXISTS scheduled_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scheduled_time TEXT NOT NULL,
                task TEXT NOT NULL,
                context TEXT NOT NULL,
                recurring TEXT,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_run TEXT,
                retries INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        Ok(queue)
    }

    fn connection(&self) -> Result<Connection> {
        Ok(Connection::open(&self.db_path)?)
    }

    pub async fn enqueue(&self, task: ScheduledTask) -> Result<i64> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO scheduled_tasks
             (scheduled_time, task, context, recurring, status, created_at, last_run, retries)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                task.scheduled_time.to_rfc3339(),
                task.task,
                task.context,
                task.recurring,
                serde_json::to_string(&task.status)?,
                task.created_at.to_rfc3339(),
                task.last_run.map(|time| time.to_rfc3339()),
                task.retries,
            ],
        )?;
        Ok(connection.last_insert_rowid())
    }

    pub async fn get_ready_tasks(&self) -> Result<Vec<ScheduledTask>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, scheduled_time, task, context, recurring, status, created_at, last_run, retries
             FROM scheduled_tasks WHERE status = ?1 AND scheduled_time <= ?2 ORDER BY scheduled_time, id",
        )?;
        let rows = statement.query_map(
            params![
                serde_json::to_string(&TaskStatus::Pending)?,
                Utc::now().to_rfc3339()
            ],
            |row| {
                let status: String = row.get(5)?;
                Ok(ScheduledTask {
                    id: row.get(0)?,
                    scheduled_time: parse_time(row.get(1)?)?,
                    task: row.get(2)?,
                    context: row.get(3)?,
                    recurring: row.get(4)?,
                    status: serde_json::from_str(&status).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            5,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?,
                    created_at: parse_time(row.get(6)?)?,
                    last_run: row
                        .get::<_, Option<String>>(7)?
                        .map(parse_time)
                        .transpose()?,
                    retries: row.get(8)?,
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub async fn get_task(&self, task_id: i64) -> Result<Option<ScheduledTask>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT id, scheduled_time, task, context, recurring, status, created_at, last_run, retries
             FROM scheduled_tasks WHERE id = ?1",
        )?;
        let task = statement
            .query_row(params![task_id], scheduled_task_from_row)
            .optional()?;
        Ok(task)
    }

    /// Cancel a pending scheduled task without deleting its audit record.
    /// Returns false for an unknown or already-started/completed task.
    pub async fn cancel(&self, task_id: i64) -> Result<bool> {
        let changed = self.connection()?.execute(
            "UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2 AND status = ?3",
            params![
                serde_json::to_string(&TaskStatus::Cancelled)?,
                task_id,
                serde_json::to_string(&TaskStatus::Pending)?,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Atomically claim a pending task for execution. This is the queue's
    /// cancellation race boundary: exactly one of claim or cancel may change
    /// the row from Pending.
    pub async fn claim(&self, task_id: i64) -> Result<bool> {
        let changed = self.connection()?.execute(
            "UPDATE scheduled_tasks SET status = ?1 WHERE id = ?2 AND status = ?3",
            params![
                serde_json::to_string(&TaskStatus::Running)?,
                task_id,
                serde_json::to_string(&TaskStatus::Pending)?,
            ],
        )?;
        Ok(changed == 1)
    }

    pub async fn mark_completed(&self, task_id: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE scheduled_tasks SET status = ?1, last_run = ?2 WHERE id = ?3",
            params![
                serde_json::to_string(&TaskStatus::Completed)?,
                Utc::now().to_rfc3339(),
                task_id
            ],
        )?;
        Ok(())
    }

    pub async fn mark_failed(&self, task_id: i64, _error: &str) -> Result<()> {
        self.connection()?.execute(
            "UPDATE scheduled_tasks SET status = ?1, last_run = ?2 WHERE id = ?3",
            params![
                serde_json::to_string(&TaskStatus::Failed)?,
                Utc::now().to_rfc3339(),
                task_id
            ],
        )?;
        Ok(())
    }

    pub async fn increment_retry(&self, task_id: i64) -> Result<()> {
        self.connection()?.execute(
            "UPDATE scheduled_tasks SET retries = retries + 1, status = ?1 WHERE id = ?2",
            params![serde_json::to_string(&TaskStatus::Pending)?, task_id],
        )?;
        Ok(())
    }
}

fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|time| time.with_timezone(&Utc))
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

fn scheduled_task_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScheduledTask> {
    let status: String = row.get(5)?;
    Ok(ScheduledTask {
        id: row.get(0)?,
        scheduled_time: parse_time(row.get(1)?)?,
        task: row.get(2)?,
        context: row.get(3)?,
        recurring: row.get(4)?,
        status: serde_json::from_str(&status).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        created_at: parse_time(row.get(6)?)?,
        last_run: row
            .get::<_, Option<String>>(7)?
            .map(parse_time)
            .transpose()?,
        retries: row.get(8)?,
    })
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
        for status in [
            TaskStatus::Pending,
            TaskStatus::Running,
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Cancelled,
        ] {
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
    async fn test_enqueue_persists_tasks() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = TaskQueue::new(database.path().to_path_buf()).unwrap();
        let task = make_task("test_task");
        let id = queue.enqueue(task).await.unwrap();
        assert!(id > 0);
        let ready = queue.get_ready_tasks().await.unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, Some(id));
        assert_eq!(ready[0].task, "test_task");
    }

    #[tokio::test]
    async fn test_get_ready_tasks_empty() {
        let queue = TaskQueue::new(PathBuf::from("/tmp/test_finch_q3.db")).unwrap();
        let tasks = queue.get_ready_tasks().await.unwrap();
        assert!(tasks.is_empty()); // Stub returns empty
    }

    #[tokio::test]
    async fn get_and_cancel_preserve_the_schedule_record() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = TaskQueue::new(database.path().to_path_buf()).unwrap();
        let id = queue.enqueue(make_task("cancel-me")).await.unwrap();

        let task = queue.get_task(id).await.unwrap().unwrap();
        assert_eq!(task.task, "cancel-me");
        assert_eq!(task.status, TaskStatus::Pending);
        assert!(queue.cancel(id).await.unwrap());
        assert!(!queue.cancel(id).await.unwrap());

        let cancelled = queue.get_task(id).await.unwrap().unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert!(queue.get_ready_tasks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn claim_and_cancel_are_mutually_exclusive() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = TaskQueue::new(database.path().to_path_buf()).unwrap();
        let claimed = queue.enqueue(make_task("claimed")).await.unwrap();
        let cancelled = queue.enqueue(make_task("cancelled")).await.unwrap();

        assert!(queue.claim(claimed).await.unwrap());
        assert!(!queue.cancel(claimed).await.unwrap());
        assert!(queue.cancel(cancelled).await.unwrap());
        assert!(!queue.claim(cancelled).await.unwrap());
        assert_eq!(
            queue.get_task(claimed).await.unwrap().unwrap().status,
            TaskStatus::Running
        );
        assert_eq!(
            queue.get_task(cancelled).await.unwrap().unwrap().status,
            TaskStatus::Cancelled
        );
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
