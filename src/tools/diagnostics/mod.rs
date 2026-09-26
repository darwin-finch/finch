// Post-edit diagnostics from a declared source (issue #757).
//
// After a write/edit/patch tool result is produced, the executor consults this
// service: if the user declared a check command for the touched file's
// extension in `[diagnostics]`, the command runs bounded and its result is
// appended to that same tool result. Nothing is inferred from project files;
// an edit's success or failure never depends on diagnostics.
//
// Authority: the declared command is evaluated through the *existing bash
// approval path* — `PermissionManager::check_tool_use("bash", …)` — so
// constitutional denials, patterns, and peer asymmetry apply unchanged, and
// its worst-case authority is bash's own ExternalWrite. No new authority
// declaration and no new registered tool exist for this feature.
//
// The command is executed directly (argv split, never a shell) with
// `kill_on_drop`, so a timed-out run cannot leave an orphaned build behind.

use crate::cli::sanitize_multiline;
use crate::config::{CheckCommandSource, DiagnosticsConfig};
use crate::tools::permissions::{PermissionCheck, PermissionManager};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

/// Whether the bash approval path admits the declared check command.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckVerdict {
    /// Execute the bounded check run.
    Run,
    /// Not executed: the bash path asks the user (peer role, or an owner
    /// session whose bash policy has not allowed this command).
    Skipped(String),
    /// Never executed: the bash path denied (tool disabled, constitutional
    /// constraint, or configured deny).
    Denied(String),
}

/// One completed check run, already reduced to its bounded report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckRun {
    status: String,
    output: String,
    truncated: bool,
}

