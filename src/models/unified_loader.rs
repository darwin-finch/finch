// Generic model loader supporting multiple families via ONNX Runtime
// Enables users to run Qwen, Gemma, Llama, or Mistral using ONNX Runtime with various execution providers

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::ExecutionTarget;
use super::download::ModelDownloader;
use super::generator_new::TextGeneration;
#[cfg(feature = "onnx")]
use super::loaders::onnx::{OnnxLoader, LoadedOnnxModel};
#[cfg(feature = "onnx")]
use super::loaders::onnx_config::{OnnxLoadConfig, ModelSize as OnnxModelSize};
use super::model_selector::QwenSize;

/// Inference provider selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InferenceProvider {
    /// ONNX Runtime (recommended, works on all platforms)
    #[serde(rename = "onnx")]
    Onnx,
    /// Candle (alternative, native Rust implementation)
    #[cfg(feature = "candle")]
    #[serde(rename = "candle")]
    Candle,
}

impl Default for InferenceProvider {
    fn default() -> Self {
        Self::Onnx  // ONNX is the default provider
    }
}

impl InferenceProvider {
    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Onnx => "ONNX Runtime",
            #[cfg(feature = "candle")]
            Self::Candle => "Candle",
        }
    }

    /// Get description for users
    pub fn description(&self) -> &'static str {
        match self {
            Self::Onnx => "ONNX Runtime (Recommended) - Cross-platform, optimized",
            #[cfg(feature = "candle")]
            Self::Candle => "Candle - Native Rust implementation, good for development",
        }
    }
}

/// Configuration for loading any model on any execution target with any provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLoadConfig {
    /// Which inference provider to use (ONNX Runtime or Candle)
    #[serde(default)]
    pub provider: InferenceProvider,
    /// Which model architecture to use
    pub family: ModelFamily,
    /// Which size variant (Small = 1-3B, Medium = 3-9B, Large = 7-14B, XLarge = 14B+)
    pub size: ModelSize,
    /// Which execution target to run on (CoreML/CPU/CUDA)
    #[serde(alias = "backend")] // Support old field name
    pub target: ExecutionTarget,
    /// Optional: override HuggingFace repository (for custom models)
    pub repo_override: Option<String>,
}

impl ModelLoadConfig {
    /// Legacy field accessor for backward compatibility
    #[deprecated(note = "Use target field directly")]
    pub fn backend(&self) -> ExecutionTarget {
        self.target
    }
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
    /// Microsoft Phi series (2B, 3.8B, 14B)
    Phi,
    /// DeepSeek Coder series (1.3B, 6.7B, 33B)
    DeepSeek,
}

