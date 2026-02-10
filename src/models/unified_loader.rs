// Generic model loader supporting multiple families and backends
// Enables users to run Qwen, Gemma, Llama, or Mistral on CoreML, Metal, CUDA, or CPU

use anyhow::{Context, Result};
use candle_core::Device;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::common::DevicePreference;
use super::download::ModelDownloader;
use super::model_selector::QwenSize;
use super::TextGeneration;

/// Configuration for loading any model on any backend
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLoadConfig {
    /// Which model architecture to use
    pub family: ModelFamily,
    /// Which size variant (Small = 1-3B, Medium = 3-9B, Large = 7-14B, XLarge = 14B+)
    pub size: ModelSize,
    /// Which backend device to run on
    pub backend: BackendDevice,
    /// Optional: override HuggingFace repository (for custom models)
    pub repo_override: Option<String>,
}

/// Supported model families
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    /// Qwen 2.5 series (1.5B, 3B, 7B, 14B)
    Qwen2,
    /// Google Gemma 2 series (2B, 9B, 27B)
    Gemma2,
    /// Meta Llama 3.x series (3B, 8B, 70B)
    Llama3,
    /// Mistral series (7B, 22B)
    Mistral,
}

impl ModelFamily {
    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen 2.5",
            Self::Gemma2 => "Gemma 2",
            Self::Llama3 => "Llama 3",
            Self::Mistral => "Mistral",
        }
    }

    /// Get description for users
    pub fn description(&self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen 2.5 (Recommended) - Best overall quality",
            Self::Gemma2 => "Gemma 2 - Google's model, good for chat",
            Self::Llama3 => "Llama 3 - Meta's model, popular choice",
            Self::Mistral => "Mistral - Efficient 7B model",
        }
    }
}

/// Model size categories (family-specific)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelSize {
    /// ~1-3B parameters (fastest, lowest memory)
    Small,
    /// ~3-9B parameters (balanced)
    Medium,
    /// ~7-14B parameters (high quality)
    Large,
    /// ~14B+ parameters (maximum quality, high memory)
    XLarge,
}

impl ModelSize {
    /// Convert legacy QwenSize to generic ModelSize
    pub fn from_qwen(qwen_size: QwenSize) -> Self {
        match qwen_size {
            QwenSize::Qwen1_5B => Self::Small,
            QwenSize::Qwen3B => Self::Medium,
            QwenSize::Qwen7B => Self::Large,
            QwenSize::Qwen14B => Self::XLarge,
        }
    }

    /// Convert to family-specific size string for repository resolution
    pub fn to_size_string(&self, family: ModelFamily) -> &'static str {
        match (family, self) {
            // Qwen: 1.5B, 3B, 7B, 14B
            (ModelFamily::Qwen2, Self::Small) => "1.5B",
            (ModelFamily::Qwen2, Self::Medium) => "3B",
            (ModelFamily::Qwen2, Self::Large) => "7B",
            (ModelFamily::Qwen2, Self::XLarge) => "14B",

            // Gemma: 2b, 9b, 27b (lowercase convention)
            (ModelFamily::Gemma2, Self::Small) => "2b",
            (ModelFamily::Gemma2, Self::Medium) => "9b",
            (ModelFamily::Gemma2, Self::Large) => "27b",
            (ModelFamily::Gemma2, Self::XLarge) => "27b", // No larger Gemma

            // Llama: 3B, 8B, 70B
            (ModelFamily::Llama3, Self::Small) => "3B",
            (ModelFamily::Llama3, Self::Medium) => "8B",
            (ModelFamily::Llama3, Self::Large) => "70B",
            (ModelFamily::Llama3, Self::XLarge) => "70B",

            // Mistral: 7B, 22B
            (ModelFamily::Mistral, Self::Small) => "7B",
            (ModelFamily::Mistral, Self::Medium) => "7B",
            (ModelFamily::Mistral, Self::Large) => "22B",
            (ModelFamily::Mistral, Self::XLarge) => "22B",
        }
    }

    /// Select appropriate size based on available RAM
    pub fn from_ram(ram_gb: usize) -> Result<Self> {
        match ram_gb {
            0..=7 => anyhow::bail!("Insufficient RAM ({}GB) - need at least 8GB", ram_gb),
            8..=15 => Ok(Self::Small),   // 1-3B models (~3-6GB RAM)
            16..=31 => Ok(Self::Medium), // 3-9B models (~6-12GB RAM)
            32..=63 => Ok(Self::Large),  // 7-14B models (~14-28GB RAM)
            _ => Ok(Self::XLarge),       // 14B+ models (~28GB+ RAM)
        }
    }
}

