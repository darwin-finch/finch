// Common model utilities and types

use anyhow::Result;
use candle_core::{Device, Tensor};
use std::path::Path;

/// Common model configuration
#[derive(Debug, Clone)]
pub struct ModelConfig {
    pub vocab_size: usize,
    pub hidden_dim: usize,
    pub num_layers: usize,
    pub num_heads: usize,
    pub max_seq_len: usize,
    pub dropout: f64,
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
        }
    }
}

/// Device selection (CPU, CUDA, or Metal for Apple Silicon)
pub fn get_device() -> Result<Device> {
    #[cfg(target_os = "macos")]
    {
        // Try Metal (Apple Silicon) first
        if let Ok(device) = Device::new_metal(0) {
            return Ok(device);
        }
    }

    // Fall back to CPU
    Ok(Device::Cpu)
}

/// Model persistence
pub trait Saveable {
    fn save(&self, path: &Path) -> Result<()>;
    fn load(path: &Path) -> Result<Self> where Self: Sized;
}
