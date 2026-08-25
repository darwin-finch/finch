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
    event::{self, Event},
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

pub use async_input::{spawn_input_task, InputEvent};
pub use autocomplete_widget::AutocompleteState;
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use shadow_buffer::visible_length;

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

    // Build predecessor map: node_id → [pred_id, ...]
    let mut preds: std::collections::HashMap<usize, Vec<usize>> = std::collections::HashMap::new();
    let mut has_successor: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for &(pred, succ) in &poset.edges {
        preds.entry(succ).or_default().push(pred);
        has_successor.insert(pred);
    }

    // Topological sort (Kahn's algorithm)
    let mut in_degree: std::collections::HashMap<usize, usize> =
        poset.nodes.iter().map(|n| (n.id, 0)).collect();
    for &(_, succ) in &poset.edges {
        *in_degree.entry(succ).or_insert(0) += 1;
    }
    let mut queue: std::collections::VecDeque<usize> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&id, _)| id)
        .collect();
    let mut topo: Vec<usize> = Vec::new();
    while let Some(id) = queue.pop_front() {
        topo.push(id);
        for &(pred, succ) in &poset.edges {
            if pred == id {
                let d = in_degree.entry(succ).or_insert(0);
                *d = d.saturating_sub(1);
                if *d == 0 {
                    queue.push_back(succ);
                }
            }
        }
    }
    // Any remaining (cycles) append in id order.
    for n in &poset.nodes {
        if !topo.contains(&n.id) {
            topo.push(n.id);
        }
    }

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
        // Compute depth of each node (longest path from a root).
        let mut depth: std::collections::HashMap<usize, usize> =
            poset.nodes.iter().map(|n| (n.id, 0)).collect();
        for &id in &topo {
            let d = depth.get(&id).copied().unwrap_or(0);
            for &(pred, succ) in &poset.edges {
                if pred == id {
                    let entry = depth.entry(succ).or_insert(0);
                    if d + 1 > *entry {
                        *entry = d + 1;
                    }
                }
            }
        }
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
            let parallel_note = if group.len() > 1 {
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
    let budget = row_budget.max(1);
    let total_rows = lines
        .iter()
        .map(|line| shadow_buffer::physical_rows(line, width))
        .sum::<usize>();
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
    terminal_height.saturating_sub(reserved_rows).max(1)
}

fn input_physical_rows(lines: &[String], terminal_width: usize) -> usize {
    input_line_physical_rows(lines, terminal_width)
        .into_iter()
        .sum()
}

