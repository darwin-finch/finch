// Write tool - create or overwrite files
//
// Returns a summary like Claude Code:
//   Created src/foo.rs (42 lines)
//   Updated src/bar.rs (Added 10 lines, removed 3 lines)

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;

use super::propose::{propose_in_editor, run_script_async};

/// Run `~/.finch/hooks/post-save <file_path>` if that script exists.
/// Fire-and-forget — the hook runs in the background; errors are ignored.
fn run_post_save_hook(file_path: &str) {
    if let Some(hook) = dirs::home_dir().map(|mut p| {
        p.push(".finch/hooks/post-save");
        p
    }) {
        if hook.exists() {
            let _ = std::process::Command::new(&hook).arg(file_path).spawn();
        }
    }
}

/// Build a bash/python script that writes content to a file.
/// Used by the propose-before-execute flow.
fn build_write_code(file_path: &str, content: &str) -> String {
    let content_b64 = base64::engine::general_purpose::STANDARD.encode(content);
    let path_py = format!("{:?}", file_path);
    let line_count = content.lines().count();
    [
        "python3 << 'PYEOF'\n",
        "import base64, os\n",
        &format!("path = {}\n", path_py),
        &format!(
            "content = base64.b64decode(b\"{}\").decode(\"utf-8\")\n",
            content_b64
        ),
        "os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)\n",
        "is_new = not os.path.exists(path)\n",
        "with open(path, \"w\") as f:\n",
        "    f.write(content)\n",
        &format!("verb = \"Created\" if is_new else \"Updated\"\n"),
        &format!("print(verb + \" {} ({} lines)\")\n", file_path, line_count),
        "PYEOF",
    ]
    .concat()
}

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

        // Interactive: propose the script in $EDITOR before writing.
        if std::io::stdin().is_terminal() {
            let (original, was_missing) = match fs::read_to_string(file_path) {
                Ok(value) => (value, false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (String::new(), true),
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Failed to read existing file: {}", file_path))
                }
            };
            let line_count = content.lines().count();
            let description = format!("Write {} ({} lines)", file_path, line_count);
            let code = build_write_code(file_path, content);
            let approved = propose_in_editor(&description, &code).await?;
            let Some(script) = approved else {
                return Ok("Write aborted by user.".to_string());
            };
            let script_stdout = run_script_async(&script).await?;
            let updated = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read written file: {}", file_path))?;
            let diff = if was_missing {
                crate::cli::diff::FileDiff::from_created(file_path, &updated)
            } else {
                crate::cli::diff::FileDiff::from_texts(file_path, &original, &updated)
            }
            .to_unified();
            return Ok(if script_stdout.trim().is_empty() {
                diff
            } else {
                format!("{}\n{}", script_stdout.trim_end(), diff)
            });
        }

        // Non-interactive (tests, daemon): write directly.
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
            run_post_save_hook(file_path);

            Ok(crate::cli::diff::FileDiff::from_created(file_path, content).to_unified())
        } else {
            // Existing file: read original, write new, show stats
            let original = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read existing file: {}", file_path))?;

            fs::write(file_path, content)
                .with_context(|| format!("Failed to write file: {}", file_path))?;
            run_post_save_hook(file_path);

            Ok(crate::cli::diff::FileDiff::from_texts(file_path, &original, content).to_unified())
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
            live_output: None,
            effect_audit: None,
            poset: None,
        };
        let result = tool.execute(input, &context).await.unwrap();
        let diff = crate::cli::diff::FileDiff::parse(&result).unwrap();
        assert_eq!(diff.display_path(), path);
        assert_eq!(diff.old_path, "/dev/null");
        assert!(diff.is_created());
        assert_eq!((diff.added(), diff.removed()), (3, 0));
    }
}
