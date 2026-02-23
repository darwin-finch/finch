// Event loop for concurrent REPL - handles user input, queries, and rendering simultaneously

use anyhow::{Context, Result};
use chrono::Utc;
use crossterm::style::Stylize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::cli::commands::{Command, format_help};
use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::{spawn_input_task, TuiRenderer};
use crate::claude::ContentBlock;
use crate::feedback::{FeedbackEntry, FeedbackLogger, FeedbackRating};
use crate::generators::{Generator, StreamChunk};
use crate::local::LocalGenerator;
use crate::models::bootstrap::GeneratorState;
use crate::models::tokenizer::TextTokenizer;
use crate::router::Router;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::{ToolDefinition, ToolUse};

use super::events::ReplEvent;
use super::query_state::{QueryState, QueryStateManager};
use super::tool_display::format_tool_label;
use super::tool_execution::ToolExecutionCoordinator;

/// Main event loop for concurrent REPL
#[allow(dead_code)]
pub struct EventLoop {
    /// Channel for receiving events
    event_rx: mpsc::UnboundedReceiver<ReplEvent>,
    /// Channel for sending events
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Channel for receiving user input
    input_rx: mpsc::UnboundedReceiver<String>,

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
    tool_results: Arc<RwLock<std::collections::HashMap<Uuid, Vec<(String, Result<String>)>>>>,

    /// Currently active query ID (for cancellation)
    active_query_id: Arc<RwLock<Option<Uuid>>>,

    /// Pending tool approval requests (query_id -> (tool_use, response_tx))
    pending_approvals: Arc<RwLock<std::collections::HashMap<Uuid, (crate::tools::types::ToolUse, tokio::sync::oneshot::Sender<super::events::ConfirmationResult>)>>>,

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
    active_tool_uses: Arc<RwLock<std::collections::HashMap<String, (String, serde_json::Value, Arc<crate::cli::messages::WorkUnit>, usize)>>>,

    /// Feedback logger — writes rated responses to ~/.finch/feedback.jsonl
    feedback_logger: Option<FeedbackLogger>,

    /// Metrics logger — reads from ~/.finch/metrics/ for /metrics command
    metrics_logger: Option<crate::metrics::MetricsLogger>,
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
        memory_tree: Option<Arc<RwLock<crate::memory::MemTree>>>,
        available_providers: Vec<crate::config::ProviderEntry>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

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

        // Initialize memtree console if memory tree is provided
        let (memtree_console, memtree_handler) = if let Some(tree) = memory_tree {
            let console = crate::cli::memtree_console::MemTreeConsole::new(tree);
            let handler = crate::cli::memtree_console::EventHandler::new();
            (
                Arc::new(RwLock::new(console)),
                Arc::new(tokio::sync::Mutex::new(handler)),
            )
        } else {
            // Create a dummy tree if no memory system is available
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

        {
            let mut tui = self.tui_renderer.lock().await;
            if let Err(e) = tui.print_startup_header(&model_name, &cwd) {
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
                    let suppress_until = cfg.license.notice_suppress_until
                        .as_deref()
                        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
                    let should_show = suppress_until.map_or(true, |d| today > d);
                    if should_show {
                        self.output_manager.write_info(
                            "Using Finch commercially? $10/yr supports development.\n  \
                             Purchase: https://polar.sh/darwin-finch\n  \
                             Activate: finch license activate --key <key>"
                        );
                        let new_date = (today + chrono::Duration::days(7))
                            .format("%Y-%m-%d").to_string();
                        cfg.license.notice_suppress_until = Some(new_date);
                        let _ = cfg.save(); // non-fatal if save fails
                    }
                }
            }
        }

        // Initialize compaction status display
        self.update_compaction_status().await;

        // Initialize plan mode indicator (starts in Normal mode)
        self.update_plan_mode_indicator(&crate::cli::repl::ReplMode::Normal);

        // Render interval (100ms) - blit overwrites visible area with shadow buffer
        let mut render_interval = tokio::time::interval(Duration::from_millis(100));

        // Cleanup interval (30 seconds)
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        // Flag to control the loop
        let mut should_exit = false;

