//! The widget vocabulary: plain-data types and pure functions a component
//! needs to build and claim a subtree, without touching `crossterm` or the
//! shadow buffer (docs/TUI_DESIGN.md, "Dependency direction").
//!
//! `cli::tui` (the engine) and message producers both depend on this crate;
//! nothing here depends back on either. Pure component projection and the
//! claiming pass live here; the engine supplies frames, hit-rect routing, and
//! paint.

use std::fmt;
use std::ops::Range;

use unicode_width::UnicodeWidthChar;
use uuid::Uuid;

mod say_turn;

pub use say_turn::{
    say_turn_lines, OutputVm, ProgramSourceVm, SayTurnStatus, SayTurnView, WorkUnitViewModel,
};

/// Stable identity for one retained application message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MessageId(Uuid);

impl MessageId {
    /// Generate a new unique message ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Restore a stable ID supplied by a canonical transcript event.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Stable identity for one expandable row within the transcript.
///
/// `path` is append-only semantic ancestry (unit, call index, input/output),
/// so streamed appends and terminal reflow never change an existing row's key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowId {
    pub message_id: MessageId,
    pub path: Vec<u32>,
}

/// Renderer-facing role of one transcript node. The ViewModel derives it from
/// domain data at projection time; the renderer uses it to route disclosure,
/// focus, and bounded tool viewports — never as a widget kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Response,
    Activity,
    Program,
    Output,
    ToolGroup,
    ToolCall,
    Input,
    ToolOutput,
}

/// One rendered transcript line: semantic text plus the row metadata the
/// claiming pass turns into hit rects. Text carries SGR today; the spans
/// migration (stage 4 of docs/TUI_DESIGN.md) replaces the baked bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedTranscriptLine {
    pub text: String,
    pub row_id: Option<RowId>,
    /// Expand/collapse for assistive consumers. Set on expandable header
    /// lines; never encoded as a second visible `[expanded]`/`[collapsed]`
    /// token in `text`.
    pub row_expanded: Option<bool>,
    /// Role of the row that produced this line, when the line belongs to an
    /// interactive row. Set on the row's header line and on its body lines.
    pub role: Option<NodeRole>,
    /// The tool result whose bounded child viewport this body line belongs to.
    /// Only `ToolOutput` body lines carry it; the hit-region rebuild uses it to
    /// give the control ownership of its own cells.
    pub body_of: Option<RowId>,
    /// True when the line belongs to a component-owned row (#882): disclosure
    /// state lives on the component's ViewModel, not the renderer's
    /// RowId-keyed maps, and a click routes to the component's handle.
    pub component_owned: bool,
}

// ─── Line metrics ─────────────────────────────────────────────────────────────
//
// Pure text measurement the claiming pass needs. Moved here from the shadow
// buffer so the vocabulary stays self-contained; the buffer re-exports them.

/// Return the terminal display width (in columns) of a single character.
///
/// Delegates to the [`unicode-width`](https://docs.rs/unicode-width) tables —
/// the same model the test-only `vt_oracle` terminal uses — so this
/// vocabulary and the terminal model agree by construction (#934):
///
/// - **Emoji and other wide codepoints measure 2 columns**: characters with
///   the `Emoji_Presentation` property and East Asian `Wide`/`Fullwidth`
///   codepoints (CJK, Hangul, fullwidth forms).
/// - **East-Asian-Ambiguous glyphs stay 1 column by design** (° ± → α Ω, box
///   drawing, Finch's `❯` prompt): that is what xterm and Western-locale
///   terminals render. Finch tracks no locale, so ambiguous widths are never
///   widened.
/// - Combining marks, the zero-width joiner, and variation selectors measure
///   0, so a ZWJ emoji sequence measures as the sum of its emoji.
///   Measurement stays per character — there is no grapheme clustering here,
///   matching the terminal model.
/// - Control characters measure 0; callers strip escape sequences before
///   measuring.
#[inline]
pub fn char_display_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Calculate visible display-column width of string (excluding ANSI escape codes).
///
/// Emoji and other wide codepoints (CJK / fullwidth: Chinese, Japanese,
/// Korean) occupy 2 terminal columns each; zero-width marks add none. All
/// other printable characters occupy 1 column. See [`char_display_width`].
pub fn visible_length(s: &str) -> usize {
    let mut len = 0;
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Handle escape sequences
                if chars.peek() == Some(&'[') {
                    // CSI sequence: \x1b[...m (color codes, cursor movement)
                    chars.next(); // consume '['
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() {
                            break; // Sequence terminator
                        }
                    }
                } else if chars.peek() == Some(&']') {
                    // OSC sequence: \x1b]...\x07 or \x1b]...\x1b\\
                    chars.next(); // consume ']'
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' || (ch == '\x1b' && chars.peek() == Some(&'\\')) {
                            if ch == '\x1b' {
                                chars.next(); // consume '\\'
                            }
                            break;
                        }
                    }
                } else {
                    // Other escape sequences, skip 1 char
                    chars.next();
                }
            }
            '\r' | '\x08' | '\x7f' => {
                // Control characters that don't add visible length
            }
            _ => {
                len += char_display_width(c);
            }
        }
    }

    len
}

