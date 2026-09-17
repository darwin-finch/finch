//! Named-Brain attach/resume is the sole active durable conversation identity (#314).
//!
//! `Repl` takes the interactive branch only when stdout `is_terminal()`, so the
//! exit-instruction and filesystem proofs spawn the real `finch` binary on a
//! pty. The child is a plain `Command` spawn: it creates no session or process
//! group, and `Session::drop` kills and reaps only the child it started.
//!
//! Deadlines are hang detectors, not latency assertions.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_DEADLINE: Duration = Duration::from_secs(90);
const ECHO_DEADLINE: Duration = Duration::from_secs(60);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
const DAEMON_READY_DEADLINE: Duration = Duration::from_secs(30);
const ROWS: u16 = 24;
const COLS: u16 = 100;
const SUPERVISOR_AUTHORITY_FDS: &[i32] = &[9, 10, 11, 12, 108, 109, 110, 111, 112];
const BRAIN: &str = "golden-ridge-0771a6";
const LEFTOVER_UUID: &str = "2fdae496-60c2-41b1-a901-857af8f0ed82";

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("disposable HOME");
        let home = temp.path().to_path_buf();
        let finch = home.join(".finch");
        std::fs::create_dir_all(finch.join("brains")).expect("create .finch/brains");
        std::fs::create_dir_all(finch.join("sessions")).expect("create leftover sessions dir");
        std::fs::write(
            finch.join("sessions").join(format!("{LEFTOVER_UUID}.json")),
            r#"{"messages":[]}"#,
        )
        .expect("seed leftover UUID session");
        std::fs::write(
            finch.join("config.toml"),
            r#"[[providers]]
type = "grok"
api_key = "not-a-real-key"
model = "grok-code-fast-1"
base_url = "http://127.0.0.1:1"
name = "attach-fixture"

[client]
use_daemon = false
daemon_address = "http://127.0.0.1:1"
auto_spawn = false
timeout_seconds = 1
auto_discover = false
prefer_local = true
"#,
        )
        .expect("seed config.toml");
        Self { _temp: temp, home }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.home.join(".finch/sessions")
    }

    fn leftover_session(&self) -> PathBuf {
        self.sessions_dir().join(format!("{LEFTOVER_UUID}.json"))
    }
}

struct Session {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl Session {
    fn spawn(fixture: &Fixture, args: &[&str]) -> Self {
        Self::spawn_on(&fixture.home, args)
    }

