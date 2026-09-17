//! `finch daemon-status` must not describe a live IPC listener as crash leftovers.
//!
//! The production defect was in `DaemonLifecycle::has_stale_files`: socket
//! existence alone made a live listener look like crashed-daemon leftovers,
//! and the suggested `finch daemon-stop` returns `StalePidLiveSocket` without
//! unlinking a live socket, so the warning could never clear. A lifecycle-only
//! test cannot see what the status command prints; this drives the binary with
//! a disposable HOME and a real bound Unix listener.

use std::path::Path;
use std::process::Command;

fn status_home() -> tempfile::TempDir {
    // Unix sockets cannot bind a default macOS TempDir path (SUN_LEN).
    #[cfg(unix)]
    let home = tempfile::Builder::new()
        .prefix("fds")
        .tempdir_in("/tmp")
        .expect("short tempdir");
    #[cfg(not(unix))]
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join(".finch")).expect("create .finch");
    home
}

#[cfg(unix)]
fn bind_live_listener(home: &Path) -> std::os::unix::net::UnixListener {
    let socket = home.join(".finch/daemon.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind live listener");
    assert!(
        socket.exists(),
        "a bound listener must leave the socket pathname in place; path={}",
        socket.display()
    );
    listener
}

fn daemon_status(home: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("daemon-status")
        .env("HOME", home)
        .output()
        .expect("run finch daemon-status")
}

#[cfg(unix)]
#[test]
fn test_daemon_status_with_live_listener_and_missing_pid_reports_still_up() {
    let home = status_home();
    let _listener = bind_live_listener(home.path());

    let output = daemon_status(home.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success()
            && stdout.contains("still has a live listener")
            && !stdout.contains("Leftover pid or socket files")
            && !stdout.contains("Clean them with")
            && !stdout.contains("Daemon is not running"),
        "daemon-status must report a live IPC listener as still up rather than as \
         crashed leftovers when the pid file is missing; status={:?} stdout={stdout:?} \
         stderr={stderr:?}",
        output.status
    );
}

#[cfg(unix)]
#[test]
fn test_daemon_status_with_live_listener_and_dead_pid_reports_still_up() {
    let home = status_home();
    let _listener = bind_live_listener(home.path());
    std::fs::write(home.path().join(".finch/daemon.pid"), "999999999")
        .expect("write dead pid file");

    let output = daemon_status(home.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success()
            && stdout.contains("still has a live listener")
            && !stdout.contains("Leftover pid or socket files")
            && !stdout.contains("Clean them with")
            && !stdout.contains("Daemon is not running"),
        "daemon-status must report a live IPC listener as still up rather than as \
         crashed leftovers when the pid file names a dead process; status={:?} \
         stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
}

#[test]
fn test_daemon_status_with_no_files_still_says_not_running_without_leftover_hint() {
    let home = status_home();

    let output = daemon_status(home.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success()
            && stdout.contains("Daemon is not running")
            && !stdout.contains("Leftover pid or socket files")
            && !stdout.contains("Clean them with"),
        "daemon-status with no daemon files must still report not-running with no \
         leftover-cleanup hint; status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
}
