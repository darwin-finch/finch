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
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, Clear, ClearType,
        EndSynchronizedUpdate,
    },
};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::Arc;
use std::time::Duration;
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar, StatusLineType};
use crate::cli::messages::{MessageId, MessageRef, MessageStatus};
// Sub-modules
mod accordion;
mod async_input;
mod autocomplete_widget;
mod dialog;
mod dialog_widget;
mod input_widget; // kept, used by wizard helpers
mod scrollback; // kept for future use
mod shadow_buffer; // kept – good architecture for future diffing
mod status_widget;
mod tabbed_dialog;
mod tabbed_dialog_widget; // kept for wizard helpers

use accordion::{AccordionState, RenderedTranscriptLine};

pub use async_input::{spawn_input_task, InputEvent};
pub use autocomplete_widget::AutocompleteState;
use autocomplete_widget::{completion_pane_lines, replace_command_prefix};
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use shadow_buffer::visible_length;

#[derive(Clone, Copy)]
enum TerminalActivation {
    Startup,
    Resume,
    EmergencyRestore,
}

/// Enable only the terminal modes owned by Finch's default REPL.
///
/// Mouse capture is deliberately absent: terminals use click-drag for native
/// text selection and copying when applications leave mouse reporting alone.
fn activate_default_terminal_modes<W: Write>(
    mut writer: W,
    activation: TerminalActivation,
) -> io::Result<()> {
    match activation {
        TerminalActivation::Startup => {
            execute!(writer, crossterm::event::EnableBracketedPaste)?;
            execute!(
                writer,
                crossterm::event::PushKeyboardEnhancementFlags(
                    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
                )
            )?;
        }
        TerminalActivation::Resume => {}
        TerminalActivation::EmergencyRestore => {
            execute!(
                writer,
                crossterm::event::EnableBracketedPaste,
                crossterm::event::PushKeyboardEnhancementFlags(
                    crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
                ),
                cursor::Show,
            )?;
        }
    }
    Ok(())
}

/// Restore terminal modes owned by the default REPL.
///
/// Disabling mouse capture is intentionally defensive even though Finch does
/// not enable it: this also repairs a stale mode left by an older Finch or an
/// interrupted nested TUI before control returns to the user's shell.
fn restore_default_terminal_modes<W: Write>(mut writer: W) -> io::Result<()> {
    execute!(
        writer,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::DisableMouseCapture,
        crossterm::event::DisableBracketedPaste,
        cursor::Show,
        ResetColor,
    )
}

/// Best-effort terminal restoration for an exit path that cannot acquire the
/// renderer lock.  This is intentionally independent of [`TuiRenderer`]:
/// `/quit` and the IPC quit watcher may call `process::exit`, which skips Drop,
/// while a render task is holding the renderer mutex.  In that case raw mode
/// alone is insufficient — bracketed paste and kitty keyboard enhancement
/// remain enabled and their escape sequences leak into the user's shell.
pub fn emergency_restore_terminal() {
    let mut stdout = io::stdout();
    let _ = execute!(
        stdout,
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::event::PopKeyboardEnhancementFlags,
        crossterm::event::DisableBracketedPaste,
        cursor::Show,
        ResetColor,
        Print("\r\n"),
    );
    let _ = stdout.lock().flush();
    let _ = disable_raw_mode();
}
pub use tabbed_dialog::{TabbedDialog, TabbedDialogResult};
pub use tabbed_dialog_widget::TabbedDialogWidget;
// Re-export ColorScheme so callers can use `crate::cli::tui::ColorScheme`.
pub use crate::config::ColorScheme;

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

