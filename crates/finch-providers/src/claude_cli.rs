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
//   `{"mcpServers":{"<name>":{"type":"stdio","command":...,"args":[...],
//   "env":{...}}}}`, `--strict-mcp-config` so the CLI ignores the user's own
//   personal MCP integrations). The MCP server is this same `finch` binary,
//   re-invoked with the hidden [`CLAUDE_CLI_MCP_BRIDGE_FLAG`]
//   (`src/cli/claude_cli_bridge.rs` in the root crate). `--allowedTools
//   "mcp__<server>__<tool>"` (bare name) is required per tool: verified
//   directly that an MCP tool call is otherwise denied by default with no
//   permission host available in `--print` mode, and that a bare-name entry
//   auto-approves the CLI's own MCP prompt with no further prompting on that
//   side — safe because the bridge never executes anything on its own
//   authority (issue #1341): it is a pure JSON-RPC-to-socket translator. A
//   `tools/call` is forwarded, line-delimited JSON, over a Unix domain socket
//   named in the server's `env` entry ([`CLAUDE_CLI_TOOL_SOCKET_ENV`]) to
//   *this* transport, which surfaces it as an ordinary
//   [`crate::StreamChunk::ToolCallComplete`] in the same stream every other
//   provider's tool calls already take, so it is executed by the frontend's
//   real, interactive `ToolLoop` — the same authority, same approval, same
//   file access as every other provider. The underlying `claude` child is
//   *parked* (kept alive, not re-spawned) while that real execution runs,
//   however long interactive approval takes; the next Finch-level round hands
//   the real result back over the same socket connection and the parked
//   child resumes.

use crate::types::{
    EventProvenance, ModelCapabilities, ProviderAllowance, ProviderRequest, ProviderResponse,
    ProviderUsage, ReasoningCapability, StreamChunk,
};
use crate::wire_types::ContentBlock;
use crate::{CapabilitySupport, ProviderBackend, ValidatedProviderRequest, WireProtocol};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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

/// Finch tool names this transport ever exposes to Claude Code. Every call to
/// one of them is forwarded by the MCP bridge to the frontend and executed by
/// Finch's real, interactive `ToolLoop` (issue #1341) — the CLI never runs
/// Bash, Read, Write, Edit, Grep, or Glob on its own authority (`--tools`
/// always stays `""`). A tool that exists in Finch but is not in this list is
/// simply never offered to this provider; the caller sees no tool-call
/// attempt for it, not a failure.
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

/// Environment variable the frontend sets on the MCP server entry in
/// `--mcp-config` (never on `claude`'s own environment) naming the Unix
/// domain socket the bridge process (`src/cli/claude_cli_bridge.rs`) must
/// connect to for every `tools/call` it receives (issue #1341). The bridge
/// no longer executes tools itself; this socket is how it hands a call to
/// the frontend process that actually owns interactive approval and real
/// file access, and gets a real result back.
pub const CLAUDE_CLI_TOOL_SOCKET_ENV: &str = "FINCH_CLAUDE_CLI_TOOL_SOCKET";

/// One `tools/call` the bridge subprocess forwards to the frontend over
/// [`CLAUDE_CLI_TOOL_SOCKET_ENV`], line-delimited JSON. `name` is already the
/// plain Finch tool name (the bridge resolves the MCP wire name itself before
/// forwarding, so the frontend never needs to know the MCP namespacing
/// convention).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeCliBridgeToolRequest {
    pub name: String,
    pub input: serde_json::Value,
}

/// The frontend's answer to a [`ClaudeCliBridgeToolRequest`], translated by
/// the bridge back into the MCP `tools/call` response for the waiting
/// `claude` CLI subprocess.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeCliBridgeToolResponse {
    pub is_error: bool,
    pub content: String,
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
    /// Concatenation of every `assistant` event's own text content seen so
    /// far this turn, in arrival order (issue #1331). A mid-turn tool call
    /// makes the real CLI emit more than one `assistant` event per turn — a
    /// preamble message (which may carry its own text alongside the
    /// `tool_use` block) followed by the final-answer message — and every
    /// `text_delta` from both is forwarded live as a `TextDelta` chunk
    /// (`stream_delta_text` has no per-message boundary tracking). This
    /// field must therefore accumulate rather than overwrite: it is what
    /// [`TurnRecord::response_text`] reports as the completed content, and
    /// it must equal the full streamed total or `query_processor.rs`'s
    /// streamed-vs-completed check fails the turn.
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
    /// Byte length of `assistant_text` already reported as a *prior*
    /// `execute_turn` call's own `ContentBlockComplete` (issue #1372's
    /// investigation). `assistant_text` accumulates across the *whole*
    /// Finch-level turn on purpose (issue #1331) — including a preamble
    /// that streamed through an earlier, now-closed `deltas` channel from a
    /// prior `send_message_stream` call, once a parked turn resumes in a
    /// new one. But each individual `execute_turn` call's own streaming
    /// consumer only ever sees `TextDelta`s that arrived *through that same
    /// call*, so reporting the whole-turn `assistant_text` as *this* call's
    /// completed content — rather than just the suffix generated during
    /// this call — desyncs `query_processor.rs`'s streamed-vs-completed
    /// check (which is scoped per call, not per Finch-level turn) even
    /// though nothing was actually lost or duplicated. Set once, right
    /// before resuming a parked turn's `pump_until_settled`, to the prefix
    /// length that call did *not* itself stream; `0` for a turn that never
    /// paused, so [`TurnRecord::newly_streamed_text`] equals
    /// [`TurnRecord::response_text`] there. See
    /// `resumed_round_reports_only_its_own_new_text_not_the_prior_rounds_preamble`.
    already_streamed_len: usize,
    /// True from the moment a `tool_use` block is observed in an `assistant`
    /// event until the next text is recorded (issue #1388). A `tool_use`
    /// genuinely bridges two text segments into one continuous reply
    /// (#1331's preamble-then-final-answer shape); its *absence* between two
    /// text-bearing `assistant` events means the CLI produced two
    /// independent, complete replies in the same continuous invocation (the
    /// reproduction: a confused first reply reacting to bare context,
    /// immediately followed by a second, real answer — no tool call ever
    /// appeared). `absorb_line` consults this to decide whether the next
    /// text segment continues the open reply (append) or starts a new one
    /// (append after [`INDEPENDENT_REPLY_SEPARATOR`]).
    tool_use_bridges_next_text: bool,
    /// True while a live `content_block_delta` run is already open for the
    /// current, not-yet-finalized `assistant` message (issue #1388).
    /// `stream_delta_text` carries no per-message boundary of its own, so
    /// this is what tells `absorb_line` "this is the first delta of a new
    /// message's run" — the one delta that may need
    /// [`INDEPENDENT_REPLY_SEPARATOR`] prefixed so the live `TextDelta`
    /// stream reported to `query_processor.rs` inserts the separator at
    /// exactly the same point [`TurnRecord::response_text`] does, keeping
    /// the streamed-vs-completed invariant intact. Reset to `false` the
    /// moment an `assistant` event finalizes that message.
    mid_delta_run: bool,
}

/// Inserted between two `assistant` text segments in the same continuous CLI
/// invocation when nothing (`tool_use`) bridges them, so two independent
/// complete replies (issue #1388) never fuse into one string with no
/// separator, mid-word. Never inserted for the #1331 preamble-then-final-
/// answer shape, where a `tool_use` genuinely bridges the two segments.
const INDEPENDENT_REPLY_SEPARATOR: &str = "\n\n";

/// Best-effort removes the bridge socket file on drop, so a paused-then-
/// abandoned turn never leaks a stale socket path on disk. A separate type
/// (rather than a `Drop` impl directly on [`RunningTurn`]) so `RunningTurn`'s
/// own fields — notably `record` and `stderr_task` — can still be moved out
/// of a completed turn; a type with its own `Drop` impl cannot be partially
/// moved out of.
#[cfg(unix)]
struct SocketGuard(PathBuf);

#[cfg(unix)]
impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One live `claude` child process plus everything needed to keep reading its
/// stdout across a pause for a real tool execution (issue #1341).
///
/// Unix-only (issue #1357): the bridge's `listener` is a Unix domain socket,
/// with no cross-platform equivalent wired up. The Claude CLI Subscription
/// provider is not supported on non-Unix platforms; see
/// [`ClaudeCliProvider`]'s non-Unix `ProviderBackend` methods for the runtime
/// error a caller gets instead.
#[cfg(unix)]
struct RunningTurn {
    child: Child,
    reader: BufReader<ChildStdout>,
    stderr_task: JoinHandle<String>,
    listener: UnixListener,
    /// Never read again after construction; retained solely so its `Drop`
    /// impl removes the socket file whenever this turn is dropped, whether
    /// completed, errored, or abandoned mid-pause.
    #[allow(dead_code)]
    socket_path: SocketGuard,
    record: TurnRecord,
    /// Monotonic counter so every tool call this `claude` process makes
    /// (across however many paused/resumed Finch-level rounds) gets a
    /// distinct sequence number in its `EventProvenance`.
    tool_call_sequence: u64,
}

/// A `claude` child process paused between Finch-level rounds while a real,
/// interactive tool execution runs in the frontend (issue #1341). Nothing
/// here spawns a new `claude` process just to answer one tool call: the same
/// process, its MCP bridge subprocess, and this transport's own bridge
/// listener socket stay exactly as they were the moment the bridge forwarded
/// the `tools/call`.
///
/// Unix-only (issue #1357): see [`RunningTurn`].
#[cfg(unix)]
struct ParkedTurn {
    running: RunningTurn,
    /// Finch-minted id for the specific call the bridge is still blocked on.
    /// The next Finch-level round must supply a matching `ToolResult`.
    pending_id: String,
    /// The bridge's own open connection, awaiting exactly one reply line.
    pending_reply: UnixStream,
}

/// The result of [`ClaudeCliProvider::parked_call_match`]: whether a request
/// correctly answers this session's currently parked tool call, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParkedCallMatch {
    /// No call is currently parked; any well-formed request may start (or
    /// continue) a fresh round.
    NoPendingCall,
    /// A call is parked and this request's tail correctly answers it.
    Matches,
    /// A call is parked, but this request's tail does not answer it — the id
    /// of the call actually waiting.
    Mismatch { pending_id: String },
}

/// What a completed [`ClaudeCliProvider::execute_turn`] produced.
enum TurnOutcome {
    /// The `claude` process exited; `record` is the finished turn.
    Complete(TurnRecord),
    /// The process is parked awaiting a real tool result; the stream ends
    /// here with no final content block, exactly like every other
    /// provider's stream after a native tool call.
    Paused,
}

/// What one [`drive`] pass produced.
///
/// Unix-only (issue #1357): see [`RunningTurn`].
#[cfg(unix)]
enum DriveOutcome {
    Exited(std::process::ExitStatus),
    ToolCallPending {
        name: String,
        input: serde_json::Value,
        reply: UnixStream,
    },
}

/// One queued typed-ahead round: the system prompt in effect when the user
/// typed it, and the text itself. See
/// [`ClaudeCliProvider::pending_followup`]/[`ClaudeCliProvider::execute_turn`]
/// (issue #1341).
type PendingFollowup = (Option<String>, String);

#[derive(Clone)]
pub struct ClaudeCliProvider {
    binary: PathBuf,
    model: String,
    session_id: uuid::Uuid,
    /// Set once a turn has completed successfully for this session id; later
    /// turns continue the conversation with `--resume`.
    resumable: Arc<Mutex<bool>>,
    /// A `claude` child process kept alive between Finch-level rounds while
    /// its own MCP bridge subprocess waits on a real, interactive tool
    /// execution result (issue #1341). `None` whenever no `claude` child is
    /// mid-tool-call. See [`RunningTurn`]/[`pump_until_settled`].
    ///
    /// Unix-only (issue #1357): the MCP bridge's transport is a Unix domain
    /// socket with no cross-platform equivalent wired up, so this field —
    /// and every turn-execution path that touches it — compiles out on
    /// non-Unix platforms. The Claude CLI Subscription provider is not
    /// supported there; see this type's non-Unix `ProviderBackend` methods.
    #[cfg(unix)]
    parked: Arc<Mutex<Option<ParkedTurn>>>,
    /// Text the user typed while a tool call was parked, queued (with the
    /// system prompt in effect when it was typed) because it could never
    /// reach that parked `claude` process directly — its stdin was already
    /// closed at spawn time. Flushed, in order, as soon as some round on
    /// this session next completes without pausing again (issue #1341's
    /// typed-ahead-during-a-parked-tool-call case). See `execute_turn`.
    pending_followup: Arc<Mutex<VecDeque<PendingFollowup>>>,
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
            #[cfg(unix)]
            parked: Arc::new(Mutex::new(None)),
            pending_followup: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    /// The session id every turn of this provider instance resumes.
    pub fn session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    /// Whether `request`'s tail correctly answers this session's currently
    /// parked tool call, if any — a non-destructive peek a caller can use to
    /// reject a mismatched round *before* calling `execute_turn` (via
    /// [`LlmProvider::send_message`]/`send_message_stream`), instead of
    /// discovering the mismatch only after `execute_turn`'s own
    /// [`Self::take_matching_parked_turn`] has already abandoned the parked
    /// turn — silently killing a live, possibly mid-human-approval `claude`
    /// child and starting an unrelated fresh one (issue #1341's original
    /// query-cancelled-or-retried case, where that is the *correct*
    /// behavior because the single frontend driving the turn made that
    /// decision itself). A caller that can be handed a request from a
    /// source that never saw the pending call — issue #1354's daemon-owned
    /// session, reachable by more than one frontend connection over its
    /// lifetime — must not let an uninformed request silently take that
    /// same path.
    #[cfg(unix)]
    pub async fn parked_call_match(&self, request: &ProviderRequest) -> ParkedCallMatch {
        let guard = self.parked.lock().await;
        let Some(parked) = guard.as_ref() else {
            return ParkedCallMatch::NoPendingCall;
        };
        match tail_tool_result(request, &parked.pending_id) {
            Some(_) => ParkedCallMatch::Matches,
            None => ParkedCallMatch::Mismatch {
                pending_id: parked.pending_id.clone(),
            },
        }
    }

