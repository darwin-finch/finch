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
    /// Default implementation just calls regular generate (no streaming support).
    fn generate_stream(
        &mut self,
        input_ids: &[u32],
        max_new_tokens: usize,
        _token_callback: TokenCallback,
    ) -> Result<Vec<u32>> {
        self.generate(input_ids, max_new_tokens)
    }

    /// Encode a text prompt into token IDs
    fn tokenize(&self, text: &str) -> Result<Vec<u32>>;

    /// Decode token IDs back into a text string
    fn decode_tokens(&self, tokens: &[u32]) -> Result<String>;

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

    /// Generate a text response from a text prompt.
    ///
    /// Tokenizes the prompt, calls generate(), and decodes the result.
    /// Works with any backend (ONNX, Candle, etc.) via the TextGeneration trait.
    pub fn generate_text(&mut self, prompt: &str, max_new_tokens: usize) -> Result<String> {
        let input_ids = self.backend.tokenize(prompt)?;
        let output_ids = self.generate(&input_ids, max_new_tokens)?;
        self.backend.decode_tokens(&output_ids)
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

    // --- Regression: TextGeneration trait must expose tokenize + decode_tokens ---
    //
    // These tests use a mock backend to verify the trait contract without
    // downloading any real model. If you add methods to TextGeneration, add
    // corresponding assertions here.

    struct MockBackend;

    impl TextGeneration for MockBackend {
        fn generate(&mut self, input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
            // Echo back input for testing
            Ok(input_ids.to_vec())
        }

        fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
            // Simple mock: return byte values
            Ok(text.bytes().map(|b| b as u32).collect())
        }

        fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
            // Reverse of above mock
            let bytes: Vec<u8> = tokens.iter().map(|&t| t as u8).collect();
            String::from_utf8(bytes).map_err(|e| anyhow::anyhow!("{}", e))
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_text_generation_trait_has_tokenize_and_decode() {
        // Verify the trait methods exist and work correctly via a mock
        let backend = MockBackend;
        let ids = backend.tokenize("hi").unwrap();
        assert!(!ids.is_empty());
        let decoded = backend.decode_tokens(&ids).unwrap();
        assert_eq!(decoded, "hi");
    }

    #[test]
    fn test_generate_text_uses_trait_not_downcast() {
        // Regression: generate_text() must work via trait methods, not downcast to
        // LoadedOnnxModel. A non-ONNX backend should succeed here.
        use crate::models::unified_loader::ModelLoadConfig;
        use crate::models::GeneratorConfig;

        struct EchoBackend;
        impl TextGeneration for EchoBackend {
            fn generate(&mut self, _ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(vec![104, 105]) // "hi"
            }
            fn tokenize(&self, _text: &str) -> Result<Vec<u32>> {
                Ok(vec![104, 105])
            }
            fn decode_tokens(&self, _tokens: &[u32]) -> Result<String> {
                Ok("hi".into())
            }
            fn name(&self) -> &str {
                "echo"
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        // We can't call GeneratorModel::new() without a model file, but we can
        // construct one directly to test generate_text() routing.
        let mut gen = GeneratorModel {
            backend: Box::new(EchoBackend),
            config: GeneratorConfig::Pretrained(ModelLoadConfig {
                provider: crate::models::unified_loader::InferenceProvider::Onnx,
                family: crate::models::unified_loader::ModelFamily::Qwen2,
                size: crate::models::unified_loader::ModelSize::Small,
                target: crate::config::ExecutionTarget::Cpu,
                repo_override: None,
            }),
        };

        let result = gen.generate_text("test", 10).unwrap();
        assert_eq!(result, "hi");
    }

    #[test]
    fn test_random_init_config_returns_error() {
        // RandomInit config was removed in Phase 4 — must return a clear error.
        use crate::models::common::ModelConfig;
        let config = GeneratorConfig::RandomInit(ModelConfig::default());
        let result = GeneratorModel::new(config);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("RandomInit"));
    }

    #[test]
    #[ignore] // Requires downloaded Qwen model
    fn test_generator_qwen_onnx() {
        use crate::config::ExecutionTarget;
        use crate::models::unified_loader::{ModelFamily, ModelLoadConfig, ModelSize};

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: crate::models::unified_loader::InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            repo_override: None,
        });

        let gen = GeneratorModel::new(config).expect("Should load Qwen ONNX model");
        assert!(gen.name().contains("Qwen"));
    }
}
