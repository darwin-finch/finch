//! Bounded child viewports for tool-use output rows.
//!
//! A completed (or streaming) tool result is presented as one semantic control:
//! its body occupies a small configured number of terminal rows, exposes
//! truncation and scroll position in plain text, and owns the wheel events over
//! those rows. Wheel X/Y is matched against the hit regions of the last painted
//! frame — the same shadow-buffer ownership the accordion disclosure uses — so
//! a wheel over the control scrolls that tool result only, never the parent
//! console, and never releases mouse tracking to native scrollback.
//!
//! The control is reusable: it applies to every row of
//! [`NodeRole::ToolOutput`], not to one tool name. Activation (click
//! on the window, or Enter with the row focused) opens a focused expanded
//! surface; closing it restores the child scroll offset, disclosure grouping,
//! and focus exactly as they were.
//!
//! Permanent native scrollback is untouched by all of this: canonical commits
//! still write the fully expanded projection exactly once
//! (`commit_complete_messages`), so the copyable record stays complete.

use std::collections::HashMap;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::view_model::{NodeRole, RowId};

use super::accordion::RenderedTranscriptLine;
use super::shadow_buffer;

/// How many terminal rows a tool result's child viewport shows by default.
///
/// This is the configured bound of the compact presentation: a long Bash
/// result occupies this many body rows, one truncation/scroll status row, and
/// nothing more, however long the output is.
pub const DEFAULT_TOOL_OUTPUT_ROWS: usize = 4;

/// Rows one wheel tick moves inside a bounded tool-result viewport.
pub const WHEEL_STEP_LINES: usize = 1;

/// Rows one PageUp/PageDown moves inside a bounded tool-result viewport.
pub const PAGE_STEP_LINES: usize = 4;

/// The focused surface that shows one tool result expanded.
///
/// `saved_scroll` is the child viewport's scroll offset captured when the
/// surface opened; closing writes it back, so the compact control shows the
/// same window it did before the expansion. Disclosure grouping and accordion
/// focus are never touched by the surface, so they restore by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedToolView {
    pub row_id: RowId,
    pub title: String,
    pub saved_scroll: usize,
    pub scroll: usize,
    /// Total body lines observed at the last surface draw.
    pub body_lines: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildViewport {
    /// Index of the first body line shown in the compact window.
    pub scroll: usize,
    /// Total body lines observed the last time the row was projected.
    pub body_lines: usize,
}

/// The cells of one bounded child viewport, in the physical coordinates of the
/// last painted frame. Rebuilt with the accordion's hit regions after every
/// render and resize; terminal coordinates are never persisted as identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolViewportRegion {
    pub row_id: RowId,
    pub top: u16,
    pub bottom: u16,
    pub left: u16,
    pub right: u16,
}

impl ToolViewportRegion {
    fn contains(&self, column: u16, row: u16) -> bool {
        row >= self.top && row <= self.bottom && column >= self.left && column <= self.right
    }
}

#[derive(Debug, Default)]
pub struct ToolViewportState {
    viewports: HashMap<RowId, ChildViewport>,
    regions: Vec<ToolViewportRegion>,
    visible_kinds: HashMap<RowId, NodeRole>,
}

impl ToolViewportState {
    /// The tool result whose viewport owns the cell at `(column, row)`, if the
    /// last painted frame placed a child viewport there.
    pub fn region_at(&self, column: u16, row: u16) -> Option<&ToolViewportRegion> {
        self.regions
            .iter()
            .find(|region| region.contains(column, row))
    }

    /// Whether the given row currently presents a tool result control. Only
    /// rows painted in the last frame can answer; an unpainted row returns
    /// `None` like the accordion's `visible_expanded` cache.
    pub fn kind_of(&self, row_id: &RowId) -> Option<NodeRole> {
        self.visible_kinds.get(row_id).copied()
    }

    pub fn child_scroll(&self, row_id: &RowId) -> usize {
        self.viewports
            .get(row_id)
            .map(|viewport| viewport.scroll)
            .unwrap_or(0)
    }

