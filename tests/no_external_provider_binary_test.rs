#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output, Stdio};

struct BoundedOutput {
    output: Output,
    timed_out: bool,
}

fn run_bounded(mut command: Command) -> BoundedOutput {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let output = child.wait_with_output().unwrap();
    BoundedOutput { output, timed_out }
}

fn assert_codex_was_not_executed(marker: &std::path::Path, boundary: &str) {
    assert!(
        !marker.exists(),
        "hostile codex executable ran during {boundary}"
    );
}

fn assert_no_connection(listener: &std::net::TcpListener, boundary: &str) {
    assert!(
        matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ),
        "unexpected external connection during {boundary}"
    );
}

#[test]
fn test_hostile_codex_on_path_is_never_spawned_by_cli_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let bin_dir = directory.path().join("bin");
    let finch_dir = directory.path().join("home/.finch");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&finch_dir).unwrap();

    let marker = directory.path().join("codex-executed");
    let codex = bin_dir.join("codex");
    std::fs::write(
        &codex,
        format!(
            "#!/bin/sh\nprintf executed > '{}'\nexit 99\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o700)).unwrap();

    let provider_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    provider_listener.set_nonblocking(true).unwrap();
    let daemon_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    daemon_listener.set_nonblocking(true).unwrap();
    std::fs::write(
        finch_dir.join("config.toml"),
        format!(
            r#"[[providers]]
type = "chatgpt_subscription"
credential_ref = "codex-app-server:managed"
model = "gpt-5.6-sol"
name = "legacy"

[[providers]]
type = "grok"
api_key = "xai-test"
model = "grok-code-fast-1"
base_url = "http://{}"
name = "must-not-be-selected"

[client]
use_daemon = false
daemon_address = "http://{}"
auto_spawn = false
timeout_seconds = 1
auto_discover = false
prefer_local = true
"#,
            provider_listener.local_addr().unwrap(),
            daemon_listener.local_addr().unwrap(),
        ),
    )
    .unwrap();

    let finch = env!("CARGO_BIN_EXE_finch");
    let base = |command: &mut Command| {
        command
            .env("HOME", directory.path().join("home"))
            .env("PATH", &bin_dir)
            .env_remove("OPENAI_API_KEY")
            .env_remove("CODEX_HOME")
            .env_remove("FINCH_LIVE_CHATGPT_APP_SERVER");
    };

    let mut request = Command::new(finch);
    base(&mut request);
    request.args(["--cloud-only", "query", "do not execute providers"]);
    let request = run_bounded(request);
    assert!(!request.timed_out, "query boundary did not terminate");
    assert!(!request.output.status.success());
    let stderr = String::from_utf8_lossy(&request.output.stderr);
    assert!(
        stderr.contains("Legacy chatgpt_subscription profiles are unsupported"),
        "{stderr}"
    );
    assert_codex_was_not_executed(
        &marker,
        "config load, provider construction, startup, or request",
    );
    assert_no_connection(&provider_listener, "fallback provider selection/request");
    assert_no_connection(&daemon_listener, "query daemon connection or auto-spawn");

    let mut auth = Command::new(finch);
    base(&mut auth);
    auth.args(["auth", "status", "chatgpt"]);
    let auth = run_bounded(auth);
    assert!(!auth.timed_out, "removed auth command did not terminate");
    assert!(!auth.output.status.success());
    assert_codex_was_not_executed(&marker, "removed auth command");
    assert_no_connection(&provider_listener, "removed auth command");
    assert_no_connection(&daemon_listener, "removed auth command");

    let mut setup = Command::new(finch);
    base(&mut setup);
    setup.arg("setup");
    let _setup = run_bounded(setup);
    assert_codex_was_not_executed(&marker, "interactive setup startup");
    assert_no_connection(&provider_listener, "interactive setup startup");
    assert_no_connection(&daemon_listener, "interactive setup startup");
}
