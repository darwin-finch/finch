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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager() -> ModelManager {
        ModelManager::new(PathBuf::from("/tmp/test_models"))
    }

    // --- ModelManager construction ---

    #[test]
    fn test_new_manager_has_empty_log() {
        let manager = make_manager();
        assert_eq!(manager.training_log.len(), 0);
    }

    #[test]
    fn test_models_dir_stored_correctly() {
        let dir = PathBuf::from("/custom/path");
        let manager = ModelManager::new(dir.clone());
        assert_eq!(manager.models_dir(), dir.as_path());
    }

    // --- record_training ---

    #[test]
    fn test_record_training_appends_event() {
        let mut manager = make_manager();
        manager.record_training("router".to_string(), "test query".to_string(), None, "expected".to_string(), true);
        assert_eq!(manager.training_log.len(), 1);
        let event = &manager.training_log[0];
        assert_eq!(event.model_name, "router");
        assert_eq!(event.query, "test query");
        assert!(event.success);
    }

    #[test]
    fn test_record_training_timestamp_is_recent() {
        let mut manager = make_manager();
        let before = chrono::Utc::now();
        manager.record_training("m".to_string(), "q".to_string(), None, "e".to_string(), false);
        let after = chrono::Utc::now();
        let ts = manager.training_log[0].timestamp;
        assert!(ts >= before && ts <= after, "timestamp should be between before and after");
    }

    #[test]
    fn test_record_training_trims_log_at_max_size() {
        let mut manager = ModelManager::new(PathBuf::from("/tmp"));
        // Fill exactly to max + 50 to trigger trim
        for i in 0..1050 {
            manager.record_training(
                "model".to_string(),
                format!("query {i}"),
                None,
                "exp".to_string(),
                i % 2 == 0,
            );
        }
        // After 1050 inserts: first trim at 1001 removes 100 → 901, then continues to 1001 again...
        // The log should be ≤ max_log_size (1000) + 100 (trim removes 100 at a time)
        assert!(manager.training_log.len() <= 1000, "log should not exceed max after trim");
    }

    // --- recent_events ---

    #[test]
    fn test_recent_events_filters_by_model_name() {
        let mut manager = make_manager();
        manager.record_training("router".to_string(), "q1".to_string(), None, "e".to_string(), true);
        manager.record_training("validator".to_string(), "q2".to_string(), None, "e".to_string(), true);
        manager.record_training("router".to_string(), "q3".to_string(), None, "e".to_string(), false);

        let router_events = manager.recent_events("router", 10);
        assert_eq!(router_events.len(), 2, "only router events should be returned");
        assert!(router_events.iter().all(|e| e.model_name == "router"));
    }

    #[test]
    fn test_recent_events_returns_in_reverse_order() {
        let mut manager = make_manager();
        manager.record_training("m".to_string(), "first".to_string(), None, "e".to_string(), true);
        manager.record_training("m".to_string(), "second".to_string(), None, "e".to_string(), true);
        manager.record_training("m".to_string(), "third".to_string(), None, "e".to_string(), true);

        let events = manager.recent_events("m", 10);
        assert_eq!(events[0].query, "third", "most recent should come first");
        assert_eq!(events[2].query, "first");
    }

    #[test]
    fn test_recent_events_respects_limit() {
        let mut manager = make_manager();
        for i in 0..10 {
            manager.record_training("m".to_string(), format!("q{i}"), None, "e".to_string(), true);
        }
        let events = manager.recent_events("m", 3);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_recent_events_empty_for_unknown_model() {
        let mut manager = make_manager();
        manager.record_training("known".to_string(), "q".to_string(), None, "e".to_string(), true);
        assert!(manager.recent_events("unknown", 10).is_empty());
    }

    // --- overall_stats ---

    #[test]
    fn test_overall_stats_empty_manager() {
        let manager = make_manager();
        let stats = manager.overall_stats();
        assert_eq!(stats.total_training_events, 0);
        assert_eq!(stats.success_rate, 0.0);
        assert_eq!(stats.models_trained, 0);
    }

    #[test]
    fn test_overall_stats_success_rate_calculation() {
        let mut manager = make_manager();
        manager.record_training("m".to_string(), "q1".to_string(), None, "e".to_string(), true);
        manager.record_training("m".to_string(), "q2".to_string(), None, "e".to_string(), true);
        manager.record_training("m".to_string(), "q3".to_string(), None, "e".to_string(), false);

        let stats = manager.overall_stats();
        assert_eq!(stats.total_training_events, 3);
        assert!((stats.success_rate - 2.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_overall_stats_counts_unique_models() {
        let mut manager = make_manager();
        manager.record_training("router".to_string(), "q".to_string(), None, "e".to_string(), true);
        manager.record_training("router".to_string(), "q".to_string(), None, "e".to_string(), true);
        manager.record_training("validator".to_string(), "q".to_string(), None, "e".to_string(), true);

        let stats = manager.overall_stats();
        assert_eq!(stats.models_trained, 2, "should count unique model names");
    }

    // --- train_from_query_response ---

    #[test]
    fn test_train_from_query_response_records_event() {
        let mut manager = make_manager();
        let report = manager
            .train_from_query_response("query", "response", true, true)
            .unwrap();

        assert!(report.success);
        assert_eq!(report.models_updated, vec!["coordinator"]);
        assert_eq!(manager.training_log.len(), 1);
    }

    #[test]
    fn test_training_report_default_is_not_successful() {
        let report = TrainingReport::default();
        assert!(!report.success);
        assert!(report.models_updated.is_empty());
        assert!(report.errors.is_empty());
    }
}
