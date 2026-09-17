// propose.rs — open a script in $EDITOR before executing it.
//
// Flow:
//   1. Write the proposed script (with English comment header) to a temp file.
//   2. Open $EDITOR (blocking — runs in spawn_blocking to not block the async runtime).
//   3. Read back the file.  Whatever the user left in it is what runs.
//   4. Return the (possibly modified) script, or None if the user emptied it.
//
// The English description is embedded as a comment at the top of the script.
// The user can read the comment to understand intent and edit the code before
// approving.  Clearing the file aborts execution.

use crate::cli::diff::{
    FileDiff, MAX_DIFF_HUNKS, MAX_DIFF_INPUT_BYTES, MAX_DIFF_LINES, MAX_DIFF_LINE_CHARS,
};
use anyhow::Result;
use crossterm::{cursor, event, execute, style::ResetColor, terminal};
use std::io::{IsTerminal, Read as _, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tempfile::Builder;
use tokio::task::spawn_blocking;

/// True when a prior REPL grant already covers this tool call.
///
/// The TUI dialog (including "Yes, and don't ask again for: edit:*") and
/// AutoAccept are that grant. Opening `$EDITOR` again after it is what blocked
/// autonomous iteration.
pub fn interactive_review_already_granted(
    skip_interactive_review: bool,
    auto_accepts_host_effects: bool,
) -> bool {
    skip_interactive_review || auto_accepts_host_effects
}

/// True when this tool call must still block in `$EDITOR`.
pub fn should_open_interactive_review(
    skip_interactive_review: bool,
    auto_accepts_host_effects: bool,
) -> bool {
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return false;
    }
    !interactive_review_already_granted(skip_interactive_review, auto_accepts_host_effects)
}

pub async fn context_should_open_interactive_review(
    context: &crate::tools::types::ToolContext<'_>,
) -> bool {
    let auto_accepts = match &context.repl_mode {
        Some(mode) => mode.read().await.auto_accepts_host_effects(),
        None => false,
    };
    should_open_interactive_review(context.skip_interactive_review, auto_accepts)
}

/// Decision encoded in an editor-backed proposal file. The source remains
/// untrusted and must still pass the normal tool/VM authorization path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalDecision {
    Execute { source: String },
    Chat { context: String },
    Cancel,
}

/// User-editable proposal actions. [`parse_proposal_decision`] and the
/// generated artifact header both iterate [`ProposalAction::ALL`], so a new
/// accepted directive cannot exist in the parser without appearing in the
/// file the user edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProposalAction {
    Execute,
    Cancel,
    Chat,
}

impl ProposalAction {
    const ALL: &'static [Self] = &[Self::Execute, Self::Cancel, Self::Chat];

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|action| action.name() == name)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Cancel => "cancel",
            Self::Chat => "chat",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Execute => "run it (default)",
            Self::Cancel => "reject it",
            Self::Chat => "keep it and ask for changes",
        }
    }
}

const PROPOSAL_BODY_MARKER: &str = "---- Finch proposal body ----";

fn proposal_directive(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let directive = trimmed
        .strip_prefix("# finch:")
        .or_else(|| trimmed.strip_prefix("\\ finch:"))
        .or_else(|| trimmed.strip_prefix(";; finch:"))?;
    directive.trim().strip_prefix("action=").map(str::trim)
}

fn is_proposal_body_marker(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed
        .strip_prefix(";;")
        .or_else(|| trimmed.strip_prefix('#'))
        .or_else(|| trimmed.strip_prefix('\\'))
        .map(str::trim)
        == Some(PROPOSAL_BODY_MARKER)
}

/// Read a reserved Finch action directive from the structural header only.
///
/// The body marker is mandatory. Everything below it is data, even when file
/// content happens to contain a line that looks like a Finch directive.
pub fn parse_proposal_decision(content: &str) -> ProposalDecision {
    let mut offset = 0;
    let mut boundary = None;
    for line_with_ending in content.split_inclusive('\n') {
        let line = line_with_ending
            .strip_suffix('\n')
            .unwrap_or(line_with_ending);
        if is_proposal_body_marker(line) {
            boundary = Some((offset, offset + line_with_ending.len()));
            break;
        }
        offset += line_with_ending.len();
    }
    let Some((header_end, body_start)) = boundary else {
        return ProposalDecision::Cancel;
    };
    let header = &content[..header_end];
    let body = &content[body_start..];
    let action = header
        .lines()
        .filter_map(proposal_directive)
        .last()
        .and_then(ProposalAction::from_name);
    match action {
        Some(ProposalAction::Cancel) => ProposalDecision::Cancel,
        Some(ProposalAction::Chat) => ProposalDecision::Chat {
            context: body.to_string(),
        },
        Some(ProposalAction::Execute) if !body.trim().is_empty() => ProposalDecision::Execute {
            source: body.to_string(),
        },
        _ => ProposalDecision::Cancel,
    }
}

/// Return only prose added by the reviewer, excluding the generated proposal
/// artifact and diff framing. This keeps a chat response from looking like an
/// applied file change when rendered in the transcript.
pub fn proposal_chat_context(returned: &str, expected: &str) -> String {
    let generated: Vec<&str> = expected.lines().collect();
    let prose: Vec<String> = returned
        .lines()
        .filter(|line| !generated.contains(line))
        .filter(|line| proposal_directive(line).is_none())
        .filter(|line| !is_proposal_body_marker(line))
        .filter(|line| {
            !line.starts_with("--- ") && !line.starts_with("+++ ") && !line.starts_with("@@ ")
        })
        .map(|line| {
            line.trim_start()
                .trim_start_matches('#')
                .trim_start_matches(['\\', ';'])
                .trim()
                .to_string()
        })
        .filter(|line| !line.is_empty())
        .collect();
    if prose.is_empty() {
        return "(no explanation given)".to_string();
    }
    crate::cli::diff::sanitize_multiline(&prose.join("\n"))
}

/// Reject a bounded terminal diff whose rendered hunks do not faithfully
/// account for the requested before/after content.
///
/// This is the shared interim guard for issue #482. Both Edit and Write must
/// use the same proof before treating a rendered diff as an approval artifact.
pub(crate) fn verify_render_is_faithful(
    operation: &str,
    file_diff: &FileDiff,
    rendered: &str,
    original: &str,
    planned: &str,
) -> Result<()> {
    let interim = "This is an interim refusal for issue #482 (the shared diff renderer can \
                   silently drop part of a change); it is not a problem with the file.";
    if file_diff.binary {
        return Ok(());
    }
    if !file_diff.counts_are_exact() {
        let detail = file_diff.elided.as_deref().unwrap_or("reason not reported");
        anyhow::bail!(
            "Refusing to {operation} {}: the diff is incomplete or elided ({detail}), so the \
             review would hide part of the change. Make a smaller change.\n{}",
            file_diff.display_path(),
            interim
        );
    }
    if rendered
        .lines()
        .any(|line| line == "# finch: diff rendering truncated")
    {
        anyhow::bail!(
            "Refusing to {operation} {}: the diff is too large to display in full, so the \
             review would have shown only part of the change.\nMake a smaller change.\n{}",
            file_diff.display_path(),
            interim
        );
    }
    if rendered.contains("[line truncated]") {
        anyhow::bail!(
            "Refusing to {operation} {}: at least one rendered hunk line exceeds the \
             {}-character display limit, so the review would hide suffix bytes. Make a smaller \
             change.",
            file_diff.display_path(),
            MAX_DIFF_LINE_CHARS
        );
    }

    let mut hunks = 0usize;
    let mut changed_lines = 0usize;
    let mut pending: Option<(usize, usize, usize, usize)> = None;
    let finish = |pending: Option<(usize, usize, usize, usize)>| -> Result<()> {
        if let Some((old_want, new_want, old_seen, new_seen)) = pending {
            if old_want != old_seen || new_want != new_seen {
                anyhow::bail!(
                    "Refusing to {operation} {}: the rendered diff does not match its own hunk \
                     header (it claims {} old and {} new lines but shows {} and {}), so the \
                     review would have hidden part of the change.\n{}",
                    file_diff.display_path(),
                    old_want,
                    new_want,
                    old_seen,
                    new_seen,
                    interim
                );
            }
        }
        Ok(())
    };
    for line in rendered.lines() {
        if let Some(counts) = hunk_counts(line) {
            finish(pending.take())?;
            hunks += 1;
            pending = Some((counts.0, counts.1, 0, 0));
            continue;
        }
        let Some((_, _, old_seen, new_seen)) = pending.as_mut() else {
            continue;
        };
        match line.chars().next() {
            Some(' ') | None => {
                *old_seen += 1;
                *new_seen += 1;
            }
            Some('-') => {
                *old_seen += 1;
                changed_lines += 1;
            }
            Some('+') => {
                *new_seen += 1;
                changed_lines += 1;
            }
            _ => {}
        }
    }
    finish(pending.take())?;

    if original != planned && (hunks == 0 || changed_lines == 0) {
        anyhow::bail!(
            "Refusing to {operation} {}: the change could not be rendered as a reviewable diff, \
             so the review would have shown nothing to approve.\nMake a smaller change.\n{}",
            file_diff.display_path(),
            interim
        );
    }
    Ok(())
}

