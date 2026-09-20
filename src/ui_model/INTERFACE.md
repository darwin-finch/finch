# ui_model — public interface

Generated from [`src/ui_model/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/ui_model/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
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
/// One laid-out node, in depth-first paint order.
pub struct NodeLayout { … }
/// Renderer-facing role of one transcript node.
pub enum NodeRole { Response, Activity, Program, Output, ToolGroup, ToolCall, Input, ToolOutput }
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
/// How one child of a [`Widget::Stack`] claims its extent along the stack's main axis.
pub enum Track { Natural, Flex, Max, Side }
/// The standard widget kinds.
pub enum Widget { Stack, Text, Rule, Completions, Composer, Viewport, DialogCard, Marked }
```

## Functions

```rust
/// Return the terminal display width (in columns) of a single character.
pub(crate) fn char_display_width(c: char) -> usize { … }
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
/// Truncate `s` to at most `columns` display columns.
pub fn truncate_to_columns(s: &str, columns: usize) -> String { … }
/// Calculate visible display-column width of string (excluding ANSI escape codes).
pub fn visible_length(s: &str) -> usize { … }
```
