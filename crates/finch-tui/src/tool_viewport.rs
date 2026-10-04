//! Bounded compact windows for tool-use output rows.
//!
//! A completed (or streaming) tool result is presented as one semantic control:
//! its body occupies a small configured number of terminal rows, always the
//! first lines of the output, and one plain-text status row says which lines
//! are shown, how many there are, and how to open the rest. The compact window
//! has no scroll position of its own and claims no wheel or scroll key: those
//! always move the surrounding conversation, whatever the pointer or the
//! keyboard focus is on (issue #1590, a block under the pointer swallowed the
//! wheel halfway through scrolling the conversation).
//!
//! The control is reusable: it applies to every row of
//! [`NodeRole::ToolOutput`], not to one tool name. Activation (a click on the
//! window's cells, matched against the hit regions of the last painted frame,
//! or Enter/Space with the row focused by F6) opens a focused expanded surface
//! that owns its own scrolling; closing it leaves disclosure grouping and
//! focus exactly as they were.
//!
//! Permanent native scrollback is untouched by all of this: canonical commits
//! still write the fully expanded projection exactly once
//! (`commit_complete_messages`), so the copyable record stays complete.

use std::collections::HashMap;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

use super::view_model::{NodeRole, RowId};

use super::accordion::RenderedTranscriptLine;
use super::shadow_buffer;

/// How many terminal rows a tool result's compact window shows by default.
///
/// This is the configured bound of the compact presentation: a long Bash
/// result occupies this many rows, the last of them the truncation status
/// row, and nothing more, however long the output is.
pub const DEFAULT_TOOL_OUTPUT_ROWS: usize = 4;

/// Rows one wheel tick moves inside the expanded tool-result surface.
pub const WHEEL_STEP_LINES: usize = 1;

/// Rows one PageUp/PageDown moves inside the expanded tool-result surface.
pub const PAGE_STEP_LINES: usize = 4;

/// The focused surface that shows one tool result expanded.
///
/// Disclosure grouping and accordion focus are never touched by the surface,
/// so they restore by construction when it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedToolView {
    pub row_id: RowId,
    pub title: String,
    /// Index of the first body line shown.
    pub scroll: usize,
    /// Total body lines observed at the last surface draw.
    pub body_lines: usize,
}

/// The cells of one compact tool-result window, in the physical coordinates of the
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

/// Where the compact tool-result windows of the last painted frame are, so a
/// click or a focused Enter can be matched to the result it should expand.
#[derive(Debug, Default)]
pub struct ToolViewportState {
    regions: Vec<ToolViewportRegion>,
    visible_kinds: HashMap<RowId, NodeRole>,
}

impl ToolViewportState {
    /// The tool result whose compact window covers the cell at `(column, row)`,
    /// if the last painted frame placed one there. Used for click-to-expand
    /// and hover only; a wheel is never matched against it.
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

    /// Rebuild the compact-window hit regions and the visible-kind cache from
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

/// Apply the compact bound to one message's projected lines.
///
/// Consecutive body lines of a `ToolOutput` row are replaced by their first
/// lines, each truncated to the terminal width, plus one plain-text status row
/// naming the visible range, the total, and how to expand the result when
/// anything was left out. Everything else passes through untouched, so this
/// bound can never widen any other row's presentation.
pub fn project(
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
        projected.extend(window(owner, body, width, budget_rows));
    }
    projected
}

/// Window one tool result's projected body lines to its first rows.
///
/// Every body line is truncated to the terminal width first, so one line
/// costs exactly one terminal row and the configured bound is a hard row
/// bound. The status row costs one of the bound rows whenever lines are left
/// out; a single-row budget shows the status alone.
fn window(
    row_id: RowId,
    body: &[RenderedTranscriptLine],
    width: usize,
    budget_rows: usize,
) -> Vec<RenderedTranscriptLine> {
    let budget_rows = budget_rows.max(1);
    let truncated = body.len() > budget_rows;
    let take = if truncated {
        budget_rows - 1
    } else {
        body.len()
    };

    let mut windowed: Vec<RenderedTranscriptLine> = body[..take]
        .iter()
        .cloned()
        .map(|mut line| {
            line.text = truncate_body_line(&line.text, width);
            line
        })
        .collect();
    if truncated {
        windowed.push(RenderedTranscriptLine {
            text: truncate_body_line(&format!("      {}", status_text(take, body.len())), width),
            body_of: Some(row_id),
            role: Some(NodeRole::ToolOutput),
            ..RenderedTranscriptLine::default()
        });
    }
    windowed
}