    /// Scroll one child viewport by `delta` lines. Clamped to the body length
    /// observed at the last projection; returns whether anything changed.
    pub fn scroll_child(&mut self, row_id: &RowId, delta: isize) -> bool {
        let Some(viewport) = self.viewports.get_mut(row_id) else {
            return false;
        };
        let max = viewport.body_lines.saturating_sub(1);
        let next = if delta < 0 {
            viewport.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            viewport.scroll.saturating_add(delta as usize).min(max)
        };
        if next == viewport.scroll {
            return false;
        }
        viewport.scroll = next;
        true
    }

    /// Directly set a child scroll offset (restoration after closing the
    /// expanded surface).
    pub fn set_child_scroll(&mut self, row_id: &RowId, scroll: usize) {
        if let Some(viewport) = self.viewports.get_mut(row_id) {
            viewport.scroll = scroll.min(viewport.body_lines.saturating_sub(1));
        }
    }

    /// Apply the bound to one message's projected lines.
    ///
    /// Consecutive body lines of a `ToolOutput` row are replaced by the
    /// window selected by that row's child scroll offset, each line truncated
    /// to the terminal width, plus one plain-text status row exposing the
    /// visible range, the total, and the scroll/expand affordances. Everything
    /// else passes through untouched, so this bound can never widen any other
    /// row's presentation.
    pub fn project(
        &mut self,
        lines: Vec<RenderedTranscriptLine>,
        width: usize,
        budget_rows: usize,
    ) -> Vec<RenderedTranscriptLine> {
        let width = width.max(1);
        let mut projected: Vec<RenderedTranscriptLine> = Vec::with_capacity(lines.len());
        let mut index = 0;
        while index < lines.len() {
            let owner = lines[index]
                .body_of
                .clone()
                .filter(|_| lines[index].role == Some(NodeRole::ToolOutput));
            let Some(owner) = owner else {
                projected.push(lines[index].clone());
                index += 1;
                continue;
            };
            let start = index;
            while index < lines.len()
                && lines[index].body_of.as_ref() == Some(&owner)
                && lines[index].role == Some(NodeRole::ToolOutput)
            {
                index += 1;
            }
            let body = &lines[start..index];
            projected.extend(self.window(owner, body, width, budget_rows));
        }
        projected
    }

    /// Window one tool result's projected body lines.
    ///
    /// Every body line is truncated to the terminal width first, so one line
    /// costs exactly one terminal row and the configured bound is a hard row
    /// bound. The window holds the child scroll offset; the status row exposes
    /// the visible range and total in plain text.
    fn window(
        &mut self,
        row_id: RowId,
        body: &[RenderedTranscriptLine],
        width: usize,
        budget_rows: usize,
    ) -> Vec<RenderedTranscriptLine> {
        let budget_rows = budget_rows.max(1);
        let viewport = self.viewports.entry(row_id.clone()).or_default();
        viewport.body_lines = body.len();
        viewport.scroll = viewport.scroll.min(body.len().saturating_sub(1));
        let scroll = viewport.scroll;
        let raw_take = budget_rows.min(body.len() - scroll);
        let truncated_above = scroll > 0;
        let truncated_below = scroll + raw_take < body.len();
        let truncated = truncated_above || truncated_below;
        // The status row costs one of the bound rows whenever the window
        // truncates; a single-row budget shows the status alone.
        let take = if truncated {
            raw_take
                .min(budget_rows.saturating_sub(1))
                .max(if budget_rows > 1 { 1 } else { 0 })
        } else {
            raw_take
        };
        let end = scroll + take;

        let mut windowed: Vec<RenderedTranscriptLine> = Vec::new();
        windowed.extend(body[scroll..end].iter().cloned().map(|mut line| {
            line.text = truncate_body_line(&line.text, width);
            line
        }));
        if truncated {
            windowed.push(RenderedTranscriptLine {
                text: truncate_body_line(&status_text(scroll, end, body.len()), width),
                body_of: Some(row_id),
                role: Some(NodeRole::ToolOutput),
                ..RenderedTranscriptLine::default()
            });
        }
        windowed
    }

