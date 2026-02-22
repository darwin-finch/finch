// Batch training system for efficient GPU utilization

use anyhow::Result;
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use crate::models::{GeneratorModel, ModelConfig, RouterModel, ValidatorModel};

/// Training example (query, response, metadata)
#[derive(Debug, Clone)]
pub struct TrainingExample {
    /// User query
    pub query: String,
    /// Claude's response (or local response if validated)
    pub response: String,
    /// Was this handled locally successfully?
    pub local_success: bool,
    /// Routing decision confidence
    pub router_confidence: Option<f64>,
    /// Validator quality score
    pub validator_score: Option<f64>,
    /// Timestamp
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl TrainingExample {
    pub fn new(query: String, response: String, local_success: bool) -> Self {
        Self {
            query,
            response,
            local_success,
            router_confidence: None,
            validator_score: None,
            timestamp: chrono::Utc::now(),
        }
    }

    pub fn with_confidence(mut self, confidence: f64) -> Self {
        self.router_confidence = Some(confidence);
        self
    }

    pub fn with_quality(mut self, quality: f64) -> Self {
        self.validator_score = Some(quality);
        self
    }
}

/// Result of batch training
#[derive(Debug, Clone)]
pub struct TrainingResult {
    /// Number of examples trained on
    pub examples_count: usize,
    /// Router loss before training
    pub router_old_loss: f64,
    /// Router loss after training
    pub router_new_loss: f64,
    /// Generator loss before training
    pub generator_old_loss: f64,
    /// Generator loss after training
    pub generator_new_loss: f64,
    /// Validator loss before training
    pub validator_old_loss: f64,
    /// Validator loss after training
    pub validator_new_loss: f64,
    /// Training duration in seconds
    pub duration_secs: f64,
}

/// Batch trainer - accumulates examples and trains in batches
pub struct BatchTrainer {
    /// Training queue (thread-safe)
    training_queue: Arc<Mutex<VecDeque<TrainingExample>>>,
    /// Batch size (number of examples per training batch)
    batch_size: usize,
    /// Learning rate
    learning_rate: f64,
    /// Router model (shared, write access during training)
    router: Arc<RwLock<RouterModel>>,
    /// Generator model (shared, write access during training)
    generator: Arc<RwLock<GeneratorModel>>,
    /// Validator model (shared, write access during training)
    validator: Arc<RwLock<ValidatorModel>>,
    /// Total examples trained on
    total_trained: Arc<Mutex<usize>>,
    /// Last training timestamp
    last_training: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
}

impl BatchTrainer {
    /// Create new batch trainer
    pub fn new(batch_size: usize, learning_rate: f64, config: &ModelConfig) -> Result<Self> {
        // Create models
        let router = RouterModel::new(config)?;
        let generator_config = crate::models::GeneratorConfig::RandomInit(config.clone());
        let generator = GeneratorModel::new(generator_config)?;
        let validator = ValidatorModel::new(config)?;

        Ok(Self {
            training_queue: Arc::new(Mutex::new(VecDeque::new())),
            batch_size,
            learning_rate,
            router: Arc::new(RwLock::new(router)),
            generator: Arc::new(RwLock::new(generator)),
            validator: Arc::new(RwLock::new(validator)),
            total_trained: Arc::new(Mutex::new(0)),
            last_training: Arc::new(Mutex::new(None)),
        })
    }

    /// Add example to training queue
    pub async fn add_example(&self, example: TrainingExample) -> Result<()> {
        let mut queue = self.training_queue.lock().await;
        queue.push_back(example);

        tracing::debug!(queue_size = queue.len(), "Added training example to queue");

        Ok(())
    }

    /// Get current queue size
    pub async fn queue_size(&self) -> usize {
        self.training_queue.lock().await.len()
    }

    /// Check if should trigger automatic training
    pub async fn should_train_automatically(&self) -> bool {
        self.queue_size().await >= self.batch_size
    }

    /// Train on accumulated examples (non-blocking - spawns background task)
    pub async fn train_async(&self) -> Result<()> {
        // Check if enough examples
        let queue_size = self.queue_size().await;
        if queue_size < self.batch_size {
            tracing::debug!(
                queue_size = queue_size,
                batch_size = self.batch_size,
                "Not enough examples for training batch"
            );
            return Ok(());
        }

        // Clone Arc references for background task
        let training_queue = Arc::clone(&self.training_queue);
        let router = Arc::clone(&self.router);
        let generator = Arc::clone(&self.generator);
        let validator = Arc::clone(&self.validator);
        let total_trained = Arc::clone(&self.total_trained);
        let last_training = Arc::clone(&self.last_training);
        let batch_size = self.batch_size;
        let learning_rate = self.learning_rate;

        // Spawn background training task
        tokio::spawn(async move {
            match Self::train_batch_internal(
                training_queue,
                router,
                generator,
                validator,
                total_trained,
                last_training,
                batch_size,
                learning_rate,
            )
            .await
            {
                Ok(result) => {
                    tracing::info!(
                        examples = result.examples_count,
                        duration_secs = result.duration_secs,
                        router_loss_improvement = result.router_old_loss - result.router_new_loss,
                        "Batch training completed successfully"
                    );
                }
                Err(e) => {
                    tracing::error!(error = %e, "Batch training failed");
                }
            }
        });

        Ok(())
    }

    /// Train immediately and wait for completion (blocking)
    pub async fn train_now(&self) -> Result<TrainingResult> {
        Self::train_batch_internal(
            Arc::clone(&self.training_queue),
            Arc::clone(&self.router),
            Arc::clone(&self.generator),
            Arc::clone(&self.validator),
            Arc::clone(&self.total_trained),
            Arc::clone(&self.last_training),
            self.batch_size,
            self.learning_rate,
        )
        .await
    }

