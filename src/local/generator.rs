// Response Generator - Generates local responses based on learned patterns
//
// Phase 1: Template-based responses for simple queries
// Phase 2: Learn response patterns from Claude
// Phase 3: Style transfer and quality matching

use crate::local::patterns::PatternClassifier;
use crate::models::learning::{
    LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData,
};
use crate::models::GeneratorModel;
use crate::training::batch_trainer::BatchTrainer;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Response template for a pattern
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ResponseTemplate {
    pattern: String,
    templates: Vec<String>,
    usage_count: usize,
    success_rate: f64,
}

/// Response generator that creates local responses
pub struct ResponseGenerator {
    pattern_classifier: PatternClassifier,
    templates: HashMap<String, ResponseTemplate>,
    learned_responses: HashMap<String, Vec<LearnedResponse>>,
    stats: ModelStats,
    /// Optional neural generator for trained model generation
    neural_generator: Option<Arc<RwLock<GeneratorModel>>>,
}

/// A response learned from Claude
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LearnedResponse {
    query_pattern: String,
    response_text: String,
    quality_score: f64,
    usage_count: usize,
}

impl ResponseGenerator {
    /// Create new response generator without neural models
    pub fn new(pattern_classifier: PatternClassifier) -> Self {
        Self::with_models(pattern_classifier, None)
    }

    /// Create response generator with optional neural models
    pub fn with_models(
        pattern_classifier: PatternClassifier,
        neural_generator: Option<Arc<RwLock<GeneratorModel>>>,
    ) -> Self {
        let mut templates = HashMap::new();

        // Initialize default templates for common patterns
        templates.insert(
            "greeting".to_string(),
            ResponseTemplate {
                pattern: "greeting".to_string(),
                templates: vec![
                    "Hello! How can I help you today?".to_string(),
                    "Hi there! What can I assist you with?".to_string(),
                    "Hello! I'm here to help. What would you like to know?".to_string(),
                ],
                usage_count: 0,
                success_rate: 0.8,
            },
        );

        templates.insert(
            "definition".to_string(),
            ResponseTemplate {
                pattern: "definition".to_string(),
                templates: vec![
                    "I'd be happy to explain that. [definition would go here]".to_string()
                ],
                usage_count: 0,
                success_rate: 0.4, // Lower confidence, more likely to forward
            },
        );

        Self {
            pattern_classifier,
            templates,
            learned_responses: HashMap::new(),
            stats: ModelStats::default(),
            neural_generator,
        }
    }

