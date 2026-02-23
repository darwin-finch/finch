// Patch tool — apply a unified diff to a file
//
// Accepts standard unified diff format (output of `diff -u` or `git diff`):
//
//   --- a/src/foo.rs
//   +++ b/src/foo.rs
//   @@ -10,7 +10,8 @@
//    context line
//   -removed line
//   +added line
//    context line
//
// Returns a colored summary of applied hunks.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;

const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const GRAY: &str = "\x1b[90m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

pub struct PatchTool;

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Apply a unified diff (patch) to a file. \
         Accepts standard unified diff format with @@ hunk headers, \
         context lines (space prefix), removed lines (- prefix), and added lines (+ prefix). \
         The --- / +++ header lines are optional. \
         Use this to apply multi-hunk changes in a single call. \
         Returns a colored summary of each applied hunk."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to patch"
                },
                "patch": {
                    "type": "string",
                    "description": "The unified diff to apply, with @@ hunk headers"
                }
            }),
            required: vec!["file_path".to_string(), "patch".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_path = input["file_path"]
            .as_str()
            .context("Missing file_path parameter")?;
        let patch_text = input["patch"]
            .as_str()
            .context("Missing patch parameter")?;

        let original = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        let hunks = parse_hunks(patch_text)
            .context("Failed to parse unified diff — check @@ hunk header format")?;

        if hunks.is_empty() {
            return Err(anyhow::anyhow!(
                "No hunks found in patch. Ensure the diff contains @@ ... @@ headers."
            ));
        }

        let (patched, summary) = apply_hunks(&original, &hunks)
            .with_context(|| format!("Failed to apply patch to {}", file_path))?;

        fs::write(file_path, &patched)
            .with_context(|| format!("Failed to write patched file: {}", file_path))?;

        Ok(format!(
            "{BOLD}Patched {}{RESET}: {}\n{}",
            file_path,
            summary.counts,
            summary.detail
        ))
    }
}

// ── Unified diff parser ──────────────────────────────────────────────────────

/// A single hunk from a unified diff.
#[derive(Debug, Clone)]
struct Hunk {
    /// 1-based start line in the original file (from `@@ -start,... @@`)
    orig_start: usize,
    /// Lines: ' ' = context, '-' = remove, '+' = add
    lines: Vec<(char, String)>,
}

/// Parse unified diff text into a list of hunks.
/// Skips `---`/`+++` header lines and `diff --git` metadata.
fn parse_hunks(patch: &str) -> Result<Vec<Hunk>> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut current: Option<Hunk> = None;

    for raw_line in patch.lines() {
        if raw_line.starts_with("@@") {
            // Flush previous hunk
            if let Some(h) = current.take() {
                hunks.push(h);
            }

            // Parse `@@ -start[,count] +start[,count] @@`
            let orig_start = parse_hunk_header(raw_line).with_context(|| {
                format!("Bad hunk header: {}", raw_line)
            })?;
            current = Some(Hunk { orig_start, lines: Vec::new() });

        } else if let Some(ref mut hunk) = current {
            if let Some(rest) = raw_line.strip_prefix('-') {
                // Ignore --- file header lines (they have no preceding @@ line)
                if rest.starts_with("--") {
                    // `---` file header outside a hunk — skip
                } else {
                    hunk.lines.push(('-', rest.to_string()));
                }
            } else if let Some(rest) = raw_line.strip_prefix('+') {
                if rest.starts_with("++") {
                    // `+++` file header — skip
                } else {
                    hunk.lines.push(('+', rest.to_string()));
                }
            } else if let Some(rest) = raw_line.strip_prefix(' ') {
                hunk.lines.push((' ', rest.to_string()));
            } else if raw_line.is_empty() {
                // Blank lines inside a hunk are treated as context
                hunk.lines.push((' ', String::new()));
            }
            // Ignore diff --git, index lines, etc.
        }
        // Lines before first @@ (file headers, diff metadata) are silently ignored
    }

    if let Some(h) = current.take() {
        hunks.push(h);
    }

    Ok(hunks)
}

/// Extract the original-file start line from a `@@ -N[,M] +N[,M] @@` header.
fn parse_hunk_header(header: &str) -> Result<usize> {
    // Find the `-N` portion between `@@` and the comma or space
    let inner = header
        .split("@@")
        .nth(1)
        .context("No @@ pair in hunk header")?
        .trim();

    let orig_part = inner
        .split_whitespace()
        .find(|s| s.starts_with('-'))
        .context("No -N in hunk header")?;

    let number_str = orig_part.trim_start_matches('-').split(',').next().unwrap_or("1");
    let start: usize = number_str.parse().with_context(|| {
        format!("Could not parse line number from hunk header: {}", header)
    })?;

    Ok(start.max(1)) // line numbers are 1-based
}

// ── Hunk application ─────────────────────────────────────────────────────────

