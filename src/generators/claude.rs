// Claude generator implementation

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::claude::{ClaudeClient, ContentBlock, Message, MessageRequest};
use crate::tools::types::ToolDefinition;

use super::{
    Generator, GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk, ToolUse,
};

/// Claude API generator implementation
pub struct ClaudeGenerator {
    client: Arc<ClaudeClient>,
    capabilities: GeneratorCapabilities,
}

impl ClaudeGenerator {
    pub fn new(client: Arc<ClaudeClient>) -> Self {
        Self {
            client,
            capabilities: GeneratorCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(50),
            },
        }
    }

    /// Convert Claude MessageResponse to unified GeneratorResponse
    fn convert_to_unified(
        &self,
        response: crate::claude::MessageResponse,
    ) -> GeneratorResponse {
        // Extract text from content blocks
        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

        // Extract tool uses
        let tool_uses = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }),
                _ => None,
            })
            .collect();

        GeneratorResponse {
            text,
            content_blocks: response.content,
            tool_uses,
            metadata: ResponseMetadata {
                generator: "claude".to_string(),
                model: response.model,
                confidence: None,
                stop_reason: response.stop_reason,
                input_tokens: None,  // TODO: Extract from response.usage when available
                output_tokens: None, // TODO: Extract from response.usage when available
                latency_ms: None,    // TODO: Track request timing
            },
        }
    }
}

#[async_trait]
impl Generator for ClaudeGenerator {
    async fn generate(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse> {
        let mut request = MessageRequest::with_context(messages);
        if let Some(tools) = tools {
            request = request.with_tools(tools);
        }

        let response = self.client.send_message(&request).await?;
        Ok(self.convert_to_unified(response))
    }

    async fn generate_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        let mut request = MessageRequest::with_context(messages);
        if let Some(tools) = tools {
            request = request.with_tools(tools);
        }

        // Get the streaming receiver from Claude client
        let rx = self.client.send_message_stream(&request).await?;
        Ok(Some(rx))
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        "Claude API"
    }
}
