#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

fn run_bounded(mut command: Command) -> BoundedOutput {
    run_bounded_with_timeout(&mut command, std::time::Duration::from_secs(5))
}

fn run_bounded_with_timeout(command: &mut Command, timeout: std::time::Duration) -> BoundedOutput {
    let output_directory = tempfile::tempdir().unwrap();
    let stdout_path = output_directory.path().join("stdout");
    let stderr_path = output_directory.path().join("stderr");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
        .process_group(0);
    let mut child = command.spawn().unwrap();
    let process_group = nix::unistd::Pid::from_raw(-(child.id() as i32));
    let deadline = std::time::Instant::now() + timeout;
    let (mut status, timed_out) = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break (Some(status), false);
        }
        if std::time::Instant::now() >= deadline {
            let _ = nix::sys::signal::kill(process_group, nix::sys::signal::Signal::SIGKILL);
            let _ = child.kill();
            break (None, true);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };

    // Kill any descendants still sharing the group even if the direct child
    // exited first, then reap the direct child without an unbounded wait.
    let _ = nix::sys::signal::kill(process_group, nix::sys::signal::Signal::SIGKILL);
    let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while status.is_none() && std::time::Instant::now() < reap_deadline {
        status = child.try_wait().unwrap();
        if status.is_none() {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let status = status.expect("bounded child did not exit after its process group was killed");
    let stdout = std::fs::read(stdout_path).unwrap();
    let stderr = std::fs::read(stderr_path).unwrap();
    BoundedOutput {
        status,
        stdout,
        stderr,
        timed_out,
    }
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
fn test_bounded_runner_kills_descendant_retaining_output() {
    let started = std::time::Instant::now();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "(sleep 60) & printf ready; wait"]);
    let result = run_bounded_with_timeout(&mut command, std::time::Duration::from_millis(200));

    assert!(result.timed_out);
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    assert_eq!(result.stdout, b"ready");
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
    assert!(!request.status.success());
    let stderr = String::from_utf8_lossy(&request.stderr);
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
    assert!(!auth.status.success());
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
