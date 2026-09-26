//! Slash-command and `@`-mention completion state and raw-mode pane rendering.

use super::command_autocomplete::CommandSpec;
use super::MentionCandidate;

/// Maximum number of autocomplete suggestions to show at once
pub(crate) const MAX_VISIBLE_SUGGESTIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum CompletionMode {
    #[default]
    Commands,
    Mentions,
}

/// Autocomplete state for TUI rendering
#[derive(Debug, Clone, Default)]
pub(crate) struct AutocompleteState {
    /// Matched commands from registry
    pub matches: Vec<CommandSpec>,
    mention_matches: Vec<MentionCandidate>,
    mode: CompletionMode,
    /// Currently selected index (for up/down navigation)
    pub selected_index: usize,
    /// First match shown in the viewport. Kept in sync with selection.
    pub first_visible: usize,
    /// Whether the dropdown is visible
    pub visible: bool,
    /// Rows emitted by the most recent production raw-frame draw.
    rendered_rows: usize,
    /// Last speakable mention-resolution error, if any.
    mention_error: Option<String>,
}

impl AutocompleteState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Update matches and show dropdown
    pub fn show_matches(&mut self, matches: Vec<CommandSpec>) {
        let selected = self
            .get_selected()
            .map(|command| (command.name, command.params));
        self.mode = CompletionMode::Commands;
        self.mention_matches.clear();
        self.mention_error = None;
        self.matches = matches;
        self.visible = !self.matches.is_empty();
        self.rendered_rows = 0;
        self.selected_index = selected
            .and_then(|identity| {
                self.matches
                    .iter()
                    .position(|command| (command.name, command.params) == identity)
            })
            .unwrap_or(0);
        self.ensure_selection_visible(MAX_VISIBLE_SUGGESTIONS);
    }

    /// Show project-file mention candidates. Selection identity is the relative path.
    pub fn show_mentions(&mut self, matches: Vec<MentionCandidate>) {
        let selected = self
            .get_selected_mention()
            .map(|candidate| candidate.relative_path.clone());
        self.mode = CompletionMode::Mentions;
        self.matches.clear();
        self.mention_matches = matches;
        self.visible = !self.mention_matches.is_empty();
        self.rendered_rows = 0;
        self.selected_index = selected
            .and_then(|path| {
                self.mention_matches
                    .iter()
                    .position(|candidate| candidate.relative_path == path)
            })
            .unwrap_or(0);
        self.ensure_selection_visible(MAX_VISIBLE_SUGGESTIONS);
    }

    /// Record a speakable mention-resolution failure for the next pane draw.
    pub(crate) fn set_mention_error(&mut self, message: impl Into<String>) {
        self.mention_error = Some(message.into());
        self.visible = true;
    }

    /// Hide the dropdown
    pub fn hide(&mut self) {
        self.visible = false;
        self.matches.clear();
        self.mention_matches.clear();
        self.mention_error = None;
        self.mode = CompletionMode::Commands;
        self.selected_index = 0;
        self.first_visible = 0;
        self.rendered_rows = 0;
    }

    /// Whether the painted pane is offering `@` mention rows.
    pub(crate) fn is_mention_mode(&self) -> bool {
        self.mode == CompletionMode::Mentions
    }

    /// Get the currently selected command (if any)
    pub fn get_selected(&self) -> Option<&CommandSpec> {
        if self.mode == CompletionMode::Commands
            && self.visible
            && self.selected_index < self.matches.len()
        {
            Some(&self.matches[self.selected_index])
        } else {
            None
        }
    }

    /// Selected mention row, when the mention pane is showing.
    pub fn get_selected_mention(&self) -> Option<&MentionCandidate> {
        if self.mode == CompletionMode::Mentions
            && self.visible
            && self.selected_index < self.mention_matches.len()
        {
            Some(&self.mention_matches[self.selected_index])
        } else {
            None
        }
    }

    /// Whether keyboard navigation is backed by a currently visible pane.
    pub fn is_interactive(&self) -> bool {
        self.visible && self.rendered_rows > 0
    }

    /// Invalidate keyboard authority when the terminal geometry changes.
    /// Matches and selection remain stable until the next viewport plan paints
    /// them again, but keys cannot act on rows from the old frame.
    pub(crate) fn invalidate_rendered_rows(&mut self) {
        self.rendered_rows = 0;
    }

    /// Move selection up (wraps around)
    pub fn select_previous(&mut self) {
        let len = self.row_count();
        if len > 0 {
            if self.selected_index == 0 {
                self.selected_index = len - 1;
            } else {
                self.selected_index -= 1;
            }
        }
        self.ensure_selection_visible(MAX_VISIBLE_SUGGESTIONS);
    }

    /// Move selection down (wraps around)
    pub fn select_next(&mut self) {
        let len = self.row_count();
        if len > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        }
        self.ensure_selection_visible(MAX_VISIBLE_SUGGESTIONS);
    }

    /// Keep the selected row inside a viewport of `visible_rows` suggestions.
    pub fn ensure_selection_visible(&mut self, visible_rows: usize) {
        let len = self.row_count();
        if visible_rows == 0 || len == 0 {
            self.first_visible = 0;
            return;
        }
        if self.selected_index < self.first_visible {
            self.first_visible = self.selected_index;
        } else if self.selected_index >= self.first_visible + visible_rows {
            self.first_visible = self.selected_index + 1 - visible_rows;
        }
        self.first_visible = self.first_visible.min(len.saturating_sub(visible_rows));
    }

    fn row_count(&self) -> usize {
        match self.mode {
            CompletionMode::Commands => self.matches.len(),
            CompletionMode::Mentions => self.mention_matches.len(),
        }
    }
}

