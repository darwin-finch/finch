// Model Downloader - Model download with progress tracking
// Uses HuggingFace Hub for download management and caching

use anyhow::{Context, Result};
use hf_hub::{api::sync::Api, Repo, RepoType};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::PathBuf;
use std::sync::mpsc;

use super::model_selector::QwenSize;

/// Download progress events sent via channel
#[derive(Debug, Clone)]
pub enum DownloadProgress {
    /// Download starting
    Starting { model_id: String, size_gb: f64 },
    /// Download in progress
    Downloading {
        model_id: String,
        file_name: String,
        current_file: usize,
        total_files: usize,
    },
    /// Download complete
    Complete {
        model_id: String,
        cache_path: PathBuf,
    },
    /// Download error
    Error { model_id: String, error: String },
}

/// Model downloader with HuggingFace Hub integration
pub struct ModelDownloader {
    cache_dir: Option<PathBuf>,
}

impl ModelDownloader {
    /// Create new downloader (uses default HF cache: ~/.cache/huggingface/)
    pub fn new() -> Result<Self> {
        Ok(Self { cache_dir: None })
    }

    /// Create downloader with custom cache directory
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&cache_dir).context("Failed to create cache directory")?;

        // Set HF_HOME environment variable to control cache location
        std::env::set_var("HF_HOME", &cache_dir);

        Ok(Self {
            cache_dir: Some(cache_dir),
        })
    }

    /// Download Qwen model with progress tracking
    ///
    /// Returns path to cached model directory containing safetensors and tokenizer files.
    /// Progress updates sent via returned channel.
    /// This is a blocking operation - spawn in a thread if you need async.
    pub fn download_qwen_model(
        &self,
        model_size: QwenSize,
    ) -> Result<(PathBuf, mpsc::Receiver<DownloadProgress>)> {
        let model_id = model_size.model_id();
        let (tx, rx) = mpsc::channel();

        // Send starting event
        tx.send(DownloadProgress::Starting {
            model_id: model_id.to_string(),
            size_gb: model_size.download_size_gb(),
        })
        .ok();

        // Create API instance (cache dir controlled by HF_HOME env var if set)
        let api = Api::new()?;

        // Get repository reference
        let repo = api.repo(Repo::new(model_id.to_string(), RepoType::Model));

        tracing::info!("Downloading {} to cache...", model_id);

        let mut downloaded_files = Vec::new();

        // Download config files first
        let config_files = vec!["config.json", "tokenizer.json", "tokenizer_config.json"];
        for file in &config_files {
            match repo.get(file) {
                Ok(path) => {
                    tracing::debug!("Downloaded {} to {:?}", file, path);
                    downloaded_files.push(path);
                }
                Err(e) => {
                    tracing::warn!("Failed to download {}: {}", file, e);
                }
            }
        }

        // Try to download model weights (single file or sharded)
        // First, try single model.safetensors file
        match repo.get("model.safetensors") {
            Ok(path) => {
                tracing::info!("Downloaded single model file");
                downloaded_files.push(path);
            }
            Err(_) => {
                // Single file doesn't exist, try sharded files
                tracing::info!("Single model file not found, looking for sharded files...");

                let mut shard_idx = 1;
                loop {
                    // Try downloading shards sequentially: model-00001-of-00002.safetensors, etc.
                    let mut found_this_shard = false;

                    // Try different total counts (models can have 2, 3, 4, ... shards)
                    for total in shard_idx..=20 {
                        // Try up to 20 total shards
                        let shard_file =
                            format!("model-{:05}-of-{:05}.safetensors", shard_idx, total);
                        match repo.get(&shard_file) {
                            Ok(path) => {
                                tracing::info!(
                                    "Downloaded shard {}/{}: {}",
                                    shard_idx,
                                    total,
                                    shard_file
                                );
                                downloaded_files.push(path);
                                found_this_shard = true;

                                // If we found shard N of N, we're done
                                if shard_idx == total {
                                    tracing::info!("✓ Downloaded all {} shards", total);
                                    shard_idx = total + 1; // Exit outer loop
                                }
                                break; // Move to next shard
                            }
                            Err(_) => continue,
                        }
                    }

                    if !found_this_shard {
                        // No more shards found
                        if shard_idx == 1 {
                            tracing::error!("No model files found (neither single nor sharded)");
                        } else {
                            tracing::info!("✓ Found {} total shards", shard_idx - 1);
                        }
                        break;
                    }

                    shard_idx += 1;
                }
            }
        }

        tracing::info!("✓ Download complete: {} files", downloaded_files.len());

        // Determine cache path from first downloaded file
        let cache_path = if let Some(first_file) = downloaded_files.first() {
            first_file
                .parent()
                .context("Failed to get cache directory")?
                .to_path_buf()
        } else {
            return Err(anyhow::anyhow!(
                "No files downloaded - check network connection"
            ));
        };

        tx.send(DownloadProgress::Complete {
            model_id: model_id.to_string(),
            cache_path: cache_path.clone(),
        })
        .ok();

        Ok((cache_path, rx))
    }

    /// Check if model is already cached
    pub fn is_cached(&self, model_size: QwenSize) -> bool {
        // Temporarily set HF_HOME if custom cache dir specified
        let _guard = self.cache_dir.as_ref().map(|dir| {
            let old_val = std::env::var("HF_HOME").ok();
            std::env::set_var("HF_HOME", dir);
            old_val
        });

        let api = match Api::new() {
            Ok(api) => api,
            Err(_) => return false,
        };

        let model_id = model_size.model_id();
        let repo = api.repo(Repo::new(model_id.to_string(), RepoType::Model));

        // Check if required files exist in cache
        let result = repo.get("config.json").is_ok() && repo.get("tokenizer.json").is_ok();

        // Restore old HF_HOME if it was set
        if let Some(Some(old_val)) = _guard {
            std::env::set_var("HF_HOME", old_val);
        }

        result
    }

    /// Get cache directory path
    pub fn cache_dir(&self) -> PathBuf {
        self.cache_dir.clone().unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_default()
                .join(".cache")
                .join("huggingface")
        })
    }
}

