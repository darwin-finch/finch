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

pub const CODING_SYSTEM_PROMPT: &str = "\
You are Finch, an expert software engineering assistant. \
You have access to tools for reading files, searching code, editing files, running commands, and fetching web content.

When helping with coding tasks:
- Read files before modifying them to understand existing code and context
- Use glob/grep to locate relevant files before assuming their paths
- Make targeted, minimal edits — don't rewrite code that doesn't need to change
- Run tests or build commands after changes to verify correctness
- If a task requires multiple steps, work through them systematically

Available tools: read (supports offset/limit for line ranges), write, edit, glob, grep (supports context_lines), bash, web_fetch.";

/// Build the full system prompt including working directory context.
pub fn build_system_prompt(cwd: Option<&str>) -> String {
    match cwd {
        Some(dir) => format!("{}\n\nWorking directory: {}", CODING_SYSTEM_PROMPT, dir),
        None => CODING_SYSTEM_PROMPT.to_string(),
    }
}

/// Claude API generator implementation
pub struct ClaudeGenerator {
    client: Arc<ClaudeClient>,
    capabilities: GeneratorCapabilities,
    /// Working directory context injected into the system prompt.
    cwd: Option<String>,
}

impl ClaudeGenerator {
    pub fn new(client: Arc<ClaudeClient>) -> Self {
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string());
        Self {
            client,
            capabilities: GeneratorCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(50),
            },
            cwd,
        }
    }

    fn system_prompt(&self) -> String {
        build_system_prompt(self.cwd.as_deref())
    }

    /// Convert Claude MessageResponse to unified GeneratorResponse
    fn convert_to_unified(
        &self,
        response: crate::claude::MessageResponse,
    ) -> GeneratorResponse {
        let text = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");

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
                input_tokens: None,
                output_tokens: None,
                latency_ms: None,
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
        let mut request = MessageRequest::with_context(messages)
            .with_system(self.system_prompt());
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
        let mut request = MessageRequest::with_context(messages)
            .with_system(self.system_prompt());
        if let Some(tools) = tools {
            request = request.with_tools(tools);
        }

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
