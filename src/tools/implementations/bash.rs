// Bash tool - executes shell commands with live output streaming

use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context, Result};
use async_trait::async_trait;
use finch_programs::ExecutionEffect;
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::propose::{propose_with_decision, ProposalDecision};

pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    /// Worst case: a shell command can write outside the workspace. The
    /// read-only refinement (`is_readonly_bash`) is applied at the
    /// approval call sites that consume this effect, not here — this
    /// method cannot see the command text.
    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Execute a host shell command when behavior exists only in an external program such as git, cargo, npm, or a build script. Never use bash, finch, target/debug/finch, echo, printf, or cat to execute, test, or display Finch Lisp/Co-Forth source: your final text response is already executed by the active Brain VM. For long-running commands (such as builds or tests), use `background_bash` or configure `timeout_secs`."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            ("command", "The bash command to execute"),
            ("description", "Brief description of what this command does"),
            (
                "timeout_secs",
                "Optional timeout in seconds (default: 30). For commands taking longer than 30 seconds, specify a higher timeout or use background_bash",
            ),
        ])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let command = input["command"]
            .as_str()
            .context("Missing command parameter")?;
        let description = input["description"].as_str().unwrap_or("");
        let timeout_secs = input
            .get("timeout_secs")
            .or_else(|| input.get("timeout"))
            .and_then(|v| {
                if let Some(n) = v.as_u64() {
                    Some(n)
                } else if let Some(s) = v.as_str() {
                    s.parse::<u64>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(30);

        // Propose the command in $EDITOR before running it — unless the REPL
        // already granted this call (bash:*, AutoAccept, or Yes).
        let script = if super::propose::context_should_open_interactive_review(context).await {
            match propose_with_decision(description, command).await? {
                ProposalDecision::Execute { source } => source,
                ProposalDecision::Chat { context } => {
                    return Ok(format!(
                        "Tool call not executed. The user asked for a different command instead:\n{context}"
                    ))
                }
                ProposalDecision::Cancel => return Ok("Tool call aborted by user.".to_string()),
            }
        } else {
            command.to_string()
        };

        let mut command = Command::new("bash");
        command
            .arg("-c")
            .arg(&script)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The outer coordinator deliberately does not time the editor
            // review. Once a script is accepted, however, the actual process
            // remains bounded and is terminated if this future is dropped.
            .kill_on_drop(true);

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }

        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to spawn command: {}", script))?;

        #[cfg(unix)]
        let mut pg_guard = ProcessGroupGuard {
            pgid: child.id().map(|pid| pid as i32),
        };

        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        // Clone the live output callback for the stdout reader
        let live_cb = context.live_output.clone();

        let outcome = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
            // Drain stderr in a background task so it doesn't block stdout reading.
            let stderr_task = tokio::spawn(async move {
                let mut buf = String::new();
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    buf.push_str(&line);
                    buf.push('\n');
                }
                buf
            });

            // Drain stdout on this task, calling the live-output callback per line.
            let mut stdout_buf = String::new();
            let mut stdout_lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = stdout_lines.next_line().await {
                if let Some(ref cb) = live_cb {
                    cb.line(line.clone());
                }
                stdout_buf.push_str(&line);
                stdout_buf.push('\n');
            }

            let stderr_buf = stderr_task.await.unwrap_or_default();
            let exit_status = child.wait().await?;
            Ok::<_, anyhow::Error>((stdout_buf, stderr_buf, exit_status))
        })
        .await;

        let (stdout_buf, stderr_buf, exit_status) = match outcome {
            Ok(res) => {
                #[cfg(unix)]
                {
                    pg_guard.pgid = None;
                }
                res?
            }
            Err(_) => {
                #[cfg(unix)]
                {
                    pg_guard.pgid = None;
                }
                terminate_process_tree(&mut child).await;
                let unit = if timeout_secs == 1 {
                    "second"
                } else {
                    "seconds"
                };
                anyhow::bail!(
                    "Command timed out after {timeout_secs} {unit}. The process and all of its \
                     child processes were terminated. For long-running commands (such as builds \
                     or tests), use `background_bash` to run them in the background (and monitor \
                     them with `background_poll` or stop them with `background_stop`), or specify \
                     a larger `timeout_secs`."
                );
            }
        };
        let exit_code = exit_status.code().unwrap_or(-1);

        let mut result = stdout_buf;

        if !stderr_buf.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str("STDERR:\n");
            result.push_str(&stderr_buf);
        }

        if exit_code != 0 {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&format!("Exit code: {}", exit_code));
        }

        // Limit to 20,000 chars
        if result.len() > 20_000 {
            Ok(format!(
                "{}\n\n[Output truncated - showing first 20,000 characters]",
                &result[..20_000]
            ))
        } else {
            Ok(result)
        }
    }
}

