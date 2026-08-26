//! Isolated integration tests for the Finch daemon binary.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

struct TestDaemon {
    _child: OwnedChild,
    _home: TempDir,
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
        anyhow::ensure!(
            std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() == Ok("1"),
            "daemon integration tests require scripts/test_brains.sh"
        );
        let suite_home = std::env::var_os("FINCH_BRAIN_TEST_HOME")
            .map(PathBuf::from)
            .context("missing isolated suite HOME")?;
        let home = tempfile::Builder::new()
            .prefix("daemon-case-")
            .tempdir_in(suite_home)
            .context("create isolated daemon HOME")?;
        let finch_dir = home.path().join(".finch");
        std::fs::create_dir_all(&finch_dir)?;
        write_config(home.path(), "127.0.0.1:0", api_key)?;
        let address_file = finch_dir.join("bound.addr");
        let child = Command::new(env!("CARGO_BIN_EXE_finch"))
            .arg("daemon")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .env("HOME", home.path())
            .env("FINCH_BRAIN_TEST_ISOLATED", "1")
            .env("FINCH_BRAIN_TEST_HOME", home.path())
            .env("FINCH_BRAIN_TEST_ROOT", finch_dir.join("brains"))
            .env("FINCH_TEST_BOUND_ADDR_FILE", &address_file)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn isolated Finch daemon")?;
        let mut child = OwnedChild(child);

        let deadline = Instant::now() + Duration::from_secs(10);
        let address = loop {
            if let Ok(address) = std::fs::read_to_string(&address_file) {
                break address.trim().to_owned();
            }
            if let Some(status) = child.0.try_wait()? {
                anyhow::bail!("isolated daemon exited before binding: {status}");
            }
            if Instant::now() >= deadline {
                anyhow::bail!("isolated daemon did not publish its ephemeral address");
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        anyhow::ensure!(address.starts_with("127.0.0.1:") && !address.ends_with(":0"));
        wait_for_health(&address).await?;
        write_config(home.path(), &address, api_key)?;

        Ok(Self {
            _child: child,
            home_path: home.path().to_path_buf(),
            _home: home,
            address,
        })
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.address)
    }
}

fn write_config(home: &Path, daemon_address: &str, api_key: &str) -> Result<()> {
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
brain_password = "isolated-test-password"
"#
    );
    std::fs::write(home.join(".finch/config.toml"), config)?;
    Ok(())
}

async fn wait_for_health(address: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(250))
        .build()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if client
            .get(format!("http://{address}/health"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "isolated daemon health check timed out"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
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
    let daemon = TestDaemon::start("isolated-health-test").await?;
    let response: serde_json::Value = reqwest::get(format!("{}/health", daemon.base_url()))
        .await?
        .json()
        .await?;
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
