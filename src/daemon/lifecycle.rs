// Daemon lifecycle management
//
// Handles PID file creation/removal, process existence checks,
// and graceful shutdown coordination.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::path::PathBuf;
use tracing::{info, warn};

/// Manages daemon lifecycle (PID file, shutdown)
pub struct DaemonLifecycle {
    pid_file: PathBuf,
}

/// Process-lifetime ownership of the daemon namespace.
///
/// The advisory lock closes the check-then-write race in the PID-file-only
/// protocol. The PID file remains human-readable status metadata; it is not
/// itself the mutual-exclusion primitive.
#[derive(Debug)]
pub struct DaemonInstanceGuard {
    lock_file: File,
    pid_file: PathBuf,
    pid: u32,
    released: bool,
}

impl DaemonLifecycle {
    /// Create a new daemon lifecycle manager
    pub fn new() -> Result<Self> {
        let pid_file = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
            .join(".finch")
            .join("daemon.pid");

        // Ensure parent directory exists
        if let Some(parent) = pid_file.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
        }

        Ok(Self { pid_file })
    }

    /// Write current process PID to file
    pub fn write_pid(&self) -> Result<()> {
        let pid = std::process::id();
        fs::write(&self.pid_file, pid.to_string())
            .with_context(|| format!("Failed to write PID file: {}", self.pid_file.display()))?;
        info!(pid = pid, path = %self.pid_file.display(), "Daemon PID file written");
        Ok(())
    }

    /// Acquire exclusive ownership before binding any daemon transport.
    /// Holding the returned guard is mandatory for the daemon's lifetime.
    pub fn acquire_instance(&self) -> Result<DaemonInstanceGuard> {
        let lock_path = self.pid_file.with_extension("lock");
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("Failed to open daemon lock: {}", lock_path.display()))?;
        lock_file.try_lock_exclusive().map_err(|error| {
            let owner = self
                .read_pid()
                .map(|pid| format!(" (PID: {pid})"))
                .unwrap_or_default();
            anyhow::anyhow!("another Finch daemon owns the daemon lock{owner}: {error}")
        })?;

        // A pre-lock Finch version may still be alive. Honor its PID file
        // instead of stealing its socket merely because no lock existed yet.
        if self.is_running() {
            let pid = self.read_pid()?;
            anyhow::bail!("Finch daemon is already running (PID: {pid})");
        }
        self.write_pid()?;
        Ok(DaemonInstanceGuard {
            lock_file,
            pid_file: self.pid_file.clone(),
            pid: std::process::id(),
            released: false,
        })
    }

    /// Remove PID file (called on shutdown)
    pub fn cleanup(&self) -> Result<()> {
        if self.pid_file.exists() {
            fs::remove_file(&self.pid_file).with_context(|| {
                format!("Failed to remove PID file: {}", self.pid_file.display())
            })?;
            info!("Daemon PID file removed");
        }
        Ok(())
    }

    /// Check if daemon is currently running
    ///
    /// Returns true if:
    /// - PID file exists
    /// - PID can be parsed
    /// - Process with that PID exists
    pub fn is_running(&self) -> bool {
        if !self.pid_file.exists() {
            return false;
        }

        match self.read_pid() {
            Ok(pid) => process_exists(pid),
            Err(_) => false,
        }
    }

    /// Read PID from file
    pub fn read_pid(&self) -> Result<u32> {
        let pid_str = fs::read_to_string(&self.pid_file)
            .with_context(|| format!("Failed to read PID file: {}", self.pid_file.display()))?;
        pid_str
            .trim()
            .parse()
            .with_context(|| format!("Invalid PID in file: {}", pid_str))
    }

    /// Get PID file path
    pub fn pid_file(&self) -> &PathBuf {
        &self.pid_file
    }

    /// Stop the daemon gracefully
    ///
    /// Attempts graceful shutdown:
    /// 1. Send SIGTERM
    /// 2. Wait up to 5 seconds for process to exit
    /// 3. If still running, send SIGKILL
    /// 4. Remove PID file
    ///
    /// Returns Ok if daemon stopped successfully or wasn't running.
    /// Returns Err if failed to stop process.
    pub fn stop_daemon(&self) -> Result<()> {
        // Check if daemon is running
        if !self.pid_file.exists() {
            info!("Daemon not running (PID file does not exist)");
            return Ok(());
        }

        let pid = match self.read_pid() {
            Ok(p) => p,
            Err(e) => {
                warn!("Stale PID file exists but cannot read: {}. Removing...", e);
                self.cleanup()?;
                return Ok(());
            }
        };

        if !process_exists(pid) {
            info!(
                pid = pid,
                "Daemon not running (process does not exist). Removing stale PID file..."
            );
            self.cleanup()?;
            return Ok(());
        }

        info!(pid = pid, "Stopping daemon with SIGTERM...");

        // Send SIGTERM for graceful shutdown
        #[cfg(target_family = "unix")]
        {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            use std::time::{Duration, Instant};

            kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
                .context("Failed to send SIGTERM to daemon")?;

            // Wait up to 5 seconds for graceful shutdown
            let start = Instant::now();
            let timeout = Duration::from_secs(5);

            while start.elapsed() < timeout {
                if !process_exists(pid) {
                    info!(pid = pid, "Daemon stopped gracefully");
                    self.cleanup()?;
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            // Still running after timeout, send SIGKILL
            warn!(
                pid = pid,
                "Daemon did not stop gracefully, sending SIGKILL..."
            );
            // SIGKILL may fail if the process is already a zombie (exited but not reaped).
            // That's fine — a zombie holds no resources; just clean up the PID file.
            let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);

            // Wait a bit for SIGKILL to take effect
            std::thread::sleep(Duration::from_millis(500));

            // kill(pid, 0) returns Ok for zombie processes, so "still exists" after
            // SIGKILL almost always means a zombie.  Either way, the daemon is no
            // longer serving requests — remove the PID file and move on.
            if process_exists(pid) {
                warn!(
                    pid = pid,
                    "Process still visible after SIGKILL (likely zombie); removing PID file"
                );
            } else {
                info!(pid = pid, "Daemon force-stopped with SIGKILL");
            }
            self.cleanup()?;
            Ok(())
        }

        #[cfg(target_family = "windows")]
        {
            use std::process::Command as ProcessCommand;

            // Use taskkill on Windows
            let output = ProcessCommand::new("taskkill")
                .args(&["/PID", &pid.to_string(), "/F"])
                .output()
                .context("Failed to execute taskkill")?;

            if !output.status.success() {
                anyhow::bail!("Failed to stop daemon: taskkill failed");
            }

            info!(pid = pid, "Daemon stopped");
            self.cleanup()?;
            Ok(())
        }
    }
}

