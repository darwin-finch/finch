// Tool display formatting
//
// Claude Code-style tool call rendering:
//
//   ⏺ Bash(git push origin main)
//     ⎿ To github.com:user/repo.git
//         abc123..def456  main -> main
//
//   ⏺ Read(src/cli/mod.rs)
//     ⎿ 33 lines

use serde_json::Value;

const CYAN: &str = "\x1b[36m";
const GRAY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

const MAX_PARAM_LEN: usize = 60;

/// Format a tool label like "Bash(git push)" or "Read(src/file.rs)"
pub fn format_tool_label(name: &str, input: &Value) -> String {
    let key_param = extract_key_param(name, input);
    if key_param.is_empty() {
        format!("{}{}{}{}", CYAN, BOLD, name, RESET)
    } else {
        format!(
            "{}{}{}{}({}{}{}{}){}",
            CYAN,
            BOLD,
            name,
            RESET,
            GRAY,
            truncate(&key_param, MAX_PARAM_LEN),
            RESET,
            CYAN,
            RESET,
        )
    }
}

/// Extract the most meaningful parameter to show in the label
fn extract_key_param(tool_name: &str, input: &Value) -> String {
    match tool_name.to_lowercase().as_str() {
        "bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            cmd.trim().to_string()
        }
        "read" => {
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
            url.trim_start_matches("https://")
                .trim_start_matches("http://")
                .to_string()
        }
        "write" => shorten_path(input["file_path"].as_str().unwrap_or("")),
        "edit" => shorten_path(input["file_path"].as_str().unwrap_or("")),
        "task" => input["description"].as_str().unwrap_or("").to_string(),
        "presentplan" | "present_plan" => {
            // Show the plan title (first # heading) rather than raw markdown content
            let plan = input["plan"].as_str().unwrap_or("");
            plan.lines()
                .find(|l| l.starts_with('#'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .unwrap_or_else(|| "proposing plan".to_string())
        }
        "askuserquestion" | "ask_user_question" => input["questions"]
            .as_array()
            .and_then(|q| q.first())
            .and_then(|q| q["question"].as_str())
            .unwrap_or("user prompt")
            .to_string(),
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

/// Shorten a file path for display.
///
/// Priority:
///   1. If the path is absolute and under the current working directory,
///      return the cwd-relative path (e.g. `src/cli/tui/mod.rs`).
///   2. Otherwise, keep the last 3 components with a `…/` prefix
///      (e.g. `…/cli/tui/mod.rs`).
///   3. Paths with ≤ 3 components are returned unchanged.
pub fn shorten_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }

    // Attempt cwd-relative shortening for absolute paths
    if std::path::Path::new(path).is_absolute() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = std::path::Path::new(path).strip_prefix(&cwd) {
                let s = rel.to_string_lossy().to_string();
                if !s.is_empty() {
                    return s;
                }
            }
        }
    }

    // Fallback: keep last 3 components
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return path.to_string();
    }
    format!("…/{}", parts[parts.len() - 3..].join("/"))
}

