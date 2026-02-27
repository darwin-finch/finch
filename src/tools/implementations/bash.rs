// Bash tool - executes shell commands with live output streaming

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

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

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let command = input["command"]
            .as_str()
            .context("Missing command parameter")?;

        let mut child = Command::new("bash")
            .arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn command: {}", command))?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // Clone the live output callback for the stdout reader
        let live_cb = context.live_output.clone();

        // Drain stderr in a background task so it doesn't block stdout reading
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                buf.push_str(&line);
                buf.push('\n');
            }
            buf
        });

        // Drain stdout on this task, calling the live-output callback per line
        let mut stdout_buf = String::new();
        let mut stdout_lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = stdout_lines.next_line().await {
            if let Some(ref cb) = live_cb {
                cb(line.clone());
            }
            stdout_buf.push_str(&line);
            stdout_buf.push('\n');
        }

        let stderr_buf = stderr_task.await.unwrap_or_default();
        let exit_status = child.wait().await?;
        let exit_code = exit_status.code().unwrap_or(-1);

        let mut result = stdout_buf;

        if !stderr_buf.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("STDERR:\n");
            result.push_str(&stderr_buf);
        }

        if exit_code != 0 {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!("Exit code: {}", exit_code));
        }

        // Limit to 20,000 chars
        if result.len() > 20_000 {
            Ok(format!(
                "{}\n\n[Output truncated - showing first 20,000 characters]",
                &result[..20_000]
            ))
        } else {
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context() -> ToolContext<'static> {
        ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        }
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "echo 'Hello, World!'",
            "description": "Test echo command"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Hello, World!"));
    }

    #[tokio::test]
    async fn test_bash_ls() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls Cargo.toml",
            "description": "List Cargo.toml"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_bash_nonzero_exit() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls /nonexistent",
            "description": "Try to list nonexistent directory"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Exit code:") || output.contains("STDERR"));
    }

    #[tokio::test]
    async fn test_bash_live_output_callback_receives_lines() {
        use std::sync::{Arc, Mutex};
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "printf 'line1\\nline2\\nline3\\n'",
            "description": "Test live output streaming"
        });

        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let cb: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line: String| {
            received_clone.lock().unwrap().push(line);
        });

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: Some(cb),
        };

        let result = tool.execute(input, &context).await.unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));

        let lines = received.lock().unwrap();
        assert!(
            lines.contains(&"line1".to_string()),
            "callback must receive line1: {:?}",
            *lines
        );
        assert!(
            lines.contains(&"line2".to_string()),
            "callback must receive line2: {:?}",
            *lines
        );
        assert!(
            lines.contains(&"line3".to_string()),
            "callback must receive line3: {:?}",
            *lines
        );
    }

    #[tokio::test]
    async fn test_bash_live_output_callback_receives_lines_in_order() {
        use std::sync::{Arc, Mutex};
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "for i in 1 2 3 4 5; do echo \"item$i\"; done",
            "description": "Test ordering"
        });

        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let cb: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line: String| {
            received_clone.lock().unwrap().push(line);
        });

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: Some(cb),
        };

        tool.execute(input, &context).await.unwrap();
        let lines = received.lock().unwrap();
        assert_eq!(lines.len(), 5, "should receive 5 lines: {:?}", *lines);
        assert_eq!(lines[0], "item1");
        assert_eq!(lines[4], "item5");
    }

    #[tokio::test]
    async fn test_bash_no_callback_still_returns_output() {
        // When live_output is None, tool must still return complete output
        let tool = BashTool;
        let input = serde_json::json!({ "command": "echo hello" });
        let result = tool.execute(input, &make_context()).await.unwrap();
        assert!(result.trim().contains("hello"));
    }
}