    fn spawn_on(home: &Path, args: &[&str]) -> Self {
        let winsize = nix::pty::Winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(&winsize, None).expect("openpty");
        let slave_in = pty.slave.try_clone().expect("clone slave for stdin");
        let slave_out = pty.slave.try_clone().expect("clone slave for stdout");
        let slave_err = pty.slave.try_clone().expect("clone slave for stderr");

        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .args(args)
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err))
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .env("HF_HOME", home.join(".cache/huggingface"))
            .env("TERM", "xterm-256color")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("XAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("SHAMMAH_LOG")
            .env_remove("RUST_LOG");
        isolate_from_supervisor(&mut command);

        let child = command.spawn().expect("spawn finch under a pty");
        drop(pty.slave);
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let master = pty.master;
        let read_fd = master.try_clone().expect("clone master for reader");
        let sink = std::sync::Arc::clone(&transcript);
        let (done_tx, reader_done) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut file = std::fs::File::from(read_fd);
            let mut buffer = [0u8; 8192];
            loop {
                match file.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => sink
                        .lock()
                        .expect("transcript poisoned")
                        .extend_from_slice(&buffer[..read]),
                }
            }
            let _ = done_tx.send(());
        });
        Self {
            child,
            master,
            transcript,
            reader: Some(reader),
            reader_done,
        }
    }

    fn readable_transcript(&self) -> String {
        let raw = String::from_utf8_lossy(&self.transcript_bytes()).into_owned();
        let mut stripped = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '\u{1b}' {
                stripped.push(character);
                continue;
            }
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for byte in chars.by_ref() {
                        if byte.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for byte in chars.by_ref() {
                        if byte == '\u{7}' || byte == '\n' {
                            break;
                        }
                    }
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        }
        stripped
    }

    fn transcript_bytes(&self) -> Vec<u8> {
        self.transcript.lock().expect("transcript poisoned").clone()
    }

    fn wait_for(&mut self, needle: &str, deadline: Duration, what: &str) {
        let expiry = Instant::now() + deadline;
        loop {
            if self.readable_transcript().contains(needle) {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "INVARIANT: {what}\nfinch exited with {status:?} before that happened.\n\
                     needle={needle:?}\nreadable terminal:\n{}",
                    self.readable_transcript()
                );
            }
            if Instant::now() >= expiry {
                panic!(
                    "the run hung: {what} did not happen within {deadline:?} \
                     ({needle:?} never reached the terminal). This deadline is a hang \
                     detector, not a latency assertion. Terminal was:\n{}",
                    self.readable_transcript()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn send_line(&mut self, line: &str) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone master for writing"));
        write!(file, "{line}\r").expect("write to the pty");
        file.flush().expect("flush the pty");
    }

    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let expiry = Instant::now() + EXIT_DEADLINE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {}
                Err(error) => panic!("could not wait for finch: {error}"),
            }
            if Instant::now() >= expiry {
                let transcript = self.readable_transcript();
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "the run hung: finch did not exit within {EXIT_DEADLINE:?} after \
                     `/exit`. This deadline is a hang detector, not a latency \
                     assertion. Terminal was:\n{transcript}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader_done.recv_timeout(Duration::from_secs(5));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn isolate_from_supervisor(command: &mut Command) {
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("FINCH_BRAIN_TEST_") || name_text.starts_with("FINCH_TEST_") {
            command.env_remove(name);
        }
    }
    command.env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1");
    unsafe {
        command.pre_exec(|| {
            for fd in SUPERVISOR_AUTHORITY_FDS {
                nix::libc::close(*fd);
            }
            Ok(())
        });
    }
}

/// User-shaped daemon for the durable attach/reattach PTY case.
///
/// The daemon and the interactive children are isolated from supervisor
/// authority the same way a real user process is: disposable HOME, no
/// inherited proof, `auto_spawn = false`, and a loopback bind. The daemon
/// owns `~/.finch/daemon.sock`; the CLI connects there without a symlink.
struct IsolatedDaemon {
    child: Child,
    _temp: tempfile::TempDir,
    home: PathBuf,
    address: String,
}

