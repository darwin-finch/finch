// LoRA Trainer - Training loop with weighted examples

use anyhow::{Context, Result};
use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{loss, ops, Optimizer};
use std::sync::Arc;
use tokenizers::Tokenizer;

use super::lora_impl::{ExampleBuffer, LoRAAdapter, WeightedExample};

/// Training statistics
#[derive(Debug, Clone)]
pub struct TrainingStats {
    pub epoch: usize,
    pub total_epochs: usize,
    pub batch: usize,
    pub total_batches: usize,
    pub loss: f64,
    pub examples_trained: usize,
}

/// LoRA trainer for fine-tuning Qwen models
pub struct LoRATrainer {
    /// LoRA adapter being trained
    adapter: LoRAAdapter,
    /// Tokenizer for encoding/decoding
    tokenizer: Arc<Tokenizer>,
    /// Learning rate
    learning_rate: f64,
    /// Batch size
    batch_size: usize,
    /// Number of epochs
    epochs: usize,
}

impl LoRATrainer {
    /// Create new trainer
    pub fn new(
        adapter: LoRAAdapter,
        tokenizer: Arc<Tokenizer>,
        learning_rate: f64,
        batch_size: usize,
        epochs: usize,
    ) -> Self {
        Self {
            adapter,
            tokenizer,
            learning_rate,
            batch_size,
            epochs,
        }
    }

    /// Train on a buffer of weighted examples
    ///
    /// Uses weighted sampling - critical examples (10x weight) appear more often
    pub fn train(&mut self, buffer: &ExampleBuffer) -> Result<Vec<TrainingStats>> {
        if buffer.examples().is_empty() {
            anyhow::bail!("No examples in buffer");
        }

        tracing::info!(
            "Starting LoRA training on {} examples (total weight: {:.1})",
            buffer.examples().len(),
            buffer.total_weight()
        );

        let mut rng = rand::thread_rng();
        let mut stats_vec = Vec::new();

        // Enable adapter for training
        self.adapter.enable();

        for epoch in 0..self.epochs {
            tracing::debug!("Epoch {}/{}", epoch + 1, self.epochs);

            // Sample batches with weighted sampling
            let num_batches = (buffer.examples().len() / self.batch_size).max(1);

            for batch_idx in 0..num_batches {
                // Weighted sampling - critical examples appear 10x more
                let batch_examples = buffer.sample_batch(self.batch_size, &mut rng);

                if batch_examples.is_empty() {
                    continue;
                }

                // Train on batch
                let loss = self.train_batch(&batch_examples)?;

                let stats = TrainingStats {
                    epoch: epoch + 1,
                    total_epochs: self.epochs,
                    batch: batch_idx + 1,
                    total_batches: num_batches,
                    loss,
                    examples_trained: batch_examples.len(),
                };

                tracing::debug!(
                    "Epoch {}/{}, Batch {}/{}, Loss: {:.4}",
                    stats.epoch,
                    stats.total_epochs,
                    stats.batch,
                    stats.total_batches,
                    stats.loss
                );

                stats_vec.push(stats);
            }
        }

        tracing::info!(
            "Training complete. Final loss: {:.4}",
            stats_vec.last().map(|s| s.loss).unwrap_or(0.0)
        );

        Ok(stats_vec)
    }

    /// Train on a single batch of examples
    fn train_batch(&mut self, examples: &[WeightedExample]) -> Result<f64> {
        let device = self.adapter.device().clone();
        let mut total_loss = 0.0;

        for example in examples {
            // Tokenize input and target
            let input_text = format!("Query: {}\nResponse:", example.query);
            let target_text = &example.response;

            let input_tokens = self
                .tokenizer
                .encode(input_text.as_str(), false)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

            let target_tokens = self
                .tokenizer
                .encode(target_text.as_str(), false)
                .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

            let input_ids = input_tokens.get_ids();
            let target_ids = target_tokens.get_ids();

            // Create tensors
            let input_tensor = Tensor::new(input_ids, &device)?.unsqueeze(0)?; // [1, seq_len]
            let target_tensor = Tensor::new(target_ids, &device)?; // [seq_len]

            // Forward pass (simplified - in real implementation, integrate with Qwen model)
            // For now, compute loss on target sequence
            // NOTE: This is a placeholder - actual implementation would:
            // 1. Run Qwen model with LoRA adapters
            // 2. Compute logits for next token prediction
            // 3. Calculate cross-entropy loss
            // 4. Backpropagate through LoRA matrices only

            // Placeholder loss computation
            let loss_value = self.compute_loss_placeholder(target_ids.len(), example.weight)?;

            total_loss += loss_value;

            // Update LoRA parameters
            self.update_parameters(loss_value)?;
        }

        Ok(total_loss / examples.len() as f64)
    }

    /// Placeholder loss computation
    /// TODO: Replace with actual model forward pass and cross-entropy
    fn compute_loss_placeholder(&self, seq_len: usize, weight: f64) -> Result<f64> {
        // Simplified loss computation
        // In real implementation:
        // 1. Forward pass through Qwen + LoRA
        // 2. Compute cross-entropy between predicted and target tokens
        // 3. Weight the loss by example weight

        // For now, return a decreasing loss (simulates convergence)
        let base_loss = 2.0 * (1.0 - 0.1 * seq_len as f64 / 100.0).max(0.5);
        Ok(base_loss * weight)
    }

