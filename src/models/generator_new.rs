// Generator Model - Unified text generation interface
// Supports both custom transformers (random init) and pre-trained Qwen models

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use std::path::Path;

use super::common::{GeneratorConfig, Saveable};
use super::generator as legacy_generator;
use super::unified_loader::UnifiedModelLoader;

/// Text generation trait - abstraction over different generator backends
pub trait TextGeneration: Send + Sync {
    /// Generate text from input tokens
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>>;

    /// Get the device this model runs on
    fn device(&self) -> &Device;

    /// Get model name/description
    fn name(&self) -> &str;
}

/// Legacy custom transformer implementation
struct LegacyGenerator {
    inner: legacy_generator::GeneratorModel,
}

impl TextGeneration for LegacyGenerator {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        let input_tensor = Tensor::from_vec(
            input_ids.to_vec(),
            (1, input_ids.len()),
            self.inner.device(),
        )?;
        self.inner.generate(&input_tensor, max_new_tokens)
    }

    fn device(&self) -> &Device {
        self.inner.device()
    }

    fn name(&self) -> &str {
        "Custom Transformer (Random Init)"
    }
}

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
    pub fn new(config: GeneratorConfig) -> Result<Self> {
        let backend: Box<dyn TextGeneration> = match &config {
            GeneratorConfig::RandomInit(model_config) => {
                tracing::info!("Creating custom transformer with random initialization");
                let inner = legacy_generator::GeneratorModel::new(model_config)?;
                Box::new(LegacyGenerator { inner })
            }
            GeneratorConfig::Pretrained(load_config) => {
                tracing::info!(
                    "Loading pre-trained model: {} {} on {}",
                    load_config.family.name(),
                    load_config.size.to_size_string(load_config.family),
                    load_config.backend.name()
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

    /// Get generator backend name
    pub fn name(&self) -> &str {
        self.backend.name()
    }

    /// Get device
    pub fn device(&self) -> &Device {
        self.backend.device()
    }

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
    /// ```rust,ignore
    /// use shammah::models::{GeneratorModel, LoRAConfig};
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
    use crate::models::common::{DevicePreference, ModelConfig};

    #[test]
    fn test_generator_random_init() {
        let model_config = ModelConfig::small();
        let config = GeneratorConfig::RandomInit(model_config);

        let generator = GeneratorModel::new(config);
        assert!(generator.is_ok());

        let gen = generator.unwrap();
        assert_eq!(gen.name(), "Custom Transformer (Random Init)");
    }

    #[test]
    #[ignore] // Requires downloaded Qwen model
    fn test_generator_qwen() {
        use crate::models::unified_loader::{ModelLoadConfig, ModelFamily, ModelSize};
        use crate::config::backend::BackendDevice;

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            backend: BackendDevice::Cpu,
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