impl IsolatedDaemon {
    fn start() -> Self {
        // macOS sockaddr_un is 104 bytes. Nested supervisor TMPDIRs overflow
        // `~/.finch/daemon.sock`, so this HOME lives directly under /tmp.
        let parent = if Path::new("/private/tmp").is_dir() {
            Path::new("/private/tmp")
        } else {
            Path::new("/tmp")
        };
        let temp = tempfile::Builder::new()
            .prefix("fa.")
            .tempdir_in(parent)
            .expect("short disposable HOME for durable attach");
        let home = temp.path().to_path_buf();
        let socket = home.join(".finch/daemon.sock");
        require_bounded_unix_socket_path(&socket);
        let finch_dir = home.join(".finch");
        std::fs::create_dir_all(finch_dir.join("brains")).expect("create isolated brains dir");
        std::fs::create_dir_all(finch_dir.join("sessions")).expect("create leftover sessions dir");
        std::fs::create_dir_all(home.join("tmp")).expect("create isolated tmp");
        std::fs::write(
            finch_dir
                .join("sessions")
                .join(format!("{LEFTOVER_UUID}.json")),
            r#"{"messages":[]}"#,
        )
        .expect("seed leftover UUID session");
        write_attach_config(&home, "127.0.0.1:1", "attach-fixture-password");

        let stderr_path = finch_dir.join("attach-daemon.stderr");
        let stderr_file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(&stderr_path)
            .expect("open daemon stderr");
        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .arg("daemon")
            .arg("--bind")
            .arg("127.0.0.1:0")
            .env("HOME", &home)
            .env("TMPDIR", home.join("tmp"))
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_CACHE_HOME", home.join(".cache"))
            .env("XDG_DATA_HOME", home.join(".local/share"))
            .stdout(Stdio::null())
            .stderr(Stdio::from(
                stderr_file.try_clone().expect("clone daemon stderr"),
            ));
        isolate_from_supervisor(&mut command);
        let mut child = command.spawn().expect("spawn isolated Finch daemon");

        let log_path = finch_dir.join("daemon.log");
        let socket_path = finch_dir.join("daemon.sock");
        let expiry = Instant::now() + DAEMON_READY_DEADLINE;
        let address = loop {
            if let Ok(Some(status)) = child.try_wait() {
                let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                panic!(
                    "isolated daemon exited before becoming healthy: {status:?}; stderr={stderr}; log={log}"
                );
            }
            if let Some(address) = bound_address_from_log(&log_path) {
                if request_health(&address).is_ok() && socket_path.exists() {
                    break address;
                }
            }
            if Instant::now() >= expiry {
                let _ = child.kill();
                let _ = child.wait();
                let stderr = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                panic!(
                    "the run hung: isolated daemon did not become healthy within \
                     {DAEMON_READY_DEADLINE:?}. This deadline is a hang detector, not a \
                     latency assertion. parsed={:?} socket={} stderr={stderr} log={log}",
                    bound_address_from_log(&log_path),
                    socket_path.exists()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        write_attach_config(&home, &address, "attach-fixture-password");
        Self {
            child,
            _temp: temp,
            home,
            address,
        }
    }

    fn leftover_session(&self) -> PathBuf {
        self.home
            .join(".finch/sessions")
            .join(format!("{LEFTOVER_UUID}.json"))
    }

    fn brain_dir(&self) -> PathBuf {
        self.home.join(".finch/brains").join(BRAIN)
    }

    fn events_path(&self) -> PathBuf {
        self.brain_dir().join("events.jsonl")
    }

    fn metadata_path(&self) -> PathBuf {
        self.brain_dir().join("metadata.json")
    }
}

impl Drop for IsolatedDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn write_attach_config(home: &Path, daemon_address: &str, brain_password: &str) {
    let config = format!(
        r#"[[providers]]
type = "grok"
api_key = "not-a-real-key"
model = "grok-code-fast-1"
base_url = "http://127.0.0.1:1"
name = "attach-fixture"

[client]
use_daemon = true
daemon_address = {daemon_address:?}
auto_spawn = false
timeout_seconds = 5
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
service_name = "finch-attach-test"
service_description = "named-Brain attach fixture"
brain_password = {brain_password:?}
"#
    );
    std::fs::write(home.join(".finch/config.toml"), config).expect("write attach config.toml");
}

fn require_bounded_unix_socket_path(path: &Path) {
    use std::os::unix::ffi::OsStrExt as _;
    let address: nix::libc::sockaddr_un = unsafe { std::mem::zeroed() };
    assert!(
        path.as_os_str().as_bytes().len() < address.sun_path.len(),
        "daemon IPC path exceeds sockaddr_un: {} ({} bytes)",
        path.display(),
        path.as_os_str().as_bytes().len()
    );
}

fn bound_address_from_log(path: &Path) -> Option<String> {
    let log = std::fs::read_to_string(path).ok()?;
    let mut found = None;
    for line in log.lines() {
        let Some(rest) = line.split("Starting Finch agent server on ").nth(1) else {
            continue;
        };
        let token = rest.split_whitespace().next()?.trim().trim_end_matches(',');
        let Ok(addr) = token.parse::<std::net::SocketAddr>() else {
            continue;
        };
        if addr.port() == 0 {
            continue;
        }
        found = Some(addr.to_string());
    }
    found
}

fn request_health(address: &str) -> Result<(), String> {
    let socket_address: std::net::SocketAddr = address
        .parse()
        .map_err(|error| format!("parse daemon address: {error}"))?;
    let timeout = Duration::from_millis(250);
    let mut stream = TcpStream::connect_timeout(&socket_address, timeout)
        .map_err(|error| format!("health connect: {error}"))?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| format!("health read timeout: {error}"))?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| format!("health write timeout: {error}"))?;
    write!(
        stream,
        "GET /health HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|error| format!("health write: {error}"))?;
    let mut response = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .map_err(|error| format!("health read: {error}"))?;
    if !response.starts_with(b"HTTP/1.1 200 ") {
        return Err(format!(
            "health status was not 200: {}",
            String::from_utf8_lossy(&response)
        ));
    }
    Ok(())
}

fn brain_id_from_metadata(path: &Path) -> String {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "INVARIANT: a durable attach must write metadata.json at {} ({error})",
            path.display()
        )
    });
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|error| {
        panic!(
            "INVARIANT: metadata.json must be JSON at {}: {error}; raw={raw}",
            path.display()
        )
    });
    value
        .get("brain_id")
        .and_then(|value| value.as_str())
        .unwrap_or_else(|| {
            panic!(
                "INVARIANT: metadata.json must name brain_id at {}; raw={raw}",
                path.display()
            )
        })
        .to_string()
}

