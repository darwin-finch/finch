//! LLM worker loop — handles AI query dispatch independently of the TUI.
//!
//! `LlmLoop` runs as a separate Tokio task spawned by `EventLoop::run()`.
//! The TUI event loop sends [`LlmRequest`] messages via a channel; `LlmLoop`
//! spawns individual query tasks whose results flow back to the TUI loop via
//! `event_tx`.
//!
//! This separation keeps LLM I/O (network, tokenisation, tool orchestration)
//! out of the TUI select loop, so the UI stays responsive even during long
//! generation turns.

use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::repl::ReplMode;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::TuiRenderer;
use crate::generators::Generator;
use crate::models::bootstrap::GeneratorState;
use crate::router::Router;
use crate::tools::types::ToolDefinition;

use super::events::{LlmRequest, ReplEvent};
use super::model_selection::GeneratorPins;
use super::query_processor::{process_query_with_tools, ActiveToolUsesMap};
use super::query_state::{QueryState, QueryStateManager};
use super::tool_execution::ToolExecutionCoordinator;

/// LLM worker loop — owns AI generation concerns, runs as its own Tokio task.
pub struct LlmLoop {
    /// Receive LLM requests from the TUI event loop.
    llm_rx: mpsc::UnboundedReceiver<LlmRequest>,
    /// Send results (streaming chunks, completion, errors) back to the TUI event loop.
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    // ── LLM-specific state ─────────────────────────────────────────────────
    cloud_gen: Arc<RwLock<Arc<dyn Generator>>>,
    /// Generator snapshot for each top-level query. Tool continuations keep
    /// using this snapshot even if `/model` changes the session default.
    pinned_generators: Arc<GeneratorPins>,
    qwen_gen: Arc<dyn Generator>,
    router: Arc<Router>,
    generator_state: Arc<RwLock<GeneratorState>>,
    tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>,
    tool_coordinator: ToolExecutionCoordinator,
    /// Shared typed runtime that receives raw provider VM-wire programs.
    program_runtime: Arc<crate::runtime::ProgramRuntime>,
    tool_call_history:
        Arc<RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>>,

    // ── Shared state (Arc clones also held by EventLoop) ───────────────────
    conversation: Arc<RwLock<ConversationHistory>>,
    query_states: Arc<QueryStateManager>,
    mode: Arc<RwLock<ReplMode>>,
    output_manager: Arc<OutputManager>,
    status_bar: Arc<StatusBar>,
    tui_renderer: Arc<Mutex<TuiRenderer>>,
    active_tool_uses: ActiveToolUsesMap,
    memory_system: Option<Arc<crate::memory::MemorySystem>>,
    current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>,

    // ── Per-session config ─────────────────────────────────────────────────
    session_label: String,
    cwd: String,
    context_lines: usize,
    max_verbatim_messages: usize,
    context_recall_k: usize,
    enable_summarization: bool,
    auto_compact_enabled: bool,
}

impl LlmLoop {
    /// Construct the LLM loop.
    ///
    /// `cwd` must already be resolved; pass `EventLoop::cwd` after `run()` has
    /// set it from the process working directory.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        llm_rx: mpsc::UnboundedReceiver<LlmRequest>,
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        cloud_gen: Arc<RwLock<Arc<dyn Generator>>>,
        qwen_gen: Arc<dyn Generator>,
        router: Arc<Router>,
        generator_state: Arc<RwLock<GeneratorState>>,
        tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>,
        tool_coordinator: ToolExecutionCoordinator,
        program_runtime: Arc<crate::runtime::ProgramRuntime>,
        tool_call_history: Arc<
            RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>,
        >,
        conversation: Arc<RwLock<ConversationHistory>>,
        query_states: Arc<QueryStateManager>,
        mode: Arc<RwLock<ReplMode>>,
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        tui_renderer: Arc<Mutex<TuiRenderer>>,
        active_tool_uses: ActiveToolUsesMap,
        memory_system: Option<Arc<crate::memory::MemorySystem>>,
        current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>,
        session_label: String,
        cwd: String,
        context_lines: usize,
        max_verbatim_messages: usize,
        context_recall_k: usize,
        enable_summarization: bool,
        auto_compact_enabled: bool,
    ) -> Self {
        Self {
            llm_rx,
            event_tx,
            cloud_gen,
            pinned_generators: Arc::new(GeneratorPins::default()),
            qwen_gen,
            router,
            generator_state,
            tool_definitions,
            tool_coordinator,
            program_runtime,
            tool_call_history,
            conversation,
            query_states,
            mode,
            output_manager,
            status_bar,
            tui_renderer,
            active_tool_uses,
            memory_system,
            current_graph,
            session_label,
            cwd,
            context_lines,
            max_verbatim_messages,
            context_recall_k,
            enable_summarization,
            auto_compact_enabled,
        }
    }

    /// Run the LLM worker loop.  Consumes `self`; returns when the request
    /// channel is closed (i.e. when `EventLoop` exits).
    pub async fn run(mut self) {
        while let Some(req) = self.llm_rx.recv().await {
            match req {
                LlmRequest::Query { id, text, no_tools } => {
                    self.spawn_query(id, text, no_tools).await;
                }
            }
        }
    }

    /// Spawn a background Tokio task for one LLM turn.
    ///
    /// `query = ""` for tool-continuation turns (graph is not reset).
    /// `no_tools = true` suppresses tool definitions for conversational turns.
    async fn spawn_query(&self, query_id: Uuid, query: String, no_tools: bool) {
        // Reset the execution graph on fresh queries (not tool continuations).
        if !query.is_empty() {
            let mut g = self.current_graph.lock().await;
            g.reset(query_id, &self.session_label);
            g.add_node(crate::graph::NodeKind::UserInput {
                text: query.clone(),
            });
        }

        let event_tx = self.event_tx.clone();
        let active_generator = self.cloud_gen.read().await.clone();
        let claude_gen = self
            .pinned_generators
            .for_turn(query_id, !query.is_empty(), active_generator)
            .await;
        let qwen_gen = Arc::clone(&self.qwen_gen);
        let router = Arc::clone(&self.router);
        let generator_state = Arc::clone(&self.generator_state);
        let tool_defs: Arc<Vec<ToolDefinition>> = if no_tools {
            Arc::new(vec![])
        } else {
            Arc::new(self.tool_definitions.read().await.clone())
        };
        let conversation = Arc::clone(&self.conversation);
        let query_states = Arc::clone(&self.query_states);
        let tool_coordinator = self.tool_coordinator.clone();
        let program_runtime = Arc::clone(&self.program_runtime);
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
        // Always use the capable cloud model for summarisation, regardless of routing.
        let summary_gen = Arc::clone(&claude_gen);
        let tool_call_history = Arc::clone(&self.tool_call_history);
        let pinned_generators = Arc::clone(&self.pinned_generators);
        let terminal_query_states = Arc::clone(&query_states);

        tokio::spawn(async move {
            process_query_with_tools(
                query_id,
                query,
                event_tx,
                claude_gen,
                qwen_gen,
                router,
                generator_state,
                tool_defs,
                conversation,
                query_states,
                tool_coordinator,
                program_runtime,
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

            // Returning in ExecutingTools means another turn with the same ID
            // is imminent. Every other return is terminal (including transport
            // errors that currently leave QueryState as Processing).
            if !matches!(
                terminal_query_states.get_state(query_id).await,
                Some(QueryState::ExecutingTools { .. })
            ) {
                pinned_generators.release(query_id).await;
            }
        });
    }
}
