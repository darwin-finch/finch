// Integration tests for daemon mode
//
// These tests verify the full daemon lifecycle:
// - Daemon spawn/shutdown
// - CLI → Daemon communication
// - Fallback behavior

use anyhow::Result;
use std::process::{Command, Stdio};
use std::time::Duration;
use tokio::time::sleep;

/// Helper to check if daemon is running on a port
async fn is_daemon_running(port: u16) -> bool {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap();

    let url = format!("http://127.0.0.1:{}/health", port);
    match client.get(&url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

/// Helper to start daemon in background
fn start_daemon(port: u16) -> Result<std::process::Child> {
    let child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("daemon")
        .arg("--bind")
        .arg(format!("127.0.0.1:{}", port))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(child)
}

/// Helper to run a query via CLI
fn run_query(query: &str) -> Result<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("query")
        .arg(query)
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[tokio::test]
#[ignore] // Requires daemon binary and network
async fn test_daemon_spawn_and_health() -> Result<()> {
    let port = 11440; // Use different port to avoid conflicts

    // Start daemon
    let mut daemon = start_daemon(port)?;

    // Wait for daemon to start
    sleep(Duration::from_secs(3)).await;

    // Check health endpoint
    assert!(is_daemon_running(port).await, "Daemon should be running");

    // Cleanup
    daemon.kill()?;
    daemon.wait()?;

    Ok(())
}

#[tokio::test]
#[ignore] // Requires daemon binary and network
async fn test_daemon_query() -> Result<()> {
    let port = 11441;

    // Start daemon
    let mut daemon = start_daemon(port)?;
    sleep(Duration::from_secs(3)).await;

    // Update config to use this port
    // TODO: This test needs config management

    // Run query
    let response = run_query("What is 2+2?")?;
    assert!(response.contains("4"), "Response should contain answer");

    // Cleanup
    daemon.kill()?;
    daemon.wait()?;

    Ok(())
}

#[tokio::test]
#[ignore] // Requires daemon binary
async fn test_fallback_without_daemon() -> Result<()> {
    // Ensure no daemon running on default port
    // (This test verifies fallback to teacher API)

    let response = run_query("test")?;
    assert!(!response.is_empty(), "Should get response from teacher API");

    Ok(())
}

#[test]
fn test_daemon_config_parsing() {
    // Test that daemon config is parsed correctly
    let config_toml = r#"
        [client]
        use_daemon = true
        daemon_address = "127.0.0.1:11435"
        auto_spawn = true
        timeout_seconds = 120
    "#;

    let config: toml::Value = toml::from_str(config_toml).unwrap();
    assert_eq!(config["client"]["daemon_address"].as_str(), Some("127.0.0.1:11435"));
    assert_eq!(config["client"]["use_daemon"].as_bool(), Some(true));
}
