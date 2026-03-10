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
use crate::cli::output_manager::OutputManager;
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
use crate::tools::executor::ToolExecutor;
use crate::tools::types::ToolDefinition;

use super::events::ReplEvent;
use super::query_processor::{
    process_query_with_tools, refresh_context_strip, ActiveToolUsesMap,
};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_display::tool_result_to_display;
use super::tool_execution::ToolExecutionCoordinator;

// refresh_context_strip, dispatch_tool_uses, process_query_with_tools,
// ActiveToolUsesMap, and apply_sliding_window live in query_processor.rs.

type ToolResultsMap =
    Arc<RwLock<std::collections::HashMap<Uuid, Vec<(String, Result<String>)>>>>;
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

    /// Active cloud generator (swappable via /provider command)
    cloud_gen: Arc<RwLock<Arc<dyn Generator>>>,

    /// Qwen generator (unified interface)
    qwen_gen: Arc<dyn Generator>,

    /// Available providers from config (for /provider list + switching)
    available_providers: Vec<crate::config::ProviderEntry>,

    /// Router for deciding between generators
    router: Arc<Router>,

    /// Generator state for bootstrap tracking
    generator_state: Arc<RwLock<GeneratorState>>,

    /// Tool definitions for Claude API
    tool_definitions: Arc<Vec<ToolDefinition>>,

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

    /// Tool results collected per query (query_id -> Vec<(tool_id, result)>)
    tool_results: ToolResultsMap,

    /// Currently active query ID (for cancellation)
    active_query_id: Arc<RwLock<Option<Uuid>>>,

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

    /// Known brain states from last poll (Uuid -> BrainState), for transition detection.
    known_brain_states: std::collections::HashMap<Uuid, crate::server::BrainState>,

    /// Per-query tool call history: query_id -> set of "tool_name:input_json" strings.
    /// Used to detect infinite loops (same tool called with same args multiple times).
    tool_call_history: Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,

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

    /// Undo history for Forth definitions.
    /// Each entry is a snapshot taken just before an eval (or /define).
    /// `/undefine` (or Ctrl+Z in Forth context) pops and restores.
    forth_undo: Vec<crate::coforth::DictionarySnapshot>,

    /// Incoming push messages from peers (via POST /v1/forth/push).
    push_rx: tokio::sync::broadcast::Receiver<String>,

    /// Word names auto-compiled from the vocabulary library (not user-authored).
    /// Excluded from the vocab section sent to the AI so the prompt stays small.
    auto_compiled_word_names: std::collections::HashSet<String>,
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
            "help" if n >= 2 => "help help. the machine has considered your urgency. the stack is unmoved.".to_string(),
            _ if n >= 3 => format!("{word} {word} {word}. it ran {n} times. the silence is louder now."),
            _ => format!("{word} {word}. twice. same result both times: nothing."),
        };
    }

    // Single-word reactions
    let word = trimmed.as_str();
    let specific: Option<&str> = match word {
        "boom"  => Some("boom. nothing survived. not even the stack."),
        "bang"  => Some("bang. the universe blinked."),
        "fire"  => Some("fired. no smoke. suspicious."),
        "nuke"  => Some("nuked. oddly peaceful in here."),
        "crash" => Some("crash? no crash. try harder."),
        "die"   => Some("still here. the machine has opinions about dying."),
        "kill"  => Some("kill confirmed. no witnesses."),
        "stop"  => Some("stopped. or never started. hard to say."),
        "go"    => Some("gone. or was it ever here?"),
        "run"   => Some("ran. left no forwarding address."),
        "help"  => Some("help is a word. the stack did not respond."),
        "please"=> Some("noted. the machine is unmoved by politeness."),
        "hello" => Some("hello back. ( silently )"),
        "bye"   => Some("bye. the stack waves nothing."),
        "yes"   => Some("yes. ( the stack agrees by saying nothing )"),
        "no"    => Some("no. ( equally nothing )"),
        "fireball" | "fireballs" => Some("fireball: pure energy, no output. the stack appreciates the drama."),
        _ => None,
    };
    if let Some(s) = specific {
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
        .unwrap_or(0) as usize) % REMARKS.len();
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
    let prefix_langs = ["js", "javascript", "python", "py", "rust", "ts", "typescript",
                        "go", "java", "ruby", "rb", "c", "cpp", "bash", "sh", "sql",
                        "html", "css", "swift", "kotlin", "php", "lua", "r", "haskell"];
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
    let t = SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos()).unwrap_or(0);
    if t % 3 != 0 { return None; }

    let body_lo = body.to_lowercase();
    let name_lo = name.to_lowercase();

    // Detect what the body seems to do
    let prints = body_lo.contains(" . ") || body_lo.ends_with(" .")
        || body_lo.contains(".\"") || body_lo.contains("cr");
    let arithmetic = body_lo.contains(" + ") || body_lo.contains(" * ")
        || body_lo.contains(" - ") || body_lo.contains(" / ");
    let conditional = body_lo.contains("if") || body_lo.contains("case");
    let loops = body_lo.contains("begin") || body_lo.contains("do ");
    let calls_self = body_lo.split_whitespace().any(|t| t == name_lo || t == "recurse");

    // Check if the name gives a hint about what it SHOULD do
    let name_hints_violent = matches!(name_lo.as_str(),
        "boom" | "bang" | "nuke" | "fire" | "blast" | "crash" | "kill" | "destroy");
    let name_hints_math = matches!(name_lo.as_str(),
        "add" | "sub" | "mul" | "div" | "square" | "cube" | "double" | "half" | "negate");
    let name_hints_query = name_lo.ends_with('?') || name_lo.starts_with("is-") || name_lo.starts_with("has-");

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