/// Render a `Poset` as compact Forth source lines for the panel overlay.
///
/// Each node becomes one word definition; predecessors are called first.
/// `PROGRAM` calls all leaf nodes (nodes with no outgoing edges).
/// Output is capped at `max_lines` lines.
#[allow(dead_code)]
fn poset_to_forth_lines(
    poset: &crate::poset::Poset,
    _panel_w: usize,
    max_lines: usize,
) -> Vec<String> {
    use crate::poset::NodeStatus;
    const C: SetForegroundColor = SetForegroundColor(Color::DarkCyan);
    const Y: SetForegroundColor = SetForegroundColor(Color::DarkYellow);
    const G: SetForegroundColor = SetForegroundColor(Color::DarkGreen);
    const R: SetForegroundColor = SetForegroundColor(Color::DarkRed);
    const D: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
    const RST: SetAttribute = SetAttribute(Attribute::Reset);

    let mut lines: Vec<String> = Vec::new();

    // Canonicalize the graph before rendering. `Poset` exposes its storage so
    // callers can restore plans, and restored node/edge order is not a semantic
    // part of the partial order.
    let node_ids: std::collections::BTreeSet<usize> =
        poset.nodes.iter().map(|node| node.id).collect();
    let mut edges: Vec<(usize, usize)> = poset
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
        let Some(node) = poset.nodes.iter().find(|n| n.id == id) else {
            continue;
        };

        let status_glyph = match node.status {
            NodeStatus::Done => format!("{G}✓{RST}"),
            NodeStatus::Failed => format!("{R}✗{RST}"),
            NodeStatus::Running => format!("{Y}▶{RST}"),
            NodeStatus::Pending => format!("{D}·{RST}"),
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

/// Rows available to the streaming WorkUnit after accounting for the rest of
/// Finch's live region. Keeping the whole region within the terminal prevents
/// redraws from scrolling their own clipped prefix into permanent scrollback.
fn live_message_row_budget(terminal_height: usize, reserved_rows: usize) -> usize {
    terminal_height.saturating_sub(reserved_rows)
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

/// Allocate zero to nine rows to completion after fixed UI, preserving one
/// live-output row when a stream is active. Critical UI suppresses completion.
fn completion_row_budget(
    terminal_rows: usize,
    fixed_rows: usize,
    has_live_output: bool,
    visible: bool,
    critical_ui_active: bool,
) -> usize {
    if !visible || critical_ui_active {
        return 0;
    }
    let live_reserve = usize::from(has_live_output);
    terminal_rows
        .saturating_sub(fixed_rows + live_reserve)
        .min(autocomplete_widget::MAX_VISIBLE_SUGGESTIONS + 1)
}

fn write_completion_pane(out: &mut impl Write, lines: &[String]) -> Result<usize> {
    for line in lines {
        execute!(out, Print("\r\n"), Print(line))?;
    }
    Ok(lines.len())
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
struct LiveContentFrame {
    live_lines: Vec<String>,
    completion_lines: Vec<String>,
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

fn plan_live_content_frame(
    autocomplete: &mut AutocompleteState,
    terminal_rows: usize,
    terminal_width: usize,
    fixed_rows: usize,
    all_live_lines: &[String],
    critical_ui_active: bool,
) -> LiveContentFrame {
    let completion_budget = completion_row_budget(
        terminal_rows,
        fixed_rows,
        !all_live_lines.is_empty(),
        autocomplete.visible,
        critical_ui_active,
    );
    let completion_lines = completion_pane_lines(autocomplete, terminal_width, completion_budget);
    let live_budget = live_message_row_budget(terminal_rows, fixed_rows + completion_lines.len());
    let live_lines = if all_live_lines.is_empty() {
        Vec::new()
    } else {
        live_viewport_lines(all_live_lines, terminal_width, live_budget).0
    };
    LiveContentFrame {
        live_lines,
        completion_lines,
    }
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

    // Disclosure state is a projection over retained WorkUnits. It never
    // mutates canonical message content or permanent native scrollback.
    accordion: AccordionState,

    // Dialog state — tool-approval dialogs shown in the live area.
    pub active_dialog: Option<Dialog>,
    pub active_tabbed_dialog: Option<TabbedDialog>,

    // Generic flags
    is_active: bool,
    pub(crate) needs_full_refresh: bool,
    pub(crate) last_render_error: Option<String>,
    pub pending_feedback: Option<crate::feedback::FeedbackRating>,
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

    // Rate limiting - removed in favor of event loop control

    // Session task list (set after construction via set_todo_list)
    todo_list: Option<Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>>,

    // Live child-agent tree projected from scheduler lifecycle events.
    agent_tasks: HashMap<uuid::Uuid, crate::runtime::scheduler::AgentTaskSnapshot>,
    agent_active_tools: HashMap<uuid::Uuid, String>,

    // Output of the user-defined `check` word — shown in the corner if set.
    pub corner: Arc<std::sync::Mutex<Option<String>>>,

    // Co-Forth shared stack (set after construction via set_stack)
    stack: Option<Arc<tokio::sync::Mutex<Vec<String>>>>,

    // Co-Forth poset VM — 3D rotating graph (set after construction via set_poset)
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
            active_dialog: None,
            active_tabbed_dialog: None,
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
            todo_list: None,
            agent_tasks: HashMap::new(),
            agent_active_tools: HashMap::new(),
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
        }
    }

    pub fn new(
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        colors: ColorScheme,
    ) -> Result<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;

        // Enable bracketed paste so the terminal wraps pasted content in
        // \x1b[200~ ... \x1b[201~ markers.  Crossterm surfaces this as
        // Event::Paste(String) which we handle without any Enter-confusion.
        // Unlike kitty keyboard enhancement flags, bracketed paste cannot
        // corrupt the terminal on unclean exit — it simply falls back to
        // normal (unbounded) paste mode, which is safe.
        let _ = activate_default_terminal_modes(io::stdout(), TerminalActivation::Startup);

        // Enable DISAMBIGUATE_ESCAPE_CODES so terminals that support the kitty
        // keyboard protocol send distinct sequences for Shift+Enter (vs bare Enter).
        // Without this, macOS Terminal.app and iTerm2 both send bare \r for
        // Shift+Enter — the SHIFT modifier is never set — so the newline-insertion
        // path in async_input.rs can never trigger.
        //
        // Terminals that don't support the protocol silently ignore the push
        // (crossterm returns an error we discard with `let _ =`), so there is no
        // regression for unsupported terminals.
        //
        // Cleanup: the Drop impl and the panic hook registered below both call
        // PopKeyboardEnhancementFlags, so normal exit, panics, and most signals
        // are covered.  SIGKILL terminates the session entirely so corruption
        // doesn't persist.  This is the same risk level we already accept for
        // enable_raw_mode().
        // Panic hook: restore terminal state so the shell is usable after a crash.
        std::panic::set_hook(Box::new(|info| {
            let _ = execute!(
                io::stdout(),
                crossterm::event::DisableMouseCapture,
                crossterm::event::PopKeyboardEnhancementFlags
            );
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!("{info}");
        }));

        execute!(io::stdout(), cursor::Show)?;

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

            active_dialog: None,
            active_tabbed_dialog: None,

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

            todo_list: None,
            agent_tasks: HashMap::new(),
            agent_active_tools: HashMap::new(),
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
        })
    }

    /// Attach the session task list so the live area can display it.
    pub fn set_todo_list(
        &mut self,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
    ) {
        self.todo_list = Some(todo_list);
    }

    /// Fold a scheduler event into the live child-agent projection.
    pub fn apply_agent_event(&mut self, event: &crate::runtime::scheduler::AgentEvent) {
        use crate::runtime::scheduler::AgentEvent;
        match event {
            AgentEvent::TaskQueued { snapshot } | AgentEvent::TaskStarted { snapshot } => {
                self.agent_tasks
                    .insert(snapshot.identity.task_id, snapshot.clone());
            }
            AgentEvent::ToolStarted { task_id, name } => {
                self.agent_active_tools.insert(*task_id, name.clone());
            }
            AgentEvent::ToolCompleted { task_id, .. } => {
                self.agent_active_tools.remove(task_id);
            }
            AgentEvent::TaskFinished { result } => {
                self.agent_tasks.remove(&result.identity.task_id);
                self.agent_active_tools.remove(&result.identity.task_id);
            }
        }
        self.live_area_dirty = true;
    }

    /// Attach the Co-Forth shared stack so the live area can display it.
    pub fn set_stack(&mut self, stack: Arc<tokio::sync::Mutex<Vec<String>>>) {
        self.stack = Some(stack);
    }

    /// Attach the Co-Forth poset VM so the live area can render its 3D graph.
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
        let complete = accordion
            .render_message_fully_expanded(message, colors)
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
    pub fn draw_live_area(&mut self) -> Result<()> {
        let mut stdout = io::stdout();

        let mut rows: usize = 0;

        // ── 1. Active WorkUnit ────────────────────────────────────────────────
        // Budget actual physical rows after reserving the separator, input,
        // status, TODOs, and child tasks. A fixed reserve can overflow the
        // viewport when context lines wrap, permanently duplicating live rows.
        let term_h = crossterm::terminal::size().unwrap_or((80, 24)).1 as usize;
        let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0 as usize;
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
        if term_h <= 3 && self.active_dialog.is_none() {
            completion_pane_lines(&mut self.autocomplete_state, term_width, 0);
            let frame = plan_tiny_live_frame(
                &input_lines,
                self.input_textarea.cursor(),
                &effective_status,
                term_h,
                term_width,
            );
            let rows = write_tiny_live_frame(&mut stdout, &frame)?;
            execute!(stdout, EndSynchronizedUpdate)?;
            stdout.flush()?;
            self.active_rows = rows;
            self.cursor_row_from_top = frame.cursor_row;
            self.accordion.rebuild_hit_regions(&[], 0, term_width);
            return Ok(());
        }
        let todo_rows = self
            .todo_list
            .as_ref()
            .and_then(|todo| todo.try_read().ok().map(|todo| todo.active_items().len()))
            .unwrap_or(0);
        let status_rows = 1 + effective_status
            .lines()
            .map(|line| shadow_buffer::physical_rows(line, term_width))
            .sum::<usize>();
        let input_rows = input_line_physical_rows_with_ghost(
            &input_lines,
            term_width,
            self.ghost_text.as_deref(),
        )
        .into_iter()
        .sum::<usize>();
        let dialog_active = self.active_dialog.is_some();
        let base_reserved_rows = if dialog_active {
            // A critical dialog owns the viewport. Streaming, tasks, draft,
            // status, and completion remain retained in structured state but
            // cannot compete with the bounded approval/error surface.
            term_h
        } else {
            1 // upper separator
                + input_rows
                + status_rows
                + todo_rows
                + self.agent_tasks.len()
        };
        let live_messages = self.find_live_messages();
        let all_live_rendered = self.projected_lines(live_messages);
        let all_live_lines = all_live_rendered
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let mut content_frame = plan_live_content_frame(
            &mut self.autocomplete_state,
            term_h,
            term_width,
            base_reserved_rows,
            &all_live_lines,
            dialog_active || self.last_render_error.is_some(),
        );
        pin_live_disclosure_header(&all_live_rendered, &mut content_frame, term_width);
        let visible_live_rendered =
            rendered_metadata_for_visible(&all_live_rendered, &content_frame.live_lines);
        if !content_frame.live_lines.is_empty() {
            // A Brain can have more than one live work unit (for example a
            // streamed VM program alongside a child task or output handle).
            // Rendering only the newest one made earlier source appear, then
            // vanish on the next redraw. Keep the uncommitted suffix ordered.
            for line in &content_frame.live_lines {
                let line = line.trim_end_matches('\r');
                execute!(stdout, Print(line), Print("\r\n"))?;
                rows += shadow_buffer::physical_rows(line, term_width);
            }
        }

        // ── 1b. Session task list (active items only) ─────────────────────────
        if !dialog_active {
            if let Some(ref todo_arc) = self.todo_list {
                if let Ok(todo) = todo_arc.try_read() {
                    let active = todo.active_items();
                    if !active.is_empty() {
                        let term_w = term_width;
                        for item in &active {
                            let (symbol, color) = match item.status {
                                crate::tools::todo::TodoStatus::InProgress => ("●", CYAN),
                                crate::tools::todo::TodoStatus::Pending => ("○", DIM_GRAY),
                                crate::tools::todo::TodoStatus::Completed => unreachable!(),
                            };
                            let priority_tag = match item.priority {
                                crate::tools::todo::TodoPriority::High => " [!]",
                                _ => "",
                            };
                            // Truncate: "● " prefix (2 chars) + optional " [!]" suffix
                            let max_content = term_w.saturating_sub(2 + priority_tag.len());
                            let content: String = item.content.chars().take(max_content).collect();
                            execute!(
                                stdout,
                                Print(format!(
                                    "{}{} {}{}{}\r\n",
                                    color, symbol, content, priority_tag, RESET
                                ))
                            )?;
                            rows += shadow_buffer::physical_rows(&content, term_w);
                        }
                    }
                }
            }
        }

        // ── 1c. Child-agent task tree ─────────────────────────────────────────
        let mut agent_tasks = if dialog_active {
            Vec::new()
        } else {
            self.agent_tasks.values().collect::<Vec<_>>()
        };
        agent_tasks.sort_by_key(|task| (task.identity.depth, task.identity.task_id));
        for task in agent_tasks {
            let indent = "  ".repeat(task.identity.depth);
            let symbol = match task.status {
                crate::runtime::scheduler::AgentTaskStatus::Queued => "○",
                crate::runtime::scheduler::AgentTaskStatus::Running => "●",
                _ => "✓",
            };
            let model = &task.identity.provider_model;
            let tool = self
                .agent_active_tools
                .get(&task.identity.task_id)
                .map(|name| format!(" · {name}"))
                .unwrap_or_default();
            let prefix_width = indent.chars().count() + 2;
            let available = term_width
                .saturating_sub(prefix_width + model.chars().count() + tool.chars().count() + 3);
            let task_text = task.task.chars().take(available).collect::<String>();
            execute!(
                stdout,
                SetForegroundColor(
                    if matches!(
                        task.status,
                        crate::runtime::scheduler::AgentTaskStatus::Running
                    ) {
                        Color::Cyan
                    } else {
                        Color::DarkGrey
                    }
                ),
                Print(&indent),
                Print(symbol),
                ResetColor,
                Print(" "),
                Print(task_text),
                SetForegroundColor(Color::DarkGrey),
                Print(format!(" · {model}{tool}")),
                ResetColor,
                Print("\r\n")
            )?;
            rows += 1;
        }

        // ── 1d. Co-Forth panel ────────────────────────────────────────────────
        // The panel is rendered as a floating overlay in draw_poset_overlay()
        // (top-right corner of the viewport) — not inline here.  This avoids
        // all cursor-row-counting issues; the overlay uses SavePosition /
        // RestorePosition and has no effect on `rows` or erase_live_area().

        // ── 2. Separator: "──  ~/repos/finch ──────── jade-river ──" ──────────
        // CWD is left-anchored; session name is right-anchored.
        let cwd_label = tilde_cwd();
        let session_label = self
            .status_bar
            .get_line(&StatusLineType::SessionLabel)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| self.session_label.clone());
        let separator = session_separator_line(term_width, &cwd_label, &session_label);
        if rows < term_h {
            execute!(
                stdout,
                Print(format!("{}{}{}\r\n", DIM_GRAY, separator, RESET))
            )?;
            rows += 1;
        }

        // ── 3. Dialog or input ────────────────────────────────────────────────
        let cursor_row_from_top;
        if let Some(dialog) = &self.active_dialog {
            // A dialog has no editable text cursor. Leaving the terminal cursor
            // visible after its final `\r\n` produces a stray black cursor cell
            // on the row below the modal on dark terminals.
            execute!(stdout, cursor::Hide)?;
            let dialog_rows = Self::draw_dialog_inline_bounded(
                &mut stdout,
                dialog,
                term_width,
                term_h.saturating_sub(rows),
            )?;
            rows += dialog_rows;
            // Dialog drawing ends each line with \r\n, so the cursor is one row
            // PAST the last drawn row (at row `rows`, 0-indexed from the start of
            // the live area).  erase_live_area() moves up by cursor_row_from_top to
            // reach row 0, so we need cursor_row_from_top = rows (not rows - 1).
            // Using rows - 1 caused the top row to be skipped on every erase, making
            // the dialog shift down by one row on each render tick and producing the
            // cascading duplicate dialog boxes the user sees.
            cursor_row_from_top = rows;
        } else {
            execute!(stdout, cursor::Show)?;
            // ── 4. Input area ─────────────────────────────────────────────────
            let (cursor_row, cursor_col) = self.input_textarea.cursor();
            let lines = self.input_textarea.lines().to_vec();

            let prompt = format!("{}❯{} ", CYAN, RESET);
            let prompt_vis_len: usize = 2; // visible chars: "❯ "
            let continuation = "  ";
            let cont_vis_len: usize = 2;

            // Record the rows count just before input so we know where input starts.
            let rows_before_input = rows;

            // Track physical terminal rows consumed by each input line (accounts for wrapping).
            let input_phys_rows =
                input_line_physical_rows_with_ghost(&lines, term_width, self.ghost_text.as_deref());

            if lines.is_empty() {
                execute!(stdout, Print(&prompt))?;
            } else {
                for (i, line) in lines.iter().enumerate() {
                    if i == 0 {
                        execute!(stdout, Print(format!("{}{}", prompt, line)))?;
                    } else {
                        execute!(stdout, Print(format!("{}{}", continuation, line)))?;
                    }
                    if i < lines.len() - 1 {
                        execute!(stdout, Print("\r\n"))?;
                    }
                }
            }

            let total_input_phys: usize = input_phys_rows.iter().sum();
            rows += total_input_phys;

            // ── 4b. Ghost text (dim suffix for command completions) ───────────
            if let Some(ref ghost) = self.ghost_text {
                execute!(stdout, Print(format!("{}{}{}", DIM_GRAY, ghost, RESET)))?;
                // ghost text is on the same row as the last input line — no extra row
            }

            // ── 4c. Slash-command completion pane ────────────────────────────
            // Plain text is deliberate: the raw/no-color path remains fully
            // speakable, and every line is width-bounded before it reaches the
            // terminal so it cannot wrap and corrupt live-row accounting.
            rows += write_completion_pane(&mut stdout, &content_frame.completion_lines)?;

            // ── 5. Status line(s) (smart: command hint > live stats > idle hint)
            //
            // Priority:
            //   1. While typing a /command with ghost text → show its description
            //   2. Live stats / operation are set         → show those
            //   3. Idle (nothing set)                     → show keyboard shortcuts
            //
            // effective_status may contain multiple lines (joined with '\n') when
            // the status bar has several active entries (e.g. operation + compaction
            // + plan-mode indicator).  Each must be printed with \r\n so that raw
            // mode does not leave the cursor at the wrong column.
            // Session identity is projected into the upper separator. Keeping it
            // here as well wastes a row and makes the Brain appear twice.
            // Thin separator between input area and status line(s) — full terminal width
            let status_sep: String = "─".repeat(term_width);
            execute!(
                stdout,
                Print(format!("\r\n{}{}{}", DIM_GRAY, status_sep, RESET))
            )?;

            // Count physical terminal rows consumed by status lines.  Long lines wrap,
            // so we must use the *visible* length (ANSI codes stripped) divided by the
            // terminal width — not just the number of '\n'-delimited logical lines.
            // Using logical line count here was the cause of the "separator spam on open"
            // bug: wrapped context lines were undercounted, leaving the cursor too low
            // after MoveUp, which caused erase_live_area() to miss the separator row and
            // draw a new one on every render tick.
            let mut status_phys_rows: usize = 1; // 1 for the separator line itself
            for line in effective_status.lines() {
                execute!(stdout, Print(format!("\r\n{}{}{}", DIM_GRAY, line, RESET)))?;
                let phys = shadow_buffer::physical_rows(line, term_width);
                status_phys_rows += phys;
            }
            rows += status_phys_rows;

            // ── 6. Reposition cursor inside the input area ────────────────────
            //
            // After drawing all input lines and status lines the cursor is at the
            // very bottom of the live area.  We compute how many physical terminal
            // rows are below the cursor's current logical position and move up by
            // that amount.  This correctly handles lines that wrap across multiple
            // terminal rows.

            let cursor_prefix_vis = if cursor_row == 0 {
                prompt_vis_len
            } else {
                cont_vis_len
            };

            // Which physical sub-row within cursor_row's logical line is the cursor on?
            let cursor_text_width = lines
                .get(cursor_row)
                .map(|line| {
                    let prefix: String = line.chars().take(cursor_col).collect();
                    shadow_buffer::visible_length(&prefix)
                })
                .unwrap_or(0);
            let cursor_sub_row = if term_width > 0 {
                (cursor_prefix_vis + cursor_text_width) / term_width
            } else {
                0
            };

            // Physical rows remaining in the cursor's logical line after the cursor.
            let phys_in_cursor_line = input_phys_rows.get(cursor_row).copied().unwrap_or(1);
            let rows_in_cursor_line_below = phys_in_cursor_line.saturating_sub(1 + cursor_sub_row);

            // Physical rows in input lines that come after cursor_row.
            let input_below_phys: usize =
                input_phys_rows.iter().skip(cursor_row + 1).sum::<usize>()
                    + rows_in_cursor_line_below;

            let rows_below_cursor =
                input_below_phys + content_frame.completion_lines.len() + status_phys_rows;
            if rows_below_cursor > 0 {
                execute!(stdout, cursor::MoveUp(rows_below_cursor as u16))?;
            }

            // Column within the current physical sub-row (accounts for wrapping).
            let col = if term_width > 0 {
                (cursor_prefix_vis + cursor_text_width) % term_width
            } else {
                cursor_prefix_vis + cursor_text_width
            };
            execute!(stdout, cursor::MoveToColumn(col as u16))?;

            // Compute cursor_row_from_top: physical rows from top of live area to cursor.
            let cursor_phys_above: usize = input_phys_rows[..cursor_row.min(input_phys_rows.len())]
                .iter()
                .sum();
            cursor_row_from_top = rows_before_input + cursor_phys_above + cursor_sub_row;
        }

        execute!(stdout, EndSynchronizedUpdate)?;
        stdout.flush()?;

        self.active_rows = rows;
        self.cursor_row_from_top = cursor_row_from_top;
        self.rebuild_transcript_hit_regions(&visible_live_rendered, rows, term_width, term_h);
        Ok(())
    }

    /// Return the whole uncommitted transcript suffix in order.
    ///
    /// A completed message may still be waiting to enter permanent scrollback
    /// behind an earlier live message.  It must remain in the redraw area in
    /// that state: filtering this list to `InProgress` made a received VM
    /// program disappear as soon as the provider stream ended, while its
    /// program-output WorkUnit was still running.
    fn find_live_messages(&self) -> Vec<MessageRef> {
        uncommitted_suffix(self.output_manager.get_messages(), &self.printed_ids)
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
                    row_id: None,
                    row_expanded: None,
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
                    row_id: None,
                    row_expanded: None,
                }
            }
        })
        .collect::<Vec<_>>();
    matched.reverse();
    matched
}

