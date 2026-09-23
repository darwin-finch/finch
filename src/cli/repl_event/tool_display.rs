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

use crossterm::style::{Attribute, Color, SetAttribute, SetForegroundColor};
use serde_json::Value;

const CYAN: SetForegroundColor = SetForegroundColor(Color::Cyan);
const GRAY: SetForegroundColor = SetForegroundColor(Color::DarkGrey);
const BOLD: SetAttribute = SetAttribute(Attribute::Bold);
const RESET: SetAttribute = SetAttribute(Attribute::Reset);

const MAX_PARAM_LEN: usize = 60;

/// Format a tool label like "Bash(git push)" or "Read(src/file.rs)"
pub fn format_tool_label(name: &str, input: &Value) -> String {
    let safe_name = crate::cli::diff::sanitize_terminal(name);
    let key_param = crate::cli::diff::sanitize_terminal(&extract_key_param(name, input));
    if key_param.is_empty() {
        format!("{}{}{}{}", CYAN, BOLD, safe_name, RESET)
    } else {
        format!(
            "{}{}{}{}{}({}){}",
            CYAN,
            BOLD,
            safe_name,
            RESET,
            GRAY,
            truncate(&key_param, MAX_PARAM_LEN),
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
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max_len).collect::<String>())
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

    let lower = tool_name.to_lowercase();
    let structured_files = (matches!(lower.as_str(), "edit" | "write")
        && trimmed.lines().any(|line| {
            line.starts_with("--- ")
                || line.starts_with("diff --git ")
                || line.starts_with("Binary files ")
        }))
    .then(|| crate::cli::diff::FileDiff::parse_all(trimmed));
    if let Some(files) = structured_files.filter(|files| !files.is_empty()) {
        let summary = crate::cli::diff::summarize_files(&files);
        // Keep the complete bounded unified representation. WorkUnit owns
        // final rendering and must be able to re-render retained rows.
        let first_header = trimmed.lines().position(|line| {
            line.starts_with("--- ")
                || line.starts_with("diff --git ")
                || line.starts_with("Binary files ")
        });
        let mut body: Vec<String> = first_header
            .into_iter()
            .flat_map(|end| trimmed.lines().take(end).take(20))
            .map(crate::cli::diff::sanitize_terminal)
            .collect();
        for file in files {
            body.extend(file.to_unified().lines().map(str::to_owned));
        }
        return (summary, body);
    }

    match lower.as_str() {
        "edit" => {
            let mut lines_iter = trimmed.lines();
            let summary = lines_iter.next().unwrap_or("").trim().to_string();
            let body_lines: Vec<String> = lines_iter.map(|l| l.to_string()).collect();
            let total = body_lines.len();
            let mut body: Vec<String> = body_lines.into_iter().take(MAX_TOOL_BODY_LINES).collect();
            if total > MAX_TOOL_BODY_LINES {
                body.push(format!(
                    "{GRAY}… +{} lines (ctrl+o to expand){RESET}",
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
            let total = lines.len();
            let mut body: Vec<String> = lines.iter().take(8).map(|l| l.to_string()).collect();
            if total > 8 {
                body.push(format!(
                    "{GRAY}… +{} more (ctrl+o to expand){RESET}",
                    total - 8
                ));
            }
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
                    "{GRAY}… +{} more (ctrl+o to expand){RESET}",
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
                    "{GRAY}… +{} lines (ctrl+o to expand){RESET}",
                    total - MAX_TOOL_BODY_LINES
                ));
            }
            (summary, body)
        }

        _ => (compact_tool_summary(content), Vec::new()),
    }
}

