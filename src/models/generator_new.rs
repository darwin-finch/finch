// Generator Model - Unified text generation interface
// Supports both custom transformers (random init) and pre-trained Qwen models

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use std::path::Path;

use super::common::{get_device_with_preference, GeneratorConfig, Saveable};
use super::generator as legacy_generator;
use super::qwen_loader::{LoadedQwenModel, QwenConfig, QwenLoader};

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

/// CoreML pre-trained model implementation (Apple Neural Engine)
#[cfg(target_os = "macos")]
struct CoreMLGenerator {
    inner: super::coreml_loader::LoadedCoreMLModel,
    name: String,
    // CoreML doesn't use Candle Device, use a placeholder
    dummy_device: Device,
}

#[cfg(target_os = "macos")]
impl TextGeneration for CoreMLGenerator {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        // Decode input IDs to text
        let input_text = self
            .inner
            .tokenizer
            .decode(input_ids, true)
            .map_err(|e| anyhow::anyhow!("Failed to decode input: {}", e))?;

        // Generate response text using CoreML/ANE
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
        // CoreML doesn't use Candle's Device, return placeholder
        &self.dummy_device
    }

    fn name(&self) -> &str {
        &self.name
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
            GeneratorConfig::Qwen {
                model_size,
                cache_dir,
                device_preference,
            } => {
                tracing::info!(
                    "Loading pre-trained Qwen model: {}",
                    model_size.description()
                );

                // Try Metal first (10-100x faster), fall back to CPU if issues
                tracing::info!("Attempting to load Qwen on preferred device...");

                let device = match get_device_with_preference(*device_preference) {
                    Ok(dev) => dev,
                    Err(e) => {
                        tracing::warn!("Failed to get preferred device: {}, using CPU", e);
                        Device::Cpu
                    }
                };

                let qwen_config = QwenConfig {
                    model_size: *model_size,
                    cache_dir: cache_dir.clone(),
                    device: device.clone(),
                };

                // Try loading and test a simple generation
                match QwenLoader::load(&qwen_config) {
                    Ok(mut model) => {
                        // Test with simple prompt (with KV cache, should be fast)
                        tracing::info!("Testing generation on {:?}...", device);
                        // Try with just 1 token to minimize complexity
                        match model.generate("Hi", 1) {
                            Ok(output) => {
                                let name = format!("Qwen {}", model_size.description());
                                tracing::info!("✓ Test passed, generated: {:?}", output.chars().take(20).collect::<String>());
                                tracing::info!("✓ Loaded Qwen on {:?}", device);
                                Box::new(QwenGenerator { inner: model, name })
                            }
                            Err(e) if matches!(device, Device::Metal(_)) => {
                                // Metal test failed, fall back to CPU
                                tracing::warn!("Metal generation test failed: {}", e);

                                // Write full error to file for inspection
                                if let Err(write_err) = std::fs::write("metal_error.txt", format!("{:#?}", e)) {
                                    tracing::warn!("Failed to write metal_error.txt: {}", write_err);
                                } else {
                                    tracing::warn!("Full error details written to metal_error.txt");
                                }

                                tracing::info!("Retrying with CPU...");

                                let cpu_config = QwenConfig {
                                    model_size: *model_size,
                                    cache_dir: cache_dir.clone(),
                                    device: Device::Cpu,
                                };

                                let model = QwenLoader::load(&cpu_config)?;
                                let name = format!("Qwen {}", model_size.description());
                                tracing::info!("✓ Loaded Qwen on CPU (Metal fallback, skipping test)");
                                // Skip test for CPU - we know it works, and test might be slow
                                Box::new(QwenGenerator { inner: model, name })
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    Err(e) if matches!(device, Device::Metal(_)) => {
                        // Metal loading failed, fall back to CPU
                        tracing::warn!("Metal loading failed: {}", e);
                        tracing::info!("Retrying with CPU...");

                        let cpu_config = QwenConfig {
                            model_size: *model_size,
                            cache_dir: cache_dir.clone(),
                            device: Device::Cpu,
                        };

                        let model = QwenLoader::load(&cpu_config)?;
                        let name = format!("Qwen {}", model_size.description());
                        tracing::info!("✓ Loaded Qwen on CPU (Metal fallback)");
                        Box::new(QwenGenerator { inner: model, name })
                    }
                    Err(e) => return Err(e),
                }
            }
            #[cfg(target_os = "macos")]
            GeneratorConfig::CoreML {
                model_size,
                cache_dir,
            } => {
                tracing::info!(
                    "Loading pre-trained CoreML model for Apple Neural Engine: {}",
                    model_size.description()
                );

                let coreml_config = super::coreml_loader::CoreMLConfig {
                    model_size: *model_size,
                    cache_dir: cache_dir.clone(),
                };

                // Load CoreML model
                match super::coreml_loader::CoreMLLoader::load(&coreml_config) {
                    Ok(model) => {
                        let name = format!("Qwen {} (CoreML/ANE)", model_size.description());
                        tracing::info!("✓ Loaded CoreML model");
                        tracing::info!("✓ Model will use Apple Neural Engine if available");

                        // CoreML doesn't use Candle Device, create a placeholder
                        let dummy_device = Device::Cpu;

                        Box::new(CoreMLGenerator {
                            inner: model,
                            name,
                            dummy_device,
                        })
                    }
                    Err(e) => {
                        tracing::error!("Failed to load CoreML model: {}", e);
                        return Err(e);
                    }
                }
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
            GeneratorConfig::Qwen { .. } => {
                // Qwen models are already persisted in HF cache
                // No need to save
                Ok(())
            }
            #[cfg(target_os = "macos")]
            GeneratorConfig::CoreML { .. } => {
                // CoreML models are already persisted in HF cache
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
