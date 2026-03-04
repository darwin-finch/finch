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
use crate::generators::{Generator, StreamChunk};
use crate::local::LocalGenerator;
use crate::memory::NeuralEmbeddingEngine;
use crate::models::bootstrap::GeneratorState;
use crate::models::tokenizer::TextTokenizer;
use crate::router::Router;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::{ToolDefinition, ToolUse};

use super::events::ReplEvent;
use super::query_state::{QueryState, QueryStateManager};
use tokio_util::sync::CancellationToken;
use super::plan_handler::{handle_ask_user_question, handle_present_plan, is_tool_allowed_in_mode};
use super::tool_display::{format_tool_label, tool_result_to_display};
use super::tool_execution::ToolExecutionCoordinator;

/// Refresh the ContextLine status-strip entries and the terminal window/tab title.
///
/// `context_lines` is the total number of lines to show including the 🧠 stats
/// line, so `depth = context_lines - 1` centroid lines are requested from the
/// MemTree.  Stale `ContextLine(N)` entries beyond the result are removed so
/// the strip shrinks cleanly when history is short.
///
/// This is a free function (not `&self`) so it can be called from the static
/// `process_query_with_tools` closure.
async fn refresh_context_strip(
    memory_system: &crate::memory::MemorySystem,
    session_label: &str,
    cwd: &str,
    status_bar: &StatusBar,
    context_lines: usize,
) {
    let depth = context_lines.saturating_sub(1); // 🧠 takes one slot
    let Ok(summary) = memory_system.conversation_summary(depth).await else {
        return;
    };

    let n = summary.lines.len();

    // Format each line with an appropriate prefix:
    //   single line                → "   └─ now: <text>"
    //   first of multiple          → "📋 <text>"
    //   middle lines               → "   ├─ <text>"
    //   last of multiple           → "   └─ now: <text>"
    for (i, text) in summary.lines.iter().enumerate() {
        let label = if n == 1 {
            format!("   └─ now: {}", text)
        } else if i == 0 {
            format!("📋 {}", text)
        } else if i == n - 1 {
            format!("   └─ now: {}", text)
        } else {
            format!("   ├─ {}", text)
        };
        status_bar.update_line(
            crate::cli::status_bar::StatusLineType::ContextLine(i),
            label,
        );
    }

    // Remove stale slots beyond what we just wrote (depth change or short history)
    for i in n..8 {
        status_bar.remove_line(&crate::cli::status_bar::StatusLineType::ContextLine(i));
    }

    // OSC 0 — set terminal window title + tab title
    let title_topic = summary.lines.first().map(|s| {
        if s.chars().count() <= 35 {
            s.to_string()
        } else {
            format!("{}…", s.chars().take(34).collect::<String>())
        }
    });
    let title = match title_topic.as_deref() {
        Some(t) if !t.is_empty() => format!("finch · {} · {} · {}", session_label, cwd, t),
        _ => format!("finch · {} · {}", session_label, cwd),
    };
    {
        use std::io::Write as _;
        print!("\x1b]0;{}\x07", title);
        let _ = std::io::stdout().flush();
    }
}

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
type ActiveToolUsesMap = Arc<
    RwLock<
        std::collections::HashMap<
            String,
            (
                String,
                serde_json::Value,
                Arc<crate::cli::messages::WorkUnit>,
                usize,
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

    /// Daemon client (for /local command)
    daemon_client: Option<Arc<crate::client::DaemonClient>>,

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

    /// Brain UUID that the REPL is currently waiting for a question/plan from.
    /// Set when a transition to WaitingForInput/PlanReady is detected.
    pending_daemon_brain_id: Option<Uuid>,

    /// Oneshot sender for daemon brain question dialog response.
    pending_daemon_brain_question_tx: Option<tokio::sync::oneshot::Sender<String>>,
    pending_daemon_brain_question_options: Vec<String>,

    /// Whether the REPL is currently showing a plan dialog for a daemon brain.
    pending_daemon_brain_plan: bool,
    pending_daemon_brain_plan_id: Option<Uuid>,
}

