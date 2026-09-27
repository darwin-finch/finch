// Claude Code CLI subscription transport.
//
// Spawns the official `claude` CLI per turn (`--print --input-format
// stream-json --output-format stream-json`), overrides the system prompt, and
// pipes its NDJSON events into Finch's provider-neutral wire types. The CLI
// holds its own OAuth session; this transport never touches credentials.
//
// Wire contract measured against claude CLI 2.1.283 on 2026-09-25 and
// 2026-09-27:
// - Input: one JSON object per line, `{"type":"user","message":{"role":
//   "user","content":[{"type":"text","text":...}]}}`. Messages-API-shaped
//   input is silently ignored by the CLI, so every emitted line is validated
//   against the accepted shape before it is written.
// - Output: `system/init`, `assistant` (final message with usage),
//   `stream_event` (SSE-style deltas, only with --include-partial-messages),
//   `rate_limit_event` (subscription windows), and a terminal `result`.
// - `--verbose` is required for stream-json output.
// - A conversation continues with `--resume <id>`; `--session-id <id>`
//   errors ("already in use") once the id exists.
// - `--tools ""` always disables the CLI's own built-in tools (Read, Write,
//   Edit, Bash, Grep, Glob, ...). Verified directly (issue #1309) that
//   several of those built-ins auto-execute for real, on the CLI's own
//   authority, before any permission hook ever runs: a file read inside the
//   CLI's own working directory, anything under the OS temp directory
//   (`/tmp`, independent of working directory), and read-only Bash commands
//   per Claude Code's own permissions docs. None of that is gateable from
//   outside the CLI, so real Finch tool calls must never go through the
//   CLI's built-ins.
// - Real, provider-native tool use instead comes from Finch's own tools,
//   served to the CLI over MCP (`--mcp-config`, verified inline-JSON shape:
//   `{"mcpServers":{"<name>":{"type":"stdio","command":...,"args":[...]}}}`,
//   `--strict-mcp-config` so the CLI ignores the user's own personal MCP
//   integrations). The MCP server is this same `finch` binary, re-invoked
//   with the hidden [`CLAUDE_CLI_MCP_BRIDGE_FLAG`] (`src/cli/claude_cli_bridge.rs`
//   in the root crate): it registers Finch's own tool implementations and
//   really executes each call through Finch's permission policy — the CLI
//   never touches the filesystem or a shell itself. `--allowedTools
//   "mcp__<server>__<tool>"` (bare name) is required per tool: verified
//   directly that an MCP tool call is otherwise denied by default with no
//   permission host available in `--print` mode, and that a bare-name entry
//   auto-approves every call to that tool with no further prompting — safe
//   here because the bridge's own handler, not blanket approval, is what
//   decides whether the call actually runs.

use crate::types::{
    ModelCapabilities, ProviderAllowance, ProviderRequest, ProviderResponse, ProviderUsage,
    ReasoningCapability, StreamChunk,
};
use crate::wire_types::ContentBlock;
use crate::{CapabilitySupport, ProviderBackend, ValidatedProviderRequest, WireProtocol};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::sync::Mutex;

/// Default upstream model, measured as the CLI's own default on 2026-09-25
/// (claude CLI 2.1.283, `system/init` event: `claude-sonnet-5`).
pub const CLAUDE_CLI_DEFAULT_MODEL: &str = "claude-sonnet-5";

pub const CLAUDE_CLI_PROVIDER_NAME: &str = "claude-cli";
const MEASURED_ON: &str = "2026-09-25";
const MEASURED_SOURCE: &str =
    "claude CLI 2.1.283 stream-json probe; subscription transport, not API attestation";
const MEASURED_CONTEXT_WINDOW: usize = 1_000_000;
const MEASURED_MAX_OUTPUT_TOKENS: usize = 64_000;
const MAX_STDERR_BYTES: usize = 8 * 1024;

/// Hidden Finch subcommand flag. When `finch` is invoked with exactly this as
/// its first argument, it runs only the stdio MCP bridge server
/// (`src/cli/claude_cli_bridge.rs`) and exits — never the normal CLI. This
/// crate never executes tools itself (see the crate's `AGENTS.md`); it only
/// needs the flag's name so it can spawn its own binary
/// (`std::env::current_exe()`) as the MCP server Claude Code calls back into.
pub const CLAUDE_CLI_MCP_BRIDGE_FLAG: &str = "--internal-claude-cli-mcp-bridge";

/// MCP server name the CLI sees in `--mcp-config`. Wire tool names are
/// `mcp__<CLAUDE_CLI_MCP_SERVER_NAME>__<tool>`.
pub const CLAUDE_CLI_MCP_SERVER_NAME: &str = "finch";

/// Finch tool names this transport ever exposes to Claude Code. Every one of
/// them is executed by Finch's own bridge process, under Finch's own
/// permission policy — the CLI never runs Bash, Read, Write, Edit, Grep, or
/// Glob on its own authority (`--tools` always stays `""`). A tool that
/// exists in Finch but is not in this list is simply never offered to this
/// provider; the caller sees no tool-call attempt for it, not a failure.
pub const CLAUDE_CLI_TOOL_NAMES: &[&str] = &["read", "write", "edit", "glob", "grep", "bash"];

/// The MCP wire name Claude Code will call for a Finch tool name.
pub fn claude_cli_mcp_wire_name(finch_tool_name: &str) -> String {
    format!("mcp__{CLAUDE_CLI_MCP_SERVER_NAME}__{finch_tool_name}")
}

