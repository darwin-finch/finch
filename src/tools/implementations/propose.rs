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

use anyhow::Result;
use crossterm::{cursor, event, execute, style::ResetColor, terminal};
use std::io::{IsTerminal, Write as _};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tempfile::Builder;
use tokio::task::spawn_blocking;

/// Decision encoded in an editor-backed proposal file. The source remains
/// untrusted and must still pass the normal tool/VM authorization path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProposalDecision {
    Execute { source: String },
    Chat { context: String },
    Cancel,
}

/// Read a reserved Finch action directive without interpreting ordinary
/// comments (including Git's instructional comments).
pub fn parse_proposal_decision(content: &str) -> ProposalDecision {
    let action = content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let directive = trimmed
                .strip_prefix("# finch:")
                .or_else(|| trimmed.strip_prefix("\\ finch:"))
                .or_else(|| trimmed.strip_prefix(";; finch:"))?;
            directive.trim().strip_prefix("action=")
        })
        .last();
    match action {
        Some("cancel") => ProposalDecision::Cancel,
        Some("chat") => ProposalDecision::Chat {
            context: content.to_string(),
        },
        _ if content.lines().all(|line| {
            let line = line.trim();
            line.is_empty()
                || line.starts_with('#')
                || line.starts_with('\\')
                || line.starts_with(";;")
        }) =>
        {
            ProposalDecision::Cancel
        }
        _ => ProposalDecision::Execute {
            source: content.to_string(),
        },
    }
}

