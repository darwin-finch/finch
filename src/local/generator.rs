// Response Generator - Generates local responses based on learned patterns
//
// Phase 1: Template-based responses for simple queries
// Phase 2: Learn response patterns from Claude
// Phase 3: Style transfer and quality matching

use crate::local::patterns::PatternClassifier;
use crate::models::adapters::{AdapterRegistry, LocalModelAdapter};
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

/// Template-based generator that creates local responses from learned patterns
pub struct TemplateGenerator {
    pattern_classifier: PatternClassifier,
    templates: HashMap<String, ResponseTemplate>,
    learned_responses: HashMap<String, Vec<LearnedResponse>>,
    stats: ModelStats,
    /// Optional neural generator for trained model generation
    neural_generator: Option<Arc<RwLock<GeneratorModel>>>,
    /// System prompt / constitution for guiding responses
    system_prompt: String,
    /// Model adapter for formatting prompts and cleaning output
    model_adapter: Box<dyn LocalModelAdapter>,
}

/// A response learned from Claude
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LearnedResponse {
    query_pattern: String,
    response_text: String,
    quality_score: f64,
    usage_count: usize,
}

impl TemplateGenerator {
    /// Create new response generator without neural models
    pub fn new(pattern_classifier: PatternClassifier) -> Self {
        Self::with_models(pattern_classifier, None, "Qwen") // Default to Qwen
    }

