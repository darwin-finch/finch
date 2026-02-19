// Grep tool - searches for patterns in files

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use std::fs;
use walkdir::WalkDir;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for patterns in files using regex. Returns matching lines."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            ("pattern", "The regex pattern to search for"),
            (
                "path",
                "Directory or file to search (default: current directory)",
            ),
        ])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .context("Missing pattern parameter")?;

        let path = input["path"].as_str().unwrap_or(".");

        let regex =
            Regex::new(pattern).with_context(|| format!("Invalid regex pattern: {}", pattern))?;

        let mut results = Vec::new();
        let mut file_count = 0;

        for entry in WalkDir::new(path)
            .max_depth(10)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                file_count += 1;
                if let Ok(contents) = fs::read_to_string(entry.path()) {
                    for (line_num, line) in contents.lines().enumerate() {
                        if regex.is_match(line) {
                            results.push(format!(
                                "{}:{}: {}",
                                entry.path().display(),
                                line_num + 1,
                                line
                            ));

                            // Limit results
                            if results.len() >= 50 {
                                results.push("... (truncated to 50 matches)".to_string());
                                return Ok(results.join("\n"));
                            }
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(format!("No matches found in {} files.", file_count))
        } else {
            Ok(results.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_grep_in_cargo_toml() {
        let tool = GrepTool;
        let input = serde_json::json!({
            "pattern": "name.*=",
            "path": "Cargo.toml"
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
        // Should find at least the package name
        assert!(content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_grep_invalid_regex() {
        let tool = GrepTool;
        let input = serde_json::json!({
            "pattern": "[invalid(",
            "path": "."
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
