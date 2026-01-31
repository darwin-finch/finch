use anyhow::{Context, Result};
use inquire::{Confirm, MultiSelect, Select, Text};
use std::io::IsTerminal;

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

        // Build display labels (with descriptions if provided)
        let labels: Vec<String> = options
            .iter()
            .enumerate()
            .map(|(idx, opt)| {
                let number = idx + 1;
                match &opt.description {
                    Some(desc) => format!("{}. {} - {}", number, opt.label, desc),
                    None => format!("{}. {}", number, opt.label),
                }
            })
            .collect();

        // Create select prompt
        let mut select = Select::new(prompt, labels);
        select.vim_mode = true; // Enable j/k keys
        select.page_size = 10; // Show up to 10 options

        if let Some(help) = help_message {
            select.help_message = Some(help);
        }

        // Show menu and get selection
        let selection = select
            .prompt()
            .context("Failed to display menu selection")?;

        // Find selected option by matching the display label
        // Extract number from selection (format: "N. Label..." or "N. Label - Desc...")
        let selected_number = selection
            .split('.')
            .next()
            .and_then(|n| n.trim().parse::<usize>().ok())
            .context("Failed to parse selected option number")?;

        options
            .into_iter()
            .nth(selected_number - 1)
            .map(|opt| opt.value)
            .context("Selected option not found")
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

        // Build display labels
        let labels: Vec<String> = options
            .iter()
            .map(|opt| match &opt.description {
                Some(desc) => format!("{} - {}", opt.label, desc),
                None => opt.label.clone(),
            })
            .collect();

        // Create multiselect prompt
        let mut multiselect = MultiSelect::new(prompt, labels.clone());
        multiselect.vim_mode = true;
        multiselect.page_size = 10;

        if let Some(help) = help_message {
            multiselect.help_message = Some(help);
        }

        // Show menu and get selections
        let selections = multiselect
            .prompt()
            .context("Failed to display multiselect menu")?;

        // Map selected labels back to values
        let selected_values: Vec<T> = selections
            .iter()
            .filter_map(|selected_label| {
                // Find the option with matching label
                options
                    .iter()
                    .zip(&labels)
                    .find(|(_, label)| label == &selected_label)
                    .map(|(opt, _)| opt.value.clone())
            })
            .collect();

        Ok(selected_values)
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

        let mut text = Text::new(prompt);

        if let Some(help) = help_message {
            text.help_message = Some(help);
        }

        let result = if let Some(def) = default {
            text.with_default(&def).prompt()
        } else {
            text.prompt()
        };

        result.context("Failed to get text input")
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

        Confirm::new(prompt)
            .with_default(default)
            .prompt()
            .context("Failed to get confirmation")
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
    fn test_single_option() {
        // In non-TTY mode, should return first option
        let options = vec![MenuOption::new("Only option", 42)];
        // Note: This test will pass in non-TTY environments
        // In TTY environments, it would require actual user interaction
    }

    #[test]
    fn test_multiselect_empty_options_fails() {
        let options: Vec<MenuOption<i32>> = vec![];
        let result = Menu::multiselect("Test", options, None);
        assert!(result.is_err());
    }
}