#[cfg(unix)]
struct ProcessGroupGuard {
    pgid: Option<i32>,
}

#[cfg(unix)]
impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(-pgid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[cfg(unix)]
fn find_descendant_pids(root_pid: u32) -> Vec<u32> {
    let output = match std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=,ppid=,pgid="])
        .output()
        .or_else(|_| {
            std::process::Command::new("/bin/ps")
                .args(["-ax", "-o", "pid=,ppid=,pgid="])
                .output()
        }) {
        Ok(out) if out.status.success() => out.stdout,
        _ => return Vec::new(),
    };
    let Ok(text) = String::from_utf8(output) else {
        return Vec::new();
    };

    let mut parent_to_children: std::collections::HashMap<u32, Vec<u32>> =
        std::collections::HashMap::new();
    let mut pgid_members: Vec<u32> = Vec::new();

    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Some(pgid) = parts.next().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };

        if pid == root_pid {
            continue;
        }

        if pgid == root_pid {
            pgid_members.push(pid);
        }
        parent_to_children.entry(ppid).or_default().push(pid);
    }

    let mut descendants = std::collections::HashSet::new();
    for member in pgid_members {
        descendants.insert(member);
    }

    let mut queue = vec![root_pid];
    while let Some(parent) = queue.pop() {
        if let Some(children) = parent_to_children.get(&parent) {
            for &child in children {
                if descendants.insert(child) {
                    queue.push(child);
                }
            }
        }
    }

    descendants.into_iter().collect()
}