    /// Unix-only (issue #1357): this platform never parks a turn (turn
    /// execution itself is unsupported here, see [`ProviderBackend`]'s
    /// non-Unix methods below), so no call is ever pending.
    #[cfg(not(unix))]
    pub async fn parked_call_match(&self, _request: &ProviderRequest) -> ParkedCallMatch {
        ParkedCallMatch::NoPendingCall
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
    ///
    /// `--restricted --permission-prompts none` (issue #1389, verified live
    /// against the real `claude` CLI 2.1.284 on 2026-09-28, not guessed from
    /// `--help` text alone): the repo owner's stated intent is that this
    /// subprocess is a scoped model-generation backend, never an independent
    /// agent with its own persistent state, skills, or interactive prompts.
    /// Without this, a completely ordinary chat message ("remember that")
    /// made the model call Finch's own MCP-bridged `write` tool — a real,
    /// legitimately-available tool, since `--tools ""` only turns off the
    /// CLI's *built-in* Read/Write/Edit — to persist a file shaped exactly
    /// like Claude Code's own auto-memory feature
    /// (`name`/`description`/`metadata.type` frontmatter) under
    /// `~/.claude/projects/<hashed-cwd>/memory/`, entirely outside Finch's
    /// own Brain/memory store, surfaced to the user as an ordinary Finch
    /// write-approval dialog with no indication it targets a different
    /// tool's storage. Reproduced directly: a fake MCP server standing in
    /// for the real bridge, advertising just a `write` tool exactly as
    /// `mcp_bridge_args` does, received two `tools/call` writes
    /// (`<memory-name>.md` then `MEMORY.md`) for "My favorite test constant
    /// is the number 8675309. Remember that." under the pre-#1389 flag set.
    ///
    /// Two flags considered and rejected — do not re-add either without new
    /// evidence:
    /// - `--bare`: its own help text says "Anthropic auth is strictly
    ///   ANTHROPIC_API_KEY or apiKeyHelper via --settings (OAuth and keychain
    ///   are never read)". This provider exists specifically to drive the
    ///   user's OAuth-based subscription login (`claude_oauth.rs`) — the
    ///   entire reason Finch shells out to the real `claude` binary instead
    ///   of calling the Anthropic API directly with a key. `--bare` breaks
    ///   this provider's reason for existing.
    /// - `--safe-mode`: looked like the right fit (disables CLAUDE.md,
    ///   skills, plugins, hooks, MCP servers, etc. while leaving "auth, model
    ///   selection, built-in tools and plugins, and permissions" alone per
    ///   its own help text) and *did* suppress the memory write in the same
    ///   live repro above. But it also disqualifies itself: the same live
    ///   repro, re-run with a real task for the MCP-bridged `write` tool
    ///   ("create a file at /tmp/... containing ..."), showed
    ///   `system/init`'s `mcp_servers` as `[]` — `--safe-mode` drops even an
    ///   *explicitly passed* `--mcp-config` server, not just ambient/settings
    ///   -discovered ones — and the model emitted a hallucinated
    ///   `<invoke name="Write">...` text block instead of a real MCP
    ///   `tool_use`, which nothing here can execute. That silently breaks
    ///   every real Finch tool call through this provider. Reproduced twice.
    ///
    /// `--restricted --permission-prompts none` is what survived: verified
    /// live, four consecutive runs, that the memory write never happens
    /// (`system/init`'s `mcp_servers` list stays healthy and no `write`
    /// `tool_use` appears), and separately verified live that a real,
    /// explicitly requested MCP tool call still round-trips correctly
    /// (`system/init` shows the `finch` server `connected`, the model emits
    /// a proper `tool_use`, and the fake bridge receives it) — under the
    /// literal OAuth subscription login already active on the verifying
    /// machine (no `ANTHROPIC_API_KEY` in the environment), so `--restricted`
    /// does not touch auth the way `--bare` does. `--permission-prompts
    /// none` additionally closes the "Edit in $EDITOR"-style terminal-hijack
    /// risk from the issue: anything that would still try to prompt a human
    /// outside Finch's own approval flow is denied automatically instead of
    /// ever reaching an interactive dialog, regardless of what triggers it.
    fn invocation_args(
        &self,
        resumable: bool,
        system: Option<&str>,
        tool_names: &[String],
        tool_socket_path: Option<&Path>,
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
            "--restricted".to_string(),
            "--permission-prompts".to_string(),
            "none".to_string(),
        ];
        args.extend(self.mcp_bridge_args(tool_names, tool_socket_path)?);
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
    /// [`CLAUDE_CLI_MCP_BRIDGE_FLAG`]. `tool_socket_path` (always `Some` when
    /// `tool_names` is non-empty) is embedded as an `env` entry on the server
    /// spec itself, never on `claude`'s own environment: it names the Unix
    /// domain socket [`CLAUDE_CLI_TOOL_SOCKET_ENV`] the bridge connects to for
    /// every `tools/call` (issue #1341) instead of executing anything itself.
    fn mcp_bridge_args(
        &self,
        tool_names: &[String],
        tool_socket_path: Option<&Path>,
    ) -> Result<Vec<String>> {
        if tool_names.is_empty() {
            return Ok(Vec::new());
        }
        let socket_path = tool_socket_path
            .context("issue #1341: a tool-serving turn always binds a bridge socket first")?;
        let exe = std::env::current_exe()
            .context("resolving Finch's own executable path for the Claude Code MCP bridge")?;
        let mcp_config = json!({
            "mcpServers": {
                CLAUDE_CLI_MCP_SERVER_NAME: {
                    "type": "stdio",
                    "command": exe.to_string_lossy(),
                    "args": [CLAUDE_CLI_MCP_BRIDGE_FLAG],
                    "env": {
                        CLAUDE_CLI_TOOL_SOCKET_ENV: socket_path.to_string_lossy(),
                    },
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

    /// If a parked turn is waiting on exactly this request's trailing tool
    /// result, takes it out of `self.parked` and returns it with that
    /// result's content/error flag. `None` when there is no parked turn, or
    /// the request's tail does not match its pending call — in which case
    /// the stale parked turn (if any) is dropped right here (its child
    /// killed via `kill_on_drop`, its socket file removed by `RunningTurn`'s
    /// `Drop`) rather than left to hang or silently double-park later
    /// (issue #1341's cancel/retry hostile-timing case).
    #[cfg(unix)]
    async fn take_matching_parked_turn(
        &self,
        request: &ProviderRequest,
    ) -> Option<(ParkedTurn, String, bool, Vec<String>)> {
        let mut guard = self.parked.lock().await;
        let parked = guard.take()?;
        match tail_tool_result(request, &parked.pending_id) {
            Some((content, is_error, extra_text)) => Some((parked, content, is_error, extra_text)),
            None => {
                tracing::warn!(
                    pending_id = %parked.pending_id,
                    "issue #1341: abandoning a parked claude CLI turn whose pending tool call \
                     was never answered by the next round (query cancelled or retried)"
                );
                None
            }
        }
    }

    #[cfg(unix)]
    async fn execute_turn(
        &self,
        request: &ProviderRequest,
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnOutcome> {
        let mut outcome = if let Some((parked, content, is_error, extra_text)) =
            self.take_matching_parked_turn(request).await
        {
            write_bridge_response(
                parked.pending_reply,
                &ClaudeCliBridgeToolResponse { is_error, content },
            )
            .await?;
            if !extra_text.is_empty() {
                // The user typed ahead while this tool call was pending
                // (`ConversationHistory::append_text_blocks_to_last_user_message`
                // folds it into this same trailing message rather than a new
                // one). It can never reach *this* `claude` process directly —
                // its stdin was already written and closed at spawn time,
                // long before this tool call happened — so it is queued to
                // become its own follow-up round the moment some round on
                // this session next completes without pausing again (issue
                // #1341). Silently dropping it here, or treating the round as
                // an unmatched/abandoned parked turn because the trailing
                // message no longer looks like a bare `ToolResult`, would
                // discard a real, already-approved tool result and kill a
                // live, healthy `claude` process for no reason.
                self.pending_followup
                    .lock()
                    .await
                    .push_back((extract_system_prompt(request), extra_text.join("\n")));
            }
            // This call's own `deltas` channel is brand new — it never
            // carried whatever text already streamed through the *prior*
            // `execute_turn` call that parked this turn in the first place.
            // Snapshot that prefix now, before `pump_until_settled` mutates
            // `record` further, so the eventual `ContentBlockComplete` this
            // call reports can be scoped to only what it itself streamed
            // (see `TurnRecord::newly_streamed_text`, issue #1372).
            let mut running = parked.running;
            running.record.already_streamed_len = running.record.response_text().len();
            self.pump_until_settled(running, deltas.clone()).await?
        } else {
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
                Ok(outcome) => outcome,
                Err(error) => {
                    // A session id only becomes resumable once the CLI has
                    // persisted it. A first turn that failed before persisting
                    // must retry as a fresh session, while a mid-conversation
                    // failure may have left the id already stored. The CLI's
                    // "already in use" exit names the latter exactly.
                    if !resumable && error.to_string().contains("already in use") {
                        self.run_turn_once(
                            true,
                            system.as_deref(),
                            &input_lines,
                            &tool_names,
                            deltas.clone(),
                        )
                        .await?
                    } else {
                        return Err(error);
                    }
                }
            }
        };

        // Drain any queued typed-ahead text the moment a round completes
        // cleanly, regardless of whether *this* call reached completion via
        // the parked-resume branch above or an ordinary fresh spawn — a
        // round can complete cleanly right after a second, nested tool call
        // resolves too, and the queue must still flush then. Each iteration
        // spawns its own `claude` process for its own `--resume` round, but
        // from `query_processor.rs`'s perspective this is still exactly one
        // Finch-level round: every one of these chained invocations' own
        // `text_delta`s already streamed live through the same `deltas` sink,
        // so `combined` accumulates their completed content the same way
        // `TurnRecord::assistant_text` already accumulates multiple
        // `assistant` events *within* one invocation (issue #1331) — losing
        // an earlier invocation's text here would desync exactly the
        // streamed-vs-completed check that fix exists to satisfy.
        let mut combined: Option<TurnRecord> = None;
        loop {
            let record = match outcome {
                TurnOutcome::Complete(record) => record,
                TurnOutcome::Paused => return Ok(TurnOutcome::Paused),
            };
            self.mark_resumable().await;
            let record = match combined.take() {
                Some(mut acc) => {
                    acc.absorb_followup(record);
                    acc
                }
                None => record,
            };
            let Some((system, text)) = self.pending_followup.lock().await.pop_front() else {
                return Ok(TurnOutcome::Complete(record));
            };
            combined = Some(record);
            // Deliberately offers no tools: this round exists only to
            // deliver text the user already typed, on a brand-new `claude`
            // process (the parked one already exited), and threading the
            // original turn's tool catalog through here is unneeded
            // complexity for what is already a rare, secondary path — the
            // model can still ask for a tool in its own reply, which
            // surfaces as an ordinary next round with tools offered again,
            // same as any other turn.
            let wire_line = user_input_line(&text)?;
            outcome = self
                .run_turn_once(true, system.as_deref(), &[wire_line], &[], deltas.clone())
                .await?;
        }
    }

    #[cfg(unix)]
    async fn mark_resumable(&self) {
        *self.resumable.lock().await = true;
    }

    /// Bind a fresh, uniquely-named Unix domain socket for the bridge
    /// subprocess this `claude` invocation will spawn (issue #1341). Bound
    /// unconditionally (even for a tool-less turn, where nothing will ever
    /// connect) so [`RunningTurn`]'s shape and [`drive`]'s control flow stay
    /// uniform; the cost is one idle listener and one socket file cleaned up
    /// by `RunningTurn`'s `Drop`.
    #[cfg(unix)]
    async fn bind_tool_socket(&self) -> Result<(UnixListener, PathBuf)> {
        // `sockaddr_un.sun_path` is a fixed, short buffer (104 bytes on
        // macOS/BSD, 108 on Linux) — `bind` fails outright past that, and
        // `std::env::temp_dir()` (Finch's per-user `$TMPDIR` on macOS) is
        // frequently long enough on its own to blow the whole budget once any
        // filename is appended. `/tmp` is short on every target this project
        // ships for and is the standard workaround for exactly this limit;
        // the filename itself stays short (one hex-simple uuid) for the same
        // reason, deliberately not the longer hyphenated form or the session
        // id.
        let path =
            PathBuf::from("/tmp").join(format!("fcb-{}.sock", uuid::Uuid::new_v4().simple()));
        let listener = UnixListener::bind(&path).with_context(|| {
            format!("binding Claude CLI MCP bridge socket at {}", path.display())
        })?;
        // This socket carries real `tools/call` requests that get real,
        // interactive execution authority once accepted — `read_bridge_request`
        // trusts any well-formed request on any accepted connection, and does
        // not verify the peer is actually the bridge subprocess `claude`
        // spawned. `/tmp` is world-listable and a live listener's default mode
        // is world-connectable (verified: 0o755 under a standard 0o022 umask),
        // so an unhardened socket here would let *any* local process on the
        // machine act as the bridge for the lifetime of this turn — able to
        // read (at minimum) any workspace file the auto-approved
        // `ExecutionEffect::WorkspaceRead` tier permits with no human
        // approval. Owner-only mode closes that hole the same way
        // `src/server/ipc.rs`'s `harden_ipc_socket_permissions` already does
        // for the structurally identical unauthenticated local-socket pattern
        // (issue #911's rationale applies verbatim here).
        harden_bridge_socket_permissions(&path)?;
        Ok((listener, path))
    }

    /// Spawn a brand-new `claude` child process for a fresh Finch-level turn.
    /// Never used to answer a pending tool call on an already-running
    /// process — see [`Self::execute_turn`]'s parked-turn branch for that.
    #[cfg(unix)]
    async fn spawn_running_turn(
        &self,
        resumable: bool,
        system: Option<&str>,
        input_lines: &[String],
        tool_names: &[String],
    ) -> Result<RunningTurn> {
        let (listener, socket_path) = self.bind_tool_socket().await?;
        let args = self.invocation_args(resumable, system, tool_names, Some(&socket_path))?;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = spawn_retrying_text_file_busy(&mut command)
            .await
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

        Ok(RunningTurn {
            child,
            reader: BufReader::new(stdout),
            stderr_task,
            listener,
            socket_path: SocketGuard(socket_path),
            record: TurnRecord::default(),
            tool_call_sequence: 0,
        })
    }

    /// Spawn a fresh `claude` process and drive it to either completion or a
    /// paused tool call. The "already in use" retry in [`Self::execute_turn`]
    /// wraps this whole spawn-and-drive sequence, matching the pre-#1341
    /// behavior of retrying the entire turn, not just the drive loop.
    #[cfg(unix)]
    async fn run_turn_once(
        &self,
        resumable: bool,
        system: Option<&str>,
        input_lines: &[String],
        tool_names: &[String],
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnOutcome> {
        let running = self
            .spawn_running_turn(resumable, system, input_lines, tool_names)
            .await?;
        self.pump_until_settled(running, deltas).await
    }

    /// Drive a (possibly just-resumed) `claude` process until it either
    /// exits (`TurnOutcome::Complete`) or makes another `tools/call` that
    /// needs real, interactive execution (`TurnOutcome::Paused`, with the
    /// process parked in `self.parked`) — issue #1341.
    #[cfg(unix)]
    async fn pump_until_settled(
        &self,
        mut running: RunningTurn,
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<TurnOutcome> {
        loop {
            match drive(&mut running, deltas.as_ref()).await? {
                DriveOutcome::Exited(status) => {
                    let stderr_text = running.stderr_task.await.unwrap_or_default();
                    if !status.success() {
                        bail!(
                            "claude CLI exited with {status}; stderr: {}",
                            bounded_text(&stderr_text)
                        );
                    }
                    running.record.validated(self.session_id)?;
                    return Ok(TurnOutcome::Complete(running.record));
                }
                DriveOutcome::ToolCallPending { name, input, reply } => {
                    running.tool_call_sequence += 1;
                    let Some(deltas) = deltas.as_ref() else {
                        // No streaming sink: there is no interactive host to
                        // route real approval through for this call (the
                        // non-streaming `send_message` path). Answer the
                        // bridge with a clear, actionable error and keep
                        // driving the same `claude` process — it decides how
                        // to continue on its own, exactly as it would for
                        // any other tool error. This never falls back to
                        // local execution.
                        write_bridge_response(
                            reply,
                            &ClaudeCliBridgeToolResponse {
                                is_error: true,
                                content: "Finch cannot service this tool call: the Claude CLI \
                                          subscription transport was invoked without a \
                                          streaming sink, so no interactive approval host is \
                                          available (issue #1341)."
                                    .to_string(),
                            },
                        )
                        .await?;
                        continue;
                    };
                    let id = uuid::Uuid::new_v4().to_string();
                    let provenance = EventProvenance {
                        provider: CLAUDE_CLI_PROVIDER_NAME.to_string(),
                        model: self.model.clone(),
                        event: "tool_call".to_string(),
                        sequence: running.tool_call_sequence,
                        opaque_replay: None,
                    };
                    deltas
                        .send(Ok(StreamChunk::ToolCallComplete {
                            id: id.clone(),
                            name,
                            input,
                            provenance,
                        }))
                        .await
                        .map_err(|error| anyhow!("claude CLI stream sink closed: {error}"))?;
                    *self.parked.lock().await = Some(ParkedTurn {
                        running,
                        pending_id: id,
                        pending_reply: reply,
                    });
                    return Ok(TurnOutcome::Paused);
                }
            }
        }
    }

    #[cfg(unix)]
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

/// How many times to re-attempt a `claude` spawn the kernel refuses with
/// `ETXTBSY`, and how long to wait between attempts.
///
/// Mirrors `src/tools/diagnostics/mod.rs`'s identical `spawn_retrying_text_file_busy`
/// (issue #1204, itself mirroring `finch_runtime::host`'s `process-run` retry,
/// issue #287): the kernel refuses to exec a file any process holds open for
/// writing, and `fork()` copies the *entire* file descriptor table — so a
/// spawn anywhere else in the same test process that forks while any thread
/// still holds a write descriptor on the fake `claude` fixture this
/// transport's own tests write hands its child an inherited copy of that
/// descriptor, and *this* exec is refused even though the real writer
/// already closed its own copy. `cargo test`'s default parallelism runs
/// many `#[tokio::test]` functions concurrently in one process, and this
/// module both writes fresh executable fixtures (`install_fake_claude`) and
/// spawns them, repeatedly, across many tests (issue #1354 added several
/// more) — exactly the combination that behavior makes racy. The condition
/// is transient and self-clearing: retrying the specific, documented,
/// self-clearing error is the fix, not reordering this module's own
/// write/chmod/spawn sequence, which never has a gap in it — the refusal
/// comes from *outside* this module.
///
/// The loop breaks before sleeping on its final attempt, so eight attempts
/// means seven waits: `5ms * (1 + 2 + ... + 7)` = 140ms of sleep as a floor.
#[cfg(unix)]
const TEXT_FILE_BUSY_ATTEMPTS: u32 = 8;

#[cfg(unix)]
const TEXT_FILE_BUSY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(5);

/// Spawn `command`, re-attempting while the kernel reports `ETXTBSY`. See
/// [`TEXT_FILE_BUSY_ATTEMPTS`] for why this condition is transient and safe
/// to retry: every attempt spawns the exact command the caller already
/// built, so a retry has no path to running anything other than what was
/// already going to run.
#[cfg(unix)]
async fn spawn_retrying_text_file_busy(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    for attempt in 1..=TEXT_FILE_BUSY_ATTEMPTS {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(nix::libc::ETXTBSY) => {
                #[cfg(test)]
                tests::record_text_file_busy_refusal();
                tracing::debug!(
                    attempt,
                    "claude CLI exec refused with ETXTBSY; a descriptor still holds it open \
                     for writing, retrying"
                );
                if attempt == TEXT_FILE_BUSY_ATTEMPTS {
                    break;
                }
                tokio::time::sleep(TEXT_FILE_BUSY_BACKOFF * attempt).await;
            }
            Err(error) => return Err(error),
        }
    }
    tracing::warn!(
        attempts = TEXT_FILE_BUSY_ATTEMPTS,
        "claude CLI exec refused with ETXTBSY on every attempt; giving up"
    );
    command.spawn()
}

/// ETXTBSY is a POSIX exec-time refusal; platforms without fork/exec
/// process spawning cannot hit it, so there is nothing to retry.
#[cfg(not(unix))]
async fn spawn_retrying_text_file_busy(
    command: &mut tokio::process::Command,
) -> std::io::Result<tokio::process::Child> {
    command.spawn()
}

/// Read `claude`'s stdout, racing it against the bridge's own listener
/// socket, until either the process exits or a `tools/call` arrives that
/// needs real execution (issue #1341). `deltas` is only consulted for text
/// output; the caller (`pump_until_settled`) decides what to do with a
/// pending tool call, including the no-streaming-sink fallback.
#[cfg(unix)]
async fn drive(
    turn: &mut RunningTurn,
    deltas: Option<&mpsc::Sender<Result<StreamChunk>>>,
) -> Result<DriveOutcome> {
    let mut line = String::new();
    loop {
        line.clear();
        tokio::select! {
            read_result = turn.reader.read_line(&mut line) => {
                let read = read_result.context("reading claude CLI stdout")?;
                if read == 0 {
                    let status = turn
                        .child
                        .wait()
                        .await
                        .context("waiting for claude CLI to exit")?;
                    return Ok(DriveOutcome::Exited(status));
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(delta) = turn.record.absorb_line(trimmed)? {
                    if let Some(deltas) = deltas {
                        deltas
                            .send(Ok(StreamChunk::TextDelta(delta)))
                            .await
                            .map_err(|error| anyhow!("claude CLI stream sink closed: {error}"))?;
                    }
                }
            }
            accept_result = turn.listener.accept() => {
                let (stream, _addr) = accept_result
                    .context("accepting Claude CLI MCP bridge tool-call connection")?;
                let (request, reply) = read_bridge_request(stream).await?;
                return Ok(DriveOutcome::ToolCallPending {
                    name: request.name,
                    input: request.input,
                    reply,
                });
            }
        }
    }
}

/// Read one line-delimited [`ClaudeCliBridgeToolRequest`] from a freshly
/// accepted bridge connection, returning the connection back so the reply
/// can be written to it later (potentially much later — real interactive
/// approval has no timeout).
#[cfg(unix)]
async fn read_bridge_request(
    mut stream: UnixStream,
) -> Result<(ClaudeCliBridgeToolRequest, UnixStream)> {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        let read = reader
            .read_line(&mut line)
            .await
            .context("reading MCP bridge tool-call request")?;
        if read == 0 {
            bail!("MCP bridge closed its tool-call connection before sending a request");
        }
    }
    let request = serde_json::from_str(line.trim()).with_context(|| {
        format!(
            "malformed MCP bridge tool-call request: {}",
            bounded_text(line.trim())
        )
    })?;
    Ok((request, stream))
}

/// Write one line-delimited [`ClaudeCliBridgeToolResponse`] back to the
/// bridge's still-open connection.
#[cfg(unix)]
async fn write_bridge_response(
    mut stream: UnixStream,
    response: &ClaudeCliBridgeToolResponse,
) -> Result<()> {
    let mut payload =
        serde_json::to_string(response).context("encode MCP bridge tool-call response")?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .await
        .context("writing MCP bridge tool-call response")?;
    stream
        .flush()
        .await
        .context("flushing MCP bridge tool-call response")?;
    Ok(())
}

/// Owner-only mode for the bridge tool-call socket, matching
/// `src/server/ipc.rs`'s `IPC_SOCKET_MODE` for the structurally identical
/// unauthenticated local-socket pattern (issue #911's rationale, applied here
/// for issue #1341).
#[cfg(unix)]
const BRIDGE_SOCKET_MODE: u32 = 0o600;

/// Restrict the freshly bound bridge socket to the owning user. See
/// [`ClaudeCliProvider::bind_tool_socket`]'s call site for why this is
/// mandatory, not defense-in-depth: without it, any local process can
/// connect and be answered as if it were the real bridge.
#[cfg(unix)]
fn harden_bridge_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(BRIDGE_SOCKET_MODE))
        .with_context(|| {
            format!(
                "hardening Claude CLI MCP bridge socket at {}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn harden_bridge_socket_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Whether `request`'s trailing message contains a `ToolResult` matching
/// `pending_id` — the shape `finalize_tool_execution` produces for the very
/// next round after a tool call, and the only shape that may resume a parked
/// turn (issue #1341). Any other `ContentBlock::Text` in that same message is
/// text the user typed while the tool call was pending
/// (`ConversationHistory::append_text_blocks_to_last_user_message` folds it
/// into this same message rather than a new one) and is returned separately
/// so a caller can queue it instead of silently discarding it. A block that
/// is neither `ToolResult` nor `Text`, or a second, mismatched `ToolResult`,
/// is not a shape this function recognizes — it fails closed (`None`) rather
/// than guess.
fn tail_tool_result(
    request: &ProviderRequest,
    pending_id: &str,
) -> Option<(String, bool, Vec<String>)> {
    let last = request.messages.last()?;
    if last.role != "user" || last.content.is_empty() {
        return None;
    }
    let mut matched = None;
    let mut extra_text = Vec::new();
    for block in &last.content {
        match block {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                if matched.is_some() || tool_use_id != pending_id {
                    return None;
                }
                matched = Some((content.clone(), is_error.unwrap_or(false)));
            }
            ContentBlock::Text { text } => {
                if !text.trim().is_empty() {
                    extra_text.push(text.clone());
                }
            }
            _ => return None,
        }
    }
    let (content, is_error) = matched?;
    Some((content, is_error, extra_text))
}

/// System-role content from a request, independent of
/// [`ClaudeCliProvider::split_request`]'s stricter "must have a pending user
/// turn to send" invariant — needed for a synthetic follow-up round (issue
/// #1341's typed-ahead-during-a-parked-tool-call case) built from queued text
/// alone, where that invariant does not apply.
fn extract_system_prompt(request: &ProviderRequest) -> Option<String> {
    let mut parts = Vec::new();
    for message in &request.messages {
        if message.role == "system" {
            parts.extend(
                message
                    .content
                    .iter()
                    .filter_map(|block| block.as_text().map(str::to_string)),
            );
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

impl TurnRecord {
    /// Merge a chained follow-up invocation's record into this one (issue
    /// #1341: typed-ahead text queued during a parked tool call is delivered
    /// as its own `claude` process, but the caller sees exactly one
    /// Finch-level round). `assistant_text` accumulates for the same reason
    /// it already accumulates across multiple `assistant` events within one
    /// invocation (issue #1331): every `text_delta` that streamed live must
    /// still be reflected in what `response_text()` reports as complete.
    /// Both invocations share one `--resume` session, so `session_confirmed`
    /// is left as the first invocation's (already validated); other fields
    /// take the follow-up's value when it has one, since it is the more
    /// recent state.
    fn absorb_followup(&mut self, next: TurnRecord) {
        self.assistant_text.push_str(&next.assistant_text);
        if next.assistant_model.is_some() {
            self.assistant_model = next.assistant_model;
        }
        if next.assistant_message_id.is_some() {
            self.assistant_message_id = next.assistant_message_id;
        }
        if next.usage.is_some() {
            self.usage = next.usage;
        }
        if next.allowance.is_some() {
            self.allowance = next.allowance;
        }
        if next.result_text.is_some() {
            self.result_text = next.result_text;
        }
        self.result_error = next.result_error;
        self.tool_calls_observed += next.tool_calls_observed;
    }

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
                    // This message's own delta run (if any) is now
                    // consolidated by this event; any further deltas start a
                    // new message's run (issue #1388).
                    self.mid_delta_run = false;
                    // Append, never overwrite (issue #1331): a preamble
                    // message's text must survive into the completed
                    // content even though a later `assistant` event (the
                    // final answer, possibly after a mid-turn tool_use in
                    // this same message) arrives afterward. See the field
                    // doc comment on `assistant_text`. But append *blindly*
                    // only when a `tool_use` actually bridges this text to
                    // whatever text came before it — two independent,
                    // complete replies with nothing bridging them (issue
                    // #1388) get `INDEPENDENT_REPLY_SEPARATOR` between them
                    // instead of fusing mid-word.
                    let message_text = content
                        .iter()
                        .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("");
                    let has_tool_use = content.iter().any(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    });
                    if !message_text.is_empty() {
                        if !self.assistant_text.is_empty() && !self.tool_use_bridges_next_text {
                            self.assistant_text.push_str(INDEPENDENT_REPLY_SEPARATOR);
                        }
                        self.assistant_text.push_str(&message_text);
                        self.tool_use_bridges_next_text = false;
                    }
                    // Re-derived for issue #1341 (previously: "observability
                    // only, because the bridge already executed this for
                    // real" — no longer true, the bridge never executes
                    // anything now). This function still emits no
                    // `StreamChunk` for a `tool_use` block: the *real*
                    // `ToolCallComplete` for this call is emitted by
                    // `pump_until_settled`/`drive`, driven by the bridge's own
                    // forwarded socket request, not by parsing this line. The
                    // two are deliberately decoupled — this stdout line and
                    // the bridge's MCP request are two independent views of
                    // the same event delivered over two different pipes, with
                    // no guaranteed ordering between them — so correlating
                    // them here would be guesswork. Emitting a *second*
                    // `ToolCallComplete` from this stdout-observation path
                    // would violate "dual encoding of the same id+input is
                    // one call at the ToolLoop" (`finch-providers`'
                    // `AGENTS.md`) by handing the ToolLoop two different ids
                    // for what a human sees as one call. The counter below
                    // remains observability/debug-logging only.
                    if has_tool_use {
                        self.tool_use_bridges_next_text = true;
                    }
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
            Some("stream_event") => {
                let Some(delta) = stream_delta_text(&event) else {
                    return Ok(None);
                };
                // The first delta of a message's run needs
                // `INDEPENDENT_REPLY_SEPARATOR` prefixed under the same
                // condition `absorb_line`'s `assistant` handling uses (issue
                // #1388), so the live `TextDelta` stream this returns stays
                // byte-for-byte in sync with `TurnRecord::response_text` —
                // both must apply the separator at the same point or
                // `query_processor.rs`'s streamed-vs-completed check fails
                // the turn. Every later delta in the same run is a plain
                // continuation of text already prefixed (or not).
                if !self.mid_delta_run {
                    self.mid_delta_run = true;
                    if !self.assistant_text.is_empty() && !self.tool_use_bridges_next_text {
                        return Ok(Some(format!("{INDEPENDENT_REPLY_SEPARATOR}{delta}")));
                    }
                }
                return Ok(Some(delta));
            }
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
        // `assistant_text` (issue #1331) is the accumulation of every
        // `assistant` event's own text across the whole turn, so it is what
        // the live `TextDelta` stream actually adds up to; it must win
        // whenever any assistant message carried text. `result_text` (the
        // terminal `result` event's own `result` field) is a fallback for
        // the case where no assistant message carried text content at all —
        // using it in preference to a non-empty `assistant_text` would
        // silently drop a preamble segment again and reintroduce the
        // streamed-vs-completed mismatch this field exists to prevent.
        if !self.assistant_text.is_empty() {
            return self.assistant_text.clone();
        }
        self.result_text.clone().unwrap_or_default()
    }

    /// What *this specific* `execute_turn` call actually streamed as
    /// `TextDelta` chunks through its own `deltas` channel — the suffix of
    /// [`TurnRecord::response_text`] beyond `already_streamed_len` (issue
    /// #1372's investigation). A turn that never paused has
    /// `already_streamed_len == 0`, so this equals `response_text()`
    /// exactly. Use this, not `response_text()`, when building a streaming
    /// call's own `ContentBlockComplete` — `query_processor.rs`'s
    /// streamed-vs-completed check is scoped to one call, not one
    /// Finch-level turn.
    fn newly_streamed_text(&self) -> String {
        let text = self.response_text();
        // `already_streamed_len` is always a byte length previously read
        // off this exact string (a strict, append-only prefix — see the
        // field's own doc comment) via `.len()`, so it always lands on a
        // valid UTF-8 boundary here; `.min()` is defensive only, in case a
        // future change to this invariant is ever introduced by mistake.
        let boundary = self.already_streamed_len.min(text.len());
        text[boundary..].to_string()
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

/// Issue #1357: the Claude CLI Subscription provider's MCP tool-call bridge
/// is a Unix domain socket with no cross-platform equivalent wired up, so
/// turn execution is unsupported outside Unix. Provider selection, the
/// daemon-owned session registry, and the daemon RPC surface all still
/// construct and store [`ClaudeCliProvider`] unconditionally on every
/// platform (issue #1354 grew that surface further) — this error is the
/// single, clear failure point a caller reaches at first actual use instead
/// of a separate platform check at every one of those call sites.
#[cfg(not(unix))]
const NOT_SUPPORTED_ON_THIS_PLATFORM: &str = "Claude CLI Subscription provider is not supported \
     on this platform: its MCP tool-call bridge requires a Unix domain socket (issue #1357)";

#[async_trait::async_trait]
impl ProviderBackend for ClaudeCliProvider {
    #[cfg(unix)]
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        match self.execute_turn(&request, None).await? {
            TurnOutcome::Complete(record) => Ok(self.response_from(&record, &request.model)),
            TurnOutcome::Paused => bail!(
                "claude CLI subscription transport paused mid-turn with no streaming sink; \
                 execute_turn's non-streaming branch must always answer a pending tool call \
                 itself rather than pausing (issue #1341 — this indicates a bug, not a \
                 recoverable runtime condition)"
            ),
        }
    }

    #[cfg(not(unix))]
    async fn send_message_validated(
        &self,
        _request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        bail!(NOT_SUPPORTED_ON_THIS_PLATFORM)
    }

    #[cfg(unix)]
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
                Ok(TurnOutcome::Complete(record)) => {
                    // `newly_streamed_text`, not `response_text` — this
                    // call's own `deltas`/`tx` only ever carried what
                    // streamed through *this* `execute_turn` invocation;
                    // `response_text()` can include an earlier call's
                    // preamble on a resumed parked turn, which would desync
                    // `query_processor.rs`'s streamed-vs-completed check
                    // (issue #1372's investigation).
                    //
                    // The usage the CLI reported goes first: the
                    // non-streaming path already returns it
                    // (`response_from`), but this path dropped it, so a
                    // whole session read as zero input tokens (#1671).
                    if let Some(usage) = &record.usage {
                        let _ = tx
                            .send(Ok(StreamChunk::Usage {
                                input_tokens: usage.input_tokens,
                                output_tokens: usage.output_tokens,
                            }))
                            .await;
                    }
                    let _ = tx
                        .send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::text(
                            record.newly_streamed_text(),
                        ))))
                        .await;
                }
                Ok(TurnOutcome::Paused) => {
                    // The stream ends here, exactly like every other
                    // provider's stream after a native tool call: the
                    // frontend's real ToolLoop executes the pending call and
                    // the next Finch-level round resumes this same `claude`
                    // process (issue #1341).
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                }
            }
        });
        Ok(rx)
    }

    #[cfg(not(unix))]
    async fn send_message_stream_validated(
        &self,
        _request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        bail!(NOT_SUPPORTED_ON_THIS_PLATFORM)
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
            // Support here means Finch's own tools, served over MCP, forwarded
            // by the bridge, and executed by Finch's real interactive
            // `ToolLoop` in the frontend (issue #1341); the prompt-injection
            // fold in `src/generators/claude.rs` is bypassed for this
            // provider accordingly.
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

    /// Count of real `ETXTBSY` refusals `spawn_retrying_text_file_busy` has
    /// observed, process-wide, so a deterministic test can wait for a
    /// genuine kernel refusal instead of a fixed sleep (a fixed sleep would
    /// make the test vacuous on a loaded runner if the held descriptor
    /// happened to close before the first spawn attempt). Mirrors
    /// `src/tools/diagnostics/mod.rs`'s identical counter for the same
    /// reason (issue #1204); unlike that module, this transport spawns
    /// only one executable (`self.binary`) per provider instance, so a
    /// single process-wide counter (not one keyed per executable path)
    /// is enough to disambiguate this test's own refusals.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    static TEXT_FILE_BUSY_REFUSALS: std::sync::OnceLock<std::sync::atomic::AtomicU32> =
        std::sync::OnceLock::new();

    pub(super) fn record_text_file_busy_refusal() {
        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "dragonfly"
        ))]
        TEXT_FILE_BUSY_REFUSALS
            .get_or_init(|| std::sync::atomic::AtomicU32::new(0))
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Only the deterministic reproduction test below reads this, and that
    /// test is itself restricted to the platforms where a shebang script's
    /// own exec is actually subject to the kernel's deny-write check
    /// (verified directly by `src/tools/diagnostics/mod.rs`: macOS is not
    /// one of them, and this transport's fake `claude` fixture is the same
    /// kind of shebang script). Matching that restriction here, rather than
    /// the broader `cfg(unix)` production retry uses, keeps this getter
    /// from going unused on macOS.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    fn text_file_busy_refusals() -> u32 {
        TEXT_FILE_BUSY_REFUSALS
            .get_or_init(|| std::sync::atomic::AtomicU32::new(0))
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Install a fake `claude` executable that records its argv and stdin and
    /// emits canned NDJSON matching the measured wire contract. Behavior is
    /// selected by the first argument word: default is a successful turn;
    /// `fail-in-use` fails with the CLI's id-reuse error when invoked with
    /// `--session-id` and succeeds when invoked with `--resume`; `exit-fail`
    /// exits nonzero after one init line; `tool-preamble` reproduces the
    /// real CLI 2.1.283 shape from issue #1331: a first `assistant` message
    /// carrying both a preamble text block and a `tool_use` block, then a
    /// second `assistant` message with the final answer, with every
    /// segment's `text_delta`s streamed first as usual. `mcp-tool-call`
    /// (issue #1341) extracts the bridge socket path this transport embedded
    /// in `--mcp-config`'s `env` entry into `$SPOOL/socket_path`, emits a
    /// `tool_use` block, then blocks (stdout stays open, no EOF) until the
    /// test creates `$SPOOL/proceed` — giving the test a window to connect to
    /// that socket directly and play the bridge's role itself, exercising
    /// this transport's real pause/resume state machine without needing the
    /// real bridge subprocess (which has its own tests). The *same* `claude`
    /// binary also answers a later invocation carrying no `--mcp-config`
    /// (i.e. `execute_turn`'s synthetic typed-ahead follow-up round, issue
    /// #1341) by echoing back the text it actually received on stdin,
    /// instead of repeating the tool_use dance. `mcp-two-tool-calls` (issue
    /// #1341, independent review) is the same pause/resume dance run twice
    /// on one process: it asks for a second tool only after the first one's
    /// real result comes back over the same still-live socket/listener --
    /// the *sequential* multi-tool-call case. See issue #1351 for what this
    /// deliberately does not cover (whether the real CLI ever pipelines two
    /// `tools/call` requests before reading the first reply).
    fn install_fake_claude(home: &TempDir, behavior: &str) -> PathBuf {
        let spool = spool(home);
        let bin = home.path().join("fake-claude");
        let script = format!(
            r#"#!/bin/bash
SPOOL="{spool}"
MODE="{behavior}"
SID=""
FLAG=""
MCP_CONFIG=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ] || [ "$prev" = "--resume" ]; then SID="$a"; FLAG="$prev"; fi
  if [ "$prev" = "--mcp-config" ]; then MCP_CONFIG="$a"; fi
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
STDIN_CONTENT=$(cat)
{{
  echo "FLAGS: $FLAG"
  echo "SID: $SID"
  echo "ARGS: $*"
  echo "STDIN:"
  printf '%s\n' "$STDIN_CONTENT"
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
if [ "$MODE" = "mcp-tool-call" ]; then
  if [ -z "$MCP_CONFIG" ]; then
    # No MCP server registered this round: this is the synthetic follow-up
    # round `execute_turn` sends to deliver text the user typed while a
    # tool call was pending (issue #1341), not the original tool-offering
    # round above. Reflect the received text back so a test can assert the
    # real queued content actually arrived, on the same --resume session.
    echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
    ECHOED=$(printf '%s' "$STDIN_CONTENT" | grep -oE '"text":"[^"]*"' | tail -1 | cut -d'"' -f4)
    echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_followup","role":"assistant","content":[{{"type":"text","text":"followup received: '"$ECHOED"'"}}]}}}}'
    echo '{{"type":"result","subtype":"success","is_error":false,"result":"followup received: '"$ECHOED"'","stop_reason":"end_turn"}}'
    exit 0
  fi
  echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
  SOCK=$(printf '%s' "$MCP_CONFIG" | grep -oE '"FINCH_CLAUDE_CLI_TOOL_SOCKET":"[^"]*"' | cut -d'"' -f4)
  printf '%s' "$SOCK" > "$SPOOL/socket_path"
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_preamble","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"mcp__finch__probe_tool","input":{{"key":"value"}}}}]}}}}'
  # This process must stay alive (stdout not yet at EOF) until the test has
  # finished acting as the bridge over the socket above -- otherwise
  # `drive`'s stdout-EOF branch could win the race against its listener
  # `accept()` branch and the pause this test exists to exercise would never
  # happen. The test signals completion by creating this file.
  tries=0
  while [ ! -f "$SPOOL/proceed" ] && [ "$tries" -lt 500 ]; do
    sleep 0.02
    tries=$((tries + 1))
  done
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_final","role":"assistant","content":[{{"type":"text","text":"tool call handled"}}]}}}}'
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"tool call handled","stop_reason":"end_turn"}}'
  exit 0