/// Number of physical terminal rows occupied by one logical line.
///
/// Live-area erasure depends on this value being identical for every producer.
/// Counting logical lines as one leaves old spinner/tool rows behind whenever a
/// WorkUnit wraps.
pub fn physical_rows(s: &str, terminal_width: usize) -> usize {
    visible_length(s).max(1).div_ceil(terminal_width.max(1))
}

/// Extract visible characters from string (strip ANSI codes)
/// Returns (visible_chars, positions_of_ansi_codes)
pub fn extract_visible_chars(s: &str) -> (Vec<char>, Vec<usize>) {
    let mut visible_chars = Vec::new();
    let mut ansi_positions = Vec::new();
    let mut chars = s.chars().peekable();
    let mut pos = 0;

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                ansi_positions.push(pos);
                // Skip ANSI escape sequence
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for ch in chars.by_ref() {
                        if ch.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else if chars.peek() == Some(&']') {
                    chars.next();
                    while let Some(ch) = chars.next() {
                        if ch == '\x07' || (ch == '\x1b' && chars.peek() == Some(&'\\')) {
                            if ch == '\x1b' {
                                chars.next();
                            }
                            break;
                        }
                    }
                } else {
                    chars.next();
                }
            }
            '\r' | '\x08' | '\x7f' => {
                // Skip control characters
            }
            _ => {
                visible_chars.push(c);
                pos += 1;
            }
        }
    }

    (visible_chars, ansi_positions)
}

/// Truncate `s` to at most `columns` display columns.
///
/// Truncating with `chars().take(n)` is wrong wherever the result is then
/// assumed to occupy one terminal row: a CJK or fullwidth character is one
/// `char` and two columns, so `n` characters can be `2n` columns and wrap.
/// A wide character straddling the boundary is dropped rather than split.
/// ANSI escape sequences are copied through and cost no columns.
pub fn truncate_to_columns(s: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(s.len());
    let mut used = 0usize;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            out.push(c);
            // Copy the sequence verbatim; it occupies no columns.
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap_or('['));
                for ch in chars.by_ref() {
                    out.push(ch);
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else if let Some(next) = chars.next() {
                out.push(next);
            }
            continue;
        }
        if matches!(c, '\r' | '\x08' | '\x7f') {
            continue;
        }
        let width = char_display_width(c);
        if used + width > columns {
            break;
        }
        used += width;
        out.push(c);
    }
    out
}

/// Physical rows the composer's draft occupies, including the ghost suffix on
/// its last line and the two-column prompt/continuation indent.
pub fn input_line_physical_rows_with_ghost(
    lines: &[String],
    terminal_width: usize,
    ghost_text: Option<&str>,
) -> Vec<usize> {
    let width = terminal_width.max(1);
    if lines.is_empty() {
        return vec![1];
    }
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let prefix_width = 2; // `❯ ` and continuation indentation are both two columns.
            let ghost_width = if lines.len() == 1 && index == 0 {
                ghost_text.map(visible_length).unwrap_or(0)
            } else {
                0
            };
            (prefix_width + visible_length(line) + ghost_width)
                .max(1)
                .div_ceil(width)
        })
        .collect()
}

// ─── Claiming layout ──────────────────────────────────────────────────────────

/// A rectangle in the live frame's own coordinate space (row 0 is the top of
/// the live area, column 0 the left terminal edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    /// First row below the rect.
    pub fn bottom(&self) -> usize {
        self.y + self.height
    }

    /// First column right of the rect.
    pub fn right(&self) -> usize {
        self.x + self.width
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// How one child of a [`Widget::Stack`] claims its extent along the stack's
/// main axis.
///
/// The TUI root column only needs `Flex` and `Natural` today; `Max` and
/// `Side` exist for width-conditional and capped tracks the GUI roots
/// (#809/#810) and the layout-boundary tests construct.
#[allow(dead_code)]
#[derive(Debug)]
pub enum Track {
    /// The child's natural content extent, clamped to the space left. Natural
    /// tracks claim after flexible floors, from the end of the stack inward,
    /// so root chrome anchored to the bottom keeps its rows.
    Natural,
    /// A proportional share (`weight`) of what remains after natural tracks
    /// claim, with `min` cells reserved first when the parent can afford them.
    /// The leftover transcript viewport is the `Flex` child of Finch's root
    /// column.
    Flex { weight: usize, min: usize },
    /// The inner track, capped at `cap` cells.
    Max { cap: usize, track: Box<Track> },
    /// The inner track, but only when the parent's width is at least
    /// `min_width`; otherwise the child claims nothing. Width-conditional
    /// rails (#810) are a breakpoint of this track, not a second layout
    /// system.
    Side { min_width: usize, track: Box<Track> },
}

/// Direction a stack lays its children out in. `Row` parents place children
/// side by side; they do not flatten to lines. The TUI root is a `Column`
/// today; `Row` is what a GUI host and the layout-boundary tests construct.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Column,
    Row,
}