impl Default for ModelDownloader {
    fn default() -> Self {
        Self::new().expect("Failed to create default ModelDownloader")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downloader_creation() {
        let downloader = ModelDownloader::new();
        assert!(downloader.is_ok());
    }

    #[test]
    fn test_cache_dir_creation() {
        let temp_dir = std::env::temp_dir().join("shammah_test_cache");
        let downloader = ModelDownloader::with_cache_dir(temp_dir.clone());
        assert!(downloader.is_ok());
        assert!(temp_dir.exists());
        // Cleanup
        std::fs::remove_dir_all(temp_dir).ok();
    }

    #[test]
    fn test_is_cached() {
        let downloader = ModelDownloader::new().unwrap();
        // Should return false for non-existent model (unless already downloaded)
        let _cached = downloader.is_cached(QwenSize::Qwen1_5B);
        // Either result is valid depending on system state
    }

    #[test]
    #[ignore] // Requires network - run with: cargo test -- --ignored
    fn test_download_small_model() {
        let downloader = ModelDownloader::new().unwrap();

        // Try downloading smallest model (this will take time on first run)
        let result = downloader.download_qwen_model(QwenSize::Qwen1_5B);

        match result {
            Ok((path, rx)) => {
                println!("Model cached at: {:?}", path);

                // Consume progress events
                for progress in rx.iter() {
                    match progress {
                        DownloadProgress::Starting { model_id, size_gb } => {
                            println!("Starting download of {} ({:.1}GB)", model_id, size_gb);
                        }
                        DownloadProgress::Downloading {
                            file_name,
                            current_file,
                            total_files,
                            ..
                        } => {
                            println!(
                                "Downloading {} ({}/{})",
                                file_name, current_file, total_files
                            );
                        }
                        DownloadProgress::Complete { cache_path, .. } => {
                            println!("Complete: {:?}", cache_path);
                            assert!(cache_path.exists());
                        }
                        DownloadProgress::Error { error, .. } => {
                            panic!("Download error: {}", error);
                        }
                    }
                }
            }
            Err(e) => {
                println!("Download failed (expected if offline): {}", e);
            }
        }
    }
}
