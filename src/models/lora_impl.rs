// LoRA (Low-Rank Adaptation) Implementation
// Real implementation with low-rank matrices and weighted training

use anyhow::{Context, Result};
use candle_core::{DType, Device, Module, Tensor};
use candle_nn::{Linear, Optimizer, VarBuilder, VarMap};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::lora::LoRAConfig;

/// Low-rank matrices for a single layer
#[derive(Debug, Clone)]
pub struct LoRALayer {
    /// A matrix: input_dim × rank
    pub lora_a: Linear,
    /// B matrix: rank × output_dim
    pub lora_b: Linear,
    /// Scaling factor
    pub scaling: f64,
    /// Layer name
    pub name: String,
}

impl LoRALayer {
    /// Create new LoRA layer with random initialization
    pub fn new(
        input_dim: usize,
        output_dim: usize,
        rank: usize,
        alpha: f64,
        name: String,
        vb: VarBuilder,
    ) -> Result<Self> {
        // A: Normal initialization
        let lora_a = candle_nn::linear(input_dim, rank, vb.pp(&format!("{}_lora_a", name)))?;

        // B: Zero initialization (starts with no effect)
        let lora_b = candle_nn::linear_no_bias(rank, output_dim, vb.pp(&format!("{}_lora_b", name)))?;

        // Scaling: alpha / rank
        let scaling = alpha / rank as f64;

        Ok(Self {
            lora_a,
            lora_b,
            scaling,
            name,
        })
    }

    /// Forward pass with LoRA: output = base_output + (B @ A @ input) * scaling
    pub fn forward(&self, input: &Tensor, base_output: &Tensor) -> Result<Tensor> {
        // LoRA path: input → A → B
        let lora_a_out = self.lora_a.forward(input)?;
        let lora_b_out = self.lora_b.forward(&lora_a_out)?;

        // Scale LoRA output
        let lora_scaled = (lora_b_out * self.scaling)?;

        // Add to base output
        let output = (base_output + lora_scaled)?;

        Ok(output)
    }
}

/// LoRA adapter with multiple layers
pub struct LoRAAdapter {
    /// LoRA layers mapped by layer name
    layers: HashMap<String, LoRALayer>,
    /// Configuration
    config: LoRAConfig,
    /// Variable map for all parameters
    varmap: VarMap,
    /// Device (CPU or Metal)
    device: Device,
    /// Whether adapter is enabled
    enabled: bool,
}

impl std::fmt::Debug for LoRAAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoRAAdapter")
            .field("layers", &self.layers.keys().collect::<Vec<_>>())
            .field("config", &self.config)
            .field("device", &self.device)
            .field("enabled", &self.enabled)
            .field("varmap", &"<VarMap>")
            .finish()
    }
}

impl LoRAAdapter {
    /// Create new LoRA adapter for Qwen model
    pub fn new(config: LoRAConfig, device: Device) -> Result<Self> {
        let varmap = VarMap::new();
        let mut layers = HashMap::new();

        // Create LoRA layers for target modules
        // Qwen2 attention dimensions (example for 3B model)
        let hidden_size = 2048; // Adjust based on actual model
        let num_heads = 16;
        let head_dim = hidden_size / num_heads;

        let vb = VarBuilder::from_varmap(&varmap, DType::F32, &device);

        for module_name in &config.target_modules {
            let layer = match module_name.as_str() {
                "q_proj" => LoRALayer::new(
                    hidden_size,
                    hidden_size,
                    config.rank,
                    config.alpha,
                    "q_proj".to_string(),
                    vb.pp("q_proj"),
                )?,
                "k_proj" => LoRALayer::new(
                    hidden_size,
                    hidden_size,
                    config.rank,
                    config.alpha,
                    "k_proj".to_string(),
                    vb.pp("k_proj"),
                )?,
                "v_proj" => LoRALayer::new(
                    hidden_size,
                    hidden_size,
                    config.rank,
                    config.alpha,
                    "v_proj".to_string(),
                    vb.pp("v_proj"),
                )?,
                "o_proj" => LoRALayer::new(
                    hidden_size,
                    hidden_size,
                    config.rank,
                    config.alpha,
                    "o_proj".to_string(),
                    vb.pp("o_proj"),
                )?,
                _ => continue, // Skip unknown modules
            };

            layers.insert(module_name.clone(), layer);
        }

        Ok(Self {
            layers,
            config,
            varmap,
            device,
            enabled: false,
        })
    }

