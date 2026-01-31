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
    Pattern { category: String, features: Vec<String> },
    /// Generator: generated response
    Response { text: String, method: String },
    /// Validator: quality assessment
    Quality { is_good: bool, issues: Vec<String> },
}

/// Expected output for training
#[derive(Debug, Clone)]
pub enum ModelExpectation {
    /// Router should have routed this way
    RouteDecision { correct_choice: bool, actual_outcome: String },
    /// Pattern should have been this category
    PatternLabel { category: String, features: Vec<String> },
    /// Response should match this text
    ResponseTarget { text: String, quality_score: f64 },
    /// Validation should have caught this
    QualityTarget { is_acceptable: bool, issues: Vec<String> },
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
