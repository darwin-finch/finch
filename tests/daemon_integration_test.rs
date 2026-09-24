//! Isolated integration tests for the Finch daemon binary.

use anyhow::{Context, Result};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct TestDaemon {
    _child: OwnedChild,
    _serial: tokio::sync::OwnedMutexGuard<()>,
    home_path: PathBuf,
    address: String,
    ipc_socket: PathBuf,
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl TestDaemon {
    async fn start(api_key: &str) -> Result<Self> {
        static SERIAL: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
            std::sync::OnceLock::new();
        let serial = SERIAL
            .get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
            .lock_owned()
            .await;
        let proof = finch::brain::isolated_test_proof()
            .context("daemon integration tests require supervisor authority")?;
        let brain_address = proof.brain_address().to_owned();
        let daemon_address = proof.daemon_address().to_owned();
        let socket_root = std::env::var("FINCH_TEST_SOCKET_ROOT").unwrap_or_default();
        let ipc_socket = std::env::var_os("FINCH_TEST_IPC_SOCKET")
            .map(PathBuf::from)
            .context("daemon integration test requires its sealed IPC socket path")?;
        require_bounded_unix_socket_path(&ipc_socket)?;
        let brain_password = proof.brain_password()?;
        let home = proof.home;
        let finch_dir = home.join(".finch");
        std::fs::create_dir_all(finch_dir.join("brains"))?;
        write_config(&home, &daemon_address, api_key, &brain_password)?;
        let address_file = finch_dir.join(format!("bound-{}.addr", uuid::Uuid::new_v4().simple()));
        let stderr_path =
            finch_dir.join(format!("daemon-{}.stderr", uuid::Uuid::new_v4().simple()));
        let stderr_file = open_diagnostic_file(&stderr_path)?;
        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .arg("daemon")
            .arg("--bind")
            .arg(&daemon_address)
            .env("FINCH_TEST_BOUND_ADDR_FILE", &address_file)
            .env("RUST_LOG", "finch=debug,tower_http=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file.try_clone()?));
        let child = command.spawn().context("spawn isolated Finch daemon")?;
        let mut child = OwnedChild(child);
        let spawn_started = Instant::now();

        // Coarse liveness bound, not a latency assertion (#476): a full daemon
        // startup on a loaded shared runner can exceed 30s (measured 0-byte
        // log, alive-but-unbound at 30s); the security assertions below
        // (address within supervisor authority, health responds) hold at any
        // speed. 60s matches the hostile-mode startup convention.
        let deadline = Instant::now() + Duration::from_secs(60);
        let address = loop {
            if let Ok(address) = std::fs::read_to_string(&address_file) {
                break address.trim().to_owned();
            }
            if let Some(status) = child.0.try_wait()? {
                let stderr = bounded_child_stderr(&stderr_file)
                    .replace(home.to_string_lossy().as_ref(), "<isolated-home>")
                    .replace(&socket_root, "<socket-root>")
                    .replace(&brain_address, "<brain-address>")
                    .replace(&daemon_address, "<daemon-address>")
                    .replace(&brain_password, "<brain-password>")
                    .replace(api_key, "<api-key>");
                let daemon_log = redact_daemon_diagnostic(
                    bounded_path_diagnostic(&finch_dir.join("daemon.log")),
                    &home,
                    &socket_root,
                    &brain_address,
                    &daemon_address,
                    &brain_password,
                    api_key,
                );
                anyhow::bail!(
                    "isolated daemon exited before binding: {status}; \
                     bounded stderr={stderr:?}; bounded daemon log={daemon_log:?}"
                );
            }
            if Instant::now() >= deadline {
                let stderr = redact_daemon_diagnostic(
                    bounded_child_stderr(&stderr_file),
                    &home,
                    &socket_root,
                    &brain_address,
                    &daemon_address,
                    &brain_password,
                    api_key,
                );
                let daemon_log = redact_daemon_diagnostic(
                    bounded_path_diagnostic(&finch_dir.join("daemon.log")),
                    &home,
                    &socket_root,
                    &brain_address,
                    &daemon_address,
                    &brain_password,
                    api_key,
                );
                // #868: a hang this deep has historically produced no
                // evidence — the daemon is alive, daemon.log is empty or
                // absent, and stderr carries only the startup hint. Capture
                // the kernel's view of the process instead: its scheduler
                // state, wait channel, current syscall, and per-thread kernel
                // stacks. This names the blocked phase without strace or a
                // debugger, which the supervised runner does not offer.
                let kernel_state = redact_daemon_diagnostic(
                    daemon_kernel_state_snapshot(child.0.id(), spawn_started.elapsed()),
                    &home,
                    &socket_root,
                    &brain_address,
                    &daemon_address,
                    &brain_password,
                    api_key,
                );
                anyhow::bail!(
                    "isolated daemon remained alive but did not publish its ephemeral address \
                     within 60s; bounded stderr={stderr:?}; bounded daemon log={daemon_log:?}; \
                     bounded kernel state={kernel_state:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        anyhow::ensure!(
            address == daemon_address,
            "daemon published an address outside supervisor authority"
        );
        if let Err(error) = wait_for_health(&address).await {
            let stderr = redact_daemon_diagnostic(
                bounded_child_stderr(&stderr_file),
                &home,
                &socket_root,
                &brain_address,
                &daemon_address,
                &brain_password,
                api_key,
            );
            let daemon_log = redact_daemon_diagnostic(
                bounded_path_diagnostic(&finch_dir.join("daemon.log")),
                &home,
                &socket_root,
                &brain_address,
                &daemon_address,
                &brain_password,
                api_key,
            );
            anyhow::bail!(
                "{error}; bounded daemon stderr={stderr:?}; bounded daemon log={daemon_log:?}"
            );
        }
        // #249 regression: `run_daemon` binds its own stdout/stderr only when the
        // detached marker is set. This harness does not set it, so this daemon
        // is in the documented foreground shape — `finch daemon` in a terminal,
        // `finch worker`, and the shipped systemd unit. Its startup line must
        // still reach the inherited stderr. Binding unconditionally sent this
        // into daemon.log and left operators with a blank journal.
        let foreground_stderr = redact_daemon_diagnostic(
            bounded_child_stderr(&stderr_file),
            &home,
            &socket_root,
            &brain_address,
            &daemon_address,
            &brain_password,
            api_key,
        );
        anyhow::ensure!(
            foreground_stderr.contains("Daemon logs:"),
            "a non-detached daemon must keep writing to its inherited stderr; \
             got {foreground_stderr:?}"
        );

        write_config(&home, &address, api_key, &brain_password)?;

        Ok(Self {
            _child: child,
            _serial: serial,
            home_path: home,
            address,
            ipc_socket,
        })
    }
}

#[cfg(unix)]
fn require_bounded_unix_socket_path(path: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt as _;

    let address: nix::libc::sockaddr_un = unsafe { std::mem::zeroed() };
    anyhow::ensure!(
        path.as_os_str().as_bytes().len() < address.sun_path.len(),
        "sealed IPC socket path exceeds the platform sockaddr_un bound"
    );
    Ok(())
}

#[cfg(not(unix))]
fn require_bounded_unix_socket_path(_path: &Path) -> Result<()> {
    Ok(())
}

fn redact_daemon_diagnostic(
    diagnostic: String,
    home: &Path,
    socket_root: &str,
    brain_address: &str,
    daemon_address: &str,
    brain_password: &str,
    api_key: &str,
) -> String {
    diagnostic
        .replace(home.to_string_lossy().as_ref(), "<isolated-home>")
        .replace(socket_root, "<socket-root>")
        .replace(brain_address, "<brain-address>")
        .replace(daemon_address, "<daemon-address>")
        .replace(brain_password, "<brain-password>")
        .replace(api_key, "<api-key>")
}

#[cfg(unix)]
fn open_diagnostic_file(path: &Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    Ok(std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn open_diagnostic_file(path: &Path) -> Result<std::fs::File> {
    Ok(std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?)
}

#[cfg(unix)]
fn bounded_child_stderr(stderr: &std::fs::File) -> String {
    use std::os::unix::fs::FileExt as _;

    const LIMIT: usize = 64 * 1024;
    let length = stderr
        .metadata()
        .map(|metadata| metadata.len().min(LIMIT as u64) as usize)
        .unwrap_or(0);
    let mut output = vec![0_u8; length];
    let mut offset = 0;
    while offset < output.len() {
        match stderr.read_at(&mut output[offset..], offset as u64) {
            Ok(0) => {
                output.truncate(offset);
                break;
            }
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return format!("<stderr read failed: {error}>"),
        }
    }
    if stderr
        .metadata()
        .is_ok_and(|metadata| metadata.len() > LIMIT as u64)
    {
        output.extend_from_slice(b"<truncated>");
    }
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(unix)]
fn bounded_path_diagnostic(path: &Path) -> String {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => return format!("<daemon log unavailable: {error}>"),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return format!("<daemon log metadata unavailable: {error}>"),
    };
    if !metadata.is_file() || metadata.uid() != unsafe { nix::libc::geteuid() } {
        return "<daemon log is not an owner-controlled regular file>".to_owned();
    }
    bounded_child_stderr(&file)
}

#[cfg(not(unix))]
fn bounded_path_diagnostic(_path: &Path) -> String {
    "<bounded daemon log capture requires Unix>".to_owned()
}

#[cfg(not(unix))]
fn bounded_child_stderr(_stderr: &std::fs::File) -> String {
    "<bounded daemon stderr capture requires Unix>".to_owned()
}

/// Upper bound on one daemon kernel-state capture.
const KERNEL_STATE_LIMIT: usize = 64 * 1024;

/// Compose named capture sections into one bounded diagnostic string.
///
/// Sections are emitted in order under `--- <name> ---` headers. Once the
/// bound is reached, the remaining headers are named but their bodies are
/// replaced with a truncation note, so a reader always sees which sources
/// existed even when their content did not fit.
fn compose_kernel_state_sections(limit: usize, sections: Vec<(String, String)>) -> String {
    let mut output = String::new();
    for (name, body) in sections {
        let section = format!("\n--- {name} ---\n{body}");
        if output.len() + section.len() > limit {
            output.push_str(&format!(
                "\n--- {name} ---\n<truncated: kernel-state capture bound of {limit} bytes reached>"
            ));
            break;
        }
        output.push_str(&section);
    }
    output
}

/// Read one small procfs file to a bounded string.
///
/// procfs files report `st_size` as 0, so the existing
/// [`bounded_child_stderr`] — which sizes its buffer from the metadata —
/// cannot read them. This reads to EOF instead, bounding by bytes consumed.
#[cfg(target_os = "linux")]
fn bounded_proc_file(path: &Path, limit: usize) -> String {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return format!("<unavailable: {error}>"),
    };
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => {
                buffer.extend_from_slice(&chunk[..count]);
                if buffer.len() >= limit {
                    buffer.truncate(limit);
                    buffer.extend_from_slice(b"<truncated>");
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return format!("<read failed: {error}>"),
        }
    }
    if buffer.is_empty() {
        // #868: some CI containers permit opening /proc/<tid>/stack but
        // return no bytes (restricted stacktrace or kernel config). An
        // empty body must be named, or it is indistinguishable from a
        // section the composer silently dropped.
        return "<empty: source opened successfully but produced no bytes>".to_owned();
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Kernel-visible state of a live daemon, for startup-hang diagnosis (#868).
///
/// Read-only: scheduler state, wait channel, current syscall, and kernel
/// stacks. Deliberately excludes `/proc/<pid>/environ` (it carries the
/// supervisor's sealed credentials), `/proc/<pid>/maps`, and
/// `/proc/<pid>/mem` — a hang diagnosis needs "where is it blocked", not a
/// memory dump. Threads are capped so a runaway thread count cannot exceed
/// the capture bound.
#[cfg(target_os = "linux")]
fn daemon_kernel_state_snapshot(pid: u32, waited: Duration) -> String {
    const THREAD_LIMIT: usize = 16;
    let root = PathBuf::from(format!("/proc/{pid}"));
    let mut sections = vec![(
        "daemon hang summary".to_owned(),
        format!(
            "pid={pid} still alive after {}s without publishing its address; \
             kernel state follows (read-only /proc capture)",
            waited.as_secs()
        ),
    )];
    for name in ["status", "wchan", "syscall", "stack"] {
        sections.push((
            format!("/proc/{pid}/{name}"),
            bounded_proc_file(&root.join(name), 8 * 1024),
        ));
    }
    let mut tids: Vec<u32> = std::fs::read_dir(root.join("task"))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter_map(|name| name.parse::<u32>().ok())
                .collect()
        })
        .unwrap_or_default();
    tids.sort_unstable();
    tids.truncate(THREAD_LIMIT);
    for tid in tids {
        for name in ["comm", "wchan", "stack"] {
            sections.push((
                format!("/proc/{pid}/task/{tid}/{name}"),
                bounded_proc_file(
                    &root.join("task").join(tid.to_string()).join(name),
                    4 * 1024,
                ),
            ));
        }
    }
    compose_kernel_state_sections(KERNEL_STATE_LIMIT, sections)
}

#[cfg(not(target_os = "linux"))]
fn daemon_kernel_state_snapshot(_pid: u32, _waited: Duration) -> String {
    "<daemon kernel-state capture requires Linux; read the bounded stderr and \
     daemon log sections above>"
        .to_owned()
}

// `[backend]` must stay disabled here: `BackendConfig::default()` (enabled=true)
// spawns real local GGUF model loading as a plain `tokio::spawn` task that does
// not yield, which on a 2-vCPU CI runner starves the same multi-thread runtime
// the HTTP listener needs to reach `publish_isolated_test_address` — the
// isolated daemon stays alive but doesn't publish its address for 40s+, well
// past the address-wait bound, even though the test asserts nothing about
// local generation. `tests/named_brain_attach.rs` already disables it for the
// same reason.
fn write_config(
    home: &Path,
    daemon_address: &str,
    api_key: &str,
    brain_password: &str,
) -> Result<()> {
    let config = format!(
        r#"[[providers]]
type = "claude"
api_key = {api_key:?}

[client]
use_daemon = true
daemon_address = {daemon_address:?}
auto_spawn = false
timeout_seconds = 10
auto_discover = false
prefer_local = true

[backend]
enabled = false
execution_target = "cpu"

[server]
enabled = true
bind_address = "127.0.0.1:0"
brain_bind_address = "127.0.0.1:0"
auth_enabled = false
api_keys = []
mode = "daemon-only"
advertise = false
service_name = "finch-isolated-test"
service_description = "isolated test"
brain_password = {brain_password:?}
"#
    );
    std::fs::write(home.join(".finch/config.toml"), config)?;
    Ok(())
}

async fn wait_for_health(address: &str) -> Result<()> {
    // Same coarse bound as the address wait above: loaded runners start the
    // daemon slower than a fixed 10s assumes, and health responding at all is
    // the assertion.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let last_result = match request_health(address, Duration::from_millis(250)) {
            Ok(_) => return Ok(()),
            Err(error) => error.to_string(),
        };
        anyhow::ensure!(
            Instant::now() < deadline,
            "isolated daemon health check timed out ({last_result})"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn request_health(address: &str, timeout: Duration) -> Result<serde_json::Value> {
    let socket_address = address.parse()?;
    let mut stream = std::net::TcpStream::connect_timeout(&socket_address, timeout)
        .context("direct loopback health connect failed")?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .context("direct loopback health response failed")?;
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("direct loopback health response has no header terminator")?;
    let (head, body) = response.split_at(separator + 4);
    anyhow::ensure!(
        head.starts_with(b"HTTP/1.1 200 "),
        "direct loopback health status was not 200"
    );
    serde_json::from_slice(body).context("direct loopback health body was not JSON")
}

fn run_query(home: &Path, query: &str) -> Result<std::process::Output> {
    Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("query")
        .arg(query)
        .env("HOME", home)
        .output()
        .context("run isolated Finch query")
}

#[tokio::test]
#[ignore = "spawns the built daemon binary"]
async fn test_daemon_spawn_and_health() -> Result<()> {
    let daemon = TestDaemon::start("sk-ant-isolated-health-test").await?;
    let response = request_health(&daemon.address, Duration::from_secs(2))?;
    assert_eq!(response["status"], "healthy");
    assert!(daemon.ipc_socket.exists());
    assert!(!daemon.home_path.join(".finch/daemon.sock").exists());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a live cloud provider API credential"]
async fn test_daemon_query() -> Result<()> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY is required for the ignored daemon query smoke")?;
    let daemon = TestDaemon::start(&api_key).await?;
    let output = run_query(&daemon.home_path, "What is 2+2?")?;
    anyhow::ensure!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains('4'));
    Ok(())
}

#[test]
fn test_daemon_config_parsing() {
    let config_toml = r#"
        [client]
        use_daemon = true
        daemon_address = "127.0.0.1:0"
        auto_spawn = false
    "#;
    let config: toml::Value = toml::from_str(config_toml).unwrap();
    assert_eq!(
        config["client"]["daemon_address"].as_str(),
        Some("127.0.0.1:0")
    );
    assert_eq!(config["client"]["auto_spawn"].as_bool(), Some(false));
}

// Regression for the isolation-gate flake root-caused on 2026-09-18: without
// `[backend] enabled = false`, `write_config` produces a config that falls
// back to `BackendConfig::default()` (enabled=true), which makes the isolated
// daemon load a real local GGUF model on a plain (non-yielding) `tokio::spawn`
// task. On the 2-vCPU ubuntu-24.04 CI runner that starves the multi-thread
// runtime's other worker for 40s+, so `TestDaemon::start` misses its
// address-publication bound even though the test never exercises local
// generation. This test fails before the `[backend]` block existed (no
// `enabled` key at all, so a real loader would default it to `true`) and
// passes after.
#[test]
fn test_write_config_disables_local_backend() {
    let home = std::env::temp_dir().join(format!(
        "finch-daemon-integration-test-write-config-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(home.join(".finch")).unwrap();
    write_config(&home, "127.0.0.1:0", "sk-ant-test", "test-password").unwrap();
    let contents = std::fs::read_to_string(home.join(".finch/config.toml")).unwrap();
    let config: toml::Value = toml::from_str(&contents).unwrap();
    let _ = std::fs::remove_dir_all(&home);
    assert_eq!(
        config["backend"]["enabled"].as_bool(),
        Some(false),
        "isolated daemon test config must disable the local model backend, or the daemon \
         spawns real local model loading and can miss its address-publication bound under CI \
         load: config={contents:?}"
    );
}

#[cfg(test)]
mod kernel_state_capture_tests {
    use super::*;

    /// The composer must name every section it was given, keep bodies in
    /// order, and — when the bound is hit — still name the remaining sources
    /// with an explicit truncation note, so a reader of a CI log can tell
    /// "this section existed but did not fit" from "this section was empty".
    #[test]
    fn test_kernel_state_sections_compose_bounded_and_name_their_sources() {
        let small = compose_kernel_state_sections(
            1024,
            vec![
                ("one".to_owned(), "first body".to_owned()),
                ("two".to_owned(), String::new()),
            ],
        );
        assert!(
            small.contains("--- one ---") && small.contains("first body"),
            "a fitting section must appear with its body; got {small:?}"
        );
        assert!(
            small.contains("--- two ---"),
            "an empty section must still be named so its emptiness is \
             distinguishable from a missing source; got {small:?}"
        );

        let big_body = "x".repeat(2048);
        let overflow_body = "y".repeat(4096);
        let bounded = compose_kernel_state_sections(
            4096,
            vec![
                ("first".to_owned(), big_body.clone()),
                ("second".to_owned(), overflow_body.clone()),
            ],
        );
        assert!(
            bounded.contains("--- first ---") && bounded.contains(&big_body),
            "the first section must survive whole when it fits the bound; got {} bytes",
            bounded.len()
        );
        assert!(
            bounded.contains("--- second ---")
                && bounded
                    .contains("<truncated: kernel-state capture bound of 4096 bytes reached>"),
            "an overflowing section must be named with a truncation note rather \
             than silently dropped; got {bounded:?}"
        );
        assert!(
            !bounded.contains(&overflow_body),
            "a section past the bound must not leak its content; got {} bytes",
            bounded.len()
        );
    }

    /// The composer's bound must hold even when a single section alone
    /// exceeds it: the output never grows past the limit plus one truncation
    /// note, and that section is named.
    #[test]
    fn test_kernel_state_sections_never_exceed_their_bound() {
        let huge = "y".repeat(4 * KERNEL_STATE_LIMIT);
        let composed =
            compose_kernel_state_sections(KERNEL_STATE_LIMIT, vec![("huge".to_owned(), huge)]);
        assert!(
            composed.len() <= KERNEL_STATE_LIMIT + 512,
            "the composed capture must stay within the bound plus one truncation \
             note; got {} bytes with bound {KERNEL_STATE_LIMIT}",
            composed.len()
        );
        assert!(
            composed.contains("--- huge ---"),
            "the oversized section must still be named; got {} bytes",
            composed.len()
        );
    }

    /// Reading a procfs file through the dedicated reader must produce
    /// content even though procfs reports `st_size` 0 — the reason
    /// `bounded_child_stderr` cannot be reused here.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_file_reader_reads_sizeless_procfs_content() {
        let status = bounded_proc_file(Path::new("/proc/self/status"), 8 * 1024);
        assert!(
            status.contains("Name:") && status.contains("State:"),
            "/proc/self/status read through the procfs reader must carry its \
             fields despite st_size 0; got {status:?}"
        );
    }

    /// A missing process must be reported as unavailable with the OS error,
    /// not as empty output — "the process is gone" and "the file is empty"
    /// are different diagnoses.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_proc_file_reader_names_a_missing_process() {
        let missing = bounded_proc_file(Path::new("/proc/4294967295/status"), 1024);
        assert!(
            missing.starts_with("<unavailable:"),
            "a nonexistent /proc entry must report unavailability with its \
             error, not an empty string; got {missing:?}"
        );
    }

    /// The snapshot of a live process must contain the hang summary, the
    /// scheduler state, and per-thread sections, so a CI failure names the
    /// blocked phase without strace.
    ///
    /// Sources that every Linux environment provides (/proc/<pid>/status
    /// fields, wchan) must carry real content. Sources that CI containers
    /// commonly restrict (per-thread kernel stacks from /proc/<tid>/stack)
    /// must appear as an explicit unavailable/empty/truncated marker rather
    /// than being silently absent — a missing section and a captured one
    /// must be distinguishable in the log.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_daemon_kernel_state_snapshot_of_self_reports_live_state() {
        let snapshot = daemon_kernel_state_snapshot(std::process::id(), Duration::from_secs(7));
        assert!(
            snapshot.contains("still alive after 7s"),
            "the snapshot must open with the hang summary naming the wait; got \
             the first 400 bytes: {:?}",
            &snapshot[..snapshot.len().min(400)]
        );
        let status_line = snapshot
            .lines()
            .find(|line| line.starts_with("State:"))
            .expect(
                "the /proc/<pid>/status section of a live self-snapshot must carry its \
                     scheduler 'State:' field on every Linux environment",
            )
            .to_owned();
        assert!(
            snapshot.contains(&format!("/proc/{}/status", std::process::id())),
            "the snapshot must name the status section by its numeric pid \
             (there is no /proc/self rewrite here); got {} bytes",
            snapshot.len()
        );
        assert!(
            status_line.contains("State:"),
            "the scheduler state line must be a well-formed status field; got \
             {status_line:?}"
        );
        assert!(
            snapshot.contains("/wchan"),
            "the snapshot must include wait-channel sections; got {} bytes",
            snapshot.len()
        );
        // Per-thread kernel stacks are the environment-dependent source:
        // restricted CI containers may open /proc/<tid>/stack and get zero
        // bytes, or refuse it outright. Either way the section must say so
        // instead of silently vanishing.
        let stack_sections: Vec<&str> = snapshot
            .split("\n--- ")
            .filter(|section| section.starts_with("/proc/") && section.contains("/stack ---"))
            .collect();
        assert!(
            !stack_sections.is_empty(),
            "the snapshot must name at least one per-thread stack section so its \
             absence is distinguishable from a dropped section; got {} bytes",
            snapshot.len()
        );
        for section in stack_sections {
            let body = section.split("---\n").nth(1).unwrap_or_default();
            assert!(
                body.contains("<unavailable:")
                    || body.contains("<empty:")
                    || body.contains("<truncated>")
                    || body.contains("<read failed:")
                    || body.contains("=>"),
                "every per-thread stack section must carry kernel-stack frames \
                 ('=>') or an explicit unavailable/empty/truncated marker — a \
                 bare empty body would be indistinguishable from a dropped \
                 section; got section body {body:?} within a {}-byte snapshot",
                snapshot.len()
            );
        }
    }

    /// A process that does not exist must still produce a usable capture:
    /// every section names its unavailability, which distinguishes "exited
    /// between try_wait and capture" from a truncated read.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_daemon_kernel_state_snapshot_names_an_unavailable_process() {
        let snapshot = daemon_kernel_state_snapshot(4294967295, Duration::from_secs(1));
        assert!(
            snapshot.matches("<unavailable:").count() >= 4,
            "all four root sections of a missing process must report \
             unavailability; got {snapshot:?}"
        );
    }

    /// Off-Linux the capture says so explicitly rather than returning an
    /// empty string, so a macOS local failure message does not silently lose
    /// the section.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_daemon_kernel_state_snapshot_requires_linux_off_linux() {
        let snapshot = daemon_kernel_state_snapshot(1, Duration::from_secs(1));
        assert!(
            snapshot.contains("requires Linux"),
            "the off-Linux capture must name its limitation; got {snapshot:?}"
        );
    }
}
