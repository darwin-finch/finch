// Dialog - Native ratatui dialog system for user interaction
//
// Replaces inquire menus with ratatui-integrated dialogs that work seamlessly
// with the TUI, avoiding the need for suspend/resume.

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent};
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
    pub custom_input: Option<String>, // Stores custom text if "Other" is being entered
    pub custom_mode_active: bool,     // Whether user is currently typing custom text
    pub custom_cursor_pos: usize,     // Char-index cursor in custom_input
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
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
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
            custom_input: Some(String::new()),
            custom_mode_active: false,
            custom_cursor_pos: 0,
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
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
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
            custom_input: Some(String::new()),
            custom_mode_active: false,
            custom_cursor_pos: 0,
        }
    }

    /// Create a new text input dialog
    pub fn text_input(title: impl Into<String>, default: Option<String>) -> Self {
        let title_str = title.into();
        Self {
            title: title_str.clone(),
            dialog_type: DialogType::TextInput {
                prompt: title_str,
                input: default.clone().unwrap_or_default(),
                cursor_pos: default.as_ref().map(|s| s.len()).unwrap_or(0),
                default,
            },
            help_message: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
        }
    }

    /// Create a new confirmation dialog
    pub fn confirm(title: impl Into<String>, default: bool) -> Self {
        let title_str = title.into();
        Self {
            title: title_str.clone(),
            dialog_type: DialogType::Confirm {
                prompt: title_str,
                default,
                selected: default,
            },
            help_message: None,
            custom_input: None,
            custom_mode_active: false,
            custom_cursor_pos: 0,
        }
    }

    /// Set the help message for this dialog
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help_message = Some(help.into());
        self
    }

    /// Returns the virtual index of the Cancel button for Select/MultiSelect dialogs.
    ///
    /// Layout (Select):   real_options | Other? | Cancel
    /// Layout (MultiSelect): real_options | Other? | Submit | Cancel
    pub fn cancel_virtual_index(&self) -> Option<usize> {
        match &self.dialog_type {
            DialogType::Select { options, allow_custom, .. } => {
                Some(options.len() + if *allow_custom { 1 } else { 0 })
            }
            DialogType::MultiSelect { options, allow_custom, .. } => {
                Some(options.len() + if *allow_custom { 2 } else { 1 })
            }
            _ => None,
        }
    }

    /// Returns the virtual index of the Submit button (MultiSelect only).
    pub fn submit_virtual_index(&self) -> Option<usize> {
        match &self.dialog_type {
            DialogType::MultiSelect { options, allow_custom, .. } => {
                Some(options.len() + if *allow_custom { 1 } else { 0 })
            }
            _ => None,
        }
    }

    /// Returns true when the cursor is on the virtual "Other" row.
    fn cursor_on_other_row(&self) -> bool {
        match &self.dialog_type {
            DialogType::Select { options, selected_index, allow_custom, .. } => {
                *allow_custom && *selected_index == options.len()
            }
            DialogType::MultiSelect { options, cursor_index, allow_custom, .. } => {
                *allow_custom && *cursor_index == options.len()
            }
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

    /// Handle a key event and return a result if the dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<DialogResult> {
        // Priority 1: Handle custom text input mode
        if self.custom_mode_active {
            return self.handle_custom_input_key(key);
        }

        // Priority 2: 'o'/'O' activates custom mode — but NOT when already on the "Other"
        // row (priority 2.5 handles that case, inserting the char directly).
        // Also moves the cursor to the Other row so the inline input is visible.
        if matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O')) {
            let (allow_custom, is_on_other) = match &self.dialog_type {
                DialogType::Select { allow_custom, options, selected_index, .. } => {
                    (*allow_custom, *selected_index == options.len())
                }
                DialogType::MultiSelect { allow_custom, options, cursor_index, .. } => {
                    (*allow_custom, *cursor_index == options.len())
                }
                _ => (false, false),
            };

            if allow_custom && !is_on_other {
                self.custom_mode_active = true;
                // Move cursor to the Other row so the inline input is visible.
                match &mut self.dialog_type {
                    DialogType::Select { selected_index, options, .. } => {
                        *selected_index = options.len();
                    }
                    DialogType::MultiSelect { cursor_index, options, .. } => {
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
                    if let DialogType::MultiSelect { selected_indices, .. } = &self.dialog_type {
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
                if key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) {
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
                // Insert character at cursor position
                input.insert(*cursor_pos, c);
                *cursor_pos += 1;
                None
            }
            KeyCode::Backspace => {
                // Delete character before cursor
                if *cursor_pos > 0 {
                    input.remove(*cursor_pos - 1);
                    *cursor_pos -= 1;
                }
                None
            }
            KeyCode::Delete => {
                // Delete character at cursor
                if *cursor_pos < input.len() {
                    input.remove(*cursor_pos);
                }
                None
            }
            KeyCode::Left => {
                *cursor_pos = cursor_pos.saturating_sub(1);
                None
            }
            KeyCode::Right => {
                *cursor_pos = (*cursor_pos + 1).min(input.len());
                None
            }
            KeyCode::Home => {
                *cursor_pos = 0;
                None
            }
            KeyCode::End => {
                *cursor_pos = input.len();
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!d.custom_mode_active, "Custom mode must be inactive after Esc");
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
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
        assert!(dialog.custom_mode_active, "must activate custom_mode_active");
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        // Cursor starts at index 0, press 'o'
        dialog.handle_key_event(KeyEvent::from(KeyCode::Char('o')));
        assert!(dialog.custom_mode_active, "'o' must activate custom mode");
        if let DialogType::Select { selected_index, options, .. } = &dialog.dialog_type {
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A"), DialogOption::new("B")],
        );
        // Navigate to Other row (index 2 for 2-option dialog)
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        // Press 'h' — should activate custom mode and insert 'h'
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('h')));
        assert!(result.is_none(), "pressing char on Other must not close dialog");
        assert!(dialog.custom_mode_active, "custom mode must activate on printable char");
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
        let mut dialog = Dialog::select_with_custom(
            "T",
            vec![DialogOption::new("A")],
        );
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
}