/// The standard widget kinds. Widgets paint a ViewModel snapshot handed to
/// them as props; they never query domain state to decide whether they are
/// visible — an empty [`Widget::Completions`] simply claims zero rows.
pub enum Widget {
    /// Children stacked along one axis, each with the track that claims its
    /// main-axis extent.
    Stack {
        axis: Axis,
        children: Vec<(Track, Widget)>,
    },
    /// Static lines of text.
    Text { lines: Vec<String> },
    /// One horizontal rule row.
    Rule,
    /// The completion pane. Claims zero rows when there is nothing to show.
    Completions { rows: Vec<String> },
    /// The composer: the draft plus its ghost-text suffix.
    Composer {
        input_lines: Vec<String>,
        ghost: Option<String>,
    },
    /// A scrolling viewport over transcript lines. Claims whatever the stack
    /// has left; only lines that fit are visible, and expandable rows inside
    /// the visible window claim the hit rects the depth-first pass records.
    Viewport { lines: Vec<RenderedTranscriptLine> },
    /// One dialog card (#807): the pinned lines `dialog_lines` produced for a
    /// `Dialog`, painted as an inline region of the conversation column. The
    /// card claims exactly the physical rows its lines occupy — the caller
    /// pins and pads the lines to the claimed budget, so the box, not the
    /// content, decides the height and Yes/No/Submit can never leave it.
    DialogCard { lines: Vec<String> },
    /// Marks a subtree so the layout result can hand back its claimed rect
    /// under a stable key.
    Marked(u16, Box<Widget>),
}

/// One laid-out node, in depth-first paint order.
#[derive(Debug)]
pub struct NodeLayout {
    /// Key from the enclosing [`Widget::Marked`], if any.
    pub key: Option<u16>,
    pub rect: Rect,
    /// Hit rects claimed by lines inside the box, as
    /// `(line index, top row, physical rows)`. Only hit-target lines carry an
    /// entry, and the rects are exactly what the depth-first pass claimed.
    pub hit_lines: Vec<(usize, usize, usize)>,
}

/// The result of one claiming pass.
#[derive(Debug, Default)]
pub struct Layout {
    pub nodes: Vec<NodeLayout>,
}

impl Layout {
    /// The claimed rect of the subtree marked with `key`.
    pub fn keyed(&self, key: u16) -> Option<Rect> {
        self.nodes
            .iter()
            .find(|node| node.key == Some(key))
            .map(|node| node.rect)
    }

    /// Every hit rect recorded by the pass, in paint order.
    pub fn hit_rects(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        self.nodes.iter().flat_map(|node| {
            node.hit_lines.iter().map(move |(index, top, rows)| {
                (
                    *index,
                    Rect {
                        x: node.rect.x,
                        y: *top,
                        width: node.rect.width,
                        height: *rows,
                    },
                )
            })
        })
    }
}

/// Run one claiming pass: `root` claims `frame`, children claim sub-rects.
pub fn layout(root: &Widget, frame: Rect) -> Layout {
    let mut result = Layout::default();
    claim(root, frame, &mut result);
    result
}

/// A line is a hit target exactly when it belongs to an expandable transcript
/// row; the disclosure toggle owns the rect that line claimed.
fn is_hit_target(line: &RenderedTranscriptLine) -> bool {
    line.row_id.is_some()
}

fn claim(widget: &Widget, offered: Rect, result: &mut Layout) -> Rect {
    match widget {
        Widget::Marked(key, inner) => {
            let mark = result.nodes.len();
            let used = claim(inner, offered, result);
            if let Some(node) = result.nodes.get_mut(mark) {
                node.key = Some(*key);
            }
            used
        }
        Widget::Stack { axis, children } => {
            let rect = claim_stack(*axis, children, offered, result);
            result.nodes.push(NodeLayout {
                key: None,
                rect,
                hit_lines: Vec::new(),
            });
            rect
        }
        Widget::Text { lines: _ }
        | Widget::Completions { rows: _ }
        | Widget::Composer { .. }
        | Widget::DialogCard { lines: _ } => {
            result.nodes.push(NodeLayout {
                key: None,
                rect: offered,
                hit_lines: Vec::new(),
            });
            offered
        }
        Widget::Rule => {
            let rect = Rect {
                height: 1,
                ..offered
            };
            result.nodes.push(NodeLayout {
                key: None,
                rect,
                hit_lines: Vec::new(),
            });
            rect
        }
        Widget::Viewport { lines } => {
            let window = viewport_window(lines, offered);
            let hit_lines = viewport_hit_lines(lines, &window, offered);
            result.nodes.push(NodeLayout {
                key: None,
                rect: offered,
                hit_lines,
            });
            offered
        }
    }
}

