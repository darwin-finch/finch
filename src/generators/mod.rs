// Unified generator interface for Claude, Qwen, and future generators

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::claude::{ContentBlock, Message};
use crate::tools::types::ToolDefinition;

pub(crate) const MAX_RESPONSE_MODEL_BYTES: usize = 256;

pub(crate) fn validate_response_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > MAX_RESPONSE_MODEL_BYTES
        || !model.bytes().all(|byte| byte.is_ascii_graphic())
    {
        anyhow::bail!("Provider response model metadata was invalid");
    }
    Ok(())
}

// Re-export implementations
pub mod claude;
pub mod daemon_local;
pub mod qwen;

/// Associates a configured profile name with a generator without changing its
/// provider-specific response metadata.
pub struct ProfiledGenerator {
    profile_name: String,
    inner: std::sync::Arc<dyn Generator>,
}

impl ProfiledGenerator {
    pub fn new(profile_name: impl Into<String>, inner: std::sync::Arc<dyn Generator>) -> Self {
        Self {
            profile_name: profile_name.into(),
            inner,
        }
    }
}

#[async_trait]
impl Generator for ProfiledGenerator {
    async fn generate(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse> {
        self.inner.generate(messages, tools).await
    }

    async fn generate_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        self.inner.generate_stream(messages, tools).await
    }

    async fn generate_stream_cancellable(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        self.inner
            .generate_stream_cancellable(messages, tools, cancellation_token)
            .await
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        self.inner.capabilities()
    }

    fn name(&self) -> &str {
        &self.profile_name
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
}

/// Unified generator interface for Claude, Qwen, and future generators
#[async_trait]
pub trait Generator: Send + Sync {
    /// Generate response with full conversation context
    async fn generate(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse>;

    /// Stream response if supported (returns None if not supported)
    async fn generate_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>>;

    async fn generate_stream_cancellable(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        _cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        self.generate_stream(messages, tools).await
    }

    /// Get generator capabilities
    fn capabilities(&self) -> &GeneratorCapabilities;

    /// Get generator name for logging
    fn name(&self) -> &str;

    /// Exact upstream model when it is known before generation. Local or
    /// compatibility generators may retain the profile name as a fallback.
    fn model_name(&self) -> &str {
        self.name()
    }
}

/// Generator capabilities (what features are supported)
#[derive(Debug, Clone)]
pub struct GeneratorCapabilities {
    pub supports_streaming: bool,
    pub supports_tools: bool,
    pub supports_conversation: bool,
    pub max_context_messages: Option<usize>,
}

/// Unified response format
#[derive(Debug, Clone)]
pub struct GeneratorResponse {
    /// Primary text response
    pub text: String,

    /// Content blocks (for rich responses)
    pub content_blocks: Vec<ContentBlock>,

    /// Tool uses requested by generator
    pub tool_uses: Vec<ToolUse>,