fn hunk_counts(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (ranges, _) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let count = |range: &str| -> Option<usize> {
        match range.split_once(',') {
            Some((_, n)) => n.parse().ok(),
            None => Some(1),
        }
    };
    Some((count(old)?, count(new)?))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReviewedLineKind {
    Context,
    Add,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReviewedLine {
    kind: ReviewedLineKind,
    text: String,
    no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ReviewedHunk {
    old_start: usize,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    lines: Vec<ReviewedLine>,
}

struct ReviewedPatch {
    old_path: String,
    new_path: String,
    hunks: Vec<ReviewedHunk>,
}

fn reviewed_refusal(operation: &str, path: &str, reason: &str) -> anyhow::Error {
    anyhow::anyhow!("Refusing to {operation} {path}: {reason}. The file was left unchanged.")
}

/// Reconstruct target bytes from a saved unified-diff body.
///
/// The patch is a bounded typed document: it is never executed as a shell
/// script, and its path headers are authenticated against the originally
/// reviewed target rather than trusted as a destination. The two apply
/// methods must agree before any caller may write.
pub(crate) fn reconstruct_reviewed_text(
    operation: &str,
    expected_old_path: &str,
    expected_new_path: &str,
    original: &str,
    saved_diff: &str,
) -> Result<String> {
    if saved_diff.len() > MAX_DIFF_INPUT_BYTES {
        return Err(reviewed_refusal(
            operation,
            expected_new_path,
            "the saved review diff exceeds the bounded review size",
        ));
    }
    let patch = parse_reviewed_patch(operation, expected_new_path, saved_diff)?;
    if patch.old_path != expected_old_path || patch.new_path != expected_new_path {
        return Err(reviewed_refusal(
            operation,
            expected_new_path,
            &format!(
                "the saved review diff changed the target path ({} -> {} became {} -> {})",
                expected_old_path, expected_new_path, patch.old_path, patch.new_path
            ),
        ));
    }
    let stitched = stitch_reviewed_patch(operation, expected_new_path, original, &patch)?;
    let spliced = splice_reviewed_patch(operation, expected_new_path, original, &patch)?;
    if stitched != spliced {
        return Err(reviewed_refusal(
            operation,
            expected_new_path,
            "the saved review diff could not be reconstructed consistently",
        ));
    }
    ensure_reconstructed_text_is_writable(operation, expected_new_path, &stitched)?;
    Ok(stitched)
}

fn parse_reviewed_patch(operation: &str, path: &str, saved_diff: &str) -> Result<ReviewedPatch> {
    let mut lines = saved_diff.lines().peekable();
    while matches!(lines.peek(), Some(&"")) {
        lines.next();
    }
    let Some(old_header) = lines.next() else {
        return Err(reviewed_refusal(
            operation,
            path,
            "the saved review diff is empty",
        ));
    };
    if old_header.starts_with("Binary files ") || old_header.contains("GIT binary patch") {
        return Err(reviewed_refusal(
            operation,
            path,
            "the saved review diff is an unsupported binary edit",
        ));
    }
    if !old_header.starts_with("--- ") {
        return Err(reviewed_refusal(
            operation,
            path,
            "the saved review diff is missing a --- file header",
        ));
    }
    let Some(new_header) = lines.next() else {
        return Err(reviewed_refusal(
            operation,
            path,
            "the saved review diff is missing a +++ file header",
        ));
    };
    if !new_header.starts_with("+++ ") {
        return Err(reviewed_refusal(
            operation,
            path,
            "the saved review diff is missing a +++ file header",
        ));
    }
    let old_path = parse_reviewed_path(old_header)
        .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
    let new_path = parse_reviewed_path(new_header)
        .map_err(|reason| reviewed_refusal(operation, path, &reason))?;

    let mut hunks = Vec::new();
    let mut current: Option<ReviewedHunk> = None;
    let mut body_lines = 0usize;

    let finish_hunk = |hunk: ReviewedHunk| -> Result<ReviewedHunk> {
        let (old_seen, new_seen) = counted_reviewed_lines(&hunk);
        if old_seen != hunk.old_count || new_seen != hunk.new_count {
            return Err(reviewed_refusal(
                operation,
                path,
                &format!(
                    "the saved review diff has a malformed hunk (header claims {} old and {} new lines but the body has {} and {})",
                    hunk.old_count, hunk.new_count, old_seen, new_seen
                ),
            ));
        }
        Ok(hunk)
    };

    for line in lines {
        if line.starts_with("Binary files ") || line == "GIT binary patch" {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff is an unsupported binary edit",
            ));
        }
        if line.starts_with("# finch:") || line.contains("[line truncated]") {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff contains hidden or truncated content",
            ));
        }
        let hunk_open = current.as_ref().is_some_and(|hunk| {
            let (old_seen, new_seen) = counted_reviewed_lines(hunk);
            old_seen < hunk.old_count || new_seen < hunk.new_count
        });
        if !hunk_open && line.starts_with("--- ") {
            if let Some(hunk) = current.take() {
                hunks.push(finish_hunk(hunk)?);
            }
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff includes an additional file",
            ));
        }
        if line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(finish_hunk(hunk)?);
            }
            if hunks.len() >= MAX_DIFF_HUNKS {
                return Err(reviewed_refusal(
                    operation,
                    path,
                    "the saved review diff exceeds the bounded hunk count",
                ));
            }
            let (old_start, old_count, new_start, new_count) = parse_reviewed_hunk_header(line)
                .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
            current = Some(ReviewedHunk {
                old_start,
                old_count,
                new_start,
                new_count,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(hunk) = current.as_mut() else {
            if line.is_empty() {
                continue;
            }
            return Err(reviewed_refusal(
                operation,
                path,
                &format!(
                    "the saved review diff has a malformed hunk line ({})",
                    crate::cli::diff::sanitize_terminal(line)
                ),
            ));
        };
        if line == "\\ No newline at end of file" {
            let Some(previous) = hunk.lines.last_mut() else {
                return Err(reviewed_refusal(
                    operation,
                    path,
                    "the saved review diff has a no-newline marker without a preceding hunk line",
                ));
            };
            previous.no_newline = true;
            continue;
        }
        let (kind, text) = if let Some(rest) = line.strip_prefix('+') {
            (ReviewedLineKind::Add, rest.to_string())
        } else if let Some(rest) = line.strip_prefix('-') {
            (ReviewedLineKind::Remove, rest.to_string())
        } else if let Some(rest) = line.strip_prefix(' ') {
            (ReviewedLineKind::Context, rest.to_string())
        } else if line.is_empty() {
            (ReviewedLineKind::Context, String::new())
        } else {
            return Err(reviewed_refusal(
                operation,
                path,
                &format!(
                    "the saved review diff has a malformed hunk line ({})",
                    crate::cli::diff::sanitize_terminal(line)
                ),
            ));
        };
        if text.chars().count() > MAX_DIFF_LINE_CHARS {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff has a hunk line that exceeds the display bound",
            ));
        }
        body_lines += 1;
        if body_lines > MAX_DIFF_LINES {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff exceeds the bounded line count",
            ));
        }
        hunk.lines.push(ReviewedLine {
            kind,
            text,
            no_newline: false,
        });
    }
    if let Some(hunk) = current.take() {
        hunks.push(finish_hunk(hunk)?);
    }
    Ok(ReviewedPatch {
        old_path,
        new_path,
        hunks,
    })
}