    /// Create response generator with optional neural models
    pub fn with_models(
        pattern_classifier: PatternClassifier,
        neural_generator: Option<Arc<RwLock<GeneratorModel>>>,
        model_name: &str,
    ) -> Self {
        // Load system prompt from constitution file
        let system_prompt = Self::load_constitution();

        // Get appropriate model adapter
        let model_adapter = AdapterRegistry::get_adapter(model_name);
        tracing::info!("Using {} adapter for model: {}", model_adapter.family_name(), model_name);

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
            system_prompt,
            model_adapter,
        }
    }

    /// Generate a response with streaming callback
    ///
    /// Calls the callback for each generated token with (token_id, token_text).
    pub fn generate_streaming<F>(
        &mut self,
        messages: &[crate::claude::Message],
        token_callback: F,
    ) -> Result<Option<crate::generators::GeneratorResponse>>
    where
        F: FnMut(u32, &str) + Send + 'static,
    {
        // Extract the user's last message
        let query = messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .and_then(|m| {
                m.content.iter().find_map(|block| match block {
                    crate::claude::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        // Try neural generator with streaming
        if let Some(generator) = &self.neural_generator {
            match self.try_neural_generate_streaming(query, generator, token_callback) {
                Ok(neural_response) => {
                    // Convert to GeneratorResponse format
                    use crate::generators::ResponseMetadata;

                    let response = crate::generators::GeneratorResponse {
                        text: neural_response.clone(),
                        content_blocks: vec![crate::claude::ContentBlock::Text {
                            text: neural_response.clone(),
                        }],
                        tool_uses: vec![],
                        metadata: ResponseMetadata {
                            generator: "qwen-local".to_string(),
                            model: "Qwen2.5-1.5B-Instruct".to_string(),
                            confidence: Some(0.9),
                            stop_reason: None,
                            input_tokens: None,
                            output_tokens: Some(neural_response.split_whitespace().count() as u32),
                            latency_ms: None,
                        },
                    };

                    return Ok(Some(response));
                }
                Err(e) => {
                    tracing::warn!("Neural streaming generation failed: {}", e);
                    return Ok(None);
                }
            }
        }

        // No neural generator available
        Ok(None)
    }

    /// Generate a response for a query
    pub fn generate(&mut self, query: &str) -> Result<GeneratedResponse> {
        // Classify the query pattern
        let (pattern, confidence) = self.pattern_classifier.classify(query);

        // 1. Try neural generator FIRST - ALWAYS show the output if generation succeeds
        if let Some(generator) = &self.neural_generator {
            match self.try_neural_generate(query, generator) {
                Ok(neural_response) => {
                    // Return neural response (quality score used internally for routing)
                    let quality_score = if neural_response.len() < 10 {
                        0.5 // Lower confidence for very short responses
                    } else if neural_response.starts_with("[Error:") {
                        0.3 // Low confidence for error responses
                    } else {
                        0.9 // High confidence for normal responses
                    };

                    return Ok(GeneratedResponse {
                        text: neural_response, // Clean output without debug prefixes
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

    /// Load constitution from file or use default
    fn load_constitution() -> String {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let constitution_path = home.join(".shammah/constitution.md");

        if constitution_path.exists() {
            match std::fs::read_to_string(&constitution_path) {
                Ok(content) => {
                    tracing::info!("Loaded constitution from {:?}", constitution_path);
                    content
                }
                Err(e) => {
                    tracing::warn!("Failed to read constitution file: {}, using default", e);
                    Self::default_constitution()
                }
            }
        } else {
            tracing::info!("No constitution file found, using default");
            Self::default_constitution()
        }
    }

    /// Default constitution if no file exists
    fn default_constitution() -> String {
        "You are Shammah, a helpful coding assistant. Be concise and accurate.".to_string()
    }

    /// Format user query with chat template using model-specific adapter
    fn format_chat_prompt(&self, user_query: &str) -> String {
        self.model_adapter.format_chat_prompt(&self.system_prompt, user_query)
    }

    /// Try to generate response using neural model with streaming
    fn try_neural_generate_streaming<F>(
        &self,
        query: &str,
        generator: &Arc<RwLock<GeneratorModel>>,
        mut token_callback: F,
    ) -> Result<String>
    where
        F: FnMut(u32, &str) + Send + 'static,
    {
        tracing::info!("[neural_gen_stream] Starting streaming neural generation");

        // Format query with system prompt using chat template
        let formatted_prompt = self.format_chat_prompt(query);

        // Acquire lock on generator
        let mut gen = generator
            .try_write()
            .map_err(|_| anyhow::anyhow!("Generator model is locked"))?;

        // Get ONNX model backend
        use crate::models::loaders::onnx::LoadedOnnxModel;
        use crate::models::TextGeneration;

        let onnx_model = gen
            .backend_mut()
            .as_any_mut()
            .downcast_mut::<LoadedOnnxModel>()
            .ok_or_else(|| anyhow::anyhow!("Backend is not an ONNX model"))?;

        // Tokenize input
        let encoding = onnx_model.tokenizer()
            .encode(formatted_prompt.as_str(), true)
            .map_err(|e| anyhow::anyhow!("Failed to encode prompt: {}", e))?;

        let input_ids = encoding.get_ids().to_vec();

        // Generate with streaming callback (filter special tokens)
        let output_ids = onnx_model.generate_stream(
            &input_ids,
            100, // max 100 new tokens
            Box::new(move |token_id, token_text| {
                // Filter out special tokens (template markers, control characters)
                // Only stream actual content tokens
                let is_special = token_text.contains("<|")  // Qwen ChatML tokens like <|im_end|>
                    || token_text.contains("|>")
                    || token_text.contains("<｜")  // DeepSeek tokens (full-width)
                    || token_text.contains("｜>")
                    || token_text.contains("▁of▁")  // DeepSeek sentence markers
                    || token_text.contains("<think>")  // DeepSeek reasoning markers
                    || token_text.contains("</think>")
                    || token_text.contains("\\boxed")  // LaTeX formatting
                    || token_text.trim().is_empty();  // Skip whitespace-only tokens

                if !is_special {
                    token_callback(token_id, token_text);
                }
            }),
        )?;

        // Decode full output
        let raw_response = onnx_model.tokenizer()
            .decode(&output_ids, true)
            .map_err(|e| anyhow::anyhow!("Failed to decode output: {}", e))?;

        // Clean output using model adapter
        let clean_response = self.model_adapter.clean_output(&raw_response);

        Ok(clean_response)
    }

    /// Try to generate response using neural model
    fn try_neural_generate(
        &self,
        query: &str,
        generator: &Arc<RwLock<GeneratorModel>>,
    ) -> Result<String> {
        tracing::info!("[neural_gen] Starting neural generation for query: {}", query);

        // Format query with system prompt using chat template
        let formatted_prompt = self.format_chat_prompt(query);
        tracing::debug!("[neural_gen] Formatted prompt length: {} chars", formatted_prompt.len());

        // Generate with neural model (try non-blocking lock)
        tracing::debug!("[neural_gen] Acquiring generator lock...");
        let mut gen = generator
            .try_write()
            .map_err(|_| anyhow::anyhow!("Generator model is locked"))?;

        tracing::info!("[neural_gen] Lock acquired, starting generation (max 100 tokens)...");

        // Use generate_text() which handles tokenization internally
        let raw_response = gen.generate_text(&formatted_prompt, 100)?; // max 100 new tokens

        tracing::info!("[neural_gen] Raw response length: {} chars", raw_response.len());

        // Clean output using model adapter
        let clean_response = self.model_adapter.clean_output(&raw_response);

        tracing::info!("[neural_gen] Cleaned response length: {} chars", clean_response.len());

        Ok(clean_response)
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

impl Default for TemplateGenerator {
    fn default() -> Self {
        Self::new(PatternClassifier::new())
    }
}

impl LearningModel for TemplateGenerator {
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
        let loaded: TemplateGenerator = serde_json::from_str(&json)?;
        Ok(loaded)
    }

    fn name(&self) -> &str {
        "TemplateGenerator"
    }

    fn stats(&self) -> ModelStats {
        self.stats.clone()
    }
}

impl serde::Serialize for TemplateGenerator {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;

        let mut state = serializer.serialize_struct("TemplateGenerator", 3)?;
        state.serialize_field("templates", &self.templates)?;
        state.serialize_field("learned_responses", &self.learned_responses)?;
        state.serialize_field("stats", &self.stats)?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for TemplateGenerator {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct TemplateGeneratorData {
            templates: HashMap<String, ResponseTemplate>,
            learned_responses: HashMap<String, Vec<LearnedResponse>>,
            stats: ModelStats,
        }

        let data = TemplateGeneratorData::deserialize(deserializer)?;
        Ok(Self {
            pattern_classifier: PatternClassifier::new(),
            templates: data.templates,
            learned_responses: data.learned_responses,
            stats: data.stats,
            neural_generator: None,
            system_prompt: Self::load_constitution(),
            model_adapter: AdapterRegistry::get_adapter("Qwen"), // Default to Qwen
        })
    }
}
