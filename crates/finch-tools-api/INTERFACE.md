# finch-tools-api — public interface

Generated from [`crates/finch-tools-api/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-tools-api/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Why [`ToolLoop::admit_execution`] refused.
pub enum AdmitError { Terminal, NotReady, AlreadyAdmitted }
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
/// Upper bound on what executing a program may affect.
pub enum ExecutionEffect { Pure, VmRead, VmWrite, WorkspaceRead, ExternalRead, WorkspaceWrite, ExternalWrite, Destructive, Unclassified }
impl ExecutionEffect {
    pub fn as_str(self) -> &'static str;
    pub fn runs_autonomously(self) -> bool;
}
/// Who is executing the tool — affects permission defaults.
pub enum ExecutorRole { Owner, Peer }
pub type LiveOutput = Arc<dyn LiveOutputSink>;
/// Type of match found
pub enum MatchType { Exact, Pattern }
/// Outcome of observing a delta or complete event.
pub enum ObserveOutcome { Accumulating, Settled, Late }
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
/// Why a tool call must not execute.
pub enum RejectReason { DuplicateId, MalformedArguments, UnknownTool, UnsupportedTool, ArgumentMismatch, EmptyId }
impl RejectReason {
    /// Speakable typed-result body.
    pub fn typed_message(&self, id: &str, name: &str) -> String;
}
/// Call that must produce a typed error and never execute.
pub struct RejectedCall { … }
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
/// JSON Schema for tool input parameters Re-exported from `finch-providers`.
pub struct ToolInputSchema { … }
impl ToolInputSchema {
    /// Create a simple schema with required string parameters
    pub fn simple(params: Vec<(&str, &str)>) -> Self;
}
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
impl ToolUse {
    /// Generate unique tool use ID
    pub fn generate_id() -> String;
    pub fn new(name: String, input: Value) -> Self;
    /// Convert to ContentBlock for conversation history
    pub fn to_content_block(&self) -> ContentBlock;
}
/// Validated call the host may execute at most once.
pub struct ValidatedCall { … }
/// A portable VM event attached to its owning ProgramRun.
pub struct VmEffectEnvelope { … }
impl VmEffectEnvelope {
    /// Stable `(execution_id, sequence)` handle for this envelope.
    pub fn handle(&self) -> VmEffectHandle;
}
/// Stable identity for one journaled VM effect.
pub struct VmEffectHandle { … }
```

## Traits

```rust
/// Opaque daemon-issued authority for physical effects in one named-Brain provider/tool loop.
pub trait EffectAuditAuthority: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}
/// Handle to the host's live session mode state.
pub trait HostModeState: Send + Sync {
    fn as_any(&self) -> &dyn std::any::Any;
}
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
/// True when a bash command is constitutionally Denied on the one-shot path.
pub fn bash_command_is_constitutionally_denied(command: &str) -> bool { … }
/// Compile policy for the registered tools.
pub fn compile_policy_from_registry(registry: &ToolRegistry, permissions: &PermissionManager) -> ToolCompilePolicy { … }
/// Production auto-approve predicate: refined-effect autonomy, but never for a path that escapes the workspace.
pub fn invocation_runs_autonomously(declared: ExecutionEffect, tool_name: &str, input: &Value, permissions: &PermissionManager) -> bool { … }
/// True when the tool's path argument resolves outside the workspace, or cannot be resolved (fail closed).
pub fn path_argument_escapes_workspace(tool_name: &str, input: &Value, workspace_root: &Path, cwd: &Path) -> bool { … }
/// Discrete path argument for tools that have one.
pub fn path_argument_for_tool(tool_name: &str, input: &Value) -> Option<String> { … }
/// True when `canonical` is the workspace root or a descendant of it.
pub fn path_is_inside_workspace(canonical: &Path, root: &Path) -> bool { … }
/// True when `raw` resolves outside `workspace_root` (or cannot be resolved).
pub fn raw_path_escapes_workspace(raw: &str, workspace_root: &Path, cwd: &Path) -> bool { … }
/// Effect a tool use presents at the approval boundary.
pub fn refined_effect_for_approval(declared: ExecutionEffect, tool_name: &str, input: &Value) -> ExecutionEffect { … }
/// Resolve `path` against `cwd`, following symlinks on existing prefixes.
pub fn resolve_canonical_path(path: &Path, cwd: &Path) -> Option<PathBuf> { … }
/// Workspace root used for path-argument containment.
pub fn resolve_workspace_root(start: &Path) -> PathBuf { … }
/// Semantic tools Finch may advertise this turn.
pub fn semantic_tools_for_advertisement(definitions: &[ToolDefinition], registry: &ToolRegistry, permissions: &PermissionManager, native_candidates: &[NativeToolGrant]) -> Vec<SemanticTool> { … }
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
