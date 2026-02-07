// Model persistence utilities
// Handles saving/loading weights + configuration

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use super::common::ModelConfig;

/// Metadata saved alongside model weights
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// Model configuration
    pub config: ModelConfig,
    /// Model type identifier
    pub model_type: String,
    /// Training step/epoch
    pub training_step: usize,
    /// Timestamp of save
    pub timestamp: String,
    /// Version of the persistence format
    pub format_version: u32,
}

impl ModelMetadata {
    pub fn new(config: ModelConfig, model_type: String, training_step: usize) -> Self {
        Self {
            config,
            model_type,
            training_step,
            timestamp: chrono::Utc::now().to_rfc3339(),
            format_version: 1,
        }
    }
}

/// Save model with metadata
///
/// Creates two files:
/// - {path}.safetensors - Model weights (Candle's VarMap format)
/// - {path}.json - Model metadata (config, type, etc.)
pub fn save_model_with_metadata(
    weights_path: &Path,
    varmap: &candle_nn::VarMap,
    metadata: &ModelMetadata,
) -> Result<()> {
    // Save weights
    varmap
        .save(weights_path)
        .with_context(|| format!("Failed to save model weights to {:?}", weights_path))?;

    // Save metadata
    let metadata_path = weights_path.with_extension("json");
    let metadata_json =
        serde_json::to_string_pretty(metadata).context("Failed to serialize model metadata")?;
    fs::write(&metadata_path, metadata_json)
        .with_context(|| format!("Failed to write metadata to {:?}", metadata_path))?;

    tracing::info!(
        "Saved model: {} at step {}",
        metadata.model_type,
        metadata.training_step
    );

    Ok(())
}

/// Load model metadata
pub fn load_model_metadata(weights_path: &Path) -> Result<ModelMetadata> {
    let metadata_path = weights_path.with_extension("json");

    if !metadata_path.exists() {
        anyhow::bail!(
            "Model metadata not found at {:?}. Model may have been saved with old format.",
            metadata_path
        );
    }

    let metadata_json = fs::read_to_string(&metadata_path)
        .with_context(|| format!("Failed to read metadata from {:?}", metadata_path))?;

    let metadata: ModelMetadata =
        serde_json::from_str(&metadata_json).context("Failed to parse model metadata JSON")?;

    Ok(metadata)
}

/// Check if a saved model exists
pub fn model_exists(weights_path: &Path) -> bool {
    let metadata_path = weights_path.with_extension("json");
    weights_path.exists() && metadata_path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_metadata_serialization() {
        let config = ModelConfig::small();
        let metadata = ModelMetadata::new(config, "test_model".to_string(), 100);

        let json = serde_json::to_string(&metadata).unwrap();
        let deserialized: ModelMetadata = serde_json::from_str(&json).unwrap();

        assert_eq!(metadata.model_type, deserialized.model_type);
        assert_eq!(metadata.training_step, deserialized.training_step);
        assert_eq!(metadata.format_version, deserialized.format_version);
    }

    #[test]
    fn test_model_exists() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path();

        // Should not exist initially
        assert!(!model_exists(path));

        // Create the files
        fs::write(path, "weights").unwrap();
        fs::write(path.with_extension("json"), "metadata").unwrap();

        // Should exist now
        assert!(model_exists(path));
    }
}
