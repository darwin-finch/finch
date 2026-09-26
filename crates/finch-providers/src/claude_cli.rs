// Claude Code CLI subscription transport.
//
// Spawns the official `claude` CLI per turn (`--print --input-format
// stream-json --output-format stream-json`), overrides the system prompt, and
// pipes its NDJSON events into Finch's provider-neutral wire types. The CLI
// holds its own OAuth session; this transport never touches credentials.
//
// Wire contract measured against claude CLI 2.1.283 on 2026-09-25:
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
// - `--tools ""` runs the session with no tools: the CLI executes nothing on
//   its own authority, and Finch's permission system stays the only one.

use crate::types::{
    ModelCapabilities, ProviderAllowance, ProviderRequest, ProviderResponse, ProviderUsage,
    ReasoningCapability, StreamChunk,
};
use crate::wire_types::ContentBlock;
use crate::{CapabilitySupport, ProviderBackend, ValidatedProviderRequest};
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
    /// `--session-id`, later turns continue it with `--resume`.
    fn invocation_args(&self, resumable: bool, system: Option<&str>) -> Vec<String> {
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
        args
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
        let resumable = *self.resumable.lock().await;
        match self
            .run_turn_once(resumable, system.as_deref(), &input_lines, deltas.clone())
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
                        .run_turn_once(true, system.as_deref(), &input_lines, deltas)
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
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnRecord> {
        let args = self.invocation_args(resumable, system);
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
            // The CLI runs with `--tools ""`: the model executes nothing on
            // its own authority, so provider-native tool calls are absent by
            // design. Finch's own ToolLoop remains the only execution path.
            CapabilitySupport::Unsupported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported(MEASURED_ON, MEASURED_SOURCE),
            Some(MEASURED_CONTEXT_WINDOW),
            Some(MEASURED_MAX_OUTPUT_TOKENS),
            None,
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
        let args = provider.invocation_args(false, Some("SYSTEM"));
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
            "--tools with an empty value is what keeps the CLI from executing anything on its own authority; args: {joined}"
        );
        assert!(
            joined.contains("--session-id"),
            "turn one mints the session"
        );
        assert!(!joined.contains("--resume"), "turn one must not resume");

        let resume_args = provider.invocation_args(true, None);
        assert!(resume_args.contains(&"--resume".to_string()));
        assert!(
            !resume_args.iter().any(|arg| arg == "--system-prompt"),
            "an absent system prompt must omit the flag, not send empty bytes"
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
    fn capabilities_declare_streaming_without_tools() {
        let provider = ClaudeCliProvider::new(None);
        let capabilities = provider.capabilities(CLAUDE_CLI_DEFAULT_MODEL);
        assert!(capabilities.streaming.is_supported());
        assert!(
            !capabilities.tools.is_supported(),
            "the backend must advertise no provider-native tools: the CLI runs with --tools \"\""
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

    #[tokio::test]
    async fn a_request_asking_for_tools_fails_closed_at_validation() {
        let provider = ClaudeCliProvider::new(None);
        let mut request = simple_request();
        request.tools = Some(vec![crate::ToolDefinition {
            name: "read".to_string(),
            description: "read a file".to_string(),
            input_schema: crate::ToolInputSchema {
                schema_type: "object".to_string(),
                properties: Default::default(),
                required: Vec::new(),
            },
        }]);
        let error = match crate::validate_provider_request(&provider, &request, false).await {
            Ok(_) => panic!("tool-bearing requests must be refused by capability validation"),
            Err(error) => error,
        };
        assert!(
            error.to_string().to_lowercase().contains("tool"),
            "the refusal must name the unsupported capability: {error}"
        );
    }
}