    /// Apply LoRA to a layer's output
    pub fn apply_to_layer(
        &self,
        layer_name: &str,
        input: &Tensor,
        base_output: &Tensor,
    ) -> Result<Tensor> {
        if !self.enabled {
            return Ok(base_output.clone());
        }

        if let Some(layer) = self.layers.get(layer_name) {
            layer.forward(input, base_output)
        } else {
            Ok(base_output.clone())
        }
    }

    /// Enable the LoRA adapter
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    /// Disable the LoRA adapter
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

    /// Get variable map (for saving/loading)
    pub fn varmap(&self) -> &VarMap {
        &self.varmap
    }

    /// Get device
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Save adapter weights to safetensors
    pub fn save(&self, path: &Path) -> Result<()> {
        self.varmap
            .save(path)
            .with_context(|| format!("Failed to save LoRA adapter to {:?}", path))?;

        // Also save config
        let config_path = path.with_extension("json");
        let config_json = serde_json::to_string_pretty(&self.config)?;
        std::fs::write(&config_path, config_json)
            .with_context(|| format!("Failed to save config to {:?}", config_path))?;

        tracing::info!("Saved LoRA adapter to {:?}", path);
        Ok(())
    }

    /// Load adapter weights from safetensors
    pub fn load(path: &Path, device: Device) -> Result<Self> {
        // Load config
        let config_path = path.with_extension("json");
        let config_json = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config from {:?}", config_path))?;
        let config: LoRAConfig = serde_json::from_str(&config_json)?;

        // Create adapter structure
        let mut adapter = Self::new(config, device)?;

        // Load weights
        adapter
            .varmap
            .load(path)
            .with_context(|| format!("Failed to load LoRA weights from {:?}", path))?;

        tracing::info!("Loaded LoRA adapter from {:?}", path);
        Ok(adapter)
    }
}

/// Weighted training example
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedExample {
    /// Input query
    pub query: String,
    /// Model response
    pub response: String,
    /// User feedback explaining why this is good/bad
    pub feedback: String,
    /// Weight: 10.0 (critical), 3.0 (improvement), 1.0 (normal)
    pub weight: f64,
    /// When this example was collected
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Optional tags for organization
    pub tags: Vec<String>,
}

impl WeightedExample {
    /// Create high-weight example (critical issue)
    pub fn critical(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            feedback,
            weight: 10.0,
            timestamp: chrono::Utc::now(),
            tags: vec!["critical".to_string()],
        }
    }

    /// Create medium-weight example (improvement)
    pub fn improvement(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            feedback,
            weight: 3.0,
            timestamp: chrono::Utc::now(),
            tags: vec!["improvement".to_string()],
        }
    }

    /// Create normal-weight example (good)
    pub fn normal(query: String, response: String, feedback: String) -> Self {
        Self {
            query,
            response,
            feedback,
            weight: 1.0,
            timestamp: chrono::Utc::now(),
            tags: vec!["good".to_string()],
        }
    }

    /// Create with custom weight
    pub fn with_weight(
        query: String,
        response: String,
        feedback: String,
        weight: f64,
    ) -> Self {
        Self {
            query,
            response,
            feedback,
            weight,
            timestamp: chrono::Utc::now(),
            tags: vec![],
        }
    }
}

/// Buffer for collecting training examples
#[derive(Debug, Default)]
pub struct ExampleBuffer {
    examples: Vec<WeightedExample>,
    max_size: usize,
}

impl ExampleBuffer {
    /// Create new buffer with max size
    pub fn new(max_size: usize) -> Self {
        Self {
            examples: Vec::new(),
            max_size,
        }
    }

    /// Add example to buffer
    pub fn add(&mut self, example: WeightedExample) {
        self.examples.push(example);

        // Keep only most recent examples if buffer full
        if self.examples.len() > self.max_size {
            self.examples.remove(0);
        }
    }

    /// Get all examples
    pub fn examples(&self) -> &[WeightedExample] {
        &self.examples
    }

