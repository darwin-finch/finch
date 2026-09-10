#![cfg(unix)]

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};

const AMBIENT_PROVIDER_ENVIRONMENT: &[&str] = &[
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "XAI_API_KEY",
    "GROK_API_KEY",
    "GEMINI_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    timed_out: bool,
}

struct ForeignAuthStoreCanary {
    path: std::path::PathBuf,
    read_marker: std::path::PathBuf,
}

struct ForeignAuthStoreMonitor {
    path: std::path::PathBuf,
    child: Option<std::process::Child>,
}

fn remove_ambient_provider_environment(command: &mut Command) {
    for variable in AMBIENT_PROVIDER_ENVIRONMENT {
        command.env_remove(variable);
    }
}

impl ForeignAuthStoreCanary {
    fn start(home: &std::path::Path) -> Self {
        let codex_dir = home.join(".codex");
        std::fs::create_dir_all(&codex_dir).unwrap();
        let path = codex_dir.join("auth.json");
        nix::unistd::mkfifo(
            &path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_fifo(),
            "foreign credential-store canary is not a FIFO: path={}",
            path.display(),
        );
        let read_marker = codex_dir.join("auth-read-observed");

        Self { path, read_marker }
    }

    fn start_monitor(&self) -> ForeignAuthStoreMonitor {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                "{ : > \"$2\"; printf 'foreign-auth-canary\\n'; } > \"$1\"",
                "foreign-auth-monitor",
            ])
            .arg(&self.path)
            .arg(&self.read_marker)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        ForeignAuthStoreMonitor {
            path: self.path.clone(),
            child: Some(command.spawn().unwrap()),
        }
    }

    fn read_was_observed(&self) -> bool {
        self.read_marker.exists()
    }
}

impl ForeignAuthStoreMonitor {
    fn finish(&mut self) {
        if let Err(error) = self.kill_and_reap() {
            panic!("{error}");
        }
    }

