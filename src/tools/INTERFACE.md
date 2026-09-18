# tools — public interface

Generated from [`src/tools/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/tools/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Why [`ToolLoop::admit_execution`] refused.
pub enum AdmitError { Terminal, NotReady, AlreadyAdmitted }
pub struct AgentAwaitTool { … }
impl AgentAwaitTool {
    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self;
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self;
}
pub struct AgentCancelTool { … }
impl AgentCancelTool {
    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self;
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self;
}
pub struct AgentPollTool { … }
impl AgentPollTool {
    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self;
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self;
}
pub struct AgentSpawnTool { … }
impl AgentSpawnTool {
    pub fn child(scheduler: Arc<AgentScheduler>, parent: AgentIdentity) -> Self;
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self;
}
pub struct AnsibleTool;
/// Source of approval for a tool execution
pub enum ApprovalSource { NotApproved, SessionExact, SessionPattern, PersistentExact, PersistentPattern }
pub struct AskUserQuestionTool;
/// Start a shell command in the background; returns a stable task ID.
pub struct BackgroundBashTool { … }
impl BackgroundBashTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self;
}
/// Poll a background task's status and captured output.
pub struct BackgroundPollTool { … }
impl BackgroundPollTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self;
}
/// Stop a background task: kill its recorded process and reap it.
pub struct BackgroundStopTool { … }
impl BackgroundStopTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self;
}
pub struct BashTool;
/// Extended ContentBlock enum to support tool use
pub enum ContentBlock { Text, ToolUse, ToolResult }
impl ContentBlock {
    /// Extract text from text block
    pub fn as_text(&self) -> Option<&str>;
    /// Extract tool use from tool use block
    pub fn as_tool_use(&self) -> Option<ToolUse>;
    /// Check if this is a text block
    pub fn is_text(&self) -> bool;
    /// Check if this is a tool result block
    pub fn is_tool_result(&self) -> bool;
    /// Check if this is a tool use block
    pub fn is_tool_use(&self) -> bool;
}
/// Create a memory explicitly (store important facts/notes)
pub struct CreateMemoryTool { … }
impl CreateMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self;
}
pub struct DeferredFrontendRestart { … }
pub struct EditTool;
pub struct EnterPlanModeTool;
/// An exact approval for a specific tool signature
pub struct ExactApproval { … }
impl ExactApproval {
    /// Increment match count
    pub fn increment_match(&mut self);
    /// Check if this approval matches the given signature
    pub fn matches(&self, signature: &ToolSignature) -> bool;
    /// Create a new exact approval
    pub fn new(signature: ToolSignature) -> Self;
}
/// Bring Excel to the front, optionally opening a file.
pub struct ExcelActivateTool;
/// Get the formula stored in a cell (not the computed value).
pub struct ExcelFormulaTool;
/// Read a rectangular range as CSV text.
pub struct ExcelRangeTool;
/// Read a single cell's displayed value.
pub struct ExcelReadTool;
/// List sheet names in the active workbook.
pub struct ExcelSheetsTool;
/// Write a value into a single cell.
pub struct ExcelWriteTool;
pub struct GetLanguageDefinitionTool;
/// Compact state used to recover from a stale manifest or inspect revisions.
pub struct GetVmStateTool { … }
impl GetVmStateTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self;
}
pub struct GlobTool;
pub struct GrepTool;
pub struct GuiClickTool;
pub struct GuiInspectTool;
pub struct GuiTypeTool;
pub struct HashCompareTool;
/// Inspect the complete canonical turn behind a semantic search result.
pub struct InspectMemoryTool { … }
impl InspectMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self;
}
/// Inspect exact source and metadata for one immutable program version.
pub struct InspectProgramTool { … }
impl InspectProgramTool {
    pub fn new(memory: Arc<MemorySystem>) -> Self;
}
/// Retrieve the complete protocol documentation for one built-in typed word.
pub struct InspectVmWordTool { … }
impl InspectVmWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self;
}
/// Canonical inspection for one core word or immutable persisted definition.
pub struct InspectWordTool { … }
impl InspectWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>, memory: Option<Arc<MemorySystem>>) -> Self;
}
/// Tool for delegating to another LLM
pub struct LLMDelegationTool { … }
impl LLMDelegationTool {
    /// Create tool for Claude delegation
    pub fn for_claude(llm: Arc<dyn LLM>) -> Self;
    /// Create tool for DeepSeek delegation
    pub fn for_deepseek(llm: Arc<dyn LLM>) -> Self;
    /// Create tool for Gemini delegation
    pub fn for_gemini(llm: Arc<dyn LLM>) -> Self;
    /// Create tool for GPT-4 delegation
    pub fn for_gpt4(llm: Arc<dyn LLM>) -> Self;
    /// Create tool for Grok delegation
    pub fn for_grok(llm: Arc<dyn LLM>) -> Self;
    /// Create a new delegation tool
    pub fn new(name: impl Into<String>, llm: Arc<dyn LLM>, description: impl Into<String>) -> Self;
}
/// List recent conversations from memory
pub struct ListRecentTool { … }
impl ListRecentTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self;
}
pub type LiveOutput = Arc<dyn LiveOutputSink>;
/// Type of match found
pub enum MatchType { Exact, Pattern }
/// MCP client that manages multiple server connections. Re-exported from `tools::mcp`.
pub struct McpClient { … }
/// A single MCP server connection over STDIO Re-exported from `tools::mcp`.
pub struct McpConnection { … }
/// MCP server configuration Re-exported from `tools::mcp`.
pub struct McpServerConfig { … }
/// Untrusted discovery data retained with its server provenance. Re-exported from `tools::mcp`.
pub struct McpToolDescriptor { … }
/// Outcome of observing a delta or complete event.
pub enum ObserveOutcome { Accumulating, Settled, Late }
pub struct PatchTool;
/// Kind of path argument a structured pattern admits.
pub enum PathSlot { Any, WorkspaceContained }
/// Type of pattern matching to use
pub enum PatternType { Wildcard, Regex, Structured }
/// Permission decision for a tool execution
pub enum PermissionCheck { Allow, AskUser, Deny }
/// Permission manager - checks if tool execution is allowed
pub struct PermissionManager { … }
impl PermissionManager {
    /// Whether policy allows advertising this tool to a provider.
    pub fn allows_advertising(&self, tool_name: &str) -> bool;
    /// Check if tool execution is permitted
    pub fn check_tool_use(&self, tool_name: &str, input: &Value) -> PermissionCheck;
    /// Directory relative path arguments resolve against.
    pub fn cwd(&self) -> &Path;
    /// Create a permission manager for an AI peer (asymmetric rules).
    pub fn for_peer() -> Self;
    /// Load from configuration
    pub fn from_config(configs: HashMap<String, ToolPermissionConfig>) -> Self;
    /// Create new permission manager with default settings (Owner role).
    pub fn new() -> Self;
    /// Register tool-specific configuration
    pub fn register_tool_config(&mut self, tool_name: String, config: ToolPermissionConfig);
    /// Set default rule for unconfigured tools
    pub fn with_default_rule(mut self, rule: PermissionRule) -> Self;
    /// Set maximum tool turns
    pub fn with_max_turns(mut self, max_turns: usize) -> Self;
    /// Pin path resolution to an explicit workspace (tests: pass a temp dir that contains `.git`; do not `chdir`).
    pub fn with_workspace_root(mut self, root: PathBuf) -> Self;
    /// Canonical workspace root used for containment.
    pub fn workspace_root(&self) -> &Path;
}
/// Permission rule configuration
pub enum PermissionRule { Allow, Ask, Deny }
/// Persistent storage for patterns and exact approvals
pub struct PersistentPatternStore { … }
impl PersistentPatternStore {
    /// Add a new exact approval
    pub fn add_exact(&mut self, approval: ExactApproval);
    /// Add a new pattern
    pub fn add_pattern(&mut self, pattern: ToolPattern);
    /// Find pattern by ID (returns index)
    pub fn find_by_id(&self, id: &str) -> Option<usize>;
    /// Find pattern by ID (returns mutable reference)
    pub fn find_by_id_mut(&mut self, id: &str) -> Option<&mut ToolPattern>;
    /// Get exact approval by ID
    pub fn get_exact(&self, id: &str) -> Option<&ExactApproval>;
    /// Get pattern by ID
    pub fn get_pattern(&self, id: &str) -> Option<&ToolPattern>;
    /// Check if an exact approval exists (without incrementing count)
    pub fn has_exact(&self, signature: &ToolSignature) -> bool;
    /// Load from JSON file (with automatic v1→v2 migration)
    pub fn load(path: &Path) -> Result<Self>;
    /// Check if a signature matches any stored pattern or exact approval Returns the most specific match (exact > pattern)
    pub fn matches(&mut self, signature: &ToolSignature) -> Option<MatchType>;
    /// Prune unused patterns (0 matches, older than 30 days)
    pub fn prune_unused(&mut self) -> usize;
    /// Remove a pattern or approval by ID
    pub fn remove(&mut self, id: &str) -> bool;
    /// Save to JSON file (atomic write)
    pub fn save(&self, path: &Path) -> Result<()>;
    /// Get total number of patterns and approvals
    pub fn total_count(&self) -> usize;
}
/// One observed call after [`ToolLoop::finish_observation`].
pub enum PreparedCall { Ready, Rejected }
impl PreparedCall {
    /// Id used to stage the assistant tool_use and the matching result.
    pub fn id(&self) -> &str;
    /// Input used to stage the assistant tool_use.
    pub fn input(&self) -> &Value;
    /// Name used to stage the assistant tool_use.
    pub fn name(&self) -> &str;
}
pub struct PresentPlanTool;
/// Decision encoded in an editor-backed proposal file.
pub enum ProposalDecision { Execute, Chat, Cancel }
pub struct ReadTool;
/// Why a tool call must not execute.
pub enum RejectReason { DuplicateId, MalformedArguments, UnknownTool, UnsupportedTool, ArgumentMismatch, EmptyId }
impl RejectReason {
    /// Speakable typed-result body.
    pub fn typed_message(&self, id: &str, name: &str) -> String;
}
/// Call that must produce a typed error and never execute.
pub struct RejectedCall { … }
pub struct RestartTool;
/// Search memory for relevant past conversations
pub struct SearchMemoryTool { … }
impl SearchMemoryTool {
    pub fn new(memory_system: Arc<MemorySystem>) -> Self;
}
/// Search the runtime's built-in typed vocabulary.
pub struct SearchVmVocabularyTool { … }
impl SearchVmVocabularyTool {
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self;
}
/// Search program names, documentation, signatures, and source keywords.
pub struct SearchVocabularyTool { … }
impl SearchVocabularyTool {
    pub fn new(memory: Arc<MemorySystem>) -> Self;
}
/// Canonical vocabulary search across built-in VM words, source syntax, and persisted definitions.
pub struct SearchWordTool { … }
impl SearchWordTool {
    pub fn new(runtime: Arc<ProgramRuntime>, memory: Option<Arc<MemorySystem>>) -> Self;
}
pub struct SubmitProgramTool { … }
impl SubmitProgramTool {
    pub fn child(runtime: Arc<ProgramRuntime>, caller: crate::scheduler::AgentIdentity) -> Self;
    pub fn new(runtime: Arc<ProgramRuntime>) -> Self;
}
/// One typed task in the Brain's authoritative task-list projection. Re-exported from `brain`. Exported as `TodoItem`.
pub struct BrainTask { … }
pub struct TodoJournalReceiver { … }
impl TodoJournalReceiver {
    /// Start the non-Send Cap'n Proto worker after the REPL enters its LocalSet.
    pub fn spawn(mut self);
}
/// Frontend-local selector for the Brain that owns model-facing task writes.
pub struct TodoJournalTarget { … }
impl TodoJournalTarget {
    /// True when a Brain client will receive the next `todo_write`.
    pub fn is_bound(&self) -> bool;
    pub fn set(&self, selected: Option<crate::brain::AttachedBrainClient>);
}
/// Send-safe model-tool endpoint for the frontend-local Brain journal worker.
pub struct TodoJournalWriter { … }
impl TodoJournalWriter {
    /// Persist a replacement when a Brain is selected.
    pub async fn replace(&self, tasks: Vec<TodoItem>) -> Result<bool>;
}
/// Local projection of the selected Brain's durable task list.
pub struct TodoList { … }
impl TodoList {
    /// Return items to display in the TUI: in_progress first, then pending.
    pub fn active_items(&self) -> Vec<&TodoItem>;
    /// Return all items (for TodoRead / serialisation).
    pub fn get_all(&self) -> &[TodoItem];
    pub fn is_empty(&self) -> bool;
    pub fn len(&self) -> usize;
    /// Replace the entire list atomically (the semantics of TodoWrite).
    pub fn replace_all(&mut self, items: Vec<TodoItem>);
}
/// Priority of one Brain-owned task. Re-exported from `brain`. Exported as `TodoPriority`.
pub enum BrainTaskPriority { High, Medium, Low }
/// Return the selected Brain's current task-list projection as JSON.
pub struct TodoReadTool { … }
impl TodoReadTool {
    pub fn new(todo_list: Arc<RwLock<TodoList>>) -> Self;
}
/// Lifecycle status of one Brain-owned task. Re-exported from `brain`. Exported as `TodoStatus`.
pub enum BrainTaskStatus { Pending, InProgress, Completed }
/// Replace the selected Brain's task list atomically.
pub struct TodoWriteTool { … }
impl TodoWriteTool {
    pub fn journaled(todo_list: Arc<RwLock<TodoList>>, journal: crate::tools::todo::TodoJournalWriter) -> Self;
    pub fn new(todo_list: Arc<RwLock<TodoList>>) -> Self;
}
/// Names the model was offered this turn, and names the host can execute.
pub struct ToolCatalog { … }
impl ToolCatalog {
    /// Split offered-this-turn from host-executable names.
    pub fn new(offered: impl IntoIterator<Item = impl Into<String>>, executable: impl IntoIterator<Item = impl Into<String>>) -> Self;
    /// Catalog where offered names are also executable.
    pub fn offered(names: impl IntoIterator<Item = impl Into<String>>) -> Self;
}
/// Context passed to tools during execution
pub struct ToolContext<'a> { … }
/// Tool definition (Claude API-compatible) Re-exported from `finch-providers`.
pub struct ToolDefinition { … }
/// Tool executor - manages tool execution lifecycle
pub struct ToolExecutor { … }
impl ToolExecutor {
    /// Execute a single tool use
    pub async fn execute_tool<F>(&self, tool_use: &ToolUse, conversation: Option<&ConversationHistory>, save_models_fn: Option<F>, batch_trainer: Option< Arc<tokio::sync::RwLock<crate::training::batch_trainer::BatchTrainer>>, >, local_generator: Option<Arc<tokio::sync::RwLock<crate::local::LocalGenerator>>>, tokenizer: Option<Arc<crate::models::TextTokenizer>>, repl_mode: Option<Arc<tokio::sync::RwLock<crate::cli::ReplMode>>>, plan_content: Option<Arc<tokio::sync::RwLock<Option<String>>>>, live_output: Option<crate::tools::types::LiveOutput>, effect_audit: Option<crate::server::RunnerEffectAuditControl>) -> Result<ToolResult> where F: Fn() -> Result<()> + Send + Sync,;
    /// Execute multiple tool uses in sequence
    pub async fn execute_tool_loop<F>(&self, tool_uses: Vec<ToolUse>, conversation: Option<&ConversationHistory>, save_models_fn: Option<F>, batch_trainer: Option< Arc<tokio::sync::RwLock<crate::training::batch_trainer::BatchTrainer>>, >, local_generator: Option<Arc<tokio::sync::RwLock<crate::local::LocalGenerator>>>, tokenizer: Option<Arc<crate::models::TextTokenizer>>, repl_mode: Option<Arc<tokio::sync::RwLock<crate::cli::ReplMode>>>, plan_content: Option<Arc<tokio::sync::RwLock<Option<String>>>>) -> Result<Vec<ToolResult>> where F: Fn() -> Result<()> + Send + Sync + Clone,;
    /// Get list of all available tools (built-in + MCP)
    pub async fn list_all_tools(&self) -> Vec<crate::tools::types::ToolDefinition>;
    /// Add MCP client to enable MCP tools  Always returns Self (never fails) - gracefully handles MCP connection errors
    pub async fn with_mcp(mut self, config: &crate::config::Config) -> Self;
    /// Approve exact command persistently
    pub fn approve_exact_persistent(&mut self, sig: ToolSignature);
    /// Approve exact command for session only
    pub fn approve_exact_session(&mut self, sig: ToolSignature);
    /// Approve pattern persistently
    pub fn approve_pattern_persistent(&mut self, pattern: ToolPattern);
    /// Approve pattern for session only
    pub fn approve_pattern_session(&mut self, pattern: ToolPattern);
    /// Clear all persistent patterns and approvals
    pub fn clear_persistent_patterns(&mut self);
    /// Clear session approvals (keep persistent)
    pub fn clear_session_approvals(&mut self);
    /// Maximum execution time for a tool call.
    pub fn execution_timeout(&self, tool_name: &str) -> Option<std::time::Duration>;
    /// Check if a tool signature is pre-approved (returns approval source)
    pub fn is_approved(&mut self, sig: &ToolSignature) -> ApprovalSource;
    /// Get reference to MCP client (for management commands)
    pub fn mcp_client(&self) -> Option<&Arc<crate::tools::mcp::McpClient>>;
    /// Create new tool executor with persistent patterns path
    pub fn new(registry: ToolRegistry, permissions: PermissionManager, patterns_path: PathBuf) -> Result<Self>;
    /// Get reference to permissions manager
    pub fn permissions(&self) -> &PermissionManager;
    /// Get reference to persistent store (for management commands)
    pub fn persistent_store(&self) -> &PersistentPatternStore;
    /// Get reference to registry
    pub fn registry(&self) -> &ToolRegistry;
    /// Register session-scoped tools after their runtime dependencies exist.
    pub fn registry_mut(&mut self) -> &mut ToolRegistry;
    /// Remove a pattern or approval by ID
    pub fn remove_pattern(&mut self, id: &str) -> bool;
    /// Save persistent tool patterns if modified during this session.
    pub fn save_if_dirty(&mut self) -> Result<()>;
    /// Save patterns to disk if modified
    pub fn save_patterns(&mut self) -> Result<()>;
}
/// JSON Schema for tool input parameters Re-exported from `finch-providers`.
pub struct ToolInputSchema { … }
/// Single tool-round lifecycle.
pub struct ToolLoop { … }
impl ToolLoop {
    /// Admit execution for a ready id.
    pub fn admit_execution(&mut self, id: &str) -> Result<ValidatedCall, AdmitError>;
    /// Append a result at most once.
    pub fn append_result(&mut self, result: ToolLoopResult) -> Option<ToolLoopResult>;
    /// Number of ids that started execution.
    pub fn execution_starts(&self) -> usize;
    /// Close observation.
    pub fn finish_observation(&mut self) -> Vec<PreparedCall>;
    /// Identity recorded for this round.
    pub fn identity(&self) -> &ToolLoopIdentity;
    /// True after cancel, timeout, disconnect, failure, or completed drain.
    pub fn is_terminal(&self) -> bool;
    /// Start a round pinned to `identity` and the offered/executable catalog.
    pub fn new(identity: ToolLoopIdentity, catalog: ToolCatalog) -> Self;
    /// Record a complete tool call (native or translated from a content block).
    pub fn observe_complete(&mut self, id: String, name: String, input: Value, provenance: EventProvenance) -> ObserveOutcome;
    /// Record an incremental argument fragment.
    pub fn observe_delta(&mut self, id: String, name: Option<String>, arguments_delta: String, provenance: EventProvenance) -> ObserveOutcome;
    /// Number of ids that appended a result.
    pub fn results_appended(&self) -> usize;
    /// Terminal reason when the round has ended.
    pub fn terminal(&self) -> Option<&ToolLoopTerminal>;
    /// End the round.
    pub fn terminalize(&mut self, terminal: ToolLoopTerminal) -> bool;
}
/// Brain/run/provider/model identity pinned for one tool round.
pub struct ToolLoopIdentity { … }
/// Result the loop will append at most once per id.
pub struct ToolLoopResult { … }
impl ToolLoopResult {
    /// Failure or typed reject.
    pub fn error(id: impl Into<String>, content: impl Into<String>) -> Self;
    /// Typed result for a rejected call.
    pub fn from_reject(call: &RejectedCall) -> Self;
    /// Successful execution output.
    pub fn success(id: impl Into<String>, content: impl Into<String>) -> Self;
}
/// Why the loop will not admit further execution.
pub enum ToolLoopTerminal { Completed, Cancelled, TimedOut, Disconnected, Failed }
/// A pattern that can match multiple tool signatures using wildcards or regex
pub struct ToolPattern { … }
impl ToolPattern {
    /// Increment match count (deprecated, use record_match instead)
    pub fn increment_match(&mut self);
    /// Check if this pattern matches the given signature
    pub fn matches(&self, signature: &ToolSignature) -> bool;
    /// Create a new pattern with wildcard matching (default)
    pub fn new(pattern: String, tool_name: String, description: String) -> Self;
    /// Create a new structured pattern
    pub fn new_structured(tool_name: String, description: String, command_pattern: Option<String>, args_pattern: Option<String>, dir_pattern: Option<String>) -> Self;
    /// Create a new pattern with explicit pattern type
    pub fn new_with_type(pattern: String, tool_name: String, description: String, pattern_type: PatternType) -> Self;
    /// Record a match (increment count and update last_used timestamp)
    pub fn record_match(&mut self);
    /// Validate the pattern (check if regex compiles, etc.)
    pub fn validate(&self) -> Result<()>;
}
/// Pattern-based tool matcher
pub struct ToolPatternMatcher { … }
impl ToolPatternMatcher {
    /// Extract tool uses from query
    pub fn extract_tool_uses(&self, query: &str) -> Result<Vec<ToolUse>>;
    /// Check if query matches any tool pattern
    pub fn matches_any(&self, query: &str) -> bool;
    /// Create new matcher with default patterns
    pub fn new() -> Self;
    /// Create matcher with built-in patterns
    pub fn with_default_patterns() -> Result<Self>;
}
/// Configuration for a specific tool's permissions
pub struct ToolPermissionConfig { … }
/// Registry of available tools
pub struct ToolRegistry { … }
impl ToolRegistry {
    /// List all alias keys (compatibility spellings accepted at dispatch time but absent from [`Self::definitions`]).
    pub fn alias_names(&self) -> Vec<String>;
    /// Declared effect for a dispatch name: the registered tool's [`Tool::effect`], alias-resolved.
    pub fn declared_effect(&self, name: &str) -> ExecutionEffect;
    /// Get all tool definitions (for Claude API)
    pub fn definitions(&self) -> Vec<ToolDefinition>;
    /// Every name accepted at dispatch time: canonical registered names plus the alias spellings mapped by [`Self::register_alias`].
    pub fn dispatch_names(&self) -> Vec<String>;
    /// Get tool by name
    pub fn get(&self, name: &str) -> Option<&dyn Tool>;
    /// Get all tools (for iteration)
    pub fn get_all_tools(&self) -> Vec<&dyn Tool>;
    /// Check if tool exists
    pub fn has_tool(&self, name: &str) -> bool;
    /// Check if registry is empty
    pub fn is_empty(&self) -> bool;
    /// Number of registered tools
    pub fn len(&self) -> usize;
    /// Create empty registry
    pub fn new() -> Self;
    /// Register a tool
    pub fn register(&mut self, tool: Box<dyn Tool>);
    /// Accept a legacy spelling for a canonical registered tool.
    pub fn register_alias(&mut self, alias: impl Into<String>, canonical: impl Into<String>);
    /// List all tool names
    pub fn tool_names(&self) -> Vec<String>;
}
/// Tool execution result
pub struct ToolResult { … }
impl ToolResult {
    pub fn error(tool_use_id: String, error_message: String) -> Self;
    pub fn success(tool_use_id: String, content: String) -> Self;
}
/// Signature for a tool execution, used for caching approval decisions
pub struct ToolSignature { … }
impl ToolSignature {
    /// Reconstruct the bash command string from structured parts.
    pub fn full_command(&self) -> Option<String>;
}
/// Tool use request after adapter-level validation. Re-exported from `finch-providers`.
pub struct ToolUse { … }
/// Transport type for MCP servers Re-exported from `tools::mcp`.
pub enum TransportType { Stdio, Sse }
/// Validated call the host may execute at most once.
pub struct ValidatedCall { … }
pub struct WebFetchTool { … }
impl WebFetchTool {
    pub fn new() -> Self;
}
pub struct WriteTool;
```

## Traits

```rust
/// Per-tool presentation binding.
pub trait LiveOutputSink: Send + Sync {
    fn line(&self, text: String);
    fn vm_side_effect(&self, effect: VmSideEffect);
    fn vm_effect_envelope(&self, envelope: VmEffectEnvelope);
    fn defer_program_effects(&self) -> bool;
}
/// Tool trait - all tools must implement this
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn effect(&self) -> ExecutionEffect;
    fn description(&self) -> &str;
    fn input_schema(&self) -> ToolInputSchema;
    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String>;
    fn aliases(&self) -> &'static [&'static str];
    fn definition(&self) -> ToolDefinition;
}
```

## Functions

```rust
/// Compile policy for the registered tools.
pub fn compile_policy_from_registry(registry: &ToolRegistry, permissions: &PermissionManager) -> ToolCompilePolicy { … }
/// Helper to create all LLM delegation tools from registry
pub fn create_llm_tools(registry: &crate::llms::LLMRegistry) -> Vec<Box<dyn Tool>> { … }
pub(crate) fn deferred_frontend_restart_from_tool_result(result: &std::result::Result<String, anyhow::Error>) -> Option<DeferredFrontendRestart> { … }
/// A Brain restart deliberately does not carry the legacy conversation-file, resume, prompt, or one-shot execution flags into the replacement process.
pub(crate) fn frontend_replacement_args<I>(current: I, brain: &str) -> Vec<OsString> where I: IntoIterator<Item = OsString>, { … }
/// Generate a context-specific signature for a tool use
pub fn generate_tool_signature(tool_use: &ToolUse, working_dir: &std::path::Path) -> ToolSignature { … }
/// Production auto-approve predicate: refined-effect autonomy, but never for a path that escapes the workspace.
pub fn invocation_runs_autonomously(declared: ExecutionEffect, tool_name: &str, input: &Value, permissions: &PermissionManager) -> bool { … }
/// Apply a unified diff in memory so a batch review can show the resulting file.
pub(crate) fn preview_patched_text(original: &str, patch: &str) -> Result<String> { … }
/// Open a proposal artifact in the user editor and preserve the explicit `execute`/`chat`/`cancel` decision.
pub async fn propose_artifact_with_decision(language: &str, description: &str, source: &str) -> Result<ProposalDecision> { … }
/// Effect a tool use presents at the approval boundary.
pub fn refined_effect_for_approval(declared: ExecutionEffect, tool_name: &str, input: &Value) -> ExecutionEffect { … }
pub(crate) fn resume_terminal_after_editor() { … }
pub(crate) fn run_editor(path: &Path) -> Result<std::process::ExitStatus> { … }
/// Semantic tools Finch may advertise this turn.
pub fn semantic_tools_for_advertisement(definitions: &[ToolDefinition], registry: &ToolRegistry, permissions: &PermissionManager, native_candidates: &[NativeToolGrant]) -> Vec<SemanticTool> { … }
pub(crate) fn suspend_terminal_for_editor() { … }
pub fn todo_journal(projection: std::sync::Arc<tokio::sync::RwLock<TodoList>>) -> (TodoJournalWriter, TodoJournalTarget, TodoJournalReceiver) { … }
/// Map a declared execution effect onto the provider-neutral authority class.
pub fn tool_authority_from_effect(effect: ExecutionEffect) -> ToolAuthority { … }
```

## Constants

```rust
/// Registered tool names a peer is hard-denied regardless of configuration.
pub const PEER_HARD_DENY_TOOLS: &[&str] = &["restart_session", "spawn_task"];
/// Registered tool names through which a peer proposes file changes.
pub const PEER_REVIEWED_CHANGESET_TOOLS: &[&str] = &["write", "edit", "patch"];
/// Registered tool names a peer may invoke silently, without an approval dialog: read-only examination plus scheduler-local agent control.
pub const PEER_SILENT_ALLOW_TOOLS: &[&str] = &[ "read", "glob", "grep", "get_vm_state", "get_language_definition", "search_vm_vocabulary", "inspect_vm_word", "search_word", "inspect_word", "search_vocabulary", "inspect_program", "spawn_agent", "await_agent", "poll_agent", "cancel_agent", ];
/// Registered tools that only inspect Finch's own typed runtime metadata.
pub const VM_DISCOVERY_TOOLS: &[&str] = &[ "get_vm_state", "get_language_definition", "search_vm_vocabulary", "inspect_vm_word", "search_word", "inspect_word", "search_vocabulary", "inspect_program", ];
```
