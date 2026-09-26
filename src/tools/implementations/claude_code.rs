//! Delegate a self-contained coding task to the official Claude Code CLI.
//!
//! Unlike `spawn_task` (an isolated Finch subagent loop reusing Finch's own
//! tool set) or `background_bash` (an arbitrary long-lived shell command),
//! this tool hands the task to a *distinct* external program — the real
//! `claude` binary, authenticated against its own Anthropic subscription —
//! and lets it run as itself: its own identity, its own tool-use
//! conventions, its own agentic loop. Finch decides *when* it runs and
//! *what task* it is given; Finch does not intercept, repurpose, or wrap its
//! wire protocol, and the result is surfaced back into the transcript
//! explicitly attributed to Claude Code, not absorbed as Finch's own output.
//!
//! Invocation shape: `claude --print --output-format json --permission-mode
//! <mode> --permission-prompts none [--model <model>] <task>`. `--print`
//! (headless, non-interactive) plus `--permission-prompts none` is required
//! because nothing in this call can answer an interactive permission
//! prompt — without it, an operation the permission mode doesn't already
//! cover would hang the turn forever instead of being denied. `--output-format
//! json` gives a single structured result (final text, session id, cost,
//! turn count, error state) instead of free-form prose, which is what this
//! tool parses to build its attributed report. There is no live streaming:
//! the call blocks until Claude Code finishes or the timeout elapses.
//!
//! Authority: once running, Claude Code has the same real, un-sandboxed
//! host authority Finch's own `bash` tool has — it is launched with a
//! working directory but nothing here confines its file or process access
//! beyond that starting point, exactly like `bash` has no path slot. That
//! is why `effect()` declares `ExternalWrite` (the same worst-case bucket as
//! `bash`, `background_bash`, and `spawn_task`) and why this tool's
//! registered name is hard-denied to peers in
//! `finch_tools_api::permissions::PEER_HARD_DENY_TOOLS`: a peer must not be
//! able to grant a second, independently-authenticated agent real file and
//! command access any more than it can spawn a Finch subagent or restart the
//! session.

use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
use serde::Deserialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Default headless permission mode: Claude Code may apply file edits
/// without prompting, but anything acceptEdits doesn't cover is denied
/// (never hung) by `--permission-prompts none`.
const DEFAULT_PERMISSION_MODE: &str = "accept_edits";

/// Default bound on how long one delegated task may run before it is killed.
const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Upper bound a caller may request via `timeout_secs`.
const MAX_TIMEOUT_SECS: u64 = 3600;

/// Output is capped the same way `bash` caps its captured output, so one
/// runaway sub-agent cannot flood the transcript.
const MAX_OUTPUT_CHARS: usize = 20_000;

/// Delegates a coding task to the official `claude` CLI as a sub-agent.
pub struct ClaudeCodeDelegateTool {
    workspace_root: PathBuf,
    claude_binary: PathBuf,
}

impl ClaudeCodeDelegateTool {
    /// Construct against the real `claude` binary resolved from `PATH`.
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self::with_binary(workspace_root, "claude")
    }

    /// Construct against an explicit binary path — the production seam
    /// tests use to point at a fake `claude` script instead of the real,
    /// authenticated CLI.
    pub fn with_binary(workspace_root: impl Into<PathBuf>, binary: impl Into<PathBuf>) -> Self {
        let workspace_root = workspace_root.into();
        Self {
            workspace_root: finch_tools_api::resolve_workspace_root(&workspace_root),
            claude_binary: binary.into(),
        }
    }

    /// Resolve the requested subdirectory against the workspace root,
    /// rejecting an escape instead of silently running outside it. This is
    /// this tool's own scoping convention for where Claude Code *starts* —
    /// not the `PathSlot` permission machinery `bash` also does not use.
    fn resolve_directory(&self, requested: Option<&str>) -> Result<PathBuf> {
        let root = self
            .workspace_root
            .canonicalize()
            .unwrap_or_else(|_| self.workspace_root.clone());
        let Some(requested) = requested else {
            return Ok(root);
        };
        let joined = if Path::new(requested).is_absolute() {
            PathBuf::from(requested)
        } else {
            root.join(requested)
        };
        let resolved = joined
            .canonicalize()
            .with_context(|| format!("directory does not exist: {}", joined.display()))?;
        if !resolved.starts_with(&root) {
            bail!(
                "directory '{}' escapes the workspace root '{}'; delegate_to_claude_code must be \
                 started from inside the workspace",
                resolved.display(),
                root.display()
            );
        }
        Ok(resolved)
    }
}

