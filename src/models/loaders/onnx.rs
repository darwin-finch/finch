use anyhow::{Context, Result, bail};
use ort::session::Session;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use tokenizers::Tokenizer;
use tracing::{debug, info, warn};

use super::onnx_config::{ExecutionProvider as ConfigExecutionProvider, ModelSize, OnnxLoadConfig};
use crate::models::download::{DownloadProgress, ModelDownloader};

/// ONNX model loader - downloads and loads models from HuggingFace
pub struct OnnxLoader {
    cache_dir: PathBuf,
}

impl OnnxLoader {
    /// Create new ONNX loader with cache directory
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Create ONNX Runtime session with execution providers
    fn create_session(
        &self,
        model_path: &Path,
        config: &OnnxLoadConfig,
    ) -> Result<Session> {
        info!("Creating ONNX session from: {:?}", model_path);

        // Get execution providers based on config
        let _execution_providers = self.get_execution_providers(config);

        // Create session with providers
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .with_intra_threads(4)?  // Parallel ops within layer
            .commit_from_file(model_path)
            .context("Failed to create ONNX session")?;

        info!("ONNX session created successfully");

        Ok(session)
    }

    /// Get execution providers based on backend configuration
    fn get_execution_providers(&self, config: &OnnxLoadConfig) -> Vec<ExecutionProvider> {
        let mut providers = vec![];

        // Add execution providers based on config
        if let Some(exec_providers) = &config.execution_providers {
            for provider in exec_providers {
                match provider {
                    super::onnx_config::ExecutionProvider::CoreML => {
                        #[cfg(target_os = "macos")]
                        {
                            info!("Requesting CoreML execution provider");
                            providers.push(ExecutionProvider::CoreML(Default::default()));
                        }
                    }
                    super::onnx_config::ExecutionProvider::CUDA => {
                        #[cfg(feature = "cuda")]
                        {
                            info!("Requesting CUDA execution provider");
                            providers.push(ExecutionProvider::CUDA(Default::default()));
                        }
                    }
                    super::onnx_config::ExecutionProvider::CPU => {
                        info!("Requesting CPU execution provider");
                        providers.push(ExecutionProvider::CPU(Default::default()));
                    }
                }
            }
        } else {
            // Default: Try platform-specific providers first, then CPU
            #[cfg(target_os = "macos")]
            {
                info!("Auto-selecting: Trying CoreML");
                providers.push(ExecutionProvider::CoreML(Default::default()));
            }

            #[cfg(feature = "cuda")]
            {
                info!("Auto-selecting: Trying CUDA");
                providers.push(ExecutionProvider::CUDA(Default::default()));
            }
        }

        // Always add CPU as fallback
        info!("Adding CPU as fallback provider");
        providers.push(ExecutionProvider::CPU(Default::default()));

        providers
    }

    /// Load ONNX model with progress tracking
    pub fn load_model_sync(
        &self,
        config: &OnnxLoadConfig,
    ) -> Result<LoadedOnnxModel> {
        info!("Loading ONNX model: {}", config.model_name);

        // Step 1: Download model files from HuggingFace
        let (model_dir, _progress_rx) = self.download_model_files(config)?;

        // Step 2: Find model.onnx file
        let model_path = model_dir.join("model.onnx");
        if !model_path.exists() {
            bail!("ONNX model file not found: {:?}", model_path);
        }

        info!("Found ONNX model at: {:?}", model_path);

        // Step 3: Load tokenizer
        let tokenizer = self.load_tokenizer(&model_dir)?;

        // Step 4: Create ONNX Runtime session
        let session = self.create_session(&model_path, config)?;

        info!("Successfully loaded ONNX model: {}", config.model_name);

        Ok(LoadedOnnxModel {
            session,
            tokenizer,
            model_name: config.model_name.clone(),
            model_size: config.size,
            model_path,
        })
    }

    /// Download model files from HuggingFace Hub
    fn download_model_files(
        &self,
        config: &OnnxLoadConfig,
    ) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)> {
        let repo = config.huggingface_repo();
        info!("Downloading from HuggingFace: {}", repo);

        let downloader = ModelDownloader::new()?;

        // Estimate size based on model size
        let estimated_size_gb = match config.size {
            ModelSize::Small => 0.5,
            ModelSize::Medium => 1.5,
            ModelSize::Large => 3.0,
            ModelSize::XLarge => 7.0,
        };

        // Download model files (model.onnx + model.onnx_data if exists)
        let (model_dir, progress_rx) = downloader
            .download_model(&repo, estimated_size_gb)
            .context("Failed to download ONNX model")?;

        Ok((model_dir, progress_rx))
    }

    // TODO Phase 3: Implement ONNX Runtime session creation
    // This will require:
    // 1. Creating ort::Session from model file
    // 2. Configuring execution providers (CoreML/CUDA/CPU)
    // 3. Setting optimization levels and threading

    /// Load tokenizer from model directory
    fn load_tokenizer(&self, model_dir: &Path) -> Result<Tokenizer> {
        let tokenizer_path = model_dir.join("tokenizer.json");

        if !tokenizer_path.exists() {
            bail!("Tokenizer file not found: {:?}", tokenizer_path);
        }

        let tokenizer = Tokenizer::from_file(&tokenizer_path)
            .map_err(|e| anyhow::anyhow!("Failed to load tokenizer from {:?}: {}", tokenizer_path, e))?;

        debug!("Tokenizer loaded successfully");
        Ok(tokenizer)
    }
}

/// Loaded ONNX model with tokenizer
pub struct LoadedOnnxModel {
    session: Session,
    tokenizer: Tokenizer,
    model_name: String,
    model_size: ModelSize,
    model_path: PathBuf,
}

impl LoadedOnnxModel {
    /// Get model name
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Get model size
    pub fn model_size(&self) -> ModelSize {
        self.model_size
    }

    /// Generate text from prompt
    ///
    /// NOTE: This is a placeholder for Phase 2.
    /// Full implementation in Phase 3 will handle:
    /// - ONNX Runtime session creation and inference
    /// - Streaming generation
    /// - Proper sampling (temperature, top_p, etc.)
    /// - Attention masks and position IDs
    /// - KV cache management
    /// - Stop tokens
    pub fn generate(&self, prompt: &str, _max_tokens: usize) -> Result<String> {
        info!("Generating response for prompt (placeholder)");

        // Step 1: Tokenize input (verify tokenizer works)
        let encoding = self
            .tokenizer
            .encode(prompt, true)
            .map_err(|e| anyhow::anyhow!("Failed to encode prompt: {}", e))?;

        let input_ids = encoding.get_ids();
        debug!("Input tokens: {} tokens", input_ids.len());

        // For Phase 2, return placeholder indicating ONNX structure is in place
        warn!("ONNX generation not yet fully implemented - returning placeholder");
        Ok(format!(
            "[ONNX placeholder - model: {}, tokenized {} tokens]",
            self.model_name,
            input_ids.len()
        ))
    }

    /// Get tokenizer reference
    pub fn tokenizer(&self) -> &Tokenizer {
        &self.tokenizer
    }

    /// Get model path
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }
}

impl std::fmt::Debug for LoadedOnnxModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedOnnxModel")
            .field("model_name", &self.model_name)
            .field("model_size", &self.model_size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_providers_default() {
        let providers = ExecutionProvider::default_for_platform();
        assert!(!providers.is_empty());

        #[cfg(target_os = "macos")]
        {
            assert_eq!(providers[0], ExecutionProvider::CoreML);
            assert_eq!(providers[1], ExecutionProvider::CPU);
        }
    }
}
