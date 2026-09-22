// Progressive Bootstrap - Async model loading with instant startup
// Enables REPL to start in <100ms while model loads in background

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::generator_new::GeneratorModel;
use super::gguf_download::{ManagedGgufArtifact, ManagedGgufDownloader};
use super::progress::ModelProgress;
use super::unified_loader::{ModelFamily, ModelLoadConfig, ModelSize};
use super::GeneratorConfig;
use crate::config::ExecutionTarget;

/// Generator loading state for progressive bootstrap
#[derive(Debug, Clone)]
pub enum GeneratorState {
    /// Checking cache and selecting model
    Initializing,

    /// Downloading model (first time only)
    Downloading {
        model_name: String, // e.g., "Qwen 2.5 3B" or "Gemma 2 9B"
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
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
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
                    "Downloading {} ({} / {} bytes): {}",
                    model_name, progress.downloaded_bytes, progress.total_bytes, progress.file_name
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
    output: Option<Arc<dyn ModelProgress>>,
    download_cancellation: CancellationToken,
    huggingface_token: Option<String>,
}

impl BootstrapLoader {
    /// Create new bootstrap loader with shared state
    pub fn new(state: Arc<RwLock<GeneratorState>>, output: Option<Arc<dyn ModelProgress>>) -> Self {
        Self {
            state,
            output,
            download_cancellation: CancellationToken::new(),
            huggingface_token: None,
        }
    }

    /// Inject the configured Hugging Face credential at the composition root.
    pub fn with_huggingface_token(mut self, token: Option<String>) -> Self {
        self.huggingface_token = token.filter(|value| !value.trim().is_empty());
        self
    }

    /// Get reference to the generator state
    pub fn state(&self) -> &Arc<RwLock<GeneratorState>> {
        &self.state
    }

    /// Cancel an in-flight managed GGUF download.
    pub fn cancel_download(&self) {
        self.download_cancellation.cancel();
    }

    /// Load generator in background using UnifiedModelLoader
    pub async fn load_generator_async(
        &self,
        provider: super::unified_loader::InferenceProvider,
        model_family: ModelFamily,
        model_size: ModelSize,
        execution_target: ExecutionTarget,
        coreml: crate::config::CoreMlConfig,
        model_repo: Option<String>,
        model_path: Option<std::path::PathBuf>,
        managed_artifact: Option<ManagedGgufArtifact>,
    ) -> Result<()> {
        // Step 1: Initializing
        *self.state.write().await = GeneratorState::Initializing;

        let requested_target = {
            #[cfg(target_os = "macos")]
            {
                if execution_target == ExecutionTarget::CoreML {
                    format!("CoreML ({})", coreml.compute_units.name())
                } else {
                    execution_target.name().to_string()
                }
            }
            #[cfg(not(target_os = "macos"))]
            {
                execution_target.name().to_string()
            }
        };
        let model_name = format!(
            "{} {} ({:?}; requested {})",
            model_family.name(),
            model_size.to_size_string(model_family),
            provider,
            requested_target
        );
        tracing::info!(
            "Loading model: {} with requested policy {}",
            model_name,
            requested_target
        );
        if let Some(ref repo) = model_repo {
            tracing::info!("Using custom repository: {}", repo);
        }

        let model_path = match (model_path, managed_artifact.as_ref()) {
            (Some(path), None) => Some(path),
            (None, Some(artifact)) => {
                let downloader =
                    ManagedGgufDownloader::from_environment(self.huggingface_token.clone())?;
                let (path, _) = downloader
                    .ensure(
                        artifact,
                        &model_name,
                        Arc::clone(&self.state),
                        &self.download_cancellation,
                    )
                    .await?;
                Some(path)
            }
            (Some(_), Some(_)) => anyhow::bail!(
                "local chat config cannot select both a managed GGUF and a custom path"
            ),
            (None, None) => {
                anyhow::bail!("local chat requires a managed GGUF or an explicit .gguf path")
            }
        };

        // Step 3: Create model load config
        let load_config = ModelLoadConfig {
            provider,
            family: model_family,
            size: model_size,
            target: execution_target,
            coreml,
            repo_override: model_repo.clone(),
            model_path,
        };

        // Step 4: Load the resolved local artifact using UnifiedModelLoader.
        *self.state.write().await = GeneratorState::Loading {
            model_name: model_name.clone(),
        };

        if let Some(output) = &self.output {
            output.write_progress(format!("⏳ Loading {}...", model_name));
        }

        // Load in blocking task (model loading + potential download is CPU/IO intensive)
        let model_name_clone = model_name.clone();
        let output_clone = self.output.clone();

        let generator = tokio::task::spawn_blocking(move || {
            if let Some(output) = &output_clone {
                output.write_progress(format!("  └─ Initializing {}...", model_name_clone));
            }

            // The loader remains path-only; managed download completed above.
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
            downloaded_bytes: 1,
            total_bytes: 4,
        };

        assert_eq!(progress.file_name, "config.json");
        assert_eq!(progress.downloaded_bytes, 1);
    }

    #[tokio::test]
    async fn test_bootstrap_loader_creation() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let loader = BootstrapLoader::new(state, None);

        // Just verify creation works
        assert!(true);
    }

    #[tokio::test]
    async fn test_bootstrap_loader_accepts_injected_progress() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let loader = BootstrapLoader::new(
            Arc::clone(&state),
            Some(Arc::new(crate::models::SilentModelProgress)),
        );
        loader.set_not_available().await;
        assert!(!state.read().await.is_ready());
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