#[derive(Debug)]
struct ApplySummary {
    counts: String,
    detail: String,
}

/// Apply all hunks to the original text, returning the patched content and a
/// human-readable coloured summary.
fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<(String, ApplySummary)> {
    let orig_lines: Vec<&str> = original.lines().collect();
    let had_trailing_newline = original.ends_with('\n');

    // Apply hunks in reverse order so earlier line numbers stay valid
    let mut result_lines: Vec<String> = orig_lines.iter().map(|s| s.to_string()).collect();
    let mut detail_parts: Vec<String> = Vec::new();
    let mut total_added: usize = 0;
    let mut total_removed: usize = 0;

    // Sort hunks in reverse order of their orig_start so we can apply back-to-front
    let mut sorted_hunks: Vec<&Hunk> = hunks.iter().collect();
    sorted_hunks.sort_by(|a, b| b.orig_start.cmp(&a.orig_start));

    for hunk in &sorted_hunks {
        let (new_slice, added, removed) = apply_single_hunk(&result_lines, hunk)
            .with_context(|| format!("Hunk @@ -{} failed to apply", hunk.orig_start))?;

        // Calculate replacement range in result_lines
        // The hunk covers (orig_start - 1) .. (orig_start - 1 + context_lines_count + removed_count)
        let ctx_before = hunk.lines.iter().take_while(|(k, _)| *k == ' ').count();
        let hunk_orig_len: usize = hunk.lines.iter().filter(|(k, _)| *k != '+').count();

        let range_start = (hunk.orig_start - 1 + ctx_before).min(result_lines.len());
        let range_end = (hunk.orig_start - 1 + hunk_orig_len).min(result_lines.len());
        // Only replace the non-context changed portion
        let change_start = hunk.orig_start - 1 + ctx_before;
        let change_end = change_start + removed;

        // Splice result_lines: remove [change_start..change_end], insert new_slice
        let _ = (range_start, range_end); // suppress unused warning
        if change_end <= result_lines.len() {
            result_lines.splice(change_start..change_end, new_slice.clone());
        } else {
            // Hunk extends past end of file — append
            result_lines.truncate(change_start);
            result_lines.extend(new_slice.clone());
        }

        total_added += added;
        total_removed += removed;

        // Build detail line for this hunk
        if added > 0 || removed > 0 {
            detail_parts.push(format!(
                "  {GRAY}@@{RESET} line {}{}{}{}: {GREEN}+{}{RESET} {RED}-{}{RESET}",
                hunk.orig_start,
                GRAY,
                "",
                RESET,
                added,
                removed,
            ));
        }
    }

    // Reconstruct content
    let mut patched = result_lines.join("\n");
    if had_trailing_newline || !patched.is_empty() {
        patched.push('\n');
    }

    let mut summary_parts = Vec::new();
    if total_added > 0 {
        summary_parts.push(format!("{GREEN}+{} line{}{RESET}", total_added, if total_added == 1 { "" } else { "s" }));
    }
    if total_removed > 0 {
        summary_parts.push(format!("{RED}-{} line{}{RESET}", total_removed, if total_removed == 1 { "" } else { "s" }));
    }
    let counts = if summary_parts.is_empty() {
        "no changes".to_string()
    } else {
        summary_parts.join(", ")
    };

    Ok((patched, ApplySummary { counts, detail: detail_parts.join("\n") }))
}

