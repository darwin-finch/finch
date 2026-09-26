// Local Generation Module
//
// Coordinates optional local response generation through pattern classification,
// learned responses, and a configured model when one is available.

mod generator;
mod patterns;
mod tiered_history;

pub use generator::GeneratedResponse;

use generator::TemplateGenerator;
use patterns::PatternClassifier;

use crate::generators::{GeneratorResponse, ToolUse};
use crate::models::GeneratorModel;
use crate::models::LocalModelAdapter;
use crate::models::{ToolCallParser, ToolPromptFormatter};
use crate::providers::{ContentBlock, Message};
use crate::tools::ToolDefinition;
use crate::training::batch_trainer::BatchTrainer;
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Local generation system that coordinates pattern classification and response generation
pub struct LocalGenerator {
    pattern_classifier: PatternClassifier,
    response_generator: TemplateGenerator,
    enabled: bool,
    /// Display name of the loaded model (e.g. "Qwen 2.5", "Llama 3", "Gemma 2")
    model_name: String,
}

impl LocalGenerator {
    /// Create new local generator without neural models
    pub fn new() -> Self {
        Self::with_models(None)
    }

    /// Create local generator with optional neural models
    pub fn with_models(neural_generator: Option<Arc<RwLock<GeneratorModel>>>) -> Self {
        let pattern_classifier = PatternClassifier::new();

        // Extract actual model name from GeneratorModel if available
        let model_name = if let Some(ref gen) = neural_generator {
            // Try non-blocking read to get model name (avoid deadlock in async context)
            match gen.try_read() {
                Ok(g) => g.name().to_string(),
                Err(_) => {
                    // Lock contention - try to get from config
                    Self::extract_model_name_from_config()
                }
            }
        } else {
            // Model not loaded yet - get from config
            Self::extract_model_name_from_config()
        };

        let response_generator = TemplateGenerator::with_models(
            pattern_classifier.clone(),
            neural_generator,
            &model_name,
        );

        Self {
            pattern_classifier,
            response_generator,
            enabled: true,
            model_name,
        }
    }

    /// Return the name of the currently-configured local model.
    pub fn model_name(&self) -> &str {
        &self.model_name
    }

    /// Extract model family name from config file
    ///
    /// This is used when the model isn't loaded yet or the lock is held,
    /// to ensure the correct adapter is selected during initialization.
    fn extract_model_name_from_config() -> String {
        // Read current config to determine model family
        if let Ok(config) = crate::config::load_config() {
            let family_name = config.backend.model_family.name();
            return family_name.to_string();
        }
        "LocalModel".to_string() // Final fallback
    }

    /// Try to generate a local response from patterns
    pub fn try_generate_from_pattern(&mut self, query: &str) -> Result<Option<String>> {
        if !self.enabled {
            return Ok(None);
        }

        // Classify the query
        let (_pattern, confidence) = self.pattern_classifier.classify(query);

        // Only try local generation if confidence is high enough
        if confidence < 0.7 {
            return Ok(None);
        }

        // Try to generate response
        match self.response_generator.generate(query) {
            Ok(response) => {
                // Only return if confidence is high enough
                if response.confidence >= 0.7 {
                    Ok(Some(response.text))
                } else {
                    Ok(None)
                }
            }
            Err(_) => Ok(None),
        }
    }

    /// Try to generate a response with streaming callback
    ///
    /// Calls the callback for each generated token with (token_id, token_text).
    /// This enables Server-Sent Events streaming to the client.
    pub fn try_generate_from_pattern_streaming<F>(
        &mut self,
        messages: &[Message],
        token_callback: F,
    ) -> Result<Option<GeneratorResponse>>
    where
        F: FnMut(u32, &str) + Send + 'static,
    {
        if !self.enabled {
            return Ok(None);
        }

        // Delegate to response generator with streaming callback
        self.response_generator
            .generate_streaming(messages, token_callback)
    }