    /// Internal training implementation
    async fn train_batch_internal(
        training_queue: Arc<Mutex<VecDeque<TrainingExample>>>,
        _router: Arc<RwLock<RouterModel>>,
        _generator: Arc<RwLock<GeneratorModel>>,
        _validator: Arc<RwLock<ValidatorModel>>,
        total_trained: Arc<Mutex<usize>>,
        last_training: Arc<Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
        batch_size: usize,
        _learning_rate: f64,
    ) -> Result<TrainingResult> {
        let start_time = std::time::Instant::now();

        // Extract batch from queue
        let batch = {
            let mut queue = training_queue.lock().await;
            let queue_len = queue.len();
            let batch: Vec<TrainingExample> = queue.drain(..batch_size.min(queue_len)).collect();
            batch
        };

        if batch.is_empty() {
            anyhow::bail!("No examples available for training");
        }

        tracing::info!(examples = batch.len(), "Starting batch training");

        // Batch training is not yet implemented.
        //
        // The current inference stack uses ONNX Runtime; injecting LoRA adapter
        // weights into a running ONNX session at inference time is tracked as
        // GitHub Issue #1. Until that is resolved, in-process batch training
        // has no effect on the served model.
        //
        // Feedback is collected (src/models/lora.rs, ~/.finch/training_queue.jsonl)
        // and a Python-based offline training pipeline is available via
        // src/training/lora_subprocess.rs.
        anyhow::bail!(
            "Batch training not yet implemented (see GitHub Issue #1). \
             {} examples were collected but not trained on. \
             Use the offline LoRA pipeline (src/training/lora_subprocess.rs) instead.",
            batch.len()
        )
    }

    /// Get training statistics
    pub async fn stats(&self) -> TrainingStats {
        TrainingStats {
            queue_size: self.queue_size().await,
            total_trained: *self.total_trained.lock().await,
            last_training: *self.last_training.lock().await,
        }
    }

    /// Get references to models (for saving/loading)
    pub fn router(&self) -> Arc<RwLock<RouterModel>> {
        Arc::clone(&self.router)
    }

    pub fn generator(&self) -> Arc<RwLock<GeneratorModel>> {
        Arc::clone(&self.generator)
    }

    pub fn validator(&self) -> Arc<RwLock<ValidatorModel>> {
        Arc::clone(&self.validator)
    }
}

/// Training statistics
#[derive(Debug, Clone)]
pub struct TrainingStats {
    pub queue_size: usize,
    pub total_trained: usize,
    pub last_training: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_training_example_new() {
        let ex = TrainingExample::new(
            "how do I use lifetimes?".to_string(),
            "Lifetimes annotate how long references are valid...".to_string(),
            true,
        );
        assert_eq!(ex.query, "how do I use lifetimes?");
        assert!(ex.local_success);
        assert!(ex.router_confidence.is_none());
        assert!(ex.validator_score.is_none());
    }

    #[test]
    fn test_training_example_builder_chain() {
        let ex = TrainingExample::new("query".to_string(), "response".to_string(), false)
            .with_confidence(0.85)
            .with_quality(0.92);
        assert_eq!(ex.router_confidence, Some(0.85));
        assert_eq!(ex.validator_score, Some(0.92));
        assert!(!ex.local_success);
    }

    #[test]
    fn test_training_example_timestamp_is_recent() {
        let before = chrono::Utc::now();
        let ex = TrainingExample::new("q".to_string(), "r".to_string(), true);
        let after = chrono::Utc::now();
        assert!(ex.timestamp >= before);
        assert!(ex.timestamp <= after);
    }

    #[test]
    fn test_training_stats_initial_state() {
        let stats = TrainingStats {
            queue_size: 0,
            total_trained: 0,
            last_training: None,
        };
        assert_eq!(stats.queue_size, 0);
        assert_eq!(stats.total_trained, 0);
        assert!(stats.last_training.is_none());
    }

    #[test]
    fn test_training_stats_with_data() {
        let stats = TrainingStats {
            queue_size: 5,
            total_trained: 150,
            last_training: Some(chrono::Utc::now()),
        };
        assert_eq!(stats.queue_size, 5);
        assert_eq!(stats.total_trained, 150);
        assert!(stats.last_training.is_some());
    }

    // --- Regression: train_batch_internal must return an error, not fake losses ---
    //
    // Previously the function returned hardcoded loss values (0.5, 0.45, etc.),
    // silently telling callers training had succeeded. Now it returns an explicit error.
    //
    // We can't call train_batch_internal directly (private, requires model Arcs),
    // but we can verify the public API path via train_async: since BatchTrainer
    // requires models, it can only be constructed in an integration test.
    // This unit test documents the regression and verifies the TrainingResult
    // type is preserved for when training is eventually implemented.

    #[test]
    fn test_training_result_type_exists_with_correct_fields() {
        // Regression: TrainingResult must keep its fields so that when real
        // training is implemented, callers don't need to update their code.
        let result = TrainingResult {
            examples_count: 10,
            router_old_loss: 0.0,
            router_new_loss: 0.0,
            generator_old_loss: 0.0,
            generator_new_loss: 0.0,
            validator_old_loss: 0.0,
            validator_new_loss: 0.0,
            duration_secs: 0.0,
        };
        assert_eq!(result.examples_count, 10);
        // Improvement field (used by callers to check if training helped):
        assert_eq!(result.router_old_loss - result.router_new_loss, 0.0);
    }
}

