// Daemon module for background HTTP server mode
//
// This module provides daemon lifecycle management, auto-spawn capabilities,
// and utilities for running Shammah as a persistent background service.

pub mod lifecycle;
pub mod log;
pub mod spawn;
pub mod upgrade;

/// Set by `spawn_daemon` on the detached child. Only a process carrying it
/// takes over its own stdout and stderr; the documented foreground modes
/// (`finch daemon` in a terminal, `finch worker`, the shipped systemd unit)
/// keep writing to the terminal or the journal.
pub const DETACHED_DAEMON_ENV: &str = "FINCH_DAEMON_DETACHED";

pub use self::log::{daemon_log_path, log_status, LogStatus, RotatingLog, RotationPolicy};
pub use lifecycle::DaemonLifecycle;
pub use spawn::{ensure_daemon_running, spawn_daemon};
pub use upgrade::{DaemonUpgradePlan, VerifiedDaemonUpgrade};
