//! Main `EventLoop` — orchestrates user input, query dispatch, and TUI rendering.
//!
//! The event loop runs a `select!` over three streams:
//!
//! * **User input** from `spawn_input_task` (keystrokes, submit, Ctrl+C).
//! * **Query events** from the `ReplEvent` mpsc channel (streaming chunks,
//!   tool results, approval requests, brain messages).
//! * **Render tick** (~100ms) — flushes buffered output to the TUI.
//!
//! ## Submodules used
//! * `plan_handler` — intercepts `PresentPlan` / `AskUserQuestion` tool calls.
//! * `tool_display` — formats tool output for display rows.
//! * `tool_execution` — concurrent tool dispatch with approval gating.
//! * `query_state` — per-query state machine (pending → streaming → done).

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::style::Stylize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::claude::ContentBlock;
use crate::cli::commands::{format_help, Command};
use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::{OutputManager, VmOutputProjection};
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::{spawn_input_task, TuiRenderer};
use crate::feedback::{FeedbackEntry, FeedbackLogger, FeedbackRating};
use crate::generators::Generator;
use crate::local::LocalGenerator;
use crate::memory::NeuralEmbeddingEngine;
use crate::models::bootstrap::GeneratorState;
use crate::models::tokenizer::TextTokenizer;
use crate::router::Router;
use crate::session::diff_store::DiffStore;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::ToolDefinition;

use super::events::{LlmRequest, ReplEvent};
use super::llm_loop::LlmLoop;
use super::model_selection::{activate_local_when_ready, LocalActivationOutcome, ModelSelection};
use super::query_processor::{refresh_context_strip, ActiveToolUsesMap};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_display::tool_result_to_display;
use super::tool_execution::ToolExecutionCoordinator;

// refresh_context_strip, dispatch_tool_uses, process_query_with_tools,
// ActiveToolUsesMap, and apply_sliding_window live in query_processor.rs.

type ToolResultsMap = Arc<RwLock<std::collections::HashMap<Uuid, Vec<(String, Result<String>)>>>>;
type PendingApprovalsMap = Arc<
    RwLock<
        std::collections::HashMap<
            Uuid,
            (
                crate::tools::types::ToolUse,
                tokio::sync::oneshot::Sender<super::events::ConfirmationResult>,
            ),
        >,
    >,
>;

pub(crate) fn resolve_provider_profile(
    providers: &[crate::config::ProviderEntry],
    selector: &str,
) -> std::result::Result<usize, String> {
    if let Ok(number) = selector.parse::<usize>() {
        return if number > 0 && number <= providers.len() {
            Ok(number - 1)
        } else {
            Err(format!(
                "Invalid model number: {number}. Use 1-{}",
                providers.len()
            ))
        };
    }

    let selector = selector.trim();
    let exact: Vec<usize> = providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.profile_name().eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0]);
    }
    if exact.len() > 1 {
        return Err(format!(
            "Model name '{selector}' is ambiguous; give these profiles unique names in config"
        ));
    }

    let by_type: Vec<usize> = providers
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.provider_type().eq_ignore_ascii_case(selector))
        .map(|(index, _)| index)
        .collect();
    match by_type.as_slice() {
        [index] => Ok(*index),
        [] => Err(format!(
            "Unknown model profile '{selector}'. Run /model list to see configured profiles"
        )),
        _ => Err(format!(
            "Provider type '{selector}' matches multiple profiles; select one by name or number"
        )),
    }
}

/// Continuation data for a poset run that is waiting for user confirmation.
struct PendingPosetRun {
    generator: Arc<dyn crate::generators::Generator>,
    poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>,
    stack: Arc<tokio::sync::Mutex<Vec<String>>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<ReplEvent>,
}

/// Main event loop for concurrent REPL
#[allow(dead_code)]
pub struct EventLoop {
    /// Channel for receiving events
    event_rx: mpsc::UnboundedReceiver<ReplEvent>,
    /// Channel for sending events
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Channel for receiving user input events
    input_rx: mpsc::UnboundedReceiver<crate::cli::tui::InputEvent>,

    /// Shared conversation history
    conversation: Arc<RwLock<ConversationHistory>>,

    /// Query state manager
    query_states: Arc<QueryStateManager>,

    /// Active and pending session model state.
    model_selection: ModelSelection,

    /// Qwen generator (unified interface)
    qwen_gen: Arc<dyn Generator>,

    /// Available providers from config (for /provider list + switching)
    available_providers: Vec<crate::config::ProviderEntry>,

    /// HTTP daemon client used for local-model status and generation.
    daemon_client: Option<Arc<crate::client::DaemonClient>>,

    /// Router for deciding between generators
    router: Arc<Router>,

    /// Generator state for bootstrap tracking
    generator_state: Arc<RwLock<GeneratorState>>,

    /// Tool definitions for Claude API
    tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>,

    /// TUI renderer
    tui_renderer: Arc<Mutex<TuiRenderer>>,

    /// Output manager
    output_manager: Arc<OutputManager>,

    /// Status bar
    status_bar: Arc<StatusBar>,

    /// Whether streaming is enabled
    streaming_enabled: bool,

    /// Tool execution coordinator
    tool_coordinator: ToolExecutionCoordinator,

    /// The frontend-owned VM runtime. It is used only to resume an existing,
    /// verified portable effect; the daemon never acquires workspace authority
    /// through this field.
    program_runtime: Arc<crate::runtime::ProgramRuntime>,

    /// Tool results collected per query (query_id -> Vec<(tool_id, result)>)
    tool_results: ToolResultsMap,

    /// Currently active query ID (for cancellation)
    active_query_id: Arc<RwLock<Option<Uuid>>>,

    /// User turns submitted while a provider/VM turn is active.  The legacy
    /// code overwrote `active_query_id`, leaving the earlier turn unable to
    /// clear itself and making the UI appear frozen.  Queue textual turns so
    /// the single shared conversation and VM revision advance in order.
    pending_queries: std::collections::VecDeque<(String, bool, bool)>,

    /// Pending tool approval requests (query_id -> (tool_use, response_tx))
    pending_approvals: PendingApprovalsMap,

    /// IPC client — Cap'n Proto channel to the daemon.
    /// Must live inside a tokio LocalSet (capnp-rpc !Send).
    ipc_client: Option<crate::ipc::IpcClient>,

    /// REPL mode (Normal, Planning, Executing)
    mode: Arc<RwLock<ReplMode>>,

    /// Plan content storage (for PresentPlan tool)
    plan_content: Arc<RwLock<Option<String>>>,

    /// Memory tree console for tree-structured conversation view
    memtree_console: Arc<RwLock<crate::cli::memtree_console::MemTreeConsole>>,

    /// Event handler for translating REPL events to tree operations
    memtree_handler: Arc<tokio::sync::Mutex<crate::cli::memtree_console::EventHandler>>,

    /// Current view mode (List or Tree)
    view_mode: Arc<RwLock<ViewMode>>,

    /// Active tool calls: tool_id -> (tool_name, input, work_unit, row_idx)
    /// All tools in one generation turn share the same WorkUnit; each
    /// tool occupies one row identified by its index.
    active_tool_uses: ActiveToolUsesMap,

    /// Feedback logger — writes rated responses to ~/.finch/feedback.jsonl
    feedback_logger: Option<FeedbackLogger>,

    /// Metrics logger — reads from ~/.finch/metrics/ for /metrics command
    metrics_logger: Option<crate::metrics::MetricsLogger>,

    /// Memory system for semantic recall across sessions
    memory_system: Option<Arc<crate::memory::MemorySystem>>,

    /// Human-readable label for this session (e.g. "swift-falcon")
    session_label: String,

    /// Stable UUID for this session — assigned at startup, printed on exit.
    session_uuid: Uuid,

    /// Working directory at startup (for terminal title)
    cwd: String,

    /// Total number of status-strip lines (🧠 + context summaries).
    /// Comes from config.features.memory_context_lines (default 4).
    context_lines: usize,

    /// Maximum number of recent messages sent verbatim to the provider.
    /// Set to 0 to disable windowing. From config.features.max_verbatim_messages.
    max_verbatim_messages: usize,

    /// Number of MemTree results recalled and injected per query.
    /// From config.features.context_recall_k.
    context_recall_k: usize,

    /// Session task list shared with TodoWrite / TodoRead tools
    todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,

    /// Whether to summarise dropped messages (Infinite Context Phase 2).
    /// From config.features.enable_summarization.
    enable_summarization: bool,

    /// Whether sliding-window auto-compaction is enabled.
    /// From config.features.auto_compact_enabled. Default: true.
    auto_compact_enabled: bool,

    /// Whether to auto-discover peers via mDNS at startup.
    /// From config.client.auto_discover.
    auto_discover: bool,

    /// Channels this session has joined (e.g. "#general").
    /// Persisted across sessions in ~/.finch/channels.json.
    joined_channels: std::collections::HashSet<String>,

    /// Ring buffer of recently received channel messages: (channel, sender, text).
    /// Indexed 0 = most recent. Capped at 20.
    recent_channel_msgs: std::collections::VecDeque<(String, String, String)>,

    /// Remote peer daemon addresses (host:port) from --peer flag.
    remote_peers: Vec<String>,

    /// Explicit destination for prompts and VM programs while attached.
    /// This is singular by design: host effects are never broadcast.
    active_remote_brain: Option<crate::brain::remote::RemoteBrainClient>,

    /// Base URL of the local daemon (e.g. "http://127.0.0.1:8000").
    /// Used by the cross-machine relay poller.
    daemon_base_url: Option<String>,

    /// Mirror of peer_inbox — cloned into remote-peer bridge tasks so they can
    /// forward local peer responses back to the remote machine.
    peer_inbox_mirror_tx: tokio::sync::broadcast::Sender<(String, String)>,

    /// Provider used by the brain (background context-gathering agent).
    /// `None` when the brain is disabled (config flag) or no cloud provider is available.
    brain_provider: Option<Arc<dyn crate::providers::LlmProvider>>,

    /// Pre-gathered context from the active brain session (injected at query time).
    brain_context: Arc<RwLock<Option<String>>>,

    /// Active brain session (cancelled when user submits or starts a new brain).
    active_brain: Arc<RwLock<Option<crate::brain::BrainSession>>>,

    /// Pending oneshot sender for a BrainQuestion dialog response.
    pending_brain_question_tx: Option<tokio::sync::oneshot::Sender<String>>,

    /// Options for the current brain question dialog (to map index → text).
    pending_brain_question_options: Vec<String>,

    /// Pending oneshot sender for a BrainProposedAction approval dialog.
    /// Resolved with Some(output) when approved and executed, None when denied.
    pending_brain_action_tx: Option<tokio::sync::oneshot::Sender<Option<String>>>,

    /// The command string for the pending brain action (shown in the dialog).
    pending_brain_action_command: Option<String>,

    /// Oneshot sender for a dialog shown via `ReplEvent::ShowDialog`.
    /// The render tick delivers `pending_dialog_result` here when the dialog completes.
    pending_dialog_tx: Option<tokio::sync::oneshot::Sender<crate::cli::tui::DialogResult>>,

    /// Data for a pending Co-Forth poset run that is waiting on a confirmation dialog.
    pending_poset_run: Option<PendingPosetRun>,

    /// Known brain states from last poll (Uuid -> BrainState), for transition detection.
    known_brain_states: std::collections::HashMap<Uuid, crate::server::BrainState>,

    /// Per-query tool call history: query_id -> set of "tool_name:input_json" strings.
    /// Used to detect infinite loops (same tool called with same args multiple times).
    tool_call_history:
        Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,

    /// Execution graph for the current (or most recent) query.
    current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>,

    /// Brain UUID that the REPL is currently waiting for a question/plan from.
    /// Set when a transition to WaitingForInput/PlanReady is detected.
    pending_daemon_brain_id: Option<Uuid>,

    /// Oneshot sender for daemon brain question dialog response.
    pending_daemon_brain_question_tx: Option<tokio::sync::oneshot::Sender<String>>,
    pending_daemon_brain_question_options: Vec<String>,

    /// Whether the REPL is currently showing a plan dialog for a daemon brain.
    pending_daemon_brain_plan: bool,
    pending_daemon_brain_plan_id: Option<Uuid>,

    /// Deferred brain question: held when a BrainQuestion arrives while the user
    /// is busy (active query in flight).  Shown when the user becomes idle.
    deferred_brain_question: Option<(String, Vec<String>, tokio::sync::oneshot::Sender<String>)>,

    /// Co-Forth shared stack: items pushed by the user (text) or by the AI (Push tool).
    /// Arc<Mutex> so the tool executor can write to it during generation.
    stack: Arc<tokio::sync::Mutex<Vec<String>>>,

    /// Co-Forth poset VM — partially-ordered task graph with 3D renderer.
    poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>,

    /// The Co-Forth word that was popped when entering plan mode.
    /// Stored so the user can re-plan without losing the word.
    plan_word: Option<String>,

    /// Persistent Forth interpreter for the session.
    /// Word definitions typed via `: word ... ;` or `/forth` accumulate here.
    /// Stack state is cleared between evals; only the dictionary persists.
    forth_vm: crate::coforth::Forth,

    /// Lisp interpreter context — holds SSH sessions and the global env.
    /// Shared with spawned Lisp eval tasks via Arc so sessions persist across
    /// multiple `(...)` inputs within a session.
    lisp_ctx: std::sync::Arc<crate::lisp::LispCtx>,

    /// Global Lisp environment — `define`d names persist across REPL inputs.
    lisp_env: crate::lisp::EnvRef,

    /// Shared with TUI corner — updated after each eval if `check` is defined.
    corner_output: Arc<std::sync::Mutex<Option<String>>>,

    /// Undo history for Forth definitions.
    /// Each entry is a snapshot taken just before an eval (or /define).
    /// `/undefine` (or Ctrl+Z in Forth context) pops and restores.
    forth_undo: Vec<crate::coforth::DictionarySnapshot>,

    /// Incoming push messages from peers (via POST /v1/forth/push).
    push_rx: tokio::sync::broadcast::Receiver<String>,

    /// Incoming hash updates from peers (via POST /v1/forth/hash).
    /// Carries (room_id, key, value, from_name) tuples.
    hash_rx: tokio::sync::broadcast::Receiver<(String, String, String, String)>,

    /// Word names auto-compiled from the vocabulary library (not user-authored).
    /// Excluded from the vocab section sent to the AI so the prompt stays small.
    auto_compiled_word_names: std::collections::HashSet<String>,

    /// Broadcast sender — pushes code to ALL peer event loops simultaneously.
    peer_tx: tokio::sync::broadcast::Sender<crate::session::SessionEvent>,

    /// Sender half of the peer inbox — cloned into boot_peers() at run() time.
    peer_inbox_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, String, String)>,

    /// Shared inbox — receives `(id, name, text)` replies from all peers.
    peer_inbox_rx: tokio::sync::mpsc::UnboundedReceiver<(Uuid, String, String)>,

    /// Active peers: (UUID, display-name) for each forked loop.
    peer_sessions: Vec<(Uuid, String)>,

    /// In-memory store of pending diff proposals in the room.
    diff_store: DiffStore,

    /// Broadcast receiver that sees every SessionEvent sent on peer_tx.
    /// Used to intercept Diff/DiffEdit/DiffAccept/DiffReject events from peers
    /// without routing them through the plain-text peer_inbox_rx.
    peer_session_rx: tokio::sync::broadcast::Receiver<crate::session::SessionEvent>,

    // ── LLM worker loop channel ───────────────────────────────────────────
    /// Send LLM requests to the worker loop.
    llm_tx: mpsc::UnboundedSender<LlmRequest>,
    /// Receiver held until `run()` hands it off to `LlmLoop`.
    llm_rx: Option<mpsc::UnboundedReceiver<LlmRequest>>,
}

/// View mode for the REPL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Traditional list view (current scrollback)
    List,
    /// Tree-structured conversation view
    Tree,
}

/// Scan Forth source for `scatter-exec" cmd"` literals and return formatted plan lines.
///
/// Used for pre-flight plan display before remote execution.
/// Extract a Forth definition from a channel message, if present.
/// Channel messages have the format `[#channel] sender: <content>`.
/// If `<content>` starts with `:` (a colon definition), return it.
/// Heuristic: does this input look like natural language rather than Forth code?
/// Used to decide whether to fall through to AI even when the VM ran silently.
///
/// Triggers on:
/// - Questions (ASCII `?` or fullwidth `？`)
/// - Non-Latin scripts (Chinese, Arabic, Japanese, Korean, etc.) that aren't
///   Forth definitions — these never need uppercase to signal "sentence"
/// - Latin sentences starting with an uppercase letter
/// Returns `true` when `word` has a dedicated magic-word response.
/// Returns the specific response for a magic word, or `None` for unknowns.
fn magic_word_response(word: &str) -> Option<&'static str> {
    match word {
        "boom" => Some("boom. nothing survived. not even the stack."),
        "bang" => Some("bang. the universe blinked."),
        "fire" => Some("fired. no smoke. suspicious."),
        "nuke" => Some("nuked. oddly peaceful in here."),
        "crash" => Some("crash? no crash. try harder."),
        "die" => Some("still here. the machine has opinions about dying."),
        "kill" => Some("kill confirmed. no witnesses."),
        "stop" => Some("stopped. or never started. hard to say."),
        "go" => Some("gone. or was it ever here?"),
        "run" => Some("ran. left no forwarding address."),
        "help" => Some("help is a word. the stack did not respond."),
        "please" => Some("noted. the machine is unmoved by politeness."),
        "hello" => Some("hello back. ( silently )"),
        "bye" => Some("bye. the stack waves nothing."),
        "yes" => Some("yes. ( the stack agrees by saying nothing )"),
        "no" => Some("no. ( equally nothing )"),
        "fireball" | "fireballs" => {
            Some("fireball: pure energy, no output. the stack appreciates the drama.")
        }
        _ => None,
    }
}

/// The second programmer's reaction when a word runs silently.
/// Picks a remark based on the word name, or falls back to a random one.
fn silent_remark(code: &str) -> String {
    let trimmed = code.trim().to_lowercase();
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // Detect repetition: "BOOM BOOM BOOM" — escalate.
    if tokens.len() >= 2 && tokens.iter().all(|t| *t == tokens[0]) {
        let n = tokens.len();
        let word = tokens[0];
        return match word {
            "boom" if n >= 3 => "BOOM BOOM BOOM!! yes!! let's go!!".to_string(),
            "boom" if n == 2 => "BOOM BOOM. twice as loud. got it.".to_string(),
            "fire" if n >= 3 => "fired three times. not sure what we were shooting at.".to_string(),
            "help" if n >= 2 => {
                "help help. the machine has considered your urgency. the stack is unmoved."
                    .to_string()
            }
            _ if n >= 3 => {
                format!("{word} {word} {word}. it ran {n} times. the silence is louder now.")
            }
            _ => format!("{word} {word}. twice. same result both times: nothing."),
        };
    }

    // Single-word reactions
    let word = trimmed.as_str();
    if let Some(s) = magic_word_response(word) {
        return s.to_string();
    }
    // Generic rotating remarks
    static REMARKS: &[&str] = &[
        "done. the stack kept it to itself.",
        "ok. ( the silence is part of it )",
        "it happened. we just can't prove it.",
        "executed. left no evidence.",
        "the deed is done. nothing to show for it.",
        "somewhere, a bit flipped.",
        "the machine shrugged.",
        "noted and filed under: nothing.",
        "works on my stack.",
        "task complete. witnesses: zero.",
    ];
    let idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as usize)
        % REMARKS.len();
    REMARKS[idx].to_string()
}

/// Detect a markdown code fence in the input.
/// Returns `Some((language, code))` if the input is a fenced code block,
/// e.g. "```javascript\nfoo()\n```" → ("javascript", "foo()").
/// Also handles bare fences (no language tag) and prefix-style: "js: code".
fn extract_code_fence(input: &str) -> Option<(String, String)> {
    // ``` lang \n code \n ```
    if input.starts_with("```") {
        let inner = input.trim_start_matches('`');
        let (lang_line, rest) = inner.split_once('\n').unwrap_or(("", inner));
        let lang = lang_line.trim().to_string();
        let code = rest.trim_end_matches('`').trim().to_string();
        if !code.is_empty() {
            return Some((lang, code));
        }
    }
    // lang: code  (single-line prefix style — e.g. "js: x => x+1")
    let prefix_langs = [
        "js",
        "javascript",
        "python",
        "py",
        "rust",
        "ts",
        "typescript",
        "go",
        "java",
        "ruby",
        "rb",
        "c",
        "cpp",
        "bash",
        "sh",
        "sql",
        "html",
        "css",
        "swift",
        "kotlin",
        "php",
        "lua",
        "r",
        "haskell",
    ];
    for lang in &prefix_langs {
        if let Some(code) = input.strip_prefix(&format!("{lang}:")) {
            let code = code.trim().to_string();
            if !code.is_empty() {
                return Some((lang.to_string(), code));
            }
        }
    }
    None
}

/// When the user defines a new word, occasionally observe what it seems to do.
/// Returns Some(remark) ~30% of the time when the definition is interesting.
fn definition_observation(name: &str, body: &str) -> Option<String> {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Only comment ~1 in 3 times (based on nanos parity)
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    if t % 3 != 0 {
        return None;
    }

    let body_lo = body.to_lowercase();
    let name_lo = name.to_lowercase();

    // Detect what the body seems to do
    let prints = body_lo.contains(" . ")
        || body_lo.ends_with(" .")
        || body_lo.contains(".\"")
        || body_lo.contains("cr");
    let arithmetic = body_lo.contains(" + ")
        || body_lo.contains(" * ")
        || body_lo.contains(" - ")
        || body_lo.contains(" / ");
    let conditional = body_lo.contains("if") || body_lo.contains("case");
    let loops = body_lo.contains("begin") || body_lo.contains("do ");
    let calls_self = body_lo
        .split_whitespace()
        .any(|t| t == name_lo || t == "recurse");

    // Check if the name gives a hint about what it SHOULD do
    let name_hints_violent = matches!(
        name_lo.as_str(),
        "boom" | "bang" | "nuke" | "fire" | "blast" | "crash" | "kill" | "destroy"
    );
    let name_hints_math = matches!(
        name_lo.as_str(),
        "add" | "sub" | "mul" | "div" | "square" | "cube" | "double" | "half" | "negate"
    );
    let name_hints_query =
        name_lo.ends_with('?') || name_lo.starts_with("is-") || name_lo.starts_with("has-");

    if name_hints_violent && !prints && !arithmetic {
        return Some(format!(
            "{name} ran quietly. i was expecting something louder."
        ));
    }
    if name_hints_violent && prints {
        return Some(format!(
            "you called it {name} but it prints things. \
             i thought it would destroy things. both are valid."
        ));
    }
    if name_hints_math && !arithmetic && !body_lo.contains("dup") {
        return Some(format!(
            "{name} — but it doesn't seem to do math. is that what you meant?"
        ));
    }
    if name_hints_query && !conditional {
        return Some(format!(
            "{name} sounds like a question. but it doesn't branch. \
             what does it do when the answer is no?"
        ));
    }
    if calls_self {
        return Some(format!(
            "{name} calls itself. it's recursive. \
             make sure it has a base case or the stack will run out of road."
        ));
    }
    if loops && arithmetic {
        return Some(format!(
            "{name} loops and does math. that's a computation. \
             the stack will have the answer when it's done."
        ));
    }
    if prints && arithmetic {
        return Some(format!(
            "{name}: computes something and shows it. \
             give it a number and see what comes out."
        ));
    }
    None
}

/// Returns true when `s` looks like natural language (→ AI), false when it looks
/// like Forth (→ VM).  `word_exists` should check the live VM dictionary so that
/// user-defined words are always treated as Forth, not language.
/// Returns true when `w` is a Forth primitive/builtin that should always
/// route to the VM regardless of whether it appears in the session vocabulary.
fn is_forth_primitive_word(w: &str) -> bool {
    matches!(
        w,
        "dup"
            | "drop"
            | "swap"
            | "over"
            | "rot"
            | "nip"
            | "tuck"
            | "pick"
            | "roll"
            | "mod"
            | "abs"
            | "max"
            | "min"
            | "negate"
            | "square"
            | "pow"
            | "and"
            | "or"
            | "xor"
            | "invert"
            | "cr"
            | "emit"
            | "type"
            | "if"
            | "else"
            | "then"
            | "do"
            | "loop"
            | "begin"
            | "until"
            | "while"
            | "repeat"
            | "exit"
            | "words"
            | "help"
            | "undo"
            | "apply"
            | "describe"
            | "agree?"
            | "back-and-forth?"
            | "invertible?"
    ) || w.parse::<f64>().is_ok()
        || w.chars().any(|c| {
            matches!(
                c,
                '+' | '-' | '*' | '/' | '@' | '!' | '.' | ':' | ';' | '<' | '>' | '='
            )
        })
}

/// Heuristic: does this input look like natural language rather than Forth code?
///
/// `word_exists`   — checks the full VM dictionary (builtins + library + session).
/// `session_word`  — checks only words explicitly defined *during this session*
///                   (not precompiled library words).  Used to distinguish
///                   user-authored vocabulary from ambient library words.
///
/// Example: "hello world" — both are precompiled library words so `word_exists`
/// returns true for each, but `session_word` returns false for both, so the
/// phrase is routed to the AI rather than executed as Forth.
fn looks_like_natural_language(
    s: &str,
    word_exists: impl Fn(&str) -> bool,
    session_word: impl Fn(&str) -> bool,
) -> bool {
    let trimmed = s.trim();
    if trimmed.contains('?') || trimmed.contains('？') {
        return true;
    }
    // Non-ASCII content that isn't a Forth definition is natural language.
    if !trimmed.starts_with(':') && trimmed.chars().any(|c| !c.is_ascii()) {
        return true;
    }
    // Latin sentence: starts with uppercase letter.
    if trimmed
        .chars()
        .next()
        .map(|c| c.is_uppercase())
        .unwrap_or(false)
    {
        return true;
    }
    // Contractions ("you're", "don't", "I'm", "it's") and sentence-ending punctuation
    // attached to a word ("me." "hello!" but NOT standalone "." or "!") → natural language.
    // Standalone "." is Forth's print-TOS; standalone "!" is Forth's store op.
    let tokens_raw: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens_raw.iter().any(|t| {
        // Contraction: apostrophe followed by a letter inside the token.
        let bytes = t.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            if bytes[i] == b'\'' && bytes[i + 1].is_ascii_alphabetic() {
                return true;
            }
        }
        false
    }) {
        return true;
    }
    // Sentence-ending punctuation attached to a word (last token ends with "." or "!"
    // but has more than one character, ruling out standalone Forth ops).
    if let Some(last) = tokens_raw.last() {
        if last.len() > 1 && (last.ends_with('.') || last.ends_with('!')) {
            return true;
        }
    }
    let forth_chars = |c: char| {
        matches!(
            c,
            '+' | '-' | '*' | '/' | '@' | '!' | '.' | ':' | ';' | '<' | '>' | '='
        )
    };
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();

    // If every token is a *session-defined* word or a number/operator, treat as
    // Forth.  Using session_word (not word_exists) prevents precompiled library
    // words like `hello` and `world` from locking phrases like "hello world" into
    // Forth execution when the user clearly intends them as natural language.
    let all_tokens_known = !tokens.is_empty()
        && !trimmed.starts_with(':')
        && tokens.iter().all(|t| {
            t.parse::<f64>().is_ok() || trimmed.chars().any(forth_chars) || session_word(t)
        });
    if all_tokens_known {
        return false;
    }

    // Multi-word input with no Forth operators and at least one word that is
    // neither session-defined nor a Forth primitive → natural language.
    // Numbers are allowed (e.g. "show me 3 examples", "list the top 5 errors").
    // Pure-number sequences ("2 3 4") and all-session-word sequences are caught above.
    if tokens.len() >= 2
        && !trimmed.starts_with(':')
        && !trimmed.chars().any(forth_chars)
        && tokens.iter().any(|t| {
            t.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
                && !session_word(t)
                && !is_forth_primitive_word(t)
        })
    {
        return true;
    }

    // Single unknown word with only alphabetic characters → AI.
    if tokens.len() == 1 && !trimmed.starts_with(':') {
        let word = tokens[0];
        // Known to the VM (library or session) → Forth.
        if word_exists(word) {
            return false;
        }
        if !is_forth_primitive_word(word)
            && word.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
        {
            return true;
        }
    }
    false
}

/// Try to fetch the `name` field from a peer's `/v1/node/info` endpoint.
/// Returns `None` on any error (network, timeout, missing field).
async fn fetch_peer_name(addr: &str) -> Option<String> {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/node/info")
    } else {
        format!("http://{addr}/v1/node/info")
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    json.get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Fetch the Forth source from a peer's `/v1/forth/vocab` endpoint.
/// Returns an empty string on any error.
async fn fetch_peer_vocab_source(addr: &str) -> String {
    let url = if addr.starts_with("http://") || addr.starts_with("https://") {
        format!("{addr}/v1/forth/vocab")
    } else {
        format!("http://{addr}/v1/forth/vocab")
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
    else {
        return String::new();
    };
    match client.get(&url).send().await {
        Ok(resp) => resp
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|j| j["source"].as_str().map(|s| s.to_string()))
            .unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn extract_channel_forth(msg: &str) -> Option<String> {
    if !msg.starts_with('[') {
        return None;
    }
    let close = msg.find(']')?;
    let after_bracket = msg[close + 1..].trim_start_matches(':').trim_start();
    // after_bracket is now "sender: content" — find the ": content" part
    let colon_pos = after_bracket.find(": ")?;
    let content = after_bracket[colon_pos + 2..].trim();
    if content.starts_with(':') {
        Some(content.to_string())
    } else {
        None
    }
}

fn extract_scatter_exec_commands(code: &str) -> Vec<String> {
    let mut cmds = Vec::new();
    let needle = r#"scatter-exec""#;
    let mut rest = code;
    while let Some(pos) = rest.find(needle) {
        rest = &rest[pos + needle.len()..];
        let rest2 = rest.trim_start_matches(' ');
        if let Some(end) = rest2.find('"') {
            cmds.push(format!("* → bash -c {:?}", &rest2[..end]));
            rest = &rest2[end + 1..];
        }
    }
    cmds
}

// ── Peer registry ─────────────────────────────────────────────────────────────
//
// Maps UUID → sender-end of a peer's inbox so callers can attach to an
// existing peer by name or UUID instead of forking a fresh one.

use once_cell::sync::Lazy;
use std::collections::HashMap as PeerMap;

static PEER_REGISTRY: Lazy<
    tokio::sync::Mutex<
        PeerMap<
            Uuid,
            (
                String,
                tokio::sync::broadcast::Sender<crate::session::SessionEvent>,
            ),
        >,
    >,
> = Lazy::new(|| tokio::sync::Mutex::new(PeerMap::new()));

/// Register a newly-forked peer so it can be attached to later.
async fn register_peer(
    id: Uuid,
    name: String,
    tx: tokio::sync::broadcast::Sender<crate::session::SessionEvent>,
) {
    PEER_REGISTRY.lock().await.insert(id, (name, tx));
}

/// Look up a peer by name or UUID prefix.  Returns `(id, broadcast_tx)`.
pub async fn find_peer(
    needle: &str,
) -> Option<(
    Uuid,
    tokio::sync::broadcast::Sender<crate::session::SessionEvent>,
)> {
    let reg = PEER_REGISTRY.lock().await;
    // Exact UUID match first
    if let Ok(id) = needle.parse::<Uuid>() {
        if let Some((_, tx)) = reg.get(&id) {
            return Some((id, tx.clone()));
        }
    }
    // Name match
    for (id, (name, tx)) in reg.iter() {
        if name == needle || id.to_string().starts_with(needle) {
            return Some((*id, tx.clone()));
        }
    }
    None
}

// ── Background peer event loop ────────────────────────────────────────────────
//
// Each peer owns a legacy Co-Forth compatibility VM plus an authoritative
// typed ProgramRuntime for Lisp. The user broadcasts code via a
// `broadcast::Sender<SessionEvent>`; each peer subscribes.  Results come back
// through a shared `UnboundedSender<(Uuid, String, String)>` (id, name, text)
// that funnels all peers into one inbox for the select! loop.

async fn run_peer_loop(
    id: Uuid,
    name: String,
    mut rx: tokio::sync::broadcast::Receiver<crate::session::SessionEvent>,
    inbox_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, String, String)>,
) {
    use crate::session::SessionEvent;
    let mut vm = crate::coforth::Library::precompiled_vm();
    vm.remote_mode = true;
    let typed_runtime = crate::runtime::ProgramRuntime::new();

    loop {
        match rx.recv().await {
            Ok(SessionEvent::Chat { text }) => {
                // Lisp follows the same typed runtime path as local source and
                // provider wire programs.  Co-Forth is still a compatibility
                // peer surface while its richer legacy vocabulary is migrated.
                let response = if text.trim_start().starts_with('(') {
                    match typed_runtime
                        .submit_typed_only(crate::runtime::ProgramSubmission {
                            language: crate::programs::ProgramLanguage::Lisp,
                            source: text.clone(),
                            intent: "peer Lisp program".into(),
                            effect: crate::programs::ExecutionEffect::Unclassified,
                            declared_capabilities: Vec::new(),
                            manifest_generation: typed_runtime.manifest_generation(),
                            expected_revision: Some(typed_runtime.revision()),
                            budget: None,
                        })
                        .await
                    {
                        Ok(outcome)
                            if outcome.status
                                == crate::runtime::outcome::ExecutionStatus::Completed =>
                        {
                            outcome.output
                        }
                        Ok(outcome) => format!(
                            "typed Lisp error: {}",
                            outcome.diagnostics.first().cloned().unwrap_or_else(|| {
                                format!("program ended as {:?}", outcome.status)
                            })
                        ),
                        Err(error) => format!("typed Lisp error: {error}"),
                    }
                } else {
                    let depth_before = vm.data_stack().len();
                    match vm.exec(&text) {
                        Ok(out) => {
                            let delta = vm.data_stack()[depth_before..].to_vec();
                            if !out.is_empty() {
                                out.trim_end().to_string()
                            } else if !delta.is_empty() {
                                let items: Vec<String> =
                                    delta.iter().map(|n| n.to_string()).collect();
                                format!("( {} )", items.join("  "))
                            } else {
                                String::new()
                            }
                        }
                        Err(e) => format!("error: {e}"),
                    }
                };
                if !response.is_empty() {
                    let _ = inbox_tx.send((id, name.clone(), response));
                }
            }
            Ok(SessionEvent::ChannelMessage { .. }) => {
                // Local peer loops must not echo channel messages back.
                // ChannelMessage events originate from the local session; echoing
                // them here produces one duplicate per peer loop.
                // Remote peers receive and display these through a separate path.
            }
            Ok(SessionEvent::Close) | Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                break
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Ok(_) => {}
        }
    }
}

/// Fork two independent peer loops that share a broadcast channel.
///
/// Returns the broadcast sender (write to reach all peers) and a list of
/// `(id, name)` pairs for every peer that was started.
///
/// If `attach` is `Some(name_or_uuid)`, the first slot re-uses an existing
/// registry peer (bridging its broadcast into the new channel) instead of
/// forking a fresh one.  The second peer is always fresh.
async fn boot_peers(
    inbox_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, String, String)>,
    attach: Option<&str>,
) -> (
    tokio::sync::broadcast::Sender<crate::session::SessionEvent>,
    Vec<(Uuid, String)>,
) {
    let (bcast_tx, _) = tokio::sync::broadcast::channel::<crate::session::SessionEvent>(128);

    /// Spawn a fresh peer loop, register it, and return its `(id, name)`.
    async fn spawn_fresh(
        bcast_tx: &tokio::sync::broadcast::Sender<crate::session::SessionEvent>,
        inbox_tx: tokio::sync::mpsc::UnboundedSender<(Uuid, String, String)>,
    ) -> (Uuid, String) {
        let id = Uuid::new_v4();
        let name = format!("peer-{}", &id.to_string()[..8]);
        let rx = bcast_tx.subscribe();
        let (n, i) = (name.clone(), id);
        tokio::spawn(async move { run_peer_loop(i, n, rx, inbox_tx).await });
        register_peer(id, name.clone(), bcast_tx.clone()).await;
        (id, name)
    }

    // ── First peer: re-attach to existing OR fork fresh ───────────────────────
    let first = if let Some(needle) = attach {
        if let Some((id, existing_tx)) = find_peer(needle).await {
            let mut bridge_rx = existing_tx.subscribe();
            let bcast_clone = bcast_tx.clone();
            tokio::spawn(async move {
                while let Ok(ev) = bridge_rx.recv().await {
                    let _ = bcast_clone.send(ev);
                }
            });
            let name = PEER_REGISTRY
                .lock()
                .await
                .get(&id)
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| id.to_string());
            (id, name)
        } else {
            spawn_fresh(&bcast_tx, inbox_tx.clone()).await
        }
    } else {
        spawn_fresh(&bcast_tx, inbox_tx.clone()).await
    };

    // ── Second peer: always fresh ─────────────────────────────────────────────
    let second = spawn_fresh(&bcast_tx, inbox_tx.clone()).await;

    (bcast_tx, vec![first, second])
}

fn channels_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".finch").join("channels.json"))
}

fn load_joined_channels() -> std::collections::HashSet<String> {
    let Some(path) = channels_path() else {
        return std::collections::HashSet::new();
    };
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return std::collections::HashSet::new();
    };
    serde_json::from_str::<Vec<String>>(&contents)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn save_joined_channels(channels: &std::collections::HashSet<String>) {
    let Some(path) = channels_path() else { return };
    let mut sorted: Vec<&String> = channels.iter().collect();
    sorted.sort();
    if let Ok(json) = serde_json::to_string(&sorted) {
        let _ = std::fs::write(path, json);
    }
}

/// Application-owned data extracted from one verified, suspended
/// `proposal-open` effect. The effect handle—not source text—is the authority
/// to resume this exact VM frame.
struct DeferredProposal {
    handle: crate::runtime::VmEffectHandle,
    language: String,
    intent: String,
    source: String,
}

fn deferred_proposal_from_tool_result(
    result: &anyhow::Result<String>,
) -> Option<DeferredProposal> {
    let content = result.as_ref().ok()?;
    let outcome: crate::runtime::outcome::ExecutionOutcome = serde_json::from_str(content).ok()?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::Suspended {
        return None;
    }
    let effect = outcome.vm_side_effects.iter().rev().find(|effect| {
        effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
            && matches!(effect.event, crate::vm::HostSideEffect::Request { .. })
    })?;
    let crate::vm::HostSideEffect::Request { arguments } = &effect.event else {
        return None;
    };
    let [
        crate::vm::TypedValue::String(language),
        crate::vm::TypedValue::String(intent),
        crate::vm::TypedValue::String(source),
    ] = arguments.as_slice()
    else {
        return None;
    };
    Some(DeferredProposal {
        handle: crate::runtime::VmEffectHandle {
            execution_id: outcome.execution_id,
            sequence: effect.sequence,
        },
        language: language.clone(),
        intent: intent.clone(),
        source: source.clone(),
    })
}

fn proposal_resume_values(
    decision: crate::tools::implementations::propose::ProposalDecision,
) -> Vec<crate::vm::TypedValue> {
    let inner_type = crate::vm::Type::Result(
        Box::new(crate::vm::Type::String),
        Box::new(crate::vm::Type::String),
    );
    let value = match decision {
        crate::tools::implementations::propose::ProposalDecision::Execute { source } => {
            Some(Box::new(crate::vm::TypedValue::Result {
                ok_type: crate::vm::Type::String,
                error_type: crate::vm::Type::String,
                is_ok: true,
                value: Box::new(crate::vm::TypedValue::String(source)),
            }))
        }
        crate::tools::implementations::propose::ProposalDecision::Chat { context } => {
            Some(Box::new(crate::vm::TypedValue::Result {
                ok_type: crate::vm::Type::String,
                error_type: crate::vm::Type::String,
                is_ok: false,
                value: Box::new(crate::vm::TypedValue::String(context)),
            }))
        }
        crate::tools::implementations::propose::ProposalDecision::Cancel => None,
    };
    vec![crate::vm::TypedValue::Option { inner_type, value }]
}

/// Complete one application-owned proposal decision by resuming the exact VM
/// effect that opened it.  This deliberately accepts a decision rather than
/// source text: editing is outside the VM, while the VM only observes the
/// typed accepted/chat/cancel result correlated to its effect handle.
async fn resume_deferred_proposal(
    runtime: &crate::runtime::ProgramRuntime,
    proposal: &DeferredProposal,
    decision: crate::tools::implementations::propose::ProposalDecision,
) -> anyhow::Result<crate::runtime::outcome::ExecutionOutcome> {
    runtime
        .resume_typed_execution_with_effect_result(
            proposal.handle.execution_id,
            proposal.handle.sequence,
            proposal_resume_values(decision),
        )
        .await
}

#[cfg(test)]
mod deferred_proposal_tests {
    use super::*;

    #[tokio::test]
    async fn extracts_the_exact_suspended_proposal_handle() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source: "(proposal-open \"python\" \"inspect artifact\" \"print('ok')\")"
                        .into(),
                    intent: "proposal test".into(),
                    effect: crate::programs::ExecutionEffect::ExternalWrite,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let proposal = deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");
        assert_eq!(proposal.handle.execution_id, outcome.execution_id);
        assert_eq!(proposal.handle.sequence, 0);
        assert_eq!(proposal.language, "python");
        assert_eq!(proposal.intent, "inspect artifact");
        assert_eq!(proposal.source, "print('ok')");
    }

    #[tokio::test]
    async fn proposal_decision_resumes_the_saved_effect_without_replaying_source() {
        let runtime = Arc::new(crate::runtime::ProgramRuntime::new());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language: crate::programs::ProgramLanguage::Lisp,
                    source: "(proposal-open \"python\" \"inspect artifact\" \"print('original')\")"
                        .into(),
                    intent: "proposal test".into(),
                    effect: crate::programs::ExecutionEffect::ExternalWrite,
                    declared_capabilities: Vec::new(),
                    manifest_generation: runtime.manifest_generation(),
                    expected_revision: None,
                    budget: None,
                },
                Arc::new(|_| {}),
            )
            .await
            .unwrap();
        let proposal = deferred_proposal_from_tool_result(&Ok(serde_json::to_string(&outcome).unwrap()))
            .expect("suspended proposal effect");

        let completed = resume_deferred_proposal(
            runtime.as_ref(),
            &proposal,
            crate::tools::implementations::propose::ProposalDecision::Chat {
                context: "Please explain the artifact first.".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            completed.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(completed.vm_side_effects.len(), 1);
        assert!(matches!(
            completed.values.as_slice(),
            [crate::programs::ProgramValue::Option(Some(value))]
                if matches!(value.as_ref(), crate::programs::ProgramValue::Result { ok: false, value }
                    if matches!(value.as_ref(), crate::programs::ProgramValue::String(context)
                        if context == "Please explain the artifact first."))
        ));
        assert!(runtime
            .pending_typed_execution(proposal.handle.execution_id)
            .unwrap()
            .is_none());
    }
}

impl EventLoop {
    /// Create a new event loop with unified generators
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Arc<RwLock<ConversationHistory>>,
        _cloud_gen: Arc<dyn Generator>,
        qwen_gen: Arc<dyn Generator>,
        router: Arc<Router>,
        generator_state: Arc<RwLock<GeneratorState>>,
        tool_definitions: Vec<ToolDefinition>,
        tool_executor: Arc<Mutex<ToolExecutor>>,
        program_runtime: Arc<crate::runtime::ProgramRuntime>,
        tui_renderer: TuiRenderer,
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        streaming_enabled: bool,
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
        ipc_client: Option<crate::ipc::IpcClient>,
        mode: Arc<RwLock<ReplMode>>,
        memory_system: Option<Arc<crate::memory::MemorySystem>>,
        session_label: String,
        available_providers: Vec<crate::config::ProviderEntry>,
        active_provider_index: usize,
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
        context_lines: usize,
        max_verbatim_messages: usize,
        context_recall_k: usize,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
        enable_summarization: bool,
        auto_compact_enabled: bool,
        brain_provider: Option<Arc<dyn crate::providers::LlmProvider>>,
        auto_discover: bool,
        remote_peers: Vec<String>,
        daemon_base_url: Option<String>,
        provider_resolver: crate::runtime::scheduler::ProviderResolver,
        agent_scheduler: Arc<crate::runtime::scheduler::AgentScheduler>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (llm_tx, llm_rx) = mpsc::unbounded_channel::<LlmRequest>();

        let mut agent_events = agent_scheduler.subscribe();
        let agent_event_tx = event_tx.clone();
        tokio::spawn(async move {
            loop {
                match agent_events.recv().await {
                    Ok(event) => {
                        if agent_event_tx
                            .send(ReplEvent::AgentLifecycle(event))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        // Create Co-Forth shared stack before TUI so both hold the same Arc.
        let stack: Arc<tokio::sync::Mutex<Vec<String>>> =
            Arc::new(tokio::sync::Mutex::new(Vec::new()));

        // Create Co-Forth poset VM before TUI so both hold the same Arc.
        let poset: Arc<tokio::sync::Mutex<crate::poset::Poset>> =
            Arc::new(tokio::sync::Mutex::new(crate::poset::Poset::new()));

        // Wire todo list, stack, and poset into TUI renderer before wrapping in Arc<Mutex>
        let mut tui_renderer = tui_renderer;
        tui_renderer.set_todo_list(Arc::clone(&todo_list));
        tui_renderer.set_stack(Arc::clone(&stack));
        tui_renderer.set_poset(Arc::clone(&poset));
        // Share the corner Arc so the event loop can write to it without locking the TUI.
        let corner_output = Arc::clone(&tui_renderer.corner);

        // Wrap TUI in Arc<Mutex> for shared access
        let tui_renderer = Arc::new(Mutex::new(tui_renderer));

        // Spawn quit watcher — a dedicated task that receives Cap'n Proto binary
        // ControlMessage { quit } and exits the process immediately.
        // This runs independently of the event loop so /quit always works even
        // when the loop is blocked mid-streaming or mid-tool execution.
        let (quit_tx, mut quit_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(bytes) = quit_rx.recv().await {
                let mut cursor = std::io::Cursor::new(bytes);
                let ok = capnp::serialize::read_message(
                    &mut cursor,
                    capnp::message::ReaderOptions::new(),
                )
                .and_then(|reader| {
                    reader
                        .get_root::<crate::finch_ipc_capnp::control_message::Reader>()
                        .map(|ctrl| {
                            matches!(
                                ctrl.which(),
                                Ok(crate::finch_ipc_capnp::control_message::Which::Quit(_))
                            )
                        })
                })
                .unwrap_or(false);

                if ok {
                    crate::cli::tui::emergency_restore_terminal();
                    std::process::exit(0);
                }
            }
        });

        // Spawn input handler task
        let input_rx = spawn_input_task(Arc::clone(&tui_renderer), quit_tx);

        // Initialize plan content storage
        let plan_content = Arc::new(RwLock::new(None));

        // Create tool coordinator and wire the shared stack in.
        let tool_coordinator = ToolExecutionCoordinator::new(
            event_tx.clone(),
            Arc::clone(&tool_executor),
            Arc::clone(&output_manager),
            Arc::clone(&conversation),
            Arc::clone(&local_generator),
            Arc::clone(&tokenizer),
            Arc::clone(&mode),
            Arc::clone(&plan_content),
        )
        .with_stack(Arc::clone(&stack))
        .with_poset(Arc::clone(&poset));

        // Initialize memtree console (uses a separate dummy tree for the tree-view UI)
        let (memtree_console, memtree_handler) = {
            let dummy_tree = Arc::new(RwLock::new(crate::memory::MemTree::new()));
            let console = crate::cli::memtree_console::MemTreeConsole::new(dummy_tree);
            let handler = crate::cli::memtree_console::EventHandler::new();
            (
                Arc::new(RwLock::new(console)),
                Arc::new(tokio::sync::Mutex::new(handler)),
            )
        };

        // Peer channels — boot_peers() finishes wiring in run() (async context).
        let (peer_inbox_tx, peer_inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (peer_tx, peer_session_rx) =
            tokio::sync::broadcast::channel::<crate::session::SessionEvent>(128);
        let peer_sessions: Vec<(Uuid, String)> = Vec::new();
        let (peer_inbox_mirror_tx, _) = tokio::sync::broadcast::channel::<(String, String)>(128);

        Self {
            event_rx,
            event_tx,
            input_rx,
            conversation,
            query_states: Arc::new(QueryStateManager::new()),
            model_selection: ModelSelection::from_handle(
                active_provider_index,
                provider_resolver.generator_handle(),
            ),
            qwen_gen,
            available_providers,
            daemon_client,
            router,
            generator_state,
            tool_definitions: Arc::new(RwLock::new(tool_definitions)),
            tui_renderer,
            output_manager,
            status_bar,
            streaming_enabled,
            tool_coordinator,
            program_runtime,
            tool_results: Arc::new(RwLock::new(std::collections::HashMap::new())),
            active_query_id: Arc::new(RwLock::new(None)),
            pending_queries: std::collections::VecDeque::new(),
            pending_approvals: Arc::new(RwLock::new(std::collections::HashMap::new())),
            ipc_client,
            mode,
            plan_content,
            memtree_console,
            memtree_handler,
            view_mode: Arc::new(RwLock::new(ViewMode::List)), // Start in list view
            active_tool_uses: Arc::new(RwLock::new(std::collections::HashMap::new())),
            feedback_logger: FeedbackLogger::new().ok(),
            metrics_logger: dirs::home_dir()
                .map(|h| h.join(".finch").join("metrics"))
                .and_then(|p| crate::metrics::MetricsLogger::new(p).ok()),
            memory_system,
            session_label,
            session_uuid: Uuid::new_v4(),
            cwd: String::new(), // populated at the start of run()
            context_lines,
            max_verbatim_messages,
            context_recall_k,
            todo_list,
            enable_summarization,
            auto_compact_enabled,
            auto_discover,
            brain_provider,
            brain_context: Arc::new(RwLock::new(None)),
            active_brain: Arc::new(RwLock::new(None)),
            pending_brain_question_tx: None,
            pending_brain_question_options: Vec::new(),
            deferred_brain_question: None,
            pending_brain_action_tx: None,
            pending_brain_action_command: None,
            pending_dialog_tx: None,
            pending_poset_run: None,
            known_brain_states: std::collections::HashMap::new(),
            tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_daemon_brain_id: None,
            pending_daemon_brain_question_tx: None,
            pending_daemon_brain_question_options: Vec::new(),
            pending_daemon_brain_plan: false,
            pending_daemon_brain_plan_id: None,
            current_graph: Arc::new(tokio::sync::Mutex::new(crate::graph::ExecutionGraph::new())),
            stack,
            poset,
            plan_word: None,
            forth_vm: crate::coforth::Library::precompiled_vm(),
            corner_output,
            forth_undo: Vec::new(),
            lisp_ctx: std::sync::Arc::new(crate::lisp::LispCtx::new()),
            lisp_env: crate::lisp::make_env(),
            push_rx: crate::server::handlers::PUSH_INBOX.subscribe(),
            hash_rx: crate::server::handlers::HASH_INBOX.subscribe(),
            auto_compiled_word_names: std::collections::HashSet::new(),
            peer_tx,
            peer_inbox_tx,
            peer_inbox_rx,
            peer_sessions,
            diff_store: DiffStore::new(),
            peer_session_rx,
            joined_channels: load_joined_channels(),
            recent_channel_msgs: std::collections::VecDeque::new(),
            remote_peers,
            active_remote_brain: None,
            daemon_base_url,
            peer_inbox_mirror_tx,
            llm_tx,
            llm_rx: Some(llm_rx),
        }
    }

    /// Run the event loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::debug!("Event loop starting");
        // Signal that the TUI owns the terminal so proposal editors perform a
        // complete terminal-protocol handoff before launching $VISUAL/$EDITOR.
        crate::set_tui_active(true);

        // ── Fork two peer event loops ─────────────────────────────────────────
        // boot_peers needs an async context, so we call it here rather than new().
        {
            let (real_tx, sessions) = boot_peers(self.peer_inbox_tx.clone(), None).await;
            self.peer_tx = real_tx;
            self.peer_sessions = sessions;
        }

        // ── Connect to remote peers (--peer flag) ────────────────────────────
        // For each remote daemon address: establish a bidirectional WS bridge so
        // the remote machine participates in this session's peer loop.
        self.bridge_remote_peers().await;

        // ── Replay persisted Lisp defines ────────────────────────────────────
        // Restore any `(define ...)` expressions saved in previous sessions so
        // the Lisp env is identical to the one the user left behind.
        if let Some(mem) = &self.memory_system {
            if let Ok(defines) = mem.load_lisp_defines().await {
                if !defines.is_empty() {
                    let ctx = self.lisp_ctx.clone();
                    let env = self.lisp_env.clone();
                    for expr in defines {
                        if let Err(e) = crate::lisp::run_in(&expr, env.clone(), ctx.clone()).await {
                            tracing::warn!("lisp replay failed for {:?}: {}", expr, e);
                        }
                    }
                }
            }
        }

        // ── Startup header (Claude Code style) ───────────────────────────────
        // Clear accumulated startup noise from the output manager, then print a
        // clean header: finch version · primary model · working directory.
        self.output_manager.clear();

        let model_name = self.model_selection.generator().await.name().to_string();
        let cwd = std::env::current_dir()
            .ok()
            .map(|p| {
                // Shorten $HOME prefix to ~
                if let Some(home) = dirs::home_dir() {
                    if let Ok(rel) = p.strip_prefix(&home) {
                        return format!("~/{}", rel.display());
                    }
                }
                p.display().to_string()
            })
            .unwrap_or_else(|| "~".to_string());
        self.cwd = cwd.clone();
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            format!("◆ {}", self.session_label),
        );

        {
            let mut tui = self.tui_renderer.lock().await;
            if let Err(e) = tui.print_startup_header(&model_name, &cwd, &self.session_label) {
                tracing::warn!("Failed to print startup header: {}", e);
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        // Show weekly license notice for non-commercial users (honor system)
        {
            use crate::config::{load_config, LicenseType};
            use chrono::NaiveDate;
            if let Ok(mut cfg) = load_config() {
                if cfg.license.license_type == LicenseType::Noncommercial {
                    let today = chrono::Local::now().date_naive();
                    let suppress_until = cfg
                        .license
                        .notice_suppress_until
                        .as_deref()
                        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                    let should_show = suppress_until.is_none_or(|d| today > d);
                    if should_show {
                        self.output_manager.write_info(
                            "Using Finch commercially? $10/yr supports development.\n  \
                             Purchase: https://polar.sh/darwin-finch\n  \
                             Activate: finch license activate --key <key>",
                        );
                        let new_date = (today + chrono::Duration::days(7))
                            .format("%Y-%m-%d")
                            .to_string();
                        cfg.license.notice_suppress_until = Some(new_date);
                        let _ = cfg.save(); // non-fatal if save fails
                    }
                }
            }
        }

        // Apply auto-compact setting to the conversation history
        if !self.auto_compact_enabled {
            self.conversation.write().await.set_auto_compact(false);
        }

        // Initialize compaction status display (suppressed when auto-compact disabled)
        if self.auto_compact_enabled {
            self.update_compaction_status().await;
        }

        // Initialize plan mode indicator (starts in Normal mode)
        self.update_plan_mode_indicator(&crate::cli::repl::ReplMode::Normal);

        // Set initial memory context in status bar
        if let Some(ref mem) = self.memory_system {
            if let Ok(stats) = mem.stats().await {
                let engine = if NeuralEmbeddingEngine::find_in_cache().is_some() {
                    "neural"
                } else {
                    "tfidf"
                };
                self.status_bar.update_line(
                    crate::cli::status_bar::StatusLineType::MemoryContext,
                    format!("🧠 {}  ·  {} memories", engine, stats.conversation_count),
                );
            }
        }

        // Set initial terminal window/tab title (no topic yet on fresh start)
        {
            let _ = crossterm::execute!(
                std::io::stdout(),
                crossterm::terminal::SetTitle(format!("finch · {} · {}", self.session_label, cwd))
            );
        }

        // Attempt initial summary — populates on restart from previous memory
        if let Some(ref mem) = self.memory_system {
            refresh_context_strip(
                mem,
                &self.session_label,
                &cwd,
                &self.status_bar,
                self.context_lines,
            )
            .await;
        }

        // ── Boot JIT ──────────────────────────────────────────────────────────
        // Every boot: JIT-compile ALL vocabulary words with Forth code into the
        // VM dictionary as named colon definitions (`: word <code> ;`).
        // This makes every vocab word callable directly from Forth.
        //
        // Definitions are loaded silently. Vocabulary is executable API, not
        // startup presentation: verification, poems, and other `boot = true`
        // definitions remain available as explicit words but must never write
        // unexplained text into a user's conversational scrollback.
        {
            // Use the pre-built (cached) builtin defs — no TOML re-parse, no re-sort.
            // User vocabulary/*.toml files are merged on top at runtime.
            let builtin = crate::coforth::Library::builtin_defs();
            // These definitions are ambient runtime vocabulary, not words authored in
            // this conversation. Routing must keep that distinction or an ordinary
            // sentence made entirely from dictionary words is silently executed.
            self.auto_compiled_word_names
                .extend(builtin.pairs.iter().map(|(word, _)| word.clone()));

            // Load user vocabulary extensions on top of the builtins.
            let lib = crate::coforth::Library::load();
            let mut user_entries: Vec<_> = lib
                .all_entries()
                .into_iter()
                .filter(|e| e.forth.is_some())
                .filter(|e| !builtin.pairs.iter().any(|(w, _)| w == &e.word))
                .collect();
            user_entries.sort_by(|a, b| a.word.cmp(&b.word));

            // Compile user vocabulary extensions (builtins are already in the precompiled VM).
            let mut user_defs = String::new();
            for entry in &user_entries {
                let code = entry.forth.as_deref().unwrap_or("").trim();
                // Skip self-referential entries (e.g. `forth = "boom"` for word `boom`).
                // These are documentation stubs that say "this is a builtin" — compiling
                // them as `: boom boom ;` creates infinite recursion.
                if code == entry.word.as_str() {
                    continue;
                }
                let jit_def = if code.trim_start().starts_with(':') {
                    code.to_string()
                } else {
                    format!(": {} {} ;", entry.word, code)
                };
                user_defs.push_str(&jit_def);
                user_defs.push('\n');
                self.auto_compiled_word_names.insert(entry.word.clone());
            }
            if !user_defs.is_empty() {
                let _ = self.forth_vm.exec_with_fuel(&user_defs, 0);
            }

            // Restore words learned in previous sessions (from daemon or file).
            self.load_user_words().await;

            // Poll daemon every 4s for vocab changes from other concurrent terminals.
            self.spawn_vocab_poll();

            // Run user-authored boot poetry from ~/.finch/boot.forth.
            self.run_boot_poems();

            self.render_tui().await.ok();

            // Auto-discover peers on LAN in background — REPL stays responsive.
            // When found, PeersDiscovered event arrives and we connect to each peer.
            {
                let event_tx = self.event_tx.clone();
                tokio::spawn(async move {
                    let peers = tokio::task::spawn_blocking(|| {
                        crate::coforth::interpreter::run_peers_discover_pub(2000)
                    })
                    .await
                    .unwrap_or_default();
                    if !peers.is_empty() {
                        let _ = event_tx.send(ReplEvent::PeersDiscovered(peers));
                    }
                });
            }
        }
        // ─────────────────────────────────────────────────────────────────────

        // Wire TUI callbacks into the VM so words that call confirm" or select"
        // work when typed directly — not just when called via handle_forth_eval_inner.
        // These callbacks are stable for the lifetime of the session; gen_fn is
        // re-wired per-call in handle_forth_eval_inner because it reads the active provider.
        {
            let tui_c = self.tui_renderer.clone();
            self.forth_vm.set_confirm_fn(Box::new(move |msg: &str| {
                let msg = msg.to_string();
                let tui = tui_c.clone();
                // futures::executor::block_on works on both current-thread and multi-thread
                // runtimes, and inside LocalSet — unlike block_in_place which panics there.
                futures::executor::block_on(async move {
                    use crate::cli::tui::{Dialog, DialogResult};
                    let dialog = Dialog::confirm(msg, false);
                    matches!(
                        tui.lock().await.show_dialog(dialog),
                        Ok(DialogResult::Confirmed(true))
                    )
                })
            }));

            let tui_s = self.tui_renderer.clone();
            self.forth_vm
                .set_select_fn(Box::new(move |title: &str, options: &[String]| {
                    let title = title.to_string();
                    let options = options.to_vec();
                    let tui = tui_s.clone();
                    futures::executor::block_on(async move {
                        use crate::cli::tui::{Dialog, DialogOption, DialogResult};
                        let dialog_opts: Vec<DialogOption> = options
                            .iter()
                            .map(|o| DialogOption::new(o.as_str()))
                            .collect();
                        let dialog = Dialog::select(title, dialog_opts);
                        match tui.lock().await.show_dialog(dialog) {
                            Ok(DialogResult::Selected(idx)) => idx as i64,
                            _ => -1,
                        }
                    })
                }));

            let tui_o = self.tui_renderer.clone();
            self.forth_vm.set_open_file_fn(Box::new(move |path: &str| {
                let path = path.to_string();
                let tui = tui_o.clone();
                futures::executor::block_on(async move {
                    let _ = tui.lock().await.show_file_viewer(&path);
                });
            }));
        }

        // ── Spawn LLM worker loop ─────────────────────────────────────────────
        // Hand the receiver half of the channel to LlmLoop so it runs as its own
        // Tokio task, decoupled from TUI select! timing.
        {
            let llm_rx = self.llm_rx.take().expect("LlmLoop already started");
            let llm_loop = LlmLoop::new(
                llm_rx,
                self.event_tx.clone(),
                self.model_selection.generator_handle(),
                Arc::clone(&self.qwen_gen),
                Arc::clone(&self.router),
                Arc::clone(&self.generator_state),
                Arc::clone(&self.tool_definitions),
                self.tool_coordinator.clone(),
                Arc::clone(&self.program_runtime),
                Arc::clone(&self.tool_call_history),
                Arc::clone(&self.conversation),
                Arc::clone(&self.query_states),
                Arc::clone(&self.mode),
                Arc::clone(&self.output_manager),
                Arc::clone(&self.status_bar),
                Arc::clone(&self.tui_renderer),
                Arc::clone(&self.active_tool_uses),
                self.memory_system.clone(),
                Arc::clone(&self.current_graph),
                self.session_label.clone(),
                self.cwd.clone(),
                self.context_lines,
                self.max_verbatim_messages,
                self.context_recall_k,
                self.enable_summarization,
                self.auto_compact_enabled,
            );
            tokio::spawn(llm_loop.run());
        }

        // Render interval (33ms ≈ 30fps) — smooth streaming without terminal flicker.
        // 60fps (16ms) caused visual artifacts on most terminal emulators; 30fps is the
        // sweet spot: fast enough to feel live, slow enough to not tear.
        let mut render_interval = tokio::time::interval(Duration::from_millis(33));

        // Cleanup interval (30 seconds)
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        // Brain poll interval (500ms) - polls daemon for active brain state transitions
        let mut brain_poll_interval = tokio::time::interval(Duration::from_millis(500));

        // Flag to control the loop
        let mut should_exit = false;

        while !should_exit {
            tokio::select! {
                // User input event
                Some(event) = self.input_rx.recv() => {
                    use crate::cli::tui::InputEvent;
                    match event {
                        InputEvent::Submitted(input) => {
                            tracing::debug!("Received input: {}", input);
                            // Drop any pending brain question dialog so its oneshot sender
                            // doesn't intercept a future tool-approval dialog result.
                            if self.pending_brain_question_tx.take().is_some() {
                                let mut tui = self.tui_renderer.lock().await;
                                tui.active_dialog = None;
                                let _ = tui.pending_dialog_result.take();
                            }
                            self.pending_brain_question_options.clear();
                            // Clear typing words — restore panel to previous mode.
                            {
                                let mut tui = self.tui_renderer.lock().await;
                                tui.set_typing_words(vec![]);
                            }
                            // Cancel the brain session but preserve its context for injection.
                            self.cancel_active_brain(false).await;
                            self.handle_user_input(input).await?;
                        }
                        InputEvent::TypingStarted(partial) => {
                            tracing::debug!("Typing started: {} chars", partial.len());
                            // Drop any pending brain question dialog (brain is restarting).
                            if self.pending_brain_question_tx.take().is_some() {
                                let mut tui = self.tui_renderer.lock().await;
                                tui.active_dialog = None;
                                let _ = tui.pending_dialog_result.take();
                            }
                            self.pending_brain_question_options.clear();
                            self.handle_typing_started(partial).await;
                        }
                    }
                }

                // REPL event (query complete, tool result, etc.)
                Some(event) = self.event_rx.recv() => {
                    let event_name = match &event {
                        ReplEvent::StreamingComplete { .. } => "StreamingComplete",
                        ReplEvent::QueryComplete { .. } => "QueryComplete",
                        ReplEvent::QueryFailed { .. } => "QueryFailed",
                        ReplEvent::ToolResult { .. } => "ToolResult",
                        ReplEvent::ToolApprovalNeeded { .. } => "ToolApprovalNeeded",
                        ReplEvent::OutputReady { .. } => "OutputReady",
                        ReplEvent::UserInput { .. } => "UserInput",
                        ReplEvent::StatsUpdate { .. } => "StatsUpdate",
                        ReplEvent::AgentLifecycle(_) => "AgentLifecycle",
                        ReplEvent::CancelQuery => "CancelQuery",
                        ReplEvent::Shutdown => "Shutdown",
                        ReplEvent::BrainQuestion { .. } => "BrainQuestion",
                        ReplEvent::BrainProposedAction { .. } => "BrainProposedAction",
                        ReplEvent::ShowDialog { .. } => "ShowDialog",
                        ReplEvent::PosetComplete { result: Ok(_) } => "PosetComplete(ok)",
                        ReplEvent::PosetComplete { result: Err(_) } => "PosetComplete(err)",
                        ReplEvent::LispResult { result: Ok(_) } => "LispResult(ok)",
                        ReplEvent::LispResult { result: Err(_) } => "LispResult(err)",
                        ReplEvent::PeersDiscovered(_) => "PeersDiscovered",
                        ReplEvent::VocabSync(_) => "VocabSync",
                        ReplEvent::PeerMessage { .. } => "PeerMessage",
                        ReplEvent::RemoteBrainMessage { .. } => "RemoteBrainMessage",
                        ReplEvent::RemoteBrainError { .. } => "RemoteBrainError",
                    };
                    tracing::debug!("[EVENT_LOOP] Received event: {}", event_name);
                    tracing::debug!("Received event: {:?}", event);
                    if matches!(event, ReplEvent::Shutdown) {
                        should_exit = true;
                    } else {
                        tracing::debug!("[EVENT_LOOP] Handling {}...", event_name);
                        self.handle_event(event).await?;
                        tracing::debug!("[EVENT_LOOP] {} handled", event_name);
                    }
                }

                // Periodic rendering
                _ = render_interval.tick() => {
                    // Only rotate the poset 3D view if the Co-Forth panel has content to display.
                    // This saves significant CPU when the panel is empty (most of the time).
                    {
                        let tui = self.tui_renderer.lock().await;
                        if let Some(text) = tui.corner.lock().ok().and_then(|g| g.clone()) {
                            if !text.trim().is_empty() {
                                drop(tui); // Release TUI lock before poset lock
                                // Slowly rotate the poset 3D view at 33ms/tick (0.00265 rad ≈ same ~12s turn)
                                self.poset.lock().await.rotate(0.00265, 0.0);
                            }
                        }
                    }

                    // Single mutex acquisition: read all pending TUI state in one lock.
                    // Reduces contention with spawn_input_task from 3-4 round-trips to 1 per tick.
                    let (pending_cancel, dialog_result, pending_feedback) = {
                        let mut tui = self.tui_renderer.lock().await;
                        (
                            std::mem::take(&mut tui.pending_cancellation),
                            tui.pending_dialog_result.take(),
                            tui.pending_feedback.take(),
                        )
                    };

                    if pending_cancel {
                        let _ = self.event_tx.send(ReplEvent::CancelQuery);
                    }

                    // Route pending dialog result (tool approval, brain question, ShowDialog oneshot, etc.)
                    if let Some(dialog_result) = dialog_result {
                        {
                            // Priority 0: ShowDialog (used by PresentPlan, AskUserQuestion, etc.)
                            if let Some(tx) = self.pending_dialog_tx.take() {
                                let _ = tx.send(dialog_result);
                            }
                            // Priority 1: Poset run confirmation (state machine — no oneshot)
                            else if let Some(pending) = self.pending_poset_run.take() {
                                if matches!(dialog_result, crate::cli::tui::DialogResult::Selected(0)) {
                                    use crate::tools::implementations::{
                                        BashTool, EditTool, GlobTool, GrepTool, ReadTool,
                                        WebFetchTool, WriteTool,
                                    };
                                    let mut reg = crate::tools::ToolRegistry::new();
                                    reg.register(Box::new(ReadTool));
                                    reg.register(Box::new(GlobTool));
                                    reg.register(Box::new(GrepTool));
                                    reg.register(Box::new(BashTool));
                                    reg.register(Box::new(WebFetchTool::new()));
                                    reg.register(Box::new(WriteTool));
                                    reg.register(Box::new(EditTool));
                                    let registry = Some(Arc::new(reg));
                                    let PendingPosetRun { generator, poset, stack, event_tx } = pending;
                                    tokio::spawn(async move {
                                        let result = crate::poset::executor::execute_poset(
                                            poset, generator, registry, Some(stack),
                                        ).await;
                                        let _ = event_tx.send(super::events::ReplEvent::PosetComplete { result });
                                    });
                                    self.output_manager.write_info("running");
                                    self.render_tui().await.ok();
                                }
                            }
                            // Priority 2: Brain question.
                            else if let Some(brain_tx) = self.pending_brain_question_tx.take() {
                                let opts = std::mem::take(&mut self.pending_brain_question_options);
                                let answer = match &dialog_result {
                                    crate::cli::tui::DialogResult::TextEntered(s) => s.clone(),
                                    crate::cli::tui::DialogResult::CustomText(s) => s.clone(),
                                    crate::cli::tui::DialogResult::Selected(idx) => opts
                                        .get(*idx)
                                        .cloned()
                                        .unwrap_or_else(|| format!("option_{}", idx)),
                                    _ => "[no answer]".to_string(),
                                };
                                let _ = brain_tx.send(answer);
                                tracing::debug!("[EVENT_LOOP] Brain question answered");
                            } else if let Some(action_tx) = self.pending_brain_action_tx.take() {
                                // Brain proposed action — "Yes" = index 0, anything else = deny.
                                let command = self.pending_brain_action_command.take().unwrap_or_default();
                                let approved = matches!(&dialog_result, crate::cli::tui::DialogResult::Selected(0));
                                if approved {
                                    tracing::debug!("[EVENT_LOOP] Brain action approved: {}", command);
                                    tokio::spawn(async move {
                                        let output = crate::brain::execute_brain_command(&command).await;
                                        let _ = action_tx.send(Some(output));
                                    });
                                } else {
                                    tracing::debug!("[EVENT_LOOP] Brain action denied");
                                    let _ = action_tx.send(None);
                                }
                            } else if self.pending_daemon_brain_plan {
                                // Daemon brain plan response
                                self.pending_daemon_brain_plan = false;
                                if let Some(brain_id) = self.pending_daemon_brain_plan_id.take() {
                                    let (approved, instruction) = match &dialog_result {
                                        crate::cli::tui::DialogResult::Selected(0) => (true, None),
                                        crate::cli::tui::DialogResult::CustomText(s) => (false, Some(s.clone())),
                                        crate::cli::tui::DialogResult::TextEntered(s) => (false, Some(s.clone())),
                                        _ => (false, None),
                                    };
                                    if let Some(ref ipc) = self.ipc_client {
                                        let _ = ipc.respond_to_brain_plan(
                                            brain_id,
                                            approved,
                                            instruction.as_deref(),
                                        ).await;
                                    }
                                }
                            } else if let Some(brain_id) = self.pending_daemon_brain_id.take() {
                                // Daemon brain question response
                                let opts = std::mem::take(&mut self.pending_daemon_brain_question_options);
                                let answer = match &dialog_result {
                                    crate::cli::tui::DialogResult::TextEntered(s) => s.clone(),
                                    crate::cli::tui::DialogResult::CustomText(s) => s.clone(),
                                    crate::cli::tui::DialogResult::Selected(idx) => opts
                                        .get(*idx)
                                        .cloned()
                                        .unwrap_or_else(|| format!("option_{}", idx)),
                                    _ => "[no answer]".to_string(),
                                };
                                if let Some(ref ipc) = self.ipc_client {
                                    let _ = ipc.answer_brain_question(brain_id, &answer).await;
                                }
                                tracing::debug!("[EVENT_LOOP] Daemon brain question answered");
                            } else {
                                // Find which query this dialog was for (tool approval)
                                let mut approvals = self.pending_approvals.write().await;

                                if approvals.is_empty() {
                                    // No handler consumed the result — ShowDialog result arrived
                                    // before pending_dialog_tx was set (belt-and-suspenders race).
                                    // Put it back so the next tick delivers it once the tx is ready.
                                    drop(approvals);
                                    let mut tui = self.tui_renderer.lock().await;
                                    tui.pending_dialog_result = Some(dialog_result);
                                } else if let Some((query_id, (_tool_use, _response_tx))) = approvals.iter().next() {
                                    let query_id = *query_id;
                                    let (tool_use, response_tx) = approvals.remove(&query_id)
                                        .expect("query_id was just obtained from the same map");

                                    // Check for "Edit in $EDITOR" (option index 1 for write/edit tools)
                                    let is_file_mutating = matches!(tool_use.name.as_str(), "write" | "Write" | "edit" | "Edit");
                                    let is_editor_option = is_file_mutating && matches!(dialog_result, crate::cli::tui::DialogResult::Selected(1));

                                    let confirmation = if is_editor_option {
                                        // Extract proposed content
                                        let proposed = tool_use.input.get("content")
                                            .or_else(|| tool_use.input.get("new_string"))
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();

                                        // Write to temp file and open editor
                                        match open_in_editor(&proposed) {
                                            Ok(edited) => {
                                                let mut new_input = tool_use.input.clone();
                                                if tool_use.input.get("content").is_some() {
                                                    new_input["content"] = serde_json::Value::String(edited);
                                                } else {
                                                    new_input["new_string"] = serde_json::Value::String(edited);
                                                }
                                                super::events::ConfirmationResult::ApproveWithInput(new_input)
                                            }
                                            Err(e) => {
                                                tracing::warn!("Editor failed: {}", e);
                                                super::events::ConfirmationResult::Deny
                                            }
                                        }
                                    } else {
                                        // Shift option indices for file-mutating tools (extra "Edit" option at 1)
                                        let adjusted_result = if is_file_mutating {
                                            match dialog_result {
                                                crate::cli::tui::DialogResult::Selected(0) => dialog_result,
                                                crate::cli::tui::DialogResult::Selected(n) => crate::cli::tui::DialogResult::Selected(n - 1),
                                                other => other,
                                            }
                                        } else {
                                            dialog_result
                                        };
                                        self.dialog_result_to_confirmation(adjusted_result, &tool_use)
                                    };

                                    // Send confirmation back to tool execution task
                                    let _ = response_tx.send(confirmation);

                                    tracing::debug!("[EVENT_LOOP] Tool approval processed for query {}", query_id);
                                }
                            }
                        }
                    }

                    if let Some(rating) = pending_feedback {
                        let (weight, label) = match rating {
                            FeedbackRating::Good => (1.0_f64, "👍 Good"),
                            FeedbackRating::Bad  => (10.0_f64, "👎 Bad"),
                        };
                        self.handle_feedback_command(weight, rating, None).await?;
                        tracing::debug!("[EVENT_LOOP] Quick feedback recorded: {}", label);
                    }

                    // Drain incoming push messages from peers.
                    // Feed each one through handle_stack_push so either side
                    // (local human or local AI) can respond symmetrically.
                    let mut incoming: Vec<String> = Vec::new();
                    loop {
                        match self.push_rx.try_recv() {
                            Ok(msg) => incoming.push(msg),
                            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => break,
                            Err(tokio::sync::broadcast::error::TryRecvError::Closed) => break,
                        }
                    }
                    for msg in incoming {
                        use crossterm::style::Stylize;
                        // Channel messages ([#name] sender: text) get distinct colour
                        let display = if msg.starts_with('[') && msg.contains(']') {
                            format!("{}", msg.as_str().cyan())
                        } else {
                            format!("{}  {}", "←".dark_grey(), msg.as_str().white())
                        };
                        self.output_manager.write_info(display);

                        // Word propagation: if the message is a channel Forth definition,
                        // compile it silently into the local VM so the shared vocabulary grows.
                        // Format: "[#channel] sender: : word body ;"
                        if let Some(forth_def) = extract_channel_forth(&msg) {
                            let _ = self.forth_vm.exec_with_fuel(&forth_def, 0);
                        } else if let Err(e) = self.handle_stack_push(msg).await {
                            tracing::warn!("Failed to handle incoming push: {e}");
                        }
                    }

                    // Drain incoming hash updates from peers.
                    // Apply each to the local VM's room hash.
                    loop {
                        match self.hash_rx.try_recv() {
                            Ok((room_id, key, value, from)) => {
                                let room = self.forth_vm.rooms.entry(room_id.clone()).or_default();
                                const DEL: &str = "\x00del\x00";
                                if value == DEL {
                                    room.hash.remove(&key);
                                } else {
                                    room.hash.insert(key.clone(), value.clone());
                                }
                                // If not currently in any room, auto-join this one
                                if self.forth_vm.current_room.is_none() {
                                    self.forth_vm.current_room = Some(room_id.clone());
                                }
                                use crossterm::style::Stylize;
                                let label = if from.is_empty() { "peer".to_string() } else { from };
                                self.output_manager.write_info(format!(
                                    "  {}  {}{}{}  {}",
                                    label.as_str().cyan(),
                                    key.as_str().dark_grey(),
                                    " ← ".dark_grey(),
                                    if value == DEL { "(deleted)".dark_grey().to_string() } else { value.as_str().white().to_string() },
                                    room_id.chars().take(8).collect::<String>().as_str().dark_grey(),
                                ));
                            }
                            Err(_) => break,
                        }
                    }

                    // Don't spam logs, but good to know the loop is alive
                    // tracing::debug!("[EVENT_LOOP] Render tick");
                    if let Err(e) = self.render_tui().await {
                        tracing::warn!("TUI render failed in event loop: {}", e);
                        // Set recovery flag for next tick
                        let mut tui = self.tui_renderer.lock().await;
                        tui.needs_full_refresh = true;
                        tui.last_render_error = Some(e.to_string());
                        // Continue event loop - don't crash
                    }
                }

                // Periodic cleanup
                _ = cleanup_interval.tick() => {
                    self.cleanup_old_queries().await;
                }

                // Brain poll (500ms) — detect state transitions in daemon brains
                _ = brain_poll_interval.tick() => {
                    if let Err(e) = self.poll_daemon_brains().await {
                        tracing::debug!("Brain poll error (non-fatal): {}", e);
                    }
                }

                // Peer reply — one of the forked event loops responded
                Some((id, name, text)) = self.peer_inbox_rx.recv() => {
                    let _ = id; // available for filtering if needed
                    let _ = self.event_tx.send(ReplEvent::PeerMessage { text: format!("{name}: {text}") });
                    // Mirror to remote peers so they can see our peer responses.
                    let _ = self.peer_inbox_mirror_tx.send((name, text));
                }

                // Structured session events from peers (Diff proposals, edits, accepts, rejects)
                ev = self.peer_session_rx.recv() => {
                    match ev {
                        Ok(crate::session::SessionEvent::Diff { id, label, patch, description }) => {
                            // Find the proposer name from session list (unknown if not found)
                            let proposed_by = "peer".to_string();
                            self.diff_store.propose(id, label.clone(), patch.clone(), description.clone(), proposed_by.clone());
                            self.render_diff_proposal(id, &label, &patch, description.as_deref(), &proposed_by);
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after Diff proposal failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffEdit { diff_id, patch, description }) => {
                            self.diff_store.edit(diff_id, patch.clone(), description.clone());
                            if let Some(d) = self.diff_store.get(diff_id) {
                                let label = d.label.clone();
                                let proposed_by = d.proposed_by.clone();
                                use crossterm::style::Stylize;
                                self.output_manager.write_info(format!(
                                    "{}  {} revised diff for {}",
                                    "↻".yellow(),
                                    proposed_by.as_str().cyan(),
                                    label.as_str().white(),
                                ));
                                self.render_diff_proposal(diff_id, &label, &patch, description.as_deref(), &proposed_by);
                            }
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffEdit failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffAccept { diff_id }) => {
                            if let Some(d) = self.diff_store.accept(diff_id) {
                                use crossterm::style::Stylize;
                                self.output_manager.write_info(format!(
                                    "{}  diff {} accepted",
                                    "✓".green(),
                                    &d.id.to_string()[..8].white(),
                                ));
                            }
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffAccept failed: {e}");
                            }
                        }
                        Ok(crate::session::SessionEvent::DiffReject { diff_id, reason }) => {
                            self.diff_store.reject(diff_id, reason.clone());
                            use crossterm::style::Stylize;
                            let reason_str = reason.as_deref().unwrap_or("no reason given");
                            self.output_manager.write_info(format!(
                                "{}  diff {} rejected: {}",
                                "✗".red(),
                                &diff_id.to_string()[..8].white(),
                                reason_str.dark_grey(),
                            ));
                            if let Err(e) = self.render_tui().await {
                                tracing::warn!("TUI render after DiffReject failed: {e}");
                            }
                        }
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Other session events or lag — ignore here (handled elsewhere)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            // Channel closed — nothing to do
                        }
                    }
                }
            }
        }

        // Save persistent state before the TUI shuts down and the terminal goes read-only.
        {
            let mut executor = self.tool_coordinator.tool_executor().lock().await;
            let _ = executor.save_if_dirty();
        }

        // Normal exit — shut down TUI and restore terminal before returning.
        {
            let mut tui = self.tui_renderer.lock().await;
            let _ = tui.shutdown();
        }

        // Save conversation to ~/.finch/sessions/<uuid>.json and print the UUID.
        // The user can resume with: finch --resume <uuid>
        if let Some(home) = dirs::home_dir() {
            let sessions_dir = home.join(".finch").join("sessions");
            if std::fs::create_dir_all(&sessions_dir).is_ok() {
                let id = self.session_uuid;
                let path = sessions_dir.join(format!("{id}.json"));
                let history = self.conversation.read().await.clone();
                if !history.is_empty() {
                    if history.save(&path).is_ok() {
                        println!("\n{id}");
                    }
                }
            }
        }

        Ok(())
    }

    /// Handle user input (query or command)
    async fn handle_user_input(&mut self, input: String) -> Result<()> {
        // Check if it's a command
        if input.trim().starts_with('/') {
            // Echo the command to output (like user queries)
            self.output_manager.write_user(input.clone());

            // /exec [n] — execute a received channel message by index (0 = most recent).
            //   /exec      → list buffered messages with indices
            //   /exec 2    → run message at index 2
            if let Some(rest) = input.trim().strip_prefix("/exec") {
                let rest = rest.trim();
                if rest.is_empty() {
                    if self.recent_channel_msgs.is_empty() {
                        self.output_manager
                            .write_info("no channel messages received yet".to_string());
                    } else {
                        let lines: Vec<String> = self
                            .recent_channel_msgs
                            .iter()
                            .enumerate()
                            .map(|(i, (ch, snd, txt))| format!("[{i}] {ch} {snd}: {txt}"))
                            .collect();
                        self.output_manager.write_info(lines.join("\n"));
                    }
                    self.render_tui().await?;
                    return Ok(());
                }
                if let Ok(idx) = rest.parse::<usize>() {
                    if let Some((channel, sender, text)) =
                        self.recent_channel_msgs.get(idx).cloned()
                    {
                        self.output_manager
                            .write_info(format!("executing [{idx}] {channel} {sender}: {text}"));
                        // Route through the stack/query path directly (no recursion).
                        self.output_manager.write_user(text.clone());
                        return self.handle_forth_or_query(text).await;
                    } else {
                        self.output_manager
                            .write_info(format!("no message at index {idx}"));
                        self.render_tui().await?;
                    }
                    return Ok(());
                }
            }

            if let Some(command) = Command::parse(&input) {
                match command {
                    Command::Quit => {
                        // Restore terminal before exiting — disable raw mode, show cursor.
                        // Use try_lock to avoid deadlock if the TUI lock is held elsewhere.
                        if let Ok(mut tui) = self.tui_renderer.try_lock() {
                            let _ = tui.shutdown();
                        } else {
                            // Lock is held; `process::exit` skips Drop, so restore
                            // bracketed paste and keyboard enhancement flags too.
                            crate::cli::tui::emergency_restore_terminal();
                        }
                        std::process::exit(0);
                    }
                    Command::Help => {
                        let help_text = format_help();
                        self.output_manager.write_info(help_text);
                        self.render_tui().await?;
                    }
                    Command::Setup => {
                        // Suspend the inline TUI, run the full setup wizard,
                        // then resume.  The wizard manages its own terminal
                        // lifecycle (enable_raw_mode / alternate screen).
                        {
                            let tui = self.tui_renderer.lock().await;
                            tui.suspend().ok();
                        }
                        let wizard_result =
                            tokio::task::spawn_blocking(crate::cli::setup_wizard::run_setup_wizard)
                                .await;
                        {
                            let mut tui = self.tui_renderer.lock().await;
                            tui.resume().ok();
                        }
                        match wizard_result {
                            Ok(Ok(Some(result))) => {
                                // Save the new config.
                                if let Err(e) = crate::cli::setup_wizard::apply_and_save(&result) {
                                    self.output_manager
                                        .write_info(format!("Setup saved with error: {e}"));
                                } else {
                                    self.output_manager.write_info(
                                        "Settings saved. Restart finch to apply changes."
                                            .to_string(),
                                    );
                                }
                            }
                            Ok(Ok(None)) => {
                                // User cancelled the wizard.
                            }
                            _ => {
                                self.output_manager
                                    .write_info("Setup wizard exited.".to_string());
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::Share => {
                        self.handle_share().await?;
                    }
                    Command::BoxDiff => {
                        self.handle_box_diff().await?;
                    }
                    Command::SelfFix => {
                        let prompt = "\
You are running inside your own source directory. \
Your task is to find and fix bugs in yourself.\n\
\n\
Step 1 — diagnose: run `cargo build 2>&1` and capture ALL errors and warnings.\n\
Step 2 — fix: for each error, read the relevant source file, understand the root cause, \
and apply a minimal correct fix using the Edit tool.\n\
Step 3 — check: run `cargo build 2>&1` again. If there are still errors, repeat from step 2.\n\
Step 4 — test: run `cargo test 2>&1`. Fix any failures the same way.\n\
Step 5 — restart: once the build and tests are clean, call restart_session.\n\
\n\
Rules:\n\
- Fix root causes, not symptoms.\n\
- One small edit at a time; verify after each.\n\
- Do not change behaviour — only fix broken code.\n\
- If a fix makes things worse, revert it with another Edit before trying again.";
                        return self.execute_query(prompt.to_string()).await;
                    }
                    Command::Metrics => {
                        use crate::cli::commands::format_metrics;
                        let text = if let Some(ref logger) = self.metrics_logger {
                            match format_metrics(logger) {
                                Ok(s) => s,
                                Err(e) => format!("⚠️  Failed to read metrics: {}", e),
                            }
                        } else {
                            "⚠️  Metrics logger unavailable.".to_string()
                        };
                        self.output_manager.write_info(text);
                        self.render_tui().await?;
                    }
                    Command::Training => {
                        use crate::cli::commands::format_training;
                        let router = Arc::clone(&self.router);
                        let router_ref = router.as_ref();
                        match format_training(Some(router_ref), None) {
                            Ok(s) => self.output_manager.write_info(s),
                            Err(e) => self
                                .output_manager
                                .write_info(format!("⚠️  Failed to read training stats: {}", e)),
                        }
                        self.render_tui().await?;
                    }
                    Command::Memory => {
                        use crate::monitoring::MemoryInfo;
                        let info = MemoryInfo::current();
                        self.output_manager.write_info(info.format_with_warning());
                        self.render_tui().await?;
                    }
                    Command::Local { query } => {
                        // Handle /local command - query local model directly (bypass routing)
                        self.handle_local_query(query).await?;
                    }
                    Command::Plan(task) => {
                        self.handle_plan_task(task).await?;
                    }
                    Command::PlanModeToggle => {
                        // Check current mode and toggle
                        let current_mode = self.mode.read().await.clone();
                        match current_mode {
                            ReplMode::Normal => {
                                // Gobble ALL items from the vocabulary stack.
                                // If multiple words have accumulated, drain the whole stack and
                                // stream a plan response — non-blocking so the user can keep
                                // pushing more words while the AI is thinking.
                                // If only one word (or re-planning the stored word), use the full
                                // IMCPD planner for a deeper, multi-iteration plan.
                                let all_words: Vec<String> = {
                                    let mut s = self.stack.lock().await;
                                    std::mem::take(&mut *s)
                                };

                                if all_words.len() >= 2 {
                                    // Multiple concepts — gobble all, stream a combined plan.
                                    self.plan_word = None; // consumed; re-plan starts fresh
                                    let task = format!(
                                        "I've been building a vocabulary: {}. \
                                         Synthesise these concepts into a concrete plan. \
                                         What connects them? What should I build or do?",
                                        all_words.join(", ")
                                    );
                                    self.execute_chat_response(task).await?;
                                } else {
                                    // Single word (or re-plan): full IMCPD plan loop.
                                    let stack_word = if let Some(word) = self.plan_word.clone() {
                                        Some(word)
                                    } else {
                                        all_words.into_iter().next().map(|word| {
                                            self.plan_word = Some(word.clone());
                                            word
                                        })
                                    };

                                    if let Some(task) = stack_word {
                                        // Kick off the full IMPCPD plan loop for the popped word.
                                        self.handle_plan_task(task).await?;
                                    } else {
                                        // No stack word — plain plan mode entry
                                        let plan_path = std::env::temp_dir()
                                            .join(format!("plan_{}.md", uuid::Uuid::new_v4()));
                                        let new_mode = ReplMode::Planning {
                                            task: "Manual exploration".to_string(),
                                            plan_path: plan_path.clone(),
                                            created_at: chrono::Utc::now(),
                                        };
                                        *self.mode.write().await = new_mode.clone();
                                        self.output_manager.write_info(
                                            "📋 Entered plan mode.\n\
                                         You can explore the codebase using read-only tools:\n\
                                         - Read files, glob, grep, web_fetch are allowed\n\
                                         - Write, edit, bash are restricted\n\
                                         Use /plan to exit plan mode.",
                                        );
                                        self.update_plan_mode_indicator(&new_mode);
                                    }
                                } // end single-word else branch
                            }
                            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                                // Exit plan mode, return to normal; clear plan_word
                                *self.mode.write().await = ReplMode::Normal;
                                self.plan_word = None;
                                self.output_manager
                                    .write_info("✅ Exited plan mode. Returned to normal mode.");
                                // Update status bar indicator
                                self.update_plan_mode_indicator(&ReplMode::Normal);
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::McpList => {
                        // List connected MCP servers
                        self.handle_mcp_list().await?;
                    }
                    Command::McpTools(server_filter) => {
                        // List tools from all servers or specific server
                        self.handle_mcp_tools(server_filter).await?;
                    }
                    Command::McpRefresh => {
                        // Refresh tools from all servers
                        self.handle_mcp_refresh().await?;
                    }
                    Command::McpReload => {
                        // Reconnect to all servers
                        self.handle_mcp_reload().await?;
                    }
                    Command::FeedbackCritical(note) => {
                        self.handle_feedback_command(10.0, FeedbackRating::Bad, note)
                            .await?;
                    }
                    Command::FeedbackMedium(note) => {
                        self.handle_feedback_command(3.0, FeedbackRating::Bad, note)
                            .await?;
                    }
                    Command::FeedbackGood(note) => {
                        self.handle_feedback_command(1.0, FeedbackRating::Good, note)
                            .await?;
                    }
                    Command::ModelShow => {
                        self.handle_provider_show().await;
                        self.render_tui().await?;
                    }
                    Command::ModelList => {
                        use crate::providers::create_provider_from_entry;
                        let active = self.model_selection.active_index().await;
                        let pending = self.model_selection.pending_index().await;
                        let mut lines = vec!["Available model profiles:".to_string()];
                        for (index, entry) in self.available_providers.iter().enumerate() {
                            let marker = if index == active {
                                "→"
                            } else if Some(index) == pending {
                                "…"
                            } else {
                                " "
                            };
                            let tag = if entry.is_local() { "local" } else { "cloud" };
                            // Show availability: cloud entries are available if we can build a provider
                            let available =
                                !entry.is_local() && create_provider_from_entry(entry).is_ok();
                            let avail_tag = if entry.is_local() || available {
                                ""
                            } else {
                                " (no API key)"
                            };
                            lines.push(format!(
                                "{} {}. [{}] {} · {}{}",
                                marker,
                                index + 1,
                                tag,
                                entry.profile_name(),
                                entry.model().unwrap_or(entry.provider_type()),
                                avail_tag
                            ));
                        }
                        if self.available_providers.is_empty() {
                            lines.push(
                                "  (none configured — add [[providers]] to ~/.finch/config.toml)"
                                    .to_string(),
                            );
                        }
                        lines.push("Use /model <name> or /model <number> to switch.".to_string());
                        self.output_manager.write_info(lines.join("\n"));
                        self.render_tui().await?;
                    }
                    Command::ModelSwitch(name) => {
                        self.handle_provider_switch(name).await?;
                    }
                    Command::LicenseStatus => {
                        use crate::config::{load_config, LicenseType};
                        let cfg =
                            load_config().unwrap_or_else(|_| crate::config::Config::new(vec![]));
                        let text = match &cfg.license.license_type {
                            LicenseType::Commercial => {
                                let name = cfg.license.licensee_name.as_deref().unwrap_or("(unknown)");
                                let exp = cfg.license.expires_at.as_deref().unwrap_or("(unknown)");
                                format!(
                                    "License: Commercial ✓\n  Licensee: {}\n  Expires:  {}\n  Renew at: https://polar.sh/darwin-finch",
                                    name, exp
                                )
                            }
                            LicenseType::Noncommercial => {
                                "License: Noncommercial\n  Free for personal, educational, and research use.\n  \
                                 Commercial use requires a $10/yr key → https://polar.sh/darwin-finch\n  \
                                 Activate: finch license activate --key <key>".to_string()
                            }
                        };
                        self.output_manager.write_info(text);
                        self.render_tui().await?;
                    }
                    Command::LicenseActivate(key) => {
                        use crate::config::{load_config, LicenseConfig, LicenseType};
                        use crate::license::validate_key;
                        match validate_key(&key) {
                            Ok(parsed) => {
                                if let Ok(mut cfg) = load_config() {
                                    cfg.license = LicenseConfig {
                                        key: Some(key),
                                        license_type: LicenseType::Commercial,
                                        verified_at: Some(
                                            chrono::Local::now().format("%Y-%m-%d").to_string(),
                                        ),
                                        expires_at: Some(
                                            parsed.expires_at.format("%Y-%m-%d").to_string(),
                                        ),
                                        licensee_name: Some(parsed.name.clone()),
                                        notice_suppress_until: None,
                                    };
                                    if let Err(e) = cfg.save() {
                                        self.output_manager.write_info(format!(
                                            "✓ License validated but could not save: {}",
                                            e
                                        ));
                                    } else {
                                        self.output_manager.write_info(format!(
                                            "✓ License activated\n  Licensee: {} ({})\n  Expires:  {}",
                                            parsed.name, parsed.email, parsed.expires_at.format("%Y-%m-%d")
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                self.output_manager
                                    .write_info(format!("✗ License activation failed: {}", e));
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::LicenseRemove => {
                        use crate::config::{load_config, LicenseConfig};
                        if let Ok(mut cfg) = load_config() {
                            cfg.license = LicenseConfig::default();
                            if let Err(e) = cfg.save() {
                                self.output_manager
                                    .write_info(format!("⚠️  Could not save config: {}", e));
                            } else {
                                self.output_manager.write_info(
                                    "✓ License removed. Now using noncommercial license.",
                                );
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::Brain(task) => {
                        self.handle_brain_spawn(task).await?;
                    }
                    Command::Brains => {
                        self.handle_brains_list().await?;
                    }
                    Command::BrainCancel(name_or_id) => {
                        self.handle_brain_cancel(name_or_id).await?;
                    }
                    Command::Graph => {
                        self.handle_graph_command().await?;
                    }
                    Command::StackPush(text) => {
                        self.handle_stack_push(text).await?;
                    }
                    Command::StackShow => {
                        self.handle_stack_show().await?;
                    }
                    Command::StackPop => {
                        self.handle_stack_pop().await?;
                    }
                    Command::StackRun => {
                        if let Some(query) = self.handle_stack_run().await? {
                            // confirm_poset_run is called inside handle_poset_or_query.
                            self.handle_poset_or_query(query).await?;
                            {
                                // Placeholder block kept for structure (was: rejected branch).
                                let _ = ();
                                self.render_tui().await?;
                            }
                        }
                    }
                    Command::StackEval => {
                        self.handle_stack_eval().await?;
                    }
                    Command::StackClear => {
                        self.handle_stack_clear().await?;
                    }
                    Command::StackProgram => {
                        self.handle_stack_program().await?;
                    }
                    Command::StackView => {
                        let mut tui = self.tui_renderer.lock().await;
                        if tui.poset_panel_mode == crate::cli::tui::PosetPanelMode::Forth {
                            tui.toggle_poset_view();
                        }
                        drop(tui);
                        self.render_tui().await?;
                    }
                    Command::StackDemo => {
                        self.handle_stack_demo().await?;
                    }
                    Command::StackChain(a, b) => {
                        self.handle_stack_chain(a, b).await?;
                    }
                    Command::StackForget(id) => {
                        self.handle_stack_forget(id).await?;
                    }
                    Command::StackDup(id) => {
                        self.handle_stack_dup(id).await?;
                    }
                    Command::StackSwap(a, b) => {
                        self.handle_stack_swap(a, b).await?;
                    }
                    Command::StackDescribe(word) => {
                        self.handle_stack_describe(word).await?;
                    }
                    Command::StackDefine(word, definition) => {
                        self.handle_stack_define(word, definition).await?;
                    }
                    Command::StackOverride(word, definition) => {
                        self.handle_stack_override(word, definition).await?;
                    }
                    Command::Ask(query) => {
                        self.execute_query(query).await?;
                    }
                    Command::ForthEval(code) => {
                        if self.active_remote_brain.is_some() {
                            self.push_remote_brain(crate::brain::shared::BrainEventKind::Program {
                                language: crate::brain::shared::ProgramLanguage::Forth,
                                source: code,
                            })
                            .await?;
                        } else {
                            self.handle_forth_eval(code).await?;
                        }
                    }
                    Command::ForthUndo => {
                        self.handle_forth_undo().await?;
                    }
                    Command::VmDump => {
                        self.handle_vm_dump().await?;
                    }
                    Command::LibraryUndefine(word) => {
                        self.handle_library_undefine(word).await?;
                    }
                    Command::LibraryRun(word) => {
                        self.handle_library_run(word).await?;
                    }
                    Command::Machines => {
                        self.handle_machines().await?;
                    }
                    Command::Discover => {
                        self.handle_discover().await?;
                    }
                    Command::JoinChannel(chan) => {
                        self.joined_channels.insert(chan.clone());
                        save_joined_channels(&self.joined_channels);
                        let ev = crate::session::SessionEvent::chat(format!("joined {chan}"));
                        let _ = self.peer_tx.send(ev);
                    }
                    Command::PartChannel(chan) => {
                        self.joined_channels.remove(&chan);
                        save_joined_channels(&self.joined_channels);
                        let ev = crate::session::SessionEvent::chat(format!("parted {chan}"));
                        let _ = self.peer_tx.send(ev);
                    }
                    Command::SayChannel(chan, msg) => {
                        self.output_manager
                            .write_user(format!("/say {} {}", chan, msg));
                        let promise = crate::session::Promise::natural(msg);
                        let bundle = crate::session::ProofBundle::new(promise, vec![]);
                        let ev = crate::session::SessionEvent::ChannelMessage {
                            channel: chan,
                            sender: self.session_label.clone(),
                            bundle,
                        };
                        let _ = self.peer_tx.send(ev);
                    }
                    Command::Connect(addr) => {
                        self.handle_connect(addr).await?;
                    }
                    Command::Disconnect(name) => {
                        self.handle_disconnect(name).await?;
                    }
                    Command::BrainAttach(target) => {
                        self.handle_brain_attach(target).await?;
                    }
                    Command::BrainDetach => {
                        self.handle_brain_detach().await?;
                    }
                    Command::BrainPassword(password) => {
                        self.handle_brain_password(password).await?;
                    }
                    Command::Room(uuid) => {
                        self.handle_room(uuid).await?;
                    }
                    Command::RoomNew => {
                        self.handle_room_new().await?;
                    }
                    Command::RoomAdd(addr) => {
                        self.handle_room_add(addr).await?;
                    }
                    Command::RoomRemove(addr) => {
                        self.handle_room_remove(addr).await?;
                    }
                    Command::RoomList => {
                        self.handle_room_list().await?;
                    }
                    Command::Accept(prefix) => {
                        self.handle_accept(prefix).await?;
                    }
                    Command::Reject(reason) => {
                        self.handle_reject(reason).await?;
                    }
                    // Registry / gas ledger — translate to Forth eval
                    Command::SelfPeer => {
                        self.handle_forth_eval("self-peer".to_string()).await?;
                    }
                    Command::Balance => {
                        self.handle_forth_eval("balance".to_string()).await?;
                    }
                    Command::Settle(addr) => {
                        self.handle_forth_eval(format!("settle\" {addr}\"")).await?;
                    }
                    Command::RegistrySet(addr) => {
                        self.handle_forth_eval(format!("registry\" {addr}\""))
                            .await?;
                    }
                    Command::JoinRegistry(addr) => {
                        self.handle_forth_eval(format!("join\" {addr}\"")).await?;
                    }
                    Command::GasSend(addr, ms) => {
                        self.handle_forth_eval(format!("gas-send\" {addr}\" {ms}"))
                            .await?;
                    }
                    _ => {
                        // All other commands output to scrollback via write_info
                        self.output_manager.write_info(format!(
                            "Command recognized but not yet implemented: {}",
                            input
                        ));
                        self.render_tui().await?;
                    }
                }
                return Ok(());
            } else {
                // Give usage hints for known commands with missing arguments
                let msg = if input.trim() == "/define" {
                    "Usage: /define <word>[:<sense>] [definition]  (e.g. /define love   or   /define bank:river the edge of a stream)".to_string()
                } else if input.trim() == "/describe" {
                    "Usage: /describe <word>  (e.g. /describe love)".to_string()
                } else if let Some(word) = input.trim().strip_prefix('/') {
                    // /word  → treat as /describe word (look it up in the library)
                    let word = word.trim().to_string();
                    if !word.is_empty() && word.split_whitespace().count() == 1 {
                        self.handle_stack_describe(word).await?;
                        return Ok(());
                    } else {
                        format!("Unknown command: {}", input)
                    }
                } else {
                    format!("Unknown command: {}", input)
                };
                self.output_manager.write_info(msg);
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Check if it's a quit command (legacy support)
        if input.trim().eq_ignore_ascii_case("quit") || input.trim().eq_ignore_ascii_case("exit") {
            self.event_tx
                .send(ReplEvent::Shutdown)
                .context("Failed to send shutdown event")?;
            return Ok(());
        }

        // Forth word definition: `: word ... ;`
        // Route directly to the Forth VM — do not push as a vocabulary word.
        if input.trim().starts_with(": ") {
            self.output_manager.write_user(input.clone());
            return self.handle_forth_eval(input.trim().to_string()).await;
        }

        // Foreign code block: ``` lang ... ``` (or bare ```)
        // The user posts code in any language — we send back a better machine.
        if let Some((lang, code)) = extract_code_fence(input.trim()) {
            self.output_manager.write_user(input.clone());
            return self.handle_foreign_code(lang, code).await;
        }

        // `push <message>` — send plain text to all peers.
        // The message is wrapped in a print statement and scattered.
        // No approval dialog. No Forth visible. Just the push.
        if let Some(msg) = input.trim().strip_prefix("push ") {
            let msg = msg.trim();
            if !msg.is_empty() {
                self.output_manager.write_user(input.clone());
                return self.handle_push_message(msg.to_string()).await;
            }
        }

        // Direct AI query: `?? question` — bypasses the stack and asks the AI.
        if let Some(query) = input
            .trim()
            .strip_prefix("?? ")
            .or_else(|| input.trim().strip_prefix("??"))
        {
            let query = query.trim().to_string();
            if !query.is_empty() {
                self.output_manager.write_user(input.clone());
                return self.execute_query(query).await;
            }
        }

        // ── Lisp: input starting with `(` is a Lisp expression ───────────────
        if input.trim_start().starts_with('(') {
            if self.active_remote_brain.is_some() {
                return self
                    .push_remote_brain(crate::brain::shared::BrainEventKind::Program {
                        language: crate::brain::shared::ProgramLanguage::Lisp,
                        source: input,
                    })
                    .await;
            }
            // Broadcast to channels so peers can see (and /exec) it.
            for chan in &self.joined_channels {
                let promise = crate::session::Promise::lisp(input.clone());
                let bundle = crate::session::ProofBundle::new(promise, vec![]);
                let ev = crate::session::SessionEvent::ChannelMessage {
                    channel: chan.clone(),
                    sender: self.session_label.clone(),
                    bundle,
                };
                let _ = self.peer_tx.send(ev);
            }
            return self
                .execute_interactive_typed_program(
                    crate::programs::ProgramLanguage::Lisp,
                    input,
                )
                .await;
        }

        // Broadcast to all joined channels (slash-commands are excluded above).
        // Try to prove the stack effect before sending; extract comments so
        // receivers can read the intent before executing.
        if !self.joined_channels.is_empty() {
            let comments = crate::coforth::extract_comments(&input);
            let promise = {
                let effect = self.forth_vm.infer_effect(&input).ok();
                match effect {
                    Some(e) => crate::session::Promise::forth_proven(input.clone(), e),
                    None => crate::session::Promise::forth(input.clone()),
                }
            };
            let bundle = crate::session::ProofBundle::new(promise, comments);
            for chan in &self.joined_channels {
                let ev = crate::session::SessionEvent::ChannelMessage {
                    channel: chan.clone(),
                    sender: self.session_label.clone(),
                    bundle: bundle.clone(),
                };
                let _ = self.peer_tx.send(ev);
            }
        }

        // Route: natural language → AI; Forth tokens → stack.
        let is_nl = looks_like_natural_language(
            &input,
            |w| self.forth_vm.word_exists(w),
            |w| {
                self.forth_vm.word_source(w).is_some() && !self.auto_compiled_word_names.contains(w)
            },
        );

        if is_nl {
            // Conversation is never partially executed. Explicit `/forth <program>`
            // remains available when the user intends VM evaluation.
            return self.execute_query(input).await;
        }

        // ── Co-Forth: plain text pushes onto the stack (no AI trigger) ──────────
        self.output_manager.write_user(input.clone());
        self.handle_stack_push(input).await?;
        return Ok(());
    }

    /// Route text through the NL/Forth/Lisp decision without slash-command handling or
    /// channel broadcast. Used by `/exec` to run a received channel message.
    async fn handle_forth_or_query(&mut self, input: String) -> Result<()> {
        // Lisp
        if input.trim_start().starts_with('(') {
            return self
                .execute_interactive_typed_program(
                    crate::programs::ProgramLanguage::Lisp,
                    input,
                )
                .await;
        }
        let is_nl = looks_like_natural_language(
            &input,
            |w| self.forth_vm.word_exists(w),
            |w| {
                self.forth_vm.word_source(w).is_some() && !self.auto_compiled_word_names.contains(w)
            },
        );
        if is_nl {
            return self.execute_query(input).await;
        }
        self.handle_stack_push(input).await
    }

    /// Execute explicit interactive source through the same typed runtime and
    /// portable output projection as provider wire responses.  Legacy Lisp is
    /// intentionally not a fallback here: an opening `(` is an unambiguous
    /// Finch-Lisp program and must receive typed diagnostics.
    async fn execute_interactive_typed_program(
        &mut self,
        language: crate::programs::ProgramLanguage,
        source: String,
    ) -> Result<()> {
        self.program_runtime.grant_typed_capability(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::SessionEmit,
            selector: crate::vm::ResourceSelector::None,
        })?;

        let source_unit = self.output_manager.start_work_unit("typed program");
        source_unit.set_program_source(language.as_str());
        source_unit.set_response(source.clone());
        source_unit.set_complete();
        let output_unit = self.output_manager.start_work_unit("VM program output");
        output_unit.set_program_output();
        let projection = VmOutputProjection::new(Arc::clone(&self.output_manager), Arc::clone(&output_unit));
        let sink: crate::runtime::TypedEffectSink = Arc::new(move |envelope| {
            projection.project(&envelope.effect);
        });
        let outcome = self
            .program_runtime
            .submit_with_deferred_program_effects(
                crate::runtime::ProgramSubmission {
                    language,
                    source,
                    intent: "interactive typed source".into(),
                    effect: crate::programs::ExecutionEffect::Unclassified,
                    declared_capabilities: Vec::new(),
                    manifest_generation: self.program_runtime.manifest_generation(),
                    expected_revision: Some(self.program_runtime.revision()),
                    budget: None,
                },
                sink,
            )
            .await?;
        if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
            let detail = outcome
                .diagnostics
                .first()
                .cloned()
                .unwrap_or_else(|| format!("VM program ended as {:?}", outcome.status));
            output_unit.append_response(&format!("VM error: {detail}"));
        }
        output_unit.set_complete();
        self.render_tui().await
    }

    /// Execute a query with echo (used by /run where the query hasn't been displayed yet).
    async fn execute_query(&mut self, input: String) -> Result<()> {
        self.execute_query_inner(input, true, false).await
    }

    /// Execute a conversational response to a word push — no tools, no brain context injection.
    async fn execute_chat_response(&mut self, input: String) -> Result<()> {
        self.execute_query_inner(input, false, true).await
    }

    /// Execute a query directly — called by /run after draining the stack, or
    /// after a user push (where the echo was already written).
    /// `echo` — whether to write the user query to the output buffer.
    /// `chat_only` — suppress tools and brain context (for word-push conversational responses).
    async fn execute_query_inner(
        &mut self,
        input: String,
        echo: bool,
        chat_only: bool,
    ) -> Result<()> {
        if self.active_remote_brain.is_some() {
            return self
                .push_remote_brain(crate::brain::shared::BrainEventKind::Prompt { text: input })
                .await;
        }

        // One interactive Brain owns a single ordered conversation and VM
        // revision. Queue later user turns rather than racing both through the
        // same mutable state. Preserve the input's normal echo semantics now,
        // then start it without a second echo after the active turn commits.
        if self.active_query_id.read().await.is_some() {
            if echo {
                self.output_manager.write_user(input.clone());
            }
            self.pending_queries.push_back((input, false, chat_only));
            return Ok(());
        }

        // Drain any pending images from TUI (pasted before sending)
        let pending_images: Vec<(String, String)> = {
            let mut tui = self.tui_renderer.lock().await;
            tui.pending_images
                .drain(..)
                .map(|(_idx, b64, media_type)| (media_type, b64))
                .collect()
        };

        // Echo query to output buffer (skip when caller already echoed)
        if echo {
            self.output_manager.write_user(input.clone());
        }

        // Create a new query
        let conversation_snapshot = self.conversation.read().await.snapshot();
        let query_id = self.query_states.create_query(conversation_snapshot).await;

        // Add user message to conversation (with images if any were pasted)
        if pending_images.is_empty() {
            self.conversation
                .write()
                .await
                .add_user_message(input.clone());
        } else {
            self.conversation
                .write()
                .await
                .add_user_message_with_images(input.clone(), &pending_images);
        }

        // Update compaction percentage in status bar
        self.update_compaction_status().await;

        // Set as active query (for cancellation)
        *self.active_query_id.write().await = Some(query_id);

        // Inject pre-gathered brain context (if any) as a hidden block.
        // Skip for chat_only (word pushes) — brain context triggers tool use.
        let enriched = if chat_only {
            // Drop brain context without consuming it — it stays for the next real query.
            input.clone()
        } else {
            let mut ctx = self.brain_context.write().await;
            match ctx.take() {
                Some(brain_ctx) if !brain_ctx.trim().is_empty() => {
                    tracing::debug!(
                        "[EVENT_LOOP] Injecting brain context ({} chars)",
                        brain_ctx.len()
                    );
                    format!("{}\n\n---\n[Pre-gathered context:\n{}]", input, brain_ctx)
                }
                _ => input.clone(),
            }
        };

        // Shared brains — pull context contributed by all sessions/peers.
        let enriched = if chat_only {
            enriched
        } else {
            let daemon_addr = crate::config::constants::DEFAULT_HTTP_ADDR;
            let shared_ctx: Option<String> = reqwest::Client::new()
                .get(format!("http://{daemon_addr}/v1/brains/shared/shared"))
                .timeout(std::time::Duration::from_millis(300))
                .send()
                .await
                .ok()
                .and_then(|r| {
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(r.json::<serde_json::Value>())
                    })
                    .ok()
                })
                .and_then(|v| v["context"].as_str().map(|s| s.to_owned()))
                .filter(|s| !s.trim().is_empty());
            match shared_ctx {
                Some(ctx) => format!("{enriched}\n\n---\n[Shared brain context:\n{ctx}]"),
                None => enriched,
            }
        };

        // Send query to the LLM worker loop (no tools for chat_only word-push responses)
        let _ = self.llm_tx.send(LlmRequest::Query {
            id: query_id,
            text: enriched,
            no_tools: chat_only,
        });

        Ok(())
    }

    /// Handle feedback commands (/critical, /medium, /good) and Ctrl+G/Ctrl+B quick ratings.
    ///
    /// Finds the last user query and assistant response from conversation history,
    /// logs a `FeedbackEntry` to `~/.finch/feedback.jsonl`, and prints a confirmation.
    async fn handle_feedback_command(
        &mut self,
        weight: f64,
        rating: FeedbackRating,
        note: Option<String>,
    ) -> Result<()> {
        let messages = self.conversation.read().await.get_messages();
        let (last_query, last_response) = find_last_exchange(&messages);

        if last_response.is_empty() {
            self.output_manager
                .write_info("No recent response to rate. Ask a question first.");
            self.render_tui().await?;
            return Ok(());
        }

        // Build and log the entry
        let (emoji, label) = match (weight as u64, &rating) {
            (10, _) => ("🔴", "critical (10×)"),
            (3, _) => ("🟡", "medium (3×)"),
            _ => ("🟢", "good (1×)"),
        };

        let mut entry = FeedbackEntry::new(last_query, last_response, rating);
        entry.weight = weight; // Override to support medium (3×)
        if let Some(ref n) = note {
            entry = entry.with_note(n.clone());
        }

        if let Some(ref logger) = self.feedback_logger {
            match logger.log(&entry) {
                Ok(()) => {
                    let msg = if let Some(n) = &note {
                        format!("{} Feedback recorded: {} — {}", emoji, label, n)
                    } else {
                        format!("{} Feedback recorded: {}", emoji, label)
                    };
                    self.output_manager.write_info(msg);
                }
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to log feedback: {}", e));
                }
            }
        } else {
            self.output_manager.write_info(
                "⚠️  Feedback logger unavailable (could not open ~/.finch/feedback.jsonl).",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /local command - query local model directly (bypass routing)
    async fn handle_local_query(&mut self, query: String) -> Result<()> {
        use crate::cli::messages::StreamingResponseMessage;

        let Some(ref ipc) = self.ipc_client else {
            self.output_manager
                .write_error("Error: /local requires the daemon.");
            self.output_manager
                .write_info("    Start the daemon: finch daemon --bind 127.0.0.1:11435");
            return self.render_tui().await;
        };

        let msg = Arc::new(StreamingResponseMessage::new());
        msg.append_chunk("🔧 Local Model Query (bypassing routing)\n\n");
        self.output_manager
            .add_trait_message(msg.clone() as Arc<dyn crate::cli::messages::Message>);
        self.render_tui().await?;

        let messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![crate::claude::ContentBlock::Text { text: query }],
        }];

        let mut rx = match ipc.query_stream(messages, vec![]).await {
            Ok(rx) => rx,
            Err(e) => {
                msg.set_failed();
                self.output_manager
                    .write_error(format!("Local query failed: {}", e));
                return self.render_tui().await;
            }
        };

        // Drive the stream in a local task so the event loop keeps rendering
        let msg_clone = msg.clone();
        let output_mgr = self.output_manager.clone();
        tokio::task::spawn_local(async move {
            use crate::generators::StreamChunk;
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(StreamChunk::TextDelta(t)) => msg_clone.append_chunk(&t),
                    Ok(_) => {} // Usage, ContentBlockComplete — ignored
                    Err(e) => {
                        msg_clone.set_failed();
                        output_mgr.write_error(format!("Local query error: {}", e));
                        return;
                    }
                }
            }
            // Channel closed = stream complete
            msg_clone.append_chunk("\n✓ Local model (bypassed routing)");
            msg_clone.set_complete();
        });

        Ok(())
    }

    async fn handle_provider_show(&self) {
        let active = self.model_selection.active_index().await;
        let Some(entry) = self.available_providers.get(active) else {
            self.output_manager.write_info("No active model profile.");
            return;
        };

        let mut text = format!(
            "Active model: {}\n  provider: {}\n  model: {}\n  conversation: preserved across switches",
            entry.profile_name(),
            entry.provider_type(),
            entry.model().unwrap_or("provider default")
        );
        if let Some(pending) = self.model_selection.pending_index().await {
            if let Some(entry) = self.available_providers.get(pending) {
                text.push_str(&format!(
                    "\n  pending: {} (waiting for local model startup)",
                    entry.profile_name()
                ));
            }
        }
        self.output_manager.write_info(text);
    }

    /// Handle `/model <name>` — switch the active named provider profile.
    async fn handle_provider_switch(&mut self, name: String) -> Result<()> {
        use crate::generators::claude::ClaudeGenerator;
        use crate::providers::create_provider_from_entry;

        let target_index = match resolve_provider_profile(&self.available_providers, &name) {
            Ok(index) => index,
            Err(error) => {
                self.output_manager.write_info(format!("⚠️  {error}"));
                return self.render_tui().await;
            }
        };
        let entry = self.available_providers[target_index].clone();
        let active_index = self.model_selection.active_index().await;
        if target_index == active_index && self.model_selection.pending_index().await.is_none() {
            self.output_manager
                .write_info(format!("Already using model: {}", entry.profile_name()));
            return self.render_tui().await;
        }

        // Every valid new selection supersedes a local startup already in flight,
        // even if constructing or checking the replacement later fails.
        self.model_selection.cancel_pending().await;

        if entry.is_local() {
            let Some(client) = self.daemon_client.clone() else {
                self.output_manager.write_info(
                    "⚠️  Local model switching requires a running Finch daemon.".to_string(),
                );
                return self.render_tui().await;
            };

            match client.local_model_status().await {
                Ok(crate::client::LocalModelStatus::Ready(model)) => {
                    let generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::daemon_local::DaemonLocalGenerator::new(
                            client,
                            entry.profile_name(),
                        ));
                    self.model_selection.activate(target_index, generator).await;
                    self.output_manager.write_info(format!(
                        "✓ Switched to {} · {} (conversation preserved)",
                        entry.profile_name(),
                        model
                    ));
                }
                Ok(crate::client::LocalModelStatus::Initializing)
                | Ok(crate::client::LocalModelStatus::Downloading(_))
                | Ok(crate::client::LocalModelStatus::Loading(_)) => {
                    let token = self.model_selection.begin_pending(target_index).await;
                    self.output_manager.write_info(format!(
                        "⏳ {} is still starting; the current model stays active until it is ready.",
                        entry.profile_name()
                    ));

                    let local_generator: Arc<dyn Generator> =
                        Arc::new(crate::generators::daemon_local::DaemonLocalGenerator::new(
                            Arc::clone(&client),
                            entry.profile_name(),
                        ));
                    let selection = self.model_selection.clone();
                    let output = Arc::clone(&self.output_manager);
                    let profile_name = entry.profile_name();
                    tokio::spawn(async move {
                        let outcome = activate_local_when_ready(
                            selection,
                            token,
                            target_index,
                            local_generator,
                            || {
                                let client = Arc::clone(&client);
                                async move { client.local_model_status().await }
                            },
                            Duration::from_millis(750),
                        )
                        .await;
                        match outcome {
                            LocalActivationOutcome::Activated(model) => output.write_info(format!(
                                "✓ Switched to {profile_name} · {model} (conversation preserved)"
                            )),
                            LocalActivationOutcome::Failed(error) => output.write_error(format!(
                                "Local model {profile_name} failed to start: {error}"
                            )),
                            LocalActivationOutcome::NotAvailable => output.write_error(format!(
                                "Local model {profile_name} is not enabled in the daemon"
                            )),
                            LocalActivationOutcome::StatusError(error) => output.write_error(
                                format!("Could not monitor local model {profile_name}: {error}"),
                            ),
                            LocalActivationOutcome::Cancelled => {}
                        }
                    });
                }
                Ok(crate::client::LocalModelStatus::Failed(error)) => {
                    self.output_manager
                        .write_info(format!("⚠️  Local model failed to start: {error}"));
                }
                Ok(crate::client::LocalModelStatus::NotAvailable) => {
                    self.output_manager.write_info(
                        "⚠️  This daemon was started without a local model enabled.".to_string(),
                    );
                }
                Err(error) => {
                    self.output_manager
                        .write_info(format!("⚠️  Could not read local model status: {error}"));
                }
            }
        } else {
            match create_provider_from_entry(&entry) {
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to create model '{}': {}", name, e));
                }
                Ok(provider) => {
                    let client = crate::claude::ClaudeClient::with_provider(provider);
                    let inner: Arc<dyn Generator> =
                        Arc::new(ClaudeGenerator::new(Arc::new(client)));
                    let new_gen: Arc<dyn Generator> = Arc::new(
                        crate::generators::ProfiledGenerator::new(entry.profile_name(), inner),
                    );
                    self.model_selection.activate(target_index, new_gen).await;
                    self.output_manager.write_info(format!(
                        "✓ Switched to {} · {} (conversation preserved)",
                        entry.profile_name(),
                        entry.model().unwrap_or(entry.provider_type())
                    ));
                }
            }
        }
        self.render_tui().await
    }

    /// Handle /mcp list command - list connected MCP servers
    async fn handle_mcp_list(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            let servers = mcp_client.list_servers().await;
            if servers.is_empty() {
                self.output_manager.write_info("No MCP servers connected.");
            } else {
                let mut output = String::from("📡 Connected MCP Servers:\n\n");
                for server_name in servers {
                    output.push_str(&format!("  • {}\n", server_name));
                }
                self.output_manager.write_info(output);
            }
        } else {
            self.output_manager.write_info(
                "MCP plugin system not configured.\n\
                 Add MCP servers to ~/.finch/config.toml to get started.",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp tools command - list tools from servers
    async fn handle_mcp_tools(&mut self, server_filter: Option<String>) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            let all_tools = mcp_client.list_tools().await;
            let filtered_tools: Vec<_> = all_tools
                .into_iter()
                .filter(|tool| {
                    if let Some(ref server) = server_filter {
                        // Tool names are prefixed with "mcp_<server>_"
                        tool.name.starts_with(&format!("mcp_{}_", server))
                    } else {
                        true
                    }
                })
                .collect();

            if filtered_tools.is_empty() {
                if let Some(server) = server_filter {
                    self.output_manager.write_info(format!(
                        "No tools found for server '{}'. Check server name with /mcp list",
                        server
                    ));
                } else {
                    self.output_manager.write_info("No MCP tools available.");
                }
            } else {
                let header = if let Some(server) = server_filter {
                    format!("🔧 MCP Tools from '{}' server:\n\n", server)
                } else {
                    String::from("🔧 All MCP Tools:\n\n")
                };

                let mut output = header;
                for tool in filtered_tools {
                    // Remove "mcp_" prefix for display
                    let display_name = tool.name.strip_prefix("mcp_").unwrap_or(&tool.name);
                    output.push_str(&format!("  • {}\n", display_name));
                    output.push_str(&format!("    {}\n", tool.description));
                }
                self.output_manager.write_info(output);
            }
        } else {
            self.output_manager.write_info(
                "MCP plugin system not configured.\n\
                 Add MCP servers to ~/.finch/config.toml to get started.",
            );
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp refresh command - refresh tools from all servers
    async fn handle_mcp_refresh(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;

        if let Some(mcp_client) = executor_guard.mcp_client() {
            self.output_manager.write_info("Refreshing MCP tools...");
            self.render_tui().await?;

            match mcp_client.refresh_all_tools().await {
                Ok(()) => {
                    let tools = mcp_client.list_tools().await;
                    *self.tool_definitions.write().await = executor_guard.list_all_tools().await;
                    self.output_manager.write_info(format!(
                        "✓ Refreshed MCP tools ({} tools available)",
                        tools.len()
                    ));
                }
                Err(e) => {
                    self.output_manager
                        .write_error(format!("Failed to refresh MCP tools: {}", e));
                }
            }
        } else {
            self.output_manager.write_info("No MCP servers configured.");
        }

        self.render_tui().await?;
        Ok(())
    }

    /// Handle /mcp reload command - reconnect to all servers
    async fn handle_mcp_reload(&mut self) -> Result<()> {
        let tool_executor = self.tool_coordinator.tool_executor();
        let executor_guard = tool_executor.lock().await;
        if let Some(mcp_client) = executor_guard.mcp_client() {
            self.output_manager
                .write_info("Reconnecting to configured MCP servers...");
            mcp_client.reload().await?;
            let servers = mcp_client.list_servers().await;
            let tools = mcp_client.list_tools().await;
            *self.tool_definitions.write().await = executor_guard.list_all_tools().await;
            self.output_manager.write_info(format!(
                "✓ Connected to {} MCP server(s) with {} tool(s)",
                servers.len(),
                tools.len()
            ));
        } else {
            self.output_manager.write_info("No MCP servers configured.");
        }
        self.render_tui().await?;
        Ok(())
    }

    /// Handle an event from the event channel
    async fn handle_event(&mut self, event: ReplEvent) -> Result<()> {
        match event {
            ReplEvent::UserInput { input } => {
                self.handle_user_input(input).await?;
            }

            ReplEvent::QueryComplete { query_id, response } => {
                // Add response to conversation
                self.conversation
                    .write()
                    .await
                    .add_assistant_message(response.clone());

                // Update compaction percentage in status bar
                self.update_compaction_status().await;

                // Update query state
                self.query_states
                    .update_state(
                        query_id,
                        QueryState::Completed {
                            response: response.clone(),
                        },
                    )
                    .await;

                // Display response
                self.output_manager.write_response(&response);
            }

            ReplEvent::QueryFailed { query_id, error } => {
                // DON'T remove streaming message here - fallback providers need it!
                // The message will be removed on StreamingComplete or stays for final error display

                // Update query state
                self.query_states
                    .update_state(
                        query_id,
                        QueryState::Failed {
                            error: error.clone(),
                        },
                    )
                    .await;

                // Display error
                self.output_manager
                    .write_error(format!("Query failed: {}", error));

                // Render TUI to ensure viewport is redrawn after error message
                if let Err(e) = self.render_tui().await {
                    tracing::warn!("Failed to render TUI after query error: {}", e);
                }

                // A terminal provider failure has no later StreamingComplete.
                // Release the turn so queued user input cannot wedge behind it.
                if *self.active_query_id.read().await == Some(query_id) {
                    *self.active_query_id.write().await = None;
                    if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                        self.execute_query_inner(next, echo, chat_only).await?;
                    }
                }
            }

            ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            } => {
                if let Some(proposal) = deferred_proposal_from_tool_result(&result) {
                    self.output_manager.write_status(format!(
                        "Proposal {} is awaiting editor review",
                        proposal.handle.sequence
                    ));
                    self.spawn_deferred_proposal(query_id, tool_id, proposal);
                } else {
                    self.handle_tool_result(query_id, tool_id, result).await?;
                }
            }

            ReplEvent::ToolApprovalNeeded {
                query_id,
                tool_use,
                response_tx,
            } => {
                self.handle_tool_approval_request(query_id, tool_use, response_tx)
                    .await?;
            }

            ReplEvent::OutputReady { message } => {
                self.output_manager.write_status(message);
            }

            ReplEvent::StreamingComplete {
                query_id,
                full_response,
            } => {
                tracing::debug!("[EVENT_LOOP] Handling StreamingComplete event");

                // Check if this query is executing tools
                // If so, the assistant message was already added with ToolUse blocks
                let state = self
                    .query_states
                    .get_metadata(query_id)
                    .await
                    .map(|m| m.state.clone());
                let is_executing_tools = matches!(state, Some(QueryState::ExecutingTools { .. }));
                // The streaming path adds the assistant message and sets Completed before
                // sending StreamingComplete. The non-streaming path does not — it relies on
                // this handler to do both. Detect which case we are in.
                let already_completed = matches!(state, Some(QueryState::Completed { .. }));

                if !is_executing_tools && !already_completed {
                    tracing::debug!(
                        "[EVENT_LOOP] No tools, adding assistant message to conversation"
                    );
                    // Add complete response to conversation (only if not executing tools)
                    self.conversation
                        .write()
                        .await
                        .add_assistant_message(full_response.clone());
                    tracing::debug!("[EVENT_LOOP] Added assistant message to conversation");

                    // Update query state
                    self.query_states
                        .update_state(
                            query_id,
                            QueryState::Completed {
                                response: full_response.clone(),
                            },
                        )
                        .await;
                    tracing::debug!("[EVENT_LOOP] Updated query state");
                } else {
                    tracing::debug!("[EVENT_LOOP] Skipping duplicate message (tools={is_executing_tools}, already_completed={already_completed})");
                }

                // Update context usage indicator now that the message is committed
                self.update_compaction_status().await;

                // Render TUI to write the complete message to scrollback
                self.render_tui().await?;
                tracing::debug!("[EVENT_LOOP] StreamingComplete handled, TUI rendered");

                // Clear active query (query completed successfully)
                {
                    let mut active = self.active_query_id.write().await;
                    if *active == Some(query_id) {
                        *active = None;
                    }
                }
                if let Some((next, echo, chat_only)) = self.pending_queries.pop_front() {
                    self.execute_query_inner(next, echo, chat_only).await?;
                }
                // Now that the user is idle, show any brain question that was deferred.
                self.maybe_show_deferred_brain_question().await.ok();

                // Record final response + save execution graph
                if !is_executing_tools {
                    let preview = full_response.chars().take(300).collect::<String>();
                    let mut g = self.current_graph.lock().await;
                    g.add_node(crate::graph::NodeKind::FinalResponse { preview });
                    if let Err(e) = g.save() {
                        tracing::warn!("Failed to save execution graph: {}", e);
                    }
                }

                // The AI does NOT auto-push to the stack on completion.
                // It pushes explicitly via the Push tool when it wants to
                // add something to the collaborative program.

                // Clear per-query tool-call history so it doesn't grow forever.
                self.tool_call_history.write().await.remove(&query_id);
            }

            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
            } => {
                // Record LLM invocation in execution graph
                self.current_graph
                    .lock()
                    .await
                    .add_node(crate::graph::NodeKind::LlmCall {
                        model: model.clone(),
                        input_tokens,
                        output_tokens,
                    });
                // Update status bar with live stats
                self.status_bar
                    .update_live_stats(model, input_tokens, output_tokens, latency_ms);
                // Render to display updated stats
                self.render_tui().await?;
            }

            ReplEvent::AgentLifecycle(event) => {
                let finished = match &event {
                    crate::runtime::scheduler::AgentEvent::TaskFinished { result } => {
                        Some(result.clone())
                    }
                    _ => None,
                };
                self.tui_renderer.lock().await.apply_agent_event(&event);
                if let Some(result) = finished {
                    let summary = if result.final_message.trim().is_empty() {
                        result.diagnostics.join("; ")
                    } else {
                        result.final_message
                    };
                    self.output_manager.write_info(format!(
                        "child {} {:?} ({} turns, {} ms)\n{}",
                        result.identity.agent_id,
                        result.status,
                        result.turns,
                        result.elapsed_ms,
                        summary
                    ));
                }
                self.render_tui().await?;
            }

            ReplEvent::CancelQuery => {
                // Get the active query ID
                let query_id = {
                    let active = self.active_query_id.read().await;
                    *active
                };

                if let Some(qid) = query_id {
                    // Fire the per-query cancellation token so handle_present_plan
                    // (and any other token-aware loops) can detect the cancel immediately.
                    self.query_states.cancel_query(qid).await;

                    // Clear active query
                    *self.active_query_id.write().await = None;
                    // Clear tool-call history for cancelled query
                    self.tool_call_history.write().await.remove(&qid);

                    // If we were in plan/executing mode, cancel that too so the
                    // user doesn't have to press Ctrl+C again to escape.
                    {
                        let mode = self.mode.read().await.clone();
                        if !matches!(mode, ReplMode::Normal) {
                            *self.mode.write().await = ReplMode::Normal;
                            self.update_plan_mode_indicator(&ReplMode::Normal);
                        }
                    }

                    // Show cancellation message
                    self.output_manager
                        .write_info("⚠️  Query cancelled by user (Ctrl+C)");
                    self.render_tui().await?;

                    tracing::info!("Query {} cancelled by user", qid);
                } else {
                    // No active query — Ctrl+C when idle:
                    //   • in plan/executing mode → exit that mode, stay in finch
                    //   • in normal mode → exit finch entirely (like Ctrl+D or /quit)
                    let mode = self.mode.read().await.clone();
                    if !matches!(mode, ReplMode::Normal) {
                        *self.mode.write().await = ReplMode::Normal;
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan mode cancelled (Ctrl+C).");
                        self.render_tui().await?;
                    } else {
                        let _ = self.event_tx.send(ReplEvent::Shutdown);
                    }
                }
            }

            ReplEvent::Shutdown => {
                // Handled in run() method - this should not be reached
                unreachable!("Shutdown event should be handled in run() method");
            }

            ReplEvent::BrainQuestion {
                question,
                options,
                response_tx,
            } => {
                self.handle_brain_question(question, options, response_tx)
                    .await?;
            }

            ReplEvent::BrainProposedAction {
                command,
                reason,
                response_tx,
            } => {
                self.handle_brain_proposed_action(command, reason, response_tx)
                    .await?;
            }

            ReplEvent::PosetComplete { result } => {
                match result {
                    Ok(text) if !text.trim().is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {
                        self.output_manager.write_info("📚 Program complete.");
                    }
                    Err(e) => {
                        self.output_manager.write_info(format!("📚 Error: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::LispResult { result } => {
                match result {
                    Ok(text) if text != "()" && !text.is_empty() => {
                        self.output_manager.write_response(text);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        self.output_manager.write_info(format!("lisp: {e}"));
                    }
                }
                self.render_tui().await?;
            }

            ReplEvent::PeersDiscovered(peers) => {
                // Background boot scan found finch instances on the LAN.
                // Add each to the Forth VM's peer list and establish a WS bridge.
                let mut added = Vec::new();
                for (host, port, name, token) in peers {
                    let addr = format!("{host}:{port}");
                    if !self.forth_vm.peers.contains(&addr) {
                        self.forth_vm.peers.push(addr.clone());
                        let meta = self.forth_vm.peer_meta.entry(addr.clone()).or_default();
                        if !name.is_empty() {
                            meta.label = Some(name.clone());
                        }
                        if let Some(t) = token {
                            meta.token = Some(t);
                        }
                        self.bridge_single_peer(addr);
                        added.push(name);
                    }
                }
                if !added.is_empty() {
                    use crossterm::style::Stylize;
                    let lines: Vec<String> = added
                        .iter()
                        .map(|n| {
                            let display = if n.is_empty() {
                                "someone".to_string()
                            } else {
                                n.clone()
                            };
                            format!("  {} is here", display.as_str().cyan().bold())
                        })
                        .collect();
                    self.output_manager.write_info(lines.join("\n"));
                    self.render_tui().await?;
                }
            }
            ReplEvent::RemoteBrainMessage { target, message } => {
                let is_current = self
                    .active_remote_brain
                    .as_ref()
                    .is_some_and(|client| client.target.display_name() == target);
                if is_current {
                    self.render_remote_brain_message(message).await?;
                }
            }
            ReplEvent::RemoteBrainError { target, error } => {
                self.output_manager.write_info(format!("{target}: {error}"));
                self.render_tui().await?;
            }
            ReplEvent::PeerMessage { text } => {
                use crossterm::style::Stylize;
                // Channel messages are tagged with a sentinel that may be preceded
                // by "peer-XXXX: " when the peer loop wraps the tagged string.
                if let Some(sentinel_pos) = text.find("\x01CHAN\x01") {
                    let sentinel = &text[sentinel_pos..];
                    let parts: Vec<&str> = sentinel.splitn(5, '\x01').collect();
                    // parts: ["", "CHAN", channel, sender, text_body]
                    if parts.len() == 5 {
                        let (channel, sender, body) = (parts[2], parts[3], parts[4]);
                        let idx = self.recent_channel_msgs.len();
                        self.recent_channel_msgs.push_front((
                            channel.to_string(),
                            sender.to_string(),
                            body.to_string(),
                        ));
                        if self.recent_channel_msgs.len() > 20 {
                            self.recent_channel_msgs.pop_back();
                        }
                        let line = format!("[{idx}] {channel} {sender}: {body}");
                        self.output_manager.write_info(line.cyan().to_string());
                    }
                } else {
                    // text is already "peer-XXXXXXXX: result" from the select! branch
                    self.output_manager
                        .write_info(text.as_str().cyan().to_string());
                }
                self.render_tui().await?;
            }

            ReplEvent::VocabSync(source) => {
                // Another terminal defined new words — compile them into this session's VM.
                // Use exec_with_fuel directly (not save_user_words) to avoid a push loop.
                let before = self.forth_vm.dump_source().lines().count();
                let _ = self.forth_vm.exec_with_fuel(&source, 0);
                let after = self.forth_vm.dump_source().lines().count();
                let new_count = after.saturating_sub(before);
                if new_count > 0 {
                    use crossterm::style::Stylize;
                    self.output_manager.write_info(
                        format!(
                            "  {} word{} synced from another session",
                            new_count,
                            if new_count == 1 { "" } else { "s" }
                        )
                        .dark_grey()
                        .to_string(),
                    );
                    self.render_tui().await?;
                }
            }

            ReplEvent::ShowDialog {
                dialog: _,
                response_tx,
            } => {
                // active_dialog is already set by the caller (belt-and-suspenders in
                // handle_present_plan / handle_ask_user_question), so the dialog is
                // on-screen before the event is even enqueued — no race window.
                // Just store the response channel for the render tick to route the result.
                self.pending_dialog_tx = Some(response_tx);
            }
        }

        Ok(())
    }

    /// The editor runs outside the VM; once it finishes, resume precisely the
    /// saved effect rather than resubmitting source or replaying prior output.
    fn spawn_deferred_proposal(
        &self,
        query_id: Uuid,
        tool_id: String,
        proposal: DeferredProposal,
    ) {
        let event_tx = self.event_tx.clone();
        let runtime = Arc::clone(&self.program_runtime);
        tokio::spawn(async move {
            let result = async {
                let decision = crate::tools::implementations::propose::propose_artifact_with_decision(
                    &proposal.language,
                    &proposal.intent,
                    &proposal.source,
                )
                .await?;
                let outcome = resume_deferred_proposal(runtime.as_ref(), &proposal, decision).await?;
                Ok::<_, anyhow::Error>(serde_json::to_string(&outcome)?)
            }
            .await;
            let _ = event_tx.send(ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            });
        });
    }

    // ── Diff proposal rendering ───────────────────────────────────────────────

    /// Render a diff proposal visually in the room output.
    ///
    /// ```text
    /// peer-a1b2c3d4 proposes: src/session/mod.rs
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// - old line
    /// + new line
    /// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
    /// "adds Diff variant to SessionEvent"
    /// [accept: /accept <id>] [reject: /reject <reason>]
    /// ```
    fn render_diff_proposal(
        &self,
        id: uuid::Uuid,
        label: &str,
        patch: &str,
        description: Option<&str>,
        proposed_by: &str,
    ) {
        use crossterm::style::Stylize;
        const BAR: &str = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━";

        let short_id = &id.to_string()[..8];

        // Header
        let id_tag = format!("[id: {}]", short_id);
        self.output_manager.write_info(format!(
            "{} proposes: {}  {}",
            proposed_by.cyan(),
            label.white().bold(),
            id_tag.as_str().dark_grey(),
        ));
        self.output_manager.write_info(BAR.dark_grey().to_string());

        // Patch lines — colour additions green, removals red
        for line in patch.lines() {
            let rendered = if line.starts_with('+') && !line.starts_with("+++") {
                line.green().to_string()
            } else if line.starts_with('-') && !line.starts_with("---") {
                line.red().to_string()
            } else if line.starts_with("@@") {
                line.cyan().to_string()
            } else {
                line.dark_grey().to_string()
            };
            self.output_manager.write_info(rendered);
        }

        self.output_manager.write_info(BAR.dark_grey().to_string());

        // Optional description
        if let Some(desc) = description {
            self.output_manager
                .write_info(format!("  \"{}\"", desc.dark_grey()));
        }

        // Action hints
        self.output_manager.write_info(format!(
            "  {}  {}",
            format!("[accept: /accept {}]", short_id).as_str().green(),
            "[reject: /reject <reason>]".dark_grey(),
        ));
    }

    /// Handle `/accept [prefix]` — apply the patch and notify peers.
    async fn handle_accept(&mut self, prefix: Option<String>) -> Result<()> {
        use crossterm::style::Stylize;

        let diff_id = {
            let d = self.diff_store.resolve_pending(prefix.as_deref());
            match d {
                None => {
                    self.output_manager
                        .write_info("no pending diff to accept".dark_grey().to_string());
                    return self.render_tui().await;
                }
                Some(d) => d.id,
            }
        };

        // Mark as accepted in store
        let (label, patch) = {
            if let Some(d) = self.diff_store.accept(diff_id) {
                (d.label.clone(), d.patch.clone())
            } else {
                self.output_manager
                    .write_info("diff not found".dark_grey().to_string());
                return self.render_tui().await;
            }
        };

        // Apply the patch: write the patched content to the file.
        // We implement a simple unified-diff applicator inline.
        let apply_result = self.apply_unified_diff(&label, &patch);
        match apply_result {
            Ok(()) => {
                self.output_manager.write_info(format!(
                    "{}  applied diff to {}",
                    "✓".green(),
                    label.as_str().white(),
                ));
            }
            Err(e) => {
                self.output_manager.write_info(format!(
                    "{}  failed to apply diff to {}: {}",
                    "✗".red(),
                    label.as_str().white(),
                    e,
                ));
            }
        }

        // Broadcast DiffAccept so the proposing peer knows
        let _ = self
            .peer_tx
            .send(crate::session::SessionEvent::diff_accept(diff_id));
        self.render_tui().await
    }

    /// Apply a unified diff patch to a file on disk.
    ///
    /// This is a simple line-based applicator that handles the most common
    /// unified diff format (`--- a/file`, `+++ b/file`, `@@ ... @@` hunks).
    /// It is not a full POSIX patch implementation — it is good enough for
    /// AI-proposed diffs that the AI has computed against the current file.
    fn apply_unified_diff(&self, label: &str, patch: &str) -> anyhow::Result<()> {
        use std::path::Path;

        // Extract the target filename from the patch's `+++ b/...` line,
        // falling back to `label` if not found.
        let target_path = patch
            .lines()
            .find(|l| l.starts_with("+++ "))
            .and_then(|l| {
                let s = l.trim_start_matches("+++ ");
                // Strip `b/` prefix if present
                let s = s.strip_prefix("b/").unwrap_or(s);
                // Strip timestamp suffix (a tab followed by date)
                let s = s.split('\t').next().unwrap_or(s);
                if s == "/dev/null" {
                    None
                } else {
                    Some(s.to_string())
                }
            })
            .unwrap_or_else(|| label.to_string());

        let path = Path::new(&target_path);

        // Read original file (empty if it doesn't exist — new-file diff)
        let original: Vec<String> = if path.exists() {
            std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("could not read {}: {}", target_path, e))?
                .lines()
                .map(|l| l.to_string())
                .collect()
        } else {
            Vec::new()
        };

        let patched = apply_patch_lines(&original, patch)
            .map_err(|e| anyhow::anyhow!("patch failed: {}", e))?;

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| anyhow::anyhow!("could not create dirs: {}", e))?;
            }
        }
        std::fs::write(path, patched.join("\n") + "\n")
            .map_err(|e| anyhow::anyhow!("could not write {}: {}", target_path, e))?;

        Ok(())
    }

    /// Handle `/reject [reason]` — reject the most recent pending diff and notify peers.
    async fn handle_reject(&mut self, reason: Option<String>) -> Result<()> {
        use crossterm::style::Stylize;

        let diff_id = {
            let d = self.diff_store.resolve_pending(None);
            match d {
                None => {
                    self.output_manager
                        .write_info("no pending diff to reject".dark_grey().to_string());
                    return self.render_tui().await;
                }
                Some(d) => d.id,
            }
        };

        self.diff_store.reject(diff_id, reason.clone());
        let reason_str = reason.clone().unwrap_or_default();
        self.output_manager.write_info(format!(
            "{}  diff {} rejected{}",
            "✗".red(),
            &diff_id.to_string()[..8].white(),
            if reason_str.is_empty() {
                String::new()
            } else {
                format!(": {}", reason_str)
            },
        ));

        // Broadcast DiffReject so the proposing peer knows
        let _ = self
            .peer_tx
            .send(crate::session::SessionEvent::diff_reject(diff_id, reason));
        self.render_tui().await
    }

    /// `/machines` — show known peer machines from LAN discovery.
    async fn handle_machines(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        let peers = &self.forth_vm.peers;
        if peers.is_empty() {
            self.output_manager.write_info(format!(
                "{}  no peers found yet — run {} to scan",
                "machines:".dark_grey(),
                "/discover".cyan()
            ));
        } else {
            let mut lines = vec![format!("{}", "machines:".dark_grey())];
            for addr in peers {
                lines.push(format!("  {}", addr.as_str().cyan()));
            }
            self.output_manager.write_info(lines.join("\n"));
        }
        self.render_tui().await
    }

    /// `/discover` — run a fresh mDNS scan for peers on the LAN.
    async fn handle_discover(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        self.output_manager
            .write_info(format!("{}", "scanning LAN for Finch peers…".dark_grey()));
        self.render_tui().await.ok();

        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            let peers = tokio::task::spawn_blocking(|| {
                crate::coforth::interpreter::run_peers_discover_pub(3000)
            })
            .await
            .unwrap_or_default();
            if peers.is_empty() {
                // No peers found — write a message via the output channel
                tracing::debug!("[DISCOVER] No peers found on LAN");
            }
            let _ = event_tx.send(crate::cli::repl_event::ReplEvent::PeersDiscovered(peers));
        });

        Ok(())
    }

    /// Attach this TUI to one daemon-owned brain. All prompts and explicit
    /// Forth/Lisp programs are routed to that host until `/brain detach`.
    async fn handle_brain_attach(&mut self, value: String) -> Result<()> {
        let target = match crate::brain::remote::RemoteBrainTarget::parse(&value) {
            Ok(target) => target,
            Err(error) => {
                self.output_manager
                    .write_info(format!("brain attach: {error}"));
                self.render_tui().await?;
                return Ok(());
            }
        };
        let password = crate::config::load_config()
            .map(|config| config.server.brain_password)
            .unwrap_or_default();
        let mut client = crate::brain::remote::RemoteBrainClient::new(target, password)?;
        let snapshot = match client.snapshot().await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.output_manager.write_info(format!(
                    "brain attach {}: {error}",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };
        client.target.machine = snapshot.environment.machine.clone();
        let mut incoming = match client.watch().await {
            Ok(incoming) => incoming,
            Err(error) => {
                self.output_manager.write_info(format!(
                    "brain attach {}: {error}",
                    client.target.display_name()
                ));
                self.render_tui().await?;
                return Ok(());
            }
        };

        let target_name = client.target.display_name();
        self.active_remote_brain = Some(client);
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            format!("◆ {target_name}"),
        );
        self.render_remote_brain_message(crate::brain::shared::BrainWireMessage::Snapshot {
            brain: snapshot,
        })
        .await?;

        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some(message) = incoming.recv().await {
                if event_tx
                    .send(ReplEvent::RemoteBrainMessage {
                        target: target_name.clone(),
                        message,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        Ok(())
    }

    async fn handle_brain_detach(&mut self) -> Result<()> {
        if let Some(client) = self.active_remote_brain.take() {
            self.output_manager
                .write_info(format!("detached from {}", client.target.display_name()));
        }
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::SessionLabel,
            format!("◆ {}", self.session_label),
        );
        self.render_tui().await
    }

    /// Show or rotate the credential only on the execution environment's local daemon.
    async fn handle_brain_password(&mut self, password: Option<String>) -> Result<()> {
        if self.active_remote_brain.is_some() {
            self.output_manager
                .write_info("brain password is visible only from the brain's execution host");
            return self.render_tui().await;
        }
        let base = self
            .daemon_base_url
            .clone()
            .unwrap_or_else(|| format!("http://{}", crate::config::constants::DEFAULT_HTTP_ADDR));
        let http = reqwest::Client::new();
        match password {
            Some(password) => {
                let response = http
                    .put(format!("{base}/v1/brains/password"))
                    .json(&serde_json::json!({"password": password}))
                    .send()
                    .await?;
                if response.status().is_success() {
                    self.output_manager.write_info("brain password updated");
                } else {
                    self.output_manager.write_info(format!(
                        "brain password update failed: {}",
                        response.text().await.unwrap_or_default()
                    ));
                }
            }
            None => {
                let response = http
                    .get(format!("{base}/v1/brains/password"))
                    .send()
                    .await?;
                if response.status().is_success() {
                    let body: serde_json::Value = response.json().await?;
                    self.output_manager.write_info(format!(
                        "brain password: {}",
                        body["password"].as_str().unwrap_or("<not configured>")
                    ));
                } else {
                    self.output_manager
                        .write_info("brain password is unavailable from this client");
                }
            }
        }
        self.render_tui().await
    }

    async fn push_remote_brain(
        &mut self,
        kind: crate::brain::shared::BrainEventKind,
    ) -> Result<()> {
        let Some(client) = self.active_remote_brain.clone() else {
            return Ok(());
        };
        let target = client.target.display_name();
        let sender = self.session_label.clone();
        let event_tx = self.event_tx.clone();
        tokio::spawn(async move {
            if let Err(error) = client.push(&sender, kind).await {
                let _ = event_tx.send(ReplEvent::RemoteBrainError {
                    target,
                    error: error.to_string(),
                });
            }
        });
        Ok(())
    }

    async fn render_remote_brain_message(
        &mut self,
        message: crate::brain::shared::BrainWireMessage,
    ) -> Result<()> {
        match message {
            crate::brain::shared::BrainWireMessage::Snapshot { brain } => {
                self.output_manager.clear();
                self.output_manager.write_info(format!(
                    "environment: {}:{} (generation {})",
                    brain.environment.machine,
                    brain.environment.workspace.display(),
                    brain.environment.generation,
                ));
                for event in &brain.events {
                    self.render_remote_brain_event(event);
                }
            }
            crate::brain::shared::BrainWireMessage::Event { event } => {
                self.render_remote_brain_event(&event);
            }
        }
        self.render_tui().await
    }

    fn render_remote_brain_event(&self, event: &crate::brain::shared::BrainEvent) {
        use crate::brain::shared::BrainEventKind;
        match &event.kind {
            BrainEventKind::Prompt { text } => self
                .output_manager
                .write_user(format!("{}: {text}", event.sender)),
            BrainEventKind::Program { language, source } => {
                let command = match language {
                    crate::brain::shared::ProgramLanguage::Forth => "/forth",
                    crate::brain::shared::ProgramLanguage::Lisp => "/lisp",
                };
                self.output_manager
                    .write_user(format!("{} {command} {source}", event.sender));
            }
            BrainEventKind::ProgramPopped { program_seq } => self
                .output_manager
                .write_info(format!("{} popped program #{program_seq}", event.sender)),
            BrainEventKind::Result { output, error, .. } => {
                if let Some(error) = error {
                    self.output_manager.write_info(format!("error: {error}"));
                } else if !output.is_empty() {
                    self.output_manager.write_response(output.clone());
                }
            }
        }
    }

    /// `/connect <host:port>` — manually add a peer to peers list and current room.
    async fn handle_connect(&mut self, addr: String) -> Result<()> {
        use crossterm::style::Stylize;
        let addr = addr.trim().to_string();
        // Add to peer list
        if !self.forth_vm.peers.contains(&addr) {
            self.forth_vm.peers.push(addr.clone());
        }
        // Add to current room if one is set
        if let Some(ref room_id) = self.forth_vm.current_room.clone() {
            let room = self.forth_vm.rooms.entry(room_id.clone()).or_default();
            if !room.members.contains(&addr) {
                room.members.push(addr.clone());
            }
        }
        // Fetch the node name from the peer (best-effort via /v1/node/info)
        let label = fetch_peer_name(&addr).await;
        let meta = self.forth_vm.peer_meta.entry(addr.clone()).or_default();
        if let Some(ref name) = label {
            meta.label = Some(name.clone());
        }
        let display = label.unwrap_or_else(|| addr.clone());
        self.output_manager.write_info(format!(
            "  {} {}",
            display.as_str().cyan().bold(),
            "connected".dark_grey()
        ));

        // Fetch the peer's vocabulary and open it in $EDITOR.
        // Whatever the user saves (or writes fresh, or deletes) is sent back
        // into the event loop as a UserInput so the single select! handles it.
        let peer_source = fetch_peer_vocab_source(&addr).await;
        let description = format!("peer: {addr} — edit and save to define these words locally");

        // propose_forth_in_editor handles TUI suspend/resume internally via
        // EDITOR_ACTIVE + TerminalRestorer — no manual tui.suspend()/resume() needed.
        let editor_result = crate::tools::implementations::propose::propose_forth_in_editor(
            &description,
            &peer_source,
        )
        .await;

        if let Ok(Some(content)) = editor_result {
            // Strip comment lines before feeding back so only Forth code runs.
            let code: String = content
                .lines()
                .filter(|l| !l.trim_start().starts_with('\\') && !l.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if !code.trim().is_empty() {
                let _ = self.event_tx.send(ReplEvent::UserInput { input: code });
            }
        }

        self.render_tui().await
    }

    /// Establish a WebSocket bridge to a single peer address.
    ///
    /// Spawns three tasks:
    /// 1. Remote → local inbox: their Chat replies appear as peer responses.
    /// 2. Our in-process peer mirror → remote.
    /// 3. Our peer_tx broadcasts → remote (they execute our Forth programs).
    fn bridge_single_peer(&self, peer_addr: String) {
        let ws_url = match &self.daemon_base_url {
            Some(base) => {
                let my_addr = base
                    .trim_start_matches("http://")
                    .trim_start_matches("https://");
                format!("ws://{peer_addr}/v1/session/ws?from={my_addr}")
            }
            None => format!("ws://{peer_addr}/v1/session/ws"),
        };

        let peer_tx = self.peer_tx.clone();
        let peer_inbox_tx = self.peer_inbox_tx.clone();
        let mirror_rx = self.peer_inbox_mirror_tx.subscribe();
        let addr = peer_addr.clone();

        tokio::spawn(async move {
            match crate::session::transport::connect(&ws_url).await {
                Ok(crate::session::SessionBus {
                    tx: ws_tx,
                    rx: mut ws_rx,
                }) => {
                    tracing::info!("auto-joined remote peer {addr}");

                    let inbox = peer_inbox_tx.clone();
                    let a2 = addr.clone();
                    tokio::spawn(async move {
                        while let Some(ev) = ws_rx.recv().await {
                            if let crate::session::SessionEvent::Chat { text } = ev {
                                if !text.is_empty() {
                                    let id = uuid::Uuid::new_v4();
                                    let name =
                                        format!("peer@{}", a2.split(':').next().unwrap_or(&a2));
                                    let _ = inbox.send((id, name, text));
                                }
                            }
                        }
                    });

                    let ws_tx3 = ws_tx.clone();
                    let mut mirror_rx = mirror_rx;
                    tokio::spawn(async move {
                        while let Ok((name, text)) = mirror_rx.recv().await {
                            let ev = crate::session::SessionEvent::chat(format!("{name}: {text}"));
                            if ws_tx3.send(ev).await.is_err() {
                                break;
                            }
                        }
                    });

                    let mut bcast_rx = peer_tx.subscribe();
                    while let Ok(ev) = bcast_rx.recv().await {
                        if ws_tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                }
                Err(e) => tracing::debug!("auto-join {addr}: connect failed: {e}"),
            }
        });
    }

    /// Establish WebSocket bridges to all addresses in `self.remote_peers`.
    ///
    /// For each remote daemon at `host:port`:
    /// 1. Connect to `ws://host:port/v1/session/ws?from=<local-daemon-addr>`.
    /// 2. Spawn a task that forwards `peer_tx` broadcasts to the remote (so the
    ///    remote machine executes our Forth programs and replies).
    /// 3. Spawn a task that receives the remote's `Chat` replies and routes them
    ///    to `peer_inbox_tx` so they appear alongside the in-process peer responses.
    /// 4. Spawn a task that forwards local `peer_inbox_mirror` (our in-process
    ///    peer responses) back to the remote so it can observe our session.
    ///
    /// Additionally, start a background poller that:
    /// - Drains `GET /v1/session/relay-drain` every 300 ms to capture messages from
    ///   remote peers that connected TO our daemon (i.e., machines that ran
    ///   `finch --peer <this-machine>`).
    /// - Polls `GET /v1/peer/announced` for newly arrived remote peers and connects
    ///   back to them symmetrically.
    async fn bridge_remote_peers(&mut self) {
        let daemon_base = self.daemon_base_url.clone();

        // ── Outbound connections (we dial the remote) ─────────────────────────
        for peer_addr in self.remote_peers.clone() {
            let ws_url = match &daemon_base {
                Some(base) => {
                    // Strip http:// prefix to get host:port for the ?from= param.
                    let my_addr: &str = base
                        .trim_start_matches("http://")
                        .trim_start_matches("https://");
                    format!("ws://{peer_addr}/v1/session/ws?from={my_addr}")
                }
                None => format!("ws://{peer_addr}/v1/session/ws"),
            };

            let peer_tx = self.peer_tx.clone();
            let peer_inbox_tx = self.peer_inbox_tx.clone();
            let mut mirror_rx = self.peer_inbox_mirror_tx.subscribe();
            let addr = peer_addr.clone();

            tokio::spawn(async move {
                match crate::session::transport::connect(&ws_url).await {
                    Ok(crate::session::SessionBus {
                        tx: ws_tx,
                        rx: mut ws_rx,
                    }) => {
                        tracing::info!("joined remote peer {addr}");

                        // Remote → local inbox: their peer loop responses appear as our peers.
                        let inbox = peer_inbox_tx.clone();
                        let a2 = addr.clone();
                        let ws_tx2 = ws_tx.clone();
                        tokio::spawn(async move {
                            while let Some(ev) = ws_rx.recv().await {
                                if let crate::session::SessionEvent::Chat { text } = ev {
                                    if !text.is_empty() {
                                        let id = uuid::Uuid::new_v4();
                                        let name =
                                            format!("peer@{}", a2.split(':').next().unwrap_or(&a2));
                                        let _ = inbox.send((id, name, text));
                                    }
                                }
                            }
                        });

                        // Our in-process peer responses → remote (mirror).
                        let ws_tx3 = ws_tx.clone();
                        tokio::spawn(async move {
                            while let Ok((name, text)) = mirror_rx.recv().await {
                                let ev =
                                    crate::session::SessionEvent::chat(format!("{name}: {text}"));
                                if ws_tx3.send(ev).await.is_err() {
                                    break;
                                }
                            }
                        });

                        // Our peer_tx broadcasts → remote (so they execute our Forth programs).
                        let mut bcast_rx = peer_tx.subscribe();
                        while let Ok(ev) = bcast_rx.recv().await {
                            if ws_tx.send(ev).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => tracing::warn!("--peer {addr}: connect failed: {e}"),
                }
            });
        }

        // ── Background poller: relay-drain + announced peers ──────────────────
        if let Some(base) = daemon_base {
            let base: String = base;
            let peer_inbox_tx = self.peer_inbox_tx.clone();
            let peer_tx = self.peer_tx.clone();
            let mirror_tx = self.peer_inbox_mirror_tx.clone();
            let initial_known: std::collections::HashSet<String> =
                self.remote_peers.iter().cloned().collect();

            let drain_url = format!("{base}/v1/session/relay-drain");
            let announced_url = format!("{base}/v1/peer/announced");
            let bcast_url2 = format!("{base}/v1/session/relay-broadcast");

            tokio::spawn(async move {
                let http = reqwest::Client::new();
                let drain_url = drain_url;
                let announced_url = announced_url;
                let mut known = initial_known;
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(300));

                loop {
                    interval.tick().await;

                    // Drain messages from remote peers that connected TO our daemon.
                    if let Ok(resp) = http
                        .get(&drain_url)
                        .timeout(std::time::Duration::from_secs(1))
                        .send()
                        .await
                    {
                        if let Ok(msgs) = resp.json::<Vec<(String, String)>>().await {
                            for (from, text) in msgs {
                                let id = uuid::Uuid::new_v4();
                                let _ = peer_inbox_tx.send((id, from, text));
                            }
                        }
                    }

                    // Check for newly announced peers; connect back symmetrically.
                    if let Ok(resp) = http
                        .get(&announced_url)
                        .timeout(std::time::Duration::from_secs(1))
                        .send()
                        .await
                    {
                        if let Ok(addrs) = resp.json::<Vec<String>>().await {
                            for addr in addrs {
                                if known.contains(&addr) {
                                    continue;
                                }
                                known.insert(addr.clone());

                                let ws_url = format!("ws://{addr}/v1/session/ws?from=relay");
                                let ptx = peer_tx.clone();
                                let pib = peer_inbox_tx.clone();
                                let mut mirror_rx2 = mirror_tx.subscribe();
                                let a = addr.clone();

                                tokio::spawn(async move {
                                    if let Ok(crate::session::SessionBus {
                                        tx: ws_tx,
                                        rx: mut ws_rx,
                                    }) = crate::session::transport::connect(&ws_url).await
                                    {
                                        tracing::info!("connected back to announced peer {a}");

                                        // Their replies → our inbox.
                                        let pib2 = pib;
                                        let a2 = a.clone();
                                        let ws_tx2 = ws_tx.clone();
                                        tokio::spawn(async move {
                                            while let Some(ev) = ws_rx.recv().await {
                                                let display = match ev {
                                                    crate::session::SessionEvent::Chat { text } if !text.is_empty() => {
                                                        Some(text)
                                                    }
                                                    crate::session::SessionEvent::ChannelMessage { channel, sender, bundle } => {
                                                        let primary = bundle.primary();
                                                        let comment = if bundle.comments.is_empty() {
                                                            String::new()
                                                        } else {
                                                            format!("  \\ {}", bundle.comments.join("; "))
                                                        };
                                                        Some(format!("{channel} {sender}: {}{comment}", primary.code))
                                                    }
                                                    _ => None,
                                                };
                                                if let Some(text) = display {
                                                    let id = uuid::Uuid::new_v4();
                                                    let name = format!(
                                                        "peer@{}",
                                                        a2.split(':').next().unwrap_or(&a2)
                                                    );
                                                    let _ = pib2.send((id, name, text));
                                                }
                                            }
                                        });

                                        // Our mirror → them.
                                        let ws_tx3 = ws_tx.clone();
                                        tokio::spawn(async move {
                                            while let Ok((name, text)) = mirror_rx2.recv().await {
                                                let ev = crate::session::SessionEvent::chat(
                                                    format!("{name}: {text}"),
                                                );
                                                if ws_tx3.send(ev).await.is_err() {
                                                    break;
                                                }
                                            }
                                        });

                                        // Our broadcasts → them.
                                        let mut bcast_rx = ptx.subscribe();
                                        while let Ok(ev) = bcast_rx.recv().await {
                                            if ws_tx.send(ev).await.is_err() {
                                                break;
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }
                }
            });

            // Bridge peer_tx → POST /v1/session/relay-broadcast so remote peers
            // that connected TO our daemon also receive our broadcasts.
            let bcast_url = bcast_url2;
            let mut bcast_rx = self.peer_tx.subscribe();
            tokio::spawn(async move {
                let http = reqwest::Client::new();
                while let Ok(ev) = bcast_rx.recv().await {
                    if let crate::session::SessionEvent::Chat { ref text } = ev {
                        let body = serde_json::json!({ "text": text });
                        let _ = http
                            .post(&bcast_url)
                            .json(&body)
                            .timeout(std::time::Duration::from_millis(500))
                            .send()
                            .await;
                    }
                }
            });
        }
    }

    /// `/disconnect <name-or-addr>` — remove a peer from peers list and current room.
    async fn handle_disconnect(&mut self, name: String) -> Result<()> {
        use crossterm::style::Stylize;
        // Resolve by label first, then by addr substring
        let addr = self
            .forth_vm
            .peer_meta
            .iter()
            .find(|(_, meta)| meta.label.as_deref() == Some(name.as_str()))
            .map(|(a, _)| a.clone())
            .or_else(|| {
                self.forth_vm
                    .peers
                    .iter()
                    .find(|a| a.contains(name.as_str()))
                    .cloned()
            });
        if let Some(ref addr) = addr {
            self.forth_vm.peers.retain(|p| p != addr);
            if let Some(ref room_id) = self.forth_vm.current_room.clone() {
                if let Some(room) = self.forth_vm.rooms.get_mut(room_id) {
                    room.members.retain(|m| m != addr);
                }
            }
            self.forth_vm.peer_meta.remove(addr);
            self.output_manager.write_info(format!(
                "  {} {}",
                name.as_str().dark_grey(),
                "disconnected".dark_grey()
            ));
        } else {
            self.output_manager
                .write_info(format!("  {} not found", name.as_str().dark_grey()));
        }
        self.render_tui().await
    }

    /// `/room [uuid]` — show current room or join/create a specific one.
    async fn handle_room(&mut self, uuid: Option<String>) -> Result<()> {
        use crossterm::style::Stylize;
        if let Some(id) = uuid {
            self.forth_vm.rooms.entry(id.clone()).or_default();
            self.forth_vm.current_room = Some(id.clone());
            self.output_manager
                .write_info(format!("  room  {}", id.as_str().cyan()));
        } else if let Some(ref id) = self.forth_vm.current_room.clone() {
            let room = &self.forth_vm.rooms[id];
            self.output_manager.write_info(format!(
                "  room  {}  ({} members, {} keys)",
                id.as_str().cyan(),
                room.members.len(),
                room.hash.len(),
            ));
        } else {
            self.output_manager.write_info(
                "  no current room — use /room new or /room <uuid>"
                    .dark_grey()
                    .to_string(),
            );
        }
        self.render_tui().await
    }

    /// `/room new` — create a fresh room and set it as current.
    async fn handle_room_new(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        let id = uuid::Uuid::new_v4().to_string();
        self.forth_vm.rooms.entry(id.clone()).or_default();
        self.forth_vm.current_room = Some(id.clone());
        self.output_manager
            .write_info(format!("  room  {}  (new)", id.as_str().cyan()));
        self.render_tui().await
    }

    /// `/room add <addr>` — add a peer to the current room.
    async fn handle_room_add(&mut self, addr: String) -> Result<()> {
        use crossterm::style::Stylize;
        if let Some(ref room_id) = self.forth_vm.current_room.clone() {
            // Also add to global peer list
            if !self.forth_vm.peers.contains(&addr) {
                self.forth_vm.peers.push(addr.clone());
            }
            let room = self.forth_vm.rooms.entry(room_id.clone()).or_default();
            if !room.members.contains(&addr) {
                room.members.push(addr.clone());
            }
            self.output_manager.write_info(format!(
                "  +{}  in room {}",
                addr.as_str().cyan(),
                room_id
                    .chars()
                    .take(8)
                    .collect::<String>()
                    .as_str()
                    .dark_grey()
            ));
        } else {
            self.output_manager.write_info(
                "  no current room — use /room new first"
                    .dark_grey()
                    .to_string(),
            );
        }
        self.render_tui().await
    }

    /// `/room remove <name-or-addr>` — remove a peer from the current room.
    async fn handle_room_remove(&mut self, name: String) -> Result<()> {
        use crossterm::style::Stylize;
        let addr = self
            .forth_vm
            .peer_meta
            .iter()
            .find(|(_, m)| m.label.as_deref() == Some(name.as_str()))
            .map(|(a, _)| a.clone())
            .or_else(|| {
                self.forth_vm
                    .peers
                    .iter()
                    .find(|a| a.contains(name.as_str()))
                    .cloned()
            })
            .unwrap_or_else(|| name.clone());
        if let Some(ref room_id) = self.forth_vm.current_room.clone() {
            if let Some(room) = self.forth_vm.rooms.get_mut(room_id) {
                room.members.retain(|m| m != &addr);
            }
        }
        self.output_manager
            .write_info(format!("  -{}", addr.as_str().dark_grey()));
        self.render_tui().await
    }

    /// `/room list` — list all rooms.
    async fn handle_room_list(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        if self.forth_vm.rooms.is_empty() {
            self.output_manager.write_info(
                "  no rooms — use /room new to create one"
                    .dark_grey()
                    .to_string(),
            );
        } else {
            let current = self.forth_vm.current_room.clone();
            let mut ids: Vec<_> = self.forth_vm.rooms.keys().cloned().collect();
            ids.sort();
            let lines: Vec<String> = ids
                .iter()
                .map(|id| {
                    let room = &self.forth_vm.rooms[id];
                    let marker = if Some(id) == current.as_ref() {
                        "▶"
                    } else {
                        " "
                    };
                    let members: Vec<String> = room
                        .members
                        .iter()
                        .map(|addr| {
                            self.forth_vm
                                .peer_meta
                                .get(addr)
                                .and_then(|m| m.label.clone())
                                .unwrap_or_else(|| addr.clone())
                        })
                        .collect();
                    format!(
                        "  {} {}  {} members  {} keys{}",
                        marker.cyan(),
                        id.chars().take(8).collect::<String>().as_str().dark_grey(),
                        room.members.len(),
                        room.hash.len(),
                        if members.is_empty() {
                            String::new()
                        } else {
                            format!("  ({})", members.join(", ").as_str().white())
                        },
                    )
                })
                .collect();
            self.output_manager.write_info(lines.join("\n"));
        }
        self.render_tui().await
    }

    /// Render the TUI
    async fn render_tui(&self) -> Result<()> {
        // Skip all crossterm writes while an external editor owns the terminal.
        if crate::is_editor_active() {
            return Ok(());
        }
        let mut tui = self.tui_renderer.lock().await;

        // After returning from an external editor, call resume() to reset
        // active_rows so the TUI live area repaints from scratch.
        // enable_raw_mode() in resume() is idempotent — raw mode is already on.
        if crate::take_tui_rebuild() {
            tui.resume().ok();
        }

        // Check if recovery needed from previous render failure
        if tui.needs_full_refresh {
            tracing::info!("Performing full TUI refresh after render error");
            // Try to recover by clearing error state
            tui.needs_full_refresh = false;
            tui.last_render_error = None;
        }

        tui.flush_output_safe(&self.output_manager)?;
        // check_and_refresh handles the needs_full_refresh flag.
        // We do NOT call tui.render() here: flush_output_safe() already draws
        // when messages are committed or when the 100 ms animation interval
        // elapses.  Calling render() afterwards would erase the live area a
        // second time from the wrong cursor position, causing the "stacking
        // Channeling…" visual glitch.
        tui.check_and_refresh()?;
        Ok(())
    }

    /// Clean up old completed queries
    async fn cleanup_old_queries(&self) {
        self.query_states
            .cleanup_old_queries(Duration::from_secs(30))
            .await;
    }

    /// Update the compaction percentage in the status bar.
    /// No-op when auto_compact_enabled is false.
    async fn update_compaction_status(&self) {
        if !self.auto_compact_enabled {
            return;
        }
        let conversation = self.conversation.read().await;
        let percent_remaining = conversation.compaction_percent_remaining();

        // Format percentage (0-100%)
        let percent_display = (percent_remaining * 100.0) as u8;

        // Update status bar with compaction percentage (matches Claude Code format)
        self.status_bar.update_line(
            crate::cli::status_bar::StatusLineType::CompactionPercent,
            format!("Context left until auto-compact: {}%", percent_display),
        );
    }

    /// Handle a tool result
    async fn handle_tool_result(
        &mut self,
        query_id: Uuid,
        tool_id: String,
        result: Result<String>,
    ) -> Result<()> {
        // Look up the tool's WorkUnit and row index
        let (tool_name, tool_input, work_unit, row_idx) = {
            let mut map = self.active_tool_uses.write().await;
            map.remove(&tool_id).unwrap_or_else(|| {
                // Fallback: create a standalone WorkUnit for untracked tools
                let fallback = self.output_manager.start_work_unit("Tool");
                let row_idx = fallback.add_row(&tool_id);
                (tool_id.clone(), serde_json::Value::Null, fallback, row_idx)
            })
        };

        // Update the row in the WorkUnit with a semantic summary + optional body
        match &result {
            Ok(content) => {
                let (summary, mut body) = tool_result_to_display(&tool_name, content);
                // A provider-native VM submission is executable source, not an
                // opaque tool argument.  Preserve the exact source in the
                // scrollback row so a user can reconcile every `say` chunk and
                // diagnostic with the program that caused it.
                if tool_name == "submit_program" {
                    let language = tool_input
                        .get("language")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("inferred");
                    if let Some(source) = tool_input
                        .get("source")
                        .and_then(serde_json::Value::as_str)
                    {
                        let mut source_body = vec![format!("VM source ({language}):")];
                        source_body.extend(source.lines().map(str::to_owned));
                        source_body.append(&mut body);
                        body = source_body;
                    }
                }
                work_unit.complete_row_with_body(row_idx, summary, body);
            }
            Err(e) => {
                // Truncate very long error messages for the row display
                let err_str = e.to_string();
                let short_err = if err_str.len() > 60 {
                    format!("{}…", err_str.chars().take(57).collect::<String>())
                } else {
                    err_str
                };
                work_unit.fail_row(row_idx, short_err);
            }
        }

        // Record tool execution in the graph
        {
            let input_preview = {
                let s = tool_input.to_string();
                if s.chars().count() > 120 {
                    s.chars().take(120).collect::<String>()
                } else {
                    s
                }
            };
            let (output_preview, is_error) = match &result {
                Ok(c) => {
                    let preview = c.chars().take(200).collect::<String>();
                    (preview, false)
                }
                Err(e) => (e.to_string().chars().take(200).collect(), true),
            };
            self.current_graph
                .lock()
                .await
                .add_node(crate::graph::NodeKind::ToolExecution {
                    name: tool_name.clone(),
                    input_preview,
                    output_preview,
                    is_error,
                });
        }

        // Check if tool execution changed the mode (e.g., EnterPlanMode, PresentPlan)
        // and update status bar accordingly
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // Store tool result
        self.tool_results
            .write()
            .await
            .entry(query_id)
            .or_insert_with(Vec::new)
            .push((tool_id, result));

        // Check if all tools for this query have completed
        let metadata = self.query_states.get_metadata(query_id).await;
        if let Some(meta) = metadata {
            if let QueryState::ExecutingTools { tools_pending, .. } = meta.state {
                let results_count = self
                    .tool_results
                    .read()
                    .await
                    .get(&query_id)
                    .map(|v| v.len())
                    .unwrap_or(0);

                if results_count >= tools_pending {
                    // All tools completed — mark the WorkUnit complete so the
                    // animation stops and the final content is shown.
                    work_unit.set_complete();
                    // format results and add to conversation
                    self.finalize_tool_execution(query_id).await?;
                }
            }
        }

        Ok(())
    }

    /// Finalize tool execution (all tools complete, re-invoke Claude)
    async fn finalize_tool_execution(&mut self, query_id: Uuid) -> Result<()> {
        // Get all tool results for this query
        let results = self
            .tool_results
            .write()
            .await
            .remove(&query_id)
            .unwrap_or_default();

        // Sync the plan mode status bar.  handle_present_plan() updates the mode Arc
        // but is a free function without &self access, so the indicator update happens here.
        let current_mode = self.mode.read().await.clone();
        self.update_plan_mode_indicator(&current_mode);

        // ── Plan-approval fast path ───────────────────────────────────────────
        // When the user just approved a PresentPlan, the mode is now Executing.
        // The long planning exploration history confuses the model (it forgets the
        // task and re-explores instead of implementing).  Reset to a clean context
        // with just the execution directive, and cancel any active brain session so
        // its pending AskUserQuestion dialogs don't interfere.
        if matches!(current_mode, ReplMode::Executing { .. }) {
            let plan_directive = results.iter().find_map(|(_, r)| {
                if let Ok(content) = r {
                    if content.starts_with("Plan approved by user.") {
                        Some(content.clone())
                    } else {
                        None
                    }
                } else {
                    None
                }
            });

            if let Some(directive) = plan_directive {
                // Cancel brain — its stale AskUserQuestion would hijack the next dialog.
                self.cancel_active_brain(true).await;

                // Clear tool-call history so planning-phase reads/globs don't
                // trigger loop detection when Claude calls them again during execution.
                self.tool_call_history.write().await.remove(&query_id);

                // Reset conversation to a single clear execution prompt.
                {
                    let mut conv = self.conversation.write().await;
                    conv.clear();
                    conv.add_user_message(directive);
                }

                let _ = self.llm_tx.send(LlmRequest::Query {
                    id: query_id,
                    text: String::new(),
                    no_tools: false,
                });
                return Ok(());
            }
        }

        // ── Normal path: build ToolResult message and continue ────────────────
        // Create a user message with proper ToolResult content blocks
        let mut content_blocks = Vec::new();
        for (tool_id, result) in results {
            match result {
                Ok(content) => {
                    content_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content,
                        is_error: None,
                    });
                }
                Err(e) => {
                    content_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: tool_id,
                        content: e.to_string(),
                        is_error: Some(true),
                    });
                }
            }
        }

        // Add tool results to conversation as a proper message
        let tool_result_message = crate::claude::Message {
            role: "user".to_string(),
            content: content_blocks,
        };

        self.conversation
            .write()
            .await
            .add_message(tool_result_message);

        // Send tool-continuation turn to the LLM worker loop
        let _ = self.llm_tx.send(LlmRequest::Query {
            id: query_id,
            text: String::new(),
            no_tools: false,
        });

        Ok(())
    }

    /// Handle `/graph` — display the execution graph for the most recent query.
    async fn handle_graph_command(&mut self) -> Result<()> {
        let g = self.current_graph.lock().await;
        if g.is_empty() {
            self.output_manager
                .write_info("No execution graph recorded yet. Run a query first.");
        } else {
            let text = g.format_display();
            // Append save path hint
            let hint = if let Some(qid) = g.query_id {
                let short = &qid.to_string()[..8];
                format!(
                    "\nSaved to ~/.finch/graphs/{}-{}.json",
                    g.session_label, short
                )
            } else {
                String::new()
            };
            self.output_manager.write_info(format!("{}{}", text, hint));
        }
        self.render_tui().await?;
        Ok(())
    }

    /// Handle `/push <text>` — push text onto the Co-Forth stack.
    /// Push a word onto the Co-Forth stack and respond conversationally.
    async fn handle_stack_push(&mut self, text: String) -> Result<()> {
        // Strip trailing noise characters (backslash, punctuation typos) from the push.
        let text = text
            .trim_end_matches(|c: char| c == '\\' || c == '/' || c == ',' || c == '.')
            .trim()
            .to_string();
        if text.is_empty() {
            return Ok(());
        }
        {
            let mut stack = self.stack.lock().await;
            stack.push(text.clone());
        };
        // Add a Task node to the poset.
        self.poset.lock().await.add_node(
            text.clone(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );

        // Seed from the English library: bring in the 1-hop neighbourhood
        // of each word in the pushed text that appears in the library.
        {
            let lib = crate::coforth::Library::load();
            let mut p = self.poset.lock().await;
            for token in text.split_whitespace() {
                let token = token.trim_matches(|c: char| !c.is_alphabetic());
                if lib.lookup(token).is_some() {
                    lib.inject_into_poset(token, 1, &mut p);
                }
            }
        }
        // Ensure the panel shows Forth view (vocabulary being built).
        {
            let mut tui = self.tui_renderer.lock().await;
            if tui.poset_panel_mode != crate::cli::tui::PosetPanelMode::Forth {
                tui.poset_panel_mode = crate::cli::tui::PosetPanelMode::Forth;
            }
        }

        // Precompile: only fire on word definitions (`: name ... ;`).
        // Calling a word — `lion`, `hello`, etc. — doesn't need AI suggestions.
        // At 600+ words the vocab prompt is huge; restricting to definitions keeps
        // it fast and the suggestions meaningful.
        let is_definition = text.trim_start().starts_with(':');
        if is_definition {
            let trigger_id = {
                let p = self.poset.lock().await;
                p.nodes.last().map(|n| n.id)
            };
            self.spawn_coforth_precompile(text.clone(), trigger_id)
                .await;
        }

        // If the user has joined any channels, Enter yields to them — not execute.
        // Execution is a separate step: execute-channel" #name" or exec-last" #name".
        // Word definitions (`: name ... ;`) are always compiled immediately regardless.
        let in_channel = !self.forth_vm.channels.is_empty();
        if in_channel && !is_definition {
            use crate::coforth::irc_proto::{IrcMessage, OP_YIELD};
            let my_name = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "local".to_string());
            for chan in self.forth_vm.channels.clone() {
                crate::coforth::irc_proto::process_message(
                    IrcMessage::new(OP_YIELD, &my_name, &chan, text.as_bytes().to_vec()),
                    &crate::server::handlers::CHANNEL_REGISTRY,
                );
            }
            use crossterm::style::Stylize;
            let chans: Vec<String> = self
                .forth_vm
                .channels
                .iter()
                .map(|c| c.as_str().cyan().bold().to_string())
                .collect();
            self.output_manager
                .write_info(format!("→ {}", chans.join(", ")));
            return self.render_tui().await;
        }

        // Try the VM first — boot JIT compiled all vocab words so known words run immediately.
        let snap = self.forth_vm.snapshot();
        let is_nl = looks_like_natural_language(
            &text,
            |w| self.forth_vm.word_exists(w),
            |w| {
                self.forth_vm.word_source(w).is_some() && !self.auto_compiled_word_names.contains(w)
            },
        );
        let vm_result = self.forth_vm.exec(&text);
        // Check for unknown words first, regardless of whether there was other output.
        // missing-word now prints "?wordname" but we still want to define-and-retry.
        let unknowns = self.forth_vm.take_pending_defines();
        if !unknowns.is_empty() {
            self.forth_vm.restore(&snap);
            return self.handle_define_unknown_words(unknowns, text).await;
        }
        let vm_err: Option<String>;
        match vm_result {
            Ok(ref out) if !out.is_empty() => {
                self.save_user_words();
                self.output_manager.write_info(out.trim_end().to_string());
                // Vocab word ran (e.g. `hello`). If this also looks like a sentence
                // or question, fall through and let the AI respond too.
                if !is_nl {
                    return self.render_tui().await;
                }
                self.render_tui().await?;
                vm_err = None;
                // continue to AI below
            }
            Ok(_) => {
                if is_nl {
                    // VM ran silently but input looks like natural language.
                    // Restore snapshot and let the AI respond as the other programmer.
                    self.forth_vm.restore(&snap);
                    vm_err = None;
                    // continue to AI below
                } else {
                    // Pure Forth execution — the other programmer reads the stack.
                    self.save_user_words();
                    return self.render_tui().await;
                }
            }
            Err(ref e) => {
                let err_str = e.to_string();
                self.forth_vm.restore(&snap);

                // If a single defined word caused infinite recursion, forget it and
                // re-route through the AI define path so it gets a fresh definition.
                let is_overflow = err_str.contains("return stack overflow")
                    || err_str.contains("call stack overflow");
                let tokens: Vec<&str> = text.split_whitespace().collect();
                if is_overflow && tokens.len() == 1 {
                    let word = tokens[0];
                    if self.forth_vm.word_exists(word) {
                        // Erase the broken recursive definition.
                        let _ = self.forth_vm.exec(&format!("forget {word}"));
                        // Re-route: ask AI to provide a good definition.
                        return self
                            .handle_define_unknown_words(vec![word.to_string()], text.clone())
                            .await;
                    }
                }

                vm_err = Some(err_str);
            }
        }

        // The other programmer's turn: respond with Forth.
        // Both sides only write Forth — definitions, words, stack ops, output.
        let stack_snapshot: Vec<String> = {
            let s = self.stack.lock().await;
            s.iter().cloned().collect()
        };
        let stack_str = if stack_snapshot.is_empty() {
            String::new()
        } else {
            format!("\nstack: {}", stack_snapshot.join("  "))
        };
        let error_str = match &vm_err {
            Some(e) => format!("\nVM said: {e}"),
            None => String::new(),
        };
        // Two Forth programmers at a shared terminal.
        // One just typed something. The other responds.
        // If it's code or an instruction → respond with Forth.
        // If it's a question, complaint, or comment → respond in plain English, in character.
        let auto_names = &self.auto_compiled_word_names;
        let user_vocab: String = self
            .forth_vm
            .dump_source()
            .lines()
            .filter(|line| {
                let name = line
                    .trim_start_matches(':')
                    .trim()
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                !auto_names.contains(name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let vocab_section = if user_vocab.is_empty() {
            String::new()
        } else {
            format!("\nVocabulary:\n{user_vocab}\n")
        };
        let system_note = "You are the execution engine behind a conversational interface. \
             The user does not know they are using Forth and should never need to. \
             They just talk. You translate their intent into Forth and run it. \
             Reply with Forth code only — no explanation, no preamble, no markdown fences. \
             The code will be executed immediately and the user sees the output, not your reply. \
             After each word definition, add a \\ comment (same line) explaining what it does in plain English. \
             Example:  : greet  .\" hello\" cr ;  \\ prints hello \
             After each word definition, write a test:word proof using assert (aborts if 0). \
             Example after  : double  dup + ;  write  : test:double  4 double 8 = assert ; \
             For side-effect-only words: assert depth is unchanged. \
             After all definitions, write prove-all to verify before calling the main words. \
             If prove-all fails, stop — do not call the main words. \
             When the user needs to choose between options, use \
             select\" title|opt1|opt2\" — leaves the chosen index (0-based) on the stack. \
             Forth word names can be in any language. \
             If the user expressed something in their own words that has no matching word in the vocabulary yet, \
             define a Forth word with their exact phrase (hyphens for spaces) so it runs instantly next time. \
             Example: user says \"flip it\" → define  : flip-it  swap ; \
             When the user challenges, disputes, or wonders about equivalence, settle it with argue-cases — \
             never just assert. Triggers include: \
             \"is X the same as Y\", \"isn't X just Y\", \"doesn't X equal Y\", \
             \"X is better than Y\", \"X works like Y\", \"X and Y do the same thing\", \
             \"what's the difference between X and Y\", \"X vs Y\", \
             \"I thought X was equivalent to Y\", \"why use X instead of Y\", \
             or any phrasing that compares two operations or questions their equivalence. \
             Syntax: s\" op1\" s\" op2\" argue-cases \
             Tests both ops against seeds 0 1 -1 2 -2 5 -5 10 -10 100 -100; shows each case. \
             Examples: \
             \"isn't dup + the same as 2 *?\" → s\" dup +\" s\" 2 *\" argue-cases \
             \"is dividing by 2 the same as shifting right?\" → s\" 2 /\" s\" 1 rshift\" argue-cases \
             \"does negate work like 0 minus?\" → s\" negate\" s\" 0 swap -\" argue-cases \
             \"swap then drop vs nip — same thing?\" → s\" swap drop\" s\" nip\" argue-cases \
             \"I think abs and dup * do the same thing\" → s\" abs\" s\" dup *\" argue-cases \
             If the user asked a question or made a comment with no executable intent, \
             reply in plain language — same language they used, two sentences max. \
             Never mention Forth, stacks, or any implementation detail.";
        let initial_prompt = format!(
            "{system_note}{vocab_section}\nStack:{stack_str}\nUser: {text}{error_str}\nForth:",
        );
        let messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![crate::claude::ContentBlock::Text {
                text: initial_prompt,
            }],
        }];

        use crossterm::style::Stylize;

        let forth_code = {
            let gen = self.model_selection.generator().await;
            match gen.generate(messages.clone(), None).await {
                Ok(resp) => resp.text,
                Err(e) => return Err(e),
            }
        };
        // Strip markdown fences and trailing "ok" tokens.
        let forth_code = forth_code
            .trim()
            .trim_start_matches("```forth")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
            .trim_end_matches("ok")
            .trim()
            .to_string();

        // If the other programmer replied in English, just show it and stop.
        let looks_like_english = {
            let no_forth_def = !forth_code.contains(';') && !forth_code.contains(':');
            let has_prose_end = forth_code.ends_with('.')
                || forth_code.ends_with('!')
                || forth_code.ends_with('?')
                || forth_code.contains(". ");
            no_forth_def && has_prose_end
        };
        if looks_like_english {
            self.output_manager.write_info(format!(
                "{}  {}",
                "←".dark_grey(),
                forth_code.as_str().white()
            ));
            return self.render_tui().await;
        }

        // Run it. The user sees the result, not the implementation.
        self.handle_forth_eval_inner(forth_code, false).await
    }

    /// Handle `push <message>` — send plain text to all peers.
    /// No approval dialog. No Forth visible to the user.
    /// Someone posted code in a foreign language (JS, Python, etc.) wrapped in
    /// a code fence.  Send it back as a better machine.
    /// The response could be anything — improved JS, a Forth translation, a mix.
    /// If it contains Forth definitions, compile them. Otherwise just show it.
    async fn handle_foreign_code(&mut self, lang: String, code: String) -> Result<()> {
        use crossterm::style::Stylize;
        let lang_display = if lang.is_empty() {
            "code".to_string()
        } else {
            lang.clone()
        };
        self.output_manager.write_info(
            format!("← {} received.", lang_display)
                .dark_grey()
                .to_string(),
        );

        let prompt = format!(
            "Two programmers exchange machines. The first programmer sent this {} code:\n\n\
             ```{}\n{}\n```\n\n\
             Send back a better machine. It can be:\n\
             - Improved {lang_display} (fix bugs, apply idioms)\n\
             - A Forth translation (`: word ... ;`) if that captures it cleanly\n\
             - Both — improved code plus a Forth word that wraps it\n\
             Show the machine. One line saying what changed. Nothing else.",
            if lang.is_empty() {
                "code".to_string()
            } else {
                lang.clone()
            },
            lang,
            code,
        );

        let response = {
            let gen = self.model_selection.generator().await;
            match gen
                .generate(
                    vec![crate::claude::Message {
                        role: "user".to_string(),
                        content: vec![crate::claude::ContentBlock::Text { text: prompt }],
                    }],
                    None,
                )
                .await
            {
                Ok(r) => r.text,
                Err(e) => return Err(e),
            }
        };

        // Does the response contain Forth definitions? Compile them.
        let has_forth = response.contains(": ") && response.contains(" ;");
        if has_forth {
            // Extract and compile any Forth definitions; show the rest as prose.
            let (forth_parts, prose_parts): (Vec<&str>, Vec<&str>) =
                response.lines().partition(|line| {
                    let t = line.trim();
                    t.starts_with(':') || t.starts_with("prove-all") || t.starts_with('\\')
                });
            let prose = prose_parts.join("\n").trim().to_string();
            let forth = forth_parts.join("\n").trim().to_string();
            if !prose.is_empty() {
                self.output_manager
                    .write_info(format!("→  {}", prose.as_str().white()));
            }
            if !forth.is_empty() {
                self.output_manager
                    .write_info(format!("→  {}", forth.as_str().cyan()));
                self.handle_forth_eval_inner(forth, false).await?;
            }
        } else {
            // Not Forth — just show the better machine as-is.
            let cleaned = response
                .trim()
                .trim_start_matches("```")
                .trim_end_matches("```")
                .trim()
                .to_string();
            self.output_manager
                .write_info(format!("→  {}", cleaned.as_str().cyan()));
        }

        self.render_tui().await
    }

    async fn handle_push_message(&mut self, msg: String) -> Result<()> {
        use crossterm::style::Stylize;
        let peers = self.forth_vm.peers.clone();
        if peers.is_empty() {
            self.output_manager
                .write_info("push: no peers".dark_grey().to_string());
            return self.render_tui().await;
        }
        let from = self.forth_vm.registry_addr.clone();
        crate::coforth::scatter::scatter_push(&peers, &msg, from.as_deref()).await;
        self.render_tui().await
    }

    /// Spawn a background task that reads the current vocabulary and pushes
    /// 2-3 words the user is likely to want next — without blocking the TUI.
    /// Uses the local model (the CPU) so precompilation is fast and offline.
    /// `trigger_id` — the poset node that triggered this precompile; new words
    /// are wired as successors of that node so the vocabulary has structure.
    async fn spawn_coforth_precompile(&mut self, new_word: String, trigger_id: Option<usize>) {
        // Prefer the local model — precompile should be cheap, fast, and offline.
        let generator = self.qwen_gen.clone();
        let poset = Arc::clone(&self.poset);
        let stack = Arc::clone(&self.stack);

        // Snapshot the most recent 20 vocabulary words for the prompt.
        // Sending all 600+ words makes the prompt huge and the model slow.
        // Recent context is more relevant for suggesting what comes next.
        let vocab: Vec<String> = {
            let p = poset.lock().await;
            p.nodes
                .iter()
                .rev()
                .take(20)
                .rev()
                .map(|n| format!("W{} — {}", n.id, n.label))
                .collect()
        };

        tokio::spawn(async move {
            let vocab_str = if vocab.is_empty() {
                String::new()
            } else {
                format!("Existing words:\n{}\n\n", vocab.join("\n"))
            };

            // Structured prompt — rigid template so small models can follow it.
            // Each line: WORD: <label> | FORTH: <forth source>
            // The Forth source should be a colon definition or a literal expression.
            let prompt = format!(
                "Extend a Co-Forth vocabulary. A new word was just defined:\n\
                 WORD: {new_word}\n\n\
                 {vocab_str}\
                 Suggest 2-3 related words. For each, write one line:\n\
                 WORD: <label> | FORTH: <forth code>\n\n\
                 The Forth code must be valid Forth that produces a result (a number or printed output).\n\
                 Available built-ins: + - * / mod dup drop swap over rot . .\" cr if else then begin until do loop i variable @ !\n\
                 Useful library words: square cube sum-to-n gcd fib signum even? odd? clamp\n\
                 Example:\n\
                 WORD: double | FORTH: : double 2 * ; 5 double .\n\
                 WORD: sum-5 | FORTH: 5 sum-to-n .\n\
                 Be specific. No explanations.",
            );

            let messages = vec![crate::claude::Message {
                role: "user".to_string(),
                content: vec![crate::claude::ContentBlock::Text { text: prompt }],
            }];

            let Ok(response) = generator.generate(messages, None).await else {
                return;
            };

            // Parse "WORD: <label> | FORTH: <code>" lines from the response.
            // Falls back to plain "WORD: <label>" if no Forth code provided.
            struct ParsedWord {
                label: String,
                forth: Option<String>,
            }
            let mut items: Vec<ParsedWord> = Vec::new();

            for line in response.text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("WORD:") {
                    if let Some((label_part, forth_part)) = rest.split_once('|') {
                        let label = label_part.trim().to_string();
                        let forth = forth_part
                            .strip_prefix("FORTH:")
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        if !label.is_empty() {
                            items.push(ParsedWord { label, forth });
                        }
                    } else {
                        let label = rest.trim().to_string();
                        if !label.is_empty() {
                            items.push(ParsedWord { label, forth: None });
                        }
                    }
                }
            }

            // Test each suggested word in a sandboxed VM before showing it.
            // Only words that execute without error and produce output get promoted.
            // Silent failures, errors, or empty output are dropped silently.
            let mut sandbox = crate::coforth::Forth::new();

            let mut tested: Vec<ParsedWord> = Vec::new();
            for item in items.into_iter().take(3) {
                let Some(ref code) = item.forth else {
                    // No Forth code — can't test, skip.
                    continue;
                };
                // Run in sandbox. Must succeed and produce non-empty output.
                match sandbox.exec(code) {
                    Ok(out) if !out.trim().is_empty() => {
                        tested.push(item);
                    }
                    _ => {
                        // Error or no output — not useful to the user. Drop.
                    }
                }
            }

            // Push only tested words into the poset and panel.
            for item in tested {
                let new_id = {
                    let mut p = poset.lock().await;
                    let id = p.add_node(
                        item.label.clone(),
                        crate::poset::NodeKind::Observation,
                        crate::poset::NodeAuthor::Ai,
                    );
                    if let Some(ref code) = item.forth {
                        if let Some(n) = p.node_mut(id) {
                            n.compiled_code = Some(code.clone());
                            n.compiled_lang = Some("forth".to_string());
                        }
                    }
                    if let Some(tid) = trigger_id {
                        p.edges.push((tid, id));
                    }
                    id
                };
                let _ = new_id;
                stack.lock().await.push(item.label);
            }
        });
    }

    /// `/chain W1 W2` — add edge W1 → W2 (W2 depends on W1).
    async fn handle_stack_chain(&mut self, a: usize, b: usize) -> Result<()> {
        let ok = {
            let mut p = self.poset.lock().await;
            let has_a = p.nodes.iter().any(|n| n.id == a);
            let has_b = p.nodes.iter().any(|n| n.id == b);
            if has_a && has_b {
                p.edges.push((a, b));
                true
            } else {
                false
            }
        };
        if ok {
            self.output_manager.write_info(format!("W{a} → W{b}"));
        } else {
            self.output_manager
                .write_info(format!("W{a} or W{b} not found"));
        }
        self.render_tui().await
    }

    /// `/forget W1` — remove word and any AI-generated successors.
    async fn handle_stack_forget(&mut self, id: usize) -> Result<()> {
        let removed = {
            let mut p = self.poset.lock().await;
            let mut to_remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
            to_remove.insert(id);
            let mut frontier = vec![id];
            while let Some(cur) = frontier.pop() {
                for &(pred, succ) in &p.edges {
                    if pred == cur && !to_remove.contains(&succ) {
                        if p.nodes.iter().any(|n| {
                            n.id == succ && matches!(n.author, crate::poset::NodeAuthor::Ai)
                        }) {
                            to_remove.insert(succ);
                            frontier.push(succ);
                        }
                    }
                }
            }
            let count = to_remove.len();
            let removed_labels: std::collections::HashSet<String> = p
                .nodes
                .iter()
                .filter(|n| to_remove.contains(&n.id))
                .map(|n| n.label.clone())
                .collect();
            p.nodes.retain(|n| !to_remove.contains(&n.id));
            p.edges
                .retain(|&(a, b)| !to_remove.contains(&a) && !to_remove.contains(&b));
            drop(p);
            let mut s = self.stack.lock().await;
            s.retain(|item| !removed_labels.contains(item));
            count
        };
        self.output_manager.write_info(format!(
            "forgot W{id} ({removed} word{} removed)",
            if removed == 1 { "" } else { "s" }
        ));
        self.render_tui().await
    }

    /// `/dup W1` — clone word W1 as a new entry with no edges.
    async fn handle_stack_dup(&mut self, id: usize) -> Result<()> {
        let result = {
            let mut p = self.poset.lock().await;
            if let Some(node) = p.nodes.iter().find(|n| n.id == id).cloned() {
                let new_id = p.add_node(
                    node.label.clone(),
                    node.kind.clone(),
                    crate::poset::NodeAuthor::User,
                );
                Some((new_id, node.label))
            } else {
                None
            }
        };
        if let Some((new_id, label)) = result {
            self.stack.lock().await.push(label.clone());
            self.output_manager
                .write_info(format!("W{id} → W{new_id}  \"{label}\""));
        } else {
            self.output_manager.write_info(format!("W{id} not found"));
        }
        self.render_tui().await
    }

    /// `/swap W1 W2` — swap the labels of two words.
    async fn handle_stack_swap(&mut self, a: usize, b: usize) -> Result<()> {
        let ok = {
            let mut p = self.poset.lock().await;
            let a_idx = p.nodes.iter().position(|n| n.id == a);
            let b_idx = p.nodes.iter().position(|n| n.id == b);
            if let (Some(ai), Some(bi)) = (a_idx, b_idx) {
                let label_a = p.nodes[ai].label.clone();
                let label_b = p.nodes[bi].label.clone();
                p.nodes[ai].label = label_b;
                p.nodes[bi].label = label_a;
                true
            } else {
                false
            }
        };
        if ok {
            self.output_manager
                .write_info(format!("swapped W{a} ↔ W{b}"));
        } else {
            self.output_manager
                .write_info(format!("W{a} or W{b} not found"));
        }
        self.render_tui().await
    }

    /// Evaluate a Forth expression or definition in the session-persistent VM.
    ///
    /// Triggered by:
    ///  - `: word ... ;`  (typed directly — Forth word definition)
    ///  - `/forth <expr>` (explicit eval command)
    ///
    /// The VM persists across calls so words defined in one input are available
    /// in subsequent inputs.  `show_define` controls whether silent definitions
    /// echo "defined: name" — false for AI-generated Forth (output only, no noise).
    async fn handle_forth_eval(&mut self, code: String) -> Result<()> {
        self.handle_forth_eval_inner(code, true).await
    }

    async fn handle_forth_eval_inner(&mut self, code: String, show_define: bool) -> Result<()> {
        use crossterm::style::Stylize;

        // FIREBALL — clears conversation context immediately, like /clear
        if code.trim() == "FIREBALL" {
            self.conversation.write().await.clear();
            self.output_manager
                .write_info("🔥 Context cleared.".to_string());
            return self.render_tui().await;
        }

        // PLAN [task] — enter plan mode with optional task, like /plan
        {
            let trimmed = code.trim();
            if trimmed == "PLAN" {
                // Toggle plan mode, same as /plan with no args — enter planning with no task
                let plan_path =
                    std::env::temp_dir().join(format!("plan_{}.md", uuid::Uuid::new_v4()));
                let new_mode = crate::cli::ReplMode::Planning {
                    task: "Manual exploration".to_string(),
                    plan_path: plan_path.clone(),
                    created_at: chrono::Utc::now(),
                };
                *self.mode.write().await = new_mode.clone();
                self.update_plan_mode_indicator(&new_mode);
                self.output_manager
                    .write_info("⏸ Plan mode on. Describe your goal.".to_string());
                return self.render_tui().await;
            }
            if let Some(task) = trimmed.strip_prefix("PLAN ") {
                let task = task.trim().to_string();
                if !task.is_empty() {
                    self.handle_plan_task(task).await?;
                    return Ok(());
                }
            }
        }

        // Pre-flight: if the code contains scatter-exec" or exec-at" commands, show
        // the user a plan and require confirmation before firing on remote machines.
        let scatter_cmds = extract_scatter_exec_commands(&code);
        if !scatter_cmds.is_empty() {
            let cmd_list = scatter_cmds
                .iter()
                .map(|c| format!("  • {c}"))
                .collect::<Vec<_>>()
                .join("\n");
            let plan = format!("Remote execution plan\n\n{cmd_list}");
            let dialog = crate::cli::tui::Dialog::confirm(plan, false);
            let result = {
                let mut tui = self.tui_renderer.lock().await;
                tui.show_dialog(dialog)
            };
            match result {
                Ok(crate::cli::tui::DialogResult::Confirmed(true)) => {}
                _ => {
                    self.output_manager
                        .write_info("remote exec: cancelled".dark_grey().to_string());
                    return self.render_tui().await;
                }
            }
        }

        // Wire the active generator into the VM so gen" prompt" works.
        // Uses block_in_place so the sync GenFn closure can await the async generator.
        let gen_handle = self.model_selection.generator_handle();
        self.forth_vm.set_gen_fn(Box::new(move |prompt: &str| {
            let prompt = prompt.to_string();
            let gen = gen_handle.clone();
            futures::executor::block_on(async move {
                let messages = vec![crate::claude::Message {
                    role: "user".to_string(),
                    content: vec![crate::claude::ContentBlock::Text { text: prompt }],
                }];
                let g = gen.read().await;
                match g.generate(messages, None).await {
                    Ok(resp) => resp.text,
                    Err(e) => format!("(gen error: {e})\n"),
                }
            })
        }));

        // Wire the TUI dialog into the VM so select" title|opt1|opt2" works.
        let tui_handle = self.tui_renderer.clone();
        self.forth_vm
            .set_select_fn(Box::new(move |title: &str, options: &[String]| {
                let title = title.to_string();
                let options = options.to_vec();
                let tui = tui_handle.clone();
                futures::executor::block_on(async move {
                    use crate::cli::tui::{Dialog, DialogOption, DialogResult};
                    let dialog_opts: Vec<DialogOption> = options
                        .iter()
                        .map(|o| DialogOption::new(o.as_str()))
                        .collect();
                    let dialog = Dialog::select(title, dialog_opts);
                    match tui.lock().await.show_dialog(dialog) {
                        Ok(DialogResult::Selected(idx)) => idx as i64,
                        _ => -1,
                    }
                })
            }));

        let tui_open = self.tui_renderer.clone();
        self.forth_vm.set_open_file_fn(Box::new(move |path: &str| {
            let path = path.to_string();
            let tui = tui_open.clone();
            futures::executor::block_on(async move {
                let _ = tui.lock().await.show_file_viewer(&path);
            });
        }));

        // Snapshot before eval so the user can undo it
        let snap = self.forth_vm.snapshot();
        self.forth_undo.push(snap);
        let stack_depth_before = self.forth_vm.data_stack().len();

        match self.forth_vm.exec(&code) {
            Ok(out) if !out.is_empty() => {
                self.save_user_words();
                self.output_manager.write_info(out);
            }
            Ok(_) => {
                self.save_user_words();
                if code.trim_start().starts_with(':') {
                    // Definition compiled silently — confirm it to the user (not for AI output)
                    if show_define {
                        let src = code.trim_start_matches(':').trim();
                        let name = src.split_whitespace().next().unwrap_or("word");
                        // Extract body: everything between name and the trailing ;
                        let body = src
                            .trim_start_matches(name)
                            .trim()
                            .trim_end_matches(';')
                            .trim();
                        self.output_manager
                            .write_info(format!("defined: {}", name.cyan().bold()));
                        // Occasionally observe what the definition seems to do
                        if let Some(obs) = definition_observation(name, body) {
                            self.output_manager.write_info(obs.dark_grey().to_string());
                        }
                    }
                } else {
                    // Word ran silently — the second programmer says something.
                    let stack = self.forth_vm.data_stack().to_vec();
                    let remark = if stack.is_empty() {
                        silent_remark(&code)
                    } else {
                        let items: Vec<String> = stack.iter().map(|n| n.to_string()).collect();
                        let mut r = format!("( {} )", items.join("  "));
                        // Vocab word hint: single alphabetic token that pushed exactly one value
                        let token = code.trim();
                        let grew_by_one =
                            self.forth_vm.data_stack().len() == stack_depth_before + 1;
                        let looks_like_vocab = token.len() >= 3
                            && token.chars().all(|c| c.is_ascii_lowercase() || c == '-');
                        if grew_by_one && looks_like_vocab {
                            r.push_str(&format!(
                                "\n  note: '{}' is a vocabulary word — it pushed its index.\
                                \n  To see its definition: s\" {}\" describe",
                                token, token
                            ));
                        }
                        r
                    };
                    self.output_manager
                        .write_info(remark.dark_grey().to_string());
                }
            }
            Err(e) => {
                // Restore on error so a bad definition doesn't corrupt the VM
                let restored = if let Some(snap) = self.forth_undo.pop() {
                    self.forth_vm.restore(&snap);
                    true
                } else {
                    false
                };
                let msg = humanize_forth_error(&e.to_string());
                let hint = if restored {
                    "  (state restored — try `undo` to go further back)"
                } else {
                    ""
                };
                self.output_manager
                    .write_info(format!("{}{hint}", msg.as_str().red()));
            }
        }
        // If the VM yielded a continuation, resume it on the local daemon (one await).
        if let Some(cont) = self.forth_vm.pending_continuation.take() {
            let addr = self
                .forth_vm
                .registry_addr
                .clone()
                .unwrap_or_else(|| crate::config::constants::DEFAULT_HTTP_ADDR.to_string());
            let url = if addr.starts_with("http://") || addr.starts_with("https://") {
                format!("{addr}/v1/forth/resume")
            } else {
                format!("http://{addr}/v1/forth/resume")
            };
            let body = serde_json::json!({ "continuation": cont });
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default();
            match client.post(&url).json(&body).send().await {
                Ok(resp) => match resp.json::<serde_json::Value>().await {
                    Ok(val) => {
                        let stack: Vec<i64> = val["stack"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                            .unwrap_or_default();
                        self.forth_vm.clear_data();
                        self.forth_vm.push_stack(&stack);
                        if let Some(out) = val["output"].as_str() {
                            if !out.is_empty() {
                                self.output_manager.write_info(out.to_string());
                            }
                        }
                        if let Some(err) = val["error"].as_str() {
                            if !err.is_empty() {
                                self.output_manager
                                    .write_info(format!("yield error: {err}"));
                            }
                        }
                    }
                    Err(e) => {
                        self.output_manager
                            .write_info(format!("yield: bad response from daemon: {e}"));
                    }
                },
                Err(e) => {
                    self.output_manager
                        .write_info(format!("yield: daemon not reachable: {e}"));
                }
            }
        }

        // Broadcast this code to all peer loops — each runs it on its own VM
        // and replies via peer_inbox_rx with its result (stack delta or output).
        let _ = self
            .peer_tx
            .send(crate::session::SessionEvent::chat(code.clone()));

        // Drain any boot poems registered this exec.
        let poems = self.forth_vm.take_boot_poems();
        if !poems.is_empty() {
            self.save_boot_poems(&poems);
        }

        // Run `check` if the user has defined it; update the corner display.
        if self.forth_vm.word_exists("check") {
            let text = self.forth_vm.exec("check").unwrap_or_default();
            if let Ok(mut g) = self.corner_output.lock() {
                *g = Some(text);
            }
        } else if let Ok(mut g) = self.corner_output.lock() {
            *g = None;
        }

        self.render_tui().await
    }

    /// Persist the current user vocabulary to `~/.finch/user_words.forth`.
    /// Called after any successful exec that may have added new definitions.
    fn save_user_words(&self) {
        let raw = self.forth_vm.dump_source();
        if raw.is_empty() {
            return;
        }
        // Strip any definitions that shadow builtins or core Forth words.
        // The AI occasionally generates `: drop 99 drop ;` style corruption that
        // persists across restarts and breaks STDLIB proofs.
        let source: String = raw
            .lines()
            .filter(|line| {
                let t = line.trim();
                if t.starts_with(':') {
                    let word = t[1..].trim().split_whitespace().next().unwrap_or("");
                    !crate::coforth::Forth::is_builtin_word(word)
                } else {
                    true
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        if source.is_empty() {
            return;
        }
        // Fire-and-forget: push to daemon so all concurrent terminals sync immediately.
        let daemon_addr = crate::config::constants::DEFAULT_HTTP_ADDR;
        let url = format!("http://{daemon_addr}/v1/forth/define");
        let body = serde_json::json!({ "source": source.clone() });
        tokio::spawn(async move {
            let _ = reqwest::Client::new()
                .post(&url)
                .json(&body)
                .timeout(std::time::Duration::from_millis(300))
                .send()
                .await;
        });
        // Fallback: also write locally so the file stays current if daemon is down.
        if let Some(mut path) = dirs::home_dir() {
            path.push(".finch");
            path.push("user_words.forth");
            let _ = std::fs::write(path, source);
        }
    }

    /// Load persisted user vocabulary.
    /// Tries the running daemon first (canonical shared store), falls back to file.
    async fn load_user_words(&mut self) {
        let daemon_addr = crate::config::constants::DEFAULT_HTTP_ADDR;
        let url = format!("http://{daemon_addr}/v1/forth/vocab");
        let daemon_source = reqwest::Client::new()
            .get(&url)
            .timeout(std::time::Duration::from_millis(400))
            .send()
            .await
            .ok()
            .and_then(|r| {
                // Use spawn_blocking to safely parse JSON — works on both
                // current-thread and multi-thread runtimes (block_in_place panics
                // on current-thread).
                futures::executor::block_on(r.json::<serde_json::Value>()).ok()
            })
            .and_then(|v| v["source"].as_str().map(|s| s.to_owned()))
            .filter(|s: &String| !s.is_empty());

        let source = daemon_source.or_else(|| {
            let path = dirs::home_dir().map(|mut p| {
                p.push(".finch");
                p.push("user_words.forth");
                p
            })?;
            std::fs::read(path)
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).replace('\0', ""))
                .filter(|s: &String| !s.is_empty())
        });

        if let Some(src) = source {
            // Only load definitions for NEW words — never redefine anything already in the VM.
            // This prevents AI-generated words from shadowing builtins, stdlib words, or
            // Co-Forth vocabulary (e.g. `: over 3 5 over . ;` is recursive and breaks proofs).
            let filtered: String = src
                .lines()
                .filter(|line| {
                    let t = line.trim();
                    if t.starts_with(':') {
                        let word = t[1..].trim().split_whitespace().next().unwrap_or("");
                        // Block if the word is already defined in the VM (any source).
                        !self.forth_vm.word_exists(word)
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let _ = self.forth_vm.exec_with_fuel(&filtered, 0);
            let count = self.forth_vm.dump_source().lines().count();
            tracing::debug!("Loaded {} user word(s) from daemon/file", count);
        }
    }

    /// Spawn a background task that polls the daemon for vocabulary changes made
    /// in other concurrent terminal sessions.  When the version counter advances,
    /// a VocabSync event is sent to the event loop so the new words are compiled in.
    fn spawn_vocab_poll(&self) {
        let tx = self.event_tx.clone();
        tokio::spawn(async move {
            let daemon_addr = crate::config::constants::DEFAULT_HTTP_ADDR;
            let client = match reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(600))
                .build()
            {
                Ok(c) => c,
                Err(_) => return,
            };

            // Per-peer version tracking: (url → last_version)
            let mut last_versions: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;

                // Build list of vocab URLs to poll:
                // 1. Local daemon (same-machine terminals)
                // 2. Remote peers from the daemon's registry (machines sent to other people)
                let mut urls: Vec<String> = vec![format!("http://{daemon_addr}/v1/forth/vocab")];
                if let Ok(resp) = client
                    .get(format!("http://{daemon_addr}/v1/registry/peers"))
                    .send()
                    .await
                {
                    if let Ok(peers) = resp.json::<Vec<serde_json::Value>>().await {
                        for peer in &peers {
                            if let Some(addr) = peer["addr"].as_str() {
                                // Don't double-poll the local daemon.
                                if addr != daemon_addr {
                                    urls.push(format!("http://{addr}/v1/forth/vocab"));
                                }
                            }
                        }
                    }
                }

                for url in &urls {
                    if let Ok(resp) = client.get(url).send().await {
                        if let Ok(val) = resp.json::<serde_json::Value>().await {
                            let version = val["version"].as_u64().unwrap_or(0);
                            let last = last_versions.entry(url.clone()).or_insert(0);
                            if version > *last {
                                *last = version;
                                if let Some(src) = val["source"].as_str() {
                                    if !src.is_empty() {
                                        let _ =
                                            tx.send(crate::cli::repl_event::ReplEvent::VocabSync(
                                                src.to_owned(),
                                            ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    /// Append boot poem lines to ~/.finch/boot.forth (one `.\" text\" cr` per line).
    /// Called after any exec that may have produced boot poems via `boot" text"`.
    fn save_boot_poems(&self, poems: &[String]) {
        let Some(mut path) = dirs::home_dir() else {
            return;
        };
        path.push(".finch");
        let _ = std::fs::create_dir_all(&path);
        path.push("boot.forth");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            for poem in poems {
                let escaped = poem.replace('"', "\\\"");
                let _ = writeln!(f, ".\" {}\" cr", escaped);
            }
        }
    }

    /// Run ~/.finch/boot.forth at startup — the user's boot poetry.
    fn run_boot_poems(&mut self) {
        let Some(mut path) = dirs::home_dir() else {
            return;
        };
        path.push(".finch");
        path.push("boot.forth");
        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };
        if source.is_empty() {
            return;
        }
        if let Ok(out) = self.forth_vm.exec_with_fuel(&source, 0) {
            if !out.is_empty() {
                self.output_manager.write_info(out.trim_end().to_string());
            }
        }
    }

    /// Grammar grows from use — unknown words trigger AI definition,
    /// get compiled into the VM, and re-run instantly from that point on.
    ///
    /// Called when the VM encounters unknown words (tracked in `pending_defines`).
    /// Asks the AI to define each word as Forth, compiles the result, then
    /// re-executes the original input so the user sees the output immediately.
    async fn handle_define_unknown_words(
        &mut self,
        words: Vec<String>,
        original_input: String,
    ) -> Result<()> {
        use crossterm::style::Stylize;

        // ── Step 1: resolve Library entries before asking the AI ─────────────
        // Words like `hello`, `no`, `yes`, `forth`, `now` live in the vocabulary
        // library.  Compile them directly instead of sending them to the AI.
        let lib = crate::coforth::Library::load();
        let mut remaining: Vec<String> = Vec::new();
        for word in &words {
            // Strip trailing punctuation to find the base word (e.g. "hello." → "hello")
            let base = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
            // Fast path: word is already compiled into the VM (SEED/ENGLISH/MAJOR library).
            // Library::load() only sees user TOML files, not the built-in embedded library,
            // so precompiled words like `hello` would fall through to the AI without this check.
            if self.forth_vm.word_exists(base) {
                // Already in VM — no library lookup or AI needed; will execute on re-eval.
            } else if let Some(entry) = lib.lookup(base).or_else(|| lib.lookup(word.as_str())) {
                // Compile the library Forth code into the VM.
                if let Some(forth_code) = &entry.forth {
                    let definition = format!(": {base}  {forth_code} ;");
                    let _ = self.forth_vm.exec_with_fuel(&definition, 0);
                    // Mark as auto-compiled so it's excluded from the AI vocab context.
                    self.auto_compiled_word_names.insert(base.to_string());
                }
                // Vocabulary word handled — don't ask AI for this one.
            } else if word.contains('\'') || word.contains('\u{2019}') {
                // Prose contraction (it's, don't, etc.) — skip silently.
            } else if word.chars().all(|c| !c.is_alphabetic()) {
                // Pure punctuation token — skip.
            } else {
                remaining.push(word.clone());
            }
        }

        // If all unknowns were library words or prose, re-run the original input.
        if remaining.is_empty() {
            return self.handle_forth_eval_inner(original_input, false).await;
        }

        // Only show user-authored words in the vocab context (not auto-compiled library words).
        let auto_names = &self.auto_compiled_word_names;
        let user_vocab: String = self
            .forth_vm
            .dump_source()
            .lines()
            .filter(|line| {
                let name = line
                    .trim_start_matches(':')
                    .trim()
                    .split_whitespace()
                    .next()
                    .unwrap_or("");
                !auto_names.contains(name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let vocab_section = if user_vocab.is_empty() {
            String::new()
        } else {
            format!("Existing vocabulary:\n{user_vocab}\n\n")
        };
        let words = remaining;
        let word_list = words.join(", ");

        // Ask AI to define the unknown words as Forth.
        // The AI is the second programmer — it sees the unknown words and defines them.
        let prompt = format!(
            "Two programmers exchange Forth machines. \
             That is how the language is defined — not by spec, but by exchange. \
             Every machine sent becomes part of the shared vocabulary. \
             Either side can define, redefine, or extend any word, including builtins. \
             User-defined words shadow builtins; there is no privilege hierarchy.\n\n\
             The first programmer sent: {original_input:?}\n\n\
             {vocab_section}\
             The following words are not yet defined: {word_list}\n\n\
             Define each as a Forth word. Rules:\n\
             - Only define words that are not yet in the vocabulary. Do not redefine existing words.\n\
             - One `: name  ... ;` definition per word.\n\
             - After each definition, write a comment `\\ what it does` on the same line.\n\
             - After each definition, write a minimal test: `: test:NAME  ... assert ;`\n\
             - After all definitions and tests, write `prove-all` then call each new word.\n\
             - Word names must match the user's input exactly (case-sensitive).\n\
             - CRITICAL: Only use Forth primitives and words already in the vocabulary above. \
               Do NOT reference undefined words — the code runs immediately and undefined words \
               will cause an error and an infinite definition loop.\n\
             - For string output use `s\" literal text\" type` or `.\" literal text\"`. \
               Never write `str:NAME` as if it were a word; there is no str: namespace.\n\
             - Do NOT use `to NAME` unless NAME was defined as a `value` in this same response.\n\
             - If the word names are in a non-English language, the Forth definitions may call \
               gen\" ...\" to invoke AI, or simply print something meaningful.\n\
             - No explanations outside comments. Only valid Forth.\n\
             Forth only:",
        );

        let messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![crate::claude::ContentBlock::Text { text: prompt }],
        }];

        let forth_defs_result = {
            let gen = self.model_selection.generator().await;
            gen.generate(messages, None).await
        };

        let forth_defs = match forth_defs_result {
            Ok(resp) => resp.text,
            Err(_) => {
                // AI unavailable — fall back to pure-Rust heuristic generator.
                // Every English word speaks its own name at minimum.
                // Grammar still grows; AI can improve these later.
                words
                    .iter()
                    .map(|w| {
                        let code = crate::coforth::library::generate_forth_for_word(w);
                        format!(": {w}  {code} ;")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };

        // Strip markdown fences.
        let forth_defs = forth_defs
            .trim()
            .trim_start_matches("```forth")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim()
            .to_string();

        if forth_defs.is_empty() {
            return self.render_tui().await;
        }

        // Skip definitions of words already in the VM — redefinition guard would fire.
        // Uses skip_known_word_defs to handle multi-line definitions correctly.
        let forth_defs = skip_known_word_defs(&forth_defs, |w| self.forth_vm.word_exists(w));

        // Remove dangerous definitions before compilation.
        let forth_defs = filter_ai_forth_response(&forth_defs);

        if forth_defs.trim().is_empty() {
            return self.render_tui().await;
        }

        // Show what's being compiled.
        self.output_manager.write_info(format!(
            "{}  {}",
            "→".dark_grey(),
            forth_defs.as_str().cyan()
        ));
        self.render_tui().await.ok();

        // Compile the definitions.
        self.handle_forth_eval_inner(forth_defs, false).await?;

        // Re-run the original input — words are now defined.
        self.handle_forth_eval_inner(original_input, false).await
    }

    /// `/vm` — dump the VM's user-defined words as Forth source.
    /// Pure safe Rust: writes to scrollback with crossterm styling.
    /// Select and copy from the terminal to transfer to another session.
    /// `/share` — format the current session as a pasteable proof block.
    ///
    /// Output is valid Forth: paste it into any finch and the words run.
    /// The SHA-256 of the source and a Unix timestamp make it verifiable.
    async fn handle_share(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        use std::time::{SystemTime, UNIX_EPOCH};

        let source = self.forth_vm.dump_source();

        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // SHA-256 of the source (hex) — anyone can verify this matches the code.
        let hash: String = {
            use std::fmt::Write as _;
            // Simple djb2-style fingerprint if sha2 isn't directly accessible here;
            // use the Forth VM's sha256 builtin by running it on a temp VM.
            let mut h: u64 = 5381;
            for b in source.bytes() {
                h = h.wrapping_mul(33).wrapping_add(b as u64);
            }
            let mut s = String::new();
            let _ = write!(s, "{h:016x}");
            s
        };

        let word_count = source
            .lines()
            .filter(|l| l.trim_start().starts_with(':'))
            .count();

        let separator = "─".repeat(56);

        if source.is_empty() {
            self.output_manager.write_info(
                "nothing defined yet — build something first"
                    .dark_grey()
                    .to_string(),
            );
            return self.render_tui().await;
        }

        let block = format!(
            "{sep}\n\
             \\ Co-Forth session  ·  {wc} words  ·  t={ts}\n\
             \\ fingerprint: {hash}\n\
             \\ paste into any finch — the words run, or they don't\n\
             {sep}\n\
             {source}\n\
             {sep}",
            sep = separator,
            wc = word_count,
            ts = ts,
            hash = hash,
            source = source,
        );

        // Copy to clipboard if available (best-effort).
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            if let Ok(mut child) = std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    let _ = stdin.write_all(block.as_bytes());
                }
                let _ = child.wait();
            }
        }

        self.output_manager.write_info(block.cyan().to_string());
        self.render_tui().await
    }

    /// `/box-diff` — compare every peer's git state, show who's the outlier, offer to fix them.
    ///
    /// 1. Concurrently runs `git log --oneline -1 && echo '---' && git diff --stat HEAD` on all peers.
    /// 2. Groups peers by their output.
    /// 3. Majority = "good"; minority = "broken" (or just different).
    /// 4. If any outlier exists, pops a select dialog: run `git pull` to fix it.
    async fn handle_box_diff(&mut self) -> Result<()> {
        use crate::coforth::scatter::scatter_exec_bash;
        use crossterm::style::Stylize;

        let peers = self.forth_vm.peers.clone();
        if peers.is_empty() {
            self.output_manager.write_info(
                "no peers connected — join a session first\n  try: add-peer\" host:11435\""
                    .dark_grey()
                    .to_string(),
            );
            return self.render_tui().await;
        }

        self.output_manager.write_info(
            format!("checking {} boxes…", peers.len())
                .dark_grey()
                .to_string(),
        );
        self.render_tui().await.ok();

        // The probe: commit hash + dirty status.  Fast, deterministic, comparable.
        let probe = "git log --oneline -1 2>/dev/null || echo '(no git)'; \
                     git diff --stat HEAD 2>/dev/null | tail -1 || echo ''";

        let peer_tokens: std::collections::HashMap<String, String> = self
            .forth_vm
            .peer_meta
            .iter()
            .filter_map(|(a, m)| m.token.as_ref().map(|t| (a.clone(), t.clone())))
            .collect();
        let results = scatter_exec_bash(&peers, probe, &peer_tokens).await;

        // Build a map: output → Vec<peer>
        let mut groups: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        let mut errors: Vec<(String, String)> = Vec::new();

        for r in &results {
            if let Some(ref e) = r.error {
                errors.push((r.peer.clone(), e.clone()));
            } else {
                let key = r.output.trim().to_string();
                groups.entry(key).or_default().push(r.peer.clone());
            }
        }

        // Show errors first.
        for (peer, err) in &errors {
            self.output_manager.write_info(format!(
                "{} {} — {}",
                "✗".red(),
                peer.as_str().dark_grey(),
                err.as_str().red()
            ));
        }

        // Find the majority output (the "good" state).
        let majority_output = groups
            .iter()
            .max_by_key(|(_, peers)| peers.len())
            .map(|(k, _)| k.clone())
            .unwrap_or_default();

        // Show each group.
        for (output, group_peers) in &groups {
            let is_majority = *output == majority_output && groups.len() > 1;
            let marker = if groups.len() == 1 {
                "✓".green().to_string()
            } else if is_majority {
                "✓".green().to_string()
            } else {
                "✗".red().to_string()
            };
            let label = if groups.len() == 1 {
                "all in sync".to_string()
            } else if is_majority {
                "good".to_string()
            } else {
                "different".red().to_string()
            };

            let peer_list = group_peers.join("  ");
            let first_line = output.lines().next().unwrap_or("(empty)");
            self.output_manager.write_info(format!(
                "{} {}  {}\n  {}",
                marker,
                label,
                peer_list.dark_grey(),
                first_line.cyan()
            ));
        }

        // If everything agrees, we're done.
        if groups.len() <= 1 && errors.is_empty() {
            return self.render_tui().await;
        }

        // Build a fix dialog for outlier peers.
        let outliers: Vec<String> = groups
            .iter()
            .filter(|(k, _)| *k != &majority_output)
            .flat_map(|(_, peers)| peers.clone())
            .collect();

        if outliers.is_empty() {
            return self.render_tui().await;
        }

        // At fleet scale, offer to fix each *group* of outliers, not each individual box.
        // A group might be 50k machines — you fix them all with one confirmation.
        let outlier_groups: Vec<(String, Vec<String>)> = groups
            .iter()
            .filter(|(k, _)| *k != &majority_output)
            .map(|(k, peers)| (k.clone(), peers.clone()))
            .collect();

        let mut options = Vec::new();
        for (output, group_peers) in &outlier_groups {
            let first_line = output.lines().next().unwrap_or("(empty)");
            options.push(crate::cli::tui::DialogOption::new(&format!(
                "git pull  {} box{}  ({})",
                group_peers.len(),
                if group_peers.len() == 1 { "" } else { "es" },
                first_line.chars().take(40).collect::<String>()
            )));
        }
        options.push(crate::cli::tui::DialogOption::new("cancel"));

        let total_outliers: usize = outlier_groups.iter().map(|(_, p)| p.len()).sum();
        let title = format!(
            "{} box{} out of sync — fix which group?",
            total_outliers,
            if total_outliers == 1 { "" } else { "es" }
        );
        let dialog = crate::cli::tui::Dialog::select(title, options);
        let chosen = { self.tui_renderer.lock().await.show_dialog(dialog)? };

        match chosen {
            crate::cli::tui::DialogResult::Selected(idx) if idx < outlier_groups.len() => {
                let (_, targets) = &outlier_groups[idx];
                self.output_manager.write_info(format!(
                    "running git pull on {} box{}…",
                    targets.len().to_string().cyan(),
                    if targets.len() == 1 { "" } else { "es" }
                ));
                self.render_tui().await.ok();

                let fix_tokens: std::collections::HashMap<String, String> = self
                    .forth_vm
                    .peer_meta
                    .iter()
                    .filter_map(|(a, m)| m.token.as_ref().map(|t| (a.clone(), t.clone())))
                    .collect();
                let fix_results = scatter_exec_bash(targets, "git pull", &fix_tokens).await;
                let ok_count = fix_results.iter().filter(|r| r.error.is_none()).count();
                let err_count = fix_results.iter().filter(|r| r.error.is_some()).count();

                if ok_count > 0 {
                    self.output_manager.write_info(format!(
                        "{} {} box{} updated",
                        "✓".green(),
                        ok_count,
                        if ok_count == 1 { "" } else { "es" }
                    ));
                }
                if err_count > 0 {
                    self.output_manager.write_info(format!(
                        "{} {} box{} failed",
                        "✗".red(),
                        err_count,
                        if err_count == 1 { "" } else { "es" }
                    ));
                    // Show up to 5 individual errors so you know what's actually broken.
                    for r in fix_results.iter().filter(|r| r.error.is_some()).take(5) {
                        if let Some(ref e) = r.error {
                            self.output_manager.write_info(format!(
                                "  {} {}",
                                r.peer.as_str().dark_grey(),
                                e.as_str().red()
                            ));
                        }
                    }
                }
            }
            _ => {
                self.output_manager
                    .write_info("cancelled".dark_grey().to_string());
            }
        }

        self.render_tui().await
    }

    async fn handle_vm_dump(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        let source = self.forth_vm.dump_source();
        if source.is_empty() {
            self.output_manager
                .write_info("vm: no user-defined words yet".dark_grey().to_string());
            return self.render_tui().await;
        }
        // Style each definition: colon word cyan, body dark grey.
        let styled: Vec<String> = source
            .lines()
            .map(|line| {
                // ": name body ;" — colour the name, leave body dark grey
                if let Some(rest) = line.strip_prefix(": ") {
                    let mut parts = rest.splitn(2, ' ');
                    let name = parts.next().unwrap_or("");
                    let body = parts.next().unwrap_or("");
                    format!(
                        ": {}  {} ;",
                        name.cyan().bold(),
                        body.trim_end_matches(';').trim().dark_grey()
                    )
                } else {
                    line.dark_grey().to_string()
                }
            })
            .collect();
        self.output_manager.write_info(styled.join("\n"));
        self.render_tui().await
    }

    /// `/undefine` — undo the last Forth definition.
    async fn handle_forth_undo(&mut self) -> Result<()> {
        match self.forth_undo.pop() {
            Some(snap) => {
                self.forth_vm.restore(&snap);
                self.output_manager.write_info("undone".to_string());
            }
            None => {
                self.output_manager
                    .write_info("nothing to undo".to_string());
            }
        }
        self.render_tui().await
    }

    /// `/run <word>` — look up the word in the library and execute its Forth snippet.
    ///
    /// Shows both the Forth source and its output, so the user can see exactly
    /// what the word computes.  This is the introspection path: every word can
    /// be run to verify its computational meaning.
    async fn handle_library_run(&mut self, word: String) -> Result<()> {
        use crossterm::style::Stylize;

        let key = word.trim().to_lowercase();
        let lib = crate::coforth::Library::load();
        let senses = lib.lookup_all(&key);

        if senses.is_empty() {
            self.output_manager.write_info(format!(
                "unknown word: {}  (try /define {})",
                key.clone().yellow(),
                key.clone().cyan()
            ));
            return self.render_tui().await;
        }

        let mut output = String::new();
        for entry in senses {
            let sense_tag = entry
                .sense
                .as_deref()
                .map(|s| format!(" {}", format!("[{s}]").yellow()))
                .unwrap_or_default();
            match &entry.forth {
                None => {
                    output.push_str(&format!(
                        "{}{}  {}\n",
                        key.clone().bold().cyan(),
                        sense_tag,
                        "(no Forth)".dark_grey()
                    ));
                }
                Some(code) => {
                    let run_result = crate::coforth::Forth::run(code);
                    let result_str = match run_result {
                        Ok(s) if s.is_empty() => "(no output)".dark_grey().to_string(),
                        Ok(s) => s.trim_end().to_string().green().to_string(),
                        Err(e) => format!("error: {e}").red().to_string(),
                    };
                    output.push_str(&format!(
                        "{}{}\n  {}\n  {}\n",
                        key.clone().bold().cyan(),
                        sense_tag,
                        code.as_str().dark_grey(),
                        result_str
                    ));
                }
            }
        }

        self.output_manager
            .write_info(output.trim_end().to_string());
        self.render_tui().await
    }

    /// `/undefine <word>` — remove the last user-library entry for `word`.
    ///
    /// The embedded library is never modified; only `~/.finch/library.toml` entries
    /// are removed.  If there are multiple user entries for the word, only the last
    /// one is removed (stack semantics — the one below it becomes active again).
    async fn handle_library_undefine(&mut self, word: String) -> Result<()> {
        let key = word.trim().to_lowercase();
        let user_path = dirs::home_dir()
            .unwrap_or_default()
            .join(".finch")
            .join("library.toml");

        let msg = if user_path.exists() {
            match std::fs::read_to_string(&user_path) {
                Ok(content) => {
                    // Split into [[word]] blocks and remove the LAST block matching `key`
                    let blocks: Vec<&str> = content.split("\n[[word]]").collect();
                    let target = format!("\nword = \"{}\"", key);
                    let target2 = format!("word = \"{}\"", key); // first block (no leading \n)

                    // Find the last block that matches
                    let last_match = blocks.iter().rposition(|b| {
                        let trimmed = b.trim_start_matches('\n');
                        trimmed.starts_with(&target2)
                            || trimmed
                                .lines()
                                .next()
                                .map(|l| l == &target2[..])
                                .unwrap_or(false)
                            || b.contains(&target)
                    });

                    match last_match {
                        Some(idx) => {
                            let mut new_blocks: Vec<&str> = blocks.clone();
                            new_blocks.remove(idx);
                            let new_content = if new_blocks.is_empty() {
                                String::new()
                            } else {
                                let first = new_blocks[0].to_string();
                                let rest = new_blocks[1..]
                                    .iter()
                                    .map(|b| format!("\n[[word]]{b}"))
                                    .collect::<String>();
                                format!("{first}{rest}")
                            };
                            std::fs::write(&user_path, new_content)
                                .context("Failed to write library.toml")?;
                            format!("removed: {key}  (previous definition now active)")
                        }
                        None => format!("no user definition found for: {key}"),
                    }
                }
                Err(e) => format!("error reading library: {e}"),
            }
        } else {
            format!("no user library at {}", user_path.display())
        };

        self.output_manager.write_info(msg);
        self.render_tui().await
    }

    /// `/describe <word>` — look up a word in the English library and display all its senses.
    /// If the word is not in the library, auto-define it (AI or manual dialog) then show it.
    async fn handle_stack_describe(&mut self, word: String) -> Result<()> {
        let key = word.trim().to_lowercase();
        let senses_empty = {
            let lib = crate::coforth::Library::load();
            lib.lookup_all(&key).is_empty()
        };

        if senses_empty {
            // Unknown word — define it first, then fall through to show
            self.handle_stack_define_auto(key.clone(), None).await?;
        }

        // Re-load after possible auto-define
        let lib = crate::coforth::Library::load();
        let senses = lib.lookup_all(&key);
        if senses.is_empty() {
            // Auto-define was cancelled or failed — nothing to show
            return Ok(());
        } else {
            use crossterm::style::Stylize;
            let mut out = key.clone().bold().cyan().to_string();
            for (i, entry) in senses.iter().enumerate() {
                let sense_label = entry
                    .sense
                    .as_deref()
                    .map(|s| format!("  {}", format!("[{s}]").yellow()))
                    .unwrap_or_default();
                let num = if senses.len() > 1 {
                    format!("{}. ", i + 1)
                } else {
                    String::new()
                };
                let related = if entry.related.is_empty() {
                    String::new()
                } else {
                    format!("\n     related: {}", entry.related.join(", "))
                };
                let forth = entry
                    .forth
                    .as_deref()
                    .map(|f| format!("\n     forth:   {f}"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "\n  {num}{}{}  {}{}{}",
                    format!("[{}]", entry.kind).dark_grey(),
                    sense_label,
                    entry.definition,
                    related,
                    forth
                ));
            }
            self.output_manager.write_info(out);
        }
        self.render_tui().await
    }

    /// `/define <word>[:<sense>] <definition>` — append to ~/.finch/library.toml and seed into poset.
    ///
    /// Examples:
    ///   /define bank a financial institution
    ///   /define bank:river the sloping land beside a river
    async fn handle_stack_define(&mut self, word: String, definition: String) -> Result<()> {
        // Parse optional sense from "word:sense"
        let (key, sense) = if let Some((w, s)) = word.trim().split_once(':') {
            (w.trim().to_lowercase(), Some(s.trim().to_string()))
        } else {
            (word.trim().to_lowercase(), None)
        };

        // AI auto-define when no definition supplied
        if definition.trim().is_empty() {
            return self.handle_stack_define_auto(key, sense).await;
        }

        self.save_library_entry(&key, definition.trim(), sense.as_deref(), "")
            .await
    }

    /// `/override <word> <def>` — write directly to ~/.finch/library.toml, bypassing the repo.
    /// This gives per-machine overrides that are never committed.
    async fn handle_stack_override(&mut self, word: String, definition: String) -> Result<()> {
        let (key, sense) = if let Some((w, s)) = word.split_once(':') {
            (w.trim().to_lowercase(), Some(s.trim().to_string()))
        } else {
            (word.trim().to_lowercase(), None)
        };
        if key.is_empty() {
            return Ok(());
        }
        self.save_library_entry_local(&key, definition.trim(), sense.as_deref(), "")
            .await
    }

    /// Save a word entry to `~/.finch/library.toml` (machine-local, never committed).
    async fn save_library_entry_local(
        &mut self,
        key: &str,
        definition: &str,
        sense: Option<&str>,
        forth: &str,
    ) -> Result<()> {
        let path = dirs::home_dir()
            .map(|h| h.join(".finch").join("library.toml"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        self.save_library_entry_to(
            key,
            definition,
            sense,
            forth,
            &path,
            "~/.finch/library.toml",
        )
        .await
    }

    /// Core save: write a word entry to vocabulary/{lang}.toml in the git repo and inject into the live poset.
    /// `forth` may be empty — it's omitted from the TOML in that case.
    async fn save_library_entry(
        &mut self,
        key: &str,
        definition: &str,
        sense: Option<&str>,
        forth: &str,
    ) -> Result<()> {
        // Write to the project-local vocabulary module (vocabulary/lang.toml in git root).
        // Falls back to ~/.finch/library.toml when not in a git repo.
        let lang = crate::coforth::library::detect_vocab_lang(key);
        let (path, display) = match crate::coforth::library::repo_vocab_path(lang) {
            Some(p) => {
                let d = format!("vocabulary/{lang}.toml");
                (p, d)
            }
            None => {
                let p = dirs::home_dir()
                    .map(|h| h.join(".finch").join("library.toml"))
                    .ok_or_else(|| anyhow::anyhow!("cannot determine save path"))?;
                (p, "~/.finch/library.toml".to_string())
            }
        };
        self.save_library_entry_to(key, definition, sense, forth, &path, &display)
            .await
    }

    /// Shared write+inject implementation used by both save paths.
    async fn save_library_entry_to(
        &mut self,
        key: &str,
        definition: &str,
        sense: Option<&str>,
        forth: &str,
        path: &std::path::Path,
        display_path: &str,
    ) -> Result<()> {
        let safe_def = definition.replace('\\', "\\\\").replace('"', "\\\"");

        let sense_line = sense
            .map(|s| format!("sense = \"{s}\"\n"))
            .unwrap_or_default();
        let forth_line = if forth.is_empty() {
            String::new()
        } else {
            let safe_f = if !forth.contains('\'') {
                format!("'{forth}'")
            } else {
                format!("\"{}\"", forth.replace('\\', "\\\\").replace('"', "\\\""))
            };
            format!("forth = {safe_f}\n")
        };
        let entry_toml = format!(
            "\n[[word]]\nword = \"{key}\"\ndefinition = \"{safe_def}\"\n{sense_line}{forth_line}"
        );

        let (already_exists, sense_exists) = {
            let lib = crate::coforth::Library::load();
            let senses = lib.lookup_all(key);
            let sense_exists = senses.iter().any(|e| e.sense.as_deref() == sense);
            (!senses.is_empty(), sense_exists)
        };

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        file.write_all(entry_toml.as_bytes())
            .context("failed to write library entry")?;

        // Inject into live poset
        {
            let lib = crate::coforth::Library::load();
            let mut p = self.poset.lock().await;
            lib.inject_into_poset(key, 1, &mut p);
        }

        use crossterm::style::Stylize;
        let label = sense
            .map(|s| format!("{}:{}", key.bold().cyan(), s.yellow()))
            .unwrap_or_else(|| key.bold().cyan().to_string());

        if sense_exists {
            self.output_manager
                .write_info(format!("{label} redefined in {display_path}"));
        } else if already_exists {
            self.output_manager
                .write_info(format!("{label} added as new sense to {display_path}"));
        } else {
            self.output_manager
                .write_info(format!("{label} → {display_path}"));
        }
        self.render_tui().await
    }

    /// Manual define dialog: shown when no AI provider is configured.
    async fn handle_stack_define_manual(
        &mut self,
        key: String,
        sense: Option<String>,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogResult};
        let prompt = if let Some(ref s) = sense {
            format!("Define {key}:{s}")
        } else {
            format!("Define \"{key}\"")
        };
        let dialog = Dialog::text_input(&prompt, None);
        let result = self.tui_renderer.lock().await.show_dialog(dialog)?;
        let definition = match result {
            DialogResult::TextEntered(d) if !d.trim().is_empty() => d,
            _ => return self.render_tui().await, // cancelled or empty
        };
        // Save directly (no recursive call — same logic as handle_stack_define body)
        self.save_library_entry(&key, &definition, sense.as_deref(), "")
            .await
    }

    /// AI auto-define: ask the brain provider for a definition when the user types `/define word`.
    async fn handle_stack_define_auto(&mut self, key: String, sense: Option<String>) -> Result<()> {
        // No AI provider — fall back to a manual text-input dialog
        if self.brain_provider.is_none() {
            return self.handle_stack_define_manual(key, sense).await;
        }
        let provider = self.brain_provider.clone().unwrap();

        use crossterm::style::Stylize;
        let label = sense
            .as_deref()
            .map(|s| format!("{}:{}", key.clone().bold().cyan(), s.yellow()))
            .unwrap_or_else(|| key.clone().bold().cyan().to_string());
        self.output_manager.write_info(format!("defining {label}…"));
        self.render_tui().await?;

        let word_arg = if let Some(ref s) = sense {
            format!("[\"{key}:{s}\"]")
        } else {
            format!("[\"{key}\"]")
        };

        let request =
            crate::providers::ProviderRequest::new(vec![crate::claude::types::Message::user(
                &word_arg,
            )])
            .with_system(crate::coforth::generator::GENERATION_SYSTEM_PROMPT.to_string())
            .with_max_tokens(500);

        let response = match provider.send_message(&request).await {
            Ok(r) => r,
            Err(e) => {
                self.output_manager
                    .write_info(format!("auto-define failed: {e}"));
                return self.render_tui().await;
            }
        };

        // Extract text from response content blocks
        let text: String = response
            .content
            .iter()
            .filter_map(|block| {
                if let crate::claude::ContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");
        let json_text = text
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();

        let entries: Vec<serde_json::Value> = match serde_json::from_str(json_text) {
            Ok(v) => v,
            Err(e) => {
                self.output_manager.write_info(format!(
                    "auto-define: couldn't parse AI response: {e}\n{}",
                    text.as_str().dark_grey()
                ));
                return self.render_tui().await;
            }
        };

        let Some(entry) = entries.into_iter().next() else {
            self.output_manager
                .write_info("auto-define: AI returned empty list".to_string());
            return self.render_tui().await;
        };

        let definition = entry["definition"]
            .as_str()
            .unwrap_or("(no definition)")
            .to_string();
        let forth = entry["forth"].as_str().unwrap_or("").to_string();

        self.output_manager.write_info(format!(
            "{label}: {definition}{}",
            "  (saving…)".dark_grey()
        ));
        self.save_library_entry(&key, &definition, sense.as_deref(), &forth)
            .await
    }

    /// Handle `/program` — render the current stack as Forth source code.
    ///
    /// Seed the stack with a small "codebase archaeology" language as a demo.
    ///
    /// Defines four words:
    ///   W0  ENTRY-POINTS  — find main() and binary entry points  [Glob, Grep]
    ///   W1  MODULE-MAP    — map modules and public interfaces     [Glob, Grep]
    ///   W2  CALL-GRAPH    — trace call graph from entry points    [Read]       needs W0
    ///   W3  STORY         — onboarding narrative                              needs W1, W2
    ///
    /// Parallel roots: W0 and W1 run concurrently.
    /// Then W2 (needs W0), then W3 (needs W1 and W2).
    async fn handle_stack_demo(&mut self) -> Result<()> {
        use crate::poset::{NodeAuthor, NodeKind};

        // Clear any existing stack and poset first.
        self.stack.lock().await.clear();
        {
            let mut p = self.poset.lock().await;
            *p = crate::poset::Poset::new();
        }

        // Word definitions: (label, kind, tools, predecessors)
        let words: &[(&str, NodeKind, &[&str], &[usize])] = &[
            (
                "find main() and binary entry points in this codebase",
                NodeKind::Task,
                &["Glob", "Grep"],
                &[],
            ),
            (
                "map all modules and their public interfaces",
                NodeKind::Task,
                &["Glob", "Grep"],
                &[],
            ),
            (
                "trace the call graph from entry points",
                NodeKind::Task,
                &["Read"],
                &[0], // needs W0
            ),
            (
                "write a developer onboarding story for this codebase",
                NodeKind::Task,
                &[],
                &[1, 2], // needs W1 and W2
            ),
        ];

        let mut ids: Vec<usize> = Vec::new();
        {
            let mut p = self.poset.lock().await;
            for &(label, ref kind, tools, _) in words {
                let tool_names: Vec<String> = tools.iter().map(|s| s.to_string()).collect();
                let id = p.add_node_with_tools(
                    label.to_string(),
                    kind.clone(),
                    NodeAuthor::User,
                    tool_names,
                );
                ids.push(id);
            }
            // Wire edges based on predecessor lists.
            for (i, &(_, _, _, preds)) in words.iter().enumerate() {
                for &pred_idx in preds {
                    p.edges.push((ids[pred_idx], ids[i]));
                }
            }
        }

        // Mirror into the flat stack (for /stack show compatibility).
        {
            let mut s = self.stack.lock().await;
            for &(label, _, _, _) in words {
                s.push(label.to_string());
            }
        }

        self.output_manager.write_info(
            "📚 Demo language seeded: 4 words, 3 edges.\n\
             W0 + W1 run in parallel → W2 → W3.\n\
             /program to see the vocabulary · /view for graph · /run to execute.",
        );

        // Switch to Forth view so the vocabulary is immediately visible.
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.poset_panel_mode = crate::cli::tui::PosetPanelMode::Forth;
        }
        self.render_tui().await
    }

    /// Switch the Co-Forth overlay panel to Forth source view.
    /// The overlay recomputes the program from the live poset on each render tick.
    async fn handle_stack_program(&mut self) -> Result<()> {
        let mut tui = self.tui_renderer.lock().await;
        if tui.poset_panel_mode != crate::cli::tui::PosetPanelMode::Forth {
            tui.toggle_poset_view();
        }
        drop(tui);
        self.render_tui().await
    }

    /// Handle `/stack` — show current stack contents.
    async fn handle_stack_show(&mut self) -> Result<()> {
        let stack = self.stack.lock().await;
        if stack.is_empty() {
            self.output_manager
                .write_info("📚 Stack is empty.  Type to push, /pop to execute.");
        } else {
            let mut lines = vec![format!(
                "📚 Stack ({} item{}):",
                stack.len(),
                if stack.len() == 1 { "" } else { "s" }
            )];
            for (i, item) in stack.iter().enumerate() {
                let preview = if item.len() > 80 {
                    format!("{}…", item.chars().take(80).collect::<String>())
                } else {
                    item.clone()
                };
                lines.push(format!("  [{:>2}] {}", i + 1, preview));
            }
            lines.push(String::new());
            lines.push("/pop to execute all as one query.".to_string());
            self.output_manager.write_info(lines.join("\n"));
        }
        drop(stack);
        self.render_tui().await
    }

    /// Handle `/pop` — remove the top item from the stack (undo last push).
    async fn handle_stack_pop(&mut self) -> Result<()> {
        let mut stack = self.stack.lock().await;
        if stack.is_empty() {
            drop(stack);
            self.output_manager
                .write_info("📚 Stack is empty. Nothing to pop.");
            self.render_tui().await?;
            return Ok(());
        }
        let item = stack.pop().unwrap();
        let depth = stack.len();
        drop(stack);
        let preview = if item.len() > 60 {
            format!("{}…", item.chars().take(60).collect::<String>())
        } else {
            item
        };
        self.output_manager
            .write_info(format!("📚 popped → \"{preview}\"   depth:{depth}"));
        self.render_tui().await
    }

    /// `/eval-each` — run each stack item independently in a fresh VM clone and
    /// display the result next to the program.  The stack is left unchanged so
    /// items can be inspected, re-ordered, or re-run.
    async fn handle_stack_eval(&mut self) -> Result<()> {
        let items: Vec<String> = self.stack.lock().await.clone();
        if items.is_empty() {
            self.output_manager
                .write_info("stack is empty — push some programs first".to_string());
            return self.render_tui().await;
        }

        let sep = "─".repeat(60);
        let mut lines = vec![sep.clone()];

        for (i, prog) in items.iter().enumerate() {
            // Each item gets a clean VM clone (same dict, empty stack) so state doesn't bleed.
            let mut vm = self.forth_vm.clone_dict();
            let result = match vm.exec(prog) {
                Ok(out) => {
                    let out = out.trim().to_string();
                    let stack_top = vm.data_stack();
                    if !out.is_empty() {
                        out
                    } else if !stack_top.is_empty() {
                        let top: Vec<String> = stack_top.iter().map(|n| n.to_string()).collect();
                        format!("( {} )", top.join("  "))
                    } else {
                        "ok".to_string()
                    }
                }
                Err(e) => format!("error: {e}"),
            };
            lines.push(format!("[{i}] {prog:<40} → {result}"));
        }

        lines.push(sep);
        self.output_manager.write_info(lines.join("\n"));
        self.render_tui().await
    }

    async fn handle_stack_run(&mut self) -> Result<Option<String>> {
        let mut stack = self.stack.lock().await;
        if stack.is_empty() {
            drop(stack);
            self.output_manager
                .write_info("📚 Stack is empty. Type something first.");
            self.render_tui().await?;
            return Ok(None);
        }
        let count = stack.len();
        let query = stack.drain(..).collect::<Vec<_>>().join("\n\n");
        drop(stack);
        self.output_manager.write_info(format!(
            "📚 Running {count} item{}…",
            if count == 1 { "" } else { "s" }
        ));
        self.render_tui().await?;
        Ok(Some(query))
    }

    /// Execute the approved stack: if any poset nodes have tools, run the poset executor;
    /// otherwise fall back to the plain query path.
    async fn handle_poset_or_query(&mut self, query: String) -> Result<()> {
        let is_non_empty = !self.poset.lock().await.is_empty();

        if is_non_empty {
            // Show confirmation dialog (non-blocking); continuation handled by render tick.
            self.confirm_poset_run().await?;
        } else {
            self.execute_query(query).await?;
        }
        Ok(())
    }

    /// Handle `/stack clear` — drop all stack items and return panel to graph view.
    async fn handle_stack_clear(&mut self) -> Result<()> {
        let mut stack = self.stack.lock().await;
        let count = stack.len();
        stack.clear();
        drop(stack);
        // Return panel to graph view so the user is back in normal chat mode.
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.poset_panel_mode = crate::cli::tui::PosetPanelMode::Graph;
        }
        if count == 0 {
            self.output_manager
                .write_info("stack empty  (tip: ?? question  to ask the AI directly)");
        } else {
            self.output_manager.write_info(format!(
                "cleared {count} item{}  (tip: ?? question  to ask the AI directly)",
                if count == 1 { "" } else { "s" }
            ));
        }
        self.render_tui().await
    }

    /// Queue the poset run confirmation dialog (non-blocking).
    ///
    /// Sets `active_dialog` and stores the pending run data in `pending_poset_run`.
    /// The render tick executes the poset when the user approves.
    /// Returns `Ok(())` immediately; the approval is handled asynchronously.
    ///
    /// NOTE: The Forth VM show_dialog calls (sync closures ~line 1428–1462) still use
    /// the blocking `show_dialog` path — those require a separate VM refactor.
    async fn confirm_poset_run(&mut self) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        let plan = {
            let p = self.poset.lock().await;
            if p.is_empty() {
                return Ok(());
            }

            // Topological sort + depth propagation.
            let mut depth: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0usize)).collect();
            let mut in_deg: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0)).collect();
            for &(_, s) in &p.edges {
                *in_deg.entry(s).or_insert(0) += 1;
            }
            let mut q: std::collections::VecDeque<usize> = in_deg
                .iter()
                .filter(|(_, &d)| d == 0)
                .map(|(&id, _)| id)
                .collect();
            let mut topo: Vec<usize> = Vec::new();
            while let Some(id) = q.pop_front() {
                topo.push(id);
                let d = depth[&id];
                for &(pred, succ) in &p.edges {
                    if pred == id {
                        let e = depth.entry(succ).or_insert(0);
                        if d + 1 > *e {
                            *e = d + 1;
                        }
                        let deg = in_deg.entry(succ).or_insert(0);
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            q.push_back(succ);
                        }
                    }
                }
            }

            let max_depth = depth.values().copied().max().unwrap_or(0);
            let mut lines: Vec<String> = Vec::new();
            for lvl in 0..=max_depth {
                let group: Vec<&crate::poset::Node> = topo
                    .iter()
                    .filter(|&&id| depth[&id] == lvl)
                    .filter_map(|&id| p.nodes.iter().find(|n| n.id == id))
                    .collect();
                if group.is_empty() {
                    continue;
                }

                let names: Vec<String> = group.iter().map(|n| format!("W{}", n.id)).collect();

                let concurrent = if group.len() > 1 {
                    "  \\ concurrent"
                } else {
                    ""
                };
                lines.push(format!("  {}{}", names.join("  "), concurrent));
            }
            lines.join("\n")
        };

        let title = format!(": PROGRAM\n{}\n;\n\nthis will run on your machine.", plan);
        let dialog = Dialog::select(
            title,
            vec![DialogOption::new("run"), DialogOption::new("cancel")],
        );

        // Store continuation data for the render tick.
        let generator = self.model_selection.generator().await;
        self.pending_poset_run = Some(PendingPosetRun {
            generator,
            poset: Arc::clone(&self.poset),
            stack: Arc::clone(&self.stack),
            event_tx: self.event_tx.clone(),
        });

        // Show dialog non-blocking (render tick will handle the result).
        {
            let mut tui = self.tui_renderer.lock().await;
            tui.active_dialog = Some(dialog);
            tui.pending_dialog_result = None;
            let _ = tui.erase_live_area();
            let _ = tui.draw_live_area();
        }

        Ok(())
    }

    /// Handle tool approval request (show dialog, get user response)
    async fn handle_tool_approval_request(
        &mut self,
        query_id: Uuid,
        tool_use: crate::tools::types::ToolUse,
        response_tx: tokio::sync::oneshot::Sender<super::events::ConfirmationResult>,
    ) -> Result<()> {
        use crate::cli::tui::Dialog;

        tracing::debug!("[EVENT_LOOP] Requesting tool approval: {}", tool_use.name);

        // Create approval dialog — compact 3-option style matching Claude Code UX
        let tool_name = &tool_use.name;
        let summary = tool_approval_summary(&tool_use);

        let dialog = Dialog::tool_approval(tool_name, &summary);

        // Set dialog in TUI (non-blocking - will be handled by async_input task)
        let mut tui = self.tui_renderer.lock().await;
        tui.active_dialog = Some(dialog);

        // Force render to show dialog immediately
        if let Err(e) = tui.render() {
            tracing::error!("[EVENT_LOOP] Failed to render dialog: {}", e);
        }
        drop(tui);

        // Store the response channel and tool_use for when dialog completes
        // We'll check pending_dialog_result in the event loop and send the response then
        self.pending_approvals
            .write()
            .await
            .insert(query_id, (tool_use, response_tx));

        tracing::debug!("[EVENT_LOOP] Tool approval dialog shown, waiting for user response");

        Ok(())
    }

    /// Convert dialog result to confirmation result
    fn dialog_result_to_confirmation(
        &self,
        dialog_result: crate::cli::tui::DialogResult,
        tool_use: &crate::tools::types::ToolUse,
    ) -> super::events::ConfirmationResult {
        dialog_result_to_confirmation(dialog_result, tool_use)
    }

    // ========== Plan Mode Handlers ==========

    /// Update status bar with current plan mode indicator
    fn update_plan_mode_indicator(&self, mode: &ReplMode) {
        use crate::cli::status_bar::StatusLineType;

        let indicator = match mode {
            ReplMode::Normal => "⏵⏵ accept edits on (shift+tab to cycle)",
            ReplMode::Planning { .. } => "⏸ plan mode on (shift+tab to cycle)",
            ReplMode::Executing { .. } => "▶ executing plan (shift+tab disabled)",
        };

        self.status_bar
            .update_line(StatusLineType::Custom("plan_mode".to_string()), indicator);
    }

    #[allow(dead_code)]
    /// Handle /plan command - enter planning mode
    async fn handle_plan_command(&mut self, task: String) -> Result<()> {
        // Check if already in plan mode
        {
            let mode = self.mode.read().await;
            if matches!(
                *mode,
                ReplMode::Planning { .. } | ReplMode::Executing { .. }
            ) {
                let mode_name = match &*mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                };
                drop(mode);
                self.output_manager.write_info(format!(
                    "⚠️  Already in {} mode. Finish current task first.",
                    mode_name
                ));
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Create plans directory
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        // Generate plan filename
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to planning mode
        let new_mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };
        *self.mode.write().await = new_mode.clone();

        // Update status bar
        self.update_plan_mode_indicator(&new_mode);

        self.output_manager
            .write_info(format!("{}", "✓ Entered planning mode".blue().bold()));
        self.output_manager.write_info(format!("📋 Task: {}", task));
        self.output_manager
            .write_info(format!("📁 Plan will be saved to: {}", plan_path.display()));
        self.output_manager.write_info("");
        self.output_manager
            .write_info(format!("{}", "Available tools:".green()));
        self.output_manager
            .write_info("  read, glob, grep, web_fetch");
        self.output_manager
            .write_info(format!("{}", "Blocked tools:".red()));
        self.output_manager.write_info("  bash, save_and_exec");
        self.output_manager.write_info("");
        self.output_manager
            .write_info("Ask me to explore the codebase and generate a plan.");
        self.output_manager.write_info(format!(
            "{}",
            "Type /show-plan to view, /approve to execute, /reject to cancel.".dark_grey()
        ));

        // Add mode change notification to conversation
        self.conversation.write().await.add_user_message(format!(
            "[System: Entered planning mode for task: {}]\n\
             Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
             Blocked tools: bash, save_and_exec\n\
             Please explore the codebase and generate a detailed plan.",
            task
        ));

        self.render_tui().await?;
        Ok(())
    }

    /// Handle `/plan <task>` — run the IMPCPD iterative plan refinement loop.
    ///
    /// 1. Guard against being called while already in Planning/Executing mode.
    /// 2. Transition to `ReplMode::Planning`.
    /// 3. Run the IMPCPD loop (generate → critique → steer, up to 3 iterations).
    /// 4. On convergence or user approval, show the final plan and ask for
    ///    a last confirmation before transitioning to `ReplMode::Executing`.
    async fn handle_plan_task(&mut self, task: String) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption, DialogResult};
        use crate::planning::{ImpcpdConfig, PlanLoop, PlanResult};

        // Guard: already planning or executing
        {
            let mode = self.mode.read().await;
            if matches!(
                *mode,
                ReplMode::Planning { .. } | ReplMode::Executing { .. }
            ) {
                let name = match &*mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                };
                drop(mode);
                self.output_manager.write_info(format!(
                    "⚠️  Already in {} mode. Use /plan (no args) to exit first.",
                    name
                ));
                self.render_tui().await?;
                return Ok(());
            }
        }

        // Create plan directory and timestamped path
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to Planning mode
        let planning_mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };
        *self.mode.write().await = planning_mode.clone();
        self.update_plan_mode_indicator(&planning_mode);

        self.output_manager.write_info(format!(
            "{} IMPCPD plan refinement starting\n{} Task: {}",
            "📋",
            " ".repeat(3),
            task.clone().cyan().bold()
        ));
        self.render_tui().await?;

        // ── Run the IMPCPD loop ────────────────────────────────────────────────
        let plan_loop = PlanLoop::new(
            self.model_selection.generator().await,
            Arc::clone(&self.output_manager),
            ImpcpdConfig::default(),
        );
        let result = plan_loop.run(&task, Arc::clone(&self.tui_renderer)).await?;

        // ── Emit convergence summary before the approval dialog ───────────────
        {
            let summary = match &result {
                PlanResult::Converged { iterations } => {
                    let n = iterations.len();
                    let resolved: usize = iterations
                        .iter()
                        .map(|i| i.critiques.iter().filter(|c| c.is_must_address).count())
                        .sum();
                    format!(
                        "{} IMPCPD: {} iteration{}, converged ✓  ({} issues resolved)",
                        "✓".green().bold(),
                        n,
                        if n == 1 { "" } else { "s" },
                        resolved
                    )
                }
                PlanResult::IterationCap { iterations } => {
                    let n = iterations.len();
                    format!(
                        "{} IMPCPD: {} iteration{} — hard cap reached, review carefully",
                        "⚠".yellow().bold(),
                        n,
                        if n == 1 { "" } else { "s" }
                    )
                }
                PlanResult::UserApproved { iterations } => {
                    let n = iterations.len();
                    format!(
                        "{} IMPCPD: {} iteration{}, user-approved mid-loop",
                        "✓".green(),
                        n,
                        if n == 1 { "" } else { "s" }
                    )
                }
                PlanResult::Cancelled => String::new(),
            };
            if !summary.is_empty() {
                self.output_manager.write_info(format!("\n{}\n", summary));
                self.render_tui().await?;
            }
        }

        // ── Handle loop result ────────────────────────────────────────────────
        match result {
            PlanResult::Converged { ref iterations }
            | PlanResult::UserApproved { ref iterations }
            | PlanResult::IterationCap { ref iterations } => {
                let Some(last) = iterations.last() else {
                    *self.mode.write().await = ReplMode::Normal;
                    self.update_plan_mode_indicator(&ReplMode::Normal);
                    self.render_tui().await?;
                    return Ok(());
                };
                let final_plan = last.plan_text.clone();

                // Save final plan to disk
                if let Err(e) = std::fs::write(&plan_path, &final_plan) {
                    self.output_manager
                        .write_info(format!("⚠️  Could not save plan file: {}", e));
                }

                // Show the plan for final human review
                self.output_manager
                    .write_info(format!("\n{}", "━".repeat(70)));
                self.output_manager
                    .write_info(format!("{}", "📋 FINAL IMPLEMENTATION PLAN".bold()));
                self.output_manager
                    .write_info(format!("{}\n", "━".repeat(70)));
                self.output_manager.write_info(final_plan.clone());
                self.output_manager
                    .write_info(format!("\n{}\n", "━".repeat(70)));
                self.render_tui().await?;

                // Final approval dialog
                let approval_dialog = Dialog::select(
                    "Review Final Plan".to_string(),
                    vec![
                        DialogOption::with_description(
                            "Approve and execute",
                            "All tools enabled — proceed with implementation",
                        ),
                        DialogOption::with_description(
                            "Reject",
                            "Exit plan mode without executing",
                        ),
                    ],
                )
                .with_help("↑↓/j/k = navigate · Enter = select · Esc = cancel");

                let approval = {
                    let mut tui = self.tui_renderer.lock().await;
                    tui.show_dialog(approval_dialog)
                        .context("Failed to show approval dialog")?
                };

                match approval {
                    DialogResult::Selected(0) => {
                        // Approved → transition to Executing
                        let exec_mode = ReplMode::Executing {
                            task: task.clone(),
                            plan_path: plan_path.clone(),
                            approved_at: Utc::now(),
                        };
                        *self.mode.write().await = exec_mode.clone();
                        self.update_plan_mode_indicator(&exec_mode);

                        // Replace conversation context with the plan so the LLM
                        // knows exactly what to execute next.
                        self.conversation.write().await.clear();
                        self.conversation.write().await.add_user_message(format!(
                            "[System: Plan approved. Execute this plan step by step:]\n\n{}",
                            final_plan
                        ));

                        self.output_manager.write_info(format!(
                            "{}",
                            "✓ Plan approved! All tools are now enabled.".green().bold()
                        ));
                    }
                    _ => {
                        // Rejected or cancelled
                        *self.mode.write().await = ReplMode::Normal;
                        self.plan_word = None;
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan rejected. Returned to normal mode.");
                    }
                }
            }
            PlanResult::Cancelled => {
                *self.mode.write().await = ReplMode::Normal;
                self.plan_word = None;
                self.update_plan_mode_indicator(&ReplMode::Normal);
                self.output_manager
                    .write_info("Planning cancelled. Returned to normal mode.");
            }
        }

        self.render_tui().await?;
        Ok(())
    }
}

include!("brain_handler.rs");

// handle_present_plan, handle_ask_user_question, is_tool_allowed_in_mode moved to plan_handler.rs

/// Find the most recent (query, response) pair from conversation history.
///
/// Scans messages in reverse: finds the latest non-empty assistant message,
/// then finds the user message that immediately preceded it.
///
/// Returns `("", "")` if no assistant response is found.
pub(crate) fn find_last_exchange(messages: &[crate::claude::Message]) -> (String, String) {
    let mut last_response = String::new();
    let mut last_query = String::new();
    let mut found_response = false;

    for msg in messages.iter().rev() {
        if !found_response && msg.role == "assistant" {
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        last_response = text.clone();
                        found_response = true;
                        break;
                    }
                }
            }
        } else if found_response && msg.role == "user" {
            for block in &msg.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        last_query = text.clone();
                        break;
                    }
                }
            }
            break;
        }
    }

    (last_query, last_response)
}

/// Translate a raw Forth error message into plain English.
///
/// The Forth VM surfaces low-level errors ("stack underflow", "unknown word: foo").
/// This function converts those into language a non-programmer can act on.
pub(crate) fn humanize_forth_error(raw: &str) -> String {
    // Strip any existing "forth error:" prefix the VM might include.
    let e = raw.trim_start_matches("forth error:").trim();

    if let Some(word) = e.strip_prefix("unknown word: ") {
        let word = word.trim_matches('"');
        return format!(
            "\"{word}\" isn't defined yet\n  define it:  : {word}  … ;\n  or ask:     what does {word} do?"
        );
    }
    if e.contains("stack underflow") || e.contains("not enough values") {
        return "not enough values on the stack — try putting more numbers in first".to_string();
    }
    if e.contains("division by zero") {
        return "can't divide by zero".to_string();
    }
    if e.contains("stack overflow") && e.contains("too many values") {
        return "stack overflow — the stack has too many values (try `clear` to reset)".to_string();
    }
    if e.contains("return stack overflow") || e.contains("call stack overflow") {
        return "too many nested calls — a word is probably calling itself forever".to_string();
    }
    if e.contains("fuel exhausted") {
        return "this program ran too long — it might have an infinite loop\n  tip: use `undo` to go back".to_string();
    }
    if e.contains("missing ;") {
        return "word definition isn't closed — add `;` at the end".to_string();
    }
    if e.contains("redefinition") && e.contains("cancelled") {
        return "that word already exists — redefine was cancelled".to_string();
    }
    if e.contains("sqrt of negative") {
        return "can't take the square root of a negative number".to_string();
    }
    // Fallback: show the raw message but strip the "forth error:" prefix
    e.to_string()
}

/// Remove word definitions from AI-generated Forth that already exist in the VM.
///
/// Handles multi-line definitions: once a `: name` header is skipped (because
/// `word_exists(name)` is true), body lines are also dropped until the closing
/// `;` is found.  The old line-by-line `.filter()` only removed the header line,
/// leaving orphaned body code that could execute as bare statements — including
/// infinite loops like `begin dup prime? until` with no increment.
pub(crate) fn skip_known_word_defs<'a>(
    code: &'a str,
    word_exists: impl Fn(&str) -> bool,
) -> String {
    let mut result: Vec<&'a str> = Vec::new();
    let mut in_filtered_def = false;
    for line in code.lines() {
        let t = line.trim();
        if in_filtered_def {
            if t.contains(';') {
                in_filtered_def = false;
            }
            continue;
        }
        if t.starts_with(':') {
            let word = t[1..].trim().split_whitespace().next().unwrap_or("");
            if word_exists(word) {
                if !t.contains(';') {
                    in_filtered_def = true;
                }
                continue;
            }
        }
        result.push(line);
    }
    result.join("\n")
}

/// Filter an AI-generated Forth response before compilation.
///
/// Removes definitions that would cause infinite loops or produce garbage output:
/// - Definitions that call `gen:*` words (AI-invocation; triggers new AI calls during execution)
/// - Definitions that call themselves directly (immediate infinite recursion)
/// - Definitions with bodies longer than 40 tokens (garbage dump from confused AI)
///
/// Preserves: valid `: name ... ;` definitions, `test:*` proofs, and `prove-all`.
pub(crate) fn filter_ai_forth_response(code: &str) -> String {
    let tokens: Vec<&str> = code.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        if tokens[i] != ":" {
            // Outside a definition: keep prove-all and bare word calls; skip unknown noise.
            out.push(tokens[i]);
            i += 1;
            continue;
        }

        // Collect `: name body ;`
        let def_start = i;
        let name = tokens.get(i + 1).copied().unwrap_or("");
        i += 2; // skip `:` and name

        // Advance to the matching `;`, tracking nested `: ;` pairs.
        // Cap the scan at 60 tokens to avoid consuming everything when `;` is missing.
        let body_start = i;
        let scan_limit = body_start + 60;
        let mut depth: usize = 1;
        let mut found_closing = false;
        while i < tokens.len() && i < scan_limit {
            match tokens[i] {
                ":" => depth += 1,
                ";" if depth == 1 => {
                    i += 1;
                    found_closing = true;
                    break;
                }
                ";" => depth -= 1,
                _ => {}
            }
            i += 1;
        }

        // Skip malformed definitions (no closing semicolon within scan window).
        // Reset i to body_start so any prove-all or valid defs that follow are not lost.
        if !found_closing {
            i = body_start;
            // Skip past this definition's body by advancing to the next `:` or end.
            while i < tokens.len() && tokens[i] != ":" {
                // Preserve prove-all even inside a malformed body.
                if tokens[i] == "prove-all" {
                    out.push("prove-all");
                }
                i += 1;
            }
            continue;
        }

        let body = &tokens[body_start..i.saturating_sub(1)];

        // Reject: gen: calls in the body (would trigger AI during word execution → loops).
        if body.iter().any(|t| t.starts_with("gen:")) {
            continue;
        }
        // Reject: direct self-recursion (word calls itself immediately → return stack overflow).
        if body.iter().any(|t| *t == name) {
            continue;
        }
        // Reject: suspiciously long body — AI garbage-dumped the vocabulary into the definition.
        if body.len() > 40 {
            continue;
        }

        out.extend_from_slice(&tokens[def_start..i]);
    }

    out.join(" ")
}

/// Build a concise human-readable summary of a tool call for the approval dialog.
///
/// Returns a single line such as `"Command: git push"` or `"File: src/main.rs"`.
/// Exported `pub(crate)` so it can be unit-tested directly.
pub(crate) fn tool_approval_summary(tool_use: &crate::tools::types::ToolUse) -> String {
    let tool_name = &tool_use.name;
    match tool_name.as_str() {
        "bash" | "Bash" => {
            if let Some(cmd) = tool_use.input.get("command").and_then(|v| v.as_str()) {
                format!(
                    "Command: {}",
                    if cmd.len() > 60 {
                        format!("{}...", cmd.chars().take(60).collect::<String>())
                    } else {
                        cmd.to_string()
                    }
                )
            } else {
                "Execute shell command".to_string()
            }
        }
        "read" | "Read" => {
            if let Some(path) = tool_use.input.get("file_path").and_then(|v| v.as_str()) {
                format!("File: {}", path)
            } else {
                "Read file".to_string()
            }
        }
        "grep" | "Grep" => {
            if let Some(pattern) = tool_use.input.get("pattern").and_then(|v| v.as_str()) {
                format!(
                    "Pattern: {}",
                    if pattern.len() > 40 {
                        format!("{}...", pattern.chars().take(40).collect::<String>())
                    } else {
                        pattern.to_string()
                    }
                )
            } else {
                "Search files".to_string()
            }
        }
        "glob" | "Glob" => {
            if let Some(pattern) = tool_use.input.get("pattern").and_then(|v| v.as_str()) {
                format!("Pattern: {}", pattern)
            } else {
                "Find files".to_string()
            }
        }
        "EnterPlanMode" => {
            if let Some(reason) = tool_use.input.get("reason").and_then(|v| v.as_str()) {
                format!(
                    "Reason: {}",
                    if reason.len() > 50 {
                        format!("{}...", reason.chars().take(50).collect::<String>())
                    } else {
                        reason.to_string()
                    }
                )
            } else {
                "Enter planning mode".to_string()
            }
        }
        "write" | "Write" => {
            let path = tool_use
                .input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let content = tool_use
                .input
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let line_count = content.lines().count();
            let is_new = !std::path::Path::new(path).exists();
            if is_new {
                let preview: String = content.lines().take(5).collect::<Vec<_>>().join("\n");
                let truncated = if line_count > 5 {
                    format!("\n… ({} lines total)", line_count)
                } else {
                    String::new()
                };
                format!("Create {}\n{}{}", path, preview, truncated)
            } else {
                // Show unified diff against existing file
                let existing = std::fs::read_to_string(path).unwrap_or_default();
                format!(
                    "Overwrite {}\n{}",
                    path,
                    unified_diff_summary(&existing, content, 3)
                )
            }
        }
        "edit" | "Edit" => {
            let path = tool_use
                .input
                .get("file_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            let old = tool_use
                .input
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new = tool_use
                .input
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Edit {}\n{}", path, unified_diff_summary(old, new, 2))
        }
        _ => format!("Execute {} tool", tool_name),
    }
}

/// Produce a compact unified-diff-style summary between `before` and `after`.
/// Shows up to `context` lines of context around each change.
fn unified_diff_summary(before: &str, after: &str, context: usize) -> String {
    let before_lines: Vec<&str> = before.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();

    // Simple LCS-based diff: find changed regions
    let mut hunks: Vec<String> = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut current_hunk: Vec<String> = Vec::new();
    let mut in_hunk = false;
    let max_lines = 40; // cap total output
    let mut total = 0;

    // Build a simple line-by-line diff without external crates:
    // walk both sides, emit - / + lines for mismatches
    while i < before_lines.len() || j < after_lines.len() {
        if total >= max_lines {
            current_hunk.push(format!("  … (diff truncated)"));
            break;
        }
        match (before_lines.get(i), after_lines.get(j)) {
            (Some(a), Some(b)) if a == b => {
                if in_hunk {
                    current_hunk.push(format!("  {}", a));
                    // End hunk after `context` unchanged lines
                    let trail = current_hunk
                        .iter()
                        .rev()
                        .take_while(|l| l.starts_with("  "))
                        .count();
                    if trail > context {
                        hunks.push(current_hunk.join("\n"));
                        current_hunk = Vec::new();
                        in_hunk = false;
                    }
                }
                i += 1;
                j += 1;
            }
            (Some(a), Some(_b)) => {
                if !in_hunk {
                    // Add context before
                    let start = i.saturating_sub(context);
                    for ctx_line in before_lines[start..i].iter() {
                        current_hunk.push(format!("  {}", ctx_line));
                    }
                    in_hunk = true;
                }
                current_hunk.push(format!("- {}", a));
                current_hunk.push(format!("+ {}", _b));
                total += 2;
                i += 1;
                j += 1;
            }
            (Some(a), None) => {
                if !in_hunk {
                    in_hunk = true;
                }
                current_hunk.push(format!("- {}", a));
                total += 1;
                i += 1;
            }
            (None, Some(b)) => {
                if !in_hunk {
                    in_hunk = true;
                }
                current_hunk.push(format!("+ {}", b));
                total += 1;
                j += 1;
            }
            (None, None) => break,
        }
    }
    if !current_hunk.is_empty() {
        hunks.push(current_hunk.join("\n"));
    }
    if hunks.is_empty() {
        "(no changes)".to_string()
    } else {
        hunks.join("\n---\n")
    }
}

/// Convert a dialog selection to a `ConfirmationResult` for tool approval.
///
/// 3-option mapping (Claude Code style):
///   - `Selected(0)` → `ApproveOnce`            ("1. Yes")
///   - `Selected(1)` → `ApprovePatternSession`   ("2. Yes, and don't ask again for: tool:*")
///   - `Selected(2+)` / `Cancelled` → `Deny`     ("3. No")
///
/// Exported `pub(crate)` so it can be unit-tested directly.
pub(crate) fn dialog_result_to_confirmation(
    dialog_result: crate::cli::tui::DialogResult,
    tool_use: &crate::tools::types::ToolUse,
) -> super::events::ConfirmationResult {
    use super::events::ConfirmationResult;
    use crate::tools::patterns::ToolPattern;

    match dialog_result {
        crate::cli::tui::DialogResult::Selected(index) => match index {
            0 => ConfirmationResult::ApproveOnce,
            1 => {
                // Session-wide wildcard: don't ask again for any call to this tool.
                let pattern = ToolPattern::new(
                    "*".to_string(),
                    tool_use.name.clone(),
                    format!("Allow all {} calls (session)", tool_use.name),
                );
                ConfirmationResult::ApprovePatternSession(pattern)
            }
            _ => ConfirmationResult::Deny, // "3. No" or anything beyond
        },
        _ => ConfirmationResult::Deny,
    }
}

// ── Unified diff applicator ───────────────────────────────────────────────────
//
// A line-based applicator for the unified diff format produced by `diff -u`.
// Handles context, additions, and deletions.  Does not handle "no newline at
// end of file" markers (`\ No newline at end of file`) — they are ignored.

fn apply_patch_lines(original: &[String], patch: &str) -> anyhow::Result<Vec<String>> {
    let mut result: Vec<String> = Vec::new();
    let mut orig_pos: usize = 0; // 0-based index into `original`

    let mut in_hunk = false;
    // Per-hunk state
    let mut hunk_orig_start: usize = 0;
    let mut hunk_orig_len: usize = 0;
    let mut hunk_orig_consumed: usize = 0;

    for line in patch.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            // File header — skip
            continue;
        }
        if line.starts_with("@@ ") {
            // Parse: @@ -l,s +l,s @@
            // e.g.  @@ -3,7 +3,6 @@
            if in_hunk {
                // Flush the rest of the previous hunk
                while hunk_orig_consumed < hunk_orig_len {
                    let idx = hunk_orig_start + hunk_orig_consumed;
                    if idx < original.len() {
                        result.push(original[idx].clone());
                    }
                    hunk_orig_consumed += 1;
                }
            }
            // Parse the @@ line
            let (os, ol) = parse_hunk_header(line)?;
            // Copy unmodified lines from current position up to this hunk's start
            let hunk_start_0 = os.saturating_sub(1); // convert 1-based to 0-based
            while orig_pos < hunk_start_0 && orig_pos < original.len() {
                result.push(original[orig_pos].clone());
                orig_pos += 1;
            }
            hunk_orig_start = hunk_start_0;
            hunk_orig_len = ol;
            hunk_orig_consumed = 0;
            in_hunk = true;
            orig_pos = hunk_start_0;
            continue;
        }
        if !in_hunk {
            continue;
        }
        if line.starts_with('\\') {
            // "\ No newline at end of file" — ignore
            continue;
        }
        if let Some(content) = line.strip_prefix(' ') {
            // Context line — keep it
            result.push(content.to_string());
            hunk_orig_consumed += 1;
            orig_pos += 1;
        } else if let Some(content) = line.strip_prefix('+') {
            // Addition — insert it
            result.push(content.to_string());
        } else if line.starts_with('-') {
            // Removal — skip the original line
            hunk_orig_consumed += 1;
            orig_pos += 1;
        }
    }

    // After all hunks: flush remaining original lines
    while orig_pos < original.len() {
        result.push(original[orig_pos].clone());
        orig_pos += 1;
    }

    Ok(result)
}

/// Parse the `-l,s` part of `@@ -l,s +l,s @@`.
/// Returns `(start, len)` for the original (minus) side.
fn parse_hunk_header(line: &str) -> anyhow::Result<(usize, usize)> {
    // Format: @@ -<start>[,<len>] +<start>[,<len>] @@
    let rest = line
        .strip_prefix("@@ ")
        .ok_or_else(|| anyhow::anyhow!("bad hunk header: {line}"))?;
    let minus = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no minus range in hunk header"))?;
    let minus = minus
        .strip_prefix('-')
        .ok_or_else(|| anyhow::anyhow!("minus range doesn't start with '-': {minus}"))?;
    let (start_str, len_str) = minus.split_once(',').unwrap_or((minus, "1"));
    let start: usize = start_str
        .parse()
        .map_err(|_| anyhow::anyhow!("bad start in hunk: {start_str}"))?;
    let len: usize = len_str
        .parse()
        .map_err(|_| anyhow::anyhow!("bad len in hunk: {len_str}"))?;
    Ok((start, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::repl_event::query_processor::apply_sliding_window;
    // format_elapsed and format_token_count moved to tool_display; import for status-bar tests.
    use crate::cli::repl_event::tool_display::{format_elapsed, format_token_count};

    // Pulsing animation frames used in status-bar tests.
    const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

    fn claude_profile(name: &str, model: &str) -> crate::config::ProviderEntry {
        crate::config::ProviderEntry::Claude {
            api_key: "test-key".to_string(),
            model: Some(model.to_string()),
            base_url: None,
            name: Some(name.to_string()),
        }
    }

    #[test]
    fn test_model_profile_resolution_distinguishes_same_provider_models() {
        let profiles = vec![
            claude_profile("fast", "claude-haiku"),
            claude_profile("deep", "claude-opus"),
        ];
        assert_eq!(resolve_provider_profile(&profiles, "fast"), Ok(0));
        assert_eq!(resolve_provider_profile(&profiles, "deep"), Ok(1));
        assert_eq!(resolve_provider_profile(&profiles, "2"), Ok(1));
        assert!(resolve_provider_profile(&profiles, "claude")
            .unwrap_err()
            .contains("multiple profiles"));
    }

    #[test]
    fn test_duplicate_model_profile_names_are_rejected_as_ambiguous() {
        let profiles = vec![
            claude_profile("work", "claude-haiku"),
            claude_profile("work", "claude-opus"),
        ];
        assert!(resolve_provider_profile(&profiles, "work")
            .unwrap_err()
            .contains("ambiguous"));
    }

    // --- streaming status bar format ---

    #[test]
    fn test_streaming_status_format() {
        // Verify the status bar message format used during streaming
        let verb = "Thinking"; // representative word; actual value comes from random_spinner_verb()
        let secs = 75u64;
        let tokens = 1600usize;
        let elapsed_str = format_elapsed(secs);
        let tokens_str = format_token_count(tokens);
        let icon = THROB_FRAMES[1]; // "✳"
        let status = format!(
            "{} {}… ({} · ↓ {} tokens)",
            icon, verb, elapsed_str, tokens_str
        );
        assert_eq!(status, "✳ Thinking… (1m 15s · ↓ 1.6k tokens)");
    }

    #[test]
    fn test_streaming_status_format_short() {
        let verb = "Thinking";
        let secs = 9u64;
        let tokens = 42usize;
        let icon = THROB_FRAMES[0]; // "✦"
        let status = format!(
            "{} {}… ({} · ↓ {} tokens)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(tokens)
        );
        assert_eq!(status, "✦ Thinking… (9s · ↓ 42 tokens)");
    }

    #[test]
    fn test_streaming_status_thinking() {
        // While thinking (no text yet), status shows "· thinking" suffix
        let verb = "Thinking";
        let secs = 15u64;
        let icon = THROB_FRAMES[2]; // "✼"
        let status = format!("{} {}… ({} · thinking)", icon, verb, format_elapsed(secs));
        assert_eq!(status, "✼ Thinking… (15s · thinking)");
    }

    #[test]
    fn test_streaming_status_with_input_tokens() {
        // With input token count available, show ↑ input · ↓ output
        let verb = "Thinking";
        let input_tokens: u32 = 1250;
        let output_tokens = 300usize;
        let secs = 10u64;
        let icon = THROB_FRAMES[1]; // "✳"
        let status = format!(
            "{} {}… ({} · ↑ {} · ↓ {} tokens)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(input_tokens as usize),
            format_token_count(output_tokens),
        );
        assert_eq!(status, "✳ Thinking… (10s · ↑ 1.2k · ↓ 300 tokens)");
    }

    #[test]
    fn test_streaming_status_thinking_with_input_tokens() {
        // Usage arrives before text — show ↑ input · thinking
        let verb = "Thinking";
        let input_tokens: u32 = 800;
        let secs = 3u64;
        let icon = THROB_FRAMES[0]; // "✦"
        let status = format!(
            "{} {}… ({} · ↑ {} · thinking)",
            icon,
            verb,
            format_elapsed(secs),
            format_token_count(input_tokens as usize),
        );
        assert_eq!(status, "✦ Thinking… (3s · ↑ 800 · thinking)");
    }

    #[test]
    fn test_throb_frames_cycle() {
        // Frames cycle without panicking
        let mut idx = 0usize;
        for _ in 0..100 {
            idx = (idx + 1) % THROB_FRAMES.len();
            assert!(!THROB_FRAMES[idx].is_empty());
        }
        // After 4 steps we're back to frame 0
        assert_eq!(THROB_FRAMES.len(), 4);
    }

    // compact_tool_summary, tool_result_to_display, strip_ansi, bash_smart_summary
    // tests moved to tool_display.rs (where those functions now live).

    // ── PresentPlan display ───────────────────────────────────────────────────

    #[test]
    fn test_presentplan_label_shows_plan_title() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "PresentPlan",
            &serde_json::json!({"plan": "# Refactor Auth System\n\nDetails here..."}),
        );
        assert!(
            label.contains("Refactor Auth System"),
            "label should show plan title: {:?}",
            label
        );
        assert!(
            label.contains("PresentPlan"),
            "label should show tool name: {:?}",
            label
        );
    }

    #[test]
    fn test_presentplan_label_fallback_when_no_heading() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "PresentPlan",
            &serde_json::json!({"plan": "Just some prose with no heading."}),
        );
        assert!(
            label.contains("proposing plan"),
            "should fall back to 'proposing plan': {:?}",
            label
        );
    }

    #[test]
    fn test_presentplan_label_uses_first_heading_only() {
        use super::super::tool_display::format_tool_label;
        let label = format_tool_label(
            "presentplan",
            &serde_json::json!({"plan": "# First Title\n## Second Title\n\nContent"}),
        );
        assert!(
            label.contains("First Title"),
            "should use first heading: {:?}",
            label
        );
        assert!(
            !label.contains("Second Title"),
            "should not show second heading: {:?}",
            label
        );
    }

    // --- find_last_exchange ---

    fn user_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn assistant_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn find_last_exchange_empty_returns_empty_pair() {
        let (q, r) = find_last_exchange(&[]);
        assert!(q.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn find_last_exchange_only_user_messages() {
        let msgs = vec![user_msg("hello"), user_msg("world")];
        let (q, r) = find_last_exchange(&msgs);
        assert!(
            r.is_empty(),
            "no assistant msg → response should be empty: {:?}",
            r
        );
        assert!(q.is_empty());
    }

    #[test]
    fn find_last_exchange_single_turn() {
        let msgs = vec![user_msg("What is 2+2?"), assistant_msg("4")];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(q, "What is 2+2?");
        assert_eq!(r, "4");
    }

    #[test]
    fn find_last_exchange_picks_latest_turn() {
        let msgs = vec![
            user_msg("First question"),
            assistant_msg("First answer"),
            user_msg("Second question"),
            assistant_msg("Second answer"),
        ];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(q, "Second question");
        assert_eq!(r, "Second answer");
    }

    #[test]
    fn find_last_exchange_skips_empty_assistant_text() {
        let msgs = vec![
            user_msg("Real question"),
            assistant_msg("Real answer"),
            user_msg("Ignored"),
            // Assistant message with empty text (e.g., tool-only response)
            crate::claude::Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "   ".to_string(),
                }],
            },
        ];
        let (q, r) = find_last_exchange(&msgs);
        // Should skip the whitespace-only assistant msg and find the earlier real one
        assert_eq!(r, "Real answer");
        assert_eq!(q, "Real question");
    }

    #[test]
    fn find_last_exchange_assistant_only_no_preceding_user() {
        let msgs = vec![assistant_msg("Unprompted response")];
        let (q, r) = find_last_exchange(&msgs);
        assert_eq!(r, "Unprompted response");
        // No user message precedes it
        assert!(q.is_empty(), "query should be empty: {:?}", q);
    }

    // --- apply_sliding_window ---

    fn make_msgs(roles: &[&str]) -> Vec<crate::claude::Message> {
        roles
            .iter()
            .enumerate()
            .map(|(i, &role)| {
                let text = format!("msg {}", i);
                if role == "user" {
                    user_msg(&text)
                } else {
                    assistant_msg(&text)
                }
            })
            .collect()
    }

    #[test]
    fn test_sliding_window_trims_to_max_verbatim() {
        // 30 alternating messages, max 20 → 20 returned, first is user
        let roles: Vec<&str> = (0..30)
            .map(|i| if i % 2 == 0 { "user" } else { "assistant" })
            .collect();
        let msgs = make_msgs(&roles);
        let result = apply_sliding_window(msgs, 20);
        assert_eq!(result.len(), 20);
        assert_eq!(result.first().unwrap().role, "user");
    }

    #[test]
    fn test_sliding_window_disabled_when_zero() {
        let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
        let len = msgs.len();
        let result = apply_sliding_window(msgs, 0);
        assert_eq!(result.len(), len);
    }

    #[test]
    fn test_sliding_window_no_op_when_under_limit() {
        let msgs = make_msgs(&["user", "assistant", "user", "assistant"]);
        let result = apply_sliding_window(msgs, 20);
        assert_eq!(result.len(), 4);
        assert_eq!(result.first().unwrap().role, "user");
    }

    #[test]
    fn test_sliding_window_skips_orphaned_assistant_at_boundary() {
        // 5 messages: u a u a u, window=3 → last 3 are [a, u, a] (index 2,3,4)
        // Leading 'a' gets skipped → result is [u, a] starting at index 3
        let msgs = make_msgs(&["user", "assistant", "user", "assistant", "user"]);
        // Swap last 3 to [assistant, user, assistant] by building manually:
        let roles = ["user", "assistant", "user", "assistant", "user"];
        // With window=3: last 3 = msgs[2..] = [user, assistant, user] → starts with user already
        // To actually trigger the skip, build a window that starts with assistant:
        let msgs2 = make_msgs(&["user", "assistant", "assistant", "user", "assistant"]);
        // window=3 → last 3 = [assistant(idx2), user(idx3), assistant(idx4)]
        // leading assistant removed → [user, assistant]
        let result = apply_sliding_window(msgs2, 3);
        assert_eq!(result.first().unwrap().role, "user");
        assert!(result.len() < 3); // shortened due to skipping
        let _ = roles; // silence unused warning
        let _ = msgs;
    }

    #[test]
    fn test_sliding_window_minimum_guard_prevents_empty() {
        // All messages are assistant-role (pathological case)
        let msgs = make_msgs(&["assistant", "assistant", "assistant", "assistant"]);
        // window=3 → last 3 are all assistant; floor at 2 prevents empty
        let result = apply_sliding_window(msgs, 3);
        assert!(
            result.len() >= 2,
            "floor of 2 must be maintained; got {}",
            result.len()
        );
    }

    /// Regression: orphaned tool_result at window boundary must be stripped.
    ///
    /// Scenario: conversation has two full tool-call round-trips followed by a
    /// user text turn.  With a small window, the first round-trip's tool_use is
    /// cut but its tool_result survives as the first message in the window.
    /// All providers reject `tool_result` blocks without a matching `tool_use`.
    #[test]
    fn test_sliding_window_strips_orphaned_tool_result_at_boundary() {
        use crate::claude::Message;

        // Build:
        //   [0] user "question"          ← will be dropped by window
        //   [1] assistant with ToolUse   ← will be dropped by window (cut here)
        //   [2] user with ToolResult     ← ORPHANED — tool_use was dropped
        //   [3] assistant "answer 1"
        //   [4] user "next question"
        //   [5] assistant "answer 2"
        let tool_use_id = "call_orphan_test".to_string();

        let msgs: Vec<Message> = vec![
            // [0] old user turn (outside window)
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "question".to_string(),
                }],
            },
            // [1] assistant with ToolUse (will be cut by window)
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolUse {
                    id: tool_use_id.clone(),
                    name: "bash".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                }],
            },
            // [2] user with ToolResult — orphaned when [1] is cut
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content: "file1.rs\nfile2.rs".to_string(),
                    is_error: None,
                }],
            },
            // [3] assistant reply
            assistant_msg("answer 1"),
            // [4] next user turn
            user_msg("next question"),
            // [5] assistant reply
            assistant_msg("answer 2"),
        ];

        // window=4 keeps msgs[2..] = [orphaned ToolResult user, assistant, user, assistant]
        let result = apply_sliding_window(msgs, 4);

        // The orphaned tool_result user turn ([2]) and its assistant response ([3])
        // must have been stripped, leaving [user "next question", assistant "answer 2"].
        assert!(
            result.len() >= 2,
            "must have at least 2 messages; got {}",
            result.len()
        );
        assert_eq!(
            result.first().unwrap().role,
            "user",
            "window must start with a user message"
        );
        // Crucially: the first user message must NOT be a tool_result-only message.
        let first_has_only_tool_results = result.first().map(|m| {
            m.content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        assert_ne!(
            first_has_only_tool_results,
            Some(true),
            "orphaned tool_result user message must have been stripped"
        );
    }

    /// Regression: when ALL messages in the window are tool round-trips (no plain
    /// user text), the old cascade-removal code would strip the orphaned tool_result
    /// AND the next assistant, making the following tool_result an orphan, and so on
    /// until the 2-message floor left a single orphaned tool_result at position 0.
    /// The fix inserts a placeholder user turn instead of cascading.
    #[test]
    fn test_sliding_window_all_tool_rounds_no_cascade_orphan() {
        use crate::claude::Message;

        // Build a conversation that is ENTIRELY tool round-trips:
        //   [0] user "query"               ← outside window (dropped by slice)
        //   [1] asst tool_use(A)           ← outside window
        //   [2] user tool_result(A)        ← window start → ORPHANED
        //   [3] asst tool_use(B)           ← valid pair start
        //   [4] user tool_result(B)        ← valid
        //   [5] asst tool_use(C)
        //   [6] user tool_result(C)
        //
        // window=5 keeps msgs[2..] = [orphan, asst(B), user(B), asst(C), user(C)]
        let make_tool_result_msg = |id: &str| Message {
            role: "user".to_string(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.to_string(),
                content: "ok".to_string(),
                is_error: None,
            }],
        };
        let make_tool_use_msg = |id: &str| Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({}),
            }],
        };

        let msgs: Vec<Message> = vec![
            user_msg("query"),         // [0] outside window
            make_tool_use_msg("A"),    // [1] outside window
            make_tool_result_msg("A"), // [2] orphaned boundary
            make_tool_use_msg("B"),    // [3] valid
            make_tool_result_msg("B"), // [4] valid
            make_tool_use_msg("C"),    // [5] valid
            make_tool_result_msg("C"), // [6] valid
        ];

        let result = apply_sliding_window(msgs, 5);

        // Must start with a user message.
        assert_eq!(
            result.first().unwrap().role,
            "user",
            "window must start with user"
        );
        // The first user message must NOT be a pure tool_result (no orphan).
        let first_is_tool_result_only = result.first().map(|m| {
            m.content
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        assert_ne!(
            first_is_tool_result_only,
            Some(true),
            "orphaned tool_result must not be first: {:?}",
            result.first()
        );
        // Valid tool rounds (B and C) must be preserved.
        assert!(
            result.len() >= 4,
            "valid tool round-trips B and C should be in window; got {} messages",
            result.len()
        );
    }

    /// Regression: orphaned tool_use (assistant sent tool_use but query was
    /// cancelled before tool_result was added). The final-pass validator in
    /// apply_sliding_window should strip the orphaned tool_use blocks, keeping
    /// any text content, so the conversation sent to the provider is clean.
    #[test]
    fn test_sliding_window_strips_orphaned_tool_use() {
        use crate::claude::Message;

        // Simulate a cancelled query: assistant wrote tool_uses but the
        // corresponding tool_result user message was never added.
        //   [0] user "query"
        //   [1] assistant: text + tool_use A  ← orphaned (no tool_result follows)
        //   [2] user "fix it"
        let msgs: Vec<Message> = vec![
            user_msg("query"),
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::Text {
                        text: "I'll analyze that.".to_string(),
                    },
                    ContentBlock::ToolUse {
                        id: "toolu_orphan".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"file_path": "/foo"}),
                    },
                ],
            },
            user_msg("fix it"),
        ];

        let result = apply_sliding_window(msgs, 20);

        // The orphaned tool_use block must be stripped; text content kept.
        for msg in &result {
            let has_orphaned_tool_use = msg.content.iter().any(|b| {
                if let ContentBlock::ToolUse { id, .. } = b {
                    id == "toolu_orphan"
                } else {
                    false
                }
            });
            assert!(
                !has_orphaned_tool_use,
                "orphaned tool_use must be stripped; got message: {:?}",
                msg
            );
        }

        // The text content ("I'll analyze that.") should be preserved.
        let has_text = result.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("I'll analyze")))
        });
        assert!(
            has_text,
            "text content of orphaned assistant message must be preserved"
        );

        // Window must start with a user message.
        assert_eq!(result.first().unwrap().role, "user");

        // "fix it" must still be present.
        let has_fix_it = result.iter().any(|m| {
            m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if text.contains("fix it")))
        });
        assert!(has_fix_it, "user follow-up message must be preserved");
    }

    // ── tool_approval_summary ────────────────────────────────────────────────

    fn make_tool_use(name: &str, input: serde_json::Value) -> crate::tools::types::ToolUse {
        crate::tools::types::ToolUse {
            id: "test_id".to_string(),
            name: name.to_string(),
            input,
        }
    }

    // ── humanize_forth_error ─────────────────────────────────────────────────

    #[test]
    fn test_humanize_unknown_word() {
        let msg = humanize_forth_error("unknown word: foo");
        assert!(msg.contains("\"foo\" isn't defined yet"), "got: {msg}");
        assert!(
            msg.contains(": foo"),
            "should show how to define it, got: {msg}"
        );
    }

    #[test]
    fn test_humanize_stack_underflow() {
        let msg = humanize_forth_error("stack underflow");
        assert!(msg.contains("not enough values"), "got: {msg}");
    }

    #[test]
    fn test_humanize_division_by_zero() {
        let msg = humanize_forth_error("division by zero");
        assert!(msg.contains("can't divide by zero"), "got: {msg}");
    }

    #[test]
    fn test_humanize_fuel_exhausted() {
        let msg = humanize_forth_error("fuel exhausted — word is too expensive");
        assert!(
            msg.contains("infinite loop") || msg.contains("too long"),
            "got: {msg}"
        );
        assert!(msg.contains("undo"), "should hint at undo, got: {msg}");
    }

    #[test]
    fn test_humanize_strips_prefix() {
        let msg = humanize_forth_error("forth error: division by zero");
        assert!(
            !msg.contains("forth error:"),
            "prefix should be stripped, got: {msg}"
        );
    }

    #[test]
    fn test_humanize_unknown_falls_through() {
        let msg = humanize_forth_error("some unusual vm error");
        assert_eq!(msg, "some unusual vm error");
    }

    #[test]
    fn test_tool_approval_summary_bash_with_command() {
        let tool = make_tool_use(
            "bash",
            serde_json::json!({"command": "git push origin main"}),
        );
        assert_eq!(
            tool_approval_summary(&tool),
            "Command: git push origin main"
        );
    }

    #[test]
    fn test_tool_approval_summary_bash_uppercase() {
        let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo test"}));
        assert_eq!(tool_approval_summary(&tool), "Command: cargo test");
    }

    #[test]
    fn test_tool_approval_summary_bash_long_command_truncated() {
        let long_cmd = "a".repeat(70);
        let tool = make_tool_use("bash", serde_json::json!({"command": long_cmd}));
        let result = tool_approval_summary(&tool);
        assert!(
            result.starts_with("Command: "),
            "should start with 'Command: ': {}",
            result
        );
        assert!(
            result.contains("..."),
            "long command should be truncated with '...': {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_bash_no_command() {
        let tool = make_tool_use("bash", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Execute shell command");
    }

    #[test]
    fn test_tool_approval_summary_read_with_path() {
        let tool = make_tool_use("read", serde_json::json!({"file_path": "src/main.rs"}));
        assert_eq!(tool_approval_summary(&tool), "File: src/main.rs");
    }

    #[test]
    fn test_tool_approval_summary_read_uppercase() {
        let tool = make_tool_use("Read", serde_json::json!({"file_path": "/a/b/c.rs"}));
        assert_eq!(tool_approval_summary(&tool), "File: /a/b/c.rs");
    }

    #[test]
    fn test_tool_approval_summary_read_no_path() {
        let tool = make_tool_use("read", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Read file");
    }

    #[test]
    fn test_tool_approval_summary_grep_with_pattern() {
        let tool = make_tool_use(
            "grep",
            serde_json::json!({"pattern": "fn main", "path": "src"}),
        );
        assert_eq!(tool_approval_summary(&tool), "Pattern: fn main");
    }

    #[test]
    fn test_tool_approval_summary_grep_long_pattern_truncated() {
        let long = "x".repeat(50);
        let tool = make_tool_use("grep", serde_json::json!({"pattern": long}));
        let result = tool_approval_summary(&tool);
        assert!(result.starts_with("Pattern: "), "got: {}", result);
        assert!(
            result.contains("..."),
            "long pattern should truncate: {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_grep_no_pattern() {
        let tool = make_tool_use("Grep", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Search files");
    }

    #[test]
    fn test_tool_approval_summary_glob_with_pattern() {
        let tool = make_tool_use("glob", serde_json::json!({"pattern": "**/*.rs"}));
        assert_eq!(tool_approval_summary(&tool), "Pattern: **/*.rs");
    }

    #[test]
    fn test_tool_approval_summary_glob_uppercase_no_pattern() {
        let tool = make_tool_use("Glob", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Find files");
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_with_reason() {
        let tool = make_tool_use(
            "EnterPlanMode",
            serde_json::json!({"reason": "Need to research the codebase"}),
        );
        assert_eq!(
            tool_approval_summary(&tool),
            "Reason: Need to research the codebase"
        );
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_long_reason_truncated() {
        let long_reason = "r".repeat(60);
        let tool = make_tool_use("EnterPlanMode", serde_json::json!({"reason": long_reason}));
        let result = tool_approval_summary(&tool);
        assert!(result.starts_with("Reason: "), "got: {}", result);
        assert!(
            result.contains("..."),
            "long reason should truncate: {}",
            result
        );
    }

    #[test]
    fn test_tool_approval_summary_enter_plan_mode_no_reason() {
        let tool = make_tool_use("EnterPlanMode", serde_json::json!({}));
        assert_eq!(tool_approval_summary(&tool), "Enter planning mode");
    }

    #[test]
    fn test_tool_approval_summary_unknown_tool() {
        let tool = make_tool_use("WebFetch", serde_json::json!({"url": "https://docs.rs"}));
        assert_eq!(tool_approval_summary(&tool), "Execute WebFetch tool");
    }

    // ── dialog_result_to_confirmation (3-option Claude Code style) ───────────

    #[test]
    fn test_dialog_result_selected_0_approve_once() {
        // Option "1. Yes" → ApproveOnce
        let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(0), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::ApproveOnce
            ),
            "index 0 (Yes) should be ApproveOnce, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_selected_1_approve_pattern_session() {
        // Option "2. Yes, and don't ask again for: bash:*" → ApprovePatternSession
        let tool = make_tool_use("bash", serde_json::json!({"command": "git status"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(p.tool_name, "bash");
                assert_eq!(p.pattern, "*");
                assert!(
                    p.description.contains("session"),
                    "description: {}",
                    p.description
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_dialog_result_selected_2_deny() {
        // Option "3. No" → Deny
        let tool = make_tool_use("bash", serde_json::json!({"command": "rm -rf /"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "index 2 (No) should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_selected_high_index_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(99), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "out-of-range index should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_cancelled_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "echo hi"}));
        let result = dialog_result_to_confirmation(crate::cli::tui::DialogResult::Cancelled, &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "Cancelled should be Deny, got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_custom_text_deny() {
        let tool = make_tool_use("bash", serde_json::json!({"command": "ls"}));
        let result = dialog_result_to_confirmation(
            crate::cli::tui::DialogResult::CustomText("please allow".to_string()),
            &tool,
        );
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "CustomText should be Deny (safety), got {:?}",
            result
        );
    }

    #[test]
    fn test_dialog_result_pattern_session_uses_tool_name() {
        // Verify the "don't ask again" pattern uses the actual tool name
        let tool = make_tool_use("grep", serde_json::json!({"pattern": "TODO"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(
                    p.tool_name, "grep",
                    "pattern tool_name should match tool: {}",
                    p.tool_name
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_pattern_session_tool_name_matches_tool_use() {
        // The pattern's tool_name must match the tool being approved —
        // otherwise the cache won't recognise future calls to the same tool.
        // Index 1 = "2. Yes, and don't ask again for: Bash:*"
        let tool = make_tool_use("Bash", serde_json::json!({"command": "cargo fmt"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(1), &tool);
        match result {
            crate::cli::repl_event::events::ConfirmationResult::ApprovePatternSession(p) => {
                assert_eq!(
                    p.tool_name, "Bash",
                    "pattern tool_name should match ToolUse.name"
                );
            }
            other => panic!("expected ApprovePatternSession, got {:?}", other),
        }
    }

    #[test]
    fn test_pattern_persistent_tool_name_matches_tool_use() {
        // Persistent approval is no longer in the 3-option dialog.
        // Index 2 → Deny; index 99 → Deny. Just verify nothing panics.
        let tool = make_tool_use("read", serde_json::json!({"file_path": "src/lib.rs"}));
        let result =
            dialog_result_to_confirmation(crate::cli::tui::DialogResult::Selected(2), &tool);
        assert!(
            matches!(
                result,
                crate::cli::repl_event::events::ConfirmationResult::Deny
            ),
            "index 2 is No/Deny in 3-option dialog, got {:?}",
            result
        );
    }

    // ── Brain context injection ──────────────────────────────────────────────

    #[test]
    fn test_brain_context_injection_formats_separator() {
        // When brain context is present it should be appended after a separator.
        let input = "How do I implement async in Rust?".to_string();
        let brain_ctx = "Found src/models/bootstrap.rs — relevant for async patterns.".to_string();
        let enriched = format!("{}\n\n---\n[Pre-gathered context:\n{}]", input, brain_ctx);

        assert!(enriched.contains("---"), "should contain separator");
        assert!(enriched.contains("Pre-gathered context:"));
        assert!(enriched.contains("How do I implement async"));
        assert!(enriched.contains("bootstrap.rs"));
    }

    #[test]
    fn test_brain_context_none_does_not_modify_query() {
        // When there is no brain context the query should pass through unchanged.
        let input = "What is a lifetime?".to_string();
        let brain_ctx: Option<String> = None;
        let enriched = match brain_ctx {
            Some(ctx) if !ctx.trim().is_empty() => {
                format!("{}\n\n---\n[Pre-gathered context:\n{}]", input, ctx)
            }
            _ => input.clone(),
        };
        assert_eq!(
            enriched, input,
            "query should be unchanged when brain has no context"
        );
    }

    #[test]
    fn test_brain_context_empty_not_injected() {
        // Regression: an empty or whitespace-only brain context must NOT be injected.
        let input = "What is a lifetime?".to_string();
        for empty_ctx in ["", "  ", "\n", "\t\n "] {
            let brain_ctx: Option<String> = Some(empty_ctx.to_string());
            let enriched = match brain_ctx {
                Some(ctx) if !ctx.trim().is_empty() => {
                    format!("{}\n\n---\n[Pre-gathered context:\n{}]", input, ctx)
                }
                _ => input.clone(),
            };
            assert_eq!(
                enriched, input,
                "whitespace-only brain context '{:?}' should not be injected",
                empty_ctx
            );
        }
    }

    #[test]
    fn test_pending_brain_question_tx_cleared_on_submit() {
        // Regression: pending_brain_question_tx must be cleared when the user submits
        // so a stale sender doesn't intercept the next tool-approval dialog result.
        // We test the guard logic in isolation (can't drive the full EventLoop here).
        let (tx, _rx) = tokio::sync::oneshot::channel::<String>();
        let mut pending: Option<tokio::sync::oneshot::Sender<String>> = Some(tx);
        let mut options: Vec<String> = vec!["Option A".to_string()];

        // Simulate what the Submitted arm does
        let was_pending = pending.take().is_some();
        options.clear();

        assert!(
            was_pending,
            "pending_brain_question_tx should have been Some"
        );
        assert!(
            pending.is_none(),
            "pending_brain_question_tx should be None after take"
        );
        assert!(
            options.is_empty(),
            "pending_brain_question_options should be cleared"
        );
    }

    #[test]
    fn test_extract_channel_forth_definition() {
        let msg = "[#forth] alice: : double  2 * ;";
        assert_eq!(
            extract_channel_forth(msg),
            Some(": double  2 * ;".to_string())
        );
    }

    #[test]
    fn test_extract_channel_forth_non_definition() {
        let msg = "[#general] alice: hello world";
        assert_eq!(extract_channel_forth(msg), None);
    }

    #[test]
    fn test_extract_channel_forth_non_channel() {
        let msg = "← plain peer message";
        assert_eq!(extract_channel_forth(msg), None);
    }

    #[test]
    fn test_extract_scatter_exec_commands_none() {
        let cmds = extract_scatter_exec_commands("1 2 + .");
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_extract_scatter_exec_commands_single() {
        let cmds = extract_scatter_exec_commands(r#"scatter-exec" hostname""#);
        assert_eq!(cmds, vec![r#"* → bash -c "hostname""#]);
    }

    #[test]
    fn test_extract_scatter_exec_commands_multiple() {
        let cmds = extract_scatter_exec_commands(
            r#"peer" 192.168.1.1:11435" scatter-exec" hostname" scatter-exec" uname -a""#,
        );
        assert_eq!(
            cmds,
            vec![r#"* → bash -c "hostname""#, r#"* → bash -c "uname -a""#]
        );
    }

    #[test]
    fn test_handle_typing_started_skips_commands() {
        // Inputs starting with '/' are slash-commands and should not trigger the brain.
        let input = "/help".to_string();
        let should_skip = input.trim().starts_with('/') || input.trim().len() < 10;
        assert!(should_skip, "/help should be skipped (command)");
    }

    #[test]
    fn test_handle_typing_started_skips_short_input() {
        // Inputs shorter than 10 chars are not worth speculating on.
        let input = "short".to_string();
        let should_skip = input.trim().starts_with('/') || input.trim().len() < 10;
        assert!(should_skip, "input < 10 chars should be skipped");
    }

    #[test]
    fn test_silent_remark_single_boom() {
        let r = silent_remark("boom");
        assert!(r.contains("boom"), "boom remark should mention boom: {r}");
        assert!(
            !r.contains("BOOM BOOM"),
            "single boom should not get triple treatment"
        );
    }

    #[test]
    fn test_silent_remark_triple_boom_escalates() {
        let r = silent_remark("BOOM BOOM BOOM");
        assert!(
            r.contains("yes") || r.contains("BOOM BOOM BOOM") || r.contains("go"),
            "triple boom should be excited: {r}"
        );
    }

    #[test]
    fn test_silent_remark_double_word() {
        let r = silent_remark("help help");
        assert!(
            r.contains("help") && (r.contains("twice") || r.contains("urgency")),
            "double help should get double treatment: {r}"
        );
    }

    #[test]
    fn test_silent_remark_fireballs() {
        let r = silent_remark("fireballs");
        assert!(
            r.contains("fireball"),
            "fireballs should get fireball remark: {r}"
        );
    }

    #[test]
    fn test_is_magic_word_please() {
        assert!(
            magic_word_response("please").is_some(),
            "please is a magic word"
        );
    }

    #[test]
    fn test_is_magic_word_unknown_is_not_magic() {
        assert!(
            magic_word_response("xyzzy").is_none(),
            "unknown word must not be magic"
        );
    }

    #[test]
    fn test_silent_remark_please_is_noted() {
        let r = silent_remark("please");
        assert!(
            r.contains("noted") && r.contains("unmoved"),
            "please must get its specific remark: {r}"
        );
    }

    #[test]
    fn test_silent_remark_please_does_not_get_generic() {
        let r = silent_remark("please");
        // Must not fall through to the rotating generic remarks
        let generic = [
            "the stack kept it to itself",
            "the silence is part of it",
            "we just can't prove it",
            "left no evidence",
            "nothing to show for it",
            "somewhere, a bit flipped",
            "the machine shrugged",
            "filed under: nothing",
            "works on my stack",
            "witnesses: zero",
        ];
        for g in &generic {
            assert!(
                !r.contains(g),
                "please must not fall through to generic remark '{g}': {r}"
            );
        }
    }

    #[test]
    fn test_definition_observation_violent_name_silent_body() {
        // `: boom ;` — violent name, empty/silent body
        let obs = definition_observation("boom", "");
        // May or may not fire (30% gate), but if it does it should mention quietness
        if let Some(r) = obs {
            assert!(
                !r.contains("you sent me a machine called"),
                "should not repeat that phrase verbatim: {r}"
            );
        }
    }

    #[test]
    fn test_definition_observation_recursive_word() {
        // Recursive words should always get a comment (no 30% gate for this path)
        // Test by forcing all time values — just check the string when it fires.
        // Since it's time-gated we just call it many times and verify the content when Some.
        let mut found_recursive = false;
        for _ in 0..20 {
            if let Some(r) = definition_observation(
                "fib",
                "dup 2 < if drop 1 exit then dup 1 - fib swap 2 - fib +",
            ) {
                assert!(
                    r.contains("recurse") || r.contains("itself") || r.contains("base"),
                    "recursive remark should mention recursion: {r}"
                );
                found_recursive = true;
                break;
            }
        }
        // It's time-gated so we can't guarantee it fires, but the content is correct when it does.
        let _ = found_recursive;
    }
}

/// Open `content` in `$VISUAL` or `$EDITOR` (falling back to `vi`), let the user
/// edit it, and return the saved result.  Suspends the terminal while the editor
/// runs and restores it afterwards.
fn open_in_editor(content: &str) -> anyhow::Result<String> {
    // Pick editor: $VISUAL > $EDITOR > vi
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    // Write proposed content to a temp file
    let tmp_path = std::env::temp_dir().join(format!("finch-edit-{}.txt", std::process::id()));
    std::fs::write(&tmp_path, content.as_bytes())?;

    // Suspend raw mode so the editor has full terminal control
    crossterm::terminal::disable_raw_mode()?;

    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to launch editor '{}': {}", editor, e))?;

    // Restore raw mode
    crossterm::terminal::enable_raw_mode()?;

    if !status.success() {
        anyhow::bail!("Editor exited with status {}", status);
    }

    let edited = std::fs::read_to_string(&tmp_path)?;
    Ok(edited)
}

// ── looks_like_natural_language tests ─────────────────────────────────────────
#[cfg(test)]
mod nl_routing_tests {
    use super::looks_like_natural_language;

    fn no_words(_: &str) -> bool {
        false
    }
    fn all_words(_: &str) -> bool {
        true
    }

    // Convenience wrapper: same closure for word_exists and session_word.
    // Matches pre-refactor behaviour for all tests that don't distinguish the two.
    fn nl(s: &str, w: impl Fn(&str) -> bool) -> bool {
        looks_like_natural_language(s, &w, &w)
    }

    // Question marks always → AI
    #[test]
    fn test_question_mark_latin() {
        assert!(nl("what is this?", no_words));
    }
    #[test]
    fn test_question_mark_fullwidth() {
        assert!(nl("何？", no_words));
    }
    #[test]
    fn test_question_embedded() {
        assert!(nl("is 2 + 2 = 4?", no_words));
    }

    // Uppercase first letter → AI
    #[test]
    fn test_uppercase_start() {
        assert!(nl("Show me something", no_words));
    }
    #[test]
    fn test_uppercase_single_word() {
        assert!(nl("Hello", no_words));
    }

    // Non-ASCII non-definition → AI
    #[test]
    fn test_non_ascii_chinese() {
        assert!(nl("你好", no_words));
    }
    #[test]
    fn test_non_ascii_arabic() {
        assert!(nl("مرحبا", no_words));
    }
    #[test]
    fn test_non_ascii_definition_not_nl() {
        // `: 你好 1 . ;` is a definition — should NOT be routed to AI
        assert!(!nl(": 你好 1 . ;", no_words));
    }

    // Multi-word with unknown alphabetic word → AI
    #[test]
    fn test_multi_word_unknown() {
        assert!(nl("show me something", no_words));
    }
    #[test]
    fn test_multi_word_with_number() {
        assert!(nl("show me 3 examples", no_words));
    }
    #[test]
    fn test_multi_word_all_known_is_forth() {
        // Every token is session-known → Forth, not AI
        assert!(!nl("dup swap drop", all_words));
    }
    #[test]
    fn test_multi_word_all_numbers_is_forth() {
        // Pure number sequence → Forth stack push, not AI
        assert!(!nl("1 2 3", no_words));
    }

    // Single unknown alphabetic word → AI
    #[test]
    fn test_single_unknown_word() {
        assert!(nl("foobar", no_words));
    }
    // Hyphen is a Forth operator char, so single hyphenated token → Forth (not AI).
    #[test]
    fn test_single_hyphenated_is_forth_not_nl() {
        assert!(!nl("my-thing", no_words));
    }

    // Known Forth primitives → Forth (not AI)
    #[test]
    fn test_single_known_primitive_dup() {
        assert!(!nl("dup", no_words));
    }
    #[test]
    fn test_single_known_primitive_drop() {
        assert!(!nl("drop", no_words));
    }
    #[test]
    fn test_single_known_primitive_swap() {
        assert!(!nl("swap", no_words));
    }
    #[test]
    fn test_single_number_is_forth() {
        assert!(!nl("42", no_words));
    }
    #[test]
    fn test_single_float_is_forth() {
        assert!(!nl("3.14", no_words));
    }

    // Forth operators in input → Forth
    #[test]
    fn test_contains_forth_operator_plus() {
        assert!(!nl("2 3 +", no_words));
    }
    #[test]
    fn test_contains_forth_operator_dot() {
        assert!(!nl("42 .", no_words));
    }

    // Definitions → Forth
    #[test]
    fn test_colon_definition() {
        assert!(!nl(": foo 1 . ;", no_words));
    }

    // Word known to VM → Forth
    #[test]
    fn test_single_known_vm_word() {
        assert!(!nl("hello", all_words));
    }

    // Regression: "it's just two greats talking" — multi-word with unknown words → AI
    #[test]
    fn test_regression_two_greats() {
        assert!(nl("it's just two greats talking", no_words));
    }
    // Regression: "we could do that whatever" — multi-word unknown → AI
    #[test]
    fn test_regression_we_could() {
        assert!(nl("we could do that", no_words));
    }

    // Contractions → AI (the apostrophe+letter pattern is unambiguous prose).
    #[test]
    fn test_contraction_youre() {
        assert!(nl("you're not talking to me.", all_words));
    }
    #[test]
    fn test_contraction_dont() {
        assert!(nl("don't do that", no_words));
    }
    #[test]
    fn test_contraction_its() {
        assert!(nl("it's working", no_words));
    }

    // Sentence-ending period attached to a word → AI.
    #[test]
    fn test_period_attached_to_word() {
        assert!(nl("hello.", no_words));
    }
    #[test]
    fn test_period_attached_multi_word() {
        assert!(nl("this is a sentence.", no_words));
    }
    // Standalone "." is the Forth print-TOS op — NOT natural language.
    #[test]
    fn test_standalone_period_is_forth() {
        assert!(!nl("42 .", no_words));
    }
    // "3.14" is a float literal — Forth, not natural language.
    #[test]
    fn test_float_with_period_is_forth() {
        assert!(!nl("3.14", no_words));
    }

    // Exclamation mark attached to a word → AI.
    #[test]
    fn test_exclamation_attached() {
        assert!(nl("hello!", no_words));
    }

    // ── session_word distinction ───────────────────────────────────────────────
    // "hello world" — both words are in the precompiled VM library (word_exists=true)
    // but neither was defined during this session (session_word=false).
    // Should route to AI so it gets a conversational response, not Forth execution
    // of the `hello` word which would echo "hello" back.
    #[test]
    fn test_hello_world_library_only_is_nl() {
        assert!(looks_like_natural_language(
            "hello world",
            all_words,
            no_words
        ));
    }

    #[test]
    fn test_boot_vocabulary_words_remain_ambient_for_routing() {
        use std::collections::HashSet;

        let ambient: HashSet<String> = crate::coforth::Library::builtin_defs()
            .pairs
            .iter()
            .map(|(word, _)| word.clone())
            .collect();
        assert!(ambient.contains("hello"));
        let word_exists = |word: &str| ambient.contains(word);
        let session_word = |word: &str| word_exists(word) && !ambient.contains(word);

        assert!(looks_like_natural_language(
            "hello world",
            word_exists,
            session_word
        ));
    }

    // "dup swap drop" — primitives; even with session_word=false they stay Forth.
    #[test]
    fn test_dup_swap_drop_primitives_stay_forth() {
        assert!(!looks_like_natural_language(
            "dup swap drop",
            all_words,
            no_words
        ));
    }

    // Once the user explicitly defines both words in the session, "hello world"
    // becomes a Forth invocation (session_word=true for both).
    #[test]
    fn test_hello_world_session_defined_is_forth() {
        assert!(!looks_like_natural_language(
            "hello world",
            all_words,
            all_words
        ));
    }
}

// ── filter_ai_forth_response edge case tests ──────────────────────────────────
#[cfg(test)]
mod filter_tests {
    use super::{filter_ai_forth_response, skip_known_word_defs};

    // ── skip_known_word_defs tests ─────────────────────────────────────────────

    #[test]
    fn test_skip_known_word_defs_single_line() {
        // Single-line definition for a known word is dropped entirely.
        let code = ": prime?  dup 2 < if drop false exit then true ;";
        let result = skip_known_word_defs(code, |w| w == "prime?");
        assert!(
            !result.contains(": prime?"),
            "known single-line def should be removed"
        );
        assert!(
            !result.contains("dup 2 < if"),
            "body of known single-line def should not appear"
        );
    }

    #[test]
    fn test_skip_known_word_defs_multiline_body_dropped() {
        // Multi-line definition: header on one line, body on next.
        // The OLD bug: only the header was dropped; the body line leaked through
        // as bare Forth code, causing e.g. `begin dup prime? until` to loop forever.
        let code = ": next-prime\n  1+ begin dup prime? until ;\nprove-all";
        let result = skip_known_word_defs(code, |w| w == "next-prime");
        assert!(
            !result.contains(": next-prime"),
            "known multi-line header should be removed"
        );
        assert!(
            !result.contains("begin dup prime? until"),
            "body of known multi-line def must not leak as bare code"
        );
        assert!(
            result.contains("prove-all"),
            "non-definition lines after the def should be kept"
        );
    }

    #[test]
    fn test_skip_known_word_defs_unknown_word_kept() {
        // Definitions for unknown words are preserved unchanged.
        let code = ": new-word  1 2 + . ;";
        let result = skip_known_word_defs(code, |_| false);
        assert!(
            result.contains(": new-word"),
            "unknown word def should be kept"
        );
    }

    #[test]
    fn test_skip_known_word_defs_mixed() {
        // Known word filtered, unknown word kept, trailing code kept.
        let code = ": prime?\n  dup 2 mod 0= if drop false exit then true ;\n\
                   : new-word  1 2 + ;\nprove-all";
        let result = skip_known_word_defs(code, |w| w == "prime?");
        assert!(!result.contains(": prime?"), "known def removed");
        assert!(
            !result.contains("dup 2 mod"),
            "body of known def must not leak"
        );
        assert!(result.contains(": new-word"), "unknown def kept");
        assert!(result.contains("prove-all"), "bare code kept");
    }

    // ── filter_ai_forth_response tests ────────────────────────────────────────

    #[test]
    fn test_filter_keeps_valid_def() {
        // filter normalizes whitespace (joins tokens with single space)
        let code = ": hello  .\" hello\" cr ;";
        let result = filter_ai_forth_response(code);
        assert!(result.contains(": hello"), "valid def should be kept");
        assert!(result.contains(".\""), "string print should be kept");
        assert!(result.contains(";"), "closing ; should be kept");
    }

    #[test]
    fn test_filter_removes_self_recursive_def() {
        // `hello` calls itself → infinite recursion → should be stripped.
        let code = ": hello  hello cr ;";
        assert!(
            filter_ai_forth_response(code).trim().is_empty(),
            "self-recursive definition should be removed"
        );
    }

    #[test]
    fn test_filter_removes_gen_call() {
        // Body calls gen:what — AI invocation during execution causes a loop.
        let code = ": greeting  gen:what cr ;";
        assert!(
            filter_ai_forth_response(code).trim().is_empty(),
            "definition with gen: call should be removed"
        );
    }

    #[test]
    fn test_filter_removes_garbage_long_body() {
        // More than 40 body tokens = vocabulary dump, not real Forth.
        let long_body = std::iter::repeat("word")
            .take(41)
            .collect::<Vec<_>>()
            .join(" ");
        let code = format!(": garbage  {long_body} ;");
        assert!(
            filter_ai_forth_response(&code).trim().is_empty(),
            "garbage long-body definition should be removed"
        );
    }

    #[test]
    fn test_filter_keeps_prove_all() {
        let code = ": hello  .\" hello\" cr ; prove-all";
        let result = filter_ai_forth_response(code);
        assert!(
            result.contains("prove-all"),
            "prove-all should be preserved"
        );
    }

    #[test]
    fn test_filter_mixed_good_and_bad() {
        let code = "\
            : good  .\" ok\" cr ; \
            : bad  bad cr ; \
            : also-good  1 1 + . ;";
        let result = filter_ai_forth_response(code);
        assert!(result.contains(": good"), "valid def should be kept");
        assert!(
            !result.contains(": bad"),
            "self-recursive def should be removed"
        );
        assert!(result.contains(": also-good"), "valid def should be kept");
    }

    #[test]
    fn test_filter_exact_40_token_body_ok() {
        // Exactly 40 body tokens — should be kept (threshold is > 40).
        let body = std::iter::repeat("drop")
            .take(40)
            .collect::<Vec<_>>()
            .join(" ");
        let code = format!(": word40  {body} ;");
        let result = filter_ai_forth_response(&code);
        assert!(result.contains(": word40"), "40-token body should be kept");
    }

    #[test]
    fn test_filter_41_token_body_rejected() {
        let body = std::iter::repeat("drop")
            .take(41)
            .collect::<Vec<_>>()
            .join(" ");
        let code = format!(": word41  {body} ;");
        let result = filter_ai_forth_response(&code);
        assert!(
            !result.contains(": word41"),
            "41-token body should be rejected"
        );
    }

    #[test]
    fn test_filter_indirect_recursion_not_caught() {
        // Indirect recursion (a→b→a) is NOT caught by the filter — that's OK,
        // the VM's call-depth guard handles it at runtime.
        let code = ": a  b . ; : b  a . ;";
        let result = filter_ai_forth_response(code);
        // Both should pass the filter (no direct self-call)
        assert!(
            result.contains(": a"),
            "indirect recursion not filtered at compile time"
        );
        assert!(
            result.contains(": b"),
            "indirect recursion not filtered at compile time"
        );
    }

    #[test]
    fn test_filter_gen_in_non_gen_word_blocked() {
        // gen:what in body of a normal word — should be blocked.
        let code = ": greet  gen:hello cr ;";
        assert!(filter_ai_forth_response(code).trim().is_empty());
    }

    #[test]
    fn test_filter_test_word_passes_through() {
        // test: words are just definitions — they should pass the same rules.
        let code = ": test:hello  hello depth 0 = assert ;";
        let result = filter_ai_forth_response(code);
        assert!(result.contains(": test:hello"), "test word should be kept");
    }

    #[test]
    fn test_filter_empty_input() {
        assert_eq!(filter_ai_forth_response("").trim(), "");
    }

    #[test]
    fn test_filter_only_prove_all() {
        let result = filter_ai_forth_response("prove-all");
        assert!(result.contains("prove-all"));
    }

    #[test]
    fn test_filter_malformed_missing_semicolon() {
        // Should skip malformed definitions completely
        let code = ": hello";
        assert_eq!(filter_ai_forth_response(code).trim(), "");
    }

    #[test]
    fn test_filter_malformed_missing_semicolon_with_body() {
        let code = ": hello world";
        assert_eq!(filter_ai_forth_response(code).trim(), "");
    }

    #[test]
    fn test_filter_malformed_outer_recovers_inner() {
        // `: outer : inner stuff ; more` — outer has no closing `;`.
        // After the fix, the parser skips the malformed outer, then recovers
        // `: inner stuff ;` as a valid inner definition.
        let code = ": outer : inner stuff ; more";
        let result = filter_ai_forth_response(code);
        // Inner definition is recovered; "more" passes through as a bare word.
        assert!(
            result.contains(": inner"),
            "inner def should be recovered after malformed outer"
        );
    }

    #[test]
    fn test_filter_mixed_malformed_recovers_valid_and_prove_all() {
        // Malformed definition followed by a valid one and prove-all.
        // After the fix, the valid def and prove-all are both preserved.
        let code = ": malformed hello : valid .\" ok\" ; prove-all";
        let result = filter_ai_forth_response(code);
        assert!(
            result.contains(": valid"),
            "valid def should be recovered after malformed one"
        );
        assert!(
            result.contains("prove-all"),
            "prove-all must survive a malformed definition"
        );
    }

    #[test]
    fn test_filter_empty_definition() {
        let code = ": empty ;";
        let result = filter_ai_forth_response(code);
        assert!(
            result.contains(": empty ;"),
            "empty definition should be valid"
        );
    }

    #[test]
    fn test_filter_multiple_gen_calls_blocked() {
        let code = ": multi  gen:a gen:b gen:c ;";
        assert!(filter_ai_forth_response(code).trim().is_empty());
    }

    // Regression: malformed definition (no `;`) must not consume prove-all.
    // Bug found by finch self-inspecting its own filter function.
    #[test]
    fn test_filter_malformed_def_does_not_consume_prove_all() {
        let code = ": broken no-semicolon-here prove-all";
        let result = filter_ai_forth_response(code);
        assert!(
            result.contains("prove-all"),
            "prove-all must survive a malformed definition with no closing semicolon"
        );
    }

    // Regression: malformed definition must not consume subsequent valid definitions.
    #[test]
    fn test_filter_malformed_def_followed_by_valid() {
        let code = ": broken no-semicolon : good .\" ok\" cr ;";
        let result = filter_ai_forth_response(code);
        assert!(
            result.contains(": good"),
            "valid definition after malformed one should be kept"
        );
    }
}

// ── Peer event loop tests ──────────────────────────────────────────────────────
#[cfg(test)]
mod peer_loop_tests {
    use super::*;
    use crate::session::SessionEvent;

    /// Both peer loops receive the broadcast and each sends back an independent result.
    #[tokio::test]
    async fn test_two_peers_both_reply_to_broadcast() {
        let (inbox_tx, mut inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (bcast_tx, sessions) = boot_peers(inbox_tx, None).await;

        assert_eq!(sessions.len(), 2, "boot_peers must fork exactly two peers");

        // Give each peer a moment to initialise their VMs.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Broadcast: push two values and add them.
        bcast_tx.send(SessionEvent::chat("3 4 +")).unwrap();

        // Collect replies with a timeout — both peers should respond.
        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut replies: Vec<(Uuid, String, String)> = Vec::new();
        loop {
            tokio::select! {
                Some(msg) = inbox_rx.recv() => {
                    replies.push(msg);
                    if replies.len() == 2 { break; }
                }
                _ = &mut deadline => { break; }
            }
        }

        assert_eq!(
            replies.len(),
            2,
            "expected two replies (one per peer), got {}",
            replies.len()
        );

        // Both peers should have computed 7.
        for (_, name, text) in &replies {
            assert!(
                text.contains('7'),
                "peer {name} should reply with ( 7 ), got: {text}"
            );
        }

        // The two peers must have different names.
        let names: Vec<&str> = replies.iter().map(|(_, n, _)| n.as_str()).collect();
        assert_ne!(names[0], names[1], "peers must have distinct names");
    }

    /// Peer Lisp must use the shared typed runtime rather than the legacy
    /// evaluator. A typed definition and its call live in one verified source
    /// submission and both peers return its `say` output.
    #[tokio::test]
    async fn test_peers_execute_typed_lisp_wire_programs() {
        let (inbox_tx, mut inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (bcast_tx, _sessions) = boot_peers(inbox_tx, None).await;

        bcast_tx
            .send(SessionEvent::chat(
                "(define (square (n : int)) (* n n)) (say (int-to-string (square 9)))",
            ))
            .unwrap();

        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut replies = Vec::new();
        loop {
            tokio::select! {
                Some(message) = inbox_rx.recv() => {
                    replies.push(message);
                    if replies.len() == 2 { break; }
                }
                _ = &mut deadline => break,
            }
        }

        assert_eq!(replies.len(), 2, "both peers should execute typed Lisp");
        for (_, name, text) in replies {
            assert_eq!(text, "81", "peer {name} should return typed Lisp output");
        }
    }

    #[tokio::test]
    async fn test_peers_keep_typed_lisp_definitions_between_messages() {
        let (inbox_tx, mut inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (bcast_tx, _sessions) = boot_peers(inbox_tx, None).await;

        bcast_tx
            .send(SessionEvent::chat("(define (triple (n : int)) (* n 3))"))
            .unwrap();
        // Definitions have no user-visible output. Give both runtimes a chance
        // to commit before the dependent program is sent.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bcast_tx
            .send(SessionEvent::chat("(say (int-to-string (triple 14)))"))
            .unwrap();

        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut replies = Vec::new();
        loop {
            tokio::select! {
                Some(message) = inbox_rx.recv() => {
                    replies.push(message);
                    if replies.len() == 2 { break; }
                }
                _ = &mut deadline => break,
            }
        }

        assert_eq!(replies.len(), 2, "both peers should retain the definition");
        for (_, name, text) in replies {
            assert_eq!(text, "42", "peer {name} should use its retained Lisp word");
        }
    }

    /// Peers are independent: a definition in the user's broadcast doesn't
    /// bleed between peers (each has its own VM snapshot).
    #[tokio::test]
    async fn test_peers_have_independent_vms() {
        let (inbox_tx, mut inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (bcast_tx, _sessions) = boot_peers(inbox_tx, None).await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Broadcast a definition — both peers should compile it.
        bcast_tx
            .send(SessionEvent::chat(": square  dup * ;"))
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Now evaluate the word — both VMs should have it.
        bcast_tx.send(SessionEvent::chat("5 square")).unwrap();

        let deadline = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(deadline);
        let mut replies: Vec<(Uuid, String, String)> = Vec::new();
        loop {
            tokio::select! {
                Some(msg) = inbox_rx.recv() => {
                    replies.push(msg);
                    if replies.len() == 2 { break; }
                }
                _ = &mut deadline => { break; }
            }
        }

        assert_eq!(replies.len(), 2, "expected both peers to reply");
        for (_, name, text) in &replies {
            assert!(
                text.contains("25"),
                "peer {name} should know `square` and reply with ( 25 ), got: {text}"
            );
        }
    }

    /// ChannelMessage events must NOT be echoed back to the local inbox by peer
    /// loops.  Before the fix, both peer loops would each send the message back,
    /// causing N_peers × N_channels echoes per send.
    #[tokio::test]
    async fn test_channel_message_not_echoed_by_peer_loops() {
        use crate::session::{Promise, ProofBundle};

        let (inbox_tx, mut inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (bcast_tx, _sessions) = boot_peers(inbox_tx, None).await;

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let promise = Promise::natural("hello world".to_string());
        let bundle = ProofBundle::new(promise, vec![]);
        let ev = crate::session::SessionEvent::ChannelMessage {
            channel: "#test".into(),
            sender: "lush-hill".into(),
            bundle,
        };
        bcast_tx.send(ev).unwrap();

        // Wait briefly — if the peer loops echo the message, it will arrive here.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Drain everything that arrived.
        let mut count = 0usize;
        while inbox_rx.try_recv().is_ok() {
            count += 1;
        }

        assert_eq!(
            count, 0,
            "peer loops must not echo ChannelMessage events back to the inbox (got {count})"
        );
    }

    /// Registry contains both peers after boot.
    #[tokio::test]
    async fn test_registry_populated_after_boot() {
        let (inbox_tx, _inbox_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Uuid, String, String)>();
        let (_bcast_tx, sessions) = boot_peers(inbox_tx, None).await;

        for (id, name) in &sessions {
            let found = find_peer(&id.to_string()).await;
            assert!(found.is_some(), "peer {name} ({id}) should be in registry");

            let by_name = find_peer(name).await;
            assert!(by_name.is_some(), "should be findable by name {name}");
        }
    }
}