/// Session-owned executor of declared post-edit diagnostics.
///
/// One bounded, serialized run per edit result whose file matches a declared
/// extension: Finch edits are tool calls rather than keystrokes, so spawn
/// count is bounded by per-turn edits by construction, and the per-source gate
/// keeps a burst of edits from overlapping spawns.
pub struct DiagnosticsService {
    config: DiagnosticsConfig,
    cwd: PathBuf,
    gates: std::sync::Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl DiagnosticsService {
    /// Build from the declared `[diagnostics]` configuration. Runs execute in
    /// `cwd` — the permission manager's canonical workspace/cwd — so relative
    /// check commands see the same root the bash tool would.
    pub fn from_config(config: &DiagnosticsConfig, cwd: PathBuf) -> Self {
        Self {
            config: config.clone(),
            cwd,
            gates: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// True when no source is declared; the post-edit hook is then inert.
    pub fn is_inert(&self) -> bool {
        self.config.is_inert()
    }

    /// Diagnostics annotation for one completed edit tool result, if any.
    ///
    /// `None` when no source is declared for the file (the common case: the
    /// result is returned unchanged and nothing executes).
    pub async fn annotation_for_edit_result(
        &self,
        file_path: &str,
        permissions: &PermissionManager,
    ) -> Option<String> {
        let source = self.config.source_for_file(file_path)?;
        let verdict = Self::verdict_for(&source.command, permissions);
        let body = match verdict {
            CheckVerdict::Denied(reason) => {
                format!("refused: the declared check command was not executed — {reason}")
            }
            CheckVerdict::Skipped(reason) => format!(
                "skipped: the declared check command was not executed — {reason}. \
                 Allow the command under your bash permissions to enable post-edit \
                 diagnostics."
            ),
            CheckVerdict::Run => {
                let gate = self.gate_for(source).clone();
                let _permit = gate.lock().await;
                let run = self.run_check(source).await;
                let mut body = run.status;
                if run.output.trim().is_empty() {
                    body.push_str("\noutput: (none)");
                } else {
                    body.push_str("\noutput:\n");
                    body.push_str(&run.output);
                }
                if run.truncated {
                    body.push_str(&format!(
                        "\n(output truncated to {} characters)",
                        self.config.max_output_chars
                    ));
                }
                body
            }
        };
        Some(format!(
            "\n\n[post-edit diagnostics — declared check command]\n\
             file: {file_path}\n\
             command: {}\n\
             {body}",
            source.command
        ))
    }

    /// The verdict the *existing bash approval path* returns for the declared
    /// command. Constitutional denials, patterns, and peer asymmetry all come
    /// from that single path; nothing here widens it.
    fn verdict_for(command: &str, permissions: &PermissionManager) -> CheckVerdict {
        let input = serde_json::json!({ "command": command });
        match permissions.check_tool_use("bash", &input) {
            PermissionCheck::Allow => CheckVerdict::Run,
            PermissionCheck::Deny(reason) => CheckVerdict::Denied(reason),
            PermissionCheck::AskUser(reason) => CheckVerdict::Skipped(reason),
        }
    }

    fn gate_for(&self, source: &CheckCommandSource) -> Arc<AsyncMutex<()>> {
        let mut gates = self
            .gates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates
            .entry(source.command.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    /// Run the declared command once, bounded.
    async fn run_check(&self, source: &CheckCommandSource) -> CheckRun {
        let mut parts = source.command.split_whitespace();
        let Some(program) = parts.next() else {
            return CheckRun {
                status: "check command is empty".to_string(),
                output: String::new(),
                truncated: false,
            };
        };
        let mut command = tokio::process::Command::new(program);
        command
            .args(parts)
            .current_dir(&self.cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = match spawn_retrying_text_file_busy(&mut command, program).await {
            Ok(child) => child,
            Err(error) => {
                return CheckRun {
                    status: format!("check command failed to start: {error}"),
                    output: String::new(),
                    truncated: false,
                };
            }
        };

        let bound = Duration::from_secs(self.config.timeout_secs);
        let outcome = tokio::time::timeout(bound, child.wait_with_output()).await;
        match outcome {
            Err(_) => CheckRun {
                status: format!(
                    "check command timed out after {}s and was stopped; the edit is unaffected",
                    self.config.timeout_secs
                ),
                output: String::new(),
                truncated: false,
            },
            Ok(Err(error)) => CheckRun {
                status: format!("check command failed: {error}"),
                output: String::new(),
                truncated: false,
            },
            Ok(Ok(output)) => {
                let status = match output.status.code() {
                    Some(0) => "check passed (exit 0)".to_string(),
                    Some(code) => format!("check reported problems (exit {code})"),
                    None => "check command was terminated by a signal".to_string(),
                };
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    if !text.trim().is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&stderr);
                }
                let text = sanitize_multiline(&text);
                let truncated = text.chars().count() > self.config.max_output_chars;
                let output_text = if truncated {
                    text.chars().take(self.config.max_output_chars).collect()
                } else {
                    text
                };
                CheckRun {
                    status,
                    output: output_text,
                    truncated,
                }
            }
        }
    }
}

/// How many times to re-attempt a check-command spawn the kernel refuses
/// with `ETXTBSY`, and how long to wait between attempts.
///
/// Mirrors `finch_runtime::host`'s `process-run` retry (issue #287, the
/// first place this exact kernel behavior was diagnosed in this
/// repository): the kernel refuses to exec a file any process holds open
/// for writing, and `fork()` copies the *entire* file descriptor table —
/// so a spawn anywhere else in the same process that forks while any
/// thread still holds a write descriptor on this file hands its child an
/// inherited copy of that descriptor, and *this* exec is refused even
/// though this module's own writer (`write_script` in tests; the file the
/// user declared in `[diagnostics]` in production) already closed its own
/// copy before this spawn ever ran. The condition is transient and
/// self-clearing: the inherited descriptor closes the moment that
/// unrelated child execs.
///
/// This is issue #1204: three separate CI failures, each in a different
/// test in this module, each shaped exactly like this — `write_script`
/// writes and closes the file, then the very next `spawn()` call is
/// refused — even though this module's own write-then-spawn sequence has
/// no gap in it. The refusal comes from *outside* this module: `cargo
/// test`'s default parallelism runs many `#[tokio::test]` functions
/// concurrently in one process, and this module is the one that both
/// writes fresh executable files *and* spawns them, repeatedly, across
/// many tests — the exact combination `fork()`'s whole-table-copy
/// semantics make racy. No reordering of this module's own write/chmod/
/// spawn sequence closes that gap, because the gap is never inside this
/// module: retrying the specific, documented, self-clearing error is the
/// fix, the same way #287 fixed it for `process-run`.
///
/// The loop breaks before sleeping on its final attempt, so eight attempts
/// means seven waits: `5ms * (1 + 2 + ... + 7)` = 140ms of sleep as a
/// floor, not a ceiling.
#[cfg(unix)]
const TEXT_FILE_BUSY_ATTEMPTS: u32 = 8;

#[cfg(unix)]
const TEXT_FILE_BUSY_BACKOFF: Duration = Duration::from_millis(5);

/// Spawn a check command, re-attempting while the kernel reports
/// `ETXTBSY`. See [`TEXT_FILE_BUSY_ATTEMPTS`] for why this condition is
/// transient and safe to retry: every attempt spawns the exact command the
/// caller already built, so a retry has no path to running anything other
/// than what was already going to run.
#[cfg(unix)]
async fn spawn_retrying_text_file_busy(
    command: &mut tokio::process::Command,
    executable: &str,
) -> std::io::Result<tokio::process::Child> {
    for attempt in 1..=TEXT_FILE_BUSY_ATTEMPTS {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.raw_os_error() == Some(nix::libc::ETXTBSY) => {
                #[cfg(test)]
                tests::record_text_file_busy_refusal(executable);
                tracing::debug!(
                    executable,
                    attempt,
                    "check command exec refused with ETXTBSY; a descriptor \
                     still holds it open for writing, retrying"
                );
                if attempt == TEXT_FILE_BUSY_ATTEMPTS {
                    break;
                }
                tokio::time::sleep(TEXT_FILE_BUSY_BACKOFF * attempt).await;
            }
            Err(error) => return Err(error),
        }
    }
    tracing::warn!(
        executable,
        attempts = TEXT_FILE_BUSY_ATTEMPTS,
        "check command exec refused with ETXTBSY on every attempt; giving up"
    );
    command.spawn()
}

/// ETXTBSY is a POSIX exec-time refusal; platforms without fork/exec
/// process spawning cannot hit it, so there is nothing to retry.
#[cfg(not(unix))]
async fn spawn_retrying_text_file_busy(
    command: &mut tokio::process::Command,
    _executable: &str,
) -> std::io::Result<tokio::process::Child> {
    command.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CheckCommandSource;
    use crate::tools::permissions::PermissionRule;
    use serde_json::json;
    use std::fs;
    use std::path::Path;
    use std::sync::OnceLock;
    use std::time::Instant;

    /// Per-executable count of real `ETXTBSY` refusals `spawn_retrying_text_file_busy`
    /// has observed, so a deterministic test can wait for a genuine kernel
    /// refusal instead of a fixed sleep (a fixed sleep would make the test
    /// vacuous on a loaded runner if the held descriptor happened to close
    /// before the first spawn attempt). Keyed by executable path so a
    /// concurrently running sibling test's own unrelated refusals on its own
    /// script cannot be mistaken for this test's.
    #[cfg(unix)]
    static TEXT_FILE_BUSY_REFUSALS: OnceLock<std::sync::Mutex<HashMap<String, u32>>> =
        OnceLock::new();

    #[cfg(unix)]
    pub(super) fn record_text_file_busy_refusal(executable: &str) {
        let mut table = TEXT_FILE_BUSY_REFUSALS
            .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *table.entry(executable.to_string()).or_insert(0) += 1;
    }

    // Only the deterministic reproduction test below reads this, and that
    // test is itself restricted to the platforms where a shebang script's
    // own exec is actually subject to the kernel's deny-write check
    // (verified directly: macOS is not one of them). Matching that
    // restriction here, rather than the broader `cfg(unix)` production
    // retry uses, keeps this getter from going unused on macOS.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    fn text_file_busy_refusals(executable: &str) -> u32 {
        TEXT_FILE_BUSY_REFUSALS
            .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(executable)
            .copied()
            .unwrap_or(0)
    }

    fn source(extensions: &[&str], command: &str) -> CheckCommandSource {
        CheckCommandSource {
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            command: command.to_string(),
        }
    }

    /// 30s, not a tight bound: every test using this helper runs a trivial
    /// echo/exit fixture script that normally finishes in milliseconds, but a
    /// loaded CI runner (this module's own sibling tests deliberately spawn
    /// slow/concurrent subprocesses — see
    /// `test_hanging_check_command_is_bounded_and_reports_the_timeout` and
    /// `test_spawn_count_is_bounded_by_edits_and_runs_are_serialized`) can
    /// push process-spawn latency far enough to intermittently trip a tight
    /// bound, which was previously 5s (#1204: two tests here failed
    /// intermittently in CI, passing clean on an unmodified rerun, because
    /// the timeout path fired instead of the intended exit-code/stderr path
    /// — a coarse liveness bound, not a correctness weakening; the one test
    /// that actually exercises the timeout path sets its own short bound
    /// explicitly instead of using this helper).
    fn config(sources: Vec<CheckCommandSource>) -> DiagnosticsConfig {
        DiagnosticsConfig {
            check: sources,
            timeout_secs: 30,
            max_output_chars: 2000,
        }
    }

    /// A tiny executable shell-script fixture (never cargo) that records its
    /// own executions in `marker` and prints `output`.
    fn write_script(dir: &Path, name: &str, body: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n"))
            .unwrap_or_else(|e| panic!("write fixture {name}: {e}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .unwrap_or_else(|e| panic!("chmod fixture {name}: {e}"));
        }
        path.to_string_lossy().into_owned()
    }

    fn owner_allowing_bash(root: &Path) -> PermissionManager {
        PermissionManager::new()
            .with_default_rule(PermissionRule::Allow)
            .with_workspace_root(root.to_path_buf())
    }

    fn owner_default(root: &Path) -> PermissionManager {
        PermissionManager::new().with_workspace_root(root.to_path_buf())
    }

    #[tokio::test]
    async fn test_declared_check_command_annotates_the_touched_file() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(
            dir.path(),
            "check.sh",
            "echo 'error[E0308]: mismatched types --> src/lib.rs:3:5'\nexit 1\n",
        );
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        );
        let permissions = owner_allowing_bash(dir.path());

        let annotation = service
            .annotation_for_edit_result("/workspace/src/lib.rs", &permissions)
            .await
            .expect("a declared source must annotate");

        assert!(
            annotation.contains("[post-edit diagnostics — declared check command]"),
            "the annotation must carry the bounded diagnostics header:\n{annotation}"
        );
        assert!(
            annotation.contains("error[E0308]"),
            "the annotation must carry the compiler error text:\n{annotation}"
        );
        assert!(
            annotation.contains("exit 1"),
            "a failing check must state its exit status:\n{annotation}"
        );
        assert!(
            annotation.contains("/workspace/src/lib.rs"),
            "the annotation must name the touched file:\n{annotation}"
        );
        assert!(
            annotation.contains(&script),
            "the annotation must name the declared source command so the model can \
             trust its origin:\n{annotation}"
        );
        for forbidden in ["--- ", "+++ ", "@@ ", "diff --git ", "Binary files "] {
            assert!(
                !annotation.lines().any(|line| line.starts_with(forbidden)),
                "no annotation line may be readable as diff structure — the transcript \
                 renderer parses {forbidden:?} lines as file changes; annotation:\n{annotation}"
            );
        }
    }

    #[tokio::test]
    async fn test_undeclared_source_is_inert_and_executes_nothing() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "touch marker; echo ran\n");
        let marker = dir.path().join("marker");

