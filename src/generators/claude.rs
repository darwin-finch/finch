// Claude generator implementation

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::claude::{ClaudeClient, ContentBlock, Message, MessageRequest};
use crate::context::{collect_instructions, InstructionSources};
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
Use `todo_read`/`todo_write` only to maintain a genuine multi-step task plan. Do not inspect the TODO
list for greetings, ordinary questions, calculations, or merely to discover whether it is empty.
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

Patterns: /patterns  /patterns add  /patterns rm <id>  /patterns clear

Feedback: /critical [note]  /medium [note]  /good [note]
  (aliases: /feedback critical|medium|good [note])

Typed Co-Forth: /forth <expr>
Execution-plan prototype: /push <text>  /pop  /run  /program  /stack  /stack clear
  /chain W1 W2  /forget W1  /dup W1  /swap W1 W2

Brains: /brain list  /brain runs  /brain cancel <run>  /brain create <name>
  /brain attach <name>  /brain invite [role] [minutes]
  /brain join <name@machine[:port]> <invite>  /brain detach  /brain archive <name>
  /brain handoff <subject>  /brain handoff identity  /brain handoff accept [id]
  /brain handoff cancel [id]  /brain password [new]  /brains
Collaboration: /say <text>  /who  /whois <subject>  @finch <prompt>

Other: /plan [task]  /graph  /setup  /license  /license activate <key>  /accept  /reject
  /ask <query>  /self-fix

Keyboard: Ctrl+C cancel  Ctrl+D forward-delete  Ctrl+G good  Ctrl+B bad  Ctrl+Z undefine
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
    /// Project instructions found at construction, with where each came from.
    instructions: InstructionSources,
}

impl ClaudeGenerator {
    pub fn new(client: Arc<ClaudeClient>) -> Self {
        Self::new_in(client, std::env::current_dir().ok(), dirs::home_dir())
    }

