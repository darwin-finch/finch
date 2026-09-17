# daemon — public interface

Generated from [`src/daemon/mod.rs`](mod.rs) by `scripts/generate_interfaces.py`; CI fails if it drifts. Edit the code, then regenerate.

- **Facade:** `src/daemon/mod.rs`
- **Capsule:** [`AGENTS.md`](AGENTS.md)

Everything below is what callers outside this module can reach. Implementation modules are private; their contents are deliberately absent.

## Types

```rust
/// Process-lifetime ownership of the daemon namespace.
pub struct DaemonInstanceGuard { … }
impl DaemonInstanceGuard {
    pub fn release(mut self) -> Result<()>;
}
/// Manages daemon lifecycle (PID file, shutdown)
pub struct DaemonLifecycle { … }
impl DaemonLifecycle {
    /// Acquire exclusive ownership before binding any daemon transport.
    pub fn acquire_instance(&self) -> Result<DaemonInstanceGuard>;
    /// Remove PID file (called on shutdown)
    pub fn cleanup(&self) -> Result<()>;
    /// True when crash leftovers remain that [`Self::stop_daemon`] would reap.
    pub fn has_stale_files(&self) -> bool;
    /// True when something is listening on the daemon IPC socket right now.
    pub fn ipc_listener_alive(&self) -> bool;
    /// Check if daemon is currently running  Returns true if: - PID file exists - PID can be parsed - Process with that PID exists
    pub fn is_running(&self) -> bool;
    /// Create a new daemon lifecycle manager
    pub fn new() -> Result<Self>;
    /// Get PID file path
    pub fn pid_file(&self) -> &PathBuf;
    /// Read PID from file
    pub fn read_pid(&self) -> Result<u32>;
    /// Stop the daemon gracefully, or reap leftover files from a crash.
    pub fn stop_daemon(&self) -> Result<DaemonStopOutcome>;
    /// Write current process PID to file
    pub fn write_pid(&self) -> Result<()>;
}
/// Result of [`DaemonLifecycle::stop_daemon`].
pub enum DaemonStopOutcome { NotRunning, ReapedStale, Stopped, StalePidLiveSocket }
pub struct DaemonUpgradePlan { … }
impl DaemonUpgradePlan {
    /// Boot the staged candidate as a complete daemon on isolated HTTP and IPC endpoints.
    pub async fn preflight(self) -> Result<VerifiedDaemonUpgrade>;
    /// Preflight against an explicit Brain root, primarily for embedders and hermetic conformance tests.
    pub async fn preflight_against(self, brain_root: Option<&Path>) -> Result<VerifiedDaemonUpgrade>;
    /// Hash, execute-preflight, and stage explicit candidate and incumbent binaries in content-addressed locations before any process handoff.
    pub fn prepare(candidate: &Path, incumbent: &Path, schema_impact: &str) -> Result<Self>;
    /// Variant for an embedder-owned content-addressed artifact store.
    pub fn prepare_with_stage_root(candidate: &Path, incumbent: &Path, schema_impact: &str, stage_root: &Path) -> Result<Self>;
}
/// Point-in-time accounting for the daemon log, for status and setup surfaces.
pub struct LogStatus { … }
impl LogStatus {
    /// One-line, secret-free summary suitable for status and setup surfaces.
    pub fn summary(&self) -> String;
    /// Total bytes currently occupied by the active file and its generations.
    pub fn total_bytes(&self) -> u64;
}
/// An append-only log writer that rotates within a bounded disk budget.
pub struct RotatingLog { … }
impl RotatingLog {
    /// Take ownership of this process's stdout and stderr.
    pub fn bind_process_stdio(&self) -> Result<()>;
    /// Open (or create) the daemon log under `policy`.
    pub fn open(path: &Path, policy: RotationPolicy) -> Result<Self>;
    /// Absolute path of the active log file.
    pub fn path(&self) -> &Path;
    /// Policy currently in force.
    pub fn policy(&self) -> RotationPolicy;
    /// Current on-disk accounting for the active file and its generations.
    pub fn status(&self) -> LogStatus;
}
/// Bounded retention policy for the daemon log.
pub struct RotationPolicy { … }
impl RotationPolicy {
    /// Read the policy from the environment, falling back to bounded defaults.
    pub fn from_env() -> Self;
    /// Worst-case disk budget: the active file plus every retained generation.
    pub fn retention_ceiling_bytes(&self) -> u64;
}
/// A live candidate daemon proven in an isolated namespace.
pub struct VerifiedDaemonUpgrade { … }
impl VerifiedDaemonUpgrade {
    pub fn plan(&self) -> &DaemonUpgradePlan;
}
```

## Functions

```rust
/// The canonical daemon log path, `~/.finch/daemon.log`.
pub fn daemon_log_path() -> Result<PathBuf> { … }
/// Ensure daemon is running, spawning if necessary  This function: 1.
pub async fn ensure_daemon_running(bind_address: Option<&str>) -> Result<()> { … }
/// On-disk accounting for `path` under `policy`, usable without an open handle.
pub fn log_status(path: &Path, policy: RotationPolicy) -> LogStatus { … }
/// Spawn daemon as background process  Detaches daemon from current process and redirects logs to ~/.finch/daemon.log - Unix: Standard spawn with log file redir…
pub fn spawn_daemon(bind_address: &str) -> Result<()> { … }
```

## Constants

```rust
/// Set by `spawn_daemon` on the detached child.
pub const DETACHED_DAEMON_ENV: &str = "FINCH_DAEMON_DETACHED";
```
