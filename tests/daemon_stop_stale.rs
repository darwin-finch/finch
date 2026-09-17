//! `finch daemon-stop` must reap leftover files from a crashed daemon.
//!
//! The production defect was in `run_daemon_stop`: it returned "Daemon is not
//! running" whenever `is_running()` was false, so it never called
//! `stop_daemon` and left `~/.finch/daemon.pid` (and the IPC socket) behind.
//! A helper-only lifecycle test cannot see that skip; this drives the binary.

use std::path::Path;
use std::process::Command;

fn leftover_home() -> tempfile::TempDir {
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

fn write_dead_pid(home: &Path) {
    std::fs::write(home.join(".finch/daemon.pid"), "999999999").expect("write pid");
}

#[cfg(unix)]
fn leave_stale_socket(home: &Path) {
    use std::os::unix::net::UnixListener;
    let socket = home.join(".finch/daemon.sock");
    let listener = UnixListener::bind(&socket).expect("bind leftover socket");
    drop(listener);
    assert!(
        socket.exists(),
        "dropping the listener must leave the socket pathname; path={}",
        socket.display()
    );
}

fn daemon_stop(home: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("daemon-stop")
        .env("HOME", home)
        .output()
        .expect("run finch daemon-stop")
}

#[test]
fn test_daemon_stop_reaps_crashed_pid_and_socket() {
    let home = leftover_home();
    write_dead_pid(home.path());
    #[cfg(unix)]
    leave_stale_socket(home.path());

    let output = daemon_stop(home.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let pid_path = home.path().join(".finch/daemon.pid");
    let socket_path = home.path().join(".finch/daemon.sock");

    let mentions_pid = stdout.contains("cleaned leftover PID 999999999");
    #[cfg(unix)]
    let mentions_socket = stdout.contains("and socket");
    #[cfg(not(unix))]
    let mentions_socket = true;
    assert!(
        output.status.success()
            && mentions_pid
            && mentions_socket
            && !pid_path.exists()
            && !socket_path.exists(),
        "finch daemon-stop must reap a crashed daemon's leftover pid and socket rather than \
         claiming the daemon was never running; status={:?} stdout={stdout:?} stderr={stderr:?} \
         pid_exists={} socket_exists={}",
        output.status,
        pid_path.exists(),
        socket_path.exists()
    );
}

#[test]
fn test_daemon_stop_with_no_files_says_not_running() {
    let home = leftover_home();
    let output = daemon_stop(home.path());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.trim() == "Daemon is not running",
        "finch daemon-stop with no leftover files should say the daemon is not running; \
         status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
}
