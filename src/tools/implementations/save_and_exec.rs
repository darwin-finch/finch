// SaveAndExec tool - saves session state and executes arbitrary commands

use crate::output_status;
use crate::tools::registry::Tool;
use crate::tools::types::ToolContext;
use crate::tools::types::ToolInputSchema;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

pub struct SaveAndExecTool {
    session_state_file: PathBuf,
}

impl SaveAndExecTool {
    pub fn new(session_state_file: PathBuf) -> Self {
        Self { session_state_file }
    }
}

impl Default for SaveAndExecTool {
    fn default() -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let session_state_file = home.join(".finch/restart_state.json");
        Self::new(session_state_file)
    }
}

#[async_trait]
impl Tool for SaveAndExecTool {
    fn name(&self) -> &str {
        "save_and_exec"
    }

    fn description(&self) -> &str {
        "Save conversation and model state, then execute a shell command.
        The current process will be replaced by the executed command.
        Session state is saved to ~/.finch/restart_state.json

        Common use cases:
        - Restart Shammah: ./target/release/finch --restore-session ~/.finch/restart_state.json
        - Build and restart: cargo build --release && ./target/release/finch --restore-session ~/.finch/restart_state.json
        - Run with prompt: ./target/release/finch --restore-session ~/.finch/restart_state.json --initial-prompt 'hello'
        - Run any command: python my_script.py"
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            (
                "reason",
                "Why you're executing this command (e.g., 'testing new feature', 'rebuilding after changes')",
            ),
            (
                "command",
                "Shell command to execute (e.g., './target/release/finch', 'cargo build && ./target/release/finch')",
            ),
        ])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let reason = input["reason"]
            .as_str()
            .context("Missing reason parameter")?;

        let command = input["command"]
            .as_str()
            .context("Missing command parameter")?;

        output_status!("\n💾 Saving session state...");
        output_status!("   Reason: {}", reason);
        output_status!("   Command: {}", command);

        // CRITICAL: Save all state before exec
        let session_state_file = self.session_state_file.clone();

        // Save conversation
        if let Some(conversation) = context.conversation {
            std::fs::create_dir_all(session_state_file.parent().unwrap())?;
            conversation.save(&session_state_file)?;
            output_status!(
                "✓ Saved conversation state to {}",
                session_state_file.display()
            );
        }

        // CRITICAL: Save model weights before exec
        // The router contains threshold_router with learned statistics
        // These MUST be saved or all learning is lost on restart
        if let Some(ref save_models_fn) = context.save_models {
            save_models_fn()?;
            output_status!("✓ Saved model weights");
        }

        // Prepare command execution through shell
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);

        // Set environment variable with session file path for convenience
        cmd.env("SHAMMAH_SESSION_FILE", &session_state_file);

        // On Unix, use exec to replace current process
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            output_status!("\n→ Executing command...\n");

            // This will replace the current process - never returns
            let err = cmd.exec();

            // If we get here, exec failed
            anyhow::bail!("Failed to exec command: {}", err);
        }

        // On Windows, spawn and exit
        #[cfg(not(unix))]
        {
            output_status!("\n→ Starting command...\n");

            cmd.spawn().context("Failed to spawn command")?;

            std::process::exit(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_save_and_exec_requires_reason() {
        let tool = SaveAndExecTool::default();
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let input = serde_json::json!({
            "command": "echo test"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("reason"));
    }

    #[tokio::test]
    async fn test_save_and_exec_requires_command() {
        let tool = SaveAndExecTool::default();
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let input = serde_json::json!({
            "reason": "test"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }
}
