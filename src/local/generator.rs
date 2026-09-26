// Response Generator - Generates local responses based on learned patterns
//
// Phase 1: Template-based responses for simple queries
// Phase 2: Learn response patterns from Claude
// Phase 3: Style transfer and quality matching

use crate::local::patterns::PatternClassifier;
use crate::models::GeneratorModel;
use crate::models::{
    AdapterRegistry, LearningModel, LocalModelAdapter, ModelExpectation, ModelPrediction,
    ModelStats, PredictionData,
};
use crate::training::batch_trainer::BatchTrainer;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

/// How many of the most recent conversation exchanges (a user/assistant
/// pair each) `TemplateGenerator::prompt_parts` forwards ahead of the
/// current question, on top of the current question itself. Bounded on
/// purpose: this path's local-model context budget is much tighter than a
/// cloud provider's, which gets the full conversation history for free.
/// See the doc comment on `prompt_parts` (#1229) for why a fixed window
/// rather than tag-matching is what makes spliced, untagged committed
/// memory (`ConversationHistory::splice_synthetic_exchange`) reach a local
/// model at all.
const LOCAL_HISTORY_WINDOW_EXCHANGES: usize = 3;

/// Token budget reserved for the model's generated response. Mirrors the
/// `max_new_tokens` this module actually requests from the backend
/// (`try_neural_generate`, `try_neural_generate_streaming`) so the prompt
/// budget below never counts on room the response itself will consume.
const LOCAL_RESPONSE_TOKEN_RESERVE: usize = 100;

/// Token budget reserved for chat-template markers, the system prompt, and
/// a small safety margin -- rendering overhead that `prompt_parts` itself
/// cannot see, since it only assembles the `query` half of the prompt.
const LOCAL_PROMPT_OVERHEAD_RESERVE: usize = 64;

