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
use std::collections::HashSet;
use std::io::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tui_textarea::TextArea;

use super::{OutputManager, StatusBar};
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

pub use async_input::spawn_input_task;
pub use autocomplete_widget::AutocompleteState;
pub use dialog::{Dialog, DialogOption, DialogResult, DialogType};
pub use dialog_widget::DialogWidget;
pub use shadow_buffer::visible_length;
pub use tabbed_dialog::{TabbedDialog, TabbedDialogResult};
pub use tabbed_dialog_widget::TabbedDialogWidget;
// Re-export ColorScheme so callers can use `crate::cli::tui::ColorScheme`.
pub use crate::config::ColorScheme;

// ─── ANSI helpers ─────────────────────────────────────────────────────────────

const RESET: &str = "\x1b[0m";
const CYAN: &str = "\x1b[36m";
const DIM_GRAY: &str = "\x1b[90m";

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

// ─── Pure logic helpers (testable without a terminal) ─────────────────────────

/// Count the number of terminal rows an `effective_status` string will occupy.
///
/// Each `\n` in the string produces an additional row.  An empty string still
/// occupies exactly one row (the idle hint is always shown).
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

    // Rate limiting
    last_render: Instant,
    render_interval: Duration,

    // Session task list (set after construction via set_todo_list)
    todo_list: Option<Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>>,

    // Session identity — set by print_startup_header(); shown in separator line.
    session_label: String,
}

// ─── Construction ─────────────────────────────────────────────────────────────