    fn kill_and_reap(&mut self) -> Result<(), String> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };
        let kill_result = child.kill();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.child = None;
                    return Ok(());
                }
                Ok(None) if std::time::Instant::now() < deadline => {}
                outcome => {
                    return Err(format!(
                        "foreign credential-store canary monitor was not reaped after handle \
                         kill: path={} kill_result={kill_result:?} wait_outcome={outcome:?}",
                        self.path.display()
                    ));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

impl Drop for ForeignAuthStoreMonitor {
    fn drop(&mut self) {
        if let Err(error) = self.kill_and_reap() {
            eprintln!("{error}");
        }
    }
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

fn run_bounded_with_canary(mut command: Command, canary: &ForeignAuthStoreCanary) -> BoundedOutput {
    let mut monitor = canary.start_monitor();
    let output = run_bounded_with_timeout(&mut command, std::time::Duration::from_secs(15));
    monitor.finish();
    output
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

fn assert_foreign_auth_was_not_read(
    canary: &ForeignAuthStoreCanary,
    boundary: &str,
    output: &BoundedOutput,
) {
    if canary.read_was_observed() {
        panic!(
            "Finch read the foreign credential store during {boundary}: path={} status={} \
             timed_out={} stdout={:?} stderr={:?}",
            canary.path.display(),
            output.status,
            output.timed_out,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
fn test_foreign_auth_store_canary_detects_read_probe() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let canary = ForeignAuthStoreCanary::start(&home);

    let mut probe = Command::new("/bin/cat");
    probe.env("HOME", &home).arg(&canary.path);
    let mut monitor = canary.start_monitor();
    let result = run_bounded_with_timeout(&mut probe, std::time::Duration::from_secs(2));
    monitor.finish();
    monitor.finish();
    assert!(
        !result.timed_out && result.status.success(),
        "foreign-store read probe did not complete: path={} \
         status={} timed_out={} stdout={:?} stderr={:?}",
        canary.path.display(),
        result.status,
        result.timed_out,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        canary.read_was_observed(),
        "foreign-store read probe escaped the production assertion: path={} status={} \
         timed_out={} stdout={:?} stderr={:?}",
        canary.path.display(),
        result.status,
        result.timed_out,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn test_foreign_auth_store_monitor_reaps_on_unwind() {
    let directory = tempfile::tempdir().unwrap();
    let canary = ForeignAuthStoreCanary::start(&directory.path().join("home"));
    let monitor = canary.start_monitor();
    let monitor_pid = nix::unistd::Pid::from_raw(monitor.child.as_ref().unwrap().id() as i32);

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _monitor = monitor;
        panic!("exercise monitor unwind cleanup");
    }));

    assert!(unwind.is_err(), "monitor cleanup probe did not unwind");
    assert!(
        matches!(
            nix::sys::signal::kill(monitor_pid, None),
            Err(nix::errno::Errno::ESRCH)
        ),
        "foreign credential-store monitor survived or remained unreaped after unwind: pid={monitor_pid}"
    );
}

#[test]
fn test_ambient_provider_environment_is_removed_from_child() {
    let mut command = Command::new("/usr/bin/true");
    for variable in AMBIENT_PROVIDER_ENVIRONMENT {
        command.env(variable, "host-secret-or-proxy");
    }
    remove_ambient_provider_environment(&mut command);

    for variable in AMBIENT_PROVIDER_ENVIRONMENT {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == *variable && value.is_none()),
            "ambient provider or proxy variable was inherited by the child: variable={variable}"
        );
    }
}

#[test]
fn test_hostile_codex_on_path_is_never_spawned_by_cli_boundaries() {
    let directory = tempfile::tempdir().unwrap();
    let bin_dir = directory.path().join("bin");
    let home = directory.path().join("home");
    let finch_dir = home.join(".finch");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&finch_dir).unwrap();
    let foreign_auth_canary = ForeignAuthStoreCanary::start(&home);

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
        remove_ambient_provider_environment(command);
        command
            .env("HOME", &home)
            .env("PATH", &bin_dir)
            .env("ACCOUNT_A_KEY", "secret-that-must-stay-local")
            .env_remove("CODEX_HOME")
            .env_remove("FINCH_LIVE_CHATGPT_APP_SERVER");
    };

    let mut request = Command::new(finch);
    base(&mut request);
    request.args(["--cloud-only", "query", "do not execute providers"]);
    let request = run_bounded_with_canary(request, &foreign_auth_canary);
    assert_foreign_auth_was_not_read(
        &foreign_auth_canary,
        "config load, provider construction, startup, or request",
        &request,
    );
    assert!(
        !request.timed_out,
        "query boundary did not terminate: status={} stdout={:?} stderr={:?}",
        request.status,
        String::from_utf8_lossy(&request.stdout),
        String::from_utf8_lossy(&request.stderr)
    );
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

    let endpoint = format!("http://{}", provider_listener.local_addr().unwrap());
    let named_rejections = [
        (
            format!(
                r#"[[credentials]]
name = "account-a"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
account = "a"
secret_ref = "env:ACCOUNT_A_KEY"

[credentials.audience]
family = "custom"
endpoint = "{endpoint}"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-4o"
base_url = "{endpoint}"
name = "missing"

[providers.credential]
credential_ref = "missing"
account = "a"
"#
            ),
            "missing credential 'missing'",
        ),
        (
            format!(
                r#"[[credentials]]
name = "account-a"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
account = "a"
secret_ref = "env:ACCOUNT_A_KEY"

[credentials.audience]
family = "custom"
endpoint = "{endpoint}"

[credentials.lifecycle]
state = "revoked"

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-4o"
base_url = "{endpoint}"
name = "revoked"

[providers.credential]
credential_ref = "account-a"
account = "a"
"#
            ),
            "is revoked",
        ),
        (
            format!(
                r#"[[credentials]]
name = "account-a"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
account = "a"
secret_ref = "env:ACCOUNT_A_KEY"

[credentials.audience]
family = "custom"
endpoint = "{endpoint}"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-4o"
base_url = "{endpoint}"
chat_path = "HTTPS://evil.example/v1/chat/completions"
name = "hostile-path"

[providers.credential]
credential_ref = "account-a"
account = "a"
"#
            ),
            "origin",
        ),
        (
            format!(
                r#"[[credentials]]
name = "account-a"
kind = "api_key"
provider = "openai_platform"
issuer = "openai-platform"
account = "a"
secret_ref = "env:ACCOUNT_A_KEY"

[credentials.audience]
family = "custom"
endpoint = "{endpoint}"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "credentialed"
provider = "openai_platform"
model = "gpt-4o"
base_url = "{endpoint}"
name = "wrong-account"

[providers.credential]
credential_ref = "account-a"
account = "b"
"#
            ),
            "incompatible credential",
        ),
    ];
    for (index, (provider_config, expected_error)) in named_rejections.iter().enumerate() {
        std::fs::write(
            finch_dir.join("config.toml"),
            format!(
                r#"{provider_config}

[client]
use_daemon = false
daemon_address = "http://{}"
auto_spawn = false
timeout_seconds = 1
auto_discover = false
prefer_local = true
"#,
                daemon_listener.local_addr().unwrap()
            ),
        )
        .unwrap();
        let mut command = Command::new(finch);
        base(&mut command);
        command.args(["--cloud-only", "query", "reject before external activity"]);
        let result = run_bounded_with_canary(command, &foreign_auth_canary);
        assert!(
            !result.timed_out,
            "named rejection {index} did not terminate"
        );
        assert!(
            !result.status.success(),
            "named rejection {index} succeeded"
        );
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains(expected_error),
            "named rejection {index}: expected {expected_error:?} in {stderr}"
        );
        assert_codex_was_not_executed(&marker, "named credential graph rejection");
        assert_no_connection(&provider_listener, "named credential graph rejection");
        assert_no_connection(&daemon_listener, "named credential graph rejection");
        assert_foreign_auth_was_not_read(
            &foreign_auth_canary,
            "named credential graph rejection",
            &result,
        );
    }

    let mut auth = Command::new(finch);
    base(&mut auth);
    auth.args(["auth", "status", "chatgpt"]);
    let auth = run_bounded_with_canary(auth, &foreign_auth_canary);
    assert!(!auth.timed_out, "local auth status did not terminate");
    assert!(auth.status.success());
    assert_eq!(
        String::from_utf8_lossy(&auth.stdout).trim(),
        "chatgpt credential=chatgpt:default status=signed_out"
    );
    assert_codex_was_not_executed(&marker, "local auth status");
    assert_no_connection(&provider_listener, "local auth status");
    assert_no_connection(&daemon_listener, "local auth status");
    assert_foreign_auth_was_not_read(&foreign_auth_canary, "local auth status", &auth);

    let mut setup = Command::new(finch);
    base(&mut setup);
    setup.arg("setup");
    let setup = run_bounded_with_canary(setup, &foreign_auth_canary);
    assert_codex_was_not_executed(&marker, "interactive setup startup");
    assert_no_connection(&provider_listener, "interactive setup startup");
    assert_no_connection(&daemon_listener, "interactive setup startup");
    assert_foreign_auth_was_not_read(&foreign_auth_canary, "interactive setup startup", &setup);
}