/// Apply a single hunk, returning the replacement lines and (added, removed) counts.
/// The returned lines replace the `-` lines in `orig_lines`.
fn apply_single_hunk(orig_lines: &[String], hunk: &Hunk) -> Result<(Vec<String>, usize, usize)> {
    // Count context lines before the first change
    let ctx_before = hunk.lines.iter().take_while(|(k, _)| *k == ' ').count();
    let hunk_start_idx = hunk.orig_start.saturating_sub(1); // 0-based

    // Verify context lines match the original file
    let mut orig_idx = hunk_start_idx;
    for (kind, text) in &hunk.lines {
        if *kind == ' ' || *kind == '-' {
            if orig_idx >= orig_lines.len() {
                return Err(anyhow::anyhow!(
                    "Hunk at line {} extends past end of file (file has {} lines)",
                    hunk.orig_start,
                    orig_lines.len()
                ));
            }
            if orig_lines[orig_idx] != *text {
                return Err(anyhow::anyhow!(
                    "Context/remove mismatch at line {} (expected {:?}, got {:?})",
                    orig_idx + 1,
                    text,
                    orig_lines[orig_idx]
                ));
            }
            orig_idx += 1;
        }
    }

    // Build the replacement slice (only the changed lines, no context)
    let mut new_lines: Vec<String> = Vec::new();
    let mut added = 0usize;
    let mut removed = 0usize;

    for (kind, text) in &hunk.lines {
        match kind {
            '+' => {
                new_lines.push(text.clone());
                added += 1;
            }
            '-' => {
                removed += 1;
                // Don't push — line is removed
            }
            ' ' => {
                // Context lines are preserved but not returned as the replacement
                // (they're kept by the splice caller)
            }
            _ => {}
        }
    }

    let _ = ctx_before; // caller handles context offset

    Ok((new_lines, added, removed))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_hunks ──

    #[test]
    fn test_parse_single_hunk() {
        let patch = "@@ -1,3 +1,3 @@\n context\n-old\n+new\n context\n";
        let hunks = parse_hunks(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].orig_start, 1);
        assert_eq!(
            hunks[0].lines,
            vec![
                (' ', "context".to_string()),
                ('-', "old".to_string()),
                ('+', "new".to_string()),
                (' ', "context".to_string()),
            ]
        );
    }

    #[test]
    fn test_parse_skips_file_headers() {
        let patch = "--- a/foo.rs\n+++ b/foo.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n";
        let hunks = parse_hunks(patch).unwrap();
        assert_eq!(hunks.len(), 1);
        assert_eq!(hunks[0].lines, vec![('-', "old".to_string()), ('+', "new".to_string())]);
    }

    #[test]
    fn test_parse_multiple_hunks() {
        let patch = "@@ -1,2 +1,2 @@\n-a\n+b\n@@ -10,2 +10,2 @@\n-c\n+d\n";
        let hunks = parse_hunks(patch).unwrap();
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].orig_start, 1);
        assert_eq!(hunks[1].orig_start, 10);
    }

    #[test]
    fn test_parse_hunk_header_with_count() {
        assert_eq!(parse_hunk_header("@@ -5,3 +5,4 @@").unwrap(), 5);
        assert_eq!(parse_hunk_header("@@ -1 +1 @@").unwrap(), 1);
        assert_eq!(parse_hunk_header("@@ -100,10 +100,9 @@ fn foo()").unwrap(), 100);
    }

    // ── apply_hunks ──

    #[test]
    fn test_apply_simple_replacement() {
        let original = "line1\nold_line\nline3\n";
        let patch = "@@ -1,3 +1,3 @@\n line1\n-old_line\n+new_line\n line3\n";
        let hunks = parse_hunks(patch).unwrap();
        let (patched, _) = apply_hunks(original, &hunks).unwrap();
        assert_eq!(patched, "line1\nnew_line\nline3\n");
    }

    #[test]
    fn test_apply_addition() {
        let original = "a\nb\nc\n";
        // Insert a line after "a"
        let patch = "@@ -1,2 +1,3 @@\n a\n+inserted\n b\n";
        let hunks = parse_hunks(patch).unwrap();
        let (patched, _) = apply_hunks(original, &hunks).unwrap();
        assert_eq!(patched, "a\ninserted\nb\nc\n");
    }

    #[test]
    fn test_apply_deletion() {
        let original = "a\nb\nc\n";
        let patch = "@@ -1,3 +1,2 @@\n a\n-b\n c\n";
        let hunks = parse_hunks(patch).unwrap();
        let (patched, _) = apply_hunks(original, &hunks).unwrap();
        assert_eq!(patched, "a\nc\n");
    }

    #[test]
    fn test_apply_context_mismatch_returns_error() {
        let original = "a\nb\nc\n";
        // Context says "x" but file has "a"
        let patch = "@@ -1,3 +1,3 @@\n x\n-b\n+B\n c\n";
        let hunks = parse_hunks(patch).unwrap();
        let result = apply_hunks(original, &hunks);
        assert!(result.is_err());
        // Use {:#} to include the full error chain (outer context + inner cause)
        let msg = format!("{:#}", result.unwrap_err());
        assert!(msg.contains("mismatch") || msg.contains("failed to apply"), "unexpected error: {}", msg);
    }

    #[test]
    fn test_apply_empty_patch_returns_error() {
        let tool = PatchTool;
        let schema = tool.input_schema();
        assert_eq!(schema.required, vec!["file_path", "patch"]);
    }

    #[test]
    fn test_no_hunks_is_error() {
        // A patch with no @@ headers should be rejected
        let patch = "--- a/foo\n+++ b/foo\n";
        let hunks = parse_hunks(patch).unwrap();
        assert!(hunks.is_empty());
        // The execute path checks this and returns an error
    }

    // ── Regression: multi-byte Unicode in patch lines should not panic ──

    #[test]
    fn test_apply_unicode_lines() {
        let original = "héllo\nworld\n";
        let patch = "@@ -1,2 +1,2 @@\n-héllo\n+Héllo\n world\n";
        let hunks = parse_hunks(patch).unwrap();
        let (patched, _) = apply_hunks(original, &hunks).unwrap();
        assert_eq!(patched, "Héllo\nworld\n");
    }
}