/// One child slot resolved against the parent's offer. `index` is the child's
/// position in paint order; `claim` is filled in by the passes below.
struct Slot {
    index: usize,
    natural: usize,
    floor: usize,
    weight: usize,
    cap: Option<usize>,
}

fn resolve_track(
    track: &Track,
    widget: &Widget,
    parent_width: usize,
    axis: Axis,
    index: usize,
) -> Option<Slot> {
    match track {
        Track::Natural => Some(Slot {
            index,
            natural: natural_size(widget, parent_width, axis),
            floor: 0,
            weight: 0,
            cap: None,
        }),
        Track::Flex { weight, min } => Some(Slot {
            index,
            natural: 0,
            floor: *min,
            weight: (*weight).max(1),
            cap: None,
        }),
        Track::Max { cap, track } => {
            let mut inner = resolve_track(track, widget, parent_width, axis, index)?;
            inner.cap = Some((*cap).max(1));
            Some(inner)
        }
        Track::Side { min_width, track } => {
            if parent_width < *min_width {
                None
            } else {
                resolve_track(track, widget, parent_width, axis, index)
            }
        }
    }
}

fn claim_stack(
    axis: Axis,
    children: &[(Track, Widget)],
    offered: Rect,
    result: &mut Layout,
) -> Rect {
    if children.is_empty() {
        return Rect::default();
    }
    let (main_size, cross) = match axis {
        Axis::Column => (offered.height, offered.width),
        Axis::Row => (offered.width, offered.height),
    };
    if main_size == 0 || cross == 0 {
        return Rect::default();
    }

    // Resolve slots. A `Side` track the parent's width cannot afford drops
    // out of this pass entirely; a `Max` cap passes its inner claim through.
    let mut slots: Vec<Slot> = children
        .iter()
        .enumerate()
        .filter_map(|(index, (track, widget))| resolve_track(track, widget, cross, axis, index))
        .collect();

    // Pass 1: flexible floors are reserved first.
    let mut remaining = main_size;
    for slot in &mut slots {
        let floor = slot.floor.min(remaining);
        slot.floor = floor;
        remaining -= floor;
    }
    // Pass 2: natural tracks claim from the end of the stack inward, so
    // bottom chrome is anchored to the bottom of the frame. Slots dropped by
    // a `Side` gate claim nothing at all and are left out of the pass.
    let mut claims: Vec<Option<usize>> = vec![None; children.len()];
    for slot in slots.iter().rev() {
        if slot.weight == 0 {
            let claim = slot
                .natural
                .min(remaining)
                .min(slot.cap.unwrap_or(usize::MAX));
            claims[slot.index] = Some(claim);
            remaining -= claim;
        }
    }
    // Pass 3: what remains is shared by weight among the flexible children,
    // on top of their reserved floors. The last flexible child absorbs the
    // integer-division remainder so the frame is fully claimed.
    let total_weight: usize = slots.iter().map(|slot| slot.weight).sum();
    let flex_slots: Vec<&Slot> = slots.iter().filter(|slot| slot.weight > 0).collect();
    if let Some((last, rest)) = flex_slots.split_last() {
        let mut distributed = 0usize;
        for slot in rest {
            let share = remaining * slot.weight / total_weight;
            claims[slot.index] = Some((slot.floor + share).min(slot.cap.unwrap_or(usize::MAX)));
            distributed += share;
        }
        claims[last.index] =
            Some((last.floor + (remaining - distributed)).min(last.cap.unwrap_or(usize::MAX)));
    }

    // Assign rects along the main axis in paint order, clamping the tail to
    // the offered box, then recurse depth-first into each child.
    let mut offset = 0usize;
    let mut assigned: Vec<Option<Rect>> = vec![None; children.len()];
    for (index, claim) in claims.iter().enumerate() {
        let Some(claim) = claim else { continue };
        let rect = match axis {
            Axis::Column => Rect {
                x: offered.x,
                y: offered.y + offset,
                width: offered.width,
                height: *claim,
            },
            Axis::Row => Rect {
                x: offered.x + offset,
                y: offered.y,
                width: *claim,
                height: offered.height,
            },
        };
        let visible = match axis {
            Axis::Column => rect.height.min(offered.bottom().saturating_sub(rect.y)),
            Axis::Row => rect.width.min(offered.right().saturating_sub(rect.x)),
        };
        assigned[index] = Some(match axis {
            Axis::Column => Rect {
                height: visible,
                ..rect
            },
            Axis::Row => Rect {
                width: visible,
                ..rect
            },
        });
        offset += visible;
    }
    for ((_, widget), rect) in children.iter().zip(assigned) {
        if let Some(rect) = rect {
            claim(widget, rect, result);
        }
    }

    match axis {
        Axis::Column => Rect {
            x: offered.x,
            y: offered.y,
            width: offered.width,
            height: offset,
        },
        Axis::Row => Rect {
            x: offered.x,
            y: offered.y,
            width: offset,
            height: offered.height,
        },
    }
}

