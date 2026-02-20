// Task scheduler daemon loop

use crate::scheduling::queue::TaskQueue;
use anyhow::Result;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};

/// Task scheduler that executes scheduled tasks
pub struct TaskScheduler {
    queue: Arc<TaskQueue>,
    running: Arc<AtomicBool>,
}

impl TaskScheduler {
    /// Create new scheduler
    pub fn new(queue: Arc<TaskQueue>) -> Self {
        Self {
            queue,
            running: Arc::new(AtomicBool::new(false)),
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
            let ready_tasks = self.queue.get_ready_tasks().await?;

            if ready_tasks.is_empty() {
                continue;
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
        }

        Ok(())
    }

    /// Execute a single task
    async fn execute_task(&self, _task: &crate::scheduling::queue::ScheduledTask) -> Result<String> {
        // TODO: Reconstruct conversation context
        // TODO: Execute via generator
        // TODO: Return response
        Ok("Task executed".to_string())
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
}
