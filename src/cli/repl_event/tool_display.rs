//! Tool display formatting
//!
//! Two responsibilities:
//!
//! 1. **Label formatting** — `format_tool_label()` and `shorten_path()` render
//!    the `⏺ ToolName(key-param)` line before a tool starts executing.
//!
//! 2. **Result formatting** — `tool_result_to_display()` converts a completed
//!    tool's raw output into a `(summary, body_lines)` pair for the inline row:
//!
//! ```text
//! ⏺ Bash(git push origin main)
//!   ⎿ abc123..def456  main -> main   ← summary
//! ```
//!
//! Also contains `format_elapsed` and `format_token_count` used in the status
//! bar and in tests.

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

// ── Tool result display ──────────────────────────────────────────────────────

/// Maximum number of body lines shown beneath a tool row before adding an
/// overflow hint.
pub(crate) const MAX_TOOL_BODY_LINES: usize = 20;

/// Produce a semantic `(summary, body_lines)` pair for a completed tool result.
///
/// The summary is a compact one-liner shown on the `⎿ label  summary` line.
/// Body lines are rendered indented below — diff content for Edit, command
/// output for Bash, file paths for Glob, match lines for Grep, etc.
///
/// Matches Claude Code's display style:
///   Edit  → "Added/Removed N lines" + colored diff body
///   Read  → "N lines" (body suppressed — file content too large inline)
///   Write → "Created foo.rs (N lines)"
///   Glob  → "N files" + first 8 paths
///   Grep  → "N matches" + first 8 match lines
///   Bash  → semantic summary line + remaining lines as body
pub(crate) fn tool_result_to_display(tool_name: &str, content: &str) -> (String, Vec<String>) {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return (String::new(), Vec::new());
    }

    match tool_name.to_lowercase().as_str() {
        "edit" => {
            let mut lines_iter = trimmed.lines();
            let summary = lines_iter.next().unwrap_or("").trim().to_string();
            let body_lines: Vec<String> = lines_iter.map(|l| l.to_string()).collect();
            let total = body_lines.len();
            let mut body: Vec<String> = body_lines.into_iter().take(MAX_TOOL_BODY_LINES).collect();
            if total > MAX_TOOL_BODY_LINES {
                body.push(format!(
                    "\x1b[90m… +{} lines (ctrl+o to expand)\x1b[0m",
                    total - MAX_TOOL_BODY_LINES
                ));
            }
            (summary, body)
        }

        "read" => {
            let count = trimmed.lines().count();
            let summary = if count == 1 {
                "1 line".to_string()
            } else {
                format!("{} lines", count)
            };
            (summary, Vec::new())
        }

        "write" => (compact_tool_summary(content), Vec::new()),

        "glob" => {
            let lines: Vec<&str> = trimmed.lines().collect();
            let count = lines.len();
            let summary = if lines[0].starts_with("No files") {
                lines[0].to_string()
            } else if count == 1 {
                "1 file".to_string()
            } else {
                format!("{} files", count)
            };
            let body: Vec<String> = lines.iter().take(8).map(|l| l.to_string()).collect();
            (summary, body)
        }

        "grep" => {
            let lines: Vec<&str> = trimmed.lines().collect();
            let count = lines.len();
            let summary = if count == 1 {
                "1 match".to_string()
            } else {
                format!("{} matches", count)
            };
            let total = lines.len();
            let mut body: Vec<String> = lines.iter().take(8).map(|l| l.to_string()).collect();
            if total > 8 {
                body.push(format!(
                    "\x1b[90m… +{} more (ctrl+o to expand)\x1b[0m",
                    total - 8
                ));
            }
            (summary, body)
        }

        "bash" => {
            let summary = bash_smart_summary(trimmed);
            let lines: Vec<&str> = trimmed.lines().collect();
            let total = lines.len();
            let mut body: Vec<String> = lines
                .iter()
                .take(MAX_TOOL_BODY_LINES)
                .map(|l| l.to_string())
                .collect();
            if total > MAX_TOOL_BODY_LINES {
                body.push(format!(
                    "\x1b[90m… +{} lines (ctrl+o to expand)\x1b[0m",
                    total - MAX_TOOL_BODY_LINES
                ));
            }
            (summary, body)
        }

        _ => (compact_tool_summary(content), Vec::new()),
    }
}

