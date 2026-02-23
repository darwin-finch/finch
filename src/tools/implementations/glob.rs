// Glob tool - finds files matching glob patterns

use crate::output_error;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use glob::glob;
use serde_json::Value;

pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern (e.g., \"**/*.rs\", \"src/**/*.ts\")."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![("pattern", "The glob pattern to match files against")])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .context("Missing pattern parameter")?;

        let mut paths = Vec::new();

        for entry in glob(pattern).with_context(|| format!("Invalid glob pattern: {}", pattern))? {
            match entry {
                Ok(path) => paths.push(path.display().to_string()),
                Err(e) => output_error!("Error reading path: {}", e),
            }
        }

        // Sort for consistent output
        paths.sort();

        // Limit to 100 files
        if paths.len() > 100 {
            paths.truncate(100);
            paths.push("... (truncated to 100 files)".to_string());
        }

        if paths.is_empty() {
            Ok("No files found matching pattern.".to_string())
        } else {
            Ok(paths.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_glob_cargo_toml() {
        let tool = GlobTool;
        let input = serde_json::json!({
            "pattern": "Cargo.toml"
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
        assert!(content.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_glob_rust_files() {
        let tool = GlobTool;
        let input = serde_json::json!({
            "pattern": "src/*.rs"
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
        // Should find at least main.rs or lib.rs
        assert!(!content.is_empty());
    }
}
