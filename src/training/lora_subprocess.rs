// LoRA training subprocess spawner
//
// Triggers Python training script in background (non-blocking)

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// Configuration for LoRA training subprocess
#[derive(Debug, Clone)]
pub struct LoRATrainingConfig {
    /// Path to Python training script
    pub script_path: PathBuf,

    /// Base model name or path (e.g., "Qwen/Qwen2.5-1.5B-Instruct")
    pub base_model: String,

    /// LoRA rank (default: 16)
    pub rank: usize,

    /// LoRA alpha (default: 32.0)
    pub alpha: f64,

    /// LoRA dropout (default: 0.05)
    pub dropout: f64,

    /// Training epochs (default: 3)
    pub epochs: usize,

    /// Batch size (default: 4)
    pub batch_size: usize,

    /// Learning rate (default: 1e-4)
    pub learning_rate: f64,
}

impl Default for LoRATrainingConfig {
    fn default() -> Self {
        // Find script path (assuming it's in the repository)
        let script_path = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
            .and_then(|dir| {
                // Try release build location
                if dir.ends_with("release") {
                    dir.parent()
                        .and_then(|p| p.parent())
                        .map(|p| p.join("scripts/train_lora.py"))
                } else if dir.ends_with("debug") {
                    dir.parent()
                        .and_then(|p| p.parent())
                        .map(|p| p.join("scripts/train_lora.py"))
                } else {
                    Some(dir.join("scripts/train_lora.py"))
                }
            })
            .unwrap_or_else(|| PathBuf::from("scripts/train_lora.py"));

        Self {
            script_path,
            base_model: "Qwen/Qwen2.5-1.5B-Instruct".to_string(),
            rank: 16,
            alpha: 32.0,
            dropout: 0.05,
            epochs: 3,
            batch_size: 4,
            learning_rate: 1e-4,
        }
    }
}

/// LoRA training subprocess manager
pub struct LoRATrainingSubprocess {
    config: LoRATrainingConfig,
}

impl LoRATrainingSubprocess {
    /// Create new subprocess manager with configuration
    pub fn new(config: LoRATrainingConfig) -> Self {
        Self { config }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(LoRATrainingConfig::default())
    }

    /// Trigger background training (non-blocking)
    ///
    /// Spawns Python training script as a detached subprocess.
    /// Does not block - training runs in background.
    ///
    /// # Arguments
    /// * `queue_path` - Path to training queue JSONL file
    /// * `output_adapter` - Path to save trained adapter (safetensors)
    ///
    /// # Returns
    /// Ok(()) if subprocess spawned successfully
    pub async fn train_async(&self, queue_path: &Path, output_adapter: &Path) -> Result<()> {
        // Validate inputs
        if !queue_path.exists() {
            anyhow::bail!("Training queue not found: {}", queue_path.display());
        }

        if !self.config.script_path.exists() {
            anyhow::bail!(
                "Training script not found: {}. Please ensure scripts/train_lora.py exists.",
                self.config.script_path.display()
            );
        }

        tracing::info!(
            "Starting LoRA training subprocess: {} → {}",
            queue_path.display(),
            output_adapter.display()
        );

        // Build command
        let mut cmd = Command::new("python3");
        cmd.arg(&self.config.script_path)
            .arg(queue_path)
            .arg(output_adapter)
            .arg("--base-model")
            .arg(&self.config.base_model)
            .arg("--rank")
            .arg(self.config.rank.to_string())
            .arg("--alpha")
            .arg(self.config.alpha.to_string())
            .arg("--dropout")
            .arg(self.config.dropout.to_string())
            .arg("--epochs")
            .arg(self.config.epochs.to_string())
            .arg("--batch-size")
            .arg(self.config.batch_size.to_string())
            .arg("--learning-rate")
            .arg(self.config.learning_rate.to_string());

        // Redirect output to log file
        let log_path = output_adapter.with_extension("training.log");
        let log_file =
            std::fs::File::create(&log_path).context("Failed to create training log file")?;

        cmd.stdout(Stdio::from(log_file.try_clone()?))
            .stderr(Stdio::from(log_file));

        tracing::info!("Training logs will be written to: {}", log_path.display());

        // Spawn and detach (non-blocking)
        let queue_path_owned = queue_path.to_path_buf();
        let output_adapter_owned = output_adapter.to_path_buf();
        let log_path_owned = log_path;

        tokio::spawn(async move {
            match cmd.spawn() {
                Ok(mut child) => {
                    tracing::info!("✅ Training subprocess spawned (PID: {:?})", child.id());

                    match child.wait().await {
                        Ok(status) if status.success() => {
                            tracing::info!(
                                "✅ LoRA training completed successfully: {}",
                                output_adapter_owned.display()
                            );

                            // Archive training queue after successful training
                            if let Err(e) = archive_training_queue(&queue_path_owned) {
                                tracing::warn!("Failed to archive training queue: {}", e);
                            }
                        }
                        Ok(status) => {
                            tracing::error!(
                                "❌ LoRA training failed with status: {:?}. Check log: {}",
                                status,
                                log_path_owned.display()
                            );
                        }
                        Err(e) => {
                            tracing::error!("Failed to wait for training subprocess: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to spawn training subprocess: {}", e);
                    tracing::error!("Make sure Python 3 and required packages are installed:");
                    tracing::error!("  pip install -r scripts/requirements.txt");
                }
            }
        });

        tracing::info!("Training subprocess launched in background");

        Ok(())
    }

    /// Check if Python dependencies are installed
    pub async fn check_dependencies(&self) -> Result<bool> {
        let output = Command::new("python3")
            .arg("-c")
            .arg("import torch, transformers, peft, safetensors; print('OK')")
            .output()
            .await
            .context("Failed to execute Python check")?;

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.trim() == "OK")
        } else {
            Ok(false)
        }
    }

    /// Get configuration
    pub fn config(&self) -> &LoRATrainingConfig {
        &self.config
    }
}

/// Archive training queue after successful training
fn archive_training_queue(queue_path: &Path) -> Result<()> {
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let archive_path =
        queue_path.with_file_name(format!("training_queue_archive_{}.jsonl", timestamp));

    std::fs::rename(queue_path, &archive_path).context("Failed to archive training queue")?;

    tracing::info!("Archived training queue to: {}", archive_path.display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = LoRATrainingConfig::default();
        assert_eq!(config.rank, 16);
        assert_eq!(config.alpha, 32.0);
        assert_eq!(config.epochs, 3);
        assert_eq!(config.batch_size, 4);
    }

    #[test]
    fn test_subprocess_creation() {
        let subprocess = LoRATrainingSubprocess::with_defaults();
        assert_eq!(subprocess.config().rank, 16);
    }

    #[tokio::test]
    async fn test_check_dependencies_runs() {
        let subprocess = LoRATrainingSubprocess::with_defaults();
        // Just check that the function runs (may fail if Python not installed)
        let _ = subprocess.check_dependencies().await;
    }
}