/// The natural main-axis extent of a widget at the given cross extent: for a
/// `Column` parent the extent is wrapped rows; for a `Row` parent it is
/// display columns.
pub fn natural_size(widget: &Widget, cross: usize, axis: Axis) -> usize {
    match axis {
        Axis::Column => natural_height(widget, cross),
        Axis::Row => natural_width(widget),
    }
}

fn natural_height(widget: &Widget, cross: usize) -> usize {
    let width = cross.max(1);
    match widget {
        Widget::Stack { axis, children } => {
            let naturals = children.iter().map(|(track, child)| match track {
                Track::Natural => natural_size(child, width, *axis),
                Track::Flex { min, .. } => *min,
                Track::Max { cap, track } => {
                    (*cap).min(flex_or_natural(track, child, width, *axis))
                }
                Track::Side { track, .. } => flex_or_natural(track, child, width, *axis),
            });
            match axis {
                Axis::Column => naturals.sum(),
                Axis::Row => naturals.max().unwrap_or(0),
            }
        }
        Widget::Text { lines } => lines.iter().map(|line| physical_rows(line, width)).sum(),
        Widget::DialogCard { lines } => lines.iter().map(|line| physical_rows(line, width)).sum(),
        Widget::Rule => 1,
        Widget::Completions { rows } => rows.len(),
        Widget::Composer { input_lines, ghost } => {
            input_line_physical_rows_with_ghost(input_lines, width, ghost.as_deref())
                .into_iter()
                .sum()
        }
        Widget::Viewport { .. } => 0,
        Widget::Marked(_, inner) => natural_height(inner, cross),
    }
}

/// Natural extent of a child along a `Row` parent's main axis: the widest
/// content it needs, in display columns.
fn natural_width(widget: &Widget) -> usize {
    match widget {
        Widget::Stack { axis, children } => {
            let naturals = children.iter().map(|(track, child)| match track {
                Track::Natural => natural_width(child),
                Track::Flex { min, .. } => *min,
                Track::Max { cap, track } => (*cap).min(natural_width_or_flex(track, child)),
                Track::Side { track, .. } => natural_width_or_flex(track, child),
            });
            match axis {
                Axis::Column => naturals.max().unwrap_or(0),
                Axis::Row => naturals.sum(),
            }
        }
        Widget::Text { lines } => lines
            .iter()
            .map(|line| visible_length(line))
            .max()
            .unwrap_or(0),
        Widget::DialogCard { lines } => lines
            .iter()
            .map(|line| visible_length(line))
            .max()
            .unwrap_or(0),
        Widget::Rule => 1,
        Widget::Completions { rows } => rows
            .iter()
            .map(|row| visible_length(row))
            .max()
            .unwrap_or(0),
        Widget::Composer { input_lines, ghost } => {
            input_lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let ghost_width = if input_lines.len() == 1 && index == 0 {
                        ghost.as_deref().map(visible_length).unwrap_or(0)
                    } else {
                        0
                    };
                    visible_length(line) + ghost_width
                })
                .max()
                .unwrap_or(0)
                + 2
        } // `❯ ` prompt / continuation indent
        Widget::Viewport { .. } => 0,
        Widget::Marked(_, inner) => natural_width(inner),
    }
}

fn flex_or_natural(track: &Track, widget: &Widget, width: usize, axis: Axis) -> usize {
    match track {
        Track::Flex { min, .. } => *min,
        other => resolve_track(other, widget, width, axis, 0)
            .map(|slot| slot.natural)
            .unwrap_or(0),
    }
}

fn natural_width_or_flex(track: &Track, widget: &Widget) -> usize {
    match track {
        Track::Flex { min, .. } => *min,
        Track::Natural => natural_width(widget),
        Track::Max { cap, track } => (*cap).min(natural_width_or_flex(track, widget)),
        Track::Side { track, .. } => natural_width_or_flex(track, widget),
    }
}

/// The bottom window of `lines` whose physical rows fit the viewport rect.
fn viewport_window(lines: &[RenderedTranscriptLine], rect: Rect) -> Option<Range<usize>> {
    if lines.is_empty() || rect.height == 0 {
        return None;
    }
    let width = rect.width.max(1);
    let mut remaining = rect.height;
    let mut start = lines.len();
    for index in (0..lines.len()).rev() {
        let rows = physical_rows(&lines[index].text, width);
        if rows > remaining {
            break;
        }
        remaining -= rows;
        start = index;
    }
    (start < lines.len()).then_some(start..lines.len())
}

