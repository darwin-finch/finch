// Read tool - reads file contents from filesystem
//
// Supports optional offset (1-indexed start line) and limit (max lines)
// so the AI can read large files in focused chunks.

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
        "Read the contents of a file. Use offset and limit to read a specific range of lines \
         (e.g., offset=100 limit=50 reads lines 100-149). Without them, reads the whole file \
         up to 50,000 characters."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-indexed, optional)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (optional)"
                }
            }),
            required: vec!["file_path".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;

        let contents = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        let offset = input["offset"].as_u64().map(|n| n as usize);
        let limit = input["limit"].as_u64().map(|n| n as usize);

        // If offset or limit specified, return line-based slice with line numbers
        if offset.is_some() || limit.is_some() {
            let all_lines: Vec<&str> = contents.lines().collect();
            let total_lines = all_lines.len();

            let start = offset.map(|o| o.saturating_sub(1)).unwrap_or(0); // convert to 0-indexed
            let end = match limit {
                Some(l) => (start + l).min(total_lines),
                None => total_lines,
            };

            if start >= total_lines {
                return Ok(format!(
                    "File has {} lines. Offset {} is past the end.",
                    total_lines, start + 1
                ));
            }

            let slice = &all_lines[start..end];
            let line_num_width = end.to_string().len();
            let numbered: String = slice
                .iter()
                .enumerate()
                .map(|(i, line)| format!("{:>width$}\t{}", start + i + 1, line, width = line_num_width))
                .collect::<Vec<_>>()
                .join("\n");

            let header = format!(
                "Lines {}-{} of {} ({})\n",
                start + 1,
                end,
                total_lines,
                file_path
            );
            return Ok(format!("{}{}", header, numbered));
        }

        // No offset/limit — return full file up to char limit
        if contents.len() > 50_000 {
            Ok(format!(
                "{}\n\n[File truncated - showing first 50,000 of {} total characters]",
                &contents[..50_000],
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

    fn ctx() -> ToolContext<'static> {
        ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        }
    }

    #[tokio::test]
    async fn test_read_existing_file() {
        let result = ReadTool.execute(serde_json::json!({"file_path": "Cargo.toml"}), &ctx()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("[package]"));
    }

    #[tokio::test]
    async fn test_read_nonexistent_file() {
        let result = ReadTool.execute(serde_json::json!({"file_path": "/nonexistent/file.txt"}), &ctx()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_read_with_offset_and_limit() {
        let result = ReadTool.execute(
            serde_json::json!({"file_path": "Cargo.toml", "offset": 1, "limit": 3}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
        let out = result.unwrap();
        assert!(out.contains("Lines 1-3"), "got: {}", out);
        // Should have line numbers
        assert!(out.contains('\t'));
    }

    #[tokio::test]
    async fn test_read_with_offset_only() {
        let result = ReadTool.execute(
            serde_json::json!({"file_path": "Cargo.toml", "offset": 1}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
        let out = result.unwrap();
        assert!(out.contains("Lines 1-"), "got: {}", out);
    }
}
