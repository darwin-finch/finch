// Dialog - Native ratatui dialog system for user interaction
//
// Replaces inquire menus with ratatui-integrated dialogs that work seamlessly
// with the TUI, avoiding the need for suspend/resume.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashSet;

/// Type of dialog to display
#[derive(Debug, Clone)]
pub enum DialogType {
    /// Single-select menu with arrow keys and number selection
    Select {
        options: Vec<DialogOption>,
        selected_index: usize,
        allow_custom: bool, // Enable "Other" option with text input
    },
    /// Multi-select menu with checkboxes and space to toggle
    MultiSelect {
        options: Vec<DialogOption>,
        selected_indices: HashSet<usize>,
        cursor_index: usize,
        allow_custom: bool, // Enable "Other" option with text input
    },
    /// Text input with cursor and editing support
    TextInput {
        prompt: String,
        input: String,
        cursor_pos: usize,
        default: Option<String>,
    },
    /// Yes/No confirmation dialog
    Confirm {
        prompt: String,
        default: bool,
        selected: bool,
    },
}

/// Option in a dialog menu
#[derive(Debug, Clone)]
pub struct DialogOption {
    pub label: String,
    pub description: Option<String>,
    /// Optional markdown preview shown in a box when this option is focused.
    pub markdown: Option<String>,
}

impl DialogOption {
    /// Create a new dialog option with just a label
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
            markdown: None,
        }
    }

    /// Create a dialog option with label and description
    pub fn with_description(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: Some(description.into()),
            markdown: None,
        }
    }

    /// Attach a markdown preview to this option
    pub fn with_markdown(mut self, markdown: impl Into<String>) -> Self {
        self.markdown = Some(markdown.into());
        self
    }
}

/// A dialog to display to the user
#[derive(Debug, Clone)]
pub struct Dialog {
    pub title: String,
    pub dialog_type: DialogType,
    pub help_message: Option<String>,
    /// Optional body text shown inside the box, above the options divider.
    /// Used to display a plan preview so the user can read it without scrolling.
    pub body: Option<String>,
    pub custom_input: Option<String>, // Stores custom text if "Other" is being entered
    pub custom_mode_active: bool,     // Whether user is currently typing custom text
    pub custom_cursor_pos: usize,     // Char-index cursor in custom_input
    pub body_scroll_offset: usize,    // Line offset for scrollable body section
}

