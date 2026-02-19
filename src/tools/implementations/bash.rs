// Bash tool - executes shell commands

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute bash commands. Use for terminal operations like git, npm, ls, etc."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            ("command", "The bash command to execute"),
            ("description", "Brief description of what this command does"),
        ])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let command = input["command"]
            .as_str()
            .context("Missing command parameter")?;

        let output = Command::new("bash")
            .arg("-c")
            .arg(command)
            .output()
            .with_context(|| format!("Failed to execute command: {}", command))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let exit_code = output.status.code().unwrap_or(-1);

        let mut result = String::new();

        if !stdout.is_empty() {
            result.push_str(&stdout);
        }

        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push_str("\n");
            }
            result.push_str("STDERR:\n");
            result.push_str(&stderr);
        }

        if exit_code != 0 {
            if !result.is_empty() {
                result.push_str("\n");
            }
            result.push_str(&format!("Exit code: {}", exit_code));
        }

        // Limit to 5,000 chars
        if result.len() > 5_000 {
            Ok(format!(
                "{}\n\n[Output truncated - showing first 5,000 characters]",
                &result[..5_000]
            ))
        } else {
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "echo 'Hello, World!'",
            "description": "Test echo command"
        });

        let context = crate::tools::types::ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

        let result = tool.execute(input, &context).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Hello, World!"));
    }

    #[tokio::test]
    async fn test_bash_ls() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls Cargo.toml",
            "description": "List Cargo.toml"
        });

        let context = crate::tools::types::ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

        let result = tool.execute(input, &context).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_bash_nonzero_exit() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls /nonexistent",
            "description": "Try to list nonexistent directory"
        });

        let context = crate::tools::types::ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

        let result = tool.execute(input, &context).await;
        assert!(result.is_ok()); // Command executes but has error output
        let output = result.unwrap();
        assert!(output.contains("Exit code:") || output.contains("STDERR"));
    }
}