    /// Try to generate a response from patterns with tools
    ///
    /// This method is used by the daemon to support tool execution. Delegates
    /// to the configured local chat generator if available. When `tools` is
    /// non-empty, the tool definitions are formatted into the prompt the same
    /// way `QwenGenerator::generate_proposing_tools` does (via
    /// [`ToolPromptFormatter`]), and any `<tool_use>` markup the model emits
    /// is parsed back out (via [`ToolCallParser`]) into real `tool_uses`
    /// instead of the previous always-empty stub (#1276). Streaming tool
    /// calls are still out of scope; this covers only the non-streaming
    /// daemon path (`handle_local_only_query` and the `local_only` branch of
    /// `handle_chat_completions` in `src/server/openai_handlers.rs`).
    ///
    /// Returns `Ok(None)` only when local generation is disabled; a real
    /// generation failure (e.g. the prompt exceeding the model's context
    /// window) returns `Err` so the caller never mistakes the failure text
    /// for a candidate wire response (#1234).
    pub fn try_generate_from_pattern_with_tools(
        &mut self,
        messages: &[Message],
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<GeneratorResponse>> {
        if !self.enabled {
            return Ok(None);
        }

        let tools = tools.filter(|tools| !tools.is_empty());
        let augmented_messages;
        let messages_to_send: &[Message] = match &tools {
            Some(tools) => {
                augmented_messages = Self::inject_tool_definitions(messages, tools);
                &augmented_messages
            }
            None => messages,
        };

        // Generate using the response generator (which tries neural model first)
        match self.response_generator.generate_messages(messages_to_send) {
            Ok(generated) => {
                // Convert generated response to GeneratorResponse format
                use crate::generators::ResponseMetadata;

                let (text, tool_uses, content_blocks, stop_reason) =
                    if ToolCallParser::has_tool_calls(&generated.text) {
                        let parsed = ToolCallParser::parse(&generated.text).with_context(|| {
                            format!(
                                "failed to parse local tool-call markup from model output: {}",
                                generated.text
                            )
                        })?;
                        let text = ToolCallParser::extract_text(&generated.text);
                        let mut content_blocks = Vec::new();
                        if !text.is_empty() {
                            content_blocks.push(ContentBlock::Text { text: text.clone() });
                        }
                        let tool_uses: Vec<ToolUse> = parsed
                            .into_iter()
                            .map(|call| {
                                content_blocks.push(ContentBlock::ToolUse {
                                    id: call.id.clone(),
                                    name: call.name.clone(),
                                    input: call.input.clone(),
                                });
                                ToolUse {
                                    id: call.id,
                                    name: call.name,
                                    input: call.input,
                                }
                            })
                            .collect();
                        (
                            text,
                            tool_uses,
                            content_blocks,
                            Some("tool_use".to_string()),
                        )
                    } else {
                        (
                            generated.text.clone(),
                            Vec::new(),
                            vec![ContentBlock::Text {
                                text: generated.text.clone(),
                            }],
                            None,
                        )
                    };

                let response = GeneratorResponse {
                    text,
                    content_blocks,
                    tool_uses,
                    metadata: ResponseMetadata {
                        // The generator field names the local path; the model
                        // field reports the family/model actually configured
                        // or loaded (never a hardcoded claim).
                        generator: "local".to_string(),
                        model: self.model_name().to_string(),
                        confidence: Some(generated.confidence),
                        stop_reason,
                        input_tokens: None,
                        output_tokens: Some(generated.text.split_whitespace().count() as u32),
                        latency_ms: None,
                        primary_allowance_used_percent: None,
                        secondary_allowance_used_percent: None,
                    },
                };

                Ok(Some(response))
            }
            Err(e) => {
                // Propagate the real failure (e.g. a context-window overflow)
                // instead of swallowing it into `Ok(None)`: callers must be
                // able to tell "local generation genuinely produced nothing"
                // apart from "local generation errored," and the error text
                // itself must never be mistaken for a candidate response
                // (#1234). `e`'s message is already a clean, actionable
                // description (see `TemplateGenerator::generate_with_system`).
                tracing::warn!("Local generation failed: {}", e);
                Err(e)
            }
        }
    }

    /// Insert a system-role message carrying the tool definitions, formatted
    /// the same way `QwenGenerator::format_prompt_with_tools` formats them
    /// (`ToolPromptFormatter::format_tools_for_prompt`), so the model sees
    /// the same `<tool_use>` markup contract on both the daemon path and the
    /// in-process `QwenGenerator` path.
    ///
    /// The block is inserted immediately after the last existing system
    /// message (or at the front, if there is none) so it joins with the
    /// caller's own system content the same way `TemplateGenerator::
    /// prompt_parts` already joins multiple system messages -- the caller's
    /// system contract (e.g. the Finch VM wire ABI) is preserved verbatim,
    /// never replaced.
    fn inject_tool_definitions(messages: &[Message], tools: &[ToolDefinition]) -> Vec<Message> {
        let tool_prompt = ToolPromptFormatter::format_tools_for_prompt(tools);
        let mut augmented = Vec::with_capacity(messages.len() + 1);
        let mut last_system_idx = None;
        for (idx, message) in messages.iter().enumerate() {
            augmented.push(message.clone());
            if message.role == "system" {
                last_system_idx = Some(idx);
            }
        }

        let tool_message = Message {
            role: "system".to_string(),
            content: vec![ContentBlock::Text { text: tool_prompt }],
        };

        match last_system_idx {
            Some(idx) => augmented.insert(idx + 1, tool_message),
            None => augmented.insert(0, tool_message),
        }

        augmented
    }

    /// Learn from a Claude response
    pub fn learn_from_claude(
        &mut self,
        query: &str,
        response: &str,
        quality_score: f64,
        batch_trainer: Option<&Arc<RwLock<BatchTrainer>>>,
    ) {
        self.response_generator
            .learn_from_claude(query, response, quality_score, batch_trainer);
    }

    /// Enable/disable local generation
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Check if enabled
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the model adapter for cleaning
    pub fn get_adapter(&self) -> Box<dyn LocalModelAdapter> {
        self.response_generator.get_adapter()
    }

    /// Save local generator to file
    pub fn save<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()> {
        use crate::models::LearningModel;
        self.response_generator.save(path.as_ref())
    }

    /// Load local generator from file
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        use crate::models::LearningModel;
        let response_generator = TemplateGenerator::load(path.as_ref())?;
        // TemplateGenerator contains its own pattern_classifier, so we create a fresh one
        // for the LocalGenerator's copy (they stay in sync via learning)
        let pattern_classifier = PatternClassifier::new();

        Ok(Self {
            pattern_classifier,
            response_generator,
            enabled: true,
            model_name: Self::extract_model_name_from_config(),
        })
    }
}