    /// Rebuild the child-viewport hit regions and the visible-kind cache from
    /// the lines of one painted frame, using the same physical-row accounting
    /// and coordinate space as the accordion's disclosure regions.
    pub fn rebuild_hit_regions(
        &mut self,
        lines: &[RenderedTranscriptLine],
        top: usize,
        width: usize,
    ) {
        self.regions.clear();
        self.visible_kinds.clear();
        let width = width.max(1);
        let mut y = top;
        let mut open: Option<(RowId, u16)> = None;
        let close_open = |open: Option<(RowId, u16)>,
                          regions: &mut Vec<ToolViewportRegion>,
                          bottom: u16,
                          width: usize| {
            if let Some((id, region_top)) = open {
                regions.push(ToolViewportRegion {
                    row_id: id,
                    top: region_top,
                    bottom,
                    left: 0,
                    right: width.saturating_sub(1) as u16,
                });
            }
        };
        for line in lines {
            let rows = shadow_buffer::physical_rows(&line.text, width) as u16;
            let owner = line
                .body_of
                .clone()
                .filter(|_| line.role == Some(NodeRole::ToolOutput));
            let continues = match (&owner, &open) {
                (Some(owner), Some((open_id, _))) => open_id == owner,
                _ => false,
            };
            if !continues {
                close_open(
                    open.take(),
                    &mut self.regions,
                    y.saturating_sub(1) as u16,
                    width,
                );
                if let Some(owner) = owner {
                    open = Some((owner, y as u16));
                }
            }
            if let Some(id) = &line.row_id {
                if let Some(role) = line.role {
                    self.visible_kinds.insert(id.clone(), role);
                }
            }
            y = y.saturating_add(rows as usize);
        }
        close_open(open, &mut self.regions, y.saturating_sub(1) as u16, width);
    }

    /// The regions of the last painted frame, for dispatch and tests.
    #[cfg(test)]
    pub fn regions(&self) -> &[ToolViewportRegion] {
        &self.regions
    }
}

/// Plain-text state description for one bounded child viewport: the visible
/// line range, the total, and the scroll/expand affordances.
fn status_text(start: usize, end: usize, total: usize) -> String {
    if total == 0 {
        return String::new();
    }
    format!(
        "… lines {}–{} of {} — ↑/↓ scroll · Enter expand",
        start + 1,
        end,
        total
    )
}

/// Truncate one body line to the terminal width so the bounded window cannot
/// grow past its row budget through wrapping. ANSI codes are preserved.
fn truncate_body_line(line: &str, width: usize) -> String {
    if shadow_buffer::visible_length(line) <= width {
        return line.to_string();
    }
    let prefix = shadow_buffer::truncate_to_columns(line, width.saturating_sub(1));
    format!("{prefix}…")
}

/// The visible line window for a fully expanded (focused) surface.
pub fn expanded_window(scroll: usize, total: usize, max_rows: usize) -> (usize, usize) {
    let max_rows = max_rows.max(1);
    let start = scroll.min(total.saturating_sub(1));
    let end = (start + max_rows).min(total);
    (start, end)
}

/// Plain-text state line for the expanded surface footer.
pub fn expanded_status_text(start: usize, end: usize, total: usize) -> String {
    format!(
        "lines {}–{} of {} — ↑/↓ scroll · Esc close",
        start + 1,
        end,
        total
    )
}