/// Fixed rows the completion pane reserves whenever it has more than one row
/// of budget: one heading row plus up to `MAX_VISIBLE_SUGGESTIONS` match
/// rows. A pane with fewer matches than this pads the remainder with blank
/// rows, so the pane's total height never depends on how many rows
/// currently match — only the transition between hidden (nothing matches)
/// and visible ever moves the composer below it (the pre-existing #232
/// boundary). Without this, narrowing a search from 8 matches to 1 shrank
/// the pane and visibly walked the `❯` input row up the screen every
/// keystroke (#1247).
pub(crate) const RESERVED_PANE_ROWS: usize = MAX_VISIBLE_SUGGESTIONS + 1;

/// Plain-text rows for the production raw-mode completion pane.
///
/// Every returned string is a complete physical row and remains useful with
/// ANSI colors disabled or when read by a screen reader. `row_budget` includes
/// the heading; a one-row viewport therefore shows the selected command.
/// The pane pads to [`RESERVED_PANE_ROWS`] (clamped to `row_budget`) so its
/// height stays constant while visible, regardless of match count (#1247).
pub(crate) fn completion_pane_lines(
    state: &mut AutocompleteState,
    width: usize,
    row_budget: usize,
) -> Vec<String> {
    let row_count = state.row_count();
    if !state.visible
        || (row_count == 0 && state.mention_error.is_none())
        || width == 0
        || row_budget == 0
    {
        state.rendered_rows = 0;
        return Vec::new();
    }

    let mut lines = if state.mode == CompletionMode::Mentions {
        mention_pane_lines(state, width, row_budget)
    } else {
        command_pane_lines(state, width, row_budget)
    };
    if row_budget > 1 {
        let target = row_budget.min(RESERVED_PANE_ROWS);
        while lines.len() < target {
            lines.push(fit_line("", width));
        }
    }
    state.rendered_rows = lines.len();
    lines
}

