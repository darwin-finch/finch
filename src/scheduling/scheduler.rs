// Task scheduler daemon loop

use crate::scheduling::queue::TaskQueue;
use anyhow::Result;
use futures::future::BoxFuture;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

/// Task scheduler that executes scheduled tasks
pub struct TaskScheduler {
    queue: Arc<TaskQueue>,
    running: Arc<AtomicBool>,
    executor: Option<
        Arc<
            dyn Fn(crate::scheduling::ScheduledTask) -> BoxFuture<'static, Result<String>>
                + Send
                + Sync,
        >,
    >,
}

impl TaskScheduler {
    /// Create new scheduler
    pub fn new(queue: Arc<TaskQueue>) -> Self {
        Self {
            queue,
            running: Arc::new(AtomicBool::new(false)),
            executor: None,
        }
    }

    pub fn with_executor(
        queue: Arc<TaskQueue>,
        executor: Arc<
            dyn Fn(crate::scheduling::ScheduledTask) -> BoxFuture<'static, Result<String>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self {
            queue,
            running: Arc::new(AtomicBool::new(false)),
            executor: Some(executor),
        }
    }

    /// Run scheduler loop (checks every minute)
    pub async fn run(&self) -> Result<()> {
        self.running.store(true, Ordering::SeqCst);
        info!("Task scheduler started");

        while self.running.load(Ordering::SeqCst) {
            // Wait 1 minute
            tokio::time::sleep(Duration::from_secs(60)).await;

            // Get ready tasks
            self.run_once().await?;
        }
        Ok(())
    }

    pub async fn run_once(&self) -> Result<()> {
        let ready_tasks = self.queue.get_ready_tasks().await?;
        if ready_tasks.is_empty() {
            return Ok(());
        }
        info!("Found {} ready tasks", ready_tasks.len());
        for task in ready_tasks {
            info!("Executing task: {}", task.task);

            // TODO: Execute task
            // TODO: Handle recurring tasks
            // TODO: Update task status

            match self.execute_task(&task).await {
                Ok(_) => {
                    info!("Task completed: {}", task.task);
                    if let Some(task_id) = task.id {
                        self.queue.mark_completed(task_id).await?;
                    }
                }
                Err(e) => {
                    error!("Task failed: {} (error: {})", task.task, e);
                    if let Some(task_id) = task.id {
                        if task.retries < 3 {
                            self.queue.increment_retry(task_id).await?;
                        } else {
                            self.queue.mark_failed(task_id, &e.to_string()).await?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn execute_task(&self, task: &crate::scheduling::queue::ScheduledTask) -> Result<String> {
        let Some(executor) = &self.executor else {
            anyhow::bail!("scheduled task executor is not attached")
        };
        executor(task.clone()).await
    }

    /// Stop scheduler
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduling::queue::TaskQueue;

    fn make_scheduler() -> TaskScheduler {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let queue = Arc::new(TaskQueue::new(tmp.path().to_path_buf()).unwrap());
        TaskScheduler::new(Arc::clone(&queue))
    }

    #[test]
    fn test_scheduler_starts_not_running() {
        let scheduler = make_scheduler();
        // Before run() is called, running flag should be false
        assert!(!scheduler.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_scheduler_stop_clears_running_flag() {
        let scheduler = make_scheduler();
        scheduler.running.store(true, Ordering::SeqCst);
        scheduler.stop();
        assert!(!scheduler.running.load(Ordering::SeqCst));
    }

    #[test]
    fn test_scheduler_stop_is_idempotent() {
        let scheduler = make_scheduler();
        scheduler.stop(); // already false
        scheduler.stop(); // still false
        assert!(!scheduler.running.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn run_once_executes_ready_task_and_marks_it_complete() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = Arc::new(TaskQueue::new(database.path().to_path_buf()).unwrap());
        let task = crate::scheduling::ScheduledTask {
            id: None,
            scheduled_time: chrono::Utc::now(),
            task: "typed callback".into(),
            context: "{}".into(),
            recurring: None,
            status: crate::scheduling::TaskStatus::Pending,
            created_at: chrono::Utc::now(),
            last_run: None,
            retries: 0,
        };
        let id = queue.enqueue(task).await.unwrap();
        let executor = Arc::new(|task: crate::scheduling::ScheduledTask| {
            Box::pin(async move { Ok(format!("ran {}", task.task)) })
                as BoxFuture<'static, Result<String>>
        });
        let scheduler = TaskScheduler::with_executor(Arc::clone(&queue), executor);
        scheduler.run_once().await.unwrap();
        assert!(queue.get_ready_tasks().await.unwrap().is_empty());
        assert!(id > 0);
    }
}
