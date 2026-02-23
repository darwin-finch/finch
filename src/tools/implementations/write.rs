// Write tool - create or overwrite files
//
// Returns a summary like Claude Code:
//   Created src/foo.rs (42 lines)
//   Updated src/bar.rs (Added 10 lines, removed 3 lines)

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
        "Write the complete content of a file (creates new or fully overwrites existing). \
         Use for new files or when rewriting most of the content. \
         For small targeted changes to an existing file, use the edit tool instead — \
         it is safer and shows a precise diff."
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
            // Existing file: read original, write new, show stats
            let original = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read existing file: {}", file_path))?;

            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;

            let old_lines = original.lines().count();
            let new_lines = content.lines().count();
            let delta: i64 = new_lines as i64 - old_lines as i64;
            let delta_str = match delta.cmp(&0) {
                std::cmp::Ordering::Greater => format!("+{} lines", delta),
                std::cmp::Ordering::Less => format!("{} lines", delta),
                std::cmp::Ordering::Equal => "unchanged line count".to_string(),
            };
            Ok(format!(
                "Updated {} ({} → {} lines, {})\n",
                file_path, old_lines, new_lines, delta_str
            ))
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
