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

use crate::config::DEFAULT_DAEMON_ADDR as DEFAULT_BIND;

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
    ensure_daemon_access_allowed()?;
    ensure_daemon_running_after_isolation_gate(bind_address).await
}

fn ensure_daemon_access_allowed() -> Result<()> {
    let supervisor_marker = std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Ok("1")
        || std::env::var_os("FINCH_BRAIN_TEST_PROOF_FD").is_some()
        || std::env::var_os("FINCH_BRAIN_TEST_PROOF_BACKUP_FD").is_some();
    let no_auto_spawn = std::env::var("FINCH_BRAIN_TEST_NO_AUTO_SPAWN").as_deref() == Ok("1");
    if !supervisor_marker && !no_auto_spawn {
        return Ok(());
    }
    if supervisor_marker {
        crate::brain::isolated_test_proof()
            .context("invalid Brain test supervisor authority at daemon lifecycle gate")?;
    }
    anyhow::bail!(
        "daemon discovery, reuse, and auto-spawn are disabled by the Brain test supervisor"
    );
}

/// Client timeout on one `GET /health` probe.
pub(crate) const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_millis(500);

async fn ensure_daemon_running_after_isolation_gate(bind_address: Option<&str>) -> Result<()> {
    connect_or_spawn(bind_address.unwrap_or(DEFAULT_BIND), DaemonLifecycle::new).await
}

