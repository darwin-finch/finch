#![cfg(unix)]

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

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

fn supervised_process_boundary_available(boundary: &str) -> bool {
    match finch::brain::isolated_test_proof_if_present() {
        Ok(Some(_)) => true,
        Ok(None) => {
            eprintln!(
                "skipping {boundary}: process-boundary proof requires scripts/test_brains.sh"
            );
            false
        }
        Err(error) => {
            eprintln!("skipping {boundary}: invalid Brain test supervisor authority: {error}");
            false
        }
    }
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
    run_bounded_with_timeout_and_input(command, timeout, None)
}

fn run_bounded_with_timeout_and_input(
    command: &mut Command,
    timeout: std::time::Duration,
    input: Option<&[u8]>,
) -> BoundedOutput {
    let output_directory = tempfile::tempdir().unwrap();
    let stdout_path = output_directory.path().join("stdout");
    let stderr_path = output_directory.path().join("stderr");
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::from(std::fs::File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(std::fs::File::create(&stderr_path).unwrap()))
        .process_group(0);
    let mut child = command.spawn().unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("piped Finch child stdin")
            .write_all(input)
            .expect("write Finch child stdin");
    }
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

struct ProviderServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    accepted_connections: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ProviderServer {
    fn start(response_source: &'static str) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let accepted_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_requests = Arc::clone(&requests);
        let thread_accepted_connections = Arc::clone(&accepted_connections);
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !thread_stop.load(std::sync::atomic::Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("provider fixture accept failed: {error}"),
                };
                if thread_stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                thread_accepted_connections.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let request = read_http_request(&mut stream);
                thread_requests.lock().unwrap().push(request);
                let body = serde_json::json!({
                    "id": "chat-direct-test",
                    "object": "chat.completion",
                    "created": 1,
                    "model": "fixture-model",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": response_source},
                        "finish_reason": "stop"
                    }]
                })
                .to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .expect("write provider fixture response");
            }
        });
        Self {
            address,
            requests,
            accepted_connections,
            stop,
            thread: Some(thread),
        }
    }

    fn request_bodies(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn accepted_connections(&self) -> usize {
        self.accepted_connections
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("join provider fixture thread");
        }
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    let mut expected_len = None;
    loop {
        let read = stream.read(&mut chunk).expect("read provider request");
        assert!(
            read > 0,
            "provider request ended before its HTTP body arrived"
        );
        bytes.extend_from_slice(&chunk[..read]);
        if expected_len.is_none() {
            if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .expect("provider request has Content-Length");
                expected_len = Some(header_end + 4 + content_length);
            }
        }
        if expected_len.is_some_and(|expected| bytes.len() >= expected) {
            return String::from_utf8(bytes).expect("provider request is UTF-8");
        }
    }
}

fn run_bounded_with_canary(mut command: Command, canary: &ForeignAuthStoreCanary) -> BoundedOutput {
    let mut monitor = canary.start_monitor();
    let output = run_bounded_with_timeout(&mut command, std::time::Duration::from_secs(15));
    monitor.finish();
    output
}

