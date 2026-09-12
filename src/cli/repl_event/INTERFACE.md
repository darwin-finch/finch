# cli::repl_event — public interface

Generated from [`src/cli/repl_event/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/cli/repl_event/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Result of a tool execution confirmation prompt
pub enum ConfirmationResult { ApproveOnce, ApproveExactSession, ApprovePatternSession, ApproveExactPersistent, ApprovePatternPersistent, ApproveWithInput, Deny }
/// Main event loop for concurrent REPL
pub struct EventLoop { … }
impl EventLoop {
    /// Run the event loop
    pub async fn run(&mut self) -> Result<()>;
    /// Create a new event loop with unified generators
    pub fn new(conversation: Arc<RwLock<ConversationHistory>>, active_persona: Arc<RwLock<crate::config::Persona>>, _cloud_gen: Arc<dyn Generator>, qwen_gen: Arc<dyn Generator>, router: Arc<Router>, generator_state: Arc<RwLock<GeneratorState>>, tool_definitions: Vec<ToolDefinition>, tool_executor: Arc<Mutex<ToolExecutor>>, program_runtime: Arc<crate::runtime::ProgramRuntime>, tui_renderer: TuiRenderer, output_manager: Arc<OutputManager>, status_bar: Arc<StatusBar>, streaming_enabled: bool, local_generator: Arc<RwLock<LocalGenerator>>, tokenizer: Arc<TextTokenizer>, ipc_client: Option<crate::ipc::IpcClient>, daemon_ipc_error: Option<String>, mode: Arc<RwLock<ReplMode>>, memory_system: Option<Arc<crate::memory::MemorySystem>>, session_label: String, session_uuid: Uuid, available_providers: Vec<crate::config::ProviderEntry>, active_provider_index: usize, daemon_client: Option<Arc<crate::client::DaemonClient>>, context_lines: usize, max_verbatim_messages: usize, context_recall_k: usize, todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>, todo_journal_target: crate::tools::todo::TodoJournalTarget, todo_journal_receiver: crate::tools::todo::TodoJournalReceiver, enable_summarization: bool, auto_compact_enabled: bool, daemon_base_url: Option<String>, provider_resolver: crate::runtime::scheduler::ProviderResolver, agent_scheduler: Arc<crate::runtime::scheduler::AgentScheduler>) -> Self;
}
/// LLM worker loop — owns AI generation concerns, runs as its own Tokio task.
pub struct LlmLoop { … }
impl LlmLoop {
    /// Run the LLM worker loop.
    pub async fn run(mut self);
    /// Construct the LLM loop.
    pub fn new(llm_rx: mpsc::UnboundedReceiver<LlmRequest>, event_tx: mpsc::UnboundedSender<ReplEvent>, cloud_gen: Arc<RwLock<Arc<dyn Generator>>>, qwen_gen: Arc<dyn Generator>, router: Arc<Router>, generator_state: Arc<RwLock<GeneratorState>>, tool_definitions: Arc<RwLock<Vec<ToolDefinition>>>, tool_coordinator: ToolExecutionCoordinator, program_runtime: Arc<crate::runtime::ProgramRuntime>, tool_call_history: Arc< RwLock<std::collections::HashMap<Uuid, std::collections::HashMap<String, u32>>>, >, conversation: Arc<RwLock<ConversationHistory>>, query_states: Arc<QueryStateManager>, mode: Arc<RwLock<ReplMode>>, output_manager: Arc<OutputManager>, status_bar: Arc<StatusBar>, tui_renderer: Arc<Mutex<TuiRenderer>>, active_tool_uses: ActiveToolUsesMap, memory_system: Option<Arc<crate::memory::MemorySystem>>, current_graph: Arc<tokio::sync::Mutex<crate::graph::ExecutionGraph>>, active_persona: Arc<RwLock<crate::config::Persona>>, session_label: String, cwd: String, context_lines: usize, max_verbatim_messages: usize, context_recall_k: usize, streaming_enabled: bool, enable_summarization: bool, auto_compact_enabled: bool, wire_metrics_logger: Option<Arc<crate::metrics::MetricsLogger>>) -> Self;
}
/// Requests sent from the TUI event loop to the LLM worker loop.
pub enum LlmRequest { Query }
/// Metadata for a query
pub struct QueryMetadata { … }
/// State of an in-flight query
pub enum QueryState { Processing, ExecutingTools, Completed, Failed, Cancelled }
/// Manages state for all in-flight queries
pub struct QueryStateManager { … }
impl QueryStateManager {
    /// Enter tool execution unless cancellation already won the race with a provider completion.
    pub async fn begin_tool_execution(&self, query_id: Uuid, tools_pending: usize) -> bool;
    /// Bind the query to its durable Brain/run before provider dispatch.
    pub async fn bind_brain_turn_provenance(&self, query_id: Uuid, provenance: BrainTurnProvenance);
    /// Bind the daemon-issued effect capability before provider/tool dispatch.
    pub async fn bind_effect_audit(&self, query_id: Uuid, effect_audit: crate::server::RunnerEffectAuditControl);
    pub async fn brain_output_work_unit(&self, query_id: Uuid) -> Option<Arc<WorkUnit>>;
    /// Cancel a query
    pub async fn cancel_query(&self, query_id: Uuid) -> bool;
    /// Clean up old completed queries (older than threshold)
    pub async fn cleanup_old_queries(&self, max_age: std::time::Duration);
    /// Get count of queries in a specific state
    pub async fn count_by_state(&self, state_matcher: impl Fn(&QueryState) -> bool) -> usize;
    /// Create a new query with initial state
    pub async fn create_query(&self, conversation_snapshot: Vec<Message>) -> Uuid;
    /// Get full metadata for a query
    pub async fn get_metadata(&self, query_id: Uuid) -> Option<QueryMetadata>;
    /// Get the current state of a query
    pub async fn get_state(&self, query_id: Uuid) -> Option<QueryState>;
    /// Remove a completed/failed/cancelled query (cleanup)
    pub async fn remove_query(&self, query_id: Uuid);
    pub async fn set_brain_output_work_unit(&self, query_id: Uuid, unit: Option<Arc<WorkUnit>>);
    pub async fn set_invocation_metadata(&self, query_id: Uuid, invocation: crate::providers::types::InvocationMetadata);
    pub async fn set_tool_work_unit(&self, query_id: Uuid, unit: Option<Arc<WorkUnit>>);
    pub async fn tool_work_unit(&self, query_id: Uuid) -> Option<Arc<WorkUnit>>;
    /// Publish a text-only provider completion while holding the same state lock used by cancellation.
    pub async fn try_publish_completion(&self, query_id: Uuid, response: String, source_for_history: String, conversation: &Arc<RwLock<crate::cli::conversation::ConversationHistory>>) -> bool;
    /// Atomically publish a provider completion with its ordered opaque continuation blocks intact.
    pub async fn try_publish_completion_content(&self, query_id: Uuid, response: String, content: Vec<crate::claude::ContentBlock>, conversation: &Arc<RwLock<crate::cli::conversation::ConversationHistory>>) -> bool;
    /// Update the state of a query
    pub async fn update_state(&self, query_id: Uuid, state: QueryState);
    /// Create a new query state manager
    pub fn new() -> Self;
}
/// Events that flow through the REPL event loop
pub enum ReplEvent { UserInput, QueryComplete, QueryFailed, ToolResult, ToolCallsStarted, ToolApprovalNeeded, VmApprovalNeeded, OutputReady, VmEffect, VmOutputComplete, VmEffectJournalComplete, TypedProgramComplete, StreamingComplete, StatsUpdate, AgentLifecycle, CancelQuery, Shutdown, ShowDialog, PosetComplete, LispResult, RemoteBrainMessage, RemoteBrainError, RemoteBrainDisconnected, HomeBrainMessage, HomeBrainWatchFailed, ReconnectHomeBrain, ReconnectHomeRunner, RunnerLeaseStatus, NamedBrainProgramRequested, NamedBrainTurnRequested, NamedBrainMemoryProjectionRequested, NamedBrainRunCancelRequested, NamedBrainProgramFinished, FrontendRestartReady }
/// Coordinates concurrent tool execution for the event loop
pub struct ToolExecutionCoordinator { … }
impl ToolExecutionCoordinator {
    /// Create a new tool execution coordinator
    pub fn new(event_tx: mpsc::UnboundedSender<ReplEvent>, tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>, output_manager: Arc<OutputManager>, conversation: Arc<RwLock<ConversationHistory>>, local_generator: Arc<RwLock<LocalGenerator>>, tokenizer: Arc<TextTokenizer>, repl_mode: Arc<RwLock<ReplMode>>, plan_content: Arc<RwLock<Option<String>>>) -> Self;
    /// Spawn a task to execute a tool (concurrent, non-blocking)  This spawns a background task that: 1.
    pub fn spawn_tool_execution(&self, query_id: Uuid, round_token: ToolRoundToken, tool_use: ToolUse, work_unit: Arc<WorkUnit>, row_idx: usize, effect_audit: Option<crate::server::RunnerEffectAuditControl>);
    /// Get access to the tool executor (for MCP commands and other management)
    pub fn tool_executor(&self) -> &Arc<tokio::sync::Mutex<ToolExecutor>>;
    /// Wire the Co-Forth poset so every tool call auto-records a trace node.
    pub fn with_poset(mut self, poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>) -> Self;
}
```

## Modules

```rust
pub mod activity_view;
pub mod event_loop;
pub mod events;
pub mod llm_loop;
pub(crate) mod model_selection;
pub mod plan_handler;
pub mod query_processor;
pub mod query_state;
pub mod tool_display;
pub mod tool_execution;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `ActiveToolUsesMap`, `BrainTurnProvenance`
