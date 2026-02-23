// Menu - User interaction with native ratatui dialogs
//
// Provides select, multiselect, text input, and confirmation dialogs
// that integrate seamlessly with the TUI without requiring suspend/resume.

use anyhow::Result;
use std::io::IsTerminal;

use super::global_output::get_global_tui_renderer;
use super::tui::{Dialog, DialogOption, DialogResult};

/// Menu option with label, optional description, and associated value
#[derive(Debug, Clone)]
pub struct MenuOption<T> {
    pub label: String,
    pub description: Option<String>,
    pub value: T,
}

impl<T> MenuOption<T> {
    /// Create a new menu option with just a label
    pub fn new(label: impl Into<String>, value: T) -> Self {
        Self {
            label: label.into(),
            description: None,
            value,
        }
    }

    /// Create a new menu option with label and description
    pub fn with_description(
        label: impl Into<String>,
        description: impl Into<String>,
        value: T,
    ) -> Self {
        Self {
            label: label.into(),
            description: Some(description.into()),
            value,
        }
    }
}

/// Menu builder for consistent styling and behavior
pub struct Menu;

impl Menu {
    /// Single-choice menu with arrow/vim keys and number selection
    ///
    /// # Features
    /// - Arrow keys (↑/↓) for navigation
    /// - Vim keys (j/k) for navigation
    /// - Number keys (1-9) for direct selection
    /// - Visual highlighting of current selection
    /// - Non-TTY fallback (returns first option)
    ///
    /// # Arguments
    /// - `prompt`: The question/prompt to display
    /// - `options`: List of menu options with values
    /// - `help_message`: Optional help text shown at bottom
    ///
    /// # Returns
    /// The value associated with the selected option
    pub fn select<T: Clone>(
        prompt: &str,
        options: Vec<MenuOption<T>>,
        help_message: Option<&str>,
    ) -> Result<T> {
        if options.is_empty() {
            anyhow::bail!("Cannot create menu with empty options");
        }

        // Non-TTY fallback: use first option as default
        if !std::io::stdout().is_terminal() {
            return Ok(options[0].value.clone());
        }

        // Try to use TUI dialog if available
        let tui_renderer = get_global_tui_renderer();
        let mut tui_lock = tui_renderer.lock().unwrap();

        if let Some(tui) = tui_lock.as_mut() {
            // Convert MenuOptions to DialogOptions
            let dialog_options: Vec<DialogOption> = options
                .iter()
                .map(|opt| {
                    if let Some(desc) = &opt.description {
                        DialogOption::with_description(&opt.label, desc)
                    } else {
                        DialogOption::new(&opt.label)
                    }
                })
                .collect();

            // Create dialog
            let mut dialog = Dialog::select(prompt, dialog_options);
            if let Some(help) = help_message {
                dialog = dialog.with_help(help);
            }

            // Show dialog and get result
            let result = tui.show_dialog(dialog)?;

            match result {
                DialogResult::Selected(index) => Ok(options[index].value.clone()),
                DialogResult::Cancelled => {
                    anyhow::bail!("Menu selection cancelled")
                }
                _ => unreachable!("Select dialog should only return Selected or Cancelled"),
            }
        } else {
            // No TUI available, use first option as default
            Ok(options[0].value.clone())
        }
    }

    /// Multi-choice menu with checkboxes
    ///
    /// # Features
    /// - Arrow keys (↑/↓) for navigation
    /// - Space to toggle selection
    /// - Enter to confirm
    /// - Vim keys (j/k) supported
    /// - Non-TTY fallback (returns empty list)
    ///
    /// # Arguments
    /// - `prompt`: The question/prompt to display
    /// - `options`: List of menu options with values
    /// - `help_message`: Optional help text shown at bottom
    ///
    /// # Returns
    /// Vector of values associated with selected options
    pub fn multiselect<T: Clone>(
        prompt: &str,
        options: Vec<MenuOption<T>>,
        help_message: Option<&str>,
    ) -> Result<Vec<T>> {
        if options.is_empty() {
            anyhow::bail!("Cannot create menu with empty options");
        }

        // Non-TTY fallback: return empty selection
        if !std::io::stdout().is_terminal() {
            return Ok(vec![]);
        }

        // Try to use TUI dialog if available
        let tui_renderer = get_global_tui_renderer();
        let mut tui_lock = tui_renderer.lock().unwrap();

        if let Some(tui) = tui_lock.as_mut() {
            // Convert MenuOptions to DialogOptions
            let dialog_options: Vec<DialogOption> = options
                .iter()
                .map(|opt| {
                    if let Some(desc) = &opt.description {
                        DialogOption::with_description(&opt.label, desc)
                    } else {
                        DialogOption::new(&opt.label)
                    }
                })
                .collect();

            // Create dialog
            let mut dialog = Dialog::multiselect(prompt, dialog_options);
            if let Some(help) = help_message {
                dialog = dialog.with_help(help);
            }

            // Show dialog and get result
            let result = tui.show_dialog(dialog)?;

            match result {
                DialogResult::MultiSelected(indices) => {
                    let selected_values: Vec<T> = indices
                        .iter()
                        .map(|&idx| options[idx].value.clone())
                        .collect();
                    Ok(selected_values)
                }
                DialogResult::Cancelled => {
                    anyhow::bail!("Menu selection cancelled")
                }
                _ => {
                    unreachable!("MultiSelect dialog should only return MultiSelected or Cancelled")
                }
            }
        } else {
            // No TUI available, return empty
            Ok(vec![])
        }
    }

