// LoRA (Low-Rank Adaptation) - Fine-tuning adapter for Qwen models
// Phase 6: Implemented JSONL queue writer and training coordinator
//
// TODO: Current Python-based training is inefficient with ONNX Runtime
// - ONNX Runtime is inference-only (no training APIs)
// - PyTorch training requires loading model twice (2x memory usage)
// - Need pure Rust solution that works with ONNX models
//
// Future options:
// 1. Build custom Rust LoRA implementation on top of ONNX Runtime
// 2. Use burn.rs or other Rust ML framework with ONNX export
// 3. Wait for ONNX Runtime training support
// 4. Implement LoRA as ONNX graph modifications (advanced)
//
// For now: Python infrastructure exists but has known limitations

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// LoRA adapter configuration
///
/// LoRA enables efficient fine-tuning of large models by learning low-rank
/// updates to weight matrices. This allows adapting pre-trained Qwen models
/// to specific domains without retraining the entire model.
///
/// # References
/// - Paper: "LoRA: Low-Rank Adaptation of Large Language Models" (Hu et al., 2021)
/// - https://arxiv.org/abs/2106.09685
///
/// # Future Implementation
/// This is a placeholder for future LoRA fine-tuning capability. When implemented,
/// it will enable:
/// - Domain-specific adaptation (legal, medical, coding, etc.)
/// - Style transfer (match specific writing styles)
/// - Knowledge injection (add new facts without full retraining)
/// - Efficient updates (train only ~0.1% of parameters)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoRAConfig {
    /// Rank of the low-rank decomposition (typically 4-64)
    ///
    /// Lower rank = fewer parameters to train, but less expressive
    /// Higher rank = more parameters, but more flexible
    ///
    /// Recommended values:
    /// - 4-8: Very efficient, good for simple adaptations
    /// - 16-32: Balanced, works for most use cases
    /// - 64: High capacity, for complex domain transfers
    pub rank: usize,

    /// Scaling factor for LoRA updates (typically 1.0-32.0)
    ///
    /// Controls the magnitude of the adaptation.
    /// Higher alpha = stronger adaptation effect
    ///
    /// Common practice: alpha = 2 * rank
    pub alpha: f64,

    /// Dropout rate for LoRA layers (0.0-0.3)
    ///
    /// Helps prevent overfitting during fine-tuning
    pub dropout: f64,

    /// Target modules to adapt (e.g., ["q_proj", "v_proj"])
    ///
    /// LoRA can be applied selectively to specific layers:
    /// - Query/Value projections: Most common, good balance
    /// - All attention: More expressive, more parameters
    /// - All linear: Maximum flexibility, slowest
    pub target_modules: Vec<String>,
}

impl Default for LoRAConfig {
    fn default() -> Self {
        Self {
            rank: 16,
            alpha: 32.0,
            dropout: 0.0,
            target_modules: vec!["q_proj".to_string(), "v_proj".to_string()],
        }
    }
}

/// LoRA adapter for fine-tuning pre-trained models
///
/// # Example (Future Usage)
/// ```text
/// use finch::models::{GeneratorModel, LoRATrainingAdapter, LoRAConfig};
///
/// // Load pre-trained Qwen model
/// let mut generator = GeneratorModel::new(config)?;
///
/// // Create LoRA adapter for domain adaptation
/// let lora_config = LoRAConfig {
///     rank: 16,
///     alpha: 32.0,
///     dropout: 0.1,
///     target_modules: vec!["q_proj".into(), "v_proj".into()],
/// };
///
/// // Fine-tune on domain-specific examples
/// let examples = vec![
///     ("Explain quantum entanglement".into(), "In quantum physics...".into()),
///     ("What is a qubit?".into(), "A qubit is...".into()),
/// ];
///
/// generator.fine_tune_with_lora(&examples, lora_config, epochs: 3)?;
///
/// // Save adapted model
/// generator.save_lora("~/.finch/adapters/physics.safetensors")?;
/// ```
#[derive(Debug)]
pub struct LoRATrainingAdapter {
    config: LoRAConfig,
    enabled: bool,
}

impl LoRATrainingAdapter {
    /// Create new LoRA adapter with given configuration
    /// Phase 4: device parameter removed (was Candle-based)
    pub fn new(config: LoRAConfig, _device: ()) -> Result<Self> {
        Ok(Self {
            config,
            enabled: false,
        })
    }

    /// Create LoRA adapter with default configuration
    pub fn default_config() -> Result<Self> {
        Self::new(LoRAConfig::default(), ())
    }