impl Dialog {
    /// Create a new single-select dialog
    pub fn select(title: impl Into<String>, options: Vec<DialogOption>) -> Self {
        Self {
            title: title.into(),
            dialog_type: DialogType::Select {
                options,
                selected_index: 0,
                allow_custom: false,
            },
            help_message: None,
            body: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a new single-select dialog with custom "Other" option
    pub fn select_with_custom(title: impl Into<String>, options: Vec<DialogOption>) -> Self {
        Self {
            title: title.into(),
            dialog_type: DialogType::Select {
                options,
                selected_index: 0,
                allow_custom: true,
            },
            help_message: None,
            body: None,
            custom_input: Some(String::new()),
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a new multi-select dialog
    pub fn multiselect(title: impl Into<String>, options: Vec<DialogOption>) -> Self {
        Self {
            title: title.into(),
            dialog_type: DialogType::MultiSelect {
                options,
                selected_indices: HashSet::new(),
                cursor_index: 0,
                allow_custom: false,
            },
            help_message: None,
            body: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a new multi-select dialog with custom "Other" option
    pub fn multiselect_with_custom(title: impl Into<String>, options: Vec<DialogOption>) -> Self {
        Self {
            title: title.into(),
            dialog_type: DialogType::MultiSelect {
                options,
                selected_indices: HashSet::new(),
                cursor_index: 0,
                allow_custom: true,
            },
            help_message: None,
            body: None,
            custom_input: Some(String::new()),
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a new text input dialog
    pub fn text_input(title: impl Into<String>, default: Option<String>) -> Self {
        let title_str = title.into();
        Self {
            title: title_str,
            dialog_type: DialogType::TextInput {
                prompt: String::new(), // title already shown above divider; no need to repeat
                input: default.clone().unwrap_or_default(),
                cursor_pos: default.as_ref().map(|s| s.len()).unwrap_or(0),
                default,
            },
            help_message: None,
            body: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a new confirmation dialog
    pub fn confirm(title: impl Into<String>, default: bool) -> Self {
        let title_str = title.into();
        Self {
            title: title_str.clone(),
            dialog_type: DialogType::Confirm {
                // prompt is shown in the dialog body; keep it empty so the
                // title (shown in the border) doesn't repeat as a content line.
                prompt: String::new(),
                default,
                selected: default,
            },
            help_message: None,
            body: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
            body_scroll_offset: 0,
        }
    }

    /// Create a tool-approval dialog (Yes / Yes-always / No).
    ///
    /// The dialog needs a name and a summary to show; Finch's `ToolUse` is converted
    /// at the caller (`cli::repl_event::tool_display::tool_approval_dialog`).
    /// File-mutating tools (write/edit) get an extra "Edit in $EDITOR" option.
    /// The title is formatted as `"{tool_name}\n{summary}"` for two-line display.
    pub fn tool_approval(tool_name: &str, summary: &str) -> Self {
        let tool_name = crate::cli::diff::sanitize_terminal(tool_name);
        let summary = crate::cli::diff::sanitize_multiline(summary);
        let is_file_mutating = matches!(tool_name.to_lowercase().as_str(), "write" | "edit");
        let options = if is_file_mutating {
            vec![
                DialogOption::new("1. Yes"),
                DialogOption::new("2. Edit in $EDITOR"),
                DialogOption::new(format!("3. Yes, and don't ask again for: {}:*", tool_name)),
                DialogOption::new("4. No"),
            ]
        } else {
            vec![
                DialogOption::new("1. Yes"),
                DialogOption::new(format!("2. Yes, and don't ask again for: {}:*", tool_name)),
                DialogOption::new("3. No"),
            ]
        };
        Dialog::select(format!("{}\n{}", tool_name, summary), options)
    }

    /// Set the help message for this dialog
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help_message = Some(help.into());
        self
    }

    /// Set optional body text shown inside the box above the options.
    /// Useful for displaying a plan or other content the user needs to read before deciding.
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(crate::cli::diff::sanitize_multiline(&body.into()));
        self
    }

    /// Returns the virtual index of the Cancel button for Select/MultiSelect dialogs.
    ///
    /// Layout (Select):   real_options | Other? | Cancel
    /// Layout (MultiSelect): real_options | Other? | Submit | Cancel
    pub fn cancel_virtual_index(&self) -> Option<usize> {
        match &self.dialog_type {
            DialogType::Select {
                options,
                allow_custom,
                ..
            } => Some(options.len() + if *allow_custom { 1 } else { 0 }),
            DialogType::MultiSelect {
                options,
                allow_custom,
                ..
            } => Some(options.len() + if *allow_custom { 2 } else { 1 }),
            _ => None,
        }
    }

    /// Returns the virtual index of the Submit button (MultiSelect only).
    pub fn submit_virtual_index(&self) -> Option<usize> {
        match &self.dialog_type {
            DialogType::MultiSelect {
                options,
                allow_custom,
                ..
            } => Some(options.len() + if *allow_custom { 1 } else { 0 }),
            _ => None,
        }
    }

    /// Returns true when the cursor is on the virtual "Other" row.
    fn cursor_on_other_row(&self) -> bool {
        match &self.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                allow_custom,
                ..
            } => *allow_custom && *selected_index == options.len(),
            DialogType::MultiSelect {
                options,
                cursor_index,
                allow_custom,
                ..
            } => *allow_custom && *cursor_index == options.len(),
            _ => false,
        }
    }

    /// Returns the current cursor index for Select/MultiSelect dialogs.
    pub fn current_cursor(&self) -> Option<usize> {
        match &self.dialog_type {
            DialogType::Select { selected_index, .. } => Some(*selected_index),
            DialogType::MultiSelect { cursor_index, .. } => Some(*cursor_index),
            _ => None,
        }
    }

    /// Option rows in the control suffix, not counting rules or the Cancel row.
    ///
    /// Used to distinguish "too many options" (top-clip) from a write approval
    /// whose chrome is one row over the budget (pin the suffix tail).
    pub(crate) fn option_row_count(&self) -> usize {
        match &self.dialog_type {
            DialogType::Select {
                options,
                allow_custom,
                ..
            } => options.len() + usize::from(*allow_custom),
            DialogType::MultiSelect {
                options,
                allow_custom,
                ..
            } => options.len() + usize::from(*allow_custom),
            DialogType::Confirm { .. } | DialogType::TextInput { .. } => 1,
        }
    }

    /// Handle a key event and return a result if the dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<DialogResult> {
        // Priority 0: Scroll the body section (when not in custom mode). Laptop
        // keyboards often have no PageUp/PageDown, so support the conventional
        // terminal equivalents as well.
        if !self.custom_mode_active && self.body.is_some() {
            match key.code {
                KeyCode::PageUp => {
                    self.body_scroll_offset = self.body_scroll_offset.saturating_sub(5);
                    return None;
                }
                KeyCode::PageDown => {
                    self.body_scroll_offset = self.body_scroll_offset.saturating_add(5);
                    return None;
                }
                KeyCode::Home => {
                    self.body_scroll_offset = 0;
                    return None;
                }
                KeyCode::End => {
                    self.body_scroll_offset = usize::MAX;
                    return None;
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.body_scroll_offset = self.body_scroll_offset.saturating_sub(5);
                    return None;
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.body_scroll_offset = self.body_scroll_offset.saturating_add(5);
                    return None;
                }
                _ => {}
            }
        }

        // Priority 1: Handle custom text input mode
        if self.custom_mode_active {
            return self.handle_custom_input_key(key);
        }

        // Priority 2: 'o'/'O' activates custom mode — but NOT when already on the "Other"
        // row (priority 2.5 handles that case, inserting the char directly).
        // Also moves the cursor to the Other row so the inline input is visible.
        if matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O')) {
            let (allow_custom, is_on_other) = match &self.dialog_type {
                DialogType::Select {
                    allow_custom,
                    options,
                    selected_index,
                    ..
                } => (*allow_custom, *selected_index == options.len()),
                DialogType::MultiSelect {
                    allow_custom,
                    options,
                    cursor_index,
                    ..
                } => (*allow_custom, *cursor_index == options.len()),
                _ => (false, false),
            };

            if allow_custom && !is_on_other {
                self.custom_mode_active = true;
                // Move cursor to the Other row so the inline input is visible.
                match &mut self.dialog_type {
                    DialogType::Select {
                        selected_index,
                        options,
                        ..
                    } => {
                        *selected_index = options.len();
                    }
                    DialogType::MultiSelect {
                        cursor_index,
                        options,
                        ..
                    } => {
                        *cursor_index = options.len();
                    }
                    _ => {}
                }
                return None;
            }
        }

        // Priority 2.5: Any printable char pressed while cursor is on the "Other" row
        // → activate custom mode AND immediately insert the character.
        if self.cursor_on_other_row() {
            if let KeyCode::Char(c) = key.code {
                self.custom_mode_active = true;
                if let Some(ref mut input) = self.custom_input {
                    let byte_pos = Self::char_to_byte_offset(input, self.custom_cursor_pos);
                    input.insert(byte_pos, c);
                    self.custom_cursor_pos += 1;
                }
                return None;
            }
        }

        // Priority 2.7: Enter on Cancel or Submit virtual rows.
        if matches!(key.code, KeyCode::Enter) {
            if let Some(cursor) = self.current_cursor() {
                if Some(cursor) == self.cancel_virtual_index() {
                    return Some(DialogResult::Cancelled);
                }
                if Some(cursor) == self.submit_virtual_index() {
                    // Submit for MultiSelect: emit the selected set.
                    if let DialogType::MultiSelect {
                        selected_indices, ..
                    } = &self.dialog_type
                    {
                        let mut indices: Vec<usize> = selected_indices.iter().copied().collect();
                        indices.sort_unstable();
                        return Some(DialogResult::MultiSelected(indices));
                    }
                }
            }
        }

        // Priority 3: Handle normal dialog input
        match &mut self.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                allow_custom,
            } => {
                // If the cursor is on the virtual "Other" row and Enter is pressed,
                // activate custom text input instead of selecting a real option.
                if matches!(key.code, KeyCode::Enter)
                    && *allow_custom
                    && *selected_index == options.len()
                {
                    self.custom_mode_active = true;
                    return None;
                }
                Self::handle_select_key(key, options, selected_index, *allow_custom)
            }

            DialogType::MultiSelect {
                options,
                selected_indices,
                cursor_index,
                allow_custom,
            } => {
                if matches!(key.code, KeyCode::Enter)
                    && *allow_custom
                    && *cursor_index == options.len()
                {
                    self.custom_mode_active = true;
                    return None;
                }
                Self::handle_multiselect_key(
                    key,
                    options,
                    selected_indices,
                    cursor_index,
                    *allow_custom,
                )
            }

            DialogType::TextInput {
                input, cursor_pos, ..
            } => Self::handle_text_input_key(key, input, cursor_pos),

            DialogType::Confirm { selected, .. } => Self::handle_confirm_key(key, selected),
        }
    }

    /// Convert a char-index to its byte offset in `s`.
    fn char_to_byte_offset(s: &str, char_pos: usize) -> usize {
        s.char_indices()
            .nth(char_pos)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    }

    /// Handle key events when in custom text input mode
    fn handle_custom_input_key(&mut self, key: KeyEvent) -> Option<DialogResult> {
        match key.code {
            KeyCode::Char(c) => {
                if let Some(ref mut input) = self.custom_input {
                    let byte_pos = Self::char_to_byte_offset(input, self.custom_cursor_pos);
                    input.insert(byte_pos, c);
                    self.custom_cursor_pos += 1;
                }
                None
            }
            KeyCode::Backspace => {
                if self.custom_cursor_pos > 0 {
                    if let Some(ref mut input) = self.custom_input {
                        self.custom_cursor_pos -= 1;
                        let byte_pos = Self::char_to_byte_offset(input, self.custom_cursor_pos);
                        input.remove(byte_pos);
                    }
                }
                None
            }
            KeyCode::Delete => {
                if let Some(ref mut input) = self.custom_input {
                    let char_count = input.chars().count();
                    if self.custom_cursor_pos < char_count {
                        let byte_pos = Self::char_to_byte_offset(input, self.custom_cursor_pos);
                        input.remove(byte_pos);
                    }
                }
                None
            }
            KeyCode::Left => {
                self.custom_cursor_pos = self.custom_cursor_pos.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                if let Some(ref input) = self.custom_input {
                    let char_count = input.chars().count();
                    if self.custom_cursor_pos < char_count {
                        self.custom_cursor_pos += 1;
                    }
                }
                None
            }
            KeyCode::Home => {
                self.custom_cursor_pos = 0;
                None
            }
            KeyCode::End => {
                if let Some(ref input) = self.custom_input {
                    self.custom_cursor_pos = input.chars().count();
                }
                None
            }
            KeyCode::Enter => {
                use crossterm::event::KeyModifiers;
                // Shift+Enter or Alt/Option+Enter inserts a newline.
                // On macOS standard VT100 raw mode, Option+Enter arrives as
                // KeyCode::Enter + KeyModifiers::ALT (same as the main textarea).
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
                {
                    // Insert a newline instead of submitting.
                    if let Some(ref mut input) = self.custom_input {
                        let byte_pos = Self::char_to_byte_offset(input, self.custom_cursor_pos);
                        input.insert(byte_pos, '\n');
                        self.custom_cursor_pos += 1;
                    }
                    return None;
                }
                // Submit custom text
                if let Some(ref input) = self.custom_input {
                    if !input.trim().is_empty() {
                        Some(DialogResult::CustomText(input.clone()))
                    } else {
                        None // Don't submit empty custom text
                    }
                } else {
                    None
                }
            }
            KeyCode::Esc => {
                // Exit custom mode (return to normal selection)
                self.custom_mode_active = false;
                if let Some(ref mut input) = self.custom_input {
                    input.clear();
                }
                self.custom_cursor_pos = 0;
                None
            }
            _ => None,
        }
    }

    /// Handle key events for single-select dialogs.
    ///
    /// When `allow_custom` is true, index `options.len()` is the virtual "Other"
    /// row. Navigation extends one step further to allow reaching it.
    fn handle_select_key(
        key: KeyEvent,
        options: &[DialogOption],
        selected_index: &mut usize,
        allow_custom: bool,
    ) -> Option<DialogResult> {
        // Virtual rows: Other? (if allow_custom) then Cancel.
        // max_index is the Cancel button index.
        let max_index = options.len() + if allow_custom { 1 } else { 0 };

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected_index = selected_index.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected_index = (*selected_index + 1).min(max_index);
                None
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let num = c.to_digit(10).unwrap() as usize;
                if num > 0 && num <= options.len() {
                    Some(DialogResult::Selected(num - 1))
                } else {
                    None
                }
            }
            KeyCode::Enter => {
                // Defensive guard: only emit Selected for real option indices.
                // The "Other" row intercept in handle_key_event fires before we
                // reach here, but guard anyway (e.g. empty options list).
                if *selected_index < options.len() {
                    Some(DialogResult::Selected(*selected_index))
                } else {
                    None
                }
            }
            KeyCode::Esc => Some(DialogResult::Cancelled),
            _ => None,
        }
    }

    /// Handle key events for multi-select dialogs.
    ///
    /// When `allow_custom` is true, index `options.len()` is the virtual "Other"
    /// row. Navigation extends one step further to allow reaching it.
    fn handle_multiselect_key(
        key: KeyEvent,
        options: &[DialogOption],
        selected_indices: &mut HashSet<usize>,
        cursor_index: &mut usize,
        allow_custom: bool,
    ) -> Option<DialogResult> {
        // Virtual rows: Other? (if allow_custom) then Submit then Cancel.
        // max_index is the Cancel button index.
        let max_index = options.len() + if allow_custom { 2 } else { 1 };

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor_index = cursor_index.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *cursor_index = (*cursor_index + 1).min(max_index);
                None
            }
            KeyCode::Char(' ') => {
                // Only toggle real options — not the virtual "Other" row (which
                // has no corresponding index in selected_indices).
                if *cursor_index < options.len() {
                    if selected_indices.contains(cursor_index) {
                        selected_indices.remove(cursor_index);
                    } else {
                        selected_indices.insert(*cursor_index);
                    }
                }
                None
            }
            KeyCode::Enter => {
                let mut indices: Vec<usize> = selected_indices.iter().copied().collect();
                indices.sort_unstable();
                Some(DialogResult::MultiSelected(indices))
            }
            KeyCode::Esc => Some(DialogResult::Cancelled),
            _ => None,
        }
    }

    /// Handle key events for text input dialogs
    fn handle_text_input_key(
        key: KeyEvent,
        input: &mut String,
        cursor_pos: &mut usize,
    ) -> Option<DialogResult> {
        match key.code {
            KeyCode::Char(c) => {
                let byte_pos = Self::char_to_byte_offset(input, *cursor_pos);
                input.insert(byte_pos, c);
                *cursor_pos += 1;
                None
            }
            KeyCode::Backspace => {
                if *cursor_pos > 0 {
                    *cursor_pos -= 1;
                    let byte_pos = Self::char_to_byte_offset(input, *cursor_pos);
                    input.remove(byte_pos);
                }
                None
            }
            KeyCode::Delete => {
                let char_count = input.chars().count();
                if *cursor_pos < char_count {
                    let byte_pos = Self::char_to_byte_offset(input, *cursor_pos);
                    input.remove(byte_pos);
                }
                None
            }
            KeyCode::Left => {
                *cursor_pos = cursor_pos.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                *cursor_pos = (*cursor_pos + 1).min(input.chars().count());
                None
            }
            KeyCode::Home => {
                *cursor_pos = 0;
                None
            }
            KeyCode::End => {
                *cursor_pos = input.chars().count();
                None
            }
            KeyCode::Enter => Some(DialogResult::TextEntered(input.clone())),
            KeyCode::Esc => Some(DialogResult::Cancelled),
            _ => None,
        }
    }

    /// Handle key events for confirmation dialogs
    fn handle_confirm_key(key: KeyEvent, selected: &mut bool) -> Option<DialogResult> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                *selected = true;
                Some(DialogResult::Confirmed(true))
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                *selected = false;
                Some(DialogResult::Confirmed(false))
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                *selected = !*selected;
                None
            }
            KeyCode::Enter => Some(DialogResult::Confirmed(*selected)),
            KeyCode::Esc => Some(DialogResult::Cancelled),
            _ => None,
        }
    }
}

