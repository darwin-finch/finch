// Edit tool - exact string replacement in files with colored diff output
//
// Returns a diff showing what changed, formatted like Claude Code:
//
//   Added 2 lines, removed 7 lines
//      196     pub fn validate(&self) -> anyhow::Result<()> {
//      199 -   // Old comment
//      199 +   // New comment

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use std::fs;
use std::io::IsTerminal;

use super::propose::{propose_in_editor, run_script_async};

/// Run `~/.finch/hooks/post-save <file_path>` if that script exists.
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

/// Build a bash/python script that applies an edit to a file.
/// Used by the propose-before-execute flow.
fn build_edit_code(
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> String {
    let old_b64 = base64::engine::general_purpose::STANDARD.encode(old_string);
    let new_b64 = base64::engine::general_purpose::STANDARD.encode(new_string);
    let path_py = format!("{:?}", file_path);
    let replace_all_py = if replace_all { "True" } else { "False" };
    [
        "python3 << 'PYEOF'\n",
        "import base64, sys\n",
        &format!("path = {}\n", path_py),
        &format!(
            "old = base64.b64decode(b\"{}\").decode(\"utf-8\")\n",
            old_b64
        ),
        &format!(
            "new_str = base64.b64decode(b\"{}\").decode(\"utf-8\")\n",
            new_b64
        ),
        "with open(path, \"r\") as f:\n",
        "    content = f.read()\n",
        "if old not in content:\n",
        "    print(\"ERROR: old_string not found in \" + path, file=sys.stderr)\n",
        "    sys.exit(1)\n",
        &format!("if {}:\n", replace_all_py),
        "    content = content.replace(old, new_str)\n",
        "else:\n",
        "    content = content.replace(old, new_str, 1)\n",
        "with open(path, \"w\") as f:\n",
        "    f.write(content)\n",
        "print(\"Edited \" + path)\n",
        "PYEOF",
    ]
    .concat()
}

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string with new content. \
         ALWAYS use this tool to modify existing files — never use bash with sed/awk/echo. \
         old_string must match exactly (including whitespace). If it appears multiple times, \
         include more context lines to make it unique, or set replace_all: true. \
         Returns a colored diff showing what changed."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to modify"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact text to replace (must be unique in the file unless replace_all is true)"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement text"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false, requires unique match)"
                }
            }),
            required: vec![
                "file_path".to_string(),
                "old_string".to_string(),
                "new_string".to_string(),
            ],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;
        let old_string = input["old_string"]
            .as_str()
            .context("Missing old_string parameter")?;
        let new_string = input["new_string"]
            .as_str()
            .context("Missing new_string parameter")?;
        let replace_all = input["replace_all"].as_bool().unwrap_or(false);

        // Interactive: propose the script in $EDITOR before applying.
        if std::io::stdin().is_terminal() {
            let original = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read file: {}", file_path))?;
            let old_lines = old_string.lines().count();
            let new_lines = new_string.lines().count();
            let description = format!(
                "Edit {}\nRemove {} line{}, add {} line{}",
                file_path,
                old_lines,
                if old_lines == 1 { "" } else { "s" },
                new_lines,
                if new_lines == 1 { "" } else { "s" },
            );
            let code = build_edit_code(file_path, old_string, new_string, replace_all);
            let approved = propose_in_editor(&description, &code).await?;
            let Some(script) = approved else {
                return Ok("Edit aborted by user.".to_string());
            };
            run_script_async(&script).await?;
            let updated = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to read edited file: {}", file_path))?;
            return Ok(
                crate::cli::diff::FileDiff::from_texts(file_path, &original, &updated).to_unified(),
            );
        }

        // Non-interactive (tests, daemon): apply in Rust.
        // Read original content
        let original = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        // Validate old_string exists
        let match_count = original.matches(old_string).count();
        if match_count == 0 {
            return Err(anyhow::anyhow!(
                "old_string not found in {}\n\
                 Tip: Check for exact whitespace and line endings",
                file_path
            ));
        }
        if match_count > 1 && !replace_all {
            return Err(anyhow::anyhow!(
                "old_string appears {} times in {}.\n\
                 Use replace_all: true to change all occurrences, or make old_string more specific \
                 by including more context lines.",
                match_count,
                file_path
            ));
        }

        // Apply edit
        let new_content = if replace_all {
            original.replace(old_string, new_string)
        } else {
            original.replacen(old_string, new_string, 1)
        };

        // Write updated content
        fs::write(file_path, &new_content)
            .with_context(|| format!("Failed to write file: {}", file_path))?;
        run_post_save_hook(file_path);

        // Generate and return colored diff
        Ok(crate::cli::diff::FileDiff::from_texts(file_path, &original, &new_content).to_unified())
    }
}

/// Generate a colored unified diff showing what changed.
///
/// Format:
///   Added N lines, removed M lines
///     196     pub fn validate(&self) -> ...
///     199 -   // Old comment
///     199 +   // New comment
pub fn generate_edit_diff(
    original: &str,
    old_string: &str,
    new_string: &str,
    occurrences: usize,
) -> String {
    let new_content = if occurrences > 1 {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    };
    crate::cli::diff::FileDiff::from_texts("file", original, &new_content).to_unified()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_summary_counts() {
        let diff = generate_edit_diff(
            "line1\nold line\nline3\n",
            "old line",
            "new line A\nnew line B",
            1,
        );
        let parsed = crate::cli::diff::FileDiff::parse(&diff).unwrap();
        assert_eq!((parsed.added(), parsed.removed()), (2, 1));
    }

    #[test]
    fn test_diff_shows_removed_added() {
        let diff = generate_edit_diff("a\nb\nc\n", "b", "x\ny", 1);
        assert!(diff.contains("b"), "should show removed line");
        assert!(diff.contains("x"), "should show added line");
        assert!(diff.contains("y"), "should show added line");
    }

    #[tokio::test]
    async fn test_edit_not_found() {
        let tool = EditTool;
        let input = serde_json::json!({
            "file_path": "Cargo.toml",
            "old_string": "THIS_STRING_DEFINITELY_DOES_NOT_EXIST_IN_FILE_12345",
            "new_string": "replacement"
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
            poset: None,
        };
        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
