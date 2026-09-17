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
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_DEADLINE: Duration = Duration::from_secs(90);
const ECHO_DEADLINE: Duration = Duration::from_secs(60);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
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
