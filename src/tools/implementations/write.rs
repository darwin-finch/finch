// Write tool - create or overwrite files
//
// Returns a summary like Claude Code:
//   Created src/foo.rs (42 lines)
//   Updated src/bar.rs (Added 10 lines, removed 3 lines)

use crate::tools::implementations::edit::generate_edit_diff;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;
use std::path::Path;

pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Writes content to a file, creating it if it doesn't exist or overwriting it if it does. \
         Always provide the complete file content."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "The complete file content to write"
                }
            }),
            required: vec!["file_path".to_string(), "content".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;
        let content = input["content"]
            .as_str()
            .context("Missing content parameter")?;

        let path = Path::new(file_path);
        let is_new = !path.exists();

        // Create parent directories if needed
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create directories for: {}", file_path))?;
            }
        }

        if is_new {
            // New file: just write and return summary
            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;

            let line_count = content.lines().count();
            Ok(format!(
                "Created {} ({} line{})\n",
                file_path,
                line_count,
                if line_count == 1 { "" } else { "s" }
            ))
        } else {
            // Existing file: read original, write new, show diff
            let original = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read existing file: {}", file_path))?;

            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;

            // Show diff between old and new content
            Ok(generate_edit_diff(&original, &original, content, 1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_write_new_file() {
        let tool = WriteTool;
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap().to_string();
        // Delete so it looks like a new file
        drop(tmp);

        let input = serde_json::json!({
            "file_path": path,
            "content": "line 1\nline 2\nline 3\n"
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
        let result = tool.execute(input, &context).await.unwrap();
        assert!(result.contains("Created"), "got: {}", result);
        assert!(result.contains("3 lines"), "got: {}", result);
    }
}
