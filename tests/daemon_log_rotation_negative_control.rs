//! Negative control for issue #240, on the base revision `ce3fc976`.
//!
//! This reproduces the daemon logging path exactly as `run_daemon` builds it
//! before the fix: `OpenOptions::new().create(true).append(true)` wrapped in an
//! `Arc` and installed as a `tracing_subscriber` fmt writer, with no rotation
//! and no retention boundary.
//!
//! The assertions below are the ones issue #240 requires. They fail here, which
//! is the recorded evidence that the regression added on
//! `claude/issue-240-daemon-log-rotation` genuinely reproduces the reported
//! failure rather than testing a helper that always passed.
//!
//! This branch is evidence only and must never merge.

use std::fs::OpenOptions;
use std::sync::Arc;
use tracing_subscriber::prelude::*;

/// The ceiling the fixed implementation enforces for this policy:
/// `max_bytes` 4096 with 3 retained generations.
const CEILING_BYTES: u64 = 4096 * 4;

#[test]
fn test_daemon_tracing_output_rotates_within_retention_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("daemon.log");

    // Exactly the pre-fix run_daemon logging init.
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap();
    let file_writer = Arc::new(log_file);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(move || file_writer.clone())
        .with_ansi(false);
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(file_layer);

    tracing::subscriber::with_default(subscriber, || {
        for i in 0..400 {
            tracing::info!(request = i, "daemon request admitted");
        }
    });

    let mut generation = path.file_name().unwrap().to_os_string();
    generation.push(".1");
    let generation = path.with_file_name(generation);
    let total = std::fs::metadata(&path).unwrap().len();

    assert!(
        generation.exists(),
        "sustained daemon tracing must rotate the active log; \
         on the base revision no generation is ever created"
    );
    assert!(
        total <= CEILING_BYTES,
        "daemon log grew to {total} bytes, above the {CEILING_BYTES} byte ceiling"
    );
}