fi
if [ "$MODE" = "mcp-tool-call-with-preamble" ]; then
  # Combines "tool-preamble"'s text-before-tool_use shape with
  # "mcp-tool-call"'s real bridge pause/resume (unlike "tool-preamble",
  # which never pauses at all and so never spans two separate Finch-level
  # `send_message_stream` calls). Production-boundary reproduction of a bug
  # found via a real Claude CLI Subscription session (issue #1372's
  # investigation): a preamble text block streamed before the tool_use, in
  # a turn that genuinely pauses for a real tool result rather than
  # completing in one uninterrupted process invocation.
  echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
  SOCK=$(printf '%s' "$MCP_CONFIG" | grep -oE '"FINCH_CLAUDE_CLI_TOOL_SOCKET":"[^"]*"' | cut -d'"' -f4)
  printf '%s' "$SOCK" > "$SPOOL/socket_path"
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"I will check "}}}}}}'
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"the file."}}}}}}'
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_preamble","role":"assistant","content":[{{"type":"text","text":"I will check the file."}},{{"type":"tool_use","id":"toolu_1","name":"mcp__finch__probe_tool","input":{{"key":"value"}}}}]}}}}'
  tries=0
  while [ ! -f "$SPOOL/proceed" ] && [ "$tries" -lt 500 ]; do
    sleep 0.02
    tries=$((tries + 1))
  done
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"The first "}}}}}}'
  echo '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"line is Finch."}}}}}}'
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_final","role":"assistant","content":[{{"type":"text","text":"The first line is Finch."}}]}}}}'
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"The first line is Finch.","stop_reason":"end_turn"}}'
  exit 0
