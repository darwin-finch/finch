// BrainActionTool — lets the background brain propose a shell command for the user to approve.
//
// The brain calls `run_command` when it discovers something actionable (e.g. a failing test,
// a missing dependency).  The event loop shows an approval dialog; if the user says yes the
// command is executed and its output is returned to the brain.

use crate::cli::repl_event::events::ReplEvent;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// Tool that lets the brain propose running a shell command with user approval.
///
/// The command is only executed if the user explicitly approves in the TUI
/// confirmation dialog.  The output (or a denial message) is returned to the
/// brain so it can incorporate the result into its context summary.
pub struct BrainActionTool {
    event_tx: mpsc::UnboundedSender<ReplEvent>,
}

impl BrainActionTool {
    pub fn new(event_tx: mpsc::UnboundedSender<ReplEvent>) -> Self {
        Self { event_tx }
    }
}

#[async_trait]
impl Tool for BrainActionTool {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Propose running a shell command. The user will be asked to approve before execution. \
         Use this when you discover something actionable (e.g. a failing test, a missing \
         dependency, a stale lock file). Only propose safe, targeted commands. \
         The command output will be returned so you can include it in your context summary."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "command": {
                    "type": "string",
                    "description": "The shell command to run (e.g. \"cargo test\", \"npm install\")."
                },
                "reason": {
                    "type": "string",
                    "description": "One sentence explaining why this command is useful right now."
                }
            }),
            required: vec!["command".to_string(), "reason".to_string()],
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> Result<String> {
        let command = input["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("run_command: missing 'command'"))?
            .to_string();
        let reason = input["reason"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let (response_tx, response_rx) = oneshot::channel();

        if self
            .event_tx
            .send(ReplEvent::BrainProposedAction {
                command,
                reason,
                response_tx,
            })
            .is_err()
        {
            return Ok("[action unavailable — event loop closed]".to_string());
        }

        // Wait up to 60 s for the user to respond (longer than a question since
        // they may not be watching the terminal).
        match tokio::time::timeout(Duration::from_secs(60), response_rx).await {
            Ok(Ok(Some(output))) => Ok(output),
            Ok(Ok(None)) => Ok("[action denied by user]".to_string()),
            _ => Ok("[action timed out or unavailable]".to_string()),
        }
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            input_schema: self.input_schema(),
        }
    }
}

/// Execute a shell command and return its output (stdout + stderr).
///
/// Runs via `sh -c` with a 30-second timeout.  Non-zero exit codes are
/// included in the returned string so the brain can reason about failures.
pub async fn execute_brain_command(command: &str) -> String {
    match tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output(),
    )
    .await
    {
        Ok(Ok(out)) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            if out.status.success() {
                let s = stdout.trim_end();
                if s.is_empty() {
                    "(command succeeded, no output)".to_string()
                } else {
                    s.to_string()
                }
            } else {
                let code = out
                    .status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".to_string());
                format!(
                    "Exit {}: {}{}",
                    code,
                    stdout.trim_end(),
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", stderr.trim_end())
                    }
                )
            }
        }
        Ok(Err(e)) => format!("Failed to spawn command: {}", e),
        Err(_) => "Command timed out after 30s".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_brain_action_tool_name() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = BrainActionTool::new(tx);
        assert_eq!(tool.name(), "run_command");
    }

    #[test]
    fn test_brain_action_tool_requires_command_and_reason() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let tool = BrainActionTool::new(tx);
        let schema = tool.input_schema();
        assert!(schema.required.contains(&"command".to_string()));
        assert!(schema.required.contains(&"reason".to_string()));
    }

    #[tokio::test]
    async fn test_brain_action_tool_closed_channel_returns_unavailable() {
        let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
        drop(_rx);
        let tool = BrainActionTool::new(tx);
        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let result = tool
            .execute(
                json!({"command": "ls", "reason": "check directory"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(result.contains("unavailable"), "got: {}", result);
    }

    #[tokio::test]
    async fn test_execute_brain_command_simple() {
        let output = execute_brain_command("echo hello").await;
        assert_eq!(output.trim(), "hello");
    }

    #[tokio::test]
    async fn test_execute_brain_command_nonzero_exit() {
        let output = execute_brain_command("exit 1").await;
        assert!(output.starts_with("Exit 1"), "got: {}", output);
    }
}
