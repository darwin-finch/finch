// Local Generation Module
//
// Handles local response generation through pattern classification and learned responses
// This is the core of Shammah's "95% local processing" capability

pub mod generator;
pub mod patterns;

pub use generator::{GeneratedResponse, TemplateGenerator};
pub use patterns::{PatternClassifier, QueryPattern};

use crate::claude::Message;
use crate::generators::GeneratorResponse;
use crate::models::adapters::LocalModelAdapter;
use crate::models::GeneratorModel;
use crate::tools::types::ToolDefinition;
use crate::training::batch_trainer::BatchTrainer;
use anyhow::Result;
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
        // Check for newer adapter before generation
        self.check_and_reload_adapter()?;

        if !self.enabled {
            return Ok(None);
        }

        // Delegate to response generator with streaming callback
        self.response_generator
            .generate_streaming(messages, token_callback)
    }

    /// Try to generate a response from patterns with tools
    ///
    /// This method is used by the daemon to support tool execution.
    /// Delegates to the neural generator (ONNX model) if available.
    pub fn try_generate_from_pattern_with_tools(
        &mut self,
        messages: &[Message],
        _tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<GeneratorResponse>> {
        // Check for newer adapter before generation
        self.check_and_reload_adapter()?;

        if !self.enabled {
            return Ok(None);
        }

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

        // Generate using the response generator (which tries neural model first)
        match self.response_generator.generate(query) {
            Ok(generated) => {
                // Convert generated response to GeneratorResponse format
                use crate::generators::ResponseMetadata;

                let response = GeneratorResponse {
                    text: generated.text.clone(),
                    content_blocks: vec![crate::claude::ContentBlock::Text {
                        text: generated.text.clone(),
                    }],
                    tool_uses: vec![], // TODO: Support tool use when integrated with QwenGenerator
                    metadata: ResponseMetadata {
                        generator: "qwen-local".to_string(),
                        model: "Qwen2.5-1.5B-Instruct".to_string(), // TODO: Get from config
                        confidence: Some(generated.confidence),
                        stop_reason: None,
                        input_tokens: None,
                        output_tokens: Some(generated.text.split_whitespace().count() as u32),
                        latency_ms: None,
                    },
                };

                Ok(Some(response))
            }
            Err(e) => {
                tracing::warn!("Local generation failed: {}", e);
                Ok(None)
            }
        }
    }

    /// Check if a newer adapter is available and reload if so
    fn check_and_reload_adapter(&mut self) -> Result<()> {
        let adapters_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".finch")
            .join("adapters");

        if !adapters_dir.exists() {
            return Ok(());
        }

        // Find latest adapter
        if let Some(latest_adapter) = self.find_latest_adapter(&adapters_dir)? {
            // TODO: Track last loaded adapter timestamp and only reload if newer
            // For now, we skip reload since adapter loading isn't implemented yet
            tracing::debug!(
                "Found adapter: {} (reload not yet implemented)",
                latest_adapter.display()
            );
        }

        Ok(())
    }

    /// Find the most recent adapter in the adapters directory
    fn find_latest_adapter(
        &self,
        adapters_dir: &std::path::Path,
    ) -> Result<Option<std::path::PathBuf>> {
        use std::fs;

        let mut latest: Option<(std::path::PathBuf, std::time::SystemTime)> = None;

        for entry in fs::read_dir(adapters_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) == Some("safetensors") {
                let metadata = fs::metadata(&path)?;
                let modified = metadata.modified()?;

                if latest.is_none() || modified > latest.as_ref().unwrap().1 {
                    latest = Some((path, modified));
                }
            }
        }

        Ok(latest.map(|(path, _)| path))
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

    /// Get pattern classifier
    pub fn pattern_classifier(&self) -> &PatternClassifier {
        &self.pattern_classifier
    }

    /// Get response generator
    pub fn response_generator(&mut self) -> &mut TemplateGenerator {
        &mut self.response_generator
    }

    /// Get the model adapter for cleaning
    pub fn get_adapter(&self) -> Box<dyn LocalModelAdapter> {
        self.response_generator.get_adapter()
    }

    /// Save local generator to file
    pub fn save<P: AsRef<std::path::Path>>(&self, path: P) -> Result<()> {
        use crate::models::learning::LearningModel;
        self.response_generator.save(path.as_ref())
    }

    /// Load local generator from file
    pub fn load<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        use crate::models::learning::LearningModel;
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
}