    /// Build a generator whose instructions are collected from `cwd`, reading user-level files
    /// under `home`. [`ClaudeGenerator::new`] passes the process working and home directories.
    pub fn new_in(client: Arc<ClaudeClient>, cwd: Option<PathBuf>, home: Option<PathBuf>) -> Self {
        let instructions = cwd
            .as_deref()
            .map(|cwd| collect_instructions(cwd, home.as_deref()))
            .unwrap_or_default();
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
            instructions,
        }
    }

    /// The instruction files found at construction and what happened to each.
    pub fn instruction_sources(&self) -> &InstructionSources {
        &self.instructions
    }

    fn system_prompt(&self) -> String {
        build_system_prompt(self.cwd.as_deref(), self.instructions.text())
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
                input_tokens: response.input_tokens,
                output_tokens: response.output_tokens,
                latency_ms: None,
                primary_allowance_used_percent: response.primary_allowance_used_percent,
                secondary_allowance_used_percent: response.secondary_allowance_used_percent,
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

    async fn generate_stream_cancellable(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
        cancellation_token: tokio_util::sync::CancellationToken,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        let mut request = MessageRequest::with_context(messages).with_system(self.system_prompt());
        if let Some(tools) = tools {
            request = request.with_tools(tools);
        }
        let rx = self
            .client
            .send_message_stream_with_cancel(&request, cancellation_token)
            .await?;
        Ok(Some(rx))
    }

    fn capabilities(&self) -> &GeneratorCapabilities {
        &self.capabilities
    }

    fn name(&self) -> &str {
        self.client.provider_name()
    }

    fn model_name(&self) -> &str {
        self.client.model_name()
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
    fn coding_prompt_does_not_probe_todos_on_ordinary_turns() {
        assert!(CODING_SYSTEM_PROMPT.contains(
            "Do not inspect the TODO\nlist for greetings, ordinary questions, calculations"
        ));
    }

    #[test]
    fn system_prompt_keeps_the_working_directory_outside_the_tool_contract() {
        let prompt = build_system_prompt(Some("/workspace"), None);
        assert!(prompt.contains("Working directory: /workspace"));
        assert!(prompt.contains("Never use `cat` to read"));
    }

    #[test]
    fn command_capsule_does_not_advertise_ctrl_d_as_exit() {
        assert!(COMMAND_REFERENCE.contains("Ctrl+D forward-delete"));
        assert!(!COMMAND_REFERENCE.contains("Ctrl+D quit"));
    }

    #[test]
    fn command_reference_exposes_only_brain_collaboration() {
        for removed in [
            "/join #",
            "/part #",
            "/room ",
            "/connect ",
            "/disconnect ",
            "/discover",
            "/machines",
            "/peers",
            "/nodes",
            "/self-peer",
            "/balance",
            "/settle ",
            "/join-registry ",
            "/registry ",
            "/gas-send",
        ] {
            assert!(!COMMAND_REFERENCE.contains(removed));
        }
        for supported in ["/say <text>", "@finch <prompt>", "/who", "/whois <subject>"] {
            assert!(COMMAND_REFERENCE.contains(supported));
        }
    }

    /// Records the exact provider request `ClaudeGenerator` sends.
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<crate::providers::ProviderRequest>>,
    }

    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for RecordingProvider {
        async fn send_message_validated(
            &self,
            request: crate::providers::ValidatedProviderRequest,
        ) -> Result<crate::providers::ProviderResponse> {
            let request = request.into_request_for(self)?;
            let model = request.model.clone();
            self.requests.lock().unwrap().push(request);
            Ok(crate::providers::ProviderResponse {
                id: "recorded".into(),
                model,
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: Some("end_turn".into()),
                role: "assistant".into(),
                provider: "recording".into(),
                usage: None,
                allowance: None,
            })
        }

        async fn send_message_stream_validated(
            &self,
            _request: crate::providers::ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<crate::providers::StreamChunk>>> {
            anyhow::bail!("recording provider does not stream")
        }

        fn name(&self) -> &str {
            "recording"
        }

        fn default_model(&self) -> &str {
            "recording-model"
        }

        fn capabilities(&self, model: &str) -> crate::providers::ModelCapabilities {
            use crate::providers::{CapabilitySupport, ModelCapabilities, ReasoningCapability};
            ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-09-11",
                "test fixture",
                CapabilitySupport::Unsupported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-09-11", "test fixture"),
                Some(100_000),
                Some(10_000),
                None,
            )
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_request_carries_symlinked_agents_md_once_and_nested_rules_last() {
        let project = tempfile::TempDir::new().unwrap();
        let home = tempfile::TempDir::new().unwrap();
        let nested = project.path().join("src/vm");
        std::fs::create_dir_all(&nested).unwrap();
        // Finch's own layout: AGENTS.md is a symlink to CLAUDE.md, plus a nested capsule.
        std::fs::write(project.path().join("CLAUDE.md"), "ROOT-INVARIANT-7f3a").unwrap();
        std::os::unix::fs::symlink("CLAUDE.md", project.path().join("AGENTS.md")).unwrap();
        std::fs::write(nested.join("AGENTS.md"), "VM-CAPSULE-19c2").unwrap();

        let provider = Arc::new(RecordingProvider {
            requests: std::sync::Mutex::new(Vec::new()),
        });
        let client = Arc::new(ClaudeClient::with_shared_provider(provider.clone()));
        let generator = ClaudeGenerator::new_in(
            client,
            Some(nested.clone()),
            Some(home.path().to_path_buf()),
        );
        generator
            .generate(vec![Message::user("hello")], None)
            .await
            .expect("recording provider accepts the request");

        let requests = provider.requests.lock().unwrap();
        let system = requests
            .first()
            .and_then(|request| request.system.clone())
            .expect("the provider request must carry a system prompt");
        assert_eq!(
            system.matches("ROOT-INVARIANT-7f3a").count(),
            1,
            "a symlinked AGENTS.md must reach the provider exactly once; sources={:?}\n{system}",
            generator.instruction_sources().sources
        );
        let root = system.find("ROOT-INVARIANT-7f3a").unwrap();
        let capsule = system.find("VM-CAPSULE-19c2").unwrap_or_else(|| {
            panic!("nested AGENTS.md capsule missing from the request:\n{system}")
        });
        assert!(
            root < capsule,
            "the nested capsule must follow (and so refine) the root instructions:\n{system}"
        );
    }
}