    /// Text input option (for "Other" choice or custom input)
    ///
    /// # Arguments
    /// - `prompt`: The question/prompt to display
    /// - `default`: Optional default value
    /// - `help_message`: Optional help text shown at bottom
    ///
    /// # Returns
    /// The user's input string
    pub fn text_input(
        prompt: &str,
        default: Option<String>,
        help_message: Option<&str>,
    ) -> Result<String> {
        // Non-TTY fallback: use default or empty string
        if !std::io::stdout().is_terminal() {
            return Ok(default.unwrap_or_default());
        }

        // Try to use TUI dialog if available
        let tui_renderer = get_global_tui_renderer();
        let mut tui_lock = tui_renderer.lock().unwrap();

        if let Some(tui) = tui_lock.as_mut() {
            // Create dialog
            let mut dialog = Dialog::text_input(prompt, default.clone());
            if let Some(help) = help_message {
                dialog = dialog.with_help(help);
            }

            // Show dialog and get result
            let result = tui.show_dialog(dialog)?;

            match result {
                DialogResult::TextEntered(text) => Ok(text),
                DialogResult::Cancelled => {
                    anyhow::bail!("Text input cancelled")
                }
                _ => unreachable!("TextInput dialog should only return TextEntered or Cancelled"),
            }
        } else {
            // No TUI available, use default or empty
            Ok(default.unwrap_or_default())
        }
    }

    /// Confirmation prompt (yes/no)
    ///
    /// # Arguments
    /// - `prompt`: The question to ask
    /// - `default`: Default answer if user just presses Enter
    ///
    /// # Returns
    /// true if user confirmed, false otherwise
    pub fn confirm(prompt: &str, default: bool) -> Result<bool> {
        // Non-TTY fallback: use default
        if !std::io::stdout().is_terminal() {
            return Ok(default);
        }

        // Try to use TUI dialog if available
        let tui_renderer = get_global_tui_renderer();
        let mut tui_lock = tui_renderer.lock().unwrap();

        if let Some(tui) = tui_lock.as_mut() {
            // Create dialog
            let dialog = Dialog::confirm(prompt, default);

            // Show dialog and get result
            let result = tui.show_dialog(dialog)?;

            match result {
                DialogResult::Confirmed(answer) => Ok(answer),
                DialogResult::Cancelled => {
                    anyhow::bail!("Confirmation cancelled")
                }
                _ => unreachable!("Confirm dialog should only return Confirmed or Cancelled"),
            }
        } else {
            // No TUI available, use default
            Ok(default)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_menu_option_creation() {
        let opt = MenuOption::new("Option 1", 1);
        assert_eq!(opt.label, "Option 1");
        assert_eq!(opt.value, 1);
        assert!(opt.description.is_none());

        let opt_with_desc = MenuOption::with_description("Option 2", "A description", 2);
        assert_eq!(opt_with_desc.label, "Option 2");
        assert_eq!(opt_with_desc.value, 2);
        assert_eq!(opt_with_desc.description, Some("A description".to_string()));
    }

    #[test]
    fn test_empty_options_fails() {
        let options: Vec<MenuOption<i32>> = vec![];
        let result = Menu::select("Test", options, None);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Cannot create menu with empty options"));
    }

    #[test]
    fn test_multiselect_empty_options_fails() {
        let options: Vec<MenuOption<i32>> = vec![];
        let result = Menu::multiselect("Test", options, None);
        assert!(result.is_err());
    }

    // Note: Interactive tests would require a real TUI environment
    // These tests verify the non-interactive fallback behavior
}