impl Default for LocalGenerator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_has_no_adapter_hot_reload_path() {
        let source = include_str!("mod.rs");
        let reload_method = ["check_and_reload", "_adapter"].concat();
        let adapter_directory = [".join(", "\"adapters\")"].concat();

        assert!(!source.contains(&reload_method));
        assert!(!source.contains(&adapter_directory));
    }

    #[test]
    fn test_local_generation_greeting() {
        let mut generator = LocalGenerator::new();

        // Try to generate response for greeting
        let result = generator.try_generate_from_pattern("Hello!");
        assert!(result.is_ok());

        if let Ok(Some(response)) = result {
            assert!(!response.is_empty());
            assert!(
                response.to_lowercase().contains("hello") || response.to_lowercase().contains("hi")
            );
        }
    }

    #[test]
    fn test_local_generation_complex_query() {
        let mut generator = LocalGenerator::new();

        // Complex query should return None (forward to Claude)
        let result = generator.try_generate_from_pattern(
            "Explain the implementation details of Rust's async/await system including how the compiler transforms async functions into state machines"
        );

        assert!(result.is_ok());
        assert!(result.unwrap().is_none()); // Should forward to Claude
    }

    #[test]
    fn test_learn_from_claude() {
        let mut generator = LocalGenerator::new();

        // Learn from a Claude response
        generator.learn_from_claude(
            "What is Rust?",
            "Rust is a systems programming language focused on safety, speed, and concurrency.",
            0.9,
            None,
        );

        // Learning should not crash
        // (Response may or may not be used for local generation depending on confidence)
    }

    /// Production-boundary regression for the caller's system prompt being
    /// dropped on the local path: the daemon reaches the neural backend
    /// through `LocalGenerator::try_generate_from_pattern_with_tools`, not
    /// through the private prompt-assembly helper. This drives that real
    /// entry point with a mock `TextGeneration` backend that records exactly
    /// what text got tokenized, proving the caller's system message (the
    /// Finch VM wire contract) reaches the model instead of being replaced
    /// by the generic local constitution.
    #[test]
    fn local_daemon_boundary_forwards_caller_system_prompt_to_model_backend() {
        use crate::config::ExecutionTarget;
        use crate::models::{
            GeneratorConfig, InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize,
        };
        use crate::providers::ContentBlock;
        use std::sync::Mutex;

        struct CapturingBackend {
            captured_prompt: Arc<Mutex<String>>,
        }

        impl crate::models::TextGeneration for CapturingBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(b"the vm reply"
                    .iter()
                    .map(|byte| u32::from(*byte))
                    .collect())
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                *self.captured_prompt.lock().expect("lock captured prompt") = text.to_string();
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

        let captured_prompt = Arc::new(Mutex::new(String::new()));
        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let backend = CapturingBackend {
            captured_prompt: Arc::clone(&captured_prompt),
        };
        let model = GeneratorModel::from_test_backend(Box::new(backend), config);
        let shared = Arc::new(RwLock::new(model));

        let mut local_generator = LocalGenerator::with_models(Some(shared));

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: vec![ContentBlock::Text {
                    text: "FINCH VM WIRE CONTRACT: emit exactly one Co-Forth program per turn"
                        .to_string(),
                }],
            },
            Message::user("what should I run"),
        ];

        let response = local_generator
            .try_generate_from_pattern_with_tools(&messages, None)
            .expect("local daemon boundary must not error")
            .expect("neural backend is configured, so a response must be produced");

        assert_eq!(
            response.text, "the vm reply",
            "unexpected response text: {response:?}"
        );

        let sent_to_model = captured_prompt.lock().expect("lock captured prompt");
        assert!(
            sent_to_model.contains("FINCH VM WIRE CONTRACT"),
            "the caller's system contract must reach the tokenizer input, got: {sent_to_model}"
        );
        assert!(
            !sent_to_model.contains("helpful coding assistant"),
            "the generic local constitution must not replace the caller's system contract, got: {sent_to_model}"
        );
    }

    /// Regression for #1234: a local generation failure (e.g. the prompt plus
    /// requested output exceeding the GGUF context window) must reach the
    /// daemon boundary as `Err`, never as `Ok(Some(response))` carrying the
    /// failure text disguised as the model's reply. Before the fix,
    /// `TemplateGenerator::generate_with_system` turned this `Err` into a
    /// fabricated `Ok(GeneratedResponse { text: "[NEURAL GENERATION
    /// FAILED]: ...", confidence: 0.0, .. })`, and
    /// `try_generate_from_pattern_with_tools` forwarded it unchecked as
    /// `Ok(Some(response))` -- the caller (the daemon's OpenAI-compatible
    /// handler) then returned that text as a normal 200 response, which
    /// `query_processor.rs` fed straight into `raw_wire_source` and the
    /// Lisp/Forth compiler, producing a nonsensical `E-FORTH-SIG-001`
    /// diagnostic about the word "NEURAL" as if it were a type definition.
    #[test]
    fn local_daemon_boundary_surfaces_context_overflow_as_err_not_fake_response() {
        use crate::config::ExecutionTarget;
        use crate::models::{
            GeneratorConfig, InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize,
        };

        struct ContextOverflowBackend;

        impl crate::models::TextGeneration for ContextOverflowBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                // Same shape as the real failure reported live:
                // `src/models/loaders/llama_cpp.rs`'s `generate_inner` bails
                // with this exact message when the prompt plus requested
                // output would overflow the model's context window.
                anyhow::bail!(
                    "GGUF prompt (1987) plus requested output (100) exceeds context (2048)"
                )
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                Ok(text.bytes().map(u32::from).collect())
            }

            fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
                let bytes = tokens.iter().map(|token| *token as u8).collect();
                Ok(String::from_utf8(bytes)?)
            }

            fn name(&self) -> &str {
                "context-overflow test backend"
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
            family: ModelFamily::Gemma2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let model = GeneratorModel::from_test_backend(Box::new(ContextOverflowBackend), config);
        let mut local_generator = LocalGenerator::with_models(Some(Arc::new(RwLock::new(model))));

        let messages = vec![Message::user("do something that blows the context budget")];

        let result = local_generator.try_generate_from_pattern_with_tools(&messages, None);

        let error = match result {
            Err(error) => error,
            Ok(response) => panic!(
                "a context-window overflow must surface as Err, not as a disguised \
                 successful response that a caller could feed to the wire compiler: \
                 {response:?}"
            ),
        };

        let message = error.to_string();
        assert!(
            message.contains("exceeds context"),
            "the clean error must preserve the actionable context-overflow detail, \
             got: {message:?}"
        );
        assert!(
            !message.contains("NEURAL GENERATION FAILED"),
            "the error must not carry the old disguised-response marker text, \
             got: {message:?}"
        );
    }

    /// Production-boundary regression for #1276: before this fix,
    /// `try_generate_from_pattern_with_tools` took `_tools` (leading
    /// underscore, genuinely unused) and unconditionally returned
    /// `tool_uses: vec![]`, so no tool-using turn against a local model
    /// through the daemon ever produced a real tool call, even when the
    /// model emitted `<tool_use>` markup. This drives the real daemon entry
    /// point (the same one `handle_local_only_query` and the `local_only`
    /// branch of `handle_chat_completions` in
    /// `src/server/openai_handlers.rs` call) with a mock `TextGeneration`
    /// backend that (a) records the exact prompt it was tokenized with, so
    /// the test can prove the tool definitions actually reached the model,
    /// and (b) always answers with `<tool_use>` markup, so the test can
    /// prove that markup is parsed back into `GeneratorResponse.tool_uses`
    /// and `content_blocks` -- the two fields the daemon boundary and the
    /// OpenAI-response converter (`convert_response_to_openai`) actually
    /// read -- instead of being dropped on the floor.
    #[test]
    fn local_daemon_boundary_wires_tool_definitions_into_prompt_and_parses_tool_uses_back_out() {
        use crate::config::ExecutionTarget;
        use crate::models::{
            GeneratorConfig, InferenceProvider, ModelFamily, ModelLoadConfig, ModelSize,
        };
        use crate::tools::ToolInputSchema;
        use std::sync::Mutex;

        struct ToolMarkupBackend {
            captured_prompt: Arc<Mutex<String>>,
        }

        const RAW_MODEL_OUTPUT: &str = "I'll read that file.\n\n\
            <tool_use>\n  <name>read</name>\n  \
            <parameters>{\"file_path\": \"/tmp/a.rs\"}</parameters>\n</tool_use>\n";

        impl crate::models::TextGeneration for ToolMarkupBackend {
            fn generate(&mut self, _input_ids: &[u32], _max: usize) -> Result<Vec<u32>> {
                Ok(RAW_MODEL_OUTPUT.bytes().map(u32::from).collect())
            }

            fn tokenize(&self, text: &str) -> Result<Vec<u32>> {
                *self.captured_prompt.lock().expect("lock captured prompt") = text.to_string();
                Ok(text.bytes().map(u32::from).collect())
            }

            fn decode_tokens(&self, tokens: &[u32]) -> Result<String> {
                let bytes = tokens.iter().map(|token| *token as u8).collect();
                Ok(String::from_utf8(bytes)?)
            }

            fn name(&self) -> &str {
                "tool-markup test backend"
            }

            fn as_any(&self) -> &dyn std::any::Any {
                self
            }

            fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
                self
            }
        }

        let captured_prompt = Arc::new(Mutex::new(String::new()));
        let config = GeneratorConfig::Pretrained(ModelLoadConfig {
            provider: InferenceProvider::LlamaCpp,
            family: ModelFamily::Qwen2,
            size: ModelSize::Small,
            target: ExecutionTarget::Cpu,
            model_path: None,
        });
        let backend = ToolMarkupBackend {
            captured_prompt: Arc::clone(&captured_prompt),
        };
        let model = GeneratorModel::from_test_backend(Box::new(backend), config);
        let mut local_generator = LocalGenerator::with_models(Some(Arc::new(RwLock::new(model))));

        let messages = vec![Message::user("please read /tmp/a.rs")];
        let tools = vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to the file")]),
        }];

        let response = local_generator
            .try_generate_from_pattern_with_tools(&messages, Some(tools))
            .expect("tool-using turn through the daemon's non-streaming local path must not error")
            .expect("neural backend is configured, so a response must be produced");

        let sent_to_model = captured_prompt.lock().expect("lock captured prompt");
        assert!(
            sent_to_model.contains("<tool_use>") && sent_to_model.contains("### read"),
            "the tool definitions must reach the model's prompt via ToolPromptFormatter, \
             same as QwenGenerator::format_prompt_with_tools does, got: {sent_to_model}"
        );
        drop(sent_to_model);

        assert_eq!(
            response.tool_uses.len(),
            1,
            "the model's <tool_use> markup must be parsed back into a real tool call \
             instead of the old always-empty stub: {response:?}"
        );
        assert_eq!(response.tool_uses[0].name, "read");
        assert_eq!(response.tool_uses[0].input["file_path"], "/tmp/a.rs");
        assert!(
            !response.tool_uses[0].id.is_empty(),
            "the parsed tool call must carry an id for the event loop: {:?}",
            response.tool_uses[0]
        );
        assert_eq!(
            response.metadata.stop_reason.as_deref(),
            Some("tool_use"),
            "a turn that proposes a tool call must stop as tool_use, not silently as \
             end_turn/None: {response:?}"
        );

        // `handle_local_only_query` and the `local_only` branch of
        // `handle_chat_completions` (src/server/openai_handlers.rs) forward
        // `response.content_blocks`, not `tool_uses`, into
        // `convert_response_to_openai` -- so the tool call must also survive
        // as a `ContentBlock::ToolUse` there, not only in the `tool_uses` field.
        let tool_use_blocks: Vec<_> = response
            .content_blocks
            .iter()
            .filter(|block| matches!(block, ContentBlock::ToolUse { .. }))
            .collect();
        assert_eq!(
            tool_use_blocks.len(),
            1,
            "the daemon's response converter reads content_blocks, so the tool call must \
             appear there too, not only in the tool_uses field: {:?}",
            response.content_blocks
        );
        assert_eq!(response.text.trim(), "I'll read that file.");
    }
}
