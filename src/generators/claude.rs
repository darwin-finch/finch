// Claude generator implementation

use anyhow::Result;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::claude::{ClaudeClient, MessageRequest};
use crate::context::{collect_instructions, InstructionSources};
use crate::models::{ToolCallParser, ToolPromptFormatter};
use crate::providers::{ContentBlock, Message};
use crate::tools::ToolDefinition;

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

Provider: /model [id]  /status  /providers  /provider <name>  /thinking [level]  /local <query>
  /model overlays a model on this Brain (same credentials). /provider switches the named entry.
  --model is one-shot and does not persist.

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

Keyboard: Esc cancel  Ctrl+C copy selection  Ctrl+D forward-delete  Ctrl+G good  Ctrl+B bad
  Ctrl+Z undefine  Ctrl+P pop  Tab complete  Shift+Tab plan mode  Shift+Enter newline";

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

    /// Whether `tools` would need the prompt-injection fold: the provider
    /// declares no native tool-calling capability and a non-empty tool set
    /// was requested. The single predicate both `split_tools_for_capability`
    /// (which request shape to build) and `requires_prompt_injected_tools`
    /// (whether streaming can be used at all) key off, so the two decisions
    /// cannot drift apart.
    fn needs_prompt_injection(tools: &Option<Vec<ToolDefinition>>, client: &ClaudeClient) -> bool {
        tools.as_ref().is_some_and(|tools| !tools.is_empty()) && !client.supports_tools()
    }

    /// Split requested tools into native `request.tools` (when the
    /// configured provider can execute function-calling itself) or prompt
    /// text to fold into the system prompt instead (when it cannot -- e.g.
    /// the Claude CLI subscription backend, run with `--tools ""` so the
    /// model executes nothing on its own authority). This is the same fold
    /// `LocalGenerator::inject_tool_definitions` performs for local models
    /// via the shared [`ToolPromptFormatter`] (issue #1276); doing it here
    /// keeps `finch-providers` free of tool-execution and root-crate
    /// dependencies while still letting a non-native-tool-calling transport
    /// participate in Finch's own `ToolLoop` (issue #1303).
    fn split_tools_for_capability(
        &self,
        tools: Option<Vec<ToolDefinition>>,
    ) -> (Option<Vec<ToolDefinition>>, Option<String>) {
        if !Self::needs_prompt_injection(&tools, &self.client) {
            return (tools, None);
        }
        let tools = tools.expect("needs_prompt_injection confirmed a non-empty tool set");
        (
            None,
            Some(ToolPromptFormatter::format_tools_for_prompt(&tools)),
        )
    }

    /// Whether `tools` would need the prompt-injection fold above. Streaming
    /// cannot support that fold: the `<tool_use>` markup must be stripped
    /// from the complete response before any of it is safe to show, so a
    /// turn that needs it must go through the buffered [`Generator::generate`]
    /// path instead (the same restriction `QwenGenerator` accepts for local
    /// tool-markup proposals, which also declares `supports_streaming:
    /// false`).
    fn requires_prompt_injected_tools(&self, tools: &Option<Vec<ToolDefinition>>) -> bool {
        Self::needs_prompt_injection(tools, &self.client)
    }

    /// Parse `<tool_use>` markup a prompt-injected-tools turn produced back
    /// into real [`ToolUse`] values, mirroring
    /// `LocalGenerator::try_generate_from_pattern_with_tools`'s use of the
    /// same [`ToolCallParser`] (issue #1276/#1303). A no-op when the model
    /// did not call any tool.
    ///
    /// Each `<tool_use>` block is parsed independently (#1307): one
    /// malformed block is logged and dropped, it no longer discards every
    /// well-formed tool call the same response also proposed.
    fn parse_prompt_injected_tool_calls(response: GeneratorResponse) -> Result<GeneratorResponse> {
        if !ToolCallParser::has_tool_calls(&response.text) {
            return Ok(response);
        }
        let GeneratorResponse {
            text: raw_text,
            metadata,
            ..
        } = response;
        let outcome = ToolCallParser::parse(&raw_text);
        for malformed in &outcome.errors {
            tracing::warn!(
                error = %malformed.message,
                raw_block = %malformed.raw_block,
                "dropping malformed prompt-injected tool-call block"
            );
        }
        let parsed = outcome.tool_uses;
        let text = ToolCallParser::extract_text(&raw_text);
        let mut content_blocks = Vec::new();
        if !text.is_empty() {
            content_blocks.push(ContentBlock::Text { text: text.clone() });
        }
        for call in &parsed {
            content_blocks.push(ContentBlock::ToolUse {
                id: call.id.clone(),
                name: call.name.clone(),
                input: call.input.clone(),
            });
        }
        Ok(GeneratorResponse {
            text,
            content_blocks,
            tool_uses: parsed,
            metadata: ResponseMetadata {
                stop_reason: Some("tool_use".to_string()),
                ..metadata
            },
        })
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
        let (native_tools, tool_prompt) = self.split_tools_for_capability(tools);
        let mut system_prompt = self.system_prompt();
        if let Some(tool_prompt) = &tool_prompt {
            system_prompt.push_str(tool_prompt);
        }
        let mut request = MessageRequest::with_context(messages).with_system(system_prompt);
        if let Some(tools) = native_tools {
            request = request.with_tools(tools);
        }

        let response = self.client.send_message(&request).await?;
        let response = self.convert_to_unified(response);
        if tool_prompt.is_some() {
            Self::parse_prompt_injected_tool_calls(response)
        } else {
            Ok(response)
        }
    }

    async fn generate_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDefinition>>,
    ) -> Result<Option<mpsc::Receiver<Result<StreamChunk>>>> {
        if self.requires_prompt_injected_tools(&tools) {
            // The caller (`process_query_with_tools`) falls back to the
            // buffered `generate` path on `Ok(None)`, the same way it does
            // for local models that cannot stream a tool-markup proposal.
            return Ok(None);
        }
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
        if self.requires_prompt_injected_tools(&tools) {
            return Ok(None);
        }
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
    #[cfg(unix)]
    struct RecordingProvider {
        requests: std::sync::Mutex<Vec<crate::providers::ProviderRequest>>,
    }

    #[cfg(unix)]
    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for RecordingProvider {
        async fn send_message_validated(
            &self,
            request: crate::providers::ValidatedProviderRequest,
        ) -> Result<crate::providers::ProviderResponse> {
            let (request, _bindings) = request.into_request_for(self)?;
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

    /// Shared capability declaration for the two fixtures below: streaming
    /// supported, tool calls unsupported. `finch_providers::ClaudeCliProvider`
    /// was the real example this generic fold was built for (issue #1303),
    /// but issue #1309 gave it a real native-tool-calling path (Finch's own
    /// tools served over MCP) and it now declares tools `Supported`; this
    /// fixture stands in for any *other* provider that still cannot execute
    /// tool calls itself (local models take the analogous
    /// `LocalGenerator`/`ToolPromptFormatter` fold, not this one), so the
    /// generic capability-driven bypass in `needs_prompt_injection` stays
    /// covered even though its original real-world example moved off it.
    #[cfg(unix)]
    fn no_native_tools_capabilities(
        name: &str,
        model: &str,
    ) -> crate::providers::ModelCapabilities {
        use crate::providers::{CapabilitySupport, ModelCapabilities, ReasoningCapability};
        ModelCapabilities::static_metadata(
            name,
            model,
            "2026-09-26",
            "test fixture",
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-09-26", "test fixture"),
            Some(100_000),
            Some(10_000),
            None,
        )
    }

    /// Declares streaming supported but tool calls unsupported, and panics
    /// from *both* `ProviderBackend` methods: this fixture proves the
    /// capability check in `ClaudeGenerator::generate_stream_cancellable`
    /// short-circuits before anything is sent for a tool-bearing turn, not
    /// merely that the returned value happens to look like `None`.
    #[cfg(unix)]
    struct PanicsIfSentNoNativeToolsProvider;

    #[cfg(unix)]
    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for PanicsIfSentNoNativeToolsProvider {
        async fn send_message_validated(
            &self,
            _request: crate::providers::ValidatedProviderRequest,
        ) -> Result<crate::providers::ProviderResponse> {
            unreachable!("a tool-bearing turn must decline before sending anything")
        }

        async fn send_message_stream_validated(
            &self,
            _request: crate::providers::ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<crate::providers::StreamChunk>>> {
            unreachable!("a tool-bearing turn must decline to stream before sending anything")
        }

        fn name(&self) -> &str {
            "no-native-tools-panics-if-sent"
        }

        fn default_model(&self) -> &str {
            "no-native-tools-panics-if-sent-model"
        }

        fn capabilities(&self, model: &str) -> crate::providers::ModelCapabilities {
            no_native_tools_capabilities(self.name(), model)
        }
    }

    /// Declares the same no-native-tools capability but actually streams,
    /// so a turn that never asked for tools can be proven to stream
    /// normally against this same class of provider.
    #[cfg(unix)]
    struct StreamsWhenNoToolsAreRequestedProvider;

    #[cfg(unix)]
    #[async_trait::async_trait]
    impl crate::providers::ProviderBackend for StreamsWhenNoToolsAreRequestedProvider {
        async fn send_message_validated(
            &self,
            _request: crate::providers::ValidatedProviderRequest,
        ) -> Result<crate::providers::ProviderResponse> {
            unreachable!("this fixture only drives the streaming path")
        }

        async fn send_message_stream_validated(
            &self,
            _request: crate::providers::ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<crate::providers::StreamChunk>>> {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx
                .send(Ok(crate::providers::StreamChunk::ContentBlockComplete(
                    ContentBlock::Text {
                        text: "ok".to_string(),
                    },
                )))
                .await;
            Ok(rx)
        }

        fn name(&self) -> &str {
            "no-native-tools-streams"
        }

        fn default_model(&self) -> &str {
            "no-native-tools-streams-model"
        }

        fn capabilities(&self, model: &str) -> crate::providers::ModelCapabilities {
            no_native_tools_capabilities(self.name(), model)
        }
    }

    #[cfg(unix)]
    fn no_native_tool_definitions() -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            input_schema: crate::tools::ToolInputSchema::simple(vec![(
                "file_path",
                "Path to read",
            )]),
        }]
    }

    /// Regression for issue #1303: before the fix, `ClaudeGenerator` attached
    /// Finch's default tool set to `ProviderRequest::tools` unconditionally,
    /// so any provider that declares tool calls `Unsupported` (real example:
    /// `finch_providers::ClaudeCliProvider`, run with `--tools ""` by design)
    /// hit `ModelCapabilities::validate_request`'s tool-calls gate on the
    /// very first turn -- including plain queries that never asked for a
    /// tool, because Finch always attaches at least its own default tools.
    #[cfg(unix)]
    #[tokio::test]
    async fn tool_bearing_turn_against_a_non_native_tool_provider_skips_streaming() {
        let provider = Arc::new(PanicsIfSentNoNativeToolsProvider);
        let client = Arc::new(ClaudeClient::with_shared_provider(provider));
        let generator = ClaudeGenerator::new_in(client, None, None);

        let with_tools = generator
            .generate_stream_cancellable(
                vec![Message::user("hi")],
                Some(no_native_tool_definitions()),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("the capability check itself must not error");
        assert!(
            with_tools.is_none(),
            "a provider without native tool calls must decline to stream a tool-bearing turn \
             (Ok(None)) so the caller's existing streaming-unavailable fallback in \
             process_query_with_tools drives it through the buffered generate() path, which can \
             fold tools into the prompt and parse <tool_use> markup back out; streaming a raw \
             request.tools attachment would instead hit validate_request's tool-calls gate"
        );
        // `provider` never panicked reaching this point, proving the
        // short-circuit ran before either ProviderBackend method could be
        // invoked -- not merely that the eventual result looked like None.
    }

    /// Companion to the regression above: declining to stream must be
    /// specific to tool-bearing turns against a non-native-tool-calling
    /// provider, not a blanket regression for this provider's
    /// otherwise-supported streaming capability.
    #[cfg(unix)]
    #[tokio::test]
    async fn turn_without_tools_still_streams_against_a_non_native_tool_provider() {
        let provider = Arc::new(StreamsWhenNoToolsAreRequestedProvider);
        let client = Arc::new(ClaudeClient::with_shared_provider(provider));
        let generator = ClaudeGenerator::new_in(client, None, None);

        let without_tools = generator
            .generate_stream_cancellable(
                vec![Message::user("hi")],
                None,
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .expect("a turn without tools must still be able to stream");
        assert!(
            without_tools.is_some(),
            "a turn that never requested tools must still stream against a provider whose only \
             unsupported capability is native tool calls"
        );
    }

    /// Installs a fake `claude` executable (mirrors the fixture pattern in
    /// `finch_providers::claude_cli::tests::install_fake_claude`) that emits
    /// a canned assistant turn with a real, structured `tool_use` content
    /// block for the `read` tool, in the wire shape measured against the
    /// real CLI 2.1.283 once it is driven through Finch's own MCP bridge
    /// (issue #1309), followed directly by the CLI's own final-answer
    /// message — matching exactly what `absorb_line` in
    /// `finch_providers::claude_cli` actually parses from real stdout
    /// (`Some("assistant") | Some("result")`; a synthetic `"user"`/
    /// `tool_result` stdout line, present in an earlier version of this
    /// fixture, is not a shape the real CLI is known to emit to its own
    /// `--output-format stream-json` and `absorb_line`'s match has no arm
    /// for `"user"` at all — it fell through to the no-op catch-all, so
    /// removing it changes nothing this test observes). Real tool-call
    /// *execution* is no longer the bridge's own job (issue #1341): a real
    /// call is forwarded over a socket to the frontend's interactive
    /// `ToolLoop`, tested in `finch_providers::claude_cli`'s own module and
    /// in `tests/claude_cli_bridge_subprocess.rs`. This fixture never drives
    /// that real socket path at all (there is no `--mcp-config` parsing
    /// here) — its `tool_use` block exists solely so this generator-level
    /// test can confirm `ClaudeGenerator` treats the CLI's own structured
    /// `tool_use` content as inert history, never re-parsing it as pending
    /// `<tool_use>` markup the caller's `ToolLoop` would try to execute a
    /// second time (the concern issue #1303's fold existed to prevent for
    /// non-native-tool-calling providers).
    #[cfg(unix)]
    fn install_fake_claude_emitting_real_tool_use(dir: &std::path::Path) -> PathBuf {
        let bin = dir.join("fake-claude-real-tool-use");
        let script = r#"#!/bin/bash
SID=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ] || [ "$prev" = "--resume" ]; then SID="$a"; fi
  prev="$a"
done
cat >/dev/null
printf '%s\n' \
  '{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"gpt-6.1-sol"}' \
  '{"type":"assistant","message":{"model":"gpt-6.1-sol","id":"msg_tool_1","role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"mcp__finch__read","input":{"file_path":"/tmp/x.txt"}}]}}' \
  '{"type":"assistant","message":{"model":"gpt-6.1-sol","id":"msg_final","role":"assistant","content":[{"type":"text","text":"The file says: file contents"}],"usage":{"input_tokens":2,"output_tokens":4}}}' \
  '{"type":"result","subtype":"success","is_error":false,"result":"The file says: file contents","stop_reason":"end_turn"}'
"#;
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// Production-boundary regression for issue #1309, driven through the
    /// real `finch_providers::ClaudeCliProvider` transport (a fake `claude`
    /// binary stands in for the real CLI, same fixture pattern that crate's
    /// own tests use): now that `ClaudeCliProvider` declares tool calls
    /// `Supported`, a tool-bearing request must go through natively
    /// (`ProviderRequest::tools` attached) instead of `ClaudeGenerator`
    /// folding it into the prompt as issue #1303 required for the old
    /// `Unsupported` declaration -- and the CLI's own real, structured
    /// `tool_use` block must surface as plain final answer text, not
    /// `<tool_use>` markup requiring a second, generator-side parse. This is
    /// a generator-level concern only: how a real `tool_use` block actually
    /// gets resolved (forwarded over a socket to the frontend's real,
    /// interactive `ToolLoop`, issue #1341) is this fixture's job to *not*
    /// exercise -- it never opens a real bridge socket at all, and that real
    /// resolution path has its own tests in `finch_providers::claude_cli` and
    /// `tests/claude_cli_bridge_subprocess.rs`. `ClaudeGenerator` must not
    /// care how the round resolved; it must simply never re-surface an
    /// already-structured `tool_use` block as pending `<tool_use>` markup the
    /// caller's `ToolLoop` would try to execute a second time.
    #[cfg(unix)]
    #[tokio::test]
    async fn claude_cli_backend_sends_tools_natively_and_never_reparses_tool_use_markup() {
        let temp = tempfile::TempDir::new().unwrap();
        let binary = install_fake_claude_emitting_real_tool_use(temp.path());
        let provider: Arc<dyn crate::providers::LlmProvider> = Arc::new(
            finch_providers::ClaudeCliProvider::with_binary(binary, None),
        );
        assert!(
            provider.supports_tools(),
            "issue #1309: ClaudeCliProvider must declare native tool support so this generator \
             takes the native path below, not the #1303 prompt-injection fold"
        );
        let client = Arc::new(ClaudeClient::with_shared_provider(provider));
        let generator = ClaudeGenerator::new_in(client, None, None);

        let response = generator
            .generate(
                vec![Message::user("please read /tmp/x.txt")],
                Some(no_native_tool_definitions()),
            )
            .await
            .expect("a tool-bearing request against a native-tool-calling provider must succeed");

        assert_eq!(
            response.text, "The file says: file contents",
            "whatever mechanism actually resolved the tool_use round (a real interactive \
             ToolLoop execution in production, issue #1341 -- this fixture never opens a real \
             bridge socket, so nothing here exercises that resolution itself), the generator \
             must simply surface the CLI's own final answer text: {:?}",
            response
        );
        assert!(
            response.tool_uses.is_empty(),
            "ClaudeGenerator must not re-parse or re-surface an already-structured tool_use \
             block from the CLI's own stdout as a pending ToolUse the caller's ToolLoop would \
             try to execute a second time, regardless of how that call was actually resolved: \
             {:?}",
            response.tool_uses
        );
        assert!(
            !response.text.contains("tool_use"),
            "no raw tool-call markup of any kind may leak into the surfaced answer text: {:?}",
            response.text
        );
    }
}
