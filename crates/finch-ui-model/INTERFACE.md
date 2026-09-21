# finch-ui-model — public interface

Generated from [`crates/finch-ui-model/src/lib.rs`](src/lib.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `crates/finch-ui-model/src/lib.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// One child-agent lifecycle row.
pub struct AgentActivityView { … }
/// One tool run inside an agent lifecycle row.
pub struct AgentToolView { … }
/// Direction a stack lays its children out in.
pub enum Axis { Column, Row }
/// The result of one claiming pass.
pub struct Layout { … }
impl Layout {
    /// Every hit rect recorded by the pass, in paint order.
    pub fn hit_rects(&self) -> impl Iterator<Item = (usize, Rect)> + '_;
    /// The claimed rect of the subtree marked with `key`.
    pub fn keyed(&self, key: u16) -> Option<Rect>;
}
/// Stable identity for one retained application message.
pub struct MessageId(Uuid);
impl MessageId {
    /// Restore a stable ID supplied by a canonical transcript event.
    pub fn from_uuid(uuid: Uuid) -> Self;
    /// Generate a new unique message ID.
    pub fn new() -> Self;
}
/// Status of a retained application message.
pub enum MessageStatus { InProgress, Complete, Failed }
/// One laid-out node, in depth-first paint order.
pub struct NodeLayout { … }
/// Renderer-facing role of one transcript node.
pub enum NodeRole { Response, Activity, Program, Output, ToolGroup, ToolCall, Input, ToolOutput }
/// The output part of a say turn's ViewModel, set when the program produces output and updated live as `say` chunks stream.
pub struct OutputVm { … }
/// The program-source part of a say turn's ViewModel: the exact wire text the provider produced, retained so the reader can reveal it on demand.
pub struct ProgramSourceVm { … }
/// A rectangle in the live frame's own coordinate space (row 0 is the top of the live area, column 0 the left terminal edge).
pub struct Rect { … }
impl Rect {
    /// First row below the rect.
    pub fn bottom(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    /// First column right of the rect.
    pub fn right(&self) -> usize;
}
/// One rendered transcript line: semantic text plus the row metadata the claiming pass turns into hit rects.
pub struct RenderedTranscriptLine { … }
/// Stable identity for one expandable row within the transcript.
pub struct RowId { … }
/// Status of a component-owned say turn.
pub enum SayTurnStatus { Running, Completed }
/// One frame's component snapshot: the retained ViewModel plus the chrome timing, captured under the same lock read.
pub struct SayTurnView { … }
/// How one child of a [`Widget::Stack`] claims its extent along the stack's main axis.
pub enum Track { Natural, Flex, Max, Side }
/// Widget props for one transcript row, projected from domain data.
pub struct TranscriptNode { … }
/// The standard widget kinds.
pub enum Widget { Stack, Text, Rule, Completions, Composer, Viewport, DialogCard, Marked }
/// Whether a row is a model tool call or internal lifecycle activity.
pub enum WorkRowPresentation { Tool, Activity }
/// Status of an individual tool or activity row.
pub enum WorkRowStatus { Running, Complete, Error }
/// One tool or activity row, with diffs already rendered to display lines.
pub struct WorkRowView { … }
/// Lightweight snapshot for consumers that classify or filter WorkUnits.
pub struct WorkUnitHead { … }
impl WorkUnitHead {
    /// Visible output body, including transient status and progress.
    pub fn output_body_lines(&self) -> Vec<String>;
}
/// How one WorkUnit is presented in the transcript.
pub enum WorkUnitPresentation { Assistant, Activity, ProgramSource, ProgramOutput }
/// Full blit-time domain snapshot of one WorkUnit run.
pub struct WorkUnitView { … }
/// The retained ViewModel of one say turn, living on the WorkUnit behind the message's existing lock.
pub struct WorkUnitViewModel { … }
```

## Functions

```rust
/// Return the terminal display width (in columns) of a single character.
pub fn char_display_width(c: char) -> usize { … }
/// Extract visible characters from string (strip ANSI codes) Returns (visible_chars, positions_of_ansi_codes)
pub fn extract_visible_chars(s: &str) -> (Vec<char>, Vec<usize>) { … }
/// Physical rows the composer's draft occupies, including the ghost suffix on its last line and the two-column prompt/continuation indent.
pub fn input_line_physical_rows_with_ghost(lines: &[String], terminal_width: usize, ghost_text: Option<&str>) -> Vec<usize> { … }
/// Run one claiming pass: `root` claims `frame`, children claim sub-rects.
pub fn layout(root: &Widget, frame: Rect) -> Layout { … }
/// The natural main-axis extent of a widget at the given cross extent: for a `Column` parent the extent is wrapped rows; for a `Row` parent it is display columns.
pub fn natural_size(widget: &Widget, cross: usize, axis: Axis) -> usize { … }
/// Number of physical terminal rows occupied by one logical line.
pub fn physical_rows(s: &str, terminal_width: usize) -> usize { … }
/// Project one WorkUnit snapshot into transcript widget props.
pub fn project_work_unit(view: &WorkUnitView) -> TranscriptNode { … }
/// Render the say turn's lines for one frame: exactly one representation for the turn's current state.
pub fn say_turn_lines(view: &SayTurnView) -> Vec<RenderedTranscriptLine> { … }
/// Truncate `s` to at most `columns` display columns.
pub fn truncate_to_columns(s: &str, columns: usize) -> String { … }
/// Calculate visible display-column width of string (excluding ANSI escape codes).
pub fn visible_length(s: &str) -> usize { … }
```
