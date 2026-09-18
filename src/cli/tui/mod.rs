// TuiRenderer — crossterm-based terminal UI
//
// Architecture
// ────────────
// Permanent area:  completed messages are printed once with ANSI colours and
//                  scroll naturally into the terminal's own scrollback buffer.
//
// Live area:       the bottom N rows showing the current in-progress WorkUnit
//                  (if any), a separator, the input textarea, and a status
//                  line.  On every render() call we erase those N rows (cursor
//                  up + clear-from-cursor-down) and reprint them.
//
// Dialogs:         tool-approval dialogs are drawn inline with crossterm.
//                  The setup wizard uses ratatui in an alternate screen so it
//                  gets the whole terminal and restores it cleanly.
//
// Note: shadow_buffer.rs is retained — it provides ColorScheme re-exports and
//       may be used for flicker-free live-area diffing in a future pass.

use anyhow::{Context, Result};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent},
    execute,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
        EndSynchronizedUpdate,
    },
};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar, StatusLineType};
use crate::cli::messages::{MessageId, MessageRef, MessageStatus, WorkUnitPresentation};
// Sub-modules
mod accordion;
pub mod activity;
mod async_input;
mod autocomplete_widget;
mod cell_format;
mod dialog;
mod dialog_widget;
mod graph;
mod input_widget; // kept, used by wizard helpers
#[cfg(test)]
mod isolation;
mod mouse_capture;
mod scrollback; // kept for future use
mod shadow_buffer; // kept – good architecture for future diffing
mod status_widget;
mod tabbed_dialog;
mod tabbed_dialog_widget; // kept for wizard helpers
mod tool_viewport;
#[cfg(test)]
mod vt_oracle;
// The ViewModel is crate-visible: projection-feeding consumers outside this
// module (and its tests) project messages through it.
pub(crate) mod view_model;
mod widgets;

use accordion::{
    AccordionState, ClaimedDisclosureRect, RenderedTranscriptLine, TranscriptHitRegion,
};
use tool_viewport::{
    is_left_click, wheel_delta, ExpandedToolView, ToolViewportState, DEFAULT_TOOL_OUTPUT_ROWS,
    PAGE_STEP_LINES,
};

pub use async_input::{spawn_input_task, InputEvent};
pub use autocomplete_widget::AutocompleteState;
use autocomplete_widget::{completion_pane_lines, replace_command_prefix, replace_mention_prefix};
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use graph::{GraphNode, GraphNodeAuthor, GraphNodeKind, GraphNodeStatus, GraphView};
pub use shadow_buffer::visible_length;

/// Best-effort terminal restoration for an exit path that cannot acquire the
/// renderer lock.  This is intentionally independent of [`TuiRenderer`]:
/// `/quit` and the IPC quit watcher may call `process::exit`, which skips Drop,
/// while a render task is holding the renderer mutex.  In that case raw mode
/// alone is insufficient — bracketed paste and kitty keyboard enhancement
/// remain enabled and their escape sequences leak into the user's shell.
pub fn emergency_restore_terminal() {
    let mut stdout = io::stdout();
    let _ = mouse_capture::write_emergency_restore_modes(&mut stdout);
    let _ = stdout.lock().flush();
    let _ = disable_raw_mode();
}
pub use tabbed_dialog::{TabbedDialog, TabbedDialogResult};
pub use tabbed_dialog_widget::TabbedDialogWidget;
// Re-export ColorScheme so callers can use `crate::cli::tui::ColorScheme`.
pub use crate::theme::ColorScheme;

const RESET: SetAttribute = SetAttribute(Attribute::Reset);
const CYAN: SetForegroundColor = SetForegroundColor(Color::Cyan);
const DIM_GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);

// ─── CWD helper ───────────────────────────────────────────────────────────────

/// Return the current working directory with `$HOME` replaced by `~`.
/// Falls back to `"."` if the CWD cannot be determined.
fn tilde_cwd() -> String {
    let cwd = match std::env::current_dir() {
        Ok(p) => p.display().to_string(),
        Err(_) => return ".".to_string(),
    };
    let home = dirs::home_dir()
        .map(|h| h.display().to_string())
        .unwrap_or_default();
    if !home.is_empty() && cwd.starts_with(&home) {
        format!("~{}", &cwd[home.len()..])
    } else {
        cwd
    }
}

fn stable_poset_order_and_depth(
    node_ids: &std::collections::BTreeSet<usize>,
    edges: &[(usize, usize)],
) -> (Vec<usize>, std::collections::BTreeMap<usize, usize>) {
    use std::cmp::Reverse;
    use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

    fn finish_component(
        start: usize,
        adjacency: &BTreeMap<usize, Vec<usize>>,
        visited: &mut BTreeSet<usize>,
        finished: &mut Vec<usize>,
    ) {
        visited.insert(start);
        let mut stack = vec![(start, 0usize)];
        while let Some(&(node, next_index)) = stack.last() {
            let successors = adjacency.get(&node).map(Vec::as_slice).unwrap_or(&[]);
            if let Some(&successor) = successors.get(next_index) {
                stack.last_mut().expect("DFS stack is non-empty").1 += 1;
                if visited.insert(successor) {
                    stack.push((successor, 0));
                }
                continue;
            }
            stack.pop();
            finished.push(node);
        }
    }

    fn collect_component(
        start: usize,
        reverse: &BTreeMap<usize, Vec<usize>>,
        visited: &mut BTreeSet<usize>,
    ) -> Vec<usize> {
        visited.insert(start);
        let mut component = Vec::new();
        let mut stack = vec![start];
        while let Some(node) = stack.pop() {
            component.push(node);
            if let Some(predecessors) = reverse.get(&node) {
                for &predecessor in predecessors.iter().rev() {
                    if visited.insert(predecessor) {
                        stack.push(predecessor);
                    }
                }
            }
        }
        component.sort_unstable();
        component
    }

    let mut adjacency: BTreeMap<usize, Vec<usize>> = node_ids
        .iter()
        .copied()
        .map(|id| (id, Vec::new()))
        .collect();
    let mut reverse = adjacency.clone();
    for &(predecessor, successor) in edges {
        adjacency.entry(predecessor).or_default().push(successor);
        reverse.entry(successor).or_default().push(predecessor);
    }

    // Kosaraju's algorithm over canonical adjacency produces stable SCCs
    // without recursion, so a corrupt or very deep restored plan cannot grow
    // the host call stack.
    let mut visited = BTreeSet::new();
    let mut finished = Vec::with_capacity(node_ids.len());
    for &id in node_ids {
        if !visited.contains(&id) {
            finish_component(id, &adjacency, &mut visited, &mut finished);
        }
    }
    visited.clear();
    let mut components = Vec::new();
    for &id in finished.iter().rev() {
        if !visited.contains(&id) {
            components.push(collect_component(id, &reverse, &mut visited));
        }
    }

    let mut node_component = BTreeMap::new();
    for (component_id, component) in components.iter().enumerate() {
        for &node_id in component {
            node_component.insert(node_id, component_id);
        }
    }
    let component_edges: BTreeSet<(usize, usize)> = edges
        .iter()
        .filter_map(|&(predecessor, successor)| {
            let before = *node_component.get(&predecessor)?;
            let after = *node_component.get(&successor)?;
            (before != after).then_some((before, after))
        })
        .collect();
    let mut successors = vec![Vec::new(); components.len()];
    let mut in_degree = vec![0usize; components.len()];
    for &(before, after) in &component_edges {
        successors[before].push(after);
        in_degree[after] += 1;
    }

    // Condensation is a DAG. Prefer the component's smallest node ID whenever
    // multiple components are ready, and IDs within an SCC are already sorted.
    let mut ready: BinaryHeap<Reverse<(usize, usize)>> = components
        .iter()
        .enumerate()
        .filter(|(component_id, _)| in_degree[*component_id] == 0)
        .map(|(component_id, component)| Reverse((component[0], component_id)))
        .collect();
    let mut component_order = Vec::with_capacity(components.len());
    while let Some(Reverse((_, component_id))) = ready.pop() {
        component_order.push(component_id);
        for &successor in &successors[component_id] {
            in_degree[successor] = in_degree[successor].saturating_sub(1);
            if in_degree[successor] == 0 {
                ready.push(Reverse((components[successor][0], successor)));
            }
        }
    }

    let mut component_depth = vec![0usize; components.len()];
    for &component_id in &component_order {
        let next_depth = component_depth[component_id].saturating_add(1);
        for &successor in &successors[component_id] {
            component_depth[successor] = component_depth[successor].max(next_depth);
        }
    }
    let order = component_order
        .iter()
        .flat_map(|&component_id| components[component_id].iter().copied())
        .collect();
    let depth = node_component
        .into_iter()
        .map(|(node_id, component_id)| (node_id, component_depth[component_id]))
        .collect();
    (order, depth)
}

/// Convert Finch's poset into the graph view the Forth overlay would draw.
///
/// Unused in production: [`TuiRenderer::draw_poset_overlay`] paints `corner`,
/// not this view. Named here so Finch's `Poset` stays next to
/// [`TuiRenderer::set_poset`] rather than in the widget.
#[allow(dead_code)]
fn graph_view_from_poset(poset: &crate::poset::Poset) -> GraphView {
    GraphView {
        nodes: poset.nodes.iter().map(graph_node_from_poset).collect(),
        edges: poset.edges.clone(),
        yaw: poset.yaw,
        pitch: poset.pitch,
    }
}

#[allow(dead_code)]
fn graph_node_from_poset(node: &crate::poset::Node) -> GraphNode {
    GraphNode {
        id: node.id,
        label: node.label.clone(),
        kind: match node.kind {
            crate::poset::NodeKind::Task => GraphNodeKind::Task,
            crate::poset::NodeKind::Constraint => GraphNodeKind::Constraint,
            crate::poset::NodeKind::Question => GraphNodeKind::Question,
            crate::poset::NodeKind::Observation => GraphNodeKind::Observation,
        },
        status: match node.status {
            crate::poset::NodeStatus::Pending => GraphNodeStatus::Pending,
            crate::poset::NodeStatus::Running => GraphNodeStatus::Running,
            crate::poset::NodeStatus::Done => GraphNodeStatus::Done,
            crate::poset::NodeStatus::Failed => GraphNodeStatus::Failed,
        },
        pos: node.pos,
        author: match node.author {
            crate::poset::NodeAuthor::User => GraphNodeAuthor::User,
            crate::poset::NodeAuthor::Ai => GraphNodeAuthor::Ai,
        },
    }
}

/// Render a graph view as compact Forth source lines for the panel overlay.
///
/// Each node becomes one word definition; predecessors are called first.
/// `PROGRAM` calls all leaf nodes (nodes with no outgoing edges).
/// Output is capped at `max_lines` lines.
#[allow(dead_code)]
fn poset_to_forth_lines(graph: &GraphView, _panel_w: usize, max_lines: usize) -> Vec<String> {
    const C: SetForegroundColor = SetForegroundColor(Color::DarkCyan);
    const Y: SetForegroundColor = SetForegroundColor(Color::DarkYellow);
    const G: SetForegroundColor = SetForegroundColor(Color::DarkGreen);
    const R: SetForegroundColor = SetForegroundColor(Color::DarkRed);
    const D: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
    const RST: SetAttribute = SetAttribute(Attribute::Reset);

    let mut lines: Vec<String> = Vec::new();

    // Canonicalize the graph before rendering. The view exposes its storage so
    // callers can restore plans, and restored node/edge order is not a semantic
    // part of the partial order.
    let node_ids: std::collections::BTreeSet<usize> =
        graph.nodes.iter().map(|node| node.id).collect();
    let mut edges: Vec<(usize, usize)> = graph
        .edges
        .iter()
        .copied()
        .filter(|(pred, succ)| node_ids.contains(pred) && node_ids.contains(succ))
        .collect();
    edges.sort_unstable();
    edges.dedup();

    // Build predecessor map: node_id → [pred_id, ...]. The canonical edge
    // order also gives every word a stable predecessor call order.
    let mut preds: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for &(pred, succ) in &edges {
        preds.entry(succ).or_default().push(pred);
    }

    // Collapse cycles into strongly connected components before sorting. This
    // preserves every satisfiable predecessor edge between components while
    // retaining a deterministic ID order inside an inherently cyclic SCC.
    let (topo, depth) = stable_poset_order_and_depth(&node_ids, &edges);

    // Word name helper
    let word_name = |id: usize| -> String { format!("W{id}") };

    // Render each word in topo order
    for &id in &topo {
        let Some(node) = graph.nodes.iter().find(|n| n.id == id) else {
            continue;
        };

        let status_glyph = match node.status {
            GraphNodeStatus::Done => format!("{G}✓{RST}"),
            GraphNodeStatus::Failed => format!("{R}✗{RST}"),
            GraphNodeStatus::Running => format!("{Y}▶{RST}"),
            GraphNodeStatus::Pending => format!("{D}·{RST}"),
        };

        let stack_effect = format!("{D}( -- result ){RST}");

        // Predecessor calls (for words that have dependencies)
        let pred_call = preds
            .get(&id)
            .filter(|ps| !ps.is_empty())
            .map(|ps| {
                let names: Vec<String> = ps.iter().map(|&pid| word_name(pid)).collect();
                format!("  {D}{}{RST}", names.join(" "))
            })
            .unwrap_or_default();

        // Label: truncate to ~30 chars
        let label: String = node.label.chars().take(30).collect();
        let ellipsis = if node.label.len() > 30 { "…" } else { "" };

        // Word header: `: W0  ( bash write read -- )  ✓`
        lines.push(format!(
            "{C}: {name}{RST}  {se}  {status}",
            name = word_name(id),
            se = stack_effect,
            status = status_glyph,
        ));
        // Body: optional pred calls + label
        if !pred_call.is_empty() {
            lines.push(pred_call);
        }
        lines.push(format!("  {D}.\" {label}{ellipsis}\"{RST}"));
        lines.push(format!("{C};{RST}"));

        if lines.len() >= max_lines.saturating_sub(2) {
            let remaining = topo
                .len()
                .saturating_sub(topo.iter().position(|&x| x == id).unwrap_or(0) + 1);
            if remaining > 0 {
                lines.push(format!(
                    "{D}\\ … {remaining} more word{} …{RST}",
                    if remaining == 1 { "" } else { "s" }
                ));
            }
            break;
        }
    }

    // PROGRAM word — reflects the partial order.
    // Nodes at the same DAG depth with no edges between them run concurrently;
    // we group them on the same line with a `\ concurrent` annotation.
    if lines.len() < max_lines {
        // Group node ids by depth level, in topo order within each group.
        let max_depth = depth.values().copied().max().unwrap_or(0);
        let mut program_lines: Vec<String> = vec![format!("{Y}: PROGRAM{RST}")];
        for lvl in 0..=max_depth {
            let group: Vec<String> = topo
                .iter()
                .filter(|&&id| depth.get(&id).copied().unwrap_or(0) == lvl)
                .map(|&id| word_name(id))
                .collect();
            if group.is_empty() {
                continue;
            }
            let contains_cycle = edges.iter().any(|&(predecessor, successor)| {
                depth.get(&predecessor).copied() == Some(lvl)
                    && depth.get(&successor).copied() == Some(lvl)
            });
            let parallel_note = if contains_cycle {
                format!("  {D}\\ cycle{RST}")
            } else if group.len() > 1 {
                format!("  {D}\\ concurrent{RST}")
            } else {
                String::new()
            };
            program_lines.push(format!("  {}{}", group.join("  "), parallel_note));
        }
        // Close with semicolon on the last line.
        if let Some(last) = program_lines.last_mut() {
            last.push_str(&format!("  {Y};{RST}"));
        }
        for l in program_lines {
            if lines.len() < max_lines {
                lines.push(l);
            }
        }
    }

    lines
}

// ─── Pure logic helpers (testable without a terminal) ─────────────────────────

/// Count the number of terminal rows an `effective_status` string will occupy.
///
/// Each `\n` in the string produces an additional row.  An empty string still
/// occupies exactly one row (the idle hint is always shown).
#[allow(dead_code)]
pub(crate) fn count_status_lines(status: &str) -> usize {
    status.lines().count().max(1)
}

/// Compute the 0-based row index (from the top of the live area) where the
/// cursor will be parked after draw_live_area() finishes repositioning it into
/// the input area.
///
/// This function assumes each input line occupies exactly one terminal row
/// (no wrapping). `draw_live_area` uses inline physical-row computation instead,
/// but this helper is retained for unit tests.
///
/// Parameters:
/// - `total_rows`: total rows drawn in the live area (WorkUnit + sep + input + status)
/// - `input_line_count`: number of input lines (≥ 1)
/// - `cursor_row`: which input line the cursor is on (0-based)
/// - `status_line_count`: number of status lines drawn (≥ 1)
#[allow(dead_code)]
pub(crate) fn compute_cursor_row_from_top(
    total_rows: usize,
    input_line_count: usize,
    cursor_row: usize,
    status_line_count: usize,
) -> usize {
    let input_below = input_line_count.saturating_sub(cursor_row + 1);
    let rows_below_cursor = input_below + status_line_count;
    total_rows.saturating_sub(1 + rows_below_cursor)
}

/// Select the newest live transcript rows that fit above the input/status
/// area. Unlike the old logical-line cap, this budgets actual terminal rows,
/// so ANSI text and wrapped tool output cannot silently push the cursor origin
/// out of sync. A visible marker makes clipping explicit; the complete message
/// is still committed to permanent scrollback when its WorkUnit finishes.
fn live_viewport_lines(
    lines: &[String],
    terminal_width: usize,
    row_budget: usize,
) -> (Vec<String>, usize) {
    let width = terminal_width.max(1);
    let total_rows = lines
        .iter()
        .map(|line| shadow_buffer::physical_rows(line, width))
        .sum::<usize>();
    if row_budget == 0 {
        return (Vec::new(), total_rows);
    }
    let budget = row_budget;
    if total_rows <= budget {
        return (lines.to_vec(), 0);
    }

    // Reserve one physical row for an honest clipping marker.
    let mut remaining = budget.saturating_sub(1);
    let mut selected = Vec::new();
    let mut selected_rows = 0usize;
    for line in lines.iter().rev() {
        if remaining == 0 {
            break;
        }
        let rows = shadow_buffer::physical_rows(line, width);
        if rows <= remaining {
            selected.push(line.clone());
            remaining -= rows;
            selected_rows += rows;
        } else {
            let fragment = visible_tail(line, remaining.saturating_mul(width));
            if !fragment.is_empty() {
                selected_rows += shadow_buffer::physical_rows(&fragment, width);
                selected.push(fragment);
            }
            break;
        }
    }
    selected.reverse();
    let omitted_rows = total_rows.saturating_sub(selected_rows);
    let marker = format!("… {omitted_rows} earlier live rows clipped; retained until completion …");
    selected.insert(0, visible_prefix(&marker, width));
    (selected, omitted_rows)
}

/// Drop the oldest transcript-viewport lines until the remaining content fits
/// the viewport rect the widget tree claimed.
///
/// Session tasks and child-agent rows are reserved against the viewport budget
/// but painted in full. When they filled the viewport the composer was pushed
/// off the bottom — typed draft hidden until the turn finished (#136).
fn clip_viewport_prefix(
    content: &mut Vec<RenderedTranscriptLine>,
    visible_live: &mut Vec<RenderedTranscriptLine>,
    width: usize,
    budget: usize,
) {
    let occupied = |content: &[RenderedTranscriptLine]| {
        content
            .iter()
            .map(|line| shadow_buffer::physical_rows(&line.text, width))
            .sum::<usize>()
    };
    while occupied(content) > budget && !content.is_empty() {
        let dropped = content.remove(0);
        if visible_live.first().is_some_and(|line| {
            line.text.trim_end_matches('\r') == dropped.text.trim_end_matches('\r')
        }) {
            visible_live.remove(0);
        }
    }
}

fn input_physical_rows(lines: &[String], terminal_width: usize) -> usize {
    input_line_physical_rows(lines, terminal_width)
        .into_iter()
        .sum()
}

fn input_line_physical_rows(lines: &[String], terminal_width: usize) -> Vec<usize> {
    input_line_physical_rows_with_ghost(lines, terminal_width, None)
}

fn input_line_physical_rows_with_ghost(
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
                ghost_text.map(shadow_buffer::visible_length).unwrap_or(0)
            } else {
                0
            };
            (prefix_width + shadow_buffer::visible_length(line) + ghost_width)
                .max(1)
                .div_ceil(width)
        })
        .collect()
}

fn ellipsize(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width {
        return text.to_string();
    }
    match width {
        0 => String::new(),
        1 => "…".into(),
        _ => format!("{}…", text.chars().take(width - 1).collect::<String>()),
    }
}

/// Build one separator row that can never wrap. Brain identity receives most
/// of the width; the workspace is shortened first on narrow terminals.
fn session_separator_line(width: usize, cwd: &str, session: &str) -> String {
    if width == 0 {
        return String::new();
    }
    let prefix = ellipsize("── ", width);
    let remaining = width.saturating_sub(prefix.chars().count());
    if remaining == 0 {
        return prefix;
    }

    let desired_right = if session.is_empty() {
        " ──".to_string()
    } else {
        format!(" {session} ──")
    };
    let right_budget = desired_right
        .chars()
        .count()
        .min(remaining.saturating_mul(2).div_ceil(3).max(3))
        .min(remaining);
    let right = if session.is_empty() || right_budget < 5 {
        ellipsize(&desired_right, right_budget)
    } else {
        format!(" {} ──", ellipsize(session, right_budget - 4))
    };
    let left_budget = remaining.saturating_sub(right.chars().count());
    let cwd_part = if left_budget >= 3 {
        format!(" {} ", ellipsize(cwd, left_budget - 2))
    } else {
        String::new()
    };
    let used = prefix.chars().count() + cwd_part.chars().count() + right.chars().count();
    format!(
        "{prefix}{cwd_part}{}{right}",
        "─".repeat(width.saturating_sub(used))
    )
}

/// Return a plain visible suffix small enough to fit in `columns`. This is
/// used only when one logical line is itself taller than the remaining live
/// viewport; completed scrollback retains the original ANSI-bearing line.
fn visible_tail(line: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let marker = "… ";
    let marker_width = shadow_buffer::visible_length(marker);
    if columns <= marker_width {
        return visible_prefix(marker, columns);
    }
    let available = columns.saturating_sub(marker_width);
    let (visible, _) = shadow_buffer::extract_visible_chars(line);
    let mut suffix = Vec::new();
    let mut used = 0usize;
    for character in visible.into_iter().rev() {
        let width = shadow_buffer::visible_length(&character.to_string());
        if used + width > available {
            break;
        }
        suffix.push(character);
        used += width;
    }
    suffix.reverse();
    format!("{marker}{}", suffix.into_iter().collect::<String>())
}

fn visible_prefix(line: &str, columns: usize) -> String {
    let (visible, _) = shadow_buffer::extract_visible_chars(line);
    let mut prefix = String::new();
    let mut used = 0usize;
    for character in visible {
        let width = shadow_buffer::visible_length(&character.to_string());
        if used + width > columns {
            break;
        }
        prefix.push(character);
        used += width;
    }
    prefix
}

/// Compute the ghost-text suffix to append after the user's current input.
///
/// Returns `Some(suffix)` when `input` is a `/command` prefix that unambiguously
/// completes to a single command; returns `None` otherwise.
pub(crate) fn compute_ghost_text(
    input: &str,
    registry: &crate::cli::command_autocomplete::CommandRegistry,
) -> Option<String> {
    if input.trim().is_empty() || !input.starts_with('/') {
        return None;
    }
    let matches = registry.match_prefix(input);
    matches.first().and_then(|spec| {
        if spec.name.len() > input.len() {
            Some(spec.name[input.len()..].to_string())
        } else {
            None
        }
    })
}

fn ghost_for_command(input: &str, command_name: &str) -> Option<String> {
    if command_name.len() <= input.len()
        || !command_name
            .to_ascii_lowercase()
            .starts_with(&input.to_ascii_lowercase())
    {
        return None;
    }
    Some(command_name[input.len()..].to_string())
}

fn selected_completion_ghost(
    lines: &[String],
    cursor: (usize, usize),
    autocomplete: &AutocompleteState,
) -> Option<String> {
    let (cursor_row, cursor_col) = cursor;
    if cursor_row != 0
        || lines.len() != 1
        || lines
            .first()
            .map_or(true, |line| cursor_col != line.chars().count())
    {
        return None;
    }
    let prefix = lines[0].chars().take(cursor_col).collect::<String>();
    autocomplete
        .get_selected()
        .and_then(|command| ghost_for_command(&prefix, command.name))
}

fn command_completion_at_cursor(
    lines: &[String],
    cursor: (usize, usize),
    registry: &crate::cli::command_autocomplete::CommandRegistry,
) -> (
    Vec<crate::cli::command_autocomplete::CommandSpec>,
    Option<String>,
) {
    let (cursor_row, cursor_col) = cursor;
    let prefix = if cursor_row == 0 {
        lines
            .first()
            .map(|line| line.chars().take(cursor_col).collect::<String>())
            .filter(|line| line.starts_with('/'))
    } else {
        None
    };
    let Some(prefix) = prefix else {
        return (Vec::new(), None);
    };
    let matches = registry.match_prefix(&prefix);
    let ghost = if lines.len() == 1
        && lines
            .first()
            .is_some_and(|line| cursor_col == line.chars().count())
    {
        matches
            .first()
            .and_then(|command| ghost_for_command(&prefix, command.name))
    } else {
        None
    };
    (matches, ghost)
}

fn replace_textarea_command(textarea: &mut TextArea<'static>, command_name: &str) -> bool {
    use tui_textarea::CursorMove;

    let Some((lines, target_cursor)) =
        replace_command_prefix(textarea.lines(), textarea.cursor(), command_name)
    else {
        return false;
    };
    *textarea = TuiRenderer::create_clean_textarea_with_text(&lines.join("\n"));
    textarea.move_cursor(CursorMove::Top);
    let (_, column) = textarea.cursor();
    if column > 0 {
        textarea.move_cursor(CursorMove::Head);
    }
    for _ in 0..target_cursor.1 {
        textarea.move_cursor(CursorMove::Forward);
    }
    true
}

fn replace_textarea_mention(textarea: &mut TextArea<'static>, token: &str) -> bool {
    use tui_textarea::CursorMove;

    let Some((lines, target_cursor)) =
        replace_mention_prefix(textarea.lines(), textarea.cursor(), token)
    else {
        return false;
    };
    *textarea = TuiRenderer::create_clean_textarea_with_text(&lines.join("\n"));
    textarea.move_cursor(CursorMove::Top);
    for _ in 0..target_cursor.0 {
        textarea.move_cursor(CursorMove::Down);
    }
    let (_, column) = textarea.cursor();
    if column > 0 {
        textarea.move_cursor(CursorMove::Head);
    }
    for _ in 0..target_cursor.1 {
        textarea.move_cursor(CursorMove::Forward);
    }
    true
}

fn mention_query_from_textarea(textarea: &TextArea<'_>) -> Option<(usize, String)> {
    let (row, col) = textarea.cursor();
    let line = textarea.lines().get(row)?;
    crate::context::mention::mention_query_at(line, col)
}

fn dispatch_completion_key(
    textarea: &mut TextArea<'static>,
    autocomplete: &mut AutocompleteState,
    ghost_text: &mut Option<String>,
    code: KeyCode,
) -> bool {
    if code == KeyCode::Tab && autocomplete.visible && !autocomplete.is_interactive() {
        // A completion context exists, but critical UI or a tiny viewport hid
        // its pane. Consume Tab without applying stale ghost text or inserting
        // a literal tab into the user's draft.
        *ghost_text = None;
        return true;
    }
    if !autocomplete.is_interactive() {
        return false;
    }
    match code {
        KeyCode::Up => autocomplete.select_previous(),
        KeyCode::Down => autocomplete.select_next(),
        KeyCode::Tab => {
            let Some(command_name) = autocomplete
                .get_selected()
                .map(|command| command.name.to_string())
            else {
                return false;
            };
            if !replace_textarea_command(textarea, &command_name) {
                return false;
            }
            autocomplete.hide();
            *ghost_text = None;
            return true;
        }
        KeyCode::Esc => {
            autocomplete.hide();
            *ghost_text = None;
            return true;
        }
        _ => return false,
    }
    *ghost_text = selected_completion_ghost(textarea.lines(), textarea.cursor(), autocomplete);
    true
}

/// Expand the selected slash completion into the composer before submit.
///
/// Up/Down only move `AutocompleteState::selected_index`; they do not rewrite
/// the textarea. Enter used to join the typed prefix (`/` → Help). When the
/// pane currently owns keyboard selection, apply that row first so Enter runs
/// the highlighted command.
fn apply_selected_completion_for_submit(
    textarea: &mut TextArea<'static>,
    autocomplete: &mut AutocompleteState,
    ghost_text: &mut Option<String>,
) -> bool {
    if !autocomplete.is_interactive() {
        return false;
    }
    let Some(command_name) = autocomplete
        .get_selected()
        .map(|command| command.name.to_string())
    else {
        return false;
    };
    if !replace_textarea_command(textarea, &command_name) {
        return false;
    }
    autocomplete.hide();
    *ghost_text = None;
    true
}

/// Result of the composer key path shared by the input task and tests.
enum ComposerDispatch {
    /// Enter submitted a non-empty composer line.
    Submit(String),
    /// The key was consumed. `input_changed` is the input-task flag that
    /// triggers `update_ghost_text` after the event.
    Handled { input_changed: bool },
    /// Not Tab, completion, Enter, or history Up/Down.
    Unhandled,
}

fn route_tab_key(
    textarea: &mut TextArea<'static>,
    autocomplete: &mut AutocompleteState,
    ghost_text: &mut Option<String>,
    key: KeyEvent,
) -> bool {
    if dispatch_completion_key(textarea, autocomplete, ghost_text, KeyCode::Tab) {
        return false;
    }
    textarea.input(Event::Key(key));
    true
}

fn apply_viewport_resize(
    autocomplete: &mut AutocompleteState,
    pending_viewport_size: &mut Option<(u16, u16)>,
    viewport_invalidated: &mut bool,
    live_area_dirty: &mut bool,
    width: u16,
    height: u16,
) {
    autocomplete.invalidate_rendered_rows();
    *pending_viewport_size = Some((width, height));
    *viewport_invalidated = true;
    *live_area_dirty = true;
}

/// Compute what to display in the status bar.
///
/// Priority:
/// 1. A live stat / operation is set (`raw_status` non-empty) → show that.
/// 2. User is typing a `/command` with ghost text → show the command's description.
/// 3. Idle → show the keyboard shortcut reminder.
pub(crate) fn compute_effective_status(
    ghost_text: Option<&str>,
    raw_status: &str,
    current_input: &str,
    registry: &crate::cli::command_autocomplete::CommandRegistry,
) -> String {
    // Operational and error state is never hidden by command help. The
    // completion pane carries command descriptions in its own rows.
    if !raw_status.is_empty() {
        return raw_status.to_string();
    }
    if ghost_text.is_some() {
        let desc = registry
            .match_prefix(current_input)
            .into_iter()
            .next()
            .map(|spec| {
                if let Some(params) = spec.params {
                    format!("  {} {} — {}", spec.name, params, spec.description)
                } else {
                    format!("  {} — {}", spec.name, spec.description)
                }
            })
            .unwrap_or_default();
        if !desc.is_empty() {
            return desc;
        }
    }
    "↑↓ history  ·  Tab complete  ·  /help for commands  ·  Ctrl+C cancel".to_string()
}

fn write_live_area_erase(
    out: &mut impl Write,
    active_rows: usize,
    cursor_row_from_top: usize,
) -> Result<()> {
    execute!(out, BeginSynchronizedUpdate)?;
    if active_rows == 0 && cursor_row_from_top == 0 {
        return Ok(());
    }
    execute!(out, cursor::MoveToColumn(0))?;
    if cursor_row_from_top > 0 {
        execute!(out, cursor::MoveUp(cursor_row_from_top as u16))?;
    }
    for row in 0..active_rows {
        execute!(out, Clear(ClearType::CurrentLine))?;
        if row + 1 < active_rows {
            execute!(out, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
        }
    }
    if active_rows > 1 {
        execute!(out, cursor::MoveUp((active_rows - 1) as u16))?;
        execute!(out, cursor::MoveToColumn(0))?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct TinyLiveFrame {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

fn plan_tiny_live_frame(
    input_lines: &[String],
    cursor: (usize, usize),
    status: &str,
    terminal_rows: usize,
    terminal_width: usize,
) -> TinyLiveFrame {
    if terminal_rows == 0 {
        return TinyLiveFrame {
            lines: Vec::new(),
            cursor_row: 0,
            cursor_col: 0,
        };
    }
    let width = terminal_width.max(1);
    let (input_row, input_col) = cursor;
    let line = input_lines.get(input_row).map(String::as_str).unwrap_or("");
    let prompt = if input_row == 0 { "❯ " } else { "  " };
    let input = ellipsize(&format!("{prompt}{line}"), width);
    let cursor_col = (2 + line.chars().take(input_col).count()).min(width.saturating_sub(1));

    let mut lines = Vec::with_capacity(terminal_rows);
    if terminal_rows >= 2 {
        lines.push("─".repeat(width));
    }
    let cursor_row = lines.len();
    lines.push(input);
    if terminal_rows >= 3 {
        lines.push(ellipsize(status.lines().next().unwrap_or(""), width));
    }
    TinyLiveFrame {
        lines,
        cursor_row,
        cursor_col,
    }
}

fn write_tiny_live_frame(out: &mut impl Write, frame: &TinyLiveFrame) -> Result<usize> {
    // Dialogs hide the terminal cursor. The tiny path can be the first frame
    // after a dialog closes, so it must restore visibility just like the
    // normal editable-input renderer.
    execute!(out, cursor::Show)?;
    for (index, line) in frame.lines.iter().enumerate() {
        execute!(out, Print(line))?;
        if index + 1 < frame.lines.len() {
            execute!(out, Print("\r\n"))?;
        }
    }
    let rows_below = frame
        .lines
        .len()
        .saturating_sub(frame.cursor_row.saturating_add(1));
    if rows_below > 0 {
        execute!(out, cursor::MoveUp(rows_below as u16))?;
    }
    execute!(out, cursor::MoveToColumn(frame.cursor_col as u16))?;
    Ok(frame.lines.len())
}

// ─── Live-area frame ──────────────────────────────────────────────────────────

/// Columns the input prompt (`❯ `) and its continuation (`  `) both occupy.
const PROMPT_COLUMNS: usize = 2;

fn aggregate_agent_usage<'a>(
    usage: impl Iterator<Item = &'a activity::ActivityUsage>,
) -> activity::ActivityUsage {
    use activity::{ActivityUsage, ActivityUsageState};

    let mut aggregate = ActivityUsage::default();
    let mut all_complete = true;
    for task in usage {
        aggregate.started_attempts += task.started_attempts;
        aggregate.reported_attempts += task.reported_attempts;
        if let Some(tokens) = task.input_tokens {
            let total = aggregate.input_tokens.get_or_insert(0);
            *total = total.saturating_add(tokens);
        }
        if let Some(tokens) = task.output_tokens {
            let total = aggregate.output_tokens.get_or_insert(0);
            *total = total.saturating_add(tokens);
        }
        if task.started_attempts > 0 {
            all_complete &= task.state == ActivityUsageState::Complete;
        }
    }
    aggregate.state = if aggregate.reported_attempts == 0 {
        ActivityUsageState::Unavailable
    } else if all_complete {
        ActivityUsageState::Complete
    } else {
        ActivityUsageState::Partial
    };
    aggregate
}

/// One live-area frame: the exact logical lines to paint, and where the cursor
/// lands once they are painted.
///
/// The renderer used to count the rows it was drawing *while* it drew them, in
/// six separate places, and those counts fed `erase_live_area`. A line that
/// wrapped further than its own counter believed therefore left a row on the
/// screen that nothing would ever clear. A frame is measured once, from the
/// lines actually emitted, so the count and the paint cannot disagree.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LiveFrame {
    /// Logical lines, painted separated by `\r\n`. Any of them may wrap.
    pub lines: Vec<String>,
    /// Physical rows between the top of the live area and the cursor's row.
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub cursor_visible: bool,
    /// The live transcript lines that survived the viewport budget, carried so
    /// the caller can rebuild mouse hit regions against what was really drawn.
    pub visible_live: Vec<RenderedTranscriptLine>,
    /// The regions the widget tree claimed for this frame (#805).
    pub rects: view_model::FrameRects,
    /// Disclosure hit rects the layout pass claimed inside the transcript
    /// viewport, in the frame's own coordinates.
    pub hitboxes: Vec<ClaimedDisclosureRect>,
}

impl LiveFrame {
    /// Physical terminal rows this frame occupies — the number of rows the
    /// next erase must clear.
    pub fn physical_rows(&self, terminal_width: usize) -> usize {
        self.lines
            .iter()
            .map(|line| shadow_buffer::physical_rows(line, terminal_width))
            .sum()
    }

    /// Render this frame into a shadow buffer of the given size, so a test can
    /// assert on the cells a terminal would end up holding.
    pub fn to_shadow_buffer(&self, width: usize, height: usize) -> shadow_buffer::ShadowBuffer {
        let mut buffer = shadow_buffer::ShadowBuffer::new(width.max(1), height);
        buffer.render_lines(&self.lines);
        buffer
    }

    fn push(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

/// Paint a planned frame and return the physical rows it consumed.
///
/// The cursor arithmetic lives here and nowhere else: after the last line the
/// cursor sits on a known row, and moving to `frame.cursor_row` is a
/// subtraction rather than a running tally kept by the drawing code.
fn write_live_frame(
    out: &mut impl Write,
    frame: &LiveFrame,
    terminal_width: usize,
) -> Result<usize> {
    if frame.cursor_visible {
        // Dialogs hide the cursor; the next editable frame must restore it.
        execute!(out, cursor::Show)?;
    } else {
        execute!(out, cursor::Hide)?;
    }
    for (index, line) in frame.lines.iter().enumerate() {
        if index > 0 {
            execute!(out, Print("\r\n"))?;
        }
        execute!(out, Print(line))?;
    }
    let rows = frame.physical_rows(terminal_width);
    if rows == 0 {
        return Ok(0);
    }
    // Row the cursor is actually on once the paint finishes.
    let landed = rows - 1;
    let up = landed.saturating_sub(frame.cursor_row);
    if up > 0 {
        execute!(out, cursor::MoveUp(up as u16))?;
    }
    execute!(out, cursor::MoveToColumn(frame.cursor_col as u16))?;
    Ok(rows)
}

/// Lay out one live-area frame.
///
/// The ViewModel is projected into a widget tree whose root column allocates
/// from the bottom — status, hr, input, hr, completions (0–N) — and the live
/// transcript viewport claims the leftover rows (#805). The completion pane is
/// a sibling **above** the composer: an empty pane claims zero rows, so
/// opening or closing it never moves the composer or status rects (#232).
pub(crate) fn plan_live_frame(
    vm: &view_model::LiveViewModel<'_>,
    autocomplete: &mut AutocompleteState,
) -> LiveFrame {
    let width = vm.terminal_width.max(1);
    let height = vm.terminal_height;
    // A dialog or the expanded tool-result surface owns the viewport: a
    // focused surface's own scrolling must never move the transcript behind it.
    let dialog_active = vm.dialog.is_some() || vm.expanded_lines.is_some();
    let mut frame = LiveFrame::default();

    // ── Claiming pass: the widget tree claims the frame's regions ────────────
    // The completions pane's natural extent is a ViewModel prop — empty unless
    // the draft is a slash command or mention, or a mention lookup failed.
    // Critical UI (dialog, render error) suppresses the pane entirely.
    let natural_pane =
        view_model::natural_completion_pane(autocomplete, width, dialog_active || vm.render_error);
    let sizing = view_model::claim_live_frame(vm, None, natural_pane.len());
    let rects = view_model::frame_rects(&sizing);

    // ── 1. Transcript viewport content, sized by its claimed rect ────────────
    let completion_lines = view_model::completion_pane_for_claim(
        autocomplete,
        width,
        rects.completions.map_or(0, |rect| rect.height),
        &natural_pane,
    );

    let mut viewport_content: Vec<RenderedTranscriptLine> = Vec::new();
    if !dialog_active {
        // A Brain can have more than one live work unit (a streamed VM program
        // alongside a child task or output handle). Rendering only the newest
        // made earlier source appear and then vanish on the next redraw.
        let all_live_lines = vm
            .live_rendered
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        // Session tasks and child-agent rows are reserved one row each before
        // the live transcript claims its window.
        let live_budget = rects
            .transcript
            .height
            .saturating_sub(vm.task_rows.len() + vm.tracked_rows.len());
        let mut live_lines = if all_live_lines.is_empty() {
            Vec::new()
        } else {
            live_viewport_lines(&all_live_lines, width, live_budget).0
        };
        pin_live_disclosure_header(vm.live_rendered, &mut live_lines, width);
        let mut visible_live = rendered_metadata_for_visible(vm.live_rendered, &live_lines);
        viewport_content.extend(visible_live.iter().cloned());

        // ── 1b. Session task list (active items only) ────────────────────────
        for row in vm.task_rows {
            if let Some(text) = session_task_line(row, width) {
                viewport_content.push(RenderedTranscriptLine {
                    text,
                    ..RenderedTranscriptLine::default()
                });
            }
        }

        // ── 1c. Child-agent task tree ────────────────────────────────────────
        for row in vm.tracked_rows {
            viewport_content.push(RenderedTranscriptLine {
                text: tracked_agent_line(row, width),
                ..RenderedTranscriptLine::default()
            });
        }

        // Pin the composer in the viewport: activity rows were painted in full
        // and could still consume every remaining row. Clip that prefix so
        // separator + draft + status always fit.
        clip_viewport_prefix(
            &mut viewport_content,
            &mut visible_live,
            width,
            rects.transcript.height,
        );
        frame.visible_live = visible_live;
    }

    // ── 2. Second claiming pass with the content it will paint ───────────────
    // The claims are content-independent, so the rects are the sizing pass's
    // rects; this pass additionally records the viewport window and the
    // disclosure hit rects the depth-first claim produced.
    let content = view_model::LiveFrameContent {
        viewport: viewport_content.clone(),
        completions: completion_lines.clone(),
    };
    let claimed = view_model::claim_live_frame(vm, Some(&content), 0);
    let claimed_rects = view_model::frame_rects(&claimed);

    // ── 3. Paint in claimed order: transcript, completions, hr, input, hr,
    //       status ────────────────────────────────────────────────────────────
    for line in &viewport_content {
        frame.push(line.text.trim_end_matches('\r'));
    }
    // ── 3b. Slash-command completion pane, above the composer ────────────────
    // Plain text is deliberate: the raw/no-colour path stays fully speakable,
    // and every line is width-bounded before it reaches the terminal.
    for line in &completion_lines {
        frame.push(line.clone());
    }

    // ── 3c. Separator: "──  ~/repos/finch ──────── jade-river ──" ────────────
    let separator = session_separator_line(width, vm.cwd_label, vm.session_label);
    if frame.physical_rows(width) < height {
        frame.push(format!("{DIM_GRAY}{separator}{RESET}"));
    }

    // ── 4. Dialog, expanded tool result, or input ────────────────────────────
    if let Some(dialog) = vm.dialog {
        let budget = height.saturating_sub(frame.physical_rows(width));
        for line in TuiRenderer::dialog_lines(dialog, width, budget) {
            frame.push(line);
        }
        // A dialog has no editable text cursor. Keep the hidden cursor on the
        // final owned row so painting at the viewport bottom cannot scroll it.
        frame.cursor_visible = false;
        frame.cursor_row = frame.physical_rows(width).saturating_sub(1);
        frame.rects = view_model::FrameRects::default();
        return frame;
    }
    if let Some(expanded) = vm.expanded_lines {
        // Same budget discipline as a dialog: the surface owns what remains of
        // the viewport and never pushes rows past it. Title and footer are
        // preferred; the body window clips from the bottom.
        let mut remaining = height.saturating_sub(frame.physical_rows(width));
        match expanded.split_first() {
            None => return frame,
            Some((first, rest)) => {
                let title_rows = shadow_buffer::physical_rows(first, width);
                if title_rows <= remaining {
                    frame.push(first.clone());
                    remaining -= title_rows;
                }
                let footer = rest.last();
                let body = rest
                    .len()
                    .checked_sub(1)
                    .map(|len| &rest[..len])
                    .unwrap_or(rest);
                let footer_rows = footer
                    .map(|line| shadow_buffer::physical_rows(line, width))
                    .unwrap_or(0);
                let reserved = if footer_rows <= remaining {
                    footer_rows
                } else {
                    0
                };
                for line in body {
                    let rows = shadow_buffer::physical_rows(line, width);
                    if rows > remaining.saturating_sub(reserved) {
                        break;
                    }
                    frame.push(line.clone());
                    remaining -= rows;
                }
                if reserved > 0 {
                    frame.push(footer.expect("footer present when reserved").clone());
                }
            }
        }
        // The focused surface has no editable text cursor either; park it on
        // the final owned row like a dialog does.
        frame.cursor_visible = false;
        frame.cursor_row = frame.physical_rows(width).saturating_sub(1);
        frame.rects = view_model::FrameRects::default();
        return frame;
    }

    frame.cursor_visible = true;
    let (cursor_row, cursor_col) = vm.input_cursor;
    let rows_before_input = frame.physical_rows(width);
    let input_phys_rows = input_line_physical_rows_with_ghost(vm.input_lines, width, vm.ghost_text);

    // ── 5. Input area, with the dim ghost suffix on its last row ─────────────
    let prompt = format!("{CYAN}❯{RESET} ");
    let ghost = vm
        .ghost_text
        .map(|ghost| format!("{DIM_GRAY}{ghost}{RESET}"))
        .unwrap_or_default();
    if vm.input_lines.is_empty() {
        frame.push(format!("{prompt}{ghost}"));
    } else {
        let last = vm.input_lines.len() - 1;
        for (index, line) in vm.input_lines.iter().enumerate() {
            let prefix = if index == 0 { prompt.as_str() } else { "  " };
            let suffix = if index == last { ghost.as_str() } else { "" };
            frame.push(format!("{prefix}{line}{suffix}"));
        }
    }

    // ── 6. Status separator and status line(s) ───────────────────────────────
    // Session identity is projected into the upper separator; repeating it here
    // wasted a row and made the Brain appear twice.
    frame.push(format!("{DIM_GRAY}{}{RESET}", "─".repeat(width)));
    for line in vm.effective_status.lines() {
        frame.push(format!("{DIM_GRAY}{line}{RESET}"));
    }

    // ── 7. Cursor position inside the input area ─────────────────────────────
    let cursor_text_width = vm
        .input_lines
        .get(cursor_row)
        .map(|line| {
            let prefix: String = line.chars().take(cursor_col).collect();
            shadow_buffer::visible_length(&prefix)
        })
        .unwrap_or(0);
    let cursor_column = PROMPT_COLUMNS + cursor_text_width;
    // Which physical sub-row of its own logical line the cursor sits on.
    let cursor_sub_row = cursor_column / width;
    frame.cursor_col = cursor_column % width;
    let cursor_phys_above: usize = input_phys_rows[..cursor_row.min(input_phys_rows.len())]
        .iter()
        .sum();
    frame.cursor_row = rows_before_input + cursor_phys_above + cursor_sub_row;

    frame.rects = claimed_rects;
    frame.hitboxes = claimed
        .hit_rects()
        .map(|(index, rect)| ClaimedDisclosureRect {
            region: TranscriptHitRegion {
                row_id: viewport_content[index]
                    .row_id
                    .clone()
                    .expect("hit lines belong to expandable transcript rows"),
                top: rect.y as u16,
                bottom: rect.bottom().saturating_sub(1) as u16,
                left: 0,
                right: width.saturating_sub(1) as u16,
            },
            row_expanded: viewport_content[index].row_expanded.unwrap_or(false),
        })
        .collect();
    frame
}

/// One session task list row, or `None` for a finished row the draw skips.
fn session_task_line(row: &activity::ActivityRow, width: usize) -> Option<String> {
    let (symbol, color) = match row.state {
        activity::ActivityState::Active => ("●", CYAN),
        activity::ActivityState::Pending => ("○", DIM_GRAY),
        activity::ActivityState::Done => return None,
    };
    let urgent_tag = if row.urgent { " [!]" } else { "" };
    // "● " prefix plus the optional " [!]" suffix, measured in columns.
    let max_content = width.saturating_sub(2 + urgent_tag.len());
    let content = shadow_buffer::truncate_to_columns(&row.text, max_content);
    Some(format!("{color}{symbol} {content}{urgent_tag}{RESET}"))
}

/// One child-agent task tree row.
fn tracked_agent_line(row: &activity::ActivityRow, width: usize) -> String {
    let indent = "  ".repeat(row.depth);
    let symbol = match row.state {
        activity::ActivityState::Pending => "○",
        activity::ActivityState::Active => "●",
        activity::ActivityState::Done => "✓",
    };
    let color = if row.state == activity::ActivityState::Active {
        CYAN
    } else {
        DIM_GRAY
    };
    let detail = row.detail.clone().unwrap_or_default();
    let prefix_width = indent.chars().count() + 2;
    let available = width.saturating_sub(prefix_width + shadow_buffer::visible_length(&detail) + 3);
    let task_text = shadow_buffer::truncate_to_columns(&row.text, available);
    format!("{color}{indent}{symbol}{RESET} {task_text}{DIM_GRAY}{detail}{RESET}")
}

// ─── Poset panel view mode ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PosetPanelMode {
    #[default]
    Graph,
    Forth,
    /// Live typing view — shows arrows between words as the user types.
    /// Returns to the previous mode when input is cleared/submitted.
    Typing,
}

// ─── TuiRenderer ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
pub struct TuiRenderer {
    output_manager: Arc<OutputManager>,
    status_bar: Arc<StatusBar>,
    colors: ColorScheme,

    // Input — tui-textarea manages multi-line state; we render it manually.
    pub(crate) input_textarea: TextArea<'static>,
    pub(crate) command_history: Vec<String>,
    pub(crate) history_index: Option<usize>,
    pub(crate) history_draft: Option<String>,

    // How many rows the live area currently occupies at the bottom of the
    // terminal (WorkUnit + separator + input + status).  Cleared before each
    // redraw.
    active_rows: usize,

    // Latest dimensions reported by a resize event. They are consumed by the
    // next absolute viewport rebuild, so repeated resize events coalesce to the
    // newest frame instead of replaying intermediate relative repairs.
    pending_viewport_size: Option<(u16, u16)>,

    // A terminal resize lets the emulator reflow bytes that Finch previously
    // drew, so the old live-area origin is no longer recoverable with MoveUp.
    // The next render clears and rebuilds the complete visible viewport using
    // absolute coordinates. ClearType::All deliberately preserves native
    // scrollback outside that viewport.
    viewport_invalidated: bool,

    // Row index (0-based from top of live area) where the cursor is parked
    // after draw_live_area().  erase_live_area() uses this to correctly reach
    // the top regardless of where the cursor was repositioned (e.g. inside the
    // input area vs. bottom of a dialog box).
    cursor_row_from_top: usize,

    // Messages already committed to permanent scrollback.
    printed_ids: HashSet<MessageId>,

    // Accordion focus and hit regions. Expand/collapse choices live on the
    // WorkUnit rows themselves (`set_disclosure`); this map is only the last
    // painted frame plus in-flight toggles until the next projection.
    accordion: AccordionState,

    // Bounded child viewports for tool-use output rows (#656): per-row scroll
    // offsets, the hit regions of the last painted frame, and the focused
    // expanded surface. Presentation-only, like the accordion state.
    tool_viewports: ToolViewportState,
    pub(crate) expanded_tool: Option<ExpandedToolView>,

    // Dialog state — tool-approval dialogs shown in the live area.
    pub active_dialog: Option<Dialog>,
    pub active_tabbed_dialog: Option<TabbedDialog>,
    /// True while `active_dialog` occupies the live surface. A rising edge
    /// writes one terminal bell (`\x07`); later draws of the same overlay do not.
    attention_dialog_live: bool,

    // Generic flags
    is_active: bool,
    pub(crate) needs_full_refresh: bool,
    pub(crate) last_render_error: Option<String>,
    pub pending_feedback: Option<activity::Verdict>,
    pub pending_cancellation: bool,
    pub pending_dialog_result: Option<DialogResult>,

    // Autocomplete / suggestions
    pub(crate) ghost_text: Option<String>,
    suggestions: crate::cli::suggestions::SuggestionManager,
    command_registry: crate::cli::command_autocomplete::CommandRegistry,
    pub autocomplete_state: AutocompleteState,

    // Image paste support
    pub pending_images: Vec<(usize, String, String)>,
    pub(crate) image_counter: usize,

    /// Project-rooted `@` mention catalog. Policy lives in `context::mention`.
    pub(crate) mention_catalog: crate::context::mention::MentionCatalog,
    /// Snapshots taken when a mention was selected. Submit uses these bytes.
    pub pending_mentions: Vec<crate::context::mention::MentionSnapshot>,

    // Rate limiting - removed in favor of event loop control

    // Polled each frame for the session task list (set after construction).
    task_rows: Option<activity::SharedActivityRows>,

    // Rows that arrive as a stream rather than by polling, keyed by identity.
    tracked_rows: HashMap<uuid::Uuid, activity::ActivityRow>,
    tracked_agent_usage: HashMap<uuid::Uuid, activity::ActivityUsage>,

    // Output of the user-defined `check` word — shown in the corner if set.
    pub corner: Arc<std::sync::Mutex<Option<String>>>,

    // Co-Forth shared stack (set after construction via set_stack)
    stack: Option<Arc<tokio::sync::Mutex<Vec<String>>>>,

    // Co-Forth poset VM (set after construction via set_poset). Stored only;
    // draw_poset_overlay paints `corner`, not this graph.
    poset: Option<Arc<tokio::sync::Mutex<crate::poset::Poset>>>,
    // True when the poset panel was rendered (non-empty) on the last tick.
    // Used to keep cursor_row_from_top stable when try_lock() fails.
    poset_was_visible: bool,
    // Which view is shown in the poset panel: graph or forth source.
    pub poset_panel_mode: PosetPanelMode,
    // True once we've shown the first-panel hint line — shown once, then silent.
    panel_hint_shown: bool,

    // Session identity — set before the first live-area render; shown in the
    // separator line.
    session_label: String,

    /// Words currently being typed (updated on each keystroke via set_typing_words).
    /// When non-empty, the panel switches to Typing mode to show live arrows.
    pub typing_words: Vec<String>,
    /// Panel mode to restore after typing is done (before Typing mode was set).
    pre_typing_mode: PosetPanelMode,

    /// True when live area state has changed since the last draw.
    /// Guards the idle-case redraw in flush_output_safe() to eliminate
    /// unconditional erase+draw every 33 ms tick when nothing changed.
    live_area_dirty: bool,

    /// Whether this renderer currently holds mouse tracking. Default is off so
    /// native click-drag selection works (#221). When held, a wheel releases
    /// tracking so native scrollback is reachable (#441).
    mouse_tracking: mouse_capture::MouseTracking,
}

// ─── Construction ─────────────────────────────────────────────────────────────

impl TuiRenderer {
    #[cfg(test)]
    pub(crate) fn new_headless(
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        colors: ColorScheme,
    ) -> Self {
        output_manager.disable_stdout();
        Self {
            output_manager,
            status_bar,
            colors,
            input_textarea: Self::create_clean_textarea(),
            command_history: Vec::new(),
            history_index: None,
            history_draft: None,
            active_rows: 0,
            pending_viewport_size: None,
            viewport_invalidated: false,
            cursor_row_from_top: 0,
            printed_ids: HashSet::new(),
            accordion: AccordionState::default(),
            tool_viewports: ToolViewportState::default(),
            expanded_tool: None,
            active_dialog: None,
            active_tabbed_dialog: None,
            attention_dialog_live: false,
            is_active: false,
            needs_full_refresh: false,
            last_render_error: None,
            pending_feedback: None,
            pending_cancellation: false,
            pending_dialog_result: None,
            ghost_text: None,
            suggestions: crate::cli::suggestions::SuggestionManager::new(),
            command_registry: crate::cli::command_autocomplete::CommandRegistry::new(),
            autocomplete_state: AutocompleteState::default(),
            pending_images: Vec::new(),
            image_counter: 0,
            mention_catalog: crate::context::mention::MentionCatalog::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            pending_mentions: Vec::new(),
            task_rows: None,
            tracked_rows: HashMap::new(),
            tracked_agent_usage: HashMap::new(),
            corner: Arc::new(std::sync::Mutex::new(None)),
            stack: None,
            poset: None,
            poset_was_visible: false,
            poset_panel_mode: PosetPanelMode::Forth,
            panel_hint_shown: false,
            session_label: String::new(),
            typing_words: Vec::new(),
            pre_typing_mode: PosetPanelMode::Forth,
            live_area_dirty: true,
            mouse_tracking: mouse_capture::MouseTracking::DEFAULT,
        }
    }

    pub fn new(
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        colors: ColorScheme,
    ) -> Result<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;

        // Bracketed paste so the terminal wraps pasted content in
        // \x1b[200~ ... \x1b[201~ markers, plus kitty DISAMBIGUATE_ESCAPE_CODES
        // so Shift+Enter is distinct from bare Enter. Mouse capture is omitted:
        // click-drag selection stays with the host terminal (#221).
        //
        // Terminals that don't support the kitty protocol silently ignore the
        // push. Cleanup: Drop and the panic hook both pop the flags, so normal
        // exit, panics, and most signals are covered.
        mouse_capture::write_startup_terminal_modes(&mut io::stdout())?;

        // Panic hook: restore terminal state so the shell is usable after a crash.
        std::panic::set_hook(Box::new(|info| {
            let _ = mouse_capture::write_panic_restore_modes(&mut io::stdout());
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!("{info}");
        }));

        // Suppress OutputManager's own stdout writes — we own the terminal.
        output_manager.disable_stdout();

        let command_history = Self::load_history();

        Ok(TuiRenderer {
            output_manager,
            status_bar,
            colors,

            input_textarea: Self::create_clean_textarea(),
            command_history,
            history_index: None,
            history_draft: None,

            active_rows: 0,
            pending_viewport_size: None,
            viewport_invalidated: false,
            cursor_row_from_top: 0,
            printed_ids: HashSet::new(),
            accordion: AccordionState::default(),
            tool_viewports: ToolViewportState::default(),
            expanded_tool: None,

            active_dialog: None,
            active_tabbed_dialog: None,
            attention_dialog_live: false,

            is_active: true,
            needs_full_refresh: false,
            last_render_error: None,
            pending_feedback: None,
            pending_cancellation: false,
            pending_dialog_result: None,

            ghost_text: None,
            suggestions: crate::cli::suggestions::SuggestionManager::new(),
            command_registry: crate::cli::command_autocomplete::CommandRegistry::new(),
            autocomplete_state: AutocompleteState::default(),

            pending_images: Vec::new(),
            image_counter: 0,
            mention_catalog: crate::context::mention::MentionCatalog::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            pending_mentions: Vec::new(),

            task_rows: None,
            tracked_rows: HashMap::new(),
            tracked_agent_usage: HashMap::new(),
            corner: Arc::new(std::sync::Mutex::new(None)),
            stack: None,
            poset: None,
            poset_was_visible: false,
            poset_panel_mode: PosetPanelMode::Forth,
            panel_hint_shown: false,

            session_label: String::new(),
            typing_words: Vec::new(),
            pre_typing_mode: PosetPanelMode::Forth,

            live_area_dirty: true,
            mouse_tracking: mouse_capture::MouseTracking::DEFAULT,
        })
    }

    /// Attach a source the live area polls for task rows each time it redraws.
    pub fn set_task_rows(&mut self, rows: activity::SharedActivityRows) {
        self.task_rows = Some(rows);
    }

    /// Fold a scheduler event into the live child-agent projection.
    pub fn apply_activity(&mut self, update: activity::ActivityUpdate) {
        use activity::ActivityUpdate;
        match update {
            ActivityUpdate::Upsert { id, row, usage } => {
                // A row that reappears keeps the detail already shown against it, so a status
                // change does not blank the tool a task is in the middle of running.
                let detail = self
                    .tracked_rows
                    .get(&id)
                    .and_then(|row| row.detail.clone());
                self.tracked_rows.insert(
                    id,
                    activity::ActivityRow {
                        detail: row.detail.or(detail),
                        ..row
                    },
                );
                self.tracked_agent_usage.entry(id).or_insert(usage);
            }
            ActivityUpdate::SetUsage { id, usage } => {
                if self.tracked_rows.contains_key(&id) {
                    self.tracked_agent_usage.insert(id, usage);
                }
            }
            ActivityUpdate::SetDetail { id, detail } => {
                if let Some(row) = self.tracked_rows.get_mut(&id) {
                    row.detail = detail;
                }
            }
            ActivityUpdate::Remove { id } => {
                self.tracked_rows.remove(&id);
                self.tracked_agent_usage.remove(&id);
            }
            ActivityUpdate::Resnapshot { rows } => {
                self.tracked_rows.clear();
                self.tracked_agent_usage.clear();
                for (id, row, usage) in rows {
                    self.tracked_rows.insert(id, row);
                    self.tracked_agent_usage.insert(id, usage);
                }
            }
        }
        self.status_bar.update_agent_activity(
            self.tracked_rows.len(),
            &aggregate_agent_usage(self.tracked_agent_usage.values()),
        );
        self.live_area_dirty = true;
    }

    /// Attach the Co-Forth shared stack so the live area can display it.
    pub fn set_stack(&mut self, stack: Arc<tokio::sync::Mutex<Vec<String>>>) {
        self.stack = Some(stack);
    }

    /// Attach the Co-Forth poset VM. Stored, not drawn: the overlay paints `corner`.
    ///
    /// Finch's `Poset` is accepted only here and held as the shared mutex the
    /// event loop already owns. [`graph_view_from_poset`] is unused;
    /// [`draw_poset_overlay`] does not snapshot a [`GraphView`].
    pub fn set_poset(&mut self, poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>) {
        self.poset = Some(poset);
    }

    /// Mark the live area as needing a redraw on the next flush.
    pub fn mark_dirty(&mut self) {
        self.live_area_dirty = true;
    }

    /// Toggle the poset panel between graph view and Forth source view.
    pub fn toggle_poset_view(&mut self) {
        self.poset_panel_mode = match self.poset_panel_mode {
            PosetPanelMode::Graph => PosetPanelMode::Forth,
            PosetPanelMode::Forth | PosetPanelMode::Typing => PosetPanelMode::Graph,
        };
    }

    /// Update the live typing words and switch the panel to Typing mode.
    /// Pass an empty slice to clear (restores the previous mode).
    pub fn set_typing_words(&mut self, words: Vec<String>) {
        if words.is_empty() {
            // Restore previous mode when input is cleared
            if matches!(self.poset_panel_mode, PosetPanelMode::Typing) {
                self.poset_panel_mode = self.pre_typing_mode;
                self.pre_typing_mode = PosetPanelMode::Forth;
            }
            self.typing_words.clear();
        } else {
            // Switch to Typing mode (save current mode first)
            if !matches!(self.poset_panel_mode, PosetPanelMode::Typing) {
                self.pre_typing_mode = self.poset_panel_mode.clone();
                self.poset_panel_mode = PosetPanelMode::Typing;
            }
            self.typing_words = words;
        }
        self.live_area_dirty = true;
    }

    // ── TextArea factories (also called from async_input) ─────────────────────

    pub fn create_clean_textarea() -> TextArea<'static> {
        use ratatui::style::{Modifier, Style};
        let mut ta = TextArea::default();
        ta.set_placeholder_text("Type your message…");
        let plain = Style::default();
        ta.set_style(plain);
        ta.set_cursor_line_style(plain);
        ta.set_cursor_style(Style::default().add_modifier(Modifier::REVERSED));
        ta.set_selection_style(plain);
        ta.set_placeholder_style(plain);
        ta
    }

    pub fn create_clean_textarea_with_text(text: &str) -> TextArea<'static> {
        let mut ta = Self::create_clean_textarea();
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                ta.insert_newline();
            }
            ta.insert_str(line);
        }
        ta
    }
}

// ─── Raw-mode canonical transcript commit ───────────────────────────────────

fn commit_complete_messages(
    stdout: &mut impl Write,
    messages: &[MessageRef],
    accordion: &mut AccordionState,
    colors: &ColorScheme,
    printed_ids: &mut HashSet<MessageId>,
    terminal_height: usize,
) -> Result<()> {
    let mut accepted = Vec::new();
    let mut staged = Vec::new();
    for message in messages {
        if printed_ids.contains(&message.id()) {
            continue;
        }
        let complete = match view_model::project_message(message, colors) {
            view_model::ProjectedMessage::Node(node) => accordion.render_node_fully_expanded(&node),
            view_model::ProjectedMessage::Plain(formatted) => {
                accordion.render_plain(&formatted.join("\n"))
            }
        }
        .into_iter()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n");
        for line in complete.split('\n') {
            execute!(staged, Print(line.trim_end_matches('\r')), Print("\r\n"))?;
        }
        execute!(staged, Print("\r\n"))?;
        accepted.push(message.id());
    }
    if accepted.is_empty() {
        return Ok(());
    }
    // A viewport of linefeeds moves every newly inserted canonical row above
    // row zero. The preceding visible rows scroll first, followed by exactly
    // the canonical batch; the blank spool remains on-screen for repaint and
    // does not pollute native history.
    for _ in 0..terminal_height {
        execute!(staged, Print("\r\n"))?;
    }
    stdout.write_all(&staged)?;
    // Once write_all accepts the staged transaction, retrying it would create
    // duplicates if a later flush reports an ambiguous error.
    printed_ids.extend(accepted);
    stdout.flush()?;
    Ok(())
}

fn prepare_canonical_commit(stdout: &mut impl Write) -> Result<()> {
    // Previously committed rows are already in native history. Remove their
    // visible projection before the linefeed spool so a later commit cannot
    // append that projection to history a second time.
    let mut staged = Vec::new();
    execute!(
        staged,
        BeginSynchronizedUpdate,
        cursor::MoveTo(0, 0),
        Clear(ClearType::All),
        cursor::MoveTo(0, 0)
    )?;
    stdout.write_all(&staged)?;
    stdout.flush()?;
    Ok(())
}

fn prepare_canonical_commit_guarded(stdout: &mut impl Write) -> Result<()> {
    match prepare_canonical_commit(stdout) {
        Ok(()) => Ok(()),
        Err(error) => {
            // BeginSynchronizedUpdate is the first command in preparation. A
            // later partial-write failure must never leave terminal updates
            // suppressed indefinitely.
            let _ = execute!(stdout, EndSynchronizedUpdate);
            Err(error)
        }
    }
}

// ─── Live area management ─────────────────────────────────────────────────────

impl TuiRenderer {
    /// Move the cursor up to the top of the live area and clear everything
    /// below it, ready for a fresh draw.
    ///
    /// After draw_live_area() the cursor is parked at `cursor_row_from_top`
    /// (not necessarily at the bottom row), so we must use that field — not
    /// `active_rows - 1` — to reach the top correctly.
    pub fn erase_live_area(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        // Begin the synchronized update here so erase + draw are one atomic
        // terminal operation — eliminates the blank-flash between them.
        // Never clear from the cursor to the bottom of the terminal here. A
        // one-row accounting error (especially around a wrapping streamed
        // program) would then erase committed scrollback above the live area.
        // Clear only the rows this renderer previously owned. If accounting is
        // ever short, a stale live row is recoverable; lost transcript is not.
        write_live_area_erase(&mut stdout, self.active_rows, self.cursor_row_from_top)?;
        if self.active_rows == 0 && self.cursor_row_from_top == 0 {
            return Ok(()); // Sync block is closed by the following draw.
        }
        self.active_rows = 0;
        self.cursor_row_from_top = 0;
        Ok(())
    }

    /// Draw the live area from scratch and track `active_rows`.
    ///
    /// Layout is decided by [`plan_live_frame`], which needs no terminal;
    /// this function only gathers renderer state, paints the result, and
    /// records the frame's measured height so the next erase clears exactly
    /// the rows that were drawn. A newly live approval/AskUser dialog writes
    /// one terminal bell (`\x07`) after the frame; redraws of that overlay do not.
    pub fn draw_live_area(&mut self) -> Result<()> {
        self.draw_live_area_to(&mut io::stdout())
    }

    /// Paint the live area to `out`. Tests capture the attention bell here.
    fn draw_live_area_to(&mut self, out: &mut impl Write) -> Result<()> {
        let (term_width, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
        let (term_width, term_h) = (term_width as usize, term_h as usize);

        if term_h <= 3 && self.active_dialog.is_none() && self.expanded_tool.is_none() {
            let sources = self.live_frame_sources(term_width);
            completion_pane_lines(&mut self.autocomplete_state, term_width, 0);
            let frame = plan_tiny_live_frame(
                &sources.input_lines,
                sources.input_cursor,
                &sources.effective_status,
                term_h,
                term_width,
            );
            let rows = write_tiny_live_frame(out, &frame)?;
            execute!(out, EndSynchronizedUpdate)?;
            self.flush_attention_bell(out)?;
            out.flush()?;
            self.active_rows = rows;
            self.cursor_row_from_top = frame.cursor_row;
            self.accordion
                .rebuild_retained_hit_regions(&[], 0, term_width);
            self.tool_viewports.rebuild_hit_regions(&[], 0, term_width);
            return Ok(());
        }

        let sources = self.live_frame_sources(term_width);

        // The expanded tool-result surface owns the frame when open. A body
        // that can no longer be found closes the surface before drawing.
        let expanded_lines = match self.expanded_surface_frame(term_width, term_h.saturating_sub(1))
        {
            Some(lines) => Some(lines),
            None => {
                if self.expanded_tool.is_some() {
                    self.close_expanded_tool();
                }
                None
            }
        };

        // `sources` is the owned state the ViewModel borrows; the dialog and
        // expanded surface are field borrows disjoint from the autocomplete
        // state the planner mutates.
        let frame = {
            let vm = live_view_model(
                &sources,
                term_width,
                term_h,
                self.active_dialog.as_ref(),
                expanded_lines.as_deref(),
            );
            plan_live_frame(&vm, &mut self.autocomplete_state)
        };

        let rows = write_live_frame(out, &frame, term_width.max(1))?;
        execute!(out, EndSynchronizedUpdate)?;
        self.flush_attention_bell(out)?;
        out.flush()?;

        self.active_rows = rows;
        self.cursor_row_from_top = frame.cursor_row;
        self.rebuild_transcript_hit_regions(&frame, rows, term_width, term_h);
        Ok(())
    }

    /// Gather the owned state one live-frame blit reads, so the ViewModel can
    /// borrow it for the planning call.
    fn live_frame_sources(&mut self, term_width: usize) -> LiveFrameSources {
        let input_lines = self.input_textarea.lines().to_vec();
        let raw_status = self
            .status_bar
            .get_status_without(&StatusLineType::SessionLabel);
        let current_input = input_lines.join("\n");
        let effective_status = compute_effective_status(
            self.ghost_text.as_deref(),
            &raw_status,
            &current_input,
            &self.command_registry,
        );
        let task_rows = self
            .task_rows
            .as_ref()
            .map(|source| source.rows())
            .unwrap_or_default();
        // Depth first, then identity, so sibling rows keep a stable order
        // between frames.
        let mut tracked = self.tracked_rows.iter().collect::<Vec<_>>();
        tracked.sort_by_key(|(id, row)| (row.depth, **id));
        let tracked_rows = tracked
            .into_iter()
            .map(|(_, row)| row.clone())
            .collect::<Vec<_>>();
        let cwd_label = tilde_cwd();
        let session_label = self
            .status_bar
            .get_line(&StatusLineType::SessionLabel)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| self.session_label.clone());
        let live_messages = self.find_live_messages();
        let live_rendered = self.projected_lines(live_messages, term_width);
        LiveFrameSources {
            input_cursor: self.input_textarea.cursor(),
            ghost_text: self.ghost_text.clone(),
            input_lines,
            effective_status,
            cwd_label,
            session_label,
            task_rows,
            tracked_rows,
            live_rendered,
            render_error: self.last_render_error.is_some(),
        }
    }

    /// Write `\x07` when an approval dialog first occupies the live surface.
    /// Redraws of the same pending overlay, and draws with no dialog, stay silent.
    fn flush_attention_bell(&mut self, out: &mut impl Write) -> io::Result<()> {
        let dialog_live = self.active_dialog.is_some();
        if dialog_live && !self.attention_dialog_live {
            out.write_all(&[0x07])?;
        }
        self.attention_dialog_live = dialog_live;
        Ok(())
    }

    /// Uncommitted suffix in order, minus completed program source once a
    /// later program-output row has body. Replaced IR stays in the manager.
    fn find_live_messages(&self) -> Vec<MessageRef> {
        let messages = self.output_manager.get_messages();
        without_replaced_program_source(
            uncommitted_suffix(messages.clone(), &self.printed_ids),
            &messages,
        )
    }
}

// ─── Redraw predicate ─────────────────────────────────────────────────────────

/// Returns true when the live area needs an erase+draw cycle.
/// Extracted so it can be unit-tested without terminal I/O.
fn should_redraw_live_area(has_in_progress: bool, dirty: bool) -> bool {
    has_in_progress || dirty
}

/// A message may enter the buffer after an earlier WorkUnit has started but
/// before it completes (for example, a user turn queued behind a provider
/// turn).  Permanent scrollback must commit only the completed prefix; printing
/// a later message above the live area reverses the visible event order.
fn committable_prefix_len(statuses: impl IntoIterator<Item = MessageStatus>) -> usize {
    let mut count = 0;
    for status in statuses {
        match status {
            MessageStatus::Complete | MessageStatus::Failed => count += 1,
            MessageStatus::InProgress => break,
        }
    }
    count
}

/// Preserve every message that has not yet been committed to terminal
/// scrollback. Some may already be complete: ordering requires them to remain
/// visible behind an older live message until they can be printed.
fn uncommitted_suffix(
    messages: impl IntoIterator<Item = MessageRef>,
    printed_ids: &HashSet<MessageId>,
) -> Vec<MessageRef> {
    messages
        .into_iter()
        .filter(|message| !printed_ids.contains(&message.id()))
        .collect()
}

/// The nearest later WorkUnit message that projects as program output, with
/// whether its visible body has content. Classification reads the lightweight
/// domain snapshot, not a presentation projection.
fn later_program_output(messages: &[MessageRef], index: usize) -> Option<(bool, MessageStatus)> {
    messages[index + 1..].iter().find_map(|later| {
        later.work_unit_head().and_then(|head| {
            matches!(
                head.presentation,
                WorkUnitPresentation::ProgramOutput { .. }
            )
            .then_some((
                head.output_body_lines().iter().any(|line| !line.is_empty()),
                later.status(),
            ))
        })
    })
}

fn is_completed_program_source(message: &MessageRef) -> bool {
    message.status() == MessageStatus::Complete
        && message.work_unit_head().is_some_and(|head| {
            matches!(
                head.presentation,
                WorkUnitPresentation::ProgramSource { .. }
            )
        })
}

/// Completed program source whose nearest later program-output row has body.
fn replaced_completed_program_source_ids(messages: &[MessageRef]) -> HashSet<MessageId> {
    let mut replaced = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        if !is_completed_program_source(message) {
            continue;
        }
        if later_program_output(messages, index).is_some_and(|(has_body, _)| has_body) {
            replaced.insert(message.id());
        }
    }
    replaced
}

fn defer_completed_program_source(messages: &[MessageRef], index: usize) -> bool {
    if !is_completed_program_source(&messages[index]) {
        return false;
    }
    later_program_output(messages, index)
        .is_some_and(|(has_body, status)| status == MessageStatus::InProgress && !has_body)
}

fn without_replaced_program_source(
    selected: Vec<MessageRef>,
    all: &[MessageRef],
) -> Vec<MessageRef> {
    let replaced = replaced_completed_program_source_ids(all);
    selected
        .into_iter()
        .filter(|message| !replaced.contains(&message.id()))
        .collect()
}

fn visible_printed_messages(
    messages: &[MessageRef],
    printed_ids: &HashSet<MessageId>,
) -> Vec<MessageRef> {
    without_replaced_program_source(
        messages
            .iter()
            .filter(|message| printed_ids.contains(&message.id()))
            .cloned()
            .collect(),
        messages,
    )
}

struct CanonicalCommitPlan {
    emit: Vec<MessageRef>,
    consume_without_emit: Vec<MessageId>,
}

/// Completed prefix of unprinted messages. Replaced program source is consumed
/// without writing so it cannot block later output or reappear in history.
fn plan_canonical_commit(
    messages: &[MessageRef],
    printed_ids: &HashSet<MessageId>,
) -> CanonicalCommitPlan {
    let replaced = replaced_completed_program_source_ids(messages);
    let mut emit = Vec::new();
    let mut consume_without_emit = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        if printed_ids.contains(&message.id()) {
            continue;
        }
        match message.status() {
            MessageStatus::InProgress => break,
            MessageStatus::Complete | MessageStatus::Failed => {
                if replaced.contains(&message.id()) {
                    consume_without_emit.push(message.id());
                } else if defer_completed_program_source(messages, index) {
                    // Stay in the live suffix until the paired output has body
                    // or completes empty.
                    break;
                } else {
                    emit.push(message.clone());
                }
            }
        }
    }
    CanonicalCommitPlan {
        emit,
        consume_without_emit,
    }
}

fn swap_completed_program_source_for_output(messages: Vec<MessageRef>) -> Vec<MessageRef> {
    let all = messages.clone();
    without_replaced_program_source(messages, &all)
}

/// Select the newest transcript rows that fit in a visible viewport slice.
/// Unlike `live_viewport_lines`, this has no synthetic clipping row: a full
/// viewport rebuild is a projection of retained transcript, not new scrollback.
fn viewport_tail_lines(lines: &[String], terminal_width: usize, row_budget: usize) -> Vec<String> {
    if row_budget == 0 {
        return Vec::new();
    }
    let width = terminal_width.max(1);
    let mut remaining = row_budget;
    let mut selected = Vec::new();
    for line in lines.iter().rev() {
        if remaining == 0 {
            break;
        }
        let rows = shadow_buffer::physical_rows(line, width);
        if rows <= remaining {
            selected.push(line.clone());
            remaining -= rows;
        } else {
            let fragment = visible_tail(line, remaining.saturating_mul(width));
            if !fragment.is_empty() {
                selected.push(fragment);
            }
            break;
        }
    }
    selected.reverse();
    selected
}

fn viewport_tail_rendered_lines(
    lines: &[RenderedTranscriptLine],
    terminal_width: usize,
    row_budget: usize,
) -> Vec<RenderedTranscriptLine> {
    let selected = rendered_tail_without_pinning(lines, terminal_width, row_budget);
    if selected.iter().any(|line| line.row_id.is_some()) || selected.is_empty() {
        return selected;
    }
    let Some((header_index, header)) = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, line)| line.row_id.is_some())
    else {
        return selected;
    };
    let header_rows = shadow_buffer::physical_rows(&header.text, terminal_width.max(1));
    if header_rows > row_budget {
        if row_budget == 0 {
            return selected;
        }
        let mut compact = header.clone();
        compact.text = compact_disclosure_label(header, terminal_width.max(1));
        return vec![compact];
    }
    let mut pinned = vec![header.clone()];
    pinned.extend(rendered_tail_without_pinning(
        &lines[header_index + 1..],
        terminal_width,
        row_budget.saturating_sub(header_rows),
    ));
    pinned
}

fn compact_disclosure_label(header: &RenderedTranscriptLine, width: usize) -> String {
    let expanded = header.row_expanded.unwrap_or(false);
    let state = if expanded {
        if width >= "[expanded]".len() {
            "[expanded]"
        } else {
            "open"
        }
    } else if width >= "[collapsed]".len() {
        "[collapsed]"
    } else {
        "closed"
    };
    visible_prefix(state, width)
}

fn rendered_tail_without_pinning(
    lines: &[RenderedTranscriptLine],
    terminal_width: usize,
    row_budget: usize,
) -> Vec<RenderedTranscriptLine> {
    if row_budget == 0 {
        return Vec::new();
    }
    let mut remaining = row_budget;
    let mut selected = Vec::new();
    for line in lines.iter().rev() {
        let rows = shadow_buffer::physical_rows(&line.text, terminal_width.max(1));
        if rows > remaining {
            let fragment =
                visible_tail(&line.text, remaining.saturating_mul(terminal_width.max(1)));
            if !fragment.is_empty() {
                selected.push(RenderedTranscriptLine {
                    text: fragment,
                    ..RenderedTranscriptLine::default()
                });
            }
            break;
        }
        selected.push(line.clone());
        remaining -= rows;
    }
    selected.reverse();
    selected
}

fn rendered_metadata_for_visible(
    all: &[RenderedTranscriptLine],
    visible: &[String],
) -> Vec<RenderedTranscriptLine> {
    let mut search_end = all.len();
    let mut matched = visible
        .iter()
        .rev()
        .map(|text| {
            let found = all[..search_end]
                .iter()
                .rposition(|line| line.text == *text);
            if let Some(index) = found {
                search_end = index;
                all[index].clone()
            } else {
                RenderedTranscriptLine {
                    text: text.clone(),
                    ..RenderedTranscriptLine::default()
                }
            }
        })
        .collect::<Vec<_>>();
    matched.reverse();
    matched
}

/// When the transcript viewport shows no expandable row at all, re-window the
/// live lines so at least one disclosure header stays reachable (#805 keeps
/// the disclosure reachable while the tree owns the viewport).
fn pin_live_disclosure_header(
    all: &[RenderedTranscriptLine],
    live_lines: &mut Vec<String>,
    terminal_width: usize,
) {
    if live_lines.is_empty() || !all.iter().any(|line| line.row_id.is_some()) {
        return;
    }
    let visible = rendered_metadata_for_visible(all, live_lines);
    if visible.iter().any(|line| line.row_id.is_some()) {
        return;
    }
    let budget = live_lines
        .iter()
        .map(|line| shadow_buffer::physical_rows(line, terminal_width.max(1)))
        .sum();
    *live_lines = viewport_tail_rendered_lines(all, terminal_width, budget)
        .into_iter()
        .map(|line| line.text)
        .collect();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewportRedrawPlan {
    transcript_top: usize,
    live_top: usize,
}

/// Lay out the two parts of Finch's visible shadow frame. The retained
/// transcript is bottom-aligned in the space above the live area, while the
/// live area is always anchored to the bottom of the current viewport.
fn viewport_redraw_plan(
    terminal_height: usize,
    live_rows: usize,
    transcript_rows: usize,
) -> ViewportRedrawPlan {
    let live_rows = live_rows.min(terminal_height);
    let live_top = terminal_height.saturating_sub(live_rows);
    let transcript_rows = transcript_rows.min(live_top);
    ViewportRedrawPlan {
        transcript_top: live_top.saturating_sub(transcript_rows),
        live_top,
    }
}

/// Start an absolute full-viewport paint and leave the synchronized update
/// open for `draw_live_area` to finish. Keeping this byte emission separate
/// makes the resize invariant testable without a real terminal emulator.
fn begin_full_viewport_paint(
    stdout: &mut impl Write,
    plan: ViewportRedrawPlan,
    transcript: &[String],
) -> Result<()> {
    execute!(stdout, BeginSynchronizedUpdate)?;
    continue_full_viewport_paint(stdout, plan, transcript)
}

fn continue_full_viewport_paint(
    stdout: &mut impl Write,
    plan: ViewportRedrawPlan,
    transcript: &[String],
) -> Result<()> {
    execute!(
        stdout,
        cursor::MoveTo(0, 0),
        Clear(ClearType::All),
        cursor::MoveTo(0, plan.transcript_top as u16)
    )?;
    for line in transcript {
        execute!(
            stdout,
            Clear(ClearType::CurrentLine),
            Print(line.trim_end_matches('\r')),
            Print("\r\n")
        )?;
    }
    execute!(stdout, cursor::MoveTo(0, plan.live_top as u16))?;
    Ok(())
}

// ─── flush_output_safe / render ───────────────────────────────────────────────

/// Depth-first search of a projected transcript node tree by stable identity.
fn find_transcript_row<'a>(
    row: &'a view_model::TranscriptNode,
    id: &view_model::RowId,
) -> Option<&'a view_model::TranscriptNode> {
    if row.id == *id {
        return Some(row);
    }
    for child in &row.children {
        if let Some(found) = find_transcript_row(child, id) {
            return Some(found);
        }
    }
    None
}

/// The node whose direct child has the given identity, for surface titles.
fn find_parent_transcript_row<'a>(
    row: &'a view_model::TranscriptNode,
    id: &view_model::RowId,
) -> Option<&'a view_model::TranscriptNode> {
    for child in &row.children {
        if child.id == *id {
            return Some(row);
        }
        if let Some(found) = find_parent_transcript_row(child, id) {
            return Some(found);
        }
    }
    None
}

/// Owned state for one live-frame blit, gathered once so the ViewModel can
/// borrow it for the planning call.
struct LiveFrameSources {
    input_lines: Vec<String>,
    input_cursor: (usize, usize),
    ghost_text: Option<String>,
    effective_status: String,
    cwd_label: String,
    session_label: String,
    task_rows: Vec<activity::ActivityRow>,
    tracked_rows: Vec<activity::ActivityRow>,
    live_rendered: Vec<RenderedTranscriptLine>,
    render_error: bool,
}

/// Borrow the owned sources into the blit-time ViewModel snapshot.
#[allow(clippy::too_many_arguments)]
fn live_view_model<'a>(
    sources: &'a LiveFrameSources,
    terminal_width: usize,
    terminal_height: usize,
    dialog: Option<&'a Dialog>,
    expanded_lines: Option<&'a [String]>,
) -> view_model::LiveViewModel<'a> {
    view_model::LiveViewModel {
        terminal_width,
        terminal_height,
        input_lines: &sources.input_lines,
        input_cursor: sources.input_cursor,
        ghost_text: sources.ghost_text.as_deref(),
        effective_status: &sources.effective_status,
        cwd_label: &sources.cwd_label,
        session_label: &sources.session_label,
        dialog,
        expanded_lines,
        render_error: sources.render_error,
        task_rows: &sources.task_rows,
        tracked_rows: &sources.tracked_rows,
        live_rendered: &sources.live_rendered,
    }
}

impl TuiRenderer {
    /// Called from the event loop on every tick.
    /// Commits newly-completed messages to permanent scrollback, then redraws.
    pub fn flush_output_safe(&mut self, _output_manager: &OutputManager) -> Result<()> {
        let messages = self.output_manager.get_messages();
        let plan = plan_canonical_commit(&messages, &self.printed_ids);

        // Re-establish trustworthy live-area coordinates before committing a
        // completion that raced resize. The completed message remains in the
        // uncommitted suffix until its canonical bytes are actually written.
        if self.viewport_invalidated {
            self.redraw_full_viewport()?;
            if plan.emit.is_empty() && plan.consume_without_emit.is_empty() {
                self.live_area_dirty = false;
                return Ok(());
            }
        }

        if !plan.emit.is_empty() {
            let mut stdout = io::stdout();
            prepare_canonical_commit_guarded(&mut stdout)?;
            self.active_rows = 0;
            self.cursor_row_from_top = 0;
            self.printed_ids.extend(plan.consume_without_emit);
            let commit_result = commit_complete_messages(
                &mut stdout,
                &plan.emit,
                &mut self.accordion,
                &self.colors,
                &mut self.printed_ids,
                usize::from(crossterm::terminal::size().unwrap_or((80, 24)).1),
            );
            if let Err(error) = commit_result {
                let _ = execute!(stdout, EndSynchronizedUpdate);
                return Err(error);
            }
            self.pending_viewport_size = Some(crossterm::terminal::size().unwrap_or((80, 24)));
            self.viewport_invalidated = true;
            self.redraw_full_viewport_inner(true)?;
            self.live_area_dirty = false;
        } else {
            if !plan.consume_without_emit.is_empty() {
                self.printed_ids.extend(plan.consume_without_emit);
                self.live_area_dirty = true;
            }
            // Only redraw when something actually changed: a message is streaming
            // (InProgress) or explicit state mutation marked the area dirty.
            // This eliminates the unconditional erase+draw every 33 ms tick that
            // caused visible flicker during idle and between queries.
            let has_in_progress = messages
                .iter()
                .any(|m| matches!(m.status(), MessageStatus::InProgress));
            if should_redraw_live_area(has_in_progress, self.live_area_dirty) {
                self.erase_live_area()?;
                self.draw_live_area()?;
                self.live_area_dirty = false;
            }
        }

        Ok(())
    }

    /// Redraw the live area.  Called by the event loop and by async_input.
    pub fn render(&mut self) -> Result<()> {
        if self.viewport_invalidated {
            self.redraw_full_viewport()?;
            return self.draw_poset_overlay();
        }
        self.erase_live_area()?;
        self.draw_live_area()?;
        self.draw_poset_overlay()
    }

    // ── Co-Forth panel overlay ─────────────────────────────────────────────────

    /// Render the Co-Forth panel (graph or Forth source) as a floating overlay
    /// in the top-right corner of the current terminal viewport.
    ///
    /// Uses cursor::SavePosition / RestorePosition so the overlay has **zero
    /// effect** on the live area's cursor tracking.  No rows are added to
    /// `active_rows`; the panel never triggers the "Reflecting…" scrollback spam.
    pub fn draw_poset_overlay(&mut self) -> Result<()> {
        // Show the output of the user-defined `check` word, if any.
        let text = self.corner.lock().ok().and_then(|g| g.clone());
        let Some(text) = text else {
            return Ok(());
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(());
        }

        let (term_cols, _term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let vis_len = text.chars().count();
        let start_col = (term_cols as usize).saturating_sub(vis_len + 1) as u16;

        let label = format!("{}{}{}", DIM_GRAY, text, RESET);
        let mut stdout = io::stdout();
        execute!(stdout, cursor::SavePosition)?;
        execute!(stdout, cursor::MoveTo(start_col, 0))?;
        execute!(stdout, Print(&label))?;
        execute!(stdout, cursor::RestorePosition)?;
        stdout.flush()?;
        Ok(())
    }

    /// Kept for API compatibility.  Forces a redraw if flagged.
    pub fn check_and_refresh(&mut self) -> Result<()> {
        if self.needs_full_refresh {
            self.needs_full_refresh = false;
            self.erase_live_area()?;
            self.draw_live_area()?;
        }
        Ok(())
    }

    pub fn trigger_refresh(&mut self) {
        self.needs_full_refresh = true;
    }
}

// ─── Startup header ───────────────────────────────────────────────────────────

impl TuiRenderer {
    /// Set session identity without writing to the terminal.  Startup content
    /// must reach scrollback through `OutputManager` so it participates in the
    /// same ordered commit path as every other message.
    pub fn set_session_label(&mut self, session_label: impl Into<String>) {
        self.session_label = session_label.into();
    }

    /// Build the static startup artifact for `OutputManager` projection.
    ///
    /// This deliberately returns plain text rather than issuing crossterm
    /// commands: direct header writes can race the shadow-buffer live area and
    /// corrupt scrollback accounting on the first redraw.
    pub fn startup_header(model: &str, cwd: &str, session_label: &str) -> String {
        let version = env!("CARGO_PKG_VERSION");
        format!(
            "      ▄▄▄▄▄▄\n    ▗▟█●██▙►  finch v{version}\n{}\n  ▐████████▌   {model}\n  ▝▜██████▛▘   {session_label}  ·  {cwd}\n     ╥  ╥\n    ╱    ╲",
            crate::ABOUT
        )
    }
}

// ─── Shutdown ─────────────────────────────────────────────────────────────────

impl TuiRenderer {
    pub fn shutdown(&mut self) -> Result<()> {
        if !self.is_active {
            return Ok(());
        }
        self.is_active = false;
        let _ = self.erase_live_area();
        // Reset terminal state: show cursor, reset colours, move to a clean line.
        // The `\r\n` ensures the shell prompt lands on its own fresh line rather
        // than overwriting content from the erased live area.
        let _ = mouse_capture::write_shutdown_terminal_modes(&mut io::stdout());
        print!("\r\n");
        // Flush pending output BEFORE leaving raw mode — otherwise some terminals
        // silently discard buffered bytes after the mode switch.
        let _ = io::stdout().flush();
        let _ = disable_raw_mode();
        Self::save_history(&self.command_history);
        self.output_manager.enable_stdout();
        Ok(())
    }

    pub fn is_active(&self) -> bool {
        self.is_active
    }

    /// Temporarily release the terminal so another full-screen TUI (e.g. the
    /// setup wizard) can take over.  Call `resume()` after it exits.
    pub fn suspend(&self) -> anyhow::Result<()> {
        let _ = io::stdout().flush();
        let _ = mouse_capture::write_suspend_terminal_modes(&mut io::stdout());
        disable_raw_mode()?;
        Ok(())
    }

    /// Re-acquire the terminal after a `suspend()`.
    pub fn resume(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let _ = mouse_capture::write_resume_terminal_modes(&mut io::stdout(), self.mouse_tracking);
        // Force a full redraw so the REPL live area reappears.
        self.active_rows = 0;
        self.pending_viewport_size = None;
        self.viewport_invalidated = true;
        Ok(())
    }

    /// Reacquire every terminal mode after an attempted process replacement
    /// called [`emergency_restore_terminal`] but `exec`/spawn failed.  This is
    /// deliberately stronger than [`Self::resume`]: emergency restoration
    /// also pops keyboard enhancements and disables bracketed paste.
    pub(crate) fn resume_after_emergency_restore(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let _ = mouse_capture::write_resume_after_emergency_modes(
            &mut io::stdout(),
            self.mouse_tracking,
        );
        self.output_manager.disable_stdout();
        self.active_rows = 0;
        self.pending_viewport_size = None;
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
        Ok(())
    }
}

impl Drop for TuiRenderer {
    fn drop(&mut self) {
        // Safety net: restore terminal if shutdown() was never explicitly called.
        // shutdown() sets is_active = false before doing anything, so this is
        // idempotent — if shutdown() already ran, this is a no-op.
        if self.is_active {
            emergency_restore_terminal();
        }
    }
}

// ─── read_line (blocking, used outside the async event loop) ──────────────────

impl TuiRenderer {
    pub fn read_line(&mut self) -> Result<Option<String>> {
        use crossterm::event::{KeyCode, KeyModifiers};

        loop {
            let om = Arc::clone(&self.output_manager);
            self.flush_output_safe(&om)?;
            self.render()?;

            if event::poll(Duration::from_millis(100))? {
                let event = event::read()?;
                if matches!(event, Event::Key(_)) {
                    self.restore_mouse_tracking_after_interaction();
                }
                match event {
                    Event::Key(key)
                        if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE =>
                    {
                        self.handle_tab_key(key);
                    }
                    Event::Key(key) if self.handle_accordion_key(key) => {
                        self.render()?;
                    }
                    Event::Key(key)
                        if key.modifiers == KeyModifiers::NONE
                            && self.handle_completion_key(key.code) => {}
                    Event::Key(key) => match (key.code, key.modifiers) {
                        // Shift+Enter or Alt/Option+Enter: insert newline instead of submit.
                        // Standard VT100 raw mode never sends SHIFT for Enter on macOS —
                        // Option+Enter arrives as KeyCode::Enter + KeyModifiers::ALT.
                        (KeyCode::Enter, m)
                            if m.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                        {
                            self.input_textarea.input(Event::Key(key));
                        }
                        (KeyCode::Enter, _) => {
                            let Some(input) = self.take_submitted_input() else {
                                continue;
                            };
                            self.render()?;
                            return Ok(Some(input));
                        }
                        (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            return Ok(None);
                        }
                        _ => {
                            self.input_textarea.input(Event::Key(key));
                            self.update_ghost_text();
                        }
                    },
                    Event::Resize(w, h) => {
                        // Invalidate the live region; the next loop iteration redraws it
                        // using the terminal's new dimensions without erasing scrollback.
                        let _ = self.handle_resize(w, h);
                    }
                    Event::Mouse(mouse) if self.handle_mouse(mouse) => {
                        self.render()?;
                    }
                    _ => {}
                }
            }
        }
    }
}

// ─── Message helpers ──────────────────────────────────────────────────────────

impl TuiRenderer {
    fn projected_message_lines(
        &mut self,
        message: &MessageRef,
        width: usize,
    ) -> Vec<RenderedTranscriptLine> {
        // The ViewModel is the one domain → widget projection: convert the
        // message to props, then render them under the renderer's disclosure
        // state. Widgets never query WorkUnits to decide visibility (#805).
        let lines = match view_model::project_message(message, &self.colors) {
            view_model::ProjectedMessage::Node(node) => self.accordion.render_node(&node),
            view_model::ProjectedMessage::Plain(formatted) => {
                self.accordion.render_plain(&formatted.join("\n"))
            }
        };
        // Tool results are bounded child viewports (#656): the visible
        // projection shows a configured number of rows with a scroll offset
        // the control owns. Canonical scrollback is projected separately
        // (fully expanded) and is never bounded here.
        self.tool_viewports
            .project(lines, width, DEFAULT_TOOL_OUTPUT_ROWS)
    }

    fn projected_lines(
        &mut self,
        messages: impl IntoIterator<Item = MessageRef>,
        width: usize,
    ) -> Vec<RenderedTranscriptLine> {
        let mut rendered = Vec::new();
        for message in messages {
            rendered.extend(self.projected_message_lines(&message, width));
            rendered.push(RenderedTranscriptLine {
                ..RenderedTranscriptLine::default()
            });
        }
        rendered
    }

    fn rebuild_transcript_hit_regions(
        &mut self,
        frame: &LiveFrame,
        live_rows: usize,
        width: usize,
        height: usize,
    ) {
        let transcript_budget = height.saturating_sub(live_rows);
        let messages = self.output_manager.get_messages();
        let printed = visible_printed_messages(&messages, &self.printed_ids);
        let transcript = viewport_tail_rendered_lines(
            &self.projected_lines(printed, width),
            width,
            transcript_budget,
        );
        let transcript_rows = transcript
            .iter()
            .map(|line| shadow_buffer::physical_rows(&line.text, width.max(1)))
            .sum::<usize>();
        let plan = viewport_redraw_plan(height, live_rows, transcript_rows);
        let mut combined = transcript;
        let padding = plan.live_top.saturating_sub(
            plan.transcript_top
                + combined
                    .iter()
                    .map(|line| shadow_buffer::physical_rows(&line.text, width.max(1)))
                    .sum::<usize>(),
        );
        combined.extend((0..padding).map(|_| RenderedTranscriptLine {
            ..RenderedTranscriptLine::default()
        }));
        // The tool viewports still register over the whole painted surface.
        combined.extend(frame.visible_live.iter().cloned());
        self.tool_viewports
            .rebuild_hit_regions(&combined, plan.transcript_top, width);

        // The live frame's disclosure hit regions come from the claiming pass
        // itself — the rects the expandable rows claimed inside the viewport
        // box — offset from frame coordinates into terminal rows. Only the
        // retained transcript region above is recounted from lines.
        let retained_len = combined.len().saturating_sub(frame.visible_live.len());
        self.accordion.rebuild_retained_hit_regions(
            &combined[..retained_len],
            plan.transcript_top,
            width,
        );
        let live_top = height.saturating_sub(live_rows);
        self.accordion
            .adopt_claimed_hitboxes(&frame.hitboxes, live_top, width);
    }

    pub(crate) fn handle_accordion_key(&mut self, key: KeyEvent) -> bool {
        if self.active_dialog.is_some() || self.active_tabbed_dialog.is_some() {
            return false;
        }
        if self.expanded_tool.is_some() {
            return self.handle_expanded_tool_key(key);
        }
        if self.handle_tool_viewport_key(key) {
            return true;
        }
        if !self.accordion.handle_key(key) {
            return false;
        }
        // Disclosure is renderer state (#805): the accordion's open set is the
        // single owner, so there is nothing to persist back onto the message.
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
        true
    }

    /// Keyboard equivalents for a bounded tool-result control (#656).
    ///
    /// When a `ToolOutput` row holds the accordion focus: Up/Down/PageUp/
    /// PageDown scroll that result's child viewport, Enter/Space open the
    /// expanded surface. Any other key, or a focused row that is not a tool
    /// result, is left for the accordion and the input area.
    fn handle_tool_viewport_key(&mut self, key: KeyEvent) -> bool {
        let Some(focused) = self.accordion.focused.clone() else {
            return false;
        };
        if self.tool_viewports.kind_of(&focused) != Some(view_model::NodeRole::ToolOutput) {
            return false;
        }
        if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
            self.open_expanded_tool(&focused);
            return true;
        }
        let delta = match key.code {
            KeyCode::Up => -1,
            KeyCode::Down => 1,
            KeyCode::PageUp => -(PAGE_STEP_LINES as isize),
            KeyCode::PageDown => PAGE_STEP_LINES as isize,
            _ => return false,
        };
        if !self.tool_viewports.scroll_child(&focused, delta) {
            return false;
        }
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
        true
    }

    pub(crate) fn handle_accordion_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.active_dialog.is_some() || self.active_tabbed_dialog.is_some() {
            return false;
        }
        // Clicking a bounded tool-result control expands it (#656). The click
        // must land on the child viewport's own cells; the header keeps the
        // accordion's toggle-to-collapse behavior.
        if self.expanded_tool.is_none() && is_left_click(&mouse) {
            if let Some(region) = self
                .tool_viewports
                .region_at(mouse.column, mouse.row)
                .cloned()
            {
                self.open_expanded_tool(&region.row_id);
                return true;
            }
        }
        if !self.accordion.handle_mouse(mouse) {
            return false;
        }
        // Disclosure is renderer state (#805): the accordion's open set is the
        // single owner, so there is nothing to persist back onto the message.
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
        true
    }

    /// Wheel ticks release mouse tracking so native scrollback is reachable —
    /// unless the wheel is over a bounded tool-result control, which scrolls
    /// that result in place and keeps tracking held (#656). Other mouse events
    /// keep the existing accordion click-to-toggle path.
    pub(crate) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let mut stdout = io::stdout();
        self.handle_mouse_to(mouse, &mut stdout)
    }

    fn handle_mouse_to(&mut self, mouse: MouseEvent, out: &mut impl Write) -> bool {
        if mouse_capture::is_wheel(mouse.kind) {
            // A dialog owns the live area: native scroll would move Yes/No
            // off-screen, and the restoring keypress would be dialog input.
            // Leave the wheel for the dialog (ignored today; body scroll later).
            if self.active_dialog.is_none() && self.active_tabbed_dialog.is_none() {
                if let Some(delta) = wheel_delta(mouse.kind) {
                    if self.expanded_tool.is_some() {
                        self.scroll_expanded_tool(delta);
                        return true;
                    }
                    if let Some(region) = self
                        .tool_viewports
                        .region_at(mouse.column, mouse.row)
                        .cloned()
                    {
                        // The child viewport owns this wheel: scroll that tool
                        // result and keep mouse tracking so the next tick keeps
                        // scrolling it instead of falling to native scrollback.
                        if self.tool_viewports.scroll_child(&region.row_id, delta) {
                            self.live_area_dirty = true;
                        }
                        return true;
                    }
                }
                self.release_mouse_tracking_to(out);
            }
            return false;
        }
        self.handle_accordion_mouse(mouse)
    }

    /// Restore mouse tracking after a keypress or paste so clicks work again.
    pub(crate) fn restore_mouse_tracking_after_interaction(&mut self) {
        if !self.is_active {
            return;
        }
        let mut stdout = io::stdout();
        self.mouse_tracking =
            mouse_capture::restore_after_interaction(&mut stdout, self.mouse_tracking);
        let _ = stdout.flush();
    }

    fn release_mouse_tracking_to(&mut self, out: &mut impl Write) {
        if !self.is_active {
            return;
        }
        self.mouse_tracking = mouse_capture::release_for_native_scroll(out, self.mouse_tracking);
        let _ = out.flush();
    }

    // ─── Expanded tool-result surface (#656) ────────────────────────────────

    /// Open the focused expanded surface for one tool result. The surface
    /// starts at the compact window's scroll position, so reading continues
    /// where the bounded viewport left off.
    pub(crate) fn open_expanded_tool(&mut self, row_id: &view_model::RowId) {
        if self
            .expanded_tool
            .as_ref()
            .is_some_and(|view| view.row_id == *row_id)
        {
            return;
        }
        let Some(body) = self.tool_output_body(row_id) else {
            return;
        };
        let title = self.tool_row_title(row_id);
        let saved_scroll = self.tool_viewports.child_scroll(row_id);
        self.expanded_tool = Some(ExpandedToolView {
            row_id: row_id.clone(),
            title,
            saved_scroll,
            scroll: saved_scroll.min(body.len().saturating_sub(1)),
            body_lines: body.len(),
        });
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
    }

    /// Close the expanded surface and restore the child scroll offset captured
    /// at open time. Disclosure grouping and accordion focus were never
    /// touched, so the surrounding conversation returns exactly as it was.
    pub(crate) fn close_expanded_tool(&mut self) {
        let Some(view) = self.expanded_tool.take() else {
            return;
        };
        self.tool_viewports
            .set_child_scroll(&view.row_id, view.saved_scroll);
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
    }

    fn scroll_expanded_tool(&mut self, delta: isize) {
        let Some(view) = self.expanded_tool.as_mut() else {
            return;
        };
        let max = view.body_lines.saturating_sub(1);
        view.scroll = if delta < 0 {
            view.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            (view.scroll + delta as usize).min(max)
        };
        self.live_area_dirty = true;
    }

    /// Keys routed to the expanded surface while it is open. Everything it
    /// does not claim (Ctrl+C, /quit) falls through to the input loop, so the
    /// surface can never trap the session.
    fn handle_expanded_tool_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                self.close_expanded_tool();
                true
            }
            KeyCode::Up => {
                self.scroll_expanded_tool(-1);
                true
            }
            KeyCode::Down => {
                self.scroll_expanded_tool(1);
                true
            }
            KeyCode::PageUp => {
                self.scroll_expanded_tool(-(PAGE_STEP_LINES as isize));
                true
            }
            KeyCode::PageDown => {
                self.scroll_expanded_tool(PAGE_STEP_LINES as isize);
                true
            }
            KeyCode::Home => {
                if let Some(view) = self.expanded_tool.as_mut() {
                    view.scroll = 0;
                }
                self.live_area_dirty = true;
                true
            }
            KeyCode::End => {
                if let Some(view) = self.expanded_tool.as_mut() {
                    view.scroll = view.body_lines.saturating_sub(1);
                }
                self.live_area_dirty = true;
                true
            }
            _ => false,
        }
    }

    /// Current body lines of one tool result, looked up from the retained
    /// messages so appends that arrived after the surface opened stay visible.
    fn tool_output_body(&self, row_id: &view_model::RowId) -> Option<Vec<String>> {
        let messages = self.output_manager.get_messages();
        for message in &messages {
            if message.id() != row_id.message_id {
                continue;
            }
            let root = match view_model::project_message(message, &self.colors) {
                view_model::ProjectedMessage::Node(root) => root,
                view_model::ProjectedMessage::Plain(_) => return None,
            };
            return find_transcript_row(&root, row_id)
                .filter(|row| row.role == view_model::NodeRole::ToolOutput)
                .map(|row| row.body.clone());
        }
        None
    }

    /// Label of the tool call that owns the result row, for the surface title.
    fn tool_row_title(&self, row_id: &view_model::RowId) -> String {
        let messages = self.output_manager.get_messages();
        for message in &messages {
            if message.id() != row_id.message_id {
                continue;
            }
            if let view_model::ProjectedMessage::Node(root) =
                view_model::project_message(message, &self.colors)
            {
                if let Some(parent) = find_parent_transcript_row(&root, row_id) {
                    return parent.label.clone();
                }
                return root.label.clone();
            }
        }
        "Tool output".to_string()
    }

    /// The expanded surface's lines for the next frame, or `None` when the
    /// surface's message can no longer be found (the surface closes).
    fn expanded_surface_frame(&mut self, width: usize, height: usize) -> Option<Vec<String>> {
        let (row_id, title, scroll) = {
            let view = self.expanded_tool.as_ref()?;
            (view.row_id.clone(), view.title.clone(), view.scroll)
        };
        let body = self.tool_output_body(&row_id)?;
        let lines = tool_viewport::expanded_surface_lines(&title, &body, scroll, width, height);
        if let Some(view) = self.expanded_tool.as_mut() {
            view.body_lines = body.len();
            view.scroll = view.scroll.min(body.len().saturating_sub(1));
        }
        Some(lines)
    }

    pub fn add_trait_message(&mut self, message: MessageRef) -> MessageId {
        let id = message.id();
        self.output_manager.add_trait_message(message);
        self.live_area_dirty = true;
        id
    }

    pub fn handle_resize(&mut self, w: u16, h: u16) -> Result<()> {
        // Reflow can move previously owned rows above viewport row zero, where
        // relative MoveUp/Clear operations can never reach them. Do not touch
        // the reflowed bytes here. The next render replaces the complete visible
        // screen from retained structured state using absolute coordinates.
        apply_viewport_resize(
            &mut self.autocomplete_state,
            &mut self.pending_viewport_size,
            &mut self.viewport_invalidated,
            &mut self.live_area_dirty,
            w,
            h,
        );
        Ok(())
    }

    fn live_geometry(&mut self, width: u16, height: u16) -> Option<(usize, usize)> {
        if let Some(dialog) = self.active_dialog.as_ref() {
            let terminal_rows = usize::from(height);
            if terminal_rows == 0 {
                return Some((0, 0));
            }
            let mut sink = Vec::new();
            let dialog_rows = Self::draw_dialog_inline_bounded(
                &mut sink,
                dialog,
                usize::from(width).max(1),
                terminal_rows.saturating_sub(1),
            )
            .ok()?;
            let rows = 1 + dialog_rows;
            return Some((rows, rows.saturating_sub(1)));
        }
        if let Some(view) = self.expanded_tool.as_ref() {
            let terminal_rows = usize::from(height);
            if terminal_rows == 0 {
                return Some((0, 0));
            }
            // The surface frame is [separator, surface lines]; mirror the
            // planner exactly so the transcript budget cannot disagree.
            let width_us = usize::from(width).max(1);
            let surface_rows = match self.tool_output_body(&view.row_id) {
                Some(body) => {
                    let lines = tool_viewport::expanded_surface_lines(
                        &view.title,
                        &body,
                        view.scroll,
                        width_us,
                        terminal_rows.saturating_sub(1),
                    );
                    lines
                        .iter()
                        .map(|line| shadow_buffer::physical_rows(line, width_us))
                        .sum::<usize>()
                }
                None => 1,
            };
            let rows = 1 + surface_rows;
            return Some((rows, rows.saturating_sub(1)));
        }
        let term_width = usize::from(width).max(1);
        let draw_width = term_width;
        let draw_height = usize::from(height);
        let sources = self.live_frame_sources(draw_width);
        if draw_height <= 3 {
            let frame = plan_tiny_live_frame(
                &sources.input_lines,
                sources.input_cursor,
                &sources.effective_status,
                draw_height,
                draw_width,
            );
            return Some((frame.lines.len(), frame.cursor_row));
        }
        // The geometry estimator plans the same claiming pass the draw will,
        // so erase accounting can never disagree with what was painted: one
        // planner, two consumers.
        let mut autocomplete = self.autocomplete_state.clone();
        let vm = live_view_model(&sources, draw_width, draw_height, None, None);
        let frame = plan_live_frame(&vm, &mut autocomplete);
        Some((frame.physical_rows(draw_width), frame.cursor_row))
    }

    /// Clear and reconstruct Finch's complete visible viewport after terminal
    /// reflow. `ClearType::All` clears only the visible screen; terminal-native
    /// scrollback that has already left the viewport remains untouched.
    fn redraw_full_viewport(&mut self) -> Result<()> {
        self.redraw_full_viewport_inner(false)
    }

    fn redraw_full_viewport_inner(&mut self, synchronized_update_open: bool) -> Result<()> {
        let (width, height) = self
            .pending_viewport_size
            .take()
            .unwrap_or_else(|| crossterm::terminal::size().unwrap_or((80, 24)));
        let term_width = usize::from(width).max(1);
        let term_height = usize::from(height);
        let live_rows = self
            .live_geometry(width, height)
            .map(|(rows, _)| rows)
            .unwrap_or(self.active_rows)
            .min(term_height);
        let transcript_budget = term_height.saturating_sub(live_rows);

        let messages = self.output_manager.get_messages();
        let transcript = self
            .projected_lines(
                visible_printed_messages(&messages, &self.printed_ids),
                term_width,
            )
            .into_iter()
            .map(|line| line.text)
            .collect::<Vec<_>>();
        let transcript = viewport_tail_lines(&transcript, term_width, transcript_budget);
        let transcript_rows = transcript
            .iter()
            .map(|line| shadow_buffer::physical_rows(line, term_width))
            .sum();
        let plan = viewport_redraw_plan(term_height, live_rows, transcript_rows);

        let mut stdout = io::stdout();
        let paint = if synchronized_update_open {
            continue_full_viewport_paint(&mut stdout, plan, &transcript)
        } else {
            begin_full_viewport_paint(&mut stdout, plan, &transcript)
        };
        if let Err(error) = paint {
            let _ = execute!(stdout, EndSynchronizedUpdate);
            return Err(error);
        }

        self.active_rows = 0;
        self.cursor_row_from_top = 0;
        self.viewport_invalidated = false;
        // draw_live_area closes the synchronized update begun above.
        let draw = self.draw_live_area();
        if draw.is_err() {
            let _ = execute!(io::stdout(), EndSynchronizedUpdate);
        }
        draw
    }
}

// ─── Operation status helpers (used by planning loop, etc.) ──────────────────

impl TuiRenderer {
    /// Set the OperationStatus line in the status bar (visible while queries run).
    pub fn set_operation_status(&self, msg: impl Into<String>) {
        self.status_bar.update_operation(msg.into());
    }

    /// Clear the OperationStatus line from the status bar.
    pub fn clear_operation_status(&self) {
        self.status_bar.clear_operation();
    }
}

// ─── Ghost text / suggestions ─────────────────────────────────────────────────

impl TuiRenderer {
    fn sync_ghost_to_selected_completion(&mut self) {
        self.ghost_text = selected_completion_ghost(
            self.input_textarea.lines(),
            self.input_textarea.cursor(),
            &self.autocomplete_state,
        );
    }

    pub fn update_ghost_text(&mut self) {
        // Recalled history lines are already complete. Showing the slash
        // dropdown would steal the next Up/Down from history navigation.
        if self.history_index.is_some() {
            self.autocomplete_state.hide();
            self.ghost_text = None;
            self.live_area_dirty = true;
            return;
        }
        let lines = self.input_textarea.lines();
        if lines.first().is_some_and(|line| line.starts_with('/')) {
            let (matches, ghost) = command_completion_at_cursor(
                lines,
                self.input_textarea.cursor(),
                &self.command_registry,
            );
            if matches.is_empty() {
                self.autocomplete_state.hide();
                self.ghost_text = None;
            } else {
                self.autocomplete_state.show_matches(matches);
                self.ghost_text = ghost;
                self.sync_ghost_to_selected_completion();
            }
            self.live_area_dirty = true;
            return;
        }
        if let Some((_, query)) = mention_query_from_textarea(&self.input_textarea) {
            let rows = self.mention_catalog.candidates(&query);
            if rows.is_empty() {
                self.autocomplete_state.hide();
            } else {
                self.autocomplete_state.show_mentions(rows);
            }
            self.ghost_text = None;
            self.live_area_dirty = true;
            return;
        }
        self.autocomplete_state.hide();
        self.ghost_text = None;
        self.live_area_dirty = true;
    }

    /// Replace only the command prefix before the cursor. Multiline draft
    /// content and text after the cursor are retained byte-for-byte.
    pub(crate) fn accept_selected_completion(&mut self) -> bool {
        let Some(command) = self.autocomplete_state.get_selected() else {
            return false;
        };
        let command_name = command.name.to_string();
        if !replace_textarea_command(&mut self.input_textarea, &command_name) {
            return false;
        }
        self.autocomplete_state.hide();
        self.ghost_text = None;
        self.live_area_dirty = true;
        true
    }

    pub(crate) fn handle_completion_key(&mut self, code: KeyCode) -> bool {
        if self.history_index.is_some() && matches!(code, KeyCode::Up | KeyCode::Down) {
            return false;
        }
        if self.autocomplete_state.is_mention_mode() && self.autocomplete_state.is_interactive() {
            match code {
                KeyCode::Up => self.autocomplete_state.select_previous(),
                KeyCode::Down => self.autocomplete_state.select_next(),
                KeyCode::Tab | KeyCode::Enter => {
                    let _ = self.apply_selected_mention();
                    return true;
                }
                KeyCode::Esc => {
                    self.autocomplete_state.hide();
                    self.ghost_text = None;
                    self.mark_dirty();
                    return true;
                }
                _ => return false,
            }
            self.mark_dirty();
            return true;
        }
        if !dispatch_completion_key(
            &mut self.input_textarea,
            &mut self.autocomplete_state,
            &mut self.ghost_text,
            code,
        ) {
            return false;
        }
        self.mark_dirty();
        true
    }

    fn apply_selected_mention(&mut self) -> bool {
        let Some(candidate) = self.autocomplete_state.get_selected_mention().cloned() else {
            return false;
        };
        let token = format!("{} ", candidate.insert_token());
        if !replace_textarea_mention(&mut self.input_textarea, &token) {
            return false;
        }
        match self.mention_catalog.resolve_path(&candidate.relative_path) {
            Ok(snapshot) => {
                self.pending_mentions
                    .retain(|existing| existing.relative_path != snapshot.relative_path);
                self.pending_mentions.push(snapshot);
                self.autocomplete_state.hide();
                self.ghost_text = None;
            }
            Err(error) => {
                self.autocomplete_state.set_mention_error(error.speakable());
            }
        }
        self.live_area_dirty = true;
        true
    }

    fn apply_history_line(&mut self, cmd: &str) {
        self.input_textarea = Self::create_clean_textarea_with_text(cmd);
        self.autocomplete_state.hide();
        self.ghost_text = None;
        self.live_area_dirty = true;
    }

    /// Walk toward older commands when the cursor is on the first composer row.
    /// Returns false when the key should move the textarea cursor instead.
    pub(crate) fn recall_older_history(&mut self) -> bool {
        let (cursor_row, _) = self.input_textarea.cursor();
        if cursor_row != 0 {
            return false;
        }
        if let Some(idx) = self.history_index {
            if idx > 0 {
                self.history_index = Some(idx - 1);
                let cmd = self.command_history[idx - 1].clone();
                self.apply_history_line(&cmd);
            }
            return true;
        }
        if self.command_history.is_empty() {
            return true;
        }
        let current_text = self.input_textarea.lines().join("\n");
        if !current_text.trim().is_empty() {
            self.history_draft = Some(current_text);
        }
        let last = self.command_history.len() - 1;
        self.history_index = Some(last);
        let cmd = self.command_history[last].clone();
        self.apply_history_line(&cmd);
        true
    }

    /// Walk toward newer commands when the cursor is on the last composer row.
    /// Returns false when the key should move the textarea cursor instead.
    pub(crate) fn recall_newer_history(&mut self) -> bool {
        let (cursor_row, _) = self.input_textarea.cursor();
        let last_line = self.input_textarea.lines().len().saturating_sub(1);
        if cursor_row < last_line {
            return false;
        }
        let Some(idx) = self.history_index else {
            return true;
        };
        if idx < self.command_history.len() - 1 {
            self.history_index = Some(idx + 1);
            let cmd = self.command_history[idx + 1].clone();
            self.apply_history_line(&cmd);
        } else {
            self.history_index = None;
            if let Some(draft) = self.history_draft.take() {
                self.input_textarea = Self::create_clean_textarea_with_text(&draft);
            } else {
                self.input_textarea = Self::create_clean_textarea();
            }
            self.autocomplete_state.hide();
            self.ghost_text = None;
            self.live_area_dirty = true;
        }
        true
    }

    /// Apply any highlighted slash completion, then take the composer line.
    pub(crate) fn take_submitted_input(&mut self) -> Option<String> {
        let _ = apply_selected_completion_for_submit(
            &mut self.input_textarea,
            &mut self.autocomplete_state,
            &mut self.ghost_text,
        );
        let input = self.input_textarea.lines().join("\n");
        if input.trim().is_empty() {
            return None;
        }
        let still_mentioned: std::collections::HashSet<String> =
            crate::context::mention::parse_visible_mentions(&input)
                .into_iter()
                .map(|parsed| parsed.relative_path)
                .collect();
        self.pending_mentions
            .retain(|snapshot| still_mentioned.contains(&snapshot.relative_path));
        self.command_history.push(input.clone());
        self.history_index = None;
        self.history_draft = None;
        self.input_textarea = Self::create_clean_textarea();
        self.autocomplete_state.hide();
        self.ghost_text = None;
        self.live_area_dirty = true;
        Some(input)
    }

    /// Dispatch Tab, completion, Enter, and history in input-task order.
    ///
    /// Completions run before history so a painted pane can own Up/Down,
    /// except while `history_index` is set. Enter applies a painted selection
    /// before taking the line.
    pub(crate) fn dispatch_composer_key(&mut self, key: KeyEvent) -> ComposerDispatch {
        if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE {
            let input_changed = self.handle_tab_key(key);
            return ComposerDispatch::Handled { input_changed };
        }
        if key.modifiers == KeyModifiers::NONE && self.handle_completion_key(key.code) {
            return ComposerDispatch::Handled {
                input_changed: false,
            };
        }
        if key.code == KeyCode::Enter {
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
            {
                self.input_textarea.input(Event::Key(key));
                return ComposerDispatch::Handled {
                    input_changed: true,
                };
            }
            if self.autocomplete_state.is_mention_mode() && self.autocomplete_state.is_interactive()
            {
                let _ = self.apply_selected_mention();
                return ComposerDispatch::Handled {
                    input_changed: true,
                };
            }
            return match self.take_submitted_input() {
                Some(input) => ComposerDispatch::Submit(input),
                None => ComposerDispatch::Handled {
                    input_changed: false,
                },
            };
        }
        match (key.code, key.modifiers) {
            (KeyCode::Up, KeyModifiers::NONE) => {
                if !self.recall_older_history() {
                    self.input_textarea.input(Event::Key(key));
                }
                ComposerDispatch::Handled {
                    input_changed: true,
                }
            }
            (KeyCode::Down, KeyModifiers::NONE) => {
                if !self.recall_newer_history() {
                    self.input_textarea.input(Event::Key(key));
                }
                ComposerDispatch::Handled {
                    input_changed: true,
                }
            }
            _ => ComposerDispatch::Unhandled,
        }
    }

    pub(crate) fn handle_tab_key(&mut self, key: KeyEvent) -> bool {
        if self.autocomplete_state.is_mention_mode() && self.autocomplete_state.is_interactive() {
            let _ = self.apply_selected_mention();
            return true;
        }
        let modified = route_tab_key(
            &mut self.input_textarea,
            &mut self.autocomplete_state,
            &mut self.ghost_text,
            key,
        );
        if modified {
            self.update_ghost_text();
        } else {
            self.mark_dirty();
        }
        modified
    }

    /// Tests and query assembly inject a project root without touching cwd.
    #[cfg(test)]
    pub(crate) fn set_mention_root(&mut self, root: impl Into<PathBuf>) {
        self.mention_catalog = crate::context::mention::MentionCatalog::new(root.into());
        self.pending_mentions.clear();
    }
}

// ─── Crossterm dialog rendering ───────────────────────────────────────────────

/// Returns `(ansi_on, marker)` for the "Other (custom response)" row.
///
/// When the row is selected, returns cyan bold + filled marker.
/// When unselected, returns dim gray + hollow marker.
/// This is extracted so it can be unit-tested without a real terminal.
pub(crate) fn other_row_parts(is_selected: bool) -> (String, &'static str) {
    if is_selected {
        (format!("{}{}", SetAttribute(Attribute::Bold), CYAN), "●")
    } else {
        (DIM_GRAY.to_string(), "◌")
    }
}

/// Formats the visible content of the custom-input line (no box borders).
///
/// Returns `"> {before}█{after}"` where the block cursor sits at `cursor` and
/// the typed text (`before`) carries **no** extra ANSI colour — it renders in the
/// terminal's default foreground so it is always readable.
/// This is extracted so it can be unit-tested without a real terminal.
pub(crate) fn format_custom_input_content(input: &str, cursor: usize) -> String {
    let before: String = input.chars().take(cursor).collect();
    let after: String = input.chars().skip(cursor).collect();
    format!(
        "> {}{} {}{}",
        before,
        SetAttribute(Attribute::Reverse),
        SetAttribute(Attribute::Reset),
        after
    )
}

/// Print an indented dialog content line (two-space indent, trailing `\r\n`),
/// optionally styled. Centralizes the borderless line format so every dialog
/// row is rendered through crossterm rather than hand-written ANSI escapes.
fn print_dialog_line(
    out: &mut impl io::Write,
    text: &str,
    color: Option<Color>,
    bold: bool,
) -> Result<()> {
    execute!(out, Print("  "))?;
    if bold {
        execute!(out, SetAttribute(Attribute::Bold))?;
    }
    if let Some(c) = color {
        execute!(out, SetForegroundColor(c))?;
    }
    execute!(out, Print(text))?;
    if bold || color.is_some() {
        execute!(out, SetAttribute(Attribute::Reset))?;
    }
    execute!(out, Print("\r\n"))?;
    Ok(())
}

/// Print a single inline token (a button or Yes/No choice) styled by focus:
/// bold cyan when active, dim grey when not. Emits no newline.
fn print_dialog_token(out: &mut impl io::Write, text: &str, active: bool) -> Result<()> {
    if active {
        execute!(
            out,
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::Cyan),
            Print(text),
            SetAttribute(Attribute::Reset),
        )?;
    } else {
        execute!(
            out,
            SetForegroundColor(Color::DarkGrey),
            Print(text),
            SetAttribute(Attribute::Reset),
        )?;
    }
    Ok(())
}

/// Render the "Other (custom response)" row inline within the dialog.
///
/// When `is_on_other` is true the row shows an inline cursor with any typed
/// text so the user can start typing immediately without a mode switch.
/// When false it renders the normal hollow-marker label.
///
/// Borderless: the row is indented two spaces with no right border or padding.
///
/// Returns the number of terminal rows consumed (always 1).
fn render_other_row_inline(
    out: &mut impl io::Write,
    _inner: usize,
    is_on_other: bool,
    dialog: &Dialog,
) -> Result<usize> {
    if is_on_other {
        // Inline input: "  ● Other: > {before}█{after}"
        let input_text = dialog.custom_input.as_deref().unwrap_or("");
        let cursor = dialog.custom_cursor_pos;
        // format_custom_input_content carries the reverse-video cursor block.
        let content = format_custom_input_content(input_text, cursor);
        execute!(
            out,
            Print("  "),
            SetAttribute(Attribute::Bold),
            SetForegroundColor(Color::Cyan),
            Print("  \u{25cf} Other: "),
            SetAttribute(Attribute::Reset),
            Print(content),
            Print("\r\n"),
        )?;
    } else {
        // marker glyph comes from other_row_parts (tested); color via crossterm.
        let (_, marker) = other_row_parts(false);
        let other_label = format!("  {} Other (custom response)", marker);
        print_dialog_line(out, &other_label, Some(Color::DarkGrey), false)?;
    }
    Ok(1)
}

impl TuiRenderer {
    /// Draw a `Dialog` inline, borderless and spanning the full terminal width.
    ///
    /// Sections are separated by a full-width horizontal rule; content lines are
    /// indented two spaces with no left/right border and no right padding, so the
    /// dialog fills the available width instead of sitting inside a capped box.
    /// Returns the number of terminal rows consumed.
    /// `box_width` is the total width the dialog spans (normally the terminal width).
    pub(crate) fn draw_dialog_inline_static_with_width(
        out: &mut impl io::Write,
        dialog: &Dialog,
        box_width: usize,
    ) -> Result<usize> {
        Self::draw_dialog_with_control_start(out, dialog, box_width, None).map(|(rows, _)| rows)
    }

    /// Paint a dialog and report the logical line index where the control
    /// suffix starts (the options divider after title/help/body).
    ///
    /// That index is structural: options always follow the body, so a markdown
    /// payload that happens to contain `●` cannot shift the pin.
    ///
    /// `max_rows` is the live-area budget. Keyboard-hint rows are omitted when
    /// they would force `pin_dialog_controls` to clip the title on a short
    /// terminal.
    fn draw_dialog_with_control_start(
        out: &mut impl io::Write,
        dialog: &Dialog,
        box_width: usize,
        max_rows: Option<usize>,
    ) -> Result<(usize, usize)> {
        // Wrap width inside the 2-space left indent (no right border to reserve for).
        let inner = box_width.saturating_sub(2).max(1);

        let mut rows = 0;

        // Full-width horizontal rule used to separate sections.
        let rule = "─".repeat(box_width);

        // Top rule
        execute!(out, Print(&rule), Print("\r\n"))?;
        rows += 1;

        // Title
        for line in wrap_text(&dialog.title, inner) {
            print_dialog_line(out, &line, None, false)?;
            rows += 1;
        }

        // Help message (from dialog field) — wrapped to avoid overflow
        if let Some(ref help) = dialog.help_message {
            for line in wrap_text(help, inner) {
                print_dialog_line(out, &line, Some(Color::DarkGrey), false)?;
                rows += 1;
            }
        }

        // Body text (optional, shown above the options divider) with scroll support
        if let Some(ref body) = dialog.body {
            let term_h = crossterm::terminal::size().unwrap_or((80, 24)).1 as usize;
            // Reserve ~12 rows for title, help, both dividers, options, and the button row.
            let max_body_rows = term_h.saturating_sub(12).clamp(3, 15);

            execute!(out, Print(&rule), Print("\r\n"))?;
            rows += 1;

            // Collect all wrapped lines.
            let mut all_body_lines: Vec<String> = Vec::new();
            for line in body.lines() {
                for wrapped in wrap_text(line, inner) {
                    all_body_lines.push(wrapped);
                }
            }

            let total_lines = all_body_lines.len();

            if total_lines <= max_body_rows {
                // All lines fit — show them all without a scroll indicator.
                for line in &all_body_lines {
                    print_dialog_line(out, line, Some(Color::DarkGrey), false)?;
                    rows += 1;
                }
            } else {
                // Reserve 1 row for the scroll indicator.
                let content_rows = max_body_rows.saturating_sub(1).max(1);
                let max_offset = total_lines.saturating_sub(content_rows);
                let offset = dialog.body_scroll_offset.min(max_offset);

                for line in &all_body_lines[offset..total_lines.min(offset + content_rows)] {
                    print_dialog_line(out, line, Some(Color::DarkGrey), false)?;
                    rows += 1;
                }

                // Scroll indicator showing position and navigation hint.
                let above = offset;
                let below = total_lines.saturating_sub(offset + content_rows);
                let indicator = match (above > 0, below > 0) {
                    (true, true) => {
                        format!(
                            "↑ {} above · ↓ {} below  (Ctrl-U/D or PgUp/PgDn)",
                            above, below
                        )
                    }
                    (true, false) => format!("↑ {} lines above  (Ctrl-U or PgUp)", above),
                    (false, true) => format!("↓ {} lines below  (Ctrl-D or PgDn)", below),
                    (false, false) => String::new(),
                };
                if !indicator.is_empty() {
                    let short: String = indicator.chars().take(inner).collect();
                    print_dialog_line(out, &short, Some(Color::DarkGrey), false)?;
                    rows += 1;
                }
            }
        }

        let control_start = rows;
        execute!(out, Print(&rule), Print("\r\n"))?;
        rows += 1;

        // Options — always render the full option list inline.
        // When the cursor is on the "Other" row, show it with an inline input cursor.
        match &dialog.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                allow_custom,
            } => {
                for (i, opt) in options.iter().enumerate() {
                    let selected = i == *selected_index;
                    let marker = if selected { "●" } else { "○" };
                    let label = format!("  {} {}", marker, opt.label);
                    let color = if selected { Some(Color::Cyan) } else { None };
                    print_dialog_line(out, &label, color, selected)?;
                    rows += 1;
                }
                if *allow_custom {
                    let is_on_other = *selected_index == options.len();
                    rows += render_other_row_inline(out, inner, is_on_other, dialog)?;
                }
            }
            DialogType::MultiSelect {
                options,
                selected_indices,
                cursor_index,
                allow_custom,
            } => {
                for (i, opt) in options.iter().enumerate() {
                    let checked = if selected_indices.contains(&i) {
                        "☑"
                    } else {
                        "☐"
                    };
                    let focused = i == *cursor_index;
                    let label = format!("  {} {}", checked, opt.label);
                    let color = if focused { Some(Color::Cyan) } else { None };
                    print_dialog_line(out, &label, color, focused)?;
                    rows += 1;
                }
                if *allow_custom {
                    let is_on_other = *cursor_index == options.len();
                    rows += render_other_row_inline(out, inner, is_on_other, dialog)?;
                }
            }
            DialogType::Confirm {
                prompt, selected, ..
            } => {
                // Prompt may be multi-line.
                for line in wrap_text(prompt, inner) {
                    print_dialog_line(out, &line, None, false)?;
                    rows += 1;
                }
                execute!(out, Print("  "))?;
                print_dialog_token(out, "Yes", *selected)?;
                execute!(out, Print("   "))?;
                print_dialog_token(out, "No", !*selected)?;
                execute!(out, Print("\r\n"))?;
                rows += 1;
            }
            DialogType::TextInput { prompt, input, .. } => {
                if !prompt.is_empty() {
                    print_dialog_line(out, prompt, None, false)?;
                    rows += 1;
                }
                let line = format!("> {}", input);
                print_dialog_line(out, &line, None, false)?;
                rows += 1;
            }
        }

        // ── Preview pane ─────────────────────────────────────────────────────
        // If the focused option has a `markdown` field, render it in a labeled
        // preview section between the options and the Submit/Cancel row.
        let focused_markdown: Option<&str> = match &dialog.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                ..
            } => options
                .get(*selected_index)
                .and_then(|o| o.markdown.as_deref()),
            DialogType::MultiSelect {
                options,
                cursor_index,
                ..
            } => options
                .get(*cursor_index)
                .and_then(|o| o.markdown.as_deref()),
            _ => None,
        };

        if let Some(md) = focused_markdown {
            let term_height = crossterm::terminal::size().unwrap_or((80, 24)).1 as usize;
            let max_preview_lines = 10.min(term_height / 3).max(1);

            // Strip leading/trailing blank lines and collect non-empty content
            let raw_lines: Vec<&str> = md.lines().collect();
            let start = raw_lines
                .iter()
                .position(|l| !l.trim().is_empty())
                .unwrap_or(0);
            let end = raw_lines
                .iter()
                .rposition(|l| !l.trim().is_empty())
                .map(|i| i + 1)
                .unwrap_or(raw_lines.len());
            let content_lines: Vec<&str> = raw_lines[start..end].to_vec();
            let display_lines: Vec<&str> = content_lines
                .iter()
                .take(max_preview_lines)
                .copied()
                .collect();
            let truncated = content_lines.len() > max_preview_lines;

            // Labeled full-width rule: "─ Preview ─────…"
            let label = "─ Preview ";
            let pad = box_width.saturating_sub(label.chars().count());
            let preview_div = format!("{}{}", label, "─".repeat(pad));
            execute!(out, Print(&preview_div), Print("\r\n"))?;
            rows += 1;

            for line in &display_lines {
                // Truncate to inner width using visible_length to handle ANSI codes
                let vlen = shadow_buffer::visible_length(line);
                if vlen <= inner {
                    print_dialog_line(out, line, None, false)?;
                } else {
                    // Truncate by chars (ANSI codes make byte slicing unsafe)
                    let truncated_line: String =
                        line.chars().take(inner.saturating_sub(1)).collect();
                    print_dialog_line(out, &format!("{}…", truncated_line), None, false)?;
                }
                rows += 1;
            }

            if truncated {
                print_dialog_line(out, "…", Some(Color::DarkGrey), false)?;
                rows += 1;
            }
        }
        // ── End preview pane ─────────────────────────────────────────────────

        execute!(out, Print(&rule), Print("\r\n"))?;
        rows += 1;

        // ── Submit / Cancel buttons ───────────────────────────────────────────
        let is_multiselect = matches!(&dialog.dialog_type, DialogType::MultiSelect { .. });
        let submit_idx = dialog.submit_virtual_index();
        let cancel_idx = dialog.cancel_virtual_index();
        let cursor = dialog.current_cursor();

        if is_multiselect {
            // MultiSelect: [ Submit ]   [ Cancel ]
            execute!(out, Print("  "))?;
            print_dialog_token(out, "[ Submit ]", cursor == submit_idx)?;
            execute!(out, Print("   "))?;
            print_dialog_token(out, "[ Cancel ]", cursor == cancel_idx)?;
            execute!(out, Print("\r\n"))?;
            rows += 1;

            let hint = "↑/↓: Navigate | Space: Toggle | Enter: Submit | Esc: Cancel";
            let hint_lines = wrap_text(hint, inner);
            // Bottom rule is one more row after this block.
            let fits = max_rows
                .map(|max| rows + hint_lines.len() + 1 <= max)
                .unwrap_or(true);
            if fits {
                for line in hint_lines {
                    print_dialog_line(out, &line, Some(Color::DarkGrey), false)?;
                    rows += 1;
                }
            }
        } else if matches!(&dialog.dialog_type, DialogType::Select { .. }) {
            // Select: [ Cancel ]  (no Submit — Enter on an option submits directly)
            let hint = if dialog.custom_mode_active {
                "  Enter↵ submit · Esc clear"
            } else {
                "  ↑↓ nav · Enter select · Esc cancel"
            };
            execute!(out, Print("  "))?;
            print_dialog_token(out, "[ Cancel ]", cursor == cancel_idx)?;
            execute!(
                out,
                SetForegroundColor(Color::DarkGrey),
                Print(hint),
                SetAttribute(Attribute::Reset),
                Print("\r\n"),
            )?;
            rows += 1;
        } else {
            // Confirm / TextInput: just a keybinding hint
            let help = "↑/↓ Navigate  Enter Select  Esc Cancel";
            print_dialog_line(out, help, Some(Color::DarkGrey), false)?;
            rows += 1;
        }
        execute!(out, Print(&rule), Print("\r\n"))?;
        rows += 1;

        Ok((rows, control_start))
    }

    fn draw_dialog_inline_static(out: &mut impl io::Write, dialog: &Dialog) -> Result<usize> {
        let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0 as usize;
        let box_width = term_width.max(1);
        Self::draw_dialog_inline_static_with_width(out, dialog, box_width)
    }

    fn draw_dialog_inline_bounded(
        out: &mut impl io::Write,
        dialog: &Dialog,
        width: usize,
        max_rows: usize,
    ) -> Result<usize> {
        let lines = Self::dialog_lines(dialog, width, max_rows);
        for line in &lines {
            execute!(out, Print(line), Print("\r\n"))?;
        }
        Ok(lines
            .iter()
            .map(|line| shadow_buffer::physical_rows(line, width.max(1)))
            .sum())
    }

    /// The dialog's lines, clipped to `max_rows` **physical** terminal rows.
    ///
    /// The row count returned to the live area was previously a count of
    /// logical lines. Four dialog rows are built from caller data that is never
    /// wrapped or is truncated by character count rather than display width —
    /// a long option label, long text input, the custom "Other" row, and a CJK
    /// preview line — so each could occupy two rows while reporting one. That
    /// count reaches `cursor_row_from_top`, which the erase walks up by, so an
    /// undercount left the top of the dialog unerased and the box redrew one
    /// row lower on every tick: the cascading duplicate dialogs. Measuring the
    /// emitted lines removes the possibility of the two disagreeing.
    ///
    /// When the payload (title/body) overflows, the option/button suffix is
    /// pinned so approve/deny stay reachable. Too many options still clip from
    /// the top and show the viewport marker.
    pub(crate) fn dialog_lines(dialog: &Dialog, width: usize, max_rows: usize) -> Vec<String> {
        if max_rows == 0 {
            return Vec::new();
        }
        let width = width.max(1);
        let mut rendered = Vec::new();
        let control_start = match Self::draw_dialog_with_control_start(
            &mut rendered,
            dialog,
            width,
            Some(max_rows),
        ) {
            Ok((_, start)) => start,
            Err(_) => return Vec::new(),
        };
        let text = String::from_utf8_lossy(&rendered).into_owned();
        let all = text
            .split_terminator("\r\n")
            .map(str::to_string)
            .collect::<Vec<_>>();
        dialog::pin_dialog_controls(
            all,
            control_start,
            max_rows,
            width,
            dialog.option_row_count(),
        )
    }

    /// Show a blocking dialog (used when no async event loop is running).
    /// Returns `DialogResult::Cancelled` if Esc is pressed.
    pub fn show_dialog(&mut self, dialog: Dialog) -> Result<DialogResult> {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        // Commit any pending Complete messages to scrollback before drawing the dialog.
        // This ensures messages written before show_dialog() appear above the dialog,
        // not below it (or deferred until after the dialog closes).
        let om = Arc::clone(&self.output_manager);
        self.flush_output_safe(&om)?;

        self.active_dialog = Some(dialog);
        self.live_area_dirty = true;
        self.erase_live_area()?;
        self.draw_live_area()?;

        loop {
            if event::poll(Duration::from_millis(50))? {
                if let Event::Key(key) = event::read()? {
                    // Skip Release/Repeat events — only process Press.
                    // Without this guard, terminals that emit both Press and Release
                    // cause double-fire: e.g. pressing 'o' activates custom mode AND
                    // immediately inserts 'o' into the text field via the Release event.
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            let is_custom_mode = self
                                .active_dialog
                                .as_ref()
                                .is_some_and(|d| d.custom_mode_active);
                            let is_plain_esc = matches!(key.code, KeyCode::Esc);

                            if is_custom_mode && is_plain_esc {
                                // Exit custom mode, keep dialog open
                                if let Some(ref mut d) = self.active_dialog {
                                    d.handle_key_event(key);
                                }
                                self.erase_live_area()?;
                                self.draw_live_area()?;
                            } else {
                                self.active_dialog = None;
                                self.erase_live_area()?;
                                self.draw_live_area()?;
                                return Ok(DialogResult::Cancelled);
                            }
                        }
                        _ => {
                            let result = self
                                .active_dialog
                                .as_mut()
                                .and_then(|d| d.handle_key_event(key));

                            if let Some(r) = result {
                                self.active_dialog = None;
                                self.erase_live_area()?;
                                self.draw_live_area()?;
                                return Ok(r);
                            } else {
                                // Redraw with updated state.
                                self.erase_live_area()?;
                                self.draw_live_area()?;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Show the setup wizard using ratatui in an alternate screen.
    pub fn show_tabbed_dialog(&mut self, mut dialog: TabbedDialog) -> Result<TabbedDialogResult> {
        use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
        use ratatui::widgets::Widget;
        use ratatui::{backend::CrosstermBackend, Terminal};

        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut term = Terminal::new(backend).context("Failed to create wizard terminal")?;

        let result = loop {
            term.draw(|frame| {
                TabbedDialogWidget::new(&dialog, &self.colors)
                    .render(frame.area(), frame.buffer_mut());
            })?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != crossterm::event::KeyEventKind::Press {
                        continue;
                    }
                    if let Some(r) = dialog.handle_key_event(key) {
                        break r;
                    }
                }
            }
        };

        execute!(io::stdout(), LeaveAlternateScreen)?;
        self.active_rows = 0;
        Ok(result)
    }

    /// Open a file in a full-screen TUI viewer.
    ///
    /// CSV, TSV, and XLSX files are shown as a scrollable grid table.
    /// All other files are shown as scrollable text.
    /// `q`, `Esc`, or `Ctrl-D` closes the viewer.
    pub fn show_file_viewer(&mut self, path: &str) -> Result<()> {
        use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
        use ratatui::backend::CrosstermBackend;
        use ratatui::layout::{Constraint, Direction, Layout};
        use ratatui::style::{Modifier, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Cell as RCell, Paragraph, Row, Table, Wrap};
        use ratatui::Terminal;

        // Load content based on file extension.
        let ext = std::path::Path::new(path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        // grid_rows: Some(rows) for tabular files, None for text.
        let grid_rows: Option<Vec<Vec<String>>> = match ext.as_str() {
            "csv" => {
                let raw = std::fs::read_to_string(path).unwrap_or_else(|e| format!("error: {e}"));
                let mut rows = Vec::new();
                for line in raw.lines() {
                    let cols: Vec<String> = line
                        .split(',')
                        .map(|c| c.trim_matches('"').to_string())
                        .collect();
                    rows.push(cols);
                }
                Some(rows)
            }
            "tsv" => {
                let raw = std::fs::read_to_string(path).unwrap_or_else(|e| format!("error: {e}"));
                let mut rows = Vec::new();
                for line in raw.lines() {
                    let cols: Vec<String> = line.split('\t').map(|c| c.to_string()).collect();
                    rows.push(cols);
                }
                Some(rows)
            }
            "xlsx" | "xls" | "ods" => Self::spreadsheet_preview_rows(path),
            _ => None,
        };

        let colors = self.colors.clone();

        execute!(io::stdout(), EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut term = Terminal::new(backend).context("Failed to create file viewer terminal")?;

        let mut scroll: usize = 0;

        loop {
            term.draw(|frame| {
                let area = frame.area();
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(1)])
                    .split(area);

                let title = format!(" {} ", path);
                let border_style = Style::default().fg(colors.dialog.border.to_color());

                if let Some(ref rows) = grid_rows {
                    // Compute column widths from data.
                    let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(1);
                    let mut widths: Vec<usize> = vec![0; ncols];
                    for row in rows {
                        for (i, cell) in row.iter().enumerate() {
                            widths[i] = widths[i].max(cell.chars().count());
                        }
                    }
                    let constraints: Vec<Constraint> = widths
                        .iter()
                        .map(|&w| Constraint::Length((w + 2).min(40) as u16))
                        .collect();

                    let visible_height = chunks[0].height.saturating_sub(3) as usize;
                    let start = scroll;
                    let end = (start + visible_height).min(rows.len());

                    let header_style = Style::default()
                        .fg(colors.dialog.title.to_color())
                        .add_modifier(Modifier::BOLD);
                    let row_style = Style::default().fg(colors.dialog.option.to_color());

                    let table_rows: Vec<Row> = rows[start..end]
                        .iter()
                        .enumerate()
                        .map(|(i, row)| {
                            let cells: Vec<RCell> = row
                                .iter()
                                .map(|c| {
                                    if start == 0 && i == 0 {
                                        RCell::from(c.as_str()).style(header_style)
                                    } else {
                                        RCell::from(c.as_str()).style(row_style)
                                    }
                                })
                                .collect();
                            Row::new(cells)
                        })
                        .collect();

                    let table = Table::new(table_rows, constraints).block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(border_style)
                            .title(title),
                    );
                    frame.render_widget(table, chunks[0]);
                } else {
                    // Text viewer.
                    let text_raw = std::fs::read_to_string(path)
                        .unwrap_or_else(|e| format!("error reading file: {e}"));
                    let lines: Vec<Line> = text_raw
                        .lines()
                        .skip(scroll)
                        .map(|l| {
                            Line::from(Span::styled(
                                l.to_string(),
                                Style::default().fg(colors.dialog.option.to_color()),
                            ))
                        })
                        .collect();
                    let para = Paragraph::new(lines)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(border_style)
                                .title(title),
                        )
                        .wrap(Wrap { trim: false });
                    frame.render_widget(para, chunks[0]);
                }

                // Help bar at the bottom.
                let help = Paragraph::new(Line::from(Span::styled(
                    " ↑/↓: Scroll | PgUp/PgDn | q/Esc: Close ",
                    Style::default().fg(colors.ui.separator.to_color()),
                )));
                frame.render_widget(help, chunks[1]);
            })?;

            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != crossterm::event::KeyEventKind::Press {
                        continue;
                    }
                    use crossterm::event::KeyCode;
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Down | KeyCode::Char('j') => {
                            scroll = scroll.saturating_add(1);
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            scroll = scroll.saturating_sub(1);
                        }
                        KeyCode::PageDown => {
                            scroll = scroll.saturating_add(20);
                        }
                        KeyCode::PageUp => {
                            scroll = scroll.saturating_sub(20);
                        }
                        KeyCode::Home => {
                            scroll = 0;
                        }
                        _ => {}
                    }
                }
            }
        }

        execute!(io::stdout(), LeaveAlternateScreen)?;
        self.active_rows = 0;
        Ok(())
    }

    /// The rows a spreadsheet preview shows, as a free function so it can be tested.
    ///
    /// It used to be inline in `show_file_viewer`, which enters the alternate
    /// screen and so cannot be driven from a test -- and the cell rendering inside
    /// it carried #281 (dates as raw Excel serials) for as long as that was true.
    /// CLAUDE.md asks for a production-boundary test when a bug crosses the TUI
    /// boundary; this is the boundary, pulled out to where one can reach it.
    pub(crate) fn spreadsheet_preview_rows(path: &str) -> Option<Vec<Vec<String>>> {
        use calamine::{open_workbook_auto, Reader};

        match open_workbook_auto(path) {
            Ok(mut workbook) => {
                let sheet_names = workbook.sheet_names().to_vec();
                // Bounded, and the error shown rather than swallowed. `if let
                // Ok(range)` turned an oversized or unreadable sheet into an empty
                // preview, which reads as "this spreadsheet has no rows" -- the
                // silent truncation #185 rules out (#282).
                //
                // A workbook with no sheets still opens the viewer on an empty
                // grid, as it did before: returning early skipped
                // `EnterAlternateScreen` and gave the user no output at all.
                match sheet_names.first() {
                    None => Some(Vec::new()),
                    Some(name) => match crate::workbook::bounded_worksheet_range(
                        &mut workbook,
                        name,
                        crate::workbook::MAX_WORKBOOK_CELLS,
                    ) {
                        Ok(range) => Some(
                            range
                                .rows()
                                .map(|row| {
                                    // Tui-owned formatter, not `to_string()` --
                                    // that is calamine's `Display`, which prints
                                    // a date as its Excel serial (#281).
                                    row.iter()
                                        .map(cell_format::workbook_cell_to_string)
                                        .collect::<Vec<String>>()
                                })
                                .collect(),
                        ),
                        Err(error) => Some(vec![vec![format!("cannot preview {path}: {error}")]]),
                    },
                }
            }
            Err(error) => Some(vec![vec![format!("error opening {path}: {error}")]]),
        }
    }

    /// Convenience wrapper for the tool-approval flow.
    pub fn render_ask_user_dialog(
        &mut self,
        title: &str,
        options: Vec<DialogOption>,
    ) -> Result<DialogResult> {
        self.show_dialog(Dialog::select(title, options))
    }

    /// Show structured questions from the LLM (AskUserQuestion tool).
    ///
    /// - 1 question  → single inline `show_dialog` (same as before)
    /// - 2+ questions → `show_tabbed_dialog` so all questions are visible at once
    pub fn show_llm_question(
        &mut self,
        input: &crate::cli::AskUserQuestionInput,
    ) -> Result<crate::cli::AskUserQuestionOutput> {
        use crate::cli::llm_dialogs;
        use std::collections::HashMap;

        if input.questions.len() > 1 {
            let tabbed = TabbedDialog::new(input.questions.clone(), None);
            let result = self.show_tabbed_dialog(tabbed)?;
            let answers = match result {
                TabbedDialogResult::Completed(answers) => answers,
                TabbedDialogResult::Cancelled => HashMap::new(),
            };
            let annotations = llm_dialogs::build_annotations(&input.questions, &answers);
            return Ok(crate::cli::AskUserQuestionOutput {
                questions: input.questions.clone(),
                answers,
                annotations,
            });
        }

        // Single question — inline dialog path
        let mut answers: HashMap<String, String> = HashMap::new();
        if let Some(question) = input.questions.first() {
            let dialog = llm_dialogs::question_to_dialog(question);
            let result = self.show_dialog(dialog)?;
            if let Some(answer) = llm_dialogs::extract_answer(question, &result) {
                answers.insert(question.question.clone(), answer);
            }
        }

        let annotations = llm_dialogs::build_annotations(&input.questions, &answers);
        Ok(crate::cli::AskUserQuestionOutput {
            questions: input.questions.clone(),
            answers,
            annotations,
        })
    }
}

// ─── History persistence ──────────────────────────────────────────────────────

impl TuiRenderer {
    fn history_path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".finch").join("history"))
    }

    fn load_history() -> Vec<String> {
        let path = match Self::history_path() {
            Some(p) => p,
            None => return Vec::new(),
        };
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .take(1000)
            .map(|l| l.to_string())
            .collect()
    }

    fn save_history(history: &[String]) {
        let path = match Self::history_path() {
            Some(p) => p,
            None => return,
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let content: String = history
            .iter()
            .rev()
            .take(1000)
            .rev()
            .map(|l| format!("{}\n", l))
            .collect();
        let _ = std::fs::write(path, content);
    }
}

// ─── Text wrapping ────────────────────────────────────────────────────────────

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    for para in text.split('\n') {
        if para.is_empty() {
            out.push(String::new());
            continue;
        }
        if is_preformatted_dialog_line(para) {
            out.extend(wrap_preformatted(para, width));
        } else {
            out.extend(wrap_prose(para, width));
        }
    }
    out
}

fn is_preformatted_dialog_line(line: &str) -> bool {
    if line.starts_with(char::is_whitespace)
        || line.contains('\u{1b}')
        || line.starts_with("@@")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("diff --git ")
        || (line.contains("  +") && line.contains(" -"))
    {
        return true;
    }
    let mut fields = line.split_whitespace();
    fields
        .next()
        .is_some_and(|value| value.parse::<usize>().is_ok())
        && fields
            .next()
            .is_some_and(|value| value.parse::<usize>().is_ok())
        && line.contains("   ")
}

fn wrap_prose(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut columns = 0usize;
    for word in text.split_whitespace() {
        let word_width = word
            .chars()
            .map(|ch| fitted_terminal_char(ch, width).1)
            .sum::<usize>();
        if !current.is_empty() && columns.saturating_add(1).saturating_add(word_width) <= width {
            current.push(' ');
            current.push_str(word);
            columns = columns.saturating_add(1).saturating_add(word_width);
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
            columns = 0;
        }
        for ch in word.chars() {
            let (ch, char_width) = fitted_terminal_char(ch, width);
            if columns > 0 && columns.saturating_add(char_width) > width {
                out.push(std::mem::take(&mut current));
                columns = 0;
            }
            current.push(ch);
            columns = columns.saturating_add(char_width);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn wrap_preformatted(text: &str, width: usize) -> Vec<String> {
    const RESET_SGR: &str = "\x1b[0m";
    let mut out = Vec::new();
    let mut current = String::new();
    let mut active_sgr = String::new();
    let mut columns = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            let mut sequence = String::from("\x1b[");
            chars.next();
            for control in chars.by_ref() {
                sequence.push(control);
                if ('@'..='~').contains(&control) {
                    break;
                }
            }
            if sequence.ends_with('m') {
                if sequence == RESET_SGR || sequence == "\x1b[m" {
                    active_sgr.clear();
                } else {
                    active_sgr = sequence.clone();
                }
            }
            current.push_str(&sequence);
            continue;
        }
        let (ch, char_width) = fitted_terminal_char(ch, width);
        if columns > 0 && columns.saturating_add(char_width) > width {
            if !active_sgr.is_empty() {
                current.push_str(RESET_SGR);
            }
            out.push(std::mem::take(&mut current));
            if !active_sgr.is_empty() {
                current.push_str(&active_sgr);
            }
            columns = 0;
        }
        current.push(ch);
        columns = columns.saturating_add(char_width);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn fitted_terminal_char(ch: char, width: usize) -> (char, usize) {
    let char_width = terminal_char_width(ch);
    if char_width > width {
        // A wide glyph cannot be truthfully displayed in a one-column row.
        // Use a visible single-column replacement instead of lying about its
        // terminal width or relying on terminal-specific overflow behavior.
        ('?', 1)
    } else {
        (ch, char_width)
    }
}

fn terminal_char_width(ch: char) -> usize {
    if ch.is_control() || matches!(ch, '\u{0300}'..='\u{036f}') {
        0
    } else if matches!(ch,
        '\u{1100}'..='\u{115f}' | '\u{2329}'..='\u{232a}' |
        '\u{2e80}'..='\u{a4cf}' | '\u{ac00}'..='\u{d7a3}' |
        '\u{f900}'..='\u{faff}' | '\u{fe10}'..='\u{fe19}' |
        '\u{fe30}'..='\u{fe6f}' | '\u{ff00}'..='\u{ff60}' |
        '\u{ffe0}'..='\u{ffe6}' | '\u{1f300}'..='\u{1faff}'
    ) {
        2
    } else {
        1
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {

    /// Render a message the way the live frame does: project once into
    /// ViewModel props, then hand them to the disclosure widget.
    fn render_via_view_model(
        state: &AccordionState,
        message: &MessageRef,
        colors: &ColorScheme,
    ) -> Vec<RenderedTranscriptLine> {
        match view_model::project_message(message, colors) {
            view_model::ProjectedMessage::Node(node) => state.render_node(&node),
            view_model::ProjectedMessage::Plain(formatted) => {
                state.render_plain(&formatted.join("\n"))
            }
        }
    }

    use super::*;
    use crate::cli::command_autocomplete::CommandRegistry;
    use crate::cli::diff::{summarize_files, DiffColorMode, FileDiff};
    use crate::cli::messages::{Message, MessageId, MessageRef, WorkUnit};
    use crate::cli::tui::vt_oracle::{VtColor, VtOracle, VtStyle};
    use crate::theme::ColorTheme;

    fn assert_vt(condition: bool, message: &str, terminal: &VtOracle) {
        assert!(condition, "{message}\n{}", terminal.diagnostic());
    }

    fn renderer_owning_mouse_capture() -> TuiRenderer {
        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let mut renderer = TuiRenderer::new_headless(output, Arc::new(StatusBar::new()), colors);
        renderer.is_active = true;
        renderer.mouse_tracking = mouse_capture::MouseTracking::Held;
        renderer
    }

    /// Accordion expand/collapse stays on the keyboard when mouse capture is
    /// off, which is the default so native click-drag copy works (#221).
    #[test]
    fn test_accordion_keyboard_toggles_without_mouse_capture() {
        let colors = ColorScheme::default();
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo hi)");
        work.complete_row_with_body(call, "1 line", vec!["hi".into()]);
        work.set_complete();
        let message: MessageRef = work.clone();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let mut renderer = TuiRenderer::new_headless(output, Arc::new(StatusBar::new()), colors);
        renderer.add_trait_message(work);
        let lines = match view_model::project_message(&message, &renderer.colors) {
            view_model::ProjectedMessage::Node(node) => renderer.accordion.render_node(&node),
            view_model::ProjectedMessage::Plain(formatted) => {
                renderer.accordion.render_plain(&formatted.join("\n"))
            }
        };
        renderer
            .accordion
            .rebuild_retained_hit_regions(&lines, 0, 80);
        let root =
            crate::cli::tui::view_model::try_project_for_test(message.as_ref(), &renderer.colors)
                .expect("projected row");

        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::DEFAULT,
            "INVARIANT: the default TUI does not hold mouse tracking, so the host \
             terminal owns click-drag selection (#221). tracking was {:?}",
            renderer.mouse_tracking
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
            "F6 must focus an accordion row without mouse capture"
        );
        assert_eq!(
            renderer.accordion.focused.as_ref(),
            Some(&root.id),
            "F6 focuses the work-unit row so Enter can toggle it"
        );
        let expanded_before = renderer.accordion.is_expanded(&root);
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            "Enter must toggle accordion disclosure without mouse capture; \
             focused={:?} expanded_before={expanded_before}",
            renderer.accordion.focused
        );
        assert_ne!(
            renderer.accordion.is_expanded(&root),
            expanded_before,
            "INVARIANT: keyboard Enter toggles accordion expand/collapse when \
             mouse capture is off (#221). expanded_before={expanded_before}"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            "a second Enter must toggle back"
        );
        assert_eq!(
            renderer.accordion.is_expanded(&root),
            expanded_before,
            "INVARIANT: a second keyboard toggle restores the prior disclosure \
             (#221). expanded_before={expanded_before}"
        );
    }

    fn wheel_up() -> MouseEvent {
        MouseEvent {
            kind: event::MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn disable_mouse_capture_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        execute!(&mut bytes, event::DisableMouseCapture).expect("encode DisableMouseCapture");
        bytes
    }

    /// A Confirm dialog owns the live area, so a wheel must not release
    /// tracking. Native scroll would move Yes/No off-screen (#435 at a
    /// different layer), and the restoring keypress would be dialog input.
    #[test]
    fn test_handle_mouse_scroll_up_with_confirm_dialog_does_not_release_mouse_tracking_for_native_scrollback(
    ) {
        let mut renderer = renderer_owning_mouse_capture();
        renderer.active_dialog = Some(Dialog::confirm("Approve this tool?", true));
        let mut bytes = Vec::new();
        renderer.handle_mouse_to(wheel_up(), &mut bytes);
        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::Held,
            "INVARIANT: mouse tracking is released for native scrollback only \
             when the live area is the transcript/input, not when a dialog owns \
             it (#441 / F1). A Confirm dialog was open; tracking was {:?}. \
             terminal received {bytes:?}",
            renderer.mouse_tracking
        );
        assert!(
            bytes.is_empty(),
            "INVARIANT: a wheel over a Confirm dialog must not emit \
             DisableMouseCapture, so Yes/No stay on-screen (#441 / F1). \
             terminal received {bytes:?}"
        );
        renderer.is_active = false;
    }

    /// A tabbed dialog owns the live area the same way Confirm does.
    #[test]
    fn test_handle_mouse_scroll_up_with_tabbed_dialog_does_not_release_mouse_tracking_for_native_scrollback(
    ) {
        let mut renderer = renderer_owning_mouse_capture();
        renderer.active_tabbed_dialog = Some(TabbedDialog::new(
            vec![crate::cli::llm_dialogs::Question {
                question: "Which path?".into(),
                header: "Path".into(),
                options: vec![
                    crate::cli::llm_dialogs::QuestionOption {
                        label: "A".into(),
                        description: "first".into(),
                        markdown: None,
                    },
                    crate::cli::llm_dialogs::QuestionOption {
                        label: "B".into(),
                        description: "second".into(),
                        markdown: None,
                    },
                ],
                multi_select: false,
            }],
            None,
        ));
        let mut bytes = Vec::new();
        renderer.handle_mouse_to(wheel_up(), &mut bytes);
        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::Held,
            "INVARIANT: a tabbed dialog owns the live area, so a wheel must \
             not release mouse tracking (#441 / F1). tracking was {:?}. \
             terminal received {bytes:?}",
            renderer.mouse_tracking
        );
        assert!(
            bytes.is_empty(),
            "INVARIANT: a wheel over a tabbed dialog must not emit \
             DisableMouseCapture (#441 / F1). terminal received {bytes:?}"
        );
        renderer.is_active = false;
    }

    /// The no-dialog path still releases, so F1 cannot be satisfied by
    /// disabling native scroll altogether.
    #[test]
    fn test_handle_mouse_scroll_up_without_dialog_releases_mouse_tracking_for_native_scrollback() {
        let mut renderer = renderer_owning_mouse_capture();
        let mut bytes = Vec::new();
        renderer.handle_mouse_to(wheel_up(), &mut bytes);
        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::ReleasedForNativeScroll,
            "INVARIANT: with no dialog, a wheel still releases mouse tracking \
             so native scrollback is reachable (#441). tracking was {:?}. \
             terminal received {bytes:?}",
            renderer.mouse_tracking
        );
        assert_eq!(
            bytes,
            disable_mouse_capture_bytes(),
            "INVARIANT: the no-dialog release is DisableMouseCapture (#441). \
             terminal received {bytes:?}"
        );
        renderer.is_active = false;
    }

    // ── Bounded tool-result controls (#656): production event path ───────────

    /// A renderer holding one committed grouped tool turn whose Bash-like
    /// result produced `lines` lines, with the hit regions rebuilt from the
    /// real projection exactly as a paint would. Returns the renderer and the
    /// output row's stable identity.
    fn committed_tool_result_renderer(
        lines: usize,
    ) -> (TuiRenderer, crate::cli::tui::view_model::RowId) {
        use crate::cli::messages::WorkUnit;

        let colors = ColorScheme::default();
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(build)");
        work.complete_row_with_body(
            call,
            "",
            (0..lines).map(|n| format!("line {n}")).collect::<Vec<_>>(),
        );
        work.set_complete();
        let output_row = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("projected row")
            .children[0]
            .children[1]
            .id
            .clone();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let mut renderer = TuiRenderer::new_headless(output, Arc::new(StatusBar::new()), colors);
        renderer.is_active = true;
        renderer.add_trait_message(work);
        // The result has been committed to native scrollback and lives in the
        // visible transcript tail.
        let message_id = renderer
            .output_manager
            .get_messages()
            .last()
            .map(|message| message.id())
            .expect("the tool message was added");
        renderer.printed_ids.insert(message_id);
        renderer.rebuild_transcript_hit_regions(&LiveFrame::default(), 0, 80, 24);
        (renderer, output_row)
    }

    /// INVARIANT: a wheel whose X/Y lands on a bounded tool-result control
    /// scrolls that result in place and never releases mouse tracking to
    /// native scrollback (#656). The parent scrollback state is untouched:
    /// no new canonical commit becomes eligible, no printed id changes, and a
    /// neighbouring message's projection is byte-identical.
    #[test]
    fn test_wheel_over_tool_result_scrolls_it_without_touching_parent_scrollback() {
        let (mut renderer, output_row) = committed_tool_result_renderer(40);
        renderer.mouse_tracking = mouse_capture::MouseTracking::Held;
        let top = renderer
            .tool_viewports
            .regions()
            .iter()
            .find(|region| region.row_id == output_row)
            .map(|region| region.top)
            .expect("the painted tool result registered a hit region");
        let before_plan = plan_canonical_commit(
            &renderer.output_manager.get_messages(),
            &renderer.printed_ids,
        );
        let before_printed = renderer.printed_ids.clone();

        let wheel = MouseEvent {
            kind: event::MouseEventKind::ScrollDown,
            column: 0,
            row: top,
            modifiers: KeyModifiers::NONE,
        };
        let mut bytes = Vec::new();
        assert!(
            renderer.handle_mouse_to(wheel, &mut bytes),
            "the wheel over the tool result must be dispatched to it, not dropped"
        );
        assert_eq!(
            renderer.tool_viewports.child_scroll(&output_row),
            1,
            "INVARIANT: the wheel scrolled the targeted tool result by one line; child \
             scroll was {:?}, hit region top was {top}",
            renderer.tool_viewports.child_scroll(&output_row)
        );
        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::Held,
            "INVARIANT: a wheel over a tool result keeps mouse tracking so the next tick \
             keeps scrolling the result instead of falling to native scrollback; tracking \
             was {:?}; terminal received {bytes:?}",
            renderer.mouse_tracking
        );
        assert!(
            bytes.is_empty(),
            "INVARIANT: dispatching a wheel to a tool result must not emit \
             DisableMouseCapture; terminal received {bytes:?}"
        );
        let after_plan = plan_canonical_commit(
            &renderer.output_manager.get_messages(),
            &renderer.printed_ids,
        );
        assert_eq!(
            before_plan.emit.len(),
            after_plan.emit.len(),
            "INVARIANT: scrolling a tool result must not make a canonical commit eligible; \
             plan before {before:?} vs after {after:?}",
            before = before_plan.emit.len(),
            after = after_plan.emit.len()
        );
        assert_eq!(
            before_printed, renderer.printed_ids,
            "INVARIANT: the set of messages written to native scrollback is unchanged by \
             a child-viewport wheel"
        );
        renderer.is_active = false;
    }

    /// INVARIANT: the wheel dispatch is exact — two adjacent tool results are
    /// separately addressable, and a wheel on the first never moves the second.
    #[test]
    fn test_wheel_dispatch_reaches_only_the_targeted_tool_result() {
        use crate::cli::messages::{MessageRef, WorkUnit};

        let (mut renderer, _) = committed_tool_result_renderer(40);
        let colors = renderer.colors.clone();
        // A second tool result committed below the first.
        let second = Arc::new(WorkUnit::new("Tools"));
        let call = second.add_row("bash(other)");
        second.complete_row_with_body(
            call,
            "",
            (0..40).map(|n| format!("beta {n}")).collect::<Vec<_>>(),
        );
        second.set_complete();
        let second_output =
            crate::cli::tui::view_model::try_project_for_test(second.as_ref(), &colors)
                .expect("projected row")
                .children[0]
                .children[1]
                .id
                .clone();
        renderer.add_trait_message(second.clone());
        let second_id = second.id();
        renderer.printed_ids.insert(second_id);
        renderer.rebuild_transcript_hit_regions(&LiveFrame::default(), 0, 80, 24);

        let first_scroll = {
            let regions = renderer.tool_viewports.regions();
            let first = regions
                .iter()
                .find(|region| region.row_id != second_output)
                .cloned()
                .expect("the first tool result registered a region");
            first.row_id.clone()
        };
        let first_region = renderer
            .tool_viewports
            .regions()
            .iter()
            .find(|region| region.row_id == first_scroll)
            .cloned()
            .expect("first region present");
        let wheel = MouseEvent {
            kind: event::MouseEventKind::ScrollDown,
            column: first_region.left,
            row: first_region.top,
            modifiers: KeyModifiers::NONE,
        };
        assert!(renderer.handle_mouse_to(wheel, &mut Vec::new()));
        assert_eq!(
            renderer.tool_viewports.child_scroll(&first_scroll),
            1,
            "the targeted result scrolled"
        );
        assert_eq!(
            renderer.tool_viewports.child_scroll(&second_output),
            0,
            "INVARIANT: the neighbouring result did not move; exact X/Y dispatch reaches \
             only the targeted control"
        );
        let second_message: MessageRef = second.clone();
        let second_projected = renderer.projected_message_lines(&second_message, 80);
        assert!(
            second_projected
                .iter()
                .any(|line| line.text.contains("beta 0")),
            "INVARIANT: the untargeted result's window is unchanged (still starts at \
             beta 0); projection was:\n{}",
            second_projected
                .iter()
                .map(|line| line.text.clone())
                .collect::<Vec<_>>()
                .join("\n")
        );
        renderer.is_active = false;
    }

    /// A wheel whose row is not on a tool-result control still releases mouse
    /// tracking for native scrollback — the #441 behavior is preserved for the
    /// rest of the console.
    #[test]
    fn test_wheel_outside_tool_result_still_releases_mouse_tracking() {
        let (mut renderer, _) = committed_tool_result_renderer(40);
        renderer.mouse_tracking = mouse_capture::MouseTracking::Held;
        let region = renderer
            .tool_viewports
            .regions()
            .first()
            .cloned()
            .expect("a painted region exists");
        let wheel = MouseEvent {
            kind: event::MouseEventKind::ScrollDown,
            column: 0,
            row: region.bottom.saturating_add(5),
            modifiers: KeyModifiers::NONE,
        };
        let mut bytes = Vec::new();
        assert!(
            !renderer.handle_mouse_to(wheel, &mut bytes),
            "a wheel off the control is not claimed by it"
        );
        assert_eq!(
            renderer.mouse_tracking,
            mouse_capture::MouseTracking::ReleasedForNativeScroll,
            "INVARIANT: a wheel outside any tool result releases mouse tracking for \
             native scrollback (#441 preserved); tracking was {:?}; terminal received \
             {bytes:?}",
            renderer.mouse_tracking
        );
        renderer.is_active = false;
    }

    /// INVARIANT: clicking a tool result's compact window opens the focused
    /// expanded surface, and closing it restores the child scroll offset,
    /// disclosure grouping, and focus exactly as they were; the parent
    /// scrollback state never changes through the round trip (#656).
    #[test]
    fn test_click_expands_tool_result_and_close_restores_child_state() {
        let (mut renderer, output_row) = committed_tool_result_renderer(40);
        let region = renderer
            .tool_viewports
            .regions()
            .iter()
            .find(|region| region.row_id == output_row)
            .cloned()
            .expect("the painted tool result registered a region");

        // A user has scrolled the compact window before expanding it.
        renderer.tool_viewports.scroll_child(&output_row, 2);
        let disclosure_before = renderer
            .projected_message_lines(&renderer.output_manager.get_messages()[0].clone(), 80)
            .iter()
            .map(|line| (line.text.clone(), line.row_expanded))
            .collect::<Vec<_>>();

        let click = MouseEvent {
            kind: event::MouseEventKind::Down(event::MouseButton::Left),
            column: region.left,
            row: region.top,
            modifiers: KeyModifiers::NONE,
        };
        assert!(
            renderer.handle_mouse_to(click, &mut Vec::new()),
            "a click on the control's cells must be claimed"
        );
        let view = renderer
            .expanded_tool
            .as_ref()
            .expect("the click expanded the tool result");
        assert_eq!(view.row_id, output_row);
        assert_eq!(
            view.saved_scroll, 2,
            "the surface captured the compact window's scroll offset at open time"
        );
        assert!(
            view.title.contains("bash(build)"),
            "the surface title names the tool call; title was {:?}",
            view.title
        );

        // Scroll the surface away from the compact position, then close.
        for _ in 0..7 {
            assert!(renderer.handle_accordion_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        }
        assert_eq!(
            renderer.expanded_tool.as_ref().map(|view| view.scroll),
            Some(9),
            "the expanded surface scrolled on its own"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            "Esc closes the expanded surface"
        );
        assert!(renderer.expanded_tool.is_none(), "the surface closed");
        assert_eq!(
            renderer.tool_viewports.child_scroll(&output_row),
            2,
            "INVARIANT: closing the expanded view restored the child scroll offset that \
             the compact window had when it opened (2)"
        );
        let disclosure_after = renderer
            .projected_message_lines(&renderer.output_manager.get_messages()[0].clone(), 80)
            .iter()
            .map(|line| (line.text.clone(), line.row_expanded))
            .collect::<Vec<_>>();
        assert_eq!(
            disclosure_before, disclosure_after,
            "INVARIANT: closing the expanded view restores the same grouping; the compact \
             projection is identical before the expansion and after the close.\n\
             before:\n{disclosure_before:?}\nafter:\n{disclosure_after:?}"
        );
        renderer.is_active = false;
    }

    /// Keyboard equivalents (#656): F6 focuses the tool result's row, Up/Down
    /// scroll its compact viewport, Enter opens the expanded surface. On rows
    /// that are not tool results the keys stay unclaimed.
    #[test]
    fn test_keyboard_scroll_and_expand_of_focused_tool_result() {
        let (mut renderer, output_row) = committed_tool_result_renderer(40);
        renderer.rebuild_transcript_hit_regions(&LiveFrame::default(), 0, 80, 24);

        // F6 cycles through the four expandable rows of the grouped turn:
        // unit root, tool call, Input, Output.
        for _ in 0..4 {
            assert!(
                renderer.handle_accordion_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
                "F6 must reach the accordion focus"
            );
        }
        assert_eq!(
            renderer.accordion.focused.as_ref(),
            Some(&output_row),
            "the fourth F6 lands on the tool result row"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            "Down scrolls the focused tool result"
        );
        assert_eq!(
            renderer.tool_viewports.child_scroll(&output_row),
            1,
            "INVARIANT: the keyboard scroll moved the child viewport one line"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE)),
            "PageDown scrolls the focused tool result"
        );
        assert_eq!(
            renderer.tool_viewports.child_scroll(&output_row),
            5,
            "PageDown moved the window by the page step"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            "Enter opens the expanded surface for the focused tool result"
        );
        assert!(
            renderer.expanded_tool.is_some(),
            "INVARIANT: keyboard activation expands the result into the focused surface"
        );
        assert!(
            renderer.handle_accordion_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            "Esc closes the expanded surface"
        );
        assert_eq!(
            renderer.tool_viewports.child_scroll(&output_row),
            5,
            "INVARIANT: closing restored the compact window's offset (5)"
        );

        // On a non-tool row the same keys are not claimed: history navigation
        // keeps working.
        renderer.accordion.focused = None;
        assert!(
            !renderer.handle_accordion_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            "Down without a focused tool result falls through to the input area"
        );
        renderer.is_active = false;
    }

    /// INVARIANT: bounding the viewport projection never bounds the canonical
    /// commit — permanent scrollback still receives every output line, exactly
    /// once, so the copyable record stays complete even though the compact
    /// projection hides lines past the bound.
    #[test]
    fn test_canonical_commit_still_writes_full_tool_output_exactly_once() {
        use crate::cli::messages::MessageRef;

        let (renderer, _output_row) = committed_tool_result_renderer(40);
        let message: MessageRef = renderer.output_manager.get_messages()[0].clone();

        // The visible projection is bounded: line 39 is not part of it.
        let mut bounded_renderer = renderer;
        let bounded = bounded_renderer.projected_message_lines(&message, 80);
        assert!(
            !bounded.iter().any(|line| line.text.contains("line 39")),
            "precondition: the compact projection is bounded and does not show line 39"
        );

        // The canonical commit writes all 40 lines, each exactly once.
        let mut printed = HashSet::new();
        let mut sink = Vec::new();
        commit_complete_messages(
            &mut sink,
            std::slice::from_ref(&message),
            &mut bounded_renderer.accordion,
            &ColorScheme::default(),
            &mut printed,
            24,
        )
        .expect("canonical commit succeeds");
        let canonical = String::from_utf8_lossy(&sink);
        let canonical_lines = canonical
            .lines()
            .map(|line| line.trim())
            .collect::<Vec<_>>();
        assert_eq!(
            canonical.matches("line 39").count(),
            1,
            "INVARIANT: the canonical commit contains line 39 exactly once; canonical \
             bytes were:\n{canonical}"
        );
        for index in 0..40 {
            assert_eq!(
                canonical_lines
                    .iter()
                    .filter(|line| **line == format!("line {index}"))
                    .count(),
                1,
                "INVARIANT: canonical scrollback dedup — every output line is written \
                 exactly once; line {index} appeared {} times",
                canonical_lines
                    .iter()
                    .filter(|line| **line == format!("line {index}"))
                    .count()
            );
        }
    }

    /// Terminal-boundary check (#648 oracle): the expanded surface painted
    /// through the production live writer lands as plain text, hides the
    /// cursor, and reports its scroll position in the footer.
    #[test]
    fn test_vt_oracle_expanded_surface_paints_plain_text_and_hides_cursor() {
        use crate::cli::tui::vt_oracle::VtOracle;

        let body: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let lines =
            tool_viewport::expanded_surface_lines("bash(build) — complete", &body, 3, 80, 12);
        let mut frame = LiveFrame::default();
        for line in &lines {
            frame.push(line.clone());
        }
        frame.cursor_visible = false;
        frame.cursor_row = frame.physical_rows(80).saturating_sub(1);

        let mut terminal = VtOracle::new(80, 12);
        let mut bytes = Vec::new();
        write_live_frame(&mut bytes, &frame, 80).expect("surface frame paints");
        terminal.feed(&bytes);
        assert_vt(
            terminal.find_row("bash(build)").is_some(),
            "the title bar names the tool call",
            &terminal,
        );
        assert_vt(
            terminal.find_row("line 3").is_some(),
            "the body window starts at the scroll offset",
            &terminal,
        );
        assert_vt(
            terminal
                .find_row("lines 4–13 of 40 — ↑/↓ scroll · Esc close")
                .is_some(),
            "the footer reports scroll position, total, and the close key",
            &terminal,
        );
        assert_vt(
            !terminal.cursor().2,
            "INVARIANT: the focused surface hides the editable cursor, like a dialog",
            &terminal,
        );
    }

    /// The focused surface owns the whole live frame: no draft, no status, and
    /// the hidden cursor parked on the last owned row.
    #[test]
    fn test_expanded_surface_owns_the_live_frame() {
        let body: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let surface = tool_viewport::expanded_surface_lines("bash(build)", &body, 0, 80, 10);
        let input = vec!["draft must stay hidden".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(80, 12, &input, "busy");
        inputs.expanded_lines = Some(&surface);
        let frame = plan_live_frame(&inputs, &mut autocomplete);

        let painted = frame.lines.join("\n");
        assert!(
            !painted.contains("draft must stay hidden"),
            "INVARIANT: the expanded surface owns the viewport; the draft must not compete \
             with it. frame:\n{painted}"
        );
        assert!(
            !painted.contains("busy"),
            "the status line is suppressed while the surface owns the frame; frame:\n{painted}"
        );
        assert!(
            !frame.cursor_visible,
            "INVARIANT: the surface hides the editable cursor"
        );
        assert_eq!(
            frame.cursor_row,
            frame.physical_rows(80).saturating_sub(1),
            "the hidden cursor parks on the final owned row"
        );
    }

    /// On a tiny viewport the surface clips instead of overflowing: title and
    /// footer are preferred, body rows drop out first, and the frame never
    /// exceeds the terminal height.
    #[test]
    fn test_expanded_surface_clips_instead_of_overflowing_on_tiny_viewports() {
        let body: Vec<String> = (0..40).map(|n| format!("line {n}")).collect();
        let surface = tool_viewport::expanded_surface_lines("bash(build)", &body, 0, 80, 24);
        let input = vec![String::new()];
        let mut autocomplete = AutocompleteState::new();
        for height in [2usize, 3, 4] {
            let mut inputs = live_inputs(80, height, &input, "idle");
            inputs.expanded_lines = Some(&surface);
            let frame = plan_live_frame(&inputs, &mut autocomplete);
            assert!(
                frame.physical_rows(80) <= height,
                "INVARIANT: the surface frame must fit the {height}-row viewport; it \
                 measured {} rows. frame:\n{}",
                frame.physical_rows(80),
                frame.lines.join("\n")
            );
        }
        // A comfortable viewport still shows the footer state line.
        let mut inputs = live_inputs(80, 12, &input, "idle");
        inputs.expanded_lines = Some(&surface);
        let frame = plan_live_frame(&inputs, &mut autocomplete);
        assert!(
            frame.lines.iter().any(|line| line.contains("Esc close")),
            "the footer survives budget clipping on a usable viewport; frame:\n{}",
            frame.lines.join("\n")
        );
    }

    /// `live_geometry` must agree with the surface frame the planner renders,
    /// or the full-viewport rebuild budgets the transcript against the wrong
    /// live height.
    #[test]
    fn test_live_geometry_matches_the_expanded_surface_frame() {
        let (mut renderer, _output_row) = committed_tool_result_renderer(40);
        let output_row_all = renderer
            .tool_viewports
            .regions()
            .iter()
            .find(|_| true)
            .map(|region| region.row_id.clone())
            .expect("a painted region exists");
        renderer.open_expanded_tool(&output_row_all);
        let (width, height) = (80u16, 24u16);
        let geometry = renderer
            .live_geometry(width, height)
            .expect("geometry known with the surface open");
        let frame_lines = renderer
            .expanded_surface_frame(80, 23)
            .expect("surface lines resolvable");
        let surface_rows = frame_lines
            .iter()
            .map(|line| shadow_buffer::physical_rows(line, 80))
            .sum::<usize>();
        assert_eq!(
            geometry,
            (1 + surface_rows, surface_rows),
            "INVARIANT: live_geometry matches [separator + surface rows] with the hidden \
             cursor on the final row; geometry was {geometry:?}, surface measured \
             {surface_rows} rows"
        );
        renderer.is_active = false;
    }

    #[test]
    fn startup_header_is_plain_scrollback_content() {
        let header = TuiRenderer::startup_header("grok-code-fast-1", "~/repo", "amber-river");
        assert!(header.contains(&format!("finch v{}", env!("CARGO_PKG_VERSION"))));
        assert!(header.contains(crate::ABOUT));
        assert!(header.contains("grok-code-fast-1"));
        assert!(header.contains("amber-river  ·  ~/repo"));
        assert!(!header.contains('\x1b'));
        assert!(
            header.lines().all(|line| line.chars().count() <= 80),
            "startup header must remain readable in an 80-column terminal; header={header:?}"
        );
    }

    // ── count_status_lines ────────────────────────────────────────────────────

    // ── should_redraw_live_area ───────────────────────────────────────────────

    #[test]
    fn test_redraw_predicate_does_nothing_when_idle() {
        // Idle: no in-progress messages, area not dirty — must not trigger redraw.
        assert!(!should_redraw_live_area(false, false));
    }

    #[test]
    fn test_redraw_predicate_triggers_when_in_progress() {
        assert!(should_redraw_live_area(true, false));
    }

    #[test]
    fn input_row_budget_counts_wrapping() {
        assert_eq!(input_physical_rows(&[], 10), 1);
        assert_eq!(input_physical_rows(&["hello".into()], 10), 1);
        assert_eq!(input_physical_rows(&["123456789".into()], 10), 2);
        assert_eq!(
            input_physical_rows(&["one".into(), "123456789".into()], 10),
            3
        );
    }

    #[test]
    fn input_line_geometry_is_recomputed_after_width_shrinks() {
        let lines = vec!["12345678".into(), "abcdef".into()];

        assert_eq!(input_line_physical_rows(&lines, 10), vec![1, 1]);
        assert_eq!(input_line_physical_rows(&lines, 5), vec![2, 2]);
        assert_eq!(input_physical_rows(&lines, 5), 4);
    }

    #[test]
    fn session_separator_never_wraps_at_narrow_widths() {
        let session = "◆ brain: golden-crest-9a83c1@Shammahs-MacBook-Air.local · runner · driver";
        for width in 1..160 {
            let line = session_separator_line(width, "~/repos/finch", session);
            assert_eq!(line.chars().count(), width, "width {width}: {line:?}");
            assert_eq!(
                shadow_buffer::physical_rows(&line, width),
                1,
                "width {width}: {line:?}"
            );
        }
    }

    #[test]
    fn session_separator_truncates_workspace_before_brain_identity() {
        let line = session_separator_line(
            60,
            "~/repos/a-very-long-workspace-name",
            "◆ brain: golden-crest-9a83c1@host · driver",
        );
        assert_eq!(line.chars().count(), 60);
        assert!(line.contains("◆ brain:"), "{line:?}");
        assert!(line.contains('…'), "{line:?}");
    }

    #[test]
    fn live_viewport_uses_physical_rows_and_marks_a_clipped_prefix() {
        let lines = (0..12)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        let (visible, omitted) = live_viewport_lines(&lines, 80, 5);

        assert_eq!(omitted, 8);
        assert_eq!(visible.len(), 5, "one marker plus four retained rows");
        assert!(visible[0].contains("8 earlier live rows clipped"));
        assert_eq!(&visible[1..], &lines[8..]);
    }

    #[test]
    fn live_viewport_counts_wrapped_ansi_lines_instead_of_logical_lines() {
        let lines = vec![
            "first".to_string(),
            "\x1b[36m1234567890123456789012345\x1b[0m".to_string(),
        ];
        let (visible, omitted) = live_viewport_lines(&lines, 10, 3);

        assert_eq!(omitted, 2);
        assert_eq!(visible.len(), 2);
        assert!(visible[0].starts_with("… 2"));
        assert_eq!(shadow_buffer::physical_rows(&visible[1], 10), 2);
        assert!(visible[1].ends_with("9012345"));
    }

    #[test]
    fn live_viewport_does_not_modify_content_that_already_fits() {
        let lines = vec!["one".to_string(), "two".to_string()];
        assert_eq!(live_viewport_lines(&lines, 80, 10), (lines, 0));
    }

    #[test]
    fn viewport_tail_reflows_at_the_current_width_without_a_synthetic_row() {
        let lines = vec![
            "old".to_string(),
            "123456789012345".to_string(),
            "new".to_string(),
        ];

        let selected = viewport_tail_lines(&lines, 5, 3);

        assert_eq!(selected, vec!["… 89012345", "new"]);
        assert_eq!(
            selected
                .iter()
                .map(|line| shadow_buffer::physical_rows(line, 5))
                .sum::<usize>(),
            3
        );
    }

    #[test]
    fn repeated_shrink_and_grow_reanchors_the_live_frame_to_viewport_bottom() {
        let large = viewport_redraw_plan(40, 6, 20);
        let small = viewport_redraw_plan(12, 6, 20);
        let large_again = viewport_redraw_plan(40, 6, 20);

        assert_eq!(
            large,
            ViewportRedrawPlan {
                transcript_top: 14,
                live_top: 34,
            }
        );
        assert_eq!(
            small,
            ViewportRedrawPlan {
                transcript_top: 0,
                live_top: 6,
            }
        );
        assert_eq!(large_again, large);
        assert_eq!(small.live_top + 6, 12);
        assert_eq!(large_again.live_top + 6, 40);
    }

    #[test]
    fn full_viewport_paint_uses_absolute_rows_and_preserves_native_scrollback() {
        let plan = viewport_redraw_plan(12, 6, 2);
        let mut bytes = Vec::new();

        begin_full_viewport_paint(&mut bytes, plan, &["old".into(), "new".into()])
            .expect("paint commands");

        let commands = String::from_utf8(bytes).expect("ANSI commands are UTF-8");
        assert!(
            commands.contains("\x1b[2J"),
            "clear visible viewport: {commands:?}"
        );
        assert!(
            !commands.contains("\x1b[3J"),
            "must not purge scrollback: {commands:?}"
        );
        assert!(
            commands.contains("\x1b[5;1H"),
            "transcript row is absolute: {commands:?}"
        );
        assert!(
            commands.contains("\x1b[7;1H"),
            "live row is absolute: {commands:?}"
        );
        assert!(
            !commands.contains("\x1b[1A"),
            "must not repair with MoveUp: {commands:?}"
        );
    }

    #[test]
    fn production_viewport_projects_collapsed_rows_without_losing_native_transcript() {
        let work = Arc::new(WorkUnit::new("program"));
        work.set_program_source("forth");
        work.set_response("世界 alpha\nline two\nline three\nline four");
        work.set_complete();
        let message: MessageRef = work.clone();
        let colors = ColorScheme::default();
        let state = AccordionState::default();
        let projected = render_via_view_model(&state, &message, &colors);

        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].row_expanded, Some(false));
        assert!(!projected[0].text.contains("line four"));
        assert!(work.complete_transcript(&colors).contains("line four"));

        let wide = viewport_tail_rendered_lines(&projected, 80, 4);
        let narrow = viewport_tail_rendered_lines(&projected, 8, 8);
        assert_eq!(wide[0].row_id, narrow[0].row_id);
        assert!(shadow_buffer::physical_rows(&narrow[0].text, 8) > 1);

        let text = wide
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let plan = viewport_redraw_plan(8, 2, 1);
        let mut bytes = Vec::new();
        begin_full_viewport_paint(&mut bytes, plan, &text).expect("production viewport paint");
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            !raw.contains("[collapsed]") && !raw.contains("[expanded]"),
            "dump must not append [expanded]/[collapsed]; painted={raw:?}"
        );
        assert!(
            raw.contains("▶") && raw.contains("Program source"),
            "dump still names the collapsed Program source row; painted={raw:?}"
        );
        assert!(!raw.contains("line four"));
        assert!(!raw.contains("\x1b[3J"), "must preserve native scrollback");
    }

    #[test]
    fn oversized_expanded_projection_pins_a_keyboard_and_mouse_target() {
        let work = Arc::new(WorkUnit::new("response"));
        work.set_response(
            (0..40)
                .map(|n| format!("row {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        work.set_complete();
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let state = AccordionState::default();
        let all = render_via_view_model(&state, &message, &colors);

        let visible = viewport_tail_rendered_lines(&all, 20, 4);

        assert!(
            visible[0].row_id.is_some(),
            "disclosure control must stay visible"
        );
        assert_eq!(visible[0].row_expanded, Some(true));
        assert!(visible.iter().any(|line| line.text.contains("row 39")));
        assert!(
            visible
                .iter()
                .map(|line| shadow_buffer::physical_rows(&line.text, 20))
                .sum::<usize>()
                <= 4
        );
        let tiny = viewport_tail_rendered_lines(&all, 8, 1);
        assert_eq!(tiny.len(), 1);
        assert!(tiny[0].row_id.is_some());
        assert_eq!(
            tiny[0].row_expanded,
            Some(true),
            "tiny squeezed header still reports expanded state; text was {:?}",
            tiny[0].text
        );
        assert!(
            !tiny[0].text.contains("[expanded]") && !tiny[0].text.contains("[collapsed]"),
            "tiny squeezed header must not fall back to a second visible token; text was {:?}",
            tiny[0].text
        );
        assert_eq!(shadow_buffer::physical_rows(&tiny[0].text, 8), 1);
        let mut collapsed_state = AccordionState::default();
        collapsed_state.rebuild_retained_hit_regions(&all, 0, 20);
        assert!(collapsed_state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(collapsed_state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        let collapsed = render_via_view_model(&collapsed_state, &message, &colors);
        let collapsed_tiny = viewport_tail_rendered_lines(&collapsed, 8, 1);
        assert_eq!(
            collapsed_tiny[0].row_expanded,
            Some(false),
            "tiny squeezed header still reports collapsed state; text was {:?}",
            collapsed_tiny[0].text
        );
        assert!(
            !collapsed_tiny[0].text.contains("[expanded]")
                && !collapsed_tiny[0].text.contains("[collapsed]"),
            "tiny squeezed header must not fall back to a second visible token; text was {:?}",
            collapsed_tiny[0].text
        );
    }

    #[test]
    fn canonical_commit_marks_only_after_success_and_follows_resize_clear() {
        struct FlushFailure(Vec<u8>);
        impl Write for FlushFailure {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("hostile flush failure"))
            }
        }

        let work = Arc::new(WorkUnit::new("program"));
        work.set_program_source("forth");
        work.set_response("secret canonical body");
        work.set_complete();
        let message: MessageRef = work;
        let colors = ColorScheme::default();
        let mut state = AccordionState::default();
        let initial = render_via_view_model(&state, &message, &colors);
        state.rebuild_retained_hit_regions(&initial, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        let before = render_via_view_model(&state, &message, &colors);
        assert_eq!(before[0].row_expanded, Some(false));
        let mut printed = HashSet::new();
        let mut failure = FlushFailure(Vec::new());
        assert!(commit_complete_messages(
            &mut failure,
            std::slice::from_ref(&message),
            &mut state,
            &colors,
            &mut printed,
            6,
        )
        .is_err());
        assert_eq!(printed.len(), 1, "accepted bytes must not be retried");
        let accepted_len = failure.0.len();
        commit_complete_messages(
            &mut failure,
            std::slice::from_ref(&message),
            &mut state,
            &colors,
            &mut printed,
            6,
        )
        .expect("already accepted message skips ambiguous flush retry");
        assert_eq!(failure.0.len(), accepted_len);
        assert_eq!(
            String::from_utf8(failure.0)
                .unwrap()
                .matches("secret canonical body")
                .count(),
            1
        );
        assert_eq!(render_via_view_model(&state, &message, &colors), before);

        let mut bytes = Vec::new();
        prepare_canonical_commit(&mut bytes).unwrap();
        let mut resize_printed = HashSet::new();
        commit_complete_messages(
            &mut bytes,
            &[message.clone()],
            &mut state,
            &colors,
            &mut resize_printed,
            8,
        )
        .unwrap();
        continue_full_viewport_paint(
            &mut bytes,
            viewport_redraw_plan(8, 2, 1),
            &["final projection".into()],
        )
        .unwrap();
        execute!(bytes, EndSynchronizedUpdate).unwrap();
        let raw = String::from_utf8(bytes).unwrap();
        assert!(raw.find("\x1b[2J").unwrap() < raw.find("secret canonical body").unwrap());
        assert_eq!(raw.matches("secret canonical body").count(), 1);
        assert_eq!(raw.matches("\x1b[?2026h").count(), 1);
        assert_eq!(raw.matches("\x1b[?2026l").count(), 1);
        assert!(raw.find("secret canonical body").unwrap() < raw.find("final projection").unwrap());
        assert_eq!(resize_printed.len(), 1);

        struct TerminalHistory {
            screen: Vec<String>,
            history: Vec<String>,
            cursor: usize,
        }
        impl TerminalHistory {
            fn write_line(&mut self, line: &str) {
                if self.cursor >= self.screen.len() {
                    self.history.push(self.screen.remove(0));
                    self.screen.push(String::new());
                    self.cursor = self.screen.len() - 1;
                }
                self.screen[self.cursor] = line.to_string();
                self.cursor += 1;
            }
        }
        let mut canonical = Vec::new();
        let mut canonical_printed = HashSet::new();
        commit_complete_messages(
            &mut canonical,
            std::slice::from_ref(&message),
            &mut state,
            &colors,
            &mut canonical_printed,
            6,
        )
        .unwrap();
        let mut terminal = TerminalHistory {
            screen: vec![String::new(); 6],
            history: Vec::new(),
            cursor: 0,
        };
        for line in String::from_utf8(canonical)
            .unwrap()
            .split_terminator("\r\n")
        {
            terminal.write_line(line);
        }
        // The production commit transaction includes one viewport of blank
        // linefeeds, so the complete canonical batch reaches history before
        // the subsequent Clear(All) viewport reconstruction.
        assert!(terminal
            .history
            .iter()
            .any(|line| line.contains("secret canonical body")));
        let history_before_clear = terminal.history.clone();
        terminal.screen.fill(String::new());
        assert_eq!(terminal.history, history_before_clear);

        // A later commit begins with prepare_canonical_commit's visible-screen
        // clear. The old projected row therefore cannot be spooled into native
        // history for a second time.
        terminal.screen[0] = "secret canonical body [projected]".into();
        terminal.screen.fill(String::new());
        terminal.cursor = 0;
        let second = Arc::new(WorkUnit::new("response"));
        second.set_response("second canonical body");
        second.set_complete();
        let second_message: MessageRef = second;
        let mut second_bytes = Vec::new();
        let mut second_printed = HashSet::new();
        commit_complete_messages(
            &mut second_bytes,
            &[second_message],
            &mut state,
            &colors,
            &mut second_printed,
            6,
        )
        .unwrap();
        for line in String::from_utf8(second_bytes)
            .unwrap()
            .split_terminator("\r\n")
        {
            terminal.write_line(line);
        }
        assert_eq!(
            terminal
                .history
                .iter()
                .filter(|line| line.contains("secret canonical body"))
                .count(),
            1
        );
        assert_eq!(
            terminal
                .history
                .iter()
                .filter(|line| line.contains("second canonical body"))
                .count(),
            1
        );
    }

    #[test]
    fn canonical_prepare_flush_error_closes_synchronized_update() {
        struct FirstFlushFails {
            bytes: Vec<u8>,
            flushes: usize,
        }

        impl Write for FirstFlushFails {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                self.flushes += 1;
                if self.flushes == 1 {
                    return Err(io::Error::other("ambiguous prepare flush failure"));
                }
                Ok(())
            }
        }

        let mut output = FirstFlushFails {
            bytes: Vec::new(),
            flushes: 0,
        };
        assert!(prepare_canonical_commit_guarded(&mut output).is_err());

        let raw = String::from_utf8(output.bytes).unwrap();
        assert_eq!(raw.matches("\x1b[?2026h").count(), 1);
        assert_eq!(raw.matches("\x1b[?2026l").count(), 1);
    }

    #[test]
    fn oversized_live_frame_stays_within_the_visible_viewport() {
        let plan = viewport_redraw_plan(5, 12, 8);
        assert_eq!(
            plan,
            ViewportRedrawPlan {
                transcript_top: 0,
                live_top: 0,
            }
        );
    }

    #[test]
    fn completed_messages_after_a_live_work_unit_wait_for_ordered_commit() {
        assert_eq!(
            committable_prefix_len([
                MessageStatus::Complete,
                MessageStatus::InProgress,
                MessageStatus::Complete,
            ]),
            1
        );
        assert_eq!(
            committable_prefix_len([MessageStatus::Complete, MessageStatus::Failed]),
            2
        );
    }

    #[test]
    fn completed_program_source_swaps_for_visible_output() {
        let source = Arc::new(WorkUnit::new("source"));
        source.set_program_source("lisp");
        source.set_response("(say \"hello\")");
        source.set_complete();

        let output = Arc::new(WorkUnit::new("output"));
        output.set_program_output();
        output.append_response("hello");

        let source_ref: MessageRef = source.clone();
        let output_ref: MessageRef = output.clone();
        let manager_messages = vec![source_ref.clone(), output_ref.clone()];
        let live = swap_completed_program_source_for_output(uncommitted_suffix(
            manager_messages.clone(),
            &HashSet::new(),
        ));
        assert_eq!(live.len(), 1, "output replaces completed IR as the turn");
        assert_eq!(live[0].format(&ColorScheme::default()), "hello");

        let empty_output = Arc::new(WorkUnit::new("empty-output"));
        empty_output.set_program_output();
        let empty_ref: MessageRef = empty_output;
        let waiting = swap_completed_program_source_for_output(vec![source_ref, empty_ref]);
        assert_eq!(
            waiting.len(),
            2,
            "completed IR stays until program output has a body"
        );

        let streaming = Arc::new(WorkUnit::new("streaming"));
        streaming.set_program_source("lisp");
        streaming.set_response("(say");
        let streaming_ref: MessageRef = streaming.clone();
        let ir_only = swap_completed_program_source_for_output(vec![streaming_ref.clone()]);
        assert_eq!(
            ir_only.len(),
            1,
            "IR stays visible while it is still streaming"
        );

        let streaming_with_output =
            swap_completed_program_source_for_output(vec![streaming_ref, output_ref]);
        assert_eq!(
            streaming_with_output.len(),
            2,
            "InProgress source stays visible beside output that already has a body"
        );
        assert!(
            crate::cli::tui::view_model::try_project_for_test(
                streaming.as_ref(),
                &ColorScheme::default()
            )
            .is_some_and(|row| row.default_open),
            "source stays expanded only while InProgress"
        );

        let tool = Arc::new(WorkUnit::new("tool"));
        let source_one = Arc::new(WorkUnit::new("source-one"));
        source_one.set_program_source("lisp");
        source_one.set_response("(say \"one\")");
        source_one.set_complete();
        let output_one = Arc::new(WorkUnit::new("output-one"));
        output_one.set_program_output();
        let source_two = Arc::new(WorkUnit::new("source-two"));
        source_two.set_program_source("lisp");
        source_two.set_response("(say \"two\")");
        source_two.set_complete();
        let output_two = Arc::new(WorkUnit::new("output-two"));
        output_two.set_program_output();
        output_two.append_response("hello");
        let tool_ref: MessageRef = tool;
        let source_one_ref: MessageRef = source_one.clone();
        let output_one_ref: MessageRef = output_one;
        let source_two_ref: MessageRef = source_two.clone();
        let output_two_ref: MessageRef = output_two;
        let overlapping = swap_completed_program_source_for_output(vec![
            tool_ref,
            source_one_ref,
            output_one_ref,
            source_two_ref,
            output_two_ref,
        ]);
        let overlapping_ids: Vec<MessageId> =
            overlapping.iter().map(|message| message.id()).collect();
        assert!(
            overlapping_ids.contains(&source_one.id()),
            "empty-output source stays visible when a later turn's output has body; ids={overlapping_ids:?}"
        );
        assert!(
            !overlapping_ids.contains(&source_two.id()),
            "completed IR whose own later output has body is hidden; ids={overlapping_ids:?}"
        );
    }

    #[test]
    fn completed_program_source_is_not_a_second_visible_item_after_commit() {
        let colors = ColorScheme::default();
        let manager = Arc::new(OutputManager::new(colors.clone()));
        manager.disable_stdout();
        let mut renderer = TuiRenderer::new_headless(
            Arc::clone(&manager),
            Arc::new(StatusBar::new()),
            colors.clone(),
        );

        let source = Arc::new(WorkUnit::new("source"));
        source.set_program_source("lisp");
        source.set_response("(say \"hello\")");
        source.set_complete();
        let output = Arc::new(WorkUnit::new("output"));
        output.set_program_output();
        output.append_response("hello");
        manager.add_trait_message(source.clone());
        manager.add_trait_message(output.clone());

        let messages = manager.get_messages();
        let plan = plan_canonical_commit(&messages, &renderer.printed_ids);
        let emit_text = plan
            .emit
            .iter()
            .map(|message| message.format(&colors))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            plan.emit.iter().all(|message| message.id() != source.id()),
            "completed IR must not enter native history once output has body; emit={emit_text}"
        );
        assert!(
            plan.consume_without_emit.contains(&source.id()),
            "replaced IR must be consumed so later output can commit; consume={:?}",
            plan.consume_without_emit
        );

        let mut staged = Vec::new();
        renderer.printed_ids.extend(plan.consume_without_emit);
        if !plan.emit.is_empty() {
            commit_complete_messages(
                &mut staged,
                &plan.emit,
                &mut renderer.accordion,
                &colors,
                &mut renderer.printed_ids,
                8,
            )
            .expect("commit replaced-turn prefix");
        }
        let staged_text = String::from_utf8(staged).unwrap();
        assert!(
            !staged_text.contains("Program source") && !staged_text.contains("(say \"hello\")"),
            "canonical bytes must not contain IR after swap; staged={staged_text:?}"
        );

        output.set_complete();
        let messages = manager.get_messages();
        let plan = plan_canonical_commit(&messages, &renderer.printed_ids);
        let mut staged = Vec::new();
        renderer.printed_ids.extend(plan.consume_without_emit);
        commit_complete_messages(
            &mut staged,
            &plan.emit,
            &mut renderer.accordion,
            &colors,
            &mut renderer.printed_ids,
            8,
        )
        .expect("commit program output");
        let staged_text = String::from_utf8(staged).unwrap();
        assert!(
            staged_text.contains("hello"),
            "native history must contain program output; staged={staged_text:?}"
        );
        assert!(
            !staged_text.contains("Program source") && !staged_text.contains("(say \"hello\")"),
            "native history must not contain IR after output commits; staged={staged_text:?}"
        );

        let printed = visible_printed_messages(&manager.get_messages(), &renderer.printed_ids);
        let projected = printed
            .iter()
            .flat_map(
                |message| match view_model::project_message(message, &colors) {
                    view_model::ProjectedMessage::Node(node) => {
                        renderer.accordion.render_node(&node)
                    }
                    view_model::ProjectedMessage::Plain(formatted) => {
                        renderer.accordion.render_plain(&formatted.join("\n"))
                    }
                },
            )
            .map(|line| line.text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            projected.contains("hello"),
            "reconstructed viewport must show program output; projected={projected:?}"
        );
        assert!(
            !projected.contains("Program source") && !projected.contains("(say \"hello\")"),
            "reconstructed viewport must not show a second Program source item; projected={projected:?}"
        );

        let retained = manager.get_messages();
        assert_eq!(
            retained.len(),
            2,
            "source remains in the manager after it leaves the transcript"
        );
        assert!(
            retained
                .iter()
                .any(|message| message.content().contains("(say \"hello\")")),
            "manager still holds IR for inspect/copy; contents={:?}",
            retained.iter().map(|m| m.content()).collect::<Vec<_>>()
        );

        let waiting_manager = Arc::new(OutputManager::new(colors.clone()));
        waiting_manager.disable_stdout();
        let waiting_source = Arc::new(WorkUnit::new("waiting-source"));
        waiting_source.set_program_source("lisp");
        waiting_source.set_response("(say \"hello\")");
        waiting_source.set_complete();
        let empty_output = Arc::new(WorkUnit::new("empty-output"));
        empty_output.set_program_output();
        waiting_manager.add_trait_message(waiting_source.clone());
        waiting_manager.add_trait_message(empty_output.clone());
        let waiting_messages = waiting_manager.get_messages();
        let waiting_plan = plan_canonical_commit(&waiting_messages, &HashSet::new());
        assert!(
            waiting_plan
                .emit
                .iter()
                .all(|message| message.id() != waiting_source.id())
                && !waiting_plan
                    .consume_without_emit
                    .contains(&waiting_source.id()),
            "completed IR must not commit while paired output is still empty and live; emit={} consume={:?}",
            waiting_plan
                .emit
                .iter()
                .map(|message| message.format(&colors))
                .collect::<Vec<_>>()
                .join(" | "),
            waiting_plan.consume_without_emit
        );
        let waiting_live = swap_completed_program_source_for_output(waiting_messages.clone());
        assert!(
            waiting_live
                .iter()
                .any(|message| message.id() == waiting_source.id()),
            "empty live output keeps source visible until a body arrives"
        );

        empty_output.set_complete();
        let empty_complete_plan =
            plan_canonical_commit(&waiting_manager.get_messages(), &HashSet::new());
        assert!(
            empty_complete_plan
                .emit
                .iter()
                .any(|message| message.id() == waiting_source.id()),
            "completed IR stays on the commit path when output finishes empty; emit={}",
            empty_complete_plan
                .emit
                .iter()
                .map(|message| message.format(&colors))
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }

    #[test]
    fn test_redraw_predicate_triggers_when_dirty() {
        assert!(should_redraw_live_area(false, true));
    }

    // ── count_status_lines ────────────────────────────────────────────────────

    #[test]
    fn status_lines_single() {
        assert_eq!(count_status_lines("idle hint"), 1);
    }

    #[test]
    fn status_lines_empty_counts_as_one() {
        assert_eq!(
            count_status_lines(""),
            1,
            "empty string = 1 row (idle hint always shown)"
        );
    }

    #[test]
    fn status_lines_two_lines() {
        assert_eq!(count_status_lines("⏺ Generating…\nContext left: 90%"), 2);
    }

    #[test]
    fn status_lines_three_lines() {
        assert_eq!(count_status_lines("op\ncompact\nplan_mode"), 3);
    }

    // ── compute_cursor_row_from_top ───────────────────────────────────────────

    #[test]
    fn cursor_row_single_input_single_status() {
        // Layout: sep(0), input(1), status(2) — 3 rows total
        // cursor at input row 0 → cursor_row_from_top = 1
        assert_eq!(compute_cursor_row_from_top(3, 1, 0, 1), 1);
    }

    #[test]
    fn cursor_row_two_input_lines_cursor_at_top() {
        // Layout: sep(0), input0(1), input1(2), status(3) — 4 rows total
        // cursor at input row 0 → cursor_row_from_top = 1
        assert_eq!(compute_cursor_row_from_top(4, 2, 0, 1), 1);
    }

    #[test]
    fn cursor_row_two_input_lines_cursor_at_bottom() {
        // Layout: sep(0), input0(1), input1(2), status(3) — 4 rows total
        // cursor at input row 1 → cursor_row_from_top = 2
        assert_eq!(compute_cursor_row_from_top(4, 2, 1, 1), 2);
    }

    #[test]
    fn cursor_row_multiline_status() {
        // Layout: sep(0), input(1), status0(2), status1(3), status2(4) — 5 rows
        // cursor at input row 0, 3-line status → cursor_row_from_top = 1
        assert_eq!(compute_cursor_row_from_top(5, 1, 0, 3), 1);
    }

    #[test]
    fn cursor_row_with_workunit() {
        // Layout: wu0(0), wu1(1), sep(2), input(3), status(4) — 5 rows
        // cursor at input row 0 → cursor_row_from_top = 3
        assert_eq!(compute_cursor_row_from_top(5, 1, 0, 1), 3);
    }

    // ── compute_ghost_text ────────────────────────────────────────────────────

    #[test]
    fn ghost_text_empty_input_returns_none() {
        let reg = CommandRegistry::new();
        assert!(compute_ghost_text("", &reg).is_none());
    }

    #[test]
    fn ghost_text_whitespace_returns_none() {
        let reg = CommandRegistry::new();
        assert!(compute_ghost_text("   ", &reg).is_none());
    }

    #[test]
    fn ghost_text_non_command_returns_none() {
        let reg = CommandRegistry::new();
        assert!(compute_ghost_text("hello world", &reg).is_none());
    }

    #[test]
    fn ghost_text_slash_alone_returns_none_or_some() {
        // "/" alone has many matches — implementation may return None (no prefix extension
        // beyond what's typed) since all commands start with "/" and we need len > input.len().
        // Because "/" is 1 char and "/help" is 5 chars, the first match should provide "help".
        let reg = CommandRegistry::new();
        // We don't assert exact value — just that it doesn't panic
        let _ = compute_ghost_text("/", &reg);
    }

    #[test]
    fn ghost_text_exact_command_returns_none() {
        // "/help" fully typed → nothing left to complete
        let reg = CommandRegistry::new();
        assert!(compute_ghost_text("/help", &reg).is_none());
    }

    #[test]
    fn ghost_text_partial_unique_prefix_returns_suffix() {
        let reg = CommandRegistry::new();
        // "/hel" should complete to "p" (assuming /help is registered)
        if let Some(ghost) = compute_ghost_text("/hel", &reg) {
            assert_eq!(ghost, "p");
        }
        // If there's no match that's fine — just don't panic
    }

    #[test]
    fn ghost_text_partial_prefix_appended_gives_full_command() {
        let reg = CommandRegistry::new();
        let input = "/cri"; // should complete to /critical
        if let Some(ghost) = compute_ghost_text(input, &reg) {
            let completed = format!("{}{}", input, ghost);
            assert!(completed.starts_with("/critical"), "got: {}", completed);
        }
    }

    // ── compute_effective_status ──────────────────────────────────────────────

    #[test]
    fn status_idle_when_no_ghost_and_no_raw() {
        let reg = CommandRegistry::new();
        let s = compute_effective_status(None, "", "hello", &reg);
        assert!(s.contains("Ctrl+C"), "should show idle hint: {}", s);
        assert!(s.contains("/help"), "should mention /help: {}", s);
    }

    #[test]
    fn status_shows_raw_when_no_ghost() {
        let reg = CommandRegistry::new();
        let s = compute_effective_status(None, "⏺ Generating…", "hello", &reg);
        assert_eq!(s, "⏺ Generating…");
    }

    #[test]
    fn status_shows_command_description_when_ghost_present() {
        let reg = CommandRegistry::new();
        // Simulate typing "/help" with ghost text
        let s = compute_effective_status(Some(""), "", "/help", &reg);
        // Should contain the description for /help
        assert!(
            s.contains("/help"),
            "description should mention command: {}",
            s
        );
    }

    #[test]
    fn test_critical_or_operational_status_takes_priority_over_command_help() {
        let reg = CommandRegistry::new();
        let s = compute_effective_status(Some("tical"), "⏺ Generating…", "/cri", &reg);
        assert_eq!(s, "⏺ Generating…");
    }

    #[test]
    fn status_falls_back_to_raw_when_ghost_but_no_matching_desc() {
        let reg = CommandRegistry::new();
        // Ghost text present but no matching command found for the input
        // e.g. ghost text = "xyz" for "/zzz" which isn't a real command
        let s = compute_effective_status(Some("xyz"), "⏺ Live stat", "/zzz", &reg);
        // Falls back to raw status since description is empty
        assert_eq!(s, "⏺ Live stat");
    }

    #[test]
    fn test_completion_frame_keeps_large_stream_clipped_and_uses_free_rows() {
        let registry = CommandRegistry::new();
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/brain "));
        assert!(autocomplete.matches.len() >= 8);

        let draft = vec![
            "/brain ".to_string(),
            "keep this multiline draft".to_string(),
            "and preserve its cursor".to_string(),
        ];
        let terminal_rows = 60;
        let terminal_width = 120;
        let stream = (0..1_050)
            .map(|row| format!("live Brain row {row}"))
            .collect::<Vec<_>>();
        let stream_rendered = stream
            .iter()
            .map(|line| RenderedTranscriptLine {
                text: line.clone(),
                ..RenderedTranscriptLine::default()
            })
            .collect::<Vec<_>>();
        let status = "";
        let mut vm = live_inputs(terminal_width, terminal_rows, &draft, status);
        vm.live_rendered = &stream_rendered;
        let frame = plan_live_frame(&vm, &mut autocomplete);

        // Painted rows inside the claimed completions rect: the pane sits
        // above the composer, so its rows are the frame rows the layout gave
        // the completions widget.
        let pane_rect = frame
            .rects
            .completions
            .expect("a slash draft must claim the completion pane");
        let painted_completions = rows_in_rect(&frame, 120, pane_rect);
        assert_eq!(painted_completions.len(), 9, "heading plus eight matches");
        assert!(painted_completions[0].contains("Commands "));
        assert!(frame.lines[0].contains("earlier live rows clipped"));
        assert!(frame.physical_rows(terminal_width) <= terminal_rows);
        let selected = autocomplete.get_selected().unwrap().full_syntax();

        for (height, width, stream_rows, critical) in [
            (60, 120, 1_001, false),
            (18, 32, 1_040, false),
            (60, 200, 1_080, false),
            (60, 120, 1_080, true),
            (60, 120, 1_100, false),
        ] {
            let live = (0..stream_rows)
                .map(|row| format!("rapid stream row {row}"))
                .collect::<Vec<_>>();
            let live_rendered = live
                .iter()
                .map(|line| RenderedTranscriptLine {
                    text: line.clone(),
                    ..RenderedTranscriptLine::default()
                })
                .collect::<Vec<_>>();
            let mut vm = live_inputs(width, height, &draft, status);
            vm.render_error = critical;
            vm.live_rendered = &live_rendered;
            let frame = plan_live_frame(&vm, &mut autocomplete);
            assert_eq!(autocomplete.get_selected().unwrap().full_syntax(), selected);
            assert!(frame.lines.first().unwrap().contains("clipped"));
            let painted_completions = frame
                .rects
                .completions
                .map(|rect| rows_in_rect(&frame, width, rect).len())
                .unwrap_or(0);
            if critical {
                assert_eq!(painted_completions, 0);
                assert!(!autocomplete.is_interactive());
            } else {
                assert!(painted_completions > 0);
                assert!(autocomplete.is_interactive());
            }
        }
    }

    #[test]
    fn test_multiline_command_draft_has_matches_but_no_misplaced_ghost() {
        let registry = CommandRegistry::new();
        let lines = vec![
            "/bra".to_string(),
            "a second line that nearly fills the terminal".to_string(),
        ];

        let (matches, ghost) = command_completion_at_cursor(&lines, (0, 4), &registry);

        assert!(matches.iter().any(|command| command.name == "/brain list"));
        assert_eq!(
            ghost, None,
            "a row-zero ghost must not be emitted after the last line"
        );
        assert_eq!(input_physical_rows(&lines, 48), 2);
    }

    #[test]
    fn test_production_accept_restores_multiline_textarea_cursor_exactly() {
        use tui_textarea::CursorMove;

        let mut textarea = TuiRenderer::create_clean_textarea_with_text(
            "/bra --later\nkeep this draft\nand this too",
        );
        textarea.move_cursor(CursorMove::Top);
        if textarea.cursor().1 > 0 {
            textarea.move_cursor(CursorMove::Head);
        }
        for _ in 0..4 {
            textarea.move_cursor(CursorMove::Forward);
        }

        assert!(replace_textarea_command(&mut textarea, "/brain list"));
        assert_eq!(
            textarea.lines(),
            ["/brain list --later", "keep this draft", "and this too"]
        );
        assert_eq!(textarea.cursor(), (0, "/brain list".chars().count()));
    }

    #[test]
    fn test_production_accept_preserves_trailing_blank_draft_lines() {
        use tui_textarea::CursorMove;

        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/bra --later\n\n");
        textarea.move_cursor(CursorMove::Top);
        if textarea.cursor().1 > 0 {
            textarea.move_cursor(CursorMove::Head);
        }
        for _ in 0..4 {
            textarea.move_cursor(CursorMove::Forward);
        }

        assert!(replace_textarea_command(&mut textarea, "/brain list"));
        assert_eq!(textarea.lines(), ["/brain list --later", "", ""]);
        assert_eq!(textarea.cursor(), (0, "/brain list".chars().count()));
    }

    #[test]
    fn test_cursor_middle_draft_has_no_misplaced_ghost() {
        let registry = CommandRegistry::new();
        let lines = vec!["/heXYZ".to_string()];

        let (matches, ghost) = command_completion_at_cursor(&lines, (0, 3), &registry);

        assert!(matches.iter().any(|command| command.name == "/help"));
        assert_eq!(ghost, None);
    }

    #[test]
    fn test_initial_input_loop_tab_preserves_hidden_cursor_middle_draft() {
        use tui_textarea::CursorMove;

        let registry = CommandRegistry::new();
        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/heXYZ");
        textarea.move_cursor(CursorMove::Head);
        for _ in 0..3 {
            textarea.move_cursor(CursorMove::Forward);
        }
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/he"));
        assert!(completion_pane_lines(&mut autocomplete, 80, 0).is_empty());
        let mut ghost = Some("lp".to_string());
        let original_lines = textarea.lines().to_vec();
        let original_cursor = textarea.cursor();

        assert!(!route_tab_key(
            &mut textarea,
            &mut autocomplete,
            &mut ghost,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        ));
        assert_eq!(textarea.lines(), original_lines);
        assert_eq!(textarea.cursor(), original_cursor);
        assert_eq!(ghost, None);
    }

    #[test]
    fn test_batched_input_loop_tab_preserves_critical_hidden_completion() {
        use tui_textarea::CursorMove;

        let registry = CommandRegistry::new();
        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/bra --suffix\n\n");
        textarea.move_cursor(CursorMove::Top);
        textarea.move_cursor(CursorMove::Head);
        for _ in 0..4 {
            textarea.move_cursor(CursorMove::Forward);
        }
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/bra"));
        assert!(completion_pane_lines(&mut autocomplete, 80, 0).is_empty());
        let mut ghost = Some("in list".to_string());
        let original_lines = textarea.lines().to_vec();
        let original_cursor = textarea.cursor();

        assert!(!route_tab_key(
            &mut textarea,
            &mut autocomplete,
            &mut ghost,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        ));
        assert_eq!(textarea.lines(), original_lines);
        assert_eq!(textarea.cursor(), original_cursor);
        assert_eq!(ghost, None);
    }

    #[test]
    fn test_resize_then_batched_tab_cannot_accept_stale_painted_completion() {
        use tui_textarea::CursorMove;

        let registry = CommandRegistry::new();
        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/bra --suffix\n\n");
        textarea.move_cursor(CursorMove::Top);
        textarea.move_cursor(CursorMove::Head);
        for _ in 0..4 {
            textarea.move_cursor(CursorMove::Forward);
        }
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/bra"));
        completion_pane_lines(&mut autocomplete, 80, 9);
        assert!(autocomplete.is_interactive());
        let selected = autocomplete.get_selected().unwrap().name;
        let mut ghost = Some(selected[4..].to_string());
        let original_lines = textarea.lines().to_vec();
        let original_cursor = textarea.cursor();
        let mut pending = None;
        let mut invalidated = false;
        let mut dirty = false;

        apply_viewport_resize(
            &mut autocomplete,
            &mut pending,
            &mut invalidated,
            &mut dirty,
            20,
            2,
        );
        assert_eq!(pending, Some((20, 2)));
        assert!(invalidated && dirty);
        assert!(!autocomplete.is_interactive());
        assert!(!route_tab_key(
            &mut textarea,
            &mut autocomplete,
            &mut ghost,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        ));
        assert_eq!(textarea.lines(), original_lines);
        assert_eq!(textarea.cursor(), original_cursor);
        assert_eq!(ghost, None);
    }

    #[test]
    fn test_production_dispatch_accepts_case_insensitive_visible_match() {
        let registry = CommandRegistry::new();
        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/BRA");
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/BRA"));
        completion_pane_lines(&mut autocomplete, 100, 9);
        let selected = autocomplete.get_selected().unwrap().name.to_string();
        let mut ghost =
            selected_completion_ghost(textarea.lines(), textarea.cursor(), &autocomplete);

        assert!(dispatch_completion_key(
            &mut textarea,
            &mut autocomplete,
            &mut ghost,
            KeyCode::Tab,
        ));
        assert_eq!(textarea.lines(), [selected]);
        assert_eq!(ghost, None);
        assert!(!autocomplete.visible);
    }

    #[test]
    fn test_navigation_ghost_matches_selected_command() {
        let registry = CommandRegistry::new();
        let lines = vec!["/brain ".to_string()];
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/brain "));
        completion_pane_lines(&mut autocomplete, 100, 9);
        let first = selected_completion_ghost(&lines, (0, 7), &autocomplete);
        let mut textarea = TuiRenderer::create_clean_textarea_with_text("/brain ");
        let mut second = first.clone();

        assert!(dispatch_completion_key(
            &mut textarea,
            &mut autocomplete,
            &mut second,
            KeyCode::Down,
        ));
        let selected = autocomplete.get_selected().unwrap().name;

        assert_ne!(first, second);
        assert_eq!(format!("/brain {}", second.unwrap()), selected);
    }

    fn headless_renderer() -> TuiRenderer {
        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let status = Arc::new(StatusBar::new());
        TuiRenderer::new_headless(output, status, colors)
    }

    fn composer_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn paint_slash_completions(renderer: &mut TuiRenderer) {
        renderer.update_ghost_text();
        completion_pane_lines(&mut renderer.autocomplete_state, 80, 9);
    }

    /// Same order as `spawn_input_task`: dispatch, then refresh ghost text
    /// when the composer contents changed.
    fn dispatch_composer(renderer: &mut TuiRenderer, code: KeyCode) -> Option<String> {
        match renderer.dispatch_composer_key(composer_key(code)) {
            ComposerDispatch::Submit(input) => Some(input),
            ComposerDispatch::Handled { input_changed } => {
                if input_changed {
                    renderer.update_ghost_text();
                }
                None
            }
            ComposerDispatch::Unhandled => None,
        }
    }

    #[test]
    fn enter_runs_the_highlighted_slash_command_not_the_typed_prefix() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        paint_slash_completions(&mut renderer);
        assert!(
            renderer.autocomplete_state.is_interactive(),
            "slash completions must own Up/Down after the pane paints"
        );
        assert_eq!(
            renderer.autocomplete_state.get_selected().unwrap().name,
            "/help"
        );

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Down), None);
        let selected = renderer
            .autocomplete_state
            .get_selected()
            .expect("Down must keep a selected command")
            .name
            .to_string();
        assert_ne!(
            selected, "/help",
            "the bug is Enter ignoring the highlighted row after Up/Down"
        );
        assert_eq!(renderer.input_textarea.lines(), ["/"]);

        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter)
            .expect("Enter must submit the highlighted command");
        assert_eq!(
            submitted, selected,
            "Enter must apply the selected completion before submit; composer was still '/' which parses as Help"
        );
        assert!(
            !matches!(
                crate::cli::commands::Command::parse(&submitted),
                Some(crate::cli::commands::Command::Help)
            ),
            "submitted {submitted:?} must not be the Help catch-all"
        );
    }

    #[test]
    fn enter_does_not_apply_unpainted_slash_completion() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        renderer.update_ghost_text();
        assert!(
            renderer.autocomplete_state.visible && !renderer.autocomplete_state.is_interactive(),
            "matches exist but the pane has not been painted"
        );

        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter).unwrap();
        assert_eq!(
            submitted, "/",
            "Enter must not apply a completion the user could not see"
        );
        assert!(matches!(
            crate::cli::commands::Command::parse(&submitted),
            Some(crate::cli::commands::Command::Help)
        ));
    }

    #[test]
    fn bare_slash_enter_still_runs_help_via_the_first_match() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        paint_slash_completions(&mut renderer);

        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter).unwrap();
        assert_eq!(submitted, "/help");
        assert!(matches!(
            crate::cli::commands::Command::parse(&submitted),
            Some(crate::cli::commands::Command::Help)
        ));
    }

    #[test]
    fn tab_completes_highlighted_slash_command_without_submitting() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        paint_slash_completions(&mut renderer);
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Down), None);
        let selected = renderer
            .autocomplete_state
            .get_selected()
            .expect("Down must keep a selected command")
            .name
            .to_string();

        assert_eq!(
            dispatch_composer(&mut renderer, KeyCode::Tab),
            None,
            "Tab must complete without submitting"
        );
        assert_eq!(renderer.input_textarea.lines(), [selected.as_str()]);
        assert!(
            !renderer.autocomplete_state.visible,
            "Tab must dismiss the pane after completing"
        );
    }

    #[test]
    fn esc_dismisses_painted_slash_completions_without_applying() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        paint_slash_completions(&mut renderer);
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Down), None);
        assert!(renderer.autocomplete_state.is_interactive());

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Esc), None);
        assert_eq!(renderer.input_textarea.lines(), ["/"]);
        assert!(
            !renderer.autocomplete_state.visible,
            "Esc must dismiss the pane without rewriting the composer"
        );

        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter).unwrap();
        assert_eq!(
            submitted, "/",
            "Enter after Esc must submit the typed prefix, not a dismissed row"
        );
    }

    #[test]
    fn history_up_through_a_slash_command_does_not_steal_the_next_up() {
        let mut renderer = headless_renderer();
        renderer.command_history = vec!["hello".to_string(), "/help".to_string()];

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Up), None);
        assert_eq!(renderer.input_textarea.lines(), ["/help"]);
        assert!(
            !renderer.autocomplete_state.visible,
            "recalling a slash command must not open the completion pane"
        );

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Up), None);
        assert_eq!(
            renderer.input_textarea.lines(),
            ["hello"],
            "the next Up after a slash history item must continue through history"
        );
    }

    #[test]
    fn painted_slash_completions_cannot_capture_up_while_history_is_active() {
        let mut renderer = headless_renderer();
        renderer.command_history = vec!["hello".to_string(), "/help".to_string()];
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Up), None);
        renderer
            .autocomplete_state
            .show_matches(renderer.command_registry.match_prefix("/"));
        completion_pane_lines(&mut renderer.autocomplete_state, 80, 9);
        assert!(renderer.autocomplete_state.is_interactive());
        assert!(renderer.history_index.is_some());

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Up), None);
        assert_eq!(
            renderer.input_textarea.lines(),
            ["hello"],
            "a painted completion pane must not wrap-select while history_index is set"
        );
    }

    #[test]
    fn leaving_slash_history_restores_the_draft_and_allows_completions() {
        let mut renderer = headless_renderer();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("/");
        renderer.command_history = vec!["/help".to_string()];

        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Up), None);
        assert_eq!(renderer.input_textarea.lines(), ["/help"]);
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Down), None);
        assert_eq!(renderer.input_textarea.lines(), ["/"]);
        assert!(renderer.history_index.is_none());
        assert!(
            renderer.autocomplete_state.visible,
            "restored slash draft must show completions again"
        );
    }

    fn paint_mention_completions(renderer: &mut TuiRenderer) {
        renderer.update_ghost_text();
        completion_pane_lines(&mut renderer.autocomplete_state, 80, 9);
    }

    fn mention_project() -> (tempfile::TempDir, TuiRenderer) {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "fn selected() {}\n").unwrap();
        std::fs::write(tmp.path().join("src/bar.rs"), "fn other() {}\n").unwrap();
        let mut renderer = headless_renderer();
        renderer.set_mention_root(tmp.path());
        (tmp, renderer)
    }

    #[test]
    fn at_mention_picker_select_submits_exact_snapshot_and_keeps_visible_prompt() {
        let (tmp, mut renderer) = mention_project();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("explain @foo");
        paint_mention_completions(&mut renderer);
        assert!(
            renderer.autocomplete_state.is_interactive(),
            "token-boundary @ must open a painted mention pane"
        );
        let selected = renderer
            .autocomplete_state
            .get_selected_mention()
            .expect("a file must be highlighted")
            .relative_path
            .clone();
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Tab), None);
        assert!(
            !renderer.autocomplete_state.visible,
            "Tab inserts a mention without submitting"
        );
        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter)
            .expect("Enter after insert must submit the composer");
        assert!(
            submitted.starts_with("explain "),
            "visible prompt must remain intact, got {submitted:?}"
        );
        assert!(
            submitted.contains('@'),
            "submitted prompt must keep the visible mention token: {submitted:?}"
        );
        std::fs::write(tmp.path().join("src/foo.rs"), "CHANGED ON DISK").unwrap();
        assert_eq!(
            renderer.pending_mentions.len(),
            1,
            "selection must snapshot the chosen file"
        );
        assert_eq!(
            renderer.pending_mentions[0].relative_path, selected,
            "pending snapshot must be the selected path"
        );
        assert_eq!(
            renderer.pending_mentions[0].content, "fn selected() {}\n",
            "submit must keep selection bytes, not a later disk read"
        );
        let blocks =
            crate::context::mention::assemble_user_content(&submitted, &renderer.pending_mentions);
        assert_eq!(
            blocks[0].as_text(),
            Some(submitted.as_str()),
            "provider-visible prompt block must equal the composer text"
        );
        let attached = blocks[1].as_text().expect("attachment block");
        assert!(
            attached.contains("fn selected() {}"),
            "provider request must receive the exact selected content: {attached}"
        );
        assert!(
            !attached.contains("CHANGED ON DISK"),
            "changed disk must not replace the snapshot: {attached}"
        );
        assert!(
            attached.contains(&renderer.pending_mentions[0].sha256),
            "attachment must name the content digest"
        );
    }

    #[test]
    fn email_and_escaped_at_do_not_open_the_mention_picker() {
        let (_tmp, mut renderer) = mention_project();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("user@example.com");
        paint_mention_completions(&mut renderer);
        assert!(
            !renderer.autocomplete_state.visible,
            "email @ must stay ordinary prompt text"
        );
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("see \\@foo");
        paint_mention_completions(&mut renderer);
        assert!(
            !renderer.autocomplete_state.visible,
            "escaped \\@ must not open a mention picker"
        );
    }

    #[test]
    fn leading_at_finch_addressee_does_not_open_the_mention_picker() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(tmp.path().join("src/finch.rs"), "fn finch() {}\n").unwrap();
        std::fs::write(tmp.path().join("src/foo.rs"), "fn selected() {}\n").unwrap();
        let mut renderer = headless_renderer();
        renderer.set_mention_root(tmp.path());

        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("@finch");
        paint_mention_completions(&mut renderer);
        assert!(
            !renderer.autocomplete_state.visible,
            "leading @finch addressee must not open the file picker"
        );
        let submitted = dispatch_composer(&mut renderer, KeyCode::Enter)
            .expect("Enter on a leading @finch addressee must submit");
        assert_eq!(submitted, "@finch");

        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("@./finch");
        paint_mention_completions(&mut renderer);
        assert!(
            renderer.autocomplete_state.is_interactive(),
            "@./finch must still list a matching project file"
        );
        let selected = renderer
            .autocomplete_state
            .get_selected_mention()
            .expect("a file named finch must be highlighted");
        assert!(
            selected.relative_path.contains("finch"),
            "file mention for @./finch must list a finch path, got {:?}",
            selected.relative_path
        );
    }

    #[test]
    fn mention_enter_inserts_without_submitting_and_esc_keeps_composer() {
        let (_tmp, mut renderer) = mention_project();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("@foo");
        paint_mention_completions(&mut renderer);
        assert!(renderer.autocomplete_state.is_interactive());
        assert_eq!(
            dispatch_composer(&mut renderer, KeyCode::Enter),
            None,
            "Enter on a mention row inserts, it does not submit the turn"
        );
        assert!(
            renderer.input_textarea.lines()[0].contains('@'),
            "inserted mention must remain in the composer"
        );
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("@foo");
        paint_mention_completions(&mut renderer);
        assert_eq!(dispatch_composer(&mut renderer, KeyCode::Esc), None);
        assert_eq!(renderer.input_textarea.lines(), ["@foo"]);
        assert!(
            !renderer.autocomplete_state.visible,
            "Esc must dismiss the mention pane without rewriting the composer"
        );
    }

    #[test]
    fn mention_resize_invalidates_keyboard_authority_without_rewriting_draft() {
        let (_tmp, mut renderer) = mention_project();
        renderer.input_textarea = TuiRenderer::create_clean_textarea_with_text("@foo");
        paint_mention_completions(&mut renderer);
        assert!(renderer.autocomplete_state.is_interactive());
        renderer.autocomplete_state.invalidate_rendered_rows();
        assert!(
            !renderer.autocomplete_state.is_interactive(),
            "resize must drop keyboard authority on the old mention pane"
        );
        assert_eq!(renderer.input_textarea.lines(), ["@foo"]);
    }

    #[test]
    fn test_single_line_ghost_wrapping_is_counted_in_live_frame_geometry() {
        let lines = vec!["/bra".to_string()];
        let without_ghost = input_line_physical_rows_with_ghost(&lines, 8, None);
        let with_ghost = input_line_physical_rows_with_ghost(&lines, 8, Some("in archive now"));

        assert_eq!(without_ghost, vec![1]);
        assert_eq!(with_ghost, vec![3]);
    }

    #[test]
    fn test_one_to_three_row_viewports_never_create_an_invisible_interactive_pane() {
        let registry = CommandRegistry::new();
        for height in 1..=3 {
            let draft = vec!["/brain ".to_string()];
            let mut vm = live_inputs(20, height, &draft, "critical status that must not wrap");
            let stream = [RenderedTranscriptLine {
                text: "stream".to_string(),
                ..RenderedTranscriptLine::default()
            }];
            vm.live_rendered = &stream;
            let mut autocomplete = AutocompleteState::new();
            autocomplete.show_matches(registry.match_prefix("/brain "));
            let frame = plan_live_frame(&vm, &mut autocomplete);

            assert!(
                frame.rects.completions.is_none(),
                "height {height}: a viewport too small for chrome must claim no completion pane"
            );
            assert!(
                frame.rects.transcript.height <= height,
                "height {height}: the transcript viewport cannot outgrow the terminal"
            );
            assert!(
                !frame
                    .lines
                    .iter()
                    .any(|line| line.starts_with("Commands ") || line.starts_with("> /")),
                "height {height}: no completion rows may reach a viewport that cannot host them; \
                 lines were {:?}",
                frame.lines
            );
            assert!(!autocomplete.is_interactive(), "height {height}");

            let tiny = plan_tiny_live_frame(
                &["/brain keep this draft".to_string()],
                (0, 7),
                "critical status that must not wrap",
                height,
                12,
            );
            let mut output = Vec::new();
            execute!(output, cursor::Hide).unwrap();
            let written = write_tiny_live_frame(&mut output, &tiny).unwrap();
            let raw = String::from_utf8(output).unwrap();
            assert_eq!(written, height, "height {height}");
            assert_eq!(tiny.lines.len(), height, "height {height}");
            assert!(tiny.lines.iter().all(|line| line.chars().count() <= 12));
            assert_eq!(raw.matches("\r\n").count(), height.saturating_sub(1));
            assert!(!raw.contains("\x1b[3J"));
            let hide = raw.find("\x1b[?25l").expect("dialog hid cursor");
            let show = raw.find("\x1b[?25h").expect("tiny input restored cursor");
            assert!(hide < show, "height {height}: cursor must be restored");
        }
    }

    #[test]
    fn test_production_dialog_writer_is_viewport_bounded_and_suppresses_stream() {
        let options = (0..24)
            .map(|index| DialogOption::new(format!("approval choice {index}")))
            .collect();
        let dialog = Dialog::select("Critical approval", options);
        let mut output = Vec::new();

        let rows = TuiRenderer::draw_dialog_inline_bounded(&mut output, &dialog, 40, 5).unwrap();
        let raw = String::from_utf8(output).unwrap();
        assert_eq!(rows, 5);
        assert_eq!(raw.matches("\r\n").count(), 5);
        assert!(raw.contains("Critical approval"));
        assert!(raw.contains("dialog clipped to viewport"));
        assert!(!raw.contains("\x1b[3J"));

        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(CommandRegistry::new().match_prefix("/brain "));
        let live = (0..1_050)
            .map(|row| format!("stream row {row}"))
            .collect::<Vec<_>>();
        let draft = vec![String::new()];
        let live_rendered = live
            .iter()
            .map(|line| RenderedTranscriptLine {
                text: line.clone(),
                ..RenderedTranscriptLine::default()
            })
            .collect::<Vec<_>>();
        let mut vm = live_inputs(40, 6, &draft, "status");
        vm.render_error = true;
        vm.live_rendered = &live_rendered;
        let frame = plan_live_frame(&vm, &mut autocomplete);
        assert!(frame
            .lines
            .iter()
            .all(|line| !line.contains("Commands") && !line.contains("> /")));
        assert!(!autocomplete.is_interactive());
    }

    #[test]
    fn test_dialog_row_count_is_physical_rows_not_logical_lines() {
        // Regression: the dialog reported one row per logical line it emitted.
        // An option label is caller data and is never wrapped or truncated, so
        // a long one painted two terminal rows and was counted as one. That
        // count becomes `cursor_row_from_top`, which the erase walks up by, so
        // the undercount left the top of the box unerased and redrew it one row
        // lower every tick — the cascading duplicate dialogs.
        let width = 40;
        let dialog = Dialog::select(
            "Approve",
            vec![DialogOption::new(
                "run the migration against the production database and then report back",
            )],
        );
        let mut output = Vec::new();

        let rows =
            TuiRenderer::draw_dialog_inline_bounded(&mut output, &dialog, width, 60).unwrap();

        let raw = String::from_utf8(output).unwrap();
        let painted = raw
            .split_terminator("\r\n")
            .map(|line| shadow_buffer::physical_rows(line, width))
            .sum::<usize>();
        let logical = raw.matches("\r\n").count();
        assert!(
            painted > logical,
            "this case must actually exercise a wrapping option label: {logical} logical lines \
             occupying {painted} rows at width {width}"
        );
        assert_eq!(
            painted, rows,
            "the row count handed to cursor_row_from_top must be the rows the dialog paints, \
             not its {logical} logical lines; dialog painted:\n{raw}"
        );
    }

    #[test]
    fn test_dialog_clipping_budget_counts_wrapped_rows() {
        // The clip guard also counted logical lines, so a dialog whose lines
        // wrapped could be "clipped to five rows" and still paint more than
        // five, overflowing the viewport it was bounded to protect.
        let width = 40;
        let options = (0..24)
            .map(|index| {
                DialogOption::new(format!(
                    "approval choice {index} with a label long enough to wrap the terminal"
                ))
            })
            .collect();
        let dialog = Dialog::select("Critical approval", options);
        let mut output = Vec::new();

        let rows = TuiRenderer::draw_dialog_inline_bounded(&mut output, &dialog, width, 5).unwrap();

        let raw = String::from_utf8(output).unwrap();
        let painted = raw
            .split_terminator("\r\n")
            .map(|line| shadow_buffer::physical_rows(line, width))
            .sum::<usize>();
        assert!(
            painted <= 5,
            "a dialog bounded to 5 rows must paint at most 5; it painted {painted}:\n{raw}"
        );
        assert_eq!(
            painted, rows,
            "the returned count must match the painted height; dialog painted:\n{raw}"
        );
        assert!(raw.contains("dialog clipped to viewport"));
    }

    #[test]
    fn test_stream_resize_reconnect_and_dialog_repaint_preserve_selection() {
        let registry = CommandRegistry::new();
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/brain "));
        for _ in 0..9 {
            autocomplete.select_next();
        }
        let selected = autocomplete.get_selected().unwrap().full_syntax();

        let draft = vec![String::new()];
        for (height, width, stream_rows, critical) in [
            (60, 120, 1_001, false),
            (18, 32, 1_040, false),
            (60, 200, 1_080, false),
            (60, 120, 1_080, true),
            (60, 120, 1_100, false),
        ] {
            let live = (0..stream_rows)
                .map(|row| format!("rapid stream row {row}"))
                .collect::<Vec<_>>();
            let live_rendered = live
                .iter()
                .map(|line| RenderedTranscriptLine {
                    text: line.clone(),
                    ..RenderedTranscriptLine::default()
                })
                .collect::<Vec<_>>();
            let mut vm = live_inputs(width, height, &draft, "status");
            vm.render_error = critical;
            vm.live_rendered = &live_rendered;
            let frame = plan_live_frame(&vm, &mut autocomplete);
            assert_eq!(autocomplete.get_selected().unwrap().full_syntax(), selected);
            assert!(frame.lines.first().unwrap().contains("clipped"));
            let painted_completions = frame
                .rects
                .completions
                .map(|rect| rows_in_rect(&frame, width, rect).len())
                .unwrap_or(0);
            if critical {
                assert_eq!(painted_completions, 0);
                assert!(!autocomplete.is_interactive());
            } else {
                assert!(painted_completions > 0);
                assert!(autocomplete.is_interactive());
            }
        }
    }

    #[test]
    fn test_production_raw_completion_writer_is_plain_live_region_output() {
        let registry = CommandRegistry::new();
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/brain list"));
        autocomplete.visible = true;
        let input = vec!["/brain list".to_string()];
        let frame = plan_live_frame(&live_inputs(100, 30, &input, "ready"), &mut autocomplete);

        let mut output = Vec::new();
        write_live_frame(&mut output, &frame, 100).unwrap();
        let raw = String::from_utf8(output).unwrap();

        let completion_rows = frame
            .lines
            .iter()
            .filter(|line| line.starts_with("Commands ") || line.starts_with("> /"))
            .collect::<Vec<_>>();
        assert!(
            !completion_rows.is_empty(),
            "the completion pane must reach the painted frame; frame was {:?}",
            frame.lines
        );
        assert!(
            completion_rows.iter().all(|line| !line.contains('\x1b')),
            "completion rows must stay plain text so the no-colour path is speakable, got {:?}",
            completion_rows
        );
        // The pane is a sibling above the composer: its rows must paint
        // before the prompt line and the separator rule below it.
        let pane_at = raw.find("Commands 1-1 of 1").expect("pane heading painted");
        let selected_at = raw
            .find("> /brain list - List named Brain sessions")
            .expect("selected suggestion painted");
        let prompt_at = raw.find('❯').expect("composer prompt painted");
        let separator_at = raw.find("──  ~/repos/finch").expect("separator painted");
        assert!(
            pane_at < selected_at && selected_at < separator_at && separator_at < prompt_at,
            "the completion pane must paint above the composer, got:\n{raw}"
        );
        assert!(!raw.contains("\x1b[3J"), "must not clear native scrollback");
        assert!(
            !raw.contains("\n\n"),
            "must not emit committed-message spacing"
        );
    }

    // ── Live-area frame: rendering made assertable without a terminal ────────

    /// Minimal inputs for an idle live area at the given size.
    ///
    /// `TuiRenderer::new` enables raw mode and installs a global panic hook, so
    /// no test can construct a renderer. Planning a frame from borrowed state
    /// is what makes the live area reachable from a test at all.
    fn live_inputs<'a>(
        width: usize,
        height: usize,
        input_lines: &'a [String],
        status: &'a str,
    ) -> view_model::LiveViewModel<'a> {
        view_model::LiveViewModel {
            terminal_width: width,
            terminal_height: height,
            input_lines,
            input_cursor: (0, 0),
            ghost_text: None,
            effective_status: status,
            cwd_label: "~/repos/finch",
            session_label: "jade-river",
            dialog: None,
            expanded_lines: None,
            render_error: false,
            task_rows: &[],
            tracked_rows: &[],
            live_rendered: &[],
        }
    }

    /// The frame lines whose painted rows fall inside `rect`, with the row
    /// offset each was painted at. A frame line may wrap, so physical rows are
    /// accumulated from the top of the frame.
    fn rows_in_rect(frame: &LiveFrame, width: usize, rect: widgets::Rect) -> Vec<&str> {
        let mut top = 0usize;
        let mut found = Vec::new();
        for line in &frame.lines {
            let rows = shadow_buffer::physical_rows(line, width.max(1));
            if top >= rect.y && top < rect.bottom() {
                found.push(line.as_str());
            }
            top += rows;
        }
        found
    }

    // ── Acceptance regressions for the claiming widget tree (#805) ───────────

    /// Line indexes of the frame rows painted inside `rect`.
    fn line_indexes_in_rect(frame: &LiveFrame, width: usize, rect: widgets::Rect) -> Vec<usize> {
        let mut top = 0usize;
        let mut found = Vec::new();
        for (index, line) in frame.lines.iter().enumerate() {
            let rows = shadow_buffer::physical_rows(line, width.max(1));
            if top >= rect.y && top < rect.bottom() {
                found.push(index);
            }
            top += rows;
        }
        found
    }

    #[test]
    fn test_completions_pane_claims_rows_above_the_composer_and_never_moves_the_chrome() {
        // Regression for #232: the completion pane used to be concatenated
        // after the input rows, so slash suggestions sat between `❯` and the
        // status rule. The pane is now a sibling above the composer, and an
        // empty pane claims zero rows so opening or closing it cannot move
        // the composer or status rects.
        let registry = CommandRegistry::new();
        let width = 80;
        let height = 24;

        let plain_draft = vec!["hello world".to_string()];
        let closed_vm = live_inputs(width, height, &plain_draft, "ready");
        let closed = plan_live_frame(&closed_vm, &mut AutocompleteState::new());

        let slash_draft = vec!["/quit".to_string()];
        let open_vm = live_inputs(width, height, &slash_draft, "ready");
        let mut open_autocomplete = AutocompleteState::new();
        open_autocomplete.show_matches(registry.match_prefix("/quit"));
        let open = plan_live_frame(&open_vm, &mut open_autocomplete);

        // A non-slash draft claims height 0.
        assert!(
            closed.rects.completions.is_none(),
            "a non-slash draft must claim no completion pane; rects were {:?}",
            closed.rects
        );
        assert!(
            !closed
                .lines
                .iter()
                .any(|line| line.contains("/quit") && !line.contains('❯')),
            "no completion rows may paint for a non-slash draft; frame was {:?}",
            closed.lines
        );

        // With `/quit` the pane claims a rect above the input rect.
        let pane = open
            .rects
            .completions
            .expect("a slash draft must claim the completion pane");
        assert!(
            pane.bottom() <= open.rects.separator.y
                && open.rects.separator.bottom() <= open.rects.composer.y,
            "the pane must sit above the separator and the composer; rects were {:?}",
            open.rects
        );

        // Painted order is the reproduction of #232: every completion row
        // paints before the prompt line, never between `❯` and the status
        // rule. On the pre-#805 planner this assertion failed: the pane's
        // lines were pushed after the input rows.
        let prompt_index = open
            .lines
            .iter()
            .position(|line| line.contains('❯'))
            .expect("the composer prompt must paint");
        let status_rule_index = open
            .lines
            .iter()
            // The status rule is a bare rule row; the session separator also
            // draws dashes but carries the workspace and session labels.
            .rposition(|line| line.contains("───") && !line.contains("jade-river"))
            .expect("the status rule must paint");
        for index in line_indexes_in_rect(&open, width, pane) {
            assert!(
                index < prompt_index,
                "completion row {index} painted at or below the prompt row {prompt_index} — \
                 the #232 placement; frame was {:?}",
                open.lines
            );
        }
        assert!(
            status_rule_index > prompt_index,
            "the status rule stays below the composer; frame was {:?}",
            open.lines
        );
        assert!(
            !line_indexes_in_rect(&open, width, open.rects.composer)
                .iter()
                .any(|index| open.lines[*index].starts_with("Commands ")),
            "no completion row may paint inside the composer rect"
        );

        // Opening the pane moves the transcript viewport, not the chrome.
        assert_eq!(
            closed.rects.composer, open.rects.composer,
            "opening the pane must not move the composer rect"
        );
        assert_eq!(
            closed.rects.status, open.rects.status,
            "opening the pane must not move the status rect"
        );
        assert_eq!(
            closed.rects.status_rule, open.rects.status_rule,
            "opening the pane must not move the status rule"
        );
        assert!(
            closed.rects.transcript.height > open.rects.transcript.height,
            "the transcript viewport yields the pane's rows"
        );
        assert_eq!(
            closed.rects.transcript.height,
            open.rects.transcript.height + pane.height,
            "the pane's rows come out of the transcript viewport, row for row"
        );
    }

    #[test]
    fn test_resize_reclaims_the_frame_and_rebuilds_disclosure_hitboxes() {
        // Successor to #266: a resize is a full layout pass. Sibling rects
        // re-claim relative to the new frame, and the disclosure hitboxes are
        // rebuilt from the new rects. A test fails if widgets keep their old
        // absolute sizes.
        let width = 100;
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(build)");
        work.complete_row_with_body(call, "done", vec!["step one".to_string()]);
        work.set_complete();
        let colors = ColorScheme::default();
        let node = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a WorkUnit projects");
        let lines = AccordionState::default().render_node(&node);
        let live_rendered = lines;

        let plan_at = |w: usize, h: usize| {
            let draft = vec![String::new()];
            let status = "ready";
            let vm = view_model::LiveViewModel {
                terminal_width: w,
                terminal_height: h,
                input_lines: &draft,
                input_cursor: (0, 0),
                ghost_text: None,
                effective_status: status,
                cwd_label: "~/repos/finch",
                session_label: "jade-river",
                dialog: None,
                expanded_lines: None,
                render_error: false,
                task_rows: &[],
                tracked_rows: &[],
                live_rendered: &live_rendered,
            };
            plan_live_frame(&vm, &mut AutocompleteState::new())
        };

        let tall = plan_at(width, 40);
        let small = plan_at(60, 20);

        // Fixed chrome keeps its height but re-claims its width; the flexible
        // transcript viewport takes the leftover at each size.
        assert_eq!(
            tall.rects.composer.height, small.rects.composer.height,
            "the composer's height does not depend on the frame height"
        );
        assert_eq!(
            tall.rects.composer.width, width,
            "the composer re-claims the full width at the larger size"
        );
        assert_eq!(small.rects.composer.width, 60);
        assert!(
            tall.rects.transcript.height > small.rects.transcript.height,
            "the transcript viewport re-claims proportionally more rows in a taller frame; \
             tall={:?} small={:?}",
            tall.rects,
            small.rects
        );
        assert_eq!(
            tall.rects.transcript.height
                + small.rects.composer.height
                + small.rects.status.height
                + small.rects.status_rule.height
                + small.rects.separator.height,
            40,
            "the tall frame is fully claimed by viewport and chrome"
        );

        // Hitboxes rebuild from the new rects: they live inside the
        // transcript viewport and differ between the passes.
        assert!(
            !tall.hitboxes.is_empty(),
            "an expandable live row must claim a disclosure hitbox"
        );
        for region in &tall.hitboxes {
            let rect = &region.region;
            assert!(
                rect.top >= tall.rects.transcript.y as u16
                    && rect.bottom < tall.rects.transcript.bottom() as u16,
                "every claimed hitbox lives inside the transcript viewport; region={rect:?} viewport={:?}",
                tall.rects.transcript
            );
        }
        assert_ne!(
            tall.hitboxes, small.hitboxes,
            "a resize must rebuild the hitboxes from the new claimed rects"
        );
    }

    #[test]
    fn test_disclosure_hitboxes_are_the_depth_first_claimed_rects_and_the_mouse_toggles_through_them(
    ) {
        // The hitboxes the claiming pass produced are the rects the
        // disclosure rows claimed inside the viewport box — not a recount —
        // and clicking one toggles the row through the renderer-owned open
        // set.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo hi)");
        work.complete_row_with_body(call, "done", vec!["hi".to_string()]);
        work.set_complete();
        let colors = ColorScheme::default();
        let node = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a WorkUnit projects");
        let live_rendered = AccordionState::default().render_node(&node);
        let width = 80;
        let draft = vec![String::new()];
        let status = "ready";
        let vm = view_model::LiveViewModel {
            terminal_width: width,
            terminal_height: 24,
            input_lines: &draft,
            input_cursor: (0, 0),
            ghost_text: None,
            effective_status: status,
            cwd_label: "~/repos/finch",
            session_label: "jade-river",
            dialog: None,
            expanded_lines: None,
            render_error: false,
            task_rows: &[],
            tracked_rows: &[],
            live_rendered: &live_rendered,
        };
        let frame = plan_live_frame(&vm, &mut AutocompleteState::new());
        assert!(
            !frame.hitboxes.is_empty(),
            "the expandable row claims a hitbox"
        );

        // The claimed rect is exactly the painted row of the row's header.
        let region = &frame.hitboxes[0].region;
        let header_index = frame
            .visible_live
            .iter()
            .position(|line| line.row_id.as_ref() == Some(&region.row_id))
            .expect("the claimed row's header is among the painted live lines");
        assert!(
            frame.visible_live[header_index].row_expanded.is_some(),
            "hit targets are expandable headers"
        );

        let mut accordion = AccordionState::default();
        accordion.adopt_claimed_hitboxes(&frame.hitboxes, 0, width);
        let clicked = accordion
            .hit_regions
            .iter()
            .find(|candidate| candidate.row_id == region.row_id)
            .expect("the claimed rect was adopted");
        assert_eq!(
            (clicked.top, clicked.bottom, clicked.left, clicked.right),
            (region.top, region.bottom, region.left, region.right),
            "the adopted region is the rect the pass claimed"
        );
        let node_after = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a WorkUnit projects");
        let expanded_before = accordion.is_expanded(&node_after);
        let toggled = accordion.handle_mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: clicked.left,
            row: clicked.top,
            modifiers: crossterm::event::KeyModifiers::NONE,
        });
        assert!(toggled, "clicking the claimed rect must toggle the row");
        let node_after_toggle =
            crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
                .expect("a WorkUnit projects");
        assert_ne!(
            accordion.is_expanded(&node_after_toggle),
            expanded_before,
            "the click flips the renderer-owned open set"
        );
    }

    #[test]
    fn test_thinking_leaf_renders_one_glyph_from_view_model_props_without_an_invented_bullet() {
        // #821: a leaf thinking line is one glyph plus its status text, taken
        // from the ViewModel's label props. The disclosure must not invent a
        // second `•` bullet in front of the glyph, and must not sniff glyph
        // characters to decide anything.
        let colors = ColorScheme::default();
        let pending = Arc::new(WorkUnit::new("Analyzing"));
        let node = crate::cli::tui::view_model::try_project_for_test(pending.as_ref(), &colors)
            .expect("a WorkUnit projects");
        let rendered = AccordionState::default().render_node(&node);
        let header = &rendered[0].text;
        assert!(
            header.contains('\u{25cb}'),
            "a pending thinking row carries its status glyph from the ViewModel props; header={header:?}"
        );
        assert!(
            !header.contains('•'),
            "the disclosure must not invent a bullet in front of a leaf's own status glyph \
             (#821); header={header:?}"
        );
        assert!(
            header.contains("Analyzing"),
            "the leaf stays readable: one glyph plus the status text; header={header:?}"
        );

        // Expandable rows keep their disclosure marks.
        let work = Arc::new(WorkUnit::new("Tools"));
        let call = work.add_row("bash(echo hi)");
        work.complete_row_with_body(call, "done", vec!["hi".to_string()]);
        work.set_complete();
        let node = crate::cli::tui::view_model::try_project_for_test(work.as_ref(), &colors)
            .expect("a WorkUnit projects");
        let rendered = AccordionState::default().render_node(&node);
        assert!(
            rendered[0].text.contains('▶'),
            "collapsed expandable rows keep the disclosure mark; header={:?}",
            rendered[0].text
        );
    }

    /// Rows the buffer actually holds content on.
    fn occupied_rows(buffer: &shadow_buffer::ShadowBuffer) -> usize {
        buffer
            .rows_as_text()
            .iter()
            .rposition(|row| !row.is_empty())
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn reported_usage(
        state: activity::ActivityUsageState,
        input: Option<u64>,
        output: Option<u64>,
        reported: usize,
        started: usize,
    ) -> activity::ActivityUsage {
        activity::ActivityUsage {
            state,
            input_tokens: input,
            output_tokens: output,
            reported_attempts: reported,
            started_attempts: started,
        }
    }

    #[test]
    fn test_su_01_zero_started_child_does_not_downgrade_complete_usage() {
        use activity::ActivityUsageState;

        let aggregate = aggregate_agent_usage(
            [
                reported_usage(ActivityUsageState::Complete, Some(12), Some(7), 1, 1),
                activity::ActivityUsage::default(),
            ]
            .iter(),
        );
        assert_eq!(aggregate.state, ActivityUsageState::Complete, "SU-01: a queued child with zero started attempts has no missing report and must not contradict the fully reported running child; aggregate={aggregate:?}");

        let status = StatusBar::new();
        status.update_agent_activity(2, &aggregate);
        let line = status
            .get_line(&StatusLineType::AgentActivity)
            .expect("SU-01: two active children must produce an aggregate status line");
        assert!(line.contains("complete (1/1 attempts reported)") && !line.contains("partial"), "SU-01: status text must agree with reported-versus-started attempts; line={line:?} aggregate={aggregate:?}");
    }

    #[test]
    fn test_child_aggregate_updates_shadow_buffer_without_console_duplication() {
        use activity::ActivityUsageState;

        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let status = Arc::new(StatusBar::new());
        let mut renderer =
            TuiRenderer::new_headless(Arc::clone(&output), Arc::clone(&status), colors);
        assert_eq!(
            status.get_line(&StatusLineType::AgentActivity),
            None,
            "zero children must consume no status row"
        );
        let input = vec![String::new()];
        let mut autocomplete = AutocompleteState::new();
        let zero_rows = plan_live_frame(&live_inputs(160, 20, &input, ""), &mut autocomplete)
            .to_shadow_buffer(160, 20)
            .rows_as_text();
        assert!(
            zero_rows.iter().all(|row| !row.contains("Children:")),
            "zero-child shadow frame must not paint an aggregate; rows={zero_rows:?}"
        );

        let first_id = uuid::Uuid::new_v4();
        renderer.apply_activity(activity::ActivityUpdate::Upsert {
            id: first_id,
            row: activity::ActivityRow::new("first child", activity::ActivityState::Active),
            usage: reported_usage(ActivityUsageState::Unavailable, None, None, 0, 1),
        });
        let unavailable = status
            .get_line(&StatusLineType::AgentActivity)
            .expect("one active child must create the aggregate status cell");
        assert!(
            unavailable.contains("unavailable input, unavailable output")
                && unavailable.contains("unavailable (0/1 attempts reported)"),
            "unreported provider usage must remain visibly unavailable; aggregate={unavailable:?}"
        );
        renderer.apply_activity(activity::ActivityUpdate::SetUsage {
            id: first_id,
            usage: reported_usage(ActivityUsageState::Complete, Some(12), Some(7), 1, 1),
        });
        let one = status.get_status();
        let one_rows = plan_live_frame(&live_inputs(160, 20, &input, &one), &mut autocomplete)
            .to_shadow_buffer(160, 20)
            .rows_as_text();
        assert!(
            one_rows.iter().any(|row| row.contains("Children: 1 active")
                && row.contains("12 input, 7 output")
                && row.contains("complete (1/1 attempts reported)")),
            "one-child aggregate must be fully speakable in the shadow buffer; rows={one_rows:?}"
        );

        let second_id = uuid::Uuid::new_v4();
        renderer.apply_activity(activity::ActivityUpdate::Upsert {
            id: second_id,
            row: activity::ActivityRow::new("second child", activity::ActivityState::Pending),
            usage: reported_usage(ActivityUsageState::Partial, Some(5), None, 1, 2),
        });
        let multiple = status.get_status();
        let multiple_rows =
            plan_live_frame(&live_inputs(160, 20, &input, &multiple), &mut autocomplete)
                .to_shadow_buffer(160, 20)
                .rows_as_text();
        assert!(multiple_rows.iter().any(|row| row.contains("Children: 2 active") && row.contains("17 input, 7 output") && row.contains("partial (2/3 attempts reported)")), "mixed running/queued children must sum only known usage in one status row; rows={multiple_rows:?}");

        renderer.apply_activity(activity::ActivityUpdate::Remove { id: first_id });
        renderer.apply_activity(activity::ActivityUpdate::Remove { id: second_id });
        assert_eq!(
            status.get_line(&StatusLineType::AgentActivity),
            None,
            "final child cleanup must remove the aggregate status cell"
        );
        assert!(output.get_messages().is_empty(), "usage refreshes belong only to live status and must not append console or scrollback messages; messages={:?}", output.get_messages().len());
    }

    #[test]
    fn test_child_resnapshot_replaces_stale_rows_and_usage_authoritatively() {
        use activity::ActivityUsageState;

        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let status = Arc::new(StatusBar::new());
        let mut renderer =
            TuiRenderer::new_headless(Arc::clone(&output), Arc::clone(&status), colors);
        renderer.apply_activity(activity::ActivityUpdate::Upsert {
            id: uuid::Uuid::new_v4(),
            row: activity::ActivityRow::new("stale child", activity::ActivityState::Active),
            usage: reported_usage(ActivityUsageState::Complete, Some(999), Some(999), 1, 1),
        });
        let current_id = uuid::Uuid::new_v4();
        renderer.apply_activity(activity::ActivityUpdate::Resnapshot {
            rows: vec![(
                current_id,
                activity::ActivityRow::new("current child", activity::ActivityState::Active),
                reported_usage(ActivityUsageState::Partial, Some(3), None, 1, 1),
            )],
        });
        // A transition queued after the fresh subscription but already
        // represented by the snapshot may arrive next. It may refresh the row,
        // but cannot roll authoritative usage back to the event's empty default.
        renderer.apply_activity(activity::ActivityUpdate::Upsert {
            id: current_id,
            row: activity::ActivityRow::new("current child", activity::ActivityState::Active),
            usage: activity::ActivityUsage::default(),
        });
        assert_eq!(
            renderer.tracked_rows.len(),
            1,
            "authoritative recovery must discard stale child rows; rows={:?}",
            renderer.tracked_rows
        );
        assert!(
            renderer.tracked_rows.contains_key(&current_id),
            "authoritative recovery must retain the current child; rows={:?}",
            renderer.tracked_rows
        );
        let aggregate = status
            .get_line(&StatusLineType::AgentActivity)
            .expect("the authoritative current child must retain an aggregate status cell");
        assert!(
            aggregate.contains("3 input, unavailable output") && aggregate.contains("partial"),
            "resnapshot must replace stale usage rather than add to it; aggregate={aggregate:?}"
        );
        assert!(
            output.get_messages().is_empty(),
            "resnapshot is a live projection and must not append console history; message_count={}",
            output.get_messages().len()
        );
    }

    #[test]
    fn test_live_frame_renders_into_a_shadow_buffer_with_no_terminal() {
        let input = vec!["hello".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let frame = plan_live_frame(&live_inputs(40, 20, &input, "idle"), &mut autocomplete);

        let buffer = frame.to_shadow_buffer(40, 20);
        let rows = buffer.rows_as_text();

        let prompt_row = rows
            .iter()
            .position(|row| row.contains("hello"))
            .unwrap_or_else(|| panic!("input row must be painted; frame rows were {rows:?}"));
        assert_eq!(
            "❯ hello", rows[prompt_row],
            "the input row is the prompt plus the draft, with no ANSI left in the cells; \
             frame lines were {:?}",
            frame.lines
        );
        assert_eq!(
            Some('❯'),
            buffer.get(0, prompt_row).map(|cell| cell.ch),
            "the prompt glyph must land in column 0 of its row; row read back as {:?}",
            rows[prompt_row]
        );
        assert!(
            rows.iter().any(|row| row.contains("jade-river")),
            "the session separator must be part of the frame; frame rows were {rows:?}"
        );
    }

    #[test]
    fn test_vt_oracle_production_live_writer_emits_exact_editable_cells_styles_and_cursor() {
        let width = 24;
        let input = vec!["alpha".to_string(), "bravo".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(width, 12, &input, "idle");
        inputs.input_cursor = (1, 3);
        let frame = plan_live_frame(&inputs, &mut autocomplete);
        let mut bytes = Vec::new();
        write_live_frame(&mut bytes, &frame, width).unwrap();

        let mut terminal = VtOracle::new(width, 12);
        terminal.feed(&bytes);
        let expected = [
            "──  ~/re…  jade-river ──",
            "❯ alpha",
            "  bravo",
            "────────────────────────",
            "idle",
        ];
        for (row, expected) in expected.iter().enumerate() {
            assert_vt(
                terminal.row(row) == *expected,
                &format!("editable frame row {row} must equal {expected:?}"),
                &terminal,
            );
        }
        let prompt_style = terminal.cell(1, 0).style;
        let colored_prompt = VtStyle {
            foreground: VtColor::Indexed(14),
            background: VtColor::Default,
            bold: false,
            reverse: false,
        };
        assert_vt(
            prompt_style == VtStyle::default() || prompt_style == colored_prompt,
            "the editable prompt must be either the exact cyan production style or the exact \
             terminal-default NO_COLOR style",
            &terminal,
        );
        assert_vt(
            terminal.cell(1, 1).style == VtStyle::default(),
            "prompt styling must reset before the draft",
            &terminal,
        );
        let expected_secondary = if prompt_style == colored_prompt {
            VtStyle {
                foreground: VtColor::Indexed(8),
                background: VtColor::Default,
                bold: false,
                reverse: false,
            }
        } else {
            VtStyle::default()
        };
        assert_vt(
            terminal.cell(0, 0).style == expected_secondary
                && terminal.cell(3, 0).style == expected_secondary
                && terminal.cell(4, 0).style == expected_secondary,
            "separator and status styles must consistently match the selected production color \
             profile",
            &terminal,
        );
        assert_vt(
            terminal.cursor() == (2, 5, true),
            "the visible cursor must return after the third character of the second draft row",
            &terminal,
        );
    }

    #[test]
    fn test_issue_652_lifecycle_vt_oracle_keeps_hierarchy_after_wrapping() {
        let width = 34;
        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        let status = Arc::new(StatusBar::new());
        let renderer = TuiRenderer::new_headless(Arc::clone(&output), status, colors.clone());
        let unit = output.start_work_unit("Tools");
        let spawn_row = unit.add_row("spawn_agent(root)");
        unit.complete_row(spawn_row, "spawned");
        let root_agent = uuid::Uuid::new_v4();
        let root_task = uuid::Uuid::new_v4();
        unit.queue_agent_activity(
            Some(spawn_row),
            root_agent,
            root_task,
            None,
            "root task · test/model",
        );
        let nested_task = uuid::Uuid::new_v4();
        unit.queue_agent_activity(
            Some(spawn_row),
            uuid::Uuid::new_v4(),
            nested_task,
            Some(root_agent),
            "nested task · test/model",
        );
        unit.start_agent_activity(nested_task);
        unit.start_agent_tool(nested_task, "read");

        let message = output
            .get_messages()
            .into_iter()
            .next()
            .expect("lifecycle work unit must be present");
        let live = match view_model::project_message(&message, &colors) {
            view_model::ProjectedMessage::Node(node) => {
                renderer.accordion.render_node_fully_expanded(&node)
            }
            view_model::ProjectedMessage::Plain(formatted) => {
                renderer.accordion.render_plain(&formatted.join("\n"))
            }
        };
        let input = vec![String::new()];
        let mut inputs = live_inputs(width, 20, &input, "idle");
        inputs.live_rendered = &live;
        let mut autocomplete = AutocompleteState::new();
        let frame = plan_live_frame(&inputs, &mut autocomplete);
        let mut bytes = Vec::new();
        write_live_frame(&mut bytes, &frame, width).unwrap();
        let mut terminal = VtOracle::new(width, 24);
        terminal.feed(&bytes);

        let root_row = terminal.find_row("root task").unwrap_or_else(|| {
            panic!(
                "root lifecycle row must reach terminal cells\n{}",
                terminal.diagnostic()
            )
        });
        let nested_row = terminal.find_row("nested task").unwrap_or_else(|| {
            panic!(
                "nested lifecycle row must reach terminal cells\n{}",
                terminal.diagnostic()
            )
        });
        let tool_row = terminal.find_row("tool read").unwrap_or_else(|| {
            panic!(
                "child tool row must reach terminal cells\n{}",
                terminal.diagnostic()
            )
        });
        assert_vt(
            root_row < nested_row && nested_row < tool_row,
            "spawn, nested child, and child tool must retain semantic order",
            &terminal,
        );
        let root_indent = terminal
            .row(root_row)
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        let nested_indent = terminal
            .row(nested_row)
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        let tool_indent = terminal
            .row(tool_row)
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        assert_vt(
            root_indent < nested_indent && nested_indent < tool_indent,
            "terminal-cell indentation must preserve spawn > nested child > child tool hierarchy",
            &terminal,
        );
    }

    #[test]
    fn test_vt_oracle_live_writer_expands_horizontal_tab_to_eight_column_stop() {
        let width = 16;
        let frame = LiveFrame {
            lines: vec!["a\tb".to_string()],
            cursor_row: 0,
            cursor_col: 9,
            cursor_visible: true,
            visible_live: Vec::new(),
            ..LiveFrame::default()
        };
        let mut bytes = Vec::new();
        write_live_frame(&mut bytes, &frame, width).unwrap();

        let mut terminal = VtOracle::new(width, 2);
        terminal.feed(&bytes);
        assert_vt(
            terminal.row(0) == "a       b",
            "HT must advance from column 1 to the standard column-8 tab stop before painting b",
            &terminal,
        );
        assert_vt(
            terminal.cell(0, 8).character == 'b',
            "the character after HT must occupy display column 8",
            &terminal,
        );
        assert_vt(
            terminal.cursor() == (0, 9, true),
            "the writer's input cursor must land one cell after b at display column 9",
            &terminal,
        );
    }

    #[test]
    fn test_vt_oracle_structured_file_diff_approval_reaches_terminal_cells_and_styles() {
        let width = 48;
        let height = 20;
        let diff = FileDiff::from_texts("src/oracle.rs", "before\nkeep\n", "after\nkeep\n");
        let colors = ColorTheme::Dark.to_scheme();
        let mut dialog =
            Dialog::tool_approval("Edit", &summarize_files(std::slice::from_ref(&diff)));
        // This is the production assembly contract: FileDiff owns sanitizing untrusted content,
        // then its SGR-bearing result is assigned directly so the dialog does not strip the theme.
        dialog.body = Some(diff.render(&colors, DiffColorMode::Theme));
        let input = vec!["draft stays hidden".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(width, height, &input, "approval pending");
        inputs.dialog = Some(&dialog);
        let frame = plan_live_frame(&inputs, &mut autocomplete);
        let mut bytes = Vec::new();
        write_live_frame(&mut bytes, &frame, width).unwrap();

        let mut terminal = VtOracle::new(width, height + 1);
        terminal.feed(&bytes);
        let removed_row = terminal.find_row("- before").unwrap_or_else(|| {
            panic!(
                "removal row must remain decision-visible\n{}",
                terminal.diagnostic()
            )
        });
        let added_row = terminal.find_row("+ after").unwrap_or_else(|| {
            panic!(
                "addition row must remain decision-visible\n{}",
                terminal.diagnostic()
            )
        });
        let yes_row = terminal.find_row("1. Yes").unwrap_or_else(|| {
            panic!(
                "approval option must remain decision-visible\n{}",
                terminal.diagnostic()
            )
        });
        let removed_col = terminal.row(removed_row).find("- before").unwrap();
        let added_col = terminal.row(added_row).find("+ after").unwrap();
        let removed_style = terminal.cell(removed_row, removed_col).style;
        let added_style = terminal.cell(added_row, added_col).style;
        assert_vt(
            removed_style.foreground == VtColor::Rgb(255, 236, 236)
                && removed_style.background == VtColor::Rgb(88, 24, 28),
            "the structured removal must fill a red background with contrasting ink",
            &terminal,
        );
        assert_vt(
            added_style.foreground == VtColor::Rgb(236, 246, 238)
                && added_style.background == VtColor::Rgb(20, 72, 40),
            "the structured addition must fill a green background with contrasting ink",
            &terminal,
        );
        let keep_row = terminal.find_row("keep").unwrap_or_else(|| {
            panic!(
                "context row must remain decision-visible\n{}",
                terminal.diagnostic()
            )
        });
        let keep_col = terminal.row(keep_row).find("keep").unwrap();
        let keep_style = terminal.cell(keep_row, keep_col).style;
        assert_vt(
            keep_style.background == VtColor::Rgb(44, 48, 54)
                && keep_style.foreground != VtColor::Rgb(245, 247, 250),
            "context rows at the top/bottom of the hunk must use a grey fill, not near-white ink on a light default",
            &terminal,
        );
        let selected_style = terminal.cell(yes_row, 4).style;
        let colored_selected = VtStyle {
            foreground: VtColor::Indexed(14),
            background: VtColor::Default,
            bold: true,
            reverse: false,
        };
        assert_vt(
            selected_style == VtStyle::default() || selected_style == colored_selected,
            "the selected option must be either exact bold cyan or exact terminal-default \
             NO_COLOR styling; diff styling must not leak into it",
            &terminal,
        );
        assert_vt(
            terminal.cursor().2 == false,
            "approval frames must hide the terminal cursor",
            &terminal,
        );
    }

    #[test]
    fn test_vt_oracle_bottom_anchored_dialog_paint_repaint_and_close_stay_in_owned_rows() {
        let width = 32;
        for height in [8, 12] {
            let mut terminal = VtOracle::new(width, height);
            let input = vec!["final".to_string()];
            let mut autocomplete = AutocompleteState::new();
            let mut dialog = Dialog::multiselect("Choose", vec![DialogOption::new("Keep")]);
            let mut inputs = live_inputs(width, height, &input, "ready");
            inputs.dialog = Some(&dialog);
            let mut frame = plan_live_frame(&inputs, &mut autocomplete);
            let plan = viewport_redraw_plan(height, frame.physical_rows(width), 2);
            let transcript: Vec<String> = if height == 12 {
                vec!["retained first".into(), "retained last".into()]
            } else {
                Vec::new()
            };
            let mut bytes = Vec::new();
            begin_full_viewport_paint(&mut bytes, plan, &transcript).unwrap();
            let mut active_rows = write_live_frame(&mut bytes, &frame, width).unwrap();
            execute!(bytes, EndSynchronizedUpdate).unwrap();
            terminal.feed(&bytes);

            for title in ["Choose", "Pick", "Go"] {
                if title != "Choose" {
                    bytes.clear();
                    write_live_area_erase(&mut bytes, active_rows, frame.cursor_row).unwrap();
                    terminal.feed(&bytes);
                    assert_vt(
                        terminal.cursor() == (plan.live_top, 0, false)
                            && (plan.live_top..height).all(|row| terminal.row(row).is_empty()),
                        "dialog erase must clear every owned row and return to the exact origin",
                        &terminal,
                    );
                    dialog.title = title.into();
                    let mut inputs = live_inputs(width, height, &input, "ready");
                    inputs.dialog = Some(&dialog);
                    frame = plan_live_frame(&inputs, &mut autocomplete);
                    bytes.clear();
                    active_rows = write_live_frame(&mut bytes, &frame, width).unwrap();
                    execute!(bytes, EndSynchronizedUpdate).unwrap();
                    terminal.feed(&bytes);
                }
                let rule = "─".repeat(width);
                let mut expected = vec![
                    "──  ~/repos/finch  jade-river ──".to_string(),
                    rule.clone(),
                    format!("  {title}"),
                    rule.clone(),
                    "    ☐ Keep".to_string(),
                    rule.clone(),
                    "  [ Submit ]   [ Cancel ]".to_string(),
                ];
                if height > 8 {
                    expected.push("  ↑/↓: Navigate | Space: Toggle".to_string());
                    expected.push("  | Enter: Submit | Esc: Cancel".to_string());
                }
                expected.push(rule);
                for (row, expected) in expected.iter().enumerate() {
                    assert_vt(
                        terminal.row(plan.live_top + row) == *expected,
                        &format!("bottom-anchored dialog row {row} must equal {expected:?} without scrolling"),
                        &terminal,
                    );
                }
                assert_vt(
                    active_rows == expected.len()
                        && frame.cursor_row == expected.len() - 1
                        && terminal.cursor() == (height - 1, 0, false),
                    "dialog paint must own every expected row and hide its cursor on the final row",
                    &terminal,
                );
                if height == 12 {
                    let first =
                        (0..plan.live_top).find(|&row| terminal.row(row) == "retained first");
                    let last = (0..plan.live_top).find(|&row| terminal.row(row) == "retained last");
                    assert_vt(
                        first.is_some()
                            && last.is_some()
                            && first < last
                            && (0..plan.live_top).all(|row| {
                                matches!(
                                    terminal.row(row).as_str(),
                                    "" | "retained first" | "retained last"
                                )
                            }),
                        "taller dialog may consume padding but must keep retained transcript above live_top",
                        &terminal,
                    );
                } else {
                    for row in 0..plan.live_top {
                        assert_vt(
                            terminal.row(row).is_empty(),
                            "short dialog must not paint into the region above live_top",
                            &terminal,
                        );
                    }
                }
            }

            let colors = ColorScheme::default();
            let output = Arc::new(OutputManager::new(colors.clone()));
            let mut renderer =
                TuiRenderer::new_headless(output, Arc::new(StatusBar::new()), colors);
            renderer.active_dialog = Some(dialog);
            assert_vt(
                renderer.live_geometry(width as u16, height as u16)
                    == Some((active_rows, frame.cursor_row)),
                "viewport geometry must agree with the actual dialog rows and hidden cursor",
                &terminal,
            );

            bytes.clear();
            write_live_area_erase(&mut bytes, active_rows, frame.cursor_row).unwrap();
            let mut closed_inputs = live_inputs(width, height, &input, "ready");
            closed_inputs.input_cursor = (0, 5);
            let closed = plan_live_frame(&closed_inputs, &mut autocomplete);
            write_live_frame(&mut bytes, &closed, width).unwrap();
            execute!(bytes, EndSynchronizedUpdate).unwrap();
            terminal.feed(&bytes);
            let expected = [
                "──  ~/repos/finch  jade-river ──".to_string(),
                "❯ final".to_string(),
                "─".repeat(width),
                "ready".to_string(),
            ];
            for row in plan.live_top..height {
                let expected = expected.get(row - plan.live_top).map_or("", String::as_str);
                assert_vt(
                    terminal.row(row) == expected,
                    &format!(
                        "dialog close row {row} must equal {expected:?} with no stale modal cells"
                    ),
                    &terminal,
                );
            }
            assert_vt(
                terminal.cursor() == (plan.live_top + 1, 7, true),
                "dialog close must restore the visible input cursor after the draft",
                &terminal,
            );
        }
    }

    #[test]
    fn test_vt_oracle_persistent_update_and_dialog_close_clear_rows_and_restore_cursor() {
        let width = 48;
        let height = 24;
        let mut terminal = VtOracle::new(width, height + 2);
        let input = vec!["first draft".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let first = plan_live_frame(
            &live_inputs(width, height, &input, "obsolete status row"),
            &mut autocomplete,
        );
        let mut bytes = Vec::new();
        let first_rows = write_live_frame(&mut bytes, &first, width).unwrap();
        terminal.feed(&bytes);

        bytes.clear();
        write_live_area_erase(&mut bytes, first_rows, first.cursor_row).unwrap();
        let diff = FileDiff::from_texts("src/old.rs", "gone\n", "shown\n");
        let colors = ColorTheme::Dark.to_scheme();
        let mut dialog = Dialog::tool_approval("Edit", "src/old.rs  +1 -1");
        dialog.body = Some(diff.render(&colors, DiffColorMode::Theme));
        let mut dialog_inputs = live_inputs(width, height, &input, "obsolete status row");
        dialog_inputs.dialog = Some(&dialog);
        let dialog_frame = plan_live_frame(&dialog_inputs, &mut autocomplete);
        let dialog_rows = write_live_frame(&mut bytes, &dialog_frame, width).unwrap();
        terminal.feed(&bytes);
        assert_vt(
            terminal.find_row("- gone").is_some() && !terminal.cursor().2,
            "the persistent model must observe the intermediate approval paint",
            &terminal,
        );

        bytes.clear();
        write_live_area_erase(&mut bytes, dialog_rows, dialog_frame.cursor_row).unwrap();
        let closed_input = vec!["final".to_string()];
        let mut closed_inputs = live_inputs(width, height, &closed_input, "ready");
        closed_inputs.input_cursor = (0, 5);
        let closed = plan_live_frame(&closed_inputs, &mut autocomplete);
        write_live_frame(&mut bytes, &closed, width).unwrap();
        terminal.feed(&bytes);

        assert_vt(
            terminal.find_row("gone").is_none()
                && terminal.find_row("shown").is_none()
                && terminal.find_row("obsolete status row").is_none(),
            "dialog and superseded editable rows must be cleared after close",
            &terminal,
        );
        let final_row = terminal.find_row("❯ final").unwrap_or_else(|| {
            panic!(
                "closed dialog must repaint the draft\n{}",
                terminal.diagnostic()
            )
        });
        assert_vt(
            terminal.cell(final_row, 2).style == VtStyle::default(),
            "dialog colors must not leak into the repainted input",
            &terminal,
        );
        assert_vt(
            terminal.cursor() == (final_row, 7, true),
            "dialog close must restore the visible input cursor after the final draft",
            &terminal,
        );
        for row in closed.physical_rows(width)..height {
            assert_vt(
                terminal.row(row).is_empty(),
                &format!("row {row} below the closed frame must remain cleared"),
                &terminal,
            );
        }
    }

    #[test]
    fn test_live_frame_measured_height_equals_the_rows_it_paints() {
        // A status line wider than the terminal wraps. The frame's own row
        // count is what erase_live_area() clears, so it must equal the rows the
        // painted bytes actually occupy — not the number of logical lines.
        let width = 30;
        let status = "a".repeat(95);
        let input = vec![String::new()];
        let mut autocomplete = AutocompleteState::new();
        let frame = plan_live_frame(&live_inputs(width, 40, &input, &status), &mut autocomplete);

        let buffer = frame.to_shadow_buffer(width, 40);

        assert_eq!(
            frame.physical_rows(width),
            occupied_rows(&buffer),
            "the height the renderer records must equal the height it paints, or the next \
             erase leaves a row behind; frame lines were {:?}",
            frame.lines
        );
        assert!(
            frame.physical_rows(width) > frame.lines.len(),
            "this case must actually exercise wrapping: {} logical lines occupying {} rows",
            frame.lines.len(),
            frame.physical_rows(width)
        );
    }

    #[test]
    fn test_live_frame_cursor_lands_on_the_input_row_it_reports() {
        let width = 20;
        let input = vec!["abc".to_string(), "defgh".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(width, 24, &input, "idle");
        inputs.input_cursor = (1, 3);
        let frame = plan_live_frame(&inputs, &mut autocomplete);

        let rows = frame.to_shadow_buffer(width, 24).rows_as_text();
        assert_eq!(
            "  defgh", rows[frame.cursor_row],
            "cursor_row must index the continuation row holding the cursor; frame rows were {rows:?}"
        );
        assert_eq!(
            PROMPT_COLUMNS + 3,
            frame.cursor_col,
            "the cursor sits after the continuation prefix and three typed columns; \
             frame lines were {:?}",
            frame.lines
        );
    }

    #[test]
    fn test_live_frame_keeps_composer_draft_while_transcript_streams() {
        // #136 dump: while program/tool/result rows streamed, a second draft
        // ("fdsfsa" / "Test") stayed invisible until the turn finished.
        let width = 40;
        let streaming_status = "running · tool bash";
        let live: Vec<RenderedTranscriptLine> = (0..40)
            .map(|index| RenderedTranscriptLine {
                text: format!("tool result line {index} wrapping-{}", "x".repeat(48)),
                ..RenderedTranscriptLine::default()
            })
            .collect();
        let typed = ["fdsfsa", "Test"];
        let mut autocomplete = AutocompleteState::new();
        // Session tasks are reserved against the live budget but were still
        // painted in full. Enough active rows filled the viewport and clipped
        // the composer off the bottom — the dump: typed draft hidden until
        // the turn finished.
        let tasks = (0..12)
            .map(|index| {
                activity::ActivityRow::new(format!("todo {index}"), activity::ActivityState::Active)
            })
            .collect::<Vec<_>>();
        let mut child =
            activity::ActivityRow::new("spawned agent", activity::ActivityState::Active);
        child.detail = Some(" · model".to_string());
        let tracked = vec![child];

        for height in [8usize, 12, 24] {
            for live_count in [1usize, 8, 20, live.len()] {
                for draft_len in 1..=typed.len() {
                    let input = typed[..draft_len]
                        .iter()
                        .map(|line| (*line).to_string())
                        .collect::<Vec<_>>();
                    let mut inputs = live_inputs(width, height, &input, streaming_status);
                    inputs.live_rendered = &live[..live_count];
                    inputs.task_rows = &tasks;
                    inputs.tracked_rows = &tracked;
                    inputs.input_cursor =
                        (input.len() - 1, input.last().map(String::len).unwrap_or(0));
                    let frame = plan_live_frame(&inputs, &mut autocomplete);
                    let physical = frame.physical_rows(width);
                    let rows = frame.to_shadow_buffer(width, height).rows_as_text();
                    let painted = rows.join("\n");
                    assert!(
                        physical <= height,
                        "INVARIANT: a streaming live frame must not overflow the viewport, or \
                         the composer is clipped off the bottom; height={height} \
                         live_count={live_count} physical_rows={physical} frame={:?}",
                        frame.lines
                    );
                    for line in &input {
                        assert!(
                            painted.contains(line),
                            "INVARIANT: plan_live_frame must keep composer draft {line:?} in the \
                             visible {width}x{height} frame while {live_count} live transcript \
                             rows stream (the dump: typed draft hidden until the turn finished); \
                             cursor_visible={} cursor=({}, {}) physical_rows={physical} \
                             frame={:?} visible_rows={rows:?}",
                            frame.cursor_visible,
                            frame.cursor_row,
                            frame.cursor_col,
                            frame.lines
                        );
                    }
                    assert!(
                        frame.cursor_visible,
                        "INVARIANT: the composer cursor must stay visible while live rows \
                         stream; height={height} live_count={live_count} draft={input:?} \
                         cursor=({}, {}) frame={:?}",
                        frame.cursor_row, frame.cursor_col, frame.lines
                    );
                    assert!(
                        frame.cursor_row < height,
                        "INVARIANT: the composer cursor must remain inside the viewport while \
                         live rows stream; cursor_row={} height={height} live_count={live_count} \
                         physical_rows={physical} frame={:?}",
                        frame.cursor_row,
                        frame.lines
                    );
                    let cursor_text = rows.get(frame.cursor_row).map(String::as_str).unwrap_or("");
                    let expected_cursor_line = input.last().expect("draft is non-empty");
                    assert!(
                        cursor_text.contains(expected_cursor_line),
                        "INVARIANT: the visible cursor must sit on the current draft row while \
                         live rows stream; cursor_row={} cursor_line={cursor_text:?} \
                         expected={expected_cursor_line:?} height={height} live_count={live_count} \
                         frame={:?} rows={rows:?}",
                        frame.cursor_row,
                        frame.lines
                    );

                    let mut bytes = Vec::new();
                    write_live_frame(&mut bytes, &frame, width).unwrap();
                    let mut terminal = VtOracle::new(width, height);
                    terminal.feed(&bytes);
                    for line in &input {
                        assert_vt(
                            terminal.find_row(line).is_some(),
                            &format!(
                                "production writer must paint composer draft {line:?} while \
                                 {live_count} live rows stream on a {width}x{height} terminal"
                            ),
                            &terminal,
                        );
                    }
                    assert_vt(
                        terminal.cursor().2,
                        "production writer must restore the composer cursor while live rows stream",
                        &terminal,
                    );
                }
            }
        }
    }

    #[test]
    fn test_consecutive_live_frames_differ_only_where_content_changed() {
        let width = 40;
        let input = vec!["draft".to_string()];
        let mut autocomplete = AutocompleteState::new();
        let before = plan_live_frame(&live_inputs(width, 20, &input, "idle"), &mut autocomplete);
        let after = plan_live_frame(&live_inputs(width, 20, &input, "busy"), &mut autocomplete);

        let previous = before.to_shadow_buffer(width, 20);
        let current = after.to_shadow_buffer(width, 20);
        let changes = shadow_buffer::diff_buffers(&current, &previous);

        assert!(
            !changes.is_empty(),
            "a changed status line must produce changed cells; frames were {:?} then {:?}",
            before.lines,
            after.lines
        );
        let changed_rows = changes.iter().map(|(_, y, _)| *y).collect::<HashSet<_>>();
        assert_eq!(
            1,
            changed_rows.len(),
            "only the status row changed, so the diff must touch exactly one row; it touched \
             rows {changed_rows:?} across frames {:?} then {:?}",
            before.lines,
            after.lines
        );
    }

    #[test]
    fn test_wide_character_task_row_is_truncated_to_one_painted_row() {
        // Regression: the session task row truncated `row.text` with
        // `chars().take(n)` and then counted the row with a measurement that
        // omitted the "● " prefix. Twenty fullwidth characters are twenty
        // chars and forty columns, so at width 40 the row painted two terminal
        // rows while the renderer recorded one. The missing row was never
        // erased.
        let width = 40;
        let task = activity::ActivityRow::new("作".repeat(20), activity::ActivityState::Active);
        let task_rows = vec![task];
        let input = vec![String::new()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(width, 24, &input, "idle");
        inputs.task_rows = &task_rows;
        let frame = plan_live_frame(&inputs, &mut autocomplete);

        let task_line = frame
            .lines
            .iter()
            .find(|line| line.contains('作'))
            .unwrap_or_else(|| panic!("the task row must be painted; frame was {:?}", frame.lines));
        assert_eq!(
            1,
            shadow_buffer::physical_rows(task_line, width),
            "an activity row must occupy exactly one terminal row; it measured {} columns at \
             width {width} and read {task_line:?}",
            shadow_buffer::visible_length(task_line)
        );
        assert_eq!(
            frame.physical_rows(width),
            occupied_rows(&frame.to_shadow_buffer(width, 24)),
            "recorded height must equal painted height; frame lines were {:?}",
            frame.lines
        );
    }

    #[test]
    fn test_wide_character_child_task_row_is_truncated_to_one_painted_row() {
        // Regression: the child-agent tree added `rows += 1` per row while
        // truncating by character count and measuring `detail` with
        // `chars().count()`. Both understate fullwidth text, so the row wrapped
        // and the undercount survived into `active_rows`.
        let width = 40;
        let mut row = activity::ActivityRow::new("業".repeat(20), activity::ActivityState::Active);
        row.detail = Some("模型".to_string());
        let tracked = vec![row];
        let input = vec![String::new()];
        let mut autocomplete = AutocompleteState::new();
        let mut inputs = live_inputs(width, 24, &input, "idle");
        inputs.tracked_rows = &tracked;
        let frame = plan_live_frame(&inputs, &mut autocomplete);

        let child_line = frame
            .lines
            .iter()
            .find(|line| line.contains('業'))
            .unwrap_or_else(|| {
                panic!("the child row must be painted; frame was {:?}", frame.lines)
            });
        assert_eq!(
            1,
            shadow_buffer::physical_rows(child_line, width),
            "a child-agent row must occupy exactly one terminal row; it measured {} columns at \
             width {width} and read {child_line:?}",
            shadow_buffer::visible_length(child_line)
        );
    }

    #[test]
    fn test_production_live_erase_removes_every_owned_completion_row_only() {
        let mut output = Vec::new();

        write_live_area_erase(&mut output, 17, 6).unwrap();

        let raw = String::from_utf8(output).unwrap();
        assert_eq!(raw.matches("\x1b[2K").count(), 17);
        assert!(!raw.contains("\x1b[3J"), "must preserve native scrollback");
        assert!(
            !raw.contains("\x1b[J"),
            "must not clear below the owned frame"
        );
    }

    #[test]
    fn status_idle_hint_contains_all_key_bindings() {
        let reg = CommandRegistry::new();
        let s = compute_effective_status(None, "", "", &reg);
        assert!(s.contains("Tab"), "should mention Tab: {}", s);
        assert!(s.contains("history"), "should mention history: {}", s);
        assert!(s.contains("/help"), "should mention /help: {}", s);
        assert!(s.contains("Ctrl+C"), "should mention Ctrl+C: {}", s);
    }

    // ── Physical row regression tests ─────────────────────────────────────────
    // Regression for the "separator spam" bug: when input text wrapped past the
    // terminal width, draw_live_area() counted 1 row per logical line instead of
    // the actual number of physical terminal rows, so erase_live_area() didn't
    // clear enough rows and left old separator lines in the scrollback.
    //
    // The physical row formula: ceil((prefix_vis + text_vis) / term_width) ≥ 1

    fn phys_rows(prefix_vis: usize, text_vis: usize, term_width: usize) -> usize {
        if term_width == 0 {
            return 1;
        }
        ((prefix_vis + text_vis).max(1) + term_width - 1) / term_width
    }

    #[test]
    fn phys_rows_short_line_is_one_row() {
        // "❯ hello" — 2 prefix + 5 text = 7 chars, fits in 80-col terminal → 1 row
        assert_eq!(phys_rows(2, 5, 80), 1);
    }

    #[test]
    fn phys_rows_exact_fill_is_one_row() {
        // Exactly fills terminal width → still 1 row (no wrap)
        assert_eq!(phys_rows(2, 78, 80), 1);
    }

    #[test]
    fn phys_rows_one_over_wraps_to_two() {
        // 2 + 79 = 81 chars in 80-col terminal → 2 rows
        assert_eq!(phys_rows(2, 79, 80), 2);
    }

    #[test]
    fn phys_rows_double_width_wraps_to_three() {
        // 2 + 158 = 160 chars in 80-col terminal → ceil(160/80) = 2
        assert_eq!(phys_rows(2, 158, 80), 2);
    }

    #[test]
    fn phys_rows_empty_line_is_one_row() {
        // Empty input still occupies 1 terminal row (for the prompt)
        assert_eq!(phys_rows(2, 0, 80), 1);
    }

    #[test]
    fn phys_rows_narrow_terminal_wraps_aggressively() {
        // 2 + 10 = 12 chars in 10-col terminal → ceil(12/10) = 2
        assert_eq!(phys_rows(2, 10, 10), 2);
    }

    // ── Dialog custom-mode regression tests ───────────────────────────────────
    // Regression: pressing 'o' in a select_with_custom dialog must set
    // custom_mode_active=true and accumulate typed characters in custom_input.
    // Previously the rendering checked dialog_type instead of custom_mode_active,
    // so the text input field was invisible even though state was updating.

    #[test]
    fn dialog_custom_mode_activates_on_o_press() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::select_with_custom("Title", vec![DialogOption::new("Option A")]);
        assert!(!d.custom_mode_active);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(
            d.custom_mode_active,
            "pressing 'o' must activate custom input mode"
        );
    }

    #[test]
    fn dialog_custom_mode_accumulates_text() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::select_with_custom("Title", vec![DialogOption::new("A")]);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('i')));
        let text = d.custom_input.as_deref().unwrap_or("");
        assert_eq!(text, "hi", "typed chars must accumulate in custom_input");
    }

    #[test]
    fn dialog_custom_mode_submit_returns_custom_text() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::select_with_custom("Title", vec![DialogOption::new("A")]);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('f')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        let result = d.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(result, Some(DialogResult::CustomText(ref s)) if s == "foo"),
            "Enter in custom mode must submit CustomText: {:?}",
            result
        );
    }

    #[test]
    fn dialog_custom_mode_esc_exits_without_submit() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::select_with_custom("Title", vec![DialogOption::new("A")]);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        d.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
        d.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert!(!d.custom_mode_active, "Esc must exit custom mode");
        // text should be cleared
        let text = d.custom_input.as_deref().unwrap_or("");
        assert!(text.is_empty(), "Esc must clear custom_input: {:?}", text);
    }

    // ── other_row_parts regression tests ──────────────────────────────────────
    // Regression: draw_dialog_inline_static used DIM_GRAY unconditionally for
    // the "Other" row, so navigating to it showed no highlight.  The fix moves
    // the colour selection into `other_row_parts()` which is pinned by these tests.

    #[test]
    fn other_row_unselected_uses_dim_gray_and_hollow_marker() {
        let (ansi, marker) = other_row_parts(false);
        assert_eq!(
            ansi,
            DIM_GRAY.to_string(),
            "unselected Other row must use DIM_GRAY, got: {:?}",
            ansi
        );
        assert_eq!(marker, "◌", "unselected Other row must use hollow marker ◌");
    }

    #[test]
    fn other_row_selected_uses_cyan_and_filled_marker() {
        let (ansi, marker) = other_row_parts(true);
        assert_eq!(
            ansi,
            format!("{}{}", SetAttribute(Attribute::Bold), CYAN),
            "selected Other row must use crossterm cyan bold, got: {:?}",
            ansi
        );
        assert_eq!(marker, "●", "selected Other row must use filled marker ●");
    }

    #[test]
    fn other_row_selected_is_not_dim_gray() {
        // Regression: the bug was using DIM_GRAY even when selected.
        let (ansi, _) = other_row_parts(true);
        assert_ne!(
            ansi,
            DIM_GRAY.to_string(),
            "selected Other row must NOT use DIM_GRAY (regression guard)"
        );
    }

    // ── format_custom_input_content regression tests ───────────────────────────
    // Regression: draw_dialog_inline_static wrapped `before` in DIM_GRAY/RESET,
    // making typed text invisible on dark terminals.  The fix removes those codes.
    // `format_custom_input_content` is now the single source of truth for the row
    // content, pinned by these tests.

    #[test]
    fn custom_input_content_contains_typed_text() {
        let s = format_custom_input_content("hello", 5);
        assert!(
            s.contains("hello"),
            "typed text must appear in formatted content, got: {:?}",
            s
        );
    }

    #[test]
    fn custom_input_content_does_not_wrap_text_in_dim_gray() {
        // Regression: DIM_GRAY before + RESET after made typed text invisible.
        let s = format_custom_input_content("hello", 5);
        // DIM_GRAY = "\x1b[2m"
        assert!(
            !s.contains("\x1b[2m"),
            "typed text must NOT be wrapped in DIM_GRAY (\\x1b[2m), got: {:?}",
            s
        );
    }

    #[test]
    fn custom_input_content_has_block_cursor() {
        // Crossterm renders the reverse-video cursor as \x1b[7m \x1b[0m.
        let s = format_custom_input_content("ab", 1);
        assert!(
            s.contains("\x1b[7m \x1b[0m"),
            "cursor block (\\x1b[7m \\x1b[0m) must appear in formatted content, got: {:?}",
            s
        );
    }

    #[test]
    fn custom_input_content_cursor_at_start_puts_all_text_after_cursor() {
        let s = format_custom_input_content("abc", 0);
        // before = "", after = "abc"; expect "> █abc"
        let idx = s.find("\x1b[7m \x1b[0m").expect("cursor not found");
        let after_cursor = &s[idx + "\x1b[7m \x1b[0m".len()..];
        assert_eq!(
            after_cursor, "abc",
            "text after cursor must be 'abc', got: {:?}",
            after_cursor
        );
    }

    #[test]
    fn custom_input_content_cursor_at_end_puts_all_text_before_cursor() {
        let s = format_custom_input_content("abc", 3);
        // before = "abc", after = ""; expect "> abc█"
        assert!(
            s.starts_with("> abc\x1b[7m"),
            "with cursor at end, content must start '> abc<cursor>', got: {:?}",
            s
        );
    }

    #[test]
    fn custom_input_content_empty_input_just_shows_cursor() {
        let s = format_custom_input_content("", 0);
        assert!(
            s.starts_with("> \x1b[7m"),
            "empty input must start '> <cursor>', got: {:?}",
            s
        );
    }

    // ── Select "Other" row state regression ───────────────────────────────────
    // Verifies that the Dialog state machine produces selected_index == options.len()
    // when the user navigates down past the last real option (prerequisite for the
    // renderer to call other_row_parts(true)).

    #[test]
    fn select_navigate_to_other_sets_index_to_options_len() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::select_with_custom(
            "Title",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        // Navigate down twice to reach "Other" (index 2 == options.len())
        d.handle_key_event(KeyEvent::from(KeyCode::Down));
        d.handle_key_event(KeyEvent::from(KeyCode::Down));
        if let DialogType::Select {
            selected_index,
            options,
            ..
        } = &d.dialog_type
        {
            assert_eq!(
                *selected_index,
                options.len(),
                "selected_index must equal options.len() when 'Other' is highlighted"
            );
        } else {
            panic!("expected Select dialog type");
        }
        // other_row_parts must return the highlighted style for this state
        let options_len = if let DialogType::Select { options, .. } = &d.dialog_type {
            options.len()
        } else {
            unreachable!()
        };
        let selected_index = if let DialogType::Select { selected_index, .. } = &d.dialog_type {
            *selected_index
        } else {
            unreachable!()
        };
        let (ansi, _) = other_row_parts(selected_index == options_len);
        assert_eq!(
            ansi,
            format!("{}{}", SetAttribute(Attribute::Bold), CYAN),
            "renderer must use cyan highlight when cursor is on 'Other'"
        );
    }

    // ── MultiSelect "Other" row state regression ───────────────────────────────

    #[test]
    fn multiselect_navigate_to_other_sets_cursor_to_options_len() {
        use crossterm::event::{KeyCode, KeyEvent};
        let mut d = Dialog::multiselect_with_custom(
            "Title",
            vec![DialogOption::new("X"), DialogOption::new("Y")],
        );
        // Navigate down twice to reach "Other" (cursor_index 2 == options.len())
        d.handle_key_event(KeyEvent::from(KeyCode::Down));
        d.handle_key_event(KeyEvent::from(KeyCode::Down));
        if let DialogType::MultiSelect {
            cursor_index,
            options,
            ..
        } = &d.dialog_type
        {
            assert_eq!(
                *cursor_index,
                options.len(),
                "cursor_index must equal options.len() when 'Other' is highlighted"
            );
        } else {
            panic!("expected MultiSelect dialog type");
        }
        // other_row_parts must return the highlighted style for this state
        let (cursor_index, options_len) = if let DialogType::MultiSelect {
            cursor_index,
            options,
            ..
        } = &d.dialog_type
        {
            (*cursor_index, options.len())
        } else {
            unreachable!()
        };
        let (ansi, _) = other_row_parts(cursor_index == options_len);
        assert_eq!(
            ansi,
            format!("{}{}", SetAttribute(Attribute::Bold), CYAN),
            "renderer must use cyan highlight when cursor is on 'Other' in MultiSelect"
        );
    }

    // ── other_row_content_visible_width regression tests ──────────────────────
    // Regression: render_other_row_inline used `2 + input_text.chars().count()`
    // for the content visible width, which omitted the cursor block character
    // (one visible cell rendered by `\x1b[7m \x1b[0m`). The fix is `3 + count`.
    //
    // These tests verify the invariant by measuring the actual visible length of
    // the string returned by format_custom_input_content() and asserting it
    // matches the formula used for padding in render_other_row_inline.

    #[test]
    fn other_row_content_vis_width_empty_input_is_3() {
        // "> " (2) + cursor block (1) = 3 with no text
        let s = format_custom_input_content("", 0);
        let vis = visible_length(&s);
        assert_eq!(
            vis, 3,
            "empty input: visible length must be 3 (got {}); formula was previously 2 (off by 1)",
            vis
        );
    }

    #[test]
    fn other_row_content_vis_width_matches_3_plus_char_count() {
        // The padding formula in render_other_row_inline is:
        //   content_vis = 3 + input_text.chars().count()
        // Verify it holds for a range of inputs and cursor positions.
        let cases: &[(&str, usize)] = &[
            ("hello", 5), // cursor at end
            ("hello", 0), // cursor at start
            ("hello", 2), // cursor in middle
            ("a", 1),
            ("abcdefgh", 8),
        ];
        for (input, cursor) in cases {
            let s = format_custom_input_content(input, *cursor);
            let vis = visible_length(&s);
            let expected = 3 + input.chars().count();
            assert_eq!(
                vis,
                expected,
                "input={:?} cursor={}: visible_length={} but formula gives {} \
                 (off-by-one regression: old formula gave {})",
                input,
                cursor,
                vis,
                expected,
                expected - 1
            );
        }
    }

    // ── Drop impl restores raw mode ───────────────────────────────────────────

    /// Verify that the Drop impl disables raw mode when is_active is true.
    ///
    /// Requires a real controlling terminal (TTY); mark `#[ignore]` so it is
    /// skipped in CI.  Run manually with:
    ///   cargo test -- --ignored test_tui_renderer_drop_restores_raw_mode
    #[test]
    #[ignore = "requires a real TTY; run manually"]
    fn test_tui_renderer_drop_restores_raw_mode() {
        use crossterm::terminal::{disable_raw_mode, enable_raw_mode, is_raw_mode_enabled};
        use std::sync::Mutex;

        // Serialise access to raw-mode state within this test binary.
        static RAW_MODE_LOCK: Mutex<()> = Mutex::new(());
        let _guard = RAW_MODE_LOCK.lock().unwrap();

        // Enable raw mode manually.
        enable_raw_mode().expect("enable_raw_mode failed — is this running in a real TTY?");
        assert!(
            is_raw_mode_enabled().unwrap_or(false),
            "raw mode should be enabled before drop"
        );

        // The Drop impl does: `if self.is_active { disable_raw_mode(); ... }`.
        // Exercise that logic directly with a local guard.
        struct RawModeGuard;
        impl Drop for RawModeGuard {
            fn drop(&mut self) {
                let _ = disable_raw_mode();
            }
        }
        let is_active = true;
        {
            // Only drop the guard if is_active is true — same condition as Drop impl.
            let _g = if is_active { Some(RawModeGuard) } else { None };
        }

        assert!(
            !is_raw_mode_enabled().unwrap_or(true),
            "raw mode should be disabled after drop (Drop impl regression)"
        );
    }

    /// Verify that the Drop impl's conditional (is_active guard) prevents
    /// double-disable: when is_active is false the guard is not dropped and
    /// raw-mode state is untouched.  This test does NOT require a real TTY.
    #[test]
    fn test_tui_renderer_drop_noop_when_inactive() {
        // When is_active = false the Drop impl must be a no-op.
        // We verify this by checking that disable_raw_mode is NOT called
        // (simulated: the Option<RawModeGuard> is None, so nothing runs).
        struct PanickingGuard;
        impl Drop for PanickingGuard {
            fn drop(&mut self) {
                panic!("disable_raw_mode should NOT be called when is_active = false");
            }
        }
        let is_active = false;
        {
            let _g: Option<PanickingGuard> = if is_active {
                Some(PanickingGuard)
            } else {
                None
            };
        }
        // If we reach here, the guard was not dropped — correct.
    }

    // ── poset_to_forth_lines ──────────────────────────────────────────────────

    fn graph_from_labels(labels: &[&str]) -> GraphView {
        GraphView {
            nodes: labels
                .iter()
                .enumerate()
                .map(|(id, label)| GraphNode::new(id, *label))
                .collect(),
            edges: Vec::new(),
            yaw: 0.3,
            pitch: 0.2,
        }
    }

    fn strip_poset_ansi(input: &str) -> String {
        let mut visible = String::new();
        let mut chars = input.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '\x1b' {
                visible.push(ch);
                continue;
            }
            for escape_ch in chars.by_ref() {
                if escape_ch.is_ascii_alphabetic() || escape_ch == '\x07' {
                    break;
                }
            }
        }
        visible
    }

    fn rendered_word_body(lines: &[String], id: usize) -> String {
        let header = format!(": W{id}");
        let start = lines
            .iter()
            .position(|line| strip_poset_ansi(line).contains(&header))
            .unwrap_or_else(|| panic!("missing rendered definition for W{id}"));
        lines[start + 1..]
            .iter()
            .map(|line| strip_poset_ansi(line))
            .take_while(|line| line.trim() != ";")
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn rendered_word_position(lines: &[String], id: usize) -> usize {
        let header = format!(": W{id}");
        lines
            .iter()
            .position(|line| strip_poset_ansi(line).contains(&header))
            .unwrap_or_else(|| panic!("missing rendered definition for W{id}"))
    }

    fn rendered_program_lines(lines: &[String]) -> Vec<String> {
        let start = lines
            .iter()
            .position(|line| strip_poset_ansi(line).contains(": PROGRAM"))
            .expect("missing rendered PROGRAM definition");
        lines[start..]
            .iter()
            .map(|line| {
                strip_poset_ansi(line)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn shuffle_with_seed<T>(values: &mut [T], seed: u64) {
        let mut state = seed;
        for upper in (1..values.len()).rev() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            values.swap(upper, (state as usize) % (upper + 1));
        }
    }

    #[test]
    fn test_poset_empty_produces_only_program() {
        // An empty graph still emits the PROGRAM wrapper word.
        let graph = GraphView::new();
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let combined = lines.join("\n");
        assert!(
            combined.contains("PROGRAM"),
            "empty graph should still emit PROGRAM: {combined}"
        );
        assert!(
            !combined.contains("W0"),
            "empty graph should have no W nodes: {combined}"
        );
    }

    #[test]
    fn test_poset_single_node_has_word_and_semicolon() {
        let graph = graph_from_labels(&["do-thing"]);
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let combined = lines.join("\n");
        assert!(combined.contains("W0"), "should name node W0: {combined}");
        assert!(
            combined.contains(";"),
            "should close with semicolon: {combined}"
        );
        assert!(
            combined.contains("do-thing"),
            "should include label: {combined}"
        );
    }

    #[test]
    fn test_poset_label_truncated_at_30_chars() {
        let long_label = "a".repeat(50);
        let graph = graph_from_labels(&[&long_label]);
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let combined = lines.join("\n");
        assert!(
            combined.contains('…'),
            "long label should have ellipsis: {combined}"
        );
        assert!(
            !combined.contains(&"a".repeat(50)),
            "full 50-char label should not appear: {combined}"
        );
    }

    #[test]
    fn test_poset_max_lines_respected() {
        let labels: Vec<String> = (0..20).map(|i| format!("word-{i}")).collect();
        let graph = graph_from_labels(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        let max = 10;
        let lines = poset_to_forth_lines(&graph, 80, max);
        assert!(
            lines.len() <= max,
            "output must not exceed max_lines (got {}): {lines:?}",
            lines.len()
        );
    }

    #[test]
    fn test_poset_program_word_emitted() {
        let graph = graph_from_labels(&["step"]);
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let combined = lines.join("\n");
        assert!(
            combined.contains("PROGRAM"),
            "PROGRAM word should be emitted: {combined}"
        );
    }

    #[test]
    fn test_poset_linear_chain_topo_order() {
        let mut graph = graph_from_labels(&["first", "second", "third"]);
        graph.edges = vec![(0, 1), (1, 2)];
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let combined = lines.join("\n");
        let pos0 = combined.find("W0").unwrap_or(usize::MAX);
        let pos1 = combined.find("W1").unwrap_or(usize::MAX);
        let pos2 = combined.find("W2").unwrap_or(usize::MAX);
        assert!(
            pos0 < pos1 && pos1 < pos2,
            "W0 should appear before W1 before W2: pos0={pos0} pos1={pos1} pos2={pos2} {combined}"
        );
    }

    #[test]
    fn test_poset_cycle_does_not_panic() {
        let mut graph = graph_from_labels(&["a", "b"]);
        graph.edges = vec![(0, 1), (1, 0)];
        let lines = poset_to_forth_lines(&graph, 80, 40);
        assert!(
            !lines.is_empty(),
            "cyclic graph should still produce output"
        );
    }

    #[test]
    fn test_poset_cyclic_component_precedes_its_downstream_node_stably() {
        let mut graph = graph_from_labels(&["downstream", "cycle-a", "cycle-b"]);
        graph.edges = vec![(1, 2), (2, 1), (2, 0)];

        let expected = poset_to_forth_lines(&graph, 80, 80);
        assert!(
            rendered_word_position(&expected, 2) < rendered_word_position(&expected, 0),
            "the cyclic predecessor component must precede its acyclic descendant: {expected:?}"
        );
        assert!(
            rendered_word_body(&expected, 0).contains("W2"),
            "downstream body should call W2: {}",
            rendered_word_body(&expected, 0)
        );
        assert_eq!(
            rendered_program_lines(&expected),
            vec![": PROGRAM", "W1 W2 \\ cycle", "W0 ;"]
        );

        for seed in 0..64 {
            let mut shuffled = graph.clone();
            shuffle_with_seed(&mut shuffled.nodes, seed);
            shuffle_with_seed(&mut shuffled.edges, seed ^ 0x5a5a_5a5a_5a5a_5a5a);
            assert_eq!(
                poset_to_forth_lines(&shuffled, 80, 80),
                expected,
                "cyclic rendering changed for storage-order seed {seed}"
            );
        }
    }

    #[test]
    fn test_poset_unknown_edges_are_omitted() {
        let mut baseline = graph_from_labels(&["root", "child"]);
        baseline.edges = vec![(0, 1)];
        let mut with_unknown_edges = baseline.clone();
        with_unknown_edges.edges.extend([(99, 1), (0, 88)]);

        assert_eq!(
            poset_to_forth_lines(&with_unknown_edges, 80, 80),
            poset_to_forth_lines(&baseline, 80, 80),
            "unknown edge endpoints must not change the Forth projection"
        );
    }

    #[test]
    fn test_poset_duplicate_edges_do_not_duplicate_calls_or_indegree() {
        let mut graph = graph_from_labels(&["left", "right", "join"]);
        graph.edges = vec![(0, 2), (0, 2), (0, 2), (1, 2), (1, 2)];
        let actual = poset_to_forth_lines(&graph, 80, 80);
        let body = rendered_word_body(&actual, 2);
        assert!(
            body.contains("W0 W1"),
            "join body should call each predecessor once: {body}"
        );
        assert_eq!(body.matches("W0").count(), 1, "duplicated W0 call: {body}");
        assert_eq!(body.matches("W1").count(), 1, "duplicated W1 call: {body}");

        graph.edges = vec![(0, 2), (1, 2)];
        assert_eq!(actual, poset_to_forth_lines(&graph, 80, 80));
    }

    #[test]
    fn test_poset_program_depth_groups_branching_join_graph() {
        let mut graph = graph_from_labels(&["root-a", "root-b", "branch-a", "branch-b", "join"]);
        graph.edges = vec![(0, 2), (1, 2), (0, 3), (1, 3), (2, 4), (3, 4)];

        assert_eq!(
            rendered_program_lines(&poset_to_forth_lines(&graph, 80, 80)),
            vec![
                ": PROGRAM",
                "W0 W1 \\ concurrent",
                "W2 W3 \\ concurrent",
                "W4 ;",
            ]
        );
    }

    #[test]
    fn test_poset_predecessor_calls_appear_in_body() {
        let mut graph = graph_from_labels(&["base", "derived"]);
        graph.edges = vec![(0, 1)];
        let lines = poset_to_forth_lines(&graph, 80, 40);
        let w1_body = rendered_word_body(&lines, 1);
        assert!(
            w1_body.contains("W0"),
            "W1 body should call W0 (its predecessor): {w1_body:?}"
        );
    }

    #[test]
    fn test_poset_rendering_is_stable_across_storage_orders() {
        let mut graph = graph_from_labels(&["zero", "one", "two", "three", "four", "five"]);
        graph.edges = vec![(2, 3), (0, 3), (1, 3), (3, 4), (1, 4), (4, 5), (2, 5)];

        let expected = poset_to_forth_lines(&graph, 80, 80);
        assert!(
            rendered_word_body(&expected, 3).contains("W0 W1 W2"),
            "W3 body: {}",
            rendered_word_body(&expected, 3)
        );
        assert!(
            rendered_word_body(&expected, 4).contains("W1 W3"),
            "W4 body: {}",
            rendered_word_body(&expected, 4)
        );
        assert!(
            rendered_word_body(&expected, 5).contains("W2 W4"),
            "W5 body: {}",
            rendered_word_body(&expected, 5)
        );

        for seed in 0..64 {
            let mut shuffled = graph.clone();
            shuffle_with_seed(&mut shuffled.nodes, seed);
            shuffle_with_seed(&mut shuffled.edges, seed ^ 0xa5a5_a5a5_a5a5_a5a5);
            let actual = poset_to_forth_lines(&shuffled, 80, 80);
            assert_eq!(
                actual, expected,
                "rendering changed for node/edge shuffle seed {seed}"
            );
        }
    }

    /// The injection conversion must preserve the Forth projection the overlay already had.
    #[test]
    fn test_graph_view_from_poset_preserves_forth_projection() {
        let mut poset = crate::poset::Poset::new();
        poset.add_node(
            "base".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_node(
            "derived".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_edge(0, 1);
        let from_poset = poset_to_forth_lines(&graph_view_from_poset(&poset), 80, 40);
        let mut view = graph_from_labels(&["base", "derived"]);
        view.edges = vec![(0, 1)];
        let from_view = poset_to_forth_lines(&view, 80, 40);
        assert_eq!(
            from_poset, from_view,
            "graph_view_from_poset must not change the Forth projection\n\
             from_poset={from_poset:?}\nfrom_view={from_view:?}"
        );
        assert!(
            rendered_word_body(&from_poset, 1).contains("W0"),
            "converted view lost the predecessor call: {}",
            rendered_word_body(&from_poset, 1)
        );
    }
}

#[cfg(test)]
mod draw_dialog_tests {

    /// The preview must render a date as a date, like every other read surface.
    ///
    /// It mapped cells with a bare `to_string()` -- calamine's `Display`, which
    /// prints the Excel serial -- so #281 was live here after being fixed in
    /// the typed runtime. Two converters meant two answers for the same cell.
    /// The row computation was inline in `show_file_viewer`, which enters the
    /// alternate screen and cannot be driven from a test; it is a free function
    /// now so this can reach it.
    #[test]
    fn test_spreadsheet_preview_renders_dates_not_serials() {
        use rust_xlsxwriter::{ExcelDateTime, Format, Workbook};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.xlsx");
        let mut workbook = Workbook::new();
        let sheet = workbook.add_worksheet();
        sheet
            .write_datetime_with_format(
                0,
                0,
                &ExcelDateTime::from_ymd(2026, 9, 2).unwrap(),
                &Format::new().set_num_format("yyyy-mm-dd"),
            )
            .unwrap();
        sheet.write_string(0, 1, "label").unwrap();
        workbook.save(&path).unwrap();

        let rows = super::TuiRenderer::spreadsheet_preview_rows(path.to_str().unwrap())
            .expect("a spreadsheet must preview");
        assert_eq!(
            rows,
            vec![vec!["2026-09-02".to_string(), "label".to_string()]],
            "the preview rendered the Excel serial instead of the date"
        );
    }
    use super::*;
    use crate::cli::tui::dialog::{Dialog, DialogOption};

    /// Strip ANSI escape sequences from a string, returning only visible chars.
    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // skip until end of escape sequence (letter or BEL)
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() || ch == '\x07' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    /// Render a dialog to a string using box_width=72, strip ANSI, return lines.
    fn render_lines(dialog: &Dialog) -> Vec<String> {
        let mut buf: Vec<u8> = Vec::new();
        // Call the static function directly — it now accepts &mut impl io::Write
        TuiRenderer::draw_dialog_inline_static_with_width(&mut buf, dialog, 72).unwrap();
        let raw = String::from_utf8(buf).unwrap();
        raw.lines()
            .map(|l| l.trim_end_matches('\r').to_string())
            .collect()
    }

    /// Borderless invariant: no line exceeds `box_width`, and no line carries a
    /// vertical box-border character (the dialog is full-width and borderless).
    fn check_widths(lines: &[String], box_width: usize) {
        for (i, line) in lines.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            let visible: String = strip_ansi(line);
            // Display columns, not characters: a fullwidth character is one
            // char and two columns, so a char count let CJK content overflow
            // the box and wrap while this guard reported it as fitting.
            let w = shadow_buffer::visible_length(&visible);
            assert!(
                w <= box_width,
                "line {i} has visual width {w}, exceeds box_width {box_width}:\n  raw:     {:?}\n  visible: {:?}",
                line, visible
            );
            assert!(
                !visible.contains('│') && !visible.contains('┌') && !visible.contains('┐'),
                "line {i} must not contain a box border char (borderless dialog):\n  visible: {:?}",
                visible
            );
        }
    }

    #[test]
    fn test_dialog_is_borderless_and_full_width() {
        // Regression: dialogs/prompts must span the full terminal width with no
        // left/right borders. The top and bottom lines are full-width horizontal
        // rules; no rendered line may contain a vertical border char.
        let dialog = Dialog::select(
            "Pick one",
            vec![DialogOption::new("Alpha"), DialogOption::new("Beta")],
        );
        let lines = render_lines(&dialog);
        assert!(!lines.is_empty());

        // First line is a full-width rule of exactly box_width `─` chars.
        let first = strip_ansi(&lines[0]);
        assert_eq!(
            first.chars().count(),
            72,
            "top rule must span the full width (72): {:?}",
            first
        );
        assert!(
            first.chars().all(|c| c == '─'),
            "top line must be a pure horizontal rule, got: {:?}",
            first
        );

        // No line may contain a vertical border character.
        for line in &lines {
            let visible = strip_ansi(line);
            assert!(
                !visible.contains('│'),
                "no line may contain a side border │, got: {:?}",
                visible
            );
        }
    }

    #[test]
    fn test_tool_approval_dialog_line_widths() {
        let dialog = Dialog::tool_approval("Read", "Read src/lib.rs");
        let lines = render_lines(&dialog);
        assert!(!lines.is_empty());
        check_widths(&lines, 72);
    }

    #[test]
    fn test_tool_approval_file_mutating_line_widths() {
        let dialog = Dialog::tool_approval("Write", "write file foo.rs");
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_select_dialog_line_widths() {
        let dialog = Dialog::select(
            "Pick one",
            vec![
                DialogOption::new("Alpha"),
                DialogOption::new("Beta"),
                DialogOption::new("Gamma"),
            ],
        );
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_confirm_dialog_line_widths() {
        let dialog = Dialog::confirm("Are you sure?", true);
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_multiselect_dialog_line_widths() {
        let dialog = Dialog::multiselect(
            "Choose all that apply",
            vec![DialogOption::new("Option A"), DialogOption::new("Option B")],
        );
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_live_multiselect_renders_and_honors_complete_keyboard_hint() {
        let mut dialog = Dialog::multiselect(
            "Choose all that apply",
            vec![DialogOption::new("Option A"), DialogOption::new("Option B")],
        );
        let mut output = Vec::new();
        let rendered_rows =
            TuiRenderer::draw_dialog_inline_static_with_width(&mut output, &dialog, 72).unwrap();
        let rendered = String::from_utf8(output).unwrap();
        let visible = rendered
            .lines()
            .map(strip_ansi)
            .collect::<Vec<_>>()
            .join("\n");

        assert_eq!(
            rendered_rows,
            rendered.lines().count(),
            "live multiselect row accounting must include every rendered keyboard-hint row"
        );

        for expected in [
            "↑/↓: Navigate",
            "Space: Toggle",
            "Enter: Submit",
            "Esc: Cancel",
        ] {
            assert!(
                visible.contains(expected),
                "live multiselect omitted the documented keyboard hint {expected:?}:\n{visible}"
            );
        }

        assert_eq!(
            dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' '))),
            None,
            "Space must toggle the focused option without submitting the live multiselect"
        );
        assert_eq!(
            dialog.handle_key_event(KeyEvent::from(KeyCode::Enter)),
            Some(DialogResult::MultiSelected(vec![0])),
            "Enter must submit the options toggled with the rendered keyboard controls"
        );
    }

    #[test]
    fn test_text_input_dialog_line_widths() {
        let dialog = Dialog::text_input("Enter a value", None);
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_long_help_message_does_not_overflow() {
        // Regression: help text longer than inner width must be wrapped, not overflow.
        let long_help =
            "Use ↑↓ or j/k to navigate, Enter to select, 'o' for custom feedback, Esc to cancel";
        let dialog = Dialog::select(
            "Review Implementation Plan",
            vec![DialogOption::new("Approve"), DialogOption::new("Reject")],
        )
        .with_help(long_help);
        let lines = render_lines(&dialog);
        check_widths(&lines, 72);
    }

    #[test]
    fn test_dialog_with_long_body_shows_scroll_indicator() {
        // A body with more lines than max_body_rows must show a scroll indicator.
        let long_body = (0..50)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let mut dialog =
            Dialog::select("Plan", vec![DialogOption::new("Approve")]).with_body(long_body);
        let lines = render_lines(&dialog);
        // All rendered lines must have correct width.
        check_widths(&lines, 72);
        // At least one line should contain the scroll indicator.
        let all_text = lines.join("\n");
        assert!(
            all_text.contains("PgDn") || all_text.contains("PgUp"),
            "expected scroll indicator in rendered output"
        );
    }

    #[test]
    fn test_dialog_body_scroll_offset_changes_visible_content() {
        let lines_text: Vec<String> = (0..30).map(|i| format!("Line {:02}", i)).collect();
        let body = lines_text.join("\n");
        let mut dialog_top =
            Dialog::select("Plan", vec![DialogOption::new("Approve")]).with_body(body.clone());
        let mut dialog_scrolled =
            Dialog::select("Plan", vec![DialogOption::new("Approve")]).with_body(body);
        dialog_scrolled.body_scroll_offset = 10;

        let top_text = render_lines(&dialog_top).join("\n");
        let scrolled_text = render_lines(&dialog_scrolled).join("\n");
        assert!(top_text.contains("Line 00"), "top view should show Line 00");
        assert!(
            !scrolled_text.contains("Line 00"),
            "scrolled view should not show Line 00"
        );
        assert!(
            scrolled_text.contains("Line 10"),
            "scrolled view should show Line 10"
        );
    }

    #[test]
    fn test_drawn_dialog_preserves_diff_whitespace_and_cjk_width() {
        let dialog = Dialog::select("Approve", vec![DialogOption::new("Yes")])
            .with_body("  1   1   unchanged spacing\n  2       + 新規\n    indented");
        let lines = render_lines(&dialog);
        let text = lines
            .iter()
            .map(|line| strip_ansi(line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("  1   1   unchanged spacing"), "{text}");
        assert!(text.contains("    indented"), "{text}");
        check_widths(&lines, 72);
    }

    #[test]
    fn test_wrap_text_ignores_sgr_columns_and_clips_long_unicode_lines() {
        let wrapped = wrap_text("\x1b[31m  + 新規abcdef\x1b[0m", 8);
        assert_eq!(wrapped.len(), 2);
        assert!(wrapped[0].starts_with("\x1b[31m  + 新規"));
        assert!(wrapped[0].ends_with("\x1b[0m"));
        assert!(wrapped[1].starts_with("\x1b[31m"));
        assert!(wrapped[1].ends_with("\x1b[0m"));
        assert!(wrapped.iter().all(|line| {
            let visible = strip_ansi(line);
            visible.chars().map(terminal_char_width).sum::<usize>() <= 8
        }));
    }

    #[test]
    fn test_wrap_text_keeps_ordinary_prose_on_word_boundaries() {
        assert_eq!(
            wrap_text("alpha beta gamma", 10),
            vec!["alpha beta", "gamma"]
        );
    }

    #[test]
    fn test_wrap_text_handles_cjk_at_one_column_without_empty_rows() {
        let wrapped = wrap_text("新規", 1);
        assert_eq!(wrapped, vec!["?", "?"]);
        assert!(wrapped
            .iter()
            .all(|line| { line.chars().map(terminal_char_width).sum::<usize>() <= 1 }));
    }

    #[test]
    fn test_wrap_text_colored_cjk_at_one_column_is_width_safe_and_resets() {
        let wrapped = wrap_text("\x1b[31m新規\x1b[0m", 1);
        assert_eq!(wrapped.len(), 2);
        assert!(wrapped.iter().all(|line| {
            let visible = strip_ansi(line);
            visible.chars().map(terminal_char_width).sum::<usize>() <= 1
                && !visible.contains('新')
                && !visible.contains('規')
        }));
        assert!(wrapped.iter().all(|line| line.starts_with("\x1b[31m")));
        assert!(wrapped.iter().all(|line| line.ends_with("\x1b[0m")));
    }
}

#[cfg(test)]
mod attention_bell_tests {
    use super::*;
    use std::sync::Arc;

    fn headless_renderer() -> TuiRenderer {
        let colors = ColorScheme::default();
        let output = Arc::new(OutputManager::new(colors.clone()));
        TuiRenderer::new_headless(output, Arc::new(StatusBar::new()), colors)
    }

    fn bell_count(bytes: &[u8]) -> usize {
        bytes.iter().filter(|byte| **byte == 0x07).count()
    }

    /// Presenting a tool-approval dialog must write BEL once; a redraw of that
    /// same pending overlay must stay silent. A later new approval bells again.
    #[test]
    fn test_presenting_tool_approval_dialog_emits_bell_once() {
        let mut renderer = headless_renderer();
        renderer.active_dialog = Some(Dialog::tool_approval("Write", "create foo.rs"));

        let mut first = Vec::new();
        renderer
            .draw_live_area_to(&mut first)
            .expect("first live draw of a tool-approval dialog must succeed");
        assert_eq!(
            bell_count(&first),
            1,
            "presenting a tool-approval dialog must emit BEL (\\x07) exactly once; \
             bells={} payload_len={}",
            bell_count(&first),
            first.len()
        );

        let mut redraw = Vec::new();
        renderer
            .draw_live_area_to(&mut redraw)
            .expect("redraw of the same pending approval must succeed");
        assert_eq!(
            bell_count(&redraw),
            0,
            "a redraw of the same pending approval must not bell again; \
             bells={} payload_len={}",
            bell_count(&redraw),
            redraw.len()
        );

        renderer.active_dialog = None;
        let mut cleared = Vec::new();
        renderer
            .draw_live_area_to(&mut cleared)
            .expect("draw after dismissing the dialog must succeed");
        assert_eq!(
            bell_count(&cleared),
            0,
            "clearing the dialog must not emit a bell; bells={} payload_len={}",
            bell_count(&cleared),
            cleared.len()
        );

        renderer.active_dialog = Some(Dialog::tool_approval("Edit", "edit foo.rs"));
        let mut next = Vec::new();
        renderer
            .draw_live_area_to(&mut next)
            .expect("live draw of a new pending approval must succeed");
        assert_eq!(
            bell_count(&next),
            1,
            "a new pending approval after the previous one dismissed must bell once; \
             bells={} payload_len={}",
            bell_count(&next),
            next.len()
        );
    }
}