/// Result returned when a dialog is closed
#[derive(Debug, Clone, PartialEq)]
pub enum DialogResult {
    /// Single select - index of selected option
    Selected(usize),
    /// Multi select - indices of selected options (sorted)
    MultiSelected(Vec<usize>),
    /// Text input - entered string
    TextEntered(String),
    /// Custom "Other" text - user provided custom response
    CustomText(String),
    /// Confirmation - boolean result
    Confirmed(bool),
    /// User cancelled (pressed Esc)
    Cancelled,
}

impl DialogResult {
    /// Check if the result was cancelled
    pub fn is_cancelled(&self) -> bool {
        matches!(self, DialogResult::Cancelled)
    }

    /// Convert a cancelled result to an error
    pub fn ok_or_cancelled(self) -> Result<Self> {
        if self.is_cancelled() {
            anyhow::bail!("Dialog cancelled by user")
        } else {
            Ok(self)
        }
    }
}

/// Keep the control suffix (options, buttons, rules) inside `max_rows` and
/// clip the payload prefix when a long title or body would overflow.
///
/// `control_start` is the structural line index of the options divider, supplied
/// by the renderer — not inferred from painted glyphs. Pin whenever the suffix
/// fits (`control_phys <= max_rows`), including an exact fill with no payload.
/// If chrome makes the suffix one or more rows over but the options themselves
/// fit, keep the suffix tail so Yes/No stay. Top-clip + marker only when there
/// are too many options to present.
pub(crate) fn pin_dialog_controls(
    lines: Vec<String>,
    control_start: usize,
    max_rows: usize,
    width: usize,
    option_row_count: usize,
) -> Vec<String> {
    let width = width.max(1);
    let rows_of = |line: &str| super::shadow_buffer::physical_rows(line, width);
    let total: usize = lines.iter().map(|line| rows_of(line)).sum();
    if total <= max_rows {
        return lines;
    }

    let start = control_start.min(lines.len());
    let suffix = &lines[start..];
    let control_phys: usize = suffix.iter().map(|line| rows_of(line)).sum();
    if control_phys <= max_rows {
        let budget = max_rows - control_phys;
        let mut kept = Vec::new();
        let mut used = 0;
        for line in &lines[..start] {
            let rows = rows_of(line);
            if used + rows > budget {
                break;
            }
            used += rows;
            kept.push(line.clone());
        }
        kept.extend(suffix.iter().cloned());
        return kept;
    }

    if option_row_count < max_rows {
        return take_last_physical(suffix, max_rows, width);
    }

    let budget = max_rows.saturating_sub(1);
    let mut kept = Vec::new();
    let mut used = 0;
    for line in lines {
        let rows = rows_of(&line);
        if used + rows > budget {
            break;
        }
        used += rows;
        kept.push(line);
    }
    kept.push(super::shadow_buffer::truncate_to_columns(
        "… dialog clipped to viewport; use navigation keys …",
        width,
    ));
    kept
}

fn take_last_physical(lines: &[String], max_rows: usize, width: usize) -> Vec<String> {
    let rows_of = |line: &str| super::shadow_buffer::physical_rows(line, width);
    let mut kept = Vec::new();
    let mut used = 0;
    for line in lines.iter().rev() {
        let rows = rows_of(line);
        if used + rows > max_rows {
            break;
        }
        used += rows;
        kept.push(line.clone());
    }
    kept.reverse();
    kept
}

