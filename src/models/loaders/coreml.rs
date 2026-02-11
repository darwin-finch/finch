// CoreML Loader - Models optimized for Apple Neural Engine
// Uses .mlpackage/.mlmodelc format from HuggingFace
// Currently supports: Qwen (full support via candle-coreml)
// Future: Mistral, Llama, Gemma (requires custom generation implementation)

use anyhow::{Context, Result};
use candle_core::Device;
use std::path::Path;
use tokenizers::Tokenizer;

use crate::models::unified_loader::{ModelFamily, ModelSize};
use crate::models::TextGeneration;

/// CoreML generator with tokenizer bridge
///
/// CoreML uses text-based API, but we need token-based API
/// for consistency with other generators. This struct bridges the two.
pub struct CoreMLGenerator {
    // Using QwenModel which has high-level complete_text() API
    // For other architectures, we'd need to implement autoregressive generation
    model: candle_coreml::qwen::QwenModel,
    tokenizer: Tokenizer,
    name: String,
    family: ModelFamily,
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
        tracing::debug!("Starting CoreML generation with {} tokens", input_ids.len());

        // Call CoreML model's complete_text method
        // Note: Uses fixed temperature (0.7) and top_k (50) internally
        let output_text = self
            .model
            .complete_text(&input_text, max_new_tokens)
            .map_err(|e| anyhow::anyhow!("CoreML generation failed: {}", e))?;

        tracing::debug!(
            "CoreML generated {} chars from {} input chars",
            output_text.len(),
            input_text.len()
        );

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

/// Parse meta.yaml file into candle-coreml ModelConfig
///
/// The meta.yaml file from HuggingFace contains model metadata in YAML format,
/// but candle-coreml expects a structured ModelConfig. This function manually
/// parses the YAML and constructs the expected config format.
#[cfg(target_os = "macos")]
fn parse_meta_yaml(meta_path: &Path) -> Result<candle_coreml::config::ModelConfig> {
    use serde_yaml::Value;
    use std::collections::HashMap;
    use std::fs;

    // Read and parse YAML
    let yaml_content = fs::read_to_string(meta_path)
        .with_context(|| format!("Failed to read meta.yaml from {:?}", meta_path))?;

    let yaml: Value = serde_yaml::from_str(&yaml_content)
        .with_context(|| format!("Failed to parse YAML from {:?}", meta_path))?;

    // Extract model_info
    let model_info_yaml = yaml
        .get("model_info")
        .ok_or_else(|| anyhow::anyhow!("meta.yaml missing model_info section"))?;

    let params = model_info_yaml
        .get("parameters")
        .ok_or_else(|| anyhow::anyhow!("meta.yaml missing model_info.parameters section"))?;

    // Extract parameters
    let context_length = params
        .get("context_length")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow::anyhow!("Missing context_length in meta.yaml"))?
        as usize;

    let batch_size = params
        .get("batch_size")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as usize;

    let architecture = model_info_yaml
        .get("architecture")
        .and_then(|v| v.as_str())
        .unwrap_or("qwen")
        .to_string();

    // Create ModelInfo
    let model_info = candle_coreml::config::ModelInfo {
        model_id: model_info_yaml
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        path: Some(meta_path.parent().unwrap().to_string_lossy().to_string()),
        model_type: architecture.clone(),
        discovered_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    // Create ShapeConfig - using reasonable defaults for Qwen
    let shapes = candle_coreml::config::ShapeConfig {
        batch_size,
        context_length,
        hidden_size: 1024, // Default for Qwen 0.6B
        vocab_size: 151_936, // Standard Qwen vocab size
    };

    // Discover .mlmodelc components in the directory
    let model_dir = meta_path.parent().unwrap();
    let mut components: HashMap<String, candle_coreml::config::ComponentConfig> = HashMap::new();

    // Scan directory for .mlmodelc files
    if let Ok(entries) = std::fs::read_dir(model_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.extension().and_then(|s| s.to_str()) == Some("mlmodelc") {
                let filename = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let path_str = path.to_string_lossy().to_string();

                // Map filenames to component names
                let component_name = if filename.contains("embedding") {
                    Some("embeddings")
                } else if filename.contains("lm_head") {
                    Some("lm_head")
                } else if filename.contains("FFN") || filename.contains("ffn") {
                    Some("ffn")
                } else {
                    None
                };

                if let Some(name) = component_name {
                    tracing::debug!("Discovered CoreML component: {} -> {}", name, filename);
                    components.insert(
                        name.to_string(),
                        candle_coreml::config::ComponentConfig {
                            file_path: Some(path_str),
                            inputs: HashMap::new(),
                            outputs: HashMap::new(),
                            functions: vec![],
                            input_order: None,
                        },
                    );
                }
            }
        }
    }

    if components.is_empty() {
        return Err(anyhow::anyhow!(
            "No .mlmodelc components found in {:?}. Expected files like qwen_embeddings.mlmodelc",
            model_dir
        ));
    }

    tracing::debug!("Discovered {} CoreML components", components.len());

    // Create NamingConfig (using empty patterns - file paths are explicit)
    let naming = candle_coreml::config::NamingConfig {
        embeddings_pattern: None,
        ffn_prefill_pattern: None,
        ffn_infer_pattern: None,
        lm_head_pattern: None,
    };