/// Truncate a string to max_len chars, adding "…" if needed
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}…", &s[..max_len])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── format_tool_label ────────────────────────────────────────────────────

    #[test]
    fn test_format_tool_label_no_param_shows_name_only() {
        let label = format_tool_label("Unknown", &serde_json::json!({}));
        assert!(label.contains("Unknown"), "label missing: {:?}", label);
        assert!(!label.contains('('), "unexpected paren: {:?}", label);
    }

    #[test]
    fn test_format_tool_label_bash() {
        let label = format_tool_label("Bash", &serde_json::json!({"command": "git status"}));
        assert!(label.contains("Bash"), "got: {:?}", label);
        assert!(label.contains("git status"), "got: {:?}", label);
    }

    #[test]
    fn test_format_tool_label_truncates_long_command() {
        let long_cmd = "a".repeat(100);
        let label = format_tool_label("Bash", &serde_json::json!({"command": long_cmd}));
        // Should be truncated — visible portion <= MAX_PARAM_LEN + ellipsis
        assert!(
            label.contains("…"),
            "long command should be truncated: {:?}",
            label
        );
    }

    // ── extract_key_param ────────────────────────────────────────────────────

    #[test]
    fn test_extract_key_param_bash() {
        let p = extract_key_param(
            "bash",
            &serde_json::json!({"command": "git push origin main"}),
        );
        assert_eq!(p, "git push origin main");
    }

    #[test]
    fn test_extract_key_param_bash_trims_whitespace() {
        let p = extract_key_param("bash", &serde_json::json!({"command": "  ls -la  "}));
        assert_eq!(p, "ls -la");
    }

    #[test]
    fn test_extract_key_param_read() {
        // Path longer than 3 components → last 3 shown (cwd-relative if under cwd)
        let p = extract_key_param("read", &serde_json::json!({"file_path": "/a/b/c/d/e.rs"}));
        // Either cwd-relative (if somehow under cwd) or last 3 components
        assert!(!p.is_empty());
        assert!(p.ends_with("e.rs"), "should end with filename: {}", p);
    }

    #[test]
    fn test_extract_key_param_read_short_path_unchanged() {
        let p = extract_key_param("read", &serde_json::json!({"file_path": "src/main.rs"}));
        assert_eq!(p, "src/main.rs");
    }

    #[test]
    fn test_extract_key_param_write() {
        let p = extract_key_param(
            "write",
            &serde_json::json!({"file_path": "/a/b/c/d/foo.rs"}),
        );
        assert!(p.ends_with("foo.rs"), "should end with filename: {}", p);
    }

    #[test]
    fn test_extract_key_param_edit() {
        let p = extract_key_param("edit", &serde_json::json!({"file_path": "/a/b/c/d/bar.rs"}));
        assert!(p.ends_with("bar.rs"), "should end with filename: {}", p);
    }

    #[test]
    fn test_extract_key_param_glob_no_dir() {
        let p = extract_key_param("glob", &serde_json::json!({"pattern": "**/*.rs"}));
        assert_eq!(p, "**/*.rs");
    }

    #[test]
    fn test_extract_key_param_glob_with_dir() {
        let p = extract_key_param(
            "glob",
            &serde_json::json!({"pattern": "*.rs", "path": "src/cli"}),
        );
        assert!(p.contains("*.rs"), "got: {}", p);
        assert!(p.contains("src/cli"), "got: {}", p);
    }

    #[test]
    fn test_extract_key_param_grep() {
        let p = extract_key_param(
            "grep",
            &serde_json::json!({"pattern": "fn main", "path": "src"}),
        );
        assert!(p.contains("fn main"), "got: {}", p);
        assert!(p.contains("src"), "got: {}", p);
    }

    #[test]
    fn test_extract_key_param_grep_long_pattern_truncated() {
        let long = "a".repeat(50);
        let p = extract_key_param("grep", &serde_json::json!({"pattern": long, "path": "."}));
        assert!(
            p.contains("…"),
            "long grep pattern should be truncated: {}",
            p
        );
    }

    #[test]
    fn test_extract_key_param_webfetch_strips_protocol() {
        let p = extract_key_param(
            "webfetch",
            &serde_json::json!({"url": "https://docs.rs/anyhow"}),
        );
        assert_eq!(p, "docs.rs/anyhow");
    }

    #[test]
    fn test_extract_key_param_webfetch_http() {
        let p = extract_key_param(
            "web_fetch",
            &serde_json::json!({"url": "http://example.com/page"}),
        );
        assert_eq!(p, "example.com/page");
    }

    #[test]
    fn test_extract_key_param_presentplan_shows_title() {
        let p = extract_key_param(
            "presentplan",
            &serde_json::json!({"plan": "# Fix the Bug\n\nSome details"}),
        );
        assert_eq!(p, "Fix the Bug");
    }

    #[test]
    fn test_extract_key_param_presentplan_fallback_when_no_heading() {
        let p = extract_key_param(
            "PresentPlan",
            &serde_json::json!({"plan": "No heading here, just prose."}),
        );
        assert_eq!(p, "proposing plan");
    }

    #[test]
    fn test_extract_key_param_presentplan_empty_plan() {
        let p = extract_key_param("presentplan", &serde_json::json!({"plan": ""}));
        assert_eq!(p, "proposing plan");
    }

    #[test]
    fn test_extract_key_param_askuserquestion_shows_question() {
        let p = extract_key_param(
            "AskUserQuestion",
            &serde_json::json!({
                "questions": [{"question": "Which approach do you prefer?", "header": "Approach", "options": [], "multiSelect": false}]
            }),
        );
        assert_eq!(p, "Which approach do you prefer?");
    }

    #[test]
    fn test_extract_key_param_askuserquestion_empty_fallback() {
        let p = extract_key_param("ask_user_question", &serde_json::json!({"questions": []}));
        assert_eq!(p, "user prompt");
    }

    #[test]
    fn test_extract_key_param_task_shows_description() {
        let p = extract_key_param(
            "task",
            &serde_json::json!({"description": "explore codebase"}),
        );
        assert_eq!(p, "explore codebase");
    }

    #[test]
    fn test_extract_key_param_unknown_tool_uses_first_string_param() {
        let p = extract_key_param(
            "custom_tool",
            &serde_json::json!({"some_key": "some value"}),
        );
        assert_eq!(p, "some value");
    }

    #[test]
    fn test_extract_key_param_unknown_tool_no_params_empty() {
        let p = extract_key_param("mystery", &serde_json::json!({}));
        assert!(p.is_empty(), "expected empty for no params: {:?}", p);
    }

    // ── shorten_path ─────────────────────────────────────────────────────────

    #[test]
    fn test_shorten_path_empty() {
        assert_eq!(shorten_path(""), "");
    }

    #[test]
    fn test_shorten_path_short_path_unchanged() {
        assert_eq!(shorten_path("src/main.rs"), "src/main.rs");
        assert_eq!(shorten_path("a/b/c"), "a/b/c");
    }

    #[test]
    fn test_shorten_path_4plus_components_keeps_last_3() {
        // Not under cwd — falls back to last 3 components
        let result = shorten_path("/a/b/c/d/e.rs");
        // Either cwd-relative (unlikely for /a/b/...) or last 3
        assert!(
            result.ends_with("c/d/e.rs") || result.contains("e.rs"),
            "got: {}",
            result
        );
    }

    #[test]
    fn test_shorten_path_exactly_3_components_unchanged() {
        assert_eq!(shorten_path("src/cli/mod.rs"), "src/cli/mod.rs");
    }

    #[test]
    fn test_shorten_path_cwd_relative_for_current_project() {
        // A file that IS under the current directory (Cargo.toml in cwd)
        let cwd = std::env::current_dir().unwrap();
        let absolute = cwd.join("src").join("lib.rs");
        let result = shorten_path(&absolute.to_string_lossy());
        // Should be the relative path, not truncated
        assert_eq!(
            result, "src/lib.rs",
            "expected relative path, got: {}",
            result
        );
    }

    #[test]
    fn test_shorten_path_last_3_with_ellipsis() {
        let result = shorten_path("/one/two/three/four/five/six.rs");
        assert!(
            result.starts_with("…/"),
            "should start with ellipsis: {}",
            result
        );
        assert!(
            result.ends_with("four/five/six.rs"),
            "should keep last 3 components: {}",
            result
        );
    }

    // ── truncate ─────────────────────────────────────────────────────────────

    #[test]
    fn test_truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_exact_length_unchanged() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_long_string_adds_ellipsis() {
        let result = truncate("hello world", 5);
        assert_eq!(result, "hello…");
    }
}
