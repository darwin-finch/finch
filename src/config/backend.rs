// Backend Configuration - Device selection and model management

use crate::models::unified_loader::{ModelFamily, ModelSize};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Backend device type for inference
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackendDevice {
    /// Apple Neural Engine via CoreML (macOS only, fastest)
    #[cfg(target_os = "macos")]
    CoreML,

    /// Metal GPU (macOS only, fast)
    #[cfg(target_os = "macos")]
    Metal,

    /// NVIDIA CUDA GPU (Windows/Linux, fast)
    #[cfg(feature = "cuda")]
    Cuda,

    /// CPU fallback (slow, works everywhere)
    Cpu,

    /// Auto-detect best available device
    Auto,
}

impl BackendDevice {
    /// Get short name for logging
    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            BackendDevice::CoreML => "CoreML (ANE)",
            #[cfg(target_os = "macos")]
            BackendDevice::Metal => "Metal (GPU)",
            #[cfg(feature = "cuda")]
            BackendDevice::Cuda => "CUDA (GPU)",
            BackendDevice::Cpu => "CPU",
            BackendDevice::Auto => "Auto",
        }
    }

    /// Get human-readable description
    pub fn description(&self) -> &'static str {
        match self {
            #[cfg(target_os = "macos")]
            BackendDevice::CoreML => "Apple Neural Engine (ANE) - Fastest, best battery life",
            #[cfg(target_os = "macos")]
            BackendDevice::Metal => "Metal GPU - Fast, flexible",
            #[cfg(feature = "cuda")]
            BackendDevice::Cuda => "NVIDIA CUDA GPU - Very fast",
            BackendDevice::Cpu => "CPU - Slow, works everywhere",
            BackendDevice::Auto => "Auto-detect best available",
        }
    }

    /// Check if this device is available on the current system
    pub fn is_available(&self) -> bool {
        match self {
            #[cfg(target_os = "macos")]
            BackendDevice::CoreML => {
                // Check if we can access CoreML
                // For now, assume available on all macOS
                true
            }
            #[cfg(target_os = "macos")]
            BackendDevice::Metal => {
                use candle_core::Device;
                Device::new_metal(0).is_ok()
            }
            #[cfg(feature = "cuda")]
            BackendDevice::Cuda => {
                use candle_core::Device;
                Device::new_cuda(0).is_ok()
            }
            BackendDevice::Cpu => true, // Always available
            BackendDevice::Auto => true, // Always available
        }
    }

    /// Get list of available devices on this system
    pub fn available_devices() -> Vec<BackendDevice> {
        let mut devices = vec![];

        #[cfg(target_os = "macos")]
        {
            if BackendDevice::CoreML.is_available() {
                devices.push(BackendDevice::CoreML);
            }
            if BackendDevice::Metal.is_available() {
                devices.push(BackendDevice::Metal);
            }
        }

        #[cfg(feature = "cuda")]
        {
            if BackendDevice::Cuda.is_available() {
                devices.push(BackendDevice::Cuda);
            }
        }

        devices.push(BackendDevice::Cpu);
        devices
    }

    /// Select best available device automatically
    pub fn auto_select() -> BackendDevice {
        #[cfg(target_os = "macos")]
        {
            if BackendDevice::CoreML.is_available() {
                return BackendDevice::CoreML;
            }
            if BackendDevice::Metal.is_available() {
                return BackendDevice::Metal;
            }
        }

        #[cfg(feature = "cuda")]
        {
            if BackendDevice::Cuda.is_available() {
                return BackendDevice::Cuda;
            }
        }

        BackendDevice::Cpu
    }
}

/// Backend configuration for model inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendConfig {
    /// Selected device for inference
    pub device: BackendDevice,

    /// Model family to use (Qwen2, Gemma2, etc.)
    #[serde(default = "default_model_family")]
    pub model_family: ModelFamily,

    /// Model size variant (Small, Medium, Large, XLarge)
    #[serde(default = "default_model_size")]
    pub model_size: ModelSize,

    /// Model repository (varies by backend)
    /// - CoreML: "anemll/Qwen2.5-3B-Instruct"
    /// - Others: "Qwen/Qwen2.5-3B-Instruct"
    pub model_repo: Option<String>,

    /// Path to downloaded model
    pub model_path: Option<PathBuf>,

    /// Fallback device chain
    #[serde(default = "default_fallback_chain")]
    pub fallback_chain: Vec<BackendDevice>,
}

fn default_model_family() -> ModelFamily {
    ModelFamily::Qwen2
}

fn default_model_size() -> ModelSize {
    ModelSize::Medium
}

fn default_fallback_chain() -> Vec<BackendDevice> {
    #[cfg(target_os = "macos")]
    return vec![
        BackendDevice::CoreML,
        BackendDevice::Metal,
        BackendDevice::Cpu,
    ];

    #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
    return vec![
        BackendDevice::Cuda,
        BackendDevice::Cpu,
    ];

    #[cfg(all(not(target_os = "macos"), not(feature = "cuda")))]
    return vec![BackendDevice::Cpu];
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            device: BackendDevice::Auto,
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_repo: None,
            model_path: None,
            fallback_chain: default_fallback_chain(),
        }
    }
}

impl BackendConfig {
    /// Create new backend config with device selection
    pub fn with_device(device: BackendDevice) -> Self {
        Self {
            device,
            model_family: default_model_family(),
            model_size: default_model_size(),
            model_repo: None,
            model_path: None,
            fallback_chain: default_fallback_chain(),
        }
    }

    /// Create new backend config with model family and size
    pub fn with_model(device: BackendDevice, family: ModelFamily, size: ModelSize) -> Self {
        Self {
            device,
            model_family: family,
            model_size: size,
            model_repo: None,
            model_path: None,
            fallback_chain: default_fallback_chain(),
        }
    }

    /// Get the model repository for the selected device and model size
    pub fn get_model_repo(&self, model_size: &str) -> String {
        if let Some(repo) = &self.model_repo {
            return repo.clone();
        }

        // Default repos based on device
        match self.device {
            #[cfg(target_os = "macos")]
            BackendDevice::CoreML => {
                // CoreML models from anemll organization
                format!("anemll/Qwen2.5-{}-Instruct", model_size)
            }
            _ => {
                // Standard Qwen models for other backends
                format!("Qwen/Qwen2.5-{}-Instruct", model_size)
            }
        }
    }

    /// Get the effective device (resolve Auto to concrete device)
    pub fn effective_device(&self) -> BackendDevice {
        match self.device {
            BackendDevice::Auto => BackendDevice::auto_select(),
            device => device,
        }
    }
}
