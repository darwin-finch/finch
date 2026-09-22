// HTTP client for daemon communication
//
// Provides DaemonClient for CLI to communicate with background daemon.
// Handles auto-spawn, health checks, and message passing.

mod daemon_client;
pub(crate) mod ipc;

pub use daemon_client::{DaemonClient, DaemonConfig, LocalModelDownloadStatus, LocalModelStatus};
pub use ipc::{BrainRunnerBootstrap, BrainSubmissionResult, IpcClient, QueryResponse};
