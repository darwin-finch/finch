use std::path::PathBuf;
use serde::{Deserialize, Serialize};

/// Model size variants for Qwen2.5 models
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelSize {
    /// 0.5B parameters (~1GB RAM)
    Small,
    /// 1.5B parameters (~3GB RAM)
    Medium,
    /// 3B parameters (~6GB RAM)
    Large,
    /// 7B parameters (~14GB RAM)
    XLarge,
}

impl ModelSize {
    /// Select appropriate model size based on available RAM
    pub fn from_ram(ram_gb: usize) -> Self {
        match ram_gb {
            0..=5 => ModelSize::Small,   // 0.5B — old laptops / 4GB machines
            6..=11 => ModelSize::Medium,  // 1.5B — 8GB machines
            12..=23 => ModelSize::Large,  // 3B   — 16GB machines
            _ => ModelSize::XLarge,       // 7B   — 32GB+ machines
        }
    }

    /// Get model size string for HuggingFace model ID
    pub fn to_string(&self) -> &str {
        match self {
            ModelSize::Small => "0.5B",
            ModelSize::Medium => "1.5B",
            ModelSize::Large => "3B",
            ModelSize::XLarge => "7B",
        }
    }

    /// Get approximate RAM requirement in GB
    pub fn ram_requirement_gb(&self) -> usize {
        match self {
            ModelSize::Small => 2,
            ModelSize::Medium => 4,
            ModelSize::Large => 8,
            ModelSize::XLarge => 16,
        }
    }
}

/// Configuration for loading ONNX models
#[derive(Debug, Clone)]
pub struct OnnxLoadConfig {
    /// Model name (e.g., "Qwen2.5-1.5B-Instruct")
    pub model_name: String,

    /// HuggingFace repository ID (e.g., "onnx-community/Qwen2.5-1.5B-Instruct")
    pub repo_id: String,

    /// Model size variant
    pub size: ModelSize,

    /// Cache directory for downloaded models
    pub cache_dir: PathBuf,

    /// Optional: specific execution providers to use
    /// If None, will try CoreML → CPU fallback
    pub execution_providers: Option<Vec<ExecutionProvider>>,
}

impl OnnxLoadConfig {
    /// Create config with automatic RAM-based model selection
    pub fn from_system_ram(cache_dir: PathBuf) -> Self {
        let ram_gb = crate::models::model_selector::ModelSelector::get_total_ram_gb();
        let size = ModelSize::from_ram(ram_gb);
        let model_name = format!("Qwen2.5-{}-Instruct", size.to_string());
        let repo_id = format!("onnx-community/{}", model_name);

        Self {
            model_name,
            repo_id,
            size,
            cache_dir,
            execution_providers: None,
        }
    }

    /// Create config for specific model size
    pub fn with_size(size: ModelSize, cache_dir: PathBuf) -> Self {
        let model_name = format!("Qwen2.5-{}-Instruct", size.to_string());
        let repo_id = format!("onnx-community/{}", model_name);

        Self {
            model_name,
            repo_id,
            size,
            cache_dir,
            execution_providers: None,
        }
    }

    /// Get HuggingFace repository ID for ONNX models
    pub fn huggingface_repo(&self) -> String {
        self.repo_id.clone()
    }
}

/// Execution provider options for ONNX Runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    /// CoreML (Apple Neural Engine on Apple Silicon)
    CoreML,
    /// CPU (fallback, works everywhere)
    CPU,
    /// CUDA (NVIDIA GPUs, Linux/Windows)
    CUDA,
    /// TensorRT (optimized NVIDIA, Linux)
    TensorRT,
    /// DirectML (Windows GPU acceleration)
    DirectML,
}

impl ExecutionProvider {
    /// Get default execution providers for current platform
    pub fn default_for_platform() -> Vec<Self> {
        #[cfg(target_os = "macos")]
        {
            vec![ExecutionProvider::CoreML, ExecutionProvider::CPU]
        }

        #[cfg(all(target_os = "linux", feature = "cuda"))]
        {
            vec![ExecutionProvider::CUDA, ExecutionProvider::CPU]
        }

        #[cfg(all(target_os = "linux", not(feature = "cuda")))]
        {
            vec![ExecutionProvider::CPU]
        }

        #[cfg(target_os = "windows")]
        {
            vec![ExecutionProvider::DirectML, ExecutionProvider::CPU]
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            vec![ExecutionProvider::CPU]
        }
    }

    /// Get name string for logging
    pub fn name(&self) -> &str {
        match self {
            ExecutionProvider::CoreML => "CoreML",
            ExecutionProvider::CPU => "CPU",
            ExecutionProvider::CUDA => "CUDA",
            ExecutionProvider::TensorRT => "TensorRT",
            ExecutionProvider::DirectML => "DirectML",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_size_from_ram() {
        assert_eq!(ModelSize::from_ram(4), ModelSize::Small);   // old laptop
        assert_eq!(ModelSize::from_ram(5), ModelSize::Small);   // 5GB
        assert_eq!(ModelSize::from_ram(8), ModelSize::Medium);  // 8GB machine
        assert_eq!(ModelSize::from_ram(16), ModelSize::Large);  // 16GB machine
        assert_eq!(ModelSize::from_ram(32), ModelSize::XLarge); // 32GB machine
        assert_eq!(ModelSize::from_ram(64), ModelSize::XLarge); // 64GB machine
    }

    #[test]
    fn test_huggingface_repo() {
        let config = OnnxLoadConfig::with_size(
            ModelSize::Medium,
            PathBuf::from("/tmp/cache"),
        );
        assert_eq!(
            config.huggingface_repo(),
            "onnx-community/Qwen2.5-1.5B-Instruct"
        );
    }
}