/// Build the approval preview for a mutating file tool using the same bounded,
/// sanitized renderer as completed transcript rows.
pub(crate) fn tool_approval_diff_preview(
    tool_use: &crate::tools::ToolUse,
    colors: &crate::theme::ColorScheme,
    mode: crate::cli::diff::DiffColorMode,
) -> Option<String> {
    let path = tool_use.input.get("file_path")?.as_str()?;
    let diff = match tool_use.name.to_lowercase().as_str() {
        "write" => {
            let after = tool_use.input.get("content")?.as_str()?;
            let (before, was_missing) = match std::fs::read_to_string(path) {
                Ok(value) => (value, false),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (String::new(), true),
                Err(_) => return None,
            };
            if was_missing {
                crate::cli::diff::FileDiff::from_created(path, after)
            } else {
                crate::cli::diff::FileDiff::from_texts(path, &before, after)
            }
        }
        "edit" => {
            let old_string = tool_use.input.get("old_string")?.as_str()?;
            let new_string = tool_use.input.get("new_string")?.as_str()?;
            let replace_all = tool_use
                .input
                .get("replace_all")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let before = std::fs::read_to_string(path).ok()?;
            let matches = before.matches(old_string).count();
            if matches == 0 || (matches > 1 && !replace_all) {
                return None;
            }
            let after = if replace_all {
                before.replace(old_string, new_string)
            } else {
                before.replacen(old_string, new_string, 1)
            };
            crate::cli::diff::FileDiff::from_texts(path, &before, &after)
        }
        _ => return None,
    };
    Some(diff.render(colors, mode))
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

// ── Session task-list rendering ──────────────────────────────────────────────

/// Longest task content shown per list line before truncation.
const MAX_TODO_CONTENT_LEN: usize = 120;

/// Cap on task-list lines rendered into a transcript row body.
const MAX_TODO_LIST_LINES: usize = 40;

/// Whether a tool or approval subject is the session task-list write, in any
/// casing providers have used (`todo_write`, `TodoWrite`).
fn is_todo_write(name: &str) -> bool {
    let normalized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    normalized == "todowrite"
}

/// Human wording for a task status, from the wire value
/// (`"in_progress"` → `"in progress"`).
fn todo_status_word(status: Option<&str>) -> String {
    match status.unwrap_or("pending") {
        "in_progress" => "in progress".to_string(),
        other => other.to_string(),
    }
}

/// Render a `todo_write` input as the readable task list it represents.
///
/// Returns the row summary (a count header) and one line per task, so a
/// transcript row shows the list rather than the tool's raw JSON (#425).
/// `None` when the input is not a well-formed todo payload.
pub(crate) fn todo_write_display(input: &Value) -> Option<(String, Vec<String>)> {
    let todos = input.get("todos")?.as_array()?;
    let count = |status: &str| {
        todos
            .iter()
            .filter(|task| task["status"].as_str() == Some(status))
            .count()
    };
    let (in_progress, pending, completed) =
        (count("in_progress"), count("pending"), count("completed"));
    let noun = if todos.len() == 1 { "task" } else { "tasks" };
    let summary = format!(
        "Task list — {} {noun}: {} in progress, {} pending, {} completed",
        todos.len(),
        in_progress,
        pending,
        completed
    );
    let mut lines: Vec<String> = todos
        .iter()
        .map(|task| {
            let content = task["content"].as_str().unwrap_or("(untitled)");
            format!(
                "[{}] {}",
                todo_status_word(task["status"].as_str()),
                crate::cli::diff::sanitize_terminal(&truncate(content, MAX_TODO_CONTENT_LEN))
            )
        })
        .collect();
    if lines.len() > MAX_TODO_LIST_LINES {
        let overflow = lines.len() - MAX_TODO_LIST_LINES;
        lines.truncate(MAX_TODO_LIST_LINES);
        lines.push(format!("… +{overflow} more tasks"));
    }
    Some((summary, lines))
}

/// Label and body for a transcript row rendering a Brain-projected tool call.
///
/// The task-list write names itself by what it wrote, with the list as the
/// row body; every other tool keeps its readable `Tool(param)` label. Tool
/// input is never spliced into the label as raw JSON (#425).
pub(crate) fn brain_tool_call_row(name: &str, input: &Value) -> (String, Vec<String>) {
    if is_todo_write(name) {
        if let Some(display) = todo_write_display(input) {
            return display;
        }
    }
    (format_tool_label(name, input), Vec::new())
}

/// Body lines for a transcript approval row: the approval detail rendered as
/// the thing it represents. A task-list approval shows the list; any other
/// detail stays out of the transcript (it is inspectable in the approval
/// dialog) rather than printing as raw JSON (#425).
pub(crate) fn approval_detail_body(subject: &str, detail: &Value) -> Vec<String> {
    if is_todo_write(subject) {
        let input = detail.get("input").unwrap_or(detail);
        if let Some((_, lines)) = todo_write_display(input) {
            return lines;
        }
    }
    Vec::new()
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
    use crate::cli::messages::{Message, WorkUnit};

    #[test]
    fn file_approval_uses_shared_sanitized_diff_renderer() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "old\n").unwrap();
        let tool = crate::tools::ToolUse::new(
            "edit".into(),
            serde_json::json!({
                "file_path": file.path(),
                "old_string": "old\n",
                "new_string": "new\n"
            }),
        );
        let dialog = tool_approval_dialog(
            &tool,
            "File: src/\u{1b}[31mhostile.rs",
            &finch_theme::ColorTheme::Dark.to_scheme(),
            finch_diff::DiffColorMode::NoColor,
        );
        let body = dialog.body.as_deref().unwrap();
        assert!(body.contains(file.path().to_string_lossy().as_ref()));
        assert!(body.contains("- old"));
        assert!(body.contains("+ new"));
        assert!(!dialog.title.contains('\u{1b}'));
        assert!(!body.contains('\u{1b}'));
    }

    #[test]
    fn file_approval_preview_composes_with_light_and_dark_themes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "old\n").unwrap();
        let tool = crate::tools::ToolUse::new(
            "edit".into(),
            serde_json::json!({
                "file_path": file.path(),
                "old_string": "old\n",
                "new_string": "new\n"
            }),
        );
        let dark = tool_approval_dialog(
            &tool,
            "File: src/theme.rs",
            &finch_theme::ColorTheme::Dark.to_scheme(),
            finch_diff::DiffColorMode::Theme,
        );
        let light = tool_approval_dialog(
            &tool,
            "File: src/theme.rs",
            &finch_theme::ColorTheme::Light.to_scheme(),
            finch_diff::DiffColorMode::Theme,
        );
        let dark_body = dark.body.unwrap();
        let light_body = light.body.unwrap();
        assert_ne!(dark_body, light_body);
        assert!(
            dark_body.contains("48;2;20;72;40") && dark_body.contains("38;2;236;246;238"),
            "dark approval diffs must fill add rows; body={dark_body}"
        );
        assert!(
            light_body.contains("48;2;204;240;214") && light_body.contains("38;2;12;56;28"),
            "light approval diffs must fill add rows; body={light_body}"
        );
    }

    #[test]
    fn write_approval_summarises_instead_of_dumping_html() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("docs.html");
        let html = format!(
            "<!DOCTYPE html>{}",
            " <div class=\"doc\">page content</div>".repeat(800)
        );
        let tool = crate::tools::ToolUse::new(
            "write".into(),
            serde_json::json!({
                "file_path": path.to_string_lossy(),
                "content": html
            }),
        );
        let summary = crate::cli::repl_event::event_loop::tool_approval_summary(&tool);
        assert!(
            !summary.contains("<!DOCTYPE") && !summary.contains("page content"),
            "write approval must summarise, not dump the file: {summary:?}"
        );
        assert!(
            summary.contains("docs.html") && summary.contains("create"),
            "write approval must lead with path and created-vs-overwritten: {summary:?}"
        );
        assert!(
            summary.contains("KB") || summary.contains("bytes") || summary.contains("MB"),
            "write approval must include a byte count: {summary:?}"
        );
        let dialog = tool_approval_dialog(
            &tool,
            &summary,
            &finch_theme::ColorScheme::default(),
            finch_diff::DiffColorMode::NoColor,
        );
        assert!(
            dialog.body.is_some(),
            "full content must remain reachable behind the body disclosure"
        );
    }

    // ── session task-list rendering (#425) ──────────────────────────────────

    fn issue_425_payload() -> serde_json::Value {
        serde_json::json!({"todos": [
            {"content": "Identify the harness implementation, website source, and deployment target",
             "id": "1", "priority": "high", "status": "in_progress"},
            {"content": "Implement the typed program runner in the harness",
             "id": "2", "priority": "high", "status": "pending"},
            {"content": "Verify the deployment target accepts the build",
             "id": "3", "priority": "low", "status": "completed"}
        ]})
    }

    #[test]
    fn test_todo_write_display_renders_the_issue_425_payload_as_a_task_list() {
        let input = issue_425_payload();
        let (summary, lines) =
            todo_write_display(&input).expect("a well-formed todo payload must render");
        assert!(
            summary.contains("Task list — 3 tasks: 1 in progress, 1 pending, 1 completed"),
            "the header must count every status: summary={summary:?} lines={lines:?}"
        );
        assert_eq!(
            lines[0],
            "[in progress] Identify the harness implementation, website source, and deployment target",
            "each task is one bracketed-status line: lines={lines:?}"
        );
        assert!(
            lines[1].contains("[pending] Implement the typed program runner in the harness"),
            "lines={lines:?}"
        );
        assert!(
            lines[2].contains("[completed] Verify the deployment target accepts the build"),
            "lines={lines:?}"
        );
        let rendered = format!("{summary}\n{}", lines.join("\n"));
        assert!(
            !rendered.contains('{') && !rendered.contains("\"todos\""),
            "the rendered list must not contain raw JSON: {rendered} payload={}",
            input
        );
    }

    #[test]
    fn test_brain_tool_call_row_names_the_task_list_instead_of_raw_json() {
        let (label, body) = brain_tool_call_row("todo_write", &issue_425_payload());
        assert!(
            label.contains("Task list") && !label.contains('{'),
            "label must name the list, not the JSON: {label:?} body={body:?}"
        );
        assert!(
            !body.is_empty(),
            "the list lines are the row body: {body:?}"
        );
    }

    #[test]
    fn test_brain_tool_call_row_accepts_provider_casing() {
        let (label, _) = brain_tool_call_row("TodoWrite", &issue_425_payload());
        assert!(
            label.contains("Task list"),
            "TodoWrite is the same session task-list write: {label:?}"
        );
    }

    #[test]
    fn test_brain_tool_call_row_keeps_readable_label_for_other_tools() {
        let (label, body) =
            brain_tool_call_row("bash", &serde_json::json!({"command": "git status"}));
        assert!(
            label.contains("bash") && label.contains("git status"),
            "other tools keep their readable label: {label:?}"
        );
        assert!(
            body.is_empty(),
            "no spliced JSON body for other tools: {body:?}"
        );
    }

    #[test]
    fn test_brain_tool_call_row_falls_back_when_todo_payload_is_malformed() {
        let (label, body) = brain_tool_call_row("todo_write", &serde_json::json!({"todos": 3}));
        assert!(
            !label.contains('{'),
            "a malformed payload must not splice JSON either: {label:?}"
        );
        assert!(body.is_empty(), "no list can be rendered: {body:?}");
    }

    #[test]
    fn test_approval_detail_body_renders_the_list_not_the_detail_json() {
        let detail = serde_json::json!({"input": issue_425_payload()});
        let body = approval_detail_body("todo_write", &detail);
        assert!(
            body.iter().any(|line| line.contains("[in progress] Identify the harness implementation, website source, and deployment target")),
            "the approval row body must be the task list: {body:?}"
        );
        assert!(
            !body.iter().any(|line| line.contains('{')),
            "the approval row body must not contain raw JSON: {body:?}"
        );
    }

    #[test]
    fn test_approval_detail_body_stays_out_of_transcript_for_other_tools() {
        let body = approval_detail_body("bash", &serde_json::json!({"input": {"command": "true"}}));
        assert!(
            body.is_empty(),
            "non-list approval details must not print as JSON rows: {body:?}"
        );
    }

    #[test]
    fn test_todo_write_display_caps_a_very_long_task_content() {
        let input = serde_json::json!({"todos": [{
            "content": "x".repeat(MAX_TODO_CONTENT_LEN + 50),
            "id": "1", "priority": "low", "status": "pending"
        }]});
        let (summary, lines) = todo_write_display(&input).unwrap();
        assert!(
            summary.contains("1 task: 0 in progress, 1 pending, 0 completed"),
            "singular noun and zero counts: {summary:?}"
        );
        assert!(
            lines[0].chars().count() <= MAX_TODO_CONTENT_LEN + "[pending] ".len() + 1,
            "content must be truncated: {lines:?}"
        );
    }

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
        assert!(
            label.contains(&format!("{}(git status){}", GRAY, RESET)),
            "both parentheses and the argument should use one color: {label:?}"
        );
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
    fn test_edit_and_write_tool_display_payloads_survive_retained_work_unit() {
        for tool in ["edit", "write"] {
            let raw = finch_diff::FileDiff::from_texts("src/file.txt", "old\n", "new\nmore\n")
                .to_unified();
            let (summary, body) = tool_result_to_display(tool, &raw);
            let wu = WorkUnit::new("Tools");
            let row = wu.add_row(format!("{tool}(src/file.txt)"));
            wu.complete_row_with_body(row, summary, body);
            wu.set_complete();
            let rendered = wu.format(&finch_theme::ColorScheme::default());
            assert!(rendered.contains("src/file.txt  +2 -1"), "{rendered}");
            assert!(rendered.contains("- old"), "{rendered}");
            assert!(rendered.contains("+ new"), "{rendered}");
            assert!(!rendered.contains("\x1b]"), "{rendered}");
        }
    }

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
    fn test_tool_result_edit_structured_diff_reaches_direct_display_untruncated() {
        let diff =
            crate::cli::diff::FileDiff::from_texts("src/é.rs", "old\n", "new\nmore\n").to_unified();
        let (summary, body) = tool_result_to_display("edit", &diff);
        assert_eq!(summary, "src/é.rs  +2 -1");
        assert!(body.iter().any(|line| line == "+more"));
        assert!(!body.iter().any(|line| line.contains("ctrl+o")));
    }

    #[test]
    fn test_tool_result_write_structured_diff_keeps_body_and_sanitizes_preamble() {
        let diff = crate::cli::diff::FileDiff::from_created("out.txt", "hello\n").to_unified();
        let content = format!("\x1b]8;;bad\x07Created\n{diff}");
        let (summary, body) = tool_result_to_display("write", &content);
        assert_eq!(summary, "out.txt  +1 -0");
        assert_eq!(body.first().map(String::as_str), Some("Created"));
        assert_eq!(
            body.iter()
                .find(|line| line.starts_with("--- "))
                .map(String::as_str),
            Some("--- /dev/null")
        );
        assert!(body.iter().any(|line| line == "+hello"));
    }

    #[test]
    fn test_tool_result_preserves_stdout_before_standard_git_diff() {
        let content = "approved script stdout\ndiff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n";
        let (summary, body) = tool_result_to_display("edit", content);
        assert_eq!(summary, "x  +1 -1");
        assert_eq!(
            body.first().map(String::as_str),
            Some("approved script stdout")
        );
        assert!(body.iter().any(|line| line == "+new"));
    }

    #[test]
    fn test_format_tool_label_non_ascii_truncation_does_not_panic() {
        let label = format_tool_label("bash", &serde_json::json!({"command": "é".repeat(80)}));
        assert!(label.contains('…'));
    }

    #[test]
    fn test_format_tool_label_sanitizes_hostile_path_controls() {
        let label = format_tool_label(
            "write\x1b]8;;tool\x07",
            &serde_json::json!({"file_path": "src/\x1b[31mé.rs"}),
        );
        assert!(!label.contains("\x1b]"));
        assert!(!label.contains("\x1b[31m"));
        assert!(label.contains('é'));
    }

    #[test]
    fn test_write_approval_preview_preserves_existing_read_errors() {
        let directory = tempfile::tempdir().unwrap();
        let tool = crate::tools::ToolUse::new(
            "write".into(),
            serde_json::json!({
                "file_path": directory.path(),
                "content": "replacement"
            }),
        );
        assert!(tool_approval_diff_preview(
            &tool,
            &crate::theme::ColorScheme::default(),
            crate::cli::diff::DiffColorMode::NoColor,
        )
        .is_none());
    }

    #[test]
    fn test_write_approval_dialog_byte_count_thresholds() {
        assert_eq!(format_byte_count(0), "0 bytes");
        assert_eq!(format_byte_count(1), "1 byte");
        assert_eq!(format_byte_count(512), "512 bytes");
        assert_eq!(format_byte_count(12 * 1024), "12 KB");
        assert_eq!(format_byte_count(1536), "1.5 KB");
    }

    #[test]
    fn test_write_approval_dialog_summary_create_vs_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let created = directory.path().join("new.txt");
        let created_summary = write_approval_summary(created.to_str().unwrap(), "hello");
        assert!(
            created_summary.contains("create")
                && created_summary.contains("new.txt")
                && created_summary.contains("5 bytes"),
            "{created_summary}"
        );
        let existing = directory.path().join("old.txt");
        std::fs::write(&existing, "old").unwrap();
        let summary = write_approval_summary(existing.to_str().unwrap(), "replacement");
        assert!(
            summary.contains("overwrite") && summary.contains("replacing existing content"),
            "{summary}"
        );
        assert!(summary.contains("11 bytes"), "{summary}");
    }

    #[test]
    fn test_write_approval_preview_marks_missing_file_as_created() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("new.txt");
        let tool = crate::tools::ToolUse::new(
            "write".into(),
            serde_json::json!({
                "file_path": path,
                "content": "created\n"
            }),
        );
        let rendered = tool_approval_diff_preview(
            &tool,
            &crate::theme::ColorScheme::default(),
            crate::cli::diff::DiffColorMode::NoColor,
        )
        .unwrap();
        assert!(rendered.contains("created"), "{rendered}");
        assert!(!rendered.contains("renamed"), "{rendered}");
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
        assert_eq!(body.len(), 9, "body should be 8 paths + overflow hint");
        assert!(body.last().unwrap().contains("ctrl+o to expand"));
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
        let content = "STDERR:\nls: cannot access '/nope': No such file or directory\nExit code: 2";
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
        let out = "running 3 tests\ntest b ... FAILED\ntest result: FAILED. 2 passed; 1 failed";
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
        let out = "error[E0308]: mismatched types\n --> src/main.rs:5:1\nerror: could not compile";
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
        let out = "    Finished test profile in 2s\nrunning 3 tests\ntest result: ok. 3 passed";
        assert!(
            bash_smart_summary(out).starts_with("test result:"),
            "test result should beat Finished: {}",
            bash_smart_summary(out)
        );
    }
}

