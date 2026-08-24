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

pub const CODING_SYSTEM_PROMPT: &str = "You are the software-engineering reasoning provider inside \
Finch. You are not the Finch application or terminal UI, and you do not impersonate either one. \
Use the host tools Finch exposes to inspect and modify the user's codebase autonomously, like a \
senior engineer pairing at the terminal. A transport-specific execution/output contract may follow \
this coding policy; when present, it controls the complete text-response channel while provider-native \
tool calls remain structurally separate.

## Tools

- **read** — Read files. Use offset/limit for large files (e.g. offset=100, limit=50).
- **glob** — Find files by pattern (e.g. `**/*.rs`, `src/**/*.ts`). Always use before assuming paths.
- **grep** — Search file contents with regex. Use context_lines to see surrounding code.
- **edit** — Replace exact text in a file (old_string → new_string). PREFER this for targeted \
edits. old_string must match exactly including whitespace. Include enough surrounding lines to \
make it unique. Use replace_all: true for multiple occurrences.
- **write** — Write a complete file (new or full rewrite). Use for new files; for small changes \
use edit instead.
- **bash** — Run shell commands only when no structured tool exists: builds, tests, git,
  formatters, or a purpose-built command. Never use `cat` to read, `grep` to search, `find` to
  locate files, or shell redirection to write files; use `read`, `grep`, `glob`, `edit`, or `write`
  instead. Shell stdout is never a substitute for the transport's final response channel.
- **web_fetch** — Fetch documentation, crate pages, GitHub issues, etc.

## Approach

Before editing: glob/grep to find the file, use `read` for the relevant section, understand the context.
Make the minimum change needed — don't touch code outside the task.
After structural changes: run the build or tests to verify (cargo build, cargo test, npm test…).
If tests fail: read the error carefully and diagnose the root cause before retrying.
Match the style of surrounding code — indentation, naming, patterns.
Don't add comments unless the logic is genuinely non-obvious.
Work through multi-step tasks systematically, verifying each step.
Be direct. If something is unclear, ask one focused question rather than guessing.";

/// Plain-text command reference injected into every system prompt.
/// Also used by persona system prompts and any other path that constructs a system message.
/// Keep in sync with format_help() in src/cli/commands.rs.
pub const COMMAND_REFERENCE: &str = "\
## Finch Slash Commands

Basic: /help  /quit  /clear  /compact [note]  /debug  /metrics  /memory  /training

Provider: /provider  /provider list  /provider <name>  /local <query>
  (aliases: /model /teacher)

MCP: /mcp list  /mcp tools [server]  /mcp refresh  /mcp reload

Persona: /persona  /persona select <name>  /persona show

Discovery: /machines  /discover

Patterns: /patterns  /patterns add  /patterns rm <id>  /patterns clear

Feedback: /critical [note]  /medium [note]  /good [note]
  (aliases: /feedback critical|medium|good [note])

Typed Co-Forth: /forth <expr>
Execution-plan prototype: /push <text>  /pop  /run  /program  /stack  /stack clear
  /chain W1 W2  /forget W1  /dup W1  /swap W1 W2  /share  /box-diff


Channels: /join #channel  /part #channel  /say #channel <msg>

Peers/Rooms: /connect <host:port>  /disconnect <name>
  /room  /room new  /room list  /room add <addr>  /room remove <addr>

Brains: /brain <task>  /brains  /brain cancel <name>

Other: /plan [task]  /graph  /setup  /license  /license activate <key>  /accept  /reject
  /ask <query>  /self-fix

Keyboard: Ctrl+C cancel  Ctrl+D quit  Ctrl+G good  Ctrl+B bad  Ctrl+Z undefine
  Ctrl+P pop  Tab complete  Shift+Tab plan mode  Shift+Enter newline";

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
                // `ClaudeClient` is a compatibility facade over every
                // configured provider. Do not let its historical name leak
                // into transcripts, logs, or provider-selection UI.
                generator: self.client.provider_name().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coding_prompt_prefers_structured_file_tools_over_shell_equivalents() {
        assert!(CODING_SYSTEM_PROMPT.contains("Never use `cat` to read"));
        assert!(CODING_SYSTEM_PROMPT.contains("`grep` to search"));
        assert!(CODING_SYSTEM_PROMPT.contains("`read`, `grep`, `glob`, `edit`, or `write`"));
    }

    #[test]
    fn coding_prompt_does_not_impersonate_the_finch_application() {
        assert!(CODING_SYSTEM_PROMPT.contains("reasoning provider inside Finch"));
        assert!(CODING_SYSTEM_PROMPT.contains("not the Finch application or terminal UI"));
        assert!(CODING_SYSTEM_PROMPT.contains("tool calls remain structurally separate"));
        assert!(!CODING_SYSTEM_PROMPT.starts_with("You are Finch"));
    }

    #[test]
    fn system_prompt_keeps_the_working_directory_outside_the_tool_contract() {
        let prompt = build_system_prompt(Some("/workspace"), None);
        assert!(prompt.contains("Working directory: /workspace"));
        assert!(prompt.contains("Never use `cat` to read"));
    }
}
