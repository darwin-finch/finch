// Generator Model - Unified text generation interface
// Phase 4: ONNX-based (Candle removed)

use anyhow::Result;
use std::path::Path;

use super::common::{GeneratorConfig, Saveable};
use super::unified_loader::UnifiedModelLoader;

/// Text generation trait - abstraction over different generator backends
/// Callback type for streaming generation
pub type TokenCallback = Box<dyn FnMut(u32, &str) + Send>;

pub trait TextGeneration: Send + Sync {
    /// Generate text from input tokens
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>>;

    /// Generate text with token-by-token callback for streaming
    ///
    /// The callback receives each generated token ID and its decoded text.
    /// Default implementation just calls regular generate (no streaming support).
    fn generate_stream(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        _token_callback: TokenCallback,
    ) -> Result<Vec<u32>> {
        // Default implementation: just call regular generate (no streaming)
        self.generate(input_ids, max_new_tokens)
    }

    /// Get model name/description
    fn name(&self) -> &str;

    /// Downcast to Any for accessing concrete type methods
    fn as_any(&self) -> &dyn std::any::Any;

    /// Downcast to Any (mutable) for accessing concrete type methods
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

// Phase 4: LegacyGenerator removed (depends on Candle-based generator module)

/// Unified generator model supporting multiple backends
pub struct GeneratorModel {
    backend: Box<dyn TextGeneration>,
    config: GeneratorConfig,
}

impl std::fmt::Debug for GeneratorModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratorModel")
            .field("name", &self.backend.name())
            .field("config", &"<config>")
            .finish()
    }
}

impl GeneratorModel {
    /// Create new generator from configuration
    ///
    /// Phase 4: Only supports Pretrained (ONNX-based)
    /// RandomInit removed with Candle
    pub fn new(config: GeneratorConfig) -> Result<Self> {
        let backend: Box<dyn TextGeneration> = match &config {
            GeneratorConfig::RandomInit(_model_config) => {
                anyhow::bail!(
                    "RandomInit removed in Phase 4 (Candle-based).\n\
                     Use GeneratorConfig::Pretrained with ONNX models."
                )
            }
            GeneratorConfig::Pretrained(load_config) => {
                tracing::info!(
                    "Loading pre-trained model: {} {} on {}",
                    load_config.family.name(),
                    load_config.size.to_size_string(load_config.family),
                    load_config.target.name()
                );

                let loader = UnifiedModelLoader::new()?;
                loader.load(load_config.clone())?
            }
        };

        Ok(Self { backend, config })
    }

