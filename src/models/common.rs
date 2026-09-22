// Common model utilities and retained training configuration.
// DevicePreference is deprecated but kept for serialized training metadata.
#![allow(deprecated)]

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Common model configuration (for custom transformers)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub vocab_size: usize,
    pub hidden_dim: usize,
    pub num_layers: usize,
    pub num_heads: usize,
    pub max_seq_len: usize,
    pub dropout: f64,
    pub device_preference: DevicePreference,
}

/// Generator configuration - supports both custom and pre-trained models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GeneratorConfig {
    /// Random initialization (existing behavior)
    RandomInit(ModelConfig),

    /// Pre-trained model using unified loader (generic across families/backends)
    Pretrained(crate::models::unified_loader::ModelLoadConfig),
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            vocab_size: 50_000,
            hidden_dim: 768,
            num_layers: 6,
            num_heads: 12,
            max_seq_len: 512,
            dropout: 0.1,
            device_preference: DevicePreference::Auto,
        }
    }
}

impl ModelConfig {
    /// Create config optimized for Apple Silicon
    pub fn for_apple_silicon() -> Self {
        Self {
            vocab_size: 50_000,
            hidden_dim: 768,
            num_layers: 6,
            num_heads: 12,
            max_seq_len: 512,
            dropout: 0.1,
            device_preference: DevicePreference::Metal,
        }
    }

    /// Create small config for fast testing (works well on CPU)
    pub fn small() -> Self {
        Self {
            vocab_size: 5000,
            hidden_dim: 128,
            num_layers: 2,
            num_heads: 4,
            max_seq_len: 256,
            dropout: 0.0,
            device_preference: DevicePreference::Auto,
        }
    }
}

/// Legacy training-device preference retained for serialized metadata.
///
/// The llama.cpp chat loader does not read this value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[deprecated(note = "No active model backend reads this training metadata")]
#[derive(Default)]
pub enum DevicePreference {
    /// Use best available device
    #[default]
    Auto,
    /// Force CPU usage
    Cpu,
    /// Force Metal (Apple Silicon GPU)
    Metal,
}

/// Model persistence
pub trait Saveable {
    fn save(&self, path: &Path) -> Result<()>;
    fn load(path: &Path) -> Result<Self>
    where
        Self: Sized;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_config_default_values() {
        let config = ModelConfig::default();
        assert_eq!(config.vocab_size, 50_000);
        assert_eq!(config.hidden_dim, 768);
        assert_eq!(config.num_layers, 6);
        assert_eq!(config.num_heads, 12);
        assert_eq!(config.max_seq_len, 512);
        assert_eq!(config.dropout, 0.1);
    }

    #[test]
    fn test_model_config_small() {
        let config = ModelConfig::small();
        assert_eq!(config.vocab_size, 5000);
        assert_eq!(config.hidden_dim, 128);
        assert_eq!(config.num_layers, 2);
        assert_eq!(config.num_heads, 4);
        assert_eq!(config.max_seq_len, 256);
        assert_eq!(config.dropout, 0.0);
    }

    #[allow(deprecated)]
    #[test]
    fn test_model_config_for_apple_silicon_uses_metal() {
        let config = ModelConfig::for_apple_silicon();
        assert!(matches!(config.device_preference, DevicePreference::Metal));
    }

    #[allow(deprecated)]
    #[test]
    fn test_device_preference_default_is_auto() {
        assert!(matches!(
            DevicePreference::default(),
            DevicePreference::Auto
        ));
    }

    #[test]
    fn test_model_config_serde_roundtrip() {
        let config = ModelConfig::default();
        let json = serde_json::to_string(&config).unwrap();
        let back: ModelConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.vocab_size, config.vocab_size);
        assert_eq!(back.hidden_dim, config.hidden_dim);
        assert_eq!(back.num_layers, config.num_layers);
    }
}
