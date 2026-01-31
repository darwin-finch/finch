// Response Generator - Generates local responses based on learned patterns
//
// Phase 1: Template-based responses for simple queries
// Phase 2: Learn response patterns from Claude
// Phase 3: Style transfer and quality matching

use crate::local::patterns::{PatternClassifier, QueryPattern};
use crate::models::learning::{
    LearningModel, ModelExpectation, ModelPrediction, ModelStats, PredictionData,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
    /// Create new response generator
    pub fn new(pattern_classifier: PatternClassifier) -> Self {
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
                    "I'd be happy to explain that. [definition would go here]".to_string(),
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
        }
    }

    /// Generate a response for a query
    pub fn generate(&mut self, query: &str) -> Result<GeneratedResponse> {
        // Classify the query pattern
        let (pattern, confidence) = self.pattern_classifier.classify(query);

        // Check if we have learned responses for this pattern
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

        // Fall back to templates
        if let Some(template) = self.templates.get_mut(pattern.as_str()) {
            // Pick a template (for now, just use the first one)
            let text = template.templates[0].clone();
            template.usage_count += 1;

            return Ok(GeneratedResponse {
                text,
                method: "template".to_string(),
                confidence: template.success_rate * confidence,
                pattern: pattern.as_str().to_string(),
            });
        }

        // No template or learned response - cannot generate
        Err(anyhow::anyhow!(
            "No template or learned response for pattern: {}",
            pattern.as_str()
        ))
    }

    /// Learn from a Claude response
    pub fn learn_from_claude(&mut self, query: &str, response: &str, quality_score: f64) {
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
    }
}

/// Generated response with metadata
#[derive(Debug, Clone)]
pub struct GeneratedResponse {
    pub text: String,
    pub method: String, // "template", "learned", or "neural"
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
                self.learn_from_claude(input, text, *quality_score);
                self.stats.total_updates += 1;
                self.stats.last_update = Some(chrono::Utc::now());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn predict(&self, input: &str) -> Result<ModelPrediction> {
        // Note: This creates a mutable copy just for prediction
        // In practice, we'd need to refactor generate() to not require &mut self
        let mut generator_copy = self.clone();
        match generator_copy.generate(input) {
            Ok(response) => Ok(ModelPrediction {
                confidence: response.confidence,
                data: PredictionData::Response {
                    text: response.text,
                    method: response.method,
                },
            }),
            Err(e) => Err(e),
        }
    }

    fn save(&self, path: &Path) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize response generator")?;
        std::fs::write(path, json).context("Failed to write response generator")?;
        Ok(())
    }

    fn load(path: &Path) -> Result<Self> {
        let json =
            std::fs::read_to_string(path).context("Failed to read response generator")?;
        let generator =
            serde_json::from_str(&json).context("Failed to deserialize response generator")?;
        Ok(generator)
    }

    fn name(&self) -> &str {
        "response_generator"
    }

    fn stats(&self) -> ModelStats {
        self.stats.clone()
    }
}

// Manual Clone for ResponseGenerator
impl Clone for ResponseGenerator {
    fn clone(&self) -> Self {
        Self {
            pattern_classifier: self.pattern_classifier.clone(),
            templates: self.templates.clone(),
            learned_responses: self.learned_responses.clone(),
            stats: self.stats.clone(),
        }
    }
}

// Manual Serialize/Deserialize
impl Serialize for ResponseGenerator {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("ResponseGenerator", 4)?;
        state.serialize_field("pattern_classifier", &self.pattern_classifier)?;
        state.serialize_field("templates", &self.templates)?;
        state.serialize_field("learned_responses", &self.learned_responses)?;
        state.serialize_field("stats", &self.stats)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for ResponseGenerator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ResponseGeneratorData {
            pattern_classifier: PatternClassifier,
            templates: HashMap<String, ResponseTemplate>,
            learned_responses: HashMap<String, Vec<LearnedResponse>>,
            stats: ModelStats,
        }

        let data = ResponseGeneratorData::deserialize(deserializer)?;
        Ok(ResponseGenerator {
            pattern_classifier: data.pattern_classifier,
            templates: data.templates,
            learned_responses: data.learned_responses,
            stats: data.stats,
        })
    }
}