fn command_pane_lines(
    state: &mut AutocompleteState,
    width: usize,
    row_budget: usize,
) -> Vec<String> {
    let suggestion_rows = if row_budget == 1 {
        1
    } else {
        (row_budget - 1).min(MAX_VISIBLE_SUGGESTIONS)
    };
    state.ensure_selection_visible(suggestion_rows);
    let end = (state.first_visible + suggestion_rows).min(state.matches.len());
    let mut lines = Vec::with_capacity(row_budget.min(suggestion_rows + 1));
    if row_budget > 1 {
        let remaining = state.matches.len().saturating_sub(end);
        let heading = format!(
            "Commands {}-{} of {} ({remaining} more)  Up/Down select, Tab complete, Enter run, Esc cancel",
            state.first_visible + 1,
            end,
            state.matches.len()
        );
        lines.push(fit_line(&heading, width));
    }
    for index in state.first_visible..end {
        let command = &state.matches[index];
        let marker = if index == state.selected_index {
            ">"
        } else {
            " "
        };
        let line = format!(
            "{marker} {} - {}",
            command.full_syntax(),
            command.description
        );
        lines.push(fit_line(&line, width));
    }
    lines
}

fn mention_pane_lines(
    state: &mut AutocompleteState,
    width: usize,
    row_budget: usize,
) -> Vec<String> {
    let suggestion_rows = if row_budget == 1 {
        1
    } else {
        (row_budget - 1).min(MAX_VISIBLE_SUGGESTIONS)
    };
    state.ensure_selection_visible(suggestion_rows);
    let end = (state.first_visible + suggestion_rows).min(state.mention_matches.len());
    let mut lines = Vec::new();
    if row_budget > 1 {
        let remaining = state.mention_matches.len().saturating_sub(end);
        let heading = if let Some(error) = state.mention_error.as_deref() {
            error.to_string()
        } else if state.mention_matches.is_empty() {
            "No project files match this @ mention  Esc cancel".to_string()
        } else {
            format!(
                "Files {}-{} of {} ({remaining} more)  Up/Down select, Tab or Enter insert, Esc cancel",
                state.first_visible + 1,
                end.max(state.first_visible),
                state.mention_matches.len()
            )
        };
        lines.push(fit_line(&heading, width));
        if lines.len() >= row_budget {
            return lines;
        }
    }
    for index in state.first_visible..end {
        if lines.len() >= row_budget {
            break;
        }
        let candidate = &state.mention_matches[index];
        let marker = if index == state.selected_index {
            ">"
        } else {
            " "
        };
        let line = format!("{marker} {}", candidate.speakable_row);
        lines.push(fit_line(&line, width));
    }
    lines
}

fn fit_line(line: &str, width: usize) -> String {
    let count = line.chars().count();
    if count <= width {
        return format!("{line}{}", " ".repeat(width - count));
    }
    if width == 1 {
        return "…".to_string();
    }
    format!("{}…", line.chars().take(width - 1).collect::<String>())
}

pub(crate) fn replace_command_prefix(
    lines: &[String],
    cursor: (usize, usize),
    command_name: &str,
) -> Option<(Vec<String>, (usize, usize))> {
    let (cursor_row, cursor_col) = cursor;
    if cursor_row != 0 {
        return None;
    }
    let mut replaced = lines.to_vec();
    let first = replaced.first_mut()?;
    if !first.starts_with('/') || cursor_col > first.chars().count() {
        return None;
    }
    let typed_prefix = first.chars().take(cursor_col).collect::<String>();
    let typed_lower = typed_prefix.to_ascii_lowercase();
    let command_lower = command_name.to_ascii_lowercase();
    if typed_lower == command_lower || typed_lower.starts_with(&format!("{command_lower} ")) {
        return Some((replaced, cursor));
    }
    if !command_lower.starts_with(&typed_lower) {
        return None;
    }
    let suffix = first.chars().skip(cursor_col).collect::<String>();
    *first = format!("{command_name}{suffix}");
    Some((replaced, (0, command_name.chars().count())))
}

