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
    rows: u16,
    cols: u16,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl Session {
    fn spawn(fixture: &Fixture, args: &[&str]) -> Self {
        Self::spawn_on(&fixture.home, args)
    }

    fn spawn_on(home: &Path, args: &[&str]) -> Self {
        Self::spawn_sized(home, args, ROWS, COLS)
    }

    /// Spawn with an explicit grid size. A shorter terminal keeps the native
    /// scrollback off the visible grid, so a viewport-tail assertion can read
    /// the live card without the canonical rows above it.
    fn spawn_sized(home: &Path, args: &[&str], rows: u16, cols: u16) -> Self {
        let winsize = nix::pty::Winsize {
            ws_row: rows,
            ws_col: cols,
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
            rows,
            cols,
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

    /// Write raw bytes to the pty without a trailing newline — escape
    /// sequences must not gain an Enter keystroke.
    fn send_raw(&mut self, bytes: &[u8]) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone master for writing"));
        file.write_all(bytes).expect("write to the pty");
        file.flush().expect("flush the pty");
    }

    /// The live screen the reader sees right now: the raw byte stream replayed
    /// through a VT parser over a fixed `ROWS`×`COLS` grid. Native scrollback
    /// that already left the viewport is off-grid, exactly as a human reader
    /// experiences the session. Repaints (absolute cursor moves, erases) are
    /// honoured, so this is the production surface, not the byte stream.
    fn screen_text(&self) -> String {
        let mut screen = vt::Screen::new(self.rows, self.cols);
        let mut parser = vte::Parser::new();
        let bytes = self.transcript_bytes();
        parser.advance(&mut screen, &bytes);
        screen.text()
    }

    /// The native scrollback a real terminal would hold above the live area:
    /// the raw byte stream replayed through the same VT parser, with every
    /// row that scrolled off the grid top retained. Rows in scrollback were
    /// written exactly once — a row can never be erased after it scrolls.
    fn scrollback_text(&self) -> String {
        let mut screen = vt::Screen::new(self.rows, self.cols);
        let mut parser = vte::Parser::new();
        let bytes = self.transcript_bytes();
        parser.advance(&mut screen, &bytes);
        screen.scrollback_text()
    }

    /// Wait until the live screen carries the needle, then return the screen.
    fn wait_for_screen(&mut self, needle: &str, deadline: Duration, what: &str) -> String {
        let expiry = Instant::now() + deadline;
        loop {
            let screen = self.screen_text();
            if screen.contains(needle) {
                return screen;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "INVARIANT: {what}\nfinch exited with {status:?} before that happened.\n\
                     needle={needle:?}\nlive screen:\n{screen}"
                );
            }
            if Instant::now() >= expiry {
                panic!(
                    "the run hung: {what} did not happen within {deadline:?} \
                     ({needle:?} never reached the live screen). This deadline is a \
                     hang detector, not a latency assertion. Live screen was:\n{screen}"
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Bounded poll over the live screen for a predicate. `None` when the
    /// deadline passes first — a liveness probe, not a latency assertion.
    fn wait_for_screen_pred<F>(&mut self, mut predicate: F, deadline: Duration) -> Option<String>
    where
        F: FnMut(&str) -> bool,
    {
        let expiry = Instant::now() + deadline;
        loop {
            let screen = self.screen_text();
            if predicate(&screen) {
                return Some(screen);
            }
            if Instant::now() >= expiry {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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

/// A minimal VT screen model: a fixed grid the raw PTY byte stream repaints
/// through `vte`. It exists so assertions can read what a human reader sees —
/// the live screen — instead of the byte stream that also carries erased
/// repaints and scrolled-away history. Honoured: printable output, CR/LF/BS,
/// absolute and relative cursor moves, line/display erases, and scrolling at
/// the bottom margin. SGR, OSC, and private modes are ignored, matching the
/// assertions' needs (text presence and absence).
mod vt {
    use vte::{Params, Perform};

    pub struct Screen {
        rows: usize,
        cols: usize,
        grid: Vec<Vec<char>>,
        /// Rows pushed off the grid top by scrolling — a real terminal's
        /// native scrollback. Once a row leaves the grid it can never be
        /// erased again: erases and repaints only ever touch the grid.
        scrollback: Vec<Vec<char>>,
        row: usize,
        col: usize,
        pending_wrap: bool,
    }

    impl Screen {
        pub fn new(rows: u16, cols: u16) -> Self {
            let (rows, cols) = (rows as usize, cols as usize);
            Self {
                grid: vec![vec![' '; cols]; rows],
                scrollback: Vec::new(),
                rows,
                cols,
                row: 0,
                col: 0,
                pending_wrap: false,
            }
        }

        pub fn text(&self) -> String {
            self.grid
                .iter()
                .map(|line| {
                    let text = line.iter().collect::<String>();
                    text.trim_end().to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        /// The native scrollback this session accumulated: every row a real
        /// terminal would hold above the live area, exactly as it was when it
        /// scrolled off the grid top.
        pub fn scrollback_text(&self) -> String {
            self.scrollback
                .iter()
                .map(|line| {
                    let text = line.iter().collect::<String>();
                    text.trim_end().to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        fn line_feed(&mut self) {
            self.pending_wrap = false;
            if self.row + 1 >= self.rows {
                self.scroll_up();
            } else {
                self.row += 1;
            }
        }

        fn scroll_up(&mut self) {
            let top = self.grid.remove(0);
            self.scrollback.push(top);
            self.grid.push(vec![' '; self.cols]);
        }

        fn scroll_down(&mut self) {
            self.grid.pop();
            self.grid.insert(0, vec![' '; self.cols]);
        }

        fn put_char(&mut self, c: char) {
            if self.pending_wrap {
                self.col = 0;
                self.line_feed();
            }
            self.grid[self.row][self.col] = c;
            if self.col + 1 >= self.cols {
                self.pending_wrap = true;
            } else {
                self.col += 1;
            }
        }

        fn move_to(&mut self, row: usize, col: usize) {
            self.pending_wrap = false;
            self.row = row.min(self.rows - 1);
            self.col = col.min(self.cols - 1);
        }

        fn erase_in_line(&mut self, mode: u16) {
            let line = &mut self.grid[self.row];
            match mode {
                0 => {
                    for cell in line.iter_mut().skip(self.col) {
                        *cell = ' ';
                    }
                }
                1 => {
                    for cell in line.iter_mut().take(self.col + 1) {
                        *cell = ' ';
                    }
                }
                _ => {
                    for cell in line.iter_mut() {
                        *cell = ' ';
                    }
                }
            }
        }

        fn erase_in_display(&mut self, mode: u16) {
            match mode {
                0 => {
                    self.erase_in_line(0);
                    for line in self.grid.iter_mut().skip(self.row + 1) {
                        *line = vec![' '; self.cols];
                    }
                }
                1 => {
                    self.erase_in_line(1);
                    for line in self.grid.iter_mut().take(self.row) {
                        *line = vec![' '; self.cols];
                    }
                }
                _ => {
                    for line in self.grid.iter_mut() {
                        *line = vec![' '; self.cols];
                    }
                }
            }
        }
    }

    fn param(params: &Params, index: usize, default: u16) -> u16 {
        params
            .iter()
            .nth(index)
            .and_then(|slice| slice.first())
            .copied()
            .filter(|value| *value != 0)
            .unwrap_or(default)
    }

    impl Perform for Screen {
        fn print(&mut self, c: char) {
            self.put_char(c);
        }

        fn execute(&mut self, byte: u8) {
            match byte {
                b'\r' => {
                    self.pending_wrap = false;
                    self.col = 0;
                }
                b'\n' => self.line_feed(),
                b'\x08' => {
                    self.pending_wrap = false;
                    self.col = self.col.saturating_sub(1);
                }
                b'\t' => {
                    self.col = ((self.col / 8) + 1).min(self.cols - 1) * 8;
                    self.col = self.col.min(self.cols - 1);
                }
                _ => {}
            }
        }

        fn csi_dispatch(
            &mut self,
            params: &Params,
            _intermediates: &[u8],
            _ignore: bool,
            action: char,
        ) {
            match action {
                'H' | 'f' => {
                    // CUP is 1-based; the TUI positions rows absolutely.
                    self.move_to(
                        (param(params, 0, 1).saturating_sub(1)).into(),
                        (param(params, 1, 1).saturating_sub(1)).into(),
                    );
                }
                'J' => self.erase_in_display(param(params, 0, 0)),
                'K' => self.erase_in_line(param(params, 0, 0)),
                'A' => {
                    self.move_to(
                        self.row.saturating_sub(param(params, 0, 1) as usize),
                        self.col,
                    );
                }
                'B' => self.move_to(self.row + param(params, 0, 1) as usize, self.col),
                'C' => self.move_to(self.row, self.col + param(params, 0, 1) as usize),
                'D' => {
                    self.move_to(
                        self.row,
                        self.col.saturating_sub(param(params, 0, 1) as usize),
                    );
                }
                _ => {}
            }
        }

        fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, action: u8) {
            match action {
                b'D' | b'E' => self.line_feed(),
                b'M' => {
                    if self.row == 0 {
                        self.scroll_down();
                    } else {
                        self.row -= 1;
                    }
                }
                _ => {}
            }
        }

        fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
        fn put(&mut self, _byte: u8) {}
        fn unhook(&mut self) {}
        fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {}
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

#[test]
fn completed_typed_program_turn_spools_its_canonical_record_into_native_scrollback() {
    const SAY_TEXT: &str = "attach-say-935";
    const SOURCE_LINE: &str = "(say \"attach-say-935\")";
    const CANONICAL_SOURCE_LABEL: &str = "Program source (lisp)";

    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture, &["attach", BRAIN]);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    session.send_line(SOURCE_LINE);
    session.wait_for_screen(
        SAY_TEXT,
        ECHO_DEADLINE,
        "the completed say rendered its prose on the live screen",
    );
    session.wait_for(
        "(ran ",
        ECHO_DEADLINE,
        "the completed say card carries its `(ran Ns)` elapsed annotation",
    );

    // The canonical commit is the only writer of the source unit's record:
    // the live viewport consolidates it away (stage 2, #882), so the byte
    // stream gaining `Program source (lisp)` means the spool ran. Bounded —
    // a liveness gate on the commit trigger, not a latency assertion.
    let commit_deadline = Instant::now() + ECHO_DEADLINE;
    loop {
        if session
            .readable_transcript()
            .contains(CANONICAL_SOURCE_LABEL)
        {
            break;
        }
        if Instant::now() >= commit_deadline {
            panic!(
                "INVARIANT: a completed typed-program turn must spool its canonical \
                 record into native scrollback while the app is live (root AGENTS.md, \
                 TUI invariant: native history is the copyable record). The source \
                 unit's canonical label {CANONICAL_SOURCE_LABEL:?} never reached the \
                 terminal. Live screen was:\n{}\nscrollback was:\n{}\nterminal:\n{}",
                session.screen_text(),
                session.scrollback_text(),
                session.readable_transcript()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // The raw program and the say bytes must live in native scrollback,
    // written exactly once, and must not sit in the live area as the record.
    let scrollback = session.scrollback_text();
    let source_hits = scrollback.matches(SOURCE_LINE).count();
    assert!(
        source_hits == 1,
        "INVARIANT: the canonical record must carry the raw program exactly once in \
         native scrollback; found {source_hits} occurrence(s). Program={SOURCE_LINE:?}\n\
         scrollback:\n{scrollback}"
    );
    let say_hits = scrollback
        .lines()
        .filter(|line| line.trim() == SAY_TEXT)
        .count();
    assert!(
        say_hits == 1,
        "INVARIANT: the canonical record must carry the say output exactly once in \
         native scrollback; found {say_hits} prose line(s).\nscrollback:\n{scrollback}"
    );
    assert!(
        scrollback.contains(CANONICAL_SOURCE_LABEL),
        "INVARIANT: the spooled record is the canonical form with its source label.\n\
         scrollback:\n{scrollback}"
    );
    assert!(
        !session.screen_text().contains(SOURCE_LINE),
        "INVARIANT: after the spool the live area must not be the record — the raw \
         program belongs above the live area.\nlive screen:\n{}",
        session.screen_text()
    );

    session.send_line("/exit");
    let status = session.wait_for_exit();
    assert!(
        status.success(),
        "a clean /exit must succeed after the say turn, status={status:?}, \
         live screen:\n{}",
        session.screen_text()
    );
}

#[test]
fn attached_typed_program_turn_spools_its_canonical_record_into_native_scrollback() {
    const SAY_TEXT: &str = "attach-say-935";
    const SOURCE_LINE: &str = "(say \"attach-say-935\")";

    let daemon = IsolatedDaemon::start();
    let mut session = Session::spawn_on(&daemon.home, &["attach", BRAIN]);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    session.wait_for(BRAIN, READY_DEADLINE, "the named Brain label was drawn");
    // The typed program only executes when this frontend holds the runner
    // lease; the header names it.
    session.wait_for_screen(
        "· runner",
        READY_DEADLINE,
        "the attach drew the active runner lease header",
    );

    session.send_line(SOURCE_LINE);
    session.wait_for(
        "attach-say-935",
        ECHO_DEADLINE,
        "the completed say bytes reached the terminal",
    );

    // The canonical record must spool while the app is live, promptly enough
    // to observe. Poll until the say prose lands in native scrollback; a
    // bounded liveness gate on the whole chain (commit trigger through
    // delivery), not a latency assertion.
    let spool_start = Instant::now();
    let spool_deadline = spool_start + Duration::from_secs(30);
    let mut elapsed_at_spool = None;
    while Instant::now() < spool_deadline {
        if session
            .scrollback_text()
            .lines()
            .any(|line| line.trim() == SAY_TEXT)
        {
            elapsed_at_spool = Some(spool_start.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let scrollback = session.scrollback_text();
    if elapsed_at_spool.is_none() {
        let journal = std::fs::read_to_string(daemon.events_path()).unwrap_or_default();
        panic!(
            "INVARIANT: a completed typed-program turn in an attached session must \
             spool its canonical record into native scrollback while the app is live \
             (root AGENTS.md, TUI invariant: native history is the copyable record). \
             The say prose never reached scrollback within the wait window.\n\
             journal:\n{journal}\nscrollback:\n{scrollback}\n\
             live screen:\n{}\nterminal:\n{}",
            session.screen_text(),
            session.readable_transcript()
        );
    }

    // The raw program and the say bytes must live in native scrollback,
    // written exactly once.
    let source_hits = scrollback.matches(SOURCE_LINE).count();
    assert!(
        source_hits == 1,
        "INVARIANT: the canonical record must carry the raw program exactly once in \
         native scrollback; found {source_hits} occurrence(s). Program={SOURCE_LINE:?}\n\
         scrollback:\n{scrollback}"
    );
    let say_hits = scrollback
        .lines()
        .filter(|line| line.trim() == SAY_TEXT)
        .count();
    assert!(
        say_hits == 1,
        "INVARIANT: the canonical record must carry the say output exactly once in \
         native scrollback; found {say_hits} prose line(s).\nscrollback:\n{scrollback}"
    );

    session.send_line("/exit");
    let status = session.wait_for_exit();
    assert!(
        status.success(),
        "a clean /exit must succeed after the attached say turn, status={status:?}, \
         live screen:\n{}",
        session.screen_text()
    );
}

#[test]
fn completed_say_renders_one_representation_per_state_and_toggles_to_the_program() {
    const SAY_TEXT: &str = "attach-say-882";
    const SOURCE_LINE: &str = "(say \"attach-say-882\")";

    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture, &["attach", BRAIN]);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    session.send_line(SOURCE_LINE);
    // The Lisp typed-program path writes no user echo row, so the say bytes on
    // screen are the completed card's prose.
    session.wait_for_screen(
        SAY_TEXT,
        ECHO_DEADLINE,
        "the completed say rendered its prose on the live screen",
    );
    session.wait_for(
        "(ran ",
        ECHO_DEADLINE,
        "the completed say card carries its `(ran Ns)` elapsed annotation",
    );

    // The completed state on the live screen: prose + `(ran Ns)`, and NOTHING
    // else — no Program source row, no Brain run row, no UUID, no result row,
    // no card chrome (the stage-1 transition duplication is dead).
    let screen = session
        .wait_for_screen_pred(
            |screen| screen.matches(SAY_TEXT).count() >= 1,
            Duration::from_secs(5),
        )
        .expect("the completed card settled on the live screen");
    assert!(
        screen.lines().any(|line| line.contains(SAY_TEXT)),
        "INVARIANT: the completed say renders its prose on the live screen.\nlive screen:\n{screen}"
    );
    assert!(
        screen.lines().any(|line| line.contains("(ran ")),
        "INVARIANT: the completed say carries its `(ran Ns)` elapsed annotation \
         (docs/TUI_DESIGN.md, stage-2 verbatim target).\nlive screen:\n{screen}"
    );
    assert!(
        !screen.contains("Program source"),
        "INVARIANT: the legacy Program source row does not render for a say turn \
         (stage-2 consolidation); the canonical record keeps the raw program.\n\
         live screen:\n{screen}"
    );
    assert!(
        !screen.contains("Brain run"),
        "INVARIANT: no Brain run row renders for a say turn (stage-2 target names the \
         UUID-carrying row explicitly).\nlive screen:\n{screen}"
    );
    assert!(
        !screen.contains("result —"),
        "INVARIANT: no result row renders for a say turn.\nlive screen:\n{screen}"
    );
    assert!(
        uuid_only_lines(&screen).is_empty(),
        "INVARIANT: no UUID row renders for a say turn.\nlive screen:\n{screen}"
    );
    assert!(
        !screen.contains("[expanded]") && !screen.contains("[collapsed]"),
        "INVARIANT: disclosure never paints an [expanded]/[collapsed] token (#417).\n\
         live screen:\n{screen}"
    );
    assert!(
        !screen
            .lines()
            .any(|line| line.contains('\u{25b6}') || line.contains('\u{25bc}')),
        "INVARIANT: the say card wears no disclosure chrome — one representation per \
         state (#882 stage 2).\nlive screen:\n{screen}"
    );
    assert!(
        !screen.lines().any(|line| line.contains("running")),
        "INVARIANT: a completed say turn leaves no `running` residue (#820 class).\n\
         live screen:\n{screen}"
    );

    // Drive the toggle through the real input path: one write is F6 (focus the
    // next semantic row, `\x1b[17~`) followed by Enter (toggle it). The
    // completed output region is the hit target, so within a couple of
    // iterations the prose swaps to the program source, observable as a second
    // rendered occurrence of the exact source line on the live screen.
    let before = session.screen_text().matches(SOURCE_LINE).count();
    let mut toggled = None;
    for _ in 0..4 {
        session.send_line("\x1b[17~"); // F6, then Enter
        if let Some(screen) = session.wait_for_screen_pred(
            |screen| {
                let after = screen.matches(SOURCE_LINE).count();
                after > before && screen.contains("(ran ")
            },
            Duration::from_secs(5),
        ) {
            toggled = Some(screen);
            break;
        }
    }
    let screen = toggled.unwrap_or_else(|| {
        panic!(
            "INVARIANT: driving the keyboard disclosure path (F6/Enter) must toggle the \
             say card's show_program through the component ViewModel and swap the \
             completed prose to the program source.\nbefore: {before} occurrence(s) of \
             {SOURCE_LINE:?}.\nlive screen:\n{}",
            session.screen_text()
        )
    });
    let say_lines: Vec<&str> = screen
        .lines()
        .filter(|line| line.contains(SAY_TEXT))
        .collect();
    assert_eq!(
        say_lines.len(),
        1,
        "INVARIANT: the completed output swapped to the program source — the prose line \
         is gone and exactly the source line carries the say bytes.\nlive screen:\n{screen}"
    );
    assert!(
        say_lines[0].contains("(say"),
        "INVARIANT: the visible say bytes are the program source form; line={:?}",
        say_lines[0]
    );
    assert!(
        screen.lines().any(|line| line.contains("(ran ")),
        "INVARIANT: the `(ran Ns)` annotation stays through the swap.\nlive screen:\n{screen}"
    );

    // Esc clears keyboard focus (the accordion's documented key), so the
    // following Enter submits the command instead of toggling the still
    // focused output region. The Esc byte goes out raw — a trailing newline
    // would arrive as Alt+Enter.
    session.send_raw(b"\x1b");
    std::thread::sleep(Duration::from_millis(300));
    session.send_line("/exit");
    let status = session.wait_for_exit();
    assert!(
        status.success(),
        "a clean /exit must succeed after the say turn, status={status:?}, live screen:\n{screen}"
    );
}

/// #970 regression at the production boundary: a completed say turn must
/// replay from the journal, on reconnect, as the component card (prose +
/// `(ran Ns)`, source reveal via the toggle) — not the pre-stage-2 legacy
/// projection (Program source / result rows inside the Brain run group).
///
/// Mirrors the durable-reattach fixture: a real daemon owns the disposable
/// HOME, session 1 completes a say turn and exits cleanly, session 2 attaches
/// the same named Brain and reads the replayed transcript. Session 2 runs on
/// a shorter grid so the visible tail is the viewport the reader sees: the
/// canonical record (which keeps the raw program and output exactly once)
/// stays above the visible region, and absence assertions read the rendered
/// card, not the record.
#[test]
fn test_reconnected_completed_say_renders_the_component_card() {
    const SAY_TEXT: &str = "attach-say-970";
    const SOURCE_LINE: &str = "(say \"attach-say-970\")";
    const REPLAY_ROWS: u16 = 16;

    let daemon = IsolatedDaemon::start();
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
    first.send_line(SOURCE_LINE);
    // The typed-program say turn completes on the daemon path; the live
    // session renders it through the run-group projection (pre-stage-2 — the
    // replay reconstruction is what this ticket fixes), so wait for the say
    // bytes and then for the durable journal to carry the terminal run.
    first.wait_for(
        SAY_TEXT,
        ECHO_DEADLINE,
        "the completed say rendered its prose in the first session",
    );

    // The replay reads the durable journal, so the turn must be terminal in
    // it before session 1 exits; otherwise the reconnect would replay a still
    // running turn and this test would measure the daemon's writer, not the
    // regression. Bounded liveness gate on the journal append, not a latency
    // assertion.
    let journal_deadline = Instant::now() + ECHO_DEADLINE;
    loop {
        let journal = std::fs::read_to_string(daemon.events_path()).unwrap_or_default();
        if journal.lines().any(|line| {
            line.contains("run_status_changed") && line.contains("\"status\":\"completed\"")
        }) {
            break;
        }
        if Instant::now() >= journal_deadline {
            panic!(
                "INVARIANT: a completed say turn must reach the durable journal as a \
                 terminal run before the reconnect, so the replay has the event pattern \
                 to reconstruct from. Journal at {} never showed the completed status.\n\
                 journal:\n{journal}",
                daemon.events_path().display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    first.send_line("/exit");
    let status = first.wait_for_exit();
    let first_text = first.readable_transcript();
    assert!(
        status.success(),
        "a clean /exit must succeed after the say turn, status={status:?}, terminal:\n{first_text}"
    );
    drop(first);

    // Reconnect through the printed command, exactly as a user resumes.
    let printed = printed_attach_command(&first_text);
    let mut second = Session::spawn_sized(
        &daemon.home,
        &printed.split_whitespace().collect::<Vec<_>>(),
        REPLAY_ROWS,
        COLS,
    );
    second.wait_for(
        "finch v",
        READY_DEADLINE,
        "the reconnect drew the startup header",
    );
    second.wait_for_screen(
        SAY_TEXT,
        ECHO_DEADLINE,
        "the replayed say turn rendered its prose on the live screen",
    );
    let screen = second
        .wait_for_screen_pred(|screen| screen.contains("(ran "), Duration::from_secs(10))
        .unwrap_or_else(|| {
            panic!(
                "INVARIANT: the replayed completed say turn must render as the component \
                 card — prose plus the `(ran Ns)` annotation. The annotation never reached \
                 the live screen, so the replay fell back to the legacy projection.\n\
                 live screen:\n{}\nscrollback:\n{}\njournal:\n{}",
                second.screen_text(),
                second.scrollback_text(),
                std::fs::read_to_string(daemon.events_path()).unwrap_or_default()
            )
        });
    assert!(
        screen.lines().any(|line| line.contains(SAY_TEXT)),
        "INVARIANT: the replayed say turn renders its prose on the live screen.\n\
         live screen:\n{screen}"
    );
    assert!(
        screen.lines().any(|line| line.contains("(ran ")),
        "INVARIANT: the replayed card carries the `(ran Ns)` annotation (stage-2 \
         verbatim target, docs/TUI_DESIGN.md).\nlive screen:\n{screen}"
    );
    assert!(
        !screen.contains("Program source") && !screen.contains("Lisp program"),
        "INVARIANT: the legacy Program source row does not render for the replayed \
         say turn — one representation per state (issue 970). The canonical record \
         still carries it above the visible region.\nlive screen:\n{screen}"
    );
    assert!(
        !screen.lines().any(|line| line.contains("result")),
        "INVARIANT: no legacy result row renders for the replayed say turn.\n\
         live screen:\n{screen}"
    );
    assert!(
        !screen.contains("Interactive run") && !screen.contains("Brain run"),
        "INVARIANT: no Brain run group row renders for the replayed say turn.\n\
         live screen:\n{screen}"
    );
    assert!(
        uuid_only_lines(&screen).is_empty(),
        "INVARIANT: no UUID row renders for the replayed say turn.\nlive screen:\n{screen}"
    );

    // The toggle works on the replayed card through the real input path: F6
    // focuses the next semantic row, Enter activates it. The card's source
    // reveal is a second occurrence of the exact source line on the live
    // screen — the legacy projection has no toggle to produce one.
    let before = second.screen_text().matches(SOURCE_LINE).count();
    let mut toggled = None;
    for _ in 0..4 {
        second.send_line("\x1b[17~"); // F6, then Enter
        if let Some(screen) = second.wait_for_screen_pred(
            |screen| {
                let after = screen.matches(SOURCE_LINE).count();
                after > before && screen.contains("(ran ")
            },
            Duration::from_secs(5),
        ) {
            toggled = Some(screen);
            break;
        }
    }
    let screen = toggled.unwrap_or_else(|| {
        panic!(
            "INVARIANT: driving the keyboard disclosure path (F6/Enter) must toggle the \
             replayed say card's show_program through the component ViewModel and swap \
             the prose to the program source.\nbefore: {before} occurrence(s) of \
             {SOURCE_LINE:?}.\nlive screen:\n{}",
            second.screen_text()
        )
    });
    assert!(
        screen.matches(SOURCE_LINE).count() == before + 1,
        "INVARIANT: exactly one new occurrence of the source line — the card's \
         reveal, not a duplicated legacy row.\nlive screen:\n{screen}"
    );

    second.send_raw(b"\x1b");
    std::thread::sleep(Duration::from_millis(300));
    second.send_line("/exit");
    let status = second.wait_for_exit();
    assert!(
        status.success(),
        "a clean /exit must succeed after the replay, status={status:?}, live screen:\n{screen}"
    );
}

fn overlay_from_metadata(path: &Path) -> (Option<String>, Option<String>) {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "INVARIANT: model overlay persist must write metadata.json at {} ({error})",
            path.display()
        )
    });
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(|error| {
        panic!(
            "metadata.json must be JSON at {}: {error}; raw={raw}",
            path.display()
        )
    });
    (
        value
            .get("provider")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        value
            .get("model")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    )
}

#[test]
fn model_overlay_survives_exit_and_named_attach() {
    let daemon = IsolatedDaemon::start();
    let mut first = Session::spawn_on(&daemon.home, &["attach", BRAIN]);
    first.wait_for(
        "finch v",
        READY_DEADLINE,
        "the first attach drew the startup header",
    );
    first.send_line("/model grok-4.6");
    first.wait_for(
        "✓ Model overlay grok-4.6",
        ECHO_DEADLINE,
        "the model overlay command completed its durable write",
    );
    let before_exit = std::fs::read_to_string(daemon.metadata_path()).unwrap();
    assert!(
        before_exit.contains("grok-4.6"),
        "the success confirmation must follow the durable write; metadata before exit={before_exit}; terminal={}",
        first.readable_transcript()
    );
    first.send_line("/exit");
    let status = first.wait_for_exit();
    let first_text = first.readable_transcript();
    assert!(
        status.success(),
        "a clean /exit after /model must succeed, status={status:?}, terminal:\n{first_text}"
    );
    drop(first);

    let (provider, model) = overlay_from_metadata(&daemon.metadata_path());
    let metadata_raw = std::fs::read_to_string(daemon.metadata_path()).unwrap();
    assert_eq!(
        model.as_deref(),
        Some("grok-4.6"),
        "INVARIANT: /model must persist the overlay on the named Brain; provider={provider:?} metadata={} raw={metadata_raw} terminal:\n{first_text}",
        daemon.metadata_path().display(),
    );

    let mut second = Session::spawn_on(&daemon.home, &["attach", BRAIN]);
    second.wait_for(
        "finch v",
        READY_DEADLINE,
        "reattach after /model drew the startup header",
    );
    second.send_line("/status");
    second.wait_for(
        "grok-4.6",
        ECHO_DEADLINE,
        "reattach /status still reports the persisted overlay",
    );
    second.send_line("/exit");
    let second_status = second.wait_for_exit();
    let second_text = second.readable_transcript();
    assert!(
        second_status.success(),
        "reattach /exit must succeed, status={second_status:?}, terminal:\n{second_text}"
    );
    assert!(
        second_text.contains("grok-4.6"),
        "INVARIANT: finch attach {BRAIN} must still have the persisted model.\nterminal:\n{second_text}"
    );
}

#[test]
fn cli_model_flag_is_one_shot_and_does_not_rewrite_brain_metadata() {
    let daemon = IsolatedDaemon::start();
    std::fs::create_dir_all(daemon.brain_dir()).expect("create named Brain dir");
    std::fs::write(
        daemon.metadata_path(),
        r#"{"version":1,"brain_id":"00000000-0000-0000-0000-0000000000aa","created_ms":1,"provider":"attach-fixture","model":"grok-code-fast-1"}"#,
    )
    .expect("seed persisted overlay");

    let help = Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["attach", "--help"])
        .output()
        .expect("finch attach --help");
    let attach_help = String::from_utf8_lossy(&help.stdout);
    assert!(
        attach_help.contains("--model"),
        "attach help must document --model, got:\n{attach_help}"
    );
    assert!(
        attach_help.to_ascii_lowercase().contains("not persist")
            || attach_help.to_ascii_lowercase().contains("one-shot")
            || attach_help.contains("Does not persist"),
        "attach --model help must say it does not persist, got:\n{attach_help}"
    );

    let mut session = Session::spawn_on(&daemon.home, &["attach", BRAIN, "--model", "grok-4.6"]);
    session.wait_for(
        "one-shot",
        READY_DEADLINE,
        "one-shot attach projected its temporary model identity",
    );
    session.send_line("/exit");
    let _ = session.wait_for_exit();
    drop(session);

    let (_provider, model) = overlay_from_metadata(&daemon.metadata_path());
    assert_eq!(
        model.as_deref(),
        Some("grok-code-fast-1"),
        "INVARIANT: --model must not rewrite the Brain overlay; metadata model={model:?}"
    );
}