    /// Generate response from input tokens
    pub fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        self.backend.generate(input_ids, max_new_tokens)
    }

    /// Generate text response from text input (handles tokenization internally)
    ///
    /// This is a convenience method that:
    /// 1. Tokenizes the input text
    /// 2. Calls generate() on the backend
    /// 3. Detokenizes the output
    ///
    /// For ONNX models, this uses the model's built-in tokenizer.
    pub fn generate_text(&mut self, prompt: &str, max_new_tokens: usize) -> Result<String> {
        // Downcast to LoadedOnnxModel to access tokenizer
        // This is safe because we only support ONNX models in Phase 5
        use super::loaders::onnx::LoadedOnnxModel;

        // Tokenize input (scope the borrow)
        let input_ids: Vec<u32> = {
            let onnx_model = self.backend
                .as_any()
                .downcast_ref::<LoadedOnnxModel>()
                .ok_or_else(|| anyhow::anyhow!("Backend is not an ONNX model"))?;

            let encoding = onnx_model.tokenizer()
                .encode(prompt, true)
                .map_err(|e| anyhow::anyhow!("Failed to encode prompt: {}", e))?;

            encoding.get_ids().to_vec()
        }; // onnx_model borrow ends here

        // Generate tokens (requires mutable borrow of self)
        let output_ids = self.generate(&input_ids, max_new_tokens)?;

        // Decode output (scope the borrow again)
        let response = {
            let onnx_model = self.backend
                .as_any()
                .downcast_ref::<LoadedOnnxModel>()
                .ok_or_else(|| anyhow::anyhow!("Backend is not an ONNX model"))?;

            onnx_model.tokenizer()
                .decode(&output_ids, true)
                .map_err(|e| anyhow::anyhow!("Failed to decode output: {}", e))?
        }; // onnx_model borrow ends here

        Ok(response)
    }

    /// Get generator backend name
    pub fn name(&self) -> &str {
        self.backend.name()
    }

    /// Get mutable reference to backend (for accessing ONNX model directly)
    pub fn backend_mut(&mut self) -> &mut dyn TextGeneration {
        self.backend.as_mut()
    }

    // Phase 4: device() removed (Candle-based)
    // ONNX Runtime manages device selection via execution providers

    /// Get configuration
    pub fn config(&self) -> &GeneratorConfig {
        &self.config
    }

    /// Fine-tune model with LoRA adapter (placeholder for future functionality)
    ///
    /// # Arguments
    /// * `examples` - Training data as (query, response) pairs
    /// * `lora_config` - LoRA configuration (rank, alpha, target modules)
    /// * `epochs` - Number of training epochs
    /// * `learning_rate` - Learning rate for optimization
    ///
    /// # Example (Future Usage)
    /// ```text
    /// use finch::models::{GeneratorModel, LoRAConfig};
    ///
    /// let mut generator = GeneratorModel::new(config)?;
    ///
    /// let examples = vec![
    ///     ("What is Rust?".into(), "Rust is a systems programming language...".into()),
    ///     ("Explain ownership".into(), "Ownership is Rust's most unique feature...".into()),
    /// ];
    ///
    /// let lora_config = LoRAConfig::default();
    /// generator.fine_tune(&examples, lora_config, 3, 1e-4)?;
    /// ```
    ///
    /// # Returns
    /// Error with message "Not yet implemented"
    pub fn fine_tune(
        &mut self,
        _examples: &[(String, String)],
        _lora_config: crate::models::lora::LoRAConfig,
        _epochs: usize,
        _learning_rate: f64,
    ) -> Result<()> {
        anyhow::bail!(
            "LoRA fine-tuning not yet implemented. This is a placeholder for future functionality.\n\
             \n\
             To use fine-tuning in the future:\n\
             1. Prepare training examples (query, response pairs)\n\
             2. Configure LoRA parameters (rank, alpha, target modules)\n\
             3. Call fine_tune() with your data\n\
             4. Save adapted model with save_lora()\n\
             \n\
             See src/models/lora.rs for detailed documentation."
        )
    }

    /// Save LoRA adapter weights (placeholder)
    pub fn save_lora(&self, _path: &Path) -> Result<()> {
        anyhow::bail!("LoRA adapter saving not yet implemented")
    }

    /// Load LoRA adapter weights (placeholder)
    pub fn load_lora(&mut self, _path: &Path) -> Result<()> {
        anyhow::bail!("LoRA adapter loading not yet implemented")
    }
}

impl Saveable for GeneratorModel {
    fn save(&self, _path: &Path) -> Result<()> {
        match &self.config {
            GeneratorConfig::RandomInit(_) => {
                // For random init, we could save the varmap
                // For now, return not implemented
                anyhow::bail!("Saving custom transformers not yet implemented")
            }
            GeneratorConfig::Pretrained(_) => {
                // Pre-trained models are already persisted in HF cache
                // No need to save
                Ok(())
            }
        }
    }

    fn load(_path: &Path) -> Result<Self>
    where
        Self: Sized,
    {
        anyhow::bail!(
            "Loading generators from file not yet implemented - use GeneratorModel::new() instead"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Requires downloaded Qwen model
    fn test_generator_qwen() {
        use crate::models::unified_loader::{ModelLoadConfig, ModelFamily, ModelSize};
        use crate::config::ExecutionTarget;

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: crate::models::unified_loader::InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            repo_override: None,
        });

        let generator = GeneratorModel::new(config);
        match generator {
            Ok(gen) => {
                println!("Created generator: {}", gen.name());
                assert!(gen.name().contains("Qwen"));
            }
            Err(e) => {
                println!("Failed to create generator: {}", e);
            }
        }
    }
}
