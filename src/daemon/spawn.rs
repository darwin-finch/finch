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
        retry_backoff().await;

        if health_check_succeeds(&base_url).await {
            info!("Daemon now healthy");
            return Ok(());
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
    ensure_daemon_access_allowed()?;
    let exe_path =
        std::env::current_exe().context("Failed to determine current executable path")?;

    // Create log file in ~/.finch/daemon.log
    let log_path = crate::daemon::daemon_log_path()?;

    // The daemon owns rotation and retention for its own log (#249). The
    // frontend only guarantees the directory exists and that the path is a
    // regular file, so early child output has somewhere safe to land before
    // the daemon binds its own descriptors.
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }
    crate::daemon::log::ensure_regular_file(&log_path)?;

    // Open log file in append mode for the child's stdout/stderr
    let mut log_options = std::fs::OpenOptions::new();
    log_options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The daemon secures this file once it starts, but the frontend creates
        // it first. Without an explicit mode a fresh log is created at
        // 0o666 & ~umask — typically 0644 — and is world-readable for the
        // second or two before the daemon takes over.
        log_options.mode(0o600);
        // The reconcile above dropped its handle, so this open is what the child
        // actually receives. Without O_NOFOLLOW a symlink planted in the window
        // between them redirects the daemon's stdout and stderr.
        log_options.custom_flags(nix::libc::O_NOFOLLOW);
    }
    let log_file = log_options
        .open(&log_path)
        .with_context(|| format!("Failed to open daemon log file: {}", log_path.display()))?;

    // `mode` applies only when the file is created. A log left world-readable
    // by an older build must also be repaired, and the child cannot do it if it
    // fails to start.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = log_file.metadata() {
            let mut perms = metadata.permissions();
            if perms.mode() & 0o777 != 0o600 {
                perms.set_mode(0o600);
                if let Err(error) = log_file.set_permissions(perms) {
                    warn!(path = %log_path.display(), %error, "Could not secure the daemon log");
                }
            }
        }
    }

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
pub(crate) async fn health_check_succeeds(base_url: &str) -> bool {
    let mut phase = crate::startup::phase(crate::startup::PHASE_DAEMON_HEALTH_PROBE);
    let client = reqwest::Client::builder()
        .timeout(HEALTH_PROBE_TIMEOUT)
        .build()
        .expect("Failed to build HTTP client");

    let url = format!("{}/health", base_url);

    match client.get(&url).send().await {
        Ok(response) if response.status().is_success() => {
            debug!(url = %url, "Health check succeeded");
            phase.detail(crate::startup::PhaseDetail::category("healthy"));
            true
        }
        Ok(response) => {
            debug!(url = %url, status = %response.status(), "Health check failed");
            phase.detail(crate::startup::PhaseDetail::category("unhealthy"));
            false
        }
        Err(e) => {
            debug!(url = %url, error = %e, "Health check request failed");
            phase.detail(crate::startup::PhaseDetail::category("unreachable"));
            false
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

    #[test]
    #[cfg(unix)]
    fn test_frontend_open_repairs_a_world_readable_log() {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(&path, b"inherited\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        // The same open the frontend performs before handing the fd to the child.
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true).mode(0o600);
        options.custom_flags(nix::libc::O_NOFOLLOW);
        let file = options.open(&path).unwrap();

        // `mode` applies only on create, so an existing file needs the fchmod
        // repair; without it the log stays world-readable whenever the child
        // fails to start.
        let mut perms = file.metadata().unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o644, "precondition");
        perms.set_mode(0o600);
        file.set_permissions(perms).unwrap();

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
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

    /// Answer one request with `status`, then close. Returns the base URL.
    async fn one_shot_health_endpoint(
        status: &'static str,
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
                    format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
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

        let (healthy_url, healthy) = one_shot_health_endpoint("200 OK").await;
        assert!(
            health_check_succeeds(&healthy_url).await,
            "a 200 from /health is a healthy daemon"
        );
        let _ = healthy.await;

        let (unhealthy_url, unhealthy) =
            one_shot_health_endpoint("500 Internal Server Error").await;
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