/// Strip ANSI escape codes from a string, returning plain text.
///
/// Handles CSI sequences (`ESC [ ... m`) and simple OSC sequences.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc == '\x07' || nc == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Extract the single most meaningful summary line from bash command output.
///
/// Scanning priority (first match wins):
///   1. `test result:` line  — cargo test final verdict
///   2. Last `Finished ` line — cargo build/check/test success
///   3. `error: could not compile` — cargo build failure
///   4. First `error[E…]` line — first compiler error
///   5. `Exit code: N` line — non-zero exit
///   6. Last non-empty line   — general fallback
fn bash_smart_summary(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return String::new();
    }
    for line in &lines {
        let clean = strip_ansi(line.trim());
        if clean.starts_with("test result:") {
            return truncate_summary(clean);
        }
    }
    for line in lines.iter().rev() {
        let clean = strip_ansi(line.trim());
        if clean.starts_with("Finished ") {
            return truncate_summary(clean);
        }
    }
    for line in lines.iter().rev() {
        let clean = strip_ansi(line.trim());
        if clean.starts_with("error: could not compile") || clean.starts_with("error: aborting") {
            return truncate_summary(clean);
        }
    }
    for line in &lines {
        let clean = strip_ansi(line.trim());
        if clean.starts_with("error[E") || clean.starts_with("error[") {
            return truncate_summary(clean);
        }
    }
    for line in &lines {
        let clean = strip_ansi(line.trim());
        if clean.starts_with("Exit code:") {
            return clean;
        }
    }
    for line in lines.iter().rev() {
        let clean = strip_ansi(line.trim());
        if !clean.is_empty() {
            return truncate_summary(clean);
        }
    }
    String::new()
}

/// Truncate a summary string to 70 visible characters.
fn truncate_summary(s: String) -> String {
    if s.len() <= 70 {
        s
    } else {
        format!("{}…", s.chars().take(69).collect::<String>())
    }
}

/// Format tool output for generic single-line or multi-line display.
///
/// - Empty content → ""
/// - Single line   → the line, truncated to 60 chars
/// - Multi-line    → "\<N\> lines"
pub(crate) fn compact_tool_summary(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() == 1 {
        let line = lines[0].trim();
        if line.len() > 60 {
            format!("{}…", line.chars().take(57).collect::<String>())
        } else {
            line.to_string()
        }
    } else {
        format!("{} lines", lines.len())
    }
}

// ── Time / token formatting ──────────────────────────────────────────────────