    /// Update LoRA parameters using gradients
    /// TODO: Implement actual gradient computation and optimization
    fn update_parameters(&mut self, _loss: f64) -> Result<()> {
        // In real implementation:
        // 1. Compute gradients via backpropagation
        // 2. Apply optimizer (SGD/Adam) to LoRA matrices only
        // 3. Update A and B matrices

        // For now, this is a placeholder
        // Actual implementation will use candle's autodiff

        Ok(())
    }

    /// Get the trained adapter
    pub fn adapter(&self) -> &LoRAAdapter {
        &self.adapter
    }

    /// Get mutable adapter
    pub fn adapter_mut(&mut self) -> &mut LoRAAdapter {
        &mut self.adapter
    }
}

/// Background training coordinator
pub struct TrainingCoordinator {
    /// Example buffer
    buffer: Arc<tokio::sync::RwLock<ExampleBuffer>>,
    /// Training threshold (train after N examples)
    threshold: usize,
    /// Whether auto-training is enabled
    auto_train: bool,
}

impl TrainingCoordinator {
    /// Create new coordinator
    pub fn new(buffer_size: usize, threshold: usize, auto_train: bool) -> Self {
        Self {
            buffer: Arc::new(tokio::sync::RwLock::new(ExampleBuffer::new(buffer_size))),
            threshold,
            auto_train,
        }
    }

    /// Add example to buffer
    pub async fn add_example(&self, example: WeightedExample) -> Result<bool> {
        let mut buffer = self.buffer.write().await;
        buffer.add(example);

        // Check if we should trigger training
        let should_train = self.auto_train && buffer.examples().len() >= self.threshold;

        Ok(should_train)
    }

    /// Get current buffer
    pub async fn buffer(&self) -> tokio::sync::RwLockReadGuard<ExampleBuffer> {
        self.buffer.read().await
    }

    /// Get mutable buffer
    pub async fn buffer_mut(&self) -> tokio::sync::RwLockWriteGuard<ExampleBuffer> {
        self.buffer.write().await
    }

    /// Trigger training (call this when should_train returns true)
    pub async fn trigger_training<F, Fut>(&self, train_fn: F) -> Result<()>
    where
        F: FnOnce(Vec<WeightedExample>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let examples = {
            let buffer = self.buffer.read().await;
            buffer.examples().to_vec()
        };

        if examples.is_empty() {
            return Ok(());
        }

        tracing::info!(
            "Triggering background training on {} examples",
            examples.len()
        );

        // Call training function
        train_fn(examples).await?;

        Ok(())
    }

    /// Save buffer to disk
    pub async fn save_buffer(&self, path: &std::path::Path) -> Result<()> {
        let buffer = self.buffer.read().await;
        buffer.save(path)
    }

    /// Load buffer from disk
    pub async fn load_buffer(&self, path: &std::path::Path) -> Result<()> {
        let loaded = ExampleBuffer::load(path, self.buffer.read().await.examples().len())?;
        let mut buffer = self.buffer.write().await;
        *buffer = loaded;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::lora::LoRAConfig;

    #[test]
    fn test_training_stats() {
        let stats = TrainingStats {
            epoch: 1,
            total_epochs: 3,
            batch: 2,
            total_batches: 5,
            loss: 1.234,
            examples_trained: 4,
        };

        assert_eq!(stats.epoch, 1);
        assert_eq!(stats.loss, 1.234);
    }

    #[tokio::test]
    async fn test_training_coordinator() {
        let coordinator = TrainingCoordinator::new(100, 10, true);

        // Add examples
        for i in 0..5 {
            let ex =
                WeightedExample::normal(format!("q{}", i), format!("r{}", i), "good".to_string());
            let should_train = coordinator.add_example(ex).await.unwrap();
            assert!(!should_train); // < threshold
        }

        // Add more to reach threshold
        for i in 5..10 {
            let ex =
                WeightedExample::normal(format!("q{}", i), format!("r{}", i), "good".to_string());
            let should_train = coordinator.add_example(ex).await.unwrap();

            if i == 9 {
                assert!(should_train); // Reached threshold
            }
        }

        let buffer = coordinator.buffer().await;
        assert_eq!(buffer.examples().len(), 10);
    }

    #[tokio::test]
    async fn test_weighted_training_priority() {
        let coordinator = TrainingCoordinator::new(100, 5, true);

        // Add 1 critical and 4 normal examples
        coordinator
            .add_example(WeightedExample::critical(
                "critical".into(),
                "response".into(),
                "This is wrong".into(),
            ))
            .await
            .unwrap();

        for i in 0..4 {
            coordinator
                .add_example(WeightedExample::normal(
                    format!("normal{}", i),
                    "response".into(),
                    "good".into(),
                ))
                .await
                .unwrap();
        }

        let buffer = coordinator.buffer().await;
        assert_eq!(buffer.examples().len(), 5);

        // Total weight: 10 (critical) + 4 (normal) = 14
        assert_eq!(buffer.total_weight(), 14.0);

        // Critical example represents 10/14 ≈ 71% of total weight
        // So it should be sampled ~71% of the time
    }
}