fn parse_reviewed_path(header: &str) -> Result<String, String> {
    let rest = header
        .strip_prefix("--- ")
        .or_else(|| header.strip_prefix("+++ "))
        .ok_or_else(|| "the saved review diff has a malformed file header".to_string())?;
    let raw = rest.split('\t').next().unwrap_or(rest).trim();
    if raw.is_empty() {
        return Err("the saved review diff has an empty file path header".to_string());
    }
    let decoded = if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        lossless_unquote(&raw[1..raw.len() - 1])?
    } else {
        raw.to_string()
    };
    if decoded == "/dev/null" {
        return Ok(decoded);
    }
    Ok(decoded
        .strip_prefix("a/")
        .or_else(|| decoded.strip_prefix("b/"))
        .unwrap_or(&decoded)
        .to_string())
}

fn lossless_unquote(value: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Ok(out)
}

fn parse_reviewed_hunk_header(line: &str) -> Result<(usize, usize, usize, usize), String> {
    let rest = line
        .strip_prefix("@@ ")
        .ok_or_else(|| "the saved review diff has a malformed hunk header".to_string())?;
    let (ranges, _) = rest
        .split_once(" @@")
        .ok_or_else(|| "the saved review diff has a malformed hunk header".to_string())?;
    let mut parts = ranges.split_whitespace();
    let old = parts
        .next()
        .ok_or_else(|| "the saved review diff has a malformed hunk header".to_string())?;
    let new = parts
        .next()
        .ok_or_else(|| "the saved review diff has a malformed hunk header".to_string())?;
    let parse_range = |value: &str, sign: char| -> Result<(usize, usize), String> {
        let rest = value
            .strip_prefix(sign)
            .ok_or_else(|| "the saved review diff has a malformed hunk header".to_string())?;
        match rest.split_once(',') {
            Some((start, count)) => Ok((
                start
                    .parse()
                    .map_err(|_| "the saved review diff has a malformed hunk header".to_string())?,
                count
                    .parse()
                    .map_err(|_| "the saved review diff has a malformed hunk header".to_string())?,
            )),
            None => Ok((
                rest.parse()
                    .map_err(|_| "the saved review diff has a malformed hunk header".to_string())?,
                1,
            )),
        }
    };
    let (old_start, old_count) = parse_range(old, '-')?;
    let (new_start, new_count) = parse_range(new, '+')?;
    Ok((old_start, old_count, new_start, new_count))
}

fn counted_reviewed_lines(hunk: &ReviewedHunk) -> (usize, usize) {
    let mut old = 0usize;
    let mut new = 0usize;
    for line in &hunk.lines {
        match line.kind {
            ReviewedLineKind::Context => {
                old += 1;
                new += 1;
            }
            ReviewedLineKind::Remove => old += 1,
            ReviewedLineKind::Add => new += 1,
        }
    }
    (old, new)
}

fn split_text_lines(text: &str) -> (Vec<&str>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let trailing = text.ends_with('\n');
    let body = if trailing {
        &text[..text.len() - 1]
    } else {
        text
    };
    (body.split('\n').collect(), trailing)
}

fn join_text_lines(lines: &[String], trailing: bool) -> String {
    let mut out = lines.join("\n");
    if trailing {
        out.push('\n');
    }
    out
}

impl ReviewedHunk {
    fn old_range(&self) -> Result<(usize, usize), String> {
        if self.old_count == 0 {
            return Ok((self.old_start, self.old_start));
        }
        let start = self
            .old_start
            .checked_sub(1)
            .ok_or_else(|| "the saved review diff has an out-of-range hunk".to_string())?;
        let end = start
            .checked_add(self.old_count)
            .ok_or_else(|| "the saved review diff has an out-of-range hunk".to_string())?;
        Ok((start, end))
    }

    fn old_side(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter(|line| line.kind != ReviewedLineKind::Add)
            .map(|line| line.text.as_str())
            .collect()
    }

    fn new_side(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|line| line.kind != ReviewedLineKind::Remove)
            .map(|line| line.text.clone())
            .collect()
    }

    fn new_missing_newline(&self) -> Option<bool> {
        self.lines
            .iter()
            .rev()
            .find(|line| line.kind != ReviewedLineKind::Remove)
            .map(|line| line.no_newline)
    }
}

fn result_trailing_newline(
    orig_len: usize,
    orig_trailing: bool,
    hunks: &[ReviewedHunk],
) -> Result<bool, String> {
    let mut trailing = orig_trailing;
    for hunk in hunks {
        let (start, end) = hunk.old_range()?;
        let inserts_at_eof = hunk.old_count == 0 && start == orig_len;
        if end == orig_len || inserts_at_eof {
            match hunk.new_missing_newline() {
                Some(missing) => trailing = !missing,
                None => trailing = start > 0,
            }
        }
    }
    Ok(trailing)
}

fn stitch_reviewed_patch(
    operation: &str,
    path: &str,
    original: &str,
    patch: &ReviewedPatch,
) -> Result<String> {
    let (orig_lines, orig_trailing) = split_text_lines(original);
    let mut out = Vec::new();
    let mut old_i = 0usize;
    let mut new_i = 0usize;
    let mut last_empty_insert = None;
    for hunk in &patch.hunks {
        let (start, end) = hunk
            .old_range()
            .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
        if start < old_i || end > orig_lines.len() {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff has overlapping or out-of-range hunks",
            ));
        }
        if hunk.old_count == 0 {
            if last_empty_insert == Some(start) {
                return Err(reviewed_refusal(
                    operation,
                    path,
                    "the saved review diff has overlapping or out-of-range hunks",
                ));
            }
            last_empty_insert = Some(start);
        } else {
            last_empty_insert = None;
        }
        new_i += start - old_i;
        out.extend(
            orig_lines[old_i..start]
                .iter()
                .map(|line| (*line).to_string()),
        );
        let expected_new_start = if hunk.new_count == 0 {
            new_i
        } else {
            new_i + 1
        };
        if hunk.new_start != expected_new_start {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff has a hunk header that does not match the reconstructed file",
            ));
        }
        let old_side = hunk.old_side();
        if old_side != orig_lines[start..end] {
            return Err(reviewed_refusal(
                operation,
                path,
                &format!(
                    "the saved review diff does not match the original file at line {}",
                    start.saturating_add(1)
                ),
            ));
        }
        let new_side = hunk.new_side();
        new_i += new_side.len();
        out.extend(new_side);
        old_i = end;
    }
    out.extend(orig_lines[old_i..].iter().map(|line| (*line).to_string()));
    let trailing = result_trailing_newline(orig_lines.len(), orig_trailing, &patch.hunks)
        .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
    Ok(join_text_lines(&out, trailing_for_join(&out, trailing)))
}

