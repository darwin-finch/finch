// Progressive Bootstrap - Async model loading with instant startup
// Enables REPL to start in <100ms while model loads in background

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::generator_new::GeneratorModel;
use crate::config::ExecutionTarget;
use super::unified_loader::{ModelFamily, ModelLoadConfig, ModelSize};
use super::GeneratorConfig;
use crate::cli::OutputManager;

/// Generator loading state for progressive bootstrap
#[derive(Debug, Clone)]
pub enum GeneratorState {
    /// Checking cache and selecting model
    Initializing,

    /// Downloading model (first time only)
    Downloading {
        model_name: String,  // e.g., "Qwen 2.5 3B" or "Gemma 2 9B"
        progress: DownloadProgressSnapshot,
    },

    /// Loading model weights into memory
    Loading { model_name: String },

    /// Model ready for use
    Ready {
        model: Arc<RwLock<GeneratorModel>>,
        model_name: String,
    },

    /// Failed to load (with error message)
    Failed { error: String },

    /// Offline mode (no network, no cached model)
    NotAvailable,
}

/// Snapshot of download progress for state updates
#[derive(Debug, Clone)]
pub struct DownloadProgressSnapshot {
    pub file_name: String,
    pub current_file: usize,
    pub total_files: usize,
}

impl GeneratorState {
    /// Check if generator is ready for use
    pub fn is_ready(&self) -> bool {
        matches!(self, GeneratorState::Ready { .. })
    }

    /// Get human-readable status message
    pub fn status_message(&self) -> String {
        match self {
            GeneratorState::Initializing => "Initializing...".to_string(),
            GeneratorState::Downloading {
                model_name,
                progress,
            } => {
                format!(
                    "Downloading {} ({}/{}): {}",
                    model_name,
                    progress.current_file,
                    progress.total_files,
                    progress.file_name
                )
            }
            GeneratorState::Loading { model_name } => {
                format!("Loading {}...", model_name)
            }
            GeneratorState::Ready { model_name, .. } => {
                format!("✓ {} ready", model_name)
            }
            GeneratorState::Failed { error } => {
                format!("✗ Failed: {}", error)
            }
            GeneratorState::NotAvailable => "⚠ Offline mode - forwarding to Claude".to_string(),
        }
    }
}

/// Background task that loads generator asynchronously
pub struct BootstrapLoader {
    state: Arc<RwLock<GeneratorState>>,
    output: Option<Arc<OutputManager>>,
}

impl BootstrapLoader {
    /// Create new bootstrap loader with shared state
    pub fn new(state: Arc<RwLock<GeneratorState>>, output: Option<Arc<OutputManager>>) -> Self {
        Self { state, output }
    }

    /// Get reference to the generator state
    pub fn state(&self) -> &Arc<RwLock<GeneratorState>> {
        &self.state
    }

    /// Check if HuggingFace token exists and is valid
    fn check_hf_token() -> Result<()> {
        let token_path = dirs::cache_dir()
            .ok_or_else(|| anyhow!("Could not determine cache directory"))?
            .join("huggingface")
            .join("token");

        if !token_path.exists() {
            return Err(anyhow!(
                "HuggingFace token not found at {:?}\n\
                 \n\
                 Shammah needs a HuggingFace token to download Qwen models.\n\
                 \n\
                 Please follow these steps:\n\
                 1. Create a token at https://huggingface.co/settings/tokens\n\
                 2. Save it: echo \"hf_YOUR_TOKEN\" > ~/.cache/huggingface/token\n\
                 3. Restart Shammah\n\
                 \n\
                 See README.md for detailed instructions.",
                token_path
            ));
        }

        // Validate token format (should start with hf_)
        let token = std::fs::read_to_string(&token_path)
            .context("Failed to read HuggingFace token file")?;

        let token = token.trim();
        if !token.starts_with("hf_") {
            return Err(anyhow!(
                "Invalid HuggingFace token format in {:?}\n\
                 Token should start with 'hf_'\n\
                 Get a new token at https://huggingface.co/settings/tokens",
                token_path
            ));
        }

        Ok(())
    }