impl ModelFamily {
    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen 2.5",
            Self::Gemma2 => "Gemma 2",
            Self::Llama3 => "Llama 3",
            Self::Mistral => "Mistral",
            Self::Phi => "Phi",
            Self::DeepSeek => "DeepSeek",
        }
    }

    /// Get description for users
    pub fn description(&self) -> &'static str {
        match self {
            Self::Qwen2 => "Qwen 2.5 (Recommended) - Best overall quality",
            Self::Gemma2 => "Gemma 2 - Google's model, good for chat",
            Self::Llama3 => "Llama 3 - Meta's model, popular choice",
            Self::Mistral => "Mistral - Efficient 7B model",
            Self::Phi => "Phi - Microsoft's compact model, efficient",
            Self::DeepSeek => "DeepSeek - Specialized for coding tasks",
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

            // Phi: 2B (Phi-2), 3.8B (Phi-3-mini), 14B (Phi-3-medium)
            (ModelFamily::Phi, Self::Small) => "2B",
            (ModelFamily::Phi, Self::Medium) => "3.8B",
            (ModelFamily::Phi, Self::Large) => "14B",
            (ModelFamily::Phi, Self::XLarge) => "14B",

            // DeepSeek: 1.3B, 6.7B, 16B (V2-Lite), 33B
            (ModelFamily::DeepSeek, Self::Small) => "1.3B",
            (ModelFamily::DeepSeek, Self::Medium) => "6.7B",
            (ModelFamily::DeepSeek, Self::Large) => "16B",
            (ModelFamily::DeepSeek, Self::XLarge) => "33B",
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

    /// Load model with configuration (supports both ONNX and Candle providers)
    pub fn load(&self, config: ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
        tracing::info!(
            "Loading model: {:?} {:?} ({:?}) on {:?}",
            config.family,
            config.size,
            config.provider,
            config.target
        );

        match config.provider {
            InferenceProvider::Onnx => {
                // Convert unified ModelLoadConfig to OnnxLoadConfig
                let onnx_config = self.to_onnx_config(&config)?;

                // Load via ONNX
                let onnx_loader = OnnxLoader::new(onnx_config.cache_dir.clone());
                let model = onnx_loader
                    .load_model_sync(&onnx_config)
                    .context("Failed to load ONNX model")?;

                tracing::info!("Successfully loaded ONNX model: {}", model.model_name());

                // Box and return as TextGeneration trait object
                Ok(Box::new(model))
            }

            #[cfg(feature = "candle")]
            InferenceProvider::Candle => {
                // Load via Candle
                self.load_candle(&config)
            }
        }
    }

    /// Load model using Candle provider
    #[cfg(feature = "candle")]
    fn load_candle(&self, config: &ModelLoadConfig) -> Result<Box<dyn TextGeneration>> {
        use super::loaders::candle::CandleLoader;

        tracing::info!("Loading Candle model");

        // Resolve repository ID
        let repo_id = self.resolve_repository(config)?;

        // Check if model is cached
        let model_path = if self.cache.is_cached(&repo_id) {
            tracing::debug!("Model found in cache");
            self.cache.get_cache_path(&repo_id)
        } else {
            tracing::info!("Model not cached, downloading from HuggingFace...");

            // Estimate download size
            let estimated_size_gb = match config.size {
                ModelSize::Small => 3.0,
                ModelSize::Medium => 8.0,
                ModelSize::Large => 16.0,
                ModelSize::XLarge => 30.0,
            };

            // Download model
            let (cache_path, _rx) = self
                .downloader
                .download_model(&repo_id, estimated_size_gb)
                .with_context(|| format!("Failed to download model from {}", repo_id))?;

            cache_path
        };

        // Load via Candle loader
        let candle_loader = CandleLoader::new();
        let model = candle_loader
            .load(&model_path, config.family, config.size, config.target)
            .context("Failed to load Candle model")?;

        tracing::info!("Successfully loaded Candle model");

        Ok(model)
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

        // Resolve HuggingFace repository ID based on family and size
        let repo_id = self.resolve_repository(config)?;

        // Extract model name from repo ID (e.g., "onnx-community/Qwen2.5-1.5B-Instruct" → "Qwen2.5-1.5B-Instruct")
        let model_name = repo_id.split('/').last().unwrap_or(&repo_id).to_string();

        // Map ExecutionTarget to ONNX Runtime execution providers
        use super::loaders::onnx_config::ExecutionProvider;

        let execution_providers = match config.target {
            #[cfg(target_os = "macos")]
            ExecutionTarget::CoreML => {
                Some(vec![
                    ExecutionProvider::CoreML,
                    ExecutionProvider::CPU,
                ])
            }
            #[cfg(feature = "cuda")]
            ExecutionTarget::Cuda => {
                Some(vec![
                    ExecutionProvider::CUDA,
                    ExecutionProvider::CPU,
                ])
            }
            ExecutionTarget::Cpu => {
                Some(vec![ExecutionProvider::CPU])
            }
            ExecutionTarget::Auto => None, // Let ONNX loader decide
        };

        Ok(OnnxLoadConfig {
            model_name,
            repo_id,
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
            config.target.name()
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

    /// Resolve HuggingFace repository ID based on provider, family, and size
    ///
    /// Uses the compatibility matrix to get the correct repository
    fn resolve_repository(&self, config: &ModelLoadConfig) -> Result<String> {
        // Check for user override first
        if let Some(ref repo) = config.repo_override {
            return Ok(repo.clone());
        }

        // Use compatibility matrix to resolve repository
        super::compatibility::get_repository(config.provider, config.family, config.size)
            .with_context(|| format!(
                "No {:?} model repository available for {:?} {:?}",
                config.provider,
                config.family,
                config.size
            ))
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

        // Qwen standard (ONNX community)
        let config = ModelLoadConfig {
            provider: InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "onnx-community/Qwen2.5-1.5B-Instruct");

        // Gemma (onnx-community)
        let config = ModelLoadConfig {
            provider: InferenceProvider::Onnx,
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "onnx-community/gemma-3-270m-it-ONNX");

        // Llama (onnx-community)
        let config = ModelLoadConfig {
            provider: InferenceProvider::Onnx,
            family: ModelFamily::Llama3,
            size: ModelSize::Medium,
            target: ExecutionTarget::Cpu,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "onnx-community/Llama-3.2-3B-Instruct-ONNX");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn test_coreml_repository_resolution() {
        let loader = UnifiedModelLoader::new().unwrap();

        // CoreML uses same onnx-community repos (execution provider difference only)
        let config = ModelLoadConfig {
            provider: InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Medium,
            target: ExecutionTarget::CoreML,
            repo_override: None,
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "onnx-community/Qwen2.5-Coder-3B-Instruct");
    }

    #[test]
    fn test_repo_override() {
        let loader = UnifiedModelLoader::new().unwrap();

        let config = ModelLoadConfig {
            provider: InferenceProvider::Onnx,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            repo_override: Some("custom-org/custom-model".to_string()),
        };
        let repo = loader.resolve_repository(&config).unwrap();
        assert_eq!(repo, "custom-org/custom-model");
    }
}