fn permission_mode_flag(requested: Option<&str>) -> Result<&'static str> {
    match requested.unwrap_or(DEFAULT_PERMISSION_MODE) {
        "accept_edits" => Ok("acceptEdits"),
        "plan" => Ok("plan"),
        other => bail!(
            "invalid permission_mode '{other}': must be 'accept_edits' (Claude Code may apply \
             file edits without prompting) or 'plan' (Claude Code may only plan, never edit or \
             run commands)"
        ),
    }
}

#[async_trait]
impl Tool for ClaudeCodeDelegateTool {
    fn name(&self) -> &str {
        "delegate_to_claude_code"
    }

    /// Worst case: identical envelope to `bash` — Claude Code can read,
    /// write, and run shell commands with real host authority once it
    /// starts, and nothing here confines it beyond its starting directory.
    /// See the module doc for why this is not classified `Destructive`
    /// (that bucket is reserved for actions on Finch's own session, such as
    /// `restart_session`) and why the name is peer hard-denied instead.
    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Delegate a self-contained coding task to the official Claude Code CLI, running \
         headlessly as itself — its own identity, its own tool use (file edits, shell commands), \
         backed by its own Anthropic subscription, not a Finch persona or wire protocol. Runs to \
         completion (no live streaming) and returns Claude Code's own final report, clearly \
         attributed as such. Use for a bounded, self-contained coding task better handled by a \
         full external agentic coding session than by Finch's own tools directly."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "task": {
                    "type": "string",
                    "description": "Self-contained task description handed to Claude Code as its prompt."
                },
                "directory": {
                    "type": "string",
                    "description": "Workspace-relative directory Claude Code starts in. Defaults to the workspace root. Must resolve inside the workspace."
                },
                "permission_mode": {
                    "type": "string",
                    "enum": ["accept_edits", "plan"],
                    "description": "accept_edits (default): Claude Code may apply file edits without prompting. plan: Claude Code may only analyze and plan, never edit or run commands."
                },
                "model": {
                    "type": "string",
                    "description": "Optional explicit model override passed to Claude Code (e.g. 'sonnet', 'opus')."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECS,
                    "description": "Maximum seconds to let Claude Code run before it is killed. Defaults to 600."
                }
            }),
            required: vec!["task".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task = input
            .get("task")
            .and_then(Value::as_str)
            .filter(|t| !t.trim().is_empty())
            .context("Missing required 'task' parameter")?;
        let directory = self.resolve_directory(input.get("directory").and_then(Value::as_str))?;
        let permission_mode =
            permission_mode_flag(input.get("permission_mode").and_then(Value::as_str))?;
        let model = input.get("model").and_then(Value::as_str);
        let timeout_secs = match input.get("timeout_secs") {
            None | Some(Value::Null) => DEFAULT_TIMEOUT_SECS,
            Some(value) => {
                let secs = value
                    .as_u64()
                    .context("'timeout_secs' must be a positive integer")?;
                if secs == 0 || secs > MAX_TIMEOUT_SECS {
                    bail!("'timeout_secs' must be between 1 and {MAX_TIMEOUT_SECS}");
                }
                secs
            }
        };

        let mut command = Command::new(&self.claude_binary);
        command
            .current_dir(&directory)
            .arg("--print")
            .arg("--output-format")
            .arg("json")
            .arg("--permission-mode")
            .arg(permission_mode)
            .arg("--permission-prompts")
            .arg("none");
        if let Some(model) = model {
            command.arg("--model").arg(model);
        }
        command.arg(task);
        command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = command.spawn().with_context(|| {
            format!(
                "Failed to spawn Claude Code CLI at '{}'. Is it installed and authenticated?",
                self.claude_binary.display()
            )
        })?;

        let output =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                .await
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Claude Code sub-agent timed out after {timeout_secs}s and was killed; no \
                     result was returned"
                    )
                })?
                .context("Failed to wait on Claude Code sub-agent process")?;

        Ok(format_report(
            task,
            &directory,
            output.status.code(),
            &output.stdout,
            &output.stderr,
        ))
    }

    fn workspace_root(&self) -> Option<&Path> {
        Some(&self.workspace_root)
    }
}

