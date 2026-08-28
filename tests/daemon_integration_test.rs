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

        let deadline = Instant::now() + Duration::from_secs(10);
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
                anyhow::bail!(
                    "isolated daemon exited before binding: {status}; bounded stderr={stderr:?}"
                );
            }
            if Instant::now() >= deadline {
                anyhow::bail!("isolated daemon did not publish its ephemeral address");
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
        write_config(&home, &address, api_key, &brain_password)?;

        Ok(Self {
            _child: child,
            _serial: serial,
            home_path: home,
            address,
        })
    }
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
    let deadline = Instant::now() + Duration::from_secs(10);
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
    assert!(daemon.home_path.join(".finch/daemon.sock").exists());
    Ok(())
}

#[tokio::test]
#[ignore = "requires a live teacher API credential"]
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
