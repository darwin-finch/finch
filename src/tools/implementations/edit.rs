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
use crossterm::style::{Attribute, Color, SetAttribute, SetBackgroundColor, SetForegroundColor};
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

const RED: SetForegroundColor = SetForegroundColor(Color::Red);
const GREEN: SetForegroundColor = SetForegroundColor(Color::Green);
const GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
const RED_BG: SetBackgroundColor = SetBackgroundColor(Color::AnsiValue(52));
const GREEN_BG: SetBackgroundColor = SetBackgroundColor(Color::AnsiValue(22));
const RESET: SetAttribute = SetAttribute(Attribute::Reset);

const CONTEXT_LINES: usize = 3;

/// Build a bash/python script that applies an edit to a file.
/// Used by the propose-before-execute flow.
fn build_edit_code(file_path: &str, old_string: &str, new_string: &str, replace_all: bool) -> String {
    let old_b64 = base64::engine::general_purpose::STANDARD.encode(old_string);
    let new_b64 = base64::engine::general_purpose::STANDARD.encode(new_string);
    let path_py = format!("{:?}", file_path);
    let replace_all_py = if replace_all { "True" } else { "False" };
    [
        "python3 << 'PYEOF'\n",
        "import base64, sys\n",
        &format!("path = {}\n", path_py),
        &format!("old = base64.b64decode(b\"{}\").decode(\"utf-8\")\n", old_b64),
        &format!("new_str = base64.b64decode(b\"{}\").decode(\"utf-8\")\n", new_b64),
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
            return match approved {
                None => Ok("Edit aborted by user.".to_string()),
                Some(script) => run_script_async(&script).await,
            };
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
        Ok(generate_edit_diff(
            &original,
            old_string,
            new_string,
            match_count.min(if replace_all { match_count } else { 1 }),
        ))
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
    let orig_lines: Vec<&str> = original.lines().collect();
    let old_str_lines: Vec<&str> = old_string.lines().collect();
    let new_str_lines: Vec<&str> = new_string.lines().collect();

    let added = new_str_lines.len();
    let removed = old_str_lines.len();

    // Build summary
    let mut summary_parts = Vec::new();
    if added > 0 {
        summary_parts.push(format!(
            "Added {} line{}",
            added,
            if added == 1 { "" } else { "s" }
        ));
    }
    if removed > 0 {
        summary_parts.push(format!(
            "removed {} line{}",
            removed,
            if removed == 1 { "" } else { "s" }
        ));
    }
    if occurrences > 1 {
        summary_parts.push(format!("{} occurrences replaced", occurrences));
    }
    let summary = if summary_parts.is_empty() {
        "No changes".to_string()
    } else {
        summary_parts.join(", ")
    };

    // Find the byte offset of first occurrence
    let start_byte = original.find(old_string).unwrap_or(0);
    let start_line = original[..start_byte].lines().count(); // 0-indexed

    let context_start = start_line.saturating_sub(CONTEXT_LINES);
    let context_end = (start_line + removed + CONTEXT_LINES).min(orig_lines.len());

    // Determine line number width for alignment
    let max_line_num = orig_lines.len().max(start_line + added + CONTEXT_LINES);
    let num_width = max_line_num.to_string().len().max(3);

    let mut output = format!("{}\n", summary);

    // Context before
    for (i, line) in orig_lines[context_start..start_line].iter().enumerate() {
        let line_num = context_start + i + 1;
        output.push_str(&format!(
            "  {GRAY}{:>width$}{RESET}     {}\n",
            line_num,
            line,
            width = num_width
        ));
    }

    // Removed lines (red)
    for (i, line) in old_str_lines.iter().enumerate() {
        let line_num = start_line + i + 1;
        output.push_str(&format!(
            "  {RED_BG}{RED}{:>width$} -{RESET}{RED_BG}   {}{RESET}\n",
            line_num,
            line,
            width = num_width
        ));
    }

    // Added lines (green)
    // New line numbers after the edit
    let new_start_line = start_line + 1;
    for (i, line) in new_str_lines.iter().enumerate() {
        let line_num = new_start_line + i;
        output.push_str(&format!(
            "  {GREEN_BG}{GREEN}{:>width$} +{RESET}{GREEN_BG}   {}{RESET}\n",
            line_num,
            line,
            width = num_width
        ));
    }

    // Context after (using new line numbers)
    let after_orig_start = start_line + removed;
    let new_context_start = start_line + added;
    for (i, line) in orig_lines[after_orig_start..context_end].iter().enumerate() {
        let new_line_num = new_context_start + i + 1;
        output.push_str(&format!(
            "  {GRAY}{:>width$}{RESET}     {}\n",
            new_line_num,
            line,
            width = num_width
        ));
    }

    output
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
        assert!(diff.contains("Added 2 lines"), "got: {}", diff);
        assert!(diff.contains("removed 1 line"), "got: {}", diff);
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
            stack: None,
            poset: None,
        };
        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
