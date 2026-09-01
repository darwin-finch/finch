//! Production-boundary regression for issue #240.
//!
//! `lsof` on the live daemon showed fds 1, 2, and 9 all open on the single
//! 705 MiB `~/.finch/daemon.log` inode, so both the tracing layer and the
//! inherited stdio stream were bound to it. An open descriptor shows binding,
//! not writes, so which stream produced the bulk of those bytes was never
//! measured. These tests bound the tracing stream; #249 binds the stdio one.
//!
//! They drive the real `tracing_subscriber` fmt layer over the same
//! `RotatingLog` writer that `run_daemon` installs, and assert the
//! bounded-retention outcome on disk.
//!
//! On the base revision `run_daemon` opened `daemon.log` with a plain
//! `OpenOptions::append(true)` handle and no rotation path existed, so the
//! assertions below about a rotated generation or a retention ceiling describe
//! behavior that revision does not have. The runtime proof is the separate
//! `claude/issue-240-negative-control` branch, which applies them to the
//! pre-fix logging init and fails.

use finch::daemon::{log_status, RotatingLog, RotationPolicy};
use std::io::Write;
use std::path::Path;
use tracing_subscriber::prelude::*;

/// Build the tracing stack exactly as `run_daemon` does: an fmt layer with
/// ANSI disabled, writing through a cloned `RotatingLog`.
fn with_daemon_logging<F: FnOnce()>(log: &RotatingLog, body: F) {
    let writer = log.clone();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || writer.clone())
        .with_ansi(false);
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(file_layer);
    tracing::subscriber::with_default(subscriber, body);
}

fn generation(path: &Path, index: usize) -> std::path::PathBuf {
    let mut name = path.file_name().unwrap().to_os_string();
    name.push(format!(".{index}"));
    path.with_file_name(name)
}

#[test]
fn test_daemon_tracing_output_rotates_within_retention_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");
    let policy = RotationPolicy {
        max_bytes: 4096,
        max_files: 3,
    };
    let log = RotatingLog::open(&path, policy).unwrap();

    with_daemon_logging(&log, || {
        for i in 0..400 {
            tracing::info!(request = i, "daemon request admitted");
        }
    });

    let status = log_status(&path, policy);
    assert!(
        generation(&path, 1).exists(),
        "sustained daemon tracing must rotate the active log"
    );
    assert!(
        !generation(&path, 4).exists(),
        "retention count must be exact; generation 4 exceeds max_files=3"
    );
    assert!(
        status.total_bytes() <= policy.retention_ceiling_bytes(),
        "daemon log grew to {} bytes, above the {} byte ceiling",
        status.total_bytes(),
        policy.retention_ceiling_bytes()
    );
}

#[test]
fn test_daemon_restart_preserves_prior_diagnostics_and_stays_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");
    let policy = RotationPolicy {
        max_bytes: 2048,
        max_files: 2,
    };

    for restart in 0..4 {
        let log = RotatingLog::open(&path, policy).unwrap();
        with_daemon_logging(&log, || {
            for i in 0..60 {
                tracing::info!(restart, event = i, "daemon lifecycle event");
            }
        });
    }

    let status = log_status(&path, policy);
    assert!(
        status.total_bytes() <= policy.retention_ceiling_bytes(),
        "repeated restarts grew the log to {} bytes, above the {} byte ceiling",
        status.total_bytes(),
        policy.retention_ceiling_bytes()
    );
    assert!(
        status.rotated_files > 0,
        "restarts must retain prior diagnostics as generations"
    );
}

#[test]
fn test_daemon_startup_migrates_an_inherited_oversized_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");
    // Stand-in for the inherited 705 MiB log from a build with no retention.
    std::fs::write(&path, "stale historical trace\n".repeat(4096)).unwrap();
    let inherited_bytes = std::fs::metadata(&path).unwrap().len();

    let log = RotatingLog::open(
        &path,
        RotationPolicy {
            max_bytes: 4096,
            max_files: 3,
        },
    )
    .unwrap();

    assert_eq!(
        log.status().active_bytes,
        0,
        "startup must begin a fresh active log"
    );
    assert_eq!(
        std::fs::metadata(generation(&path, 1)).unwrap().len(),
        inherited_bytes,
        "inherited diagnostics must be archived intact, never truncated or deleted"
    );
}

#[test]
fn test_daemon_log_write_after_rotation_is_not_lost() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");
    let mut log = RotatingLog::open(
        &path,
        RotationPolicy {
            max_bytes: 64,
            max_files: 2,
        },
    )
    .unwrap();

    for i in 0..20 {
        writeln!(log, "line-{i:03}-padding-padding").unwrap();
    }
    log.flush().unwrap();

    let active = std::fs::read_to_string(&path).unwrap();
    assert!(
        active.contains("line-019"),
        "the most recent write must land in the active file after rotation"
    );
}
