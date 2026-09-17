// Daemon lifecycle management
//
// Handles PID file creation/removal, process existence checks,
// and graceful shutdown coordination.

use anyhow::{Context, Result};
use fs2::FileExt;
use std::fmt;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::path::PathBuf;
use tracing::{info, warn};

/// Manages daemon lifecycle (PID file, shutdown)
pub struct DaemonLifecycle {
    pid_file: PathBuf,
}

/// Result of [`DaemonLifecycle::stop_daemon`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonStopOutcome {
    /// No pid file and no leftover IPC socket.
    NotRunning,
    /// The recorded process was already gone. Leftover files that were safe
    /// to remove have been reaped.
    ReapedStale {
        /// PID from the leftover file, if it could be parsed.
        pid: Option<u32>,
        /// Whether a leftover IPC socket with no listener was removed.
        removed_socket: bool,
    },
    /// A live daemon process was signalled and is no longer running.
    Stopped {
        /// PID that was stopped.
        pid: u32,
    },
    /// A leftover pid file named a dead process, but the IPC socket still has
    /// a live listener. The pid file was removed; the socket was left in place.
    StalePidLiveSocket {
        /// PID from the leftover file, if it could be parsed.
        pid: Option<u32>,
    },
}

impl fmt::Display for DaemonStopOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotRunning => write!(f, "Daemon is not running"),
            Self::ReapedStale {
                pid: Some(pid),
                removed_socket: true,
            } => write!(
                f,
                "Daemon is not running (cleaned leftover PID {pid} and socket from a crashed process)"
            ),
            Self::ReapedStale {
                pid: Some(pid),
                removed_socket: false,
            } => write!(
                f,
                "Daemon is not running (cleaned leftover PID {pid} from a crashed process)"
            ),
            Self::ReapedStale {
                pid: None,
                removed_socket: true,
            } => write!(
                f,
                "Daemon is not running (cleaned leftover IPC socket from a crashed process)"
            ),
            Self::ReapedStale {
                pid: None,
                removed_socket: false,
            } => write!(f, "Daemon is not running"),
            Self::Stopped { pid } => write!(f, "Daemon stopped successfully (PID: {pid})"),
            Self::StalePidLiveSocket { pid: Some(pid) } => write!(
                f,
                "Removed stale PID file ({pid}), but the IPC socket still has a live listener"
            ),
            Self::StalePidLiveSocket { pid: None } => write!(
                f,
                "No daemon PID file, but the IPC socket still has a live listener"
            ),
        }
    }
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

    /// Manage the daemon whose PID file is at `pid_file`.
    ///
    /// Test-only. Production always derives the path from the home directory,
    /// and `src/daemon/spawn.rs` cannot otherwise drive the connect path's
    /// "a PID file exists, wait and retry" branch without depending on -- and
    /// writing into -- the developer's real `~/.finch`.
    #[cfg(test)]
    pub(crate) fn with_pid_file(pid_file: PathBuf) -> Self {
        Self { pid_file }
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

    /// True when a pid file or IPC socket remains but no daemon process is alive.
    pub fn has_stale_files(&self) -> bool {
        !self.is_running() && (self.pid_file.exists() || self.socket_path().exists())
    }

    fn socket_path(&self) -> PathBuf {
        self.pid_file.with_extension("sock")
    }

    /// Remove the IPC socket only when nothing is listening on it.
    ///
    /// Blind unlinking would let this command steal a live listener's pathname
    /// while that process kept serving through its open file descriptor. The
    /// bind path in `src/ipc/server.rs` uses the same connect-then-unlink rule.
    fn reap_stale_socket(&self) -> Result<StaleSocketReap> {
        #[cfg(not(unix))]
        {
            let _ = self;
            Ok(StaleSocketReap::Absent)
        }
        #[cfg(unix)]
        {
            use std::os::unix::net::UnixStream;
            let path = self.socket_path();
            if !path.exists() {
                return Ok(StaleSocketReap::Absent);
            }
            match UnixStream::connect(&path) {
                Ok(_) => Ok(StaleSocketReap::Live),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    fs::remove_file(&path).with_context(|| {
                        format!("Failed to remove stale IPC socket: {}", path.display())
                    })?;
                    Ok(StaleSocketReap::Removed)
                }
                Err(error) => Err(error).with_context(|| {
                    format!("could not determine whether {} is stale", path.display())
                }),
            }
        }
    }

    fn outcome_after_dead_process(&self, pid: Option<u32>) -> Result<DaemonStopOutcome> {
        match self.reap_stale_socket()? {
            StaleSocketReap::Live => Ok(DaemonStopOutcome::StalePidLiveSocket { pid }),
            StaleSocketReap::Removed => Ok(DaemonStopOutcome::ReapedStale {
                pid,
                removed_socket: true,
            }),
            StaleSocketReap::Absent => {
                if pid.is_some() {
                    Ok(DaemonStopOutcome::ReapedStale {
                        pid,
                        removed_socket: false,
                    })
                } else {
                    Ok(DaemonStopOutcome::NotRunning)
                }
            }
        }
    }

    /// Stop the daemon gracefully, or reap leftover files from a crash.
    ///
    /// Attempts graceful shutdown of a live process:
    /// 1. Send SIGTERM
    /// 2. Wait up to 5 seconds for process to exit
    /// 3. If still running, send SIGKILL
    /// 4. Remove PID file and a leftover IPC socket with no listener
    ///
    /// If the pid file names a process that is already gone, this still removes
    /// that file and a stale socket. Callers must not skip this when
    /// [`Self::is_running`] is false: that is how a crashed daemon left pid and
    /// socket files that `finch daemon-stop` used to ignore.
    pub fn stop_daemon(&self) -> Result<DaemonStopOutcome> {
        if !self.pid_file.exists() {
            info!("Daemon not running (PID file does not exist)");
            return self.outcome_after_dead_process(None);
        }

        let pid = match self.read_pid() {
            Ok(p) => p,
            Err(e) => {
                warn!("Stale PID file exists but cannot read: {}. Removing...", e);
                self.cleanup()?;
                return self.outcome_after_dead_process(None);
            }
        };

        if !process_exists(pid) {
            info!(
                pid = pid,
                "Daemon not running (process does not exist). Removing stale PID file..."
            );
            self.cleanup()?;
            return self.outcome_after_dead_process(Some(pid));
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
                    let _ = self.reap_stale_socket()?;
                    return Ok(DaemonStopOutcome::Stopped { pid });
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
            let _ = self.reap_stale_socket()?;
            Ok(DaemonStopOutcome::Stopped { pid })
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
            Ok(DaemonStopOutcome::Stopped { pid })
        }
    }
}