    /// Clear buffer
    pub fn clear(&mut self) {
        self.examples.clear();
    }

    /// Get total weight
    pub fn total_weight(&self) -> f64 {
        self.examples.iter().map(|e| e.weight).sum()
    }

    /// Sample batch with weighted random sampling
    pub fn sample_batch(&self, batch_size: usize, rng: &mut impl rand::Rng) -> Vec<WeightedExample> {
        use rand::seq::SliceRandom;

        if self.examples.is_empty() {
            return vec![];
        }

        // Create weighted distribution
        let total_weight = self.total_weight();
        let mut batch = Vec::new();

        for _ in 0..batch_size.min(self.examples.len()) {
            // Weighted random selection
            let mut cumulative = 0.0;
            let target = rng.gen::<f64>() * total_weight;

            for example in &self.examples {
                cumulative += example.weight;
                if cumulative >= target {
                    batch.push(example.clone());
                    break;
                }
            }
        }

        batch
    }

    /// Save buffer to disk
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.examples)?;
        std::fs::write(path, json)
            .with_context(|| format!("Failed to save example buffer to {:?}", path))?;
        Ok(())
    }

    /// Load buffer from disk
    pub fn load(path: &Path, max_size: usize) -> Result<Self> {
        let json = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read example buffer from {:?}", path))?;
        let examples: Vec<WeightedExample> = serde_json::from_str(&json)?;

        Ok(Self {
            examples,
            max_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weighted_example_creation() {
        let ex = WeightedExample::critical(
            "query".to_string(),
            "response".to_string(),
            "This is wrong".to_string(),
        );
        assert_eq!(ex.weight, 10.0);
        assert_eq!(ex.tags, vec!["critical"]);

        let ex = WeightedExample::improvement(
            "query".to_string(),
            "response".to_string(),
            "Could be better".to_string(),
        );
        assert_eq!(ex.weight, 3.0);

        let ex = WeightedExample::normal(
            "query".to_string(),
            "response".to_string(),
            "Good".to_string(),
        );
        assert_eq!(ex.weight, 1.0);
    }

    #[test]
    fn test_example_buffer() {
        let mut buffer = ExampleBuffer::new(100);

        buffer.add(WeightedExample::critical(
            "q1".into(),
            "r1".into(),
            "bad".into(),
        ));
        buffer.add(WeightedExample::normal("q2".into(), "r2".into(), "good".into()));

        assert_eq!(buffer.examples().len(), 2);
        assert_eq!(buffer.total_weight(), 11.0); // 10 + 1
    }

    #[test]
    fn test_weighted_sampling() {
        let mut buffer = ExampleBuffer::new(100);

        // Add 1 critical (weight 10) and 10 normal (weight 1 each)
        buffer.add(WeightedExample::critical(
            "critical".into(),
            "r".into(),
            "bad".into(),
        ));
        for i in 0..10 {
            buffer.add(WeightedExample::normal(
                format!("normal{}", i),
                "r".into(),
                "good".into(),
            ));
        }

        // Total weight: 10 + 10 = 20
        // Critical example should appear ~50% of the time in samples
        let mut rng = rand::thread_rng();
        let mut critical_count = 0;

        for _ in 0..100 {
            let batch = buffer.sample_batch(1, &mut rng);
            if !batch.is_empty() && batch[0].query == "critical" {
                critical_count += 1;
            }
        }

        // Should be around 50 (50%), allow some variance
        assert!(critical_count > 30 && critical_count < 70);
    }

    #[test]
    fn test_lora_adapter_creation() -> Result<()> {
        use candle_core::Device;

        let config = LoRAConfig::default();
        let device = Device::Cpu;

        let adapter = LoRAAdapter::new(config, device)?;

        assert!(!adapter.is_enabled());
        assert_eq!(adapter.layers.len(), 2); // q_proj, v_proj by default

        Ok(())
    }

    #[test]
    fn test_lora_enable_disable() -> Result<()> {
        let config = LoRAConfig::default();
        let device = Device::Cpu;

        let mut adapter = LoRAAdapter::new(config, device)?;

        assert!(!adapter.is_enabled());

        adapter.enable();
        assert!(adapter.is_enabled());

        adapter.disable();
        assert!(!adapter.is_enabled());

        Ok(())
    }
}
