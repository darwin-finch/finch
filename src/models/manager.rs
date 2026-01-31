// Model Manager - Coordinates training across all learning models
//
// Orchestrates the training pipeline, manages model versions, and coordinates
// feedback loops between different models

use super::learning::{LearningModel, ModelExpectation, ModelPrediction, ModelStats};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Coordinates training and inference across all models
pub struct ModelManager {
    models_dir: PathBuf,
    training_log: Vec<TrainingEvent>,
    max_log_size: usize,
}

/// Record of a training event
#[derive(Debug, Clone)]
pub struct TrainingEvent {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub model_name: String,
    pub query: String,
    pub prediction: Option<ModelPrediction>,
    pub expectation: String, // Description of expected outcome
    pub success: bool,
}

impl ModelManager {
    /// Create new model manager
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            models_dir,
            training_log: Vec::new(),
            max_log_size: 1000, // Keep last 1000 training events
        }
    }

    /// Get models directory
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Record a training event
    pub fn record_training(
        &mut self,
        model_name: String,
        query: String,
        prediction: Option<ModelPrediction>,
        expectation: String,
        success: bool,
    ) {
        self.training_log.push(TrainingEvent {
            timestamp: chrono::Utc::now(),
            model_name,
            query,
            prediction,
            expectation,
            success,
        });

        // Trim log if too large
        if self.training_log.len() > self.max_log_size {
            self.training_log.drain(0..100); // Remove oldest 100
        }
    }

    /// Get recent training events for a model
    pub fn recent_events(&self, model_name: &str, limit: usize) -> Vec<&TrainingEvent> {
        self.training_log
            .iter()
            .rev()
            .filter(|e| e.model_name == model_name)
            .take(limit)
            .collect()
    }

    /// Get overall training statistics
    pub fn overall_stats(&self) -> OverallStats {
        let total_events = self.training_log.len();
        let successful = self.training_log.iter().filter(|e| e.success).count();

        OverallStats {
            total_training_events: total_events,
            success_rate: if total_events > 0 {
                successful as f64 / total_events as f64
            } else {
                0.0
            },
            models_trained: self.count_unique_models(),
        }
    }

    /// Count unique models in training log
    fn count_unique_models(&self) -> usize {
        use std::collections::HashSet;
        self.training_log
            .iter()
            .map(|e| e.model_name.as_str())
            .collect::<HashSet<_>>()
            .len()
    }

    /// Train multiple models from a single query-response pair
    pub fn train_from_query_response(
        &mut self,
        query: &str,
        response: &str,
        routing_was_local: bool,
        was_successful: bool,
    ) -> Result<TrainingReport> {
        let mut report = TrainingReport::default();

        // This is where we'd call update() on each model
        // For now, just record the event
        self.record_training(
            "coordinator".to_string(),
            query.to_string(),
            None,
            format!("local={}, success={}", routing_was_local, was_successful),
            was_successful,
        );

        report.models_updated = vec!["coordinator".to_string()];
        report.success = true;

        Ok(report)
    }
}

/// Overall training statistics
#[derive(Debug, Clone)]
pub struct OverallStats {
    pub total_training_events: usize,
    pub success_rate: f64,
    pub models_trained: usize,
}

/// Report from a training cycle
#[derive(Debug, Clone, Default)]
pub struct TrainingReport {
    pub models_updated: Vec<String>,
    pub success: bool,
    pub errors: Vec<String>,
}