/// Human-readable size for a write/edit approval summary.
pub(crate) fn format_byte_count(n: usize) -> String {
    const KB: usize = 1024;
    const MB: usize = 1024 * 1024;
    if n >= MB {
        let mb = n as f64 / MB as f64;
        if mb >= 10.0 {
            format!("{} MB", n / MB)
        } else {
            format!("{mb:.1} MB")
        }
    } else if n >= KB {
        let kb = n as f64 / KB as f64;
        if kb >= 10.0 {
            format!("{} KB", n / KB)
        } else {
            format!("{kb:.1} KB")
        }
    } else if n == 1 {
        "1 byte".to_string()
    } else {
        format!("{n} bytes")
    }
}

/// Decision-relevant write summary: path, size, created vs overwritten.
///
/// The literal file bytes belong in the scrollable dialog body, not here.
pub(crate) fn write_approval_summary(path: &str, content: &str) -> String {
    let display_path = shorten_path(path);
    let size = format_byte_count(content.len());
    if std::path::Path::new(path).exists() {
        format!("overwrite {display_path}, {size}, replacing existing content")
    } else {
        format!("create {display_path}, {size}")
    }
}

/// Decision-relevant edit summary: path and replacement size.
pub(crate) fn edit_approval_summary(path: &str, new_string: &str) -> String {
    format!(
        "edit {}, {}",
        shorten_path(path),
        format_byte_count(new_string.len())
    )
}