/// The expand affordance printed on every truncated compact window: the two
/// real ways to open the expanded view. `F6` is the key that moves keyboard
/// focus onto the result; `Enter` alone would submit the composer.
pub const EXPAND_HINT: &str = "click, or F6 then Enter, to expand";

/// Plain-text state description for one compact tool-result window: how many
/// leading lines are shown, the total, and how to expand.
fn status_text(shown: usize, total: usize) -> String {
    if shown == 0 {
        return format!("… 0 lines visible of {total} — {EXPAND_HINT}");
    }
    format!("… lines 1–{shown} of {total} — {EXPAND_HINT}")
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

/// Plain-text state line for the expanded surface footer.
pub fn expanded_status_text(start: usize, end: usize, total: usize) -> String {
    if total == 0 {
        return "empty — Esc close".to_string();
    }
    if start >= end {
        return format!("0 lines visible of {} — ↑/↓ scroll · Esc close", total);
    }
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
    let mut max_start = body.len();
    let mut available_budget = body_budget;
    for i in (0..body.len()).rev() {
        let indented = format!("      {}", body[i]);
        let rows = shadow_buffer::physical_rows(&indented, width);
        if available_budget < rows {
            break;
        }
        available_budget -= rows;
        max_start = i;
    }
    if max_start == body.len() {
        max_start = max_start.saturating_sub(1);
    }

    let start = scroll.min(max_start);
    let mut end = start;
    let mut forward_budget = body_budget;
    for line in &body[start..] {
        let indented = format!("      {}", line);
        let rows = shadow_buffer::physical_rows(&indented, width);
        if forward_budget < rows {
            if end == start {
                // Must show at least one line even if it exceeds the window bounds
                end += 1;
            }
            break;
        }
        forward_budget -= rows;
        end += 1;
    }
    let end = end.min(body.len());

    let mut lines = Vec::new();
    lines.push(truncate_body_line(&format!("      ── {} ", title), width));
    for line in &body[start..end] {
        lines.push(format!("      {}", line));
    }
    lines.push(truncate_body_line(
        &format!("      {}", expanded_status_text(start, end, body.len())),
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
    use finch_messages::{MessageRef, WorkUnit};
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
        let output_row = crate::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a tool group projects a transcript row")
            .children[0]
            .children[1]
            .id
            .clone();
        let message: MessageRef = work;
        let state = AccordionState::default();
        let projected = match crate::view_model::project_message(&message, &colors) {
            crate::view_model::ProjectedMessage::Node(node) => state.render_node(&node),
            crate::view_model::ProjectedMessage::Plain(lines) => {
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
        let state = ToolViewportState::default();
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);

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
        assert_eq!(
            status.trim(),
            "… lines 1–3 of 40 — click, or F6 then Enter, to expand",
            "INVARIANT: the status row names the two real ways to open the expanded view \
             (a click on the block; F6 to focus it, then Enter) and promises no inline \
             scrolling, which the compact window does not have"
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
    fn test_body_that_fits_the_budget_is_shown_whole_with_no_status_row() {
        // A 2-line tool result under the 4-row default budget is shown in
        // full: no truncation, and no status row offering an expansion that
        // would reveal nothing more.
        let (_row_id, projected) = projected_tool_group(2);
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        let body = body_lines_of(&bounded)
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            body,
            vec!["      line 0".to_string(), "      line 1".to_string()],
            "INVARIANT: a body that fits the budget renders with no truncation and no \
             status row; body was {body:?}"
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
        let projected = match crate::view_model::project_message(&message, &colors) {
            crate::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let bounded = project(projected, 20, DEFAULT_TOOL_OUTPUT_ROWS);

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
    fn test_empty_tool_output_creates_no_compact_window_rows() {
        // An empty result renders the tool call header only: no window rows
        // and no regions.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(true)");
        work.complete_row_with_body(call, "no output", Vec::<String>::new());
        work.set_complete();
        let colors = ColorScheme::default();
        let message: MessageRef = work;
        let projected = match crate::view_model::project_message(&message, &colors) {
            crate::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let mut state = ToolViewportState::default();
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
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
        // Two tool calls in one grouped turn each own their own window and
        // their own click target.
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
        let rows = crate::view_model::try_project_for_test(work.as_ref(), &colors)
            .unwrap()
            .children;
        let first_output = rows[0].children[1].id.clone();
        let second_output = rows[1].children[1].id.clone();
        let message: MessageRef = work;
        let projected = match crate::view_model::project_message(&message, &colors) {
            crate::view_model::ProjectedMessage::Node(node) => {
                AccordionState::default().render_node(&node)
            }
            crate::view_model::ProjectedMessage::Plain(lines) => {
                AccordionState::default().render_plain(&lines.join("\n"))
            }
        };
        let mut state = ToolViewportState::default();
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);

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

        let alpha_window = bounded
            .iter()
            .filter(|line| line.body_of.as_ref() == Some(&first_output))
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let beta_window = bounded
            .iter()
            .filter(|line| line.body_of.as_ref() == Some(&second_output))
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        assert!(
            alpha_window
                .first()
                .is_some_and(|text| text.contains("alpha 0"))
                && alpha_window
                    .last()
                    .is_some_and(|text| text.contains("lines 1–3 of 40")),
            "INVARIANT: the first control shows its own first lines and status row; \
             window was {alpha_window:?}"
        );
        assert!(
            beta_window
                .first()
                .is_some_and(|text| text.contains("beta 0"))
                && beta_window
                    .last()
                    .is_some_and(|text| text.contains("lines 1–3 of 40")),
            "INVARIANT: the second control shows its own first lines and status row; \
             window was {beta_window:?}"
        );
    }

    #[test]
    fn test_appended_output_keeps_the_first_lines_and_updates_the_total() {
        // Output arriving on a row already shown must not move the compact
        // window: it stays on the first lines and the status row reports the
        // grown total.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(stream)");
        let colors = ColorScheme::default();
        let message: MessageRef = work.clone();
        let mut windows = Vec::new();
        for total in [10, 15] {
            work.complete_row_with_body(
                call,
                "",
                (0..total).map(|n| format!("out {n}")).collect::<Vec<_>>(),
            );
            work.set_complete();
            let projected = match crate::view_model::project_message(&message, &colors) {
                crate::view_model::ProjectedMessage::Node(node) => {
                    AccordionState::default().render_node(&node)
                }
                crate::view_model::ProjectedMessage::Plain(lines) => {
                    AccordionState::default().render_plain(&lines.join("\n"))
                }
            };
            windows.push(
                project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS)
                    .iter()
                    .filter(|line| line.body_of.is_some())
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>(),
            );
        }
        for (window, total) in windows.iter().zip([10, 15]) {
            assert!(
                window.first().is_some_and(|text| text.contains("out 0"))
                    && window
                        .last()
                        .is_some_and(|text| text.contains(&format!("lines 1–3 of {total}"))),
                "INVARIANT: the compact window stays on the first lines and its status row \
                 reports the current total ({total}); window was {window:?}"
            );
        }
    }

    #[test]
    fn test_hit_regions_cover_only_the_painted_control_cells() {
        // The regions must measure physical rows exactly like the accordion's
        // disclosure regions, and only where the control's cells were painted.
        let (row_id, projected) = projected_tool_group(40);
        let mut state = ToolViewportState::default();
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
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
             ({} rows), so click X/Y dispatch cannot leak into neighbouring rows; \
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
        let bounded = project(projected, 80, DEFAULT_TOOL_OUTPUT_ROWS);
        state.rebuild_hit_regions(&bounded, 0, 80);
        assert!(!state.regions.is_empty());

        state.rebuild_hit_regions(&[], 0, 80);
        assert!(
            state.regions.is_empty(),
            "INVARIANT: a frame that painted no control cells must leave no hit regions \
             (an invisible pane cannot answer for a click)"
        );
        assert_eq!(
            state.kind_of(&row_id),
            None,
            "a row the frame did not paint cannot answer keyboard activation either"
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
    fn test_empty_expanded_surface_shows_sensible_status() {
        let body: Vec<String> = Vec::new();
        let lines = expanded_surface_lines("bash(true)", &body, 0, 80, 20);
        let joined = lines.join("\n");
        assert!(
            joined.contains("empty — Esc close"),
            "INVARIANT: an empty expanded surface shows a sensible empty status, not \
             a negative range like 'lines 1-0'; surface was:\n{joined}"
        );
        assert!(
            !joined.contains("1–0"),
            "INVARIANT: negative line ranges must not be displayed; surface was:\n{joined}"
        );
    }

    #[test]
    fn test_zero_visible_lines_shows_sensible_status() {
        // When the window truncates but the budget is so small (e.g. 1 row) that only
        // the status text is visible, the status row must say 0 lines visible instead of a negative range.
        let (_row_id, projected) = projected_tool_group(2);
        // Force the budget to 1 row.
        let bounded = project(projected, 80, 1);

        let status = body_lines_of(&bounded)
            .last()
            .map(|line| line.text.clone())
            .unwrap_or_default();
        assert!(
            status.contains("0 lines visible of 2"),
            "INVARIANT: a viewport with 0 visible body lines shows 0 lines visible instead \
             of a negative range; status was {:?}",
            status
        );
        assert!(
            !status.contains("1–0"),
            "INVARIANT: negative line ranges must not be displayed; status was {:?}",
            status
        );
    }

    #[test]
    fn test_expanded_surface_window_clamps_past_the_end() {
        let body: Vec<String> = (0..5).map(|n| format!("line {n}")).collect();
        let lines = expanded_surface_lines("bash", &body, 100, 80, 20);
        assert!(
            lines.iter().any(|l| l.contains("line 4")),
            "an out-of-range scroll clamps to the last full page, showing the last line; \
             surface was:\n{}",
            lines.join("\n")
        );
    }

    #[test]
    fn test_expanded_surface_maintains_height_when_scrolled_to_bottom() {
        let body: Vec<String> = (0..20).map(|n| format!("line {n}")).collect();
        let lines_start = expanded_surface_lines("bash", &body, 0, 80, 6);
        assert_eq!(
            lines_start.len(),
            6,
            "surface should occupy full allotted height at top"
        );

        let lines_bottom = expanded_surface_lines("bash", &body, 100, 80, 6);
        assert_eq!(
            lines_bottom.len(),
            6,
            "INVARIANT: the expanded surface must maintain its full allotted height \
             even when scrolled past the end, rather than shrinking; surface was:\n{}",
            lines_bottom.join("\n")
        );
        assert!(
            lines_bottom[4].contains("line 19"),
            "should display the end of the content; surface was:\n{}",
            lines_bottom.join("\n")
        );
    }

    #[test]

    fn test_expanded_surface_lines_are_indented_with_six_spaces() {
        let body: Vec<String> = (0..5).map(|n| format!("line {n}")).collect();
        let lines = expanded_surface_lines("bash", &body, 0, 80, 20);
        for line in lines {
            assert!(
                line.starts_with("      "),
                "INVARIANT: expanded tool output lines are indented with exactly 6 spaces; \
                 line was {:?}",
                line
            );
            assert!(
                !line.starts_with("       "),
                "INVARIANT: expanded tool output lines are indented with exactly 6 spaces, not 7; \
                 line was {:?}",
                line
            );
        }
    }
}
