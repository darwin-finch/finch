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

    /// Handle a key event and return a result if the dialog should close
    pub fn handle_key_event(&mut self, key: KeyEvent) -> Option<DialogResult> {
        // Priority 1: Handle custom text input mode
        if self.custom_mode_active {
            return self.handle_custom_input_key(key);
        }

        // Priority 2: Check for 'o' key to enter custom mode (if allowed)
        if matches!(key.code, KeyCode::Char('o') | KeyCode::Char('O')) {
            let allow_custom = match &self.dialog_type {
                DialogType::Select { allow_custom, .. } => *allow_custom,
                DialogType::MultiSelect { allow_custom, .. } => *allow_custom,
                _ => false,
            };

            if allow_custom {
                self.custom_mode_active = true;
                return None; // Don't close dialog, just enter custom mode
            }
        }

        // Priority 3: Handle normal dialog input
        match &mut self.dialog_type {
            DialogType::Select {
                options,
                selected_index,
                ..
            } => Self::handle_select_key(key, options, selected_index),

            DialogType::MultiSelect {
                options,
                selected_indices,
                cursor_index,
                ..
            } => Self::handle_multiselect_key(key, options, selected_indices, cursor_index),

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

    /// Handle key events for single-select dialogs
    fn handle_select_key(
        key: KeyEvent,
        options: &[DialogOption],
        selected_index: &mut usize,
    ) -> Option<DialogResult> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected_index = selected_index.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected_index = (*selected_index + 1).min(options.len().saturating_sub(1));
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
            KeyCode::Enter => Some(DialogResult::Selected(*selected_index)),
            KeyCode::Esc => Some(DialogResult::Cancelled),
            _ => None,
        }
    }

    /// Handle key events for multi-select dialogs
    fn handle_multiselect_key(
        key: KeyEvent,
        options: &[DialogOption],
        selected_indices: &mut HashSet<usize>,
        cursor_index: &mut usize,
    ) -> Option<DialogResult> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *cursor_index = cursor_index.saturating_sub(1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *cursor_index = (*cursor_index + 1).min(options.len().saturating_sub(1));
                None
            }
            KeyCode::Char(' ') => {
                // Toggle selection at cursor
                if selected_indices.contains(cursor_index) {
                    selected_indices.remove(cursor_index);
                } else {
                    selected_indices.insert(*cursor_index);
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
    fn test_select_down_at_bottom_stays() {
        let mut dialog = Dialog::select("T", vec![DialogOption::new("A"), DialogOption::new("B")]);
        // Move past the end — should cap at last index
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        dialog.handle_key_event(KeyEvent::from(KeyCode::Down));
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Enter));
        assert_eq!(result, Some(DialogResult::Selected(1)));
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
}