/// Render the focused expanded surface for one tool result as plain text.
///
/// Layout: a title bar naming the tool call, the scrolled body window, and a
/// footer carrying the scroll position, total, and the close affordance. The
/// surface is bounded to `max_rows`; a body line longer than the terminal
/// wraps, so the footer may sit a row lower without the bound being exceeded
/// (the frame planner measures physical rows, never trusting this count).
pub fn expanded_surface_lines(
    title: &str,
    body: &[String],
    scroll: usize,
    width: usize,
    max_rows: usize,
) -> Vec<String> {
    let width = width.max(1);
    let max_rows = max_rows.max(2);
    let body_budget = max_rows.saturating_sub(2);
    let (start, end) = expanded_window(scroll, body.len(), body_budget);
    let mut lines = Vec::new();
    lines.push(truncate_body_line(&format!("── {} ", title), width));
    for line in &body[start..end] {
        lines.push(line.clone());
    }
    lines.push(truncate_body_line(
        &expanded_status_text(start, end, body.len()),
        width,
    ));
    lines
}

/// Wheel delta for crossterm wheel events over a tool-result control.
/// `None` means the event is not a vertical wheel tick and stays unclaimed.
pub fn wheel_delta(kind: MouseEventKind) -> Option<isize> {
    match kind {
        MouseEventKind::ScrollUp => Some(-(WHEEL_STEP_LINES as isize)),
        MouseEventKind::ScrollDown => Some(WHEEL_STEP_LINES as isize),
        _ => None,
    }
}