fi
if [ "$MODE" = "mcp-two-tool-calls" ]; then
  # Issue #1341's independent review: does this transport handle a turn
  # where the model asks for more than one tool? This mode reproduces the
  # sequential case -- one `claude` process asks for a second tool only
  # after the first one's real result comes back, over the *same* still-live
  # bridge socket/listener, spanning two separate pause/resume cycles.
  # (Whether the real CLI ever pipelines two `tools/call` requests to its
  # MCP server before reading the first reply -- true concurrent dispatch --
  # is NOT reproduced here and is not verified against the real CLI; see the
  # disclosed limitation in this crate's AGENTS.md and issue #1351.)
  echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
  SOCK=$(printf '%s' "$MCP_CONFIG" | grep -oE '"FINCH_CLAUDE_CLI_TOOL_SOCKET":"[^"]*"' | cut -d'"' -f4)
  printf '%s' "$SOCK" > "$SPOOL/socket_path"
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_tool1","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"mcp__finch__probe_tool","input":{{"n":"1"}}}}]}}}}'
  tries=0
  while [ ! -f "$SPOOL/proceed1" ] && [ "$tries" -lt 500 ]; do
    sleep 0.02
    tries=$((tries + 1))
  done
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_tool2","role":"assistant","content":[{{"type":"tool_use","id":"toolu_2","name":"mcp__finch__probe_tool","input":{{"n":"2"}}}}]}}}}'
  tries=0
  while [ ! -f "$SPOOL/proceed2" ] && [ "$tries" -lt 500 ]; do
    sleep 0.02
    tries=$((tries + 1))
  done
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_final","role":"assistant","content":[{{"type":"text","text":"both tools handled"}}]}}}}'
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"both tools handled","stop_reason":"end_turn"}}'
  exit 0