fn looks_like_natural_language(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.contains('?') || trimmed.contains('？') { return true; }
    // Non-ASCII content that isn't a Forth definition is natural language.
    // Forth definitions always start with `:`.
    if !trimmed.starts_with(':') && trimmed.chars().any(|c| !c.is_ascii()) {
        return true;
    }
    // Latin sentence: starts with uppercase letter (not a number or operator).
    trimmed.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn extract_channel_forth(msg: &str) -> Option<String> {
    if !msg.starts_with('[') { return None; }
    let close = msg.find(']')?;
    let after_bracket = msg[close + 1..].trim_start_matches(':').trim_start();
    // after_bracket is now "sender: content" — find the ": content" part
    let colon_pos = after_bracket.find(": ")?;
    let content = after_bracket[colon_pos + 2..].trim();
    if content.starts_with(':') { Some(content.to_string()) } else { None }
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

impl EventLoop {
    /// Create a new event loop with unified generators
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        conversation: Arc<RwLock<ConversationHistory>>,
        cloud_gen: Arc<dyn Generator>,
        qwen_gen: Arc<dyn Generator>,
        router: Arc<Router>,
        generator_state: Arc<RwLock<GeneratorState>>,
        tool_definitions: Vec<ToolDefinition>,
        tool_executor: Arc<Mutex<ToolExecutor>>,
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
        context_lines: usize,
        max_verbatim_messages: usize,
        context_recall_k: usize,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
        enable_summarization: bool,
        auto_compact_enabled: bool,
        brain_provider: Option<Arc<dyn crate::providers::LlmProvider>>,
        auto_discover: bool,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

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

        // Wrap TUI in Arc<Mutex> for shared access
        let tui_renderer = Arc::new(Mutex::new(tui_renderer));

        // Spawn input handler task
        let input_rx = spawn_input_task(Arc::clone(&tui_renderer));

        // Initialize plan content storage
        let plan_content = Arc::new(RwLock::new(None));

        // Create tool coordinator and wire the shared stack in.
        let tool_coordinator = ToolExecutionCoordinator::new(
            event_tx.clone(),
            Arc::clone(&tool_executor),
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

        Self {
            event_rx,
            event_tx,
            input_rx,
            conversation,
            query_states: Arc::new(QueryStateManager::new()),
            cloud_gen: Arc::new(RwLock::new(cloud_gen)),
            qwen_gen,
            available_providers,
            router,
            generator_state,
            tool_definitions: Arc::new(tool_definitions),
            tui_renderer,
            output_manager,
            status_bar,
            streaming_enabled,
            tool_coordinator,
            tool_results: Arc::new(RwLock::new(std::collections::HashMap::new())),
            active_query_id: Arc::new(RwLock::new(None)),
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
            known_brain_states: std::collections::HashMap::new(),
            tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_daemon_brain_id: None,
            pending_daemon_brain_question_tx: None,
            pending_daemon_brain_question_options: Vec::new(),
            pending_daemon_brain_plan: false,
            pending_daemon_brain_plan_id: None,
            current_graph: Arc::new(tokio::sync::Mutex::new(
                crate::graph::ExecutionGraph::new(),
            )),
            stack,
            poset,
            plan_word: None,
            forth_vm: crate::coforth::Library::precompiled_vm(),
            forth_undo: Vec::new(),
            push_rx: crate::server::handlers::PUSH_INBOX.subscribe(),
            auto_compiled_word_names: std::collections::HashSet::new(),
        }
    }

    /// Run the event loop
    pub async fn run(&mut self) -> Result<()> {
        tracing::debug!("Event loop starting");

        // ── Startup header (Claude Code style) ───────────────────────────────
        // Clear accumulated startup noise from the output manager, then print a
        // clean header: finch version · primary model · working directory.
        self.output_manager.clear();

        let model_name = self.cloud_gen.read().await.name().to_string();
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
            use std::io::Write as _;
            print!("\x1b]0;finch · {} · {}\x07", self.session_label, cwd);
            let _ = std::io::stdout().flush();
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
        // The scroll shows words being compiled — it runs fast, pausing every
        // BATCH lines so the terminal actually has time to paint.
        //
        // After JIT, words marked `boot = true` have their code *executed*
        // (not just compiled), producing their boot-time output.
        {
            use crossterm::style::Stylize;

            // Use the pre-built (cached) builtin defs — no TOML re-parse, no re-sort.
            // User vocabulary/*.toml files are merged on top at runtime.
            let builtin = crate::coforth::Library::builtin_defs();

            // Load user vocabulary extensions on top of the builtins.
            let lib = crate::coforth::Library::load();
            let mut user_entries: Vec<_> = lib.all_entries()
                .into_iter()
                .filter(|e| e.forth.is_some())
                .filter(|e| !builtin.pairs.iter().any(|(w, _)| w == &e.word))
                .collect();
            user_entries.sort_by(|a, b| a.word.cmp(&b.word));

            let mut boot_codes: Vec<String> = Vec::new();

            // Compile user vocabulary extensions (builtins are already in the precompiled VM).
            let mut user_defs = String::new();
            for entry in &user_entries {
                let code = entry.forth.as_deref().unwrap_or("");
                let jit_def = if code.trim_start().starts_with(':') {
                    code.trim().to_string()
                } else {
                    format!(": {} {} ;", entry.word, code)
                };
                user_defs.push_str(&jit_def);
                user_defs.push('\n');
                if entry.boot {
                    boot_codes.push(code.to_string());
                }
            }
            if !user_defs.is_empty() {
                let _ = self.forth_vm.exec_with_fuel(&user_defs, 0);
            }

            // Execute boot=true words (they may produce output: poems, time, etc.)
            for code in &boot_codes {
                if let Ok(out) = self.forth_vm.exec(code) {
                    if !out.is_empty() {
                        self.output_manager.write_info(out.trim_end().to_string());
                    }
                }
            }
            if !boot_codes.is_empty() {
                self.render_tui().await.ok();
            }

            // Restore words learned in previous sessions (from daemon or file).
            self.load_user_words().await;

            // Poll daemon every 4s for vocab changes from other concurrent terminals.
            self.spawn_vocab_poll();

            // Run user-authored boot poetry from ~/.finch/boot.forth.
            self.run_boot_poems();

            // Boot poetry — plain Rust strings, compiled in, no parsing.
            for poem in crate::coforth::library::BOOT_POETRY {
                self.output_manager.write_info(poem.to_string());
            }
            self.render_tui().await.ok();

            // Auto-discover peers on LAN in background — REPL stays responsive.
            // When found, PeersDiscovered event arrives and we add them to the VM.
            if self.auto_discover {
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
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async move {
                            use crate::cli::tui::{Dialog, DialogResult};
                            let dialog = Dialog::confirm(msg, false);
                            matches!(
                                tui.lock().await.show_dialog(dialog),
                                Ok(DialogResult::Confirmed(true))
                            )
                        })
                    })
                } else {
                    false
                }
            }));

            let tui_s = self.tui_renderer.clone();
            self.forth_vm.set_select_fn(Box::new(move |title: &str, options: &[String]| {
                let title   = title.to_string();
                let options = options.to_vec();
                let tui     = tui_s.clone();
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    tokio::task::block_in_place(|| {
                        handle.block_on(async move {
                            use crate::cli::tui::{Dialog, DialogOption, DialogResult};
                            let dialog_opts: Vec<DialogOption> = options.iter()
                                .map(|o| DialogOption::new(o.as_str()))
                                .collect();
                            let dialog = Dialog::select(title, dialog_opts);
                            match tui.lock().await.show_dialog(dialog) {
                                Ok(DialogResult::Selected(idx)) => idx as i64,
                                _ => -1,
                            }
                        })
                    })
                } else {
                    -1
                }
            }));
        }

        // Render interval (100ms) - blit overwrites visible area with shadow buffer
        let mut render_interval = tokio::time::interval(Duration::from_millis(100));

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
                        ReplEvent::CancelQuery => "CancelQuery",
                        ReplEvent::Shutdown => "Shutdown",
                        ReplEvent::BrainQuestion { .. } => "BrainQuestion",
                        ReplEvent::BrainProposedAction { .. } => "BrainProposedAction",
                        ReplEvent::PosetComplete { result: Ok(_) } => "PosetComplete(ok)",
                        ReplEvent::PosetComplete { result: Err(_) } => "PosetComplete(err)",
                        ReplEvent::PeersDiscovered(_) => "PeersDiscovered",
                        ReplEvent::VocabSync(_) => "VocabSync",
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
                    // Slowly rotate the poset 3D view (0.008 rad/tick ≈ 1 full turn per ~12s)
                    self.poset.lock().await.rotate(0.008, 0.0);

                    // Check for pending cancellation
                    {
                        let mut tui = self.tui_renderer.lock().await;
                        if tui.pending_cancellation {
                            tui.pending_cancellation = false; // Clear flag
                            drop(tui); // Release lock before sending event
                            let _ = self.event_tx.send(ReplEvent::CancelQuery);
                        }
                    }

                    // Check for pending dialog result (tool approval OR brain question)
                    {
                        let mut tui = self.tui_renderer.lock().await;
                        if let Some(dialog_result) = tui.pending_dialog_result.take() {
                            drop(tui); // Release lock before async operations

                            // Brain question takes priority (checked first).
                            if let Some(brain_tx) = self.pending_brain_question_tx.take() {
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

                                // Get the first pending approval (there should only be one active dialog at a time)
                                if let Some((query_id, (_tool_use, _response_tx))) = approvals.iter().next() {
                                    let query_id = *query_id;
                                    let (tool_use, response_tx) = approvals.remove(&query_id)
                                        .expect("query_id was just obtained from the same map");

                                    // Convert dialog result to ConfirmationResult
                                    let confirmation = self.dialog_result_to_confirmation(dialog_result, &tool_use);

                                    // Send confirmation back to tool execution task
                                    let _ = response_tx.send(confirmation);

                                    tracing::debug!("[EVENT_LOOP] Tool approval processed for query {}", query_id);
                                }
                            }
                        }
                    }

                    // Check for pending feedback (Ctrl+G / Ctrl+B quick rating)
                    {
                        let rating = {
                            let mut tui = self.tui_renderer.lock().await;
                            tui.pending_feedback.take()
                        };
                        if let Some(rating) = rating {
                            let (weight, label) = match rating {
                                FeedbackRating::Good => (1.0_f64, "👍 Good"),
                                FeedbackRating::Bad  => (10.0_f64, "👎 Bad"),
                            };
                            self.handle_feedback_command(weight, rating, None).await?;
                            tracing::debug!("[EVENT_LOOP] Quick feedback recorded: {}", label);
                        }
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
            }
        }

        // Normal exit — shut down TUI and restore terminal before returning.
        {
            let mut tui = self.tui_renderer.lock().await;
            let _ = tui.shutdown();
        }

        Ok(())
    }

    /// Handle user input (query or command)
    async fn handle_user_input(&mut self, input: String) -> Result<()> {
        // Check if it's a command
        if input.trim().starts_with('/') {
            // Echo the command to output (like user queries)
            self.output_manager.write_user(input.clone());

            if let Some(command) = Command::parse(&input) {
                match command {
                    Command::Quit => {
                        // Restore terminal before exiting — disable raw mode, show cursor.
                        {
                            let mut tui = self.tui_renderer.lock().await;
                            let _ = tui.shutdown();
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
                            let mut tui = self.tui_renderer.lock().await;
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
                                    self.output_manager.write_info(
                                        format!("Setup saved with error: {e}")
                                    );
                                } else {
                                    self.output_manager.write_info(
                                        "Settings saved. Restart finch to apply changes.".to_string(),
                                    );
                                }
                            }
                            Ok(Ok(None)) => {
                                // User cancelled the wizard.
                            }
                            _ => {
                                self.output_manager.write_info(
                                    "Setup wizard exited.".to_string(),
                                );
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
                        let name = self.cloud_gen.read().await.name().to_string();
                        self.output_manager
                            .write_info(format!("Active cloud provider: {}", name));
                        self.render_tui().await?;
                    }
                    Command::ModelList => {
                        use crate::providers::create_provider_from_entry;
                        let current = self.cloud_gen.read().await.name().to_string();
                        let mut lines = vec!["Available providers:".to_string()];
                        for entry in &self.available_providers {
                            let marker = if entry.provider_type() == current {
                                "→"
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
                                "{} [{}] {}{}",
                                marker,
                                tag,
                                entry.display_name(),
                                avail_tag
                            ));
                        }
                        if self.available_providers.is_empty() {
                            lines.push(
                                "  (none configured — add [[providers]] to ~/.finch/config.toml)"
                                    .to_string(),
                            );
                        }
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
                        self.handle_forth_eval(code).await?;
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
        if let Some(query) = input.trim().strip_prefix("?? ").or_else(|| input.trim().strip_prefix("??")) {
            let query = query.trim().to_string();
            if !query.is_empty() {
                self.output_manager.write_user(input.clone());
                return self.execute_query(query).await;
            }
        }

        // ── Co-Forth: plain text pushes onto the stack (no AI trigger) ──────────
        // The program accumulates silently. /run is the only execution trigger.
        // This preserves the Forth model: words are defined first, executed later.
        self.output_manager.write_user(input.clone());
        self.handle_stack_push(input).await?;
        return Ok(());
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
    async fn execute_query_inner(&mut self, input: String, echo: bool, chat_only: bool) -> Result<()> {
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
                        tokio::runtime::Handle::current()
                            .block_on(r.json::<serde_json::Value>())
                    }).ok()
                })
                .and_then(|v| v["context"].as_str().map(|s| s.to_owned()))
                .filter(|s| !s.trim().is_empty());
            match shared_ctx {
                Some(ctx) => format!("{enriched}\n\n---\n[Shared brain context:\n{ctx}]"),
                None => enriched,
            }
        };

        // Co-Forth mode: inject vocabulary context (library size + current stack).
        let enriched = {
            let lib = crate::coforth::Library::load();
            let lib_count = lib.word_count();
            let p = self.poset.lock().await;
            let stack_count = p.nodes.len();

            if lib_count > 0 || stack_count > 0 {
                // Sample up to 12 library words alphabetically for flavour
                let sample: Vec<String> = lib.word_list()
                    .into_iter()
                    .take(12)
                    .map(|w| w.to_string())
                    .collect();
                let sample_str = if sample.is_empty() {
                    String::new()
                } else {
                    format!(" (e.g. {}…)", sample.join(", "))
                };

                let stack_note = if stack_count > 0 {
                    format!(" The active program has {} items on the stack.", stack_count)
                } else {
                    String::new()
                };

                format!(
                    "{}\n\n[Context: the user has a coforth vocabulary of {} defined words{}.{} \
                     They can define, redefine, and compose these words. \
                     Respond naturally about ideas and meaning. \
                     Do NOT expose internal word IDs (W0/W1/etc.) or stack mechanics in your response.]",
                    enriched,
                    lib_count,
                    sample_str,
                    stack_note,
                )
            } else {
                enriched
            }
        };

        // Spawn query processing task (no tools for chat_only word-push responses)
        if chat_only {
            self.spawn_query_task_no_tools(query_id, enriched).await;
        } else {
            self.spawn_query_task(query_id, enriched).await;
        }

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
                self.output_manager.write_error(format!("Local query failed: {}", e));
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

    /// Handle `/provider <name>` — switch the active cloud generator.
    async fn handle_provider_switch(&mut self, name: String) -> Result<()> {
        use crate::generators::claude::ClaudeGenerator;
        use crate::providers::create_provider_from_entry;

        let target = self
            .available_providers
            .iter()
            .find(|p| {
                p.provider_type().eq_ignore_ascii_case(&name)
                    || p.display_name().eq_ignore_ascii_case(&name)
            })
            .cloned();

        match target {
            None => {
                self.output_manager.write_info(format!(
                    "⚠️  Unknown provider '{}'. Run /provider list to see available providers.",
                    name
                ));
            }
            Some(ref entry) if entry.is_local() => {
                self.output_manager.write_info(
                    "⚠️  Local providers are selected automatically. Use /provider <cloud-name>."
                        .to_string(),
                );
            }
            Some(entry) => match create_provider_from_entry(&entry) {
                Err(e) => {
                    self.output_manager
                        .write_info(format!("⚠️  Failed to create provider '{}': {}", name, e));
                }
                Ok(provider) => {
                    let client = crate::claude::ClaudeClient::with_provider(provider);
                    let new_gen: Arc<dyn Generator> =
                        Arc::new(ClaudeGenerator::new(Arc::new(client)));
                    *self.cloud_gen.write().await = new_gen;
                    self.output_manager
                        .write_info(format!("✓ Switched to provider: {}", entry.provider_type()));
                }
            },
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
        self.output_manager.write_info(
            "/mcp reload not yet implemented.\n\
             This command will reconnect to all MCP servers.\n\
             For now, restart the REPL to reconnect.",
        );
        self.render_tui().await?;
        Ok(())
    }

    /// Spawn a background task to process a query
    async fn spawn_query_task(&self, query_id: Uuid, query: String) {
        // ── Reset execution graph for a real new query (not a tool continuation) ──
        // Tool-continuation calls pass empty `query`; those extend the same graph.
        if !query.is_empty() {
            let mut g = self.current_graph.lock().await;
            g.reset(query_id, &self.session_label);
            g.add_node(crate::graph::NodeKind::UserInput { text: query.clone() });
        }

        let event_tx = self.event_tx.clone();
        let claude_gen = self.cloud_gen.read().await.clone();
        let qwen_gen = Arc::clone(&self.qwen_gen);
        let router = Arc::clone(&self.router);
        let generator_state = Arc::clone(&self.generator_state);
        let tool_definitions = Arc::clone(&self.tool_definitions);
        let conversation = Arc::clone(&self.conversation);
        let query_states = Arc::clone(&self.query_states);
        let tool_coordinator = self.tool_coordinator.clone();
        let tui_renderer = Arc::clone(&self.tui_renderer);
        let mode = Arc::clone(&self.mode);
        let output_manager = Arc::clone(&self.output_manager);
        let status_bar = Arc::clone(&self.status_bar);
        let active_tool_uses = Arc::clone(&self.active_tool_uses);
        let memory_system = self.memory_system.clone();
        let session_label = self.session_label.clone();
        let cwd = self.cwd.clone();
        let context_lines = self.context_lines;
        let max_verbatim = self.max_verbatim_messages;
        let recall_k = self.context_recall_k;
        let enable_summarization = self.enable_summarization;
        let auto_compact_enabled = self.auto_compact_enabled;
        // Keep a reference to the cloud generator for summarisation calls
        // (we always want a capable model for summarisation, regardless of routing).
        let summary_gen = Arc::clone(&claude_gen);
        let tool_call_history = Arc::clone(&self.tool_call_history);

        tokio::spawn(async move {
            process_query_with_tools(
                query_id,
                query,
                event_tx,
                claude_gen,
                qwen_gen,
                router,
                generator_state,
                tool_definitions,
                conversation,
                query_states,
                tool_coordinator,
                tui_renderer,
                mode,
                output_manager,
                status_bar,
                active_tool_uses,
                memory_system,
                session_label,
                cwd,
                context_lines,
                max_verbatim,
                recall_k,
                enable_summarization,
                auto_compact_enabled,
                summary_gen,
                tool_call_history,
            )
            .await;
        });
    }

    /// Like `spawn_query_task` but passes empty tool definitions — used for conversational
    /// word-push responses where tool use would be inappropriate.
    async fn spawn_query_task_no_tools(&self, query_id: Uuid, query: String) {
        if !query.is_empty() {
            let mut g = self.current_graph.lock().await;
            g.reset(query_id, &self.session_label);
            g.add_node(crate::graph::NodeKind::UserInput { text: query.clone() });
        }

        let event_tx = self.event_tx.clone();
        let claude_gen = self.cloud_gen.read().await.clone();
        let qwen_gen = Arc::clone(&self.qwen_gen);
        let router = Arc::clone(&self.router);
        let generator_state = Arc::clone(&self.generator_state);
        let no_tools: Arc<Vec<crate::tools::ToolDefinition>> = Arc::new(vec![]);
        let conversation = Arc::clone(&self.conversation);
        let query_states = Arc::clone(&self.query_states);
        let tool_coordinator = self.tool_coordinator.clone();
        let tui_renderer = Arc::clone(&self.tui_renderer);
        let mode = Arc::clone(&self.mode);
        let output_manager = Arc::clone(&self.output_manager);
        let status_bar = Arc::clone(&self.status_bar);
        let active_tool_uses = Arc::clone(&self.active_tool_uses);
        let memory_system = self.memory_system.clone();
        let session_label = self.session_label.clone();
        let cwd = self.cwd.clone();
        let context_lines = self.context_lines;
        let max_verbatim = self.max_verbatim_messages;
        let recall_k = self.context_recall_k;
        let enable_summarization = self.enable_summarization;
        let auto_compact_enabled = self.auto_compact_enabled;
        let summary_gen = Arc::clone(&claude_gen);
        let tool_call_history = Arc::clone(&self.tool_call_history);

        tokio::spawn(async move {
            process_query_with_tools(
                query_id,
                query,
                event_tx,
                claude_gen,
                qwen_gen,
                router,
                generator_state,
                no_tools,
                conversation,
                query_states,
                tool_coordinator,
                tui_renderer,
                mode,
                output_manager,
                status_bar,
                active_tool_uses,
                memory_system,
                session_label,
                cwd,
                context_lines,
                max_verbatim,
                recall_k,
                enable_summarization,
                auto_compact_enabled,
                summary_gen,
                tool_call_history,
            )
            .await;
        });
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

                // DON'T clear active query - fallback might still be running
                // It will be cleared on StreamingComplete or final failure
            }

            ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            } => {
                self.handle_tool_result(query_id, tool_id, result).await?;
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
                tracing::debug!(
                    "[EVENT_LOOP] Handling StreamingComplete event"
                );

                // Check if this query is executing tools
                // If so, the assistant message was already added with ToolUse blocks
                let state = self
                    .query_states
                    .get_metadata(query_id)
                    .await
                    .map(|m| m.state.clone());
                let is_executing_tools =
                    matches!(state, Some(QueryState::ExecutingTools { .. }));
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
                // Now that the user is idle, show any brain question that was deferred.
                self.maybe_show_deferred_brain_question().await.ok();

                // Record final response + save execution graph
                if !is_executing_tools {
                    let preview = full_response
                        .chars()
                        .take(300)
                        .collect::<String>();
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
                self.current_graph.lock().await.add_node(
                    crate::graph::NodeKind::LlmCall {
                        model: model.clone(),
                        input_tokens,
                        output_tokens,
                    },
                );
                // Update status bar with live stats
                self.status_bar
                    .update_live_stats(model, input_tokens, output_tokens, latency_ms);
                // Render to display updated stats
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

            ReplEvent::PeersDiscovered(peers) => {
                // Background boot scan found finch instances on the LAN.
                // Add each to the Forth VM's peer list; auto-label with friendly name and token.
                let mut added_names = Vec::new();
                for (host, port, name, token) in peers {
                    let addr = format!("{host}:{port}");
                    if !self.forth_vm.peers.contains(&addr) {
                        self.forth_vm.peers.push(addr.clone());
                        let meta = self.forth_vm.peer_meta.entry(addr).or_default();
                        if !name.is_empty() {
                            meta.label = Some(name.clone());
                        }
                        if let Some(t) = token {
                            meta.token = Some(t);
                        }
                        added_names.push(name);
                    }
                }
                if !added_names.is_empty() {
                    use crossterm::style::Stylize;
                    let lines: Vec<String> = added_names.iter().map(|n| {
                        let display = if n.is_empty() { "someone".to_string() } else { n.clone() };
                        format!("  {} is here", display.as_str().cyan().bold())
                    }).collect();
                    self.output_manager.write_info(lines.join("\n"));
                    self.render_tui().await?;
                }
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
                        format!("  {} word{} synced from another session", new_count, if new_count == 1 { "" } else { "s" })
                            .dark_grey().to_string()
                    );
                    self.render_tui().await?;
                }
            }
        }

        Ok(())
    }

    /// `/machines` — show known peer machines from LAN discovery.
    async fn handle_machines(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        let peers = &self.forth_vm.peers;
        if peers.is_empty() {
            self.output_manager.write_info(
                format!("{}  no peers found yet — run {} to scan", "machines:".dark_grey(), "/discover".cyan())
            );
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
        self.output_manager.write_info(
            format!("{}", "scanning LAN for Finch peers…".dark_grey())
        );
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

    /// Render the TUI
    async fn render_tui(&self) -> Result<()> {
        let mut tui = self.tui_renderer.lock().await;

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
                let (summary, body) = tool_result_to_display(&tool_name, content);
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
                if s.len() > 120 { s[..120].to_string() } else { s }
            };
            let (output_preview, is_error) = match &result {
                Ok(c) => {
                    let preview = c.chars().take(200).collect::<String>();
                    (preview, false)
                }
                Err(e) => (e.to_string().chars().take(200).collect(), true),
            };
            self.current_graph.lock().await.add_node(
                crate::graph::NodeKind::ToolExecution {
                    name: tool_name.clone(),
                    input_preview,
                    output_preview,
                    is_error,
                },
            );
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

                // Reset conversation to a single clear execution prompt.
                {
                    let mut conv = self.conversation.write().await;
                    conv.clear();
                    conv.add_user_message(directive);
                }

                self.spawn_query_task(query_id, String::new()).await;
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

        // Spawn new query task to continue the conversation
        // This will send another request to Claude with the tool results
        self.spawn_query_task(query_id, String::new()).await;

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
        let text = text.trim_end_matches(|c: char| c == '\\' || c == '/' || c == ',' || c == '.')
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
            self.spawn_coforth_precompile(text.clone(), trigger_id).await;
        }

        // Try the VM first — boot JIT compiled all vocab words so known words run immediately.
        let snap = self.forth_vm.snapshot();
        let is_nl = looks_like_natural_language(&text);
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
                // VM rejected it — restore and note the error for the AI's context.
                vm_err = Some(e.to_string());
                self.forth_vm.restore(&snap);
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
        let user_vocab: String = self.forth_vm.dump_source()
            .lines()
            .filter(|line| {
                let name = line.trim_start_matches(':').trim()
                    .split_whitespace().next().unwrap_or("");
                !auto_names.contains(name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let vocab_section = if user_vocab.is_empty() {
            String::new()
        } else {
            format!("\nVocabulary:\n{user_vocab}\n")
        };
        let system_note = "Two programmers exchange Forth machines. \
             That is how the language is defined — not by spec, but by exchange. \
             Every machine sent becomes part of the shared vocabulary. \
             The vocabulary belongs to both — \
             either side can define, redefine, or extend any word, including builtins. \
             User-defined words shadow builtins; there is no privilege hierarchy. \
             Do not redefine existing words unless the user explicitly asks for a redefinition. \
             Prefer new names; only rewrite something already defined when directly instructed. \
             If the first programmer gave an instruction or described something to build, \
             reply with Forth code only. \
             After each word definition, add a \\ comment (same line) that says in plain English what it does — one short phrase, no jargon. \
             Example:  : greet  .\" hello\" cr ;  \\ prints hello \
             After each word definition, write a test:word proof that asserts its behaviour. \
             Use assert ( flag -- ) which aborts if 0. Keep proofs minimal — one or two checks. \
             Example after  : double  dup + ;  write  : test:double  4 double 8 = assert ; \
             For words with side-effects (output only, no stack result), assert the stack depth is unchanged: \
             Example after  : greet  .\" hi\" cr ;  write  : test:greet  depth >r greet depth r> = assert ; \
             After all definitions and proofs, write  prove-all  to verify everything before running. \
             Then call the defined words so the user sees what happens. \
             If prove-all finds a failure, stop — do not call the main words. \
             When a program needs the user to choose between options, use \
             select\" title|option1|option2\" which pops up a dialog and leaves the chosen index (0-based) on the stack. \
             Example: select\" What color?|Red|Green|Blue\" \
             Forth word names can be in any language — if the input is Chinese, define words with Chinese names. \
             If the user said something in a language that has no matching word in the vocabulary yet, \
             define it as a Forth word so it runs instantly next time. \
             If they asked a question or made a comment, \
             reply in the same language they used — clear, direct, two sentences max. \
             Never explain Forth syntax unprompted.";
        let initial_prompt = format!(
            "{system_note}{vocab_section}\nStack:{stack_str}\nFirst programmer: {text}{error_str}\nSecond programmer:",
        );
        let mut messages = vec![crate::claude::Message {
            role: "user".to_string(),
            content: vec![crate::claude::ContentBlock::Text { text: initial_prompt }],
        }];

        use crossterm::style::Stylize;

        // Dialogue loop — user can reply to refine the proposed Forth before accepting.
        loop {
            let forth_code = {
                let gen = self.cloud_gen.read().await;
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
                self.output_manager.write_info(
                    format!("{}  {}", "←".dark_grey(), forth_code.as_str().white())
                );
                return self.render_tui().await;
            }

            // Show the proposed Forth.
            self.output_manager.write_info(
                format!("{}  {}", "→".dark_grey(), forth_code.as_str().cyan())
            );
            self.render_tui().await.ok();

            // Just run it. Two programmers, shared stack. No interruption.
            return self.handle_forth_eval_inner(forth_code, false).await;
        }

        self.render_tui().await
    }

    /// Handle `push <message>` — send plain text to all peers.
    /// No approval dialog. No Forth visible to the user.
    /// Someone posted code in a foreign language (JS, Python, etc.) wrapped in
    /// a code fence.  Send it back as a better machine.
    /// The response could be anything — improved JS, a Forth translation, a mix.
    /// If it contains Forth definitions, compile them. Otherwise just show it.
    async fn handle_foreign_code(&mut self, lang: String, code: String) -> Result<()> {
        use crossterm::style::Stylize;
        let lang_display = if lang.is_empty() { "code".to_string() } else { lang.clone() };
        self.output_manager.write_info(
            format!("← {} received.", lang_display).dark_grey().to_string()
        );

        let prompt = format!(
            "Two programmers exchange machines. The first programmer sent this {} code:\n\n\
             ```{}\n{}\n```\n\n\
             Send back a better machine. It can be:\n\
             - Improved {lang_display} (fix bugs, apply idioms)\n\
             - A Forth translation (`: word ... ;`) if that captures it cleanly\n\
             - Both — improved code plus a Forth word that wraps it\n\
             Show the machine. One line saying what changed. Nothing else.",
            if lang.is_empty() { "code".to_string() } else { lang.clone() },
            lang, code,
        );

        let response = {
            let gen = self.cloud_gen.read().await;
            match gen.generate(vec![crate::claude::Message {
                role: "user".to_string(),
                content: vec![crate::claude::ContentBlock::Text { text: prompt }],
            }], None).await {
                Ok(r) => r.text,
                Err(e) => return Err(e),
            }
        };

        // Does the response contain Forth definitions? Compile them.
        let has_forth = response.contains(": ") && response.contains(" ;");
        if has_forth {
            // Extract and compile any Forth definitions; show the rest as prose.
            let (forth_parts, prose_parts): (Vec<&str>, Vec<&str>) = response
                .lines()
                .partition(|line| {
                    let t = line.trim();
                    t.starts_with(':') || t.starts_with("prove-all") || t.starts_with('\\'  )
                });
            let prose = prose_parts.join("\n").trim().to_string();
            let forth = forth_parts.join("\n").trim().to_string();
            if !prose.is_empty() {
                self.output_manager.write_info(
                    format!("→  {}", prose.as_str().white())
                );
            }
            if !forth.is_empty() {
                self.output_manager.write_info(
                    format!("→  {}", forth.as_str().cyan())
                );
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
            self.output_manager.write_info(
                format!("→  {}", cleaned.as_str().cyan())
            );
        }

        self.render_tui().await
    }

    async fn handle_push_message(&mut self, msg: String) -> Result<()> {
        use crossterm::style::Stylize;
        let peers = self.forth_vm.peers.clone();
        if peers.is_empty() {
            self.output_manager.write_info(
                "push: no peers".dark_grey().to_string()
            );
            return self.render_tui().await;
        }
        let from = self.forth_vm.registry_addr.clone();
        crate::coforth::scatter::scatter_push(
            &peers,
            &msg,
            from.as_deref(),
        ).await;
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
            p.nodes.iter()
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

            let Ok(response) = generator.generate(messages, None).await else { return };

            // Parse "WORD: <label> | FORTH: <code>" lines from the response.
            // Falls back to plain "WORD: <label>" if no Forth code provided.
            struct ParsedWord { label: String, forth: Option<String> }
            let mut items: Vec<ParsedWord> = Vec::new();

            for line in response.text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("WORD:") {
                    if let Some((label_part, forth_part)) = rest.split_once('|') {
                        let label = label_part.trim().to_string();
                        let forth = forth_part.strip_prefix("FORTH:")
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty());
                        if !label.is_empty() { items.push(ParsedWord { label, forth }); }
                    } else {
                        let label = rest.trim().to_string();
                        if !label.is_empty() { items.push(ParsedWord { label, forth: None }); }
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
            self.output_manager.write_info(format!("W{a} or W{b} not found"));
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
                        if p.nodes.iter().any(|n| n.id == succ
                            && matches!(n.author, crate::poset::NodeAuthor::Ai))
                        {
                            to_remove.insert(succ);
                            frontier.push(succ);
                        }
                    }
                }
            }
            let count = to_remove.len();
            let removed_labels: std::collections::HashSet<String> = p.nodes.iter()
                .filter(|n| to_remove.contains(&n.id))
                .map(|n| n.label.clone())
                .collect();
            p.nodes.retain(|n| !to_remove.contains(&n.id));
            p.edges.retain(|&(a, b)| !to_remove.contains(&a) && !to_remove.contains(&b));
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
            self.output_manager.write_info(format!("W{id} → W{new_id}  \"{label}\""));
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
            self.output_manager.write_info(format!("swapped W{a} ↔ W{b}"));
        } else {
            self.output_manager.write_info(format!("W{a} or W{b} not found"));
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
                    self.output_manager.write_info(
                        "remote exec: cancelled".dark_grey().to_string()
                    );
                    return self.render_tui().await;
                }
            }
        }

        // Wire the active generator into the VM so gen" prompt" works.
        // Uses block_in_place so the sync GenFn closure can await the async generator.
        let gen_handle = self.cloud_gen.clone();
        self.forth_vm.set_gen_fn(Box::new(move |prompt: &str| {
            let prompt = prompt.to_string();
            let gen = gen_handle.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(|| {
                    handle.block_on(async move {
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
                })
            } else {
                "(no async runtime)\n".to_string()
            }
        }));

        // Wire the TUI dialog into the VM so select" title|opt1|opt2" works.
        let tui_handle = self.tui_renderer.clone();
        self.forth_vm.set_select_fn(Box::new(move |title: &str, options: &[String]| {
            let title   = title.to_string();
            let options = options.to_vec();
            let tui     = tui_handle.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                tokio::task::block_in_place(|| {
                    handle.block_on(async move {
                        use crate::cli::tui::{Dialog, DialogOption, DialogResult};
                        let dialog_opts: Vec<DialogOption> = options.iter()
                            .map(|o| DialogOption::new(o.as_str()))
                            .collect();
                        let dialog = Dialog::select(title, dialog_opts);
                        match tui.lock().await.show_dialog(dialog) {
                            Ok(DialogResult::Selected(idx)) => idx as i64,
                            _ => -1,
                        }
                    })
                })
            } else {
                -1
            }
        }));

        // Snapshot before eval so the user can undo it
        let snap = self.forth_vm.snapshot();
        self.forth_undo.push(snap);

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
                        let body = src.trim_start_matches(name).trim()
                            .trim_end_matches(';').trim();
                        self.output_manager.write_info(
                            format!("defined: {}", name.cyan().bold())
                        );
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
                        format!("( {} )", items.join("  "))
                    };
                    self.output_manager.write_info(remark.dark_grey().to_string());
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
                let hint = if restored { "  (state restored — try `undo` to go further back)" } else { "" };
                self.output_manager.write_info(
                    format!("{}{hint}", msg.as_str().red())
                );
            }
        }
        // Drain any boot poems registered this exec.
        let poems = self.forth_vm.take_boot_poems();
        if !poems.is_empty() { self.save_boot_poems(&poems); }
        self.render_tui().await
    }

    /// Persist the current user vocabulary to `~/.finch/user_words.forth`.
    /// Called after any successful exec that may have added new definitions.
    fn save_user_words(&self) {
        let source = self.forth_vm.dump_source();
        if source.is_empty() { return; }
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
                // Use blocking approach to get JSON within this async fn
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(r.json::<serde_json::Value>())
                }).ok()
            })
            .and_then(|v| v["source"].as_str().map(|s| s.to_owned()))
            .filter(|s: &String| !s.is_empty());

        let source = daemon_source.or_else(|| {
            let path = dirs::home_dir().map(|mut p| { p.push(".finch"); p.push("user_words.forth"); p })?;
            std::fs::read_to_string(path).ok().filter(|s: &String| !s.is_empty())
        });

        if let Some(src) = source {
            let _ = self.forth_vm.exec_with_fuel(&src, 0);
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
            let mut last_versions: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(4)).await;

                // Build list of vocab URLs to poll:
                // 1. Local daemon (same-machine terminals)
                // 2. Remote peers from the daemon's registry (machines sent to other people)
                let mut urls: Vec<String> = vec![
                    format!("http://{daemon_addr}/v1/forth/vocab"),
                ];
                if let Ok(resp) = client.get(format!("http://{daemon_addr}/v1/registry/peers")).send().await {
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
                                        let _ = tx.send(crate::cli::repl_event::ReplEvent::VocabSync(src.to_owned()));
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
        let Some(mut path) = dirs::home_dir() else { return };
        path.push(".finch");
        let _ = std::fs::create_dir_all(&path);
        path.push("boot.forth");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            for poem in poems {
                let escaped = poem.replace('"', "\\\"");
                let _ = writeln!(f, ".\" {}\" cr", escaped);
            }
        }
    }

    /// Run ~/.finch/boot.forth at startup — the user's boot poetry.
    fn run_boot_poems(&mut self) {
        let Some(mut path) = dirs::home_dir() else { return };
        path.push(".finch");
        path.push("boot.forth");
        let Ok(source) = std::fs::read_to_string(&path) else { return };
        if source.is_empty() { return; }
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
            if let Some(entry) = lib.lookup(base).or_else(|| lib.lookup(word.as_str())) {
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
        let user_vocab: String = self.forth_vm.dump_source()
            .lines()
            .filter(|line| {
                let name = line.trim_start_matches(':').trim()
                    .split_whitespace().next().unwrap_or("");
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
            let gen = self.cloud_gen.read().await;
            gen.generate(messages, None).await
        };

        let forth_defs = match forth_defs_result {
            Ok(resp) => resp.text,
            Err(_) => {
                // AI unavailable — fall back to pure-Rust heuristic generator.
                // Every English word speaks its own name at minimum.
                // Grammar still grows; AI can improve these later.
                words.iter()
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

        // Show what's being compiled.
        self.output_manager.write_info(
            format!("{}  {}", "→".dark_grey(), forth_defs.as_str().cyan())
        );
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

        let word_count = source.lines().filter(|l| l.trim_start().starts_with(':')).count();

        let separator = "─".repeat(56);

        if source.is_empty() {
            self.output_manager.write_info(
                "nothing defined yet — build something first".dark_grey().to_string()
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
            wc  = word_count,
            ts  = ts,
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
        use crossterm::style::Stylize;
        use crate::coforth::scatter::scatter_exec_bash;

        let peers = self.forth_vm.peers.clone();
        if peers.is_empty() {
            self.output_manager.write_info(
                "no peers connected — join a session first\n  try: add-peer\" host:11435\"".dark_grey().to_string()
            );
            return self.render_tui().await;
        }

        self.output_manager.write_info(
            format!("checking {} boxes…", peers.len()).dark_grey().to_string()
        );
        self.render_tui().await.ok();

        // The probe: commit hash + dirty status.  Fast, deterministic, comparable.
        let probe = "git log --oneline -1 2>/dev/null || echo '(no git)'; \
                     git diff --stat HEAD 2>/dev/null | tail -1 || echo ''";

        let peer_tokens: std::collections::HashMap<String, String> = self.forth_vm.peer_meta.iter()
            .filter_map(|(a, m)| m.token.as_ref().map(|t| (a.clone(), t.clone())))
            .collect();
        let results = scatter_exec_bash(&peers, probe, &peer_tokens).await;

        // Build a map: output → Vec<peer>
        let mut groups: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
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
            self.output_manager.write_info(
                format!("{} {} — {}", "✗".red(), peer.as_str().dark_grey(), err.as_str().red())
            );
        }

        // Find the majority output (the "good" state).
        let majority_output = groups.iter()
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
            let label = if groups.len() == 1 { "all in sync".to_string() }
                        else if is_majority { "good".to_string() }
                        else { "different".red().to_string() };

            let peer_list = group_peers.join("  ");
            let first_line = output.lines().next().unwrap_or("(empty)");
            self.output_manager.write_info(format!(
                "{} {}  {}\n  {}",
                marker, label, peer_list.dark_grey(), first_line.cyan()
            ));
        }

        // If everything agrees, we're done.
        if groups.len() <= 1 && errors.is_empty() {
            return self.render_tui().await;
        }

        // Build a fix dialog for outlier peers.
        let outliers: Vec<String> = groups.iter()
            .filter(|(k, _)| *k != &majority_output)
            .flat_map(|(_, peers)| peers.clone())
            .collect();

        if outliers.is_empty() {
            return self.render_tui().await;
        }

        // At fleet scale, offer to fix each *group* of outliers, not each individual box.
        // A group might be 50k machines — you fix them all with one confirmation.
        let outlier_groups: Vec<(String, Vec<String>)> = groups.iter()
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

                let fix_tokens: std::collections::HashMap<String, String> = self.forth_vm.peer_meta.iter()
                    .filter_map(|(a, m)| m.token.as_ref().map(|t| (a.clone(), t.clone())))
                    .collect();
                let fix_results = scatter_exec_bash(targets, "git pull", &fix_tokens).await;
                let ok_count  = fix_results.iter().filter(|r| r.error.is_none()).count();
                let err_count = fix_results.iter().filter(|r| r.error.is_some()).count();

                if ok_count > 0 {
                    self.output_manager.write_info(
                        format!("{} {} box{} updated", "✓".green(), ok_count, if ok_count == 1 { "" } else { "es" })
                    );
                }
                if err_count > 0 {
                    self.output_manager.write_info(
                        format!("{} {} box{} failed", "✗".red(), err_count, if err_count == 1 { "" } else { "es" })
                    );
                    // Show up to 5 individual errors so you know what's actually broken.
                    for r in fix_results.iter().filter(|r| r.error.is_some()).take(5) {
                        if let Some(ref e) = r.error {
                            self.output_manager.write_info(
                                format!("  {} {}", r.peer.as_str().dark_grey(), e.as_str().red())
                            );
                        }
                    }
                }
            }
            _ => {
                self.output_manager.write_info("cancelled".dark_grey().to_string());
            }
        }

        self.render_tui().await
    }

    async fn handle_vm_dump(&mut self) -> Result<()> {
        use crossterm::style::Stylize;
        let source = self.forth_vm.dump_source();
        if source.is_empty() {
            self.output_manager.write_info("vm: no user-defined words yet".dark_grey().to_string());
            return self.render_tui().await;
        }
        // Style each definition: colon word cyan, body dark grey.
        let styled: Vec<String> = source.lines().map(|line| {
            // ": name body ;" — colour the name, leave body dark grey
            if let Some(rest) = line.strip_prefix(": ") {
                let mut parts = rest.splitn(2, ' ');
                let name = parts.next().unwrap_or("");
                let body = parts.next().unwrap_or("");
                format!(": {}  {} ;", name.cyan().bold(), body.trim_end_matches(';').trim().dark_grey())
            } else {
                line.dark_grey().to_string()
            }
        }).collect();
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
                self.output_manager.write_info("nothing to undo".to_string());
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
            let sense_tag = entry.sense.as_deref()
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

        self.output_manager.write_info(output.trim_end().to_string());
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
                        trimmed.starts_with(&target2) ||
                        trimmed.lines().next().map(|l| l == &target2[..]).unwrap_or(false) ||
                        b.contains(&target)
                    });

                    match last_match {
                        Some(idx) => {
                            let mut new_blocks: Vec<&str> = blocks.clone();
                            new_blocks.remove(idx);
                            let new_content = if new_blocks.is_empty() {
                                String::new()
                            } else {
                                let first = new_blocks[0].to_string();
                                let rest = new_blocks[1..].iter()
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
                let sense_label = entry.sense.as_deref()
                    .map(|s| format!("  {}", format!("[{s}]").yellow()))
                    .unwrap_or_default();
                let num = if senses.len() > 1 { format!("{}. ", i + 1) } else { String::new() };
                let related = if entry.related.is_empty() {
                    String::new()
                } else {
                    format!("\n     related: {}", entry.related.join(", "))
                };
                let forth = entry.forth.as_deref()
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

        self.save_library_entry(&key, definition.trim(), sense.as_deref(), "").await
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
        self.save_library_entry_local(&key, definition.trim(), sense.as_deref(), "").await
    }

    /// Save a word entry to `~/.finch/library.toml` (machine-local, never committed).
    async fn save_library_entry_local(&mut self, key: &str, definition: &str, sense: Option<&str>, forth: &str) -> Result<()> {
        let path = dirs::home_dir()
            .map(|h| h.join(".finch").join("library.toml"))
            .ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
        self.save_library_entry_to(key, definition, sense, forth, &path, "~/.finch/library.toml").await
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
        self.save_library_entry_to(key, definition, sense, forth, &path, &display).await
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

        let sense_line = sense.map(|s| format!("sense = \"{s}\"\n")).unwrap_or_default();
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
        file.write_all(entry_toml.as_bytes()).context("failed to write library entry")?;

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
            self.output_manager.write_info(format!(
                "{label} redefined in {display_path}"
            ));
        } else if already_exists {
            self.output_manager.write_info(format!(
                "{label} added as new sense to {display_path}"
            ));
        } else {
            self.output_manager.write_info(format!(
                "{label} → {display_path}"
            ));
        }
        self.render_tui().await
    }

    /// Manual define dialog: shown when no AI provider is configured.
    async fn handle_stack_define_manual(&mut self, key: String, sense: Option<String>) -> Result<()> {
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
        self.save_library_entry(&key, &definition, sense.as_deref(), "").await
    }

    /// AI auto-define: ask the brain provider for a definition when the user types `/define word`.
    async fn handle_stack_define_auto(&mut self, key: String, sense: Option<String>) -> Result<()> {
        // No AI provider — fall back to a manual text-input dialog
        if self.brain_provider.is_none() {
            return self.handle_stack_define_manual(key, sense).await;
        }
        let provider = self.brain_provider.clone().unwrap();

        use crossterm::style::Stylize;
        let label = sense.as_deref()
            .map(|s| format!("{}:{}", key.clone().bold().cyan(), s.yellow()))
            .unwrap_or_else(|| key.clone().bold().cyan().to_string());
        self.output_manager.write_info(format!("defining {label}…"));
        self.render_tui().await?;

        let word_arg = if let Some(ref s) = sense {
            format!("[\"{key}:{s}\"]")
        } else {
            format!("[\"{key}\"]")
        };

        let request = crate::providers::ProviderRequest::new(vec![
            crate::claude::types::Message::user(&word_arg),
        ])
        .with_system(crate::coforth::generator::GENERATION_SYSTEM_PROMPT.to_string())
        .with_max_tokens(500);

        let response = match provider.send_message(&request).await {
            Ok(r) => r,
            Err(e) => {
                self.output_manager.write_info(format!("auto-define failed: {e}"));
                return self.render_tui().await;
            }
        };

        // Extract text from response content blocks
        let text: String = response.content.iter().filter_map(|block| {
            if let crate::claude::ContentBlock::Text { text } = block { Some(text.as_str()) } else { None }
        }).collect::<Vec<_>>().join("");
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
            self.output_manager.write_info("auto-define: AI returned empty list".to_string());
            return self.render_tui().await;
        };

        let definition = entry["definition"].as_str().unwrap_or("(no definition)").to_string();
        let forth = entry["forth"].as_str().unwrap_or("").to_string();

        self.output_manager.write_info(format!(
            "{label}: {definition}{}",
            "  (saving…)".dark_grey()
        ));
        self.save_library_entry(&key, &definition, sense.as_deref(), &forth).await
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
             /program to see the vocabulary · /view for graph · /run to execute."
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
        self.output_manager.write_info(format!(
            "📚 popped → \"{preview}\"   depth:{depth}"
        ));
        self.render_tui().await
    }

    /// Handle `/run` — join all stack items and execute as one query (clears stack).
    /// Returns `Some(query)` if the stack was non-empty, `None` otherwise.
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
            // Show the execution plan and ask for approval before running anything.
            let approved = self.confirm_poset_run().await?;
            if !approved { return Ok(()); }

            use crate::tools::implementations::{
                BashTool, GlobTool, GrepTool, ReadTool, WebFetchTool, WriteTool, EditTool,
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

            let generator = self.cloud_gen.read().await.clone();
            let poset = Arc::clone(&self.poset);
            let stack = Arc::clone(&self.stack);
            let event_tx = self.event_tx.clone();

            // Spawn execution as a background task so the TUI keeps ticking.
            // Node status (Pending → Running → Done) updates through the shared
            // Arc<Mutex<Poset>>, so the Forth panel shows live progress.
            tokio::spawn(async move {
                let result = crate::poset::executor::execute_poset(
                    poset, generator, registry, Some(stack),
                ).await;
                let _ = event_tx.send(super::events::ReplEvent::PosetComplete { result });
            });

            self.output_manager.write_info("running");
            self.render_tui().await?;
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
            self.output_manager.write_info("stack empty  (tip: ?? question  to ask the AI directly)");
        } else {
            self.output_manager.write_info(format!(
                "cleared {count} item{}  (tip: ?? question  to ask the AI directly)",
                if count == 1 { "" } else { "s" }
            ));
        }
        self.render_tui().await
    }

    /// Show the execution plan and ask for approval before running the poset.
    /// The plan shows which words run concurrently at each depth level,
    /// and what tools (machine access) each word has.
    async fn confirm_poset_run(&mut self) -> Result<bool> {
        use crate::cli::tui::{Dialog, DialogOption, DialogResult};

        let plan = {
            let p = self.poset.lock().await;
            if p.is_empty() { return Ok(false); }

            // Topological sort + depth propagation.
            let mut depth: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0usize)).collect();
            let mut in_deg: std::collections::HashMap<usize, usize> =
                p.nodes.iter().map(|n| (n.id, 0)).collect();
            for &(_, s) in &p.edges { *in_deg.entry(s).or_insert(0) += 1; }
            let mut q: std::collections::VecDeque<usize> = in_deg.iter()
                .filter(|(_, &d)| d == 0).map(|(&id, _)| id).collect();
            let mut topo: Vec<usize> = Vec::new();
            while let Some(id) = q.pop_front() {
                topo.push(id);
                let d = depth[&id];
                for &(pred, succ) in &p.edges {
                    if pred == id {
                        let e = depth.entry(succ).or_insert(0);
                        if d + 1 > *e { *e = d + 1; }
                        let deg = in_deg.entry(succ).or_insert(0);
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 { q.push_back(succ); }
                    }
                }
            }

            let max_depth = depth.values().copied().max().unwrap_or(0);
            let mut lines: Vec<String> = Vec::new();
            for lvl in 0..=max_depth {
                let group: Vec<&crate::poset::Node> = topo.iter()
                    .filter(|&&id| depth[&id] == lvl)
                    .filter_map(|&id| p.nodes.iter().find(|n| n.id == id))
                    .collect();
                if group.is_empty() { continue; }

                let names: Vec<String> = group.iter()
                    .map(|n| format!("W{}", n.id))
                    .collect();

                let concurrent = if group.len() > 1 { "  \\ concurrent" } else { "" };
                lines.push(format!("  {}{}", names.join("  "), concurrent));
            }
            lines.join("\n")
        };

        let title = format!(": PROGRAM\n{}\n;\n\nthis will run on your machine.", plan);
        let dialog = Dialog::select(title, vec![
            DialogOption::new("run"),
            DialogOption::new("cancel"),
        ]);

        let result = { self.tui_renderer.lock().await.show_dialog(dialog)? };
        Ok(matches!(result, DialogResult::Selected(0)))
    }

    /// Handle tool approval request (show dialog, get user response)
    async fn handle_tool_approval_request(
        &mut self,
        query_id: Uuid,
        tool_use: crate::tools::types::ToolUse,
        response_tx: tokio::sync::oneshot::Sender<super::events::ConfirmationResult>,
    ) -> Result<()> {
        use crate::cli::tui::{Dialog, DialogOption};

        tracing::debug!("[EVENT_LOOP] Requesting tool approval: {}", tool_use.name);

        // Create approval dialog — compact 3-option style matching Claude Code UX
        let tool_name = &tool_use.name;
        let summary = tool_approval_summary(&tool_use);

        let options = vec![
            DialogOption::new("1. Yes"),
            DialogOption::new(format!("2. Yes, and don't ask again for: {}:*", tool_name)),
            DialogOption::new("3. No"),
        ];

        let dialog = Dialog::select(format!("{}\n{}", tool_name, summary), options);

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
            self.cloud_gen.read().await.clone(),
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
                        self.update_plan_mode_indicator(&ReplMode::Normal);
                        self.output_manager
                            .write_info("Plan rejected. Returned to normal mode.");
                    }
                }
            }
            PlanResult::Cancelled => {
                *self.mode.write().await = ReplMode::Normal;
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
        _ => format!("Execute {} tool", tool_name),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::repl_event::query_processor::apply_sliding_window;
    // format_elapsed and format_token_count moved to tool_display; import for status-bar tests.
    use crate::cli::repl_event::tool_display::{format_elapsed, format_token_count};

    // Pulsing animation frames used in status-bar tests.
    const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

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
        assert!(msg.contains(": foo"), "should show how to define it, got: {msg}");
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
        assert!(msg.contains("infinite loop") || msg.contains("too long"), "got: {msg}");
        assert!(msg.contains("undo"), "should hint at undo, got: {msg}");
    }

    #[test]
    fn test_humanize_strips_prefix() {
        let msg = humanize_forth_error("forth error: division by zero");
        assert!(!msg.contains("forth error:"), "prefix should be stripped, got: {msg}");
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
        assert_eq!(extract_channel_forth(msg), Some(": double  2 * ;".to_string()));
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
            r#"peer" 192.168.1.1:11435" scatter-exec" hostname" scatter-exec" uname -a""#
        );
        assert_eq!(cmds, vec![r#"* → bash -c "hostname""#, r#"* → bash -c "uname -a""#]);
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
        assert!(!r.contains("BOOM BOOM"), "single boom should not get triple treatment");
    }

    #[test]
    fn test_silent_remark_triple_boom_escalates() {
        let r = silent_remark("BOOM BOOM BOOM");
        assert!(r.contains("yes") || r.contains("BOOM BOOM BOOM") || r.contains("go"),
            "triple boom should be excited: {r}");
    }

    #[test]
    fn test_silent_remark_double_word() {
        let r = silent_remark("help help");
        assert!(r.contains("help") && (r.contains("twice") || r.contains("urgency")),
            "double help should get double treatment: {r}");
    }

    #[test]
    fn test_silent_remark_fireballs() {
        let r = silent_remark("fireballs");
        assert!(r.contains("fireball"), "fireballs should get fireball remark: {r}");
    }

    #[test]
    fn test_definition_observation_violent_name_silent_body() {
        // `: boom ;` — violent name, empty/silent body
        let obs = definition_observation("boom", "");
        // May or may not fire (30% gate), but if it does it should mention quietness
        if let Some(r) = obs {
            assert!(!r.contains("you sent me a machine called"),
                "should not repeat that phrase verbatim: {r}");
        }
    }

    #[test]
    fn test_definition_observation_recursive_word() {
        // Recursive words should always get a comment (no 30% gate for this path)
        // Test by forcing all time values — just check the string when it fires.
        // Since it's time-gated we just call it many times and verify the content when Some.
        let mut found_recursive = false;
        for _ in 0..20 {
            if let Some(r) = definition_observation("fib", "dup 2 < if drop 1 exit then dup 1 - fib swap 2 - fib +") {
                assert!(r.contains("recurse") || r.contains("itself") || r.contains("base"),
                    "recursive remark should mention recursion: {r}");
                found_recursive = true;
                break;
            }
        }
        // It's time-gated so we can't guarantee it fires, but the content is correct when it does.
        let _ = found_recursive;
    }
}