    /// Load generator in background using UnifiedModelLoader
    pub async fn load_generator_async(
        &self,
        provider: super::unified_loader::InferenceProvider,
        model_family: ModelFamily,
        model_size: ModelSize,
        execution_target: ExecutionTarget,
        model_repo: Option<String>,
    ) -> Result<()> {
        // Step 1: Initializing
        *self.state.write().await = GeneratorState::Initializing;

        let model_name = format!(
            "{} {} ({:?})",
            model_family.name(),
            model_size.to_size_string(model_family),
            provider
        );

        tracing::info!("Loading model: {} on {:?}", model_name, execution_target);
        if let Some(ref repo) = model_repo {
            tracing::info!("Using custom repository: {}", repo);
        }

        // Step 3: Create model load config
        let load_config = ModelLoadConfig {
            provider,
            family: model_family,
            size: model_size,
            target: execution_target,
            repo_override: model_repo.clone(),
        };

        // Step 4: Load using UnifiedModelLoader (handles download + loading)
        *self.state.write().await = GeneratorState::Loading {
            model_name: model_name.clone(),
        };

        if let Some(output) = &self.output {
            output.write_progress(format!("⏳ Loading {}...", model_name));
        }

        // Check HF token before attempting (UnifiedModelLoader will download if needed)
        if let Err(e) = Self::check_hf_token() {
            tracing::warn!("HuggingFace token check failed: {}. Model must be cached.", e);
            // Don't fail here - model might be cached
        }

        // Load in blocking task (model loading + potential download is CPU/IO intensive)
        let model_name_clone = model_name.clone();
        let output_clone = self.output.clone();

        let generator = tokio::task::spawn_blocking(move || {
            if let Some(output) = &output_clone {
                output.write_progress(format!("  └─ Initializing {}...", model_name_clone));
            }

            // GeneratorModel::new() handles download + loading internally
            let config = GeneratorConfig::Pretrained(load_config);
            GeneratorModel::new(config)
        })
        .await??;

        // Step 5: Ready! (wrap in Arc<RwLock> for shared mutable access)
        *self.state.write().await = GeneratorState::Ready {
            model: Arc::new(RwLock::new(generator)),
            model_name: model_name.clone(),
        };

        tracing::info!("✓ Generator ready: {}", model_name);
        if let Some(output) = &self.output {
            output.write_progress(format!("✓ {} ready", model_name));
        }

        Ok(())
    }

    /// Handle loading errors gracefully
    pub async fn handle_error(&self, error: anyhow::Error) {
        let error_msg = format!("{:#}", error);
        tracing::error!("Generator loading failed: {}", error_msg);

        *self.state.write().await = GeneratorState::Failed {
            error: error_msg.clone(),
        };
    }

    /// Set state to not available (offline mode)
    pub async fn set_not_available(&self) {
        *self.state.write().await = GeneratorState::NotAvailable;
    }

    /// Find snapshot directory within cache path
    #[allow(dead_code)]
    fn find_snapshot_dir(cache_path: &PathBuf) -> Result<PathBuf> {
        // Check if cache_path itself is valid
        if cache_path.join("config.json").exists() {
            return Ok(cache_path.clone());
        }

        // Look for snapshot subdirectory
        if let Ok(entries) = std::fs::read_dir(cache_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("config.json").exists() {
                    return Ok(path);
                }
            }
        }

        Err(anyhow::anyhow!(
            "Could not find valid model snapshot in {:?}",
            cache_path
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generator_state_transitions() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));

        // Check initial state
        assert!(!state.read().await.is_ready());

        // Transition to loading
        *state.write().await = GeneratorState::Loading {
            model_name: "Qwen2.5-1.5B-Instruct".to_string(),
        };
        assert!(!state.read().await.is_ready());

        // Status messages
        assert!(state.read().await.status_message().contains("Loading"));
    }

    #[test]
    fn test_download_progress_snapshot() {
        let progress = DownloadProgressSnapshot {
            file_name: "config.json".to_string(),
            current_file: 1,
            total_files: 4,
        };

        assert_eq!(progress.file_name, "config.json");
        assert_eq!(progress.current_file, 1);
    }

    #[tokio::test]
    async fn test_bootstrap_loader_creation() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let loader = BootstrapLoader::new(state, None);

        // Just verify creation works
        assert!(true);
    }

    #[tokio::test]
    async fn test_not_available_state() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let loader = BootstrapLoader::new(Arc::clone(&state), None);

        loader.set_not_available().await;

        assert!(!state.read().await.is_ready());
        assert!(state.read().await.status_message().contains("Offline"));
    }
}
