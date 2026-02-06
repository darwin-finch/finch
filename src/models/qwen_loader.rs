// Qwen Loader - Load pre-trained Qwen models from HuggingFace
// Supports Qwen-2.5-1.5B/3B/7B/14B-Instruct variants

use anyhow::{Context, Result};
use candle_core::{Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::qwen2::{Config as Qwen2Config, ModelForCausalLM as Qwen2Model};
use std::path::Path;
use tokenizers::Tokenizer;

use super::model_selector::QwenSize;

/// Configuration for Qwen model loading
#[derive(Debug, Clone)]
pub struct QwenConfig {
    pub model_size: QwenSize,
    pub cache_dir: std::path::PathBuf,
    pub device: Device,
}

/// Loaded Qwen model with tokenizer
pub struct LoadedQwenModel {
    pub model: Qwen2Model,
    pub tokenizer: Tokenizer,
    pub config: Qwen2Config,
    pub device: Device,
}

impl LoadedQwenModel {
    /// Generate text from input prompt
    pub fn generate(&mut self, prompt: &str, max_tokens: usize) -> Result<String> {
        // Tokenize input
        let tokens = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let input_ids = tokens.get_ids();
        let input_tensor =
            Tensor::new(input_ids, &self.device)?.unsqueeze(0)?; // Add batch dimension

        // Generate tokens autoregressively
        let mut generated_ids = input_ids.to_vec();

        for _ in 0..max_tokens {
            // Forward pass
            let logits = self
                .model
                .forward(&input_tensor, generated_ids.len() - 1)?;

            // Get logits for last token
            let last_logits = logits.i((0, generated_ids.len() - 1))?;

            // Sample next token (greedy for now)
            let next_token = last_logits.argmax(0)?.to_scalar::<u32>()?;

            // Check for EOS token
            if next_token == self.tokenizer.token_to_id("</s>").unwrap_or(2) {
                break;
            }

            generated_ids.push(next_token);

            // Stop if max position embeddings reached
            if generated_ids.len() >= self.config.max_position_embeddings {
                break;
            }
        }

        // Decode generated tokens
        let output = self
            .tokenizer
            .decode(&generated_ids, true)
            .map_err(|e| anyhow::anyhow!("Decoding failed: {}", e))?;

        Ok(output)
    }
}

/// Qwen model loader
pub struct QwenLoader;

impl QwenLoader {
    /// Load Qwen model from cache directory
    ///
    /// Expects directory structure:
    /// ```
    /// cache_dir/
    ///   ├── config.json          (model config)
    ///   ├── tokenizer.json       (tokenizer)
    ///   └── model.safetensors    (weights)
    /// ```
    pub fn load(config: &QwenConfig) -> Result<LoadedQwenModel> {
        tracing::info!(
            "Loading {} from {:?}",
            config.model_size.description(),
            config.cache_dir
        );

        // 1. Load model configuration
        let config_path = config.cache_dir.join("config.json");
        let qwen_config: Qwen2Config = serde_json::from_reader(
            std::fs::File::open(&config_path)
                .with_context(|| format!("Failed to open config at {:?}", config_path))?,
        )
        .context("Failed to parse Qwen2 config.json")?;

        tracing::debug!("Loaded config: vocab_size={}, hidden_size={}, num_layers={}",
            qwen_config.vocab_size,
            qwen_config.hidden_size,
            qwen_config.num_hidden_layers
        );

        // 2. Load tokenizer
        let tokenizer_path = config.cache_dir.join("tokenizer.json");
        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer from {:?}: {}", tokenizer_path, e))?;

        tracing::debug!("Loaded tokenizer with vocab size: {}", tokenizer.get_vocab_size(true));

        // 3. Load model weights from safetensors
        let weights_path = Self::find_safetensors_file(&config.cache_dir)?;
        tracing::info!("Loading weights from {:?}", weights_path);

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[weights_path],
                candle_core::DType::F32,
                &config.device,
            )?
        };

        // 4. Build Qwen model architecture with loaded weights
        let model = Qwen2Model::new(&qwen_config, vb)
            .context("Failed to build Qwen2 model from weights")?;

        tracing::info!("Successfully loaded {}", config.model_size.description());

        Ok(LoadedQwenModel {
            model,
            tokenizer,
            config: qwen_config,
            device: config.device.clone(),
        })
    }

    /// Find safetensors file in cache directory
    ///
    /// Handles both single file (model.safetensors) and sharded files
    /// (model-00001-of-00002.safetensors, etc.)
    fn find_safetensors_file(cache_dir: &Path) -> Result<std::path::PathBuf> {
        // Try single file first
        let single_file = cache_dir.join("model.safetensors");
        if single_file.exists() {
            return Ok(single_file);
        }

        // Try sharded files (use first shard)
        for entry in std::fs::read_dir(cache_dir)
            .context("Failed to read cache directory")?
        {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            if let Some(filename) = path.file_name() {
                if let Some(name) = filename.to_str() {
                    if name.starts_with("model-") && name.ends_with(".safetensors") {
                        // For sharded models, we need all shards
                        // For now, just return error - will handle sharding later
                        anyhow::bail!(
                            "Sharded model detected ({}) - not yet supported. \
                             Please use a model with single safetensors file.",
                            name
                        );
                    }
                }
            }
        }

        Err(anyhow::anyhow!(
            "No safetensors file found in {:?}. Expected model.safetensors",
            cache_dir
        ))
    }

    /// Check if model is loadable from cache directory
    pub fn is_loadable(cache_dir: &Path) -> bool {
        cache_dir.join("config.json").exists()
            && cache_dir.join("tokenizer.json").exists()
            && (cache_dir.join("model.safetensors").exists()
                || Self::has_sharded_safetensors(cache_dir))
    }

    /// Check if directory contains sharded safetensors files
    fn has_sharded_safetensors(cache_dir: &Path) -> bool {
        if let Ok(entries) = std::fs::read_dir(cache_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if name.starts_with("model-") && name.ends_with(".safetensors") {
                        return true;
                    }
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::common::get_device;

    #[test]
    fn test_qwen_config_creation() {
        let device = get_device().unwrap();
        let config = QwenConfig {
            model_size: QwenSize::Qwen1_5B,
            cache_dir: std::path::PathBuf::from("/tmp/test"),
            device,
        };

        assert_eq!(config.model_size, QwenSize::Qwen1_5B);
    }

    #[test]
    fn test_is_loadable_missing_files() {
        let temp_dir = std::env::temp_dir().join("test_qwen_missing");
        std::fs::create_dir_all(&temp_dir).ok();

        // Should return false when files are missing
        assert!(!QwenLoader::is_loadable(&temp_dir));

        // Cleanup
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    #[ignore] // Requires actual downloaded model
    fn test_load_qwen_model() {
        // This test requires a real Qwen model to be downloaded
        // Run with: cargo test test_load_qwen_model -- --ignored

        let device = get_device().unwrap();
        let cache_dir = dirs::home_dir()
            .unwrap()
            .join(".cache/huggingface/hub/models--Qwen--Qwen2.5-1.5B-Instruct/snapshots");

        // Find the latest snapshot
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let snapshot_dir = entry.path();
                if snapshot_dir.is_dir() && QwenLoader::is_loadable(&snapshot_dir) {
                    let config = QwenConfig {
                        model_size: QwenSize::Qwen1_5B,
                        cache_dir: snapshot_dir.clone(),
                        device,
                    };

                    let result = QwenLoader::load(&config);
                    match result {
                        Ok(mut model) => {
                            println!("Successfully loaded model from {:?}", snapshot_dir);

                            // Try generating text
                            let prompt = "Hello, world!";
                            let output = model.generate(prompt, 20);
                            match output {
                                Ok(text) => println!("Generated: {}", text),
                                Err(e) => println!("Generation error: {}", e),
                            }
                        }
                        Err(e) => {
                            println!("Failed to load model: {}", e);
                        }
                    }
                    return;
                }
            }
        }

        println!("No Qwen model found in cache - run download test first");
    }
}
