//! Production-boundary regressions for `delegate_to_claude_code`.
//!
//! The real `claude` CLI needs an authenticated Anthropic subscription that
//! CI does not have, so every test here mocks the subprocess boundary with a
//! small fake executable script standing in for `claude --print
//! --output-format json ...` — exactly the seam `ClaudeCodeDelegateTool`
//! actually shells out through (`Command::new(self.claude_binary)`), so
//! these tests exercise the real code path, not a helper-only unit.

use super::*;
use crate::tools::types::ToolContext;
use std::fs;
use std::os::unix::fs::PermissionsExt;

fn make_context() -> ToolContext<'static> {
    ToolContext::default()
}

/// Write an executable `sh` script at `dir/claude` with `body` and return its
/// path. Tests point `ClaudeCodeDelegateTool::with_binary` at this instead of
/// the real CLI.
fn fake_claude(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("claude");
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake claude script");
    let mut perms = fs::metadata(&path).expect("stat fake claude").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod fake claude");
    path
}

fn success_json() -> &'static str {
    r#"{"result":"Did the requested thing.","is_error":false,"session_id":"sess-123","total_cost_usd":0.0456,"num_turns":3}"#
}

#[test]
fn test_declares_external_write_like_bash_and_spawn_task() {
    let workspace = tempfile::tempdir().expect("workspace");
    let tool = ClaudeCodeDelegateTool::new(workspace.path());
    assert_eq!(
        tool.effect(),
        ExecutionEffect::ExternalWrite,
        "delegate_to_claude_code grants the same real host authority bash and spawn_task do \
         (file writes, shell commands) once Claude Code starts, so it must declare the same \
         worst-case effect, not a weaker one"
    );
}

#[tokio::test]
async fn test_missing_task_parameter_is_rejected() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(workspace.path(), &format!("echo '{}'", success_json()));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool.execute(serde_json::json!({}), &make_context()).await;
    assert!(
        result.is_err(),
        "a call with no 'task' must be rejected before any process is spawned, got {result:?}"
    );
}

#[tokio::test]
async fn test_directory_escaping_workspace_root_is_rejected() {
    let workspace = tempfile::tempdir().expect("workspace");
    let outside = tempfile::tempdir().expect("outside dir");
    let bin = fake_claude(workspace.path(), &format!("echo '{}'", success_json()));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(
            serde_json::json!({
                "task": "do something",
                "directory": outside.path().to_str().unwrap(),
            }),
            &make_context(),
        )
        .await;
    let error = result.expect_err("a directory outside the workspace root must be rejected");
    assert!(
        error.to_string().contains("escapes the workspace root"),
        "error must name the escape, not fail some other way: {error}"
    );
}

#[tokio::test]
async fn test_nonexistent_directory_is_rejected_with_actionable_error() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(workspace.path(), &format!("echo '{}'", success_json()));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(
            serde_json::json!({"task": "do something", "directory": "does/not/exist"}),
            &make_context(),
        )
        .await;
    let error = result.expect_err("a nonexistent directory must be rejected");
    assert!(
        error.to_string().contains("does not exist"),
        "error must name the missing directory: {error}"
    );
}

#[tokio::test]
async fn test_permission_mode_cannot_be_escalated_past_the_declared_enum() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(workspace.path(), &format!("echo '{}'", success_json()));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(
            serde_json::json!({
                "task": "do something",
                // Not one of the two modes this tool exposes — in
                // particular, not bypassPermissions.
                "permission_mode": "bypassPermissions",
            }),
            &make_context(),
        )
        .await;
    let error = result.expect_err("an unsupported permission_mode must be rejected");
    assert!(
        error.to_string().contains("invalid permission_mode"),
        "error must name the rejected mode: {error}"
    );
}

#[tokio::test]
async fn test_success_report_is_attributed_to_claude_code_not_absorbed_as_finch_output() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(workspace.path(), &format!("echo '{}'", success_json()));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(serde_json::json!({"task": "add a test"}), &make_context())
        .await
        .expect("fake claude success must produce a report, not an error");
    assert!(
        result.contains("Claude Code"),
        "report must visibly attribute the output to Claude Code: {result:?}"
    );
    assert!(
        result.contains("Did the requested thing."),
        "report must carry Claude Code's own result text: {result:?}"
    );
    assert!(
        result.contains("sess-123") && result.contains("3") && result.contains("0.0456"),
        "report must carry session id, turn count, and cost so the delegation is auditable: {result:?}"
    );
    assert!(
        !result.contains("error"),
        "a successful run must not be reported as an error: {result:?}"
    );
}