pub(crate) fn replace_mention_prefix(
    lines: &[String],
    cursor: (usize, usize),
    token: &str,
    query_at: impl FnOnce(&str, usize) -> Option<(usize, String)>,
) -> Option<(Vec<String>, (usize, usize))> {
    let (cursor_row, cursor_col) = cursor;
    let line = lines.get(cursor_row)?;
    let (at_offset, _) = query_at(line, cursor_col)?;
    let at_chars = line[..at_offset].chars().count();
    let prefix: String = line.chars().take(at_chars).collect();
    let suffix: String = line.chars().skip(cursor_col).collect();
    let mut replaced = lines.to_vec();
    replaced[cursor_row] = format!("{prefix}{token}{suffix}");
    let new_col = prefix.chars().count() + token.chars().count();
    Some((replaced, (cursor_row, new_col)))
}

#[cfg(test)]
mod tests {
    use super::super::command_autocomplete::{CommandCategory, CommandRegistry};
    use super::*;

    #[test]
    fn test_autocomplete_state() {
        let mut state = AutocompleteState::new();
        assert!(!state.visible);
        assert_eq!(state.matches.len(), 0);

        let matches = vec![
            CommandSpec {
                name: "/clear",
                params: None,
                description: "Clear history",
                category: CommandCategory::Basic,
            },
            CommandSpec {
                name: "/compact",
                params: Some("[instruction]"),
                description: "Compact history",
                category: CommandCategory::Basic,
            },
        ];

        state.show_matches(matches.clone());
        assert!(state.visible);
        assert_eq!(state.matches.len(), 2);
        assert_eq!(state.selected_index, 0);

        state.select_next();
        assert_eq!(state.selected_index, 1);

        state.select_next();
        assert_eq!(state.selected_index, 0); // Wrapped around

        state.select_previous();
        assert_eq!(state.selected_index, 1); // Wrapped backward

        state.hide();
        assert!(!state.visible);
        assert_eq!(state.matches.len(), 0);
    }

    #[test]
    fn test_get_selected() {
        let mut state = AutocompleteState::new();

        let matches = vec![CommandSpec {
            name: "/help",
            params: None,
            description: "Show help",
            category: CommandCategory::Basic,
        }];

        state.show_matches(matches);
        assert!(state.get_selected().is_some());
        assert_eq!(state.get_selected().unwrap().name, "/help");

        state.hide();
        assert!(state.get_selected().is_none());
    }

    #[test]
    fn test_production_pane_renders_exact_match_as_accessible_plain_text() {
        let registry = CommandRegistry::new();
        let mut state = AutocompleteState::new();
        state.show_matches(registry.match_prefix("/help"));

        let lines = completion_pane_lines(&mut state, 80, 9);

        // The pane reserves RESERVED_PANE_ROWS (9: heading + 8 rows) whenever
        // it has room, padding unused rows blank, so a single match does not
        // shrink the pane below what an 8-match pane claims (#1247).
        assert_eq!(lines.len(), RESERVED_PANE_ROWS);
        assert!(lines[0].contains("Commands 1-1 of 1"));
        assert!(lines[1].contains("> /help - Show available commands"));
        assert!(
            lines[2..].iter().all(|line| line.trim().is_empty()),
            "unused suggestion rows must pad blank, not disappear; got {lines:?}"
        );
        assert!(lines.iter().all(|line| !line.contains('\x1b')));
        assert!(lines.iter().all(|line| line.chars().count() == 80));
    }

    #[test]
    fn test_production_pane_handles_no_match_and_one_row_terminal() {
        let registry = CommandRegistry::new();
        let mut missing = AutocompleteState::new();
        missing.show_matches(registry.match_prefix("/not-a-command"));
        assert!(completion_pane_lines(&mut missing, 80, 9).is_empty());

        let mut exact = AutocompleteState::new();
        exact.show_matches(registry.match_prefix("/help"));
        let tiny = completion_pane_lines(&mut exact, 12, 1);
        assert_eq!(tiny.len(), 1);
        assert_eq!(tiny[0].chars().count(), 12);
        assert!(tiny[0].starts_with("> /help"));
    }

