// Common model utilities and types
// Phase 4: Candle removed, ONNX only
// DevicePreference is deprecated but kept for config backward-compat and tests.
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

/// Device configuration options (DEPRECATED: Phase 4 - kept for compatibility)
///
/// With ONNX Runtime, device selection is handled by execution providers:
/// - CoreML (Apple Neural Engine)
/// - CPU (fallback)
/// - CUDA/TensorRT (NVIDIA GPUs)
/// - DirectML (Windows GPUs)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[deprecated(note = "Use ONNX Runtime execution providers instead")]
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


// Phase 4: Device functions removed (Candle-based)
// ONNX Runtime handles device selection via execution providers

/// Stub: Device selection removed (Phase 4)
#[deprecated(note = "Device selection removed - use ONNX Runtime execution providers")]
pub fn get_device_with_preference(_preference: DevicePreference) -> Result<()> {
    anyhow::bail!(
        "get_device_with_preference removed in Phase 4.\n\
         ONNX Runtime handles device selection automatically via execution providers."
    )
}

/// Stub: Device info removed (Phase 4)
#[deprecated(note = "Device info removed - ONNX Runtime manages devices")]
pub fn device_info() -> String {
    "ONNX Runtime (device managed automatically)".to_string()
}

/// Stub: Metal availability check removed (Phase 4)
#[deprecated(note = "Metal check removed - ONNX Runtime handles CoreML EP")]
pub fn is_metal_available() -> bool {
    // Assume true on macOS for compatibility
    cfg!(target_os = "macos")
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
        assert!(matches!(DevicePreference::default(), DevicePreference::Auto));
    }

    #[allow(deprecated)]
    #[test]
    fn test_device_info_stub_mentions_onnx() {
        let info = device_info();
        assert!(info.contains("ONNX"), "expected ONNX in: {info}");
    }

    #[allow(deprecated)]
    #[test]
    fn test_get_device_with_preference_always_errors() {
        let result = get_device_with_preference(DevicePreference::Auto);
        assert!(result.is_err(), "Phase 4 stub should always error");
    }

    #[allow(deprecated)]
    #[test]
    fn test_is_metal_available_returns_bool() {
        // Just verify it doesn't panic; actual value is platform-dependent
        let _ = is_metal_available();
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
