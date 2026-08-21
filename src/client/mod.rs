// HTTP client for daemon communication
//
// Provides DaemonClient for CLI to communicate with background daemon.
// Handles auto-spawn, health checks, and message passing.

mod daemon_client;

pub use daemon_client::{DaemonClient, DaemonConfig, LocalModelStatus};