impl TuiRenderer {
    pub fn new(
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        colors: ColorScheme,
    ) -> Result<Self> {
        enable_raw_mode().context("Failed to enable raw mode")?;

        // We intentionally do NOT push any KeyboardEnhancementFlags.
        //
        // The kitty keyboard enhancement protocol (DISAMBIGUATE_ESCAPE_CODES /
        // REPORT_ALL_KEYS_AS_ESCAPE_CODES) corrupts the terminal if the pop
        // sequence is not received before the shell takes over — which happens
        // on panic, SIGKILL, or any non-clean exit.  The user then sees raw
        // escape-sequence numeric fragments (e.g. "442;5u") for every keypress
        // and cannot type until they kill the terminal window.
        //
        // Everything finch needs works without enhancement:
        //   • Shift+Enter  → standard terminals send Enter + SHIFT modifier
        //   • Shift+Tab    → standard \x1b[Z (BackTab)
        //   • Ctrl+keys    → sent as-is in standard raw mode
        // The only real loss is disambiguation of Esc vs Alt+key, which is not
        // a use-case finch currently handles.

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

            last_render: Instant::now(),
            render_interval: Duration::from_millis(100),

            todo_list: None,

            session_label: String::new(),
        })
    }

    /// Attach the session task list so the live area can display it.
    pub fn set_todo_list(
        &mut self,
        todo_list: Arc<tokio::sync::RwLock<crate::tools::todo::TodoList>>,
    ) {
        self.todo_list = Some(todo_list);
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
    fn erase_live_area(&mut self) -> Result<()> {
        if self.active_rows == 0 {
            return Ok(());
        }
        let mut stdout = io::stdout();
        execute!(stdout, cursor::MoveToColumn(0))?;
        if self.cursor_row_from_top > 0 {
            execute!(stdout, cursor::MoveUp(self.cursor_row_from_top as u16))?;
        }
        execute!(stdout, Clear(ClearType::FromCursorDown))?;
        self.active_rows = 0;
        self.cursor_row_from_top = 0;
        Ok(())
    }

    /// Draw the live area from scratch and track `active_rows`.
    fn draw_live_area(&mut self) -> Result<()> {
        let mut stdout = io::stdout();
        execute!(stdout, BeginSynchronizedUpdate)?;

        let mut rows: usize = 0;

        // ── 1. Active WorkUnit ────────────────────────────────────────────────
        let live_msg = self.find_live_message();
        if let Some(msg) = &live_msg {
            let formatted = msg.format(&self.colors);
            for line in formatted.split('\n') {
                let line = line.trim_end_matches('\r');
                execute!(stdout, Print(line), Print("\r\n"))?;
                rows += 1;
            }
        }

        // ── 1b. Session task list (active items only) ─────────────────────────
        if let Some(ref todo_arc) = self.todo_list {
            if let Ok(todo) = todo_arc.try_read() {
                let active = todo.active_items();
                if !active.is_empty() {
                    let term_w =
                        crossterm::terminal::size().unwrap_or((80, 24)).0 as usize;
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
                        let max_content =
                            term_w.saturating_sub(2 + priority_tag.len());
                        let content: String =
                            item.content.chars().take(max_content).collect();
                        execute!(
                            stdout,
                            Print(format!(
                                "{}{} {}{}{}\r\n",
                                color, symbol, content, priority_tag, RESET
                            ))
                        )?;
                        rows += 1;
                    }
                }
            }
        }

        // ── 2. Separator: "──  ~/repos/finch ──────── jade-river ──" ──────────
        // CWD is left-anchored; session name is right-anchored.
        let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0 as usize;
        let cwd_label = tilde_cwd();
        let prefix = "── ";
        let prefix_vis = 3_usize;
        let cwd_part = format!(" {} ", cwd_label);
        let right_part = if self.session_label.is_empty() {
            " ──".to_string()
        } else {
            format!(" {} ──", self.session_label)
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
            let dialog_rows = Self::draw_dialog_inline_static(&mut stdout, dialog)?;
            rows += dialog_rows;
            // Dialog drawing leaves the cursor at the last drawn row (no reposition).
            cursor_row_from_top = rows.saturating_sub(1);
        } else {
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
            let mut input_phys_rows: Vec<usize> = Vec::new();

            if lines.is_empty() {
                execute!(stdout, Print(&prompt))?;
                input_phys_rows.push(1);
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

                    // Physical terminal rows = ceil((prefix_vis + text_vis) / term_width).
                    // A line that exactly fills the terminal still counts as 1 physical row;
                    // one that overflows wraps into additional rows.
                    let prefix_vis = if i == 0 { prompt_vis_len } else { cont_vis_len };
                    let text_vis = line.chars().count();
                    let total_vis = prefix_vis + text_vis;
                    let phys = if term_width > 0 {
                        (total_vis.max(1) + term_width - 1) / term_width
                    } else {
                        1
                    };
                    input_phys_rows.push(phys.max(1));
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
            let raw_status = self.status_bar.get_status();
            let current_input = self.input_textarea.lines().join("\n");
            let effective_status = compute_effective_status(
                self.ghost_text.as_deref(),
                &raw_status,
                &current_input,
                &self.command_registry,
            );

            // Thin separator between input area and status line(s) — full terminal width
            let status_sep: String = "─".repeat(term_width);
            execute!(
                stdout,
                Print(format!("\r\n{}{}{}", DIM_GRAY, status_sep, RESET))
            )?;

            let status_line_count = count_status_lines(&effective_status) + 1; // +1 for separator
            for line in effective_status.lines() {
                execute!(stdout, Print(format!("\r\n{}{}{}", DIM_GRAY, line, RESET)))?;
            }
            rows += status_line_count;

            // ── 6. Reposition cursor inside the input area ────────────────────
            //
            // After drawing all input lines and status lines the cursor is at the
            // very bottom of the live area.  We compute how many physical terminal
            // rows are below the cursor's current logical position and move up by
            // that amount.  This correctly handles lines that wrap across multiple
            // terminal rows.

            let cursor_prefix_vis = if cursor_row == 0 { prompt_vis_len } else { cont_vis_len };

            // Which physical sub-row within cursor_row's logical line is the cursor on?
            let cursor_sub_row = if term_width > 0 {
                (cursor_prefix_vis + cursor_col) / term_width
            } else {
                0
            };

            // Physical rows remaining in the cursor's logical line after the cursor.
            let phys_in_cursor_line = input_phys_rows.get(cursor_row).copied().unwrap_or(1);
            let rows_in_cursor_line_below = phys_in_cursor_line.saturating_sub(1 + cursor_sub_row);

            // Physical rows in input lines that come after cursor_row.
            let input_below_phys: usize = input_phys_rows
                .iter()
                .skip(cursor_row + 1)
                .sum::<usize>()
                + rows_in_cursor_line_below;

            let rows_below_cursor = input_below_phys + status_line_count;
            if rows_below_cursor > 0 {
                execute!(stdout, cursor::MoveUp(rows_below_cursor as u16))?;
            }

            // Column within the current physical sub-row (accounts for wrapping).
            let col = if term_width > 0 {
                (cursor_prefix_vis + cursor_col) % term_width
            } else {
                cursor_prefix_vis + cursor_col
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

    /// Return the most recent InProgress message for the live area.
    fn find_live_message(&self) -> Option<MessageRef> {
        self.output_manager
            .get_messages()
            .into_iter()
            .filter(|m| !self.printed_ids.contains(&m.id()))
            .find(|m| matches!(m.status(), MessageStatus::InProgress))
    }
}

// ─── flush_output_safe / render ───────────────────────────────────────────────

impl TuiRenderer {
    /// Called from the event loop on every tick.
    /// Commits newly-completed messages to permanent scrollback, then redraws.
    pub fn flush_output_safe(&mut self, _output_manager: &OutputManager) -> Result<()> {
        let messages = self.output_manager.get_messages();

        let mut to_commit: Vec<MessageRef> = Vec::new();
        for msg in &messages {
            let id = msg.id();
            if self.printed_ids.contains(&id) {
                continue;
            }
            match msg.status() {
                MessageStatus::Complete | MessageStatus::Failed => {
                    to_commit.push(msg.clone());
                    self.printed_ids.insert(id);
                }
                MessageStatus::InProgress => {}
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
        } else if self.last_render.elapsed() >= self.render_interval {
            // Periodic redraw for animation / status updates.
            self.erase_live_area()?;
            self.draw_live_area()?;
        }

        self.last_render = Instant::now();
        Ok(())
    }

    /// Redraw the live area.  Called by the event loop and by async_input.
    pub fn render(&mut self) -> Result<()> {
        self.erase_live_area()?;
        self.draw_live_area()
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
    pub fn print_startup_header(&mut self, model: &str, cwd: &str, session_label: &str) -> Result<()> {
        let version = env!("CARGO_PKG_VERSION");

        // Store session label so blit_visible_area() can embed it in the separator.
        self.session_label = session_label.to_string();

        // Clear the visible terminal so we start from a clean slate.
        execute!(io::stdout(), Clear(ClearType::All), cursor::MoveTo(0, 0))?;

        // Darwin finch ASCII bird — 6 lines.
        // Columns 0-14: bird art.  Column 15+: info text.
        //
        //   Line 1:       ▄▄▄▄▄▄              (head top — art only)
        //   Line 2:     ▗▟█●██▙►  finch v…    (head + beak + version)
        //   Line 3:   ▐████████▌  <model>      (upper body + model name)
        //   Line 4:   ▝▜██████▛▘  <cwd>        (lower body + cwd)
        //   Line 5:      ╥  ╥                  (legs)
        //   Line 6:     ╱    ╲                 (perch)
        execute!(
            io::stdout(),
            // Line 1 — head top
            Print("      "),
            SetForegroundColor(Color::DarkYellow),
            Print("▄▄▄▄▄▄"),
            ResetColor,
            Print("\r\n"),
            // Line 2 — head with eye, beak, version
            Print("    "),
            SetForegroundColor(Color::DarkYellow),
            Print("▗▟█"),
            SetForegroundColor(Color::White),
            Print("●"),
            SetForegroundColor(Color::DarkYellow),
            Print("██▙"),
            SetForegroundColor(Color::Yellow),
            SetAttribute(Attribute::Bold),
            Print("►"),
            ResetColor,
            Print("  "),
            SetAttribute(Attribute::Bold),
            Print(format!("finch v{}", version)),
            SetAttribute(Attribute::Reset),
            Print("\r\n"),
            // Line 3 — upper body + model name
            Print("  "),
            SetForegroundColor(Color::DarkYellow),
            Print("▐████████▌"),
            ResetColor,
            Print(format!("   {}\r\n", model)),
            // Line 4 — lower body + session · cwd
            Print("  "),
            SetForegroundColor(Color::DarkYellow),
            Print("▝▜██████▛▘"),
            ResetColor,
            Print("   "),
            SetForegroundColor(Color::DarkGrey),
            Print(format!("{}  ·  {}", session_label, cwd)),
            ResetColor,
            Print("\r\n"),
            // Line 5 — legs
            Print("     "),
            SetForegroundColor(Color::DarkGrey),
            Print("╥  ╥"),
            ResetColor,
            Print("\r\n"),
            // Line 6 — perch
            Print("    "),
            SetForegroundColor(Color::DarkGrey),
            Print("╱    ╲"),
            ResetColor,
            Print("\r\n"),
        )?;

        self.draw_live_area()
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
        let _ = execute!(io::stdout(), cursor::Show, ResetColor);
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
                        (KeyCode::Enter, m) if m.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) => {
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
                    Event::Resize(_, _) => {
                        self.active_rows = 0;
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
        id
    }

    pub fn handle_resize(&mut self, _w: u16, _h: u16) -> Result<()> {
        // Clear the entire screen on resize to prevent ghosts from old layout
        execute!(io::stdout(), Clear(ClearType::All), cursor::MoveTo(0, 0))?;
        self.active_rows = 0;
        Ok(())
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
    }
}

// ─── Crossterm dialog rendering ───────────────────────────────────────────────

impl TuiRenderer {
    /// Draw a `Dialog` inline using crossterm box-drawing characters.
    /// Returns the number of terminal rows consumed.
    fn draw_dialog_inline_static(stdout: &mut io::Stdout, dialog: &Dialog) -> Result<usize> {
        let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0 as usize;
        let box_width = term_width.min(72);
        let inner = box_width.saturating_sub(6); // │ + 2 spaces on each side + │ = 6

        let mut rows = 0;

        let top = format!("┌{}┐", "─".repeat(box_width - 2));
        let div = format!("├{}┤", "─".repeat(box_width - 2));
        let bot = format!("└{}┘", "─".repeat(box_width - 2));

        // Top border
        execute!(stdout, Print(format!("{}\r\n", top)))?;
        rows += 1;

        // Title
        for line in wrap_text(&dialog.title, inner) {
            execute!(stdout, Print(format!("│  {:<w$}  │\r\n", line, w = inner)))?;
            rows += 1;
        }

        // Help message (from dialog field)
        if let Some(ref help) = dialog.help_message {
            execute!(
                stdout,
                Print(format!(
                    "│  {}{:<w$}{}  │\r\n",
                    DIM_GRAY,
                    help,
                    RESET,
                    w = inner
                ))
            )?;
            rows += 1;
        }

        execute!(stdout, Print(format!("{}\r\n", div)))?;
        rows += 1;

        // Options — when custom text mode is active, show text input instead
        if dialog.custom_mode_active {
            let input_text = dialog.custom_input.as_deref().unwrap_or("");
            execute!(
                stdout,
                Print(format!("│  {:<w$}  │\r\n", "Enter your response:", w = inner))
            )?;
            rows += 1;
            // Show input field with cursor marker
            let cursor = dialog.custom_cursor_pos;
            let before: String = input_text.chars().take(cursor).collect();
            let after: String = input_text.chars().skip(cursor).collect();
            let with_cursor = format!("{}\x1b[7m \x1b[m{}", before, after); // block cursor
            let visible = format!("> {}", input_text);
            let _ = with_cursor; // used below for display
            execute!(
                stdout,
                Print(format!(
                    "│  > {}{}\x1b[7m \x1b[m{}{:<w$}  │\r\n",
                    DIM_GRAY,
                    before,
                    RESET,
                    after,
                    w = inner.saturating_sub(3 + visible.len())
                ))
            )?;
            rows += 1;
        } else {
        match &dialog.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                allow_custom,
            } => {
                for (i, opt) in options.iter().enumerate() {
                    let marker = if i == *selected_index { "●" } else { "○" };
                    let on = if i == *selected_index {
                        "\x1b[1;36m"
                    } else {
                        ""
                    };
                    let off = if i == *selected_index { RESET } else { "" };
                    let label = format!("  {} {}", marker, opt.label);
                    execute!(
                        stdout,
                        Print(format!("│  {}{:<w$}{}  │\r\n", on, label, off, w = inner))
                    )?;
                    rows += 1;
                }
                if *allow_custom {
                    let other_label = "  ◌ Other (custom response)";
                    execute!(
                        stdout,
                        Print(format!(
                            "│  {}{:<w$}{}  │\r\n",
                            DIM_GRAY,
                            other_label,
                            RESET,
                            w = inner
                        ))
                    )?;
                    rows += 1;
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
                    let on = if i == *cursor_index { "\x1b[1;36m" } else { "" };
                    let off = if i == *cursor_index { RESET } else { "" };
                    let label = format!("  {} {}", checked, opt.label);
                    execute!(
                        stdout,
                        Print(format!("│  {}{:<w$}{}  │\r\n", on, label, off, w = inner))
                    )?;
                    rows += 1;
                }
                if *allow_custom {
                    let other_label = "  ◌ Other (custom response)";
                    execute!(
                        stdout,
                        Print(format!(
                            "│  {}{:<w$}{}  │\r\n",
                            DIM_GRAY,
                            other_label,
                            RESET,
                            w = inner
                        ))
                    )?;
                    rows += 1;
                }
            }
            DialogType::Confirm {
                prompt, selected, ..
            } => {
                execute!(
                    stdout,
                    Print(format!("│  {:<w$}  │\r\n", prompt, w = inner))
                )?;
                rows += 1;
                let yes_style = if *selected { "\x1b[1;36m" } else { DIM_GRAY };
                let no_style = if !selected { "\x1b[1;36m" } else { DIM_GRAY };
                execute!(
                    stdout,
                    Print(format!(
                        "│  {}Yes{}   {}No{}  {:<w$}  │\r\n",
                        yes_style,
                        RESET,
                        no_style,
                        RESET,
                        "",
                        w = inner.saturating_sub(12)
                    ))
                )?;
                rows += 1;
            }
            DialogType::TextInput { prompt, input, .. } => {
                execute!(
                    stdout,
                    Print(format!("│  {:<w$}  │\r\n", prompt, w = inner))
                )?;
                execute!(
                    stdout,
                    Print(format!(
                        "│  > {:<w$}  │\r\n",
                        input,
                        w = inner.saturating_sub(2)
                    ))
                )?;
                rows += 2;
            }
        }

        } // end else (custom_mode_active)

        // ── Preview pane ─────────────────────────────────────────────────────
        // If the focused option has a `markdown` field, render it in a bordered
        // preview section between the options and the keybindings hint.
        let focused_markdown: Option<&str> = if !dialog.custom_mode_active {
            match &dialog.dialog_type {
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
            }
        } else {
            None
        };

        if let Some(md) = focused_markdown {
            let term_height = crossterm::terminal::size().unwrap_or((80, 24)).1 as usize;
            let max_preview_lines = 10.min(term_height / 3).max(1);

            // Strip leading/trailing blank lines and collect non-empty content
            let raw_lines: Vec<&str> = md.lines().collect();
            let start = raw_lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
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

            let preview_div = format!("├─ Preview {}", "─".repeat(box_width.saturating_sub(12)));
            execute!(stdout, Print(format!("{}\r\n", preview_div)))?;
            rows += 1;

            for line in &display_lines {
                // Truncate to inner width using visible_length to handle ANSI codes
                let vlen = shadow_buffer::visible_length(line);
                let display = if vlen <= inner {
                    format!("│  {:<w$}  │\r\n", line, w = inner)
                } else {
                    // Truncate by chars (ANSI codes make byte slicing unsafe)
                    let truncated_line: String = line.chars().take(inner.saturating_sub(1)).collect();
                    format!("│  {}…  │\r\n", truncated_line)
                };
                execute!(stdout, Print(display))?;
                rows += 1;
            }

            if truncated {
                execute!(
                    stdout,
                    Print(format!(
                        "│  {}{:<w$}{}  │\r\n",
                        DIM_GRAY,
                        "…",
                        RESET,
                        w = inner
                    ))
                )?;
                rows += 1;
            }
        }
        // ── End preview pane ─────────────────────────────────────────────────

        execute!(stdout, Print(format!("{}\r\n", div)))?;
        let allow_custom = matches!(
            &dialog.dialog_type,
            DialogType::Select { allow_custom: true, .. }
                | DialogType::MultiSelect { allow_custom: true, .. }
        );
        let help = if dialog.custom_mode_active {
            "Enter Submit  Esc Cancel mode"
        } else if allow_custom {
            "↑/↓ Navigate  Enter Select  o: Other  Esc Cancel"
        } else {
            "↑/↓ Navigate  Enter Select  Esc Cancel"
        };
        execute!(
            stdout,
            Print(format!(
                "│  {}{:<w$}{}  │\r\n",
                DIM_GRAY,
                help,
                RESET,
                w = inner
            ))
        )?;
        execute!(stdout, Print(&bot))?;
        rows += 3;

        Ok(rows)
    }

    /// Show a blocking dialog (used when no async event loop is running).
    /// Returns `DialogResult::Cancelled` if Esc is pressed.
    pub fn show_dialog(&mut self, dialog: Dialog) -> Result<DialogResult> {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};

        self.active_dialog = Some(dialog);
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
        assert!(d.custom_mode_active, "pressing 'o' must activate custom input mode");
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
}