    #[test]
    fn test_nested_brain_matches_are_contextual_and_selection_scrolls() {
        let registry = CommandRegistry::new();
        let matches = registry.match_prefix("/brain ");
        assert!(matches.len() >= 8, "expected contextual Brain subcommands");
        assert!(matches
            .iter()
            .all(|command| command.name.starts_with("/brain ")));

        let mut state = AutocompleteState::new();
        state.show_matches(matches);
        for _ in 0..8 {
            state.select_next();
        }
        let selected = state.get_selected().unwrap().name;
        let lines = completion_pane_lines(&mut state, 120, 9);
        assert_eq!(lines.len(), 9);
        assert!(state.first_visible > 0);
        assert!(lines
            .iter()
            .any(|line| line.contains(&format!("> {selected}"))));
    }

    #[test]
    fn test_changing_prefix_preserves_a_still_matching_selection() {
        let registry = CommandRegistry::new();
        let mut state = AutocompleteState::new();
        state.show_matches(registry.match_prefix("/brain "));
        while state.get_selected().unwrap().name != "/brain handoff" {
            state.select_next();
        }

        state.show_matches(registry.match_prefix("/brain h"));

        assert_eq!(state.get_selected().unwrap().name, "/brain handoff");
    }

    #[test]
    fn test_accepting_completion_preserves_multiline_draft_suffix_and_cursor() {
        let lines = vec![
            "/bra --later".to_string(),
            "keep this draft".to_string(),
            "and this too".to_string(),
        ];

        let (accepted, cursor) = replace_command_prefix(&lines, (0, 4), "/brain list").unwrap();

        assert_eq!(
            accepted,
            ["/brain list --later", "keep this draft", "and this too"]
        );
        assert_eq!(cursor, (0, "/brain list".chars().count()));
    }

    #[test]
    fn test_accepting_parameter_hint_preserves_typed_arguments_and_cursor() {
        let lines = vec!["/brain archive old-session".to_string()];
        let cursor = (0, lines[0].chars().count());

        let (accepted, accepted_cursor) =
            replace_command_prefix(&lines, cursor, "/brain archive").unwrap();

        assert_eq!(accepted, lines);
        assert_eq!(accepted_cursor, cursor);
    }

    #[test]
    fn test_accepting_case_insensitive_match_inserts_canonical_command() {
        let lines = vec!["/BRA and keep this".to_string()];
        let (accepted, cursor) = replace_command_prefix(&lines, (0, 4), "/brain list").unwrap();

        assert_eq!(accepted, ["/brain list and keep this"]);
        assert_eq!(cursor, (0, "/brain list".chars().count()));
    }

    #[test]
    fn test_pane_rows_never_wrap_at_narrow_or_wide_sizes() {
        let registry = CommandRegistry::new();
        for width in [1, 8, 24, 80, 240] {
            let mut state = AutocompleteState::new();
            state.show_matches(registry.match_prefix("/"));
            let lines = completion_pane_lines(&mut state, width, 9);
            assert!(!lines.is_empty());
            assert!(lines.iter().all(|line| line.chars().count() == width));
        }
    }

    #[test]
    fn test_hidden_zero_row_pane_never_captures_keys() {
        let registry = CommandRegistry::new();
        let mut state = AutocompleteState::new();
        state.show_matches(registry.match_prefix("/brain "));
        assert!(!state.is_interactive(), "matches have not been painted yet");

        assert!(completion_pane_lines(&mut state, 80, 0).is_empty());
        assert!(!state.is_interactive());

        assert_eq!(completion_pane_lines(&mut state, 80, 1).len(), 1);
        assert!(state.is_interactive());

        state.show_matches(registry.match_prefix("/brain h"));
        assert!(
            !state.is_interactive(),
            "an edited draft needs a fresh paint"
        );
    }
}