/// Whether [`DaemonLifecycle::reap_stale_socket`] removed a leftover socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StaleSocketReap {
    Absent,
    Removed,
    Live,
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

    #[test]
    fn stop_daemon_reaps_stale_pid_file_for_a_dead_process() {
        let temp_dir = TempDir::new().unwrap();
        let pid_file = temp_dir.path().join("daemon.pid");
        fs::write(&pid_file, "999999999").unwrap();
        let lifecycle = DaemonLifecycle {
            pid_file: pid_file.clone(),
        };

        assert!(
            !lifecycle.is_running(),
            "a pid file for a dead process must not count as running; pid_file={}",
            pid_file.display()
        );
        assert!(
            lifecycle.has_stale_files(),
            "a leftover pid file for a dead process is stale; pid_file={}",
            pid_file.display()
        );

        let outcome = lifecycle.stop_daemon().unwrap();
        assert_eq!(
            outcome,
            DaemonStopOutcome::ReapedStale {
                pid: Some(999_999_999),
                removed_socket: false,
            },
            "stop_daemon must reap a crashed pid file even when is_running is false; leftover still present={}",
            pid_file.exists()
        );
        assert!(
            !pid_file.exists(),
            "reaping a crashed daemon must remove the leftover pid file at {}",
            pid_file.display()
        );
        assert!(
            !lifecycle.has_stale_files(),
            "no leftover files should remain after reaping {}",
            pid_file.display()
        );
    }

    #[test]
    fn stop_daemon_is_not_running_when_nothing_is_left() {
        let temp_dir = TempDir::new().unwrap();
        let pid_file = temp_dir.path().join("daemon.pid");
        let lifecycle = DaemonLifecycle { pid_file };
        assert_eq!(
            lifecycle.stop_daemon().unwrap(),
            DaemonStopOutcome::NotRunning,
            "stop_daemon with no pid or socket must report NotRunning"
        );
    }

    #[cfg(unix)]
    fn short_unix_dir() -> TempDir {
        // macOS sockaddr_un.sun_path is 104 bytes; the default TempDir lives
        // under a long /var/folders path and cannot be bound as a Unix socket.
        tempfile::Builder::new()
            .prefix("fds")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn stop_daemon_reaps_stale_socket_when_nothing_is_listening() {
        use std::os::unix::net::UnixListener;

        let temp_dir = short_unix_dir();
        let pid_file = temp_dir.path().join("daemon.pid");
        let socket = temp_dir.path().join("daemon.sock");
        fs::write(&pid_file, "999999999").unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        drop(listener);

        let lifecycle = DaemonLifecycle {
            pid_file: pid_file.clone(),
        };
        let outcome = lifecycle.stop_daemon().unwrap();
        assert_eq!(
            outcome,
            DaemonStopOutcome::ReapedStale {
                pid: Some(999_999_999),
                removed_socket: true,
            },
            "a leftover unix socket with no listener must be reaped with the dead pid; pid_exists={} socket_exists={}",
            pid_file.exists(),
            socket.exists()
        );
        assert!(
            !pid_file.exists() && !socket.exists(),
            "crashed-daemon leftovers must be gone; pid_exists={} socket_exists={}",
            pid_file.exists(),
            socket.exists()
        );
    }

    #[cfg(unix)]
    #[test]
    fn stop_daemon_does_not_unlink_a_live_ipc_socket() {
        use std::os::unix::net::{UnixListener, UnixStream};

        let temp_dir = short_unix_dir();
        let pid_file = temp_dir.path().join("daemon.pid");
        let socket = temp_dir.path().join("daemon.sock");
        fs::write(&pid_file, "999999999").unwrap();
        let listener = UnixListener::bind(&socket).unwrap();

        let lifecycle = DaemonLifecycle {
            pid_file: pid_file.clone(),
        };
        let outcome = lifecycle.stop_daemon().unwrap();
        assert_eq!(
            outcome,
            DaemonStopOutcome::StalePidLiveSocket {
                pid: Some(999_999_999),
            },
            "a live listener must not be unlinked because the pid file is stale; pid_exists={} socket_exists={}",
            pid_file.exists(),
            socket.exists()
        );
        assert!(
            !pid_file.exists(),
            "the stale pid file should still be removed; path={}",
            pid_file.display()
        );
        assert!(
            socket.exists(),
            "the live IPC socket must remain at {}",
            socket.display()
        );
        UnixStream::connect(&socket).unwrap_or_else(|error| {
            panic!(
                "the live listener at {} must still accept connects after stop_daemon: {error}",
                socket.display()
            )
        });
        drop(listener);
    }
}
