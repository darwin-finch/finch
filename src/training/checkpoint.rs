// Checkpoint management system

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::models::{GeneratorModel, Saveable};

/// Checkpoint metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Checkpoint ID (timestamp-based)
    pub id: String,
    /// Timestamp of creation
    pub timestamp: DateTime<Utc>,
    /// Total queries processed at checkpoint
    pub total_queries: usize,
    /// Metrics at checkpoint time
    pub metrics: CheckpointMetrics,
    /// Path to saved generator model
    pub generator_path: PathBuf,
}

/// Metrics snapshot at checkpoint time
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMetrics {
    /// Forward rate (% of queries forwarded to Claude)
    pub forward_rate: f64,
    /// Local success rate (% of local queries that passed validation)
    pub local_success_rate: f64,
    /// Router average loss
    pub router_loss: f64,
    /// Generator average loss
    pub generator_loss: f64,
    /// Validator average loss
    pub validator_loss: f64,
}

/// Manages checkpoints (automatic snapshots for rollback)
pub struct CheckpointManager {
    /// Checkpoint directory
    checkpoint_dir: PathBuf,
    /// Maximum number of checkpoints to keep
    max_checkpoints: usize,
}

impl CheckpointManager {
    /// Create new checkpoint manager
    pub fn new(checkpoint_dir: PathBuf, max_checkpoints: usize) -> Result<Self> {
        // Create directory if it doesn't exist
        fs::create_dir_all(&checkpoint_dir).with_context(|| {
            format!(
                "Failed to create checkpoint directory: {:?}",
                checkpoint_dir
            )
        })?;

        Ok(Self {
            checkpoint_dir,
            max_checkpoints,
        })
    }

    /// Create a checkpoint
    pub fn create_checkpoint(
        &self,
        generator: &GeneratorModel,
        total_queries: usize,
        metrics: CheckpointMetrics,
    ) -> Result<Checkpoint> {
        // Generate checkpoint ID (timestamp-based)
        let timestamp = Utc::now();
        let id = timestamp.format("%Y%m%d_%H%M%S").to_string();

        // Create checkpoint subdirectory
        let checkpoint_subdir = self.checkpoint_dir.join(&id);
        fs::create_dir_all(&checkpoint_subdir).with_context(|| {
            format!(
                "Failed to create checkpoint subdirectory: {:?}",
                checkpoint_subdir
            )
        })?;

        // Save model
        let generator_path = checkpoint_subdir.join("generator.safetensors");
        generator
            .save(&generator_path)
            .context("Failed to save generator model")?;

        // Create checkpoint metadata
        let checkpoint = Checkpoint {
            id: id.clone(),
            timestamp,
            total_queries,
            metrics: metrics.clone(),
            generator_path: generator_path.clone(),
        };

        // Save checkpoint metadata
        let metadata_path = checkpoint_subdir.join("checkpoint.json");
        let metadata_json = serde_json::to_string_pretty(&checkpoint)
            .context("Failed to serialize checkpoint metadata")?;
        fs::write(&metadata_path, metadata_json)
            .with_context(|| format!("Failed to write checkpoint metadata: {:?}", metadata_path))?;

        tracing::info!(
            checkpoint_id = %id,
            total_queries = total_queries,
            forward_rate = metrics.forward_rate,
            "Created checkpoint"
        );

        // Cleanup old checkpoints
        self.cleanup_old_checkpoints()?;

        Ok(checkpoint)
    }

