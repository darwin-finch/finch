// Generic model loader supporting multiple families and backends
// Enables users to run Qwen, Gemma, Llama, or Mistral on CoreML, Metal, CUDA, or CPU

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::BackendDevice;
use super::download::ModelDownloader;
use super::generator_new::TextGeneration;
use super::loaders::onnx::{OnnxLoader, LoadedOnnxModel};
use super::loaders::onnx_config::{OnnxLoadConfig, ModelSize as OnnxModelSize};
use super::model_selector::QwenSize;

// Phase 4: Candle removed, ONNX only
// Legacy types kept for compatibility (will be removed after migration complete)

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
        let model_dir = self.cache_root.join(cache_name);

        // Return the latest snapshot directory if it exists
        let snapshots_dir = model_dir.join("snapshots");
        if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
            // Find the most recent snapshot (last modified)
            let mut snapshots: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .collect();

            snapshots.sort_by_key(|e| {
                e.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
            });

            if let Some(latest) = snapshots.last() {
                return latest.path();
            }
        }

        // Fallback to model_dir if no snapshots found
        model_dir
    }

    fn is_cached(&self, repo_id: &str) -> bool {
        let cache_path = self.get_cache_path(repo_id);
        if !cache_path.exists() {
            return false;
        }

        // Check for snapshots directory (HF Hub structure)
        let snapshots_dir = cache_path.join("snapshots");
        if !snapshots_dir.exists() {
            return false;
        }

        // Check if any snapshot has required files
        if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
            for entry in entries.flatten() {
                let snapshot_path = entry.path();
                if snapshot_path.is_dir() {
                    // Check for required files
                    let has_config = snapshot_path.join("config.json").exists();
                    let has_tokenizer = snapshot_path.join("tokenizer.json").exists();

                    if has_config && has_tokenizer {
                        return true;
                    }
                }
            }
        }

        false
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

    /// Load model with configuration (Phase 5: ONNX implementation)
    pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
        tracing::info!("Loading model: {:?} {:?} on {:?}", config.family, config.size, config.backend);

        // Phase 5: Currently only Qwen2 is supported via ONNX
        // Other families will be added as ONNX models become available
        if config.family != ModelFamily::Qwen2 {
            anyhow::bail!(
                "Only Qwen2 models are currently supported via ONNX.\n\
                 Other families (Gemma2, Llama3, Mistral) will be added soon."
            );
        }

        // Convert unified ModelLoadConfig to OnnxLoadConfig
        let onnx_config = self.to_onnx_config(&config)?;

        // Load via ONNX
        let onnx_loader = OnnxLoader::new(onnx_config.cache_dir.clone());
        let model = onnx_loader.load_model_sync(&onnx_config)
            .context("Failed to load ONNX model")?;

        tracing::info!("Successfully loaded model: {}", model.model_name());

        // Box and return as TextGeneration trait object
        Ok(Box::new(model))
    }

    /// Convert ModelLoadConfig to OnnxLoadConfig (Phase 5 helper)
    fn to_onnx_config(&self, config: &ModelLoadConfig) -> Result<OnnxLoadConfig> {
        // Get cache directory
        let cache_dir = dirs::home_dir()
            .context("Failed to determine home directory")?
            .join(".cache/huggingface/hub");

        // Map unified ModelSize to ONNX ModelSize
        let onnx_size = match config.size {
            ModelSize::Small => OnnxModelSize::Medium,   // 1.5B
            ModelSize::Medium => OnnxModelSize::Large,   // 3B
            ModelSize::Large => OnnxModelSize::XLarge,   // 7B
            ModelSize::XLarge => OnnxModelSize::XLarge,  // 7B (max for ONNX currently)
        };

        // Build model name based on family and size
        let model_name = if let Some(ref repo) = config.repo_override {
            // Extract model name from custom repo (e.g., "user/model-name" → "model-name")
            repo.split('/').last().unwrap_or(repo).to_string()
        } else {
            // Standard naming for Qwen2
            format!("Qwen2.5-{}-Instruct", onnx_size.to_string())
        };

        // Map BackendDevice to execution providers
        use super::loaders::onnx_config::ExecutionProvider;

        let execution_providers = match config.backend {
            #[cfg(target_os = "macos")]
            BackendDevice::CoreML => {
                Some(vec![
                    ExecutionProvider::CoreML,
                    ExecutionProvider::CPU,
                ])
            }
            #[cfg(feature = "cuda")]
            BackendDevice::Cuda => {
                Some(vec![
                    ExecutionProvider::CUDA,
                    ExecutionProvider::CPU,
                ])
            }
            #[cfg(target_os = "macos")]
            BackendDevice::Metal => {
                // Metal not directly supported by ONNX, use CoreML (which uses ANE/GPU)
                tracing::warn!("Metal not directly supported by ONNX, using CoreML instead");
                Some(vec![
                    ExecutionProvider::CoreML,
                    ExecutionProvider::CPU,
                ])
            }
            BackendDevice::Cpu => {
                Some(vec![ExecutionProvider::CPU])
            }
            BackendDevice::Auto => None, // Let ONNX loader decide
        };

        Ok(OnnxLoadConfig {
            model_name,
            size: onnx_size,
            cache_dir,
            execution_providers,
        })
    }

    /// DEPRECATED: Candle-based loading removed
    #[allow(dead_code)]
    fn load_legacy(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
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

            // Estimate download size based on model size
            let estimated_size_gb = match config.size {
                ModelSize::Small => 3.0,   // ~1-3B models
                ModelSize::Medium => 8.0,  // ~3-9B models
                ModelSize::Large => 16.0,  // ~7-14B models
                ModelSize::XLarge => 30.0, // ~14B+ models
            };

            // Download model (blocking)
            let (cache_path, _rx) = self
                .downloader
                .download_model(&repo_id, estimated_size_gb)
                .with_context(|| format!("Failed to download model from {}", repo_id))?;

            cache_path
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
            // CoreML needs pre-converted .mlpackage models (macOS only)
            // Community and official conversions from HuggingFace
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::CoreML) => {
                // Only Small (0.6B) has proper CoreML structure with config.json + tokenizer.json
                match config.size {
                    ModelSize::Small => "anemll/anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4".to_string(),
                    _ => {
                        anyhow::bail!(
                            "CoreML Qwen only available for Small (0.6B).\n\
                             \n\
                             Repository: anemll/anemll-Qwen-Qwen3-0.6B-ctx512_0.3.4\n\
                             \n\
                             For larger models, use CPU backend (Metal has rms-norm bug).\n\
                             \n\
                             To fix: Change config.toml:\n\
                             [backend]\n\
                             device = \"coreml\"\n\
                             model_size = \"Small\"  # <-- Change this"
                        )
                    }
                }
            }

            #[cfg(target_os = "macos")]
            (ModelFamily::Llama3, BackendDevice::CoreML) => {
                // Community CoreML conversions (andmev, smpanaro)
                match config.size {
                    ModelSize::Small => "smpanaro/Llama-3.2-1B-Instruct-CoreML".to_string(),
                    ModelSize::Medium => "andmev/Llama-3.2-3B-Instruct-CoreML".to_string(),
                    ModelSize::Large => "andmev/Llama-3.1-8B-Instruct-CoreML".to_string(),
                    _ => {
                        anyhow::bail!(
                            "CoreML Llama only available up to 8B. \
                             For larger models, use Metal or CPU."
                        )
                    }
                }
            }

            #[cfg(target_os = "macos")]
            (ModelFamily::Gemma2, BackendDevice::CoreML) => {
                // anemll has Gemma 3 270M
                match config.size {
                    ModelSize::Small => "anemll/anemll-google-gemma-3-270m-it-M1-ctx512-monolithic_0.3.5".to_string(),
                    _ => {
                        anyhow::bail!(
                            "CoreML Gemma only available for Small (270M). \
                             For larger models, use Metal or CPU."
                        )
                    }
                }
            }

            #[cfg(target_os = "macos")]
            (ModelFamily::Mistral, BackendDevice::CoreML) => {
                // Apple's official CoreML conversion (7B)
                "apple/mistral-coreml".to_string()
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
    ///
    /// DEPRECATED: References deleted Candle loaders (Phase 4)
    #[allow(dead_code, unused_variables)]
    fn load_model_variant(
        &self,
        config: &ModelLoadConfig,
        model_path: &std::path::Path,
    ) -> Result<Box<dyn TextGeneration>> {
        anyhow::bail!("Candle loaders removed - use load_onnx() instead")
    }

    /// DEPRECATED: Old Candle-based implementation
    #[allow(dead_code, unused_variables)]
    fn load_model_variant_legacy(
        &self,
        config: &ModelLoadConfig,
        model_path: &std::path::Path,
    ) -> Result<Box<dyn TextGeneration>> {
        /*
        match (&config.family, &config.backend) {
            // Qwen on CoreML (macOS only)
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::CoreML) => {
                loaders::coreml::load(model_path, config.family, config.size)
            }

            // Qwen on Metal (macOS only)
            #[cfg(target_os = "macos")]
            (ModelFamily::Qwen2, BackendDevice::Metal) => {
                let device = Device::new_metal(0)
                    .context("Failed to initialize Metal device")?;
                loaders::qwen::load(model_path, config.size, device)
            }

            // Qwen on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Qwen2, BackendDevice::Cuda) => {
                let device = Device::new_cuda(0)
                    .context("Failed to initialize CUDA device")?;
                loaders::qwen::load(model_path, config.size, device)
            }

            // Qwen on CPU (all platforms)
            (ModelFamily::Qwen2, BackendDevice::Cpu) => {
                loaders::qwen::load(model_path, config.size, Device::Cpu)
            }

            // Gemma on Metal (macOS)
            #[cfg(target_os = "macos")]
            (ModelFamily::Gemma2, BackendDevice::Metal) => {
                let device = Device::new_metal(0)
                    .context("Failed to initialize Metal device")?;
                loaders::gemma::load(model_path, config.size, device)
            }

            // Gemma on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Gemma2, BackendDevice::Cuda) => {
                let device = Device::new_cuda(0)
                    .context("Failed to initialize CUDA device")?;
                loaders::gemma::load(model_path, config.size, device)
            }

            // Gemma on CPU (all platforms)
            (ModelFamily::Gemma2, BackendDevice::Cpu) => {
                loaders::gemma::load(model_path, config.size, Device::Cpu)
            }

            // Llama on CoreML (macOS only) - uses community conversions
            #[cfg(target_os = "macos")]
            (ModelFamily::Llama3, BackendDevice::CoreML) => {
                // For Small/Medium, use CoreML conversions if downloaded
                // Otherwise fall back to Metal (handled by error)
                loaders::coreml::load(model_path, config.family, config.size)
            }

            // Llama on Metal (macOS)
            #[cfg(target_os = "macos")]
            (ModelFamily::Llama3, BackendDevice::Metal) => {
                let device = Device::new_metal(0)
                    .context("Failed to initialize Metal device")?;
                loaders::llama::load(model_path, config.size, device)
            }

            // Llama on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Llama3, BackendDevice::Cuda) => {
                let device = Device::new_cuda(0)
                    .context("Failed to initialize CUDA device")?;
                loaders::llama::load(model_path, config.size, device)
            }

            // Llama on CPU (all platforms)
            (ModelFamily::Llama3, BackendDevice::Cpu) => {
                loaders::llama::load(model_path, config.size, Device::Cpu)
            }

            // Mistral on CoreML (macOS only) - uses Apple's official conversion
            #[cfg(target_os = "macos")]
            (ModelFamily::Mistral, BackendDevice::CoreML) => {
                loaders::coreml::load(model_path, config.family, config.size)
            }

            // Mistral on Metal (macOS)
            #[cfg(target_os = "macos")]
            (ModelFamily::Mistral, BackendDevice::Metal) => {
                let device = Device::new_metal(0)
                    .context("Failed to initialize Metal device")?;
                loaders::mistral::load(model_path, config.size, device)
            }

            // Mistral on CUDA (Linux/Windows)
            #[cfg(feature = "cuda")]
            (ModelFamily::Mistral, BackendDevice::Cuda) => {
                let device = Device::new_cuda(0)
                    .context("Failed to initialize CUDA device")?;
                loaders::mistral::load(model_path, config.size, device)
            }

            // Mistral on CPU (all platforms)
            (ModelFamily::Mistral, BackendDevice::Cpu) => {
                loaders::mistral::load(model_path, config.size, Device::Cpu)
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
        */
        // Commented out - Candle loaders removed in Phase 4
        anyhow::bail!("Legacy Candle loading removed")
    }

    /// Load ONNX model (Phase 4: Primary loading method)
    ///
    /// This will replace the Candle-based loaders in Phase 4.
    /// For now, it coexists with the old loaders for testing.
    pub fn load_onnx(&self, ram_gb: Option<usize>) -> Result<LoadedOnnxModel> {
        tracing::info!("Loading model via ONNX Runtime (Phase 3)");

        // Create cache directory
        let cache_dir = dirs::home_dir()
            .context("Failed to determine home directory")?
            .join(".cache/huggingface/hub");

        std::fs::create_dir_all(&cache_dir)
            .context("Failed to create cache directory")?;

        // Create ONNX config with automatic size selection
        let config = if let Some(ram) = ram_gb {
            let size = OnnxModelSize::from_ram(ram);
            OnnxLoadConfig::with_size(size, cache_dir)
        } else {
            OnnxLoadConfig::from_system_ram(cache_dir)
        };

        // Create ONNX loader
        let loader = OnnxLoader::new(config.cache_dir.clone());

        // Load model
        let model = loader.load_model_sync(&config)
            .context("Failed to load ONNX model")?;

        tracing::info!("Successfully loaded ONNX model: {}", model.model_name());

        Ok(model)
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