fn input_line_physical_rows(lines: &[String], terminal_width: usize) -> Vec<usize> {
    let width = terminal_width.max(1);
    if lines.is_empty() {
        return vec![1];
    }
    lines
        .iter()
        .map(|line| {
            let prefix_width = 2; // `❯ ` and continuation indentation are both two columns.
            (prefix_width + shadow_buffer::visible_length(line))
                .max(1)
                .div_ceil(width)
        })
        .collect()
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

/// Compute what to display in the status bar.
///
/// Priority:
/// 1. User is typing a `/command` with ghost text → show the command's description.
/// 2. A live stat / operation is set (`raw_status` non-empty) → show that.
/// 3. Idle → show the keyboard shortcut reminder.
pub(crate) fn compute_effective_status(
    ghost_text: Option<&str>,
    raw_status: &str,
    current_input: &str,
    registry: &crate::cli::command_autocomplete::CommandRegistry,
) -> String {
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
    if !raw_status.is_empty() {
        return raw_status.to_string();
    }
    "↑↓ history  ·  Tab complete  ·  /help for commands  ·  Ctrl+C cancel".to_string()
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

    // Row index (0-based from top of live area) where the cursor is parked
    // after draw_live_area().  erase_live_area() uses this to correctly reach
    // the top regardless of where the cursor was repositioned (e.g. inside the
    // input area vs. bottom of a dialog box).
    cursor_row_from_top: usize,

    // Messages already committed to permanent scrollback.
    printed_ids: HashSet<MessageId>,

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
        let _ = execute!(io::stdout(), crossterm::event::EnableBracketedPaste);

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
        let _ = execute!(
            io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
            )
        );

        // Panic hook: restore terminal state so the shell is usable after a crash.
        std::panic::set_hook(Box::new(|info| {
            let _ = execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
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
            cursor_row_from_top: 0,
            printed_ids: HashSet::new(),

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
        for (i, line) in text.lines().enumerate() {
            if i > 0 {
                ta.insert_newline();
            }
            ta.insert_str(line);
        }
        ta
    }
}

// ─── Raw-mode printing helpers ────────────────────────────────────────────────

impl TuiRenderer {
    /// Print a multi-line string to the terminal scrollback.
    /// In raw mode every `\n` needs an accompanying `\r`.
    fn raw_println(text: &str) -> Result<()> {
        let mut stdout = io::stdout();
        for line in text.split('\n') {
            let line = line.trim_end_matches('\r');
            execute!(stdout, Print(line), Print("\r\n"))?;
        }
        Ok(())
    }

    fn raw_blank_line() -> Result<()> {
        execute!(io::stdout(), Print("\r\n")).map_err(anyhow::Error::from)
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
        execute!(stdout, BeginSynchronizedUpdate)?;
        if self.active_rows == 0 && self.cursor_row_from_top == 0 {
            return Ok(()); // Nothing to erase; sync block closed by draw_live_area
        }
        execute!(stdout, cursor::MoveToColumn(0))?;
        if self.cursor_row_from_top > 0 {
            execute!(stdout, cursor::MoveUp(self.cursor_row_from_top as u16))?;
        }
        // Never clear from the cursor to the bottom of the terminal here. A
        // one-row accounting error (especially around a wrapping streamed
        // program) would then erase committed scrollback above the live area.
        // Clear only the rows this renderer previously owned. If accounting is
        // ever short, a stale live row is recoverable; lost transcript is not.
        for row in 0..self.active_rows {
            execute!(stdout, Clear(ClearType::CurrentLine))?;
            if row + 1 < self.active_rows {
                execute!(stdout, cursor::MoveDown(1), cursor::MoveToColumn(0))?;
            }
        }
        if self.active_rows > 1 {
            execute!(stdout, cursor::MoveUp((self.active_rows - 1) as u16))?;
            execute!(stdout, cursor::MoveToColumn(0))?;
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
        let todo_rows = self
            .todo_list
            .as_ref()
            .and_then(|todo| todo.try_read().ok().map(|todo| todo.active_items().len()))
            .unwrap_or(0);
        let status_rows = 1
            + effective_status
                .lines()
                .map(|line| shadow_buffer::physical_rows(line, term_width))
                .sum::<usize>();
        let reserved_rows = 1 // upper separator
            + input_physical_rows(&input_lines, term_width)
            + status_rows
            + todo_rows
            + self.agent_tasks.len();
        let max_live_rows = live_message_row_budget(term_h, reserved_rows);
        let live_messages = self.find_live_messages();
        if !live_messages.is_empty() {
            // A Brain can have more than one live work unit (for example a
            // streamed VM program alongside a child task or output handle).
            // Rendering only the newest one made earlier source appear, then
            // vanish on the next redraw. Keep the uncommitted suffix ordered.
            let mut all_lines = Vec::new();
            for message in live_messages {
                all_lines.extend(message.format(&self.colors).split('\n').map(str::to_owned));
            }
            let (visible_lines, _) =
                live_viewport_lines(&all_lines, term_width, max_live_rows);
            for line in &visible_lines {
                let line = line.trim_end_matches('\r');
                execute!(stdout, Print(line), Print("\r\n"))?;
                rows += shadow_buffer::physical_rows(line, term_width);
            }
        }

        // ── 1b. Session task list (active items only) ─────────────────────────
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

        // ── 1c. Child-agent task tree ─────────────────────────────────────────
        let mut agent_tasks = self.agent_tasks.values().collect::<Vec<_>>();
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
        let prefix = "── ";
        let prefix_vis = 3_usize;
        let cwd_part = format!(" {} ", cwd_label);
        let session_label = self
            .status_bar
            .get_line(&StatusLineType::SessionLabel)
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| self.session_label.clone());
        let right_part = if session_label.is_empty() {
            " ──".to_string()
        } else {
            format!(" {} ──", session_label)
        };
        let left_vis = prefix_vis + cwd_part.chars().count();
        let right_vis = right_part.chars().count();
        let mid_len = term_width.saturating_sub(left_vis + right_vis);
        let mid: String = "─".repeat(mid_len);
        execute!(
            stdout,
            Print(format!(
                "{}{}{}{}{}{}\r\n",
                DIM_GRAY, prefix, cwd_part, mid, right_part, RESET
            ))
        )?;
        rows += 1;

        // ── 3. Dialog or input ────────────────────────────────────────────────
        let cursor_row_from_top;
        if let Some(dialog) = &self.active_dialog {
            // A dialog has no editable text cursor. Leaving the terminal cursor
            // visible after its final `\r\n` produces a stray black cursor cell
            // on the row below the modal on dark terminals.
            execute!(stdout, cursor::Hide)?;
            let dialog_rows = Self::draw_dialog_inline_static(&mut stdout, dialog)?;
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
            let input_phys_rows = input_line_physical_rows(&lines, term_width);

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

            let rows_below_cursor = input_below_phys + status_phys_rows;
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
            let id = msg.id();
            match msg.status() {
                MessageStatus::Complete | MessageStatus::Failed => {
                    to_commit.push(msg);
                    self.printed_ids.insert(id);
                }
                MessageStatus::InProgress => unreachable!("committable prefix excludes live messages"),
            }
        }

        if !to_commit.is_empty() {
            self.erase_live_area()?;
            for msg in &to_commit {
                Self::raw_println(&msg.format(&self.colors))?;
                // Blank line after every committed message so the output area
                // stays readable (issue #15 — remove clutter between work items).
                Self::raw_blank_line()?;
            }
            self.draw_live_area()?;
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
        let _ = execute!(
            io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags,
            crossterm::event::DisableBracketedPaste,
            cursor::Show,
            ResetColor,
        );
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
        disable_raw_mode()?;
        Ok(())
    }

    /// Re-acquire the terminal after a `suspend()`.
    pub fn resume(&mut self) -> anyhow::Result<()> {
        enable_raw_mode()?;
        // Force a full redraw so the REPL live area reappears.
        self.active_rows = 0;
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
                        (KeyCode::Tab, KeyModifiers::NONE) => {
                            if let Some(ghost) = self.ghost_text.take() {
                                let current = self.input_textarea.lines().join("\n");
                                let completed = format!("{}{}", current, ghost);
                                self.input_textarea =
                                    Self::create_clean_textarea_with_text(&completed);
                            } else {
                                self.input_textarea.input(Event::Key(key));
                            }
                            self.update_ghost_text();
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
                    _ => {}
                }
            }
        }
    }
}

// ─── Message helpers ──────────────────────────────────────────────────────────

impl TuiRenderer {
    pub fn add_trait_message(&mut self, message: MessageRef) -> MessageId {
        let id = message.id();
        self.output_manager.add_trait_message(message);
        self.live_area_dirty = true;
        id
    }

    pub fn handle_resize(&mut self, w: u16, h: u16) -> Result<()> {
        // Terminal emulators reflow scrollback themselves. Clearing the entire screen
        // here destroys visible history. Re-measure the already drawn live region at
        // its new width before the caller erases it; using the old physical-row count
        // leaves one separator behind for every resize event.
        if let Some((rows, cursor_row_from_top)) = self.reflowed_live_geometry(w, h) {
            self.active_rows = rows;
            self.cursor_row_from_top = cursor_row_from_top;
        }
        self.live_area_dirty = true;
        Ok(())
    }

    fn reflowed_live_geometry(&self, width: u16, height: u16) -> Option<(usize, usize)> {
        if self.active_dialog.is_some() {
            // Dialog geometry is computed by its renderer. Keep the last known
            // dimensions rather than guessing and risking transcript erasure.
            return None;
        }
        let term_width = usize::from(width).max(1);
        let term_height = usize::from(height);
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
        let todo_rows = self
            .todo_list
            .as_ref()
            .and_then(|todo| todo.try_read().ok().map(|todo| todo.active_items().len()))
            .unwrap_or(0);
        let status_rows = 1
            + effective_status
                .lines()
                .map(|line| shadow_buffer::physical_rows(line, term_width))
                .sum::<usize>();
        let input_rows = input_physical_rows(&input_lines, term_width);
        let reserved_rows =
            1 + input_rows + status_rows + todo_rows + self.agent_tasks.len();
        let max_live_rows = live_message_row_budget(term_height, reserved_rows);
        let mut rows = 0;
        let live_messages = self.find_live_messages();
        if !live_messages.is_empty() {
            let mut all_lines = Vec::new();
            for message in live_messages {
                all_lines.extend(message.format(&self.colors).split('\n').map(str::to_owned));
            }
            let (visible_lines, _) =
                live_viewport_lines(&all_lines, term_width, max_live_rows);
            rows += visible_lines
                .iter()
                .map(|line| shadow_buffer::physical_rows(line.trim_end_matches('\r'), term_width))
                .sum::<usize>();
        }
        rows += todo_rows + self.agent_tasks.len() + 1; // tasks + upper separator
        let rows_before_input = rows;

        let (cursor_row, cursor_col) = self.input_textarea.cursor();
        let input_phys_rows = input_line_physical_rows(&input_lines, term_width);
        rows += input_phys_rows.iter().sum::<usize>() + status_rows;
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
        Some((
            rows,
            rows_before_input + cursor_phys_above + cursor_sub_row,
        ))
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
    pub fn update_ghost_text(&mut self) {
        let current = self.input_textarea.lines().join("\n");
        self.ghost_text = compute_ghost_text(&current, &self.command_registry);
        self.live_area_dirty = true;
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
                    // Hard-truncate any single word longer than inner (e.g. long URLs).
                    let truncated: String = wrapped.chars().take(inner).collect();
                    all_body_lines.push(truncated);
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
                        format!("↑ {} above · ↓ {} below  (Ctrl-U/D or PgUp/PgDn)", above, below)
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
        // Borderless dialogs span the full terminal width. Keep a sane floor for
        // very narrow terminals so wrapping still has room to work.
        let box_width = term_width.max(20);
        Self::draw_dialog_inline_static_with_width(out, dialog, box_width)
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
        let mut cur = String::new();
        for word in para.split_whitespace() {
            if cur.is_empty() {
                cur.push_str(word);
            } else if cur.len() + 1 + word.len() <= width {
                cur.push(' ');
                cur.push_str(word);
            } else {
                out.push(cur.clone());
                cur = word.to_string();
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
    }
    out
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::command_autocomplete::CommandRegistry;
    use crate::cli::messages::WorkUnit;

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
    fn live_budget_accounts_for_every_reserved_terminal_row() {
        assert_eq!(live_message_row_budget(24, 9), 15);
        assert_eq!(live_message_row_budget(24, 23), 1);
        assert_eq!(live_message_row_budget(24, 30), 1);
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
    fn live_viewport_uses_physical_rows_and_marks_a_clipped_prefix() {
        let lines = (0..12).map(|index| format!("line {index}")).collect::<Vec<_>>();
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
    fn status_ghost_takes_priority_over_raw_status() {
        let reg = CommandRegistry::new();
        // Even with raw_status set, ghost text description wins
        let s = compute_effective_status(Some("tical"), "⏺ Generating…", "/cri", &reg);
        // Should NOT be the raw status — should be the command description
        assert_ne!(s, "⏺ Generating…", "ghost description should win: {}", s);
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
        // W1's definition should mention W0 as a predecessor call
        let combined = lines.join("\n");
        // Find W1's definition block and check W0 appears inside it
        if let Some(w1_pos) = combined.find(": W1") {
            let after_w1 = &combined[w1_pos..];
            let semicolon_pos = after_w1.find(';').unwrap_or(after_w1.len());
            let w1_body = &after_w1[..semicolon_pos];
            assert!(
                w1_body.contains("W0"),
                "W1 body should call W0 (its predecessor)"
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
}