/// Editor-backed proposal API that preserves the user's explicit action.
/// Existing callers may continue using `propose_in_editor` while migrating.
pub async fn propose_with_decision(description: &str, code: &str) -> Result<ProposalDecision> {
    Ok(match propose_in_editor(description, code).await? {
        Some(content) => parse_proposal_decision(&content),
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

/// Owns the render-loop handoff for the whole editor lifecycle, including the
/// async grace period before the blocking editor task starts. This matters
/// when the proposal future is cancelled during that grace period: the TUI
/// must not remain permanently gated just because no terminal mode changed.
struct TerminalRestorer {
    restore: Option<Box<dyn FnOnce() + Send>>,
}

impl TerminalRestorer {
    fn for_tui(tui_mode: bool) -> Self {
        let restore = tui_mode.then(|| {
            Box::new(|| {
                crate::request_tui_rebuild();
                crate::set_editor_active(false);
            }) as Box<dyn FnOnce() + Send>
        });
        Self { restore }
    }

    fn suspend(&mut self) {
        if self.restore.is_none() {
            return;
        }
        // Arm the full terminal restore before the first terminal mutation.
        // A panic or I/O failure during suspension must still unwind to the
        // primary screen and release the render loop.
        self.restore = Some(Box::new(resume_terminal_after_editor));
        suspend_terminal_for_editor();
    }

    #[cfg(test)]
    fn with_restore(restore: impl FnOnce() + Send + 'static) -> Self {
        Self {
            restore: Some(Box::new(restore)),
        }
    }
}

impl Drop for TerminalRestorer {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore();
        }
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

    let modified = std::fs::read_to_string(&path)?;
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
/// skipped and `Some(code)` is returned immediately.
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
    // Unit tests often run under a PTY, so `is_terminal()` alone would launch
    // the developer's real $EDITOR and wedge the test runner. Test builds use
    // the same noninteractive result as daemon/piped invocations; editor
    // lifecycle behavior is covered through the explicit decision/resume
    // tests rather than a human editor process.
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return Ok(Some(code.to_string()));
    }

    let script = build_artifact(description, code, comment_prefix, executable);
    let tui_mode = crate::is_tui_active();

    if tui_mode {
        // Gate the render loop so it won't write crossterm sequences while the
        // editor has the terminal.
        crate::set_editor_active(true);
    }
    let mut restore = TerminalRestorer::for_tui(tui_mode);

    if tui_mode {
        // Give any in-flight render one tick (33 ms) to finish. The guard is
        // already armed, so cancellation here cannot strand EDITOR_ACTIVE.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The terminal restore runs inside spawn_blocking so that disable/enable
    // raw mode are always paired even if the closure returns early via `?`.
    spawn_blocking(move || {
        restore.suspend();
        edit_artifact(&script, suffix, executable, comment_prefix, run_editor)
    })
    .await?
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
fn build_artifact(description: &str, code: &str, comment_prefix: &str, executable: bool) -> String {
    let mut out = if executable {
        String::from("#!/bin/bash\n")
    } else {
        String::new()
    };
    out.push_str(comment_prefix);
    out.push_str(" Finch proposal: save and quit to accept; set action=cancel to reject or action=chat to request changes.\n");
    out.push_str(comment_prefix);
    out.push_str(" finch: action=execute\n");
    for line in description.lines() {
        out.push_str(comment_prefix);
        out.push(' ');
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out.push_str(code);
    if !code.ends_with('\n') {
        out.push('\n');
    }
    out
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
/// In non-interactive environments the editor is skipped and the original
/// code is returned immediately.
pub async fn propose_forth_in_editor(description: &str, code: &str) -> Result<Option<String>> {
    // Keep the typed Forth proposal path consistent with every other
    // artifact: integration tests can own a PTY, but must never launch the
    // developer's real editor or receive generated comment headers as the
    // accepted source.
    if cfg!(test) || !std::io::stdin().is_terminal() {
        return Ok(Some(code.to_string()));
    }

    let mut header = String::from(
        "\\ Finch proposal: save and quit to accept; set action=cancel to reject or action=chat to request changes.\n\\ finch: action=execute\n",
    );
    header.extend(description.lines().map(|line| format!("\\ {line}\n")));
    let content = format!("{header}\n{code}");
    let tui_mode = crate::is_tui_active();

    if tui_mode {
        crate::set_editor_active(true);
    }
    let mut restore = TerminalRestorer::for_tui(tui_mode);

    if tui_mode {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    spawn_blocking(move || {
        restore.suspend();
        edit_artifact(&content, ".forth", false, "\\", run_editor)
    })
    .await?
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
        Arc,
    };

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

    #[test]
    fn editor_screen_wrapper_finishes_on_primary_after_nested_nonstacking_editor() {
        let mut output = Vec::new();
        enter_editor_screen(&mut output).unwrap();
        // Model a full-screen editor using the same non-stacking xterm mode.
        execute!(&mut output, terminal::EnterAlternateScreen).unwrap();
        execute!(&mut output, terminal::LeaveAlternateScreen).unwrap();
        leave_editor_screen(&mut output).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("\x1b[?1049h").count(), 2);
        assert_eq!(output.matches("\x1b[?1049l").count(), 2);
        assert!(output.ends_with("\x1b[0m"));
        assert!(output.rfind("\x1b[?1049l").unwrap() > output.rfind("\x1b[?1049h").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn fake_editor_covers_success_empty_nonzero_and_launch_error() {
        let accepted = edit_artifact("before\n", ".txt", false, "#", |path| {
            std::fs::write(path, "after\n")?;
            Ok(exit_status(0))
        })
        .unwrap();
        assert_eq!(accepted.as_deref(), Some("after\n"));

        let empty = edit_artifact("before\n", ".txt", false, "#", |path| {
            std::fs::write(path, "# review only\n\n")?;
            Ok(exit_status(0))
        })
        .unwrap();
        assert_eq!(empty, None);

        let rejected =
            edit_artifact("before\n", ".txt", false, "#", |_| Ok(exit_status(7))).unwrap();
        assert_eq!(rejected, None);

        let error = edit_artifact("before\n", ".txt", false, "#", |_| {
            Err(anyhow::anyhow!("fake editor launch failed"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("fake editor launch failed"));
    }

    #[cfg(unix)]
    #[test]
    fn terminal_guard_restores_exactly_once_for_every_editor_outcome() {
        for outcome in [
            Ok(Some("accepted".to_string())),
            Ok(None),
            Err(anyhow::anyhow!("failed")),
        ] {
            let restores = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&restores);
            let result = {
                let _restore = TerminalRestorer::with_restore(move || {
                    observed.fetch_add(1, Ordering::SeqCst);
                });
                outcome
            };
            drop(result);
            assert_eq!(restores.load(Ordering::SeqCst), 1);
        }
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
        assert!(matches!(
            parse_proposal_decision("# finch: action=execute\necho hi"),
            ProposalDecision::Execute { .. }
        ));
        assert!(matches!(
            parse_proposal_decision("# finch: action=chat\nplease discuss this"),
            ProposalDecision::Chat { .. }
        ));
        assert_eq!(
            parse_proposal_decision("# finch: action=cancel\necho hi"),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn empty_or_comment_only_proposal_is_rejected() {
        assert_eq!(parse_proposal_decision(""), ProposalDecision::Cancel);
        assert_eq!(parse_proposal_decision("  \n\t\n"), ProposalDecision::Cancel);
        assert_eq!(
            parse_proposal_decision(
                "# Finch proposal: delete the source to reject\n# finch: action=execute\n"
            ),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn last_proposal_directive_is_the_users_final_decision() {
        assert_eq!(
            parse_proposal_decision(
                "# finch: action=execute\necho dangerous\n# finch: action=cancel\n"
            ),
            ProposalDecision::Cancel
        );
    }

    #[test]
    fn generated_shell_proposal_exposes_all_review_actions() {
        let artifact = build_script("Inspect files", "find . -type f");
        assert!(artifact.contains("# finch: action=execute\n"));
        assert!(artifact.contains("action=cancel to reject"));
        assert!(artifact.contains("action=chat to request changes"));
    }

    #[test]
    fn ordinary_git_comments_do_not_control_proposal() {
        assert!(matches!(
            parse_proposal_decision("# Please enter the commit message\necho hi"),
            ProposalDecision::Execute { .. }
        ));
    }

    #[test]
    fn lisp_artifacts_receive_lisp_comment_headers() {
        let artifact = build_artifact("Explain intent", "(say \"ok\")", ";;", false);
        assert!(artifact.starts_with(";; Finch proposal:"));
        assert!(artifact.contains(";; finch: action=execute\n"));
        assert!(artifact.contains(";; Explain intent\n"));
        assert!(!artifact.starts_with("#!/bin/bash"));
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
}