        while !should_exit {
            tokio::select! {
                // User input event
                Some(input) = self.input_rx.recv() => {
                    tracing::debug!("Received input: {}", input);
                    self.handle_user_input(input).await?;
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

                    // Check for pending dialog result (tool approval)
                    {
                        let mut tui = self.tui_renderer.lock().await;
                        if let Some(dialog_result) = tui.pending_dialog_result.take() {
                            drop(tui); // Release lock before async operations

                            // Find which query this dialog was for
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
                            Err(e) => self.output_manager.write_info(
                                format!("⚠️  Failed to read training stats: {}", e)
                            ),
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
                                let plan_path = std::env::temp_dir().join(format!("plan_{}.md", uuid::Uuid::new_v4()));
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
                                     Use /plan to exit plan mode."
                                );
                                // Update status bar indicator
                                self.update_plan_mode_indicator(&new_mode);
                            }
                            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                                // Exit plan mode, return to normal
                                *self.mode.write().await = ReplMode::Normal;
                                self.output_manager.write_info(
                                    "✅ Exited plan mode. Returned to normal mode."
                                );
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
                        self.handle_feedback_command(10.0, FeedbackRating::Bad, note).await?;
                    }
                    Command::FeedbackMedium(note) => {
                        self.handle_feedback_command(3.0, FeedbackRating::Bad, note).await?;
                    }
                    Command::FeedbackGood(note) => {
                        self.handle_feedback_command(1.0, FeedbackRating::Good, note).await?;
                    }
                    Command::ModelShow => {
                        let name = self.cloud_gen.read().await.name().to_string();
                        self.output_manager.write_info(format!("Active cloud provider: {}", name));
                        self.render_tui().await?;
                    }
                    Command::ModelList => {
                        use crate::providers::create_provider_from_entry;
                        let current = self.cloud_gen.read().await.name().to_string();
                        let mut lines = vec!["Available providers:".to_string()];
                        for entry in &self.available_providers {
                            let marker = if entry.provider_type() == current { "→" } else { " " };
                            let tag = if entry.is_local() { "local" } else { "cloud" };
                            // Show availability: cloud entries are available if we can build a provider
                            let available = !entry.is_local() && create_provider_from_entry(entry).is_ok();
                            let avail_tag = if entry.is_local() || available { "" } else { " (no API key)" };
                            lines.push(format!("{} [{}] {}{}", marker, tag, entry.display_name(), avail_tag));
                        }
                        if self.available_providers.is_empty() {
                            lines.push("  (none configured — add [[providers]] to ~/.finch/config.toml)".to_string());
                        }
                        self.output_manager.write_info(lines.join("\n"));
                        self.render_tui().await?;
                    }
                    Command::ModelSwitch(name) => {
                        self.handle_provider_switch(name).await?;
                    }
                    Command::LicenseStatus => {
                        use crate::config::{load_config, LicenseType};
                        let cfg = load_config().unwrap_or_else(|_| crate::config::Config::new(vec![]));
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
                                        verified_at: Some(chrono::Local::now().format("%Y-%m-%d").to_string()),
                                        expires_at: Some(parsed.expires_at.format("%Y-%m-%d").to_string()),
                                        licensee_name: Some(parsed.name.clone()),
                                        notice_suppress_until: None,
                                    };
                                    if let Err(e) = cfg.save() {
                                        self.output_manager.write_info(
                                            format!("✓ License validated but could not save: {}", e)
                                        );
                                    } else {
                                        self.output_manager.write_info(format!(
                                            "✓ License activated\n  Licensee: {} ({})\n  Expires:  {}",
                                            parsed.name, parsed.email, parsed.expires_at.format("%Y-%m-%d")
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                self.output_manager.write_info(format!("✗ License activation failed: {}", e));
                            }
                        }
                        self.render_tui().await?;
                    }
                    Command::LicenseRemove => {
                        use crate::config::{load_config, LicenseConfig};
                        if let Ok(mut cfg) = load_config() {
                            cfg.license = LicenseConfig::default();
                            if let Err(e) = cfg.save() {
                                self.output_manager.write_info(format!("⚠️  Could not save config: {}", e));
                            } else {
                                self.output_manager.write_info("✓ License removed. Now using noncommercial license.");
                            }
                        }
                        self.render_tui().await?;
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
        if input.trim().eq_ignore_ascii_case("quit")
            || input.trim().eq_ignore_ascii_case("exit")
        {
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

        // Spawn query processing task
        self.spawn_query_task(query_id, input).await;

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
            self.output_manager.write_info(
                "No recent response to rate. Ask a question first.",
            );
            self.render_tui().await?;
            return Ok(());
        }

        // Build and log the entry
        let (emoji, label) = match (weight as u64, &rating) {
            (10, _) => ("🔴", "critical (10×)"),
            (3, _)  => ("🟡", "medium (3×)"),
            _       => ("🟢", "good (1×)"),
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
                    self.output_manager.write_info(format!(
                        "⚠️  Failed to log feedback: {}", e
                    ));
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
            self.output_manager.add_trait_message(msg.clone() as Arc<dyn crate::cli::messages::Message>);
            self.render_tui().await?;

            // Spawn streaming query in background so event loop continues running
            // This allows TUI to keep rendering while tokens stream in
            let daemon_client = daemon_client.clone();
            let msg_clone = msg.clone();
            let output_mgr = self.output_manager.clone();

            tokio::spawn(async move {
                match daemon_client.query_local_only_streaming_with_callback(&query, move |token_text| {
                    tracing::debug!("[/local] Received chunk: {:?}", token_text);
                    msg_clone.append_chunk(token_text);
                }).await {
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
            self.output_manager.write_error("Error: /local requires daemon mode.");
            self.output_manager.write_info("    Start the daemon: finch daemon --bind 127.0.0.1:11435");
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
                    self.output_manager.write_info(format!(
                        "✓ Switched to provider: {}",
                        entry.provider_type()
                    ));
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
                 Add MCP servers to ~/.finch/config.toml to get started."
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
                 Add MCP servers to ~/.finch/config.toml to get started."
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
                    self.output_manager.write_error(format!(
                        "Failed to refresh MCP tools: {}",
                        e
                    ));
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
             For now, restart the REPL to reconnect."
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
        active_tool_uses: Arc<RwLock<std::collections::HashMap<String, (String, serde_json::Value, Arc<crate::cli::messages::WorkUnit>, usize)>>>,
    ) {
        tracing::debug!("process_query_with_tools starting for query_id: {:?}", query_id);

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
                        tracing::debug!("Client-side routing: Qwen (confidence: {:.2})", confidence);
                        Arc::clone(&qwen_gen)
                    }
                    _ => {
                        // Use Claude
                        tracing::debug!("Client-side routing: teacher (low confidence or no match)");
                        Arc::clone(&claude_gen)
                    }
                }
            } else {
                // Qwen not ready, use Claude
                tracing::debug!("Client-side routing: teacher (Qwen not ready)");
                Arc::clone(&claude_gen)
            }
        };

        const MAX_TOOL_ITERATIONS: usize = 100;
        #[allow(unused_assignments)]
        let mut iteration = 0;

        loop {
            if iteration >= MAX_TOOL_ITERATIONS {
                let _ = event_tx.send(ReplEvent::QueryFailed {
                    query_id,
                    error: format!("Max tool iterations ({}) reached", MAX_TOOL_ITERATIONS),
                });
                return;
            }

            iteration += 1;
            let _ = iteration; // suppress unused_assignment warning

            // Get conversation context
            let messages = conversation.read().await.get_messages();
            let caps = generator.capabilities();

            // Try streaming first if supported
            if caps.supports_streaming {
                tracing::debug!("Generator supports streaming, attempting to stream");

                // Create a WorkUnit for this generation turn BEFORE streaming begins.
                // The shadow-buffer / insert_before architecture requires the message to
                // exist in output_manager before any blit cycles run — the WorkUnit's
                // time-driven animation will be visible during streaming.
                let work_unit = output_manager.start_work_unit("Channeling");

                let stream_start = std::time::Instant::now();
                let mut token_count: usize = 0;
                let mut throb_idx: usize = 0;
                status_bar.update_operation("✳ Channeling…");

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
                                Ok(StreamChunk::TextDelta(delta)) => {
                                    tracing::debug!("Received TextDelta: {} bytes", delta.len());
                                    text.push_str(&delta);
                                    let delta_tokens = delta.split_whitespace().count();
                                    token_count += delta_tokens;
                                    // WorkUnit accumulates tokens for its own animated display
                                    work_unit.add_tokens(&delta);
                                    // Status bar also shows throb animation
                                    throb_idx = (throb_idx + 1) % THROB_FRAMES.len();
                                    let icon = THROB_FRAMES[throb_idx];
                                    let secs = stream_start.elapsed().as_secs();
                                    let elapsed_str = format_elapsed(secs);
                                    let tokens_str = format_token_count(token_count);
                                    status_bar.update_operation(format!(
                                        "{} Channeling… ({} · ↓ {} tokens)",
                                        icon, elapsed_str, tokens_str
                                    ));
                                }
                                Ok(StreamChunk::ContentBlockComplete(block)) => {
                                    tracing::debug!("Received ContentBlockComplete: {:?}", block);
                                    // Advance throb during thinking phase (before text arrives)
                                    throb_idx = (throb_idx + 1) % THROB_FRAMES.len();
                                    if token_count == 0 {
                                        let icon = THROB_FRAMES[throb_idx];
                                        let secs = stream_start.elapsed().as_secs();
                                        status_bar.update_operation(format!(
                                            "{} Channeling… ({} · thinking)",
                                            icon, format_elapsed(secs)
                                        ));
                                    }
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

                        tracing::debug!("[EVENT_LOOP] Stream receive loop ended, {} blocks received", blocks.len());
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
                            input_tokens: None,
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

                        // Clear streaming status
                        status_bar.clear_operation();

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

                            tracing::debug!("[EVENT_LOOP] Query state updated, adding assistant message");
                            // Add assistant message with ALL content blocks (text + tool uses)
                            // This is critical for proper conversation structure
                            let assistant_message = crate::claude::Message {
                                role: "assistant".to_string(),
                                content: blocks.clone(),
                            };
                            tracing::debug!("[EVENT_LOOP] Acquiring conversation write lock...");
                            conversation.write().await.add_message(assistant_message);
                            tracing::debug!("[EVENT_LOOP] Assistant message added, spawning tool executions");

                            // Tool calls share the WorkUnit that was created before streaming.
                            // Each tool gets its own sub-row within the same WorkUnit.

                            // Execute tools (check for AskUserQuestion first, then mode restrictions)
                            let current_mode = mode.read().await;
                            for tool_use in tool_uses {
                                // Check if tool is allowed in current mode
                                if !Self::is_tool_allowed_in_mode(&tool_use.name, &*current_mode) {
                                    // Tool blocked by plan mode - add error row and send result
                                    let label = format_tool_label(&tool_use.name, &tool_use.input);
                                    let row_idx = work_unit.add_row(label);
                                    let error_msg = format!(
                                        "Tool '{}' is not allowed in planning mode.\n\
                                         Reason: This tool can modify system state.\n\
                                         Available tools: read, glob, grep, web_fetch\n\
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
                                    (tool_use.name.clone(), tool_use.input.clone(), Arc::clone(&work_unit), row_idx),
                                );

                                // Check if this is AskUserQuestion (handle specially)
                                if let Some(result) = handle_ask_user_question(&tool_use, Arc::clone(&tui_renderer)).await {
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
                                    Arc::clone(&conversation),
                                    Arc::clone(&output_manager),
                                ).await {
                                    // Send result immediately
                                    let _ = event_tx.send(ReplEvent::ToolResult {
                                        query_id,
                                        tool_id: tool_use.id.clone(),
                                        result,
                                    });
                                } else {
                                    // Regular tool execution
                                    tool_coordinator.spawn_tool_execution(query_id, tool_use);
                                }
                            }
                            drop(current_mode);
                            tracing::debug!("[EVENT_LOOP] Tool executions spawned, returning");
                            return;
                        }

                        // No tools — mark WorkUnit complete so blit shows final response
                        work_unit.set_complete();

                        // Add assistant message to conversation
                        tracing::debug!("[EVENT_LOOP] No tools found, adding assistant message to conversation");
                        conversation
                            .write()
                            .await
                            .add_assistant_message(text.clone());

                        // Update query state
                        query_states
                            .update_state(query_id, QueryState::Completed { response: text.clone() })
                            .await;

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
            let work_unit = output_manager.start_work_unit("Channeling");
            status_bar.update_operation("✳ Channeling…");
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
                            // Check if tool is allowed in current mode
                            if !Self::is_tool_allowed_in_mode(&tool_use.name, &*current_mode) {
                                let label = format_tool_label(&tool_use.name, &tool_use.input);
                                let row_idx = work_unit.add_row(label);
                                work_unit.fail_row(row_idx, "blocked in plan mode");
                                let error_msg = format!(
                                    "Tool '{}' is not allowed in planning mode.\n\
                                     Reason: This tool can modify system state.\n\
                                     Available tools: read, glob, grep, web_fetch\n\
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
                                (tool_use.name.clone(), tool_use.input.clone(), Arc::clone(&work_unit), row_idx),
                            );

                            // Check if this is AskUserQuestion (handle specially)
                            if let Some(result) = handle_ask_user_question(&tool_use, Arc::clone(&tui_renderer)).await {
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
                                Arc::clone(&conversation),
                                Arc::clone(&output_manager),
                            ).await {
                                // Send result immediately
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result,
                                });
                            } else {
                                // Regular tool execution
                                tool_coordinator.spawn_tool_execution(query_id, tool_use);
                            }
                        }
                        drop(current_mode);
                        return;
                    }

                    // No tools — mark WorkUnit complete
                    work_unit.set_complete();
                    tracing::debug!("Query complete (no tools), non-streaming finished");
                    return;
                }
                Err(e) => {
                    let _ = event_tx.send(ReplEvent::QueryFailed {
                        query_id,
                        error: format!("{}", e),
                    });
                    return;
                }
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
                    .update_state(query_id, QueryState::Completed { response: response.clone() })
                    .await;

                // Display response
                self.output_manager.write_response(&response);
            }

            ReplEvent::QueryFailed { query_id, error } => {
                // DON'T remove streaming message here - fallback providers need it!
                // The message will be removed on StreamingComplete or stays for final error display

                // Update query state
                self.query_states
                    .update_state(query_id, QueryState::Failed { error: error.clone() })
                    .await;

                // Display error
                self.output_manager.write_error(format!("Query failed: {}", error));

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

            ReplEvent::StreamingComplete { query_id, full_response } => {
                tracing::debug!("[EVENT_LOOP] Handling StreamingComplete event (non-streaming path)");

                // Clear streaming status
                self.status_bar.clear_operation();

                // Check if this query is executing tools
                // If so, the assistant message was already added with ToolUse blocks
                let is_executing_tools = if let Some(metadata) = self.query_states.get_metadata(query_id).await {
                    matches!(metadata.state, QueryState::ExecutingTools { .. })
                } else {
                    false
                };

                if !is_executing_tools {
                    tracing::debug!("[EVENT_LOOP] No tools, adding assistant message to conversation");
                    // Add complete response to conversation (only if not executing tools)
                    self.conversation
                        .write()
                        .await
                        .add_assistant_message(full_response.clone());
                    tracing::debug!("[EVENT_LOOP] Added assistant message to conversation");

                    // Update query state
                    self.query_states
                        .update_state(query_id, QueryState::Completed { response: full_response.clone() })
                        .await;
                    tracing::debug!("[EVENT_LOOP] Updated query state");
                } else {
                    tracing::debug!("[EVENT_LOOP] Tools executing, skipping duplicate message");
                }

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
            }

            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
            } => {
                // Update status bar with live stats
                self.status_bar.update_live_stats(
                    model,
                    input_tokens,
                    output_tokens,
                    latency_ms,
                );
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
                    // Update query state to cancelled
                    self.query_states
                        .update_state(qid, QueryState::Failed {
                            error: "Cancelled by user".to_string(),
                        })
                        .await;

                    // Clear active query
                    *self.active_query_id.write().await = None;

                    // Show cancellation message
                    self.output_manager.write_info("⚠️  Query cancelled by user (Ctrl+C)");
                    self.status_bar.clear_operation();
                    self.render_tui().await?;

                    tracing::info!("Query {} cancelled by user", qid);
                } else {
                    tracing::debug!("Ctrl+C pressed but no active query to cancel");
                }
            }