        // No service at all.
        let none =
            DiagnosticsService::from_config(&DiagnosticsConfig::default(), dir.path().into());
        assert!(
            none.annotation_for_edit_result("/w/src/lib.rs", &owner_allowing_bash(dir.path()))
                .await
                .is_none(),
            "an undeclared source must produce no annotation"
        );
        assert!(
            !marker.exists(),
            "an undeclared source must never execute the check command"
        );

        // Declared for a different extension only.
        let other = DiagnosticsService::from_config(
            &config(vec![source(&["py"], &script)]),
            dir.path().to_path_buf(),
        );
        let annotation = other
            .annotation_for_edit_result("/w/src/lib.rs", &owner_allowing_bash(dir.path()))
            .await;
        assert!(
            annotation.is_none(),
            "an edit to a file whose extension no source covers must not annotate: \
             {annotation:?}"
        );
        assert!(
            !marker.exists(),
            "an edit to an uncovered extension must never execute the check command"
        );
    }

    #[tokio::test]
    async fn test_failing_check_command_never_fails_or_bounds_the_edit_by_its_error() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(
            dir.path(),
            "check.sh",
            "echo 'internal error' >&2\nexit 3\n",
        );
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        );
        let permissions = owner_allowing_bash(dir.path());

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("an erroring source still reports, bounded");

        assert!(
            annotation.contains("exit 3") && annotation.contains("internal error"),
            "the annotation must report the failing status and captured stderr:\n{annotation}"
        );
        assert!(
            !annotation.contains("aborted") && !annotation.contains("timed out"),
            "an erroring source must read as a completed bounded report, not a \
             failure of the annotation path:\n{annotation}"
        );
    }

    #[tokio::test]
    async fn test_hanging_check_command_is_bounded_and_reports_the_timeout() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "sleep 30\n");
        let mut declared = config(vec![source(&["rs"], &script)]);
        declared.timeout_secs = 1;
        let service = DiagnosticsService::from_config(&declared, dir.path().to_path_buf());
        let permissions = owner_allowing_bash(dir.path());

        let started = Instant::now();
        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("a hanging source must still produce a bounded annotation");
        let elapsed = started.elapsed();

        assert!(
            annotation.contains("timed out after 1s"),
            "the annotation must name the timeout bound; elapsed {elapsed:?}:\n{annotation}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(20),
            "a hanging source must be bounded well below its own sleep; took {elapsed:?}. \
             (Coarse liveness bound paired with the exact 'timed out' assertion above.)"
        );
    }

    #[tokio::test]
    async fn test_diagnostics_output_is_bounded_in_size_with_a_truncation_note() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(
            dir.path(),
            "check.sh",
            "i=0\nwhile [ $i -lt 1000 ]; do echo 'error: line of a very large build log'; i=$((i+1)); done\n",
        );
        let mut declared = config(vec![source(&["rs"], &script)]);
        declared.max_output_chars = 500;
        let service = DiagnosticsService::from_config(&declared, dir.path().to_path_buf());
        let permissions = owner_allowing_bash(dir.path());

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("a verbose source must still annotate");

        let excerpt = annotation
            .split("output:\n")
            .nth(1)
            .expect("annotation must contain an output section");
        assert!(
            excerpt.chars().count() <= 500 + "(output truncated to 500 characters)\n".len() + 2,
            "the excerpt must be bounded by max_output_chars; got {} chars:\n{annotation}",
            excerpt.chars().count()
        );
        assert!(
            annotation.contains("(output truncated to 500 characters)"),
            "truncation must be declared, not silent:\n{annotation}"
        );
    }

    #[tokio::test]
    async fn test_spawn_count_is_bounded_by_edits_and_runs_are_serialized() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let log = dir.path().join("runs.log");
        let log_arg = log.to_string_lossy().into_owned();
        let script = write_script(
            dir.path(),
            "check.sh",
            &format!("echo \"start $$\" >> {log_arg}\nsleep 0.2\necho \"end $$\" >> {log_arg}\n"),
        );
        let service = Arc::new(DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        ));
        let permissions = Arc::new(owner_allowing_bash(dir.path()));

        // Four edit results touching declared files, some concurrent: exactly
        // four runs, serialized per source (no interleaved start/end pairs).
        let mut tasks = Vec::new();
        for index in 0..4 {
            let service = Arc::clone(&service);
            let permissions = Arc::clone(&permissions);
            tasks.push(tokio::spawn(async move {
                service
                    .annotation_for_edit_result(&format!("/w/src/file{index}.rs"), &permissions)
                    .await
                    .expect("declared source must annotate")
            }));
        }
        for task in tasks {
            task.await.expect("annotation task");
        }

        let log_text = fs::read_to_string(&log).expect("fixture must record runs");
        let starts = log_text.matches("start ").count();
        let ends = log_text.matches("end ").count();
        assert_eq!(
            starts, 4,
            "four edit results must spawn exactly four bounded runs (bounded by \
             per-turn edits, never more): log:\n{log_text}"
        );
        assert_eq!(ends, 4, "every spawned run must complete: log:\n{log_text}");
        let mut open_run: Option<&str> = None;
        let mut interleaved = String::new();
        for line in log_text.lines() {
            if let Some(pid) = line.strip_prefix("start ") {
                if open_run.is_some() {
                    interleaved.push_str(line);
                    interleaved.push('\n');
                }
                open_run = Some(pid);
            } else if let Some(pid) = line.strip_prefix("end ") {
                if open_run != Some(pid) && open_run.is_some() {
                    interleaved.push_str(line);
                    interleaved.push('\n');
                }
                open_run = None;
            }
        }
        assert!(
            interleaved.is_empty() && open_run.is_none(),
            "runs for one source must be serialized — no start may overlap an \
             unclosed earlier run; offending lines:\n{interleaved}log:\n{log_text}"
        );
    }

    #[tokio::test]
    async fn test_peer_check_command_surfaces_ask_user_and_is_not_executed() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "touch marker; echo ran\n");
        let marker = dir.path().join("marker");
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        );
        let peer = PermissionManager::for_peer().with_workspace_root(dir.path().to_path_buf());

        // The shared bash approval path is the surface: a peer's non-readonly
        // command must read as AskUser there, exactly as a model-issued bash
        // call would.
        let verdict = peer.check_tool_use("bash", &json!({ "command": script }));
        assert!(
            matches!(verdict, PermissionCheck::AskUser(_)),
            "the check-command path must require the same approval as bash (peer \
             surfaces AskUser); verdict: {verdict:?}"
        );

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &peer)
            .await
            .expect("a skipped run must still be declared on the result");

        assert!(
            annotation.contains("skipped:"),
            "the annotation must declare the skip rather than silently producing \
             nothing:\n{annotation}"
        );
        assert!(
            !marker.exists(),
            "a peer must not execute the declared check command without approval"
        );
    }

    #[tokio::test]
    async fn test_unapproved_owner_check_command_is_skipped_not_executed() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "touch marker; echo ran\n");
        let marker = dir.path().join("marker");
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        );
        // Default owner policy: bash is Ask, and the session has not approved
        // this command.
        let permissions = owner_default(dir.path());

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("the skip must be declared, not silent");

        assert!(
            annotation.contains("skipped:"),
            "an unapproved owner command must read as a declared skip:\n{annotation}"
        );
        assert!(
            !marker.exists(),
            "the check command must not execute without the same approval bash \
             would require"
        );
    }

    #[tokio::test]
    async fn test_constitutionally_denied_check_command_is_never_executed() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "touch marker; echo ran\n");
        let marker = dir.path().join("marker");
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &format!("sudo {script}"))]),
            dir.path().to_path_buf(),
        );
        let permissions = owner_allowing_bash(dir.path());

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("a refused declaration must be explained on the result");

        assert!(
            annotation.contains("refused:"),
            "a constitutionally denied declaration must read as a refusal:\n{annotation}"
        );
        assert!(
            !marker.exists(),
            "a constitutionally denied command must never execute, even though \
             the user declared it"
        );
    }

    #[tokio::test]
    async fn test_unspawnable_command_reports_bounded_error_without_panicking() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let service = DiagnosticsService::from_config(
            &config(vec![source(
                &["rs"],
                "/definitely/not/an/existing/finch-fixture-binary",
            )]),
            dir.path().to_path_buf(),
        );
        let permissions = owner_allowing_bash(dir.path());

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("an unspawnable command must report, not panic");

        assert!(
            annotation.contains("failed to start"),
            "spawn failure must be a bounded report naming the failure:\n{annotation}"
        );
    }

    /// #1204: a check-command spawn the kernel refuses with `ETXTBSY` is
    /// retried, not surfaced to the model as a startup failure.
    ///
    /// Three separate CI failures (PR #1144, PR #1193, and main run
    /// 36262031799) hit this — a different specific test in this module
    /// each time, never a test that touches `src/tools/diagnostics/` in its
    /// diff. `write_script` writes and closes the fixture script, then
    /// `run_check` spawns it next; there is no gap between those two steps
    /// in this module's own code, and the gate mutex already serializes
    /// concurrent runs of one source. The refusal instead comes from
    /// *outside* this test: the kernel's ETXTBSY check is per-inode, and
    /// `fork()` copies the *entire* file descriptor table, so cargo test's
    /// default parallelism running many other `#[tokio::test]` functions in
    /// this same module concurrently means some *other*, unrelated spawn
    /// can fork while this test's own script still has a writer open
    /// somewhere, and inherit a copy of that write descriptor into its
    /// child — refusing *this* test's exec even though this test's own
    /// writer already closed its copy.
    ///
    /// This does not need a second process racing to reproduce: the
    /// kernel's check is per-inode, so a second write descriptor opened
    /// here on this test's own script blocks its own exec exactly the same
    /// way, deterministically instead of at CI's whim. Before the fix this
    /// fails at the "must still annotate" expect: the first refusal was
    /// returned to the caller verbatim, as `check command failed to start:
    /// Text file busy (os error 26)`.
    ///
    /// Restricted to the platforms where a shebang script's own exec is
    /// actually subject to the kernel's deny-write check (matching
    /// `finch_runtime::host`'s identical #287 test): verified directly that
    /// macOS does not refuse to exec a shell script that is still open for
    /// writing, so this reproduction is not meaningful there, and gating it
    /// there would make the test flicker between "proves the retry" and
    /// "proves nothing" depending on the host kernel, not this fix.
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn test_transient_text_file_busy_check_command_is_retried_not_reported_as_a_failure() {
        let dir = tempfile::tempdir().expect("fixture dir");
        let script = write_script(dir.path(), "check.sh", "echo ok\n");
        let service = DiagnosticsService::from_config(
            &config(vec![source(&["rs"], &script)]),
            dir.path().to_path_buf(),
        );
        let permissions = owner_allowing_bash(dir.path());

        // Hold a second write descriptor open on the exact script the spawn
        // below is about to exec. The kernel's ETXTBSY check is per-inode,
        // so this blocks the exec the same way an unrelated concurrent
        // test's in-flight fork would, on demand instead of by chance.
        let writer = fs::OpenOptions::new()
            .write(true)
            .open(&script)
            .expect("open the script for writing while it is still named");
        let held: Arc<std::sync::Mutex<Option<fs::File>>> =
            Arc::new(std::sync::Mutex::new(Some(writer)));
        let releaser_script = script.clone();
        let releaser_held = Arc::clone(&held);
        std::thread::spawn(move || {
            // Release once a real refusal has actually been observed,
            // rather than after a fixed delay: a timer would make this test
            // vacuous on a fast or lightly loaded machine — if the work
            // before the spawn outlasts a fixed delay, the descriptor would
            // already be closed before the first attempt, every assertion
            // below would still pass, and the retry path would never have
            // run.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while text_file_busy_refusals(&releaser_script) == 0
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            releaser_held
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
        });

        let annotation = service
            .annotation_for_edit_result("/w/src/main.rs", &permissions)
            .await
            .expect("a transiently busy check command must still annotate once retried");

        assert!(
            text_file_busy_refusals(&script) >= 1,
            "the test's own held-open descriptor must have produced at least one \
             real ETXTBSY refusal, or this proves nothing about the retry path: \
             refusals=0, script={script}"
        );
        assert!(
            annotation.contains("check passed (exit 0)"),
            "a transiently busy check command must be retried through to its real \
             result, not reported as a startup failure:\n{annotation}"
        );
        assert!(
            !annotation.contains("failed to start"),
            "ETXTBSY must never reach the model as a startup failure once the \
             kernel-documented retry has run:\n{annotation}"
        );

        // Safety net: the releaser thread should already have taken this,
        // but drop it explicitly before `dir` (and the script inside it)
        // goes away regardless.
        held.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    /// Stress coverage for #1204 alongside the forced, deterministic
    /// reproduction above: real OS threads (`worker_threads = 8`, not the
    /// single-threaded default `#[tokio::test]` runtime every other test in
    /// this module uses, which cannot make two `fork()` calls overlap at
    /// all) each repeatedly write a fresh script and spawn it, so genuinely
    /// concurrent forks can land inside each other's write windows the same
    /// way distinct test functions did in the three CI failures. Every one
    /// of the resulting spawns must still resolve to a passing check --
    /// the retry must absorb any live ETXTBSY this actually triggers, not
    /// merely the forced one above. Report the pass count in the commit's
    /// verification evidence; a platform where the race never materializes
    /// (verified directly: macOS) simply exercises the code path with zero
    /// live refusals rather than proving nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_many_concurrent_check_command_spawns_never_report_a_startup_failure() {
        const WORKERS: usize = 8;
        const ITERATIONS_PER_WORKER: usize = 6;

        let dir = tempfile::tempdir().expect("fixture dir");
        let cwd = dir.path().to_path_buf();
        let permissions = Arc::new(owner_allowing_bash(&cwd));

        let mut tasks = Vec::new();
        for worker in 0..WORKERS {
            let cwd = cwd.clone();
            let permissions = Arc::clone(&permissions);
            tasks.push(tokio::spawn(async move {
                let mut failures = Vec::new();
                for iteration in 0..ITERATIONS_PER_WORKER {
                    let name = format!("check-{worker}-{iteration}.sh");
                    let script = write_script(&cwd, &name, "echo ok\n");
                    let service = DiagnosticsService::from_config(
                        &config(vec![source(&["rs"], &script)]),
                        cwd.clone(),
                    );
                    match service
                        .annotation_for_edit_result("/w/src/main.rs", &permissions)
                        .await
                    {
                        Some(annotation) if annotation.contains("check passed (exit 0)") => {}
                        other => failures.push(format!(
                            "worker={worker} iteration={iteration} result={other:?}"
                        )),
                    }
                }
                failures
            }));
        }

        let mut all_failures = Vec::new();
        for task in tasks {
            all_failures.extend(task.await.expect("worker task must not panic"));
        }

        assert!(
            all_failures.is_empty(),
            "{} of {} concurrent check-command spawns did not resolve to a passing \
             check after retry:\n{}",
            all_failures.len(),
            WORKERS * ITERATIONS_PER_WORKER,
            all_failures.join("\n")
        );
    }

    #[test]
    fn test_service_is_inert_when_no_source_is_declared() {
        let service =
            DiagnosticsService::from_config(&DiagnosticsConfig::default(), PathBuf::from("."));
        assert!(
            service.is_inert(),
            "a default config must leave the post-edit hook inert"
        );
    }
}