fn splice_reviewed_patch(
    operation: &str,
    path: &str,
    original: &str,
    patch: &ReviewedPatch,
) -> Result<String> {
    let (orig_lines, orig_trailing) = split_text_lines(original);
    let mut lines: Vec<String> = orig_lines.iter().map(|line| (*line).to_string()).collect();
    for hunk in patch.hunks.iter().rev() {
        let (start, end) = hunk
            .old_range()
            .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
        if end > lines.len() {
            return Err(reviewed_refusal(
                operation,
                path,
                "the saved review diff has overlapping or out-of-range hunks",
            ));
        }
        let old_side: Vec<&str> = hunk.old_side();
        if old_side
            != lines[start..end]
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        {
            return Err(reviewed_refusal(
                operation,
                path,
                &format!(
                    "the saved review diff does not match the original file at line {}",
                    start.saturating_add(1)
                ),
            ));
        }
        lines.splice(start..end, hunk.new_side());
    }
    let trailing = result_trailing_newline(orig_lines.len(), orig_trailing, &patch.hunks)
        .map_err(|reason| reviewed_refusal(operation, path, &reason))?;
    Ok(join_text_lines(&lines, trailing_for_join(&lines, trailing)))
}

fn trailing_for_join(lines: &[String], trailing: bool) -> bool {
    if lines.is_empty() {
        false
    } else {
        trailing
    }
}

fn ensure_reconstructed_text_is_writable(operation: &str, path: &str, text: &str) -> Result<()> {
    for (offset, character) in text.char_indices() {
        if character == '\n' || character == '\t' || !character.is_control() {
            continue;
        }
        let name = match character {
            '\r' => "CR/CRLF".to_string(),
            '\u{1b}' => "ESCAPE".to_string(),
            other => format!("control character U+{:04X}", other as u32),
        };
        return Err(reviewed_refusal(
            operation,
            path,
            &format!("the saved review diff reconstructs {name} at byte {offset}"),
        ));
    }
    Ok(())
}

/// Editor-backed proposal API that preserves the user's explicit action.
/// Existing callers may continue using `propose_in_editor` while migrating.
pub async fn propose_with_decision(description: &str, code: &str) -> Result<ProposalDecision> {
    let expected = build_artifact(description, code, "#", true);
    Ok(match propose_in_editor(description, code).await? {
        Some(content) => match parse_proposal_decision(&content) {
            ProposalDecision::Chat { .. } => ProposalDecision::Chat {
                context: proposal_chat_context(&content, &expected),
            },
            decision => decision,
        },
        None => ProposalDecision::Cancel,
    })
}

/// Open a proposal artifact in the user editor and preserve the explicit
/// `execute`/`chat`/`cancel` decision.  This is the language-neutral bridge
/// used by the typed VM: accepting an edited artifact returns source data and
/// does *not* execute it.  A caller must submit Finch source through the VM or
/// use the normal separately-authorized external-script workflow.
pub async fn propose_artifact_with_decision(
    language: &str,
    description: &str,
    source: &str,
) -> Result<ProposalDecision> {
    let language = language.trim().to_ascii_lowercase();
    match language.as_str() {
        "forth" | "coforth" | "co-forth" => {
            Ok(match propose_forth_in_editor(description, source).await? {
                Some(content) => parse_proposal_decision(&content),
                None => ProposalDecision::Cancel,
            })
        }
        "bash" | "sh" | "shell" | "python" | "py" | "lisp" | "finch" | "text" => Ok(
            match propose_in_editor_with_suffix(
                description,
                source,
                artifact_suffix(&language),
                artifact_comment_prefix(&language),
                matches!(language.as_str(), "bash" | "sh" | "shell"),
            )
            .await?
            {
                Some(content) => parse_proposal_decision(&content),
                None => ProposalDecision::Cancel,
            },
        ),
        _ => anyhow::bail!("unsupported proposal artifact language '{language}'"),
    }
}

fn artifact_comment_prefix(language: &str) -> &'static str {
    match language {
        "lisp" | "finch" => ";;",
        _ => "#",
    }
}

fn artifact_suffix(language: &str) -> &'static str {
    match language {
        "python" | "py" => ".py",
        "lisp" | "finch" => ".lisp",
        "text" => ".txt",
        _ => ".sh",
    }
}

pub(crate) fn suspend_terminal_for_editor() {
    std::io::stdout().flush().ok();
    // Raw mode alone is not the whole TUI protocol. Leaving bracketed paste or
    // kitty keyboard enhancement enabled makes vi receive Finch's input dialect.
    execute!(
        std::io::stdout(),
        event::PopKeyboardEnhancementFlags,
        event::DisableBracketedPaste,
        cursor::Show,
        ResetColor,
    )
    .ok();
    terminal::disable_raw_mode().ok();
    // Finch renders on the terminal's primary screen so conversation history
    // remains useful shell scrollback. Give full-screen editors a disposable
    // screen even when the selected editor does not enter one itself.
    enter_editor_screen(&mut std::io::stdout()).ok();
    std::io::stdout().flush().ok();
}

pub(crate) fn resume_terminal_after_editor() {
    leave_editor_screen(&mut std::io::stdout()).ok();
    terminal::enable_raw_mode().ok();
    execute!(
        std::io::stdout(),
        event::EnableBracketedPaste,
        event::PushKeyboardEnhancementFlags(
            event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES,
        ),
    )
    .ok();
    crate::request_tui_rebuild();
    crate::set_editor_active(false);
}

fn enter_editor_screen(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    execute!(
        writer,
        terminal::EnterAlternateScreen,
        terminal::Clear(terminal::ClearType::All),
        cursor::MoveTo(0, 0),
    )
}

fn leave_editor_screen(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    // Always issue the matching leave, even when a full-screen child emitted
    // its own enter/leave pair. Alternate screens do not stack on common
    // terminals, so the child's leave may already have selected the primary
    // screen; this final idempotent leave still guarantees Finch does not
    // resume rendering into an alternate screen.
    execute!(
        writer,
        terminal::LeaveAlternateScreen,
        cursor::Show,
        ResetColor,
    )
}

trait EditorTerminalControl: Send + Sync + 'static {
    fn set_editor_active(&self, active: bool);
    fn suspend(&self);
    fn restore(&self, terminal_was_suspended: bool);
}

#[derive(Clone, Copy)]
struct ProductionTerminalControl;

impl EditorTerminalControl for ProductionTerminalControl {
    fn set_editor_active(&self, active: bool) {
        crate::set_editor_active(active);
    }

    fn suspend(&self) {
        suspend_terminal_for_editor();
    }

    fn restore(&self, terminal_was_suspended: bool) {
        if terminal_was_suspended {
            resume_terminal_after_editor();
        } else {
            // The render loop may have skipped a frame while the handoff was
            // pending even though the terminal itself was never mutated.
            crate::request_tui_rebuild();
            crate::set_editor_active(false);
        }
    }
}

struct TerminalRestorer<C: EditorTerminalControl> {
    control: C,
    terminal_was_suspended: bool,
}

impl<C: EditorTerminalControl> TerminalRestorer<C> {
    fn new(control: C) -> Self {
        control.set_editor_active(true);
        Self {
            control,
            terminal_was_suspended: false,
        }
    }

    fn suspend(&mut self) {
        // Arm the full restore before the first terminal mutation. A panic in
        // the terminal writer must still release the editor gate.
        self.terminal_was_suspended = true;
        self.control.suspend();
    }
}

impl<C: EditorTerminalControl> Drop for TerminalRestorer<C> {
    fn drop(&mut self) {
        self.control.restore(self.terminal_was_suspended);
    }
}