fn run_bounded_with_canary_and_input(
    mut command: Command,
    canary: &ForeignAuthStoreCanary,
    input: Option<&[u8]>,
) -> BoundedOutput {
    let mut monitor = canary.start_monitor();
    let output =
        run_bounded_with_timeout_and_input(&mut command, std::time::Duration::from_secs(15), input);
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

fn daemon_connection_count(listener: &std::net::TcpListener) -> usize {
    let mut count = 0;
    loop {
        match listener.accept() {
            Ok(_) => count += 1,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return count,
            Err(error) => panic!("controlled daemon listener failed: {error}"),
        }
    }
}

fn install_external_provider_canaries(bin_dir: &std::path::Path, marker: &std::path::Path) {
    for binary in ["codex", "claude"] {
        let path = bin_dir.join(binary);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nprintf executed > '{}'\nexit 99\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn controlled_provider_config(
    provider_address: std::net::SocketAddr,
    daemon_address: std::net::SocketAddr,
) -> String {
    format!(
        r#"[[credentials]]
name = "fixture-key"
kind = "api_key"
provider = "openai_compatible"
issuer = "openai-compatible"
secret_ref = "env:FIXTURE_PROVIDER_KEY"

[credentials.audience]
family = "custom"
endpoint = "http://{provider_address}"

[credentials.lifecycle]
state = "active"
refreshable = false

[[providers]]
type = "openai_compatible"
name = "fixture"
base_url = "http://{provider_address}"
chat_path = "/v1/chat/completions"
models_path = "/v1/models"
model = "fixture-model"
tool_choice = "auto"
strict_tool_schemas = false

[providers.credential]
credential_ref = "fixture-key"

[providers.capabilities]
streaming = false
tools = true
parallel_tool_calls = false
image_input = false
context_window_tokens = 262144
max_output_tokens = 32768

[client]
use_daemon = true
daemon_address = "{daemon_address}"
auto_spawn = false
timeout_seconds = 1
auto_discover = false
prefer_local = true
"#,
    )
}

fn assert_direct_query_boundary(
    arguments: &[&str],
    input: Option<&[u8]>,
    expects_daemon_acquisition: bool,
    boundary: &str,
) {
    const WIRE_OUTPUT: &str = "direct-wire-executed-1457";
    const DAEMON_ACQUISITION_DIAGNOSTIC: &str = "daemon lifecycle gate";

    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    let bin_dir = directory.path().join("bin");
    let finch_dir = home.join(".finch");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&finch_dir).unwrap();

    let external_binary_marker = directory.path().join("external-provider-executed");
    install_external_provider_canaries(&bin_dir, &external_binary_marker);

    let foreign_auth_canary = ForeignAuthStoreCanary::start(&home);
    let provider = ProviderServer::start("(say \"direct-wire-executed-1457\")");
    let daemon_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    daemon_listener.set_nonblocking(true).unwrap();
    std::fs::write(
        finch_dir.join("config.toml"),
        controlled_provider_config(provider.address, daemon_listener.local_addr().unwrap()),
    )
    .unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
    remove_ambient_provider_environment(&mut command);
    command
        .args(arguments)
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("PATH", &bin_dir)
        .env("FIXTURE_PROVIDER_KEY", "fixture-key-secret")
        .env_remove("CODEX_HOME")
        .env_remove("FINCH_LIVE_CHATGPT_APP_SERVER");
    let output = run_bounded_with_canary_and_input(command, &foreign_auth_canary, input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.timed_out && output.status.success(),
        "{boundary} must execute one valid provider VM-wire response and exit cleanly: \
         status={} timed_out={} stdout={stdout:?} stderr={stderr:?}",
        output.status,
        output.timed_out,
    );
    assert_eq!(
        stdout.matches(WIRE_OUTPUT).count(),
        1,
        "{boundary} must execute and present the provider VM wire exactly once: \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        stderr.contains(DAEMON_ACQUISITION_DIAGNOSTIC),
        expects_daemon_acquisition,
        "{boundary} daemon-acquisition behavior changed: expected_attempt={} \
         stdout={stdout:?} stderr={stderr:?}",
        expects_daemon_acquisition,
    );
    assert_eq!(
        daemon_connection_count(&daemon_listener),
        0,
        "{boundary} must not escape the supervisor's daemon gate to the controlled listener: \
         stdout={stdout:?} stderr={stderr:?}"
    );

    let requests = provider.request_bodies();
    assert_eq!(
        requests.len(),
        1,
        "{boundary} must issue exactly one configured direct cloud request: \
         requests={requests:#?} stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        requests[0].starts_with("POST /v1/chat/completions HTTP/1.1\r\n")
            && requests[0]
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-key-secret")
            && requests[0].contains("fixture-model"),
        "{boundary} did not use the configured provider endpoint/key/model: request={:?}",
        requests[0]
    );
    assert!(
        !external_binary_marker.exists(),
        "{boundary} executed an ambient external-provider binary"
    );
    assert_foreign_auth_was_not_read(&foreign_auth_canary, boundary, &output);
    assert!(
        !finch_dir.join("brains").exists(),
        "{boundary} created named-Brain state under {}",
        finch_dir.join("brains").display()
    );
    assert!(
        !stderr.contains("Running first-time setup wizard")
            && !stderr.contains("Approve")
            && !directory.path().join("tool-effect").exists(),
        "{boundary} surfaced a prompt or tool effect: stdout={stdout:?} stderr={stderr:?}"
    );
}

fn assert_blank_query_has_no_external_or_durable_effects(
    arguments: &[&str],
    input: Option<&[u8]>,
    expects_usage_error: bool,
    boundary: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    let bin_dir = directory.path().join("bin");
    let finch_dir = home.join(".finch");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&finch_dir).unwrap();

    let external_binary_marker = directory.path().join("external-provider-executed");
    install_external_provider_canaries(&bin_dir, &external_binary_marker);

    let foreign_auth_canary = ForeignAuthStoreCanary::start(&home);
    let provider = ProviderServer::start("(say \"blank-query-reached-provider\")");
    let daemon_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    daemon_listener.set_nonblocking(true).unwrap();
    let config_path = finch_dir.join("config.toml");
    let config =
        controlled_provider_config(provider.address, daemon_listener.local_addr().unwrap());
    std::fs::write(&config_path, &config).unwrap();

    let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
    remove_ambient_provider_environment(&mut command);
    command
        .args(arguments)
        .current_dir(&workspace)
        .env("HOME", &home)
        .env("PATH", &bin_dir)
        .env("FIXTURE_PROVIDER_KEY", "fixture-key-secret")
        .env_remove("CODEX_HOME")
        .env_remove("FINCH_LIVE_CHATGPT_APP_SERVER");
    let output = run_bounded_with_canary_and_input(command, &foreign_auth_canary, input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.timed_out,
        "{boundary} must terminate without provider or daemon work: status={} stdout={stdout:?} stderr={stderr:?}",
        output.status,
    );
    if expects_usage_error {
        assert!(
            !output.status.success()
                && stderr.contains("query must contain at least one non-whitespace character"),
            "{boundary} must return an actionable nonzero usage error: status={} stdout={stdout:?} stderr={stderr:?}",
            output.status,
        );
    } else {
        assert!(
            output.status.success() && stdout.is_empty() && stderr.is_empty(),
            "{boundary} must preserve blank-piped-input silent success: status={} stdout={stdout:?} stderr={stderr:?}",
            output.status,
        );
    }
    assert_eq!(
        provider.accepted_connections(),
        0,
        "{boundary} must make zero provider connections: stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        daemon_connection_count(&daemon_listener),
        0,
        "{boundary} must make zero daemon connections: stdout={stdout:?} stderr={stderr:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        config,
        "{boundary} must not modify configuration state"
    );
    assert!(
        !external_binary_marker.exists()
            && !finch_dir.join("brains").exists()
            && !finch_dir.join("tool_patterns.json").exists()
            && !finch_dir.join("source-index").exists()
            && !directory.path().join("tool-effect").exists(),
        "{boundary} produced an external-provider, Brain, query-tool, source-index, or tool effect: home_entries={:?} stdout={stdout:?} stderr={stderr:?}",
        std::fs::read_dir(&finch_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>()
    );
    assert_foreign_auth_was_not_read(&foreign_auth_canary, boundary, &output);
}

#[test]
fn test_explicit_blank_queries_fail_before_external_or_durable_effects() {
    if !supervised_process_boundary_available("explicit blank one-shot query") {
        return;
    }
    for query in ["", "\u{2003}\t\u{00a0}\n"] {
        assert_blank_query_has_no_external_or_durable_effects(
            &["--direct", "query", query],
            None,
            true,
            &format!("explicit one-shot query {query:?}"),
        );
    }
}

#[test]
fn test_blank_piped_query_remains_silent_success_without_effects() {
    if !supervised_process_boundary_available("blank piped one-shot query") {
        return;
    }
    assert_blank_query_has_no_external_or_durable_effects(
        &["--direct"],
        Some("\u{2003}\t\u{00a0}\n".as_bytes()),
        false,
        "blank piped one-shot query",
    );
}

#[test]
fn test_direct_query_bypasses_daemon_and_executes_one_cloud_wire_response() {
    if !supervised_process_boundary_available("finch --direct query") {
        return;
    }
    assert_direct_query_boundary(
        &[
            "--direct",
            "query",
            "answer through the controlled provider",
        ],
        None,
        false,
        "finch --direct query",
    );
}

#[test]
fn test_piped_direct_bypasses_daemon_and_executes_one_cloud_wire_response() {
    if !supervised_process_boundary_available("piped finch --direct") {
        return;
    }
    assert_direct_query_boundary(
        &["--direct"],
        Some(b"answer through the controlled provider\n"),
        false,
        "piped finch --direct",
    );
}

#[test]
fn test_cloud_only_remains_daemon_free_and_direct() {
    if !supervised_process_boundary_available("finch --cloud-only query") {
        return;
    }
    assert_direct_query_boundary(
        &[
            "--cloud-only",
            "query",
            "answer through the controlled provider",
        ],
        None,
        false,
        "finch --cloud-only query",
    );
}

#[test]
fn test_ordinary_query_remains_daemon_first() {
    if !supervised_process_boundary_available("ordinary finch query") {
        return;
    }
    assert_direct_query_boundary(
        &["query", "answer through the controlled provider"],
        None,
        true,
        "ordinary finch query",
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
    if !supervised_process_boundary_available("bounded descendant cleanup") {
        return;
    }
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
    if !supervised_process_boundary_available("foreign auth-store read probe") {
        return;
    }
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
    if !supervised_process_boundary_available("foreign auth-store unwind cleanup") {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let canary = ForeignAuthStoreCanary::start(&directory.path().join("home"));
    let monitor = canary.start_monitor();
    let monitor_pid = nix::unistd::Pid::from_raw(monitor.child.as_ref().unwrap().id() as i32);

    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _monitor = monitor;
        panic!("exercise monitor unwind cleanup");
    }));

    assert!(unwind.is_err(), "monitor cleanup probe did not unwind");
    // Reaping is proven by parentage, not by signal: after the monitor's Drop
    // killed and waited the child, `waitpid` on that pid from this process
    // must answer ECHILD. A bare `kill(pid, None)` expecting ESRCH races PID
    // recycling on loaded CI runners — a recycled pid belongs to an unrelated
    // live process, so the probe can succeed even though the check means
    // nothing, or fail while cleanup actually worked. ECHILD cannot be fooled
    // by recycling: only "this pid is no longer our child" answers it.
    assert!(
        matches!(
            nix::sys::wait::waitpid(monitor_pid, Some(nix::sys::wait::WaitPidFlag::WNOHANG)),
            Err(nix::errno::Errno::ECHILD)
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
fn test_missing_configuration_guidance_is_plain_on_redirected_stderr() {
    if !supervised_process_boundary_available("missing-configuration diagnostic") {
        return;
    }

    let home = tempfile::tempdir().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
    remove_ambient_provider_environment(&mut command);
    command
        .env("HOME", home.path())
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .args(["query", "show the missing configuration diagnostic"]);

    let output = run_bounded_with_timeout(&mut command, std::time::Duration::from_secs(15));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.timed_out,
        "missing-configuration Finch process did not terminate: status={} stdout={:?} stderr={stderr:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        !output.status.success(),
        "missing-configuration Finch process unexpectedly succeeded: status={} stdout={:?} stderr={stderr:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        output.stdout.is_empty(),
        "missing-configuration Finch process wrote to redirected stdout: status={} stdout={:?} stderr={stderr:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
    );
    assert!(
        stderr.contains("finch setup"),
        "missing-configuration stderr lost its actionable setup command: status={} stderr={stderr:?}",
        output.status,
    );
    assert!(
        !output.stderr.contains(&0x1b) && !output.stderr.contains(&0x9b),
        "missing-configuration stderr contained ANSI styling control bytes despite redirected stderr, TERM=dumb, and NO_COLOR=1: status={} stderr_bytes={:?}",
        output.status,
        output.stderr,
    );
}

#[test]
fn test_hostile_codex_on_path_is_never_spawned_by_cli_boundaries() {
    if !supervised_process_boundary_available("external-provider binary boundaries") {
        return;
    }
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