/// Backend device for model execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendDevice {
    /// CPU (all platforms) - slowest but universal
    Cpu,
    /// Metal (macOS) - Apple Silicon GPU
    Metal,
    /// CoreML (macOS) - Apple Neural Engine (fastest on Mac)
    #[cfg(target_os = "macos")]
    CoreML,
    /// CUDA (Linux/Windows) - NVIDIA GPU
    #[cfg(feature = "cuda")]
    Cuda,
}

impl BackendDevice {
    /// Convert from legacy DevicePreference
    pub fn from_preference(pref: DevicePreference) -> Self {
        match pref {
            DevicePreference::Cpu => Self::Cpu,
            DevicePreference::Metal => Self::Metal,
            DevicePreference::Auto => {
                // Auto-select best backend for platform
                #[cfg(target_os = "macos")]
                {
                    // Prefer Metal over CoreML for now (CoreML requires pre-converted models)
                    Self::Metal
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Self::Cpu
                }
            }
        }
    }

    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Metal => "Metal (GPU)",
            #[cfg(target_os = "macos")]
            Self::CoreML => "CoreML (ANE)",
            #[cfg(feature = "cuda")]
            Self::Cuda => "CUDA (GPU)",
        }
    }
}

/// Model cache management
struct ModelCache {
    cache_root: PathBuf,
}

impl ModelCache {
    fn new() -> Result<Self> {
        // Use standard HuggingFace cache location
        let cache_root = dirs::home_dir()
            .context("Failed to determine home directory")?
            .join(".cache/huggingface/hub");

        std::fs::create_dir_all(&cache_root)
            .context("Failed to create HuggingFace cache directory")?;

        Ok(Self { cache_root })
    }

    fn get_cache_path(&self, repo_id: &str) -> PathBuf {
        // Convert repo ID to cache directory name
        // e.g., "Qwen/Qwen2.5-1.5B-Instruct" -> "models--Qwen--Qwen2.5-1.5B-Instruct"
        let cache_name = format!("models--{}", repo_id.replace('/', "--"));
        self.cache_root.join(cache_name)
    }

    fn is_cached(&self, repo_id: &str) -> bool {
        let cache_path = self.get_cache_path(repo_id);
        cache_path.exists()
    }
}

/// Generic model loader supporting multiple families and backends
pub struct UnifiedModelLoader {
    downloader: ModelDownloader,
    cache: ModelCache,
}

impl UnifiedModelLoader {
    /// Create new unified loader
    pub fn new() -> Result<Self> {
        Ok(Self {
            downloader: ModelDownloader::new()?,
            cache: ModelCache::new()?,
        })
    }

    /// Load model with automatic download and backend selection
    pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
        // 1. Resolve repository ID
        let repo_id = self.resolve_repository(&config)?;

        tracing::info!(
            "Loading {} {} on {}",
            config.family.name(),
            config.size.to_size_string(config.family),
            config.backend.name()
        );

        // 2. Check cache or download
        let model_path = if self.cache.is_cached(&repo_id) {
            tracing::debug!("Model found in cache");
            self.cache.get_cache_path(&repo_id)
        } else {
            tracing::info!("Model not cached, downloading from HuggingFace...");
            // TODO: Implement generic download (Phase 3)
            anyhow::bail!("Model download not yet implemented - please use existing Qwen download")
        };