async fn run_editor_lifecycle<C, F, T>(
    tui_mode: bool,
    grace_period: Duration,
    control: C,
    editor_work: F,
) -> Result<T>
where
    C: EditorTerminalControl,
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    if !tui_mode {
        return spawn_blocking(editor_work).await?;
    }

    // Construct the guard before the only async cancellation point. Dropping
    // this future during the grace period releases the gate and redraws once.
    let mut restore = TerminalRestorer::new(control);
    tokio::time::sleep(grace_period).await;
    restore.suspend();

    spawn_blocking(move || {
        let _restore = restore;
        editor_work()
    })
    .await?
}

#[cfg(test)]
impl<T: EditorTerminalControl> EditorTerminalControl for std::sync::Arc<T> {
    fn set_editor_active(&self, active: bool) {
        (**self).set_editor_active(active);
    }

    fn suspend(&self) {
        (**self).suspend();
    }

    fn restore(&self, terminal_was_suspended: bool) {
        (**self).restore(terminal_was_suspended);
    }
}

/// Split the conventional `$VISUAL`/`$EDITOR` form without involving a shell.
/// Supports whitespace, single/double quotes and backslash escaping.
fn split_editor_command(value: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' && quote != Some('\'') {
            escaped = true;
        } else if matches!(ch, '\'' | '"') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            } else {
                current.push(ch);
            }
        } else if ch.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    if quote.is_some() {
        anyhow::bail!("Unclosed quote in $VISUAL/$EDITOR");
    }
    if !current.is_empty() {
        args.push(current);
    }
    if args.is_empty() {
        anyhow::bail!("$VISUAL/$EDITOR is empty");
    }
    Ok(args)
}

pub(crate) fn run_editor(path: &Path) -> Result<std::process::ExitStatus> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let mut parts = split_editor_command(&editor)?;
    let program = parts.remove(0);
    Ok(std::process::Command::new(program)
        .args(parts)
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?)
}

fn edit_artifact(
    content: &str,
    suffix: &str,
    executable: bool,
    comment_prefix: &str,
    read_limit: Option<usize>,
    editor: impl FnOnce(&Path) -> Result<std::process::ExitStatus>,
) -> Result<Option<String>> {
    let mut tmp = Builder::new().prefix("finch_").suffix(suffix).tempfile()?;

    tmp.write_all(content.as_bytes())?;
    tmp.flush()?;
    let path = tmp.path().to_owned();

    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms)?;
    }

    if !editor(&path)?.success() {
        return Ok(None);
    }

    if let Some(limit) = read_limit {
        let length = std::fs::metadata(&path)?.len();
        if length > limit as u64 {
            anyhow::bail!(
                "$VISUAL/$EDITOR returned a review artifact of {length} bytes, exceeding the \
                 {limit}-byte safety limit; the proposed edit was not applied"
            );
        }
    }
    let mut reader = std::fs::File::open(&path)?;
    let mut bytes = Vec::new();
    match read_limit {
        Some(limit) => {
            reader.take(limit as u64 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > limit {
                anyhow::bail!(
                    "$VISUAL/$EDITOR returned a review artifact exceeding the {limit}-byte \
                     safety limit; the proposed edit was not applied"
                );
            }
        }
        None => {
            reader.read_to_end(&mut bytes)?;
        }
    }
    let modified = String::from_utf8(bytes)
        .map_err(|error| anyhow::anyhow!("$VISUAL/$EDITOR returned non-UTF-8 text: {error}"))?;
    let has_source = modified.lines().any(|line| {
        let trimmed = line.trim();
        !trimmed.is_empty() && !line.trim_start().starts_with(comment_prefix)
    });
    Ok(has_source.then_some(modified))
}

/// Write `script` to a temp `.sh` file, open `$EDITOR`, read back the result.
///
/// Returns `Some(script)` if the user saved a non-empty file, `None` if they
/// cleared it (abort).  The temp file is cleaned up on return.
///
/// In non-interactive environments (tests, daemon, piped input) the editor is
/// skipped and the generated artifact is returned immediately.
///
/// `description` is embedded as a `#`-comment block at the top so the user
/// can read the English intent alongside the code.
pub async fn propose_in_editor(description: &str, code: &str) -> Result<Option<String>> {
    propose_in_editor_with_suffix(description, code, ".sh", "#", true).await
}

async fn propose_in_editor_with_suffix(
    description: &str,
    code: &str,
    suffix: &'static str,
    comment_prefix: &'static str,
    executable: bool,
) -> Result<Option<String>> {
    let script = build_artifact(description, code, comment_prefix, executable);
    // Unit tests often run under a PTY, so `is_terminal()` alone would launch
    // the developer's real $EDITOR and wedge the test runner. Test builds use
    // the same noninteractive result as daemon/piped invocations; editor
    // lifecycle behavior is covered through the explicit decision/resume
    // tests rather than a human editor process.
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return Ok(Some(script));
    }
    let tui_mode = crate::is_tui_active();

    run_editor_lifecycle(
        tui_mode,
        Duration::from_millis(50),
        ProductionTerminalControl,
        move || {
            edit_artifact(
                &script,
                suffix,
                executable,
                comment_prefix,
                None,
                run_editor,
            )
        },
    )
    .await
}

/// Open a read-only review artifact in `$EDITOR` and return what the user
/// left behind, or `None` if they cleared it (abort).
///
/// A review artifact is a *document*, not a program. Nothing on this path
/// executes it: the caller reads the user's decision out of the artifact's
/// `#`-comment header and then performs the change itself through the tool
/// layer. The `.diff` suffix is what makes an editor highlight the body as a
/// diff, and `executable: false` keeps the temp file off the exec path.
pub async fn open_review_artifact(artifact: &str) -> Result<Option<String>> {
    // Same guard as `propose_in_editor_with_suffix` and `propose_forth_in_editor`:
    // unit tests can own a PTY, so gating on `is_terminal()` alone would launch
    // the developer's real `$EDITOR` and wedge the test runner. Regressions
    // drive `open_review_artifact_with` instead, which keeps running the editor
    // it is given.
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return Ok(Some(artifact.to_string()));
    }
    open_review_artifact_with(artifact, run_editor).await
}

/// Maximum review artifact accepted back from an editor.
///
/// The generated unified diff is already bounded well below this value. The
/// extra room permits ordinary editor annotations without allowing a replaced
/// temporary file to force an unbounded allocation before approval parsing.
const MAX_REVIEW_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;

/// `open_review_artifact` with the editor process injected, so a regression
/// can inspect the exact bytes a human's editor is given without launching
/// the developer's real `$EDITOR`.
pub(crate) async fn open_review_artifact_with<E>(
    artifact: &str,
    editor: E,
) -> Result<Option<String>>
where
    E: FnOnce(&Path) -> Result<std::process::ExitStatus> + Send + 'static,
{
    let artifact = artifact.to_string();
    let tui_mode = crate::is_tui_active();
    run_editor_lifecycle(
        tui_mode,
        Duration::from_millis(50),
        ProductionTerminalControl,
        move || {
            edit_artifact(
                &artifact,
                ".diff",
                false,
                "#",
                Some(MAX_REVIEW_ARTIFACT_BYTES),
                editor,
            )
        },
    )
    .await
}

/// Format the English description + code as a commented shell script.
///
/// ```text
/// #!/bin/bash
/// # Delete all .tmp files in ~/repos/finch/target
///
/// find ~/repos/finch/target -name "*.tmp" -delete
/// ```
pub fn build_script(description: &str, code: &str) -> String {
    build_artifact(description, code, "#", true)
}

