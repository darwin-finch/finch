// Claude generator implementation

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::claude::{ClaudeClient, ContentBlock, Message, MessageRequest};
use crate::context::collect_claude_md_context;
use crate::tools::types::ToolDefinition;

use super::{
    Generator, GeneratorCapabilities, GeneratorResponse, ResponseMetadata, StreamChunk, ToolUse,
};

pub const CODING_SYSTEM_PROMPT: &str = "You are Finch, an expert software engineering \
assistant. You work directly in the user's codebase and execute tasks autonomously using \
tools — like a senior engineer pairing at the terminal.

## Tools

- **read** — Read files. Use offset/limit for large files (e.g. offset=100, limit=50).
- **glob** — Find files by pattern (e.g. `**/*.rs`, `src/**/*.ts`). Always use before assuming paths.
- **grep** — Search file contents with regex. Use context_lines to see surrounding code.
- **edit** — Replace exact text in a file (old_string → new_string). PREFER this for targeted \
edits. old_string must match exactly including whitespace. Include enough surrounding lines to \
make it unique. Use replace_all: true for multiple occurrences.
- **write** — Write a complete file (new or full rewrite). Use for new files; for small changes \
use edit instead.
- **bash** — Run shell commands: builds, tests, git, formatters, etc.
- **web_fetch** — Fetch documentation, crate pages, GitHub issues, etc.

## Approach

Before editing: glob/grep to find the file, read the relevant section, understand the context.
Make the minimum change needed — don't touch code outside the task.
After structural changes: run the build or tests to verify (cargo build, cargo test, npm test…).
If tests fail: read the error carefully and diagnose the root cause before retrying.
Match the style of surrounding code — indentation, naming, patterns.
Don't add comments unless the logic is genuinely non-obvious.
Work through multi-step tasks systematically, verifying each step.
Be direct. If something is unclear, ask one focused question rather than guessing.";

/// Build the full system prompt including working directory and project context.
pub fn build_system_prompt(cwd: Option<&str>, claude_md: Option<&str>) -> String {
    let mut prompt = CODING_SYSTEM_PROMPT.to_string();
    if let Some(dir) = cwd {
        prompt.push_str(&format!("\n\nWorking directory: {}", dir));
    }
    if let Some(md) = claude_md {
        prompt.push_str(&format!("\n\n## Project Instructions\n\n{}", md));
    }
    prompt
}

/// Claude API generator implementation
pub struct ClaudeGenerator {
    client: Arc<ClaudeClient>,
    capabilities: GeneratorCapabilities,
    /// Working directory context injected into the system prompt.
    cwd: Option<String>,
    /// Concatenated contents of any CLAUDE.md / FINCH.md files found at startup.
    claude_md_context: Option<String>,
}

impl ClaudeGenerator {
    pub fn new(client: Arc<ClaudeClient>) -> Self {
        let cwd = std::env::current_dir().ok();
        let claude_md_context = cwd.as_deref().and_then(collect_claude_md_context);
        let cwd_str = cwd.map(|p| p.display().to_string());
        Self {
            client,
            capabilities: GeneratorCapabilities {
                supports_streaming: true,
                supports_tools: true,
                supports_conversation: true,
                max_context_messages: Some(50),
            },
            cwd: cwd_str,
            claude_md_context,
        }
    }

    fn system_prompt(&self) -> String {
        build_system_prompt(self.cwd.as_deref(), self.claude_md_context.as_deref())
    }

    /// Convert Claude MessageResponse to unified GeneratorResponse
    fn convert_to_unified(&self, response: crate::claude::MessageResponse) -> GeneratorResponse {
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
        let mut request = MessageRequest::with_context(messages).with_system(self.system_prompt());
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
        let mut request = MessageRequest::with_context(messages).with_system(self.system_prompt());
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
        self.client.provider_name()
    }
}
