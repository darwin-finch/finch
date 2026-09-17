//! Claiming widget layout for the live console.
//!
//! A parent offers a box, children claim sub-rectangles, and the parent learns
//! the used size. Layout is one depth-first pass per frame over the widget
//! tree [`view_model`](super::view_model) projects from the ViewModel: on a
//! resize the pass simply runs again, so no widget keeps a frozen cell count
//! from the previous frame (#805, successor to #266). This is not React: there
//! are no signals, observers, or callbacks — a widget with nothing to show
//! claims zero rows and stays in the tree.

use std::ops::Range;

use super::accordion::RenderedTranscriptLine;
use super::input_line_physical_rows_with_ghost;
use super::shadow_buffer;

/// A rectangle in the live frame's own coordinate space (row 0 is the top of
/// the live area, column 0 the left terminal edge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Rect {
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
pub(crate) enum Track {
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
pub(crate) enum Axis {
    Column,
    Row,
}

/// The standard widget kinds. Widgets paint a ViewModel snapshot handed to
/// them as props; they never query domain state to decide whether they are
/// visible — an empty [`Widget::Completions`] simply claims zero rows.
pub(crate) enum Widget {
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
    /// Marks a subtree so the layout result can hand back its claimed rect
    /// under a stable key.
    Marked(u16, Box<Widget>),
}

/// One laid-out node, in depth-first paint order.
#[derive(Debug)]
pub(crate) struct NodeLayout {
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
pub(crate) struct Layout {
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
pub(crate) fn layout(root: &Widget, frame: Rect) -> Layout {
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
        Widget::Text { lines: _ } | Widget::Completions { rows: _ } | Widget::Composer { .. } => {
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
pub(crate) fn natural_size(widget: &Widget, cross: usize, axis: Axis) -> usize {
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
        Widget::Text { lines } => lines
            .iter()
            .map(|line| shadow_buffer::physical_rows(line, width))
            .sum(),
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
            .map(|line| shadow_buffer::visible_length(line))
            .max()
            .unwrap_or(0),
        Widget::Rule => 1,
        Widget::Completions { rows } => rows
            .iter()
            .map(|row| shadow_buffer::visible_length(row))
            .max()
            .unwrap_or(0),
        Widget::Composer { input_lines, ghost } => {
            input_lines
                .iter()
                .enumerate()
                .map(|(index, line)| {
                    let ghost_width = if input_lines.len() == 1 && index == 0 {
                        ghost
                            .as_deref()
                            .map(shadow_buffer::visible_length)
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    shadow_buffer::visible_length(line) + ghost_width
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
        let rows = shadow_buffer::physical_rows(&lines[index].text, width);
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
        let rows = shadow_buffer::physical_rows(&line.text, rect.width.max(1));
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
            row_id: Some(super::super::view_model::RowId {
                message_id: crate::cli::messages::MessageId::new(),
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
            assert_eq!(shadow_buffer::physical_rows(&lines[*index].text, 40), *rows);
        }
    }
}