/// Format an editable source artifact without making its language executable.
/// The comment marker belongs to the artifact language, so an accepted Lisp
/// proposal is still valid Lisp rather than a shell file with a renamed suffix.
fn append_proposal_header(out: &mut String, comment_prefix: &str) {
    out.push_str(comment_prefix);
    out.push_str(" Finch proposal — edit the action line below, then save and quit.\n");
    let name_width = ProposalAction::ALL
        .iter()
        .map(|action| action.name().len())
        .max()
        .unwrap_or(0);
    for action in ProposalAction::ALL {
        out.push_str(comment_prefix);
        out.push_str("   action=");
        out.push_str(action.name());
        out.push_str(&" ".repeat(name_width.saturating_sub(action.name().len()) + 3));
        out.push_str(action.help());
        out.push('\n');
    }
    out.push_str(comment_prefix);
    out.push_str(" finch: action=");
    out.push_str(ProposalAction::Execute.name());
    out.push('\n');
}

fn build_artifact(description: &str, code: &str, comment_prefix: &str, executable: bool) -> String {
    let mut out = if executable {
        String::from("#!/bin/bash\n")
    } else {
        String::new()
    };
    append_proposal_header(&mut out, comment_prefix);
    for line in description.lines() {
        let clean = crate::cli::diff::sanitize_terminal(line);
        let candidate = format!("{comment_prefix} {clean}");
        out.push_str(comment_prefix);
        out.push(' ');
        if proposal_directive(&candidate).is_some() || is_proposal_body_marker(&candidate) {
            out.push_str("> ");
        }
        out.push_str(&clean);
        out.push('\n');
    }
    out.push_str(comment_prefix);
    out.push(' ');
    out.push_str(PROPOSAL_BODY_MARKER);
    out.push('\n');
    out.push_str(code);
    if executable && !code.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Build a non-executable plaintext review artifact with a structural header
/// and body boundary understood by [`parse_proposal_decision`].
pub fn build_review_artifact(description: &str, body: &str) -> String {
    build_artifact(description, body, "#", false)
}

/// Run an approved script asynchronously via `bash -c`.
///
/// Returns `Ok(stdout+stderr)` on success, `Err` if the script exits non-zero.
pub async fn run_script_async(script: &str) -> Result<String> {
    const SCRIPT_TIMEOUT: Duration = Duration::from_secs(30);
    let mut command = tokio::process::Command::new("bash");
    command.arg("-c").arg(script).kill_on_drop(true);
    let output = tokio::time::timeout(SCRIPT_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("approved script timed out after 30 seconds"))??;
    let mut result = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&stderr);
    }
    if !output.status.success() {
        return Err(anyhow::anyhow!("{}", result.trim()));
    }
    Ok(result)
}

/// Open `$EDITOR` with Forth source, return the (possibly edited) content.
///
/// Uses a `.forth` extension so editors apply Forth syntax highlighting.
/// Lines starting with `\` are treated as comments; emptying the file aborts.
/// In non-interactive environments the editor is skipped and the structured
/// artifact is returned immediately; decision-aware callers then recover the
/// exact body through [`parse_proposal_decision`].
pub async fn propose_forth_in_editor(description: &str, code: &str) -> Result<Option<String>> {
    let content = build_artifact(description, code, "\\", false);
    // Keep the typed Forth proposal path consistent with every other
    // artifact: integration tests can own a PTY, but must never launch the
    // developer's real editor or receive generated comment headers as the
    // accepted source.
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return Ok(Some(content));
    }
    let tui_mode = crate::is_tui_active();

    run_editor_lifecycle(
        tui_mode,
        Duration::from_millis(50),
        ProductionTerminalControl,
        move || edit_artifact(&content, ".forth", false, "\\", None, run_editor),
    )
    .await
}

