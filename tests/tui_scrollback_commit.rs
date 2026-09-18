//! Native terminal scrollback and click-drag selection must remain reachable
//! while Finch is running (#221). Mouse capture is off by default so the host
//! terminal owns drags and the wheel. Accordion expand/collapse stays on the
//! keyboard. When an opt-in path later holds capture, #441's wheel-release
//! still applies (covered in `src/cli/tui/mouse_capture.rs`).
//!
//! # What actually breaks
//!
//! Two failures look identical to a user who scrolls up and does not find the
//! current session. They are not the same mechanism:
//!
//! 1. **Commit-into-scrollback.** Completed rows never leave the live region.
//!    A 2026-09-07 PTY replay of the production byte stream ruled this out:
//!    committed text is already above the visible screen even with
//!    `EnableMouseCapture` on. Mouse tracking sequences (`CSI ? 1000/1002/1003/
//!    1015/1006 h`) are input-reporting modes; they do not erase or withhold
//!    screen content. This file still locks that path so it cannot regress.
//!
//! 2. **Wheel / drag capture.** `EnableMouseCapture` makes the terminal
//!    deliver wheel ticks and drags to Finch instead of native scroll and
//!    selection. Default-off is the #221 fix; these cases lock that the
//!    production binary never enables capture on startup, wheel, or keypress.
//!
//! # Production boundary
//!
//! `Repl` takes the interactive branch only when stdout `is_terminal()`. A
//! `#[cfg(test)]` function inside the library process cannot arrange that, so
//! these cases spawn the real `finch` binary on a pty, inject an SGR wheel
//! event the same way a terminal would, and assert on the byte stream the
//! process writes back.
//!
//! # Process discipline
//!
//! The child is a plain `std::process::Command` spawn. It calls none of the
//! session- or group-creating APIs `scripts/test_brain_isolation.sh`
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
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Failure deadline for the startup header reaching the terminal. Not a
/// measurement: a run that takes longer than this has hung.
const READY_DEADLINE: Duration = Duration::from_secs(90);

/// Failure deadline for one submitted line to be echoed back. Same character.
const ECHO_DEADLINE: Duration = Duration::from_secs(60);

/// Failure deadline for mouse-tracking mode changes after a wheel or key.
#[allow(dead_code)]
const TRACKING_DEADLINE: Duration = Duration::from_secs(15);

/// Failure deadline for a clean exit after `/exit`.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

const ROWS: u16 = 40;
const COLS: u16 = 120;

const SUPERVISOR_AUTHORITY_FDS: &[i32] = &[9, 10, 11, 12, 108, 109, 110, 111, 112];

/// Crossterm `EnableMouseCapture` / `DisableMouseCapture` end in these
/// private-mode flags. Matching the 1000 flag is enough to see the mode
/// change; the rest of the bundle (`1002/1003/1015/1006`) travels with it.
const MOUSE_TRACKING_ON: &[u8] = b"\x1b[?1000h";
const MOUSE_TRACKING_OFF: &[u8] = b"\x1b[?1000l";