/// The subset of `claude --print --output-format json`'s result object this
/// tool reads. Unknown/missing fields are tolerated — a future CLI version
/// adding fields must not break parsing, and one that changes shape falls
/// back to raw text below.
#[derive(Debug, Deserialize, Default)]
struct ClaudeCodeResult {
    result: Option<String>,
    #[serde(default)]
    is_error: bool,
    session_id: Option<String>,
    total_cost_usd: Option<f64>,
    num_turns: Option<u64>,
}

/// Build the attributed transcript report from one completed invocation.
///
/// Always returns a report string, never an error: a nonzero exit or an
/// error Claude Code itself reported is information about the delegated
/// task, not a Finch tool-execution failure, the same convention `bash`
/// uses for a nonzero exit code.
fn format_report(
    task: &str,
    directory: &Path,
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> String {
    let stdout_text = String::from_utf8_lossy(stdout);
    let stderr_text = String::from_utf8_lossy(stderr);
    let parsed: Option<ClaudeCodeResult> = serde_json::from_str(stdout_text.trim()).ok();

    let mut report = String::new();
    report.push_str("=== Claude Code sub-agent (its own Anthropic-subscription-backed identity, not Finch) ===\n");
    report.push_str(&format!("Task handed to Claude Code: {task}\n"));
    report.push_str(&format!("Directory: {}\n", directory.display()));

    match &parsed {
        Some(result) => {
            let status = if result.is_error {
                "Claude Code reported an error"
            } else {
                "Claude Code completed the task"
            };
            report.push_str(&format!("Status: {status}\n"));
            if let Some(session_id) = &result.session_id {
                report.push_str(&format!("Session: {session_id}\n"));
            }
            if let Some(turns) = result.num_turns {
                report.push_str(&format!("Turns: {turns}\n"));
            }
            if let Some(cost) = result.total_cost_usd {
                report.push_str(&format!("Cost: ${cost:.4}\n"));
            }
            report.push_str("--- Claude Code's own result text ---\n");
            match &result.result {
                Some(text) if !text.is_empty() => report.push_str(text),
                _ => report.push_str("(Claude Code returned no result text)"),
            }
            report.push_str("\n--- end Claude Code output ---\n");
        }
        None => {
            // The CLI did not emit the expected JSON shape (crash, version
            // drift, or a genuinely empty run) — fall back to raw output so
            // nothing is silently dropped.
            report.push_str(
                "Status: Claude Code did not emit the expected structured result; showing raw output\n",
            );
            report.push_str("--- Claude Code raw stdout ---\n");
            report.push_str(stdout_text.trim());
            report.push_str("\n--- end raw stdout ---\n");
        }
    }

    if !stderr_text.trim().is_empty() {
        report.push_str("--- Claude Code stderr ---\n");
        report.push_str(stderr_text.trim());
        report.push('\n');
    }

    if let Some(code) = exit_code {
        if code != 0 {
            report.push_str(&format!("Exit code: {code}\n"));
        }
    } else {
        report.push_str("Exit: terminated by signal\n");
    }

    if report.len() > MAX_OUTPUT_CHARS {
        report.truncate(MAX_OUTPUT_CHARS);
        report.push_str("\n[Output truncated - showing first 20,000 characters]");
    }
    report
}

#[cfg(test)]
mod tests;
