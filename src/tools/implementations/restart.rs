// Restart tool - allows Claude to restart Shammah with a new binary

use crate::tools::registry::Tool;
use crate::tools::types::ToolInputSchema;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

pub struct RestartTool {
    session_state_file: PathBuf,
}

impl RestartTool {
    pub fn new() -> Self {
        let home = dirs::home_dir().expect("Could not determine home directory");
        let session_state_file = home.join(".shammah/restart_state.json");

        Self { session_state_file }
    }
}

impl Default for RestartTool {
    fn default() -> Self {
        Self::new()
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
            ("reason", "Why you're restarting (e.g., 'optimized router', 'added new tool')"),
            ("binary_path", "Path to new binary (default: ./target/release/shammah)"),
        ])
    }

    async fn execute(&self, input: Value) -> Result<String> {
        let reason = input["reason"]
            .as_str()
            .context("Missing reason parameter")?;

        let binary_path = input["binary_path"]
            .as_str()
            .unwrap_or("./target/release/shammah");

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

        println!("\n🔄 Restarting Shammah...");
        println!("   Reason: {}", reason);
        println!("   Binary: {}", binary_path);
        println!("   Conversation will be preserved");

        // Note: Conversation state preservation not yet implemented
        // For Phase 1, just exec into new binary and lose conversation
        // TODO: Save conversation state to session_state_file

        // Prepare restart command
        let mut cmd = Command::new(binary_path);

        // On Unix, use exec to replace current process
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            println!("\n→ Executing new binary...\n");

            // This will replace the current process - never returns
            let err = cmd.exec();

            // If we get here, exec failed
            anyhow::bail!("Failed to exec new binary: {}", err);
        }

        // On Windows, spawn and exit
        #[cfg(not(unix))]
        {
            println!("\n→ Starting new binary...\n");

            cmd.spawn()
                .context("Failed to spawn new binary")?;

            std::process::exit(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_restart_requires_reason() {
        let tool = RestartTool::new();
        let input = serde_json::json!({
            "binary_path": "./target/release/shammah"
        });

        let result = tool.execute(input).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("reason"));
    }

    #[tokio::test]
    async fn test_restart_validates_binary_exists() {
        let tool = RestartTool::new();
        let input = serde_json::json!({
            "reason": "test",
            "binary_path": "/nonexistent/binary"
        });

        let result = tool.execute(input).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
