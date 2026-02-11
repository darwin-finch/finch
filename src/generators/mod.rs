// Unified generator interface for Claude, Qwen, and future generators

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::claude::{ContentBlock, Message};
use crate::tools::types::ToolDefinition;

// Re-export implementations
pub mod claude;
pub mod qwen;

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

    /// Get generator capabilities
    fn capabilities(&self) -> &GeneratorCapabilities;

    /// Get generator name for logging
    fn name(&self) -> &str;
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
    pub generator: String,      // "claude" | "qwen"
    pub model: String,          // specific model name
    pub confidence: Option<f64>, // for Qwen
    pub stop_reason: Option<String>, // for Claude
}

/// Streaming chunk (text delta or complete block)
#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta(String),                      // Incremental text
    ContentBlockComplete(ContentBlock),     // Complete tool_use or text block
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
