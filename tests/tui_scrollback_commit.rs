//! The current session's committed transcript must reach the terminal's own
//! scrollback, with mouse capture enabled (#441, "Terminal scrollback is empty
//! for the current session once mouse capture is on").
//!
//! # Why this test is shaped the way it is
//!
//! The defect is invisible to every assertion that reads Finch's own state.
//! `src/cli/tui/mod.rs` keeps a `printed_ids` set and a shadow buffer, and both
//! are correct today: the renderer believes it committed the message, and a
//! unit test that asks it whether it did will agree. The user still sees an
//! empty scrollback. So the only assertion worth making is on **what the
//! terminal received** — the byte stream on the far side of a pty — and on what
//! a terminal does with those bytes.
//!
//! A raw pty is not a terminal: it has no screen and no scrollback, so reading
//! the byte stream alone cannot answer "is the text above the live region".
//! [`Vt`] below is therefore a small terminal emulator — cursor addressing,
//! erase, autowrap, and a scroll region of one: when a linefeed arrives on the
//! bottom row, the top row leaves the screen and lands in scrollback. That is
//! precisely the transition the renderer's commit path depends on, and
//! precisely the one no in-process test can observe.
//!
//! # The mechanism under test
//!
//! `commit_complete_messages` (`src/cli/tui/mod.rs`) prints a completed message
//! at the top of a cleared screen and then emits one viewport of linefeeds, so
//! the message rows scroll off the top and into native history. The live area
//! is repainted afterwards with absolute cursor addressing, which never
//! scrolls. Committed text therefore exists in exactly one place a user can
//! reach — the terminal's scrollback — and this test asserts it is there.
//!
//! # Process discipline
//!
//! The pty harness is modelled on the one written for #364 ("Instrument and
//! reduce Finch interactive TUI time-to-ready"), which is not yet on `main`.
//! Like it, the child is a plain `std::process::Command` spawn that calls none
//! of the session- or group-creating APIs `scripts/test_brain_isolation.sh`
//! allowlists by name, so it stays inside the supervisor's owned process group
//! as `AGENTS.md` requires. `Session::drop` kills and reaps only the child it
//! spawned. The pty is deliberately not made the child's controlling terminal:
//! `is_terminal` needs only a tty descriptor.
//!
//! # No wall-clock assertions
//!
//! Nothing here asserts a duration. The deadlines are hang detectors, and a
//! test that hits one says so and prints what the terminal showed.

#![cfg(unix)]

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Failure deadline for the startup header reaching the terminal. Not a
/// measurement: a run that takes longer than this has hung.
const READY_DEADLINE: Duration = Duration::from_secs(90);

/// Failure deadline for one submitted line to be echoed back. Same character.
const ECHO_DEADLINE: Duration = Duration::from_secs(60);

/// Failure deadline for a clean exit after `/exit`.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

const ROWS: u16 = 40;
const COLS: u16 = 120;

// ─── A terminal, with scrollback ────────────────────────────────────────────

/// A minimal terminal emulator: enough of the VT sequence set for the
/// crossterm-direct renderer, and a scrollback that records every row pushed
/// off the top of the screen.
///
/// Only what the renderer emits is interpreted — cursor addressing, relative
/// cursor motion, erase-in-display, erase-in-line, autowrap and linefeed. Mode
/// sets (`CSI ? … h/l`), SGR and OSC are consumed and discarded, which is what
/// a terminal does with them as far as the screen contents are concerned.
struct Vt {
    width: usize,
    height: usize,
    screen: Vec<Vec<char>>,
    scrollback: Vec<String>,
    row: usize,
    col: usize,
}

