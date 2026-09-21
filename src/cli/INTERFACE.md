# cli — public interface

Generated from [`src/cli/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/cli/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Per-question annotation echoing back the selected option's metadata.
pub struct AnnotationEntry { … }
/// Input to AskUserQuestion tool
pub struct AskUserQuestionInput { … }
/// Output from AskUserQuestion tool
pub struct AskUserQuestionOutput { … }
/// Production Finch-native ChatGPT authentication service.
pub struct ChatGptAuthService { … }
impl ChatGptAuthService {
    /// Begin the named-credential ceremony without presenting anything: reuse or refresh a valid local account when one exists, or start a device authorization for…
    pub async fn begin_named_credential(&self, reference: &str, cancel: CancellationToken) -> Result<ChatGptNamedCredentialStart>;
    /// Reuse a valid local named account, refresh the same account when necessary, or start explicit device login.
    pub async fn ensure_named_credential(&self, reference: &str, presentation: DeviceLoginPresentation, cancel: CancellationToken) -> Result<EnsuredChatGptCredential>;
    /// Complete a began ceremony: poll to one terminal outcome, persist the validated account, and return its compensation authority (#424).
    pub async fn finish_named_credential(&self, reference: &str, pending: &DeviceAuthorization, cancel: CancellationToken) -> Result<EnsuredChatGptCredential>;
    /// Run the exact device lifecycle and return #174 metadata only after signed-token validation and crash-safe persistence.
    pub async fn login(&self, reference: &str, presentation: DeviceLoginPresentation, cancel: CancellationToken) -> Result<ProviderCredential>;
    /// Revoke remotely and persist a local tombstone.
    pub async fn logout(&self, reference: &str, cancel: CancellationToken) -> Result<ProviderCredential>;
    pub fn production() -> Result<Self>;
    /// Resolve an interrupted refresh/revoke locally without contacting the provider, retaining a durable tombstone for explicit reauthentication.
    pub fn recover(&self, reference: &str) -> Result<ProviderCredential>;
    /// Read local status without refresh, HTTP, or discovery.
    pub fn status(&self, reference: &str) -> Result<ChatGptAuthStatus>;
}
/// User-selected presentation actions. Exported as `ChatGptDeviceLoginPresentation`.
pub struct DeviceLoginPresentation { … }
/// A node in the console tree view
pub struct ConsoleNode { … }
/// Types of nodes in the console tree
pub enum ConsoleNodeType { UserMessage, AssistantResponse, ToolCall, ToolResult, System, Thinking }
/// Manages conversation history for multi-turn interactions with context window management
pub struct ConversationHistory { … }
impl ConversationHistory {
    /// Drop a staged round without changing committed provider history.
    pub fn abort_staged(&mut self, query_id: Uuid) -> bool;
    /// Add an assistant message to the conversation
    pub fn add_assistant_message(&mut self, content: String);
    /// Add a complete message to the conversation
    pub fn add_message(&mut self, message: Message);
    /// Add a user message to the conversation
    pub fn add_user_message(&mut self, content: String);
    /// Add a user message from already-assembled content blocks.
    pub fn add_user_message_with_content(&mut self, content: Vec<ContentBlock>);
    /// Add a user message with optional image attachments.
    pub fn add_user_message_with_images(&mut self, text: String, images: &[(String, String)]);
    /// Append text blocks to the last user message.
    pub fn append_text_blocks_to_last_user_message(&mut self, texts: &[String]) -> bool;
    /// Clear conversation history (start fresh)
    pub fn clear(&mut self);
    /// Publish the complete assistant payload and all matching results under one conversation write lock.
    pub fn commit_tool_round(&mut self, query_id: Uuid, token: ToolRoundToken) -> std::result::Result<Vec<ToolRoundResult>, ToolRoundError>;
    /// Get percentage remaining until auto-compaction (0.0 to 1.0)  Returns the percentage of context window remaining before compaction triggers.
    pub fn compaction_percent_remaining(&self) -> f32;
    pub fn completed_tool_results(&self, query_id: Uuid, token: ToolRoundToken) -> std::result::Result<Vec<ToolRoundResult>, ToolRoundError>;
    /// Get percentage of context window used (0.0 to 1.0)
    pub fn context_usage_percent(&self) -> f32;
    /// Get estimated token count (rough approximation)
    pub fn estimated_tokens(&self) -> usize;
    /// Apply ordinary context limits to the complete pair before its durable checkpoint and publication permit are released.
    pub fn finalize_tool_round_commit(&mut self);
    /// Get all messages for API request
    pub fn get_messages(&self) -> Vec<Message>;
    /// Check if conversation has any messages
    pub fn is_empty(&self) -> bool;
    /// Load conversation from JSON file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self>;
    /// Get total number of messages
    pub fn message_count(&self) -> usize;
    /// Create a new conversation history with default limits
    pub fn new() -> Self;
    /// Record one result exactly once for the matching staged round.
    pub fn record_tool_result(&mut self, query_id: Uuid, token: ToolRoundToken, tool_id: &str, result: &std::result::Result<String, anyhow::Error>) -> std::result::Result<ToolRoundProgress, ToolRoundError>;
    /// Restore conversation from a snapshot
    pub fn restore_snapshot(&mut self, snapshot: Vec<Message>);
    /// Restore the immediately preceding complete round to provider-invisible staging if the admitted continuation could not be spawned.
    pub fn rollback_last_tool_round(&mut self, query_id: Uuid, token: ToolRoundToken) -> std::result::Result<(), ToolRoundError>;
    /// Save conversation to JSON file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()>;
    /// Enable or disable auto-compaction
    pub fn set_auto_compact(&mut self, enabled: bool);
    /// Set compaction threshold (0.0 to 1.0, e.g., 0.8 = 80%)
    pub fn set_compaction_threshold(&mut self, threshold: f32);
    /// Check if compaction should be triggered
    pub fn should_compact(&self) -> bool;
    /// Create a snapshot of current conversation state
    pub fn snapshot(&self) -> Vec<Message>;
    /// Stage a complete provider assistant payload without making it visible to request builders, snapshots, compaction, or persistence.
    pub fn stage_assistant(&mut self, query_id: Uuid, assistant: Message) -> std::result::Result<ToolRoundToken, ToolRoundError>;
    pub fn staged_round(&self, query_id: Uuid) -> Option<(ToolRoundToken, usize, usize)>;
    /// Get number of complete turns (pairs of user + assistant messages)
    pub fn turn_count(&self) -> usize;
    /// Create a conversation history with custom limits
    pub fn with_limits(max_messages: usize, max_tokens_estimate: usize) -> Self;
}
/// A dialog to display to the user Re-exported from `cli::tui`.
pub struct Dialog { … }
/// Option in a dialog menu Re-exported from `cli::tui`.
pub struct DialogOption { … }
/// Result returned when a dialog is closed Re-exported from `cli::tui`.
pub enum DialogResult { Selected, MultiSelected, TextEntered, CustomText, Confirmed, Cancelled }
/// Re-exported from `finch-diff`.
pub(crate) enum DiffColorMode { Theme, NoColor }
/// Re-exported from `finch-diff`.
pub(crate) struct DiffHunk { … }
/// Re-exported from `finch-diff`.
pub(crate) struct DiffLine { … }
/// Re-exported from `finch-diff`.
pub(crate) enum DiffLineKind { Context, Add, Remove, NoNewline }
/// Secret-free identity the status line, `/status`, and `/model` all project. Re-exported from `cli::repl_event`.
pub struct EffectiveSelection { … }
/// Main event loop for concurrent REPL Re-exported from `cli::repl_event`.
pub struct EventLoop { … }
/// Bounded structured diff for one file. Re-exported from `finch-diff`.
pub(crate) struct FileDiff { … }
/// Production Finch-native SuperGrok authentication service.
pub struct GrokAuthService { … }
impl GrokAuthService {
    pub async fn begin_named_credential(&self, reference: &str, cancel: CancellationToken) -> Result<GrokNamedCredentialStart>;
    pub async fn ensure_named_credential(&self, reference: &str, presentation: DeviceLoginPresentation, cancel: CancellationToken) -> Result<EnsuredGrokCredential>;
    pub async fn finish_named_credential(&self, reference: &str, pending: &DeviceAuthorization, cancel: CancellationToken) -> Result<EnsuredGrokCredential>;
    pub async fn login(&self, reference: &str, presentation: DeviceLoginPresentation, cancel: CancellationToken) -> Result<ProviderCredential>;
    pub async fn logout(&self, reference: &str, cancel: CancellationToken) -> Result<ProviderCredential>;
    pub fn production() -> Result<Self>;
    pub fn recover(&self, reference: &str) -> Result<ProviderCredential>;
    pub fn status(&self, reference: &str) -> Result<GrokAuthStatus>;
}
/// User-selected presentation actions. Exported as `GrokDeviceLoginPresentation`.
pub struct DeviceLoginPresentation { … }
pub struct InputHandler { … }
impl InputHandler {
    /// Create new input handler with history support
    pub fn new() -> Result<Self>;
    /// Read a line of input with editing support  Returns: - `Ok(Some(line))` - user entered text - `Ok(None)` - user pressed Ctrl+C - `Err(e)` - I/O or other error
    pub fn read_line(&mut self, prompt: &str) -> Result<Option<String>>;
    /// Save history to disk
    pub fn save_history(&mut self) -> Result<()>;
}
/// MemTree Console state
pub struct MemTreeConsole { … }
impl MemTreeConsole {
    /// Add an assistant response node as child of current message
    pub fn add_assistant_response(&mut self, parent_id: NodeId, text: String) -> Result<NodeId>;
    /// Add a thinking/stats node
    pub fn add_thinking(&mut self, parent_id: NodeId, duration_ms: u64, text: String) -> Result<NodeId>;
    /// Add a tool call node
    pub fn add_tool_call(&mut self, parent_id: NodeId, tool_name: String, description: String) -> Result<NodeId>;
    /// Add a tool result node
    pub fn add_tool_result(&mut self, parent_id: NodeId, success: bool, text: String) -> Result<NodeId>;
    /// Add a user message node
    pub fn add_user_message(&mut self, text: String) -> Result<NodeId>;
    /// Get all visible nodes for rendering (respects expanded/collapsed state)
    pub fn get_visible_nodes(&self) -> Vec<NodeId>;
    /// Create a new MemTree console
    pub fn new(tree: Arc<RwLock<MemTree>>) -> Self;
    /// Render the tree view
    pub fn render(&self, f: &mut Frame, area: Rect);
    /// Navigate selection down
    pub fn select_next(&mut self);
    /// Navigate selection up
    pub fn select_previous(&mut self);
    /// Toggle expansion of a node
    pub fn toggle_expand(&mut self, node_id: NodeId);
    /// Toggle expansion of selected node
    pub fn toggle_selected(&mut self);
}
/// Stable identity for one retained application message. Re-exported from `finch-ui-model`.
pub struct MessageId(Uuid);
/// Type alias for a shared message reference Re-exported from `cli::messages`.
pub type MessageRef = Arc<dyn Message>;
/// Status of a retained application message. Re-exported from `finch-ui-model`.
pub enum MessageStatus { InProgress, Complete, Failed }
/// Visitor to extract the log message from tracing events.
pub(crate) struct MessageVisitor { … }
/// Thread-safe output buffer manager
pub struct OutputManager { … }
impl OutputManager {
    /// Add a trait-based message to the buffer
    pub fn add_trait_message(&self, message: MessageRef);
    /// Append to the last provider response (for streaming)
    pub fn append_response(&self, content: impl Into<String>);
    /// Clear all messages
    pub fn clear(&self);
    /// Disable buffering mode - writes go to stdout immediately
    pub fn disable_buffering(&self);
    /// Disable writing to stdout (for testing or special modes)
    pub fn disable_stdout(&self);
    /// Drain all pending output lines for flushing
    pub fn drain_pending(&self) -> Vec<String>;
    /// Enable buffering mode - accumulate writes for batch flush
    pub fn enable_buffering(&self);
    /// Enable writing to stdout (for TUI mode with scrollback)
    pub fn enable_stdout(&self);
    /// Get the last N messages
    pub fn get_last_messages(&self, n: usize) -> Vec<MessageRef>;
    /// Get all messages (for rendering)
    pub fn get_messages(&self) -> Vec<MessageRef>;
    /// Check if there are pending lines to flush
    pub fn has_pending(&self) -> bool;
    /// Check if the buffer is empty
    pub fn is_empty(&self) -> bool;
    /// Get the number of messages in the buffer
    pub fn len(&self) -> usize;
    /// Create a new OutputManager
    pub fn new(colors: crate::theme::ColorScheme) -> Self;
    /// Remove one transient projection after its contents have been adopted by a durable grouped work unit.
    pub fn remove_message(&self, id: crate::cli::messages::MessageId);
    /// Create and register a live tool message that supports streaming updates.
    pub fn start_live_tool(&self, header: impl Into<String>) -> Arc<LiveToolMessage>;
    /// Create and register an OperationMessage that groups tool-call rows for one generation turn.
    pub fn start_operation(&self, header: impl Into<String>) -> Arc<OperationMessage>;
    /// Create and register a WorkUnit for one AI generation turn.
    pub fn start_work_unit(&self, verb: impl Into<String>) -> Arc<WorkUnit>;
    /// Register a replayable WorkUnit using identity derived from its canonical event envelope rather than frontend construction time.
    pub fn start_work_unit_with_id(&self, id: MessageId, verb: impl Into<String>) -> Arc<WorkUnit>;
    /// Project an attributed shared-Brain participant message.
    pub fn write_brain_participant(&self, subject: impl Into<String>, content: impl Into<String>, invokes_model: bool);
    /// Write error message
    pub fn write_error(&self, content: impl Into<String>);
    /// Write system information message (help, patterns, stats)
    pub fn write_info(&self, content: impl Into<String>);
    /// Write progress update
    pub fn write_progress(&self, content: impl Into<String>);
    /// Write a provider response (can be called incrementally for streaming)
    pub fn write_response(&self, content: impl Into<String>);
    /// Write status information (deprecated - use write_progress or write_info)
    pub fn write_status(&self, content: impl Into<String>);
    /// Write tool execution output
    pub fn write_tool(&self, tool_name: impl Into<String>, content: impl Into<String>);
    /// Write pre-formatted tool output (Claude Code-style, with ANSI colors already embedded)
    pub fn write_tool_raw(&self, content: impl Into<String>);
    /// Write a user message
    pub fn write_user(&self, content: impl Into<String>);
}
/// Custom tracing layer that routes logs to OutputManager
pub struct OutputManagerLayer { … }
impl OutputManagerLayer {
    /// Create a new OutputManagerLayer
    pub fn new() -> Self;
    /// Create with debug logging enabled
    pub fn with_debug() -> Self;
}
/// Progress message for downloads, uploads, etc. Re-exported from `cli::messages`.
pub struct ProgressMessage { … }
/// A single question to ask the user
pub struct Question { … }
/// A single option in a question
pub struct QuestionOption { … }
pub struct Repl { … }
impl Repl {
    /// Add a pattern interactively
    pub async fn add_pattern_interactive(&mut self) -> Result<String>;
    /// Clear all patterns with confirmation
    pub async fn clear_patterns(&mut self) -> Result<String>;
    /// List all confirmation patterns
    pub async fn list_patterns(&self) -> Result<String>;
    pub async fn new(config: Config, claude_client: ClaudeClient, router: Router, metrics_logger: MetricsLogger, daemon_client: Option<Arc<crate::client::DaemonClient>>, session_label: String) -> Self;
    pub async fn process_query(&mut self, query: &str) -> Result<String>;
    /// Remove a pattern by ID (supports partial matching with 8+ chars)
    pub async fn remove_pattern(&mut self, id: &str) -> Result<String>;
    pub async fn run(&mut self) -> Result<()>;
    /// Run REPL in event loop mode (concurrent queries and tools) Falls back to traditional mode if TUI is not available
    pub async fn run_event_loop(&mut self, initial_prompt: Option<String>) -> Result<()>;
    /// Run REPL with an optional initial prompt
    pub async fn run_with_initial_prompt(&mut self, initial_prompt: Option<String>) -> Result<()>;
    /// `--model` is one-shot; `--provider` persists on this Brain.
    pub fn set_cli_selection(&mut self, model: Option<String>, provider: Option<String>);
    /// Retain a daemon IPC bootstrap failure until the TUI owns the screen, so it appears once as an actionable startup diagnostic rather than being cleared with pr…
    pub fn set_daemon_ipc_error(&mut self, error: impl Into<String>);
    /// Set the IPC client for daemon communication (must be called inside a LocalSet).
    pub fn set_ipc_client(&mut self, client: crate::client::IpcClient);
}
/// Events that flow through the REPL event loop Re-exported from `cli::repl_event`.
pub enum ReplEvent { UserInput, QueryComplete, QueryFailed, ToolResult, ToolCallsStarted, ToolApprovalNeeded, VmApprovalNeeded, OutputReady, VmEffect, VmOutputComplete, VmEffectJournalComplete, TypedProgramComplete, StreamingComplete, StatsUpdate, AgentLifecycle, CancelQuery, Shutdown, ShowDialog, PosetComplete, LispResult, RemoteBrainMessage, RemoteBrainError, RemoteBrainDisconnected, HomeBrainMessage, HomeBrainWatchFailed, ReconnectHomeBrain, ReconnectHomeRunner, RunnerLeaseStatus, NamedBrainProgramRequested, NamedBrainTurnRequested, NamedBrainMemoryProjectionRequested, NamedBrainRunCancelRequested, NamedBrainProgramFinished, FrontendRestartReady }
/// REPL operating mode
pub enum ReplMode { Normal, AutoAccept, Planning, Executing }
impl ReplMode {
    /// True when this mode actually waives AskUser / VM capability dialogs.
    pub fn auto_accepts_host_effects(&self) -> bool;
    /// Plan and executing-plan overlays.
    pub fn is_plan_overlay(&self) -> bool;
}
/// Handle to the live session mode, injected where the tool API cannot name [`ReplMode`].
pub struct ReplModeState(pub Arc<RwLock<ReplMode>>);
/// Inputs used to resolve one Brain's effective provider/model. Re-exported from `cli::repl_event`.
pub struct SelectionRequest { … }
/// Where the effective provider/model identity came from. Re-exported from `cli::repl_event`.
pub enum SelectionSource { Inherited, Override, OneShot }
/// Result of the shared first-run, `finch setup`, and `/setup` commit ceremony.
pub enum SetupApplyOutcome { Saved, Cancelled }
/// Check if a model family is compatible with an execution target Setup wizard result containing all collected configuration
pub struct SetupResult { … }
impl SetupResult {
    /// Legacy field accessor for backward compatibility
    pub fn backend_device(&self) -> ExecutionTarget;
}
/// Static message (immutable, for errors, system info, etc.) Re-exported from `cli::messages`.
pub struct StaticMessage { … }
/// Thread-safe status bar manager
pub struct StatusBar { … }
impl StatusBar {
    /// Clear all status lines
    pub fn clear(&self);
    /// Clear live stats (shorthand)
    pub fn clear_live_stats(&self);
    /// Clear operation status (shorthand)
    pub fn clear_operation(&self);
    /// Get one status line without exposing the internal map or its lock.
    pub fn get_line(&self, line_type: &StatusLineType) -> Option<String>;
    /// Get all status lines in a consistent order
    pub fn get_lines(&self) -> Vec<StatusLine>;
    /// Get status content as a string (for change detection)
    pub fn get_status(&self) -> String;
    /// Get rendered status content while projecting one line somewhere else.
    pub fn get_status_without(&self, excluded: &StatusLineType) -> String;
    /// Check if there are any status lines
    pub fn is_empty(&self) -> bool;
    /// Get the number of active status lines
    pub fn len(&self) -> usize;
    /// Create a new StatusBar
    pub fn new() -> Self;
    /// Remove a status line
    pub fn remove_line(&self, line_type: &StatusLineType);
    /// Render the status bar as a multi-line string
    pub fn render(&self) -> String;
    /// Replace the child activity aggregate in place.
    pub fn update_agent_activity(&self, active_children: usize, usage: &crate::cli::tui::activity::ActivityUsage);
    /// Update download progress line
    pub fn update_download_progress(&self, model_name: impl Into<String>, percentage: f64, downloaded: u64, total: u64);
    /// Add or update a status line
    pub fn update_line(&self, line_type: StatusLineType, content: impl Into<String>);
    /// Update live query statistics
    pub fn update_live_stats(&self, model: impl Into<String>, input_tokens: Option<u32>, output_tokens: Option<u32>, latency_ms: Option<u64>);
    /// Update operation status line
    pub fn update_operation(&self, operation: impl Into<String>);
    /// Replace the session-cumulative usage line for this Brain.
    pub fn update_session_usage(&self, ledger: &crate::cli::usage::SessionUsageLedger, pricing: Option<&crate::cli::usage::ModelPricingTable>);
    /// Update training stats line
    pub fn update_training_stats(&self, total_queries: usize, local_percentage: f64, quality_score: f64);
}
/// A single status line
pub struct StatusLine { … }
/// Types of status lines
pub enum StatusLineType { SessionLabel, SessionUsage, MemoryContext, ConversationTopic, ConversationFocus, ContextLine, BrainContextLine, LiveStats, AgentActivity, TrainingStats, DownloadProgress, OperationStatus, Suggestions, CompactionPercent, Custom }
/// Streaming response message (for Claude/Qwen) Re-exported from `cli::messages`.
pub struct StreamingResponseMessage { … }
/// Tabbed dialog for multiple questions Re-exported from `cli::tui`.
pub struct TabbedDialog { … }
/// Result from a tabbed dialog Re-exported from `cli::tui`.
pub enum TabbedDialogResult { Completed, Cancelled }
/// Tool execution message with separate stdout/stderr Re-exported from `cli::messages`.
pub struct ToolExecutionMessage { … }
/// Re-exported from `cli::tui`.
pub struct TuiRenderer { … }
/// User query message (immutable after creation) Re-exported from `cli::messages`.
pub struct UserQueryMessage { … }
/// Host-side projection of portable VM output events into Finch's reactive scrollback.
pub struct VmOutputProjection { … }
impl VmOutputProjection {
    /// Append host-rendered context (for example a proposal lifecycle notice) to this projection's response port.
    pub fn append_default(&self, text: &str);
    pub fn new(output: Arc<OutputManager>, default_response: Arc<WorkUnit>) -> Self;
    /// Apply one ordered VM event.
    pub fn project(&self, effect: &VmSideEffect);
    /// Project a portable, correlated VM effect exactly once and in its journal order.
    pub fn project_envelope(&self, envelope: VmEffectEnvelope) -> Vec<VmEffectEnvelope>;
}
/// A unified message covering one AI generation turn. Re-exported from `cli::messages`.
pub struct WorkUnit { … }
```

## Traits

```rust
/// Trait that all messages must implement  This is a minimal read-only interface. Re-exported from `cli::messages`.
pub trait Message: Send + Sync {
    fn id(&self) -> MessageId;
    fn format(&self, colors: &crate::theme::ColorScheme) -> String;
    fn status(&self) -> MessageStatus;
    fn content(&self) -> String;
    fn complete_transcript(&self, colors: &crate::theme::ColorScheme) -> String;
    fn work_unit_head(&self) -> Option<WorkUnitHead>;
    fn work_unit_view(&self, _colors: &crate::theme::ColorScheme) -> Option<WorkUnitView>;
    fn say_turn_view(&self) -> Option<SayTurnView>;
    fn transcript_action(&self, _path: &[u32]) -> Option<ComponentAction>;
    fn handle_transcript_action(&self, _action: &ComponentAction) -> bool;
    fn background_style(&self, _colors: &crate::theme::ColorScheme) -> Option<ratatui::style::Style>;
    fn background_style_for_line(&self, colors: &crate::theme::ColorScheme, _line_index: usize, _line_count: usize) -> Option<ratatui::style::Style>;
}
```

## Functions

```rust
/// Build the annotations map from questions and user answers.
pub fn build_annotations(questions: &[Question], answers: &HashMap<String, String>) -> HashMap<String, AnnotationEntry> { … }
/// Format a token count as "N" or "N.Nk". Re-exported from `cli::repl_event`.
pub fn format_token_count(n: usize) -> String { … }
/// Format a tool label like "Bash(git push)" or "Read(src/file.rs)" Re-exported from `cli::repl_event`.
pub fn format_tool_label(name: &str, input: &Value) -> String { … }
/// Get a reference to the global TUI renderer
pub fn get_global_tui_renderer() -> &'static Mutex<Option<TuiRenderer>> { … }
/// Get reference to global OutputManager
pub fn global_output() -> Arc<OutputManager> { … }
/// Get reference to global StatusBar
pub fn global_status() -> Arc<StatusBar> { … }
pub fn handle_command(command: Command, metrics_logger: &MetricsLogger, router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter validator: Option<&ThresholdValidator>, debug_enabled: &mut bool) -> Result<CommandOutput> { … }
/// Check if we're in non-interactive mode (stdout is not a TTY)
pub fn is_non_interactive() -> bool { … }
/// Returns `true` when `tool_name` may be called in `mode`. Re-exported from `cli::repl_event`.
pub(crate) fn is_tool_allowed_in_mode(tool_name: &str, mode: &ReplMode) -> bool { … }
/// Check if SHAMMAH_LOG environment variable is set
pub fn logging_enabled() -> bool { … }
/// Render one stable, secret-free status line for scripts and interactive use. Exported as `render_chatgpt_auth_status_line`.
pub fn render_status_line(status: &ChatGptAuthStatus) -> Result<String> { … }
/// Render one bounded changeset using a single theme and total output limit. Re-exported from `finch-diff`.
pub(crate) fn render_files(files: &[FileDiff], colors: &ColorScheme, mode: DiffColorMode) -> String { … }
/// Render one stable, secret-free status line for scripts and interactive use. Exported as `render_grok_auth_status_line`.
pub fn render_status_line(status: &GrokAuthStatus) -> Result<String> { … }
/// Pick the configured entry and overlays. Re-exported from `cli::repl_event`.
pub fn resolve_selection(providers: &[ProviderEntry], request: &SelectionRequest) -> Result<EffectiveSelection> { … }
/// Remove terminal controls from bounded multi-line dialog content. Re-exported from `finch-diff`.
pub(crate) fn sanitize_multiline(s: &str) -> String { … }
/// Re-exported from `finch-diff`.
pub(crate) fn sanitize_terminal(s: &str) -> String { … }
/// Replace or append one secret-free named credential and save no token data to config.toml. Exported as `save_chatgpt_named_credential`.
pub fn save_named_credential(config: crate::config::Config, credential: ProviderCredential) -> Result<()> { … }
/// Exported as `save_grok_named_credential`.
pub fn save_named_credential(config: crate::config::Config, credential: ProviderCredential) -> Result<()> { … }
/// Set the global OutputManager (called from main at startup)
pub fn set_global_output(output_manager: Arc<OutputManager>) { … }
/// Set the global StatusBar (called from main at startup)
pub fn set_global_status(status_bar: Arc<StatusBar>) { … }
/// Set the global TUI renderer (called when TUI mode is enabled)
pub fn set_global_tui_renderer(renderer: TuiRenderer) { … }
/// Show first-run setup wizard and return configuration
pub fn show_setup_wizard() -> Result<SetupResult> { … }
/// Shutdown the global TUI renderer and restore terminal state
pub fn shutdown_global_tui() -> anyhow::Result<()> { … }
/// Build the stable path and aggregate line-count summary for parsed files. Re-exported from `finch-diff`.
pub(crate) fn summarize_files(files: &[FileDiff]) -> String { … }
/// Run the shared ceremony for the explicit `finch setup` command.
pub async fn validate_command_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> { … }
/// Run the shared ceremony for automatic first-run setup.
pub async fn validate_first_run_and_apply(result: &SetupResult) -> Result<SetupApplyOutcome> { … }
```

## Constants

```rust
/// Re-exported from `finch-diff`.
pub(crate) const MAX_DIFF_HUNKS: usize = 128;
/// Re-exported from `finch-diff`.
pub(crate) const MAX_DIFF_INPUT_BYTES: usize = 1_048_576;
/// Maximum semantic lines retained for a bounded diff. Re-exported from `finch-diff`.
pub(crate) const MAX_DIFF_LINES: usize = 1024;
/// Re-exported from `finch-diff`.
pub(crate) const MAX_DIFF_LINE_CHARS: usize = 512;
/// Tools permitted in `ReplMode::Planning`, by canonical registered name. Re-exported from `cli::repl_event`.
pub(crate) const PLANNING_ALLOWED_TOOLS: &[&str] = &[ "read", "glob", "grep", "web_fetch", // Read-only by convention and confirmed normally, so the model can run // inspection commands like `which gh` or `cargo check` while planning. "bash", // Session-local plan visibility is not a workspace or host mutation. Keep // the familiar checklist usable while the model is deliberately planning. "todo_read", "todo_write", // Re-entering planning while already planning is idempotent // (`EnterPlanModeTool::execute` returns "already in planning mode" and // changes nothing). The canonical name is the only spelling the provider // is shown: `ToolRegistry::definitions()` omits aliases. "enter_plan_mode", "present_plan", "ask_user_question", ];
/// Compatibility spellings the planning gate accepts, mapped to the canonical entry in [`PLANNING_ALLOWED_TOOLS`]. Re-exported from `cli::repl_event`.
pub(crate) const PLANNING_ALLOWED_TOOL_ALIASES: &[(&str, &str)] = &[ ("Bash", "bash"), ("TodoRead", "todo_read"), ("TodoWrite", "todo_write"), ("EnterPlanMode", "enter_plan_mode"), ("PresentPlan", "present_plan"), ("AskUserQuestion", "ask_user_question"), ];
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `ChatGptAuthStatus`, `ChatGptNamedCredentialStart`, `Command`, `CommandOutput`, `EnsuredChatGptCredential`, `EnsuredGrokCredential`, `GrokAuthStatus`, `GrokNamedCredentialStart`, `ModelPricingTable`, `SessionUsageLedger`, `ToolRoundError`, `ToolRoundProgress`, `ToolRoundResult`, `ToolRoundToken`