    /// List all available checkpoints (sorted by timestamp, newest first)
    pub fn list_checkpoints(&self) -> Result<Vec<Checkpoint>> {
        let mut checkpoints = Vec::new();

        if !self.checkpoint_dir.exists() {
            return Ok(checkpoints);
        }

        // Read all checkpoint directories
        for entry in fs::read_dir(&self.checkpoint_dir).with_context(|| {
            format!(
                "Failed to read checkpoint directory: {:?}",
                self.checkpoint_dir
            )
        })? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let metadata_path = path.join("checkpoint.json");
                if metadata_path.exists() {
                    match self.load_checkpoint_metadata(&metadata_path) {
                        Ok(checkpoint) => checkpoints.push(checkpoint),
                        Err(e) => {
                            tracing::warn!(
                                path = ?metadata_path,
                                error = %e,
                                "Failed to load checkpoint metadata"
                            );
                        }
                    }
                }
            }
        }

        // Sort by timestamp (newest first)
        checkpoints.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        Ok(checkpoints)
    }

    /// Load checkpoint metadata from file
    fn load_checkpoint_metadata(&self, path: &Path) -> Result<Checkpoint> {
        let metadata_json = fs::read_to_string(path)
            .with_context(|| format!("Failed to read checkpoint metadata: {:?}", path))?;

        let checkpoint: Checkpoint = serde_json::from_str(&metadata_json)
            .context("Failed to parse checkpoint metadata JSON")?;

        Ok(checkpoint)
    }

    /// Get the most recent checkpoint
    pub fn get_latest_checkpoint(&self) -> Result<Option<Checkpoint>> {
        let checkpoints = self.list_checkpoints()?;
        Ok(checkpoints.into_iter().next())
    }

    /// Restore from a checkpoint
    pub fn restore_checkpoint(&self, checkpoint_id: &str) -> Result<GeneratorModel> {
        // Find checkpoint
        let checkpoints = self.list_checkpoints()?;
        let checkpoint = checkpoints
            .into_iter()
            .find(|c| c.id == checkpoint_id)
            .ok_or_else(|| anyhow::anyhow!("Checkpoint not found: {}", checkpoint_id))?;

        // Load model
        let generator = GeneratorModel::load(&checkpoint.generator_path)
            .context("Failed to load generator model from checkpoint")?;

        tracing::info!(
            checkpoint_id = %checkpoint_id,
            timestamp = %checkpoint.timestamp,
            "Restored from checkpoint"
        );

        Ok(generator)
    }

    /// Delete a checkpoint
    pub fn delete_checkpoint(&self, checkpoint_id: &str) -> Result<()> {
        let checkpoint_dir = self.checkpoint_dir.join(checkpoint_id);

        if checkpoint_dir.exists() {
            fs::remove_dir_all(&checkpoint_dir).with_context(|| {
                format!(
                    "Failed to delete checkpoint directory: {:?}",
                    checkpoint_dir
                )
            })?;

            tracing::info!(checkpoint_id = %checkpoint_id, "Deleted checkpoint");
        }

        Ok(())
    }

    /// Cleanup old checkpoints (keep only max_checkpoints)
    fn cleanup_old_checkpoints(&self) -> Result<()> {
        let checkpoints = self.list_checkpoints()?;

        if checkpoints.len() > self.max_checkpoints {
            let to_delete = &checkpoints[self.max_checkpoints..];

            for checkpoint in to_delete {
                self.delete_checkpoint(&checkpoint.id)?;
            }

            tracing::info!(
                deleted = to_delete.len(),
                kept = self.max_checkpoints,
                "Cleaned up old checkpoints"
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_checkpoint_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let manager = CheckpointManager::new(temp_dir.path().to_path_buf(), 5);
        assert!(manager.is_ok());
    }

    #[test]
    fn test_checkpoint_metadata_serialization() {
        let metrics = CheckpointMetrics {
            forward_rate: 0.15,
            local_success_rate: 0.92,
            router_loss: 0.23,
            generator_loss: 1.1,
            validator_loss: 0.31,
        };

        let checkpoint = Checkpoint {
            id: "test_checkpoint".to_string(),
            timestamp: Utc::now(),
            total_queries: 1000,
            metrics,
            generator_path: PathBuf::from("/tmp/generator.safetensors"),
        };

        let json = serde_json::to_string(&checkpoint).unwrap();
        let deserialized: Checkpoint = serde_json::from_str(&json).unwrap();

        assert_eq!(checkpoint.id, deserialized.id);
        assert_eq!(checkpoint.total_queries, deserialized.total_queries);
    }
}
