// Readline input handler with history and editing support

use anyhow::{Context, Result};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::path::PathBuf;

pub struct InputHandler {
    editor: DefaultEditor,
    history_path: PathBuf,
}

impl InputHandler {
    /// Create new input handler with history support
    pub fn new() -> Result<Self> {
        let mut editor = DefaultEditor::new().context("Failed to initialize readline editor")?;

        // History path: ~/.finch/history.txt
        let history_path = dirs::home_dir()
            .context("Failed to determine home directory")?
            .join(".finch")
            .join("history.txt");

        // Load existing history if available
        if history_path.exists() {
            let _ = editor.load_history(&history_path);
        }

        Ok(Self {
            editor,
            history_path,
        })
    }

    /// Read a line of input with editing support
    ///
    /// Returns:
    /// - `Ok(Some(line))` - user entered text
    /// - `Ok(None)` - user pressed Ctrl+C or Ctrl+D
    /// - `Err(e)` - I/O or other error
    pub fn read_line(&mut self, prompt: &str) -> Result<Option<String>> {
        match self.editor.readline(prompt) {
            Ok(line) => {
                let line = line.trim().to_string();
                if !line.is_empty() {
                    // Add to history (in-memory)
                    self.editor
                        .add_history_entry(&line)
                        .context("Failed to add history entry")?;
                }
                Ok(Some(line))
            }
            Err(ReadlineError::Interrupted) => {
                // Ctrl+C - graceful exit
                Ok(None)
            }
            Err(ReadlineError::Eof) => {
                // Ctrl+D - graceful exit
                Ok(None)
            }
            Err(err) => {
                // Other errors (I/O, etc.)
                Err(err).context("Failed to read input")
            }
        }
    }

    /// Save history to disk
    pub fn save_history(&mut self) -> Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.history_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        // Save history
        self.editor
            .save_history(&self.history_path)
            .with_context(|| {
                format!("Failed to save history to {}", self.history_path.display())
            })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_handler_creation() {
        // Should create successfully
        let result = InputHandler::new();
        assert!(result.is_ok());
    }
}