/// Hit rects for a viewport's visible window: each hit-target line claims its
/// own sub-rect inside the viewport box, clipped to the box.
fn viewport_hit_lines(
    lines: &[RenderedTranscriptLine],
    window: &Option<Range<usize>>,
    rect: Rect,
) -> Vec<(usize, usize, usize)> {
    let Some(window) = window else {
        return Vec::new();
    };
    let mut hit_lines = Vec::new();
    let mut top = rect.y;
    for (offset, line) in lines[window.clone()].iter().enumerate() {
        let rows = physical_rows(&line.text, rect.width.max(1));
        let visible_rows = rows.min(rect.bottom().saturating_sub(top));
        if is_hit_target(line) && visible_rows > 0 {
            hit_lines.push((window.start + offset, top, visible_rows));
        }
        top += rows;
        if top >= rect.bottom() {
            break;
        }
    }
    hit_lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: usize, height: usize) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    fn marked(key: u16, widget: Widget) -> Widget {
        Widget::Marked(key, Box::new(widget))
    }

    #[test]
    fn test_row_stack_places_two_children_side_by_side_without_flattening() {
        // INVARIANT (#805): a `Row` parent must place its children side by
        // side — a narrow list track and a transcript track share the same
        // rows. A layout that only stacks vertically would give the second
        // child rows below the first, so its rect's top would be the first
        // child's bottom.
        const LIST: u16 = 10;
        const BODY: u16 = 11;
        let tree = Widget::Stack {
            axis: Axis::Row,
            children: vec![
                (
                    Track::Natural,
                    marked(
                        LIST,
                        Widget::Text {
                            lines: vec!["alpha".into(), "beta".into(), "gamma".into()],
                        },
                    ),
                ),
                (
                    Track::Flex { weight: 1, min: 0 },
                    marked(
                        BODY,
                        Widget::Text {
                            lines: vec!["body".into()],
                        },
                    ),
                ),
            ],
        };
        let layout = layout(&tree, frame(80, 3));
        let list = layout.keyed(LIST).expect("list track claimed a rect");
        let body = layout.keyed(BODY).expect("body track claimed a rect");
        assert_eq!(
            (list.x, list.width, list.height),
            (0, 5, 3),
            "the natural list track claims its text width on the left; list={list:?}"
        );
        assert_eq!(
            (body.x, body.y, body.width),
            (5, 0, 75),
            "the flexible body track must claim the columns to the RIGHT of the list, \
             not rows below it; body={body:?}"
        );
        assert_eq!(body.y, list.y, "side-by-side children share rows");
    }

    #[test]
    fn test_side_track_participates_only_when_the_width_allows_it() {
        // INVARIANT (#805/#810): grid tracks are conditional on available
        // width — a narrow frame omits the side track entirely, a wide frame
        // includes it. The rest of the layout must not move in shape.
        const SIDE: u16 = 20;
        const MAIN: u16 = 21;
        let tree_for = |width_gate: usize| Widget::Stack {
            axis: Axis::Column,
            children: vec![
                (
                    Track::Flex { weight: 1, min: 0 },
                    marked(
                        MAIN,
                        Widget::Text {
                            lines: vec!["main".into()],
                        },
                    ),
                ),
                (
                    Track::Side {
                        min_width: width_gate,
                        track: Box::new(Track::Natural),
                    },
                    marked(
                        SIDE,
                        Widget::Text {
                            lines: vec!["rail".into()],
                        },
                    ),
                ),
            ],
        };
        let narrow = layout(&tree_for(80), frame(40, 10));
        assert!(
            narrow.keyed(SIDE).is_none(),
            "a side track gated at 80 columns must not claim anything on a 40-column frame"
        );
        let wide = layout(&tree_for(80), frame(120, 10));
        let side = wide
            .keyed(SIDE)
            .expect("a side track gated at 80 columns claims its rect on a 120-column frame");
        assert_eq!((side.width, side.height), (120, 1), "side={side:?}");
    }

    #[test]
    fn test_resize_is_a_full_layout_pass_and_sibling_rects_move_proportionally() {
        // INVARIANT (#805, successor to #266): growing or shrinking the frame
        // re-claims every rect relative to the new frame. Widgets must not
        // keep old absolute sizes, and flexible siblings split the leftover
        // by their weights at both sizes.
        const LEFT: u16 = 30;
        const RIGHT: u16 = 31;
        let tree = Widget::Stack {
            axis: Axis::Row,
            children: vec![
                (
                    Track::Flex { weight: 1, min: 0 },
                    marked(
                        LEFT,
                        Widget::Text {
                            lines: vec!["left".into()],
                        },
                    ),
                ),
                (
                    Track::Flex { weight: 3, min: 0 },
                    marked(
                        RIGHT,
                        Widget::Text {
                            lines: vec!["right".into()],
                        },
                    ),
                ),
            ],
        };
        let small = layout(&tree, frame(40, 5));
        let large = layout(&tree, frame(200, 5));
        let (left_small, right_small) = (
            small.keyed(LEFT).expect("left rect"),
            small.keyed(RIGHT).expect("right rect"),
        );
        let (left_large, right_large) = (
            large.keyed(LEFT).expect("left rect"),
            large.keyed(RIGHT).expect("right rect"),
        );
        assert_eq!(
            (left_small.width, right_small.width),
            (10, 30),
            "40 columns split 1:3; got left={left_small:?} right={right_small:?}"
        );
        assert_eq!(
            (left_large.width, right_large.width),
            (50, 150),
            "200 columns split 1:3 — widgets must re-claim on resize, not keep old sizes; \
             got left={left_large:?} right={right_large:?}"
        );
        assert_eq!(
            (left_large.width, right_large.width),
            (left_small.width * 5, right_small.width * 5),
            "proportional tracks keep their ratio across the resize"
        );
    }

    #[test]
    fn test_natural_chrome_is_anchored_to_the_bottom_and_flex_viewport_takes_the_leftover() {
        // INVARIANT (#805): root chrome allocates from the bottom — status,
        // hr, input, hr, completions — and the transcript viewport claims the
        // leftover. An empty completion pane claims zero rows and does not
        // move the chrome.
        const TRANSCRIPT: u16 = 0;
        const COMPLETIONS: u16 = 1;
        const STATUS: u16 = 5;
        let tree_for = |completion_rows: usize| Widget::Stack {
            axis: Axis::Column,
            children: vec![
                (
                    Track::Flex { weight: 1, min: 1 },
                    marked(TRANSCRIPT, Widget::Viewport { lines: Vec::new() }),
                ),
                (
                    Track::Natural,
                    marked(
                        COMPLETIONS,
                        Widget::Completions {
                            rows: vec![String::new(); completion_rows],
                        },
                    ),
                ),
                (Track::Natural, Widget::Rule),
                (
                    Track::Natural,
                    Widget::Composer {
                        input_lines: vec!["draft".into()],
                        ghost: None,
                    },
                ),
                (Track::Natural, Widget::Rule),
                (
                    Track::Natural,
                    marked(
                        STATUS,
                        Widget::Text {
                            lines: vec!["ready".into()],
                        },
                    ),
                ),
            ],
        };
        let closed = layout(&tree_for(0), frame(80, 24));
        let open = layout(&tree_for(5), frame(80, 24));
        let closed_status = closed.keyed(STATUS).expect("status rect");
        let open_status = open.keyed(STATUS).expect("status rect");
        assert_eq!(
            closed_status, open_status,
            "opening a pane of natural rows must not move the bottom chrome"
        );
        assert_eq!(
            closed
                .keyed(TRANSCRIPT)
                .expect("transcript rect")
                .height,
            20,
            "an empty pane claims zero rows, so the transcript keeps the leftover              (24 rows minus rule, composer, rule, status)"
        );
        assert_eq!(
            open.keyed(TRANSCRIPT).expect("transcript rect").height,
            15,
            "five pane rows shrink the transcript viewport, not the chrome"
        );
    }

    #[test]
    fn test_viewport_hit_lines_are_the_rects_the_depth_first_pass_claimed() {
        // INVARIANT (#805): a parent allocates a box, children claim
        // sub-rects, and hitboxes are those rects after the depth-first pass.
        // An expandable transcript row claims its own line rect inside the
        // viewport; clipped lines claim nothing.
        let header = |text: &str| RenderedTranscriptLine {
            text: text.to_string(),
            row_id: Some(RowId {
                message_id: MessageId::new(),
                path: vec![0],
            }),
            ..RenderedTranscriptLine::default()
        };
        let plain = |text: &str| RenderedTranscriptLine {
            text: text.to_string(),
            ..RenderedTranscriptLine::default()
        };
        let lines = vec![
            plain("prose"),
            header("▶ work"),
            plain("body"),
            header("▶ next"),
        ];
        let rect = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 3,
        };
        let result = layout(
            &Widget::Viewport {
                lines: lines.clone(),
            },
            rect,
        );
        let hit_lines = &result.nodes[0].hit_lines;
        // Four lines cannot fit three rows: the bottom window clips the first
        // line, so the surviving headers claim the viewport's first and third
        // rows.
        assert_eq!(
            hit_lines,
            &[(1usize, 0usize, 1usize), (3usize, 2usize, 1usize)],
            "only the expandable headers claim hit rects, at their own rows; got {hit_lines:?}"
        );
        for (index, top, rows) in hit_lines {
            assert!(
                rect.y <= *top && top + rows <= rect.bottom(),
                "hit rect escapes the viewport box"
            );
            assert_eq!(physical_rows(&lines[*index].text, 40), *rows);
        }
    }

    // ─── Display-width regressions (#934) ─────────────────────────────────────

    fn styled_emoji_line_at_eighty_columns() -> String {
        // 76 ASCII columns + 4 emoji × 2 columns = 84 visible columns.
        format!("\x1b[32m{}\x1b[0m", "x".repeat(76) + "🔥🔥🔥🔥")
    }

    #[test]
    fn test_char_display_width_emoji_measures_two_columns() {
        // #934: emoji are two terminal columns in every modern terminal; the
        // vocabulary must measure them at 2, exactly like the test-only
        // vt_oracle terminal model does.
        for emoji in ['😀', '🦀', '🔥', '🎉', '✅', '⏳', '⭐', '🟢', '🤖', '🚀'] {
            assert_eq!(
                char_display_width(emoji),
                2,
                "emoji {emoji} (U+{:04X}) must measure 2 columns",
                emoji as u32
            );
        }
    }

    #[test]
    fn test_char_display_width_east_asian_ambiguous_stays_one_column() {
        // #934 changes only wide/emoji codepoints. East-Asian-Ambiguous
        // glyphs stay 1 column by design (xterm, Western locales); Finch's
        // prompt (`❯`), status glyphs, and box drawing depend on it.
        for ambiguous in ['°', '±', '·', 'α', 'Ω', '→', '∞', '✓', '▶', '❯'] {
            assert_eq!(
                char_display_width(ambiguous),
                1,
                "East-Asian-Ambiguous {ambiguous} (U+{:04X}) must stay 1 column by design",
                ambiguous as u32
            );
        }
    }

    #[test]
    fn test_char_display_width_zero_width_marks_and_joiners_measure_zero() {
        // Combining marks, the zero-width joiner, and variation selectors add
        // no columns, so a ZWJ family emoji measures as the sum of its emoji.
        assert_eq!(char_display_width('\u{0301}'), 0, "combining acute");
        assert_eq!(char_display_width('\u{200D}'), 0, "zero-width joiner");
        assert_eq!(
            char_display_width('\u{FE0F}'),
            0,
            "emoji-presentation variation selector"
        );
        assert_eq!(
            visible_length("👨\u{200D}👩\u{200D}👧"),
            6,
            "three 2-column emoji plus zero-width joiners = 6 columns"
        );
    }

    #[test]
    fn test_visible_length_styled_emoji_line_measures_eighty_four_columns() {
        let line = styled_emoji_line_at_eighty_columns();
        assert_eq!(
            visible_length(&line),
            84,
            "76 ANSI-stripped ASCII columns + 4 emoji × 2 columns"
        );
    }

    #[test]
    fn test_physical_rows_styled_emoji_line_wraps_at_eighty_columns() {
        // #934 regression: before the fix the emoji counted 1 column each, the
        // line measured 80, and the live area claimed (and later erased) one
        // row while the terminal painted two — leaving stale rows behind.
        let line = styled_emoji_line_at_eighty_columns();
        assert_eq!(
            physical_rows(&line, 80),
            2,
            "84 visible columns wrap into 2 physical rows at width 80"
        );
        assert_eq!(
            physical_rows(&line, 90),
            1,
            "the same line fits one row on a 90-column terminal"
        );
    }

    #[test]
    fn test_physical_rows_line_of_only_emoji_measures_correctly() {
        let forty = "🎉".repeat(40);
        let forty_one = "🎉".repeat(41);
        assert_eq!(visible_length(&forty), 80, "40 emoji × 2 columns");
        assert_eq!(
            physical_rows(&forty, 80),
            1,
            "exactly 80 columns fill one row at width 80"
        );
        assert_eq!(visible_length(&forty_one), 82, "41 emoji × 2 columns");
        assert_eq!(
            physical_rows(&forty_one, 80),
            2,
            "82 columns overflow one row at width 80"
        );
    }

    #[test]
    fn test_truncate_to_columns_drops_emoji_straddling_the_boundary() {
        assert_eq!(
            visible_length(&truncate_to_columns("a🔥b", 2)),
            1,
            "the emoji needs columns 2-3, so a 2-column budget keeps only 'a'"
        );
        assert_eq!(
            truncate_to_columns("a🔥b", 4),
            "a🔥b",
            "4 columns fit the whole 4-column line"
        );
        let styled = format!("\x1b[1m{}🔥🔥", "x".repeat(78));
        let truncated = truncate_to_columns(&styled, 80);
        assert_eq!(
            truncated,
            format!("\x1b[1m{}🔥", "x".repeat(78)),
            "78 styled x's + one emoji fill 80 columns; the second emoji is dropped, \
             escape sequences copied through"
        );
        assert_eq!(visible_length(&truncated), 80);
    }

    #[test]
    fn test_claiming_boundary_emoji_line_claims_its_wrapped_rows() {
        // #934 at the claiming boundary: the live area's transcript viewport
        // and natural Text widgets measure rows with this vocabulary, so a
        // styled emoji line must claim 2 rows at 80 columns — both for height
        // and for the disclosure hit rect.
        let line = styled_emoji_line_at_eighty_columns();
        let text = Widget::Text {
            lines: vec![line.clone()],
        };
        assert_eq!(
            natural_height(&text, 80),
            2,
            "natural height of an 84-column emoji line at 80 columns is 2 rows"
        );

        let header = RenderedTranscriptLine {
            text: line,
            row_id: Some(RowId {
                message_id: MessageId::new(),
                path: vec![1],
            }),
            component_owned: true,
            ..RenderedTranscriptLine::default()
        };
        let result = layout(
            &Widget::Viewport {
                lines: vec![header],
            },
            Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 2,
            },
        );
        assert_eq!(
            result.nodes[0].hit_lines,
            vec![(0usize, 0usize, 2usize)],
            "the component-owned emoji header claims both of its physical rows"
        );
    }
}