    /// Train LoRA adapter on examples
    ///
    /// # Arguments
    /// * `examples` - Training data as (query, response) pairs
    /// * `epochs` - Number of training epochs (1-10 typical)
    /// * `learning_rate` - Learning rate (1e-5 to 1e-3 typical)
    ///
    /// # Returns
    /// Error with message "Not yet implemented"
    ///
    /// # Future Implementation
    /// Will use:
    /// - Candle's linear layers for low-rank matrices
    /// - SGD or AdamW optimizer
    /// - Cross-entropy loss
    /// - Gradient accumulation for large batches
    pub fn train(
        &mut self,
        _examples: &[(String, String)],
        _epochs: usize,
        _learning_rate: f64,
    ) -> Result<()> {
        anyhow::bail!(
            "LoRA fine-tuning not yet implemented. This is a placeholder for future functionality.\n\
             \n\
             Planned implementation:\n\
             - Low-rank adaptation of attention layers\n\
             - Efficient fine-tuning (train ~0.1% of parameters)\n\
             - Domain-specific adaptation\n\
             - Style transfer\n\
             \n\
             See: https://arxiv.org/abs/2106.09685"
        )
    }

    /// Enable the LoRA adapter
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Disable the LoRA adapter (revert to base model)
    pub fn disable(&mut self) {
        self.enabled = false;
    }

    /// Check if adapter is enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get adapter configuration
    pub fn config(&self) -> &LoRAConfig {
        &self.config
    }

    /// Save adapter weights to file
    ///
    /// # Future Implementation
    /// Will save:
    /// - Low-rank matrices (A and B)
    /// - Configuration (rank, alpha, target modules)
    /// - Metadata (training stats, timestamp)
    pub fn save(&self, _path: &std::path::Path) -> Result<()> {
        anyhow::bail!("LoRA adapter saving not yet implemented")
    }

    /// Load adapter weights from file
    ///
    /// # Future Implementation
    /// Will load previously trained adapter from safetensors format
    pub fn load(_path: &std::path::Path) -> Result<Self> {
        anyhow::bail!("LoRA adapter loading not yet implemented")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lora_config_default() {
        let config = LoRAConfig::default();
        assert_eq!(config.rank, 16);
        assert_eq!(config.alpha, 32.0);
        assert_eq!(config.dropout, 0.0);
        assert_eq!(config.target_modules.len(), 2);
    }

    #[test]
    fn test_lora_adapter_creation() {
        let config = LoRAConfig::default();
        let adapter = LoRATrainingAdapter::new(config, ()).unwrap();

        assert!(!adapter.is_enabled());
        assert_eq!(adapter.config().rank, 16);
    }

    #[test]
    fn test_lora_adapter_enable_disable() {
        let mut adapter = LoRATrainingAdapter::default_config().unwrap();

        assert!(!adapter.is_enabled());

        adapter.enable();
        assert!(adapter.is_enabled());

        adapter.disable();
        assert!(!adapter.is_enabled());
    }

    #[test]
    fn test_lora_train_not_implemented() {
        let mut adapter = LoRATrainingAdapter::default_config().unwrap();

        let examples = vec![
            ("Hello".to_string(), "World".to_string()),
            ("Foo".to_string(), "Bar".to_string()),
        ];

        let result = adapter.train(&examples, 1, 1e-4);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("not yet implemented"));
    }

    #[test]
    fn test_lora_save_not_implemented() {
        let adapter = LoRATrainingAdapter::default_config().unwrap();
        let path = std::path::Path::new("/tmp/test.safetensors");

        let result = adapter.save(path);
        assert!(result.is_err());
    }

    #[test]
    fn test_lora_load_not_implemented() {
        let path = std::path::Path::new("/tmp/test.safetensors");
        let result = LoRATrainingAdapter::load(path);
        assert!(result.is_err());
    }
}

// Phase 4: Stub types for removed Candle-based LoRA implementation
// These will be replaced with Python/ONNX-based implementation in Phase 5

/// Weighted training example (Phase 6: Added serialization for JSONL export)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedExample {
    pub query: String,
    pub response: String,
    pub weight: f64,
    pub feedback: Option<String>,
}

impl WeightedExample {
    pub fn critical(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            weight: 10.0,
            feedback: Some(feedback),
        }
    }

    pub fn improvement(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            weight: 3.0,
            feedback: Some(feedback),
        }
    }

    pub fn normal(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            weight: 1.0,
            feedback: Some(feedback),
        }
    }

    pub fn with_weight(query: String, response: String, feedback: String, weight: f64) -> Self {
        Self {
            query,
            response,
            weight,
            feedback: Some(feedback),
        }
    }
}

/// Example buffer for batching (stub for Phase 5)
#[derive(Debug)]
pub struct ExampleBuffer {
    examples: Vec<WeightedExample>,
}

impl ExampleBuffer {
    pub fn new(_capacity: usize) -> Self {
        Self {
            examples: Vec::new(),
        }
    }

