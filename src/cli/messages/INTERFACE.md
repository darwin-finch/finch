# cli::messages — public interface

Generated from [`src/cli/messages/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/cli/messages/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// One child-agent lifecycle row. Re-exported from `finch-ui-model`.
pub struct AgentActivityView { … }
/// One tool run inside an agent lifecycle row. Re-exported from `finch-ui-model`.
pub struct AgentToolView { … }
/// An attributed message projected from a shared Brain.
pub struct BrainParticipantMessage { … }
impl BrainParticipantMessage {
    pub fn new(subject: impl Into<String>, content: impl Into<String>, invokes_model: bool) -> Self;
}
/// Opaque component-defined action.
pub struct ComponentAction(Box<dyn std::any::Any + Send + Sync>);
impl ComponentAction {
    pub fn downcast_ref<A: std::any::Any>(&self) -> Option<&A>;
    pub fn new<A: std::any::Any + Send + Sync>(action: A) -> Self;
}
/// A live tool call message that shows: - "● Edit(src/foo.rs)" header immediately when tool starts - Diff/output lines streaming in as they arrive  The `content…
pub struct LiveToolMessage { … }
impl LiveToolMessage {
    /// Append a line to the content (used for streaming diff lines)
    pub fn append_line(&self, line: &str);
    /// Get a clone of the Arc content for background streaming
    pub fn content_arc(&self) -> Arc<RwLock<String>>;
    pub fn new(header: impl Into<String>) -> Self;
    /// Mark as complete (hides the running indicator)
    pub fn set_complete(&self);
    /// Replace the full content (for immediate complete display)
    pub fn set_content(&self, content: impl Into<String>);
    /// Mark as failed
    pub fn set_failed(&self);
    /// Get a clone of the Arc status for background streaming
    pub fn status_arc(&self) -> Arc<RwLock<MessageStatus>>;
}
/// Stable identity for one retained application message. Re-exported from `finch-ui-model`.
pub struct MessageId(Uuid);
/// Type alias for a shared message reference
pub type MessageRef = Arc<dyn Message>;
/// Status of a retained application message. Re-exported from `finch-ui-model`.
pub enum MessageStatus { InProgress, Complete, Failed }
/// Live operation message that groups tool calls for a generation turn.
pub struct OperationMessage { … }
impl OperationMessage {
    /// Append a running row and return its index for later updates.
    pub fn add_row(&self, label: impl Into<String>) -> usize;
    /// Mark a row complete with an optional short summary.
    pub fn complete_row(&self, idx: usize, summary: impl Into<String>);
    /// Mark a row as failed with an error message.
    pub fn fail_row(&self, idx: usize, error: impl Into<String>);
    pub fn new(header: impl Into<String>) -> Self;
    /// Mark the whole operation complete (all tools done).
    pub fn set_complete(&self);
}
/// A single sub-row representing one tool call
pub struct OperationRow { … }
/// Status of an individual row within an OperationMessage
pub enum OperationRowStatus { Running, Complete, Error }
/// The output part of a say turn's ViewModel, set when the program produces output and updated live as `say` chunks stream. Re-exported from `finch-ui-model`.
pub struct OutputVm { … }
/// The program-source part of a say turn's ViewModel: the exact wire text the provider produced, retained so the reader can reveal it on demand. Re-exported from `finch-ui-model`.
pub struct ProgramSourceVm { … }
/// Progress message for downloads, uploads, etc.
pub struct ProgressMessage { … }
impl ProgressMessage {
    pub fn new(label: impl Into<String>, total: u64) -> Self;
    /// Mark as complete
    pub fn set_complete(&self);
    /// Mark as failed
    pub fn set_failed(&self);
    /// Update progress
    pub fn update_progress(&self, current: u64);
}
/// Status of a component-owned say turn. Re-exported from `finch-ui-model`.
pub enum SayTurnStatus { Running, Completed }
/// One frame's component snapshot: the retained ViewModel plus the chrome timing, captured under the same lock read. Re-exported from `finch-ui-model`.
pub struct SayTurnView { … }
/// Static message (immutable, for errors, system info, etc.)
pub struct StaticMessage { … }
impl StaticMessage {
    pub fn error(content: impl Into<String>) -> Self;
    pub fn info(content: impl Into<String>) -> Self;
    pub fn plain(content: impl Into<String>) -> Self;
    pub fn success(content: impl Into<String>) -> Self;
    pub fn warning(content: impl Into<String>) -> Self;
}
pub enum StaticMessageType { Info, Error, Success, Warning, Plain }
/// Streaming response message (for Claude/Qwen)
pub struct StreamingResponseMessage { … }
impl StreamingResponseMessage {
    /// Append a chunk of streamed text
    pub fn append_chunk(&self, text: &str);
    pub fn new() -> Self;
    /// Mark this response as complete
    pub fn set_complete(&self);
    /// Mark this response as failed
    pub fn set_failed(&self);
    /// Set whether the model is thinking (for UI indicator)
    pub fn set_thinking(&self, thinking: bool);
}
/// The say component's action vocabulary.
pub struct ToggleProgram;
/// Tool execution message with separate stdout/stderr
pub struct ToolExecutionMessage { … }
impl ToolExecutionMessage {
    /// Append to stderr
    pub fn append_stderr(&self, text: &str);
    /// Append to stdout
    pub fn append_stdout(&self, text: &str);
    pub fn new(tool_name: impl Into<String>) -> Self;
    /// Set exit code (marks as complete)
    pub fn set_exit_code(&self, code: i32);
    /// Mark as failed
    pub fn set_failed(&self);
}
/// User query message (immutable after creation)
pub struct UserQueryMessage { … }
impl UserQueryMessage {
    pub fn new(content: impl Into<String>) -> Self;
}
/// A single tool-call sub-item rendered below the WorkUnit header
pub struct WorkRow { … }
/// Whether a row is a model tool call or internal lifecycle activity. Re-exported from `finch-ui-model`.
pub enum WorkRowPresentation { Tool, Activity }
/// Status of an individual tool or activity row. Re-exported from `finch-ui-model`.
pub enum WorkRowStatus { Running, Complete, Error }
/// One tool or activity row, with diffs already rendered to display lines. Re-exported from `finch-ui-model`.
pub struct WorkRowView { … }
/// A unified message covering one AI generation turn.
pub struct WorkUnit { … }
impl WorkUnit {
    /// Add a running internal lifecycle row that is not a model tool call.
    pub fn add_activity_row(&self, label: impl Into<String>) -> usize;
    /// Add a running tool-call sub-row; returns its index for later updates.
    pub fn add_row(&self, label: impl Into<String>) -> usize;
    /// Accumulate tokens from a text delta (approximate: counts whitespace words).
    pub fn add_tokens(&self, text: &str);
    /// Append a chunk to the response text (for partial updates).
    pub fn append_response(&self, text: &str);
    /// Append a live output line to a Running sub-row's body.
    pub fn append_row_body_line(&self, idx: usize, line: String);
    /// Migrate this output unit to component-owned say-turn rendering (#882).
    pub fn begin_say_turn(&self, language: impl Into<String>, source: &str);
    /// Mark a sub-row complete with an optional compact one-line summary.
    pub fn complete_row(&self, idx: usize, summary: impl Into<String>);
    /// Mark a sub-row complete with a one-line summary and body lines shown below it.
    pub fn complete_row_with_body(&self, idx: usize, summary: impl Into<String>, body_lines: Vec<String>);
    /// Complete a tool row with a structured, theme-independent file diff.
    pub fn complete_row_with_diff(&self, idx: usize, diff: FileDiff);
    /// The lightweight domain snapshot (see [`WorkUnitHead`]).
    pub fn domain_head(&self) -> WorkUnitHead;
    /// The full domain snapshot the renderer's ViewModel projects per blit.
    pub fn domain_view(&self, colors: &ColorScheme) -> WorkUnitView;
    /// Mark a sub-row as failed.
    pub fn fail_row(&self, idx: usize, error: impl Into<String>);
    /// Mark a sub-row as failed, optionally attaching diagnostic body lines shown when the row is expanded.
    pub fn fail_row_with_body(&self, idx: usize, error: impl Into<String>, body_lines: Vec<String>);
    /// Route a component action to the say component's handle: toggles `show_program` under the message's lock.
    pub fn handle_say_turn_action(&self, action: &ComponentAction) -> bool;
    /// True after [`Self::present_as_assistant_prose`] succeeded.
    pub fn is_assistant_prose(&self) -> bool;
    /// Record that host-rendered lifecycle text belongs on this port.
    pub fn mark_host_lifecycle(&self);
    /// Create a new WorkUnit with the given verb (e.g.
    pub fn new(verb: impl Into<String>) -> Self;
    /// Mark successful untitled default-port `say` as assistant prose.
    pub fn present_as_assistant_prose(&self);
    /// The component-defined action a click on the turn's output region at `path` produces (stage 2, docs/TUI_DESIGN.md): the completed output is the toggle hit tar…
    pub fn say_turn_action(&self, path: &[u32]) -> Option<ComponentAction>;
    /// The component-owned ViewModel snapshot plus chrome timing, read under the message's own lock.
    pub fn say_turn_snapshot(&self) -> Option<SayTurnView>;
    /// Render retained rows as internal lifecycle activity rather than model tool calls.
    pub fn set_activity_presentation(&self, title: impl Into<String>);
    /// Return a generation unit to ordinary assistant/tool presentation.
    pub fn set_assistant_presentation(&self);
    /// Mark the whole WorkUnit complete (stops animation, shows final content).
    pub fn set_complete(&self);
    /// Mark the whole WorkUnit failed.
    pub fn set_failed(&self);
    /// Render this unit as an independently addressable VM output handle.
    pub fn set_output_handle(&self, title: impl Into<String>);
    /// Update an explicit output handle's progress independently from its body and status.
    pub fn set_output_progress(&self, completed: u64, total: Option<u64>);
    /// Render this unit as output emitted by a VM program, not an assistant message.
    pub fn set_program_output(&self);
    /// Render this unit as the exact program received from the provider.
    pub fn set_program_source(&self, language: impl Into<String>);
    /// Set the final response text (call after streaming ends).
    pub fn set_response(&self, text: impl Into<String>);
    /// Set the "thinking" flag shown in the animated status line.
    pub fn set_thinking(&self, thinking: bool);
    /// Update transient status independently from the durable visible body.
    pub fn set_transient_status(&self, status: Option<String>);
    /// Reconstruct a WorkUnit with the stable ID carried by retained/canonical session data so disclosure state survives frontend reconnects.
    pub fn with_id(id: MessageId, verb: impl Into<String>) -> Self;
}
/// Lightweight snapshot for consumers that classify or filter WorkUnits. Re-exported from `finch-ui-model`.
pub struct WorkUnitHead { … }
/// How one WorkUnit is presented in the transcript. Re-exported from `finch-ui-model`.
pub enum WorkUnitPresentation { Assistant, Activity, ProgramSource, ProgramOutput }
/// Full blit-time domain snapshot of one WorkUnit run. Re-exported from `finch-ui-model`.
pub struct WorkUnitView { … }
/// The retained ViewModel of one say turn, living on the WorkUnit behind the message's existing lock. Re-exported from `finch-ui-model`.
pub struct WorkUnitViewModel { … }
```

## Traits

```rust
/// Trait that all messages must implement  This is a minimal read-only interface.
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
/// Pick the next spinner verb in round-robin order.
pub fn random_spinner_verb() -> &'static str { … }
```

## Modules

```rust
pub mod concrete;
pub mod work_unit;
```
