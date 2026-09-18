//! Issue #754 regressions for the background task lifecycle.
//!
//! Each test pins a contract from the solution contract: beyond-turn survival,
//! stop kills and reaps, bounded retention and count, exactly-once terminal
//! state, and daemon-death reaping. Process liveness is asserted through
//! `kill(pid, 0)`: `ESRCH` proves the child was reaped, not merely signalled.

use super::*;
use std::time::{Duration, Instant};

const GENEROUS_LIVENESS_BOUND: Duration = Duration::from_secs(10);

fn process_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as nix::libc::pid_t), None).is_ok()
}

async fn wait_terminal(
    manager: &BackgroundTaskManager,
    id: &str,
    context: &str,
) -> BackgroundTaskSnapshot {
    let deadline = Instant::now() + GENEROUS_LIVENESS_BOUND;
    loop {
        match manager.poll(id).await {
            Ok(snapshot) if !snapshot.state.is_running() => return snapshot,
            Ok(snapshot) => {
                if Instant::now() > deadline {
                    panic!(
                        "background task did not reach terminal state within \
                         {GENEROUS_LIVENESS_BOUND:?} ({context}): still {:?}, \
                         stdout={:?}, stderr={:?}",
                        snapshot.state, snapshot.stdout, snapshot.stderr
                    );
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(error) => panic!("poll of background task {id} failed ({context}): {error}"),
        }
    }
}

#[tokio::test]
async fn test_start_returns_id_before_completion_and_task_survives_the_call() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("sleep 30", "long-lived sentinel")
        .await
        .expect("start must succeed");
    assert!(id.as_str().starts_with("bg_"), "stable id shape: {id}");

    let snapshot = manager.poll(id.as_str()).await.expect("pollable");
    assert!(
        matches!(snapshot.state, BackgroundTaskState::Running),
        "task must be running after start returned: {:?}",
        snapshot.state
    );
    let pid = snapshot
        .pid
        .expect("recorded pid must be present for a running task");
    assert!(
        process_alive(pid),
        "recorded pid {pid} must be alive after start returned"
    );
    manager.stop(id.as_str()).await.expect("cleanup stop");
}

#[cfg(unix)]
#[tokio::test]
async fn test_background_child_stays_in_supervisor_process_group() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("sleep 30", "process-group sentinel")
        .await
        .expect("start must succeed");
    let snapshot = manager.poll(id.as_str()).await.expect("pollable");
    let pid = snapshot.pid.expect("running task records its pid");

    let child_group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid as i32)))
        .expect("child process group must be queryable");
    assert_eq!(
        child_group,
        nix::unistd::getpgrp(),
        "background child {pid} must stay in the owning process's group \
         (no setsid/setpgid/process-group escape)"
    );
    manager.stop(id.as_str()).await.expect("cleanup stop");
}

#[cfg(unix)]
#[tokio::test]
async fn test_stop_kills_and_reaps_recorded_process() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("sleep 60", "stop sentinel")
        .await
        .expect("start must succeed");
    let pid = manager
        .poll(id.as_str())
        .await
        .expect("pollable")
        .pid
        .expect("running task records its pid");

    let snapshot = manager.stop(id.as_str()).await.expect("stop must succeed");
    assert_eq!(
        snapshot.state,
        BackgroundTaskState::Stopped(ExitOutcome::Signal(9)),
        "stop must record SIGKILL as the terminal outcome: {:?}",
        snapshot.state
    );
    assert!(
        !process_alive(pid),
        "recorded pid {pid} must be reaped after stop: kill(pid, 0) \
         must fail with ESRCH, proving the zombie was reaped"
    );

    // Idempotent: a second stop returns the recorded terminal snapshot and
    // signals nothing.
    let again = manager.stop(id.as_str()).await.expect("second stop");
    assert_eq!(
        again, snapshot,
        "second stop must return the identical terminal snapshot: {again:?}"
    );
}

#[tokio::test]
async fn test_poll_after_complete_returns_final_buffer_and_exit_status_exactly_once() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("printf 'a\\nb\\nc\\n'", "completion sentinel")
        .await
        .expect("start must succeed");

    let snapshot = wait_terminal(&manager, id.as_str(), "poll-after-complete").await;
    assert_eq!(
        snapshot.state,
        BackgroundTaskState::Completed(Some(0)),
        "natural completion must record the exit code: {:?}",
        snapshot.state
    );
    assert_eq!(
        snapshot.stdout, "a\nb\nc",
        "final poll must return the complete captured buffer"
    );
    assert_eq!(snapshot.stdout_discarded_lines, 0, "nothing was evicted");
    assert!(
        snapshot.stderr.is_empty(),
        "stderr ring must be empty for a quiet task: {:?}",
        snapshot.stderr
    );

    let again = manager.poll(id.as_str()).await.expect("repeat poll");
    assert_eq!(
        again, snapshot,
        "repeat polls of a completed task must return the identical \
         snapshot (terminal state recorded exactly once)"
    );
}

