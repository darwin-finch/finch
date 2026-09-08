// Edit tool - exact string replacement in files with colored diff output
//
// Returns a diff showing what changed, formatted like Claude Code:
//
//   Added 2 lines, removed 7 lines
//      196     pub fn validate(&self) -> anyhow::Result<()> {
//      199 -   // Old comment
//      199 +   // New comment
//
// Interactive review (#437): the artifact opened in `$EDITOR` is the unified
// diff of the proposed change, not a program that would perform it. Approval
// is a security boundary, so the reviewed bytes are the human-readable change
// itself; the edit is then applied here, in Rust, from the tool's own
// parameters. Nothing on this path is handed to a shell.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;
use std::io::IsTerminal;

use super::propose::open_review_artifact;
use crate::cli::diff::{FileDiff, MAX_DIFF_LINE_CHARS};

/// Separates the machine-read decision header from the human-read diff.
///
/// Matched as a whole line, so no diff content can be mistaken for it: every
/// line of a unified diff body is prefixed with `-`, `+`, ` `, `@`, or `\`.
const REVIEW_DIFF_MARKER: &str =
    "# ---- proposed diff below (review only; edits to it are not applied) ----";

/// Where a user's post-save hook lives, if they have one.
fn post_save_hook_path() -> Option<std::path::PathBuf> {
    let mut path = dirs::home_dir()?;
    path.push(".finch/hooks/post-save");
    path.exists().then_some(path)
}

/// Bound on a post-save hook. A hook is the user's own program, but it must
/// not be able to hold the tool open indefinitely.
const POST_SAVE_HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run one post-save hook to completion and report anything worth telling the
/// caller about.
///
/// Every stream is redirected. The hook runs after the terminal has been put
/// back into raw mode, so inherited stdout/stderr would be painted straight
/// into the live frame, and an inherited stdin would race the key reader for
/// the user's keystrokes. The child is awaited rather than dropped, so an
/// accepted edit cannot leave a zombie behind.
async fn run_hook(hook: &std::path::Path, file_path: &str) -> Option<String> {
    let mut command = tokio::process::Command::new(hook);
    command
        .arg(file_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    match tokio::time::timeout(POST_SAVE_HOOK_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => None,
        Ok(Ok(output)) => {
            let mut detail = String::from_utf8_lossy(&output.stderr).into_owned();
            if detail.trim().is_empty() {
                detail = String::from_utf8_lossy(&output.stdout).into_owned();
            }
            Some(format!(
                "post-save hook {} exited {}: {}",
                hook.display(),
                output.status,
                crate::cli::diff::sanitize_multiline(detail.trim())
            ))
        }
        Ok(Err(error)) => Some(format!(
            "post-save hook {} could not be started: {}",
            hook.display(),
            error
        )),
        Err(_) => Some(format!(
            "post-save hook {} did not finish within {} seconds and was terminated",
            hook.display(),
            POST_SAVE_HOOK_TIMEOUT.as_secs()
        )),
    }
}

/// Run `~/.finch/hooks/post-save <file_path>` if that script exists.
async fn run_post_save_hook(file_path: &str) -> Option<String> {
    let hook = post_save_hook_path()?;
    run_hook(&hook, file_path).await
}

/// What the user's saved review artifact says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewOutcome {
    /// Apply the edit exactly as it was presented.
    Apply,
    /// Do not apply; the user rejected it, cleared the artifact, or left an
    /// action Finch does not recognise.
    Cancel { reason: String },
    /// Do not apply; the user wants a different change and wrote why.
    Chat { context: String },
    /// Do not apply; the diff body was edited and no longer describes the
    /// change this tool call would perform.
    Modified,
}

/// What a single header line says about the user's decision.
///
/// `Some(Err(()))` is a *near miss*: a line that is plainly trying to be a
/// directive but is not one. A near miss must never be read as consent, so it
/// is reported rather than ignored. Recognises the same comment markers the
/// rest of the proposal protocol uses (`#`, `\`, `;;`), because a user who
/// has seen the Forth and Lisp artifacts will reasonably type those here.
fn directive_on_line(line: &str) -> Option<Result<&str, ()>> {
    let trimmed = line.trim();
    let body = trimmed
        .strip_prefix(";;")
        .or_else(|| trimmed.strip_prefix('#'))
        .or_else(|| trimmed.strip_prefix('\\'))?
        .trim_start();
    let Some(rest) = body.strip_prefix("finch:") else {
        // Only a line that *starts* with the reserved word is a near miss;
        // quoted description text (`> finch: ...`) is ordinary prose.
        return (body.starts_with("finch") && body.contains("action=")).then_some(Err(()));
    };
    match rest.trim_start().strip_prefix("action=") {
        Some(action) => Some(Ok(action.trim())),
        None => Some(Err(())),
    }
}

/// Render one header line, keeping model-supplied text out of the decision
/// protocol.
///
/// `file_path` reaches the description straight from the model, so an
/// unescaped description line could otherwise write `finch: action=cancel`
/// into the header and decide the human's review for them. The escape is
/// decided by asking `directive_on_line` itself, so the two can never drift
/// apart: anything the parser would read as a directive is quoted, and a
/// quoted line is never read as a directive.
fn header_comment(line: &str) -> String {
    let clean = crate::cli::diff::sanitize_terminal(line);
    let candidate = format!("# {}", clean);
    if directive_on_line(&candidate).is_some() {
        format!("# > {}\n", clean)
    } else {
        format!("{}\n", candidate)
    }
}

