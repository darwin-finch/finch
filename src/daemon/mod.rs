// Daemon module for background HTTP server mode
//
// This module provides daemon lifecycle management, auto-spawn capabilities,
// and utilities for running Shammah as a persistent background service.

pub mod lifecycle;
pub mod spawn;
pub mod upgrade;

pub use lifecycle::DaemonLifecycle;
pub use spawn::{ensure_daemon_running, spawn_daemon};
pub use upgrade::{DaemonUpgradePlan, VerifiedDaemonUpgrade};
