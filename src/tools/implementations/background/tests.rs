//! Issue #754 production-boundary regressions: the lifecycle is exercised
//! through the real tools (`background_bash` / `background_poll` /
//! `background_stop`) against real child processes, proving tasks survive the
//! tool call (turn boundary) that started them.

use super::*;
use crate::brain::{BackgroundTaskManager, BackgroundTaskState};
use crate::tools::types::ToolContext;
use std::time::{Duration, Instant};

fn make_context() -> ToolContext<'static> {
    ToolContext::default()
}

fn tools() -> (
    BackgroundBashTool,
    BackgroundPollTool,
    BackgroundStopTool,
    Arc<BackgroundTaskManager>,
) {
    let manager = Arc::new(BackgroundTaskManager::new());
    (
        BackgroundBashTool::new(Arc::clone(&manager)),
        BackgroundPollTool::new(Arc::clone(&manager)),
        BackgroundStopTool::new(Arc::clone(&manager)),
        manager,
    )
}

fn task_id_from_start(result: &str) -> String {
    let marker = "Started background task ";
    let start = result
        .find(marker)
        .unwrap_or_else(|| panic!("start result must name the task: {result:?}"))
        + marker.len();
    let end = result[start..]
        .find(". ")
        .unwrap_or_else(|| panic!("start result must delimit the task id: {result:?}"));
    result[start..start + end].to_string()
}

/// The call must return before the command finishes: a `sleep 30` sentinel
/// proves no blocking.
#[tokio::test]
async fn test_background_bash_returns_id_immediately_for_a_long_lived_command() {
    let (bash, poll, stop, _manager) = tools();
    let started_at = Instant::now();
    let result = bash
        .execute(
            serde_json::json!({
                "command": "sleep 30",
                "description": "long-lived sentinel"
            }),
            &make_context(),
        )
        .await
        .expect("start must succeed");
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "start must return immediately, not block on the command; took {:?}",
        started_at.elapsed()
    );

    let id = task_id_from_start(&result);
    let polled = poll
        .execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("poll must succeed");
    assert!(
        polled.contains("Running"),
        "task must be running right after start returned: {polled:?}"
    );
    stop.execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("cleanup stop must succeed");
}

/// The gate "turn ends while task runs → task survives and stays capturable":
/// the tool call runs inside a spawned future (a stand-in for the turn), the
/// future is awaited to completion (the turn ends), and the task must still be
/// running and capturable afterwards — the foreground bash behavior of
/// `kill_on_drop(true)` must not carry over.
#[tokio::test]
async fn test_turn_end_leaves_task_running_and_capturable() {
    let (bash, poll, stop, manager) = tools();

    let turn = tokio::spawn(async move {
        bash.execute(
            serde_json::json!({
                "command": "sleep 30",
                "description": "outlives the turn"
            }),
            &make_context(),
        )
        .await
        .expect("start must succeed")
    });
    let result = turn
        .await
        .unwrap_or_else(|error| panic!("turn future panicked: {error}"));
    let id = task_id_from_start(&result);

    // The turn is over (its future completed); the reader tasks on the runtime
    // keep capturing output beyond it.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let polled = poll
        .execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("task must remain capturable after the turn ended");
    assert!(
        polled.contains("Running"),
        "task must survive the turn that started it: {polled:?}"
    );
    assert!(
        polled.contains("outlives the turn"),
        "poll must carry the task description: {polled:?}"
    );

    let snapshot = manager
        .poll(&id)
        .await
        .expect("manager-level poll after turn end");
    assert!(
        matches!(snapshot.state, BackgroundTaskState::Running),
        "manager state must still be Running after the turn: {:?}",
        snapshot.state
    );
    stop.execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("cleanup stop must succeed");
}

#[tokio::test]
async fn test_background_poll_reports_final_buffer_and_exit_status() {
    let (bash, poll, _stop, _manager) = tools();
    let result = bash
        .execute(
            serde_json::json!({
                "command": "printf 'x\\ny\\n'",
                "description": "buffer sentinel"
            }),
            &make_context(),
        )
        .await
        .expect("start must succeed");
    let id = task_id_from_start(&result);

    let deadline = Instant::now() + Duration::from_secs(10);
    let polled = loop {
        let polled = poll
            .execute(serde_json::json!({ "task_id": id }), &make_context())
            .await
            .expect("poll must succeed");
        if polled.contains("Completed") || Instant::now() > deadline {
            break polled;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert!(
        polled.contains("Completed (exit 0)"),
        "poll after completion must return the exit status: {polled:?}"
    );
    assert!(
        polled.contains('x') && polled.contains('y'),
        "poll after completion must return the final buffer: {polled:?}"
    );
}

#[tokio::test]
async fn test_background_stop_kills_via_tool_and_reports_terminal_status() {
    let (bash, poll, stop, _manager) = tools();
    let result = bash
        .execute(
            serde_json::json!({
                "command": "sleep 60",
                "description": "stop sentinel"
            }),
            &make_context(),
        )
        .await
        .expect("start must succeed");
    let id = task_id_from_start(&result);

    let stopped = stop
        .execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("stop must succeed");
    assert!(
        stopped.contains("Stopped") && stopped.contains("signal 9"),
        "stop must report the kill: {stopped:?}"
    );

    let polled = poll
        .execute(serde_json::json!({ "task_id": id }), &make_context())
        .await
        .expect("poll after stop must succeed");
    assert!(
        polled.contains("Stopped") && polled.contains("signal 9"),
        "poll after stop must return the recorded terminal status: {polled:?}"
    );
}

#[tokio::test]
async fn test_background_tools_fail_closed_on_unknown_or_missing_input() {
    let (bash, poll, stop, _manager) = tools();

    let unknown = poll
        .execute(
            serde_json::json!({ "task_id": "bg_missing" }),
            &make_context(),
        )
        .await;
    let error = unknown.expect_err("unknown task must fail closed");
    assert!(
        format!("{error:#}").contains("No such background task"),
        "error chain must name the missing task: {error:#}"
    );

    let unknown_stop = stop
        .execute(
            serde_json::json!({ "task_id": "bg_missing" }),
            &make_context(),
        )
        .await;
    assert!(
        unknown_stop.is_err(),
        "stop of an unknown task must fail closed"
    );

    let missing_command = bash
        .execute(
            serde_json::json!({ "description": "no command" }),
            &make_context(),
        )
        .await;
    let error = missing_command.expect_err("missing command must fail closed");
    assert!(
        format!("{error:#}").contains("Missing command parameter"),
        "error chain must name the missing parameter: {error:#}"
    );

    let missing_task_id = poll.execute(serde_json::json!({}), &make_context()).await;
    let error = missing_task_id.expect_err("missing task_id must fail closed");
    assert!(
        format!("{error:#}").contains("Missing task_id parameter"),
        "error chain must name the missing parameter: {error:#}"
    );
}
