# cli::tui — public interface

Generated from [`src/cli/tui/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/cli/tui/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Autocomplete state for TUI rendering
pub struct AutocompleteState { … }
impl AutocompleteState {
    /// Keep the selected row inside a viewport of `visible_rows` suggestions.
    pub fn ensure_selection_visible(&mut self, visible_rows: usize);
    /// Get the currently selected command (if any)
    pub fn get_selected(&self) -> Option<&CommandSpec>;
    /// Selected mention row, when the mention pane is showing.
    pub fn get_selected_mention(&self) -> Option<&MentionCandidate>;
    /// Hide the dropdown
    pub fn hide(&mut self);
    /// Whether keyboard navigation is backed by a currently visible pane.
    pub fn is_interactive(&self) -> bool;
    pub fn new() -> Self;
    /// Move selection down (wraps around)
    pub fn select_next(&mut self);
    /// Move selection up (wraps around)
    pub fn select_previous(&mut self);
    /// Update matches and show dropdown
    pub fn show_matches(&mut self, matches: Vec<CommandSpec>);
    /// Show project-file mention candidates.
    pub fn show_mentions(&mut self, matches: Vec<MentionCandidate>);
}
/// Color scheme for TUI elements Re-exported from `theme`.
pub struct ColorScheme { … }
/// A dialog to display to the user
pub struct Dialog { … }
impl Dialog {
    /// Returns the virtual index of the Cancel button for Select/MultiSelect dialogs.
    pub fn cancel_virtual_index(&self) -> Option<usize>;
    /// Create a new confirmation dialog
    pub fn confirm(title: impl Into<String>, default: bool) -> Self;
    /// Returns the current cursor index for Select/MultiSelect dialogs.
    pub fn current_cursor(&self) -> Option<usize>;
    /// Handle a key event and return a result if the dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<DialogResult>;
    /// Create a new multi-select dialog
    pub fn multiselect(title: impl Into<String>, options: Vec<DialogOption>) -> Self;
    /// Create a new multi-select dialog with custom "Other" option
    pub fn multiselect_with_custom(title: impl Into<String>, options: Vec<DialogOption>) -> Self;
    /// Create a new single-select dialog
    pub fn select(title: impl Into<String>, options: Vec<DialogOption>) -> Self;
    /// Create a new single-select dialog with custom "Other" option
    pub fn select_with_custom(title: impl Into<String>, options: Vec<DialogOption>) -> Self;
    /// Returns the virtual index of the Submit button (MultiSelect only).
    pub fn submit_virtual_index(&self) -> Option<usize>;
    /// Create a new text input dialog
    pub fn text_input(title: impl Into<String>, default: Option<String>) -> Self;
    /// Create a tool-approval dialog (Yes / Yes-always / No).
    pub fn tool_approval(tool_name: &str, summary: &str) -> Self;
    /// Set optional body text shown inside the box above the options.
    pub fn with_body(mut self, body: impl Into<String>) -> Self;
    /// Set the help message for this dialog
    pub fn with_help(mut self, help: impl Into<String>) -> Self;
}
/// Option in a dialog menu
pub struct DialogOption { … }
impl DialogOption {
    /// Create a new dialog option with just a label
    pub fn new(label: impl Into<String>) -> Self;
    /// Create a dialog option with label and description
    pub fn with_description(label: impl Into<String>, description: impl Into<String>) -> Self;
    /// Attach a markdown preview to this option
    pub fn with_markdown(mut self, markdown: impl Into<String>) -> Self;
}
/// Result returned when a dialog is closed
pub enum DialogResult { Selected, MultiSelected, TextEntered, CustomText, Confirmed, Cancelled }
impl DialogResult {
    /// Check if the result was cancelled
    pub fn is_cancelled(&self) -> bool;
    /// Convert a cancelled result to an error
    pub fn ok_or_cancelled(self) -> Result<Self>;
}
/// Type of dialog to display
pub enum DialogType { Select, MultiSelect, TextInput, Confirm }
/// Widget for rendering dialogs
pub struct DialogWidget<'a> { … }
impl DialogWidget {
    /// Create a new dialog widget
    pub fn new(dialog: &'a Dialog, colors: &'a ColorScheme) -> Self;
}
/// One node of the live graph panel.
pub struct GraphNode { … }
impl GraphNode {
    /// A pending user task at the origin.
    pub fn new(id: usize, label: impl Into<String>) -> Self;
}
/// Who created the node.
pub enum GraphNodeAuthor { User, Ai }
/// What a graph node is for.
pub enum GraphNodeKind { Task, Constraint, Question, Observation }
/// How far along a node's work is.
pub enum GraphNodeStatus { Pending, Running, Done, Failed }
/// Nodes and edges the graph widget can draw, plus the camera the overlay uses.
pub struct GraphView { … }
impl GraphView {
    /// True when there is nothing to draw.
    pub fn is_empty(&self) -> bool;
    /// An empty graph with the default camera.
    pub fn new() -> Self;
}
/// Events produced by the async input task and consumed by the event loop.
pub enum InputEvent { Submitted, TypingStarted }
/// One live-area frame: the exact logical lines to paint, and where the cursor lands once they are painted.
pub(crate) struct LiveFrame { … }
impl LiveFrame {
    /// Physical terminal rows this frame occupies — the number of rows the next erase must clear.
    pub fn physical_rows(&self, terminal_width: usize) -> usize;
    /// Render this frame into a shadow buffer of the given size, so a test can assert on the cells a terminal would end up holding.
    pub fn to_shadow_buffer(&self, width: usize, height: usize) -> shadow_buffer::ShadowBuffer;
}
/// Everything [`plan_live_frame`] reads.
pub(crate) struct LiveFrameInputs<'a> { … }
pub enum PosetPanelMode { Graph, Forth, Typing }
/// Tabbed dialog for multiple questions
pub struct TabbedDialog { … }
impl TabbedDialog {
    /// Check if all questions have been answered
    pub fn all_answered(&self) -> bool;
    /// Get all answers as a HashMap
    pub fn collect_answers(&self) -> HashMap<String, String>;
    /// Get the current tab state
    pub fn current_tab(&self) -> &TabState;
    /// Get current tab index
    pub fn current_tab_index(&self) -> usize;
    /// Handle a key event and return a result if dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<TabbedDialogResult>;
    /// Create a new tabbed dialog from questions
    pub fn new(questions: Vec<Question>, title: Option<String>) -> Self;
    /// Get all tabs (for rendering)
    pub fn tabs(&self) -> &[TabState];
    /// Get title
    pub fn title(&self) -> Option<&str>;
}
/// Result from a tabbed dialog
pub enum TabbedDialogResult { Completed, Cancelled }
/// Widget for rendering tabbed dialogs
pub struct TabbedDialogWidget<'a> { … }
impl TabbedDialogWidget {
    pub fn new(dialog: &'a TabbedDialog, colors: &'a ColorScheme) -> Self;
}
pub struct TuiRenderer { … }
impl TuiRenderer {
    pub fn add_trait_message(&mut self, message: MessageRef) -> MessageId;
    /// Fold a scheduler event into the live child-agent projection.
    pub fn apply_activity(&mut self, update: activity::ActivityUpdate);
    /// Kept for API compatibility.
    pub fn check_and_refresh(&mut self) -> Result<()>;
    /// Clear the OperationStatus line from the status bar.
    pub fn clear_operation_status(&self);
    pub fn create_clean_textarea() -> TextArea<'static>;
    pub fn create_clean_textarea_with_text(text: &str) -> TextArea<'static>;
    /// Draw the live area from scratch and track `active_rows`.
    pub fn draw_live_area(&mut self) -> Result<()>;
    /// Render the Co-Forth panel (graph or Forth source) as a floating overlay in the top-right corner of the current terminal viewport.
    pub fn draw_poset_overlay(&mut self) -> Result<()>;
    /// Move the cursor up to the top of the live area and clear everything below it, ready for a fresh draw.
    pub fn erase_live_area(&mut self) -> Result<()>;
    /// Called from the event loop on every tick.
    pub fn flush_output_safe(&mut self, _output_manager: &OutputManager) -> Result<()>;
    pub fn handle_resize(&mut self, w: u16, h: u16) -> Result<()>;
    pub fn is_active(&self) -> bool;
    /// Mark the live area as needing a redraw on the next flush.
    pub fn mark_dirty(&mut self);
    pub fn new(output_manager: Arc<OutputManager>, status_bar: Arc<StatusBar>, colors: ColorScheme) -> Result<Self>;
    pub fn read_line(&mut self) -> Result<Option<String>>;
    /// Redraw the live area.
    pub fn render(&mut self) -> Result<()>;
    /// Convenience wrapper for the tool-approval flow.
    pub fn render_ask_user_dialog(&mut self, title: &str, options: Vec<DialogOption>) -> Result<DialogResult>;
    /// Re-acquire the terminal after a `suspend()`.
    pub fn resume(&mut self) -> anyhow::Result<()>;
    /// Set the OperationStatus line in the status bar (visible while queries run).
    pub fn set_operation_status(&self, msg: impl Into<String>);
    /// Attach the Co-Forth poset VM.
    pub fn set_poset(&mut self, poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>);
    /// Set session identity without writing to the terminal.
    pub fn set_session_label(&mut self, session_label: impl Into<String>);
    /// Attach the Co-Forth shared stack so the live area can display it.
    pub fn set_stack(&mut self, stack: Arc<tokio::sync::Mutex<Vec<String>>>);
    /// Attach a source the live area polls for task rows each time it redraws.
    pub fn set_task_rows(&mut self, rows: activity::SharedActivityRows);
    /// Update the live typing words and switch the panel to Typing mode.
    pub fn set_typing_words(&mut self, words: Vec<String>);
    /// Show a blocking dialog (used when no async event loop is running).
    pub fn show_dialog(&mut self, dialog: Dialog) -> Result<DialogResult>;
    /// Open a file in a full-screen TUI viewer.
    pub fn show_file_viewer(&mut self, path: &str) -> Result<()>;
    /// Show structured questions from the LLM (AskUserQuestion tool).
    pub fn show_llm_question(&mut self, input: &crate::cli::AskUserQuestionInput) -> Result<crate::cli::AskUserQuestionOutput>;
    /// Show the setup wizard using ratatui in an alternate screen.
    pub fn show_tabbed_dialog(&mut self, mut dialog: TabbedDialog) -> Result<TabbedDialogResult>;
    pub fn shutdown(&mut self) -> Result<()>;
    /// Build the static startup artifact for `OutputManager` projection.
    pub fn startup_header(model: &str, cwd: &str, session_label: &str) -> String;
    /// Temporarily release the terminal so another full-screen TUI (e.g.
    pub fn suspend(&self) -> anyhow::Result<()>;
    /// Toggle the poset panel between graph view and Forth source view.
    pub fn toggle_poset_view(&mut self);
    pub fn trigger_refresh(&mut self);
    pub fn update_ghost_text(&mut self);
}
```

## Functions

```rust
/// Compute the 0-based row index (from the top of the live area) where the cursor will be parked after draw_live_area() finishes repositioning it into the input…
pub(crate) fn compute_cursor_row_from_top(total_rows: usize, input_line_count: usize, cursor_row: usize, status_line_count: usize) -> usize { … }
/// Compute what to display in the status bar.
pub(crate) fn compute_effective_status(ghost_text: Option<&str>, raw_status: &str, current_input: &str, registry: &crate::cli::command_autocomplete::CommandRegistry) -> String { … }
/// Compute the ghost-text suffix to append after the user's current input.
pub(crate) fn compute_ghost_text(input: &str, registry: &crate::cli::command_autocomplete::CommandRegistry) -> Option<String> { … }
/// Count the number of terminal rows an `effective_status` string will occupy.
pub(crate) fn count_status_lines(status: &str) -> usize { … }
/// Best-effort terminal restoration for an exit path that cannot acquire the renderer lock.
pub fn emergency_restore_terminal() { … }
/// Formats the visible content of the custom-input line (no box borders).
pub(crate) fn format_custom_input_content(input: &str, cursor: usize) -> String { … }
/// Returns `(ansi_on, marker)` for the "Other (custom response)" row.
pub(crate) fn other_row_parts(is_selected: bool) -> (String, &'static str) { … }
/// Lay out one live-area frame.
pub(crate) fn plan_live_frame(inputs: &LiveFrameInputs<'_>, autocomplete: &mut AutocompleteState) -> LiveFrame { … }
/// Spawn a background task that polls keyboard input and sends to channel  This enables non-blocking input handling in the event loop: - Polls keyboard with 100…
pub fn spawn_input_task(tui_renderer: Arc<Mutex<TuiRenderer>>, quit_tx: mpsc::UnboundedSender<Vec<u8>>) -> mpsc::UnboundedReceiver<InputEvent> { … }
/// Calculate visible display-column width of string (excluding ANSI escape codes).
pub fn visible_length(s: &str) -> usize { … }
```

## Modules

```rust
pub mod activity;
```

## Referenced but not exported

These types appear in the signatures above but the facade does not export them, so a caller can hold a value and never name its type. Export them or change the signature: `ActivityUpdate`, `ShadowBuffer`, `SharedActivityRows`, `TabState`