#[tokio::test]
async fn test_claude_codes_own_error_is_reported_not_raised_as_a_finch_tool_failure() {
    let workspace = tempfile::tempdir().expect("workspace");
    let error_json = r#"{"result":"Could not complete: permission denied.","is_error":true,"session_id":"sess-err","num_turns":1}"#;
    let bin = fake_claude(workspace.path(), &format!("echo '{error_json}'"));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(
            serde_json::json!({"task": "do something impossible"}),
            &make_context(),
        )
        .await
        .expect(
            "Claude Code reporting its own task-level error is information, not a Finch \
             execution failure — the same convention bash uses for a nonzero exit code",
        );
    assert!(
        result.contains("Claude Code reported an error"),
        "report must surface Claude Code's own error state: {result:?}"
    );
    assert!(
        result.contains("Could not complete: permission denied."),
        "report must carry Claude Code's own error text verbatim: {result:?}"
    );
}

#[tokio::test]
async fn test_non_json_output_falls_back_to_raw_stdout_instead_of_dropping_it() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(
        workspace.path(),
        "echo 'not json, a crash before the JSON was emitted'; exit 1",
    );
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(serde_json::json!({"task": "do something"}), &make_context())
        .await
        .expect("unparseable output must still produce a report, not an error");
    assert!(
        result.contains("not json, a crash before the JSON was emitted"),
        "raw stdout must not be silently dropped when JSON parsing fails: {result:?}"
    );
    assert!(
        result.contains("Exit code: 1"),
        "the nonzero exit must be visible in the report: {result:?}"
    );
}

#[tokio::test]
async fn test_flags_pass_permission_mode_and_model_through_to_the_real_invocation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let capture = workspace.path().join("argv.txt");
    let bin = fake_claude(
        workspace.path(),
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\necho '{}'",
            capture.display(),
            success_json()
        ),
    );
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    tool.execute(
        serde_json::json!({
            "task": "plan a refactor",
            "permission_mode": "plan",
            "model": "opus",
        }),
        &make_context(),
    )
    .await
    .expect("invocation must succeed");

    let argv = fs::read_to_string(&capture).expect("fake claude must have captured argv");
    let args: Vec<&str> = argv.lines().collect();
    assert!(
        args.windows(2).any(|w| w == ["--permission-mode", "plan"]),
        "plan mode must reach the real invocation as --permission-mode plan, not acceptEdits: {args:?}"
    );
    assert!(
        args.windows(2)
            .any(|w| w == ["--permission-prompts", "none"]),
        "headless invocation must always pass --permission-prompts none so an unanswerable \
         prompt is denied instead of hanging the turn: {args:?}"
    );
    assert!(
        args.windows(2).any(|w| w == ["--model", "opus"]),
        "an explicit model override must reach the real invocation: {args:?}"
    );
    assert!(
        args.contains(&"--print"),
        "the invocation must run headlessly via --print: {args:?}"
    );
    assert!(
        args.last() == Some(&"plan a refactor"),
        "the task text must be passed as the prompt argument: {args:?}"
    );
}

#[tokio::test]
async fn test_timeout_kills_the_process_and_reports_an_error_not_a_hang() {
    let workspace = tempfile::tempdir().expect("workspace");
    let bin = fake_claude(workspace.path(), "sleep 5; echo 'should never print'");
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let started = std::time::Instant::now();
    let result = tool
        .execute(
            serde_json::json!({"task": "do something slow", "timeout_secs": 1}),
            &make_context(),
        )
        .await;
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "a timed-out call must return promptly after the timeout, not wait for the full sleep; \
         took {:?}",
        started.elapsed()
    );
    let error = result.expect_err("a run exceeding timeout_secs must return an error");
    assert!(
        error.to_string().contains("timed out"),
        "the error must name the timeout, not some other failure: {error}"
    );
}

#[tokio::test]
async fn test_missing_binary_reports_an_actionable_spawn_error() {
    let workspace = tempfile::tempdir().expect("workspace");
    let missing = workspace.path().join("no-such-claude-binary");
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), missing);
    let result = tool
        .execute(serde_json::json!({"task": "do something"}), &make_context())
        .await;
    let error = result.expect_err("spawning a nonexistent binary must fail, not panic");
    assert!(
        error
            .to_string()
            .contains("Failed to spawn Claude Code CLI"),
        "error must name what failed and how to fix it (is it installed?): {error}"
    );
}

#[tokio::test]
async fn test_large_output_is_truncated_like_bash_caps_its_output() {
    let workspace = tempfile::tempdir().expect("workspace");
    let huge = "x".repeat(30_000);
    let json = format!(r#"{{"result":"{huge}","is_error":false}}"#);
    let bin = fake_claude(workspace.path(), &format!("echo '{json}'"));
    let tool = ClaudeCodeDelegateTool::with_binary(workspace.path(), bin);
    let result = tool
        .execute(
            serde_json::json!({"task": "produce a lot of output"}),
            &make_context(),
        )
        .await
        .expect("a large result must still be reported, truncated, not dropped or erroring");
    assert!(
        result.len() <= MAX_OUTPUT_CHARS + 100,
        "report must be capped near the documented bound, got {} chars",
        result.len()
    );
    assert!(
        result.contains("truncated"),
        "truncation must be visible to the reader, not silent: {result:?}"
    );
}