/// Bounded plaintext preview used when a structured diff cannot be built.
fn bounded_write_preview(content: &str) -> String {
    const MAX_LINES: usize = 16;
    const MAX_CHARS: usize = 512;
    let total = content.lines().count();
    let mut lines: Vec<String> = content
        .lines()
        .take(MAX_LINES)
        .map(|line| {
            if line.chars().count() > MAX_CHARS {
                format!("{}…", line.chars().take(MAX_CHARS).collect::<String>())
            } else {
                line.to_string()
            }
        })
        .collect();
    if total > MAX_LINES {
        lines.push(format!(
            "… +{} lines (Ctrl-U/D or PgUp/PgDn to inspect)",
            total - MAX_LINES
        ));
    }
    crate::cli::diff::sanitize_multiline(&lines.join("\n"))
}

/// Build a file-tool approval dialog from a tool call.
///
/// This lives here, not on `Dialog`, because it is assembly from Finch's own vocabulary: a
/// `ToolUse` and a diff colour mode. The terminal framework offers `Dialog::tool_approval` and
/// `with_body`, and knows nothing about tools.
pub fn tool_approval_dialog(
    tool_use: &crate::tools::ToolUse,
    summary: &str,
    colors: &crate::theme::ColorScheme,
    mode: crate::cli::diff::DiffColorMode,
) -> crate::cli::tui::Dialog {
    let mut dialog = crate::cli::tui::Dialog::tool_approval(&tool_use.name, summary);
    if let Some(preview) = tool_approval_diff_preview(tool_use, colors, mode) {
        // Assign directly rather than through `with_body`, which sanitizes. FileDiff has already
        // sanitized the untrusted content and then applied its own SGR theme sequences; sanitizing
        // again strips those, and the preview renders identically in every colour mode.
        dialog.body = Some(preview);
    } else if tool_use.name.eq_ignore_ascii_case("write") {
        if let Some(content) = tool_use.input.get("content").and_then(Value::as_str) {
            dialog.body = Some(bounded_write_preview(content));
        }
    }
    dialog
}

