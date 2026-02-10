// CoreML Loader - Qwen models optimized for Apple Neural Engine
// Uses .mlpackage format from anemll organization

use anyhow::{Context, Result};
use candle_core::Device;
use std::path::Path;
use tokenizers::Tokenizer;

use crate::models::unified_loader::ModelSize;
use crate::models::TextGeneration;

/// CoreML generator with tokenizer bridge
///
/// CoreML uses text-based API (complete_text), but we need token-based API
/// for consistency with other generators. This struct bridges the two.
pub struct CoreMLGenerator {
    model: candle_coreml::qwen::QwenModel,
    tokenizer: Tokenizer,
    name: String,
    // CoreML doesn't use Candle Device, use placeholder
    dummy_device: Device,
}

// SAFETY: CoreML models are used in single-threaded context within the generator
// The model is wrapped at a higher level with appropriate synchronization (Arc<RwLock>)
unsafe impl Send for CoreMLGenerator {}
unsafe impl Sync for CoreMLGenerator {}

impl TextGeneration for CoreMLGenerator {
    fn generate(&mut self, input_ids: &[u32], max_new_tokens: usize) -> Result<Vec<u32>> {
        // 1. Decode input token IDs to text
        let input_text = self
            .tokenizer
            .decode(input_ids, true)
            .map_err(|e| anyhow::anyhow!("Failed to decode input: {}", e))?;

        tracing::debug!(
            "CoreML generation: {} input tokens -> text: {}",
            input_ids.len(),
            input_text.chars().take(50).collect::<String>()
        );

        // 2. Use CoreML's text-based generation (runs on ANE)
        // Note: CoreML provides its own generation implementation
        const DEFAULT_TEMPERATURE: f64 = 0.8;

        // For now, we'll use a simple stub. The actual implementation would call
        // the CoreML model's generation method when it's properly wired up.
        // TODO: Wire up actual CoreML generation once candle_coreml::qwen::QwenModel
        // provides a high-level generation API.

        tracing::warn!("CoreML generation stub - using tokenizer round-trip");

        // Stub: Just echo the input for now
        let output_text = format!("{} [CoreML response]", input_text);

        // 3. Encode output text back to token IDs
        let output_tokens = self
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

/// Load Qwen CoreML model from cache directory
///
/// # Arguments
/// * `model_path` - Directory containing model.mlpackage, config.json, tokenizer.json
/// * `size` - Model size variant (Small=1.5B, Medium=3B, Large=7B, XLarge=14B)
///
/// # Returns
/// Boxed TextGeneration implementation that uses Apple Neural Engine
pub fn load(model_path: &Path, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
    let size_str = size.to_size_string(crate::models::unified_loader::ModelFamily::Qwen2);
    tracing::info!(
        "Loading Qwen {} CoreML model from {:?}",
        size_str,
        model_path
    );

    // 1. Check for required files
    let mlpackage_path = model_path.join("model.mlpackage");
    if !mlpackage_path.exists() {
        return Err(anyhow::anyhow!(
            "model.mlpackage not found in {:?}\n\
             \n\
             CoreML models require pre-converted .mlpackage format.\n\
             Expected from anemll organization (e.g., anemll/Qwen2.5-3B-Instruct).",
            model_path
        ));
    }

    // 2. Load tokenizer (needed for token ↔ text conversion)
    let tokenizer_path = model_path.join("tokenizer.json");
    let tokenizer = Tokenizer::from_file(&tokenizer_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to load tokenizer from {:?}: {}",
            tokenizer_path,
            e
        )
    })?;

    tracing::debug!(
        "Loaded tokenizer with vocab size: {}",
        tokenizer.get_vocab_size(true)
    );

    // 3. Load CoreML model using candle-coreml's API
    tracing::info!("Loading CoreML model from {:?}", mlpackage_path);

    // Use candle_coreml::qwen::QwenModel::load_from_directory
    // Pass None to use default config
    let model = candle_coreml::qwen::QwenModel::load_from_directory(model_path, None)
        .context("Failed to load CoreML model")?;

    tracing::info!("✓ Loaded Qwen {} on CoreML/ANE", size_str);
    tracing::info!("✓ Model will use Apple Neural Engine if available");

    let name = format!("Qwen 2.5 {} (CoreML/ANE)", size_str);
    let dummy_device = Device::Cpu; // Placeholder

    Ok(Box::new(CoreMLGenerator {
        model,
        tokenizer,
        name,
        dummy_device,
    }))
}

/// Check if CoreML model is loadable from cache directory
pub fn is_loadable(cache_dir: &Path) -> bool {
    let has_mlpackage = cache_dir.join("model.mlpackage").exists();
    let has_tokenizer = cache_dir.join("tokenizer.json").exists();
    has_mlpackage && has_tokenizer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_loadable_missing_files() {
        let temp_dir = std::env::temp_dir().join("test_coreml_missing");
        std::fs::create_dir_all(&temp_dir).ok();

        // Should return false when files are missing
        assert!(!is_loadable(&temp_dir));

        // Cleanup
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    #[ignore] // Requires actual CoreML model
    fn test_load_coreml_model() {
        let cache_dir = dirs::home_dir()
            .unwrap()
            .join(".cache/huggingface/hub/models--anemll--Qwen2.5-1.5B-Instruct/snapshots");

        // Find the latest snapshot
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let snapshot_dir = entry.path();
                if snapshot_dir.is_dir() && is_loadable(&snapshot_dir) {
                    let result = load(&snapshot_dir, ModelSize::Small);
                    match result {
                        Ok(mut generator) => {
                            println!("Successfully loaded CoreML model from {:?}", snapshot_dir);

                            // Try generating tokens
                            let input_ids = vec![1, 2, 3]; // Dummy token IDs
                            let output = generator.generate(&input_ids, 5);
                            match output {
                                Ok(tokens) => println!("Generated {} tokens", tokens.len()),
                                Err(e) => println!("Generation error: {}", e),
                            }
                        }
                        Err(e) => {
                            println!("Failed to load CoreML model: {}", e);
                        }
                    }
                    return;
                }
            }
        }

        println!("No CoreML model found in cache - download anemll/Qwen2.5-1.5B-Instruct first");
    }
}