/// The reverse of [`claude_cli_mcp_wire_name`], for logging/observability
/// only (the bridge process is what actually decodes and dispatches calls).
pub fn claude_cli_tool_name_from_wire(wire_name: &str) -> Option<&str> {
    wire_name.strip_prefix(&format!("mcp__{CLAUDE_CLI_MCP_SERVER_NAME}__"))
}

/// Whether the `claude` CLI is usable as a backend.
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCliAvailability {
    pub binary: PathBuf,
    /// `Some` when the binary ran; `None` means not installed / not runnable.
    pub version: Option<String>,
    /// From `claude auth status`; `None` when the check could not run.
    pub logged_in: Option<bool>,
    pub auth_method: Option<String>,
}

impl ClaudeCliAvailability {
    /// Human-readable diagnosis naming exactly what is missing.
    pub fn status_line(&self) -> String {
        match (&self.version, self.logged_in) {
            (None, _) => format!(
                "claude CLI not found at {} (install Claude Code, then run it once interactively to log in)",
                self.binary.display()
            ),
            (Some(version), Some(true)) => format!(
                "claude CLI {version} at {}, logged in",
                self.binary.display()
            ),
            (Some(version), _) => format!(
                "claude CLI {version} at {} is installed but not logged in; run `claude auth status` to inspect",
                self.binary.display()
            ),
        }
    }
}

/// One completed CLI turn, before it is shaped into wire types.
#[derive(Debug, Default)]
struct TurnRecord {
    session_confirmed: Option<String>,
    assistant_text: String,
    assistant_model: Option<String>,
    assistant_message_id: Option<String>,
    usage: Option<ProviderUsage>,
    allowance: Option<ProviderAllowance>,
    result_text: Option<String>,
    result_error: Option<String>,
    /// Count of `tool_use` blocks seen across every `assistant` event in this
    /// turn — observability only, see [`TurnRecord::absorb_line`].
    tool_calls_observed: usize,
}

#[derive(Clone)]
pub struct ClaudeCliProvider {
    binary: PathBuf,
    model: String,
    session_id: uuid::Uuid,
    /// Set once a turn has completed successfully for this session id; later
    /// turns continue the conversation with `--resume`.
    resumable: Arc<Mutex<bool>>,
}

impl ClaudeCliProvider {
    pub fn new(model: Option<String>) -> Self {
        Self::with_binary(PathBuf::from("claude"), model)
    }

    pub fn with_binary(binary: PathBuf, model: Option<String>) -> Self {
        Self {
            binary,
            model: model.unwrap_or_else(|| CLAUDE_CLI_DEFAULT_MODEL.to_string()),
            session_id: uuid::Uuid::new_v4(),
            resumable: Arc::new(Mutex::new(false)),
        }
    }

    /// The session id every turn of this provider instance resumes.
    pub fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Whether the CLI is installed and logged in. Read-only probes only;
    /// never launches a generation turn.
    pub async fn detect(&self) -> ClaudeCliAvailability {
        let version = match run_capture(&self.binary, &["--version"]).await {
            Ok(version) => Some(version.trim().to_string()),
            Err(_) => {
                return ClaudeCliAvailability {
                    binary: self.binary.clone(),
                    version: None,
                    logged_in: None,
                    auth_method: None,
                }
            }
        };
        let (logged_in, auth_method) = match run_capture(&self.binary, &["auth", "status"]).await {
            Ok(status) => serde_json::from_str::<serde_json::Value>(&status)
                .ok()
                .map(|parsed| {
                    (
                        parsed.get("loggedIn").and_then(|v| v.as_bool()),
                        parsed
                            .get("authMethod")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    )
                })
                .unwrap_or((None, None)),
            Err(_) => (None, None),
        };
        ClaudeCliAvailability {
            binary: self.binary.clone(),
            version,
            logged_in,
            auth_method,
        }
    }

    /// Pure argument-vector builder: turn 1 mints the session with
    /// `--session-id`, later turns continue it with `--resume`. `tool_names`
    /// are Finch tool names to expose over MCP for this turn (already
    /// intersected with [`CLAUDE_CLI_TOOL_NAMES`] by the caller); empty means
    /// no tools at all. `--tools` itself is always `""`: the CLI's own
    /// built-ins never run (see the module doc comment).
    fn invocation_args(
        &self,
        resumable: bool,
        system: Option<&str>,
        tool_names: &[String],
    ) -> Result<Vec<String>> {
        let mut args = vec![
            "--print".to_string(),
            "--verbose".to_string(),
            "--include-partial-messages".to_string(),
            "--input-format".to_string(),
            "stream-json".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--tools".to_string(),
            String::new(),
        ];
        args.extend(self.mcp_bridge_args(tool_names)?);
        if let Some(system) = system {
            args.push("--system-prompt".to_string());
            args.push(system.to_string());
        }
        if resumable {
            args.push("--resume".to_string());
        } else {
            args.push("--session-id".to_string());
        }
        args.push(self.session_id.to_string());
        args.push("--model".to_string());
        args.push(self.model.clone());
        Ok(args)
    }

