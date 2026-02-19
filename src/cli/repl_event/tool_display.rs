// Tool display formatting
//
// Claude Code-style tool call rendering:
//
//   ● Bash(git push origin main)
//     └ To github.com:user/repo.git
//         abc123..def456  main -> main
//
//   ● Read(src/cli/mod.rs)
//     └ 33 lines read

use std::sync::Arc;
use serde_json::Value;

const CYAN: &str = "\x1b[36m";
const GRAY: &str = "\x1b[90m";
const RED: &str = "\x1b[31m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

const MAX_PARAM_LEN: usize = 60;
const MAX_OUTPUT_LINES: usize = 8;

/// Format a tool label like "Bash(git push)" or "Read(src/file.rs)"
pub fn format_tool_label(name: &str, input: &Value) -> String {
    let key_param = extract_key_param(name, input);
    if key_param.is_empty() {
        format!("{}{}{}{}", CYAN, BOLD, name, RESET)
    } else {
        format!(
            "{}{}{}{}({}{}{}{}){}",
            CYAN, BOLD, name, RESET,
            GRAY, truncate(&key_param, MAX_PARAM_LEN), RESET,
            CYAN, RESET,
        )
    }
}

/// Format the full tool call display (label + output)
pub fn format_tool_result(label: &str, content: &str, is_error: bool) -> String {
    let bullet = if is_error {
        format!("{}●{}", RED, RESET)
    } else {
        format!("{}●{}", CYAN, RESET)
    };

    let mut result = format!("{} {}\n", bullet, label);

    if content.trim().is_empty() {
        return result;
    }

    let lines: Vec<&str> = content.lines().collect();
    let shown = lines.len().min(MAX_OUTPUT_LINES);

    for (i, line) in lines.iter().take(shown).enumerate() {
        if i == 0 {
            result.push_str(&format!("  {}└{} {}\n", GRAY, RESET, line));
        } else {
            result.push_str(&format!("    {}{}{}\n", GRAY, line, RESET));
        }
    }

    if lines.len() > MAX_OUTPUT_LINES {
        result.push_str(&format!(
            "  {}… +{} lines{}\n",
            GRAY,
            lines.len() - MAX_OUTPUT_LINES,
            RESET
        ));
    }

    result
}

/// Extract the most meaningful parameter to show in the label
fn extract_key_param(tool_name: &str, input: &Value) -> String {
    match tool_name.to_lowercase().as_str() {
        "bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            // Show first 60 chars of command, stripping leading whitespace
            cmd.trim().to_string()
        }
        "read" => {
            // Shorten path: keep last 2 components
            let path = input["file_path"].as_str().unwrap_or("");
            shorten_path(path)
        }
        "glob" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            let dir = input["path"].as_str().unwrap_or("");
            if dir.is_empty() {
                pattern.to_string()
            } else {
                format!("{} in {}", pattern, shorten_path(dir))
            }
        }
        "grep" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or(".");
            format!("{} in {}", truncate(pattern, 30), shorten_path(path))
        }
        "webfetch" | "web_fetch" => {
            let url = input["url"].as_str().unwrap_or("");
            // Strip protocol for display
            url.trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string()
        }
        "write" => shorten_path(input["file_path"].as_str().unwrap_or("")),
        "edit" => shorten_path(input["file_path"].as_str().unwrap_or("")),
        "task" => input["description"].as_str().unwrap_or("").to_string(),
        "askuserquestion" | "ask_user_question" => {
            // Show the question text
            input["questions"]
                .as_array()
                .and_then(|q| q.first())
                .and_then(|q| q["question"].as_str())
                .unwrap_or("user prompt")
                .to_string()
        }
        _ => {
            // For unknown tools, show first string param value
            if let Some(obj) = input.as_object() {
                for (_k, v) in obj.iter() {
                    if let Some(s) = v.as_str() {
                        if !s.is_empty() {
                            return s.to_string();
                        }
                    }
                }
            }
            String::new()
        }
    }
}

/// Shorten a file path to last 2-3 components
fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return path.to_string();
    }
    format!("…/{}", parts[parts.len() - 2..].join("/"))
}

/// Truncate a string to max_len chars, adding "…" if needed
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len])
    }
}

/// Spawn a background task that streams tool output into a LiveToolMessage line by line.
///
/// Each line is appended with a small delay, creating the visual "streaming diff"
/// effect that makes it obvious what changed. The message is marked complete when done.
pub fn stream_tool_output_to_message(
    msg: Arc<crate::cli::messages::LiveToolMessage>,
    content: String,
    is_error: bool,
) {
    tokio::spawn(async move {
        // Build the full formatted output (summary line + diff lines)
        // For non-error content: wrap with └ on first line
        let formatted = build_result_content(&content, is_error);
        let lines: Vec<&str> = formatted.lines().collect();

        // Stream lines with a short delay for visual effect
        // Shorter delay = faster; adjust to taste
        let delay_ms = if lines.len() > 20 { 8 } else { 15 };

        for line in &lines {
            msg.append_line(line);
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
        }

        if is_error {
            msg.set_failed();
        } else {
            msg.set_complete();
        }
    });
}

/// Build the formatted content for a tool result (without the ● header - that's in the message)
fn build_result_content(content: &str, is_error: bool) -> String {
    if content.trim().is_empty() {
        return String::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let shown = lines.len().min(MAX_OUTPUT_LINES);
    let mut result = String::new();

    for (i, line) in lines.iter().take(shown).enumerate() {
        if i == 0 {
            if is_error {
                result.push_str(&format!("  {}└{} {}{}{}\n", GRAY, RESET, RED, line, RESET));
            } else {
                result.push_str(&format!("  {}└{} {}\n", GRAY, RESET, line));
            }
        } else {
            // Preserve existing ANSI colors in diff lines (don't add extra gray wrapping)
            result.push_str(&format!("    {}\n", line));
        }
    }

    if lines.len() > MAX_OUTPUT_LINES {
        result.push_str(&format!(
            "  {}… +{} lines (ctrl+o to expand){}\n",
            GRAY,
            lines.len() - MAX_OUTPUT_LINES,
            RESET
        ));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_label() {
        let label = extract_key_param("bash", &serde_json::json!({"command": "git push origin main"}));
        assert_eq!(label, "git push origin main");
    }

    #[test]
    fn test_read_label() {
        let label = extract_key_param("read", &serde_json::json!({"file_path": "/Users/foo/repos/project/src/main.rs"}));
        assert_eq!(label, "…/src/main.rs");
    }

    #[test]
    fn test_shorten_path() {
        assert_eq!(shorten_path("/a/b/c/d/e.rs"), "…/d/e.rs");
        assert_eq!(shorten_path("src/main.rs"), "src/main.rs");
        assert_eq!(shorten_path(""), "");
    }

    #[test]
    fn test_format_tool_result_truncation() {
        let content = (0..20).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let result = format_tool_result("Bash(ls)", &content, false);
        assert!(result.contains("+12 lines"));
    }
}
