// Common model utilities and types

use anyhow::Result;
use candle_core::Device;
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

    /// Pre-trained model using unified loader (NEW - generic across families/backends)
    Pretrained(crate::models::unified_loader::ModelLoadConfig),

    /// Pre-trained Qwen model (Metal/CPU via Candle)
    /// DEPRECATED: Use Pretrained variant instead
    #[deprecated(note = "Use GeneratorConfig::Pretrained instead")]
    Qwen {
        model_size: crate::models::model_selector::QwenSize,
        cache_dir: std::path::PathBuf,
        device_preference: DevicePreference,
    },

    /// Pre-trained CoreML model (Apple Neural Engine)
    /// DEPRECATED: Use Pretrained variant with backend=CoreML instead
    #[deprecated(note = "Use GeneratorConfig::Pretrained with backend=CoreML instead")]
    #[cfg(target_os = "macos")]
    CoreML {
        model_size: crate::models::model_selector::QwenSize,
        cache_dir: std::path::PathBuf,
    },
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

/// Device configuration options
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevicePreference {
    /// Use best available device (Metal > CPU)
    Auto,
    /// Force CPU usage
    Cpu,
    /// Force Metal (Apple Silicon GPU)
    Metal,
}

impl Default for DevicePreference {
    fn default() -> Self {
        Self::Auto
    }
}

/// Device selection (CPU, CUDA, or Metal for Apple Silicon)
pub fn get_device() -> Result<Device> {
    get_device_with_preference(DevicePreference::Auto)
}

/// Get device with explicit preference
pub fn get_device_with_preference(preference: DevicePreference) -> Result<Device> {
    match preference {
        DevicePreference::Cpu => {
            tracing::info!("Using CPU device (forced)");
            Ok(Device::Cpu)
        }
        DevicePreference::Metal => {
            #[cfg(target_os = "macos")]
            {
                match Device::new_metal(0) {
                    Ok(device) => {
                        tracing::info!("Using Metal device (Apple Silicon GPU)");
                        Ok(device)
                    }
                    Err(e) => {
                        tracing::error!("Failed to initialize Metal device: {}", e);
                        anyhow::bail!("Metal device requested but not available: {}", e)
                    }
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                anyhow::bail!("Metal device requested but not available on non-macOS platform")
            }
        }
        DevicePreference::Auto => {
            #[cfg(target_os = "macos")]
            {
                // Try Metal (Apple Silicon) first
                match Device::new_metal(0) {
                    Ok(device) => {
                        tracing::info!(
                            "Using Metal device (Apple Silicon GPU) - 10-100x faster than CPU"
                        );
                        return Ok(device);
                    }
                    Err(e) => {
                        tracing::warn!("Metal device unavailable ({}), falling back to CPU", e);
                    }
                }
            }

            // Fall back to CPU
            tracing::info!("Using CPU device");
            Ok(Device::Cpu)
        }
    }
}

/// Get information about the current device
pub fn device_info(device: &Device) -> String {
    match device {
        Device::Cpu => "CPU".to_string(),
        Device::Cuda(_) => "CUDA GPU".to_string(),
        Device::Metal(_) => {
            #[cfg(target_os = "macos")]
            {
                // Try to get more specific info about Apple Silicon
                if let Ok(sysname) = std::process::Command::new("sysctl")
                    .args(&["-n", "machdep.cpu.brand_string"])
                    .output()
                {
                    if let Ok(name) = String::from_utf8(sysname.stdout) {
                        return format!("Metal ({})", name.trim());
                    }
                }
            }
            "Metal (Apple Silicon GPU)".to_string()
        }
    }
}

/// Check if Metal is available on this system
pub fn is_metal_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        Device::new_metal(0).is_ok()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Model persistence
pub trait Saveable {
    fn save(&self, path: &Path) -> Result<()>;
    fn load(path: &Path) -> Result<Self>
    where
        Self: Sized;
}
