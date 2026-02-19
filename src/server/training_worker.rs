// Background training worker for daemon
//
// Collects weighted examples via mpsc channel and triggers LoRA training
// when batch threshold is reached or timeout occurs.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use crate::models::{TrainingCoordinator, WeightedExample};
use crate::training::lora_subprocess::LoRATrainingSubprocess;

/// Training worker state
pub struct TrainingWorker {
    /// Channel for receiving weighted examples
    example_rx: mpsc::UnboundedReceiver<WeightedExample>,
    /// Training coordinator for JSONL queue management
    coordinator: Arc<TrainingCoordinator>,
    /// Python subprocess spawner for LoRA training
    subprocess: LoRATrainingSubprocess,
    /// Batch threshold (trigger training after N examples)
    batch_threshold: usize,
    /// Timeout duration (trigger training after duration if batch not full)
    batch_timeout: Duration,
}

impl TrainingWorker {
    /// Create a new training worker
    pub fn new(
        example_rx: mpsc::UnboundedReceiver<WeightedExample>,
        coordinator: Arc<TrainingCoordinator>,
        batch_threshold: usize,
        batch_timeout_minutes: u64,
    ) -> Self {
        let subprocess = LoRATrainingSubprocess::with_defaults();

        Self {
            example_rx,
            coordinator,
            subprocess,
            batch_threshold,
            batch_timeout: Duration::from_secs(batch_timeout_minutes * 60),
        }
    }

    /// Run the training worker loop
    ///
    /// This runs indefinitely, accumulating examples and triggering training
    /// when batch threshold is reached or timeout occurs.
    pub async fn run(mut self) {
        info!(
            batch_threshold = self.batch_threshold,
            timeout_minutes = self.batch_timeout.as_secs() / 60,
            "Training worker started"
        );

        let mut batch = Vec::new();
        let mut flush_interval = tokio::time::interval(self.batch_timeout);
        flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                // Receive examples from API
                Some(example) = self.example_rx.recv() => {
                    debug!(weight = example.weight, "Received training example");
                    batch.push(example.clone());

                    // Add to coordinator buffer
                    if let Err(e) = self.coordinator.add_example(example) {
                        error!(error = %e, "Failed to add example to coordinator");
                    }

                    // Check if batch threshold reached
                    if batch.len() >= self.batch_threshold {
                        info!(count = batch.len(), "Batch threshold reached, triggering training");
                        if let Err(e) = self.process_batch(&mut batch).await {
                            error!(error = %e, "Failed to process training batch");
                        }
                    }
                }

                // Periodic flush (timeout)
                _ = flush_interval.tick() => {
                    if !batch.is_empty() {
                        info!(count = batch.len(), "Batch timeout reached, triggering training");
                        if let Err(e) = self.process_batch(&mut batch).await {
                            error!(error = %e, "Failed to process training batch");
                        }
                    } else {
                        debug!("Flush interval tick, but batch is empty");
                    }
                }
            }
        }
    }

    /// Process accumulated batch of examples
    async fn process_batch(&self, batch: &mut Vec<WeightedExample>) -> Result<()> {
        info!(count = batch.len(), "Processing training batch");

        // Write to JSONL queue
        self.coordinator
            .write_training_queue()
            .map_err(|e| anyhow::anyhow!("Failed to write training queue: {}", e))?;

        info!("Training queue written successfully");

        // Clear coordinator buffer
        self.coordinator
            .clear_buffer()
            .map_err(|e| anyhow::anyhow!("Failed to clear coordinator buffer: {}", e))?;

        // Trigger Python training subprocess (non-blocking)
        let queue_path = self.coordinator.queue_path().to_path_buf();
        let adapter_path = self.get_adapter_path();

        info!(
            queue = %queue_path.display(),
            adapter = %adapter_path.display(),
            "Starting LoRA training subprocess"
        );

        self.subprocess
            .train_async(&queue_path, &adapter_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to start training subprocess: {}", e))?;

        info!("Training subprocess started successfully");

        // Clear batch
        batch.clear();

        Ok(())
    }

    /// Get adapter output path
    fn get_adapter_path(&self) -> std::path::PathBuf {
        let home = dirs::home_dir().expect("Cannot determine home directory");
        home.join(".finch")
            .join("adapters")
            .join("latest.safetensors")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_worker_creation() {
        let (_tx, rx) = mpsc::unbounded_channel();
        let coordinator = Arc::new(TrainingCoordinator::new(100, 10, true));

        let worker = TrainingWorker::new(rx, coordinator, 10, 5);

        assert_eq!(worker.batch_threshold, 10);
        assert_eq!(worker.batch_timeout, Duration::from_secs(5 * 60));
    }
}