    // Create ModelConfig
    let config = candle_coreml::config::ModelConfig {
        model_info,
        shapes,
        components,
        naming,
        ffn_execution: Some("unified".to_string()),
    };

    tracing::debug!(
        "Parsed ModelConfig: context_length={}, batch_size={}, model_type={}",
        config.shapes.context_length,
        config.shapes.batch_size,
        config.model_info.model_type
    );

    Ok(config)
}

/// Load CoreML model from cache directory
///
/// # Arguments
/// * `model_path` - Directory containing model components and tokenizer
/// * `family` - Model architecture family
/// * `size` - Model size variant
///
/// # Returns
/// Boxed TextGeneration implementation that uses Apple Neural Engine
///
/// # Supported Families
/// - **Qwen**: Full support with anemll models
/// - **Mistral, Llama, Gemma**: Not yet implemented (need custom generation loop)
pub fn load(model_path: &Path, family: ModelFamily, size: ModelSize) -> Result<Box<dyn TextGeneration>> {
    let size_str = size.to_size_string(family);
    tracing::info!(
        "Loading {} {} CoreML model from {:?}",
        family.name(),
        size_str,
        model_path
    );

    // Check family support
    match family {
        ModelFamily::Qwen2 => {
            // Supported - continue
        }
        _ => {
            return Err(anyhow::anyhow!(
                "CoreML {} models not yet supported.\n\
                 \n\
                 Currently only Qwen models work with CoreML because candle-coreml\n\
                 only provides high-level API for Qwen.\n\
                 \n\
                 To add {} support, you'd need to:\n\
                 1. Implement autoregressive generation loop with candle_coreml::CoreMLModel\n\
                 2. Handle tokenization and sampling manually\n\
                 3. Test with Apple's {} CoreML model\n\
                 \n\
                 For now, try:\n\
                 - Use Qwen with CoreML (works great!)\n\
                 - Use {} with Metal/CPU backends (if rms-norm is supported)",
                family.name(),
                family.name(),
                family.name(),
                family.name()
            ));
        }
    }

    // Load tokenizer (needed for token ↔ text conversion)
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

    // Load CoreML model using candle-coreml's Qwen API
    tracing::info!("Loading CoreML model components...");
    tracing::debug!("Model path: {:?}", model_path);

    // Load ModelConfig from meta.yaml
    let meta_path = model_path.join("meta.yaml");
    tracing::debug!("Loading config from: {:?}", meta_path);

    // Check if file exists
    if !meta_path.exists() {
        return Err(anyhow::anyhow!(
            "meta.yaml not found at {:?}. Downloaded files may be in wrong location.",
            meta_path
        ));
    }

    // Parse YAML manually since candle-coreml expects JSON
    let model_config = parse_meta_yaml(&meta_path)
        .map_err(|e| {
            tracing::error!("Failed to parse meta.yaml: {:?}", e);
            tracing::error!("File path: {:?}", meta_path);
            e
        })
        .context("Failed to load meta.yaml - check file format")?;

    tracing::debug!(
        "Model config loaded with {} components",
        model_config.components.len()
    );

    // Create QwenConfig from ModelConfig
    let qwen_config = candle_coreml::qwen::QwenConfig::from_model_config(model_config);

    // Pass the config with discovered components to QwenModel
    let model = candle_coreml::qwen::QwenModel::load_from_directory(model_path, Some(qwen_config))
        .map_err(|e| {
            tracing::error!("CoreML load error: {:?}", e);
            tracing::error!("Note: CoreML models must have .mlmodelc files in the directory");
            tracing::error!("Expected files: qwen_embeddings.mlmodelc, qwen_lm_head.mlmodelc, qwen_FFN_*.mlmodelc");
            e
        })
        .context("Failed to load CoreML model components")?;

    tracing::info!("✓ Loaded {} {} on CoreML/ANE", family.name(), size_str);
    tracing::info!("✓ Model will use Apple Neural Engine if available");

    let name = format!("{} {} (CoreML/ANE)", family.name(), size_str);
    let dummy_device = Device::Cpu; // Placeholder

    Ok(Box::new(CoreMLGenerator {
        model,
        tokenizer,
        name,
        family,
        dummy_device,
    }))
}

/// Check if CoreML model is loadable from cache directory
pub fn is_loadable(cache_dir: &Path) -> bool {
    // Check for tokenizer (required)
    let has_tokenizer = cache_dir.join("tokenizer.json").exists();

    // Check for CoreML model components (.mlmodelc directories)
    let has_coreml = std::fs::read_dir(cache_dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.path().is_dir()
                        && e.file_name().to_string_lossy().ends_with(".mlmodelc")
                })
        })
        .unwrap_or(false);

    has_tokenizer && has_coreml
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
            .join(".cache/huggingface/hub/models--anemll--anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4/snapshots");

        // Find the latest snapshot
        if let Ok(entries) = std::fs::read_dir(&cache_dir) {
            for entry in entries.flatten() {
                let snapshot_dir = entry.path();
                if snapshot_dir.is_dir() && is_loadable(&snapshot_dir) {
                    let result = load(&snapshot_dir, ModelFamily::Qwen2, ModelSize::Small);
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

        println!("No CoreML model found in cache - download anemll/anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4 first");
    }
}