/// Format elapsed seconds as "Xs" or "Xm Ys".
pub fn format_elapsed(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Format a token count as "N" or "N.Nk".
pub fn format_token_count(n: usize) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{}", n)
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

    // ── format_elapsed ───────────────────────────────────────────────────────

    #[test]
    fn test_format_elapsed_seconds() {
        assert_eq!(format_elapsed(0), "0s");
        assert_eq!(format_elapsed(1), "1s");
        assert_eq!(format_elapsed(59), "59s");
    }

    #[test]
    fn test_format_elapsed_minutes() {
        assert_eq!(format_elapsed(60), "1m 0s");
        assert_eq!(format_elapsed(61), "1m 1s");
        assert_eq!(format_elapsed(90), "1m 30s");
        assert_eq!(format_elapsed(600), "10m 0s");
        assert_eq!(format_elapsed(3661), "61m 1s");
    }

    // ── format_token_count ───────────────────────────────────────────────────

    #[test]
    fn test_format_token_count_small() {
        assert_eq!(format_token_count(0), "0");
        assert_eq!(format_token_count(1), "1");
        assert_eq!(format_token_count(999), "999");
    }

    #[test]
    fn test_format_token_count_thousands() {
        assert_eq!(format_token_count(1000), "1.0k");
        assert_eq!(format_token_count(1500), "1.5k");
        assert_eq!(format_token_count(9900), "9.9k");
        assert_eq!(format_token_count(10000), "10.0k");
    }

    // ── compact_tool_summary ─────────────────────────────────────────────────

    #[test]
    fn test_compact_tool_summary_empty() {
        assert_eq!(compact_tool_summary(""), "");
        assert_eq!(compact_tool_summary("   "), "");
    }

    #[test]
    fn test_compact_tool_summary_single_line() {
        assert_eq!(compact_tool_summary("hello"), "hello");
        let long = "a".repeat(70);
        let result = compact_tool_summary(&long);
        assert!(result.ends_with('…'));
        assert!(result.len() <= 61);
    }

    #[test]
    fn test_compact_tool_summary_multi_line() {
        let multi = "line1\nline2\nline3";
        assert_eq!(compact_tool_summary(multi), "3 lines");
    }

    // ── tool_result_to_display ───────────────────────────────────────────────

    #[test]
    fn test_tool_result_edit_extracts_summary_and_diff() {
        let content = "Removed 3 lines\n  line 1\n  line 2\n  line 3";
        let (summary, body) = tool_result_to_display("edit", content);
        assert_eq!(summary, "Removed 3 lines");
        assert_eq!(body.len(), 3);
        assert!(body[0].contains("line 1"));
    }

    #[test]
    fn test_tool_result_edit_added_and_removed() {
        let content = "Added 2 lines, removed 1 line\n+ new A\n+ new B\n- old";
        let (summary, body) = tool_result_to_display("edit", content);
        assert_eq!(summary, "Added 2 lines, removed 1 line");
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn test_tool_result_edit_only_summary_no_body() {
        let (summary, body) = tool_result_to_display("edit", "No changes");
        assert_eq!(summary, "No changes");
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_edit_truncates_large_diff() {
        let summary_line = "Removed 30 lines";
        let diff_lines: Vec<String> = (0..30).map(|i| format!("  diff line {}", i)).collect();
        let content = format!("{}\n{}", summary_line, diff_lines.join("\n"));
        let (summary, body) = tool_result_to_display("edit", &content);
        assert_eq!(summary, summary_line);
        assert_eq!(
            body.len(),
            MAX_TOOL_BODY_LINES + 1,
            "should have lines + overflow hint"
        );
        assert!(
            body.last().unwrap().contains("ctrl+o to expand"),
            "overflow hint missing: {:?}",
            body.last()
        );
    }

    #[test]
    fn test_tool_result_edit_case_insensitive() {
        let content = "Removed 1 line\n  x";
        let (summary, _) = tool_result_to_display("Edit", content);
        assert_eq!(summary, "Removed 1 line");
    }

    #[test]
    fn test_tool_result_read_returns_line_count_no_body() {
        let content = (0..50)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (summary, body) = tool_result_to_display("read", &content);
        assert_eq!(summary, "50 lines");
        assert!(body.is_empty(), "Read must not show file content inline");
    }

    #[test]
    fn test_tool_result_read_single_line() {
        let (summary, body) = tool_result_to_display("read", "just one line");
        assert_eq!(summary, "1 line");
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_read_large_file_still_no_body() {
        let content = (0..1000)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let (summary, body) = tool_result_to_display("read", &content);
        assert_eq!(summary, "1000 lines");
        assert!(body.is_empty(), "Large file must not bloat body");
    }

    #[test]
    fn test_tool_result_write_created() {
        let content = "Created foo.rs (42 lines)";
        let (summary, body) = tool_result_to_display("write", content);
        assert_eq!(summary, "Created foo.rs (42 lines)");
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_write_updated() {
        let content = "Updated foo.rs (10 → 15 lines, +5 lines)";
        let (summary, body) = tool_result_to_display("write", content);
        assert!(summary.contains("Updated"), "got: {}", summary);
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_glob_counts_files() {
        let content = "src/main.rs\nsrc/lib.rs\nsrc/foo.rs";
        let (summary, body) = tool_result_to_display("glob", content);
        assert_eq!(summary, "3 files");
        assert_eq!(body.len(), 3);
    }

    #[test]
    fn test_tool_result_glob_single_file() {
        let (summary, body) = tool_result_to_display("glob", "src/main.rs");
        assert_eq!(summary, "1 file");
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn test_tool_result_glob_no_files_found() {
        let (summary, body) = tool_result_to_display("glob", "No files found matching pattern.");
        assert!(summary.contains("No files"), "got: {}", summary);
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn test_tool_result_glob_many_files_body_capped_at_8() {
        let paths: Vec<String> = (0..20).map(|i| format!("file{}.rs", i)).collect();
        let content = paths.join("\n");
        let (summary, body) = tool_result_to_display("glob", &content);
        assert_eq!(summary, "20 files");
        assert_eq!(body.len(), 8, "body should be capped at 8 paths");
    }

    #[test]
    fn test_tool_result_grep_counts_matches() {
        let content = "src/foo.rs:10:> match A\nsrc/bar.rs:20:> match B";
        let (summary, body) = tool_result_to_display("grep", content);
        assert_eq!(summary, "2 matches");
        assert_eq!(body.len(), 2);
    }

    #[test]
    fn test_tool_result_grep_single_match() {
        let (summary, body) = tool_result_to_display("grep", "src/foo.rs:5:> found it");
        assert_eq!(summary, "1 match");
        assert_eq!(body.len(), 1);
    }

    #[test]
    fn test_tool_result_grep_many_matches_overflow_hint() {
        let lines: Vec<String> = (0..15).map(|i| format!("file.rs:{}:> hit", i)).collect();
        let content = lines.join("\n");
        let (summary, body) = tool_result_to_display("grep", &content);
        assert_eq!(summary, "15 matches");
        assert_eq!(body.len(), 9);
        assert!(body.last().unwrap().contains("ctrl+o to expand"));
    }

    #[test]
    fn test_tool_result_bash_cargo_test_success() {
        let content = "   Compiling finch v0.7.7\n    Finished test profile in 5s\n\
                       running 42 tests\ntest foo ... ok\n\
                       test result: ok. 42 passed; 0 failed; 2 ignored";
        let (summary, _) = tool_result_to_display("bash", content);
        assert_eq!(summary, "test result: ok. 42 passed; 0 failed; 2 ignored");
    }

    #[test]
    fn test_tool_result_bash_cargo_test_failure() {
        let content = "running 5 tests\ntest foo ... ok\ntest bar ... FAILED\n\
                       test result: FAILED. 1 passed; 1 failed; 0 ignored";
        let (summary, _) = tool_result_to_display("bash", content);
        assert_eq!(
            summary,
            "test result: FAILED. 1 passed; 1 failed; 0 ignored"
        );
    }

    #[test]
    fn test_tool_result_bash_cargo_build_success() {
        let content =
            "   Compiling foo v1.0\n    Finished `dev` profile [unoptimized] target(s) in 3s";
        let (summary, _) = tool_result_to_display("bash", content);
        assert!(summary.contains("Finished"), "got: {}", summary);
    }

    #[test]
    fn test_tool_result_bash_cargo_build_error() {
        let content =
            "error[E0308]: mismatched types\n  --> src/main.rs:5:10\nerror: could not compile `foo`";
        let (summary, _) = tool_result_to_display("bash", content);
        assert!(
            summary.contains("could not compile") || summary.contains("error[E"),
            "got: {}",
            summary
        );
    }

    #[test]
    fn test_tool_result_bash_exit_code_nonzero() {
        let content =
            "STDERR:\nls: cannot access '/nope': No such file or directory\nExit code: 2";
        let (summary, _) = tool_result_to_display("bash", content);
        assert!(
            summary.contains("Exit code:") || !summary.is_empty(),
            "got: {}",
            summary
        );
    }

    #[test]
    fn test_tool_result_bash_git_push_shows_last_line() {
        let content = "To github.com:user/repo.git\n   abc1234..def5678  main -> main";
        let (summary, _) = tool_result_to_display("bash", content);
        assert!(!summary.is_empty(), "summary should not be empty");
    }

    #[test]
    fn test_tool_result_bash_single_line() {
        let (summary, body) = tool_result_to_display("bash", "Hello, World!");
        assert_eq!(summary, "Hello, World!");
        let _ = body;
    }

    #[test]
    fn test_tool_result_bash_strips_ansi_from_summary() {
        let content = "\x1b[32mtest result: ok. 5 passed; 0 failed\x1b[0m";
        let (summary, _) = tool_result_to_display("bash", content);
        assert!(
            summary.contains("test result:"),
            "ANSI stripping failed, got: {:?}",
            summary
        );
    }

    #[test]
    fn test_tool_result_bash_body_shown() {
        let content = "line 1\nline 2\nline 3";
        let (_, body) = tool_result_to_display("bash", content);
        assert!(!body.is_empty(), "bash should show output lines in body");
    }

    #[test]
    fn test_tool_result_bash_large_output_overflow_hint() {
        let lines: Vec<String> = (0..30).map(|i| format!("output line {}", i)).collect();
        let content = lines.join("\n");
        let (_, body) = tool_result_to_display("bash", &content);
        assert!(
            body.len() <= MAX_TOOL_BODY_LINES + 1,
            "body should be capped"
        );
        if body.len() == MAX_TOOL_BODY_LINES + 1 {
            assert!(body.last().unwrap().contains("ctrl+o to expand"));
        }
    }

    #[test]
    fn test_tool_result_empty_returns_empty() {
        for tool in &["bash", "read", "edit", "write", "glob", "grep"] {
            let (summary, body) = tool_result_to_display(tool, "");
            assert!(
                summary.is_empty(),
                "tool={} summary should be empty for empty content",
                tool
            );
            assert!(
                body.is_empty(),
                "tool={} body should be empty for empty content",
                tool
            );
        }
    }

    #[test]
    fn test_tool_result_whitespace_only_returns_empty() {
        let (summary, body) = tool_result_to_display("bash", "   \n  \n  ");
        assert!(summary.is_empty(), "got: {:?}", summary);
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_unknown_tool_falls_back_to_compact() {
        let (summary, body) = tool_result_to_display("mystery_tool", "single line result");
        assert_eq!(summary, "single line result");
        assert!(body.is_empty());
    }

    #[test]
    fn test_tool_result_unknown_tool_multiline_compact() {
        let content = "line1\nline2\nline3";
        let (summary, body) = tool_result_to_display("unknown", content);
        assert_eq!(summary, "3 lines");
        assert!(body.is_empty());
    }

    // ── strip_ansi ───────────────────────────────────────────────────────────

    #[test]
    fn test_strip_ansi_plain_string_unchanged() {
        assert_eq!(strip_ansi("hello world"), "hello world");
    }

    #[test]
    fn test_strip_ansi_removes_color_codes() {
        let colored = "\x1b[32mgreen text\x1b[0m";
        assert_eq!(strip_ansi(colored), "green text");
    }

    #[test]
    fn test_strip_ansi_removes_bold() {
        let bold = "\x1b[1mbold\x1b[0m";
        assert_eq!(strip_ansi(bold), "bold");
    }

    #[test]
    fn test_strip_ansi_complex_sequence() {
        let s = "\x1b[2;90mfaint gray\x1b[0m normal";
        assert_eq!(strip_ansi(s), "faint gray normal");
    }

    #[test]
    fn test_strip_ansi_empty_string() {
        assert_eq!(strip_ansi(""), "");
    }

    // ── bash_smart_summary ───────────────────────────────────────────────────

    #[test]
    fn test_bash_smart_summary_cargo_test_ok() {
        let out = "running 5 tests\ntest a ... ok\ntest result: ok. 5 passed; 0 failed";
        assert_eq!(
            bash_smart_summary(out),
            "test result: ok. 5 passed; 0 failed"
        );
    }

    #[test]
    fn test_bash_smart_summary_cargo_test_failed() {
        let out =
            "running 3 tests\ntest b ... FAILED\ntest result: FAILED. 2 passed; 1 failed";
        assert_eq!(
            bash_smart_summary(out),
            "test result: FAILED. 2 passed; 1 failed"
        );
    }

    #[test]
    fn test_bash_smart_summary_cargo_build_finished() {
        let out = "   Compiling foo v1.0\n    Finished `dev` profile in 3s";
        assert!(bash_smart_summary(out).contains("Finished"));
    }

    #[test]
    fn test_bash_smart_summary_cargo_build_error() {
        let out =
            "error[E0308]: mismatched types\n --> src/main.rs:5:1\nerror: could not compile";
        let s = bash_smart_summary(out);
        assert!(
            s.contains("could not compile") || s.contains("error["),
            "got: {}",
            s
        );
    }

    #[test]
    fn test_bash_smart_summary_fallback_last_line() {
        let out = "line one\nline two\nmost meaningful";
        assert_eq!(bash_smart_summary(out), "most meaningful");
    }

    #[test]
    fn test_bash_smart_summary_strips_ansi_codes() {
        let out = "\x1b[32mtest result: ok. 1 passed; 0 failed\x1b[0m";
        assert!(
            bash_smart_summary(out).contains("test result:"),
            "ANSI codes should be stripped: {:?}",
            bash_smart_summary(out)
        );
    }

    #[test]
    fn test_bash_smart_summary_empty() {
        assert_eq!(bash_smart_summary(""), "");
        assert_eq!(bash_smart_summary("   "), "");
    }

    #[test]
    fn test_bash_smart_summary_test_result_beats_finished() {
        let out =
            "    Finished test profile in 2s\nrunning 3 tests\ntest result: ok. 3 passed";
        assert!(
            bash_smart_summary(out).starts_with("test result:"),
            "test result should beat Finished: {}",
            bash_smart_summary(out)
        );
    }
}