    /// `--mcp-config`/`--strict-mcp-config`/`--allowedTools` for serving
    /// Finch's own tools over MCP (issue #1309). Empty `tool_names` means no
    /// tool support this turn — no MCP server is registered at all, matching
    /// the pre-#1309 `--tools ""`-only behavior exactly.
    ///
    /// The MCP server is this same Finch binary, re-invoked with
    /// [`CLAUDE_CLI_MCP_BRIDGE_FLAG`]: real execution happens in that
    /// subprocess, under Finch's own permission policy
    /// (`src/cli/claude_cli_bridge.rs`), never inside the `claude` CLI.
    fn mcp_bridge_args(&self, tool_names: &[String]) -> Result<Vec<String>> {
        if tool_names.is_empty() {
            return Ok(Vec::new());
        }
        let exe = std::env::current_exe()
            .context("resolving Finch's own executable path for the Claude Code MCP bridge")?;
        let mcp_config = json!({
            "mcpServers": {
                CLAUDE_CLI_MCP_SERVER_NAME: {
                    "type": "stdio",
                    "command": exe.to_string_lossy(),
                    "args": [CLAUDE_CLI_MCP_BRIDGE_FLAG],
                }
            }
        });
        let allowed_tools = tool_names
            .iter()
            .map(|name| claude_cli_mcp_wire_name(name))
            .collect::<Vec<_>>()
            .join(",");
        Ok(vec![
            "--mcp-config".to_string(),
            mcp_config.to_string(),
            "--strict-mcp-config".to_string(),
            "--allowedTools".to_string(),
            allowed_tools,
        ])
    }

    /// Finch tool names requested for this turn, intersected with
    /// [`CLAUDE_CLI_TOOL_NAMES`] in the CLI's own advertised order (stable
    /// argv, better for the CLI's prompt cache). A requested tool this
    /// transport does not support is silently omitted, not an error: the
    /// model simply never sees or attempts it through this provider.
    fn supported_tool_names(&self, request: &ProviderRequest) -> Vec<String> {
        let requested: std::collections::HashSet<&str> = request
            .tools
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|definition| definition.name.as_str())
            .collect();
        CLAUDE_CLI_TOOL_NAMES
            .iter()
            .filter(|name| requested.contains(*name))
            .map(|name| name.to_string())
            .collect()
    }

    /// Extract (system prompt, pending input lines) from a provider request.
    ///
    /// The CLI session owns conversation history across turns (`--resume`),
    /// so this transport sends only what the CLI session has not seen:
    /// everything after the last assistant message. System-role messages
    /// (Finch persona, VM wire contract) become `--system-prompt`, whose
    /// stable bytes are what make the CLI's prompt cache effective.
    fn split_request(&self, request: &ProviderRequest) -> Result<(Option<String>, Vec<String>)> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut last_assistant = None;
        for (index, message) in request.messages.iter().enumerate() {
            match message.role.as_str() {
                "system" => system_parts.extend(
                    message
                        .content
                        .iter()
                        .filter_map(|block| block.as_text().map(str::to_string)),
                ),
                "assistant" => last_assistant = Some(index),
                "user" => {}
                other => bail!("claude-cli transport cannot send message role {other:?}"),
            }
        }
        let system = (!system_parts.is_empty()).then(|| system_parts.join("\n\n"));

        let increment_start = last_assistant.map_or(0, |index| index + 1);
        let mut lines = Vec::new();
        for message in &request.messages[increment_start..] {
            match message.role.as_str() {
                // System content anywhere in the array already went into the
                // system prompt above; it is never part of the turn increment.
                "system" => continue,
                "user" => {}
                other => bail!(
                    "claude-cli transport sends only pending user turns, found role {other:?}"
                ),
            }
            let mut parts = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => parts.push(text.clone()),
                    ContentBlock::ToolResult { content, .. } => parts.push(content.clone()),
                    other => bail!(
                        "claude-cli transport is text-only; cannot send content block {other:?}"
                    ),
                }
            }
            let text = parts.join("\n");
            if !text.trim().is_empty() {
                lines.push(user_input_line(&text)?);
            }
        }
        if lines.is_empty() {
            bail!(
                "claude-cli transport found no pending user turn to send: the CLI session \
                 already holds this conversation state"
            );
        }
        Ok((system, lines))
    }

    async fn execute_turn(
        &self,
        request: &ProviderRequest,
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnRecord> {
        let (system, input_lines) = self.split_request(request)?;
        let tool_names = self.supported_tool_names(request);
        let resumable = *self.resumable.lock().await;
        match self
            .run_turn_once(
                resumable,
                system.as_deref(),
                &input_lines,
                &tool_names,
                deltas.clone(),
            )
            .await
        {
            Ok(record) => {
                self.mark_resumable().await;
                Ok(record)
            }
            Err(error) => {
                // A session id only becomes resumable once the CLI has
                // persisted it. A first turn that failed before persisting
                // must retry as a fresh session, while a mid-conversation
                // failure may have left the id already stored. The CLI's
                // "already in use" exit names the latter exactly.
                if !resumable && error.to_string().contains("already in use") {
                    let retry = self
                        .run_turn_once(true, system.as_deref(), &input_lines, &tool_names, deltas)
                        .await?;
                    self.mark_resumable().await;
                    return Ok(retry);
                }
                Err(error)
            }
        }
    }

    async fn mark_resumable(&self) {
        *self.resumable.lock().await = true;
    }

    async fn run_turn_once(
        &self,
        resumable: bool,
        system: Option<&str>,
        input_lines: &[String],
        tool_names: &[String],
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnRecord> {
        let args = self.invocation_args(resumable, system, tool_names)?;
        let mut child = tokio::process::Command::new(&self.binary)
            .args(&args)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "spawning claude CLI at {}; `claude auth status` inspects its login",
                    self.binary.display()
                )
            })?;
        let mut stdin = child.stdin.take().context("claude CLI stdin")?;
        let stdout = child.stdout.take().context("claude CLI stdout")?;
        let mut stderr = child.stderr.take().context("claude CLI stderr")?;

        let mut stdin_buffer = String::new();
        for line in input_lines {
            stdin_buffer.push_str(line);
            stdin_buffer.push('\n');
        }
        stdin
            .write_all(stdin_buffer.as_bytes())
            .await
            .context("write claude CLI input")?;
        stdin.flush().await.context("flush claude CLI input")?;
        drop(stdin);

        let stderr_task = tokio::spawn(async move {
            let mut bounded = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) if bounded.len() < MAX_STDERR_BYTES => {
                        bounded.extend_from_slice(&buffer[..read]);
                    }
                    Ok(_) => {}
                }
            }
            String::from_utf8_lossy(&bounded).to_string()
        });

        let mut reader = tokio::io::BufReader::new(stdout);
        let mut record = TurnRecord::default();
        let mut line = String::new();
        loop {
            line.clear();
            let read = reader.read_line(&mut line).await?;
            if read == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(delta) = record.absorb_line(trimmed)? {
                if let Some(deltas) = &deltas {
                    deltas
                        .send(Ok(StreamChunk::TextDelta(delta)))
                        .await
                        .map_err(|error| anyhow!("claude CLI stream sink closed: {error}"))?;
                }
            }
        }
        let status = child.wait().await?;
        let stderr_text = stderr_task.await.unwrap_or_default();
        if !status.success() {
            bail!(
                "claude CLI exited with {status}; stderr: {}",
                bounded_text(&stderr_text)
            );
        }
        record.validated(self.session_id)?;
        Ok(record)
    }

    fn response_from(&self, record: &TurnRecord, requested_model: &str) -> ProviderResponse {
        ProviderResponse {
            id: record
                .assistant_message_id
                .clone()
                .unwrap_or_else(|| self.session_id.to_string()),
            model: record
                .assistant_model
                .clone()
                .unwrap_or_else(|| requested_model.to_string()),
            content: vec![ContentBlock::text(record.response_text())],
            stop_reason: Some("end_turn".to_string()),
            role: "assistant".to_string(),
            provider: CLAUDE_CLI_PROVIDER_NAME.to_string(),
            usage: record.usage.clone(),
            allowance: record.allowance.clone(),
        }
    }
}

