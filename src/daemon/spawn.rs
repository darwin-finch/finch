// Daemon auto-spawn utilities
//
// Provides functions to check if daemon is running and spawn it if needed.
// Used by CLI to automatically start daemon in background.

use anyhow::{bail, Context, Result};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::lifecycle::DaemonLifecycle;
use crate::errors;

use crate::config::constants::DEFAULT_DAEMON_ADDR as DEFAULT_BIND;

/// Ensure daemon is running, spawning if necessary
///
/// This function:
/// 1. Checks if daemon is responding to health checks
/// 2. If not, checks PID file for stale process
/// 3. If daemon not running, spawns it
/// 4. Waits for daemon to become ready (max 10 seconds)
///
/// Returns Ok(()) if daemon is ready, error otherwise.
pub async fn ensure_daemon_running(bind_address: Option<&str>) -> Result<()> {
    let bind = bind_address.unwrap_or(DEFAULT_BIND);
    let base_url = format!("http://{}", bind);

    // Quick health check first
    if health_check_succeeds(&base_url).await {
        debug!("Daemon already running and healthy");
        return Ok(());
    }

    // Check PID file
    let lifecycle = DaemonLifecycle::new()?;
    if lifecycle.is_running() {
        // Daemon process exists but not responding yet
        // Wait a bit and retry (it might be starting up)
        info!("Daemon process exists, waiting for health check...");
        tokio::time::sleep(Duration::from_secs(2)).await;

        if health_check_succeeds(&base_url).await {
            info!("Daemon now healthy");
            return Ok(());
        }

        warn!("Daemon process exists but not responding to health checks");
        let pid = lifecycle.read_pid()?;
        bail!(errors::wrap_error_with_suggestion(
            format!("Daemon is running (PID: {}) but not responding to health checks", pid),
            "Try stopping and restarting:\n\
             1. finch daemon-stop\n\
             2. finch daemon-start\n\n\
             Or check logs: tail -f ~/.finch/daemon.log"
        ));
    }

    // No daemon running, spawn it
    info!("Daemon not running, spawning...");
    spawn_daemon(bind)?;

    // Wait for daemon to start (max 10 seconds)
    for attempt in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;

        if health_check_succeeds(&base_url).await {
            info!("Daemon started successfully");
            return Ok(());
        }

        if attempt % 4 == 0 && attempt > 0 {
            debug!("Waiting for daemon to start... ({}/10s)", attempt / 2);
        }
    }

    bail!(errors::wrap_error_with_suggestion(
        "Daemon failed to start within 10 seconds",
        "Check daemon logs for errors:\n\
         tail -f ~/.finch/daemon.log\n\n\
         Common issues:\n\
         • Port already in use\n\
         • Insufficient permissions\n\
         • Missing dependencies"
    ))
}

/// Spawn daemon as background process
///
/// Detaches daemon from current process and redirects logs to ~/.finch/daemon.log
/// - Unix: Standard spawn with log file redirection
/// - Windows: Uses CREATE_NO_WINDOW flag to avoid console
pub fn spawn_daemon(bind_address: &str) -> Result<()> {
    let exe_path = std::env::current_exe()
        .context("Failed to determine current executable path")?;

    // Create log file in ~/.finch/daemon.log
    let log_path = dirs::home_dir()
        .context("Failed to determine home directory")?
        .join(".finch")
        .join("daemon.log");

    // Ensure .finch directory exists
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    // Open log file in append mode
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("Failed to open daemon log file: {}", log_path.display()))?;

    info!(
        exe = %exe_path.display(),
        bind = bind_address,
        log = %log_path.display(),
        "Spawning daemon subprocess"
    );

    #[cfg(target_family = "unix")]
    {
        Command::new(&exe_path)
            .arg("daemon")
            .arg("--bind")
            .arg(bind_address)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file.try_clone().context("Failed to clone log file handle")?))
            .stderr(Stdio::from(log_file))
            .spawn()
            .with_context(|| format!("Failed to spawn daemon: {}", exe_path.display()))?;
    }

    #[cfg(target_family = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        Command::new(&exe_path)
            .arg("daemon")
            .arg("--bind")
            .arg(bind_address)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file.try_clone().context("Failed to clone log file handle")?))
            .stderr(Stdio::from(log_file))
            .spawn()
            .with_context(|| format!("Failed to spawn daemon: {}", exe_path.display()))?;
    }

    debug!(log = %log_path.display(), "Daemon subprocess spawned, logs at {}", log_path.display());
    Ok(())
}

/// Check if daemon health endpoint responds
async fn health_check_succeeds(base_url: &str) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .expect("Failed to build HTTP client");

    let url = format!("{}/health", base_url);

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            debug!(url = %url, "Health check succeeded");
            true
        }
        Ok(response) => {
            debug!(url = %url, status = %response.status(), "Health check failed");
            false
        }
        Err(e) => {
            debug!(url = %url, error = %e, "Health check request failed");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_check_fails_for_invalid_url() {
        // Non-existent server should fail health check
        let result = health_check_succeeds("http://127.0.0.1:99999").await;
        assert!(!result);
    }
}