impl Vt {
    fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            screen: vec![vec![' '; width]; height],
            scrollback: Vec::new(),
            row: 0,
            col: 0,
        }
    }

    /// Interpret a whole transcript.
    fn feed_all(width: usize, height: usize, bytes: &[u8]) -> Self {
        let mut vt = Vt::new(width, height);
        let text = String::from_utf8_lossy(bytes).into_owned();
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            index = vt.step(&chars, index);
        }
        vt
    }

    /// Consume one character or one escape sequence; return the next index.
    fn step(&mut self, chars: &[char], index: usize) -> usize {
        let character = chars[index];
        if character == '\u{1b}' {
            return self.escape(chars, index + 1);
        }
        match character {
            '\r' => self.col = 0,
            '\n' => self.line_feed(),
            '\u{8}' => self.col = self.col.saturating_sub(1),
            '\t' => self.col = ((self.col / 8) + 1) * 8,
            character if (character as u32) < 0x20 => {}
            character => self.put(character),
        }
        index + 1
    }

    fn escape(&mut self, chars: &[char], index: usize) -> usize {
        match chars.get(index) {
            // CSI: parameter and intermediate bytes, then one final byte.
            Some('[') => {
                let mut cursor = index + 1;
                let start = cursor;
                while cursor < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[cursor]) {
                    cursor += 1;
                }
                if cursor >= chars.len() {
                    return chars.len();
                }
                let body: String = chars[start..cursor].iter().collect();
                self.csi(&body, chars[cursor]);
                cursor + 1
            }
            // OSC: terminated by BEL or ST.
            Some(']') => {
                let mut cursor = index + 1;
                while cursor < chars.len() {
                    if chars[cursor] == '\u{7}' {
                        return cursor + 1;
                    }
                    if chars[cursor] == '\u{1b}' && chars.get(cursor + 1) == Some(&'\\') {
                        return cursor + 2;
                    }
                    cursor += 1;
                }
                chars.len()
            }
            Some(_) => index + 1,
            None => index,
        }
    }

    fn csi(&mut self, body: &str, final_byte: char) {
        // Private modes (`CSI ? … h/l`) change no screen content.
        if body.starts_with('?') {
            return;
        }
        let params: Vec<usize> = body
            .split(';')
            .map(|part| part.parse::<usize>().unwrap_or(0))
            .collect();
        let first = params.first().copied().unwrap_or(0);
        let count = first.max(1);
        match final_byte {
            'H' | 'f' => {
                self.row = first.saturating_sub(1).min(self.height - 1);
                self.col = params
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(1)
                    .min(self.width - 1);
            }
            'A' => self.row = self.row.saturating_sub(count),
            'B' => self.row = (self.row + count).min(self.height - 1),
            'C' => self.col = (self.col + count).min(self.width - 1),
            'D' => self.col = self.col.saturating_sub(count),
            'G' => self.col = first.saturating_sub(1).min(self.width - 1),
            'E' => {
                self.row = (self.row + count).min(self.height - 1);
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(count);
                self.col = 0;
            }
            'J' => self.erase_in_display(first),
            'K' => self.erase_in_line(first),
            _ => {}
        }
    }

    /// `CSI 2 J` clears the visible screen. It does **not** push those rows
    /// into scrollback — only a scroll does — which is exactly why the commit
    /// path spools linefeeds rather than relying on the clear.
    fn erase_in_display(&mut self, mode: usize) {
        match mode {
            0 => {
                for column in self.col..self.width {
                    self.screen[self.row][column] = ' ';
                }
                for row in (self.row + 1)..self.height {
                    self.screen[row] = vec![' '; self.width];
                }
            }
            1 => {
                for column in 0..=self.col.min(self.width - 1) {
                    self.screen[self.row][column] = ' ';
                }
                for row in 0..self.row {
                    self.screen[row] = vec![' '; self.width];
                }
            }
            2 => {
                self.screen = vec![vec![' '; self.width]; self.height];
            }
            3 => {
                self.scrollback.clear();
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: usize) {
        match mode {
            0 => {
                for column in self.col..self.width {
                    self.screen[self.row][column] = ' ';
                }
            }
            1 => {
                for column in 0..=self.col.min(self.width - 1) {
                    self.screen[self.row][column] = ' ';
                }
            }
            2 => self.screen[self.row] = vec![' '; self.width],
            _ => {}
        }
    }

    /// A linefeed on the bottom row scrolls: the top row leaves the screen and
    /// is appended to scrollback, permanently.
    fn line_feed(&mut self) {
        if self.row + 1 < self.height {
            self.row += 1;
            return;
        }
        let departing: String = self.screen.remove(0).into_iter().collect();
        self.scrollback.push(departing.trim_end().to_string());
        self.screen.push(vec![' '; self.width]);
    }

    /// Deferred autowrap, as xterm implements it: reaching the right margin
    /// wraps on the *next* printable character, not on the one that filled it.
    fn put(&mut self, character: char) {
        if self.col >= self.width {
            self.col = 0;
            self.line_feed();
        }
        self.screen[self.row][self.col] = character;
        self.col += 1;
    }

    /// Every row that has left the screen, oldest first.
    fn scrollback_lines(&self) -> Vec<String> {
        self.scrollback
            .iter()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect()
    }

    /// Every row still on the visible screen.
    fn screen_lines(&self) -> Vec<String> {
        self.screen
            .iter()
            .map(|row| {
                row.iter()
                    .collect::<String>()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    }

    fn scrollback_contains(&self, needle: &str) -> bool {
        self.scrollback_lines()
            .iter()
            .any(|line| line.contains(needle))
    }

    fn screen_contains(&self, needle: &str) -> bool {
        self.screen_lines().iter().any(|line| line.contains(needle))
    }

    /// A rendering of both regions, for an assertion payload.
    fn report(&self) -> String {
        let scrollback = self.scrollback_lines();
        let screen = self.screen_lines();
        let mut out = String::new();
        out.push_str(&format!(
            "--- terminal scrollback ({} rows above the visible screen) ---\n",
            scrollback.len()
        ));
        for line in &scrollback {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&format!(
            "--- visible screen ({} rows, the live region) ---\n",
            screen.len()
        ));
        for line in &screen {
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

// ─── Fixture and session ────────────────────────────────────────────────────

/// A disposable HOME with the daemon switched off, so nothing on this path
/// contacts a developer's running Finch.
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
        // A complete `[client]` section with the daemon off, and one provider
        // that exists only so startup finds a configured model. Its base_url is
        // a port nothing listens on, so a regression that made this path call a
        // provider would fail rather than pass quietly. Nothing in this test
        // submits a query: the typed runtime answers locally.
        std::fs::write(
            finch.join("config.toml"),
            r#"[[providers]]
type = "grok"
api_key = "not-a-real-key"
model = "grok-code-fast-1"
base_url = "http://127.0.0.1:1"
name = "scrollback-fixture"

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
}

/// A `finch` running on the far side of a pty.
struct Session {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl Session {
    fn spawn(fixture: &Fixture) -> Self {
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
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err))
            .env("HOME", &fixture.home)
            .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
            .env("XDG_CACHE_HOME", fixture.home.join(".cache"))
            .env("XDG_DATA_HOME", fixture.home.join(".local/share"))
            .env("HF_HOME", fixture.home.join(".cache/huggingface"))
            .env("TERM", "xterm-256color")
            // Blocks daemon discovery, reuse and auto-spawn without needing a
            // supervisor proof (`src/daemon/spawn.rs`).
            .env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("XAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("RUST_LOG")
            // Under the mandated launcher this process carries the supervisor's
            // environment; the child must not inherit a proof descriptor it was
            // not given, nor a Brain root that no longer matches its HOME.
            .env_remove("FINCH_BRAIN_TEST_ISOLATED")
            .env_remove("FINCH_BRAIN_TEST_TOKEN")
            .env_remove("FINCH_BRAIN_TEST_PROOF_FD")
            .env_remove("FINCH_BRAIN_TEST_PROOF_BACKUP_FD")
            .env_remove("FINCH_BRAIN_TEST_AUTH_FD")
            .env_remove("FINCH_BRAIN_TEST_HOME")
            .env_remove("FINCH_BRAIN_TEST_ROOT")
            .env_remove("FINCH_TEST_SUPERVISOR_PID")
            .env_remove("FINCH_TEST_SUPERVISOR_BIN")
            .env_remove("FINCH_TEST_IPC_SOCKET")
            .env_remove("FINCH_TEST_SOCKET_ROOT")
            .env_remove("FINCH_TEST_DAEMON_ADDR")
            .env_remove("FINCH_TEST_BRAIN_ADDR");

        let child = command.spawn().expect("spawn finch under a pty");
        // Close this side's copy of the slave so the master sees EOF when the
        // child exits, rather than blocking on a descriptor we hold.
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

    fn transcript_bytes(&self) -> Vec<u8> {
        self.transcript.lock().expect("transcript poisoned").clone()
    }

    /// The transcript with terminal control sequences removed and all
    /// whitespace collapsed, for liveness checks only. Assertions about where
    /// content ended up go through [`Vt`], because this rendering cannot tell
    /// scrollback from the live region.
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
                        if ('\u{40}'..='\u{7e}').contains(&byte) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    while let Some(byte) = chars.next() {
                        if byte == '\u{7}' {
                            break;
                        }
                        if byte == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
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
        stripped.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Wait until `needle` has appeared anywhere on the terminal.
    ///
    /// Synchronisation is on the artifact — bytes the process actually wrote —
    /// never on a sleep standing in for one. The deadline is a hang detector.
    fn wait_for(&mut self, needle: &str, deadline: Duration, what: &str) {
        let expiry = Instant::now() + deadline;
        loop {
            if self.readable_transcript().contains(needle) {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "finch exited with {status:?} before {what} ({needle:?} never reached \
                     the terminal). Terminal was:\n{}",
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
        // The reader owns a dup of the master; dropping ours lets it see EOF.
        let _ = self.reader_done.recv_timeout(Duration::from_secs(5));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// ─── The regression ─────────────────────────────────────────────────────────

/// Markers submitted as Finch-Lisp, which the typed runtime answers locally.
/// No provider is contacted, so what reaches the terminal is deterministic.
const MARKERS: [&str; 3] = ["770011", "770022", "770033"];

/// Committed messages must be in the terminal's scrollback, not merely on the
/// visible screen, while mouse capture is enabled.
///
/// This is the user-visible claim in #441 ("Terminal scrollback is empty for
/// the current session once mouse capture is on"): an engineer scrolls up in
/// the first minute and expects to find what they just did. It fails if the
/// commit path's linefeed spool never runs, if it is defeated before the rows
/// leave the screen, or if the renderer only ever repaints committed text into
/// the live region — all of which look identical to a user, and all of which
/// pass an assertion on `printed_ids`.
#[test]
fn test_committed_messages_reach_terminal_scrollback_under_mouse_capture() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture);

    // The startup header is written through the same OutputManager path as
    // every other message, so its arrival means the renderer is committing.
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");

    for marker in MARKERS {
        session.send_line(&format!("(+ {marker} 0)"));
        session.wait_for(
            marker,
            ECHO_DEADLINE,
            &format!("the typed program `(+ {marker} 0)` was echoed"),
        );
    }

    session.send_line("/exit");
    let status = session.wait_for_exit();

    let vt = Vt::feed_all(
        usize::from(COLS),
        usize::from(ROWS),
        &session.transcript_bytes(),
    );

    let missing: Vec<&str> = MARKERS
        .iter()
        .copied()
        .filter(|marker| !vt.scrollback_contains(marker))
        .collect();
    assert!(
        missing.is_empty(),
        "INVARIANT: a message Finch has committed is in the terminal's own \
         scrollback, so the user can scroll back to it (#441).\n\
         Expected every marker {MARKERS:?} to appear above the visible screen.\n\
         Missing from scrollback: {missing:?}.\n\
         Still on the visible screen (so written, but never scrolled off): {:?}.\n\
         finch exited with {status:?}.\n{}",
        missing
            .iter()
            .copied()
            .filter(|marker| vt.screen_contains(marker))
            .collect::<Vec<_>>(),
        vt.report(),
    );
}

/// The startup header itself must reach scrollback.
///
/// It is the very first thing Finch commits, before any user input, so it
/// isolates the commit path from anything the typed runtime does. If this
/// fails and the marker test also fails, the commit path is broken from the
/// first message rather than degrading after a few.
#[test]
fn test_startup_header_reaches_terminal_scrollback_under_mouse_capture() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");

    // One committed message after the header, so the header has certainly been
    // spooled rather than still sitting in the live region awaiting a commit.
    session.send_line(&format!("(+ {} 0)", MARKERS[0]));
    session.wait_for(
        MARKERS[0],
        ECHO_DEADLINE,
        "the first typed program was echoed",
    );

    session.send_line("/exit");
    let status = session.wait_for_exit();

    let vt = Vt::feed_all(
        usize::from(COLS),
        usize::from(ROWS),
        &session.transcript_bytes(),
    );

    assert!(
        vt.scrollback_contains("finch v"),
        "INVARIANT: the startup header, the first message Finch commits, is in \
         the terminal's own scrollback (#441).\n\
         It was{} still on the visible screen.\n\
         finch exited with {status:?}.\n{}",
        if vt.screen_contains("finch v") {
            ""
        } else {
            " not"
        },
        vt.report(),
    );
}
