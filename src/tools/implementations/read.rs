// Read tool - reads file contents from filesystem

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;

pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file from the filesystem."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![("file_path", "Absolute path to the file to read")])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;

        let contents = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        // Limit output to first 10,000 chars to avoid token explosion
        if contents.len() > 10_000 {
            Ok(format!(
                "{}\n\n[File truncated - showing first 10,000 characters of {}]",
                &contents[..10_000],
                contents.len()
            ))
        } else {
            Ok(contents)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_existing_file() {
        let tool = ReadTool;
        let input = serde_json::json!({
            "file_path": "Cargo.toml"
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
        let content = result.unwrap();
        assert!(content.contains("[package]"));
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let tool = ReadTool;
        let input = serde_json::json!({
            "file_path": "/nonexistent/file.txt"
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
        assert!(result.is_err());
    }
}