        // 3. Load model based on family + backend combination
        self.load_model_variant(&config, &model_path)
    }

    /// Resolve HuggingFace repository ID based on family and backend
    fn resolve_repository(&self, config: &ModelLoadConfig) -> Result<String> {
        // Check for user override first
        if let Some(ref repo) = config.repo_override {
            return Ok(repo.clone());
        }

        let size_str = config.size.to_size_string(config.family);

        let repo = match (&config.family, &config.backend) {
            // CoreML needs pre-converted models from anemll
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::CoreML) => {
                format!("anemll/Qwen2.5-{}-Instruct", size_str)
            }

            // Standard Candle-compatible repos
            (ModelFamily::Qwen2, _) => {
                format!("Qwen/Qwen2.5-{}-Instruct", size_str)
            }

            (ModelFamily::Gemma2, _) => {
                format!("google/gemma-2-{}-it", size_str)
            }

            (ModelFamily::Llama3, _) => {
                format!("meta-llama/Llama-3.2-{}-Instruct", size_str)
            }

            (ModelFamily::Mistral, _) => {
                // Mistral has fixed model names, not size-parameterized
                if matches!(config.size, ModelSize::Large | ModelSize::XLarge) {
                    "mistralai/Mistral-22B-Instruct-v0.3".to_string()
                } else {
                    "mistralai/Mistral-7B-Instruct-v0.3".to_string()
                }
            }

            // Unsupported combinations will be caught in load_model_variant
            _ => anyhow::bail!(
                "Repository resolution not implemented for {:?} on {:?}",
                config.family,
                config.backend
            ),
        };

        Ok(repo)
    }

    /// Load model variant based on family + backend combination
    fn load_model_variant(
        &self,
        config: &ModelLoadConfig,
        model_path: &Path,
    ) -> Result<Box<dyn TextGeneration>> {
        match (&config.family, &config.backend) {
            // Qwen on CoreML (macOS only)
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::CoreML) => {
                tracing::info!("Loading Qwen on CoreML/ANE");
                // TODO: Implement in Phase 3
                anyhow::bail!("CoreML loading not yet implemented")
            }

            // Qwen on Metal (macOS only)
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::Metal) => {
                tracing::info!("Loading Qwen on Metal");
                // TODO: Use refactored loader in Phase 2
                anyhow::bail!("Unified Qwen Metal loader not yet implemented - use legacy GeneratorConfig::Qwen")
            }

            // Qwen on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Qwen2, BackendDevice::Cuda) => {
                tracing::info!("Loading Qwen on CUDA");
                // TODO: Implement in Phase 2
                anyhow::bail!("Qwen CUDA loading not yet implemented")
            }

            // Qwen on CPU (all platforms)
            (ModelFamily::Qwen2, BackendDevice::Cpu) => {
                tracing::info!("Loading Qwen on CPU");
                // TODO: Use refactored loader in Phase 2
                anyhow::bail!("Unified Qwen CPU loader not yet implemented - use legacy GeneratorConfig::Qwen")
            }

            // Gemma on Metal (macOS)
            #[cfg(target_os = "macos")]
            (ModelFamily::Gemma2, BackendDevice::Metal) => {
                tracing::info!("Loading Gemma on Metal");
                // TODO: Implement in Phase 4
                anyhow::bail!("Gemma Metal loading not yet implemented")
            }

            // Gemma on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Gemma2, BackendDevice::Cuda) => {
                tracing::info!("Loading Gemma on CUDA");
                // TODO: Implement in Phase 4
                anyhow::bail!("Gemma CUDA loading not yet implemented")
            }

            // Gemma on CPU (all platforms)
            (ModelFamily::Gemma2, BackendDevice::Cpu) => {
                tracing::info!("Loading Gemma on CPU");
                // TODO: Implement in Phase 4
                anyhow::bail!("Gemma CPU loading not yet implemented")
            }

            // Llama variants (Phase 5 - optional)
            (ModelFamily::Llama3, _) => {
                anyhow::bail!("Llama 3 support not yet implemented")
            }

            // Mistral variants (Phase 5 - optional)
            (ModelFamily::Mistral, _) => {
                anyhow::bail!("Mistral support not yet implemented")
            }

            // Unsupported combinations
            _ => {
                anyhow::bail!(
                    "Unsupported combination: {} on {}",
                    config.family.name(),
                    config.backend.name()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_size_from_ram() {
        assert_eq!(ModelSize::from_ram(8).unwrap(), ModelSize::Small);
        assert_eq!(ModelSize::from_ram(16).unwrap(), ModelSize::Medium);
        assert_eq!(ModelSize::from_ram(32).unwrap(), ModelSize::Large);
        assert_eq!(ModelSize::from_ram(64).unwrap(), ModelSize::XLarge);

        // Insufficient RAM
        assert!(ModelSize::from_ram(4).is_err());
    }

    #[test]
    fn test_repository_resolution() {
        let loader = UnifiedModelLoader::new().unwrap();

        // Qwen standard
        let config = ModelLoadConfig {
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            backend: BackendDevice::Metal,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "Qwen/Qwen2.5-1.5B-Instruct");

        // Gemma
        let config = ModelLoadConfig {
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            backend: BackendDevice::Cpu,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "google/gemma-2-2b-it");

        // Llama
        let config = ModelLoadConfig {
            family: ModelFamily::Llama3,
            size: ModelSize::Medium,
            backend: BackendDevice::Cpu,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "meta-llama/Llama-3.2-8B-Instruct");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_coreml_repository_resolution() {
        let loader = UnifiedModelLoader::new().unwrap();

        // CoreML uses anemll org
        let config = ModelLoadConfig {
            family: ModelFamily::Qwen2,
            size: ModelSize::Medium,
            backend: BackendDevice::CoreML,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "anemll/Qwen2.5-3B-Instruct");
    }

    #[test]
    fn test_repo_override() {
        let loader = UnifiedModelLoader::new().unwrap();

        let config = ModelLoadConfig {
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            backend: BackendDevice::Cpu,
            repo_override: Some("custom-org/custom-model".to_string()),
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "custom-org/custom-model");
    }
}