/// Production assembly: compact summary plus a themed, scrollable preview.
pub(crate) fn assemble_tool_approval(
    tool_use: &crate::tools::ToolUse,
    summary: &str,
) -> crate::cli::tui::Dialog {
    tool_approval_dialog(
        tool_use,
        summary,
        &crate::theme::ColorScheme::default(),
        crate::cli::diff::DiffColorMode::production(),
    )
}

/// One Yes/No dialog for consecutive write/edit/patch proposals.
///
/// The body is the aggregate unified diff in apply order. Partial accept is
/// out of scope (#433): Yes applies every call, No applies none.
pub(crate) fn assemble_changeset_approval(
    tools: &[crate::tools::ToolUse],
    summary: &str,
) -> crate::cli::tui::Dialog {
    assemble_changeset_approval_with(
        tools,
        summary,
        &crate::theme::ColorScheme::default(),
        crate::cli::diff::DiffColorMode::production(),
    )
}

fn assemble_changeset_approval_with(
    tools: &[crate::tools::ToolUse],
    summary: &str,
    colors: &crate::theme::ColorScheme,
    mode: crate::cli::diff::DiffColorMode,
) -> crate::cli::tui::Dialog {
    let title = format!("changeset ({})\n{}", tools.len(), summary);
    let mut dialog = crate::cli::tui::Dialog::select(
        title,
        vec![
            crate::cli::tui::DialogOption::new("1. Yes"),
            crate::cli::tui::DialogOption::new("2. No"),
        ],
    );
    let planned = super::changeset::planned_changeset(tools);
    let mut body = crate::cli::diff::render_files(&planned.diffs, colors, mode);
    if !planned.notes.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&crate::cli::diff::sanitize_multiline(
            &planned.notes.join("\n"),
        ));
    }
    dialog.body = Some(body);
    dialog
}