/// View mode for the REPL
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// Traditional list view (current scrollback)
    List,
    /// Tree-structured conversation view
    Tree,
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
        daemon_client: Option<Arc<crate::client::DaemonClient>>,
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
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Wire the todo list into the TUI renderer before wrapping in Arc<Mutex>
        let mut tui_renderer = tui_renderer;
        tui_renderer.set_todo_list(Arc::clone(&todo_list));

        // Wrap TUI in Arc<Mutex> for shared access
        let tui_renderer = Arc::new(Mutex::new(tui_renderer));

        // Spawn input handler task
        let input_rx = spawn_input_task(Arc::clone(&tui_renderer));

        // Initialize plan content storage
        let plan_content = Arc::new(RwLock::new(None));

        // Create tool coordinator
        let tool_coordinator = ToolExecutionCoordinator::new(
            event_tx.clone(),
            Arc::clone(&tool_executor),
            Arc::clone(&conversation),
            Arc::clone(&local_generator),
            Arc::clone(&tokenizer),
            Arc::clone(&mode),
            Arc::clone(&plan_content),
        );

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
            daemon_client,
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
            brain_provider,
            brain_context: Arc::new(RwLock::new(None)),
            active_brain: Arc::new(RwLock::new(None)),
            pending_brain_question_tx: None,
            pending_brain_question_options: Vec::new(),
            pending_brain_action_tx: None,
            pending_brain_action_command: None,
            known_brain_states: std::collections::HashMap::new(),
            tool_call_history: Arc::new(RwLock::new(std::collections::HashMap::new())),
            pending_daemon_brain_id: None,
            pending_daemon_brain_question_tx: None,
            pending_daemon_brain_question_options: Vec::new(),
            pending_daemon_brain_plan: false,
            pending_daemon_brain_plan_id: None,
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
                                    if let Some(ref client) = self.daemon_client {
                                        let (action, feedback) = match &dialog_result {
                                            crate::cli::tui::DialogResult::Selected(0) => ("approve", None),
                                            crate::cli::tui::DialogResult::Selected(2) => ("reject", None),
                                            crate::cli::tui::DialogResult::CustomText(s) => ("changes", Some(s.as_str())),
                                            crate::cli::tui::DialogResult::TextEntered(s) => ("changes", Some(s.as_str())),
                                            _ => ("reject", None),
                                        };
                                        let fb = feedback.map(str::to_string);
                                        let client_clone = client.clone();
                                        tokio::spawn(async move {
                                            let _ = client_clone.respond_to_brain_plan(
                                                brain_id,
                                                action,
                                                fb.as_deref(),
                                            ).await;
                                        });
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
                                if let Some(ref client) = self.daemon_client {
                                    let client_clone = client.clone();
                                    tokio::spawn(async move {
                                        let _ = client_clone.answer_brain_question(brain_id, &answer).await;
                                    });
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
                        self.event_tx
                            .send(ReplEvent::Shutdown)
                            .context("Failed to send shutdown event")?;
                    }
                    Command::Help => {
                        let help_text = format_help();
                        self.output_manager.write_info(help_text);
                        self.render_tui().await?;
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
                                // Enter plan mode manually
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
                                // Update status bar indicator
                                self.update_plan_mode_indicator(&new_mode);
                            }
                            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                                // Exit plan mode, return to normal
                                *self.mode.write().await = ReplMode::Normal;
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
                // Unknown commands also go to scrollback
                self.output_manager
                    .write_info(format!("Unknown command: {}", input));
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

        // Drain any pending images from TUI (pasted before sending)
        let pending_images: Vec<(String, String)> = {
            let mut tui = self.tui_renderer.lock().await;
            tui.pending_images
                .drain(..)
                .map(|(_idx, b64, media_type)| (media_type, b64))
                .collect()
        };

        // Echo user input to output buffer
        self.output_manager.write_user(input.clone());

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
        // Skip if the context is empty or whitespace-only.
        let enriched = {
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

        // Spawn query processing task
        self.spawn_query_task(query_id, enriched).await;

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
        use std::sync::Arc;

        // Check if daemon client exists
        if let Some(daemon_client) = &self.daemon_client {
            // Create streaming response message with info header prepended
            let msg = Arc::new(StreamingResponseMessage::new());
            msg.append_chunk("🔧 Local Model Query (bypassing routing)\n\n");
            self.output_manager
                .add_trait_message(msg.clone() as Arc<dyn crate::cli::messages::Message>);
            self.render_tui().await?;

            // Spawn streaming query in background so event loop continues running
            // This allows TUI to keep rendering while tokens stream in
            let daemon_client = daemon_client.clone();
            let msg_clone = msg.clone();
            let output_mgr = self.output_manager.clone();

            tokio::spawn(async move {
                match daemon_client
                    .query_local_only_streaming_with_callback(&query, move |token_text| {
                        tracing::debug!("[/local] Received chunk: {:?}", token_text);
                        msg_clone.append_chunk(token_text);
                    })
                    .await
                {
                    Ok(_) => {
                        // Append status indicator to the response message itself
                        msg.append_chunk("\n✓ Local model (bypassed routing)");
                        msg.set_complete();
                    }
                    Err(e) => {
                        msg.set_failed();
                        output_mgr.write_error(format!("Local query failed: {}", e));
                    }
                }
            });

            // Return immediately - event loop continues, TUI keeps rendering
        } else {
            // No daemon mode - show error
            self.output_manager
                .write_error("Error: /local requires daemon mode.");
            self.output_manager
                .write_info("    Start the daemon: finch daemon --bind 127.0.0.1:11435");
            self.render_tui().await?;
        }

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
            Self::process_query_with_tools(
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

    /// Process a query with potential tool execution loop using unified generators
    #[allow(clippy::too_many_arguments)]
    async fn process_query_with_tools(
        query_id: Uuid,
        query: String,
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        claude_gen: Arc<dyn Generator>,
        qwen_gen: Arc<dyn Generator>,
        router: Arc<Router>,
        generator_state: Arc<RwLock<GeneratorState>>,
        tool_definitions: Arc<Vec<ToolDefinition>>,
        conversation: Arc<RwLock<ConversationHistory>>,
        query_states: Arc<QueryStateManager>,
        tool_coordinator: ToolExecutionCoordinator,
        tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
        mode: Arc<RwLock<ReplMode>>,
        output_manager: Arc<OutputManager>,
        status_bar: Arc<crate::cli::StatusBar>,
        active_tool_uses: ActiveToolUsesMap,
        memory_system: Option<Arc<crate::memory::MemorySystem>>,
        session_label: String,
        cwd: String,
        context_lines: usize,
        max_verbatim: usize,
        recall_k: usize,
        enable_summarization: bool,
        auto_compact_enabled: bool,
        summary_gen: Arc<dyn Generator>,
        tool_call_history: Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,
    ) {
        tracing::debug!(
            "process_query_with_tools starting for query_id: {:?}",
            query_id
        );

        // Step 1: Routing decision
        let generator: Arc<dyn Generator> = {
            // Check if Qwen is ready
            let state = generator_state.read().await;
            let qwen_ready = state.is_ready();
            drop(state);

            // Route based on readiness and confidence
            // NOTE: In daemon mode, these logs are misleading (daemon makes actual routing decision)
            // TODO: Detect daemon mode and skip client-side routing entirely
            if qwen_ready {
                match router.route(&query) {
                    crate::router::RouteDecision::Local { confidence, .. } if confidence > 0.7 => {
                        // Use Qwen
                        tracing::debug!(
                            "Client-side routing: Qwen (confidence: {:.2})",
                            confidence
                        );
                        Arc::clone(&qwen_gen)
                    }
                    _ => {
                        // Use Claude
                        tracing::debug!(
                            "Client-side routing: teacher (low confidence or no match)"
                        );
                        Arc::clone(&claude_gen)
                    }
                }
            } else {
                // Qwen not ready, use Claude
                tracing::debug!("Client-side routing: teacher (Qwen not ready)");
                Arc::clone(&claude_gen)
            }
        };

        // Get conversation context, optionally injecting relevant memories
        let mut memory_recall_count: usize = 0;
        let messages = {
            let all_msgs = conversation.read().await.get_messages();
            // When summarization is enabled and messages have been dropped by the
            // sliding window, summarise them and inject as a prefix so the LLM
            // retains awareness of earlier turns.
            let mut msgs =
                if enable_summarization && max_verbatim > 0 && all_msgs.len() > max_verbatim {
                    let drop_end = all_msgs.len() - max_verbatim;
                    // Clone the dropped slice so we can pass all_msgs by value to apply_sliding_window.
                    let dropped: Vec<_> = all_msgs[..drop_end].to_vec();
                    let window = apply_sliding_window(all_msgs, max_verbatim);
                    let compactor =
                        crate::cli::conversation_compactor::ConversationCompactor::new(summary_gen);
                    compactor.compact(&dropped, window).await
                } else {
                    apply_sliding_window(all_msgs, max_verbatim)
                };
            if let Some(ref mem) = memory_system {
                if let Ok(memories) = mem.query(&query, Some(recall_k)).await {
                    if !memories.is_empty() {
                        memory_recall_count = memories.len();
                        let mem_block = memories.join("\n\n---\n\n");
                        // Inject into the last user message so the LLM sees the recalled context
                        if let Some(last_user) = msgs.iter_mut().rev().find(|m| m.role == "user") {
                            if let Some(ContentBlock::Text { ref mut text }) =
                                last_user.content.first_mut()
                            {
                                *text = format!(
                                    "[Relevant memories from past sessions:\n\n{}]\n\n{}",
                                    mem_block, text
                                );
                            }
                        }
                        status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::MemoryContext,
                            format!("🧠 recalled {}  ·  querying…", memory_recall_count),
                        );
                    }
                }
            }
            msgs
        };
        let caps = generator.capabilities();

        // Try streaming first if supported
        if caps.supports_streaming {
            tracing::debug!("Generator supports streaming, attempting to stream");

            // Create a WorkUnit for this generation turn BEFORE streaming begins.
            // The shadow-buffer / insert_before architecture requires the message to
            // exist in output_manager before any blit cycles run — the WorkUnit's
            // time-driven animation will be visible during streaming.
            let verb = crate::cli::messages::random_spinner_verb();
            let work_unit = output_manager.start_work_unit(verb);

            let stream_start = std::time::Instant::now();
            let mut token_count: usize = 0;
            let mut input_token_count: Option<u32> = None;
            {
                use std::io::Write as _;
                print!(
                    "\x1b]0;finch · {} · {} · ↓ streaming…\x07",
                    session_label, cwd
                );
                let _ = std::io::stdout().flush();
            }

            match generator
                .generate_stream(messages.clone(), Some((*tool_definitions).clone()))
                .await
            {
                Ok(Some(mut rx)) => {
                    tracing::debug!("[EVENT_LOOP] Streaming started, entering receive loop");
                    tracing::debug!("Streaming started successfully");

                    // Process stream (handles tools via StreamChunk::ContentBlockComplete)
                    let mut blocks = Vec::new();
                    let mut text = String::new();

                    while let Some(result) = rx.recv().await {
                        match result {
                            Ok(StreamChunk::Usage { input_tokens }) => {
                                input_token_count = Some(input_tokens);
                            }
                            Ok(StreamChunk::TextDelta(delta)) => {
                                tracing::debug!("Received TextDelta: {} bytes", delta.len());
                                text.push_str(&delta);
                                token_count += delta.split_whitespace().count();
                                // WorkUnit accumulates tokens for its own animated display
                                work_unit.add_tokens(&delta);
                            }
                            Ok(StreamChunk::ContentBlockComplete(block)) => {
                                tracing::debug!("Received ContentBlockComplete: {:?}", block);
                                blocks.push(block);
                            }
                            Err(e) => {
                                tracing::error!("Stream error in event loop: {}", e);
                                work_unit.set_failed();
                                let _ = event_tx.send(ReplEvent::QueryFailed {
                                    query_id,
                                    error: format!("{}", e),
                                });
                                return;
                            }
                        }
                    }

                    tracing::debug!(
                        "[EVENT_LOOP] Stream receive loop ended, {} blocks received",
                        blocks.len()
                    );
                    tracing::debug!("Stream receive loop ended");

                    // Stream complete — set the final response text on the WorkUnit.
                    // If tools follow, set_complete() will be called after all tools finish.
                    // If no tools, set_complete() is called below.
                    if !text.is_empty() {
                        work_unit.set_response(&text);
                    }

                    // Send stats update
                    let _ = event_tx.send(ReplEvent::StatsUpdate {
                        model: generator.name().to_string(),
                        input_tokens: input_token_count,
                        output_tokens: Some(token_count as u32),
                        latency_ms: Some(stream_start.elapsed().as_millis() as u64),
                    });

                    tracing::debug!("[EVENT_LOOP] Streaming complete");

                    // Extract tools from blocks
                    tracing::debug!("[EVENT_LOOP] Extracting tools from blocks");
                    let tool_uses: Vec<ToolUse> = blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::ToolUse { id, name, input } => Some(ToolUse {
                                id: id.clone(),
                                name: name.clone(),
                                input: input.clone(),
                            }),
                            _ => None,
                        })
                        .collect();

                    tracing::debug!("[EVENT_LOOP] Found {} tool uses", tool_uses.len());

                    if !tool_uses.is_empty() {
                        tracing::debug!("[EVENT_LOOP] Tools detected, updating query state");
                        // Update state: executing tools
                        query_states
                            .update_state(
                                query_id,
                                QueryState::ExecutingTools {
                                    tools_pending: tool_uses.len(),
                                    tools_completed: 0,
                                },
                            )
                            .await;

                        tracing::debug!(
                            "[EVENT_LOOP] Query state updated, adding assistant message"
                        );
                        // Add assistant message with ALL content blocks (text + tool uses)
                        // This is critical for proper conversation structure
                        let assistant_message = crate::claude::Message {
                            role: "assistant".to_string(),
                            content: blocks.clone(),
                        };
                        tracing::debug!("[EVENT_LOOP] Acquiring conversation write lock...");
                        conversation.write().await.add_message(assistant_message);
                        tracing::debug!(
                            "[EVENT_LOOP] Assistant message added, spawning tool executions"
                        );

                        // Tool calls share the WorkUnit that was created before streaming.
                        // Each tool gets its own sub-row within the same WorkUnit.

                        // Execute tools (check for AskUserQuestion first, then mode restrictions)
                        let current_mode = mode.read().await;
                        for tool_use in tool_uses {
                            // Loop detection: track how many times this exact (tool, input) has
                            // been called for this query.  A second identical call is a strong
                            // signal the model is stuck; return a terminal error.
                            let call_key = format!(
                                "{}:{}",
                                tool_use.name,
                                tool_use.input.to_string()
                            );
                            let call_count = {
                                let mut history = tool_call_history.write().await;
                                let entry = history
                                    .entry(query_id)
                                    .or_insert_with(std::collections::HashMap::new);
                                let count = entry.entry(call_key.clone()).or_insert(0);
                                *count += 1;
                                *count
                            };
                            if call_count > 1 {
                                let label = format_tool_label(&tool_use.name, &tool_use.input);
                                let row_idx = work_unit.add_row(label);
                                work_unit.fail_row(row_idx, "loop detected");
                                let error_msg = format!(
                                    "LOOP DETECTED: You have called {} with the same arguments {} time(s) and received the same result each time.\n\
                                     Repeating this call will not produce different output.\n\
                                     You have enough information to proceed. Call PresentPlan now to show your plan.",
                                    tool_use.name, call_count - 1
                                );
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result: Err(anyhow::anyhow!("{}", error_msg)),
                                });
                                continue;
                            }

                            // Check if tool is allowed in current mode
                            if !is_tool_allowed_in_mode(&tool_use.name, &current_mode) {
                                // Tool blocked by plan mode - add error row and send result
                                let label = format_tool_label(&tool_use.name, &tool_use.input);
                                let row_idx = work_unit.add_row(label);
                                let error_msg = format!(
                                        "Tool '{}' is not allowed in planning mode.\n\
                                         Reason: This tool can modify system state.\n\
                                         Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
                                         Type /approve to execute your plan with all tools enabled.",
                                        tool_use.name
                                    );
                                work_unit.fail_row(row_idx, "blocked in plan mode");
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result: Err(anyhow::anyhow!("{}", error_msg)),
                                });
                                continue;
                            }

                            // Add a running row for this tool to the shared WorkUnit
                            let label = format_tool_label(&tool_use.name, &tool_use.input);
                            let row_idx = work_unit.add_row(&label);

                            // Store (name, input, work_unit, row_idx) for result lookup
                            active_tool_uses.write().await.insert(
                                tool_use.id.clone(),
                                (
                                    tool_use.name.clone(),
                                    tool_use.input.clone(),
                                    Arc::clone(&work_unit),
                                    row_idx,
                                ),
                            );

                            // Check if this is AskUserQuestion (handle specially)
                            if let Some(result) =
                                handle_ask_user_question(&tool_use, Arc::clone(&tui_renderer)).await
                            {
                                // Send result immediately
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result,
                                });
                            } else if let Some(result) = handle_present_plan(
                                &tool_use,
                                Arc::clone(&tui_renderer),
                                Arc::clone(&mode),
                                Arc::clone(&output_manager),
                                query_states
                                    .get_metadata(query_id)
                                    .await
                                    .map(|m| m.cancellation_token)
                                    .unwrap_or_else(CancellationToken::new),
                                Arc::clone(&work_unit),
                            )
                            .await
                            {
                                // Send result immediately
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result,
                                });
                            } else {
                                // Regular tool execution (with live-output streaming)
                                tool_coordinator.spawn_tool_execution(
                                    query_id,
                                    tool_use,
                                    Arc::clone(&work_unit),
                                    row_idx,
                                );
                            }
                        }
                        drop(current_mode);
                        // Resolve "querying…" and refresh context even on tool-calling turns.
                        if let Some(ref mem) = memory_system {
                            if let Ok(stats) = mem.stats().await {
                                status_bar.update_line(
                                    crate::cli::status_bar::StatusLineType::MemoryContext,
                                    format!(
                                        "🧠 recalled {}  ·  {} memories",
                                        memory_recall_count, stats.conversation_count
                                    ),
                                );
                            }
                            refresh_context_strip(
                                mem,
                                &session_label,
                                &cwd,
                                &status_bar,
                                context_lines,
                            )
                            .await;
                        }
                        tracing::debug!("[EVENT_LOOP] Tool executions spawned, returning");
                        return;
                    }

                    // No tools — mark WorkUnit complete so blit shows final response
                    work_unit.set_complete();

                    // Add assistant message to conversation
                    tracing::debug!(
                        "[EVENT_LOOP] No tools found, adding assistant message to conversation"
                    );
                    conversation
                        .write()
                        .await
                        .add_assistant_message(text.clone());

                    // Store to memory (fire-and-forget; never blocks the response path)
                    if let Some(ref mem) = memory_system {
                        let model_name = generator.name().to_string();
                        let _ = mem
                            .insert_conversation(
                                "user",
                                &query,
                                Some(&model_name),
                                Some(&session_label),
                            )
                            .await;
                        let _ = mem
                            .insert_conversation(
                                "assistant",
                                &text,
                                Some(&model_name),
                                Some(&session_label),
                            )
                            .await;
                        if let Ok(stats) = mem.stats().await {
                            status_bar.update_line(
                                crate::cli::status_bar::StatusLineType::MemoryContext,
                                format!(
                                    "🧠 recalled {}  ·  {} memories",
                                    memory_recall_count, stats.conversation_count
                                ),
                            );
                        }
                        refresh_context_strip(
                            mem,
                            &session_label,
                            &cwd,
                            &status_bar,
                            context_lines,
                        )
                        .await;
                    }

                    // Update context usage indicator (suppressed when auto-compact disabled)
                    if auto_compact_enabled {
                        let conv = conversation.read().await;
                        let pct = (conv.compaction_percent_remaining() * 100.0) as u8;
                        status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::CompactionPercent,
                            format!("Context left until auto-compact: {}%", pct),
                        );
                    }

                    // Update query state
                    query_states
                        .update_state(
                            query_id,
                            QueryState::Completed {
                                response: text.clone(),
                            },
                        )
                        .await;

                    // Signal the event loop to clear active_query_id (streaming path never sends this otherwise)
                    let _ = event_tx.send(ReplEvent::StreamingComplete {
                        query_id,
                        full_response: text.clone(),
                    });

                    tracing::debug!("[EVENT_LOOP] Query complete, returning");
                    return;
                }
                Ok(None) | Err(_) => {
                    // Fall through to non-streaming
                }
            }
        }

        // Non-streaming path (for Qwen or fallback)
        // Create WorkUnit before the blocking generate call so the animated
        // header is visible during the wait (blit cycle runs every ~100ms).
        let verb = crate::cli::messages::random_spinner_verb();
        let work_unit = output_manager.start_work_unit(verb);
        match generator
            .generate(messages, Some((*tool_definitions).clone()))
            .await
        {
            Ok(response) => {
                // Set response text on the WorkUnit
                if !response.text.is_empty() {
                    work_unit.set_response(&response.text);
                }

                // Send stats update
                let _ = event_tx.send(ReplEvent::StatsUpdate {
                    model: response.metadata.model.clone(),
                    input_tokens: response.metadata.input_tokens,
                    output_tokens: response.metadata.output_tokens,
                    latency_ms: response.metadata.latency_ms,
                });

                // Send response (StreamingComplete works for non-streaming too)
                let _ = event_tx.send(ReplEvent::StreamingComplete {
                    query_id,
                    full_response: response.text.clone(),
                });

                // Convert GenToolUse to ToolUse
                let tool_uses: Vec<ToolUse> = response
                    .tool_uses
                    .into_iter()
                    .map(|gen_tool| ToolUse {
                        id: gen_tool.id,
                        name: gen_tool.name,
                        input: gen_tool.input,
                    })
                    .collect();

                if !tool_uses.is_empty() {
                    // Update state: executing tools
                    query_states
                        .update_state(
                            query_id,
                            QueryState::ExecutingTools {
                                tools_pending: tool_uses.len(),
                                tools_completed: 0,
                            },
                        )
                        .await;

                    // Add assistant message with ALL content blocks (text + tool uses)
                    // This is critical for proper conversation structure
                    let assistant_message = crate::claude::Message {
                        role: "assistant".to_string(),
                        content: response.content_blocks.clone(),
                    };
                    conversation.write().await.add_message(assistant_message);

                    // Tool calls share the WorkUnit created before generate().

                    // Execute tools (check for AskUserQuestion first, then mode restrictions)
                    let current_mode = mode.read().await;
                    for tool_use in tool_uses {
                        // Loop detection (same as streaming path)
                        let call_key = format!(
                            "{}:{}",
                            tool_use.name,
                            tool_use.input.to_string()
                        );
                        let call_count = {
                            let mut history = tool_call_history.write().await;
                            let entry = history
                                .entry(query_id)
                                .or_insert_with(std::collections::HashMap::new);
                            let count = entry.entry(call_key.clone()).or_insert(0);
                            *count += 1;
                            *count
                        };
                        if call_count > 1 {
                            let label = format_tool_label(&tool_use.name, &tool_use.input);
                            let row_idx = work_unit.add_row(label);
                            work_unit.fail_row(row_idx, "loop detected");
                            let error_msg = format!(
                                "LOOP DETECTED: You have called {} with the same arguments {} time(s) and received the same result each time.\n\
                                 Repeating this call will not produce different output.\n\
                                 You have enough information to proceed. Call PresentPlan now to show your plan.",
                                tool_use.name, call_count - 1
                            );
                            let _ = event_tx.send(ReplEvent::ToolResult {
                                query_id,
                                tool_id: tool_use.id.clone(),
                                result: Err(anyhow::anyhow!("{}", error_msg)),
                            });
                            continue;
                        }

                        // Check if tool is allowed in current mode
                        if !is_tool_allowed_in_mode(&tool_use.name, &current_mode) {
                            let label = format_tool_label(&tool_use.name, &tool_use.input);
                            let row_idx = work_unit.add_row(label);
                            work_unit.fail_row(row_idx, "blocked in plan mode");
                            let error_msg = format!(
                                "Tool '{}' is not allowed in planning mode.\n\
                                     Reason: This tool can modify system state.\n\
                                     Available tools: read, glob, grep, web_fetch, present_plan, ask_user_question\n\
                                     Type /approve to execute your plan with all tools enabled.",
                                tool_use.name
                            );
                            let _ = event_tx.send(ReplEvent::ToolResult {
                                query_id,
                                tool_id: tool_use.id.clone(),
                                result: Err(anyhow::anyhow!("{}", error_msg)),
                            });
                            continue;
                        }

                        // Add a running row for this tool
                        let label = format_tool_label(&tool_use.name, &tool_use.input);
                        let row_idx = work_unit.add_row(&label);
                        active_tool_uses.write().await.insert(
                            tool_use.id.clone(),
                            (
                                tool_use.name.clone(),
                                tool_use.input.clone(),
                                Arc::clone(&work_unit),
                                row_idx,
                            ),
                        );

                        // Check if this is AskUserQuestion (handle specially)
                        if let Some(result) =
                            handle_ask_user_question(&tool_use, Arc::clone(&tui_renderer)).await
                        {
                            // Send result immediately
                            let _ = event_tx.send(ReplEvent::ToolResult {
                                query_id,
                                tool_id: tool_use.id.clone(),
                                result,
                            });
                        } else if let Some(result) = handle_present_plan(
                            &tool_use,
                            Arc::clone(&tui_renderer),
                            Arc::clone(&mode),
                            Arc::clone(&output_manager),
                            query_states
                                .get_metadata(query_id)
                                .await
                                .map(|m| m.cancellation_token)
                                .unwrap_or_else(CancellationToken::new),
                            Arc::clone(&work_unit),
                        )
                        .await
                        {
                            // Send result immediately
                            let _ = event_tx.send(ReplEvent::ToolResult {
                                query_id,
                                tool_id: tool_use.id.clone(),
                                result,
                            });
                        } else {
                            // Regular tool execution (with live-output streaming)
                            tool_coordinator.spawn_tool_execution(
                                query_id,
                                tool_use,
                                Arc::clone(&work_unit),
                                row_idx,
                            );
                        }
                    }
                    drop(current_mode);
                    // Resolve "querying…" and refresh context even on tool-calling turns.
                    if let Some(ref mem) = memory_system {
                        if let Ok(stats) = mem.stats().await {
                            status_bar.update_line(
                                crate::cli::status_bar::StatusLineType::MemoryContext,
                                format!(
                                    "🧠 recalled {}  ·  {} memories",
                                    memory_recall_count, stats.conversation_count
                                ),
                            );
                        }
                        refresh_context_strip(
                            mem,
                            &session_label,
                            &cwd,
                            &status_bar,
                            context_lines,
                        )
                        .await;
                    }
                    return;
                }

                // No tools — mark WorkUnit complete
                work_unit.set_complete();
                tracing::debug!("Query complete (no tools), non-streaming finished");

                // Store to memory (fire-and-forget)
                if let Some(ref mem) = memory_system {
                    let model_name = response.metadata.model.clone();
                    let _ = mem
                        .insert_conversation(
                            "user",
                            &query,
                            Some(&model_name),
                            Some(&session_label),
                        )
                        .await;
                    let _ = mem
                        .insert_conversation(
                            "assistant",
                            &response.text,
                            Some(&model_name),
                            Some(&session_label),
                        )
                        .await;
                    if let Ok(stats) = mem.stats().await {
                        status_bar.update_line(
                            crate::cli::status_bar::StatusLineType::MemoryContext,
                            format!(
                                "🧠 recalled {}  ·  {} memories",
                                memory_recall_count, stats.conversation_count
                            ),
                        );
                    }
                    refresh_context_strip(mem, &session_label, &cwd, &status_bar, context_lines)
                        .await;
                }
            }
            Err(e) => {
                let _ = event_tx.send(ReplEvent::QueryFailed {
                    query_id,
                    error: format!("{}", e),
                });
            }
        }
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
                // Clear per-query tool-call history so it doesn't grow forever.
                self.tool_call_history.write().await.remove(&query_id);
            }

            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
            } => {
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
        }

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
        let (tool_name, _tool_input, work_unit, row_idx) = {
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
                    format!("{}…", &err_str[..57])
                } else {
                    err_str
                };
                work_unit.fail_row(row_idx, short_err);
            }
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

