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

/// Render one header line, keeping model-supplied text out of the decision
/// protocol.
///
/// `file_path` reaches the description straight from the model, so an
/// unescaped description line could otherwise write `finch: action=cancel`
/// into the header and decide the human's review for them.
fn header_comment(line: &str) -> String {
    let clean = crate::cli::diff::sanitize_terminal(line);
    if clean.trim_start().starts_with("finch:") {
        format!("#  {}\n", clean)
    } else {
        format!("# {}\n", clean)
    }
}

/// Build the artifact opened in `$EDITOR`: a comment header carrying the
/// action directive, then the unified diff of the proposed change.
///
/// The diff is the reviewed content. It is never executed, never encoded, and
/// never passed through a shell, so hostile file content cannot escape it.
fn build_review_artifact(description: &str, diff: &str) -> String {
    let mut out = String::new();
    out.push_str("# Finch proposal: save and quit to apply this change.\n");
    out.push_str(
        "# Reject it with action=cancel, or ask for a different change with action=chat.\n",
    );
    out.push_str("# finch: action=execute\n");
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

/// Compare artifact bodies without failing over an editor's line-ending or
/// trailing-newline conventions.
fn normalize_body(body: &str) -> String {
    body.replace("\r\n", "\n")
        .trim_end_matches('\n')
        .to_string()
}

/// Read the user's decision out of the artifact they saved.
///
/// The action directive is read from the header only — the region above
/// `REVIEW_DIFF_MARKER`. File content below it is data: a reviewed file that
/// happens to contain the literal text `# finch: action=cancel` must not be
/// able to cancel or redirect the user's own decision.
fn parse_review_artifact(artifact: &str, expected_diff: &str) -> ReviewOutcome {
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
    if !in_body {
        return ReviewOutcome::Cancel {
            reason: "the review marker was removed, so the decision could not be \
                     separated from the diff"
                .to_string(),
        };
    }

    let action = last_action(&header);
    match action.as_deref() {
        Some("cancel") => ReviewOutcome::Cancel {
            reason: "the user set action=cancel".to_string(),
        },
        Some("chat") => ReviewOutcome::Chat {
            context: crate::cli::diff::sanitize_multiline(artifact),
        },
        Some(other) if other != "execute" => ReviewOutcome::Cancel {
            reason: format!(
                "the artifact requested the unrecognised action {:?}",
                crate::cli::diff::sanitize_terminal(other)
            ),
        },
        _ => {
            if normalize_body(&body) == normalize_body(expected_diff) {
                ReviewOutcome::Apply
            } else {
                ReviewOutcome::Modified
            }
        }
    }
}

/// The last `# finch: action=...` directive in the header, if any.
fn last_action(header: &str) -> Option<String> {
    header
        .lines()
        .filter_map(|line| {
            line.trim()
                .strip_prefix("# finch:")
                .and_then(|directive| directive.trim().strip_prefix("action="))
                .map(|action| action.trim().to_string())
        })
        .next_back()
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
fn commit_edit(file_path: &str, new_content: &str) -> Result<()> {
    fs::write(file_path, new_content)
        .with_context(|| format!("Failed to write file: {}", file_path))?;
    run_post_save_hook(file_path);
    Ok(())
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

    let file_diff = FileDiff::from_texts(file_path, &original, &planned);
    let diff = file_diff.to_unified();
    let mut description = format!("Edit {}", file_path);
    for note in fidelity_notes(&file_diff, &original, &planned) {
        description.push('\n');
        description.push_str(&note);
    }
    let artifact = build_review_artifact(&description, &diff);

    let Some(returned) = open_editor(artifact).await? else {
        return Ok("Edit aborted by user.".to_string());
    };

    match parse_review_artifact(&returned, &diff) {
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
            commit_edit(file_path, &planned)?;
            Ok(diff)
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
        commit_edit(file_path, &new_content)?;
        Ok(FileDiff::from_texts(file_path, &original, &new_content).to_unified())
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
        // A base64 payload shows up as one long unbroken run of the base64
        // alphabet; readable source and diffs are broken up by spaces and
        // punctuation.
        for line in artifact.lines() {
            let mut run = 0usize;
            let mut longest = 0usize;
            for c in line.chars() {
                if c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=' {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
            assert!(
                longest < 120,
                "$EDITOR review artifact line looks like an encoded blob ({context}): \
                 {longest} unbroken base64-alphabet characters in {line:?}.\n\
                 The human would have opened this:\n{artifact}"
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
            parse_review_artifact(&artifact, "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n"),
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

    /// A diff that `FileDiff` could not compute in full must announce that
    /// above the diff. Approving an incomplete view is the failure mode this
    /// whole change exists to prevent.
    #[tokio::test]
    async fn test_incomplete_diff_is_flagged_before_the_reviewer_approves() {
        let bulk = "x\n".repeat(300_000);
        let (_dir, path) = temp_file("huge.txt", &format!("{bulk}MARKER\n"));
        let seen = Arc::new(Mutex::new(None));
        let result = review_and_apply_edit(
            &path,
            "MARKER",
            "REPLACED",
            false,
            capturing_editor(seen.clone(), Some),
        )
        .await
        .expect("interactive edit");

        let artifact = seen.lock().unwrap().clone().expect("editor was opened");
        let head: String = artifact.lines().take(12).collect::<Vec<_>>().join("\n");
        assert!(
            artifact.contains("WARNING: this diff is incomplete"),
            "a diff whose counts are not exact must warn the reviewer in the header.\n\
             Artifact header:\n{head}"
        );
        assert!(
            !artifact.contains("+REPLACED"),
            "this fixture is only meaningful while the change is genuinely not shown; if the \
             diff now renders, the warning is no longer load-bearing.\nArtifact header:\n{head}"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{bulk}REPLACED\n"),
            "an incomplete *view* must still apply the complete change; tool said: {result}"
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
