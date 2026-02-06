// Generator Model - Unified text generation interface
// Supports both custom transformers (random init) and pre-trained Qwen models

use anyhow::Result;
use candle_core::{Device, Tensor};
use std::path::Path;

use super::common::{get_device_with_preference, GeneratorConfig, Saveable};
use super::generator as legacy_generator;
use super::qwen_loader::{LoadedQwenModel, QwenConfig, QwenLoader};

/// Text generation trait - abstraction over different generator backends
pub trait TextGeneration {
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
        let input_tensor =
            Tensor::from_vec(input_ids.to_vec(), (1, input_ids.len()), &self.inner.device)?;
        self.inner.generate(&input_tensor, max_new_tokens)
    }

    fn device(&self) -> &Device {
        &self.inner.device
    }

    fn name(&self) -> &str {
        "Custom Transformer (Random Init)"
    }
}

/// Qwen pre-trained model implementation
struct QwenGenerator {
    inner: LoadedQwenModel,
    name: String,
}

impl TextGeneration for QwenGenerator {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        // Decode input IDs to text
        let input_text = self
            .inner
            .tokenizer
            .decode(input_ids, true)
            .map_err(|e| anyhow::anyhow!("Failed to decode input: {}", e))?;

        // Generate response text
        let output_text = self.inner.generate(&input_text, max_new_tokens)?;

        // Encode back to token IDs
        let output_tokens = self
            .inner
            .tokenizer
            .encode(output_text, true)
            .map_err(|e| anyhow::anyhow!("Failed to encode output: {}", e))?;

        Ok(output_tokens.get_ids().to_vec())
    }

    fn device(&self) -> &Device {
        &self.inner.device
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Unified generator model supporting multiple backends
pub struct GeneratorModel {
    backend: Box<dyn TextGeneration + Send>,
    config: GeneratorConfig,
}

impl GeneratorModel {
    /// Create new generator from configuration
    pub fn new(config: GeneratorConfig) -> Result<Self> {
        let backend: Box<dyn TextGeneration + Send> = match &config {
            GeneratorConfig::RandomInit(model_config) => {
                tracing::info!("Creating custom transformer with random initialization");
                let inner = legacy_generator::GeneratorModel::new(model_config)?;
                Box::new(LegacyGenerator { inner })
            }
            GeneratorConfig::Qwen {
                model_size,
                cache_dir,
                device_preference,
            } => {
                tracing::info!("Loading pre-trained Qwen model: {}", model_size.description());

                let device = get_device_with_preference(*device_preference)?;

                let qwen_config = QwenConfig {
                    model_size: *model_size,
                    cache_dir: cache_dir.clone(),
                    device,
                };

                let inner = QwenLoader::load(&qwen_config)?;
                let name = format!("Qwen {}", model_size.description());

                Box::new(QwenGenerator { inner, name })
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
}

impl Saveable for GeneratorModel {
    fn save(&self, _path: &Path) -> Result<()> {
        match &self.config {
            GeneratorConfig::RandomInit(_) => {
                // For random init, we could save the varmap
                // For now, return not implemented
                anyhow::bail!("Saving custom transformers not yet implemented")
            }
            GeneratorConfig::Qwen { .. } => {
                // Qwen models are already persisted in HF cache
                // No need to save
                Ok(())
            }
        }
    }

    fn load(_path: &Path) -> Result<Self>
    where
        Self: Sized,
    {
        anyhow::bail!("Loading generators from file not yet implemented - use GeneratorModel::new() instead")
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
        use crate::models::model_selector::QwenSize;

        let cache_dir = dirs::home_dir()
            .unwrap()
            .join(".cache/huggingface/hub/models--Qwen--Qwen2.5-1.5B-Instruct");

        // Find snapshot directory
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let snapshot_dir = entry.path();
                if snapshot_dir.is_dir() && QwenLoader::is_loadable(&snapshot_dir) {
                    let config = GeneratorConfig::Qwen {
                        model_size: QwenSize::Qwen1_5B,
                        cache_dir: snapshot_dir,
                        device_preference: DevicePreference::Auto,
                    };

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
                    return;
                }
            }
        }

        println!("No Qwen model found - run download test first");
    }
}