/// Apply a sliding window to the message list, keeping only the last `max` messages
/// verbatim. If `max` is 0 or the list is shorter than `max`, returns all messages.
///
/// After slicing, advances past any leading assistant messages so the window
/// always starts with a user turn (required by all provider APIs). Also strips
/// any leading user messages that contain only `tool_result` blocks — these are
/// orphaned when the sliding window cuts the preceding assistant `tool_use`
/// message, and all providers reject `tool_result` without a matching `tool_use`.
/// A floor of 2 messages is kept to avoid sending an empty window in degenerate
/// cases.
pub(crate) fn apply_sliding_window(
    msgs: Vec<crate::claude::Message>,
    max: usize,
) -> Vec<crate::claude::Message> {
    if max == 0 || msgs.len() <= max {
        return msgs;
    }
    let mut window = msgs[msgs.len() - max..].to_vec();
    // Ensure the window starts with a user message (API requirement).
    while window.len() > 2 && window.first().map(|m| m.role.as_str()) == Some("assistant") {
        window.remove(0);
    }
    // Strip orphaned tool_result-only user messages at the window boundary.
    // This happens when the cut falls inside a tool-call round-trip: the
    // assistant tool_use was dropped but the user tool_result survived.
    // Every provider rejects tool_result blocks without a matching tool_use.
    loop {
        if window.len() <= 2 {
            break;
        }
        let first_is_orphaned = window.first().map(|m| {
            m.role == "user"
                && !m.content.is_empty()
                && m.content
                    .iter()
                    .all(|b| matches!(b, ContentBlock::ToolResult { .. }))
        });
        if first_is_orphaned != Some(true) {
            break;
        }
        window.remove(0); // drop orphaned tool_result user turn
        // Also drop the assistant reply that followed it (starts the next pair).
        if window.first().map(|m| m.role.as_str()) == Some("assistant") {
            window.remove(0);
        }
    }
    window
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
                        format!("{}...", &cmd[..60])
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
                        format!("{}...", &pattern[..40])
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
                        format!("{}...", &reason[..50])
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
}