#[tokio::test]
async fn test_retention_budget_bounds_memory() {
    let ring_budget = 4096usize;
    let manager = BackgroundTaskManager::with_limits(16, 64, ring_budget);
    let id = manager
        .start("seq 1 200000", "retention flood")
        .await
        .expect("start must succeed");

    let snapshot = wait_terminal(&manager, id.as_str(), "retention flood").await;
    let retained = snapshot.stdout.len();
    // The ring may overshoot the budget by at most the final pushed line.
    let overshoot_bound = ring_budget + "200000".len() + 1;
    assert!(
        retained <= overshoot_bound,
        "retention must bound memory: retained {retained} bytes exceeds the \
         {ring_budget}-byte budget plus one-line overshoot {overshoot_bound}"
    );
    assert!(
        snapshot.stdout_discarded_lines > 0,
        "a 200000-line flood against a {ring_budget}-byte budget must discard lines"
    );
    assert!(
        snapshot.stdout.ends_with("200000"),
        "retention keeps the newest lines: {:?}",
        snapshot.stdout
    );
}

#[tokio::test]
async fn test_running_bound_rejects_start_beyond_limit() {
    let manager = BackgroundTaskManager::with_limits(1, 8, 1024);
    let first = manager
        .start("sleep 30", "occupies the only running slot")
        .await
        .expect("first start must succeed");

    let rejected = manager
        .start("echo hi", "second start beyond the running bound")
        .await;
    let error = rejected.expect_err("start beyond the running bound must fail");
    assert!(
        error.to_string().contains("limit reached"),
        "rejection must name the bound: {error:#}"
    );

    manager.stop(first.as_str()).await.expect("cleanup stop");
    let after = manager
        .start("echo hi", "start after a stop frees the slot")
        .await;
    assert!(
        after.is_ok(),
        "a stop must free a running slot: {error:#} was the earlier rejection"
    );
    manager
        .stop(after.expect("started").as_str())
        .await
        .expect("cleanup stop");
}

#[tokio::test]
async fn test_total_bound_reaps_oldest_finished_task() {
    let manager = BackgroundTaskManager::with_limits(4, 2, 1024);
    let first = manager
        .start("true", "oldest finished")
        .await
        .expect("start");
    wait_terminal(&manager, first.as_str(), "total-bound reap").await;
    let second = manager
        .start("true", "second finished")
        .await
        .expect("start");
    wait_terminal(&manager, second.as_str(), "total-bound reap").await;

    let third = manager
        .start("true", "forces a reap of the oldest finished entry")
        .await;
    assert!(
        third.is_ok(),
        "a finished entry must be reaped to make room: {:?}",
        third.err()
    );
    let unknown = manager.poll(first.as_str()).await;
    assert!(
        unknown.is_err(),
        "the reaped oldest task must no longer be pollable: {:?}",
        unknown
    );
    assert_eq!(
        manager.total_count().await,
        2,
        "total retained entries must respect the total bound"
    );
    for id in [second, third.expect("started")] {
        manager.stop(id.as_str()).await.expect("cleanup stop");
    }
}

#[tokio::test]
async fn test_total_bound_rejects_when_all_slots_running() {
    let manager = BackgroundTaskManager::with_limits(4, 2, 1024);
    let a = manager
        .start("sleep 30", "running slot a")
        .await
        .expect("start");
    let b = manager
        .start("sleep 30", "running slot b")
        .await
        .expect("start");

    let rejected = manager.start("echo hi", "no slot is finished").await;
    let error = rejected.expect_err("a start with no reapable slot must fail");
    assert!(
        error.to_string().contains("still running"),
        "rejection must say every retained task is running: {error:#}"
    );
    manager.stop(a.as_str()).await.expect("cleanup stop");
    manager.stop(b.as_str()).await.expect("cleanup stop");
}

#[cfg(unix)]
#[tokio::test]
async fn test_late_completion_after_stop_does_not_overwrite_stopped() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("sleep 0.5", "would have completed naturally")
        .await
        .expect("start must succeed");

    let stopped = manager.stop(id.as_str()).await.expect("stop must succeed");
    assert_eq!(
        stopped.state,
        BackgroundTaskState::Stopped(ExitOutcome::Signal(9)),
        "the stop that kills first owns the terminal state: {:?}",
        stopped.state
    );

    // Outlive the would-be natural completion: the watcher has exited and no
    // late completion may overwrite the recorded terminal state.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let later = manager.poll(id.as_str()).await.expect("pollable");
    assert_eq!(
        later.state, stopped.state,
        "late completion must not overwrite the recorded stop: {:?}",
        later.state
    );
}

