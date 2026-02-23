// Learning Model Trait - Unified interface for all learning components
//
// All models (router, validator, pattern classifier, generator) implement this trait
// for consistent training and persistence

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Prediction from a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrediction {
    /// Confidence score (0.0 to 1.0)
    pub confidence: f64,
    /// Model-specific prediction data
    pub data: PredictionData,
}

/// Model-specific prediction data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PredictionData {
    /// Router: should try local?
    Route { try_local: bool, reason: String },
    /// Pattern classifier: query type
    Pattern {
        category: String,
        features: Vec<String>,
    },
    /// Generator: generated response
    Response { text: String, method: String },
    /// Validator: quality assessment
    Quality { is_good: bool, issues: Vec<String> },
}

/// Expected output for training
#[derive(Debug, Clone)]
pub enum ModelExpectation {
    /// Router should have routed this way
    RouteDecision {
        correct_choice: bool,
        actual_outcome: String,
    },
    /// Pattern should have been this category
    PatternLabel {
        category: String,
        features: Vec<String>,
    },
    /// Response should match this text
    ResponseTarget { text: String, quality_score: f64 },
    /// Validation should have caught this
    QualityTarget {
        is_acceptable: bool,
        issues: Vec<String>,
    },
}

/// Core trait for all learning models
pub trait LearningModel: Send + Sync {
    /// Update model with new training example
    fn update(&mut self, input: &str, expected: &ModelExpectation) -> Result<()>;

    /// Make a prediction
    fn predict(&self, input: &str) -> Result<ModelPrediction>;

    /// Save model to disk
    fn save(&self, path: &Path) -> Result<()>;

    /// Load model from disk
    fn load(path: &Path) -> Result<Self>
    where
        Self: Sized;

    /// Get model name
    fn name(&self) -> &str;

    /// Get training statistics
    fn stats(&self) -> ModelStats;
}

/// Training statistics for a model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelStats {
    pub total_updates: usize,
    pub last_update: Option<chrono::DateTime<chrono::Utc>>,
    pub accuracy: f64,
    pub confidence_avg: f64,
}

impl Default for ModelStats {
    fn default() -> Self {
        Self {
            total_updates: 0,
            last_update: None,
            accuracy: 0.0,
            confidence_avg: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_stats_default_zeroed() {
        let stats = ModelStats::default();
        assert_eq!(stats.total_updates, 0);
        assert!(stats.last_update.is_none());
        assert_eq!(stats.accuracy, 0.0);
        assert_eq!(stats.confidence_avg, 0.0);
    }

    #[test]
    fn test_model_stats_serde_roundtrip() {
        let stats = ModelStats {
            total_updates: 42,
            last_update: None,
            accuracy: 0.95,
            confidence_avg: 0.78,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let back: ModelStats = serde_json::from_str(&json).unwrap();
        assert_eq!(back.total_updates, 42);
        assert_eq!(back.accuracy, 0.95);
        assert_eq!(back.confidence_avg, 0.78);
    }

    #[test]
    fn test_prediction_data_route_variant() {
        let data = PredictionData::Route {
            try_local: true,
            reason: "high confidence".to_string(),
        };
        match data {
            PredictionData::Route { try_local, reason } => {
                assert!(try_local);
                assert_eq!(reason, "high confidence");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_prediction_data_pattern_variant() {
        let data = PredictionData::Pattern {
            category: "greeting".to_string(),
            features: vec!["hello".to_string(), "hi".to_string()],
        };
        match data {
            PredictionData::Pattern { category, features } => {
                assert_eq!(category, "greeting");
                assert_eq!(features.len(), 2);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_prediction_data_response_variant() {
        let data = PredictionData::Response {
            text: "The answer is 42".to_string(),
            method: "local".to_string(),
        };
        match data {
            PredictionData::Response { text, method } => {
                assert_eq!(text, "The answer is 42");
                assert_eq!(method, "local");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_prediction_data_quality_variant() {
        let data = PredictionData::Quality {
            is_good: false,
            issues: vec!["too short".to_string()],
        };
        match data {
            PredictionData::Quality { is_good, issues } => {
                assert!(!is_good);
                assert_eq!(issues[0], "too short");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_prediction_construction() {
        let pred = ModelPrediction {
            confidence: 0.85,
            data: PredictionData::Response {
                text: "answer".to_string(),
                method: "local".to_string(),
            },
        };
        assert!((pred.confidence - 0.85).abs() < f64::EPSILON);
    }

    #[test]
    fn test_model_expectation_route_decision() {
        let exp = ModelExpectation::RouteDecision {
            correct_choice: true,
            actual_outcome: "local".to_string(),
        };
        match exp {
            ModelExpectation::RouteDecision {
                correct_choice,
                actual_outcome,
            } => {
                assert!(correct_choice);
                assert_eq!(actual_outcome, "local");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_expectation_quality_target() {
        let exp = ModelExpectation::QualityTarget {
            is_acceptable: false,
            issues: vec!["ambiguous".to_string(), "too vague".to_string()],
        };
        match exp {
            ModelExpectation::QualityTarget {
                is_acceptable,
                issues,
            } => {
                assert!(!is_acceptable);
                assert_eq!(issues.len(), 2);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_expectation_pattern_label() {
        let exp = ModelExpectation::PatternLabel {
            category: "code".to_string(),
            features: vec!["fn".to_string()],
        };
        match exp {
            ModelExpectation::PatternLabel { category, .. } => assert_eq!(category, "code"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_model_expectation_response_target() {
        let exp = ModelExpectation::ResponseTarget {
            text: "42".to_string(),
            quality_score: 1.0,
        };
        match exp {
            ModelExpectation::ResponseTarget { quality_score, .. } => {
                assert_eq!(quality_score, 1.0);
            }
            _ => panic!("Wrong variant"),
        }
    }
}