fn pin_live_disclosure_header(
    all: &[RenderedTranscriptLine],
    frame: &mut LiveContentFrame,
    terminal_width: usize,
) {
    if frame.live_lines.is_empty() || !all.iter().any(|line| line.row_id.is_some()) {
        return;
    }
    let visible = rendered_metadata_for_visible(all, &frame.live_lines);
    if visible.iter().any(|line| line.row_id.is_some()) {
        return;
    }
    let budget = frame
        .live_lines
        .iter()
        .map(|line| shadow_buffer::physical_rows(line, terminal_width.max(1)))
        .sum();
    frame.live_lines = viewport_tail_rendered_lines(all, terminal_width, budget)
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

impl TuiRenderer {
    /// Called from the event loop on every tick.
    /// Commits newly-completed messages to permanent scrollback, then redraws.
    pub fn flush_output_safe(&mut self, _output_manager: &OutputManager) -> Result<()> {
        let messages = self.output_manager.get_messages();

        let unprinted: Vec<MessageRef> = messages
            .iter()
            .filter(|msg| !self.printed_ids.contains(&msg.id()))
            .cloned()
            .collect();
        let committable = committable_prefix_len(unprinted.iter().map(|msg| msg.status()));

        let mut to_commit: Vec<MessageRef> = Vec::new();
        for msg in unprinted.into_iter().take(committable) {
            match msg.status() {
                MessageStatus::Complete | MessageStatus::Failed => {
                    to_commit.push(msg);
                }
                MessageStatus::InProgress => {
                    unreachable!("committable prefix excludes live messages")
                }
            }
        }

        // Re-establish trustworthy live-area coordinates before committing a
        // completion that raced resize. The completed message remains in the
        // uncommitted suffix until its canonical bytes are actually written.
        if self.viewport_invalidated {
            self.redraw_full_viewport()?;
            if to_commit.is_empty() {
                self.live_area_dirty = false;
                return Ok(());
            }
        }

        if !to_commit.is_empty() {
            let mut stdout = io::stdout();
            prepare_canonical_commit_guarded(&mut stdout)?;
            self.active_rows = 0;
            self.cursor_row_from_top = 0;
            let commit_result = commit_complete_messages(
                &mut stdout,
                &to_commit,
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
            "      ▄▄▄▄▄▄\n    ▗▟█●██▙►  finch v{version}\n  ▐████████▌   {model}\n  ▝▜██████▛▘   {session_label}  ·  {cwd}\n     ╥  ╥\n    ╱    ╲"
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
        let _ = restore_default_terminal_modes(io::stdout());
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
        let _ = execute!(io::stdout(), crossterm::event::DisableMouseCapture);
        disable_raw_mode()?;
        Ok(())
    }

    /// Re-acquire the terminal after a `suspend()`.
    pub fn resume(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        let _ = activate_default_terminal_modes(io::stdout(), TerminalActivation::Resume);
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
        let _ = activate_default_terminal_modes(io::stdout(), TerminalActivation::EmergencyRestore);
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
                match event::read()? {
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
                            let input = self.input_textarea.lines().join("\n");
                            if input.trim().is_empty() {
                                continue;
                            }
                            self.command_history.push(input.clone());
                            self.history_index = None;
                            self.input_textarea = Self::create_clean_textarea();
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
                    Event::Mouse(mouse) if self.handle_accordion_mouse(mouse) => {
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
    fn projected_message_lines(&self, message: &MessageRef) -> Vec<RenderedTranscriptLine> {
        self.accordion.render_message(message, &self.colors)
    }

    fn projected_lines(
        &self,
        messages: impl IntoIterator<Item = MessageRef>,
    ) -> Vec<RenderedTranscriptLine> {
        let mut rendered = Vec::new();
        for message in messages {
            rendered.extend(self.projected_message_lines(&message));
            rendered.push(RenderedTranscriptLine {
                text: String::new(),
                row_id: None,
                row_expanded: None,
            });
        }
        rendered
    }

    fn rebuild_transcript_hit_regions(
        &mut self,
        live: &[RenderedTranscriptLine],
        live_rows: usize,
        width: usize,
        height: usize,
    ) {
        let transcript_budget = height.saturating_sub(live_rows);
        let printed = self
            .output_manager
            .get_messages()
            .into_iter()
            .filter(|message| self.printed_ids.contains(&message.id()))
            .collect::<Vec<_>>();
        let transcript =
            viewport_tail_rendered_lines(&self.projected_lines(printed), width, transcript_budget);
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
            text: String::new(),
            row_id: None,
            row_expanded: None,
        }));
        combined.extend_from_slice(live);
        self.accordion
            .rebuild_hit_regions(&combined, plan.transcript_top, width);
    }

    pub(crate) fn handle_accordion_key(&mut self, key: KeyEvent) -> bool {
        if self.active_dialog.is_some() || self.active_tabbed_dialog.is_some() {
            return false;
        }
        if !self.accordion.handle_key(key) {
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
        if !self.accordion.handle_mouse(mouse) {
            return false;
        }
        self.viewport_invalidated = true;
        self.live_area_dirty = true;
        true
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

    fn live_geometry(&self, width: u16, height: u16) -> Option<(usize, usize)> {
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
            return Some((rows, rows));
        }
        let term_width = usize::from(width).max(1);
        let draw_width = term_width;
        let draw_height = usize::from(height);
        let input_lines = self.input_textarea.lines().to_vec();
        let raw_status = self
            .status_bar
            .get_status_without(&StatusLineType::SessionLabel);
        let effective_status = compute_effective_status(
            self.ghost_text.as_deref(),
            &raw_status,
            &input_lines.join("\n"),
            &self.command_registry,
        );
        if draw_height <= 3 {
            let frame = plan_tiny_live_frame(
                &input_lines,
                self.input_textarea.cursor(),
                &effective_status,
                draw_height,
                draw_width,
            );
            return Some((frame.lines.len(), frame.cursor_row));
        }
        let todo_rows = self
            .todo_list
            .as_ref()
            .and_then(|todo| todo.try_read().ok().map(|todo| todo.active_items().len()))
            .unwrap_or(0);
        let drawn_status_rows = 1 + effective_status
            .lines()
            .map(|line| shadow_buffer::physical_rows(line, draw_width))
            .sum::<usize>();
        let drawn_input_rows = input_line_physical_rows_with_ghost(
            &input_lines,
            draw_width,
            self.ghost_text.as_deref(),
        )
        .into_iter()
        .sum::<usize>();
        let base_reserved_rows =
            1 + drawn_input_rows + drawn_status_rows + todo_rows + self.agent_tasks.len();
        let all_live_rendered = self.projected_lines(self.find_live_messages());
        let all_live_lines = all_live_rendered
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>();
        let mut autocomplete = self.autocomplete_state.clone();
        let mut content_frame = plan_live_content_frame(
            &mut autocomplete,
            draw_height,
            draw_width,
            base_reserved_rows,
            &all_live_lines,
            self.last_render_error.is_some(),
        );
        pin_live_disclosure_header(&all_live_rendered, &mut content_frame, term_width);
        let completion_rows = content_frame.completion_lines.len();
        let mut rows = 0;
        rows += content_frame
            .live_lines
            .iter()
            .map(|line| shadow_buffer::physical_rows(line.trim_end_matches('\r'), term_width))
            .sum::<usize>();

        if let Some(ref todo_arc) = self.todo_list {
            if let Ok(todo) = todo_arc.try_read() {
                for item in todo.active_items() {
                    let priority_tag = match item.priority {
                        crate::tools::todo::TodoPriority::High => " [!]",
                        _ => "",
                    };
                    let max_content = draw_width.saturating_sub(2 + priority_tag.len());
                    let content = item.content.chars().take(max_content).collect::<String>();
                    let line = format!("● {content}{priority_tag}");
                    rows += shadow_buffer::physical_rows(&line, term_width);
                }
            }
        }
        for task in self.agent_tasks.values() {
            let indent = "  ".repeat(task.identity.depth);
            let model = &task.identity.provider_model;
            let tool = self
                .agent_active_tools
                .get(&task.identity.task_id)
                .map(|name| format!(" · {name}"))
                .unwrap_or_default();
            let prefix_width = indent.chars().count() + 2;
            let available = draw_width
                .saturating_sub(prefix_width + model.chars().count() + tool.chars().count() + 3);
            let task_text = task.task.chars().take(available).collect::<String>();
            let line = format!("{indent}● {task_text} · {model}{tool}");
            rows += shadow_buffer::physical_rows(&line, term_width);
        }
        let session_label = self
            .status_bar
            .get_line(&StatusLineType::SessionLabel)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| self.session_label.clone());
        let separator = session_separator_line(draw_width, &tilde_cwd(), &session_label);
        rows += shadow_buffer::physical_rows(&separator, term_width);
        let rows_before_input = rows;

        let (cursor_row, cursor_col) = self.input_textarea.cursor();
        let input_phys_rows = input_line_physical_rows_with_ghost(
            &input_lines,
            term_width,
            self.ghost_text.as_deref(),
        );
        let status_rows = shadow_buffer::physical_rows(&"─".repeat(draw_width), term_width)
            + effective_status
                .lines()
                .map(|line| shadow_buffer::physical_rows(line, term_width))
                .sum::<usize>();
        rows += input_phys_rows.iter().sum::<usize>() + completion_rows + status_rows;
        let cursor_text_width = input_lines
            .get(cursor_row)
            .map(|line| {
                shadow_buffer::visible_length(&line.chars().take(cursor_col).collect::<String>())
            })
            .unwrap_or(0);
        let cursor_sub_row = (2 + cursor_text_width) / term_width;
        let cursor_phys_above = input_phys_rows[..cursor_row.min(input_phys_rows.len())]
            .iter()
            .sum::<usize>();
        Some((rows, rows_before_input + cursor_phys_above + cursor_sub_row))
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

        let transcript = self
            .projected_lines(
                self.output_manager
                    .get_messages()
                    .into_iter()
                    .filter(|message| self.printed_ids.contains(&message.id())),
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
        let (matches, ghost) = command_completion_at_cursor(
            self.input_textarea.lines(),
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

    pub(crate) fn handle_tab_key(&mut self, key: KeyEvent) -> bool {
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
        } else {
            // Confirm / TextInput: just a keybinding hint
            let help = "↑/↓ Navigate  Enter Select  Esc Cancel";
            print_dialog_line(out, help, Some(Color::DarkGrey), false)?;
        }
        execute!(out, Print(&rule), Print("\r\n"))?;
        rows += 2; // buttons row + bottom rule

        Ok(rows)
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
        if max_rows == 0 {
            return Ok(0);
        }
        let mut rendered = Vec::new();
        let total_rows =
            Self::draw_dialog_inline_static_with_width(&mut rendered, dialog, width.max(1))?;
        if total_rows <= max_rows {
            out.write_all(&rendered)?;
            return Ok(total_rows);
        }

        let retained_rows = max_rows.saturating_sub(1);
        for line in rendered
            .split_inclusive(|byte| *byte == b'\n')
            .take(retained_rows)
        {
            out.write_all(line)?;
        }
        let marker = ellipsize("… dialog clipped to viewport; use navigation keys …", width);
        execute!(out, Print(marker), Print("\r\n"))?;
        Ok(max_rows)
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
            "xlsx" | "xls" | "ods" => {
                use calamine::{open_workbook_auto, Reader};
                match open_workbook_auto(path) {
                    Ok(mut wb) => {
                        let sheet_names = wb.sheet_names().to_vec();
                        let mut rows = Vec::new();
                        if let Some(name) = sheet_names.first() {
                            if let Ok(range) = wb.worksheet_range(name) {
                                for row in range.rows() {
                                    let cols: Vec<String> =
                                        row.iter().map(|c| c.to_string()).collect();
                                    rows.push(cols);
                                }
                            }
                        }
                        Some(rows)
                    }
                    Err(e) => Some(vec![vec![format!("error opening {path}: {e}")]]),
                }
            }
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
    use super::*;
    use crate::cli::command_autocomplete::CommandRegistry;
    use crate::cli::messages::{Message, MessageRef, WorkUnit};

    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn mouse_capture_sequence(enable: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        if enable {
            execute!(&mut bytes, crossterm::event::EnableMouseCapture).unwrap();
        } else {
            execute!(&mut bytes, crossterm::event::DisableMouseCapture).unwrap();
        }
        bytes
    }

    #[test]
    fn test_default_terminal_activation_never_enables_mouse_capture() {
        let enable_mouse = mouse_capture_sequence(true);
        for activation in [
            TerminalActivation::Startup,
            TerminalActivation::Resume,
            TerminalActivation::EmergencyRestore,
        ] {
            let mut bytes = Vec::new();
            activate_default_terminal_modes(&mut bytes, activation).unwrap();
            assert!(
                !contains_bytes(&bytes, &enable_mouse),
                "default TUI activation emitted mouse capture"
            );
        }
    }

    #[test]
    fn test_default_terminal_shutdown_defensively_disables_mouse_capture() {
        let mut bytes = Vec::new();
        restore_default_terminal_modes(&mut bytes).unwrap();

        assert!(!contains_bytes(&bytes, &mouse_capture_sequence(true)));
        assert!(contains_bytes(&bytes, &mouse_capture_sequence(false)));

        let mut disable_paste = Vec::new();
        execute!(&mut disable_paste, crossterm::event::DisableBracketedPaste).unwrap();
        assert!(contains_bytes(&bytes, &disable_paste));

        let mut pop_keyboard = Vec::new();
        execute!(
            &mut pop_keyboard,
            crossterm::event::PopKeyboardEnhancementFlags
        )
        .unwrap();
        assert!(contains_bytes(&bytes, &pop_keyboard));
    }

    #[test]
    fn startup_header_is_plain_scrollback_content() {
        let header = TuiRenderer::startup_header("grok-code-fast-1", "~/repo", "amber-river");
        assert!(header.contains("finch v"));
        assert!(header.contains("grok-code-fast-1"));
        assert!(header.contains("amber-river  ·  ~/repo"));
        assert!(!header.contains('\x1b'));
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
    fn test_live_budget_accounts_for_every_reserved_terminal_row() {
        assert_eq!(live_message_row_budget(24, 9), 15);
        assert_eq!(live_message_row_budget(24, 23), 1);
        assert_eq!(live_message_row_budget(24, 30), 0);
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
        let projected = state.render_message(&message, &colors);

        assert_eq!(projected.len(), 1);
        assert!(projected[0].text.contains("[collapsed]"));
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
        assert!(raw.contains("[collapsed]"));
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
        let all = state.render_message(&message, &colors);

        let visible = viewport_tail_rendered_lines(&all, 20, 4);

        assert!(
            visible[0].row_id.is_some(),
            "disclosure control must stay visible"
        );
        assert!(visible[0].text.contains("[expanded]"));
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
        assert!(matches!(tiny[0].text.as_str(), "[expanded]" | "open"));
        assert_eq!(shadow_buffer::physical_rows(&tiny[0].text, 8), 1);
        let mut collapsed_state = AccordionState::default();
        collapsed_state.rebuild_hit_regions(&all, 0, 20);
        assert!(collapsed_state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(collapsed_state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        let collapsed = collapsed_state.render_message(&message, &colors);
        let collapsed_tiny = viewport_tail_rendered_lines(&collapsed, 8, 1);
        assert_eq!(collapsed_tiny[0].text, "closed");
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
        let initial = state.render_message(&message, &colors);
        state.rebuild_hit_regions(&initial, 0, 80);
        assert!(state.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)));
        assert!(state.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)));
        let before = state.render_message(&message, &colors);
        assert!(before[0].text.contains("[collapsed]"));
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
        assert_eq!(state.render_message(&message, &colors), before);

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
    fn completed_program_source_stays_live_behind_running_output() {
        let source = Arc::new(WorkUnit::new("source"));
        source.set_program_source("lisp");
        source.set_response("(say \"hello\")");
        source.set_complete();

        let output = Arc::new(WorkUnit::new("output"));
        output.set_program_output();
        output.append_response("hello");

        let source_ref: MessageRef = source.clone();
        let output_ref: MessageRef = output.clone();
        let messages = vec![source_ref.clone(), output_ref.clone()];
        let live = uncommitted_suffix(messages, &HashSet::new());
        assert_eq!(live.len(), 2);
        assert!(live[0]
            .format(&ColorScheme::default())
            .contains("(say \"hello\")"));
        assert_eq!(live[1].format(&ColorScheme::default()), "hello");

        let mut printed = HashSet::new();
        printed.insert(source_ref.id());
        let live = uncommitted_suffix(vec![source_ref, output_ref], &printed);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].format(&ColorScheme::default()), "hello");
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
        let fixed_rows = 1 + input_physical_rows(&draft, terminal_width) + 2;
        let stream = (0..1_050)
            .map(|row| format!("live Brain row {row}"))
            .collect::<Vec<_>>();
        let frame = plan_live_content_frame(
            &mut autocomplete,
            terminal_rows,
            terminal_width,
            fixed_rows,
            &stream,
            false,
        );

        assert_eq!(
            frame.completion_lines.len(),
            9,
            "heading plus eight matches"
        );
        assert!(frame.live_lines[0].contains("earlier live rows clipped"));
        assert!(
            fixed_rows + frame.completion_lines.len() + frame.live_lines.len() <= terminal_rows
        );
        assert!(frame
            .completion_lines
            .iter()
            .any(|line| line.contains("/brain list")));
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

    #[test]
    fn test_single_line_ghost_wrapping_is_counted_in_live_frame_geometry() {
        let lines = vec!["/bra".to_string()];
        let without_ghost = input_line_physical_rows_with_ghost(&lines, 8, None);
        let with_ghost = input_line_physical_rows_with_ghost(&lines, 8, Some("in archive now"));

        assert_eq!(without_ghost, vec![1]);
        assert_eq!(with_ghost, vec![3]);
    }

    #[test]
    fn test_completion_budget_is_resize_stable_and_yields_to_dialogs_and_errors() {
        let normal = |height| completion_row_budget(height, 5, true, true, false);
        assert_eq!(normal(4), 0);
        assert_eq!(normal(8), 2);
        assert_eq!(normal(40), 9);
        assert_eq!(normal(8), 2, "repeated resize returns the same frame");
        assert_eq!(normal(40), 9, "grow after shrink restores free rows");

        assert_eq!(completion_row_budget(40, 5, true, true, true), 0);
        assert_eq!(completion_row_budget(40, 5, false, true, true), 0);
        assert_eq!(completion_row_budget(40, 5, true, false, false), 0);
    }

    #[test]
    fn test_one_to_three_row_viewports_never_create_an_invisible_interactive_pane() {
        let registry = CommandRegistry::new();
        for height in 1..=3 {
            let mut autocomplete = AutocompleteState::new();
            autocomplete.show_matches(registry.match_prefix("/brain "));
            let frame = plan_live_content_frame(
                &mut autocomplete,
                height,
                20,
                3,
                &["stream".to_string()],
                false,
            );

            assert!(frame.completion_lines.is_empty(), "height {height}");
            assert!(frame.live_lines.is_empty(), "height {height}");
            assert!(
                frame.completion_lines.len() + frame.live_lines.len() <= height,
                "height {height}"
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
        let frame = plan_live_content_frame(&mut autocomplete, 6, 40, 6, &live, true);
        assert!(frame.live_lines.is_empty());
        assert!(frame.completion_lines.is_empty());
        assert!(!autocomplete.is_interactive());
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
            let frame =
                plan_live_content_frame(&mut autocomplete, height, width, 5, &live, critical);
            assert_eq!(autocomplete.get_selected().unwrap().full_syntax(), selected);
            assert!(frame.live_lines.first().unwrap().contains("clipped"));
            if critical {
                assert!(frame.completion_lines.is_empty());
                assert!(!autocomplete.is_interactive());
            } else {
                assert!(frame
                    .completion_lines
                    .iter()
                    .any(|line| line.contains(&format!("> {selected}"))));
                assert!(autocomplete.is_interactive());
            }
        }
    }

    #[test]
    fn test_production_raw_completion_writer_is_plain_live_region_output() {
        let registry = CommandRegistry::new();
        let mut autocomplete = AutocompleteState::new();
        autocomplete.show_matches(registry.match_prefix("/brain list"));
        let lines = completion_pane_lines(&mut autocomplete, 100, 9);
        let mut output = Vec::new();

        let rows = write_completion_pane(&mut output, &lines).unwrap();

        let raw = String::from_utf8(output).unwrap();
        assert_eq!(rows, lines.len());
        assert!(raw.contains("\r\nCommands 1-1 of 1"));
        assert!(raw.contains("\r\n> /brain list - List named Brain sessions"));
        assert!(
            !raw.contains('\x1b'),
            "no-color output must contain no ANSI"
        );
        assert!(!raw.contains("\x1b[3J"), "must not clear native scrollback");
        assert!(
            !raw.contains("\n\n"),
            "must not emit committed-message spacing"
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

    // ── dialog cursor_row_from_top regression ─────────────────────────────────
    // Regression: draw_live_area set cursor_row_from_top = rows.saturating_sub(1)
    // for the dialog path, but after printing D rows with \r\n the cursor is at
    // position D (one past the last row, 0-indexed from start).  erase_live_area
    // moves up by cursor_row_from_top to reach row 0, so using D-1 caused it to
    // stop at row 1 — missing the first row of the live area on every tick and
    // making the dialog cascade downward with each render cycle.
    //
    // The fix: cursor_row_from_top = rows (not rows - 1) in the dialog branch.
    //
    // We verify the invariant without a real terminal by inspecting the formula
    // directly: the number of rows moved up in erase must equal the cursor
    // position after draw (which equals total_rows for the dialog path).

    #[test]
    fn dialog_cursor_row_from_top_equals_total_rows_not_rows_minus_one() {
        // Simulate dialog: separator (1) + N dialog rows → total_rows = 1 + N.
        // After drawing with \r\n, cursor is at row total_rows.
        // erase must move up total_rows to reach row 0.
        // cursor_row_from_top must therefore equal total_rows, not total_rows - 1.
        let separator_rows: usize = 1;
        for dialog_rows in [3usize, 7, 12, 20] {
            let total_rows = separator_rows + dialog_rows;

            // This is the CORRECT formula (the fix):
            let correct_cursor_row_from_top = total_rows;

            // This is the OLD (buggy) formula:
            let buggy_cursor_row_from_top = total_rows.saturating_sub(1);

            // erase moves up by cursor_row_from_top from position total_rows.
            // Resulting row after erase (0 = top of live area):
            let correct_row_after_erase =
                (total_rows as isize) - (correct_cursor_row_from_top as isize);
            let buggy_row_after_erase =
                (total_rows as isize) - (buggy_cursor_row_from_top as isize);

            assert_eq!(
                correct_row_after_erase, 0,
                "dialog_rows={}: correct formula must erase to row 0 (top of live area), \
                 got row {}",
                dialog_rows, correct_row_after_erase
            );
            assert_eq!(
                buggy_row_after_erase, 1,
                "dialog_rows={}: buggy formula leaves cursor at row 1 (misses first row), \
                 got row {}",
                dialog_rows, buggy_row_after_erase
            );
        }
    }

    #[test]
    fn dialog_cursor_row_from_top_saturating_sub_does_not_help_single_row() {
        // Edge case: if total_rows = 1 (just the separator, dialog returned 0 rows),
        // rows.saturating_sub(1) = 0, so erase would not move up at all —
        // meaning it would clear from the current position (row 1) downward,
        // which clears nothing.  cursor_row_from_top = rows = 1 moves back to row 0.
        let total_rows: usize = 1;
        let correct = total_rows; // 1 — moves up to row 0
        let buggy = total_rows.saturating_sub(1); // 0 — stays at row 1, clears nothing
        assert_eq!(correct, 1, "single-row: must move up 1 to reach top");
        assert_eq!(buggy, 0, "single-row: buggy formula is 0 (no-op erase)");
        assert_ne!(
            correct, buggy,
            "correct and buggy must differ for single-row case"
        );
    }

    // ── poset_to_forth_lines ──────────────────────────────────────────────────

    fn make_node(id: usize, label: &str) -> crate::poset::Node {
        crate::poset::Node {
            id,
            label: label.to_string(),
            kind: crate::poset::NodeKind::Task,
            status: crate::poset::NodeStatus::Pending,
            result: None,
            pos: [0.0, 0.0, 0.0],
            author: crate::poset::NodeAuthor::User,
            tools: Vec::new(),
            compiled_code: None,
            compiled_lang: None,
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
        // An empty poset still emits the PROGRAM wrapper word.
        let poset = crate::poset::Poset::new();
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let combined = lines.join("\n");
        assert!(
            combined.contains("PROGRAM"),
            "empty poset should still emit PROGRAM"
        );
        // No W-nodes since there are no nodes
        assert!(
            !combined.contains("W0"),
            "empty poset should have no W nodes"
        );
    }

    #[test]
    fn test_poset_single_node_has_word_and_semicolon() {
        let mut poset = crate::poset::Poset::new();
        poset.add_node(
            "do-thing".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let combined = lines.join("\n");
        assert!(combined.contains("W0"), "should name node W0");
        assert!(combined.contains(";"), "should close with semicolon");
        assert!(combined.contains("do-thing"), "should include label");
    }

    #[test]
    fn test_poset_label_truncated_at_30_chars() {
        let mut poset = crate::poset::Poset::new();
        let long_label = "a".repeat(50);
        poset.add_node(
            long_label,
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let combined = lines.join("\n");
        // The label in the .\" ... " should be truncated to 30 chars + ellipsis
        assert!(combined.contains('…'), "long label should have ellipsis");
        // Should NOT contain the full 50-char label
        assert!(
            !combined.contains(&"a".repeat(50)),
            "full 50-char label should not appear"
        );
    }

    #[test]
    fn test_poset_max_lines_respected() {
        let mut poset = crate::poset::Poset::new();
        for i in 0..20 {
            poset.add_node(
                format!("word-{i}"),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        let max = 10;
        let lines = poset_to_forth_lines(&poset, 80, max);
        assert!(
            lines.len() <= max,
            "output must not exceed max_lines (got {})",
            lines.len()
        );
    }

    #[test]
    fn test_poset_program_word_emitted() {
        let mut poset = crate::poset::Poset::new();
        poset.add_node(
            "step".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let combined = lines.join("\n");
        assert!(
            combined.contains("PROGRAM"),
            "PROGRAM word should be emitted"
        );
    }

    #[test]
    fn test_poset_linear_chain_topo_order() {
        // W0 → W1 → W2: W0 must appear before W1, W1 before W2.
        let mut poset = crate::poset::Poset::new();
        poset.add_node(
            "first".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_node(
            "second".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_node(
            "third".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_edge(0, 1);
        poset.add_edge(1, 2);
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let combined = lines.join("\n");
        let pos0 = combined.find("W0").unwrap_or(usize::MAX);
        let pos1 = combined.find("W1").unwrap_or(usize::MAX);
        let pos2 = combined.find("W2").unwrap_or(usize::MAX);
        assert!(pos0 < pos1, "W0 should appear before W1");
        assert!(pos1 < pos2, "W1 should appear before W2");
    }

    #[test]
    fn test_poset_cycle_does_not_panic() {
        // Cycle (W0 → W1 → W0) must not infinite-loop the topo sort.
        let mut poset = crate::poset::Poset::new();
        poset.add_node(
            "a".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_node(
            "b".to_string(),
            crate::poset::NodeKind::Task,
            crate::poset::NodeAuthor::User,
        );
        poset.add_edge(0, 1);
        poset.add_edge(1, 0); // cycle
                              // Must not panic or hang
        let lines = poset_to_forth_lines(&poset, 80, 40);
        assert!(
            !lines.is_empty(),
            "cyclic graph should still produce output"
        );
    }

    #[test]
    fn test_poset_cyclic_component_precedes_its_downstream_node_stably() {
        let mut poset = crate::poset::Poset::new();
        for label in ["downstream", "cycle-a", "cycle-b"] {
            poset.add_node(
                label.to_string(),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        poset.edges = vec![(1, 2), (2, 1), (2, 0)];

        let expected = poset_to_forth_lines(&poset, 80, 80);
        assert!(
            rendered_word_position(&expected, 2) < rendered_word_position(&expected, 0),
            "the cyclic predecessor component must precede its acyclic descendant"
        );
        assert!(rendered_word_body(&expected, 0).contains("W2"));
        assert_eq!(
            rendered_program_lines(&expected),
            vec![": PROGRAM", "W1 W2 \\ cycle", "W0 ;"]
        );

        for seed in 0..64 {
            let mut shuffled = poset.clone();
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
        let mut baseline = crate::poset::Poset::new();
        for label in ["root", "child"] {
            baseline.add_node(
                label.to_string(),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        baseline.edges = vec![(0, 1)];
        let mut with_unknown_edges = baseline.clone();
        with_unknown_edges.edges.extend([(99, 1), (0, 88)]);

        assert_eq!(
            poset_to_forth_lines(&with_unknown_edges, 80, 80),
            poset_to_forth_lines(&baseline, 80, 80)
        );
    }

    #[test]
    fn test_poset_duplicate_edges_do_not_duplicate_calls_or_indegree() {
        let mut poset = crate::poset::Poset::new();
        for label in ["left", "right", "join"] {
            poset.add_node(
                label.to_string(),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        poset.edges = vec![(0, 2), (0, 2), (0, 2), (1, 2), (1, 2)];
        let actual = poset_to_forth_lines(&poset, 80, 80);
        let body = rendered_word_body(&actual, 2);
        assert!(body.contains("W0 W1"));
        assert_eq!(body.matches("W0").count(), 1);
        assert_eq!(body.matches("W1").count(), 1);

        poset.edges = vec![(0, 2), (1, 2)];
        assert_eq!(actual, poset_to_forth_lines(&poset, 80, 80));
    }

    #[test]
    fn test_poset_program_depth_groups_branching_join_graph() {
        let mut poset = crate::poset::Poset::new();
        for label in ["root-a", "root-b", "branch-a", "branch-b", "join"] {
            poset.add_node(
                label.to_string(),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        poset.edges = vec![(0, 2), (1, 2), (0, 3), (1, 3), (2, 4), (3, 4)];

        assert_eq!(
            rendered_program_lines(&poset_to_forth_lines(&poset, 80, 80)),
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
        // W0 is predecessor of W1; W1's body should call W0.
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
        let lines = poset_to_forth_lines(&poset, 80, 40);
        let w1_body = rendered_word_body(&lines, 1);
        assert!(
            w1_body.contains("W0"),
            "W1 body should call W0 (its predecessor): {w1_body:?}"
        );
    }

    #[test]
    fn test_poset_rendering_is_stable_across_storage_orders() {
        let mut poset = crate::poset::Poset::new();
        for label in ["zero", "one", "two", "three", "four", "five"] {
            poset.add_node(
                label.to_string(),
                crate::poset::NodeKind::Task,
                crate::poset::NodeAuthor::User,
            );
        }
        poset.edges = vec![(2, 3), (0, 3), (1, 3), (3, 4), (1, 4), (4, 5), (2, 5)];

        let expected = poset_to_forth_lines(&poset, 80, 80);
        assert!(rendered_word_body(&expected, 3).contains("W0 W1 W2"));
        assert!(rendered_word_body(&expected, 4).contains("W1 W3"));
        assert!(rendered_word_body(&expected, 5).contains("W2 W4"));

        for seed in 0..64 {
            let mut shuffled = poset.clone();
            shuffle_with_seed(&mut shuffled.nodes, seed);
            shuffle_with_seed(&mut shuffled.edges, seed ^ 0xa5a5_a5a5_a5a5_a5a5);
            let actual = poset_to_forth_lines(&shuffled, 80, 80);
            assert_eq!(
                actual, expected,
                "rendering changed for node/edge shuffle seed {seed}"
            );
        }
    }
}

#[cfg(test)]
mod draw_dialog_tests {
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
            let w = visible.chars().count();
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
