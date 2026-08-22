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
    let action = content.lines().find_map(|line| {
        let trimmed = line.trim();
        let directive = trimmed
            .strip_prefix("# finch:")
            .or_else(|| trimmed.strip_prefix("\\ finch:"))
            .or_else(|| trimmed.strip_prefix(";; finch:"))?;
        directive.trim().strip_prefix("action=")
    });
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
        }) => ProposalDecision::Cancel,
        _ => ProposalDecision::Execute {
            source: content.to_string(),
        },
    }
}

fn suspend_terminal_for_editor() {
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
    std::io::stdout().flush().ok();
}

fn resume_terminal_after_editor() {
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

fn run_editor(path: &Path) -> Result<std::process::ExitStatus> {
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
    // Skip the editor when not running in an interactive terminal.
    if !std::io::stdin().is_terminal() {
        return Ok(Some(code.to_string()));
    }

    let script = build_script(description, code);
    let tui_mode = crate::is_tui_active();

    if tui_mode {
        // Gate the render loop so it won't write crossterm sequences while the
        // editor has the terminal.
        crate::set_editor_active(true);
        // Give any in-flight render one tick (33 ms) to finish.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The terminal restore runs inside spawn_blocking so that disable/enable
    // raw mode are always paired even if the closure returns early via `?`.
    spawn_blocking(move || {
        // RAII: restore terminal state no matter how the closure exits.
        struct TerminalRestorer {
            tui_mode: bool,
        }
        impl Drop for TerminalRestorer {
            fn drop(&mut self) {
                if self.tui_mode {
                    resume_terminal_after_editor();
                }
            }
        }
        let _restore = TerminalRestorer { tui_mode };

        if tui_mode {
            // Flush any pending TUI output before handing the terminal to the editor.
            suspend_terminal_for_editor();
        }

        // Write to a temp file with a .sh extension so editors syntax-highlight it.
        let mut tmp = Builder::new().prefix("finch_").suffix(".sh").tempfile()?;

        tmp.write_all(script.as_bytes())?;
        tmp.flush()?;

        let path = tmp.path().to_owned();

        // Make executable so the user can test it directly in a terminal.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms)?;
        }

        // Open the editor.  Fall back to vi if $EDITOR is not set.
        // $VISUAL is the POSIX full-screen editor variable (preferred for diff editors).
        // Fall back to $EDITOR, then vi.
        let status = run_editor(&path)?;

        if !status.success() {
            // Editor exited non-zero — treat as abort.
            return Ok(None);
        }

        // Read back whatever the user left in the file.
        let modified = std::fs::read_to_string(&path)?;

        // Keep if any non-comment, non-blank lines remain.
        let executable_content = modified
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .count();

        if executable_content == 0 {
            Ok(None)
        } else {
            Ok(Some(modified))
        }
        // _restore drops here: enable_raw_mode + set_editor_active(false)
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
    let mut out = String::from("#!/bin/bash\n");
    for line in description.lines() {
        out.push_str("# ");
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
    let script = script.to_string();
    spawn_blocking(move || {
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()?;
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
    })
    .await?
}

/// Open `$EDITOR` with Forth source, return the (possibly edited) content.
///
/// Uses a `.forth` extension so editors apply Forth syntax highlighting.
/// Lines starting with `\` are treated as comments; emptying the file aborts.
/// In non-interactive environments the editor is skipped and the original
/// code is returned immediately.
pub async fn propose_forth_in_editor(description: &str, code: &str) -> Result<Option<String>> {
    if !std::io::stdin().is_terminal() {
        return Ok(Some(code.to_string()));
    }

    let header: String = description.lines().map(|l| format!("\\ {l}\n")).collect();
    let content = format!("{header}\n{code}");
    let tui_mode = crate::is_tui_active();

    if tui_mode {
        crate::set_editor_active(true);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    spawn_blocking(move || {
        struct TerminalRestorer {
            tui_mode: bool,
        }
        impl Drop for TerminalRestorer {
            fn drop(&mut self) {
                if self.tui_mode {
                    resume_terminal_after_editor();
                }
            }
        }
        let _restore = TerminalRestorer { tui_mode };

        if tui_mode {
            suspend_terminal_for_editor();
        }

        let mut tmp = Builder::new()
            .prefix("finch_")
            .suffix(".forth")
            .tempfile()?;

        tmp.write_all(content.as_bytes())?;
        tmp.flush()?;
        let path = tmp.path().to_owned();

        // $VISUAL is the POSIX full-screen editor variable (preferred for diff editors).
        // Fall back to $EDITOR, then vi.
        let status = run_editor(&path)?;

        if !status.success() {
            return Ok(None);
        }

        let modified = std::fs::read_to_string(&path)?;
        let executable = modified
            .lines()
            .filter(|l| !l.trim_start().starts_with('\\') && !l.trim().is_empty())
            .count();

        if executable == 0 {
            Ok(None)
        } else {
            Ok(Some(modified))
        }
        // _restore drops here
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
    fn ordinary_git_comments_do_not_control_proposal() {
        assert!(matches!(
            parse_proposal_decision("# Please enter the commit message\necho hi"),
            ProposalDecision::Execute { .. }
        ));
    }
}