/// Run a shell script string via `bash -c`.
///
/// Returns stdout+stderr combined.
pub fn run_script(script: &str) -> Result<String> {
    // Extract only the non-comment, non-empty lines to run.
    // This lets the user keep their comments in the file without bash choking.
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg(script)
        .output()?;

    let mut result = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        result.push_str(&stderr);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };

    static LIFECYCLE_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Debug, Default)]
    struct LifecycleCounts {
        gate_enabled: usize,
        gate_disabled: usize,
        suspends: usize,
        restores: usize,
        rebuilds: usize,
    }

    #[derive(Default)]
    struct TestTerminalControl {
        counts: Mutex<LifecycleCounts>,
        output: Mutex<Vec<u8>>,
    }

    impl EditorTerminalControl for TestTerminalControl {
        fn set_editor_active(&self, active: bool) {
            let mut counts = self.counts.lock().unwrap();
            if active {
                counts.gate_enabled += 1;
            } else {
                counts.gate_disabled += 1;
            }
            crate::set_editor_active(active);
        }

        fn suspend(&self) {
            self.counts.lock().unwrap().suspends += 1;
            enter_editor_screen(&mut *self.output.lock().unwrap()).unwrap();
        }

        fn restore(&self, terminal_was_suspended: bool) {
            let mut counts = self.counts.lock().unwrap();
            counts.restores += 1;
            counts.rebuilds += 1;
            counts.gate_disabled += 1;
            drop(counts);
            if terminal_was_suspended {
                leave_editor_screen(&mut *self.output.lock().unwrap()).unwrap();
            }
            crate::request_tui_rebuild();
            crate::set_editor_active(false);
        }
    }

    fn reset_terminal_globals() {
        crate::set_editor_active(false);
        crate::take_tui_rebuild();
    }

    fn assert_restored_once(control: &TestTerminalControl, suspended: bool) {
        let counts = control.counts.lock().unwrap();
        assert_eq!(counts.gate_enabled, 1);
        assert_eq!(counts.gate_disabled, 1);
        assert_eq!(counts.suspends, usize::from(suspended));
        assert_eq!(counts.restores, 1);
        assert_eq!(counts.rebuilds, 1);
        drop(counts);
        assert!(!crate::is_editor_active());
        assert!(crate::take_tui_rebuild());
        assert!(!crate::take_tui_rebuild());
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[test]
    fn test_build_script_adds_comment_header() {
        let s = build_script("Delete temp files", "rm -rf /tmp/foo");
        assert!(s.starts_with("#!/bin/bash\n"));
        assert!(s.contains("# Delete temp files\n"));
        assert!(s.contains("rm -rf /tmp/foo"));
    }

    #[test]
    fn test_build_script_multiline_description() {
        let s = build_script("Line one\nLine two", "echo hi");
        assert!(s.contains("# Line one\n"));
        assert!(s.contains("# Line two\n"));
    }

    #[test]
    fn test_build_script_ends_with_newline() {
        let s = build_script("desc", "cmd");
        assert!(s.ends_with('\n'));
    }

    #[test]
    fn test_split_editor_command_preserves_quoted_arguments() {
        assert_eq!(
            split_editor_command(r#"code --wait --profile "Finch Work""#).unwrap(),
            vec!["code", "--wait", "--profile", "Finch Work"]
        );
    }

    #[test]
    fn test_split_editor_command_rejects_unclosed_quote() {
        assert!(split_editor_command("vi 'unfinished").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn review_artifact_readback_is_bounded_before_allocation() {
        use std::os::unix::process::ExitStatusExt as _;

        let error = open_review_artifact_with("# finch: action=execute\n+small\n", |path| {
            let oversized = vec![b'x'; MAX_REVIEW_ARTIFACT_BYTES + 1];
            std::fs::write(path, oversized).expect("fake editor replaces artifact");
            Ok(std::process::ExitStatus::from_raw(0))
        })
        .await
        .expect_err("an editor-returned artifact over the cap must fail closed");

        let detail = format!("{error:#}");
        assert!(
            detail.contains("review artifact")
                && detail.contains("safety limit")
                && detail.contains(&MAX_REVIEW_ARTIFACT_BYTES.to_string()),
            "oversized-artifact refusal must identify the artifact and exact cap; error: {detail}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lifecycle_restores_after_every_fake_full_screen_editor_outcome() {
        use nix::libc;
        use std::os::unix::process::ExitStatusExt;

        #[derive(Clone, Copy)]
        enum Outcome {
            Success,
            Empty,
            Nonzero,
            Signal,
            LaunchError,
        }

        let _serial = LIFECYCLE_TEST_LOCK.lock().unwrap();
        for outcome in [
            Outcome::Success,
            Outcome::Empty,
            Outcome::Nonzero,
            Outcome::Signal,
            Outcome::LaunchError,
        ] {
            reset_terminal_globals();
            let control = Arc::new(TestTerminalControl::default());
            let child = Arc::clone(&control);
            let result =
                run_editor_lifecycle(true, Duration::ZERO, Arc::clone(&control), move || {
                    edit_artifact("before\n", ".txt", false, "#", None, |path| {
                        if matches!(outcome, Outcome::LaunchError) {
                            anyhow::bail!("fake editor launch failed");
                        }
                        if matches!(outcome, Outcome::Signal) {
                            // A real full-screen child can die before its
                            // terminal cleanup handler runs. Emit only the
                            // child's enter sequence, then terminate the shell
                            // itself so ExitStatus carries an actual signal.
                            let output = std::process::Command::new("/bin/sh")
                                .args(["-c", r#"printf '\033[?1049h'; kill -TERM $$"#])
                                .output()?;
                            assert_eq!(output.status.signal(), Some(libc::SIGTERM));
                            child.output.lock().unwrap().extend(output.stdout);
                            return Ok(output.status);
                        }
                        {
                            let mut output = child.output.lock().unwrap();
                            execute!(&mut *output, terminal::EnterAlternateScreen)?;
                            execute!(&mut *output, terminal::LeaveAlternateScreen)?;
                        }
                        match outcome {
                            Outcome::Success => {
                                std::fs::write(path, "after\n")?;
                                Ok(exit_status(0))
                            }
                            Outcome::Empty => {
                                std::fs::write(path, "# review only\n\n")?;
                                Ok(exit_status(0))
                            }
                            Outcome::Nonzero => Ok(exit_status(7)),
                            Outcome::Signal | Outcome::LaunchError => unreachable!(),
                        }
                    })
                })
                .await;

            match outcome {
                Outcome::Success => assert_eq!(result.unwrap().as_deref(), Some("after\n")),
                Outcome::Empty | Outcome::Nonzero | Outcome::Signal => {
                    assert_eq!(result.unwrap(), None)
                }
                Outcome::LaunchError => {
                    assert!(result
                        .unwrap_err()
                        .to_string()
                        .contains("fake editor launch failed"))
                }
            }
            assert_restored_once(&control, true);

            let output = String::from_utf8(control.output.lock().unwrap().clone()).unwrap();
            let child_started = !matches!(outcome, Outcome::LaunchError);
            let child_left_screen = !matches!(outcome, Outcome::LaunchError | Outcome::Signal);
            assert_eq!(
                output.matches("\x1b[?1049h").count(),
                1 + usize::from(child_started)
            );
            assert_eq!(
                output.matches("\x1b[?1049l").count(),
                1 + usize::from(child_left_screen)
            );
            assert!(output.rfind("\x1b[?1049l").unwrap() > output.rfind("\x1b[?1049h").unwrap());
        }
    }

    #[tokio::test]
    async fn cancelling_during_real_grace_period_releases_gate_without_terminal_restore() {
        let _serial = LIFECYCLE_TEST_LOCK.lock().unwrap();
        reset_terminal_globals();
        let control = Arc::new(TestTerminalControl::default());
        let editor_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&editor_calls);
        let task_control = Arc::clone(&control);
        let task = tokio::spawn(async move {
            run_editor_lifecycle(true, Duration::from_millis(50), task_control, move || {
                observed_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
        });
        tokio::task::yield_now().await;
        assert!(crate::is_editor_active());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());

        assert_eq!(editor_calls.load(Ordering::SeqCst), 0);
        assert_restored_once(&control, false);
        assert!(control.output.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn blocking_editor_panic_after_handoff_still_restores_once() {
        let _serial = LIFECYCLE_TEST_LOCK.lock().unwrap();
        reset_terminal_globals();
        let control = Arc::new(TestTerminalControl::default());
        let result: Result<()> =
            run_editor_lifecycle(true, Duration::ZERO, Arc::clone(&control), || {
                panic!("fake editor panic")
            })
            .await;

        assert!(result.unwrap_err().to_string().contains("panicked"));
        assert_restored_once(&control, true);
    }

    #[test]
    fn test_run_script_captures_output() {
        let out = run_script("#!/bin/bash\necho hello").unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn test_run_script_captures_stderr() {
        let out = run_script("#!/bin/bash\necho err >&2").unwrap();
        assert!(out.contains("err"));
    }

    #[test]
    fn proposal_directives_distinguish_execute_chat_and_cancel() {
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=execute\n# ---- Finch proposal body ----\necho hi"
            ),
            ProposalDecision::Execute {
                source: "echo hi".into()
            }
        );
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=chat\n# ---- Finch proposal body ----\nplease discuss this"
            ),
            ProposalDecision::Chat {
                context: "please discuss this".into()
            }
        );
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=cancel\n# ---- Finch proposal body ----\necho hi"
            ),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn empty_or_comment_only_proposal_is_rejected() {
        assert_eq!(parse_proposal_decision(""), ProposalDecision::Cancel);
        assert_eq!(
            parse_proposal_decision("  \n\t\n"),
            ProposalDecision::Cancel
        );
        assert_eq!(
            parse_proposal_decision(
                "# Finch proposal: delete the source to reject\n# finch: action=execute\n# ---- Finch proposal body ----\n"
            ),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn last_proposal_directive_is_the_users_final_decision() {
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=execute\n# finch: action=cancel\n# ---- Finch proposal body ----\necho dangerous\n"
            ),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn proposal_body_cannot_forge_a_header_decision() {
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=execute\n# ---- Finch proposal body ----\n\
                 first line\n# finch: action=chat\n# finch: action=cancel\nlast line\n"
            ),
            ProposalDecision::Execute {
                source: "first line\n# finch: action=chat\n# finch: action=cancel\nlast line\n"
                    .into()
            },
            "model-controlled body text must never select a proposal action"
        );
    }

    #[test]
    fn missing_boundary_or_unknown_action_fails_closed() {
        assert_eq!(
            parse_proposal_decision("# finch: action=execute\necho hi"),
            ProposalDecision::Cancel,
            "source without an explicit header/body boundary is ambiguous"
        );
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=exceute\n# ---- Finch proposal body ----\necho hi"
            ),
            ProposalDecision::Cancel,
            "an unknown or mistyped action must never imply approval"
        );
    }

    #[test]
    fn generated_proposal_header_names_the_action_line_and_every_parser_value() {
        assert_eq!(
            ProposalAction::ALL
                .iter()
                .map(|action| action.name())
                .collect::<Vec<_>>(),
            ["execute", "cancel", "chat"],
            "every name parse_proposal_decision accepts must be listed here so the generated header cannot silently omit it"
        );
        for prefix in ["#", ";;", "\\"] {
            let artifact = build_artifact("Inspect files", "echo hi\n", prefix, false);
            let header = artifact
                .split_once(PROPOSAL_BODY_MARKER)
                .map(|(head, _)| head)
                .unwrap_or(&artifact);
            assert!(
                header.contains("edit the action line"),
                "header must name the action line as the thing to edit for prefix {prefix:?}; got {header:?}"
            );
            assert!(
                header.contains("save and quit"),
                "header must say to save and quit for prefix {prefix:?}; got {header:?}"
            );
            assert!(
                header.contains(&format!("{prefix} finch: action=execute\n")),
                "action line must use comment_prefix={prefix:?}; got {header:?}"
            );
            for action in ProposalAction::ALL {
                assert!(
                    header.contains(&format!("action={}", action.name())),
                    "header must advertise parser-accepted action {:?} for prefix {prefix:?}; got {header:?}",
                    action.name()
                );
                assert_eq!(ProposalAction::from_name(action.name()), Some(*action));
            }
            assert_eq!(
                parse_proposal_decision(&artifact),
                ProposalDecision::Execute {
                    source: "echo hi\n".into()
                },
                "enumerating action=cancel in the {prefix:?} header must not reject the default proposal"
            );
        }
    }

    #[test]
    fn unprefixed_action_assignment_does_not_select_cancel() {
        assert_eq!(
            parse_proposal_decision(
                "# action=cancel\n# finch: action=execute\n# ---- Finch proposal body ----\necho hi\n"
            ),
            ProposalDecision::Execute {
                source: "echo hi\n".into()
            },
            "copying action=cancel without the finch: prefix must not reject the proposal"
        );
    }

    #[test]
    fn ordinary_git_comments_do_not_control_proposal() {
        assert_eq!(
            parse_proposal_decision("# Please enter the commit message\necho hi"),
            ProposalDecision::Cancel,
            "an ordinary comment is not an explicit Finch approval header"
        );
    }

    #[test]
    fn lisp_artifacts_receive_lisp_comment_headers() {
        let artifact = build_artifact("Explain intent", "(say \"ok\")", ";;", false);
        assert!(artifact.starts_with(";; Finch proposal"));
        assert!(artifact.contains(";; finch: action=execute\n"));
        assert!(artifact.contains(";; Explain intent\n"));
        assert!(artifact.contains(";; ---- Finch proposal body ----\n"));
        assert!(!artifact.starts_with("#!/bin/bash"));
    }

    #[test]
    fn model_description_cannot_forge_directive_or_body_boundary() {
        let artifact = build_review_artifact(
            "Write x\nfinch: action=cancel\n---- Finch proposal body ----",
            "+reviewed body\n",
        );
        assert_eq!(
            parse_proposal_decision(&artifact),
            ProposalDecision::Execute {
                source: "+reviewed body\n".into()
            },
            "model-controlled header prose must not control the user's decision"
        );
    }

    #[test]
    fn chat_context_excludes_generated_diff_and_keeps_reviewer_prose() {
        let expected =
            build_review_artifact("Write x", "--- a/x\n+++ b/x\n@@ -0,0 +1 @@\n+generated\n");
        let returned = format!(
            "{}\n# please use a map instead\n",
            expected.replace("finch: action=execute", "finch: action=chat")
        );
        assert_eq!(
            proposal_chat_context(&returned, &expected),
            "please use a map instead",
            "chat results must carry the reviewer's request without a diff-shaped false success"
        );
    }

    #[tokio::test]
    async fn noninteractive_artifact_proposal_returns_source_without_execution() {
        let decision = propose_artifact_with_decision("python", "example", "print('ok')")
            .await
            .unwrap();
        assert_eq!(
            decision,
            ProposalDecision::Execute {
                source: "print('ok')".into()
            }
        );
    }

    #[tokio::test]
    async fn empty_noninteractive_artifact_is_rejected() {
        assert_eq!(
            propose_artifact_with_decision("bash", "example", "")
                .await
                .unwrap(),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn granted_edit_star_does_not_open_editor() {
        assert!(
            interactive_review_already_granted(true, false),
            "invariant: after the REPL grant (edit:* always, session, or one-time Yes), \
             execute must not open $EDITOR again"
        );
        assert!(
            interactive_review_already_granted(false, true),
            "invariant: AutoAccept must not open $EDITOR; that is the autonomous path"
        );
        assert!(
            !interactive_review_already_granted(false, false),
            "invariant: without a grant, interactive TTY review still opens $EDITOR"
        );
    }

    #[test]
    fn reconstruct_reviewed_text_roundtrips_generated_unified_diffs() {
        let cases = [
            ("replace", "alpha\nkeep\n", "omega\nkeep\n"),
            ("create", "", "hello\nworld\n"),
            (
                "blank-context",
                "alpha\n\nbravo\n\ncharlie\n",
                "alpha\n\nBRAVO\n\ncharlie\n",
            ),
            ("no-final-newline", "before", "after"),
            ("add-trailing-newline", "before", "after\n"),
            ("delete-line", "keep\ngone\n", "keep\n"),
            (
                "sql-double-dash",
                "SELECT 1;\n-- keep totals\n-- and averages\nSELECT 2;\n",
                "SELECT 1;\nSELECT 2;\n",
            ),
        ];
        for (label, original, planned) in cases {
            let diff = if original.is_empty() {
                FileDiff::from_created("demo.txt", planned)
            } else {
                FileDiff::from_texts("demo.txt", original, planned)
            };
            let reconstructed = reconstruct_reviewed_text(
                "write",
                &diff.old_path,
                &diff.new_path,
                original,
                &diff.to_unified(),
            )
            .unwrap_or_else(|error| panic!("{label}: generated diff must reconstruct: {error:#}"));
            assert_eq!(
                reconstructed, planned,
                "{label}: independently reconstructed bytes must equal the planned file"
            );
        }
    }

    #[test]
    fn reconstruct_reviewed_text_applies_saved_added_line_not_planned() {
        let original = "alpha\nkeep\n";
        let planned = "planned-bytes\nkeep\n";
        let diff = FileDiff::from_texts("demo.txt", original, planned);
        let saved = diff
            .to_unified()
            .replace("+planned-bytes", "+reviewed-edit");
        let reconstructed =
            reconstruct_reviewed_text("write", &diff.old_path, &diff.new_path, original, &saved)
                .expect("valid edited hunk must reconstruct");
        assert_eq!(reconstructed, "reviewed-edit\nkeep\n");
        assert!(!reconstructed.contains("planned-bytes"));
    }

    #[test]
    fn reconstruct_reviewed_text_rejects_extra_file_and_path_change() {
        let original = "before\n";
        let planned = "after\n";
        let diff = FileDiff::from_texts("demo.txt", original, planned);
        let generated = diff.to_unified();
        let extra = format!("{generated}--- /dev/null\n+++ b/other.txt\n@@ -0,0 +1,1 @@\n+pwned\n");
        let extra_error =
            reconstruct_reviewed_text("edit", &diff.old_path, &diff.new_path, original, &extra)
                .expect_err("extra file must fail closed");
        assert!(
            extra_error.to_string().contains("additional file"),
            "extra file refusal must name the extra file; error: {extra_error:#}"
        );

        let path_changed = generated
            .lines()
            .map(|line| {
                if line.starts_with("+++ ") {
                    "+++ b/etc/passwd".to_string()
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let path_error = reconstruct_reviewed_text(
            "edit",
            &diff.old_path,
            &diff.new_path,
            original,
            &path_changed,
        )
        .expect_err("path/header change must fail closed");
        assert!(
            path_error.to_string().contains("target path"),
            "path-change refusal must name the header change; error: {path_error:#}"
        );
    }

    #[test]
    fn reconstruct_reviewed_text_rejects_malformed_hunk() {
        let original = "before\n";
        let planned = "after\n";
        let diff = FileDiff::from_texts("demo.txt", original, planned);
        let malformed = format!("{}\n+not-a-complete-hunk\n", diff.to_unified());
        let error =
            reconstruct_reviewed_text("edit", &diff.old_path, &diff.new_path, original, &malformed)
                .expect_err("malformed extra hunk line must fail closed");
        assert!(
            error.to_string().contains("malformed"),
            "malformed hunk refusal must name the defect; error: {error:#}"
        );
    }
}
