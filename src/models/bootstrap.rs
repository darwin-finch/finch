// Progressive Bootstrap - Async model loading with instant startup
// Enables REPL to start in <100ms while model loads in background

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::download::{DownloadProgress, ModelDownloader};
use super::generator_new::GeneratorModel;
use super::model_selector::{ModelSelector, QwenSize};
use super::{DevicePreference, GeneratorConfig};

/// Generator loading state for progressive bootstrap
#[derive(Debug, Clone)]
pub enum GeneratorState {
    /// Checking cache and selecting model
    Initializing,

    /// Downloading model (first time only)
    Downloading {
        model_size: QwenSize,
        progress: DownloadProgressSnapshot,
    },

    /// Loading model weights into memory
    Loading { model_size: QwenSize },

    /// Model ready for use
    Ready {
        model: Arc<RwLock<GeneratorModel>>,
        model_size: QwenSize,
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
                model_size,
                progress,
            } => {
                format!(
                    "Downloading {} ({}/{}): {}",
                    model_size.description(),
                    progress.current_file,
                    progress.total_files,
                    progress.file_name
                )
            }
            GeneratorState::Loading { model_size } => {
                format!("Loading {}...", model_size.description())
            }
            GeneratorState::Ready { model_size, .. } => {
                format!("✓ {} ready", model_size.description())
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
}

impl BootstrapLoader {
    /// Create new bootstrap loader with shared state
    pub fn new(state: Arc<RwLock<GeneratorState>>) -> Self {
        Self { state }
    }

    /// Load generator in background (blocking operation, run in tokio::task::spawn_blocking)
    pub async fn load_generator_async(
        &self,
        override_model: Option<QwenSize>,
        device_preference: DevicePreference,
    ) -> Result<()> {
        // Step 1: Initializing
        *self.state.write().await = GeneratorState::Initializing;

        // Step 2: Select model based on RAM (or use override)
        let model_size = match override_model {
            Some(size) => {
                tracing::info!("Using manual override: {}", size.description());
                size
            }
            None => ModelSelector::select_model_for_system().map_err(|e| {
                tracing::error!("Failed to select model: {}", e);
                e
            })?,
        };

        tracing::info!("Selected model: {}", model_size.description());

        // Step 3: Check if model is cached
        let downloader = ModelDownloader::new()?;
        let is_cached = downloader.is_cached(model_size);

        let cache_path = if !is_cached {
            // Step 4: Download if not cached (this is the slow part)
            tracing::info!("Model not cached, downloading...");

            // Update state to downloading
            *self.state.write().await = GeneratorState::Downloading {
                model_size,
                progress: DownloadProgressSnapshot {
                    file_name: "Preparing...".to_string(),
                    current_file: 0,
                    total_files: 4,
                },
            };

            // Spawn blocking task for download (hf-hub is synchronous)
            let state_clone = Arc::clone(&self.state);
            let model_size_clone = model_size;

            let result = tokio::task::spawn_blocking(move || {
                let downloader = ModelDownloader::new()?;
                let (path, rx) = downloader.download_qwen_model(model_size_clone)?;

                // Update progress from channel
                for progress_event in rx.iter() {
                    match progress_event {
                        DownloadProgress::Downloading {
                            file_name,
                            current_file,
                            total_files,
                            ..
                        } => {
                            // Update state with progress
                            if let Ok(mut state) = state_clone.try_write() {
                                *state = GeneratorState::Downloading {
                                    model_size: model_size_clone,
                                    progress: DownloadProgressSnapshot {
                                        file_name,
                                        current_file,
                                        total_files,
                                    },
                                };
                            }
                        }
                        DownloadProgress::Complete { .. } => {
                            tracing::info!("Download complete");
                        }
                        DownloadProgress::Error { error, .. } => {
                            tracing::error!("Download error: {}", error);
                        }
                        _ => {}
                    }
                }

                Ok::<PathBuf, anyhow::Error>(path)
            })
            .await??;

            result
        } else {
            // Model is cached, get cache path
            tracing::info!("Model cached, loading from disk...");
            downloader.cache_dir().join(format!(
                "hub/models--Qwen--{}/snapshots",
                model_size.model_id().replace('/', "--")
            ))
        };

        // Step 5: Load model (this is also slow, ~2-5 seconds)
        *self.state.write().await = GeneratorState::Loading { model_size };

        // Find the snapshot directory
        let snapshot_dir = Self::find_snapshot_dir(&cache_path)?;

        // Build generator config
        let config = GeneratorConfig::Qwen {
            model_size,
            cache_dir: snapshot_dir,
            device_preference,
        };

        // Load in blocking task (model loading is CPU-intensive)
        let generator = tokio::task::spawn_blocking(move || GeneratorModel::new(config)).await??;

        // Step 6: Ready! (wrap in Arc<RwLock> for shared mutable access)
        *self.state.write().await = GeneratorState::Ready {
            model: Arc::new(RwLock::new(generator)),
            model_size,
        };

        tracing::info!("✓ Generator ready: {}", model_size.description());

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
            model_size: QwenSize::Qwen1_5B,
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
        let loader = BootstrapLoader::new(state);

        // Just verify creation works
        assert!(true);
    }

    #[tokio::test]
    async fn test_not_available_state() {
        let state = Arc::new(RwLock::new(GeneratorState::Initializing));
        let loader = BootstrapLoader::new(Arc::clone(&state));

        loader.set_not_available().await;

        assert!(!state.read().await.is_ready());
        assert!(state.read().await.status_message().contains("Offline"));
    }
}
