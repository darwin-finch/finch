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
        allow_custom: bool,  // Enable "Other" option with text input
    },
    /// Multi-select menu with checkboxes and space to toggle
    MultiSelect {
        options: Vec<DialogOption>,
        selected_indices: HashSet<usize>,
        cursor_index: usize,
        allow_custom: bool,  // Enable "Other" option with text input
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
}

impl DialogOption {
    /// Create a new dialog option with just a label
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: None,
        }
    }

    /// Create a dialog option with label and description
    pub fn with_description(label: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            description: Some(description.into()),
        }
    }
}

/// A dialog to display to the user
#[derive(Debug, Clone)]
pub struct Dialog {
    pub title: String,
    pub dialog_type: DialogType,
    pub help_message: Option<String>,
    pub custom_input: Option<String>,  // Stores custom text if "Other" is being entered
    pub custom_mode_active: bool,       // Whether user is currently typing custom text
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
                input,
                cursor_pos,
                ..
            } => Self::handle_text_input_key(key, input, cursor_pos),

            DialogType::Confirm { selected, .. } => Self::handle_confirm_key(key, selected),
        }
    }

    /// Handle key events when in custom text input mode
    fn handle_custom_input_key(&mut self, key: KeyEvent) -> Option<DialogResult> {
        match key.code {
            KeyCode::Char(c) => {
                // Insert character
                if let Some(ref mut input) = self.custom_input {
                    input.push(c);
                }
                None
            }
            KeyCode::Backspace => {
                // Delete last character
                if let Some(ref mut input) = self.custom_input {
                    input.pop();
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
            vec![
                DialogOption::new("Option 1"),
                DialogOption::new("Option 2"),
            ],
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
        let mut dialog = Dialog::select(
            "Test",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
            ],
        );

        // Press '2' for second option
        let result = dialog.handle_key_event(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(result, Some(DialogResult::Selected(1)));
    }

    #[test]
    fn test_multiselect_toggle() {
        let mut dialog = Dialog::multiselect(
            "Test",
            vec![
                DialogOption::new("A"),
                DialogOption::new("B"),
            ],
        );

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
}