#[tokio::test]
async fn test_stop_after_completion_preserves_completed_status() {
    let manager = BackgroundTaskManager::new();
    let id = manager
        .start("echo done", "completes before stop")
        .await
        .expect("start");
    let completed = wait_terminal(&manager, id.as_str(), "stop-after-complete").await;

    let stopped = manager
        .stop(id.as_str())
        .await
        .expect("stop after complete");
    assert_eq!(
        stopped, completed,
        "stopping a completed task must preserve the recorded completion \
         exactly once and signal nothing: {stopped:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_shutdown_all_kills_and_reaps_every_running_task() {
    let manager = BackgroundTaskManager::new();
    let mut sentinels = Vec::new();
    for index in 0..3 {
        let id = manager
            .start("sleep 60", "daemon-death sentinel")
            .await
            .unwrap_or_else(|error| panic!("start {index} must succeed: {error}"));
        let pid = manager
            .poll(id.as_str())
            .await
            .expect("pollable")
            .pid
            .expect("running task records its pid");
        sentinels.push((id, pid));
    }

    manager.shutdown_all().await;

    for (id, pid) in &sentinels {
        assert!(
            !process_alive(*pid),
            "daemon death must reap task pid {pid}: kill(pid, 0) must fail \
             with ESRCH"
        );
        let snapshot = manager
            .poll(id.as_str())
            .await
            .unwrap_or_else(|error| panic!("task record {id} must remain pollable: {error}"));
        assert_eq!(
            snapshot.state,
            BackgroundTaskState::Stopped(ExitOutcome::Signal(9)),
            "shutdown must record the kill for every running task: {:?}",
            snapshot.state
        );
    }
}

#[tokio::test]
async fn test_poll_unknown_task_returns_error() {
    let manager = BackgroundTaskManager::new();
    let error = manager
        .poll("bg_does_not_exist")
        .await
        .expect_err("unknown id must fail closed");
    assert!(
        error.to_string().contains("No such background task"),
        "error must name the missing task: {error:#}"
    );
}

#[tokio::test]
async fn test_concurrent_polls_and_stops_converge_to_one_terminal_state() {
    let manager = Arc::new(BackgroundTaskManager::new());
    let id = manager
        .start("sleep 1", "concurrent hostile timing")
        .await
        .expect("start must succeed");
    let id_text = id.as_str().to_string();

    let mut handles = Vec::new();
    for index in 0..3 {
        let manager = Arc::clone(&manager);
        let id_text = id_text.clone();
        handles.push(tokio::spawn(async move {
            manager
                .poll(&id_text)
                .await
                .map(|snapshot| snapshot.state)
                .map_err(|error| format!("poll {index} failed: {error}"))
        }));
    }
    for index in 0..3 {
        let manager = Arc::clone(&manager);
        let id_text = id_text.clone();
        handles.push(tokio::spawn(async move {
            let result = manager.stop(&id_text).await;
            match result {
                Ok(snapshot) => Ok(snapshot.state),
                // A concurrent stop that already took the child fails closed;
                // that is honest, not a terminal-state violation.
                Err(error) if error.to_string().contains("already in progress") => {
                    Ok(BackgroundTaskState::Running)
                }
                Err(error) => Err(format!("stop {index} failed: {error}")),
            }
        }));
    }

    let mut outcomes = Vec::new();
    for handle in handles {
        outcomes.push(
            handle
                .await
                .unwrap_or_else(|error| panic!("hostile-timing task panicked: {error}")),
        );
    }
    for outcome in &outcomes {
        assert!(
            outcome.is_ok(),
            "every concurrent observer must fail closed, not panic: {outcomes:?}"
        );
    }

    let final_snapshot = manager
        .poll(id.as_str())
        .await
        .expect("the task record must remain pollable");
    assert!(
        !final_snapshot.state.is_running(),
        "exactly-once terminal state must hold after hostile concurrent \
         polling and stopping: {:?}",
        final_snapshot.state
    );
    let repeat = manager.poll(id.as_str()).await.expect("repeat poll");
    assert_eq!(
        repeat, final_snapshot,
        "terminal state must be stable across repeat polls: {repeat:?}"
    );
    manager.shutdown_all().await;
}

#[test]
fn test_output_ring_evicts_oldest_and_counts_discards() {
    let mut ring = OutputRing::new(12);
    ring.push("aaaa".to_string()); // 5 bytes with newline
    ring.push("bbbb".to_string()); // 5 bytes, total 10
    ring.push("cc".to_string()); // 3 bytes, total 13 > 12: evict "aaaa"
    let (snapshot, discarded) = ring.snapshot();
    assert_eq!(snapshot, "bbbb\ncc", "oldest lines must be evicted first");
    assert_eq!(discarded, 1, "each eviction must be counted");
}