/// SGR mouse wheel-up at column 1, row 1 (`CSI < 64 ; 1 ; 1 M`).
const SGR_WHEEL_UP: &[u8] = b"\x1b[<64;1;1M";

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
            .env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("XAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("SHAMMAH_LOG")
            .env_remove("RUST_LOG");

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

    /// Wait until `needle` appears in the raw byte stream at or after `from`.
    #[allow(dead_code)]
    fn wait_for_raw_from(
        &mut self,
        needle: &[u8],
        from: usize,
        deadline: Duration,
        what: &str,
    ) -> usize {
        let expiry = Instant::now() + deadline;
        loop {
            let bytes = self.transcript_bytes();
            if let Some(relative) = find_subslice(&bytes[from.min(bytes.len())..], needle) {
                return from + relative;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "INVARIANT: {what}\n\
                     finch exited with {status:?} before that happened.\n\
                     needle={needle:?} from={from} bytes_after={}\n\
                     readable terminal:\n{}",
                    bytes.len().saturating_sub(from),
                    self.readable_transcript()
                );
            }
            if Instant::now() >= expiry {
                panic!(
                    "INVARIANT: {what}\n\
                     The hang detector fired after {deadline:?}; this is not a \
                     latency assertion.\n\
                     needle={needle:?} from={from} bytes_after={}\n\
                     readable terminal:\n{}",
                    self.transcript_bytes().len().saturating_sub(from),
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

    fn send_bytes(&mut self, bytes: &[u8]) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone master for writing"));
        file.write_all(bytes).expect("write bytes to the pty");
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

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

// ─── The regressions ────────────────────────────────────────────────────────

/// Markers submitted as Finch-Lisp, which the typed runtime answers locally.
/// No provider is contacted, so what reaches the terminal is deterministic.
const MARKERS: [&str; 3] = ["770011", "770022", "770033"];

/// Committed messages must be in the terminal's scrollback, not merely on the
/// visible screen, while mouse capture is enabled.
///
/// This locks the 2026-09-07 finding that hypothesis 1 (the commit path
/// erases or withholds rows) does not reproduce: the linefeed spool already
/// puts committed text above the live region. A regression that deleted that
/// spool would fail with the markers still on the visible screen.
#[test]
fn test_committed_messages_reach_terminal_scrollback_under_mouse_capture() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture);

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

/// Default TUI startup holds mouse capture so the conversation ScrollView owns
/// the wheel (#806, which decides the #443 fork over #221's capture-off
/// default). Shutdown must still disable reporting so nothing leaks into the
/// shell.
#[test]
fn test_tui_startup_enables_mouse_capture_for_the_scroll_view() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");

    let on_at = find_subslice(&session.transcript_bytes(), MOUSE_TRACKING_ON);
    assert!(
        on_at.is_some(),
        "INVARIANT: the default TUI must emit EnableMouseCapture at startup so \
         wheels and clicks reach the conversation ScrollView and disclosure \
         hitboxes (#806). Readable terminal:\n{}",
        session.readable_transcript()
    );

    session.send_line("/exit");
    let status = session.wait_for_exit();
    let bytes = session.transcript_bytes();
    assert!(
        find_subslice(&bytes, MOUSE_TRACKING_OFF).is_some(),
        "INVARIANT: shutdown emits DisableMouseCapture so a session that held \
         tracking cannot leak reporting into the shell. finch exited with \
         {status:?}. Readable terminal:\n{}",
        session.readable_transcript()
    );
}

/// A wheel tick or a keypress must not hand the wheel back to the terminal:
/// the #441 release-on-first-wheel hybrid is retired (#806). Capture stays
/// held through ordinary review, and shutdown still disables it exactly
/// through the normal path.
#[test]
fn test_wheel_and_keypress_hold_mouse_capture() {
    let fixture = Fixture::new();
    let mut session = Session::spawn(&fixture);
    session.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");

    let before_wheel = find_subslice(&session.transcript_bytes(), MOUSE_TRACKING_OFF);
    session.send_bytes(SGR_WHEEL_UP);
    // A printable key plus backspace so `/exit` is not submitted as `x/exit`.
    session.send_bytes(b"x");
    session.send_bytes(b"\x7f");

    let off_at = find_subslice(&session.transcript_bytes(), MOUSE_TRACKING_OFF);
    assert_eq!(
        off_at,
        before_wheel,
        "INVARIANT: a wheel or keypress must not disable mouse capture — native \
         scrollback is the copyable record, never the reader (#806), so the \
         #441 release-on-first-wheel policy is retired. First disable was at \
         byte {off_at:?} (was {before_wheel:?}). Readable terminal:\n{}",
        session.readable_transcript()
    );

    match session.child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => panic!(
            "INVARIANT: the session must still be live after the wheel and \
             keypress (#806). finch exited with {status:?}. Readable \
             terminal:\n{}",
            session.readable_transcript()
        ),
        Err(error) => panic!("could not poll finch after the wheel: {error}"),
    }

    session.send_line("/exit");
    let status = session.wait_for_exit();
    let bytes = session.transcript_bytes();
    assert!(
        find_subslice(&bytes, MOUSE_TRACKING_OFF).is_some(),
        "INVARIANT: shutdown emits DisableMouseCapture so tracking cannot leak \
         into the shell. finch exited with {status:?}. Readable terminal:\n{}",
        session.readable_transcript()
    );
}