    pub fn add(&mut self, example: WeightedExample) {
        self.examples.push(example);
    }

    pub fn len(&self) -> usize {
        self.examples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.examples.is_empty()
    }

    pub fn as_slice(&self) -> &[WeightedExample] {
        &self.examples
    }

    pub fn examples(&self) -> &[WeightedExample] {
        &self.examples
    }

    pub fn total_weight(&self) -> f64 {
        self.examples.iter().map(|e| e.weight).sum()
    }
}

/// LoRA trainer (stub for Phase 5)
#[derive(Debug)]
pub struct LoRATrainer {
    _adapter: LoRATrainingAdapter,
    _learning_rate: f64,
    _batch_size: usize,
    _epochs: usize,
}

impl LoRATrainer {
    pub fn new(
        adapter: LoRATrainingAdapter,
        _tokenizer: std::sync::Arc<tokenizers::Tokenizer>,
        learning_rate: f64,
        batch_size: usize,
        epochs: usize,
    ) -> Self {
        Self {
            _adapter: adapter,
            _learning_rate: learning_rate,
            _batch_size: batch_size,
            _epochs: epochs,
        }
    }

    pub fn train(&mut self, _examples: &ExampleBuffer) -> Result<Vec<TrainingStats>> {
        anyhow::bail!("LoRA training moved to Python (Phase 5)")
    }

    pub fn adapter(&self) -> Result<&LoRATrainingAdapter> {
        // Phase 4: LoRATrainer removed
        anyhow::bail!("LoRA training moved to Python (Phase 5)")
    }
}

/// Training coordinator (Phase 6: Implemented JSONL queue writer)
#[derive(Debug)]
pub struct TrainingCoordinator {
    buffer: std::sync::RwLock<ExampleBuffer>,
    threshold: usize,
    auto_train: bool,
    queue_path: std::path::PathBuf,
}

impl TrainingCoordinator {
    pub fn new(buffer_size: usize, threshold: usize, auto_train: bool) -> Self {
        // Training queue: ~/.finch/training_queue.jsonl
        let queue_path = dirs::home_dir()
            .expect("No home directory")
            .join(".finch")
            .join("training_queue.jsonl");

        Self {
            buffer: std::sync::RwLock::new(ExampleBuffer::new(buffer_size)),
            threshold,
            auto_train,
            queue_path,
        }
    }

    /// Add example to buffer, returns true if training threshold reached
    pub fn add_example(&self, example: WeightedExample) -> Result<bool> {
        let mut buffer = self.buffer.write()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        buffer.add(example);

        // Check if threshold reached
        let should_trigger = self.auto_train && buffer.len() >= self.threshold;

        Ok(should_trigger)
    }

    pub fn buffer(&self) -> Result<std::sync::RwLockReadGuard<'_, ExampleBuffer>> {
        self.buffer.read().map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))
    }

    pub fn should_train(&self) -> bool {
        if let Ok(buffer) = self.buffer.read() {
            self.auto_train && buffer.len() >= self.threshold
        } else {
            false
        }
    }

    /// Write buffered examples to JSONL queue file
    pub fn write_training_queue(&self) -> Result<usize> {
        let buffer = self.buffer.read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        if buffer.is_empty() {
            return Ok(0);
        }

        // Ensure directory exists
        if let Some(parent) = self.queue_path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create training queue directory")?;
        }

        // Append examples to JSONL file
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.queue_path)
            .context("Failed to open training queue file")?;

        let count = buffer.len();

        use std::io::Write;
        for example in buffer.examples() {
            let json = serde_json::to_string(example)
                .context("Failed to serialize example")?;
            writeln!(file, "{}", json)
                .context("Failed to write example to queue")?;
        }

        tracing::info!("Wrote {} examples to training queue: {}", count, self.queue_path.display());

        Ok(count)
    }

    /// Clear buffer after writing to queue
    pub fn clear_buffer(&self) -> Result<()> {
        let mut buffer = self.buffer.write()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        let count = buffer.len();
        *buffer = ExampleBuffer::new(0);

        tracing::debug!("Cleared {} examples from buffer", count);

        Ok(())
    }

    /// Get training queue path
    pub fn queue_path(&self) -> &std::path::Path {
        &self.queue_path
    }

    /// Placeholder for external training trigger
    pub fn train(&self) -> Result<()> {
        anyhow::bail!("Training moved to external Python scripts (Phase 6)")
    }
}

/// Training stats (stub for Phase 5)
#[derive(Debug, Clone)]
pub struct TrainingStats {
    pub total_examples: usize,
    pub loss: f64,
}

impl Default for TrainingStats {
    fn default() -> Self {
        Self::new()
    }
}

impl TrainingStats {
    pub fn new() -> Self {
        Self {
            total_examples: 0,
            loss: 0.0,
        }
    }
}
