//! Local-model generator backed by the Finch daemon.

use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::claude::{ContentBlock, Message};
use crate::client::DaemonClient;
use crate::tools::types::ToolDefinition;

use super::{
    Generator, GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk, ToolUse,
};

/// Presents the daemon-owned local model through the same interface used by
/// cloud providers. Model loading remains in the daemon, so changing profiles
/// does not block the TUI or discard conversation state.
pub struct DaemonLocalGenerator {
    client: Arc<DaemonClient>,
    profile_name: String,
    capabilities: GeneratorCapabilities,
}

impl DaemonLocalGenerator {
    pub fn new(client: Arc<DaemonClient>, profile_name: impl Into<String>) -> Self {
        Self {
            client,
            profile_name: profile_name.into(),
            capabilities: GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(20),
            },
        }
    }
}

#[async_trait]
impl Generator for DaemonLocalGenerator {
    async fn generate(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<GeneratorResponse> {
        let response = self
            .client
            .query_local(messages, tools.unwrap_or_default())
            .await?;
        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Local model returned no choices"))?;

        let text = choice.message.content.unwrap_or_default();
        let mut content_blocks = Vec::new();
        if !text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: text.clone() });
        }

        let tool_uses: Vec<ToolUse> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|call| {
                let input = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|_| serde_json::json!({}));
                content_blocks.push(ContentBlock::ToolUse {
                    id: call.id.clone(),
                    name: call.function.name.clone(),
                    input: input.clone(),
                });
                ToolUse {
                    id: call.id,
                    name: call.function.name,
                    input,
                }
            })
            .collect();

        Ok(GeneratorResponse {
            text,
            content_blocks,
            tool_uses,
            metadata: ResponseMetadata {
                generator: "local".to_string(),
                model: response.model,
                confidence: None,
                stop_reason: Some(choice.finish_reason),
                input_tokens: Some(response.usage.prompt_tokens),
                output_tokens: Some(response.usage.completion_tokens),
                latency_ms: None,
                primary_allowance_used_percent: None,
                secondary_allowance_used_percent: None,
            },
        })
    }

    async fn generate_stream(
        &self,
        _messages: Vec<Message>,
        _tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        Ok(None)
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        &self.profile_name
    }
}