/// Whether this mouse event is a left-click on a tool-result control.
pub fn is_left_click(mouse: &MouseEvent) -> bool {
    matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::messages::{MessageRef, WorkUnit};
    use finch_theme::ColorScheme;
    use std::sync::Arc;

    use super::super::accordion::AccordionState;

    /// A completed tool group whose Bash-like result produced `lines` lines of
    /// output, projected by the real accordion renderer. Completed output with
    /// an empty summary defaults to expanded, so the body lines are part of
    /// every default projection — the shape the bound must control.
    fn projected_tool_group(lines: usize) -> (RowId, Vec<RenderedTranscriptLine>) {
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(build)");
        work.complete_row_with_body(
            call,
            "",
            (0..lines).map(|n| format!("line {n}")).collect::<Vec<_>>(),
        );
        work.set_complete();
        let colors = ColorScheme::default();
        let output_row = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a tool group projects a transcript row")
            .children[0]
            .children[1]
            .id
            .clone();
        let message: MessageRef = work;
        let state = AccordionState::default();
        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => state.render_node(&node),
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                state.render_plain(&lines.join("\n"))
            }
        };
        (output_row, projected)
    }

    fn body_lines_of(lines: &[RenderedTranscriptLine]) -> Vec<&RenderedTranscriptLine> {
        lines.iter().filter(|line| line.body_of.is_some()).collect()
    }

    #[test]
    fn test_project_bounds_tool_output_to_the_configured_row_budget() {
        // INVARIANT: a long tool result occupies the configured number of
        // body rows plus at most one plain-text status row, whatever the
        // output length; the status row names the visible range and total
        // (bound 4, output 40).
        let (row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);

        let body = body_lines_of(&bounded);
        assert!(
            body.len() <= DEFAULT_TOOL_OUTPUT_ROWS,
            "INVARIANT: the tool result control must stay within the configured bound \
             ({} rows total: window + status row) but projected {} body rows for a \
             40-line output; bounded projection was:\n{}",
            DEFAULT_TOOL_OUTPUT_ROWS,
            body.len(),
            bounded
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        );
        let status = body
            .last()
            .map(|line| line.text.clone())
            .unwrap_or_default();
        assert!(
            status.contains("lines 1–3 of 40"),
            "INVARIANT: the status row must expose the visible range and total in plain \
             text (lines 1–3 of 40: three window rows plus the status row inside the \
             4-row bound and 40 output lines); status was {status:?}"
        );
        assert!(
            !bounded.iter().any(|line| line.text.contains("line 39")),
            "INVARIANT: output past the bound must not leak into the compact projection"
        );
        // The control still answers for its own identity.
        assert_eq!(
            state.kind_of(&row_id),
            None,
            "kind cache is only populated by the hit-region rebuild, not by projection"
        );
    }

    #[test]
    fn test_child_scroll_moves_the_window_within_the_bound() {
        // Wheel/keyboard scrolling slides the window through the body without
        // ever widening the bound; the status row tracks the new position.
        let (row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let _ = state.project(projected.clone(), 80, DEFAULT_TOOL_OUTPUT_ROWS);

        assert!(
            state.scroll_child(&row_id, 1),
            "scrolling down must move the child viewport"
        );
        assert_eq!(state.child_scroll(&row_id), 1);
        let rescrolled = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        let body = body_lines_of(&rescrolled)
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        assert!(
            body[0].contains("line 1"),
            "INVARIANT: after one scroll tick the window starts at line 1; body was {body:?}"
        );
        assert!(
            !body
                .iter()
                .any(|text| text.contains("line 0\n") || *text == "line 0"),
            "line 0 scrolled out of the window; body was {body:?}"
        );
        assert!(
            body.last()
                .is_some_and(|status| status.contains("lines 2–4 of 40")),
            "INVARIANT: the status row must report the scrolled range (lines 2–4 of 40); \
             body was {body:?}"
        );
        // Clamped at the end of the body: the window can slide until the last
        // body line is its first visible line.
        for _ in 0..100 {
            state.scroll_child(&row_id, 1);
        }
        assert_eq!(
            state.child_scroll(&row_id),
            39,
            "INVARIANT: scrolling clamps so the window never passes the end of the body"
        );
    }

    #[test]
    fn test_scroll_up_never_goes_above_the_first_line() {
        let (_row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let row_id = projected
            .iter()
            .find(|line| line.body_of.is_some())
            .and_then(|line| line.body_of.clone())
            .unwrap();
        let _ = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        assert!(
            !state.scroll_child(&row_id, -5),
            "scrolling up from the first line must be a no-op, not a wraparound"
        );
        state.scroll_child(&row_id, 2);
        state.scroll_child(&row_id, -10);
        assert_eq!(
            state.child_scroll(&row_id),
            0,
            "scrolling up clamps at the first line"
        );
    }

    #[test]
    fn test_wrapped_line_is_truncated_to_one_row_and_exposes_truncation() {
        // A body line wider than the terminal must not wrap past the bound:
        // truncation is exposed on the line itself, keeping one row per line.
        let wide = "x".repeat(200);
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(cat)");
        work.complete_row_with_body(call, "", vec![wide, "tail".to_string()]);
        work.set_complete();
        let colors = ColorScheme::default();
        let message: MessageRef = work;
        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 20, DEFAULT_TOOL_OUTPUT_ROWS);

        for line in body_lines_of(&bounded) {
            let rows = shadow_buffer::physical_rows(&line.text, 20);
            assert_eq!(
                rows, 1,
                "INVARIANT: a bounded tool-output row must stay one physical row at the \
                 terminal width; {:?} measured {} rows",
                line.text, rows
            );
        }
        let truncated = body_lines_of(&bounded)
            .iter()
            .find(|line| line.text.contains("x"))
            .map(|line| line.text.clone())
            .expect("the wide body line is part of the window");
        assert!(
            truncated.ends_with('…'),
            "INVARIANT: a truncated body line must say so with an ellipsis, not lose the \
             tail silently; line was {truncated:?}"
        );
    }

    #[test]
    fn test_empty_tool_output_creates_no_child_viewport_rows() {
        // An empty result renders the tool call header only: no viewport rows,
        // no regions, and scrolling it is a no-op.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(true)");
        work.complete_row_with_body(call, "no output", Vec::<String>::new());
        work.set_complete();
        let colors = ColorScheme::default();
        let message: MessageRef = work;
        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        assert!(
            body_lines_of(&bounded).is_empty(),
            "INVARIANT: an empty tool result must not produce viewport rows; projection was:\n{}",
            bounded
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        );
        state.rebuild_hit_regions(&bounded, 0, 80);
        assert!(
            state.regions.is_empty(),
            "INVARIANT: an empty tool result must not register a hit region — a click on \
             nothing must not expand a phantom control; regions were {:?}",
            state.regions
        );
    }

    #[test]
    fn test_adjacent_tool_controls_window_independently() {
        // Two tool calls in one grouped turn each own their own window: wheel
        // scrolling the first must leave the second exactly where it was.
        let work = Arc::new(WorkUnit::new("Tools"));
        let first = work.add_row("bash(first)");
        work.complete_row_with_body(
            first,
            "",
            (0..40).map(|n| format!("alpha {n}")).collect::<Vec<_>>(),
        );
        let second = work.add_row("bash(second)");
        work.complete_row_with_body(
            second,
            "",
            (0..40).map(|n| format!("beta {n}")).collect::<Vec<_>>(),
        );
        work.set_complete();
        let colors = ColorScheme::default();
        let rows = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .unwrap()
            .children;
        let first_output = rows[0].children[1].id.clone();
        let second_output = rows[1].children[1].id.clone();
        let message: MessageRef = work;
        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);

        state.rebuild_hit_regions(&bounded, 0, 80);
        let regions = state.regions.clone();
        assert_eq!(
            regions.len(),
            2,
            "INVARIANT: two adjacent tool results must register two independently \
             addressable controls; regions were {regions:?}"
        );
        assert_eq!(regions[0].row_id, first_output);
        assert_eq!(regions[1].row_id, second_output);

        state.scroll_child(&first_output, 3);
        let rescrolled = state.project(
            match crate::cli::tui::view_model::project_message(&message, &colors) {
                crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                    AccordionState::default().render_node(&node)
                }
                crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                    AccordionState::default().render_plain(&lines.join("\n"))
                }
            },
            80,
            DEFAULT_TOOL_OUTPUT_ROWS,
        );
        let alpha_window = rescrolled
            .iter()
            .filter(|line| line.body_of.as_ref() == Some(&first_output))
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let beta_window = rescrolled
            .iter()
            .filter(|line| line.body_of.as_ref() == Some(&second_output))
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        assert!(
            alpha_window
                .first()
                .is_some_and(|text| text.contains("alpha 3")),
            "INVARIANT: the scrolled control's window starts at its offset (alpha 3); \
             window was {alpha_window:?}"
        );
        assert!(
            beta_window
                .first()
                .is_some_and(|text| text.contains("beta 0")),
            "INVARIANT: the unscrolled control's window must not move (beta 0); \
             window was {beta_window:?}"
        );
    }

    #[test]
    fn test_interleaved_appends_preserve_child_scroll_offsets() {
        // Output arriving while a result is open (or between wheel ticks) must
        // not reset the user's position; the window slides with the body.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(stream)");
        work.complete_row_with_body(
            call,
            "",
            (0..10).map(|n| format!("out {n}")).collect::<Vec<_>>(),
        );
        work.set_complete();
        let colors = ColorScheme::default();
        let message: MessageRef = work.clone();
        let mut state = ToolViewportState::default();

        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let row_id = projected
            .iter()
            .find(|line| line.body_of.is_some())
            .and_then(|line| line.body_of.clone())
            .unwrap();
        let _ = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        state.scroll_child(&row_id, 2);
        assert_eq!(state.child_scroll(&row_id), 2);

        // Interleaved update: five more lines arrive on the same row.
        work.complete_row_with_body(
            call,
            "",
            (0..15).map(|n| format!("out {n}")).collect::<Vec<_>>(),
        );
        let projected = match crate::cli::tui::view_model::project_message(&message, &colors) {
            crate::cli::tui::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::cli::tui::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let rescrolled = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        assert_eq!(
            state.child_scroll(&row_id),
            2,
            "INVARIANT: an interleaved tool update must not reset the child scroll offset \
             (was 2, appended 5 lines)"
        );
        let window = rescrolled
            .iter()
            .filter(|line| line.body_of.is_some())
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        assert!(
            window.first().is_some_and(|text| text.contains("out 2")),
            "INVARIANT: the window still starts at the user's offset (out 2); window was \
             {window:?}"
        );
        let status = window.last().cloned().unwrap_or_default();
        assert!(
            status.contains("of 15"),
            "INVARIANT: the status row reflects the grown body (… of 15); status was \
             {status:?}"
        );
    }

    #[test]
    fn test_hit_regions_cover_only_the_painted_control_cells() {
        // The regions must measure physical rows exactly like the accordion's
        // disclosure regions, and only where the control's cells were painted.
        let (row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        let top = 7;
        state.rebuild_hit_regions(&bounded, top, 80);

        let expected_rows = body_lines_of(&bounded).len();
        let expected_top = top
            + bounded
                .iter()
                .take_while(|line| line.body_of.is_none())
                .map(|line| shadow_buffer::physical_rows(&line.text, 80))
                .sum::<usize>();
        let region = state
            .regions
            .iter()
            .find(|region| region.row_id == row_id)
            .expect("the painted control registers a region");
        assert_eq!(
            region.bottom.saturating_sub(region.top) + 1,
            expected_rows as u16,
            "INVARIANT: the hit region covers exactly the painted control rows \
             ({} rows), so wheel X/Y dispatch cannot leak into neighbouring rows; \
             region was {region:?}",
            expected_rows
        );
        assert_eq!(
            region.top, expected_top as u16,
            "INVARIANT: the region starts at the first painted control cell, not at the \
             frame top ({}); region was {region:?}",
            expected_top
        );
        // A cell outside the region does not dispatch to this control.
        assert!(state.region_at(0, top.saturating_sub(1) as u16).is_none());
        assert!(state.region_at(79, region.bottom + 1).is_none());
        assert!(state.region_at(0, region.top).is_some());
    }

    #[test]
    fn test_unpainted_frame_registers_no_regions_or_kinds() {
        // The ≤3-row live path rebuilds from an empty slice: no phantom
        // interactive pane may survive a frame that painted nothing.
        let (row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let bounded = state.project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        state.rebuild_hit_regions(&bounded, 0, 80);
        assert!(!state.regions.is_empty());

        state.rebuild_hit_regions(&[], 0, 80);
        assert!(
            state.regions.is_empty(),
            "INVARIANT: a frame that painted no control cells must leave no hit regions \
             (an invisible pane cannot answer for a wheel)"
        );
        assert_eq!(
            state.kind_of(&row_id),
            None,
            "a row the frame did not paint cannot answer keyboard scroll either"
        );
    }

    #[test]
    fn test_expanded_surface_lines_are_bounded_and_plain_text() {
        let body: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let lines = expanded_surface_lines("bash(build) — complete", &body, 5, 80, 20);
        assert!(
            lines.len() <= 20,
            "INVARIANT: the expanded surface is bounded to the requested rows; produced {}",
            lines.len()
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains('\x1b'),
            "INVARIANT: the expanded surface is plain text so raw/no-colour terminals and \
             assistive readers see the same words; surface was:\n{joined}"
        );
        assert!(
            joined.contains("bash(build)"),
            "the title names the tool call; surface was:\n{joined}"
        );
        assert!(
            lines[1].contains("line 5"),
            "the window starts at the scroll offset (line 5); surface was:\n{joined}"
        );
        assert!(
            lines
                .last()
                .is_some_and(|footer| footer.contains("lines 6–23 of 40")),
            "INVARIANT: the footer reports the scroll position, total, and how to close; \
             footer was {:?}",
            lines.last()
        );
    }

    #[test]
    fn test_expanded_surface_window_clamps_past_the_end() {
        let body: Vec<String> = (0..5).map(|n| format!("line {n}")).collect();
        let lines = expanded_surface_lines("bash", &body, 100, 80, 20);
        assert!(
            lines[1].contains("line 4"),
            "an out-of-range scroll clamps to the last line instead of showing a blank \
             surface; surface was:\n{}",
            lines.join("\n")
        );
    }
}
