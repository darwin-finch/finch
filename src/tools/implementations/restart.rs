// Restart tool - allows Claude to restart Shammah with a new binary

use crate::output_status;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

pub struct RestartTool {
    session_state_file: PathBuf,
}

impl RestartTool {
    pub fn new(session_state_file: PathBuf) -> Self {
        Self { session_state_file }
    }
}

impl Default for RestartTool {
    fn default() -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let session_state_file = home.join(".finch/restart_state.json");
        Self::new(session_state_file)
    }
}

#[async_trait]
impl Tool for RestartTool {
    fn name(&self) -> &str {
        "restart_session"
    }

    fn description(&self) -> &str {
        "Restart Shammah with a newly built binary, preserving the current conversation.
        Use this after modifying code and running 'cargo build --release'.
        The current process will terminate and restart with the new binary.

        IMPORTANT: Only use after successfully building the new binary."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            (
                "reason",
                "Why you're restarting (e.g., 'optimized router', 'added new tool')",
            ),
            (
                "binary_path",
                "Path to new binary (default: ./target/release/finch)",
            ),
        ])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let reason = input["reason"]
            .as_str()
            .context("Missing reason parameter")?;

        let binary_path = input["binary_path"]
            .as_str()
            .unwrap_or("./target/release/finch");

        // Verify new binary exists
        if !std::path::Path::new(binary_path).exists() {
            anyhow::bail!(
                "Binary not found at '{}'. Did you forget to run 'cargo build --release'?",
                binary_path
            );
        }

        // Check if binary is executable
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(binary_path)?;
            let permissions = metadata.permissions();
            if permissions.mode() & 0o111 == 0 {
                anyhow::bail!("Binary at '{}' is not executable", binary_path);
            }
        }

        output_status!("\n🔄 Restarting Shammah...");
        output_status!("   Reason: {}", reason);
        output_status!("   Binary: {}", binary_path);
        output_status!("   Conversation will be preserved");

        // Save conversation state
        let session_state_file = self.session_state_file.clone();
        if let Some(conversation) = context.conversation {
            std::fs::create_dir_all(session_state_file.parent().unwrap())?;
            conversation.save(&session_state_file)?;
            output_status!(
                "✓ Saved conversation state to {}",
                session_state_file.display()
            );
        }

        // Save model weights before restart
        if let Some(ref save_models_fn) = context.save_models {
            save_models_fn()?;
            output_status!("✓ Saved model weights");
        }

        // Prepare restart command with session restoration
        let mut cmd = Command::new(binary_path);
        cmd.arg("--restore-session").arg(&session_state_file);

        // On Unix, use exec to replace current process
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            output_status!("\n→ Executing new binary...\n");

            // This will replace the current process - never returns
            let err = cmd.exec();

            // If we get here, exec failed
            anyhow::bail!("Failed to exec new binary: {}", err);
        }

        // On Windows, spawn and exit
        #[cfg(not(unix))]
        {
            output_status!("\n→ Starting new binary...\n");

            cmd.spawn().context("Failed to spawn new binary")?;

            std::process::exit(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_restart_requires_reason() {
        let tool = RestartTool::default();
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
            "binary_path": "./target/release/finch"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("reason"));
    }

    #[tokio::test]
    async fn test_restart_validates_binary_exists() {
        let tool = RestartTool::default();
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
            "reason": "test",
            "binary_path": "/nonexistent/binary"
        });

        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