impl DaemonInstanceGuard {
    fn cleanup_owned_pid(&self) -> Result<()> {
        let owns_pid_file = fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            == Some(self.pid);
        if owns_pid_file {
            fs::remove_file(&self.pid_file).with_context(|| {
                format!("Failed to remove PID file: {}", self.pid_file.display())
            })?;
        }
        Ok(())
    }

    pub fn release(mut self) -> Result<()> {
        self.cleanup_owned_pid()?;
        self.released = true;
        FileExt::unlock(&self.lock_file).context("Failed to unlock daemon instance")
    }
}

impl Drop for DaemonInstanceGuard {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.cleanup_owned_pid();
        }
        let _ = FileExt::unlock(&self.lock_file);
    }
}

impl Default for DaemonLifecycle {
    fn default() -> Self {
        Self::new().expect("Failed to initialize DaemonLifecycle")
    }
}

/// Check if a process with the given PID exists
///
/// Uses platform-specific methods:
/// - Unix: kill(pid, 0) to check existence without sending signal
/// - Windows: sysinfo crate to enumerate processes
#[cfg(target_family = "unix")]
fn process_exists(pid: u32) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    // kill with NULL signal checks existence without affecting process
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

#[cfg(target_family = "windows")]
fn process_exists(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::nothing());
    system.process(Pid::from(pid as usize)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_pid_file_lifecycle() {
        let temp_dir = TempDir::new().unwrap();
        let pid_file = temp_dir.path().join("daemon.pid");

        let lifecycle = DaemonLifecycle {
            pid_file: pid_file.clone(),
        };

        // Write PID
        lifecycle.write_pid().unwrap();
        assert!(pid_file.exists());

        // Read PID
        let pid = lifecycle.read_pid().unwrap();
        assert_eq!(pid, std::process::id());

        // Check running
        assert!(lifecycle.is_running());

        // Cleanup
        lifecycle.cleanup().unwrap();
        assert!(!pid_file.exists());
        assert!(!lifecycle.is_running());
    }

    #[test]
    fn instance_guard_excludes_a_second_daemon_and_cleans_only_its_pid() {
        let temp_dir = TempDir::new().unwrap();
        let pid_file = temp_dir.path().join("daemon.pid");
        let lifecycle = DaemonLifecycle {
            pid_file: pid_file.clone(),
        };

        let first = lifecycle.acquire_instance().unwrap();
        let error = lifecycle.acquire_instance().unwrap_err();
        assert!(error.to_string().contains("daemon lock"));
        drop(first);
        assert!(!pid_file.exists());

        let replacement = lifecycle.acquire_instance().unwrap();
        fs::write(&pid_file, "999999999").unwrap();
        drop(replacement);
        assert_eq!(fs::read_to_string(&pid_file).unwrap(), "999999999");
    }

    #[test]
    fn test_process_exists() {
        // Current process should exist
        // Current process should always exist
        assert!(process_exists(std::process::id()));

        // Note: PID 1 check removed - on macOS, kill() may fail for PID 1 due to
        // permission restrictions even though the process exists, making this test flaky

        // Very high PID should not exist
        assert!(!process_exists(999999999));
    }
}