#[cfg(unix)]
async fn terminate_process_tree(child: &mut tokio::process::Child) {
    let Some(root_pid) = child.id() else {
        return;
    };

    let descendants = find_descendant_pids(root_pid);
    let pgid = root_pid as i32;

    // Send SIGKILL to the entire process group (-pgid).
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(-pgid),
        nix::sys::signal::Signal::SIGKILL,
    );

    // Explicitly send SIGKILL to each descendant as well.
    for pid in &descendants {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(*pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    // Kill the root process and reap it so it is never a zombie.
    let _ = child.start_kill();
    let _ = child.wait().await;

    // Confirm that all descendants have terminated.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        let mut any_alive = false;
        for pid in &descendants {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(*pid as i32), None).is_ok() {
                any_alive = true;
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(*pid as i32),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
        if !any_alive {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[cfg(not(unix))]
async fn terminate_process_tree(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context() -> ToolContext<'static> {
        ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: None,
            effect_audit: None,
            grant_ceiling: None,
            skip_interactive_review: false,
        }
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "echo 'Hello, World!'",
            "description": "Test echo command"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Hello, World!"));
    }

    #[tokio::test]
    async fn test_bash_ls() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls Cargo.toml",
            "description": "List Cargo.toml"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_bash_nonzero_exit() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "ls /nonexistent",
            "description": "Try to list nonexistent directory"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_ok());
        let output = result.unwrap();
        assert!(output.contains("Exit code:") || output.contains("STDERR"));
    }

    #[tokio::test]
    async fn test_bash_live_output_callback_receives_lines() {
        use std::sync::{Arc, Mutex};
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "printf 'line1\\nline2\\nline3\\n'",
            "description": "Test live output streaming"
        });

        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let cb: crate::tools::types::LiveOutput = Arc::new(move |line: String| {
            received_clone.lock().unwrap().push(line);
        });

        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: Some(cb),
            effect_audit: None,
            grant_ceiling: None,
            skip_interactive_review: false,
        };

        let result = tool.execute(input, &context).await.unwrap();
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));

        let lines = received.lock().unwrap();
        assert!(
            lines.contains(&"line1".to_string()),
            "callback must receive line1: {:?}",
            *lines
        );
        assert!(
            lines.contains(&"line2".to_string()),
            "callback must receive line2: {:?}",
            *lines
        );
        assert!(
            lines.contains(&"line3".to_string()),
            "callback must receive line3: {:?}",
            *lines
        );
    }

    #[tokio::test]
    async fn test_bash_live_output_callback_receives_lines_in_order() {
        use std::sync::{Arc, Mutex};
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "for i in 1 2 3 4 5; do echo \"item$i\"; done",
            "description": "Test ordering"
        });

        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);
        let cb: crate::tools::types::LiveOutput = Arc::new(move |line: String| {
            received_clone.lock().unwrap().push(line);
        });

        let context = ToolContext {
            save_models: None,
            host_mode_state: None,
            plan_content: None,
            live_output: Some(cb),
            effect_audit: None,
            grant_ceiling: None,
            skip_interactive_review: false,
        };

        tool.execute(input, &context).await.unwrap();
        let lines = received.lock().unwrap();
        assert_eq!(lines.len(), 5, "should receive 5 lines: {:?}", *lines);
        assert_eq!(lines[0], "item1");
        assert_eq!(lines[4], "item5");
    }

    #[tokio::test]
    async fn test_bash_no_callback_still_returns_output() {
        // When live_output is None, tool must still return complete output
        let tool = BashTool;
        let input = serde_json::json!({ "command": "echo hello" });
        let result = tool.execute(input, &make_context()).await.unwrap();
        assert!(result.trim().contains("hello"));
    }

    #[tokio::test]
    async fn test_bash_timeout_terminates_entire_process_tree() {
        let tool = BashTool;
        let temp_dir = tempfile::tempdir().unwrap();
        let marker_file = temp_dir.path().join("child.pid");
        let marker_path = marker_file.to_string_lossy().to_string();

        // Spawn a background child process from bash, record its PID, and wait in bash.
        // On timeout, both bash and the background child process must be terminated.
        let command_str = format!("sh -c 'sleep 60 & echo $! > \"{marker_path}\"; wait'");
        let input = serde_json::json!({
            "command": command_str,
            "timeout_secs": 1,
            "description": "Test process tree termination on timeout"
        });

        let start = std::time::Instant::now();
        let result = tool.execute(input, &make_context()).await;
        let elapsed = start.elapsed();

        assert!(result.is_err(), "command must fail on timeout");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("timed out after 1 second"),
            "error message must state the timeout limit: {err_msg}"
        );
        assert!(
            err_msg.contains("background_bash"),
            "error message must state what to do instead (background_bash): {err_msg}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "timeout must trigger quickly, took {elapsed:?}"
        );

        // Read the child PID from the marker file
        assert!(
            marker_file.exists(),
            "child pid file must have been created"
        );
        let pid_str = std::fs::read_to_string(&marker_file).unwrap();
        let child_pid: u32 = pid_str.trim().parse().expect("valid pid");

        // Verify the child process is NOT alive
        #[cfg(unix)]
        {
            let alive =
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(child_pid as i32), None).is_ok();
            assert!(
                !alive,
                "child process {child_pid} must be terminated across its tree, but was still alive"
            );
        }
    }

    #[tokio::test]
    async fn test_bash_configurable_timeout_succeeds() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "sleep 1 && echo finished",
            "timeout_secs": 5,
            "description": "Test configurable timeout allows command to finish"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(
            result.is_ok(),
            "command must succeed within extended timeout: {:?}",
            result
        );
        assert!(result.unwrap().contains("finished"));
    }

    #[tokio::test]
    async fn test_bash_default_timeout_message() {
        let tool = BashTool;
        let input = serde_json::json!({
            "command": "sleep 2",
            "timeout_secs": 1,
            "description": "Test timeout message phrasing"
        });
        let result = tool.execute(input, &make_context()).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Command timed out after 1 second"));
        assert!(err.contains("background_bash"));
        assert!(err.contains("background_poll"));
        assert!(err.contains("background_stop"));
        assert!(err.contains("timeout_secs"));
    }
}