/// The connect path proper: probe, then retry behind a PID file, then spawn.
///
/// `lifecycle` is a constructor rather than a `DaemonLifecycle` so the healthy
/// path still does not create `~/.finch/daemon.pid`'s parent directory, and so
/// the retry branch below can be driven from a test against a synthetic PID
/// file instead of the developer's real one. #364, "Instrument and reduce
/// Finch interactive TUI time-to-ready", is about what this function's phases
/// say happened, so a test has to be able to run *this function*.
async fn connect_or_spawn<F>(bind: &str, lifecycle: F) -> Result<()>
where
    F: FnOnce() -> Result<DaemonLifecycle>,
{
    let base_url = format!("http://{}", bind);

    // Quick health check first. HTTP 200 is not compatibility: a leftover
    // daemon from another protocol generation must not be reused.
    match probe_daemon_health(&base_url).await {
        HealthProbe::Compatible => {
            debug!("Daemon already running and healthy");
            return Ok(());
        }
        HealthProbe::Incompatible(mismatch) => {
            return Err(mismatch.into_error());
        }
        HealthProbe::Unhealthy | HealthProbe::Unreachable => {}
    }

    // Check PID file
    let lifecycle = lifecycle()?;
    if lifecycle.is_running() {
        // Daemon process exists but not responding yet
        // Wait a bit and retry (it might be starting up)
        info!("Daemon process exists, waiting for health check...");
        retry_backoff().await;

        match probe_daemon_health(&base_url).await {
            HealthProbe::Compatible => {
                info!("Daemon now healthy");
                return Ok(());
            }
            HealthProbe::Incompatible(mismatch) => {
                return Err(mismatch.into_error());
            }
            HealthProbe::Unhealthy | HealthProbe::Unreachable => {}
        }

        warn!("Daemon process exists but not responding to health checks");
        let pid = lifecycle.read_pid()?;
        bail!(errors::wrap_error_with_suggestion(
            format!(
                "Daemon is running (PID: {}) but not responding to health checks",
                pid
            ),
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

        match probe_daemon_health(&base_url).await {
            HealthProbe::Compatible => {
                info!("Daemon started successfully");
                return Ok(());
            }
            HealthProbe::Incompatible(mismatch) => {
                return Err(mismatch.into_error());
            }
            HealthProbe::Unhealthy | HealthProbe::Unreachable => {}
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

/// Owner-only mode for the frontend-created daemon log.
///
/// `~/.finch/daemon.log` can contain provider errors, Brain names, paths, and
/// diagnostics, so the frontend must not create or leave it world-readable.
#[cfg(unix)]
const FRONTEND_LOG_MODE: u32 = 0o600;

/// Open the daemon log the frontend hands the child as stdout/stderr.
///
/// The daemon owns rotation and retention for its own log. The frontend only
/// guarantees the directory exists and that the path is a regular file, so
/// early child output has somewhere safe to land before the daemon binds its
/// own descriptors. A fresh log is created owner-only; a log left
/// world-readable by an older build is repaired in place, because the child
/// cannot do that if it fails to start.
fn open_frontend_log(log_path: &std::path::Path) -> Result<std::fs::File> {
    let log_file = open_frontend_log_append(log_path)?;
    #[cfg(unix)]
    repair_frontend_log_permissions(&log_file, log_path);
    Ok(log_file)
}

/// Create or append-open the frontend log without repairing an existing mode.
///
/// `OpenOptions::mode` applies only on create. Callers that must observe that
/// fact (and `open_frontend_log`, which then repairs) share this open so a
/// mutation of the create mode cannot hide behind a test-local copy.
fn open_frontend_log_append(log_path: &std::path::Path) -> Result<std::fs::File> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    crate::daemon::log::ensure_regular_file(log_path)?;

    let mut log_options = std::fs::OpenOptions::new();
    log_options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The daemon secures this file once it starts, but the frontend creates
        // it first. Without an explicit mode a fresh log is created at
        // 0o666 & ~umask — typically 0644 — and is world-readable for the
        // second or two before the daemon takes over.
        log_options.mode(FRONTEND_LOG_MODE);
        // `ensure_regular_file` dropped its handle, so this open is what the
        // child actually receives. Without O_NOFOLLOW a symlink planted in the
        // window between them redirects the daemon's stdout and stderr.
        log_options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    log_options
        .open(log_path)
        .with_context(|| format!("Failed to open daemon log file: {}", log_path.display()))
}

/// Repair a log left world-readable by an older build.
///
/// `OpenOptions::mode` does not change an existing file. The child cannot do
/// this repair if it fails to start, so the frontend must.
#[cfg(unix)]
fn repair_frontend_log_permissions(log_file: &std::fs::File, log_path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = log_file.metadata() {
        let mut perms = metadata.permissions();
        if perms.mode() & 0o777 != FRONTEND_LOG_MODE {
            perms.set_mode(FRONTEND_LOG_MODE);
            if let Err(error) = log_file.set_permissions(perms) {
                warn!(path = %log_path.display(), %error, "Could not secure the daemon log");
            }
        }
    }
}

/// Spawn daemon as background process
///
/// Detaches daemon from current process and redirects logs to ~/.finch/daemon.log
/// - Unix: Standard spawn with log file redirection
/// - Windows: Uses CREATE_NO_WINDOW flag to avoid console
pub fn spawn_daemon(bind_address: &str) -> Result<()> {
    ensure_daemon_access_allowed()?;
    let exe_path =
        std::env::current_exe().context("Failed to determine current executable path")?;

    let log_path = crate::daemon::daemon_log_path()?;
    let log_file = open_frontend_log(&log_path)?;

    info!(
        exe = %exe_path.display(),
        bind = bind_address,
        log = %log_path.display(),
        "Spawning daemon subprocess"
    );

    #[cfg(target_family = "unix")]
    {
        use std::os::unix::process::CommandExt;
        let mut command = Command::new(&exe_path);
        command
            .arg("daemon")
            .arg("--bind")
            .arg(bind_address)
            .stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file
                    .try_clone()
                    .context("Failed to clone log file handle")?,
            ))
            .stderr(Stdio::from(log_file));

        // Start a new session in the child. This supersedes a new process
        // group: it detaches from the controlling terminal as well, so the
        // daemon is independent of the shell's job control and zsh no longer
        // reports it as "[1] terminated" when the REPL exits.
        //
        // The call runs in the pre-exec window, where only async-signal-safe
        // functions are legal; the new-session call is one. It must stay in
        // the parent spawn path, which `ensure_daemon_access_allowed()` denies
        // under test isolation. A daemon that started its own session from
        // inside `run_daemon` would escape the test supervisor's process group
        // and could not be reaped. See scripts/test_brain_isolation.sh.
        //
        // Note: the escape-API allowlist in that script matches the bare
        // token, so the call below is the only place it may appear here.
        //
        // SAFETY: the closure calls one async-signal-safe libc function and
        // allocates nothing.
        unsafe {
            command.pre_exec(|| {
                if nix::libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        // Marks the child as the detached daemon. `run_daemon` binds its own
        // stdout/stderr only when this is set, so the documented foreground
        // modes (`finch daemon` in a terminal, `finch worker`, and the shipped
        // systemd unit) keep writing to the terminal or the journal.
        command.env(crate::daemon::DETACHED_DAEMON_ENV, "1");

        command
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
            .stdout(Stdio::from(
                log_file
                    .try_clone()
                    .context("Failed to clone log file handle")?,
            ))
            .stderr(Stdio::from(log_file))
            .spawn()
            .with_context(|| format!("Failed to spawn daemon: {}", exe_path.display()))?;
    }

    debug!(log = %log_path.display(), "Daemon subprocess spawned, logs at {}", log_path.display());
    Ok(())
}

/// The unconditional wait between a failed first probe and the one retry.
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_secs(2);

/// Wait `RETRY_BACKOFF`, recorded as its own startup phase.
///
/// Issue #364, "Instrument and reduce Finch interactive TUI time-to-ready",
/// requires the daemon connect to separate "probe latency from the
/// `sleep(2s)` fallback". Undivided, a 2.5 s launch reports
/// `daemon_http_connect ms=2503.1` and a maintainer cannot tell a slow daemon
/// -- fix the daemon -- from this flat constant, which is reached whenever a
/// live daemon pid simply missed one 500 ms probe window -- fix the probe
/// timeout, or the thing that made the daemon miss it. The two have different
/// fixes, so the report has to name which one happened.
async fn retry_backoff() {
    let _phase = crate::startup::phase(crate::startup::PHASE_DAEMON_RETRY_BACKOFF);
    tokio::time::sleep(RETRY_BACKOFF).await;
}

/// Check if daemon health endpoint responds
///
/// Recorded as [`crate::startup::PHASE_DAEMON_RETRY_BACKOFF`]'s sibling,
/// [`crate::startup::PHASE_DAEMON_HEALTH_PROBE`], nested inside
/// `daemon_http_connect`. Instrumented here rather than at the three call
/// sites so that every probe is timed -- the first one, the retry after the
/// back-off, and each poll of the spawn wait -- and so that the category
/// distinguishes "the daemon answered and said no" from "nothing answered
/// inside the 500 ms timeout", which are different failures with different
/// fixes and were previously indistinguishable in the report.
///
/// The timeline this writes into is process-global, and one caller is not on
/// the startup path: `upgrade::wait_for_shadow_health` polls here up to sixty
/// times while a shadow daemon comes up. That is left instrumented rather than
/// scoped to startup, deliberately. The cost is bounded (sixty records, once,
/// in a process that is performing an upgrade), and nothing reads those
/// records: [`crate::startup::ready`] is the only thing that renders a report
/// and `finch daemon-upgrade` never reaches it, so the entries are accumulated
/// and dropped with the process. The alternative -- a startup-only flag
/// threaded through this function -- would add a parameter whose only purpose
/// is to suppress records no one sees, and would let a future startup caller
/// pass the wrong value and lose the probe from the report silently. What is
/// true and worth stating plainly: the timeline is "phases this process
/// recorded", not "phases of startup", and a reader who dumps it mid-upgrade
/// will see upgrade probes in it.
/// Outcome of one `GET /health` probe, including leftover-daemon detection.
#[derive(Debug)]
pub(crate) enum HealthProbe {
    Compatible,
    Incompatible(LeftoverDaemon),
    Unhealthy,
    Unreachable,
}

/// A running daemon answered HTTP 200 but does not speak this protocol generation.
#[derive(Debug, Clone)]
pub(crate) struct LeftoverDaemon {
    pub frontend_generation: u32,
    pub daemon_generation: u32,
    pub uptime_seconds: u64,
}

impl LeftoverDaemon {
    fn into_error(self) -> anyhow::Error {
        record_rejected_leftover_handshake(&self);
        anyhow::Error::msg(crate::ipc::leftover_daemon_message(
            self.frontend_generation,
            self.daemon_generation,
            Some(self.uptime_seconds).filter(|seconds| *seconds > 0),
        ))
    }
}

/// Append the rejected leftover handshake to `daemon.log` so users do not have
/// to reconstruct it from a mute TUI. Best-effort: a missing log must not
/// hide the error returned to the caller.
fn record_rejected_leftover_handshake(mismatch: &LeftoverDaemon) {
    let Ok(path) = crate::daemon::daemon_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = format!(
        "leftover daemon handshake rejected: frontend protocol {}, daemon protocol {}, uptime {}s\n",
        mismatch.frontend_generation, mismatch.daemon_generation, mismatch.uptime_seconds
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = std::io::Write::write_all(&mut file, line.as_bytes());
    }
}

/// Check if daemon health endpoint responds with a compatible generation.
///
/// Recorded as [`crate::startup::PHASE_DAEMON_RETRY_BACKOFF`]'s sibling,
/// [`crate::startup::PHASE_DAEMON_HEALTH_PROBE`], nested inside
/// `daemon_http_connect`. Instrumented here rather than at the three call
/// sites so that every probe is timed -- the first one, the retry after the
/// back-off, and each poll of the spawn wait -- and so that the category
/// distinguishes "the daemon answered and said no" from "nothing answered
/// inside the 500 ms timeout", which are different failures with different
/// fixes and were previously indistinguishable in the report.
///
/// The timeline this writes into is process-global, and one caller is not on
/// the startup path: `upgrade::wait_for_shadow_health` polls here up to sixty
/// times while a shadow daemon comes up. That is left instrumented rather than
/// scoped to startup, deliberately. The cost is bounded (sixty records, once,
/// in a process that is performing an upgrade), and nothing reads those
/// records: [`crate::startup::ready`] is the only thing that renders a report
/// and `finch daemon-upgrade` never reaches it, so the entries are accumulated
/// and dropped with the process. The alternative -- a startup-only flag
/// threaded through this function -- would add a parameter whose only purpose
/// is to suppress records no one sees, and would let a future startup caller
/// pass the wrong value and lose the probe from the report silently. What is
/// true and worth stating plainly: the timeline is "phases this process
/// recorded", not "phases of startup", and a reader who dumps it mid-upgrade
/// will see upgrade probes in it.
pub(crate) async fn health_check_succeeds(base_url: &str) -> bool {
    matches!(probe_daemon_health(base_url).await, HealthProbe::Compatible)
}

pub(crate) async fn probe_daemon_health(base_url: &str) -> HealthProbe {
    let mut phase = crate::startup::phase(crate::startup::PHASE_DAEMON_HEALTH_PROBE);
    let client = reqwest::Client::builder()
        .timeout(HEALTH_PROBE_TIMEOUT)
        .build()
        .expect("Failed to build HTTP client");

    let url = format!("{}/health", base_url);

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            let body = response.text().await.unwrap_or_default();
            let parsed: serde_json::Value =
                serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
            let daemon_generation = crate::ipc::protocol_generation_from_health_json(&parsed);
            let uptime_seconds = crate::ipc::uptime_seconds_from_health_json(&parsed);
            if daemon_generation == crate::ipc::IPC_PROTOCOL_VERSION {
                debug!(url = %url, generation = daemon_generation, "Health check succeeded");
                phase.detail(crate::startup::PhaseDetail::category("healthy"));
                HealthProbe::Compatible
            } else {
                debug!(
                    url = %url,
                    frontend = crate::ipc::IPC_PROTOCOL_VERSION,
                    daemon = daemon_generation,
                    "Health check found a leftover daemon"
                );
                phase.detail(crate::startup::PhaseDetail::category("incompatible"));
                HealthProbe::Incompatible(LeftoverDaemon {
                    frontend_generation: crate::ipc::IPC_PROTOCOL_VERSION,
                    daemon_generation,
                    uptime_seconds,
                })
            }
        }
        Ok(response) => {
            debug!(url = %url, status = %response.status(), "Health check failed");
            phase.detail(crate::startup::PhaseDetail::category("unhealthy"));
            HealthProbe::Unhealthy
        }
        Err(e) => {
            debug!(url = %url, error = %e, "Health check request failed");
            phase.detail(crate::startup::PhaseDetail::category("unreachable"));
            HealthProbe::Unreachable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn test_ensure_regular_file_rejects_a_fifo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        let c_path = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: creating a FIFO at a path inside a fresh temporary directory.
        assert_eq!(unsafe { nix::libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        let error = crate::daemon::log::ensure_regular_file(&path)
            .expect_err("a FIFO must be refused before anything opens it");

        assert!(
            error.to_string().contains("not a regular file"),
            "the error must name the reason: {error}"
        );
        // Opening a FIFO with no reader blocks forever, so refusing it here is
        // what keeps the frontend from hanging with no diagnostic.
    }

    #[test]
    fn test_ensure_regular_file_rejects_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::create_dir(&path).unwrap();

        let error = crate::daemon::log::ensure_regular_file(&path)
            .expect_err("a directory must be refused");
        assert!(error.to_string().contains("not a regular file"));
    }

    #[test]
    fn test_ensure_regular_file_accepts_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, b"existing\n").unwrap();
        crate::daemon::log::ensure_regular_file(&path).unwrap();
    }

    /// Unix mode bits of `path`, excluding type bits.
    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
            .permissions()
            .mode()
            & 0o777
    }

    /// The frontend spawn path must create `~/.finch/daemon.log` owner-only.
    /// Without `OpenOptions::mode(0o600)` a fresh log is `0o666 & ~umask`
    /// (typically 0o644) and world-readable until the daemon starts. The
    /// repair that follows does not mask this: the test opens through the
    /// production create path and does not call the repair.
    #[test]
    #[cfg(unix)]
    fn test_frontend_open_creates_an_owner_only_log() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        assert!(
            !path.exists(),
            "precondition: OpenOptions::mode applies only when the file is created"
        );

        open_frontend_log_append(&path).unwrap_or_else(|error| {
            panic!("frontend spawn must be able to create the daemon log: {error:#}")
        });

        let mode = unix_mode(&path);
        assert_eq!(
            mode, 0o600,
            "frontend spawn must create the daemon log owner-only (mode 0o600); \
             without OpenOptions::mode a fresh log is 0o666 & ~umask (typically 0o644) and \
             world-readable. The log can contain provider errors, Brain names, paths, and \
             diagnostics. observed mode={mode:#o}"
        );
    }

    /// The frontend spawn path must repair a log left world-readable by an
    /// older build. `OpenOptions::mode` does not change an existing file, and
    /// the child cannot fchmod if it fails to start, so `spawn_daemon` has to
    /// do it. This calls `open_frontend_log` — the same function
    /// `spawn_daemon` uses — rather than reproducing the chmod in the test.
    #[test]
    #[cfg(unix)]
    fn test_frontend_open_repairs_a_world_readable_log() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, b"inherited\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let planted = unix_mode(&path);
        assert_eq!(
            planted, 0o644,
            "precondition: planted a world-readable daemon log so the production \
             repair has something to fix. observed mode={planted:#o}"
        );

        // Platform premise the repair exists for: OpenOptions::mode does not
        // change an existing file. If this failed, the fchmod would be dead
        // and deleting it would still leave the log at 0o600.
        open_frontend_log_append(&path).unwrap_or_else(|error| {
            panic!("frontend spawn must be able to open the inherited daemon log: {error:#}")
        });
        let after_open = unix_mode(&path);
        assert_eq!(
            after_open, 0o644,
            "precondition: OpenOptions::mode applies only on create, so an inherited \
             0o644 daemon.log stays world-readable until the fchmod repair. \
             observed mode={after_open:#o}"
        );

        open_frontend_log(&path).unwrap_or_else(|error| {
            panic!("frontend spawn must be able to repair the inherited daemon log: {error:#}")
        });

        let mode = unix_mode(&path);
        assert_eq!(
            mode, 0o600,
            "frontend spawn must repair a world-readable daemon log to owner-only \
             (mode 0o600); OpenOptions::mode does not change an existing file, and \
             the child cannot do this if it fails to start. The log can contain \
             provider errors, Brain names, paths, and diagnostics. \
             observed mode={mode:#o}"
        );
    }

    #[tokio::test]
    async fn test_health_check_fails_for_invalid_url() {
        let _serialised = timeline_lock().await;
        // Non-existent server should fail health check
        let result = health_check_succeeds("http://127.0.0.1:99999").await;
        assert!(!result);
    }

    /// Serialise the tests that read the process-global startup timeline.
    ///
    /// The timeline is one `Mutex<Timeline>` for the process and libtest runs
    /// these on parallel threads, so a test that asserts "no fallback phase
    /// was recorded while I probed" needs the test that records one not to be
    /// running. Without this the two below fail each other roughly at random,
    /// which is worse than either being absent.
    /// A tokio mutex, not a `std` one: these are async tests and the guard is
    /// held across `.await`, which a blocking guard must never be.
    async fn timeline_lock() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        LOCK.lock().await
    }

    /// Categories of the `daemon_health_probe` phases recorded so far, in
    /// order. Only the probe emits that name; `timeline_lock` keeps the other
    /// timeline test from interleaving.
    fn probe_categories() -> Vec<Option<&'static str>> {
        crate::startup::recorded()
            .iter()
            .filter(|record| record.name == crate::startup::PHASE_DAEMON_HEALTH_PROBE)
            .map(|record| record.detail.category)
            .collect()
    }

    fn backoff_count() -> usize {
        crate::startup::recorded()
            .iter()
            .filter(|record| record.name == crate::startup::PHASE_DAEMON_RETRY_BACKOFF)
            .count()
    }

    /// The daemon connect's sub-phases recorded so far, in the order they
    /// were recorded. `PhaseGuard` pushes on drop, so this is the order the
    /// connect path actually closed them in.
    fn daemon_phase_names() -> Vec<&'static str> {
        crate::startup::recorded()
            .iter()
            .map(|record| record.name)
            .filter(|name| {
                *name == crate::startup::PHASE_DAEMON_HEALTH_PROBE
                    || *name == crate::startup::PHASE_DAEMON_RETRY_BACKOFF
            })
            .collect()
    }

    fn compatible_health_body() -> String {
        serde_json::json!({
            "status": "healthy",
            "uptime_seconds": 1,
            "named_brains": 0,
            "pending_brain_terminalizations": 0,
            "protocol_generation": crate::ipc::IPC_PROTOCOL_VERSION,
            "package_identity": crate::ipc::package_identity(),
        })
        .to_string()
    }

    fn leftover_health_body(generation: u32, uptime_seconds: u64) -> String {
        serde_json::json!({
            "status": "healthy",
            "uptime_seconds": uptime_seconds,
            "named_brains": 0,
            "pending_brain_terminalizations": 0,
            "protocol_generation": generation,
        })
        .to_string()
    }

    /// Answer one request with `status` and `body`, then close. Returns the base URL.
    async fn one_shot_health_endpoint(
        status: &'static str,
        body: String,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a kernel-assigned loopback port");
        let address = listener.local_addr().expect("bound address");
        let server = tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut scratch = [0u8; 1024];
            let _ = stream.read(&mut scratch).await;
            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.flush().await;
        });
        (format!("http://{address}"), server)
    }

    /// #364, "Instrument and reduce Finch interactive TUI time-to-ready",
    /// enumerates "daemon HTTP connect, separating probe latency from the
    /// `sleep(2s)` fallback". Undivided, both live inside one
    /// `daemon_http_connect` phase and a 2.5 s launch reports
    /// `daemon_http_connect ms=2503.1 SLOW category=unavailable` -- which does
    /// not tell a maintainer whether the daemon was slow or whether a healthy
    /// daemon merely missed one 500 ms probe window and cost this launch the
    /// flat two-second floor. Those need different fixes.
    ///
    /// Structural, with no duration asserted: what is checked is that a probe
    /// records a probe phase and no fallback phase, that the phase says how
    /// the probe ended, and that the fallback records a phase of its own.
    #[tokio::test]
    async fn test_the_health_probe_records_its_own_phase_and_says_how_it_ended() {
        let _serialised = timeline_lock().await;
        let before_backoffs = backoff_count();

        let (healthy_url, healthy) =
            one_shot_health_endpoint("200 OK", compatible_health_body()).await;
        assert!(
            health_check_succeeds(&healthy_url).await,
            "a 200 from /health is healthy only when protocol_generation matches this Finch"
        );
        let _ = healthy.await;

        let (unhealthy_url, unhealthy) =
            one_shot_health_endpoint("500 Internal Server Error", String::new()).await;
        assert!(
            !health_check_succeeds(&unhealthy_url).await,
            "a 500 from /health is not a healthy daemon"
        );
        let _ = unhealthy.await;

        // A port nothing is listening on: the probe gets no reply at all,
        // which is a different failure from a daemon answering 500.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a kernel-assigned loopback port");
        let closed_address = closed.local_addr().expect("bound address");
        drop(closed);
        assert!(!health_check_succeeds(&format!("http://{closed_address}")).await);

        let categories = probe_categories();
        let tail: Vec<Option<&'static str>> =
            categories.iter().rev().take(3).rev().copied().collect();
        assert_eq!(
            tail,
            vec![Some("healthy"), Some("unhealthy"), Some("unreachable")],
            "each `GET /health` probe must record a `{}` phase naming how it \
             ended, so a report can distinguish a daemon that answered \
             negatively from one that did not answer inside the {} ms \
             timeout. Recorded probe categories were {categories:?}",
            crate::startup::PHASE_DAEMON_HEALTH_PROBE,
            HEALTH_PROBE_TIMEOUT.as_millis(),
        );
        assert_eq!(
            backoff_count(),
            before_backoffs,
            "probing must not record a `{}` phase: the fallback wait is a \
             separate span and folding the two together is the conflation \
             #364 asks this split to end. Probe categories were {categories:?}",
            crate::startup::PHASE_DAEMON_RETRY_BACKOFF,
        );
    }

    /// The fallback wait is its own phase, so a report can show that the two
    /// seconds were a constant rather than a slow daemon.
    ///
    /// The tokio clock is paused, so the wait itself is free; nothing here
    /// asserts how long anything took.
    #[tokio::test(start_paused = true)]
    async fn test_the_retry_fallback_is_recorded_as_a_phase_of_its_own() {
        let _serialised = timeline_lock().await;
        let before_backoffs = backoff_count();
        let before_probes = probe_categories().len();

        retry_backoff().await;

        assert_eq!(
            backoff_count(),
            before_backoffs + 1,
            "the unconditional {:?} wait between the failed first probe and \
             the retry must record a `{}` phase. Without it the wait is \
             absorbed into `daemon_http_connect` and a maintainer reading \
             `ms=2503.1` cannot tell a slow daemon from this flat floor.",
            RETRY_BACKOFF,
            crate::startup::PHASE_DAEMON_RETRY_BACKOFF,
        );
        assert_eq!(
            probe_categories().len(),
            before_probes,
            "waiting is not probing: the fallback must not record a `{}` \
             phase, or the report double-counts probes that never happened.",
            crate::startup::PHASE_DAEMON_HEALTH_PROBE,
        );
    }

    /// The connect path itself, driven end to end.
    ///
    /// `test_the_retry_fallback_is_recorded_as_a_phase_of_its_own` calls
    /// `retry_backoff()` directly, so it proves the helper records a phase and
    /// nothing else. It does not prove the connect path calls the helper:
    /// replacing the call in `connect_or_spawn` with a bare
    /// `tokio::time::sleep(RETRY_BACKOFF).await` -- the pre-#364 shape, with
    /// the flat two-second floor absorbed back into `daemon_http_connect` --
    /// left the whole suite green. This test runs the real function, so the
    /// phases it asserts on are recorded as a consequence of the code under
    /// test rather than of the test.
    ///
    /// The fixture is the retry branch's exact precondition: a PID file naming
    /// a process that is alive (this one), and an address nothing is listening
    /// on. So the path probes, finds the PID file, waits, probes again, and
    /// fails with the "running but not responding" diagnostic. Nothing is
    /// spawned -- the spawn branch is only reached when no PID file is live --
    /// and the PID file is inside a `tempfile` directory, so the developer's
    /// `~/.finch` is neither read nor written.
    ///
    /// Structural, with no duration asserted; the clock is paused so the two
    /// second wait costs nothing.
    #[tokio::test(start_paused = true)]
    async fn test_the_connect_path_records_the_backoff_between_its_two_probes() {
        let _serialised = timeline_lock().await;

        let home = tempfile::tempdir().expect("a disposable directory for the PID file");
        let pid_file = home.path().join("daemon.pid");
        std::fs::write(&pid_file, std::process::id().to_string()).expect("write the PID file");

        // A port the kernel just handed out and nothing is bound to: the probe
        // gets no answer, which is the "daemon exists but is not responding"
        // case this branch is for.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a kernel-assigned loopback port");
        let dead_address = closed.local_addr().expect("bound address").to_string();
        drop(closed);

        let before = daemon_phase_names().len();

        let error = connect_or_spawn(&dead_address, || {
            Ok(DaemonLifecycle::with_pid_file(pid_file.clone()))
        })
        .await
        .expect_err("nothing is listening on the address, so the connect must fail");

        let recorded: Vec<&'static str> = daemon_phase_names().split_off(before);
        assert_eq!(
            recorded,
            vec![
                crate::startup::PHASE_DAEMON_HEALTH_PROBE,
                crate::startup::PHASE_DAEMON_RETRY_BACKOFF,
                crate::startup::PHASE_DAEMON_HEALTH_PROBE,
            ],
            "the connect path must record its {:?} fallback wait as a phase of \
             its own, between the probe that failed and the retry it is \
             waiting for. Absorbed back into `daemon_http_connect`, a 2.5 s \
             launch reports one undivided `ms=2503.1` and a maintainer cannot \
             tell a slow daemon from this flat floor -- the split #364 \
             requires. Connect failed with: {error:#}",
            RETRY_BACKOFF,
        );
        assert!(
            error
                .to_string()
                .contains("not responding to health checks"),
            "the retry branch must be the branch that ran, or the phases above \
             came from somewhere else. Error was: {error:#}"
        );
        assert!(
            error.to_string().contains(&std::process::id().to_string()),
            "the diagnostic must name the PID it found in the PID file, which \
             is this test's own. Error was: {error:#}"
        );
    }

    /// HTTP 200 from a leftover daemon is not compatibility. Older daemons omit
    /// `protocol_generation` (read as 0) and must fail closed with the kick
    /// command rather than being reused as a mute driver.
    #[tokio::test]
    async fn leftover_daemon_http_200_is_not_protocol_compatibility() {
        let _serialised = timeline_lock().await;
        let home = tempfile::tempdir().expect("disposable HOME for leftover daemon.log");
        struct RestoreHome(Option<std::ffi::OsString>);
        impl Drop for RestoreHome {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("HOME", value),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
        let _restore_home = RestoreHome(std::env::var_os("HOME"));
        std::env::set_var("HOME", home.path());

        let (legacy_url, legacy) = one_shot_health_endpoint(
            "200 OK",
            serde_json::json!({
                "status": "healthy",
                "uptime_seconds": 7200,
                "named_brains": 1
            })
            .to_string(),
        )
        .await;
        let probe = probe_daemon_health(&legacy_url).await;
        let _ = legacy.await;
        match &probe {
            HealthProbe::Incompatible(mismatch) => {
                assert_eq!(
                    mismatch.daemon_generation, 0,
                    "omitted protocol_generation must fail closed at 0; mismatch={mismatch:?}"
                );
                assert_eq!(mismatch.uptime_seconds, 7200);
                assert_eq!(
                    mismatch.frontend_generation,
                    crate::ipc::IPC_PROTOCOL_VERSION
                );
            }
            other => panic!("HTTP 200 without protocol_generation must be leftover, not {other:?}"),
        }
        assert!(
            !matches!(probe, HealthProbe::Compatible),
            "health_check_succeeds must not treat a leftover HTTP 200 as a reusable daemon; probe={probe:?}"
        );

        let (old_url, old) =
            one_shot_health_endpoint("200 OK", leftover_health_body(8, 7200)).await;
        let error = connect_or_spawn(old_url.trim_start_matches("http://"), || {
            anyhow::bail!("leftover daemon must not reach PID-file or spawn construction")
        })
        .await
        .expect_err("a leftover daemon must not be reused");
        let _ = old.await;
        let message = error.to_string();
        assert!(
            message.contains("speaks 8"),
            "leftover error must name the running daemon generation; error={message}"
        );
        assert!(
            message.contains(&format!("protocol {}", crate::ipc::IPC_PROTOCOL_VERSION)),
            "leftover error must name this Finch generation; error={message}"
        );
        assert!(
            message.contains("up for 2h"),
            "leftover error must include uptime; error={message}"
        );
        assert!(
            message.contains("finch daemon-stop"),
            "leftover error must name the exact kick command; error={message}"
        );

        let log = std::fs::read_to_string(home.path().join(".finch").join("daemon.log"))
            .unwrap_or_default();
        assert!(
            log.contains("leftover daemon handshake rejected"),
            "daemon.log must record the rejected leftover handshake; log={log:?}"
        );
        assert!(
            log.contains("frontend protocol") && log.contains("daemon protocol 8"),
            "daemon.log must name expected vs found generation; log={log:?}"
        );
    }

    #[test]
    fn test_isolation_gate_denies_before_probe_reuse_or_spawn() {
        // The permanent Brain-isolation CI gate supplies authenticated test authority.
        if std::env::var_os("FINCH_BRAIN_TEST_TOKEN").is_none() {
            return;
        }
        let error = ensure_daemon_access_allowed().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("discovery, reuse, and auto-spawn are disabled"),
            "unexpected isolation-gate error: {error:#}"
        );
    }

    #[test]
    fn test_isolation_gate_also_denies_direct_detached_spawn() {
        // The permanent Brain-isolation CI gate supplies authenticated test authority.
        if std::env::var_os("FINCH_BRAIN_TEST_TOKEN").is_none() {
            return;
        }
        assert!(ensure_daemon_access_allowed().is_err());
    }
}