fn named_brain_dirs(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".finch/brains");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

fn printed_attach_command(text: &str) -> String {
    let needle = "To resume, run: finch attach ";
    let line = text
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| {
            panic!(
                "INVARIANT: a durable interactive exit must print one copyable attach command.\n\
             terminal:\n{text}"
            )
        });
    let name = line
        .split(needle)
        .nth(1)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            panic!("attach instruction had no Brain name: {line:?}\nterminal:\n{text}")
        });
    format!("attach {name}")
}

fn session_json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

fn is_uuid_line(line: &str) -> bool {
    let line = line.trim();
    if line.len() != 36 {
        return false;
    }
    line.as_bytes()
        .iter()
        .enumerate()
        .all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn uuid_only_lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|line| is_uuid_line(line))
        .collect()
}

#[test]
fn attach_is_the_documented_top_level_command() {
    let help = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--help")
        .output()
        .expect("finch --help");
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.status.success(),
        "finch --help must succeed, stderr={}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        stdout.contains("attach"),
        "top-level help must list attach, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("--resume"),
        "top-level help must not advertise --resume, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("--restore-session"),
        "top-level help must not advertise --restore-session, got:\n{stdout}"
    );
    assert!(
        !stdout.contains("--brain"),
        "top-level help must not advertise the hidden --brain alias, got:\n{stdout}"
    );

    let attach_help = Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["attach", "--help"])
        .output()
        .expect("finch attach --help");
    let attach_out = String::from_utf8_lossy(&attach_help.stdout);
    assert!(
        attach_help.status.success(),
        "finch attach --help must succeed, stderr={}",
        String::from_utf8_lossy(&attach_help.stderr)
    );
    assert!(
        attach_out.contains("Brain") || attach_out.contains("brain"),
        "attach help must describe named-Brain resume, got:\n{attach_out}"
    );
}

#[test]
fn retired_resume_flags_fail_at_the_executable_boundary() {
    for args in [
        vec!["--resume", LEFTOVER_UUID],
        vec!["--restore-session", "/tmp/old.json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_finch"))
            .args(&args)
            .output()
            .unwrap_or_else(|error| panic!("finch {args:?} failed to spawn: {error}"));
        assert!(
            !output.status.success(),
            "retired {args:?} must not enter the REPL, status={:?}, stdout={}, stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("finch attach"),
            "retired {args:?} must name the replacement command, stderr={stderr}"
        );
        assert!(
            !stderr.contains("sessions/<uuid>"),
            "retired {args:?} must not teach UUID resume, stderr={stderr}"
        );
    }
}

#[test]
fn hostile_attach_name_is_rejected_at_the_executable_boundary() {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["attach", "evil; rm -rf /"])
        .output()
        .expect("finch attach hostile");
    assert!(
        !output.status.success(),
        "hostile Brain names must not start the REPL"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("brain name must use 1-64 letters, numbers, '-' or '_'"),
        "the executable must fail closed with validate_name's message, stderr={stderr}"
    );
}