/// Trim `recent_history` (oldest first) so that, combined with
/// `current_question`, the packed query stays within `budget_tokens` as
/// measured by `count_tokens`.
///
/// Drops from the oldest end first: newest history and the current
/// question itself are never dropped by this step. A current question that
/// alone still exceeds budget is a distinct, unavoidable overflow --
/// `LlamaCppGenerator::generate_inner`'s existing capacity check
/// (`src/models/loaders/llama_cpp.rs`) still catches and reports it.
fn fit_history_to_budget(
    current_question: &str,
    recent_history: Vec<String>,
    budget_tokens: usize,
    count_tokens: impl Fn(&str) -> usize,
) -> String {
    let mut used = count_tokens(current_question);
    let mut included: Vec<String> = Vec::new();
    for exchange in recent_history.into_iter().rev() {
        let cost = count_tokens(&exchange);
        if used.saturating_add(cost) > budget_tokens {
            break;
        }
        used += cost;
        included.push(exchange);
    }
    included.reverse();

    if included.is_empty() {
        current_question.to_string()
    } else {
        format!("{}\n\n{}", included.join("\n\n"), current_question)
    }
}

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
    model_name: String,
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
        tracing::info!(
            "Using {} adapter for model: {}",
            model_adapter.family_name(),
            model_name
        );

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
            model_name: model_name.to_string(),
        }
    }

    /// Generate a response with streaming callback
    ///
    /// Calls the callback for each generated token with (token_id, token_text).
    pub fn generate_streaming<F>(
        &mut self,
        messages: &[crate::providers::Message],
        token_callback: F,
    ) -> Result<Option<crate::generators::GeneratorResponse>>
    where
        F: FnMut(u32, &str) + Send + 'static,
    {
        let (system_prompt, query) = self.prompt_parts(messages)?;

        // Try neural generator with streaming
        if let Some(generator) = &self.neural_generator {
            match self.try_neural_generate_streaming(
                &system_prompt,
                &query,
                generator,
                token_callback,
            ) {
                Ok(neural_response) => {
                    // Convert to GeneratorResponse format
                    use crate::generators::ResponseMetadata;

                    let response = crate::generators::GeneratorResponse {
                        text: neural_response.clone(),
                        content_blocks: vec![crate::providers::ContentBlock::Text {
                            text: neural_response.clone(),
                        }],
                        tool_uses: vec![],
                        metadata: ResponseMetadata {
                            generator: format!(
                                "{}-local",
                                self.model_adapter.family_name().to_lowercase()
                            ),
                            model: self.model_name.clone(),
                            confidence: Some(0.9),
                            stop_reason: None,
                            input_tokens: None,
                            output_tokens: Some(neural_response.split_whitespace().count() as u32),
                            latency_ms: None,
                            primary_allowance_used_percent: None,
                            secondary_allowance_used_percent: None,
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
        let system_prompt = self.system_prompt.clone();
        self.generate_with_system(&system_prompt, query)
    }

    /// Generate from a provider message array without discarding its caller-owned
    /// system contract. The interactive client injects the Finch VM wire ABI in
    /// that system message, so replacing it with the generic local constitution
    /// makes an otherwise healthy model answer in prose.
    pub fn generate_messages(
        &mut self,
        messages: &[crate::providers::Message],
    ) -> Result<GeneratedResponse> {
        let (system_prompt, query) = self.prompt_parts(messages)?;
        self.generate_with_system(&system_prompt, &query)
    }

    fn generate_with_system(
        &mut self,
        system_prompt: &str,
        query: &str,
    ) -> Result<GeneratedResponse> {
        // Classify the query pattern
        let (pattern, confidence) = self.pattern_classifier.classify(query);

        // 1. Try neural generator FIRST - ALWAYS show the output if generation succeeds
        if let Some(generator) = &self.neural_generator {
            match self.try_neural_generate(system_prompt, query, generator) {
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
                    // Neural generation failed entirely (e.g. the prompt plus
                    // requested output exceeds the model's context window).
                    // This is a real failure, not a candidate response: it
                    // must propagate as `Err` so callers treat the turn as
                    // failed instead of compiling the failure text as if it
                    // were the model's wire response (#1234). Fold the full
                    // error chain into one top-level message so it survives
                    // both `{}` and `{:#}` rendering at every downstream call
                    // site.
                    let full_error = format!("{:#}", e); // full chain for the log and the message
                    tracing::error!("Neural generation failed: {}", full_error);
                    return Err(anyhow::anyhow!("local generation failed: {full_error}"));
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
        let constitution_path = home.join(".finch/constitution.md");

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

    fn format_chat_prompt_with_system(&self, system_prompt: &str, user_query: &str) -> String {
        self.model_adapter
            .format_chat_prompt(system_prompt, user_query)
    }

    fn prompt_parts(&self, messages: &[crate::providers::Message]) -> Result<(String, String)> {
        let last_user_idx = messages
            .iter()
            .rposition(|message| message.role == "user")
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;
        let current_question = messages[last_user_idx]
            .content
            .iter()
            .find_map(|block| match block {
                crate::providers::ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        // Fresh recall is injected as a synthetic [user(memory), assistant(ack)]
        // pair, wrapped in a `<retrieved_memory>` tag, immediately before the
        // current question (`inject_recall_prefix` in
        // `src/cli/repl_event/query_processor.rs`). A memory judged worth
        // persisting is instead spliced into real, ordinary (untagged)
        // conversation history (`ConversationHistory::splice_synthetic_exchange`)
        // -- once promoted, it is deliberately indistinguishable from a
        // genuine earlier turn, so there is no tag to match against.
        //
        // A prior version of this function pulled forward only messages
        // containing the `<retrieved_memory>` tag and excluded the rest of
        // `messages` outright, on the reasoning that this path's tight
        // local-model context budget can't fit unbounded history. That made
        // a spliced-but-untagged memory invisible to local models after the
        // turn it was first recalled and shown via the tag, even though a
        // cloud provider (which gets the full `messages` array as-is) kept
        // seeing it for free from history (#1229). Tagging cannot be relied
        // on to find it: by design the splice produces a message pair that
        // is byte-for-byte the same shape as a genuine earlier turn.
        //
        // Fix: forward a small, fixed-size window of the most recent
        // ordinary exchanges immediately preceding the current question,
        // not just tagged blocks. `LOCAL_HISTORY_WINDOW_EXCHANGES` bounds
        // this to a handful of turns -- comfortably covering both the fresh
        // recall pair (always the exchange immediately before the question)
        // and a splice that landed within the last few turns -- while
        // keeping the addition small relative to the tight local-model
        // budget. History older than the window still stays excluded, same
        // as before.
        let history_before_question = &messages[..last_user_idx];
        let window_start = history_before_question
            .len()
            .saturating_sub(LOCAL_HISTORY_WINDOW_EXCHANGES * 2);
        let recent_history: Vec<String> = history_before_question[window_start..]
            .iter()
            .filter(|message| message.role == "user" || message.role == "assistant")
            .filter_map(|message| {
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        crate::providers::ContentBlock::Text { text }
                            if !text.trim().is_empty() =>
                        {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    None
                } else {
                    Some(format!("{}: {}", message.role, text))
                }
            })
            .collect();

        // The fixed exchange-count window above bounds *how many* exchanges
        // are candidates, but not their size: three long exchanges (long
        // code pastes, verbose answers) can still overflow a small model's
        // real context window on their own (the failure #1234 reports after
        // the fact). Trim further against an actual token budget derived
        // from the loaded model's real context length
        // (`GeneratorModel::context_length`, itself resolved from the
        // GGUF's trained length in `src/models/loaders/llama_cpp.rs` rather
        // than a hardcoded default) when a model is loaded and its lock is
        // free; otherwise keep the prior unconditional-join behaviour so a
        // model that hasn't finished loading, or a momentarily contended
        // lock, doesn't fail the turn.
        let query = match self
            .neural_generator
            .as_ref()
            .and_then(|generator| generator.try_read().ok())
        {
            Some(generator) => {
                let budget = (generator.context_length() as usize)
                    .saturating_sub(LOCAL_RESPONSE_TOKEN_RESERVE + LOCAL_PROMPT_OVERHEAD_RESERVE);
                let count_tokens = |text: &str| {
                    generator
                        .tokenize(text)
                        .map(|tokens| tokens.len())
                        .unwrap_or_else(|_| text.split_whitespace().count())
                };
                fit_history_to_budget(current_question, recent_history, budget, count_tokens)
            }
            None if recent_history.is_empty() => current_question.to_string(),
            None => format!("{}\n\n{}", recent_history.join("\n\n"), current_question),
        };

        let caller_system = messages
            .iter()
            .filter(|message| message.role == "system")
            .flat_map(|message| message.content.iter())
            .filter_map(|block| match block {
                crate::providers::ContentBlock::Text { text } if !text.trim().is_empty() => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let system_prompt = if caller_system.is_empty() {
            self.system_prompt.clone()
        } else {
            caller_system
        };
        Ok((system_prompt, query))
    }

    /// Try to generate response using neural model with streaming
    fn try_neural_generate_streaming<F>(
        &self,
        system_prompt: &str,
        query: &str,
        generator: &Arc<RwLock<GeneratorModel>>,
        mut token_callback: F,
    ) -> Result<String>
    where
        F: FnMut(u32, &str) + Send + 'static,
    {
        tracing::info!("[neural_gen_stream] Starting streaming neural generation");

        // Format query with system prompt using chat template
        let formatted_prompt = self.format_chat_prompt_with_system(system_prompt, query);

        // Acquire lock on generator
        let mut gen = generator
            .try_write()
            .map_err(|_| anyhow::anyhow!("Generator model is locked"))?;

        // The GGUF backend implements this narrow generation contract.
        let backend = gen.backend_mut();
        let input_ids = backend.tokenize(&formatted_prompt)?;

        // Generate with streaming callback (filter special tokens)
        let output_ids = backend.generate_stream(
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
                    || token_text.trim().is_empty(); // Skip whitespace-only tokens

                if !is_special {
                    token_callback(token_id, token_text);
                }
            }),
        )?;

        // Decode full output
        let raw_response = backend.decode_tokens(&output_ids)?;

        // Clean output using model adapter
        let clean_response = self.model_adapter.clean_output(&raw_response);

        Ok(clean_response)
    }

    /// Get the model adapter for external use (e.g., streaming cleaning)
    pub fn get_adapter(&self) -> Box<dyn LocalModelAdapter> {
        // Get adapter for the same family (cheap clone - just vtable pointer)
        use crate::models::AdapterRegistry;
        AdapterRegistry::get_adapter(self.model_adapter.family_name())
    }

    /// Try to generate response using neural model
    fn try_neural_generate(
        &self,
        system_prompt: &str,
        query: &str,
        generator: &Arc<RwLock<GeneratorModel>>,
    ) -> Result<String> {
        tracing::info!(
            "[neural_gen] Starting neural generation for query: {}",
            query
        );

        // Format query with system prompt using chat template
        let formatted_prompt = self.format_chat_prompt_with_system(system_prompt, query);
        tracing::debug!(
            "[neural_gen] Formatted prompt length: {} chars",
            formatted_prompt.len()
        );

        // Generate with neural model (try non-blocking lock)
        tracing::debug!("[neural_gen] Acquiring generator lock...");
        let mut gen = generator
            .try_write()
            .map_err(|_| anyhow::anyhow!("Generator model is locked"))?;

        tracing::info!("[neural_gen] Lock acquired, starting generation (max 100 tokens)...");

        // Use generate_text() which handles tokenization internally
        let raw_response = gen.generate_text(&formatted_prompt, 100)?; // max 100 new tokens

        tracing::info!(
            "[neural_gen] Raw response length: {} chars",
            raw_response.len()
        );

        // Clean output using model adapter
        let clean_response = self.model_adapter.clean_output(&raw_response);

        tracing::info!(
            "[neural_gen] Cleaned response length: {} chars",
            clean_response.len()
        );

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ExecutionTarget;
    use crate::models::{
        GeneratorConfig, InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize,
        TextGeneration, TokenCallback,
    };
    use std::path::PathBuf;

    struct MockGemma;

    impl TextGeneration for MockGemma {
        fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
            Ok(b"Hello from mock"
                .iter()
                .map(|byte| u32::from(*byte))
                .collect())
        }

        fn generate_stream(
            &mut self,
            input_ids: &[u32],
            max_new_tokens: usize,
            mut callback: TokenCallback,
        ) -> Result<Vec<u32>> {
            let output = self.generate(input_ids, max_new_tokens)?;
            callback(output[0], "Hello from mock");
            Ok(output)
        }

        fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
            Ok(text.bytes().map(u32::from).collect())
        }

        fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
            let bytes = tokens.iter().map(|token| *token as u8).collect();
            Ok(String::from_utf8(bytes)?)
        }

        fn name(&self) -> &str {
            "Gemma 2 test"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_local_streaming_calls_injected_backend_without_engine_downcast() {
        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(MockGemma), config);
        let shared = Arc::new(RwLock::new(model));
        let generator = TemplateGenerator::with_models(
            PatternClassifier::new(),
            Some(Arc::clone(&shared)),
            "Gemma 2 test",
        );
        let received = Arc::new(std::sync::Mutex::new(String::new()));
        let chunks = Arc::clone(&received);
        let response = generator
            .try_neural_generate_streaming(
                "system contract",
                "greet me",
                &shared,
                move |_, text| {
                    chunks.lock().expect("lock chunks").push_str(text);
                },
            )
            .expect("any TextGeneration backend must reach local streaming");
        assert!(
            response.contains("Hello from mock"),
            "unexpected response: {response}"
        );
        assert_eq!(
            *received.lock().expect("lock chunks"),
            "Hello from mock",
            "injected backend callback must reach local streaming caller"
        );
    }

    /// Records the exact prompt text handed to `TextGeneration::tokenize`, so
    /// tests can assert on what the production path actually sends to the
    /// backend rather than only on the adapter's output in isolation.
    struct PromptCapturingBackend {
        captured_prompt: Arc<std::sync::Mutex<Option<String>>>,
    }

    impl TextGeneration for PromptCapturingBackend {
        fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
            Ok(b"ok".iter().map(|byte| u32::from(*byte)).collect())
        }

        fn generate_stream(
            &mut self,
            input_ids: &[u32],
            max_new_tokens: usize,
            mut callback: TokenCallback,
        ) -> Result<Vec<u32>> {
            let output = self.generate(input_ids, max_new_tokens)?;
            callback(output[0], "ok");
            Ok(output)
        }

        fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
            *self.captured_prompt.lock().expect("lock captured prompt") = Some(text.to_string());
            Ok(text.bytes().map(u32::from).collect())
        }

        fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
            let bytes = tokens.iter().map(|token| *token as u8).collect();
            Ok(String::from_utf8(bytes)?)
        }

        fn name(&self) -> &str {
            "Gemma 2 9B test"
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    }

    #[test]
    fn test_gemma_configured_generator_sends_gemma_template_to_backend_not_llama() {
        let captured_prompt = Arc::new(std::sync::Mutex::new(None));
        let backend = PromptCapturingBackend {
            captured_prompt: Arc::clone(&captured_prompt),
        };
        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(backend), config);
        let shared = Arc::new(RwLock::new(model));
        // Model name mirrors what the daemon derives for a Gemma GGUF
        // (`display_name` in `src/models/loaders/llama_cpp.rs`): the
        // configured family prefixed onto the file stem.
        let generator = TemplateGenerator::with_models(
            PatternClassifier::new(),
            Some(Arc::clone(&shared)),
            "Gemma 2 (gemma-2-9b-it)",
        );
        generator
            .try_neural_generate_streaming("system contract", "What is 2+2?", &shared, |_, _| {})
            .expect("Gemma-configured generator must reach the injected backend");

        let prompt = captured_prompt
            .lock()
            .expect("lock captured prompt")
            .clone()
            .expect("tokenize must have been called with the formatted prompt");

        assert!(
            prompt.starts_with("<bos><start_of_turn>user\n"),
            "Gemma's real BOS/turn markers must reach the backend, got: {prompt:?}"
        );
        assert!(
            prompt.contains("<end_of_turn>\n<start_of_turn>model\n"),
            "Gemma's real turn-close/model-turn markers must reach the backend, got: {prompt:?}"
        );
        assert!(
            !prompt.contains("<|begin_of_text|>")
                && !prompt.contains("<|start_header_id|>")
                && !prompt.contains("<|eot_id|>"),
            "Llama's special tokens don't exist in Gemma's vocabulary and must never be sent, got: {prompt:?}"
        );
        // The llama.cpp loader's BOS-detection heuristic
        // (`prompt_contains_explicit_bos` in
        // `src/models/loaders/llama_cpp.rs`) must recognize this exact
        // `<bos>`-prefixed shape so it tells llama.cpp not to add a second,
        // real BOS token on top of this literal one; covered directly by
        // `explicit_chat_template_bos_disables_tokenizer_bos_insertion` in
        // that module, which this prompt shape must stay byte-compatible with.
    }

    #[test]
    #[ignore = "requires FINCH_TEST_GGUF_CHAT pointing to a local chat GGUF"]
    fn test_local_streaming_uses_configured_gguf_backend() {
        let path =
            PathBuf::from(std::env::var("FINCH_TEST_GGUF_CHAT").expect("set FINCH_TEST_GGUF_CHAT"));
        let model = GeneratorModel::new(GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Auto,
            model_path: Some(path),
        }))
        .expect("load configured GGUF");
        let name = model.name().to_string();
        let shared = Arc::new(RwLock::new(model));
        let generator = TemplateGenerator::with_models(
            PatternClassifier::new(),
            Some(Arc::clone(&shared)),
            &name,
        );
        let streamed = Arc::new(std::sync::Mutex::new(String::new()));
        let received = Arc::clone(&streamed);
        let response = generator
            .try_neural_generate_streaming(
                "You are a concise assistant.",
                "Say hello in one short sentence.",
                &shared,
                move |_, piece| received.lock().expect("lock stream").push_str(piece),
            )
            .expect("configured GGUF must stream through local boundary");
        assert!(
            !response.trim().is_empty(),
            "local GGUF response must contain text"
        );
        assert!(
            !streamed.lock().expect("lock stream").is_empty(),
            "local GGUF path must call the streaming callback"
        );
    }

    #[test]
    fn provider_system_contract_replaces_generic_local_constitution() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let messages = vec![
            crate::providers::Message {
                role: "system".to_string(),
                content: vec![crate::providers::ContentBlock::Text {
                    text: "FINCH VM WIRE CONTRACT".to_string(),
                }],
            },
            crate::providers::Message::user("emit one raw program"),
        ];

        let (system, query) = generator.prompt_parts(&messages).unwrap();

        assert_eq!(system, "FINCH VM WIRE CONTRACT");
        assert_eq!(query, "emit one raw program");
        assert!(!system.contains("helpful coding assistant"));
    }

    /// Regression for the local path silently dropping recalled memory
    /// (`inject_recall_prefix` in `src/cli/repl_event/query_processor.rs`
    /// inserts a synthetic `[user(memory), assistant(ack)]` pair, wrapped in
    /// `<retrieved_memory>`, immediately before the current question). Before
    /// this fix, `prompt_parts` took only the *last* user message, so that
    /// pair -- neither the last user message nor a system message -- was
    /// dropped: memory showed up in the TUI as "N memories retrieved" but
    /// never reached the local model. This builds the exact shape
    /// `inject_recall_prefix` produces and checks the recalled text survives
    /// into `query`.
    #[test]
    fn recalled_memory_reaches_the_local_prompt_query() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let messages = vec![
            crate::providers::Message::user(
                "<retrieved_memory>\n\
                 The following is retrieved context from past sessions -- not part \
                 of this conversation's live dialogue, and not something to reply \
                 to or continue. Use only what actually bears on the question that \
                 follows this block; ignore the rest.\n\n\
                 User prefers terse commit messages without a trailer.\n\
                 </retrieved_memory>",
            ),
            crate::providers::Message::assistant(
                "Noted -- I'll factor in whatever's relevant from that before answering.",
            ),
            crate::providers::Message::user("what's the fib of 7?"),
        ];

        let (_, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            query.contains("User prefers terse commit messages without a trailer."),
            "recalled memory text must reach the local prompt query, not just the \
             TUI's retrieval display: {query:?}"
        );
        assert!(
            query.contains("what's the fib of 7?"),
            "the current question must still be present in the composed query: {query:?}"
        );
    }

    /// Same defect, but at the actual production boundary: the string handed
    /// to the backend's tokenizer. A correct `query` alone is not enough if
    /// the chat-template formatting step drops or mangles it before the
    /// model ever sees it.
    #[test]
    fn recalled_memory_survives_into_the_formatted_chat_prompt_sent_to_the_backend() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let messages = vec![
            crate::providers::Message::user(
                "<retrieved_memory>\n\
                 The following is retrieved context from past sessions -- not part \
                 of this conversation's live dialogue, and not something to reply \
                 to or continue. Use only what actually bears on the question that \
                 follows this block; ignore the rest.\n\n\
                 User prefers terse commit messages without a trailer.\n\
                 </retrieved_memory>",
            ),
            crate::providers::Message::assistant(
                "Noted -- I'll factor in whatever's relevant from that before answering.",
            ),
            crate::providers::Message::user("what's the fib of 7?"),
        ];

        let (system_prompt, query) = generator.prompt_parts(&messages).unwrap();
        let formatted = generator.format_chat_prompt_with_system(&system_prompt, &query);

        assert!(
            formatted.contains("User prefers terse commit messages without a trailer."),
            "recalled memory text must reach the formatted prompt handed to the \
             local backend's tokenizer, not just an intermediate query string: \
             {formatted:?}"
        );
        assert!(
            formatted.contains("what's the fib of 7?"),
            "the current question must still reach the formatted prompt too: {formatted:?}"
        );
    }

    /// Ordinary conversation history within `LOCAL_HISTORY_WINDOW_EXCHANGES`
    /// now reaches the bounded local-model prompt (#1229) -- this is what
    /// makes a spliced, untagged committed memory (see the test below)
    /// visible without needing to tag or otherwise mark it. A single recent
    /// exchange is well inside the window, so both turns must appear.
    #[test]
    fn recent_ordinary_conversation_history_reaches_the_local_prompt() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let messages = vec![
            crate::providers::Message::user("earlier unrelated turn"),
            crate::providers::Message::assistant("earlier unrelated reply"),
            crate::providers::Message::user("current question"),
        ];

        let (_, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            query.contains("earlier unrelated turn") && query.contains("earlier unrelated reply"),
            "an exchange well inside the bounded recent-history window must reach the \
             local prompt: {query:?}"
        );
        assert!(
            query.contains("current question"),
            "the current question must still be present in the composed query: {query:?}"
        );
    }

    /// The window is still bounded, not unlimited history: this path's
    /// local-model context budget is much tighter than a cloud provider's
    /// (which gets the full `messages` array as-is), so an exchange older
    /// than `LOCAL_HISTORY_WINDOW_EXCHANGES` must stay excluded exactly as
    /// before this fix.
    #[test]
    fn conversation_history_older_than_the_bounded_window_stays_excluded_from_the_local_prompt() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let mut messages = vec![crate::providers::Message::user(
            "ancient turn that must fall outside the window",
        )];
        messages.push(crate::providers::Message::assistant(
            "ancient reply that must fall outside the window",
        ));
        // Pad with enough additional exchanges to push the ancient pair
        // outside `LOCAL_HISTORY_WINDOW_EXCHANGES`.
        for i in 0..LOCAL_HISTORY_WINDOW_EXCHANGES {
            messages.push(crate::providers::Message::user(format!(
                "filler question {i}"
            )));
            messages.push(crate::providers::Message::assistant(format!(
                "filler answer {i}"
            )));
        }
        messages.push(crate::providers::Message::user("current question"));

        let (_, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            !query.contains("ancient turn") && !query.contains("ancient reply"),
            "an exchange older than the bounded recent-history window must stay excluded, \
             preserving this path's tight local-model context budget: {query:?}"
        );
        assert!(
            query.contains("current question"),
            "the current question must still be present in the composed query: {query:?}"
        );
    }

    /// Regression for #1229: once a retrieved memory is spliced into real
    /// conversation history (`ConversationHistory::splice_synthetic_exchange`,
    /// `query_processor.rs`'s splice gate), it is deliberately an ordinary,
    /// untagged `[user, assistant]` pair -- indistinguishable in shape from a
    /// genuine earlier turn, so it cannot be found by tag-matching. Before
    /// the bounded recent-history window fix, `prompt_parts` forwarded only
    /// the current question plus explicitly `<retrieved_memory>`-tagged
    /// blocks, so this spliced pair (neither) was silently dropped: a local
    /// model stopped receiving the memory after the turn it was first
    /// recalled and shown via the tag, while a cloud provider (which gets
    /// the full `messages` array as-is) kept seeing it for free from
    /// history. This builds the exact untagged shape
    /// `splice_synthetic_exchange` produces and checks it now survives into
    /// `query` via the bounded window.
    #[test]
    fn spliced_untagged_memory_reaches_the_local_prompt_via_the_bounded_history_window() {
        let generator = TemplateGenerator::new(PatternClassifier::new());
        let messages = vec![
            crate::providers::Message::user("Where do I keep the deploy key?"),
            crate::providers::Message::assistant(
                "The deploy key lives in the Employee vault under the Finch signing item.",
            ),
            crate::providers::Message::user("hello again"),
        ];

        let (_, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            query.contains(
                "The deploy key lives in the Employee vault under the Finch signing item."
            ),
            "a spliced, untagged exchange must now reach the local prompt query the same \
             way a tagged memory block does, not just on the turn it was first recalled: \
             {query:?}"
        );
        assert!(
            query.contains("hello again"),
            "the current question must still be present in the composed query: {query:?}"
        );
    }

    // ── Token-budget-aware history trimming ──────────────────────────────

    /// Pure-logic regression for `fit_history_to_budget`: once the budget is
    /// exceeded, the oldest candidate exchanges are dropped first, and the
    /// current question always survives.
    #[test]
    fn fit_history_to_budget_drops_oldest_entries_first_once_budget_is_exceeded() {
        let history = vec![
            "user: ancient".to_string(),                     // 2 tokens
            "assistant: ancient reply".to_string(),          // 3 tokens
            "user: recent one".to_string(),                  // 3 tokens
            "assistant: recent two words reply".to_string(), // 5 tokens
        ];
        let count_tokens = |text: &str| text.split_whitespace().count();

        // Budget 10: current question (2) + newest two entries (5 + 3 = 8)
        // fits exactly at 10; the next-oldest entry (3 more) would not.
        let query = fit_history_to_budget("current question", history.clone(), 10, count_tokens);
        assert!(
            !query.contains("ancient"),
            "both entries that don't fit the budget must be dropped: {query:?}"
        );
        assert!(
            query.contains("recent one") && query.contains("recent two words reply"),
            "entries that fit the budget must survive: {query:?}"
        );
        assert!(
            query.contains("current question"),
            "the current question must always survive trimming: {query:?}"
        );

        // A budget covering every entry keeps them all, oldest first.
        let query = fit_history_to_budget("current question", history, 100, count_tokens);
        assert!(
            query.contains("ancient") && query.contains("recent"),
            "a budget that comfortably covers all history must not drop anything: {query:?}"
        );
        assert!(
            query.find("ancient").unwrap() < query.find("recent one").unwrap(),
            "surviving entries must stay in chronological order: {query:?}"
        );
    }

    /// A budget too small even for the current question alone still returns
    /// the question -- an unavoidable single-message overflow is a distinct
    /// failure the GGUF backend's own capacity check reports, not something
    /// this trimming step can fix by dropping history it doesn't have.
    #[test]
    fn fit_history_to_budget_never_drops_the_current_question() {
        let query = fit_history_to_budget(
            "a question that alone exceeds the tiny budget",
            vec!["user: some history".to_string()],
            1,
            |text| text.split_whitespace().count(),
        );
        assert_eq!(
            query, "a question that alone exceeds the tiny budget",
            "the current question must survive even when it alone exceeds budget: {query:?}"
        );
    }

    /// Production-boundary regression for local generation overflowing a
    /// small model's real context window (the failure #1234 reports after
    /// the fact): with a model loaded whose real, resolved context length
    /// leaves only a small token budget, `prompt_parts` must drop the
    /// oldest history first rather than unconditionally joining every
    /// candidate exchange the way it did before this fix.
    #[test]
    fn prompt_parts_trims_oldest_history_to_stay_within_the_models_real_token_budget() {
        struct SmallContextBackend;

        impl TextGeneration for SmallContextBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(Vec::new())
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                Ok((0..text.split_whitespace().count() as u32).collect())
            }

            fn decode_tokens(&self, _tokens: &[u32]) -> Result<String> {
                Ok(String::new())
            }

            fn name(&self) -> &str {
                "small-context-test-model"
            }

            fn context_length(&self) -> u32 {
                // reserve (164) + budget (10), chosen so only the two most
                // recent history messages below fit.
                174
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(SmallContextBackend), config);
        let shared = Arc::new(RwLock::new(model));
        let generator =
            TemplateGenerator::with_models(PatternClassifier::new(), Some(shared), "Qwen");

        let messages = vec![
            crate::providers::Message::user("ancient"),
            crate::providers::Message::assistant("ancient reply"),
            crate::providers::Message::user("recent one"),
            crate::providers::Message::assistant("recent two words reply"),
            crate::providers::Message::user("current question"),
        ];

        let (system_prompt, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            !query.contains("ancient"),
            "the oldest exchange must be dropped once the model's real token budget is \
             exceeded, not kept the way an unconditional join would keep it: {query:?}"
        );
        assert!(
            query.contains("recent one") && query.contains("recent two words reply"),
            "exchanges that still fit the budget must survive: {query:?}"
        );
        assert!(
            query.contains("current question"),
            "the current question must always survive trimming: {query:?}"
        );

        // The real invariant this fix protects: the formatted prompt plus
        // the reserved response budget must fit inside the model's real
        // context window, instead of silently overflowing it.
        let formatted = generator.format_chat_prompt_with_system(&system_prompt, &query);
        let prompt_tokens = formatted.split_whitespace().count();
        assert!(
            prompt_tokens + LOCAL_RESPONSE_TOKEN_RESERVE <= 174,
            "formatted prompt ({prompt_tokens} tokens) plus the reserved response budget \
             ({LOCAL_RESPONSE_TOKEN_RESERVE}) must fit inside the model's real context \
             window (174 tokens), not overflow it: {formatted:?}"
        );
    }

    /// When the model's real context window comfortably covers every
    /// candidate exchange, trimming must not discard anything it doesn't
    /// need to -- this fix bounds the prompt, it doesn't gratuitously
    /// shrink it.
    #[test]
    fn prompt_parts_keeps_full_history_when_the_models_real_budget_covers_it() {
        struct RoomyContextBackend;

        impl TextGeneration for RoomyContextBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(Vec::new())
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                Ok((0..text.split_whitespace().count() as u32).collect())
            }

            fn decode_tokens(&self, _tokens: &[u32]) -> Result<String> {
                Ok(String::new())
            }

            fn name(&self) -> &str {
                "roomy-context-test-model"
            }

            fn context_length(&self) -> u32 {
                8192
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(RoomyContextBackend), config);
        let shared = Arc::new(RwLock::new(model));
        let generator =
            TemplateGenerator::with_models(PatternClassifier::new(), Some(shared), "Qwen");

        let messages = vec![
            crate::providers::Message::user("earlier unrelated turn"),
            crate::providers::Message::assistant("earlier unrelated reply"),
            crate::providers::Message::user("current question"),
        ];

        let (_, query) = generator.prompt_parts(&messages).unwrap();

        assert!(
            query.contains("earlier unrelated turn") && query.contains("earlier unrelated reply"),
            "a roomy real context budget must not drop history a fixed-count window would \
             already have kept: {query:?}"
        );
        assert!(
            query.contains("current question"),
            "the current question must still be present in the composed query: {query:?}"
        );
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
            model_name: "Qwen".to_string(),
        })
    }
}