/// The speakable, copyable record of an answered dialog (#807).
///
/// Native scrollback must carry the question, the options, and what was
/// picked — never a pixel-only confirmation. The record is plain text: the
/// question (the title), one line per option with its radio/checkbox state at
/// submit time, and an explicit `Answer:` line naming the choice. Sanitised so
/// provider-supplied question or option text cannot smuggle control sequences
/// into the copyable record.
pub(crate) fn settled_dialog_record(dialog: &Dialog, result: &DialogResult) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (index, title_line) in dialog.title.lines().enumerate() {
        let prefix = if index == 0 { "? " } else { "  " };
        lines.push(format!("{prefix}{title_line}"));
    }

    let mut answer = String::new();
    match (&dialog.dialog_type, result) {
        (DialogType::Select { options, .. }, DialogResult::Selected(picked)) => {
            for (index, option) in options.iter().enumerate() {
                let marker = if index == *picked { "●" } else { "○" };
                lines.push(format!("  {marker} {}", option.label));
            }
            if let Some(option) = options.get(*picked) {
                answer.clone_from(&option.label);
            }
        }
        (DialogType::Select { .. }, DialogResult::CustomText(text)) => {
            lines.push(format!("  ● Other: {text}"));
            answer = format!("Other: {text}");
        }
        (DialogType::MultiSelect { options, .. }, DialogResult::MultiSelected(picked)) => {
            for (index, option) in options.iter().enumerate() {
                let marker = if picked.contains(&index) {
                    "☑"
                } else {
                    "☐"
                };
                lines.push(format!("  {marker} {}", option.label));
            }
            let labels: Vec<&str> = picked
                .iter()
                .filter_map(|index| options.get(*index))
                .map(|option| option.label.as_str())
                .collect();
            answer = if labels.is_empty() {
                "(none)".to_string()
            } else {
                labels.join(", ")
            };
        }
        (DialogType::MultiSelect { .. }, DialogResult::CustomText(text)) => {
            lines.push(format!("  ● Other: {text}"));
            answer = format!("Other: {text}");
        }
        (DialogType::Confirm { .. }, DialogResult::Confirmed(chosen)) => {
            answer = if *chosen { "Yes" } else { "No" }.to_string();
        }
        (DialogType::TextInput { .. }, DialogResult::TextEntered(text)) => {
            answer.clone_from(text);
        }
        (_, DialogResult::Cancelled) => {}
        // A submit never produces these pairings; if one ever does, the
        // question and the raw result are still recorded speakably.
        (dialog_type, other) => {
            lines.push(format!(
                "  (unresolved result {other:?} for {dialog_type:?})"
            ));
            answer = format!("{other:?}");
        }
    }

    if result.is_cancelled() {
        lines.push("✗ Dismissed without answering".to_string());
    } else {
        lines.push(format!("✓ Answer: {answer}"));
    }
    crate::cli::diff::sanitize_multiline(&lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip_sgr(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                match chars.peek() {
                    Some('[') => {
                        chars.next();
                        for nc in chars.by_ref() {
                            if nc.is_ascii_alphabetic() {
                                break;
                            }
                        }
                    }
                    Some(']') => {
                        chars.next();
                        for nc in chars.by_ref() {
                            if nc == '\x07' || nc == '\x1b' {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn test_dialog_option_creation() {
        let opt = DialogOption::new("Option 1");
        assert_eq!(opt.label, "Option 1");
        assert!(opt.description.is_none());

        let opt_with_desc = DialogOption::with_description("Option 2", "A description");
        assert_eq!(opt_with_desc.label, "Option 2");
        assert_eq!(opt_with_desc.description, Some("A description".to_string()));
    }

    #[test]
    fn test_select_dialog_creation() {
        let dialog = Dialog::select(
            "Choose one",
            vec![DialogOption::new("Option 1"), DialogOption::new("Option 2")],
        );
        assert_eq!(dialog.title, "Choose one");
        assert!(matches!(dialog.dialog_type, DialogType::Select { .. }));
    }

    #[test]
    fn test_select_navigation() {
        let mut dialog = Dialog::select(
            "Test",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
        );

        // Down arrow
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        assert!(result.is_none());

        // Enter
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Selected(1)));
    }

    #[test]
    fn test_select_number_keys() {
        let mut dialog =
            Dialog::select("Test", vec![DialogOption::new("A"), DialogOption::new("B")]);

        // Press '2' for second option
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(result, Some(DialogResult::Selected(1)));
    }

    #[test]
    fn test_multiselect_toggle() {
        let mut dialog =
            Dialog::multiselect("Test", vec![DialogOption::new("A"), DialogOption::new("B")]);

        // Toggle selection with space
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        assert!(result.is_none());

        // Move down
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        assert!(result.is_none());

        // Toggle second option
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        assert!(result.is_none());

        // Confirm
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::MultiSelected(vec![0, 1])));
    }

    #[test]
    fn test_text_input() {
        let mut dialog = Dialog::text_input("Enter text", None);

        // Type "hello"
        for c in "hello".chars() {
            let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
            assert!(result.is_none());
        }

        // Press enter
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("hello".to_string())));
    }

    #[test]
    fn test_text_input_backspace() {
        let mut dialog = Dialog::text_input("Enter text", Some("hello".to_string()));

        // Press backspace
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Backspace));
        assert!(result.is_none());

        // Confirm
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("hell".to_string())));
    }

    #[test]
    fn test_confirm_dialog() {
        let mut dialog = Dialog::confirm("Are you sure?", true);

        // Press 'n' for no
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('n')));
        assert_eq!(result, Some(DialogResult::Confirmed(false)));
    }

    #[test]
    fn test_confirm_toggle() {
        let mut dialog = Dialog::confirm("Are you sure?", true);

        // Press left/right to toggle
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Left));
        assert!(result.is_none());

        // Press enter
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Confirmed(false)));
    }

    #[test]
    fn test_cancel() {
        let mut dialog = Dialog::select("Test", vec![DialogOption::new("A")]);

        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, Some(DialogResult::Cancelled));
        assert!(result.unwrap().is_cancelled());
    }

    // ─── select navigation wrapping ──────────────────────────────────────────

    #[test]
    fn test_select_up_at_top_stays_at_zero() {
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // Already at 0, pressing up should not underflow
        dialog.handle_key_event(KeyEvent::from(KeyCode::Up));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Selected(0)));
    }

    #[test]
    fn test_select_down_reaches_cancel_button() {
        // With the Cancel virtual row, navigating past the last real option reaches
        // the Cancel button; pressing Enter there returns Cancelled.
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // 2 options, Cancel at index 2. Navigate down twice to reach it.
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Cancelled));
    }

    #[test]
    fn test_select_enter_on_real_option_still_works() {
        // Regular option selection still works with Enter — no Submit step needed.
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Selected(1)));
    }

    #[test]
    fn test_select_down_clamps_at_cancel() {
        // Pressing Down many times must not exceed the Cancel button index.
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        for _ in 0..10 {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        // Cancel is at options.len() = 2 for no-custom Select.
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Cancelled));
    }

    #[test]
    fn test_select_vim_keys_j_and_k() {
        let mut dialog = Dialog::select(
            "T",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
        );
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('j')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('j')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('k')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Selected(1)));
    }

    #[test]
    fn test_select_number_zero_is_ignored() {
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A")]);
        // '0' is out of range for 1-indexed selection
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('0')));
        assert!(result.is_none());
    }

    #[test]
    fn test_select_number_out_of_range_ignored() {
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A")]);
        // '9' > options.len() (1) — should be ignored
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('9')));
        assert!(result.is_none());
    }

    // ─── multiselect ─────────────────────────────────────────────────────────

    #[test]
    fn test_multiselect_empty_confirm() {
        let mut dialog = Dialog::multiselect("T", vec![DialogOption::new("A")]);
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::MultiSelected(vec![])));
    }

    #[test]
    fn test_multiselect_toggle_deselect() {
        let mut dialog = Dialog::multiselect("T", vec![DialogOption::new("A")]);
        // Select then deselect
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::MultiSelected(vec![])));
    }

    #[test]
    fn test_multiselect_result_is_sorted() {
        let mut dialog = Dialog::multiselect(
            "T",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
        );
        // Select C then B (reverse order)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' '))); // select C (index 2)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('k'))); // move up to B
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' '))); // select B (index 1)
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        if let Some(DialogResult::MultiSelected(indices)) = result {
            // Result must be sorted ascending
            assert_eq!(indices, vec![1, 2]);
        } else {
            panic!("Expected MultiSelected");
        }
    }

    #[test]
    fn test_multiselect_cancel() {
        let mut dialog = Dialog::multiselect("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert_eq!(result, Some(DialogResult::Cancelled));
    }

    // ─── text input editing ──────────────────────────────────────────────────

    #[test]
    fn test_text_input_left_right_cursor() {
        let mut dialog = Dialog::text_input("T", Some("ab".to_string()));
        // Cursor at end (2). Move left twice, then type 'X'
        dialog.handle_key_event(KeyEvent::from(KeyCode::Left));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Left));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('X')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("Xab".to_string())));
    }

    #[test]
    fn test_text_input_delete_key() {
        let mut dialog = Dialog::text_input("T", Some("abc".to_string()));
        // Cursor at end. Move to position 1, press Delete to remove 'b'
        dialog.handle_key_event(KeyEvent::from(KeyCode::Home));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Delete));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("ac".to_string())));
    }

    #[test]
    fn test_text_input_home_end() {
        let mut dialog = Dialog::text_input("T", Some("hello".to_string()));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Home));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('!')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            result,
            Some(DialogResult::TextEntered("!hello".to_string()))
        );
    }

    #[test]
    fn test_text_input_backspace_at_start_noop() {
        let mut dialog = Dialog::text_input("T", None);
        // Already empty, backspace should be a no-op
        dialog.handle_key_event(KeyEvent::from(KeyCode::Backspace));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("".to_string())));
    }

    #[test]
    fn test_text_input_cursor_cant_go_past_end() {
        let mut dialog = Dialog::text_input("T", Some("ab".to_string()));
        // Press right multiple times past end
        for _ in 0..5 {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        }
        // Should still produce "ab" without panic
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::TextEntered("ab".to_string())));
    }

    // ─── confirm dialog ───────────────────────────────────────────────────────

    #[test]
    fn test_confirm_yes_key() {
        let mut dialog = Dialog::confirm("Sure?", false);
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('y')));
        assert_eq!(result, Some(DialogResult::Confirmed(true)));
    }

    #[test]
    fn test_confirm_uppercase_y() {
        let mut dialog = Dialog::confirm("Sure?", false);
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('Y')));
        assert_eq!(result, Some(DialogResult::Confirmed(true)));
    }

    #[test]
    fn test_confirm_enter_uses_current_selected() {
        let mut dialog = Dialog::confirm("Sure?", true);
        // Default is true; press Enter immediately
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Confirmed(true)));
    }

    #[test]
    fn test_confirm_right_key_toggles() {
        let mut dialog = Dialog::confirm("Sure?", true);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Confirmed(false)));
    }

    #[test]
    fn test_confirm_multiline_prompt_stored_correctly() {
        // Regression: confirm("run this?\n\ncode") used to break box rendering
        // because the raw \n was passed to a single-line format string.
        // Now multi-line content goes in .with_body() and the prompt stays single-line.
        // The title holds the question; prompt is intentionally empty to avoid duplicate
        // display (the title is already shown in the dialog border/header).
        let dialog = Dialog::confirm("run this?", false).with_body(": foo 1 . ;");
        assert_eq!(dialog.title, "run this?");
        match &dialog.dialog_type {
            DialogType::Confirm { prompt, .. } => {
                assert!(!prompt.contains('\n'), "prompt must not contain newlines");
                // prompt is empty — the title is used for display to prevent duplication.
                assert!(
                    prompt.is_empty(),
                    "prompt must be empty; title holds the question"
                );
            }
            _ => panic!("expected Confirm dialog"),
        }
        assert_eq!(dialog.body.as_deref(), Some(": foo 1 . ;"));
    }

    // ─── DialogResult helpers ─────────────────────────────────────────────────

    #[test]
    fn test_dialog_result_ok_or_cancelled_err_on_cancel() {
        let result = DialogResult::Cancelled;
        assert!(result.ok_or_cancelled().is_err());
    }

    #[test]
    fn test_dialog_result_ok_or_cancelled_ok_on_select() {
        let result = DialogResult::Selected(0);
        assert!(result.ok_or_cancelled().is_ok());
    }

    #[test]
    fn test_dialog_result_is_cancelled_false_for_others() {
        assert!(!DialogResult::Selected(0).is_cancelled());
        assert!(!DialogResult::Confirmed(true).is_cancelled());
        assert!(!DialogResult::TextEntered("x".to_string()).is_cancelled());
        assert!(!DialogResult::MultiSelected(vec![]).is_cancelled());
        assert!(!DialogResult::CustomText("x".to_string()).is_cancelled());
    }

    // ─── custom text mode ─────────────────────────────────────────────────────

    #[test]
    fn test_custom_text_mode_activation() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(dialog.custom_mode_active);
    }

    #[test]
    fn test_custom_text_mode_input_and_submit() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        for c in "myvalue".chars() {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
        }
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            result,
            Some(DialogResult::CustomText("myvalue".to_string()))
        );
    }

    #[test]
    fn test_custom_text_mode_esc_exits_mode() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(dialog.custom_mode_active);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert!(!dialog.custom_mode_active);
    }

    #[test]
    fn test_custom_text_empty_does_not_submit() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        // Press enter with empty custom input — should not submit
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(result.is_none());
    }

    #[test]
    fn test_normal_select_ignores_o_without_allow_custom() {
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A")]);
        // 'o' key with allow_custom=false should not activate custom mode
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(!dialog.custom_mode_active);
    }

    // ─── custom text cursor movement ──────────────────────────────────────────

    #[test]
    fn test_custom_text_cursor_insert_at_position() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o'))); // enter custom mode
                                                                     // Type "ac"
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('c')));
        // Move to position 1 (between 'a' and 'c'), insert 'b'
        dialog.handle_key_event(KeyEvent::from(KeyCode::Home));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('b')));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::CustomText("abc".to_string())));
    }

    #[test]
    fn test_custom_text_cursor_left_right_movement() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('y')));
        // cursor is at 2 (end). Left moves to 1, Right brings back to 2.
        dialog.handle_key_event(KeyEvent::from(KeyCode::Left));
        assert_eq!(dialog.custom_cursor_pos, 1);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        assert_eq!(dialog.custom_cursor_pos, 2);
        // Right at end should not exceed char_count
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right));
        assert_eq!(dialog.custom_cursor_pos, 2);
    }

    #[test]
    fn test_custom_text_home_end_keys() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        for c in "hello".chars() {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(dialog.custom_cursor_pos, 5);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Home));
        assert_eq!(dialog.custom_cursor_pos, 0);
        dialog.handle_key_event(KeyEvent::from(KeyCode::End));
        assert_eq!(dialog.custom_cursor_pos, 5);
    }

    #[test]
    fn test_custom_text_delete_key() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('b')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('c')));
        // Move to position 1 and delete 'b' (the char at cursor)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Home));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Right)); // pos 1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Delete));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::CustomText("ac".to_string())));
    }

    #[test]
    fn test_custom_text_esc_resets_cursor() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(dialog.custom_cursor_pos, 2);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert_eq!(dialog.custom_cursor_pos, 0);
        assert!(!dialog.custom_mode_active);
    }

    #[test]
    fn test_custom_text_backspace_moves_cursor() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('a')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('b')));
        assert_eq!(dialog.custom_cursor_pos, 2);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(dialog.custom_cursor_pos, 1);
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::CustomText("a".to_string())));
    }

    #[test]
    fn test_custom_text_left_at_start_noop() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        // cursor already at 0
        dialog.handle_key_event(KeyEvent::from(KeyCode::Left));
        assert_eq!(dialog.custom_cursor_pos, 0);
    }

    // ─── help message ─────────────────────────────────────────────────────────

    #[test]
    fn test_dialog_with_help_message() {
        let dialog =
            Dialog::select("T", vec![DialogOption::new("A")]).with_help("Press Enter to confirm");
        assert_eq!(
            dialog.help_message.as_deref(),
            Some("Press Enter to confirm")
        );
    }

    #[test]
    fn test_dialog_no_help_message_by_default() {
        let dialog = Dialog::select("T", vec![DialogOption::new("A")]);
        assert!(dialog.help_message.is_none());
    }

    // ─── regression tests for #18 ────────────────────────────────────────────

    /// Regression #18: Esc in custom mode must return None (not Cancelled),
    /// keeping the dialog open and only exiting custom input mode.
    #[test]
    fn test_custom_mode_esc_exits_mode_not_dialog() {
        let mut d = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(d.custom_mode_active);
        let result = d.handle_key_event(KeyEvent::from(KeyCode::Esc));
        assert!(
            result.is_none(),
            "Esc in custom mode must return None, not Cancelled: {:?}",
            result
        );
        assert!(
            !d.custom_mode_active,
            "Custom mode must be inactive after Esc"
        );
    }

    /// Regression #18: allow_custom=false must not activate custom mode on 'o'.
    #[test]
    fn test_custom_mode_not_activated_when_disallowed() {
        let mut d = Dialog::select("T", vec![DialogOption::new("A")]);
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(
            !d.custom_mode_active,
            "Custom mode must not activate when allow_custom=false"
        );
    }

    // ─── regression: KeyEventKind::Press guard (issue #33-related) ───────────

    /// Regression: pressing 'o' to activate custom mode then typing "Hello" and
    /// pressing Enter must return `CustomText("Hello")`, NOT `CustomText("oHello")`.
    ///
    /// Root cause: `show_dialog` was processing both Press and Release events.
    /// When 'o' was pressed, the Press event activated `custom_mode_active`.
    /// The subsequent Release event then hit `handle_custom_input_key`, inserting
    /// the literal 'o' character into the text field.
    ///
    /// The fix (KeyEventKind::Press guard in show_dialog) is in the TUI layer, but
    /// we verify here that pressing 'o' then typing "Hello" works correctly at the
    /// Dialog struct level — i.e., 'o' must not be double-inserted.
    #[test]
    fn test_steering_dialog_o_key_does_not_double_insert() {
        let mut d = Dialog::select_with_custom(
            "Steer",
            vec![
                DialogOption::with_description("Continue", "Run another pass"),
                DialogOption::with_description("Approve", "Accept plan"),
            ],
        );

        // Press 'o' — activates custom mode
        d.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(d.custom_mode_active, "custom mode must be active after 'o'");

        // Verify that the custom input is empty (no 'o' leaked in at Dialog level)
        assert_eq!(
            d.custom_input.as_deref(),
            Some(""),
            "custom input must be empty right after 'o' activates custom mode"
        );

        // Type "Hello"
        for c in "Hello".chars() {
            d.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
        }

        // Enter submits the text
        let result = d.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            result,
            Some(DialogResult::CustomText("Hello".to_string())),
            "custom text must be exactly 'Hello' (no leading 'o')"
        );
    }

    // ─── navigation to "Other" row ────────────────────────────────────────────

    /// Regression: Down navigation must reach the virtual "Other" row
    /// when allow_custom = true, stopping at index == options.len().
    #[test]
    fn test_select_navigate_down_reaches_other_when_allow_custom() {
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // 2 real options → Other row is at index 2
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
        if let DialogType::Select { selected_index, .. } = &dialog.dialog_type {
            assert_eq!(*selected_index, 2, "cursor must reach Other row (index 2)");
        } else {
            panic!("unexpected dialog type");
        }
    }

    /// Pressing Down many times on a select_with_custom dialog must clamp at
    /// the Cancel button (options.len() + 1 — Other is options.len()).
    #[test]
    fn test_select_with_custom_down_clamps_at_cancel() {
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        for _ in 0..10 {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        if let DialogType::Select {
            selected_index,
            options,
            ..
        } = &dialog.dialog_type
        {
            // Cancel is at options.len() + 1 for allow_custom Select.
            assert_eq!(
                *selected_index,
                options.len() + 1,
                "cursor must clamp at Cancel (options.len()+1)"
            );
        } else {
            panic!("unexpected dialog type");
        }
    }

    /// Regression: pressing Enter when cursor is on the "Other" row must
    /// activate custom_mode_active and return None (not close the dialog).
    #[test]
    fn test_select_enter_on_other_activates_custom_mode() {
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(
            result.is_none(),
            "Enter on Other row must return None (not close dialog)"
        );
        assert!(
            dialog.custom_mode_active,
            "Enter on Other row must activate custom_mode_active"
        );
    }

    /// Regression: same as above but for MultiSelect.
    #[test]
    fn test_multiselect_navigate_down_reaches_other_when_allow_custom() {
        let mut dialog = Dialog::multiselect_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
        if let DialogType::MultiSelect {
            cursor_index,
            options,
            ..
        } = &dialog.dialog_type
        {
            assert_eq!(*cursor_index, options.len(), "cursor must reach Other row");
        } else {
            panic!("unexpected dialog type");
        }
    }

    /// Regression: pressing Enter when MultiSelect cursor is on "Other" must
    /// activate custom_mode_active.
    #[test]
    fn test_multiselect_enter_on_other_activates_custom_mode() {
        let mut dialog = Dialog::multiselect_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert!(result.is_none(), "Enter on Other row must not close dialog");
        assert!(
            dialog.custom_mode_active,
            "must activate custom_mode_active"
        );
    }

    /// Regression (#45): pressing a printable char on the MultiSelect "Other" row
    /// must immediately activate custom mode and insert the character — no Enter
    /// required.  Mirrors test_select_other_row_char_activates_custom_mode for
    /// the MultiSelect dialog type.
    #[test]
    fn test_multiselect_other_row_char_activates_custom_mode() {
        let mut dialog = Dialog::multiselect_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        // Navigate to Other row (index 2 for a 2-option multiselect).
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
                                                                // Press 'x' — must activate custom mode and insert the char.
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
        assert!(
            result.is_none(),
            "pressing char on Other must not close dialog"
        );
        assert!(
            dialog.custom_mode_active,
            "custom mode must activate on printable char in MultiSelect"
        );
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("x"),
            "char must be inserted into custom_input without pressing Enter first"
        );
    }

    /// Regression (#45): multiple chars typed on MultiSelect "Other" row accumulate.
    #[test]
    fn test_multiselect_other_row_char_accumulates_without_enter() {
        let mut dialog = Dialog::multiselect_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1 (Other)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("hi"),
            "chars must accumulate on MultiSelect Other row without pressing Enter"
        );
    }

    /// Guard: Space on the "Other" row in MultiSelect must not insert
    /// options.len() into selected_indices (would be an out-of-bounds index).
    #[test]
    fn test_multiselect_space_on_other_row_is_noop() {
        let mut dialog = Dialog::multiselect_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 0→1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // 1→2 (Other)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' '))); // Space on Other row
                                                                     // Verify selected_indices does NOT contain options.len() (= 2).
                                                                     // NOTE: Enter at the Other row activates custom_mode_active (not MultiSelected),
                                                                     // so we check the internal state directly.
        if let DialogType::MultiSelect {
            selected_indices,
            options,
            ..
        } = &dialog.dialog_type
        {
            assert!(
                !selected_indices.contains(&options.len()),
                "Space on Other row must not add options.len() ({}) to selected_indices",
                options.len()
            );
        } else {
            panic!("unexpected dialog type");
        }
    }

    /// Defensive guard: Enter with allow_custom=false and empty options must
    /// not panic. With the Cancel virtual row at index 0, Enter returns Cancelled.
    #[test]
    fn test_select_enter_empty_options_no_crash() {
        let mut dialog = Dialog::select("T", vec![]);
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        // Cancel is at index 0 for empty Select; Enter on Cancel → Cancelled.
        assert_eq!(
            result,
            Some(DialogResult::Cancelled),
            "Enter on empty options must return Cancelled (cursor on Cancel button)"
        );
    }

    // ─── 'o' shortcut cursor-jump regression ─────────────────────────────────

    /// Regression: pressing 'o' from a non-Other row must also move the cursor
    /// to the Other row so the inline input is visible.
    #[test]
    fn test_o_key_moves_cursor_to_other_row() {
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // Cursor starts at index 0, press 'o'
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(dialog.custom_mode_active, "'o' must activate custom mode");
        if let DialogType::Select {
            selected_index,
            options,
            ..
        } = &dialog.dialog_type
        {
            assert_eq!(
                *selected_index,
                options.len(),
                "'o' must move cursor to Other row (options.len())"
            );
        } else {
            panic!("expected Select");
        }
    }

    // ─── WS1b: immediate typing on "Other" row ────────────────────────────────

    /// Regression: navigating to the "Other" row and pressing a printable char
    /// must activate custom mode AND insert the character — no Enter required.
    #[test]
    fn test_select_other_row_char_activates_custom_mode() {
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // Navigate to Other row (index 2 for 2-option dialog)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        // Press 'h' — should activate custom mode and insert 'h'
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        assert!(
            result.is_none(),
            "pressing char on Other must not close dialog"
        );
        assert!(
            dialog.custom_mode_active,
            "custom mode must activate on printable char"
        );
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("h"),
            "char must be inserted into custom_input"
        );
    }

    /// Regression: multiple chars typed on "Other" row accumulate without
    /// requiring the user to press Enter first.
    #[test]
    fn test_select_other_row_char_accumulates_without_enter() {
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // → Other (index 1)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('i')));
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("hi"),
            "chars typed on Other row must accumulate in custom_input"
        );
    }

    // ─── WS2: Submit/Cancel virtual rows ─────────────────────────────────────

    /// The Cancel button must be reachable by Down navigation and pressing
    /// Enter there must return Cancelled (MultiSelect).
    #[test]
    fn test_multiselect_cancel_button_navigable() {
        let mut dialog =
            Dialog::multiselect("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // 2 options. Submit=2, Cancel=3.
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // →1
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // →2 (Submit)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down)); // →3 (Cancel)
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Cancelled));
    }

    /// The Submit button (MultiSelect) must emit MultiSelected when Enter is pressed.
    #[test]
    fn test_multiselect_submit_button_emits_selection() {
        let mut dialog =
            Dialog::multiselect("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // Toggle option 0
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        // Navigate to Submit (index 2 for 2-option no-custom multiselect)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::MultiSelected(vec![0])));
    }

    // ─── WS2: Shift+Enter inserts newline in custom mode ─────────────────────

    /// Pressing Shift+Enter (or Alt+Enter, which macOS sends in standard VT100 mode)
    /// while in custom text mode must insert '\n' rather than submitting the text.
    #[test]
    fn test_custom_mode_shift_enter_inserts_newline() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o'))); // activate custom mode
        for c in "hello".chars() {
            dialog.handle_key_event(KeyEvent::from(KeyCode::Char(c)));
        }
        // Shift+Enter → insert newline, NOT submit
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let result = dialog.handle_key_event(shift_enter);
        assert!(result.is_none(), "Shift+Enter must not submit");
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("hello\n"),
            "Shift+Enter must insert newline into custom_input"
        );
    }

    /// Alt+Enter (Option+Enter on macOS in standard VT100 raw mode) must also
    /// insert a newline — same as Shift+Enter.
    #[test]
    fn test_custom_mode_alt_enter_inserts_newline() {
        use crossterm::event::{KeyEvent, KeyModifiers};
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('x')));
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let result = dialog.handle_key_event(alt_enter);
        assert!(result.is_none(), "Alt+Enter must not submit");
        assert_eq!(
            dialog.custom_input.as_deref(),
            Some("x\n"),
            "Alt+Enter must insert newline into custom_input"
        );
    }

    // ── tool_approval factory ──────────────────────────────────────────────────

    #[test]
    fn test_tool_approval_non_mutating_has_three_options() {
        let dialog = Dialog::tool_approval("Read", "Read src/lib.rs");
        if let DialogType::Select { options, .. } = &dialog.dialog_type {
            assert_eq!(options.len(), 3);
            assert!(options[0].label.contains("Yes"));
            assert!(options[1].label.contains("don't ask again"));
            assert!(options[1].label.contains("Read:*"));
            assert!(options[2].label.contains("No"));
        } else {
            panic!("expected Select dialog");
        }
    }

    #[test]
    fn test_tool_approval_file_mutating_has_four_options() {
        for name in &["write", "Write", "edit", "Edit"] {
            let dialog = Dialog::tool_approval(name, "summary");
            if let DialogType::Select { options, .. } = &dialog.dialog_type {
                assert_eq!(options.len(), 4, "tool '{}' should have 4 options", name);
                assert!(
                    options[1].label.contains("$EDITOR"),
                    "tool '{}': option 2 should be Edit in $EDITOR",
                    name
                );
            } else {
                panic!("expected Select dialog for tool '{}'", name);
            }
        }
    }

    #[test]
    fn test_tool_approval_title_includes_name_and_summary() {
        let dialog = Dialog::tool_approval("Bash", "run command");
        assert!(dialog.title.contains("Bash"));
        assert!(dialog.title.contains("run command"));
    }

    #[test]
    fn test_file_approval_uses_shared_sanitized_diff_renderer() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "old\n").unwrap();
        let tool = crate::tools::ToolUse::new(
            "edit".into(),
            serde_json::json!({
                "file_path": file.path(),
                "old_string": "old\n",
                "new_string": "new\n"
            }),
        );
        let dialog = crate::cli::repl_event::tool_display::tool_approval_dialog(
            &tool,
            "File: src/\u{1b}[31mhostile.rs",
            &crate::theme::ColorTheme::Dark.to_scheme(),
            crate::cli::diff::DiffColorMode::NoColor,
        );
        let body = dialog.body.as_deref().unwrap();
        assert!(body.contains(file.path().to_string_lossy().as_ref()));
        assert!(body.contains("- old"));
        assert!(body.contains("+ new"));
        assert!(!dialog.title.contains('\u{1b}'));
        assert!(!body.contains('\u{1b}'));
    }

    #[test]
    fn test_file_approval_preview_composes_with_light_and_dark_themes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "old\n").unwrap();
        let tool = crate::tools::ToolUse::new(
            "edit".into(),
            serde_json::json!({
                "file_path": file.path(),
                "old_string": "old\n",
                "new_string": "new\n"
            }),
        );
        let dark = crate::cli::repl_event::tool_display::tool_approval_dialog(
            &tool,
            "File: src/theme.rs",
            &crate::theme::ColorTheme::Dark.to_scheme(),
            crate::cli::diff::DiffColorMode::Theme,
        );
        let light = crate::cli::repl_event::tool_display::tool_approval_dialog(
            &tool,
            "File: src/theme.rs",
            &crate::theme::ColorTheme::Light.to_scheme(),
            crate::cli::diff::DiffColorMode::Theme,
        );
        let dark_body = dark.body.unwrap();
        let light_body = light.body.unwrap();
        assert_ne!(dark_body, light_body);
        assert!(
            dark_body.contains("48;2;20;72;40") && dark_body.contains("38;2;236;246;238"),
            "dark approval diffs must fill add rows; body={dark_body}"
        );
        assert!(
            light_body.contains("48;2;204;240;214") && light_body.contains("38;2;12;56;28"),
            "light approval diffs must fill add rows; body={light_body}"
        );
    }

    #[test]
    fn body_scroll_supports_laptop_terminal_keys() {
        let mut dialog = Dialog::select("Plan", vec![DialogOption::new("Approve")])
            .with_body("one\ntwo\nthree\nfour\nfive\nsix");

        dialog.handle_key_event(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert_eq!(dialog.body_scroll_offset, 5);
        dialog.handle_key_event(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(dialog.body_scroll_offset, 0);

        dialog.body_scroll_offset = 3;
        dialog.handle_key_event(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(dialog.body_scroll_offset, 0);
        dialog.handle_key_event(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(dialog.body_scroll_offset, usize::MAX);
    }

    // ── random key input (fuzz-style) ─────────────────────────────────────────
    // These tests feed a variety of characters and nav keys into each dialog
    // type while in typing mode, verifying no panics occur regardless of input.

    #[test]
    fn test_text_input_random_chars_do_not_panic() {
        use crossterm::event::KeyModifiers;
        let mut dialog = Dialog::text_input("Enter text", None);
        let chars = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 !@#$%^&*()-_=+[]{}|;':\",./<>?";
        for ch in chars.chars() {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Nav + editing keys
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Backspace,
            KeyCode::Delete,
        ] {
            dialog.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    #[test]
    fn test_text_input_unicode_chars_do_not_panic() {
        use crossterm::event::KeyModifiers;
        let mut dialog = Dialog::text_input("Enter text", None);
        // Multi-byte chars
        for ch in "héllo wörld 日本語 中文 한국어 🦀".chars() {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        // Move cursor through the whole string without panicking
        for _ in 0..40 {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        for _ in 0..40 {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        }
        // Delete from end
        for _ in 0..60 {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
    }

    #[test]
    fn test_custom_mode_random_chars_do_not_panic() {
        use crossterm::event::KeyModifiers;
        let mut dialog = Dialog::select_with_custom("T", vec![DialogOption::new("A")]);
        // Navigate to Other row and activate custom mode
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        let chars = "hello world héllo 日本語 🦀 !@#$%";
        for ch in chars.chars() {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::Backspace,
            KeyCode::Delete,
        ] {
            dialog.handle_key_event(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }

    #[test]
    fn test_select_nav_keys_do_not_panic() {
        use crossterm::event::KeyModifiers;
        let mut dialog = Dialog::select(
            "T",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
                DialogOption::new("C"),
            ],
        );
        // Hammer up/down/number keys well past bounds
        for _ in 0..20 {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        for _ in 0..20 {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        }
        for ch in '1'..='9' {
            dialog.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    fn visible_dialog_text(lines: &[String]) -> String {
        lines
            .iter()
            .map(|line| strip_sgr(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn control_row(lines: &[String], needle: &str) -> Option<usize> {
        lines
            .iter()
            .position(|line| strip_sgr(line).contains(needle))
    }

    fn plan_approval_frame(
        dialog: &Dialog,
        width: usize,
        height: usize,
    ) -> super::super::LiveFrame {
        let input = vec!["draft stays hidden".to_string()];
        let mut autocomplete = super::super::AutocompleteState::new();
        let inputs = super::super::view_model::LiveViewModel {
            terminal_width: width,
            terminal_height: height,
            input_lines: &input,
            input_cursor: (0, 0),
            ghost_text: None,
            effective_status: "approval pending",
            cwd_label: "~/repos/finch",
            session_label: "jade-river",
            dialog: Some(dialog),
            expanded_lines: None,
            render_error: false,
            task_rows: &[],
            tracked_rows: &[],
            live_rendered: &[],
        };
        super::super::plan_live_frame(&inputs, &mut autocomplete)
    }

    /// A frame with a live conversation streaming above an open dialog.
    fn plan_conversation_with_dialog(
        dialog: &Dialog,
        width: usize,
        height: usize,
    ) -> super::super::LiveFrame {
        let live: Vec<super::super::accordion::RenderedTranscriptLine> = (0..40)
            .map(|row| super::super::accordion::RenderedTranscriptLine {
                text: format!("conversation row {row}"),
                ..super::super::accordion::RenderedTranscriptLine::default()
            })
            .collect();
        let input = vec![String::new()];
        let mut autocomplete = super::super::AutocompleteState::new();
        let inputs = super::super::view_model::LiveViewModel {
            terminal_width: width,
            terminal_height: height,
            input_lines: &input,
            input_cursor: (0, 0),
            ghost_text: None,
            effective_status: "approval pending",
            cwd_label: "~/repos/finch",
            session_label: "jade-river",
            dialog: Some(dialog),
            expanded_lines: None,
            render_error: false,
            task_rows: &[],
            tracked_rows: &[],
            live_rendered: &live,
        };
        super::super::plan_live_frame(&inputs, &mut autocomplete)
    }

    /// The card's painted lines: the trailing physical rows of the frame up to
    /// the card's claimed height (the card is the frame's last region).
    fn card_lines_of(frame: &super::super::LiveFrame, width: usize) -> Vec<String> {
        let card_height = frame.rects.dialog_card.height;
        let mut lines = Vec::new();
        let mut used = 0usize;
        for line in frame.lines.iter().rev() {
            lines.push(line.clone());
            used += super::super::shadow_buffer::physical_rows(line, width);
            if used >= card_height {
                break;
            }
        }
        lines.reverse();
        lines
    }

    fn plain_text(lines: &[String]) -> String {
        lines
            .iter()
            .map(|line| strip_sgr(line))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// #807: an open dialog is an inline region of the conversation — a
    /// claimed card below a still-projected transcript, with radio markers
    /// inside the card and focus driving the untouched state machine.
    #[test]
    fn test_open_dialog_is_an_inline_region_in_the_conversation_frame() {
        let width = 80;
        let height = 24;
        let mut dialog = Dialog::select(
            "Which database should the migration target?",
            vec![
                DialogOption::new("production"),
                DialogOption::new("staging"),
            ],
        );
        let frame = plan_conversation_with_dialog(&dialog, width, height);
        let card = frame.rects.dialog_card;

        assert!(
            !card.is_empty() && card.width == width && card.bottom() <= height,
            "the open dialog must claim an inline card region inside the frame; \
             card={card:?} frame={width}x{height}"
        );
        assert!(
            frame.rects.transcript.height > 0,
            "the conversation must stay projected above the card: rects={:?}",
            frame.rects
        );
        assert!(
            frame.rects.transcript.bottom() <= card.y,
            "the card must not overlap the transcript region: transcript={:?} card={card:?}",
            frame.rects.transcript
        );
        assert!(
            frame.physical_rows(width) <= height,
            "the card must never push the frame past the terminal: painted={} height={height}",
            frame.physical_rows(width)
        );
        assert_eq!(
            card.bottom(),
            frame.physical_rows(width),
            "the card must claim the frame's trailing rows, exactly where its lines \
             are painted: card={card:?} painted={}",
            frame.physical_rows(width)
        );
        assert_eq!(
            frame.rects.separator.bottom(),
            card.y,
            "the separator must be claimed directly above the card, matching the paint \
             order: separator={:?} card={card:?}",
            frame.rects.separator
        );

        // The conversation ScrollView keeps the transcript claim it had
        // without a dialog, so the open card does not own the whole viewport.
        let mut scroll_view = super::super::scroll_view::TranscriptScrollView::new();
        scroll_view.set_claim(frame.rects.transcript);
        assert!(
            scroll_view.owns(0, frame.rects.transcript.y as u16),
            "the ScrollView must own the projected conversation above the card; \
             claim={:?}",
            frame.rects.transcript
        );

        // The card paints the question and radio markers; Space toggles the
        // focused radio inside the still-focused state machine.
        let card_text = plain_text(&card_lines_of(&frame, width));
        assert!(
            card_text.contains("Which database should the migration target?")
                && card_text.contains("● production")
                && card_text.contains("○ staging"),
            "the card must render the question with radio markers:\n{card_text}"
        );

        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        let focused = plan_conversation_with_dialog(&dialog, width, height);
        let focused_text = plain_text(&card_lines_of(&focused, width));
        assert!(
            focused_text.contains("○ production") && focused_text.contains("● staging"),
            "Space/arrows must toggle the focused radio inside the card:\n{focused_text}"
        );
        assert_eq!(
            dialog.handle_key_event(KeyEvent::from(KeyCode::Enter)),
            Some(DialogResult::Selected(1)),
            "Enter must submit the focused option through the untouched state machine"
        );
    }

    /// #435 regression, card-shaped (#807): a long write preview scrolls
    /// INSIDE the card's box — Yes/No stay pinned inside the card's claimed
    /// rect at the same trailing rows, the card's height never moves, and the
    /// frame never overflows the terminal. The old overlay had no claimed card
    /// region at all, so the claimed-rect assertions fail against it.
    #[test]
    fn test_write_approval_card_keeps_controls_inside_the_card_while_body_scrolls() {
        let body = (0..400)
            .map(|i| format!("payload-line-{i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let dialog = Dialog::tool_approval(
            "Write",
            "overwrite docs.html, 12 KB, replacing existing content",
        )
        .with_body(body);
        let width = 72;
        let height = 18;

        let frame = plan_approval_frame(&dialog, width, height);
        let card = frame.rects.dialog_card;
        let card_text = plain_text(&card_lines_of(&frame, width));
        assert!(
            !card.is_empty() && card.bottom() <= height,
            "the open approval must claim its card inside the frame: card={card:?} \
             frame={width}x{height}"
        );
        assert!(
            card_text.contains("1. Yes") && card_text.contains("4. No"),
            "Yes/No must be inside the card's claimed rect: card={card:?}\n{card_text}"
        );
        assert!(
            card_text.contains("payload-line-000"),
            "the preview body must render inside the card:\n{card_text}"
        );
        assert!(
            frame.physical_rows(width) <= height,
            "the card must never push the frame past the terminal: painted={} height={height}",
            frame.physical_rows(width)
        );
        assert_eq!(
            card.bottom(),
            frame.physical_rows(width),
            "the card must claim the frame's trailing rows, exactly where its lines \
             are painted: card={card:?} painted={}",
            frame.physical_rows(width)
        );
        let yes_trailing = card_text
            .lines()
            .position(|line| line.contains("1. Yes"))
            .expect("Yes inside the card");

        let mut scrolled = dialog.clone();
        scrolled.body_scroll_offset = 80;
        let scrolled_frame = plan_approval_frame(&scrolled, width, height);
        let scrolled_card = scrolled_frame.rects.dialog_card;
        let scrolled_text = plain_text(&card_lines_of(&scrolled_frame, width));
        assert_eq!(
            card.height, scrolled_card.height,
            "scrolling the body must not resize the card: {card:?} vs {scrolled_card:?}"
        );
        let scrolled_yes = scrolled_text
            .lines()
            .position(|line| line.contains("1. Yes"))
            .expect("Yes inside the card after scroll");
        assert_eq!(
            yes_trailing, scrolled_yes,
            "Yes must keep its row inside the card while the body scrolls:\nbefore:\n{card_text}\
             \nafter:\n{scrolled_text}"
        );
        assert!(
            !scrolled_text.contains("payload-line-000")
                && scrolled_text.contains("payload-line-080"),
            "the body must scroll INSIDE the card, not move the card:\n{scrolled_text}"
        );
    }

    /// The exact-fit budget the overlay guaranteed at tiny terminals carries
    /// over to the card: on an 8-row frame the card keeps Yes/No visible.
    #[test]
    fn test_dialog_card_keeps_approval_controls_on_a_tiny_frame() {
        let (dialog, payload_bytes) = huge_html_write_dialog();
        let width = 80;
        let height = 8;
        let frame = plan_approval_frame(&dialog, width, height);
        let card = frame.rects.dialog_card;
        let card_text = plain_text(&card_lines_of(&frame, width));
        assert!(
            card_text.contains("1. Yes") && card_text.contains("4. No"),
            "an exact-fit card must keep approve/deny on an 8-row frame: card={card:?} \
             payload_bytes={payload_bytes}\n{card_text}"
        );
        assert!(
            !card_text.contains("dialog clipped to viewport"),
            "the exact-fit suffix must not take the too-many-options top-clip: \
             card={card:?} payload_bytes={payload_bytes}\n{card_text}"
        );
    }

    /// A minified HTML write used to push approve/deny off-screen before the
    /// payload was bounded. One long line wraps into hundreds of title rows.
    fn huge_html_write_dialog() -> (Dialog, usize) {
        let html = format!(
            "<!DOCTYPE html>{}",
            " <div class=\"doc\">page content</div>".repeat(800)
        );
        let payload_bytes = html.len();
        let dialog = Dialog::tool_approval("Write", &format!("Create docs.html\n{html}"));
        (dialog, payload_bytes)
    }

    #[test]
    fn test_long_write_payload_keeps_approval_controls_visible() {
        let (dialog, payload_bytes) = huge_html_write_dialog();
        let width = 80;
        let height = 16;
        let lines = super::super::TuiRenderer::dialog_lines(&dialog, width, height);
        let painted: usize = lines
            .iter()
            .map(|line| super::super::shadow_buffer::physical_rows(line, width))
            .sum();
        let visible = visible_dialog_text(&lines);
        let yes_row = control_row(&lines, "1. Yes");
        let no_row = control_row(&lines, "No");
        assert!(
            yes_row.is_some() && no_row.is_some() && painted <= height,
            "approval controls must stay in the viewport: height={height} painted={painted} \
             payload_bytes={payload_bytes} yes_row={yes_row:?} no_row={no_row:?}\n{visible}"
        );
        assert!(
            yes_row.unwrap() < lines.len() && no_row.unwrap() < lines.len(),
            "control rows must be inside the painted dialog: yes_row={yes_row:?} \
             no_row={no_row:?} painted_lines={} height={height} payload_bytes={payload_bytes}",
            lines.len()
        );
    }

    #[test]
    fn test_long_write_payload_live_frame_keeps_controls_on_screen() {
        let (dialog, payload_bytes) = huge_html_write_dialog();
        let width = 80;
        let height = 16;
        let frame = plan_approval_frame(&dialog, width, height);
        let painted = frame.physical_rows(width);
        let visible = visible_dialog_text(&frame.lines);
        let yes_row = control_row(&frame.lines, "1. Yes");
        let no_row = control_row(&frame.lines, "No");
        assert!(
            yes_row.is_some() && no_row.is_some() && painted <= height,
            "plan_live_frame must keep approve/deny on-screen: viewport={width}x{height} \
             painted={painted} payload_bytes={payload_bytes} yes_row={yes_row:?} \
             no_row={no_row:?}\n{visible}"
        );
    }

    #[test]
    fn test_long_body_scroll_moves_payload_not_controls() {
        let body = (0..400)
            .map(|i| format!("payload-line-{i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut dialog = Dialog::tool_approval(
            "Write",
            "overwrite docs.html, 12 KB, replacing existing content",
        )
        .with_body(body);
        let width = 72;
        let height = 18;

        let top = super::super::TuiRenderer::dialog_lines(&dialog, width, height);
        let top_text = visible_dialog_text(&top);
        let yes_top = control_row(&top, "1. Yes");
        assert!(
            yes_top.is_some() && top_text.contains("No"),
            "controls must be visible before scrolling: viewport={width}x{height} \
             yes_row={yes_top:?} painted={}\n{top_text}",
            top.len()
        );

        dialog.body_scroll_offset = 80;
        let scrolled = super::super::TuiRenderer::dialog_lines(&dialog, width, height);
        let scrolled_text = visible_dialog_text(&scrolled);
        let yes_scrolled = control_row(&scrolled, "1. Yes");
        assert!(
            yes_scrolled.is_some() && scrolled_text.contains("No"),
            "scrolling the payload must not move controls off-screen: viewport={width}x{height} \
             yes_row={yes_scrolled:?} offset={}\n{scrolled_text}",
            dialog.body_scroll_offset
        );
        assert!(
            top_text.contains("payload-line-000"),
            "top window must show the first payload line: viewport={width}x{height}\n{top_text}"
        );
        assert!(
            !scrolled_text.contains("payload-line-000"),
            "scrolling must drop the first payload line: offset={} viewport={width}x{height}\n\
             {scrolled_text}",
            dialog.body_scroll_offset
        );
        assert!(
            scrolled_text.contains("payload-line-080"),
            "scrolling must show a later payload line: offset={} viewport={width}x{height}\n\
             {scrolled_text}",
            dialog.body_scroll_offset
        );
        let no_top = control_row(&top, "4. No");
        let no_scrolled = control_row(&scrolled, "4. No");
        assert_eq!(
            yes_top.map(|row| top.len() - row),
            yes_scrolled.map(|row| scrolled.len() - row),
            "Yes must occupy the same trailing row after scroll: top={yes_top:?} \
             scrolled={yes_scrolled:?} viewport={width}x{height}"
        );
        assert_eq!(
            no_top.map(|row| top.len() - row),
            no_scrolled.map(|row| scrolled.len() - row),
            "No must occupy the same trailing row after scroll: top={no_top:?} \
             scrolled={no_scrolled:?} viewport={width}x{height}"
        );
    }

    #[test]
    fn test_write_approval_summarises_instead_of_dumping_html() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("docs.html");
        let html = format!(
            "<!DOCTYPE html>{}",
            " <div class=\"doc\">page content</div>".repeat(800)
        );
        let tool = crate::tools::ToolUse::new(
            "write".into(),
            serde_json::json!({
                "file_path": path.to_string_lossy(),
                "content": html
            }),
        );
        let summary = crate::cli::repl_event::event_loop::tool_approval_summary(&tool);
        assert!(
            !summary.contains("<!DOCTYPE") && !summary.contains("page content"),
            "write approval must summarise, not dump the file: {summary:?}"
        );
        assert!(
            summary.contains("docs.html") && summary.contains("create"),
            "write approval must lead with path and created-vs-overwritten: {summary:?}"
        );
        assert!(
            summary.contains("KB") || summary.contains("bytes") || summary.contains("MB"),
            "write approval must include a byte count: {summary:?}"
        );

        let dialog = crate::cli::repl_event::tool_display::tool_approval_dialog(
            &tool,
            &summary,
            &crate::theme::ColorScheme::default(),
            crate::cli::diff::DiffColorMode::NoColor,
        );
        assert!(
            dialog.body.is_some(),
            "full content must remain reachable behind the body disclosure"
        );
        let width = 80;
        let height = 16;
        let lines = super::super::TuiRenderer::dialog_lines(&dialog, width, height);
        let visible = visible_dialog_text(&lines);
        let yes_row = control_row(&lines, "1. Yes");
        assert!(
            yes_row.is_some() && visible.contains("No"),
            "assembled write approval must keep controls visible: height={height} \
             payload_bytes={} yes_row={yes_row:?}\n{visible}",
            html.len()
        );
    }

    #[test]
    fn test_long_payload_does_not_shift_other_row_virtual_index() {
        let body = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut dialog =
            Dialog::select_with_custom("T", vec![DialogOption::new("A")]).with_body(body);
        assert_eq!(
            dialog.cancel_virtual_index(),
            Some(2),
            "Select layout is options | Other | Cancel"
        );
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(
            dialog.custom_mode_active,
            "o must still activate the Other row when the payload is long"
        );
        assert_eq!(
            dialog.current_cursor(),
            Some(1),
            "Other-row activation must keep virtual indices stable"
        );
        assert_eq!(dialog.cancel_virtual_index(), Some(2));
    }

    #[test]
    fn test_exact_fit_viewport_keeps_approval_controls_visible() {
        // Write-approval suffix is 8 painted rows at width 80 (options divider,
        // four options, buttons divider, Cancel, bottom rule). plan_live_frame
        // spends one row on the session separator, so an 8-row terminal gives
        // the dialog 7 rows. Pin must keep Yes/No even when chrome is one row
        // over, not top-clip to a marker.
        let (dialog, payload_bytes) = huge_html_write_dialog();
        let width = 80;
        let height = 8;
        let dialog_budget = 7;

        let lines = super::super::TuiRenderer::dialog_lines(&dialog, width, dialog_budget);
        let painted: usize = lines
            .iter()
            .map(|line| super::super::shadow_buffer::physical_rows(line, width))
            .sum();
        let visible = visible_dialog_text(&lines);
        let yes_row = control_row(&lines, "1. Yes");
        let no_row = control_row(&lines, "4. No");
        assert!(
            yes_row.is_some() && no_row.is_some() && painted <= dialog_budget,
            "exact-fit pin must keep approve/deny: max_rows={dialog_budget} painted={painted} \
             payload_bytes={payload_bytes} yes_row={yes_row:?} no_row={no_row:?}\n{visible}"
        );
        assert!(
            !visible.contains("dialog clipped to viewport"),
            "exact-fit suffix must not take the too-many-options top-clip: \
             max_rows={dialog_budget} payload_bytes={payload_bytes}\n{visible}"
        );

        let frame = plan_approval_frame(&dialog, width, height);
        let frame_painted = frame.physical_rows(width);
        let frame_text = visible_dialog_text(&frame.lines);
        let frame_yes = control_row(&frame.lines, "1. Yes");
        let frame_no = control_row(&frame.lines, "4. No");
        assert!(
            frame_yes.is_some() && frame_no.is_some() && frame_painted <= height,
            "plan_live_frame 80x8 must keep approve/deny: painted={frame_painted} \
             payload_bytes={payload_bytes} yes_row={frame_yes:?} no_row={frame_no:?}\n{frame_text}"
        );
        assert!(
            !frame_text.contains("dialog clipped to viewport"),
            "live-frame exact-fit must not show the clip marker: height={height} \
             payload_bytes={payload_bytes}\n{frame_text}"
        );
    }

    #[test]
    fn test_markdown_body_bullets_do_not_steal_approval_controls() {
        let decoy = format!(
            "● 1. Yes\n○ spoofed option\n{}\n{}",
            "─".repeat(40),
            (0..200)
                .map(|i| format!("markdown-item-{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let dialog = Dialog::tool_approval("Write", "create docs.html, 12 bytes").with_body(decoy);
        let width = 80;
        let height = 16;
        let lines = super::super::TuiRenderer::dialog_lines(&dialog, width, height);
        let painted: usize = lines
            .iter()
            .map(|line| super::super::shadow_buffer::physical_rows(line, width))
            .sum();
        let visible = visible_dialog_text(&lines);
        let no_row = control_row(&lines, "4. No");
        let cancel_row = control_row(&lines, "[ Cancel ]");
        assert!(
            no_row.is_some() && cancel_row.is_some() && painted <= height,
            "structural pin must ignore payload bullets: height={height} painted={painted} \
             no_row={no_row:?} cancel_row={cancel_row:?}\n{visible}"
        );
        assert!(
            !visible.contains("dialog clipped to viewport"),
            "a bullet in the body must not inflate the suffix into the top-clip path:\n{visible}"
        );
    }
}