    /// Metadata about generation
    pub metadata: ResponseMetadata,
}

#[derive(Debug, Clone)]
pub struct ResponseMetadata {
    pub generator: String,           // "claude" | "qwen"
    pub model: String,               // specific model name
    pub confidence: Option<f64>,     // for Qwen
    pub stop_reason: Option<String>, // for Claude
    pub input_tokens: Option<u32>,   // Input token count (if available)
    pub output_tokens: Option<u32>,  // Output token count (if available)
    pub latency_ms: Option<u64>,     // Response latency in milliseconds
    pub primary_allowance_used_percent: Option<f32>,
    pub secondary_allowance_used_percent: Option<f32>,
}

/// Streaming chunk (text delta or complete block)
#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta(String),                  // Incremental text
    ContentBlockComplete(ContentBlock), // Complete tool_use or text block
    /// Provider-reported model that actually served the streaming response.
    ResponseMetadata {
        model: String,
    },
    /// Usage metadata from message_start — carries the input token count
    /// reported by the API before any text arrives.
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
    /// Provider-owned subscription allowance snapshot. Percentages are
    /// bounded to 0..=100 and are not Platform billing data.
    Allowance {
        primary_used_percent: Option<f32>,
        secondary_used_percent: Option<f32>,
    },
}

/// Tool use request from generator
#[derive(Debug, Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl ToolUse {
    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock {
        ContentBlock::ToolUse {
            id: self.id.clone(),
            name: self.name.clone(),
            input: self.input.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::ContentBlock;

    fn make_tool_use(id: &str, name: &str) -> ToolUse {
        ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input: serde_json::json!({"file_path": "/tmp/test.txt"}),
        }
    }

    // --- ToolUse ---

    #[test]
    fn test_tool_use_to_content_block_preserves_fields() {
        let tu = make_tool_use("toolu_1", "read");
        let block = tu.to_content_block();
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "read");
                assert_eq!(input["file_path"], "/tmp/test.txt");
            }
            _ => panic!("Expected ToolUse ContentBlock"),
        }
    }

    #[test]
    fn test_tool_use_to_content_block_can_round_trip_via_clone() {
        let tu = make_tool_use("toolu_2", "grep");
        let block = tu.to_content_block();
        // Verify the original ToolUse is still usable after to_content_block (it clones)
        assert_eq!(tu.name, "grep");
        assert!(matches!(block, ContentBlock::ToolUse { .. }));
    }

    // --- GeneratorCapabilities ---

    #[test]
    fn test_generator_capabilities_all_enabled() {
        let caps = GeneratorCapabilities {
            supports_streaming: true,
            supports_tools: true,
            supports_conversation: true,
            max_context_messages: Some(100),
        };
        assert!(caps.supports_streaming);
        assert!(caps.supports_tools);
        assert!(caps.supports_conversation);
        assert_eq!(caps.max_context_messages, Some(100));
    }

    #[test]
    fn test_generator_capabilities_local_model_profile() {
        // Typical local model: no streaming, no tools
        let caps = GeneratorCapabilities {
            supports_streaming: false,
            supports_tools: false,
            supports_conversation: true,
            max_context_messages: Some(8),
        };
        assert!(!caps.supports_streaming);
        assert!(!caps.supports_tools);
        assert_eq!(caps.max_context_messages, Some(8));
    }

    #[test]
    fn test_generator_capabilities_unlimited_context() {
        let caps = GeneratorCapabilities {
            supports_streaming: true,
            supports_tools: true,
            supports_conversation: true,
            max_context_messages: None, // None = no limit
        };
        assert!(caps.max_context_messages.is_none());
    }

    // --- ResponseMetadata ---

    #[test]
    fn test_response_metadata_construction() {
        let meta = ResponseMetadata {
            generator: "claude".to_string(),
            model: "claude-sonnet-4-6".to_string(),
            confidence: None,
            stop_reason: Some("end_turn".to_string()),
            input_tokens: Some(150),
            output_tokens: Some(400),
            latency_ms: Some(1200),
            primary_allowance_used_percent: None,
            secondary_allowance_used_percent: None,
        };
        assert_eq!(meta.generator, "claude");
        assert_eq!(meta.input_tokens, Some(150));
        assert_eq!(meta.output_tokens, Some(400));
        assert_eq!(meta.latency_ms, Some(1200));
    }

    #[test]
    fn test_response_metadata_local_model_has_confidence() {
        let meta = ResponseMetadata {
            generator: "qwen".to_string(),
            model: "Qwen2.5-3B".to_string(),
            confidence: Some(0.87),
            stop_reason: None,
            input_tokens: None,
            output_tokens: None,
            latency_ms: Some(45),
            primary_allowance_used_percent: None,
            secondary_allowance_used_percent: None,
        };
        assert_eq!(meta.confidence, Some(0.87));
        assert!(meta.stop_reason.is_none());
    }

    // --- StreamChunk ---

    #[test]
    fn test_stream_chunk_text_delta() {
        let chunk = StreamChunk::TextDelta("hello ".to_string());
        match chunk {
            StreamChunk::TextDelta(text) => assert_eq!(text, "hello "),
            _ => panic!("Expected TextDelta"),
        }
    }

    #[test]
    fn test_stream_chunk_content_block_complete() {
        let block = ContentBlock::text("final answer");
        let chunk = StreamChunk::ContentBlockComplete(block);
        match chunk {
            StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                assert_eq!(text, "final answer");
            }
            _ => panic!("Expected ContentBlockComplete with Text"),
        }
    }

    #[test]
    fn test_stream_chunk_usage() {
        let chunk = StreamChunk::Usage {
            input_tokens: 1024,
            output_tokens: 256,
        };
        match chunk {
            StreamChunk::Usage {
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(input_tokens, 1024);
                assert_eq!(output_tokens, 256);
            }
            _ => panic!("Expected Usage"),
        }
    }

    #[test]
    fn test_stream_chunk_response_metadata_preserves_actual_model() {
        let chunk = StreamChunk::ResponseMetadata {
            model: "gpt-5.6-sol-served".to_string(),
        };
        match chunk {
            StreamChunk::ResponseMetadata { model } => {
                assert_eq!(model, "gpt-5.6-sol-served");
            }
            _ => panic!("Expected ResponseMetadata"),
        }
    }

    #[test]
    fn test_response_model_metadata_is_bounded_and_log_safe() {
        validate_response_model(&"m".repeat(MAX_RESPONSE_MODEL_BYTES)).unwrap();
        let empty_error = validate_response_model("").unwrap_err().to_string();
        assert_eq!(empty_error, "Provider response model metadata was invalid");
        for model in [
            format!(
                "OVERSIZED_MODEL_SECRET{}",
                "m".repeat(MAX_RESPONSE_MODEL_BYTES)
            ),
            "CONTROL_MODEL_SECRET\nINJECTED".to_string(),
            "SPACE_MODEL_SECRET INJECTED".to_string(),
            "UNICODE_MODEL_SECRET_mødël".to_string(),
        ] {
            let error = validate_response_model(&model).unwrap_err().to_string();
            assert_eq!(error, "Provider response model metadata was invalid");
            assert!(!error.contains(&model));
        }
    }

    // --- GeneratorResponse ---

    #[test]
    fn test_generator_response_construction() {
        let response = GeneratorResponse {
            text: "The answer is 42".to_string(),
            content_blocks: vec![ContentBlock::text("The answer is 42")],
            tool_uses: vec![],
            metadata: ResponseMetadata {
                generator: "claude".to_string(),
                model: "claude-sonnet-4-6".to_string(),
                confidence: None,
                stop_reason: Some("end_turn".to_string()),
                input_tokens: Some(10),
                output_tokens: Some(5),
                latency_ms: Some(500),
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        };
        assert_eq!(response.text, "The answer is 42");
        assert!(response.tool_uses.is_empty());
        assert_eq!(response.content_blocks.len(), 1);
    }
}