#[test]
fn query_mode_does_not_emit_resume_prose() {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["query", "(say \"attach-query-silence\")"])
        .output()
        .expect("finch query");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}{stderr}");
    assert!(
        !combined.contains("To resume, run:"),
        "noninteractive query must not print conversational resume prose, got:\n{combined}"
    );
    assert!(
        uuid_only_lines(&combined).is_empty(),
        "noninteractive query must not print a UUID-only line, got:\n{combined}"
    );
}

#[test]
fn interactive_exit_prints_no_uuid_and_writes_no_session_json() {
    let fixture = Fixture::new();
    assert!(
        fixture.leftover_session().is_file(),
        "the leftover UUID file must exist before the run so preservation is observable"
    );

    let mut session = Session::spawn(&fixture, &["attach", BRAIN]);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    session.send_line("(+ 770011 0)");
    session.wait_for("770011", ECHO_DEADLINE, "the typed program was echoed");
    session.send_line("/exit");
    let status = session.wait_for_exit();
    assert!(
        status.success(),
        "a clean /exit must succeed, status={status:?}, terminal:\n{}",
        session.readable_transcript()
    );

    let text = session.readable_transcript();
    let uuid_lines = uuid_only_lines(&text);
    assert!(
        uuid_lines.is_empty(),
        "INVARIANT: a clean interactive exit must not print a UUID-only resume line.\n\
         uuid_lines={uuid_lines:?}\nterminal:\n{text}"
    );
    assert!(
        !text.contains("finch --resume"),
        "INVARIANT: exit guidance must not advertise UUID --resume.\nterminal:\n{text}"
    );

    let attach_hits = text
        .matches(&format!("To resume, run: finch attach {BRAIN}"))
        .count();
    let failure_hits = text
        .matches("This run was not saved as a named Brain and cannot be resumed.")
        .count();
    assert_eq!(
        attach_hits + failure_hits,
        1,
        "INVARIANT: exit prints exactly one truthful resume instruction or persistence failure.\n\
         attach_hits={attach_hits} failure_hits={failure_hits}\nterminal:\n{text}"
    );
    assert!(
        failure_hits == 1,
        "this fixture has no daemon, so it must not claim the Brain can be resumed.\nterminal:\n{text}"
    );

    let sessions = session_json_files(&fixture.sessions_dir());
    assert_eq!(
        sessions,
        vec![fixture.leftover_session()],
        "INVARIANT: Finch must not write a new UUID session file, and must preserve leftover files.\n\
         sessions={sessions:?}"
    );
}

#[test]
fn compatibility_brain_flag_still_starts_the_named_repl() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture, &["--brain", BRAIN]);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    session.wait_for(BRAIN, READY_DEADLINE, "the named Brain label was drawn");
    session.send_line("/exit");
    let status = session.wait_for_exit();
    assert!(
        status.success(),
        "--brain must remain a compatibility alias, status={status:?}, terminal:\n{}",
        session.readable_transcript()
    );
    assert_eq!(
        session_json_files(&fixture.sessions_dir()),
        vec![fixture.leftover_session()],
        "--brain must not write a new UUID session file"
    );
}