    /// Generate a response for a query
    pub fn generate(&mut self, query: &str) -> Result<GeneratedResponse> {
        // Classify the query pattern
        let (pattern, confidence) = self.pattern_classifier.classify(query);

        // 1. Try neural generator FIRST - ALWAYS show the output if generation succeeds
        if let Some(generator) = &self.neural_generator {
            match self.try_neural_generate(query, generator) {
                Ok(neural_response) => {
                    // ALWAYS return neural response if generation succeeded
                    // Even if it's short or contains errors - let user see what model produces
                    let quality_score = if neural_response.len() < 10 {
                        0.1 // Very low confidence for short responses
                    } else if neural_response.starts_with("[Error:") {
                        0.2 // Low confidence for error responses
                    } else {
                        0.8 // High confidence for normal responses
                    };

                    let display_text = if neural_response.len() < 10 {
                        format!("[NEURAL - LOW QUALITY]: {}", neural_response)
                    } else if neural_response.starts_with("[Error:") {
                        format!("[NEURAL - ERROR]: {}", neural_response)
                    } else {
                        neural_response
                    };

                    return Ok(GeneratedResponse {
                        text: display_text,
                        method: "neural".to_string(),
                        confidence: quality_score,
                        pattern: pattern.as_str().to_string(),
                    });
                }
                Err(e) => {
                    // Neural generation failed entirely - show the full error with context
                    let full_error = format!("{:#}", e); // Use alternate display for full error chain
                    tracing::error!("Neural generation failed: {}", full_error);
                    return Ok(GeneratedResponse {
                        text: format!("[NEURAL GENERATION FAILED]: {}", full_error),
                        method: "neural_error".to_string(),
                        confidence: 0.0,
                        pattern: pattern.as_str().to_string(),
                    });
                }
            }
        }

        // 2. Check if we have learned responses for this pattern (fallback)
        if let Some(learned) = self.learned_responses.get(pattern.as_str()) {
            if !learned.is_empty() {
                // Use best learned response
                let best = learned.iter().max_by(|a, b| {
                    a.quality_score
                        .partial_cmp(&b.quality_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                if let Some(response) = best {
                    return Ok(GeneratedResponse {
                        text: response.response_text.clone(),
                        method: "learned".to_string(),
                        confidence: response.quality_score * confidence,
                        pattern: pattern.as_str().to_string(),
                    });
                }
            }
        }

        // 3. No neural models available - return error so router forwards to Claude
        Err(anyhow::anyhow!(
            "No neural models available for local generation"
        ))
    }

    /// Try to generate response using neural model
    fn try_neural_generate(
        &self,
        query: &str,
        generator: &Arc<RwLock<GeneratorModel>>,
    ) -> Result<String> {
        tracing::info!("[neural_gen] Starting neural generation for query: {}", query);

        // Generate with neural model (try non-blocking lock)
        tracing::debug!("[neural_gen] Acquiring generator lock...");
        let mut gen = generator
            .try_write()
            .map_err(|_| anyhow::anyhow!("Generator model is locked"))?;

        tracing::info!("[neural_gen] Lock acquired, starting generation (max 100 tokens)...");

        // Use generate_text() which handles tokenization internally
        let response = gen.generate_text(query, 100)?; // max 100 new tokens

        tracing::info!("[neural_gen] Neural generation finished, response length: {} chars", response.len());

        Ok(response)
    }

    /// Learn from a Claude response
    pub fn learn_from_claude(
        &mut self,
        query: &str,
        response: &str,
        quality_score: f64,
        batch_trainer: Option<&Arc<RwLock<BatchTrainer>>>,
    ) {
        let (pattern, _) = self.pattern_classifier.classify(query);

        let learned = LearnedResponse {
            query_pattern: pattern.as_str().to_string(),
            response_text: response.to_string(),
            quality_score,
            usage_count: 0,
        };

        self.learned_responses
            .entry(pattern.as_str().to_string())
            .or_default()
            .push(learned);

        // Limit learned responses per pattern
        if let Some(responses) = self.learned_responses.get_mut(pattern.as_str()) {
            if responses.len() > 10 {
                // Keep only top 10 by quality
                responses.sort_by(|a, b| {
                    b.quality_score
                        .partial_cmp(&a.quality_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                responses.truncate(10);
            }
        }

        // NEW: Also add to BatchTrainer for neural training
        if let Some(trainer) = batch_trainer {
            if quality_score >= 0.7 {
                use crate::training::batch_trainer::TrainingExample;

                let example = TrainingExample::new(
                    query.to_string(),
                    response.to_string(),
                    false, // from Claude
                )
                .with_quality(quality_score);

                // Queue for async training
                let trainer = Arc::clone(trainer);
                tokio::spawn(async move {
                    let t = trainer.write().await;
                    let _ = t.add_example(example).await;
                });
            }
        }
    }
}

/// Generated response with metadata
#[derive(Debug, Clone)]
pub struct GeneratedResponse {
    pub text: String,
    pub method: String, // "template", "learned", "neural", or "neural_error"
    pub confidence: f64,
    pub pattern: String,
}

impl Default for ResponseGenerator {
    fn default() -> Self {
        Self::new(PatternClassifier::new())
    }
}

impl LearningModel for ResponseGenerator {
    fn update(&mut self, input: &str, expected: &ModelExpectation) -> Result<()> {
        match expected {
            ModelExpectation::ResponseTarget {
                text,
                quality_score,
            } => {
                self.learn_from_claude(input, text, *quality_score, None);
                self.stats.total_updates += 1;
                self.stats.last_update = Some(chrono::Utc::now());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn predict(&self, input: &str) -> Result<ModelPrediction> {
        let (pattern, confidence) = self.pattern_classifier.classify(input);

        // Create prediction data based on what we'd generate
        let data = if let Some(learned) = self.learned_responses.get(pattern.as_str()) {
            if let Some(best) = learned.iter().max_by(|a, b| {
                a.quality_score
                    .partial_cmp(&b.quality_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }) {
                PredictionData::Response {
                    text: best.response_text.clone(),
                    method: "learned".to_string(),
                }
            } else {
                PredictionData::Response {
                    text: "No learned response available".to_string(),
                    method: "fallback".to_string(),
                }
            }
        } else {
            PredictionData::Response {
                text: "No learned response available".to_string(),
                method: "fallback".to_string(),
            }
        };

        Ok(ModelPrediction { confidence, data })
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).context("Failed to save response generator")
    }

    fn load(path: &Path) -> Result<Self>
    where
        Self: Sized,
    {
        if !path.exists() {
            anyhow::bail!("File not found: {}", path.display());
        }

        let json = std::fs::read_to_string(path)?;
        let loaded: ResponseGenerator = serde_json::from_str(&json)?;
        Ok(loaded)
    }

    fn name(&self) -> &str {
        "ResponseGenerator"
    }

    fn stats(&self) -> ModelStats {
        self.stats.clone()
    }
}

impl serde::Serialize for ResponseGenerator {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("ResponseGenerator", 3)?;
        state.serialize_field("templates", &self.templates)?;
        state.serialize_field("learned_responses", &self.learned_responses)?;
        state.serialize_field("stats", &self.stats)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for ResponseGenerator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ResponseGeneratorData {
            templates: HashMap<String, ResponseTemplate>,
            learned_responses: HashMap<String, Vec<LearnedResponse>>,
            stats: ModelStats,
        }

        let data = ResponseGeneratorData::deserialize(deserializer)?;
        Ok(Self {
            pattern_classifier: PatternClassifier::new(),
            templates: data.templates,
            learned_responses: data.learned_responses,
            stats: data.stats,
            neural_generator: None,
        })
    }
}
