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
use std::io::{IsTerminal, Write as _};
use tempfile::Builder;
use tokio::task::spawn_blocking;

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

    spawn_blocking(move || {
        // Write to a temp file with a .sh extension so editors syntax-highlight it.
        let mut tmp = Builder::new()
            .prefix("finch_")
            .suffix(".sh")
            .tempfile()?;

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
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        let status = std::process::Command::new(&editor)
            .arg(&path)
            .status()?;

        if !status.success() {
            // Editor exited non-zero — treat as abort.
            return Ok(None);
        }

        // Read back whatever the user left in the file.
        let modified = std::fs::read_to_string(&path)?;

        // Explicit keep: file is non-empty after stripping comments and whitespace.
        let executable_content = modified
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
            .count();

        if executable_content == 0 {
            // User cleared everything — abort.
            Ok(None)
        } else {
            Ok(Some(modified))
        }
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
    fn test_run_script_captures_output() {
        let out = run_script("#!/bin/bash\necho hello").unwrap();
        assert_eq!(out.trim(), "hello");
    }

    #[test]
    fn test_run_script_captures_stderr() {
        let out = run_script("#!/bin/bash\necho err >&2").unwrap();
        assert!(out.contains("err"));
    }
}