            ReplEvent::Shutdown => {
                // Handled in run() method - this should not be reached
                unreachable!("Shutdown event should be handled in run() method");
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

    /// Update the compaction percentage in the status bar
    async fn update_compaction_status(&self) {
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
        let (_tool_name, _tool_input, work_unit, row_idx) = {
            let mut map = self.active_tool_uses.write().await;
            map.remove(&tool_id).unwrap_or_else(|| {
                // Fallback: create a standalone WorkUnit for untracked tools
                let fallback = self.output_manager.start_work_unit("Tool");
                let row_idx = fallback.add_row(&tool_id);
                (tool_id.clone(), serde_json::Value::Null, fallback, row_idx)
            })
        };

        // Update the row in the WorkUnit with a compact summary
        match &result {
            Ok(content) => {
                work_unit.complete_row(row_idx, compact_tool_summary(content));
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

        // Create approval dialog
        let tool_name = &tool_use.name;

        // Create a concise summary of key parameters (not full JSON dump)
        let summary = match tool_name.as_str() {
            "bash" | "Bash" => {
                if let Some(cmd) = tool_use.input.get("command").and_then(|v| v.as_str()) {
                    format!("Command: {}", if cmd.len() > 60 { format!("{}...", &cmd[..60]) } else { cmd.to_string() })
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
                    format!("Pattern: {}", if pattern.len() > 40 { format!("{}...", &pattern[..40]) } else { pattern.to_string() })
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
                    format!("Reason: {}", if reason.len() > 50 { format!("{}...", &reason[..50]) } else { reason.to_string() })
                } else {
                    "Enter planning mode".to_string()
                }
            }
            _ => format!("Execute {} tool", tool_name)
        };

        let options = vec![
            DialogOption::with_description("Allow Once", "Execute this tool once without saving approval"),
            DialogOption::with_description("Allow Exact (Session)", "Allow this exact tool call for this session"),
            DialogOption::with_description("Allow Pattern (Session)", "Allow similar tool calls for this session"),
            DialogOption::with_description("Allow Exact (Persistent)", "Always allow this exact tool call"),
            DialogOption::with_description("Allow Pattern (Persistent)", "Always allow similar tool calls"),
            DialogOption::with_description("Deny", "Do not execute this tool"),
        ];

        let dialog = Dialog::select_with_custom(
            format!("Tool '{}' requires approval\n{}", tool_name, summary),
            options,
        );

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
        self.pending_approvals.write().await.insert(query_id, (tool_use, response_tx));

        tracing::debug!("[EVENT_LOOP] Tool approval dialog shown, waiting for user response");

        Ok(())
    }

    /// Convert dialog result to confirmation result
    fn dialog_result_to_confirmation(
        &self,
        dialog_result: crate::cli::tui::DialogResult,
        tool_use: &crate::tools::types::ToolUse,
    ) -> super::events::ConfirmationResult {
        use super::events::ConfirmationResult;
        use crate::tools::executor::generate_tool_signature;
        use crate::tools::patterns::ToolPattern;

        match dialog_result {
            crate::cli::tui::DialogResult::Selected(index) => match index {
                0 => ConfirmationResult::ApproveOnce,
                1 => {
                    let signature = generate_tool_signature(tool_use, std::path::Path::new("."));
                    ConfirmationResult::ApproveExactSession(signature)
                }
                2 => {
                    // Create pattern from tool use
                    let pattern = ToolPattern::new(
                        format!("{}:*", tool_use.name),
                        tool_use.name.clone(),
                        format!("Auto-generated pattern for {}", tool_use.name),
                    );
                    ConfirmationResult::ApprovePatternSession(pattern)
                }
                3 => {
                    let signature = generate_tool_signature(tool_use, std::path::Path::new("."));
                    ConfirmationResult::ApproveExactPersistent(signature)
                }
                4 => {
                    // Create pattern from tool use
                    let pattern = ToolPattern::new(
                        format!("{}:*", tool_use.name),
                        tool_use.name.clone(),
                        format!("Auto-generated pattern for {}", tool_use.name),
                    );
                    ConfirmationResult::ApprovePatternPersistent(pattern)
                }
                _ => ConfirmationResult::Deny, // Index 5 or cancelled
            },
            crate::cli::tui::DialogResult::CustomText(text) => {
                // User provided custom response - log it and deny for safety
                tracing::info!("Tool approval custom response: {}", text);
                ConfirmationResult::Deny
            }
            _ => ConfirmationResult::Deny,
        }
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

        self.status_bar.update_line(
            StatusLineType::Custom("plan_mode".to_string()),
            indicator,
        );
    }

    /// Check if a tool is allowed in the current mode
    fn is_tool_allowed_in_mode(tool_name: &str, mode: &ReplMode) -> bool {
        match mode {
            ReplMode::Normal | ReplMode::Executing { .. } => {
                // All tools allowed (subject to normal confirmation)
                true
            }
            ReplMode::Planning { .. } => {
                // Only inspection tools allowed
                matches!(tool_name, "read" | "glob" | "grep" | "web_fetch")
            }
        }
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

        self.output_manager.write_info(format!("{}", "✓ Entered planning mode".blue().bold()));
        self.output_manager.write_info(format!("📋 Task: {}", task));
        self.output_manager.write_info(format!("📁 Plan will be saved to: {}", plan_path.display()));
        self.output_manager.write_info("");
        self.output_manager.write_info(format!("{}", "Available tools:".green()));
        self.output_manager.write_info("  read, glob, grep, web_fetch");
        self.output_manager.write_info(format!("{}", "Blocked tools:".red()));
        self.output_manager.write_info("  bash, save_and_exec");
        self.output_manager.write_info("");
        self.output_manager.write_info("Ask me to explore the codebase and generate a plan.");
        self.output_manager.write_info(format!(
            "{}",
            "Type /show-plan to view, /approve to execute, /reject to cancel.".dark_grey()
        ));

        // Add mode change notification to conversation
        self.conversation.write().await.add_user_message(format!(
            "[System: Entered planning mode for task: {}]\n\
             Available tools: read, glob, grep, web_fetch\n\
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
            if matches!(*mode, ReplMode::Planning { .. } | ReplMode::Executing { .. }) {
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
        let result = plan_loop
            .run(&task, Arc::clone(&self.tui_renderer))
            .await?;

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
                    self.output_manager.write_info(format!(
                        "⚠️  Could not save plan file: {}",
                        e
                    ));
                }

                // Show the plan for final human review
                self.output_manager.write_info(format!("\n{}", "━".repeat(70)));
                self.output_manager
                    .write_info(format!("{}", "📋 FINAL IMPLEMENTATION PLAN".bold()));
                self.output_manager.write_info(format!("{}\n", "━".repeat(70)));
                self.output_manager.write_info(final_plan.clone());
                self.output_manager.write_info(format!("\n{}\n", "━".repeat(70)));
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

/// Handle PresentPlan tool call specially (shows approval dialog instead of executing as tool)
///
/// Returns Some(tool_result) if this is a PresentPlan call, None otherwise
async fn handle_present_plan(
    tool_use: &ToolUse,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
    mode: Arc<tokio::sync::RwLock<crate::cli::ReplMode>>,
    conversation: Arc<tokio::sync::RwLock<crate::cli::ConversationHistory>>,
    output_manager: Arc<crate::cli::OutputManager>,
) -> Option<Result<String>> {
    use chrono::Utc;
    use crossterm::style::Stylize;

    // Check if this is PresentPlan
    if tool_use.name != "PresentPlan" {
        return None;
    }

    tracing::debug!("[EVENT_LOOP] Detected PresentPlan tool call - showing approval dialog");

    // Extract plan content
    let plan_content = match tool_use.input["plan"].as_str() {
        Some(content) => content,
        None => return Some(Err(anyhow::anyhow!("Missing 'plan' field in PresentPlan input"))),
    };

    // Verify we're in planning mode and get plan path
    let (task, plan_path) = {
        let current_mode = mode.read().await;
        match &*current_mode {
            crate::cli::ReplMode::Planning { task, plan_path, .. } => (task.clone(), plan_path.clone()),
            _ => return Some(Ok("⚠️  Not in planning mode. Use EnterPlanMode first.".to_string())),
        }
    };

    // Save plan to file
    if let Err(e) = std::fs::write(&plan_path, plan_content) {
        return Some(Err(anyhow::anyhow!("Failed to save plan: {}", e)));
    }

    // Show plan in output
    output_manager.write_info(format!("\n{}\n", "━".repeat(70)));
    output_manager.write_info(format!("{}", "📋 IMPLEMENTATION PLAN".bold()));
    output_manager.write_info(format!("{}\n", "━".repeat(70)));
    output_manager.write_info(plan_content.to_string());
    output_manager.write_info(format!("\n{}\n", "━".repeat(70)));

    // Show approval dialog
    let dialog = crate::cli::tui::Dialog::select_with_custom(
        "Review Implementation Plan".to_string(),
        vec![
            crate::cli::tui::DialogOption::with_description(
                "Approve and execute",
                "Clear context and proceed with implementation (all tools enabled)",
            ),
            crate::cli::tui::DialogOption::with_description(
                "Request changes",
                "Provide feedback for Claude to revise the plan",
            ),
            crate::cli::tui::DialogOption::with_description(
                "Reject plan",
                "Exit plan mode and return to normal conversation",
            ),
        ],
    ).with_help("Use ↑↓ or j/k to navigate, Enter to select, 'o' for custom feedback, Esc to cancel");

    let mut tui = tui_renderer.lock().await;
    let dialog_result = tui.show_dialog(dialog);
    drop(tui);

    let dialog_result = match dialog_result {
        Ok(result) => result,
        Err(e) => return Some(Err(anyhow::anyhow!("Failed to show approval dialog: {}", e))),
    };

    // Handle dialog result
    match dialog_result {
        crate::cli::tui::DialogResult::Selected(0) => {
            // Approved - ask about context clearing
            let clear_dialog = crate::cli::tui::Dialog::select(
                "Clear conversation context?".to_string(),
                vec![
                    crate::cli::tui::DialogOption::with_description(
                        "Clear context (recommended)",
                        "Reduces token usage and focuses execution on the plan",
                    ),
                    crate::cli::tui::DialogOption::with_description(
                        "Keep context",
                        "Preserves exploration history in conversation",
                    ),
                ],
            );

            let mut tui = tui_renderer.lock().await;
            let clear_result = tui.show_dialog(clear_dialog);
            drop(tui);

            let clear_context = match clear_result {
                Ok(crate::cli::tui::DialogResult::Selected(0)) => true,
                Ok(crate::cli::tui::DialogResult::Selected(1)) => false,
                _ => false, // Default to not clearing on cancel
            };

            // Transition to executing mode
            *mode.write().await = crate::cli::ReplMode::Executing {
                task: task.clone(),
                plan_path: plan_path.clone(),
                approved_at: Utc::now(),
            };

            if clear_context {
                // Clear conversation and add plan as context
                output_manager.write_info(format!("{}", "Clearing conversation context...".blue()));
                conversation.write().await.clear();
                conversation.write().await.add_user_message(format!(
                    "[System: Plan approved! Execute this plan:]\n\n{}",
                    plan_content
                ));
                output_manager.write_info(format!("{}", "✓ Context cleared. Plan loaded as execution guide.".green()));
            } else {
                // Keep history, just add approval message
                conversation.write().await.add_user_message(
                    "[System: Plan approved! All tools are now enabled. You may execute bash commands and modify files.]".to_string()
                );
            }

            output_manager.write_info(format!("{}", "✓ Plan approved! All tools enabled.".green().bold()));

            Some(Ok("Plan approved by user. Context has been prepared. You may now proceed with implementation using all available tools (Bash, Write, Edit, etc.).".to_string()))
        }
        crate::cli::tui::DialogResult::Selected(1) | crate::cli::tui::DialogResult::CustomText(_) => {
            // Request changes
            let feedback = if let crate::cli::tui::DialogResult::CustomText(text) = dialog_result {
                text
            } else {
                // Show text input for changes
                let feedback_dialog = crate::cli::tui::Dialog::text_input(
                    "What changes would you like?".to_string(),
                    None,
                );

                let mut tui = tui_renderer.lock().await;
                let feedback_result = tui.show_dialog(feedback_dialog);
                drop(tui);

                match feedback_result {
                    Ok(crate::cli::tui::DialogResult::TextEntered(text)) => text,
                    _ => return Some(Ok("Plan review cancelled.".to_string())),
                }
            };

            output_manager.write_info(format!("{}", "📝 Changes requested. Revising plan...".yellow()));

            Some(Ok(format!(
                "User reviewed the plan and requests the following changes:\n\n{}\n\n\
                 Please revise the implementation plan based on this feedback and call PresentPlan again with the updated version.",
                feedback
            )))
        }
        crate::cli::tui::DialogResult::Selected(2) => {
            // Rejected
            *mode.write().await = crate::cli::ReplMode::Normal;
            output_manager.write_info(format!("{}", "✗ Plan rejected. Returning to normal mode.".yellow()));
            conversation.write().await.add_user_message("[System: Plan rejected by user. Returning to normal conversation.]".to_string());

            Some(Ok("Plan rejected by user. Exiting plan mode and returning to normal conversation.".to_string()))
        }
        crate::cli::tui::DialogResult::Cancelled => {
            Some(Ok("Plan approval cancelled. Staying in planning mode.".to_string()))
        }
        _ => Some(Ok("Invalid dialog result.".to_string())),
    }
}

/// Handle AskUserQuestion tool call specially (shows dialog instead of executing as tool)
/// Pulsing animation frames for the streaming status indicator.
/// Cycles from small → large → small to create a "throb" effect.
const THROB_FRAMES: &[&str] = &["✦", "✳", "✼", "✳"];

/// Format elapsed seconds as "Xs" or "Xm Ys".
pub fn format_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format a token count as "N" or "N.Nk".
pub fn format_token_count(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}", n)
    }
}

/// Produce a compact one-line summary of tool output for display in an OperationMessage row.
///
/// - Empty content → ""
/// - Single line   → the line, truncated to 60 chars
/// - Multi-line    → "<N> lines"
fn compact_tool_summary(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() == 1 {
        let line = lines[0].trim();
        if line.len() > 60 {
            format!("{}…", &line[..57])
        } else {
            line.to_string()
        }
    } else {
        format!("{} lines", lines.len())
    }
}

///
/// Returns Some(tool_result) if this is an AskUserQuestion call, None otherwise
async fn handle_ask_user_question(
    tool_use: &ToolUse,
    tui_renderer: Arc<tokio::sync::Mutex<TuiRenderer>>,
) -> Option<Result<String>> {
    // Check if this is AskUserQuestion
    if tool_use.name != "AskUserQuestion" {
        return None;
    }

    tracing::debug!("[EVENT_LOOP] Detected AskUserQuestion tool call");

    // Parse input
    let input: crate::cli::AskUserQuestionInput = match serde_json::from_value(tool_use.input.clone()) {
        Ok(input) => input,
        Err(e) => {
            return Some(Err(anyhow::anyhow!("Failed to parse AskUserQuestion input: {}", e)));
        }
    };

    // Show dialog and collect answers
    let mut tui = tui_renderer.lock().await;
    let result = tui.show_llm_question(&input);
    drop(tui);

    match result {
        Ok(output) => {
            // Serialize output as JSON
            match serde_json::to_string_pretty(&output) {
                Ok(json) => Some(Ok(json)),
                Err(e) => Some(Err(anyhow::anyhow!("Failed to serialize output: {}", e))),
            }
        }
        Err(e) => {
            Some(Err(anyhow::anyhow!("Failed to show LLM question: {}", e)))
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // --- format_elapsed ---

    #[test]
    fn test_format_elapsed_seconds() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(1), "1s");
        assert_eq!(format_elapsed(59), "59s");
    }

    #[test]
    fn test_format_elapsed_minutes() {
        assert_eq!(format_elapsed(60), "1m 0s");
        assert_eq!(format_elapsed(61), "1m 1s");
        assert_eq!(format_elapsed(90), "1m 30s");
        assert_eq!(format_elapsed(600), "10m 0s");
        assert_eq!(format_elapsed(3661), "61m 1s");
    }

    // --- format_token_count ---

    #[test]
    fn test_format_token_count_small() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(1), "1");
        assert_eq!(format_token_count(999), "999");
    }

    #[test]
    fn test_format_token_count_thousands() {
        assert_eq!(format_token_count(1000), "1.0k");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(9900), "9.9k");
        assert_eq!(format_token_count(10000), "10.0k");
    }

    // --- streaming status bar format ---

    #[test]
    fn test_streaming_status_format() {
        // Verify the status bar message format used during streaming
        let secs = 75u64;
        let tokens = 1600usize;
        let elapsed_str = format_elapsed(secs);
        let tokens_str = format_token_count(tokens);
        let icon = THROB_FRAMES[1]; // "✳"
        let status = format!("{} Channeling… ({} · ↓ {} tokens)", icon, elapsed_str, tokens_str);
        assert_eq!(status, "✳ Channeling… (1m 15s · ↓ 1.6k tokens)");
    }

    #[test]
    fn test_streaming_status_format_short() {
        let secs = 9u64;
        let tokens = 42usize;
        let icon = THROB_FRAMES[0]; // "✦"
        let status = format!(
            "{} Channeling… ({} · ↓ {} tokens)",
            icon,
            format_elapsed(secs),
            format_token_count(tokens)
        );
        assert_eq!(status, "✦ Channeling… (9s · ↓ 42 tokens)");
    }

    #[test]
    fn test_streaming_status_thinking() {
        // While thinking (no text yet), status shows "· thinking" suffix
        let secs = 15u64;
        let icon = THROB_FRAMES[2]; // "✼"
        let status = format!("{} Channeling… ({} · thinking)", icon, format_elapsed(secs));
        assert_eq!(status, "✼ Channeling… (15s · thinking)");
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

    // --- compact_tool_summary ---

    #[test]
    fn test_compact_tool_summary_empty() {
        assert_eq!(compact_tool_summary(""), "");
        assert_eq!(compact_tool_summary("   "), "");
    }

    #[test]
    fn test_compact_tool_summary_single_line() {
        assert_eq!(compact_tool_summary("hello"), "hello");
        let long = "a".repeat(70);
        let result = compact_tool_summary(&long);
        assert!(result.ends_with('…'));
        assert!(result.len() <= 61); // 57 chars + "…" (3 bytes) = max 60 visible
    }

    #[test]
    fn test_compact_tool_summary_multi_line() {
        let multi = "line1\nline2\nline3";
        assert_eq!(compact_tool_summary(multi), "3 lines");
    }

    // --- find_last_exchange ---

    fn user_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
        }
    }

    fn assistant_msg(text: &str) -> crate::claude::Message {
        crate::claude::Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text { text: text.to_string() }],
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
        assert!(r.is_empty(), "no assistant msg → response should be empty: {:?}", r);
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
                content: vec![ContentBlock::Text { text: "   ".to_string() }],
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
}