#[test]
fn durable_attach_prints_one_command_and_reattaches_the_same_brain() {
    let daemon = IsolatedDaemon::start();
    assert!(
        daemon.leftover_session().is_file(),
        "the leftover UUID file must exist before the run so preservation is observable"
    );
    assert!(
        daemon.home.join(".finch/daemon.sock").exists(),
        "the user CLI must reach the daemon at ~/.finch/daemon.sock"
    );

    let expected = format!("To resume, run: finch attach {BRAIN}");
    let mut first = Session::spawn_on(&daemon.home, &["attach", BRAIN]);
    first.wait_for(
        "finch v",
        READY_DEADLINE,
        "the first attach drew the startup header",
    );
    first.wait_for(
        BRAIN,
        READY_DEADLINE,
        "the first attach drew the named Brain",
    );
    first.send_line("(+ 770011 0)");
    first.wait_for(
        "770011",
        ECHO_DEADLINE,
        "the first session echoed the typed program",
    );
    first.send_line("/exit");
    let status = first.wait_for_exit();
    let first_text = first.readable_transcript();
    assert!(
        status.success(),
        "a clean /exit must succeed after a durable attach, status={status:?}, daemon={}, terminal:\n{first_text}",
        daemon.address
    );
    drop(first);

    let uuid_lines = uuid_only_lines(&first_text);
    assert!(
        uuid_lines.is_empty(),
        "INVARIANT: a clean interactive exit must not print a UUID-only resume line.\n\
         uuid_lines={uuid_lines:?}\nterminal:\n{first_text}"
    );
    assert!(
        !first_text.contains("finch --resume"),
        "INVARIANT: exit guidance must not advertise UUID --resume.\nterminal:\n{first_text}"
    );
    assert_eq!(
        first_text.matches(&expected).count(),
        1,
        "INVARIANT: a durable exit prints exactly one copyable attach command.\nterminal:\n{first_text}"
    );
    assert!(
        !first_text.contains("This run was not saved as a named Brain"),
        "a live daemon attach must not claim the Brain cannot be resumed.\nterminal:\n{first_text}"
    );
    let printed = printed_attach_command(&first_text);
    assert_eq!(
        printed,
        format!("attach {BRAIN}"),
        "the printed command must be copyable as `finch attach {BRAIN}`"
    );
    assert_eq!(
        session_json_files(&daemon.home.join(".finch/sessions")),
        vec![daemon.leftover_session()],
        "INVARIANT: Finch must not write a new UUID session file, and must preserve leftover files."
    );

    let first_brain_id = brain_id_from_metadata(&daemon.metadata_path());
    let first_events = std::fs::read(daemon.events_path()).unwrap_or_else(|error| {
        panic!(
            "INVARIANT: a durable attach must write events.jsonl at {} ({error})",
            daemon.events_path().display()
        )
    });
    assert!(
        !first_events.is_empty(),
        "INVARIANT: attaching a named Brain must commit at least one durable event"
    );
    assert!(
        daemon.brain_dir().is_dir(),
        "the first attach must create the named Brain directory {}",
        daemon.brain_dir().display()
    );
    assert!(
        named_brain_dirs(&daemon.home).contains(&daemon.brain_dir()),
        "the first attach must leave the named Brain in the store"
    );

    let mut second = Session::spawn_on(
        &daemon.home,
        &printed.split_whitespace().collect::<Vec<_>>(),
    );
    second.wait_for(
        "finch v",
        READY_DEADLINE,
        "the printed attach command drew the startup header",
    );
    second.wait_for(
        BRAIN,
        READY_DEADLINE,
        "the printed attach command reopened the same named Brain",
    );
    second.send_line("/exit");
    let status = second.wait_for_exit();
    let second_text = second.readable_transcript();
    assert!(
        status.success(),
        "reattach /exit must succeed, status={status:?}, terminal:\n{second_text}"
    );
    drop(second);

    assert_eq!(
        brain_id_from_metadata(&daemon.metadata_path()),
        first_brain_id,
        "INVARIANT: finch attach {BRAIN} must reopen the same Brain ID, not mint a duplicate"
    );
    let second_events = std::fs::read(daemon.events_path()).expect("reread events.jsonl");
    assert!(
        second_events.starts_with(&first_events),
        "INVARIANT: reattach must append to the canonical journal, not duplicate or rewrite the first session.\n\
         first_len={} second_len={}",
        first_events.len(),
        second_events.len()
    );
    assert!(
        named_brain_dirs(&daemon.home).contains(&daemon.brain_dir()),
        "reattach must not replace the named Brain with a different directory"
    );
    assert_eq!(
        session_json_files(&daemon.home.join(".finch/sessions")),
        vec![daemon.leftover_session()],
        "reattach must not write a new UUID session file"
    );
    assert_eq!(
        second_text.matches(&expected).count(),
        1,
        "reattach exit must still print one attach command.\nterminal:\n{second_text}"
    );
    assert!(
        uuid_only_lines(&second_text).is_empty(),
        "reattach must not print a UUID-only resume line.\nterminal:\n{second_text}"
    );
}