fi
if [ "$MODE" = "two-unrelated-replies" ]; then
  # Production-boundary reproduction of issue #1388: the real claude CLI
  # 2.1.283, in one continuous --print invocation with no tool_use anywhere
  # in the transcript, emitted two independent, complete `assistant` text
  # events back to back -- a confused first reply reacting to bare context,
  # then a second, real answer -- with nothing bridging them. Before the fix,
  # `TurnRecord::assistant_text` fused both into one string with no
  # separator, mid-word ("...help with?I don't have...").
  printf '%s\n' \
    '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"I do not see a specific request yet."}}}}}}' \
    '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_confused","role":"assistant","content":[{{"type":"text","text":"I do not see a specific request yet."}}]}}}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"47 "}}}}}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"times 89 is 4183."}}}}}}' \
    '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_real_answer","role":"assistant","content":[{{"type":"text","text":"47 times 89 is 4183."}}]}}}}' \
    '{{"type":"result","subtype":"success","is_error":false,"result":"47 times 89 is 4183.","stop_reason":"end_turn"}}'
  exit 0
fi
if [ "$MODE" = "tool-preamble" ]; then
  printf '%s\n' \
    '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"I will check "}}}}}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"the file."}}}}}}' \
    '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_preamble","role":"assistant","content":[{{"type":"text","text":"I will check the file."}},{{"type":"tool_use","id":"toolu_1","name":"mcp__finch__read","input":{{"file_path":"README.md"}}}}],"usage":{{"input_tokens":3,"output_tokens":9}}}}}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"The first "}}}}}}' \
    '{{"type":"stream_event","event":{{"type":"content_block_delta","delta":{{"type":"text_delta","text":"line is Finch."}}}}}}' \
    '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_final","role":"assistant","content":[{{"type":"text","text":"The first line is Finch."}}],"usage":{{"input_tokens":5,"output_tokens":6}}}}}}' \
    '{{"type":"result","subtype":"success","is_error":false,"result":"The first line is Finch.","stop_reason":"end_turn"}}'
  exit 0
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
        let mut usage = Vec::new();
        while let Some(chunk) = rx.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(text) => deltas.push(text),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                StreamChunk::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    assert!(
                        complete.is_none(),
                        "usage must precede the terminal text block"
                    );
                    usage.push((input_tokens, output_tokens));
                }
                other => panic!("unexpected chunk {other:?}"),
            }
        }
        assert_eq!(
            usage,
            vec![(2, 4)],
            "INVARIANT: the usage the CLI's assistant message reported (input 2, output 4) \
             reaches the stream exactly once; deltas={deltas:?} complete={complete:?}"
        );
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

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn a_transiently_busy_claude_binary_is_retried_not_reported_as_a_spawn_failure() {
        // Regression (issue #1354's own CI: this exact, already-documented
        // #1204 ETXTBSY race — see spawn_retrying_text_file_busy's own doc
        // comment — started firing for real once this issue added several
        // more spawn-heavy tests to this module). Deterministic
        // reproduction mirrors `src/tools/diagnostics/mod.rs`'s identical
        // test: hold a second write descriptor open on the exact fake
        // `claude` binary about to be exec'd (the kernel's check is
        // per-inode, so this is deterministic, not timing-dependent), and
        // prove the retry recovers it instead of surfacing a spawn error.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "ok");
        let provider = ClaudeCliProvider::with_binary(binary.clone(), None);

        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&binary)
            .expect("open the fake claude binary for writing while it is still named");
        let held: std::sync::Arc<std::sync::Mutex<Option<std::fs::File>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Some(writer)));
        let releaser_held = std::sync::Arc::clone(&held);
        std::thread::spawn(move || {
            // Release once a real refusal has actually been observed,
            // rather than after a fixed delay: a timer would make this
            // test vacuous on a fast or lightly loaded machine.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while text_file_busy_refusals() == 0 && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            releaser_held
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take();
        });

        let mut rx = provider
            .send_message_stream(&simple_request())
            .await
            .expect("a transiently busy claude binary must still be retried and succeed");
        let mut complete = None;
        while let Some(chunk) = rx.recv().await {
            if let StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) = chunk.unwrap() {
                complete = Some(text);
            }
        }
        assert_eq!(
            complete.as_deref(),
            Some("hello"),
            "the retried spawn must still complete the turn normally, not just avoid an error"
        );
        assert!(
            text_file_busy_refusals() > 0,
            "this test's own held write descriptor must have produced at least one real \
             ETXTBSY refusal for the retry path to have actually been exercised — a refusal \
             count of 0 means this test raced its own setup and proved nothing"
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
            .invocation_args(false, Some("SYSTEM"), &[], None)
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

        let resume_args = provider.invocation_args(true, None, &[], None).unwrap();
        assert!(resume_args.contains(&"--resume".to_string()));
        assert!(
            !resume_args.iter().any(|arg| arg == "--system-prompt"),
            "an absent system prompt must omit the flag, not send empty bytes"
        );
    }

    #[test]
    fn invocation_args_with_tools_wires_the_mcp_bridge_not_the_clis_own_builtins() {
        let provider = ClaudeCliProvider::new(None);
        let socket_path = std::path::Path::new("/tmp/finch-test-claude-cli-bridge.sock");
        let args = provider
            .invocation_args(
                false,
                None,
                &["read".to_string(), "bash".to_string()],
                Some(socket_path),
            )
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
        assert_eq!(
            server["env"][CLAUDE_CLI_TOOL_SOCKET_ENV],
            socket_path.to_string_lossy().to_string(),
            "issue #1341: the bridge must learn the tool-call socket path via its own MCP \
             server env entry, never via claude's own environment: {server:?}"
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

    /// Issue #1389: the inner `claude` subprocess must never act as an
    /// independent agent with its own persistent state or its own
    /// interactive prompts, regardless of whether this turn requests Finch
    /// tools. `--restricted` and `--permission-prompts none` are verified
    /// live (this file's `invocation_args` doc comment records the exact
    /// repro and the two flags rejected first) to close the auto-memory
    /// write and the terminal-hijack risk without disturbing the real MCP
    /// tool bridge or OAuth subscription auth — unlike `--safe-mode`
    /// (drops even an explicit `--mcp-config` server) or `--bare` (forces
    /// API-key auth, breaking this provider's own subscription login).
    #[test]
    fn invocation_args_always_scope_the_subprocess_against_its_own_persistent_state_and_prompts() {
        let provider = ClaudeCliProvider::new(None);
        let no_tools_args = provider
            .invocation_args(false, Some("SYSTEM"), &[], None)
            .unwrap();
        let socket_path = std::path::Path::new("/tmp/finch-test-claude-cli-bridge.sock");
        let with_tools_args = provider
            .invocation_args(false, None, &["read".to_string()], Some(socket_path))
            .unwrap();
        for (label, args) in [
            ("no tools", &no_tools_args),
            ("with tools", &with_tools_args),
        ] {
            assert!(
                args.contains(&"--restricted".to_string()),
                "{label}: --restricted must always be present so the CLI never falls back to \
                 an independent agent's own persistent state or ambient customization; args: {args:?}"
            );
            assert!(
                args.iter().zip(args.iter().skip(1)).any(|(flag, value)| flag
                    == "--permission-prompts"
                    && value == "none"),
                "{label}: --permission-prompts none must always be present so anything that would \
                 prompt a human outside Finch's own approval flow is denied automatically instead \
                 of hijacking the terminal (issue #1389's \"Edit in $EDITOR\" repro); args: {args:?}"
            );
            assert!(
                !args.contains(&"--bare".to_string()),
                "{label}: --bare forces ANTHROPIC_API_KEY/apiKeyHelper auth and never reads OAuth \
                 or the keychain, which would break this provider's whole reason for existing \
                 (driving the user's Claude subscription login); args: {args:?}"
            );
            assert!(
                !args.contains(&"--safe-mode".to_string()),
                "{label}: --safe-mode was verified live to drop even an explicitly passed \
                 --mcp-config server (system/init's mcp_servers came back empty) and broke real \
                 tool calls through the bridge; args: {args:?}"
            );
        }
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
            "the tool_use-only event contributed no text, so the final text-only event's \
             text is the whole accumulated response"
        );
    }

    #[test]
    fn a_preamble_before_a_tool_use_survives_into_the_accumulated_response_text() {
        // Issue #1331: unlike the fixture above, the real claude CLI 2.1.283
        // can put a preamble text block *alongside* the tool_use block in
        // the same first `assistant` event (narrating what it's about to
        // do). That text must not vanish when the second, final-answer
        // `assistant` event arrives — `assistant_text` accumulates instead
        // of being overwritten, so both segments are preserved in the order
        // the CLI emitted them.
        let mut record = TurnRecord::default();
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_1","content":[{"type":"text","text":"I'll check that file."},{"type":"tool_use","id":"toolu_1","name":"mcp__finch__read","input":{"file_path":"README.md"}}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.tool_calls_observed, 1,
            "the tool_use block sharing this event with a preamble text block must still be observed"
        );
        assert_eq!(
            record.assistant_text, "I'll check that file.",
            "a preamble text block accompanying a tool_use block must be recorded, not dropped"
        );
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_2","content":[{"type":"text","text":" The first line is Finch."}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.response_text(),
            "I'll check that file. The first line is Finch.",
            "the completed content must be every assistant-message text segment in this turn, \
             concatenated in arrival order — not just the last message's text"
        );
    }

    #[test]
    fn two_text_only_assistant_events_with_no_tool_use_between_them_get_a_separator() {
        // Issue #1388, `TurnRecord::absorb_line` unit-level: unlike the
        // fixture above, nothing (no `tool_use` block, in this event or an
        // earlier one) bridges the two text segments, so they are two
        // independent, complete replies -- appending them directly would
        // reproduce the exact reported seam ("...help with?I don't have...").
        let mut record = TurnRecord::default();
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_1","content":[{"type":"text","text":"I don't see a specific request yet."}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.assistant_text, "I don't see a specific request yet.",
            "the first, unbridged text-only assistant event is recorded as-is"
        );
        assert_eq!(
            record.tool_calls_observed, 0,
            "this fixture never emits a tool_use block"
        );
        record
            .absorb_line(
                r#"{"type":"assistant","message":{"model":"claude-sonnet-5","id":"msg_2","content":[{"type":"text","text":"I'll compute it directly: 47 x 89 = 4183."}]}}"#,
            )
            .unwrap();
        assert_eq!(
            record.response_text(),
            format!(
                "I don't see a specific request yet.{INDEPENDENT_REPLY_SEPARATOR}I'll compute \
                 it directly: 47 x 89 = 4183."
            ),
            "two independent complete replies with no tool_use bridging them must be joined \
             by INDEPENDENT_REPLY_SEPARATOR, never fused with nothing between them"
        );
        assert!(
            !record.response_text().contains("yet.I'll"),
            "the exact pre-fix seam ('yet.I'll', analogous to the real transcript's \
             'help with?I don't') must not reappear: {:?}",
            record.response_text()
        );
    }

    #[tokio::test]
    async fn preamble_text_before_a_mid_turn_tool_use_does_not_desync_streamed_from_completed_text()
    {
        // Production-boundary reproduction of issue #1331: every line the
        // real claude CLI 2.1.283 emits, replayed through the real
        // `send_message_stream` path (spawn, stream-json parse, MCP
        // tool_use observation, terminal ContentBlockComplete) — not just a
        // direct `TurnRecord::absorb_line` call. Before the fix, every
        // `text_delta` (preamble + final answer) still forwarded live as a
        // `TextDelta`, but the completed content reported only the final
        // message's text (the preamble was overwritten), which is exactly
        // the "Provider streaming text did not match its completed content"
        // failure `query_processor.rs` raised against a real subscription
        // session on the query "read the file README.md and tell me its
        // first line".
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "tool-preamble");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let mut rx = provider
            .send_message_stream(&simple_request())
            .await
            .expect("streaming must be supported");
        let mut streamed_text = String::new();
        let mut complete = None;
        while let Some(chunk) = rx.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(text) => streamed_text.push_str(&text),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                StreamChunk::Usage { .. } => {}
                other => panic!("unexpected chunk {other:?}"),
            }
        }
        let complete = complete.expect("a ContentBlockComplete chunk must terminate the stream");
        assert_eq!(
            streamed_text, "I will check the file.The first line is Finch.",
            "every text_delta across both assistant messages (preamble, then final answer) \
             must reach the caller, matching real claude CLI 2.1.283 behavior"
        );
        assert_eq!(
            complete, streamed_text,
            "completed content must equal everything actually streamed, or \
             query_processor.rs's streamed-vs-completed check fails the turn (issue #1331); \
             got completed={complete:?} streamed={streamed_text:?}"
        );
    }

    #[tokio::test]
    async fn two_unrelated_assistant_replies_with_no_tool_use_get_a_separator_not_fused_mid_word() {
        // Production-boundary reproduction of issue #1388: real claude CLI
        // 2.1.283, asked "Please spawn a subagent to compute 47*89 and tell
        // me the result", emitted two independent, complete `assistant` text
        // events in one continuous --print invocation with no `tool_use`
        // anywhere in the transcript (no "Tools (N call)" indicator ever
        // appeared) -- a confused first reply reacting to bare context,
        // immediately followed by the real answer. Before the fix, both
        // `TurnRecord::response_text()` and the live `TextDelta` stream fused
        // the two into one string with no separator, mid-word
        // ("...yet.47 times..." in this fixture; "...help with?I don't
        // have..." in the real transcript) -- fed straight into the wire
        // parser as bogus "source" and shown to the user as one garbled
        // reply.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "two-unrelated-replies");
        let provider = ClaudeCliProvider::with_binary(binary, None);

        let mut rx = provider
            .send_message_stream(&simple_request())
            .await
            .expect("streaming must be supported");
        let mut streamed_text = String::new();
        let mut complete = None;
        while let Some(chunk) = rx.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(text) => streamed_text.push_str(&text),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                other => panic!("unexpected chunk {other:?}"),
            }
        }
        let complete = complete.expect("a ContentBlockComplete chunk must terminate the stream");
        let expected = format!(
            "I do not see a specific request yet.{INDEPENDENT_REPLY_SEPARATOR}47 times 89 is 4183."
        );
        assert_eq!(
            streamed_text, expected,
            "two independent assistant replies with no tool_use bridging them must be \
             separated, not fused mid-word ('yet.47' would be the pre-fix regression)"
        );
        assert!(
            !streamed_text.contains("yet.47"),
            "the exact pre-fix seam ('yet.47', analogous to the real transcript's \
             'help with?I don't') must not reappear in the streamed text: {streamed_text:?}"
        );
        assert_eq!(
            complete, streamed_text,
            "completed content must equal everything actually streamed, or \
             query_processor.rs's streamed-vs-completed check fails the turn; \
             got completed={complete:?} streamed={streamed_text:?}"
        );
    }

    /// Bounded poll for a file the fake `claude` process (or the transport
    /// itself) writes asynchronously. Not a correctness oracle — the actual
    /// assertions are on the values read, not on timing — just a liveness
    /// bound so a genuine hang fails fast with a named cause instead of
    /// blocking the suite.
    async fn wait_for_text_file(path: &Path) -> String {
        for _ in 0..500 {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if !contents.trim().is_empty() {
                    return contents;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!(
            "timed out waiting for {} to be published — see install_fake_claude's \
             mcp-tool-call mode",
            path.display()
        );
    }

    /// Build the two trailing messages `finalize_tool_execution` produces for
    /// the very next round after a tool call: the staged assistant `ToolUse`
    /// and the real `ToolResult`. This is the only shape that can resume a
    /// parked turn (issue #1341).
    fn with_tool_result(
        mut request: ProviderRequest,
        call_id: &str,
        call_name: &str,
        call_input: serde_json::Value,
        content: &str,
        is_error: bool,
    ) -> ProviderRequest {
        request.messages.push(crate::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: call_id.to_string(),
                name: call_name.to_string(),
                input: call_input,
            }],
        });
        request.messages.push(crate::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: call_id.to_string(),
                content: content.to_string(),
                is_error: Some(is_error),
            }],
        });
        request
    }

    #[tokio::test]
    async fn parked_turn_pauses_then_resumes_with_a_real_tool_result() {
        // Production-boundary reproduction of issue #1341's core mechanism:
        // a `tools/call` forwarded over the bridge socket must surface as an
        // ordinary `StreamChunk::ToolCallComplete` (the same shape every
        // other provider's tool calls take), end the stream exactly like a
        // native tool_use stop reason, and — once the next Finch-level round
        // supplies a real `ToolResult` — the reply must reach the *exact*
        // bridge connection that made the request, and the same underlying
        // `claude` process must resume and finish, never spawning a second
        // one.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);

        let mut rx = provider
            .send_message_stream(&request)
            .await
            .expect("streaming must be supported");

        // Play the bridge's role directly: connect to the socket this
        // transport bound and told the fake `claude` process about (via
        // --mcp-config's env entry), then send exactly the shape a real
        // bridge forwards for one `tools/call`.
        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let (call_id, call_name, call_input) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!(
                "expected a real ToolCallComplete translated from the bridge's socket \
                 request, got {other:?}"
            ),
        };
        assert_eq!(call_name, "probe_tool");
        assert_eq!(call_input, json!({"key": "value"}));
        assert!(
            rx.recv().await.is_none(),
            "the stream must end right after the tool call with no trailing \
             ContentBlockComplete, exactly like every other provider's stream after a \
             native tool_use stop reason"
        );

        let followup = with_tool_result(
            request.clone(),
            &call_id,
            &call_name,
            call_input,
            "real tool output",
            false,
        );
        let mut rx2 = provider
            .send_message_stream(&followup)
            .await
            .expect("resuming a parked turn must still return a stream");

        // The reply must land on the exact bridge connection that made the
        // request, carrying the real result content.
        let mut reader = BufReader::new(&mut bridge_stream);
        let mut reply_line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut reply_line),
        )
        .await
        .expect("the resumed round must answer the still-open bridge connection")
        .unwrap();
        let reply: ClaudeCliBridgeToolResponse = serde_json::from_str(reply_line.trim()).unwrap();
        assert_eq!(reply.content, "real tool output");
        assert!(!reply.is_error);

        // Let the fake claude process finish now that the bridge has its
        // answer — proving the *same* process resumed rather than a second
        // one being spawned (there is no second `--session-id`/`--resume`
        // invocation logged anywhere this test can see; the only process
        // that ever ran is this one, still blocked on $SPOOL/proceed).
        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();

        let mut complete = None;
        while let Some(chunk) = rx2.recv().await {
            match chunk.unwrap() {
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                other => panic!("unexpected chunk on the resumed stream: {other:?}"),
            }
        }
        assert_eq!(
            complete.as_deref(),
            Some("tool call handled"),
            "the resumed claude process must run to completion after the real tool result lands"
        );
    }

    /// Issue #1389: `--restricted --permission-prompts none` are always in
    /// `invocation_args()`'s output now, but the flags that suppressed the
    /// auto-memory write were chosen specifically *because* they were
    /// verified live not to disturb the real MCP tool bridge (unlike
    /// `--safe-mode`, which dropped the explicit `--mcp-config` server
    /// entirely). This production-boundary test is the automated half of
    /// that claim: a real tool call must still round-trip end to end through
    /// the spawned process with the new flags present on its argv, not just
    /// that the flags are present in isolation (covered by
    /// `invocation_args_always_scope_the_subprocess_against_its_own_persistent_state_and_prompts`).
    #[tokio::test]
    async fn real_tool_call_still_round_trips_with_the_new_subprocess_scoping_flags_present() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);

        let mut rx = provider
            .send_message_stream(&request)
            .await
            .expect("streaming must be supported");

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete { name, input, .. } => {
                assert_eq!(name, "probe_tool");
                assert_eq!(input, json!({"key": "value"}));
            }
            other => panic!("expected a real ToolCallComplete, got {other:?}"),
        }

        // Let the fake claude process finish; the tool-call round trip above
        // already proves the process spawned, accepted a real MCP tools/call
        // over the bridge, and paused correctly with these flags on argv.
        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();
        while rx.recv().await.is_some() {}

        let log = calls_log(&temp);
        assert!(
            log.contains("--restricted"),
            "the real spawned process must have been invoked with --restricted: {log}"
        );
        assert!(
            log.contains("--permission-prompts none"),
            "the real spawned process must have been invoked with --permission-prompts none: {log}"
        );
    }

    #[tokio::test]
    async fn resumed_round_reports_only_its_own_new_text_not_the_prior_rounds_preamble() {
        // Production-boundary reproduction of a bug found via a real Claude
        // CLI Subscription session (query: "What is the exact line count
        // of Cargo.toml in this repo?"): a preamble ("I'll check that
        // file.") streamed in the round before the tool call, the tool
        // executed for real, and the *second*, separate
        // `send_message_stream` call that resumes the paused process
        // failed with "Provider streaming text did not match its completed
        // content" — `query_processor.rs`'s consistency check compares what
        // streamed as `TextDelta` chunks *during this one call* against the
        // terminal `ContentBlockComplete`'s text. Before this fix,
        // `ContentBlockComplete` reported `TurnRecord::response_text()`
        // unconditionally — the *whole* turn's accumulated text spanning
        // both the paused-and-resumed process's rounds — even though this
        // call's own `deltas` channel only ever carried the second round's
        // new text; the first round's preamble streamed through a
        // different, already-closed channel from an earlier
        // `send_message_stream` call. Every other provider's streams are
        // each genuinely self-contained, so this mismatch is specific to
        // this transport's cross-call parked-turn resume.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call-with-preamble");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);

        let mut rx = provider
            .send_message_stream(&request)
            .await
            .expect("streaming must be supported");

        // Play the bridge's role directly, matching
        // `parked_turn_pauses_then_resumes_with_a_real_tool_result`: connect
        // to the socket this transport bound and told the fake `claude`
        // process about, then send exactly the shape a real bridge forwards
        // for one `tools/call`. This must happen *before* draining `rx` for
        // `ToolCallComplete` — that chunk is only sent once `drive`'s
        // `listener.accept()` branch actually wins its race against
        // continued stdout reads, which requires a real connection to
        // arrive and deliver a well-formed request.
        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let mut first_round_text = String::new();
        let (call_id, call_name, call_input) = loop {
            match rx
                .recv()
                .await
                .expect("stream must yield the preamble then the tool call")
                .unwrap()
            {
                StreamChunk::TextDelta(text) => first_round_text.push_str(&text),
                StreamChunk::ToolCallComplete {
                    id, name, input, ..
                } => break (id, name, input),
                other => panic!("unexpected chunk before the tool call: {other:?}"),
            }
        };
        assert_eq!(
            first_round_text, "I will check the file.",
            "the preamble must still stream live before the tool call, exactly as issue \
             #1331 established"
        );
        assert!(
            rx.recv().await.is_none(),
            "the stream must end right after the tool call, like every other provider's \
             stream after a native tool_use stop reason"
        );

        let followup = with_tool_result(
            request.clone(),
            &call_id,
            &call_name,
            call_input,
            "real tool output",
            false,
        );
        let mut rx2 = provider
            .send_message_stream(&followup)
            .await
            .expect("resuming a parked turn must still return a stream");

        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();

        let mut second_round_streamed = String::new();
        let mut second_round_complete = None;
        while let Some(chunk) = rx2.recv().await {
            match chunk.unwrap() {
                StreamChunk::TextDelta(text) => second_round_streamed.push_str(&text),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    second_round_complete = Some(text);
                }
                other => panic!("unexpected chunk on the resumed stream: {other:?}"),
            }
        }
        assert_eq!(
            second_round_streamed, "The first line is Finch.",
            "the resumed round's own TextDelta chunks must be only its own new text, not \
             the first round's preamble (that already streamed through a different, closed \
             channel)"
        );
        assert_eq!(
            second_round_complete.as_deref(),
            Some("The first line is Finch."),
            "the resumed round's ContentBlockComplete must equal exactly what this specific \
             call streamed, matching query_processor.rs's streamed-vs-completed invariant — \
             not TurnRecord::response_text()'s whole-turn total, which also includes the \
             first round's preamble; second_round_streamed={second_round_streamed:?}"
        );
    }

    #[tokio::test]
    async fn parked_call_match_reports_no_pending_call_before_any_turn() {
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        assert_eq!(
            provider.parked_call_match(&simple_request()).await,
            ParkedCallMatch::NoPendingCall,
            "a provider that never started a turn has nothing parked to mismatch against"
        );
    }

    #[tokio::test]
    async fn parked_call_match_is_a_non_destructive_peek_that_never_abandons_a_correct_resume() {
        // Issue #1354: calling parked_call_match to *check* whether a
        // request answers the pending call must never itself consume or
        // abandon the parked turn — a caller (the daemon-owned session
        // registry) checks before deciding whether to proceed, and the
        // subsequent real resume must still work exactly as if the check
        // had never happened.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let (call_id, call_name, call_input) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete, got {other:?}"),
        };

        let correct_followup = with_tool_result(
            request.clone(),
            &call_id,
            &call_name,
            call_input.clone(),
            "real tool output",
            false,
        );

        // Peek twice — neither call may disturb the parked state.
        assert_eq!(
            provider.parked_call_match(&correct_followup).await,
            ParkedCallMatch::Matches
        );
        assert_eq!(
            provider.parked_call_match(&correct_followup).await,
            ParkedCallMatch::Matches,
            "a second peek must report the identical answer; the first peek must not have \
             consumed the parked turn"
        );

        // The real resume must still work normally after those peeks.
        let mut rx2 = provider
            .send_message_stream(&correct_followup)
            .await
            .expect("resuming after non-destructive peeks must still return a stream");
        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();
        let mut complete = None;
        while let Some(chunk) = rx2.recv().await {
            if let StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) = chunk.unwrap() {
                complete = Some(text);
            }
        }
        assert_eq!(
            complete.as_deref(),
            Some("tool call handled"),
            "peeking with parked_call_match must never prevent a correct real resume from \
             completing normally afterward"
        );
    }

    #[tokio::test]
    async fn parked_call_match_reports_mismatch_and_leaves_the_parked_turn_alive() {
        // Issue #1354's core reattach-safety fix: a request that does not
        // answer the currently parked call (e.g. a reattaching frontend
        // that never saw the pending tool call and just resent its own
        // reconstructed conversation) must be reported as a clear
        // mismatch, and — unlike `execute_turn`'s own internal
        // `take_matching_parked_turn`, whose intentional behavior for the
        // single-frontend cancel/retry case is to silently abandon a
        // non-matching parked turn — the parked turn itself must remain
        // alive and resumable afterward. A caller must check this *before*
        // ever invoking execute_turn with the mismatched request, never
        // discover the mismatch by watching a real turn silently restart.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let (real_call_id, call_name, call_input) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete, got {other:?}"),
        };

        // A request answering some *other* call id entirely — exactly what
        // an uninformed reattaching frontend could plausibly send.
        let mismatched = with_tool_result(
            request.clone(),
            "some-other-call-id-the-parked-turn-never-asked-for",
            &call_name,
            call_input,
            "wrong answer",
            false,
        );
        match provider.parked_call_match(&mismatched).await {
            ParkedCallMatch::Mismatch { pending_id } => {
                assert_eq!(
                    pending_id, real_call_id,
                    "the reported pending id must name the call that is actually parked"
                );
            }
            other => panic!("expected Mismatch, got {other:?}"),
        }

        // The parked turn must still be exactly where it was: answerable
        // with the *correct* result, on the same still-open bridge
        // connection, with the same underlying process.
        let correct_followup = with_tool_result(
            request,
            &real_call_id,
            &call_name,
            json!({"key": "value"}),
            "real tool output",
            false,
        );
        let mut rx2 = provider
            .send_message_stream(&correct_followup)
            .await
            .expect("the parked turn must still be resumable after a mismatched peek");

        let mut reader = BufReader::new(&mut bridge_stream);
        let mut reply_line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut reply_line),
        )
        .await
        .expect(
            "a mismatched peek must not have answered or dropped the bridge's still-open \
             connection",
        )
        .unwrap();
        let reply: ClaudeCliBridgeToolResponse = serde_json::from_str(reply_line.trim()).unwrap();
        assert_eq!(
            reply.content, "real tool output",
            "the eventual real answer must reach the bridge, proving the mismatched peek never \
             consumed or answered the pending call itself"
        );

        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();
        let mut complete = None;
        while let Some(chunk) = rx2.recv().await {
            if let StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) = chunk.unwrap() {
                complete = Some(text);
            }
        }
        assert_eq!(complete.as_deref(), Some("tool call handled"));
    }

    #[tokio::test]
    async fn bridge_disconnecting_mid_call_fails_the_resume_cleanly_not_a_hang_or_panic() {
        // Hostile timing (issue #1341): the bridge subprocess can die between
        // forwarding a tools/call and the frontend answering it (its parent
        // `claude` process crashed, was killed, or the bridge itself
        // panicked). The resume attempt must fail with a named error, not
        // hang forever waiting on a socket nothing will ever read again, and
        // must not panic.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim()).await.unwrap();
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let (call_id, call_name, call_input) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete, got {other:?}"),
        };
        assert!(rx.recv().await.is_none());

        // Simulate the bridge process dying before this transport ever gets
        // a chance to answer it.
        drop(bridge_stream);

        let followup = with_tool_result(
            request.clone(),
            &call_id,
            &call_name,
            call_input,
            "real tool output",
            false,
        );
        let mut rx2 = provider.send_message_stream(&followup).await.unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), rx2.recv())
            .await
            .expect("a dead bridge connection must fail fast, not hang the resume");
        match outcome {
            Some(Err(error)) => {
                let message = error.to_string().to_lowercase();
                assert!(
                    message.contains("bridge")
                        || message.contains("broken")
                        || message.contains("pipe")
                        || message.contains("reset"),
                    "the error should name what happened, not a bare generic failure: {error}"
                );
            }
            other => panic!("expected an Err reporting the dead bridge connection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_malformed_bridge_request_fails_the_turn_with_a_named_error_not_a_panic() {
        // Hostile input (issue #1341): the bridge is trusted Finch code today,
        // but the socket protocol itself must still fail closed and
        // diagnosably on a malformed line rather than panicking the whole
        // provider task.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim()).await.unwrap();
        bridge_stream.write_all(b"not json at all\n").await.unwrap();

        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("a malformed request must fail the turn promptly, not hang");
        match outcome {
            Some(Err(error)) => {
                assert!(
                    error.to_string().contains("malformed"),
                    "the error must name the malformed request, not a bare failure: {error}"
                );
            }
            other => panic!("expected an Err naming the malformed bridge request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dropping_every_provider_clone_while_parked_cleans_up_child_and_socket() {
        // Hostile timing (issue #1341): the whole query (and therefore this
        // provider instance) can be cancelled or torn down while a real
        // approval decision is still pending — nobody ever calls
        // `execute_turn` again for this session. The parked child and its
        // bridge socket file must not leak: `kill_on_drop` reaps the child
        // and `SocketGuard::drop` removes the socket file as soon as the
        // last clone of the provider (and therefore the last `Arc` holding
        // the parked state) goes away.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let socket_path = PathBuf::from(socket_path_text.trim());
        let mut bridge_stream = UnixStream::connect(&socket_path).await.unwrap();
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete { .. } => {}
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
        assert!(rx.recv().await.is_none());
        assert!(
            socket_path.exists(),
            "the bridge socket file must exist while the turn is parked"
        );

        drop(provider);
        drop(bridge_stream);

        for _ in 0..200 {
            if !socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !socket_path.exists(),
            "dropping every clone of a provider with a parked turn must remove its bridge \
             socket file, proving the parked child and listener were torn down rather than \
             leaked (issue #1341)"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_bridge_socket_is_hardened_to_owner_only_permissions() {
        // Security regression (issue #1341, found in independent review):
        // `read_bridge_request` trusts any well-formed request on any
        // accepted connection with no peer verification. A world-connectable
        // socket in `/tmp` would let *any* local process act as the bridge
        // and receive real tool-execution results -- the same
        // unauthenticated-local-socket pattern `src/server/ipc.rs` already
        // hardens (issue #911). Verified directly: `UnixListener::bind`
        // leaves the socket file at the ambient umask (0o755 under a
        // standard 0o022 umask) unless hardened.
        use std::os::unix::fs::PermissionsExt;
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let _rx = provider.send_message_stream(&request).await.unwrap();

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let socket_path = PathBuf::from(socket_path_text.trim());
        let mode = std::fs::metadata(&socket_path)
            .expect("the bridge socket file must exist once its path was published")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the bridge socket must be owner-only, matching src/server/ipc.rs's \
             IPC_SOCKET_MODE for the same unauthenticated local-socket pattern; got mode {mode:o}"
        );
    }

    #[tokio::test]
    async fn typed_ahead_text_during_a_parked_tool_call_is_delivered_not_lost() {
        // Correctness regression (issue #1341, found in independent review):
        // `ConversationHistory::append_text_blocks_to_last_user_message`
        // folds any text the user types while a tool call is pending into
        // the *same* trailing message as the eventual `ToolResult`, so the
        // next round's tail can be `[ToolResult, Text]`, not a bare
        // `ToolResult`. Before this fix, `tail_tool_result` required exactly
        // one content block and treated that shape as an abandoned/mismatched
        // parked turn -- silently discarding the real, already-approved tool
        // result and killing the still-live `claude` process for no reason.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-tool-call");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider
            .send_message_stream(&request)
            .await
            .expect("streaming must be supported");

        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let mut bridge_stream = UnixStream::connect(socket_path_text.trim())
            .await
            .expect("connect to the live bridge socket the paused turn is listening on");
        let request_line = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"key": "value"}),
        })
        .unwrap()
            + "\n";
        bridge_stream
            .write_all(request_line.as_bytes())
            .await
            .unwrap();

        let (call_id, call_name, call_input) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete, got {other:?}"),
        };
        assert!(rx.recv().await.is_none());

        // Build exactly the shape `commit_tool_round_and_continue` /
        // `append_text_blocks_to_last_user_message` produce when the user
        // typed something while this tool call was pending: the ToolResult
        // plus a trailing Text block in the *same* user message.
        let mut followup = with_tool_result(
            request.clone(),
            &call_id,
            &call_name,
            call_input,
            "real tool output",
            false,
        );
        let last = followup
            .messages
            .last_mut()
            .expect("with_tool_result always appends a trailing user message");
        last.content.push(ContentBlock::Text {
            text: "please also check the other file".to_string(),
        });

        let mut rx2 = provider
            .send_message_stream(&followup)
            .await
            .expect("a ToolResult plus queued text must still resume the parked turn");

        // The real tool result must still reach the bridge -- proving the
        // parked turn was resumed, not abandoned/killed.
        let mut reader = BufReader::new(&mut bridge_stream);
        let mut reply_line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut reply_line),
        )
        .await
        .expect("the resumed round must still answer the still-open bridge connection")
        .unwrap();
        let reply: ClaudeCliBridgeToolResponse = serde_json::from_str(reply_line.trim()).unwrap();
        assert_eq!(
            reply.content, "real tool output",
            "the real, already-approved tool result must not be discarded"
        );
        std::fs::write(spool_dir.join("proceed"), b"go").unwrap();

        // The tool-answer round and the queued-text follow-up round are two
        // separate `claude` invocations (the tool result can only be
        // answered over the socket, never via stdin on an already-running
        // process), but from this stream's perspective they must appear as
        // exactly one completed Finch-level round — the queued text must
        // never surface as a second, separate `ContentBlockComplete`, or
        // query_processor.rs would try to stage two assistant messages for
        // what the caller staged as one tool round.
        let mut complete = None;
        while let Some(chunk) = rx2.recv().await {
            match chunk.unwrap() {
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    assert!(
                        complete.is_none(),
                        "the chained follow-up round must merge into one completed content \
                         block, not surface as a second one"
                    );
                    complete = Some(text);
                }
                other => panic!("unexpected chunk on the resumed stream: {other:?}"),
            }
        }
        let complete = complete.expect("a ContentBlockComplete chunk must terminate the stream");
        assert!(
            complete.contains("tool call handled"),
            "the original parked claude process's own real answer must survive the merge: \
             {complete:?}"
        );
        assert!(
            complete.contains("please also check the other file"),
            "typed-ahead text queued during the parked tool call must be delivered and merged \
             into the completed content, not lost: {complete:?}"
        );
    }

    #[tokio::test]
    async fn two_sequential_tool_calls_in_one_turn_each_get_their_own_pause_resume_cycle() {
        // Independent-review follow-up for issue #1341 (tracked further in
        // issue #1351): real Claude models commonly request more than one
        // tool in a turn. This proves the *sequential* case -- the model
        // asks for a second tool only once the first one's real result comes
        // back -- works correctly across two separate pause/resume cycles on
        // the *same* underlying `claude` process and its *same* still-live
        // bridge socket/listener: no deadlock, no cross-talk between the two
        // calls' results, and the turn still completes normally afterward.
        // Whether the real CLI ever pipelines two `tools/call` requests
        // before reading the first reply (true concurrent dispatch) is not
        // reproduced here -- see issue #1351 and this crate's AGENTS.md.
        let temp = TempDir::new().unwrap();
        let binary = install_fake_claude(&temp, "mcp-two-tool-calls");
        let provider = ClaudeCliProvider::with_binary(binary, None);
        let spool_dir = PathBuf::from(spool(&temp));

        let mut request = simple_request();
        request.tools = Some(vec![tool_definition("read")]);
        let mut rx = provider
            .send_message_stream(&request)
            .await
            .expect("streaming must be supported");

        // --- Tool call #1 ---
        let socket_path_text = wait_for_text_file(&spool_dir.join("socket_path")).await;
        let socket_path = socket_path_text.trim().to_string();
        let mut bridge_stream_1 = UnixStream::connect(&socket_path)
            .await
            .expect("connect to the live bridge socket for the first tool call");
        let request_line_1 = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"n": "1"}),
        })
        .unwrap()
            + "\n";
        bridge_stream_1
            .write_all(request_line_1.as_bytes())
            .await
            .unwrap();

        let (call_id_1, call_name_1, call_input_1) = match rx.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete for the first tool call, got {other:?}"),
        };
        assert_eq!(call_input_1, json!({"n": "1"}));
        assert!(
            rx.recv().await.is_none(),
            "the stream must end right after the first tool call, before the second is ever \
             requested"
        );

        let followup_1 = with_tool_result(
            request.clone(),
            &call_id_1,
            &call_name_1,
            call_input_1,
            "result-1",
            false,
        );
        let mut rx2 = provider
            .send_message_stream(&followup_1)
            .await
            .expect("resuming after the first tool call must still return a stream");

        let mut reader_1 = BufReader::new(&mut bridge_stream_1);
        let mut reply_line_1 = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader_1.read_line(&mut reply_line_1),
        )
        .await
        .expect("the resumed round must answer the first bridge connection")
        .unwrap();
        let reply_1: ClaudeCliBridgeToolResponse =
            serde_json::from_str(reply_line_1.trim()).unwrap();
        assert_eq!(
            reply_1.content, "result-1",
            "the first tool call's own real result must reach its own bridge connection"
        );
        std::fs::write(spool_dir.join("proceed1"), b"go").unwrap();

        // --- Tool call #2, on the SAME process, over the SAME socket path ---
        let mut bridge_stream_2 = UnixStream::connect(&socket_path)
            .await
            .expect("connect to the still-live bridge socket for the second tool call");
        let request_line_2 = serde_json::to_string(&ClaudeCliBridgeToolRequest {
            name: "probe_tool".to_string(),
            input: json!({"n": "2"}),
        })
        .unwrap()
            + "\n";
        bridge_stream_2
            .write_all(request_line_2.as_bytes())
            .await
            .unwrap();

        let (call_id_2, call_name_2, call_input_2) = match rx2.recv().await.unwrap().unwrap() {
            StreamChunk::ToolCallComplete {
                id, name, input, ..
            } => (id, name, input),
            other => panic!("expected ToolCallComplete for the second tool call, got {other:?}"),
        };
        assert_eq!(
            call_input_2,
            json!({"n": "2"}),
            "the second tool call's own input must not be confused with the first's"
        );
        assert_ne!(
            call_id_2, call_id_1,
            "each tool call in the same turn must get its own distinct id"
        );
        assert!(rx2.recv().await.is_none());

        let followup_2 = with_tool_result(
            followup_1,
            &call_id_2,
            &call_name_2,
            call_input_2,
            "result-2",
            false,
        );
        let mut rx3 = provider
            .send_message_stream(&followup_2)
            .await
            .expect("resuming after the second tool call must still return a stream");

        let mut reader_2 = BufReader::new(&mut bridge_stream_2);
        let mut reply_line_2 = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader_2.read_line(&mut reply_line_2),
        )
        .await
        .expect("the resumed round must answer the second bridge connection")
        .unwrap();
        let reply_2: ClaudeCliBridgeToolResponse =
            serde_json::from_str(reply_line_2.trim()).unwrap();
        assert_eq!(
            reply_2.content, "result-2",
            "the second tool call's own real result must reach its own bridge connection, not \
             the first's"
        );
        std::fs::write(spool_dir.join("proceed2"), b"go").unwrap();

        let mut complete = None;
        while let Some(chunk) = rx3.recv().await {
            match chunk.unwrap() {
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    complete = Some(text);
                }
                other => panic!("unexpected chunk on the final resumed stream: {other:?}"),
            }
        }
        assert_eq!(
            complete.as_deref(),
            Some("both tools handled"),
            "the same underlying claude process must run to completion after both sequential \
             tool calls resolve, with no deadlock and no cross-talk between them"
        );
    }
}