/// Build the artifact opened in `$EDITOR`: a comment header carrying the
/// action directive, then the unified diff of the proposed change.
///
/// The diff is the reviewed content. It is never executed, never encoded, and
/// never passed through a shell, so hostile file content cannot escape it.
fn build_review_artifact(description: &str, diff: &str) -> String {
    let mut out = String::new();
    out.push_str("# Finch proposal. Read the diff below, then save and quit to apply it.\n");
    out.push_str("# To reject it or ask for something different, change the value on the\n");
    out.push_str("# next line to cancel or chat, then save and quit.\n");
    out.push_str("# finch: action=execute\n");
    out.push_str("#   execute = apply it   cancel = reject it   chat = ask for a change\n");
    if let Some(hook) = post_save_hook_path() {
        out.push_str(&header_comment(&format!(
            "Applying also runs your post-save hook {}.",
            hook.display()
        )));
    }
    out.push_str("#\n");
    for line in description.lines() {
        out.push_str(&header_comment(line));
    }
    out.push_str(REVIEW_DIFF_MARKER);
    out.push('\n');
    out.push_str(diff);
    if !diff.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Compare artifact bodies without failing over structural diff whitespace.
///
/// `to_unified` writes a blank context line as a single space. Editors commonly
/// strip that structural marker, so canonicalize exactly that one case. Never
/// trim a changed or non-blank context line: its trailing bytes are content,
/// and treating an editor-stripped line as equal would apply bytes the human
/// no longer saw. `str::lines` already makes the final artifact newline
/// immaterial.
fn normalize_body(body: &str) -> String {
    body.lines()
        .map(|line| if line == " " { "" } else { line })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read the user's decision out of the artifact they saved.
///
/// The action directive is read from the header only — the region above
/// `REVIEW_DIFF_MARKER`. File content below it is data: a reviewed file that
/// happens to contain the literal text `# finch: action=cancel` must not be
/// able to cancel or redirect the user's own decision.
fn split_review_artifact(artifact: &str) -> (String, Option<String>) {
    let mut header = String::new();
    let mut body = String::new();
    let mut in_body = false;
    for line in artifact.lines() {
        if !in_body {
            if line == REVIEW_DIFF_MARKER {
                in_body = true;
                continue;
            }
            header.push_str(line);
            header.push('\n');
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }
    (header, in_body.then_some(body))
}

fn parse_review_artifact(returned: &str, expected: &str) -> ReviewOutcome {
    let (header, returned_body) = split_review_artifact(returned);
    let Some(body) = returned_body else {
        return ReviewOutcome::Cancel {
            reason: "the review marker was removed, so the decision could not be \
                     separated from the diff"
                .to_string(),
        };
    };
    let (_expected_header, expected_body) = split_review_artifact(expected);
    let expected_diff = expected_body.unwrap_or_default();
    let expected_diff = expected_diff.as_str();

    match header_decision(&header) {
        HeaderDecision::Cancel(reason) => ReviewOutcome::Cancel { reason },
        HeaderDecision::Chat => ReviewOutcome::Chat {
            context: user_prose(returned, expected),
        },
        HeaderDecision::Execute => {
            if normalize_body(&body) == normalize_body(expected_diff) {
                ReviewOutcome::Apply
            } else {
                ReviewOutcome::Modified
            }
        }
    }
}

/// The decision the header as a whole expresses.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeaderDecision {
    Execute,
    Cancel(String),
    Chat,
}

/// Reduce every directive in the header to one decision, failing closed.
///
/// A rejection typed anywhere wins over an approval left elsewhere, an
/// unrecognised action is a rejection, a near-miss line is a rejection, and a
/// header with no directive at all is a rejection. Only an unambiguous
/// `action=execute` applies anything: a user's typo must cost them a second
/// look, never an unintended write.
fn header_decision(header: &str) -> HeaderDecision {
    let mut execute = false;
    let mut chat = false;
    for line in header.lines() {
        match directive_on_line(line) {
            None => {}
            Some(Err(())) => {
                return HeaderDecision::Cancel(format!(
                    "the line {:?} looks like an action directive but is not one, so Finch \
                     did not assume it meant approval",
                    crate::cli::diff::sanitize_terminal(line.trim())
                ))
            }
            Some(Ok("cancel")) => {
                return HeaderDecision::Cancel("the artifact said action=cancel".to_string())
            }
            Some(Ok("chat")) => chat = true,
            Some(Ok("execute")) => execute = true,
            Some(Ok(other)) => {
                return HeaderDecision::Cancel(format!(
                    "the artifact asked for the unrecognised action {:?}",
                    crate::cli::diff::sanitize_terminal(other)
                ))
            }
        }
    }
    if chat {
        return HeaderDecision::Chat;
    }
    if execute {
        return HeaderDecision::Execute;
    }
    HeaderDecision::Cancel(
        "the action directive was removed, so no approval was recorded".to_string(),
    )
}

/// The lines the user added to the header, without Finch's own boilerplate or
/// the diff.
///
/// A chat request is quoted back to the model, and returning the whole
/// artifact would send the diff too — which the transcript renderer treats as
/// a successful edit, making a refusal look like an application.
fn user_prose(returned: &str, expected: &str) -> String {
    let generated: Vec<&str> = expected.lines().collect();
    let prose: Vec<String> = returned
        .lines()
        // Anything Finch itself wrote is not the user speaking. Notes are
        // taken from anywhere in the artifact, not only the header: the
        // bottom of the file is just as natural a place to type them.
        .filter(|line| !generated.contains(line))
        .filter(|line| directive_on_line(line).is_none())
        .filter(|line| *line != REVIEW_DIFF_MARKER)
        // Never carry diff structure into the result: `tool_display` reads a
        // `--- ` line as a rendered file change, which would make a refusal
        // look like a successful edit.
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

/// Read the target as text, refusing rather than mangling non-UTF-8 content.
///
/// A lossy read would show the reviewer characters that are not in the file
/// and then write those characters back, so the edit is declined instead.
fn read_text(file_path: &str) -> Result<String> {
    match fs::read_to_string(file_path) {
        Ok(text) => Ok(text),
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => Err(anyhow::anyhow!(
            "{} is not valid UTF-8 text, so the change cannot be shown as a reviewable diff.\n\
             Finch declines to edit it rather than write back a lossy conversion.",
            file_path
        )),
        Err(error) => Err(error).with_context(|| format!("Failed to read file: {}", file_path)),
    }
}

/// Refuse text that the shared terminal-oriented diff model rewrites.
///
/// `FileDiff` is intentionally safe to print directly in a terminal: it
/// expands tabs, strips ANSI escape sequences, and substitutes other control
/// characters. Those transformations are appropriate for a transcript, but
/// not for an approval artifact whose bytes authorize a file write. Until the
/// shared diff model has a lossless editor representation, fail closed and
/// name the first byte the reviewer could not see faithfully.
fn ensure_review_representation_is_exact(label: &str, text: &str) -> Result<()> {
    // NUL selects FileDiff's existing binary path. That path does not pretend
    // to show bytes: its review artifact explicitly says the content is binary
    // and reports both sizes, which is the agreed review contract for binary
    // edits. This guard applies only to text diffs that otherwise look exact.
    if text.contains('\0') {
        return Ok(());
    }
    for (byte_offset, character) in text.char_indices() {
        let allowed_line_ending = character == '\n'
            || (character == '\r' && text.as_bytes().get(byte_offset + 1) == Some(&b'\n'));
        if allowed_line_ending || !character.is_control() {
            continue;
        }
        let name = match character {
            '\t' => "TAB".to_string(),
            '\u{1b}' => "ESCAPE".to_string(),
            other => format!("control character U+{:04X}", other as u32),
        };
        anyhow::bail!(
            "Cannot open a byte-faithful edit review: the {label} contains {name} at byte \
             {byte_offset}. The review diff would rewrite or hide that byte, so Finch refused \
             the edit before opening $VISUAL/$EDITOR."
        );
    }
    Ok(())
}

/// Header lines stating everything the diff below does not faithfully show.
///
/// `FileDiff` is bounded: it gives up on very large or pathological inputs,
/// truncates over-long lines, and refuses to line-diff binary content. An
/// approval surface must never let someone approve a change they were shown
/// only part of, so each of those cases is stated *above* the diff — where
/// the reader meets it before deciding — rather than only in the footer.
fn fidelity_notes(diff: &FileDiff, original: &str, planned: &str) -> Vec<String> {
    let mut notes = Vec::new();
    if diff.binary {
        notes.push(
            "WARNING: binary content — this change cannot be shown as a line diff.".to_string(),
        );
        notes.push(format!(
            "Size: {} bytes -> {} bytes ({}{} bytes).",
            original.len(),
            planned.len(),
            if planned.len() >= original.len() {
                "+"
            } else {
                "-"
            },
            planned.len().abs_diff(original.len()),
        ));
    }
    if let Some(elided) = &diff.elided {
        notes.push(format!("NOTE: {}", elided));
    }
    if !diff.counts_are_exact() {
        notes.push(
            "WARNING: this diff is incomplete — the line counts are approximate and part of \
             the change is not shown below. Do not approve unless you can see all of it."
                .to_string(),
        );
    } else if !diff.binary {
        notes.push(format!("+{} -{} lines.", diff.added(), diff.removed()));
    }
    let longest = original
        .lines()
        .chain(planned.lines())
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    if longest > MAX_DIFF_LINE_CHARS {
        notes.push(format!(
            "NOTE: a line of {} characters exceeds the {} character display limit and is shown \
             cut off, marked with … [line truncated].",
            longest, MAX_DIFF_LINE_CHARS
        ));
    }
    notes
}

/// Reject a diff whose rendering does not actually show the change.
///
/// INTERIM GUARD for #482 (`src/cli/diff.rs` can render a diff that omits
/// part of the change without saying so). Two known ways it happens:
///
/// * `to_unified` ends with `bound_rendered`, which cuts the rendered string
///   at `MAX_RENDER_CHARS` and updates neither `elided` nor the exactness
///   flags — so asking the struct whether it is complete returns "yes" while
///   whole hunks are missing;
/// * `FileDiff::from_texts` renders with `similar` and re-parses its own
///   output, so a removed line beginning `-- ` (SQL, Lua, Haskell, Ada) is
///   re-read as a `--- ` file header and disappears from the diff.
///
/// Both let a human approve, in good faith, a change the artifact did not
/// show. Until #482 fixes the module, the tool refuses rather than presents
/// an unfaithful view. This checks the rendering against itself — each hunk
/// header states how many old and new lines its body holds — so it needs no
/// second diff engine and has no timing dependence.
fn verify_render_is_faithful(
    file_diff: &FileDiff,
    rendered: &str,
    original: &str,
    planned: &str,
) -> Result<()> {
    let interim = "This is an interim refusal for issue #482 (the shared diff renderer can \
                   silently drop part of a change); it is not a problem with the file.";
    if file_diff.binary {
        // Binary content is presented as prose with byte sizes, which is a
        // complete description rather than a partial diff.
        return Ok(());
    }
    if rendered
        .lines()
        .any(|line| line == "# finch: diff rendering truncated")
    {
        anyhow::bail!(
            "Refusing to edit {}: the diff is too large to display in full, so the review \
             would have shown only part of the change.\nMake a smaller edit.\n{}",
            file_diff.display_path(),
            interim
        );
    }

    let mut hunks = 0usize;
    let mut changed_lines = 0usize;
    let mut pending: Option<(usize, usize, usize, usize)> = None; // old_want, new_want, old_seen, new_seen
    let finish = |pending: Option<(usize, usize, usize, usize)>| -> Result<()> {
        if let Some((old_want, new_want, old_seen, new_seen)) = pending {
            if old_want != old_seen || new_want != new_seen {
                anyhow::bail!(
                    "Refusing to edit {}: the rendered diff does not match its own hunk header \
                     (it claims {} old and {} new lines but shows {} and {}), so the review \
                     would have hidden part of the change.\n{}",
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
            // "\ No newline at end of file" belongs to neither side, and a
            // "# finch:" footer note ends the hunk body.
            _ => {}
        }
    }
    finish(pending.take())?;

    if original != planned && (hunks == 0 || changed_lines == 0) {
        anyhow::bail!(
            "Refusing to edit {}: the change could not be rendered as a reviewable diff, so \
             the review would have shown nothing to approve.\nMake a smaller edit.\n{}",
            file_diff.display_path(),
            interim
        );
    }
    Ok(())
}

/// Old-side and new-side line counts declared by a `@@ -a,b +c,d @@` header.
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

/// Validate the replacement and produce the file's new content.
///
/// Shared by the interactive and non-interactive paths so an ambiguous or
/// absent `old_string` is refused identically, and — interactively — before a
/// human is asked to review anything.
fn plan_edit(
    original: &str,
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Result<String> {
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
    Ok(if replace_all {
        original.replace(old_string, new_string)
    } else {
        original.replacen(old_string, new_string, 1)
    })
}

/// Write the planned content and run the post-save hook.
async fn commit_edit(file_path: &str, new_content: &str) -> Result<Option<String>> {
    fs::write(file_path, new_content)
        .with_context(|| format!("Failed to write file: {}", file_path))?;
    Ok(run_post_save_hook(file_path).await)
}

/// Interactive flow: show the human the diff, then apply it here.
///
/// `open_editor` receives the complete artifact bytes and returns what the
/// user saved, or `None` if they cleared it. Injecting it lets a regression
/// assert on exactly what a human would see.
async fn review_and_apply_edit<F, Fut>(
    file_path: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
    open_editor: F,
) -> Result<String>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<Option<String>>>,
{
    let original = read_text(file_path)?;
    // Refuse an impossible or ambiguous edit before spending the user's
    // attention on a review.
    let planned = plan_edit(&original, file_path, old_string, new_string, replace_all)?;

    ensure_review_representation_is_exact("current file", &original)?;
    ensure_review_representation_is_exact("proposed file", &planned)?;

    let file_diff = FileDiff::from_texts(file_path, &original, &planned);
    let diff = file_diff.to_unified();
    verify_render_is_faithful(&file_diff, &diff, &original, &planned)?;
    let mut description = format!("Edit {}", file_path);
    for note in fidelity_notes(&file_diff, &original, &planned) {
        description.push('\n');
        description.push_str(&note);
    }
    let artifact = build_review_artifact(&description, &diff);

    let Some(returned) = open_editor(artifact.clone()).await? else {
        return Ok("Edit aborted by user.".to_string());
    };

    match parse_review_artifact(&returned, &artifact) {
        ReviewOutcome::Cancel { reason } => Ok(format!("Edit aborted by user: {}.", reason)),
        ReviewOutcome::Chat { context } => Ok(format!(
            "Edit not applied. The user asked for a different change instead of approving:\n{}",
            context
        )),
        ReviewOutcome::Modified => Ok(format!(
            "Edit not applied: the proposed diff for {} was edited during review.\n\
             That diff is a read-only view of this tool call — the change is applied from \
             old_string/new_string, so edits to the diff would not have been what was written. \
             Re-issue the edit with the content you want, or set `# finch: action=chat` to \
             describe it.",
            file_path
        )),
        ReviewOutcome::Apply => {
            // The reviewed diff described one specific starting state. If the
            // file moved underneath the review, applying it would write
            // something the human never saw.
            let current = fs::read_to_string(file_path)
                .with_context(|| format!("Failed to re-read file: {}", file_path))?;
            if current != original {
                return Ok(format!(
                    "Edit not applied: {} changed while the diff was under review. \
                     Re-read the file and propose the edit again.",
                    file_path
                ));
            }
            match commit_edit(file_path, &planned).await? {
                None => Ok(diff),
                Some(hook_note) => Ok(format!("{}\n{}", diff, hook_note)),
            }
        }
    }
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

        // Interactive: review the diff in $EDITOR before applying.
        if std::io::stdin().is_terminal() {
            return review_and_apply_edit(
                file_path,
                old_string,
                new_string,
                replace_all,
                |artifact| async move { open_review_artifact(&artifact).await },
            )
            .await;
        }

        // Non-interactive (tests, daemon): apply directly.
        let original = read_text(file_path)?;
        let new_content = plan_edit(&original, file_path, old_string, new_string, replace_all)?;
        let hook_note = commit_edit(file_path, &new_content).await?;
        let diff = FileDiff::from_texts(file_path, &original, &new_content).to_unified();
        Ok(match hook_note {
            None => diff,
            Some(note) => format!("{}\n{}", diff, note),
        })
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
    use std::sync::{Arc, Mutex};

    fn temp_file(name: &str, contents: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(name);
        fs::write(&path, contents).expect("seed file");
        let display = path.to_string_lossy().into_owned();
        (dir, display)
    }

    /// Capture the artifact and reply with `reply(artifact)`.
    fn capturing_editor(
        seen: Arc<Mutex<Option<String>>>,
        reply: impl Fn(String) -> Option<String> + Send + 'static,
    ) -> impl FnOnce(
        String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
    > {
        move |artifact: String| {
            *seen.lock().expect("artifact slot") = Some(artifact.clone());
            let answer = reply(artifact);
            Box::pin(async move { Ok(answer) })
        }
    }

    /// The invariant of #437, asserted as a property rather than a byte
    /// string: whatever the artifact is, a human must be able to read the
    /// change out of it, and it must not be a program.
    fn assert_readable_diff(artifact: &str, must_contain: &[&str], context: &str) {
        for forbidden in [
            "base64",
            "b64decode",
            "python3",
            "PYEOF",
            "#!/bin/bash",
            "import ",
        ] {
            assert!(
                !artifact.contains(forbidden),
                "$EDITOR review artifact must carry no encoded payload and no interpreter \
                 invocation ({context}), but it contains {forbidden:?}.\n\
                 The human would have opened this:\n{artifact}"
            );
        }
        for expected in ["--- ", "+++ ", "@@ "] {
            assert!(
                artifact.lines().any(|line| line.starts_with(expected)),
                "$EDITOR review artifact must be a unified diff ({context}), but it is missing \
                 the {expected:?} marker.\nThe human would have opened this:\n{artifact}"
            );
        }
        for expected in must_contain {
            assert!(
                artifact.contains(expected),
                "$EDITOR review artifact must show the literal changed text ({context}), but it \
                 is missing {expected:?}.\nThe human would have opened this:\n{artifact}"
            );
        }
        assert_body_is_only_diff_lines(artifact, context);
    }

    /// Reply with `edit(artifact)` and record whether the editor was opened.
    fn replying_editor(
        edit: impl Fn(String) -> Option<String> + Send + 'static,
    ) -> impl FnOnce(
        String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
    > {
        move |artifact: String| {
            let answer = edit(artifact);
            Box::pin(async move { Ok(answer) })
        }
    }

    /// The body below the marker must consist of diff lines and nothing else.
    ///
    /// This is the implementation-independent form of "it is a diff, not a
    /// program": a program's lines (`import base64, sys`, `python3 << 'PYEOF'`,
    /// `path = "..."`) cannot satisfy it, and no future encoding scheme can
    /// sneak past it the way a fixed forbidden-substring list could.
    fn assert_body_is_only_diff_lines(artifact: &str, context: &str) {
        let (_, body) = split_review_artifact(artifact);
        let body = body.unwrap_or_else(|| {
            panic!("artifact has no review marker ({context}).\nArtifact:\n{artifact}")
        });
        for line in body.lines() {
            let is_diff_line = line.is_empty()
                || line.starts_with(' ')
                || line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with("@@ ")
                || line.starts_with('\\')
                || line.starts_with("Binary files ")
                || line.starts_with("# finch: ");
            assert!(
                is_diff_line,
                "every line below the review marker must be part of the diff ({context}), but \
                 {line:?} is not.\nThe human would have opened this:\n{artifact}"
            );
        }
    }

    /// Production boundary: drive `review_and_apply_edit` — the function the
    /// interactive `edit` path calls — and assert on the exact bytes handed
    /// to the editor.
    #[tokio::test]
    async fn test_editor_review_artifact_is_a_readable_diff() {
        let (_dir, path) = temp_file("demo.rs", "fn main() {\n    let old = 1;\n}\n");
        let seen = Arc::new(Mutex::new(None));
        let result = review_and_apply_edit(
            &path,
            "let old = 1;",
            "let new = 2;",
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_readable_diff(
            &artifact,
            &["-    let old = 1;", "+    let new = 2;"],
            "approved edit",
        );
        assert!(
            artifact.contains(REVIEW_DIFF_MARKER),
            "artifact must mark where the review-only diff begins.\nArtifact:\n{artifact}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fn main() {\n    let new = 2;\n}\n",
            "approving the reviewed diff must apply exactly that change; tool said: {result}"
        );
    }

    /// Same invariant one layer lower: the bytes actually written to the temp
    /// file that `$EDITOR` is launched on.
    #[tokio::test]
    async fn test_editor_temp_file_contains_the_readable_diff() {
        let (_dir, path) = temp_file("demo.txt", "alpha\nbravo\ncharlie\n");
        let original = fs::read_to_string(&path).unwrap();
        let planned = plan_edit(&original, &path, "bravo", "BRAVO", false).unwrap();
        let diff = crate::cli::diff::FileDiff::from_texts(&path, &original, &planned).to_unified();
        let artifact = build_review_artifact("Edit demo.txt", &diff);

        let opened = Arc::new(Mutex::new(None));
        let recorder = opened.clone();
        let returned = crate::tools::implementations::propose::open_review_artifact_with(
            &artifact,
            move |file: &std::path::Path| {
                let on_disk = std::fs::read_to_string(file).expect("editor reads the artifact");
                let suffix = file
                    .extension()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mode = std::fs::metadata(file).unwrap().permissions().mode();
                    assert_eq!(
                        mode & 0o111,
                        0,
                        "the review artifact must not be executable; mode {mode:o} on {file:?}"
                    );
                }
                *recorder.lock().unwrap() = Some((on_disk, suffix));
                Ok(std::os::unix::process::ExitStatusExt::from_raw(0))
            },
        )
        .await
        .expect("editor lifecycle");

        let (on_disk, suffix) = opened.lock().unwrap().clone().expect("editor was launched");
        assert_eq!(
            suffix, "diff",
            "the artifact must be named so an editor highlights it as a diff, got {suffix:?}"
        );
        assert_readable_diff(&on_disk, &["-bravo", "+BRAVO"], "bytes on disk for $EDITOR");
        assert_eq!(
            returned.as_deref(),
            Some(on_disk.as_str()),
            "an unmodified save must return exactly the reviewed artifact"
        );
    }

    /// Hostile content: shell metacharacters must be shown literally, and
    /// nothing on this path may execute them.
    #[tokio::test]
    async fn test_hostile_shell_content_is_shown_literally_and_never_executed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("PWNED");
        let path_display = dir.path().join("hostile.sh").to_string_lossy().into_owned();
        fs::write(&path_display, "placeholder\n").unwrap();

        let hostile = format!(
            "\"quoted\" 'single' `touch {m}` $(touch {m}) ${{HOME}} ; rm -rf / && touch {m} | tee {m}",
            m = marker.display()
        );
        let seen = Arc::new(Mutex::new(None));
        review_and_apply_edit(
            &path_display,
            "placeholder",
            &hostile,
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_readable_diff(&artifact, &["-placeholder"], "hostile shell content");
        assert!(
            artifact.contains("$(touch") && artifact.contains("`touch"),
            "hostile metacharacters must appear literally in the reviewed diff, not encoded \
             or stripped.\nArtifact:\n{artifact}"
        );
        assert!(
            !marker.exists(),
            "no part of the review or apply path may execute reviewed content, but {:?} was \
             created.\nArtifact:\n{artifact}",
            marker
        );
        assert_eq!(
            fs::read_to_string(&path_display).unwrap(),
            format!("{hostile}\n"),
            "the applied file must contain the hostile text verbatim"
        );
    }

    /// Newlines, CRLF and non-ASCII survive review and application.
    #[tokio::test]
    async fn test_crlf_newlines_and_non_ascii_stay_readable() {
        let (_dir, path) = temp_file("i18n.txt", "one\r\ntwo\r\nthree\r\n");
        let replacement = "deux — naïve café\r\n日本語 🦀";
        let seen = Arc::new(Mutex::new(None));
        review_and_apply_edit(
            &path,
            "two",
            replacement,
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_readable_diff(
            &artifact,
            &["日本語 🦀", "naïve café"],
            "CRLF and non-ASCII",
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("one\r\n{replacement}\r\nthree\r\n"),
            "review must not rewrite the bytes that get applied"
        );
    }

    /// A very long line is truncated *for display* with a visible marker —
    /// never encoded — and the full line is still what gets written.
    #[tokio::test]
    async fn test_very_long_line_is_visibly_truncated_not_encoded() {
        let (_dir, path) = temp_file("long.txt", "short\n");
        let long: String = "Z".repeat(4000);
        let seen = Arc::new(Mutex::new(None));
        review_and_apply_edit(
            &path,
            "short",
            &long,
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        for forbidden in ["base64", "b64decode", "python3", "PYEOF"] {
            assert!(
                !artifact.contains(forbidden),
                "a long line must not push the artifact back to an encoded payload, but it \
                 contains {forbidden:?}.\nArtifact:\n{artifact}"
            );
        }
        assert!(
            artifact.contains("display limit"),
            "an over-long line must be declared in the header, where the reviewer meets it \
             before deciding.\nArtifact:\n{artifact}"
        );
        assert!(
            artifact.contains("[line truncated]"),
            "an over-long line must be truncated with a visible marker so the reader knows \
             the view is partial.\nArtifact:\n{artifact}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{long}\n"),
            "display truncation must never truncate what is applied"
        );
    }

    /// Reviewed file content that looks like the decision protocol must not
    /// be able to decide for the user.
    #[tokio::test]
    async fn test_file_content_cannot_forge_the_action_directive() {
        let (_dir, path) = temp_file(
            "trap.txt",
            "# finch: action=cancel\nkeep me\n# finch: action=chat\n",
        );
        let seen = Arc::new(Mutex::new(None));
        let result = review_and_apply_edit(
            &path,
            "keep me",
            "changed",
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# finch: action=cancel\nchanged\n# finch: action=chat\n",
            "a directive inside reviewed file content must not cancel or divert the user's \
             own approval; tool said: {result}\nArtifact:\n{artifact}"
        );
    }

    /// The description carries a model-supplied path; it must not be able to
    /// write a directive into the header either.
    #[test]
    fn test_description_cannot_forge_the_action_directive() {
        let artifact = build_review_artifact(
            "Edit /tmp/x\nfinch: action=cancel\n# finch: action=chat",
            "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n",
        );
        assert_eq!(
            parse_review_artifact(&artifact, &artifact),
            ReviewOutcome::Apply,
            "a description line must not be readable as the reserved action directive.\n\
             Artifact:\n{artifact}"
        );
    }

    #[tokio::test]
    async fn test_cancel_directive_leaves_the_file_untouched() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(&path, "before", "after", false, |artifact: String| {
            let edited = artifact.replace("action=execute", "action=cancel");
            Box::pin(async move { Ok(Some(edited)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect("interactive edit");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "action=cancel must not write; tool said: {result}"
        );
        assert!(
            result.contains("aborted"),
            "a cancelled edit must say so, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_chat_directive_leaves_the_file_untouched() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(&path, "before", "after", false, |artifact: String| {
            let edited = format!(
                "{}\n# please rename it instead\n",
                artifact.replace("action=execute", "action=chat")
            );
            Box::pin(async move { Ok(Some(edited)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect("interactive edit");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "action=chat must not write; tool said: {result}"
        );
        assert!(
            result.contains("different change") && result.contains("rename it instead"),
            "a chat request must be reported with the user's own words, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_edited_diff_body_is_refused_rather_than_silently_ignored() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(&path, "before", "after", false, |artifact: String| {
            let edited = artifact.replace("+after", "+something else entirely");
            Box::pin(async move { Ok(Some(edited)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect("interactive edit");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "a hand-edited review diff must not be applied as if it were approved; \
             tool said: {result}"
        );
        assert!(
            result.contains("read-only view"),
            "the refusal must explain that the diff is a view, not the change, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_emptied_artifact_aborts() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(&path, "before", "after", false, |_artifact: String| {
            Box::pin(async move { Ok(None) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect("interactive edit");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "clearing the artifact must abort; tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_file_changed_under_review_is_not_clobbered() {
        let (_dir, path) = temp_file("race.txt", "before\n");
        let racing = path.clone();
        let result =
            review_and_apply_edit(&path, "before", "after", false, move |artifact: String| {
                // The human is still reading when someone else saves the file.
                fs::write(&racing, "somebody else's work\n").unwrap();
                Box::pin(async move { Ok(Some(artifact)) })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                    >
            })
            .await
            .expect("interactive edit");

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "somebody else's work\n",
            "an edit reviewed against stale content must not overwrite a concurrent change; \
             tool said: {result}"
        );
        assert!(
            result.contains("changed while the diff was under review"),
            "the refusal must name the reason, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_ambiguous_match_is_refused_before_the_editor_opens() {
        let (_dir, path) = temp_file("dup.txt", "dup\ndup\n");
        let opened = Arc::new(Mutex::new(false));
        let flag = opened.clone();
        let error = review_and_apply_edit(&path, "dup", "x", false, move |artifact: String| {
            *flag.lock().unwrap() = true;
            Box::pin(async move { Ok(Some(artifact)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect_err("an ambiguous edit must fail");

        assert!(
            !*opened.lock().unwrap(),
            "an edit that cannot be applied must not cost the user a review; error: {error}"
        );
        assert!(
            error.to_string().contains("appears 2 times"),
            "the error must name the ambiguity, got {error}"
        );
    }

    /// Binary content cannot be shown as a line diff, so the artifact must
    /// say so in prose with the sizes involved — never a blank-looking diff
    /// the reviewer would approve as "no change".
    #[tokio::test]
    async fn test_binary_content_is_described_not_diffed() {
        let (_dir, path) = temp_file("data.bin", "header\u{0}payload\n");
        let original = fs::read_to_string(&path).unwrap();
        let seen = Arc::new(Mutex::new(None));
        let result = review_and_apply_edit(
            &path,
            "payload",
            "PAYLOAD-EXTENDED",
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert!(
            artifact.contains("WARNING: binary content"),
            "binary content must be declared in the header rather than left to an empty-looking \
             diff.\nArtifact:\n{artifact}"
        );
        assert!(
            artifact.contains(&format!("Size: {} bytes", original.len())),
            "the binary description must state the sizes so the reviewer knows the scale of what \
             they are approving.\nArtifact:\n{artifact}"
        );
        assert!(
            !artifact.lines().any(|line| line.starts_with("@@ ")),
            "no hunk may be fabricated for binary content.\nArtifact:\n{artifact}"
        );
        for forbidden in ["base64", "b64decode", "python3", "PYEOF"] {
            assert!(
                !artifact.contains(forbidden),
                "binary content must not send the artifact back to an encoded payload, but it \
                 contains {forbidden:?}.\nArtifact:\n{artifact}"
            );
        }
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "header\u{0}PAYLOAD-EXTENDED\n",
            "approving the described binary change must apply exactly it; tool said: {result}"
        );
    }

    /// A change too large for `FileDiff` to render is refused outright. The
    /// reviewer is never handed a view that shows nothing to approve.
    #[tokio::test]
    async fn test_change_too_large_to_render_is_refused() {
        let bulk = "x\n".repeat(300_000);
        let (_dir, path) = temp_file("huge.txt", &format!("{bulk}MARKER\n"));
        let opened = Arc::new(Mutex::new(false));
        let flag = opened.clone();
        let error = review_and_apply_edit(&path, "MARKER", "REPLACED", false, move |a: String| {
            *flag.lock().unwrap() = true;
            Box::pin(async move { Ok(Some(a)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect_err("an unrenderable change must be refused");

        assert!(
            !*opened.lock().unwrap(),
            "a reviewer must not be shown a diff that omits the change; error: {error}"
        );
        assert!(
            error.to_string().contains("#482"),
            "the interim refusal must name the module defect it is standing in for, got: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{bulk}MARKER\n"),
            "a refused edit must leave the file untouched"
        );
    }

    /// The bounded-rendering warning still fires if the gate above is ever
    /// relaxed: `fidelity_notes` reports inexact counts from the struct.
    #[test]
    fn test_fidelity_notes_report_inexact_counts() {
        let bulk = "x\n".repeat(300_000);
        let old = format!("{bulk}MARKER\n");
        let new = format!("{bulk}REPLACED\n");
        let diff = FileDiff::from_texts("huge.txt", &old, &new);
        assert!(
            !diff.counts_are_exact(),
            "fixture must actually produce an inexact diff or the note below is untested"
        );
        let notes = fidelity_notes(&diff, &old, &new).join("\n");
        assert!(
            notes.contains("WARNING: this diff is incomplete"),
            "an inexact diff must be described as incomplete, got: {notes}"
        );
    }

    /// F1 (#482): `to_unified` cuts the rendered string at `MAX_RENDER_CHARS`
    /// without marking the struct inexact, so the reassuring branch would be
    /// emitted over a diff missing every changed line.
    #[tokio::test]
    async fn test_truncated_rendering_is_refused_not_presented_as_complete() {
        let filler = "A".repeat(340);
        let original: String = (0..500).map(|i| format!("{filler}{i}\n")).collect();
        let replacement: String = (0..500).map(|i| format!("B{filler}{i}\n")).collect();
        let (_dir, path) = temp_file("wide.txt", &original);
        let opened = Arc::new(Mutex::new(false));
        let flag = opened.clone();
        let error =
            review_and_apply_edit(&path, &original, &replacement, false, move |a: String| {
                *flag.lock().unwrap() = true;
                Box::pin(async move { Ok(Some(a)) })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                    >
            })
            .await
            .expect_err("a diff cut by the render bound must be refused");

        assert!(
            !*opened.lock().unwrap(),
            "the reviewer must not be shown a diff whose changed lines were cut away; \
             error: {error}"
        );
        assert!(
            error.to_string().contains("#482"),
            "the interim refusal must name the module defect, got: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            original,
            "a refused edit must leave the file untouched"
        );
    }

    /// F2 (#482): `FileDiff::from_texts` re-parses its own rendering, so a
    /// removed line beginning `-- ` is re-read as a `--- ` file header and
    /// disappears. Common in SQL, Lua, Haskell and Ada.
    #[tokio::test]
    async fn test_removed_line_starting_with_double_dash_is_refused() {
        let source = "SELECT 1;\n-- keep totals\n-- and averages\nSELECT 2;\n";
        let (_dir, path) = temp_file("q.sql", source);
        let opened = Arc::new(Mutex::new(false));
        let flag = opened.clone();
        let error = review_and_apply_edit(
            &path,
            "-- keep totals\n-- and averages\n",
            "",
            false,
            move |a: String| {
                *flag.lock().unwrap() = true;
                Box::pin(async move { Ok(Some(a)) })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                    >
            },
        )
        .await
        .expect_err("a diff that swallowed a removed line must be refused");

        assert!(
            !*opened.lock().unwrap(),
            "the reviewer must not be shown a diff missing the lines being deleted; \
             error: {error}"
        );
        assert!(
            error.to_string().contains("#482"),
            "the interim refusal must name the module defect, got: {error}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            source,
            "a refused edit must leave the file untouched"
        );
    }

    /// Non-UTF-8 content is refused, not lossily converted. A lossy read
    /// would show the reviewer characters that are not in the file and then
    /// write those characters back.
    #[tokio::test]
    async fn test_non_utf8_file_is_refused_not_mangled() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("raw.dat");
        let bytes: [u8; 5] = [0xff, 0xfe, b'a', b'b', 0x80];
        fs::write(&path, bytes).expect("seed file");
        let display = path.to_string_lossy().into_owned();

        let opened = Arc::new(Mutex::new(false));
        let flag = opened.clone();
        let error = review_and_apply_edit(&display, "ab", "cd", false, move |artifact: String| {
            *flag.lock().unwrap() = true;
            Box::pin(async move { Ok(Some(artifact)) })
                as std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<Option<String>>> + Send>,
                >
        })
        .await
        .expect_err("a non-UTF-8 target must be refused");

        assert!(
            !*opened.lock().unwrap(),
            "the reviewer must not be shown a lossy rendering of non-UTF-8 content; error: {error}"
        );
        assert!(
            error.to_string().contains("not valid UTF-8"),
            "the refusal must name the reason so it is actionable, got: {error}"
        );
        assert_eq!(
            fs::read(&path).unwrap(),
            bytes.to_vec(),
            "a refused edit must leave the bytes untouched"
        );
    }

    /// `replace_all` had no coverage anywhere in the repository: swapping
    /// `replace` for `replacen` survived as a mutant.
    #[tokio::test]
    async fn test_replace_all_changes_every_occurrence() {
        let (_dir, path) = temp_file("dup.txt", "a\nx\nb\nx\nc\n");
        let seen = Arc::new(Mutex::new(None));
        let result =
            review_and_apply_edit(&path, "x", "y", true, capturing_editor(seen.clone(), Some))
                .await
                .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "a\ny\nb\ny\nc\n",
            "replace_all must change every occurrence, not only the first; tool said: {result}"
        );
        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_eq!(
            artifact.matches("+y").count(),
            2,
            "the reviewed diff must show both replacements.\nArtifact:\n{artifact}"
        );
    }

    #[tokio::test]
    async fn test_replace_all_through_the_tool_boundary() {
        let (_dir, path) = temp_file("dup.txt", "x\nkeep\nx\n");
        let tool = EditTool;
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
        let out = tool
            .execute(
                serde_json::json!({
                    "file_path": path.clone(), "old_string": "x",
                    "new_string": "z", "replace_all": true
                }),
                &context,
            )
            .await
            .expect("replace_all edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "z\nkeep\nz\n",
            "replace_all must apply to every occurrence through the tool; tool said: {out}"
        );
    }

    /// A rejection typed anywhere must beat an approval left elsewhere. Taking
    /// only the last directive let a `cancel` typed at the top lose.
    #[tokio::test]
    async fn test_cancel_anywhere_in_the_header_wins() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(
            &path,
            "before",
            "after",
            false,
            replying_editor(|artifact| Some(format!("# finch: action=cancel\n{artifact}"))),
        )
        .await
        .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "a cancel written above the generated directive must still reject; \
             tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_removed_directive_fails_closed() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(
            &path,
            "before",
            "after",
            false,
            replying_editor(|artifact| {
                Some(
                    artifact
                        .lines()
                        .filter(|line| !line.contains("action=execute"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }),
        )
        .await
        .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "deleting the directive must not be read as approval; tool said: {result}"
        );
    }

    /// A typo in the directive *prefix* used to fail open while a typo in its
    /// *value* failed closed. Both must fail closed.
    #[tokio::test]
    async fn test_near_miss_directive_prefix_fails_closed() {
        for typo in ["# finch action=cancel", "# finchx: action=cancel"] {
            let (_dir, path) = temp_file("keep.txt", "before\n");
            let replacement = typo.to_string();
            let result = review_and_apply_edit(
                &path,
                "before",
                "after",
                false,
                replying_editor(move |artifact| {
                    Some(artifact.replace("# finch: action=execute", &replacement))
                }),
            )
            .await
            .expect("interactive edit");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "before\n",
                "the near-miss directive {typo:?} must not be read as approval; \
                 tool said: {result}"
            );
        }
    }

    /// The Forth and Lisp proposal artifacts of the same product use `\\` and
    /// `;;`, so a user will reasonably type those here.
    #[tokio::test]
    async fn test_sibling_comment_prefixes_are_honoured() {
        for prefix in [";; finch: action=cancel", "\\ finch: action=cancel"] {
            let (_dir, path) = temp_file("keep.txt", "before\n");
            let replacement = prefix.to_string();
            let result = review_and_apply_edit(
                &path,
                "before",
                "after",
                false,
                replying_editor(move |artifact| {
                    Some(artifact.replace("# finch: action=execute", &replacement))
                }),
            )
            .await
            .expect("interactive edit");
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                "before\n",
                "{prefix:?} must be honoured as a rejection; tool said: {result}"
            );
        }
    }

    #[tokio::test]
    async fn test_unrecognised_action_fails_closed() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(
            &path,
            "before",
            "after",
            false,
            replying_editor(|artifact| Some(artifact.replace("action=execute", "action=aplpy"))),
        )
        .await
        .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "an unrecognised action must not apply anything; tool said: {result}"
        );
        assert!(
            result.contains("aplpy"),
            "the refusal must quote what was actually written, got {result:?}"
        );
    }

    /// Editors that strip trailing whitespace on save are common, and
    /// `to_unified` writes a blank context line as a single space. Without
    /// tolerance every such user would be told they edited the diff.
    #[tokio::test]
    async fn test_trailing_whitespace_trimming_editor_still_approves() {
        let (_dir, path) = temp_file("blank.txt", "alpha\n\nbravo\n\ncharlie\n");
        let result = review_and_apply_edit(
            &path,
            "bravo",
            "BRAVO",
            false,
            replying_editor(|artifact| {
                Some(
                    artifact
                        .lines()
                        .map(str::trim_end)
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }),
        )
        .await
        .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "alpha\n\nBRAVO\n\ncharlie\n",
            "an untouched approval saved by a whitespace-trimming editor must still apply; \
             tool said: {result}"
        );
    }

    #[tokio::test]
    async fn test_missing_trailing_newline_still_approves() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(
            &path,
            "before",
            "after",
            false,
            replying_editor(|artifact| Some(artifact.trim_end().to_string())),
        )
        .await
        .expect("interactive edit");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "after\n",
            "an editor that drops the final newline must not be read as an edited diff; \
             tool said: {result}"
        );
    }

    /// End-to-end version of the forgery guard: the path really is
    /// model-controlled, and a filename may legally contain a newline.
    #[tokio::test]
    async fn test_model_supplied_path_cannot_forge_the_directive_end_to_end() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("evil\nfinch: action=cancel\nx.txt");
        fs::write(&path, "before\n").expect("seed file");
        let display = path.to_string_lossy().into_owned();

        let seen = Arc::new(Mutex::new(None));
        let result = review_and_apply_edit(
            &display,
            "before",
            "after",
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "after\n",
            "a directive smuggled through the model-supplied file_path must not decide the \
             user's review; tool said: {result}\nArtifact:\n{artifact}"
        );
    }

    /// A refused edit must not be reported to the model, or rendered in the
    /// transcript, as though it had been applied.
    #[tokio::test]
    async fn test_chat_returns_only_the_users_words() {
        let (_dir, path) = temp_file("keep.txt", "before\n");
        let result = review_and_apply_edit(
            &path,
            "before",
            "after",
            false,
            replying_editor(|artifact| {
                Some(format!(
                    "{}\n# please rename the function instead\n",
                    artifact.replace("action=execute", "action=chat")
                ))
            }),
        )
        .await
        .expect("interactive edit");

        assert!(
            result.contains("please rename the function instead"),
            "the user's request must reach the model, got {result:?}"
        );
        assert!(
            !result.lines().any(|line| line.starts_with("--- ")),
            "a refusal must not carry the diff, which the transcript renderer reads as a \
             successful edit, got:\n{result}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "before\n",
            "action=chat must not write"
        );
    }

    fn write_hook(dir: &tempfile::TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let hook = dir.path().join(name);
        fs::write(&hook, format!("#!/bin/sh\n{body}\n")).expect("write hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).expect("chmod hook");
        }
        hook
    }

    /// The hook runs after the terminal is back in raw mode, so inherited
    /// stdout would be painted straight into the live frame.
    #[tokio::test]
    async fn test_post_save_hook_output_is_captured_not_inherited() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hook = write_hook(&dir, "noisy", "echo out; echo problem >&2; exit 3");
        let note = run_hook(&hook, "/tmp/target").await;
        let note = note.expect("a failing hook must be reported rather than swallowed");
        assert!(
            note.contains("problem"),
            "the hook's own diagnostics must reach the caller instead of the terminal, \
             got: {note}"
        );
        assert!(
            note.contains("exit"),
            "the report must name the failure, got: {note}"
        );
    }

    /// An inherited stdin would race the key reader for the user's keystrokes,
    /// and a hook that waits for input would hang the tool.
    #[tokio::test]
    async fn test_post_save_hook_reading_stdin_does_not_hang() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hook = write_hook(&dir, "reader", "read line; echo \"read:$line\"");
        // Returning at all is the assertion: with inherited stdin this waits
        // forever. stdin is /dev/null, so the read sees EOF immediately.
        let note = run_hook(&hook, "/tmp/target").await;
        assert!(
            note.as_deref().map(|n| n.contains("exit")).unwrap_or(true),
            "a hook reading stdin must finish on EOF, got: {note:?}"
        );
    }

    #[tokio::test]
    async fn test_post_save_hook_that_cannot_start_is_reported() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hook = dir.path().join("not-executable");
        fs::write(&hook, "#!/bin/sh\ntrue\n").expect("write hook");
        let note = run_hook(&hook, "/tmp/target").await;
        let note = note.expect("a hook that cannot start must be reported, not swallowed");
        assert!(
            note.contains("could not be started"),
            "the report must say the hook never ran, got: {note}"
        );
    }

    #[tokio::test]
    async fn test_successful_hook_is_silent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let hook = write_hook(&dir, "quiet", "exit 0");
        assert_eq!(
            run_hook(&hook, "/tmp/target").await,
            None,
            "a hook that succeeds must add nothing to the tool result"
        );
    }

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
            effect_audit: None,
            poset: None,
        };
        let result = tool.execute(input, &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }
}
