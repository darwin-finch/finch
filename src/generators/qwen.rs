// Qwen local generator implementation

use anyhow::{Context, Result};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::claude::{ContentBlock, Message};
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::tools::types::ToolDefinition;

use super::{
    Generator, GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk,
};

/// Qwen local generator implementation
pub struct QwenGenerator {
    local_gen: Arc<RwLock<LocalGenerator>>,
    tokenizer: Arc<TextTokenizer>,
    capabilities: GeneratorCapabilities,
}

impl QwenGenerator {
    pub fn new(
        local_gen: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
    ) -> Self {
        Self {
            local_gen,
            tokenizer,
            capabilities: GeneratorCapabilities {
                supports_streaming: false,  // Qwen blocks
                supports_tools: false,      // Not yet implemented
                supports_conversation: false, // Only single-turn
                max_context_messages: None,
            },
        }
    }
}

#[async_trait]
impl Generator for QwenGenerator {
    async fn generate(
        &self,
        messages: Vec<Message>,
        _tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse> {
        // Extract last user message (Qwen doesn't support full history yet)
        let query = messages
            .last()
            .and_then(|m| {
                // Get text from first content block
                m.content.first().and_then(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
            })
            .ok_or_else(|| anyhow::anyhow!("No user message found"))?;

        // Generate (blocking, so spawn_blocking)
        let local_gen = Arc::clone(&self.local_gen);
        let query = query.to_string();

        let generated = tokio::task::spawn_blocking(move || -> Result<_> {
            // Get write lock synchronously
            let mut gen = local_gen.blocking_write();
            // Use try_generate which returns Option<String>
            match gen.try_generate(&query)? {
                Some(text) => Ok(crate::local::GeneratedResponse {
                    text,
                    method: "local".to_string(),
                    confidence: 0.8, // Default confidence from try_generate
                    pattern: "local".to_string(),
                }),
                None => Err(anyhow::anyhow!("Local generation returned None")),
            }
        })
        .await
        .context("Failed to spawn blocking task for Qwen generation")??;

        Ok(GeneratorResponse {
            text: generated.text.clone(),
            content_blocks: vec![ContentBlock::Text {
                text: generated.text.clone(),
            }],
            tool_uses: vec![], // Qwen doesn't support tools yet
            metadata: ResponseMetadata {
                generator: "qwen".to_string(),
                model: "Qwen2.5-3B".to_string(),
                confidence: Some(generated.confidence),
                stop_reason: None,
            },
        })
    }

    async fn generate_stream(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        // Qwen doesn't support streaming
        Ok(None)
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Qwen Local"
    }
}