impl TurnRecord {
    fn absorb_line(&mut self, line: &str) -> Result<Option<String>> {
        let event: serde_json::Value = serde_json::from_str(line).with_context(|| {
            format!("claude CLI emitted a non-JSON line: {}", bounded_text(line))
        })?;
        match event.get("type").and_then(|value| value.as_str()) {
            Some("system") => {
                if event.get("subtype").and_then(|v| v.as_str()) == Some("init") {
                    self.session_confirmed = event
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            Some("assistant") => {
                let message = event.get("message").cloned().unwrap_or_default();
                if let Some(content) = message.get("content").and_then(|v| v.as_array()) {
                    self.assistant_text = content
                        .iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                    // Observability only (issue #1309): a tool_use block here
                    // was already served and really executed by Finch's own
                    // MCP bridge process by the time this line arrives —
                    // Claude Code folds the real tool_result back into the
                    // same turn automatically. Logging (and the count this
                    // module's tests assert on) is deliberately the only
                    // effect: emitting a ToolCallComplete/ContentBlockComplete
                    // chunk here would make the generation layer execute the
                    // same call a second time through Finch's interactive
                    // ToolLoop (see `finch-providers`' `AGENTS.md`: "Dual
                    // encoding of the same id+input is one call at the
                    // ToolLoop").
                    for block in content {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                            let wire_name = block.get("name").and_then(|n| n.as_str());
                            self.tool_calls_observed += 1;
                            tracing::debug!(
                                wire_name,
                                finch_tool = wire_name.and_then(claude_cli_tool_name_from_wire),
                                "claude CLI called a Finch tool over MCP"
                            );
                        }
                    }
                }
                self.assistant_model = message
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                self.assistant_message_id = message
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                self.usage = message.get("usage").and_then(parse_usage);
            }
            Some("stream_event") => return Ok(stream_delta_text(&event)),
            Some("rate_limit_event") => self.allowance = parse_allowance(&event),
            Some("result") => {
                if event.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
                    self.result_error = Some(
                        event
                            .get("result")
                            .or_else(|| event.get("error"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("claude CLI reported an error without a message")
                            .to_string(),
                    );
                } else {
                    self.result_text = event
                        .get("result")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// Terminal-state validation: the session identity matches, the result
    /// event reported success, and some response text exists.
    fn validated(&self, expected_session: uuid::Uuid) -> Result<()> {
        if let Some(error) = &self.result_error {
            bail!("claude CLI turn failed: {error}");
        }
        match &self.session_confirmed {
            Some(confirmed) if confirmed == &expected_session.to_string() => {}
            other => bail!(
                "claude CLI session identity mismatch: expected {expected_session}, init reported {other:?}"
            ),
        }
        Ok(())
    }

    fn response_text(&self) -> String {
        self.result_text
            .clone()
            .unwrap_or_else(|| self.assistant_text.clone())
    }
}

fn user_input_line(text: &str) -> Result<String> {
    let line = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}],
        },
    });
    serde_json::to_string(&line).context("encode claude CLI input line")
}

fn parse_usage(usage: &serde_json::Value) -> Option<ProviderUsage> {
    Some(ProviderUsage {
        input_tokens: u32::try_from(usage.get("input_tokens")?.as_u64()?).ok()?,
        output_tokens: u32::try_from(usage.get("output_tokens")?.as_u64()?).ok()?,
    })
}

fn parse_allowance(event: &serde_json::Value) -> Option<ProviderAllowance> {
    let windows = event.get("rate_limit_info")?.get("unifiedWindows")?;
    let percent = |key: &str| {
        windows
            .get(key)
            .and_then(|window| window.get("utilization"))
            .and_then(|value| value.as_f64())
            .map(|value| (value * 100.0) as f32)
    };
    Some(ProviderAllowance {
        primary_used_percent: percent("five_hour"),
        secondary_used_percent: percent("seven_day"),
    })
}

fn stream_delta_text(event: &serde_json::Value) -> Option<String> {
    let inner = event.get("event")?;
    if inner.get("type").and_then(|v| v.as_str()) != Some("content_block_delta") {
        return None;
    }
    let delta = inner.get("delta")?;
    if delta.get("type").and_then(|v| v.as_str()) == Some("text_delta") {
        return delta
            .get("text")
            .and_then(|v| v.as_str())
            .map(str::to_string);
    }
    None
}

fn bounded_text(text: &str) -> String {
    if text.len() <= MAX_STDERR_BYTES {
        return text.to_string();
    }
    let mut boundary = MAX_STDERR_BYTES;
    while !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    format!("{}…[truncated]", &text[..boundary])
}

async fn run_capture(binary: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .with_context(|| format!("running {} {}", binary.display(), args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "{} {} exited with status {}",
            binary.display(),
            args.join(" "),
            output.status
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[async_trait::async_trait]
impl ProviderBackend for ClaudeCliProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        let record = self.execute_turn(&request, None).await?;
        Ok(self.response_from(&record, &request.model))
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let (request, _bindings) = request.into_request_for(self)?;
        let (tx, rx) = mpsc::channel::<Result<StreamChunk>>(64);
        let worker = self.clone();
        tokio::spawn(async move {
            let result = worker.execute_turn(&request, Some(tx.clone())).await;
            match result {
                Ok(record) => {
                    let _ = tx
                        .send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::text(
                            record.response_text(),
                        ))))
                        .await;
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                }
            }
        });
        Ok(rx)
    }

    fn name(&self) -> &str {
        CLAUDE_CLI_PROVIDER_NAME
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        ModelCapabilities::static_metadata(
            CLAUDE_CLI_PROVIDER_NAME,
            model,
            MEASURED_ON,
            MEASURED_SOURCE,
            CapabilitySupport::Supported,
            // Tool calls are supported, but not via the CLI's own built-in
            // tools (`--tools` always stays `""`, so the CLI itself executes
            // nothing on its own authority — see the module doc comment).
            // Support here means Finch's own tools, served over MCP and
            // executed by Finch's own bridge process (issue #1309); the
            // prompt-injection fold in `src/generators/claude.rs` is bypassed
            // for this provider accordingly.
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported(MEASURED_ON, MEASURED_SOURCE),
            Some(MEASURED_CONTEXT_WINDOW),
            Some(MEASURED_MAX_OUTPUT_TOKENS),
            None,
        )
        // Finch's own `ToolDefinition`s compile against the Anthropic wire
        // shape for this transport too (id/name/input `tool_use` blocks,
        // matching the real `claude` CLI's stream-json output); the compiled
        // table itself goes unused here — `supported_tool_names` derives the
        // exposed tool list directly from `request.tools`, since the CLI's
        // own `--tools`/`--allowedTools` flags take plain names, not a JSON
        // schema.
        .with_wire_protocol(
            WireProtocol::AnthropicMessages,
            MEASURED_ON,
            MEASURED_SOURCE,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LlmProvider;
    use tempfile::TempDir;

    /// Install a fake `claude` executable that records its argv and stdin and
    /// emits canned NDJSON matching the measured wire contract. Behavior is
    /// selected by the first argument word: default is a successful turn;
    /// `fail-in-use` fails with the CLI's id-reuse error when invoked with
    /// `--session-id` and succeeds when invoked with `--resume`; `exit-fail`
    /// exits nonzero after one init line.
    fn install_fake_claude(home: &TempDir, behavior: &str) -> PathBuf {
        let spool = spool(home);
        let bin = home.path().join("fake-claude");
        let script = format!(
            r#"#!/bin/bash
SPOOL="{spool}"
MODE="{behavior}"
SID=""
FLAG=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ] || [ "$prev" = "--resume" ]; then SID="$a"; FLAG="$prev"; fi
  prev="$a"
done
if [ "$1" = "--version" ]; then
  echo "2.1.283 (Claude Code)"
  exit 0
fi
if [ "$1" = "auth" ]; then
  echo '{{"loggedIn": true, "authMethod": "claude.ai", "apiProvider": "firstParty"}}'
  exit 0
fi
{{
  echo "FLAGS: $FLAG"
  echo "SID: $SID"
  echo "ARGS: $*"
  echo "STDIN:"
  cat
  echo "END-CALL"
}} >> "$SPOOL/calls.log"
if [ "$MODE" = "exit-fail" ]; then
  echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
  echo "boom: simulated claude failure" >&2
  exit 3
fi
if [ "$MODE" = "fail-in-use" ] && [ "$FLAG" = "--session-id" ]; then
  echo "Error: Session ID $SID is already in use." >&2
  exit 1
fi
printf '%s\n' \
  '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}' \
  '{{"type":"rate_limit_event","rate_limit_info":{{"status":"allowed","unifiedWindows":{{"five_hour":{{"utilization":0.07}},"seven_day":{{"utilization":0.06}}}}}}}}' \
  '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"he"}}}}}}' \
  '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"llo"}}}}}}' \
  '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_fake_1","role":"assistant","content":[{{"type":"text","text":"hello"}}],"usage":{{"input_tokens":2,"output_tokens":4}}}}}}' \
  '{{"type":"result","subtype":"success","is_error":false,"result":"hello","stop_reason":"end_turn"}}'
"#
        );
        std::fs::write(&bin, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    fn spool(temp: &TempDir) -> String {
        let spool = temp.path().join("spool");
        std::fs::create_dir_all(&spool).unwrap();
        spool.to_string_lossy().to_string()
    }

    fn calls_log(temp: &TempDir) -> String {
        std::fs::read_to_string(temp.path().join("spool/calls.log")).unwrap()
    }

    fn simple_request() -> ProviderRequest {
        ProviderRequest {
            messages: vec![crate::Message {
                role: "user".to_string(),
                content: vec![ContentBlock::text("say hello")],
            }],
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
            max_tokens: 256,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            tool_policy: Default::default(),
        }
    }

    fn request_with_system_and_history() -> ProviderRequest {
        ProviderRequest {
            messages: vec![
                crate::Message {
                    role: "system".to_string(),
                    content: vec![ContentBlock::text("PERSONA BLOCK")],
                },
                crate::Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::text("turn one")],
                },
                crate::Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::text("turn one answer")],
                },
                crate::Message {
                    role: "system".to_string(),
                    content: vec![ContentBlock::text("VM MANIFEST")],
                },
                crate::Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::text("turn two")],
                },
            ],
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
            max_tokens: 256,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            tool_policy: Default::default(),
        }
    }

    #[tokio::test]
    async fn first_turn_uses_session_id_and_next_turn_resumes_the_same_id() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let first = provider
            .send_message(&simple_request())
            .await
            .expect("first turn must succeed against the fake CLI");
        assert_eq!(first.provider, "claude-cli");
        assert_eq!(first.content.len(), 1);
        assert_eq!(
            first.content[0].as_text().unwrap(),
            "hello",
            "the buffered path must return the result event's text"
        );
        assert_eq!(first.usage.as_ref().unwrap().output_tokens, 4);
        assert_eq!(
            first.allowance.as_ref().unwrap().primary_used_percent,
            Some(7.0),
            "the five-hour subscription window maps to the primary allowance"
        );

        let second = provider
            .send_message(&simple_request())
            .await
            .expect("second turn must resume successfully");

        let log = calls_log(&temp);
        let first_flag = log.split("END-CALL").next().unwrap();
        assert!(
            first_flag.contains("FLAGS: --session-id"),
            "turn one must mint the session with --session-id; log: {log}"
        );
        let second_block = log.split("END-CALL").nth(1).unwrap();
        assert!(
            second_block.contains("FLAGS: --resume"),
            "turn two must continue with --resume; log: {log}"
        );
        let first_sid = first_flag
            .lines()
            .find_map(|line| line.strip_prefix("SID: "))
            .unwrap()
            .to_string();
        let second_sid = second_block
            .lines()
            .find_map(|line| line.strip_prefix("SID: "))
            .unwrap()
            .to_string();
        assert_eq!(
            first_sid, second_sid,
            "both turns must address the same CLI session id"
        );
        assert_eq!(first_sid, provider.session_id().to_string());
        assert_eq!(second.id, "msg_fake_1");
    }

    #[tokio::test]
    async fn input_lines_carry_only_the_pending_user_turn_in_the_measured_shape() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        provider
            .send_message(&request_with_system_and_history())
            .await
            .expect("turn with history must succeed");

        let log = calls_log(&temp);
        let stdin_section = log
            .split("STDIN:")
            .nth(1)
            .and_then(|rest| rest.split("END-CALL").next())
            .unwrap();
        let lines: Vec<&str> = stdin_section
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "only the pending user turn after the last assistant message may be sent; got {lines:?}"
        );
        let parsed: serde_json::Value = serde_json::from_str(lines[0])
            .expect("each emitted input line must be one JSON object");
        assert_eq!(parsed["type"], "user", "input lines use the CLI user shape");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(parsed["message"]["content"][0]["type"], "text");
        assert_eq!(parsed["message"]["content"][0]["text"], "turn two");
    }

    #[tokio::test]
    async fn system_role_messages_become_the_system_prompt_flag() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        provider
            .send_message(&request_with_system_and_history())
            .await
            .unwrap();

        let log = calls_log(&temp);
        let args_line = log
            .lines()
            .find(|line| line.starts_with("ARGS: "))
            .expect("the fake must record the full argv");
        assert!(
            args_line.contains("--system-prompt"),
            "system content must travel via --system-prompt; argv: {args_line}"
        );
        let system_text_start = args_line
            .split("--system-prompt")
            .nth(1)
            .expect("--system-prompt present")
            .trim_start();
        assert!(
            system_text_start.contains("PERSONA BLOCK")
                || system_text_start.contains("VM MANIFEST"),
            "the concatenated system-role text must be the flag's value; argv: {args_line}"
        );
    }

    #[tokio::test]
    async fn streaming_emits_text_deltas_then_a_complete_text_block() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let mut rx = provider
            .send_message_stream(&simple_request())
            .await
            .expect("streaming must be supported");
        let mut deltas = Vec::new();
        let mut complete = None;
        while let Some(chunk) = rx.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(text) => deltas.push(text),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                other => panic!("unexpected chunk {other:?}"),
            }
        }
        assert_eq!(
            deltas,
            vec!["he".to_string(), "llo".to_string()],
            "content_block_delta text must surface as ordered TextDelta chunks"
        );
        assert_eq!(
            complete.as_deref(),
            Some("hello"),
            "the terminal assistant text must arrive as one complete text block"
        );
    }

    #[tokio::test]
    async fn error_result_maps_to_err_with_the_cli_message() {
        let temp = TempDir::new().unwrap();
        let bin = temp.path().join("fake-claude");
        std::fs::write(
            &bin,
            r#"#!/bin/bash
# Drain stdin before emitting output. The real CLI (and every other fake
# CLI in this test module, see install_fake_claude's trailing `cat`) reads
# its stdin; a fake that never touches stdin can run to completion and
# close its end of the pipe before the parent's write_all/flush finishes,
# racing `stdin.write_all(...).context("write claude CLI input")` in
# run_turn_once into a spurious EPIPE that masks the intended CLI error
# (issue #1274). Reading to EOF forces the parent's write-then-close to
# happen before this script proceeds.
cat >/dev/null
printf '%s\n' \
  '{"type":"system","subtype":"init","session_id":"ignored","model":"claude-sonnet-5"}' \
  '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"Credit balance too low"}'
"#
            ,).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let provider = ClaudeCliProvider::with_binary(bin, None);

        let error = provider
            .send_message(&simple_request())
            .await
            .expect_err("an is_error result must fail the turn");
        assert!(
            error.to_string().contains("Credit balance too low"),
            "the CLI's error message must surface: {error}"
        );
    }

    #[tokio::test]
    async fn nonzero_exit_surfaces_bounded_stderr() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "exit-fail");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let error = provider
            .send_message(&simple_request())
            .await
            .expect_err("a nonzero CLI exit must fail the turn");
        assert!(
            error.to_string().contains("boom: simulated claude failure"),
            "stderr must be captured for diagnosis: {error}"
        );
    }

    #[tokio::test]
    async fn first_turn_rejected_as_already_in_use_retries_with_resume() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "fail-in-use");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let response = provider
            .send_message(&simple_request())
            .await
            .expect("the already-in-use retry must land on --resume and succeed");
        assert_eq!(response.content[0].as_text().unwrap(), "hello");

        let log = calls_log(&temp);
        let blocks: Vec<&str> = log.split("END-CALL").collect();
        assert!(
            blocks[0].contains("FLAGS: --session-id"),
            "the first attempt must still try to mint the session; log: {log}"
        );
        assert!(
            blocks[1].contains("FLAGS: --resume"),
            "the retry must resume the existing session; log: {log}"
        );
    }

    #[tokio::test]
    async fn detect_reports_version_login_and_missing_binary() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary.clone(), None);
        let available = provider.detect().await;
        assert_eq!(available.version.as_deref(), Some("2.1.283 (Claude Code)"));
        assert_eq!(available.logged_in, Some(true));
        assert_eq!(available.auth_method.as_deref(), Some("claude.ai"));
        assert!(
            available.status_line().contains("logged in"),
            "the status line must describe a usable install: {}",
            available.status_line()
        );

        let missing = ClaudeCliProvider::with_binary(temp.path().join("no-such-claude"), None)
            .detect()
            .await;
        assert_eq!(missing.version, None);
        assert_eq!(missing.logged_in, None);
        assert!(
            missing.status_line().contains("not found"),
            "a missing binary must be named as the failure: {}",
            missing.status_line()
        );
    }

    #[test]
    fn invocation_args_always_disable_tools_and_enable_streaming() {
        let provider = ClaudeCliProvider::new(None);
        let args = provider
            .invocation_args(false, Some("SYSTEM"), &[])
            .unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("--print"));
        assert!(
            joined.contains("--verbose"),
            "--verbose is required for stream-json output"
        );
        assert!(joined.contains("--include-partial-messages"));
        assert!(joined.contains("--input-format stream-json"));
        assert!(joined.contains("--output-format stream-json"));
        assert!(joined.contains("--system-prompt SYSTEM"));
        assert!(
            args.iter().zip(args.iter().skip(1)).any(|(flag, value)| flag == "--tools" && value.is_empty()),
            "--tools with an empty value is what keeps the CLI's own built-in tools from executing on its own authority; args: {joined}"
        );
        assert!(
            !joined.contains("--mcp-config"),
            "no tool names requested must mean no MCP server is registered at all: {joined}"
        );
        assert!(
            joined.contains("--session-id"),
            "turn one mints the session"
        );
        assert!(!joined.contains("--resume"), "turn one must not resume");

        let resume_args = provider.invocation_args(true, None, &[]).unwrap();
        assert!(resume_args.contains(&"--resume".to_string()));
        assert!(
            !resume_args.iter().any(|arg| arg == "--system-prompt"),
            "an absent system prompt must omit the flag, not send empty bytes"
        );
    }

    #[test]
    fn invocation_args_with_tools_wires_the_mcp_bridge_not_the_clis_own_builtins() {
        let provider = ClaudeCliProvider::new(None);
        let args = provider
            .invocation_args(false, None, &["read".to_string(), "bash".to_string()])
            .unwrap();
        assert!(
            args.iter().zip(args.iter().skip(1)).any(|(flag, value)| flag == "--tools" && value.is_empty()),
            "the CLI's own built-in tools must stay disabled even when Finch tools are exposed over MCP: {args:?}"
        );
        let mcp_config_value = args
            .iter()
            .zip(args.iter().skip(1))
            .find(|(flag, _)| *flag == "--mcp-config")
            .map(|(_, value)| value.clone())
            .expect("tool names requested must register the Finch MCP bridge server");
        let parsed: serde_json::Value = serde_json::from_str(&mcp_config_value)
            .expect("--mcp-config value must be valid inline JSON");
        let server = &parsed["mcpServers"][CLAUDE_CLI_MCP_SERVER_NAME];
        assert_eq!(server["type"], "stdio");
        assert_eq!(
            server["args"][0], CLAUDE_CLI_MCP_BRIDGE_FLAG,
            "the MCP server command must be this same Finch binary, re-invoked with the hidden bridge flag"
        );
        assert!(
            args.contains(&"--strict-mcp-config".to_string()),
            "the user's own personal MCP integrations must never be pulled into this session: {args:?}"
        );
        let allowed_tools = args
            .iter()
            .zip(args.iter().skip(1))
            .find(|(flag, _)| *flag == "--allowedTools")
            .map(|(_, value)| value.clone())
            .expect("--allowedTools must name the exposed tools so the bridge is not blocked on approval");
        assert_eq!(
            allowed_tools,
            format!(
                "{},{}",
                claude_cli_mcp_wire_name("read"),
                claude_cli_mcp_wire_name("bash")
            ),
            "allowedTools must name exactly the requested, supported tools, in CLAUDE_CLI_TOOL_NAMES order"
        );
    }

    #[test]
    fn supported_tool_names_intersects_with_the_safe_allowlist_and_ignores_unknown_tools() {
        let provider = ClaudeCliProvider::new(None);
        let mut request = simple_request();
        request.tools = Some(vec![
            tool_definition("bash"),
            tool_definition("read"),
            tool_definition("some_other_finch_tool_not_on_the_cli_allowlist"),
        ]);
        let names = provider.supported_tool_names(&request);
        assert_eq!(
            names,
            vec!["read".to_string(), "bash".to_string()],
            "only tools in CLAUDE_CLI_TOOL_NAMES are exposed, in that fixed order; \
             unsupported tool names are silently dropped, not an error: {names:?}"
        );
    }

    #[test]
    fn split_request_rejects_unsupported_shapes_with_actionable_errors() {
        let provider = ClaudeCliProvider::new(None);

        let image = ProviderRequest {
            messages: vec![crate::Message {
                role: "user".to_string(),
                content: vec![ContentBlock::image("image/png", "aGVsbG8=")],
            }],
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
            max_tokens: 16,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            tool_policy: Default::default(),
        };
        let error = provider.split_request(&image).unwrap_err().to_string();
        assert!(
            error.contains("text-only"),
            "image blocks must be refused with the transport's text-only constraint: {error}"
        );

        let no_pending = ProviderRequest {
            messages: vec![crate::Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::text("only an answer")],
            }],
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
            max_tokens: 16,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            tool_policy: Default::default(),
        };
        let error = provider.split_request(&no_pending).unwrap_err().to_string();
        assert!(
            error.contains("no pending user turn"),
            "an all-assistant request has nothing the CLI session has not seen: {error}"
        );

        let unknown_role = ProviderRequest {
            messages: vec![crate::Message {
                role: "tool".to_string(),
                content: vec![ContentBlock::text("x")],
            }],
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
            max_tokens: 16,
            system: None,
            tools: None,
            temperature: None,
            stream: false,
            cancellation_token: None,
            tool_policy: Default::default(),
        };
        let error = provider
            .split_request(&unknown_role)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("role \"tool\""),
            "unknown roles must be named, not silently dropped: {error}"
        );
    }

    #[test]
    fn capabilities_declare_native_tools_via_the_mcp_bridge() {
        let provider = ClaudeCliProvider::new(None);
        let capabilities = provider.capabilities(CLAUDE_CLI_DEFAULT_MODEL);
        assert!(capabilities.streaming.is_supported());
        assert!(
            capabilities.tools.is_supported(),
            "issue #1309: real tool calls are supported via Finch's own MCP bridge, \
             even though the CLI's own built-in tools stay disabled"
        );
        assert_eq!(
            capabilities.wire_protocol.protocol,
            Some(WireProtocol::AnthropicMessages),
            "tool bindings must compile against a known wire protocol for validation to accept a tool-bearing request"
        );
        assert_eq!(
            capabilities.context_window.max_tokens,
            Some(MEASURED_CONTEXT_WINDOW)
        );
        assert_eq!(
            capabilities.output_token_limit.max_tokens,
            Some(MEASURED_MAX_OUTPUT_TOKENS)
        );
    }

    fn tool_definition(name: &str) -> crate::ToolDefinition {
        crate::ToolDefinition {
            name: name.to_string(),
            description: format!("{name} tool"),
            input_schema: crate::ToolInputSchema {
                schema_type: "object".to_string(),
                properties: serde_json::json!({}),
                required: Vec::new(),
            },
        }
    }

    #[tokio::test]
    async fn a_request_asking_for_a_supported_tool_is_accepted_by_validation() {
        let provider = ClaudeCliProvider::new(None);
        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        crate::validate_provider_request(&provider, &request, false)
            .await
            .expect(
                "issue #1309: a request for a tool this transport supports over MCP \
                 must no longer be refused by capability validation",
            );
    }

    #[tokio::test]
    async fn a_full_turn_with_a_real_tool_use_block_is_observed_and_never_double_forwarded() {
        // Reproduces the exact shape verified against the real claude CLI
        // 2.1.283 (issue #1309): an `assistant` event's content array can
        // contain a `tool_use` block calling this transport's own MCP bridge
        // tool, followed later by the CLI's normal final-answer event. The
        // bridge process has already executed the call for real by the time
        // this line arrives — `TurnRecord` must count/log it (observability)
        // without emitting anything the generation layer would execute a
        // second time.
        let mut record = TurnRecord::default();
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_1","content":[{"type":"tool_use","id":"toolu_1","name":"mcp__finch__read","input":{"file_path":"/tmp/x"}}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.tool_calls_observed, 1,
            "a tool_use block in an assistant event must be observed exactly once"
        );
        assert_eq!(
            record.assistant_text, "",
            "a tool_use-only assistant event carries no text of its own"
        );
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_2","content":[{"type":"text","text":"Done."}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.tool_calls_observed, 1,
            "a later text-only assistant event must not double-count the earlier tool call"
        );
        assert_eq!(
            record.response_text(),
            "Done.",
            "the final text-only assistant event must win, overwriting the intermediate tool-use turn"
        );
    }
}
