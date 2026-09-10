//! Interactive startup, driven through a real PTY (#364, "Instrument and
//! reduce Finch interactive TUI time-to-ready").
//!
//! # Why a PTY
//!
//! `Repl` decides it is interactive with `io::stdout().is_terminal()`
//! (`src/cli/repl.rs`). Every existing attempt to test startup misses the TUI
//! entirely for that reason: `tests/tui_integration_test.rs` uses
//! `Stdio::piped()`, so the binary takes the non-interactive branch in
//! `src/main.rs`, and `scripts/test_tui_debug.sh` redirects stdout to a file.
//! Nothing in the repository has ever executed the interactive startup path
//! under test. These tests do, by giving the child a pty slave for its three
//! standard descriptors.
//!
//! # What these tests do and do not cover
//!
//! Every fixture sets `use_daemon = false`, so `DaemonClient::connect` never
//! runs and `GET /health` -- the probe whose Brain enumeration this work also
//! fixes -- is never on this path. That is deliberate: a fixture with a live
//! daemon would make the assertions depend on that daemon's warmth. So these
//! tests cover the **frontend** startup path, and the health probe's
//! bounded-work property is asserted at the server boundary instead, in
//! `src/server/handlers.rs`.
//!
//! What they prove about the Brain inventory is narrower and is stated exactly,
//! because an earlier version of this comment claimed a hydration counter
//! inside `BrainStore::ensure_loaded` that does not exist in this change and
//! was rejected when it did: **the set of phases and marks the frontend
//! records before the first frame is identical at zero Brains and at four
//! hundred, and identical again when the inventory is malformed and
//! half-written.** That is a claim about the frontend not enumerating the Brain
//! root, asserted on the report the real binary publishes. It is not a claim
//! about how many Brains anything hydrates.
//!
//! # Why the assertions have no clock in them
//!
//! Issue #364, "Instrument and reduce Finch interactive TUI time-to-ready",
//! states it under Required coverage: "Synchronization and structural
//! assertions, not absolute wall-clock thresholds." The precedent it cites is
//! the four attempts at a timing assertion on #242, "Make ordinary TUI
//! startup prompt-first and lazily hydrate MemTree" -- one of which reached
//! 35/35 green CI while depending on the machine being busy. `a0ea2c64`
//! ("assert hydration state, not a wall-clock ratio") is the commit that
//! replaced the last of them; #364 names the issue, and the commit is one
//! step further out.
//!
//! Cited precisely, because two earlier revisions of this file did not, and
//! because a third revision named the commit as though #364 had. This
//! requirement is written in #364 and its precedent is #242's four attempts,
//! the last of them repaired by `a0ea2c64`. It is
//! **not** a rule in `AGENTS.md`: PR #391, "docs(agents): write down the
//! no-wall-clock-assertion rule", proposed adding it and was closed DO NOT
//! MERGE, on the ground that a blanket prohibition is not what `a0ea2c64`
//! established and conflicts with the coarse liveness and resource bounds
//! Finch accepts elsewhere -- including in the deadlines below. A reader who
//! greps `AGENTS.md` for this rule will not find it, and should not have been
//! told to look there.
//!
//! So nothing here asserts a duration, a ratio, or a deadline as a
//! *property*. The timings are recorded and reported; what is asserted is
//! structure -- which phases ran, in what order, how they nest, how many
//! things each carried to completion, and that the set does not change with
//! the size or the sanity of the Brain inventory on disk.
//!
//! Waiting is on artifacts, never on sleeps-as-synchronisation: the harness
//! waits for the report file the process writes at readiness. The timeouts
//! that do exist are failure deadlines, not measurements, and a test that hits
//! one reports what it saw on the terminal.
//!
//! # Process discipline
//!
//! The child is a plain `std::process::Command` spawn. It calls none of the
//! session- or group-creating APIs that `scripts/test_brain_isolation.sh`
//! allowlists by name, so it stays inside the supervisor's owned process group
//! as `AGENTS.md` requires. (Those names are deliberately not spelled here:
//! the isolation gate greps the whole `src`, `scripts` and `tests` closure for
//! them, and prose saying "we do not call X" reads to that scanner exactly
//! like a call to X.) `Session::drop` kills and reaps the one child it
//! spawned, by handle, and signals no pid it did not create.
//!
//! The pty is deliberately *not* made the child's controlling terminal.
//! `is_terminal` and `tcsetattr` need only a tty descriptor, and claiming a
//! controlling terminal would require precisely the APIs trusted test code is
//! forbidden to call.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Failure deadline for reaching readiness. Not a measurement: a test that
/// takes longer than this has hung, and the assertion says what was on the
/// terminal when it did.
const READY_DEADLINE: Duration = Duration::from_secs(90);

/// Failure deadline for a clean exit after `/exit`.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// Independent executable oracle: do not mirror the production constant.
const EXPECTED_ABOUT: &str =
    "Terminal coding assistant with typed programs, named Brains, and tool use";

const FORBIDDEN_IDENTITY_PHRASES: &[&str] = &[
    "proxy",
    "constitutional",
    "local-first",
    "local first",
    "offline",
    "shammah v",
];
const STALE_IDENTITY_PHRASES: &[&str] = &[
    "proxy",
    "constitutional",
    "local-first",
    "local first",
    "shammah v",
];

const SUPERVISOR_AUTHORITY_FDS: &[i32] = &[9, 10, 11, 12, 108, 109, 110, 111, 112];
const CAPTURE_DIAGNOSTIC_LIMIT: u64 = 64 * 1024;

/// One parsed line of the startup report.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    kind: String,
    name: String,
    at_ms: String,
    ms: String,
    /// How many phases enclose this one. Phases nest, so durations do not sum.
    depth: Option<u64>,
    count: Option<u64>,
    category: Option<String>,
    slow: bool,
}

impl Entry {
    /// The offset the report states, or a failure that names the garbled line.
    ///
    /// Never a default. An earlier version used `parse().unwrap_or(0.0)`,
    /// which turned an unparseable offset into the most permissive possible
    /// value and passed.
    fn at_ms(&self, report: &str) -> f64 {
        self.at_ms.parse::<f64>().unwrap_or_else(|error| {
            panic!(
                "the {:?} entry's at_ms field is {:?}, which is not a number \
                 ({error}). The report is machine-read by this harness and by \
                 the benchmark; a field that does not parse is a broken \
                 report, not a zero. Report was:\n{report}",
                self.name, self.at_ms
            )
        })
    }

    /// The duration the report states, or a failure that names the garbled
    /// line. Never a default, for the reason [`Entry::at_ms`] gives.
    fn ms(&self, report: &str) -> f64 {
        self.ms.parse::<f64>().unwrap_or_else(|error| {
            panic!(
                "the {:?} entry's ms field is {:?}, which is not a number \
                 ({error}). The coverage assertion recomputes accounted_ms \
                 from these, so a field that does not parse is a broken \
                 report, not a zero. Report was:\n{report}",
                self.name, self.ms
            )
        })
    }
}

/// A parsed startup report.
#[derive(Debug, Clone)]
struct Report {
    raw: String,
    entries: Vec<Entry>,
    time_to_ready_ms: Option<f64>,
    accounted_ms: Option<f64>,
    unaccounted_ms: Option<f64>,
}

impl Report {
    fn parse(raw: &str) -> Self {
        let mut entries = Vec::new();
        let mut time_to_ready_ms = None;
        let mut accounted_ms = None;
        let mut unaccounted_ms = None;
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("time_to_ready_ms=") {
                time_to_ready_ms = value.parse::<f64>().ok();
                continue;
            }
            if line.starts_with("accounted_ms=") {
                for token in line.split_whitespace() {
                    if let Some(value) = token.strip_prefix("accounted_ms=") {
                        accounted_ms = value.parse::<f64>().ok();
                    }
                    if let Some(value) = token.strip_prefix("unaccounted_ms=") {
                        unaccounted_ms = value.parse::<f64>().ok();
                    }
                }
                continue;
            }
            let mut tokens = line.split_whitespace();
            let Some(kind) = tokens.next() else { continue };
            if kind != "phase" && kind != "mark" {
                continue;
            }
            let Some(name) = tokens.next() else { continue };
            let mut entry = Entry {
                kind: kind.to_string(),
                name: name.to_string(),
                at_ms: String::new(),
                ms: String::new(),
                depth: None,
                count: None,
                category: None,
                slow: false,
            };
            for token in tokens {
                match token.split_once('=') {
                    Some(("at_ms", value)) => entry.at_ms = value.to_string(),
                    Some(("ms", value)) => entry.ms = value.to_string(),
                    Some(("count", value)) => entry.count = value.parse().ok(),
                    Some(("depth", value)) => entry.depth = value.parse().ok(),
                    Some(("category", value)) => entry.category = Some(value.to_string()),
                    _ if token == "SLOW" => entry.slow = true,
                    _ => {}
                }
            }
            entries.push(entry);
        }
        Self {
            raw: raw.to_string(),
            entries,
            time_to_ready_ms,
            accounted_ms,
            unaccounted_ms,
        }
    }

    /// Phase and mark names in the order they were recorded.
    fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    fn position(&self, name: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.name == name)
    }
}

/// A disposable HOME with a seeded Brain inventory.
struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    timings: PathBuf,
}

impl Fixture {
    /// `brains` well-formed Brain directories, each with a plausible event log.
    fn new(brains: usize) -> Self {
        Self::with_inventory(brains, |_| {})
    }

    /// As [`Fixture::new`], then `seed` may add hostile entries to the root.
    fn with_inventory(brains: usize, seed: impl FnOnce(&Path)) -> Self {
        let temp = tempfile::tempdir().expect("disposable HOME");
        let home = temp.path().to_path_buf();
        let finch = home.join(".finch");
        std::fs::create_dir_all(finch.join("brains")).expect("create .finch/brains");

        // A complete `[client]` section -- every field is required -- with the
        // daemon switched off, so the probe that would otherwise find this
        // developer's real daemon on 127.0.0.1:11435 never runs and the phase
        // set is the same on any machine. The provider exists only to satisfy
        // "config has providers"; nothing at startup contacts it, and its
        // base_url is a port nothing listens on so a regression that made
        // startup call a provider would fail rather than succeed quietly.
        std::fs::write(
            finch.join("config.toml"),
            r#"[[providers]]
type = "grok"
api_key = "not-a-real-key"
model = "grok-code-fast-1"
base_url = "http://127.0.0.1:1"
name = "startup-fixture"

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

        let root = finch.join("brains");
        for index in 0..brains {
            let directory = root.join(format!("brain-{index:04}"));
            std::fs::create_dir_all(&directory).expect("create Brain directory");
            // Enough content that a hydrating startup would have real work to
            // do, so "the phase set did not change" is a claim about laziness
            // rather than about an empty directory.
            let mut log = String::new();
            for seq in 1..=8 {
                log.push_str(&format!(
                    "{{\"schema_version\":1,\"seq\":{seq},\"sender\":\"seed\",\"created_ms\":{seq}}}\n"
                ));
            }
            std::fs::write(directory.join("events.jsonl"), log).expect("seed events");
        }
        seed(&root);

        let timings = home.join("startup-timings.txt");
        Self {
            _temp: temp,
            home,
            timings,
        }
    }
}

/// A command for the real binary with every mutable path redirected into the
/// fixture and every provider credential removed.
fn isolated_finch_command(fixture: &Fixture) -> Command {
    isolated_child_command(Command::new(env!("CARGO_BIN_EXE_finch")), fixture)
}

fn isolated_child_command(mut command: Command, fixture: &Fixture) -> Command {
    command
        .env("HOME", &fixture.home)
        .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
        .env("XDG_CACHE_HOME", fixture.home.join(".cache"))
        .env("XDG_DATA_HOME", fixture.home.join(".local/share"))
        .env("HF_HOME", fixture.home.join(".cache/huggingface"))
        .env("TERM", "xterm-256color")
        .env("FINCH_STARTUP_TIMINGS", &fixture.timings)
        // No provider credential can be picked up from the developer's
        // environment, so no request can be made even in principle.
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("XAI_API_KEY")
        .env_remove("GEMINI_API_KEY")
        .env_remove("GOOGLE_API_KEY")
        .env_remove("SHAMMAH_DEBUG")
        .env_remove("SHAMMAH_LOG")
        .env_remove("RUST_LOG");

    // The supervisor intentionally exports more authority variables over
    // time. Prefix removal fails closed for additions instead of relying on a
    // hand-maintained list that silently omitted passwords and listener FDs.
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("FINCH_BRAIN_TEST_") || name_text.starts_with("FINCH_TEST_") {
            command.env_remove(name);
        }
    }
    // This is a restriction, not authority: a regression that tries to find
    // or launch a daemon must fail rather than escape the fixture.
    command.env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1");

    // Environment labels are not capabilities. Close every descriptor the
    // authenticated supervisor deliberately makes inheritable, in the child
    // after stdio has been installed and immediately before exec.
    unsafe {
        command.pre_exec(|| {
            for fd in SUPERVISOR_AUTHORITY_FDS {
                nix::libc::close(*fd);
            }
            Ok(())
        });
    }
    command
}

/// A `finch` running on the far side of a pty.
struct Session {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    /// Signalled by the reader thread as it exits, so `Drop` can join with a
    /// deadline instead of unconditionally.
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl Session {
    /// Spawn the real binary with a pty on all three standard descriptors.
    fn spawn(fixture: &Fixture) -> Self {
        Self::spawn_with_args(fixture, &[])
    }

    fn spawn_with_args(fixture: &Fixture, args: &[&str]) -> Self {
        Self::spawn_with_env(fixture, args, &[])
    }

    fn spawn_with_env(fixture: &Fixture, args: &[&str], extra_env: &[(&str, &str)]) -> Self {
        let winsize = nix::pty::Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let pty = nix::pty::openpty(&winsize, None).expect("openpty");

        let slave_in = pty.slave.try_clone().expect("clone slave for stdin");
        let slave_out = pty.slave.try_clone().expect("clone slave for stdout");
        let slave_err = pty.slave.try_clone().expect("clone slave for stderr");

        let mut command = isolated_finch_command(fixture);
        command
            .args(args)
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err));
        for (name, value) in extra_env {
            command.env(name, value);
        }

        let child = command.spawn().expect("spawn finch under a pty");
        // Close this side's copy of the slave, so the master sees EOF when the
        // child exits rather than blocking forever on a descriptor we hold.
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

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("transcript poisoned")).into_owned()
    }

    /// The transcript with terminal control sequences removed, line structure
    /// intact.
    fn stripped_transcript(&self) -> String {
        let raw = self.transcript();
        let mut out = String::with_capacity(raw.len());
        let mut chars = raw.chars().peekable();
        while let Some(character) = chars.next() {
            if character != '\u{1b}' {
                out.push(character);
                continue;
            }
            match chars.peek() {
                // CSI: parameters and intermediates, then one final byte.
                Some('[') => {
                    chars.next();
                    for byte in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&byte) {
                            break;
                        }
                    }
                }
                // OSC: terminated by BEL or ST.
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
                // Two-character escapes.
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
        }
        out
    }

    /// The stripped transcript with *all* whitespace collapsed to single
    /// spaces, so a `contains` assertion about what a user can read is not
    /// defeated by the renderer having placed a cursor move or a line wrap
    /// between two words of the same message.
    fn readable_transcript(&self) -> String {
        self.stripped_transcript()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// One entry per terminal line, spaces within a line collapsed, blank
    /// lines dropped.
    ///
    /// [`Session::readable_transcript`] joins on *all* whitespace, which folds
    /// the whole terminal into one string -- so calling `.lines()` on it
    /// yields a single item and a loop meant to inspect each message inspects
    /// one enormous line instead. A per-line assertion needs this.
    fn readable_lines(&self) -> Vec<String> {
        self.stripped_transcript()
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|line| !line.is_empty())
            .collect()
    }

    /// Wait for the process to publish its startup report.
    ///
    /// Synchronisation is on the artifact, not on a duration: the file appears
    /// exactly when `startup::ready()` runs, immediately before the loop that
    /// consumes keys.
    fn wait_for_report(&mut self, fixture: &Fixture) -> Report {
        let deadline = Instant::now() + READY_DEADLINE;
        loop {
            if let Ok(raw) = std::fs::read_to_string(&fixture.timings) {
                // `publish` writes a per-pid temporary and renames, so this
                // path is either absent or complete. Checking the terminating
                // line as well costs nothing and keeps the harness correct if
                // the destination ever lands on a filesystem where the rename
                // is not atomic.
                if raw.contains("time_to_ready_ms=") {
                    return Report::parse(&raw);
                }
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "finch exited with {status:?} before it became input-ready, so no \
                     startup report was written to {}. Terminal was:\n{}",
                    fixture.timings.display(),
                    self.transcript()
                );
            }
            if Instant::now() >= deadline {
                panic!(
                    "finch did not become input-ready within {READY_DEADLINE:?}; no \
                     startup report at {}. This deadline is a hang detector, not a \
                     latency assertion. Terminal was:\n{}",
                    fixture.timings.display(),
                    self.transcript()
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Type a line into the terminal.
    fn send_line(&mut self, line: &str) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone master for writing"));
        write!(file, "{line}\r").expect("write to the pty");
        file.flush().expect("flush the pty");
        // `file` owns a dup of the master and closes it here. That neither
        // flushes nor discards the tty's input queue, so the bytes stay
        // readable by the child.
    }

    /// Wait for a clean exit, or report what the terminal showed instead.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + EXIT_DEADLINE;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {}
                Err(error) => panic!("could not wait for finch: {error}"),
            }
            if Instant::now() >= deadline {
                let transcript = self.transcript();
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!(
                    "finch did not exit within {EXIT_DEADLINE:?} of `/exit`, so the \
                     input-ready state it reported does not actually act on input. \
                     Terminal was:\n{transcript}"
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The supervisor owns the process group; this only reaps the direct
        // child this test spawned, by handle, so a failing assertion cannot
        // leave a raw-mode `finch` behind and no pid this harness did not
        // create is ever signalled.
        let _ = self.child.kill();
        let _ = self.child.wait();

        // The reader exits when every pty slave descriptor is closed. A
        // grandchild that inherited the child's stdio -- an MCP server, say --
        // holds one open, and an unconditional `join` would then hang the test
        // process forever with no assertion and no deadline. Today's fixtures
        // configure no MCP servers and block daemon auto-spawn, so it cannot
        // happen; it is one config line away, and a hung harness is a worse
        // failure than a leaked blocked thread.
        if let Some(reader) = self.reader.take() {
            match self.reader_done.recv_timeout(Duration::from_secs(5)) {
                Ok(()) => {
                    let _ = reader.join();
                }
                Err(_) => {
                    eprintln!(
                        "startup pty harness: the terminal reader did not \
                         finish within 5s, so a descendant is still holding a \
                         pty slave open. Detaching the thread rather than \
                         hanging the test process."
                    );
                }
            }
        }
    }
}

/// Run the real REPL with terminal stdin but redirected output. This is the
/// production path for `finch >log`: main does not take the piped-query early
/// return because stdin is a terminal, while `Repl` correctly observes that
/// stdout is not one and selects its non-interactive banner.
#[derive(Debug)]
struct CapturedOutcome {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn bounded_capture(file: &mut std::fs::File, label: &str) -> String {
    let total = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => return format!("<{label} metadata unavailable: {error}>"),
    };
    if let Err(error) = file.seek(SeekFrom::Start(0)) {
        return format!("<{label} rewind failed: {error}; captured_bytes={total}>");
    }
    let mut bytes = Vec::new();
    if let Err(error) = file.take(CAPTURE_DIAGNOSTIC_LIMIT).read_to_end(&mut bytes) {
        return format!("<{label} read failed: {error}; captured_bytes={total}>");
    }
    let mut rendered = String::from_utf8_lossy(&bytes).into_owned();
    if total > CAPTURE_DIAGNOSTIC_LIMIT {
        rendered.push_str(&format!(
            "\n<{label} truncated: showing {} of {total} bytes>",
            CAPTURE_DIAGNOSTIC_LIMIT
        ));
    }
    rendered
}

fn run_bounded_command(command: &mut Command, deadline: Duration) -> CapturedOutcome {
    let mut stdout = tempfile::tempfile().expect("temporary stdout capture");
    let mut stderr = tempfile::tempfile().expect("temporary stderr capture");
    command
        .stdout(Stdio::from(
            stdout.try_clone().expect("clone stdout capture"),
        ))
        .stderr(Stdio::from(
            stderr.try_clone().expect("clone stderr capture"),
        ));
    let mut child = command.spawn().expect("spawn bounded child process");
    let expires = Instant::now() + deadline;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < expires => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let kill = child.kill();
                let wait = child.wait();
                let stdout = bounded_capture(&mut stdout, "stdout");
                let stderr = bounded_capture(&mut stderr, "stderr");
                panic!(
                    "bounded child exceeded {deadline:?} and was killed and reaped; \
                     kill={kill:?} wait={wait:?}\nstdout:\n{stdout}\nstderr:\n{stderr}"
                );
            }
            Err(error) => {
                let _ = child.kill();
                let wait_after_kill = child.wait();
                panic!("could not wait for bounded child: {error}; wait_after_kill={wait_after_kill:?}");
            }
        }
    };
    CapturedOutcome {
        status,
        stdout: bounded_capture(&mut stdout, "stdout"),
        stderr: bounded_capture(&mut stderr, "stderr"),
    }
}

fn run_with_redirected_output(fixture: &Fixture, logging: bool) -> CapturedOutcome {
    let pty = nix::pty::openpty(None, None).expect("open pty for terminal stdin");
    let mut stdout = tempfile::tempfile().expect("temporary stdout capture");
    let mut stderr = tempfile::tempfile().expect("temporary stderr capture");
    let mut command = isolated_finch_command(fixture);
    command
        .arg("--raw")
        .stdin(Stdio::from(pty.slave))
        .stdout(Stdio::from(
            stdout.try_clone().expect("clone stdout capture"),
        ));
    if logging {
        command.env("SHAMMAH_LOG", "1");
    }
    command.stderr(Stdio::from(
        stderr.try_clone().expect("clone stderr capture"),
    ));

    let mut child = command.spawn().expect("spawn finch with redirected output");
    let mut input = std::fs::File::from(pty.master);
    write!(input, "/exit\r").expect("send /exit to redirected repl");
    input.flush().expect("flush redirected repl input");

    let deadline = Instant::now() + READY_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => panic!("could not wait for redirected finch: {error}"),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let wait_after_kill = child.wait();
            let stdout_text = bounded_capture(&mut stdout, "stdout");
            let stderr_text = bounded_capture(&mut stderr, "stderr");
            panic!(
                "redirected finch did not consume `/exit` within {READY_DEADLINE:?}; \
                 this deadline detects a stuck real REPL, not startup latency. \
                 wait_after_kill={wait_after_kill:?}\nstdout:\n{stdout_text}\n\
                 stderr:\n{stderr_text}"
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };

    CapturedOutcome {
        status,
        stdout: bounded_capture(&mut stdout, "stdout"),
        stderr: bounded_capture(&mut stderr, "stderr"),
    }
}

fn command_payload(output: &CapturedOutcome) -> String {
    format!(
        "status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status, output.stdout, output.stderr
    )
}

fn assert_no_forbidden_identity_phrases(
    context: &str,
    status: std::process::ExitStatus,
    channels: &[(&str, &str)],
    forbidden_phrases: &[&str],
) {
    for (channel, text) in channels {
        let lowered = text.to_ascii_lowercase();
        for forbidden in forbidden_phrases {
            assert!(
                !lowered.contains(forbidden),
                "{context}: no user-visible channel may restore a stale or \
                 unevidenced product identity, case-insensitively. \
                 forbidden={forbidden:?} channel={channel} status={status:?}\n\
                 channel contents:\n{text}"
            );
        }
    }
}

#[test]
fn test_cli_help_reports_the_real_finch_identity() {
    let fixture = Fixture::new(0);
    let mut command = isolated_finch_command(&fixture);
    command.arg("--help");
    let help = run_bounded_command(&mut command, EXIT_DEADLINE);
    let help_payload = command_payload(&help);
    assert!(
        help.status.success(),
        "the built `finch --help` must exit successfully; {help_payload}"
    );
    assert_eq!(
        help.stdout.lines().next(),
        Some(EXPECTED_ABOUT),
        "the first line emitted by the real clap parser must be Finch's \
         evidence-backed description; {help_payload}"
    );
    assert_no_forbidden_identity_phrases(
        "the real `finch --help` process",
        help.status,
        &[("stdout", &help.stdout), ("stderr", &help.stderr)],
        FORBIDDEN_IDENTITY_PHRASES,
    );
}

#[test]
fn test_real_raw_and_redirected_startup_report_the_same_finch_identity() {
    let expected = format!("finch {} - {}", env!("CARGO_PKG_VERSION"), EXPECTED_ABOUT);

    let interactive_fixture = Fixture::new(0);
    let mut interactive = Session::spawn_with_args(&interactive_fixture, &["--raw"]);
    let _report = interactive.wait_for_report(&interactive_fixture);
    interactive.send_line("/exit");
    let interactive_status = interactive.wait_for_exit();
    let interactive_transcript = interactive.readable_transcript();
    assert!(
        interactive_status.success() && interactive_transcript.contains(&expected),
        "the real raw interactive REPL must visibly report the Cargo version \
         and evidence-backed product description before accepting input. \
         expected={expected:?} status={interactive_status:?} terminal:\n\
         {interactive_transcript}"
    );
    assert_no_forbidden_identity_phrases(
        "the real raw interactive REPL",
        interactive_status,
        &[("pty transcript", &interactive_transcript)],
        FORBIDDEN_IDENTITY_PHRASES,
    );

    let redirected_fixture = Fixture::new(0);
    let redirected = run_with_redirected_output(&redirected_fixture, false);
    assert!(
        redirected.status.success() && redirected.stderr.is_empty(),
        "redirected startup must preserve the established quiet-by-default \
         status contract when SHAMMAH_LOG is absent. outcome={redirected:?}"
    );
    assert_no_forbidden_identity_phrases(
        "the quiet redirected REPL",
        redirected.status,
        &[
            ("stdout", &redirected.stdout),
            ("stderr", &redirected.stderr),
        ],
        FORBIDDEN_IDENTITY_PHRASES,
    );

    let logged_fixture = Fixture::new(0);
    let logged = run_with_redirected_output(&logged_fixture, true);
    let expected_redirected = format!("# {expected} - non-interactive mode");
    assert!(
        logged.status.success()
            && logged
                .stderr
                .lines()
                .any(|line| line == format!("[STATUS] {expected_redirected}")),
        "redirected startup with SHAMMAH_LOG must preserve the status gate and \
         prefix while reporting the shared identity. \
         expected={expected_redirected:?} outcome={logged:?}"
    );
    assert_no_forbidden_identity_phrases(
        "the logged redirected REPL",
        logged.status,
        &[("stdout", &logged.stdout), ("stderr", &logged.stderr)],
        FORBIDDEN_IDENTITY_PHRASES,
    );
}

#[test]
fn test_isolated_subprocess_cannot_inherit_supervisor_authority() {
    const PROBE: &str = "FINCH_IDENTITY_ISOLATION_CHILD_PROBE";
    if std::env::var_os(PROBE).is_some() {
        let mut authority_variables: Vec<(String, String)> = std::env::vars()
            .filter(|(name, _)| {
                name.starts_with("FINCH_BRAIN_TEST_") || name.starts_with("FINCH_TEST_")
            })
            .collect();
        authority_variables.sort();
        assert_eq!(
            authority_variables,
            vec![(
                "FINCH_BRAIN_TEST_NO_AUTO_SPAWN".to_string(),
                "1".to_string()
            )],
            "an isolated test child may inherit only the fail-closed \
             no-auto-spawn restriction, never supervisor proof, passwords, \
             listener identities, or paths; inherited={authority_variables:?}"
        );

        for fd in SUPERVISOR_AUTHORITY_FDS {
            let result = unsafe { nix::libc::fcntl(*fd, nix::libc::F_GETFD) };
            let error = std::io::Error::last_os_error();
            assert_eq!(
                (result, error.raw_os_error()),
                (-1, Some(nix::libc::EBADF)),
                "supervisor authority descriptor {fd} must be closed in the \
                 child immediately before exec; fcntl_result={result} \
                 os_error={error}"
            );
        }
        return;
    }

    let fixture = Fixture::new(0);
    let mut command = isolated_child_command(
        Command::new(std::env::current_exe().expect("current integration-test executable")),
        &fixture,
    );
    command
        .args([
            "--exact",
            "test_isolated_subprocess_cannot_inherit_supervisor_authority",
            "--nocapture",
        ])
        .env(PROBE, "1");
    let output = run_bounded_command(&mut command, EXIT_DEADLINE);
    let payload = command_payload(&output);
    assert!(
        output.status.success(),
        "the subprocess-boundary authority probe must observe EBADF for every \
         supervisor descriptor and no authority environment. {payload}"
    );
}

/// Every phase and mark an interactive start records, in the order it records
/// them.
///
/// **This list is the presence assertion for the whole instrumentation.** At
/// `2e383b52` it named eight entries out of the twenty-two the report
/// contained, so nine instrumented phases had no presence assertion anywhere
/// and five of them -- `repl_construct` (the largest phase in the report),
/// `memory_open`, `tool_registry`, `mcp_connect` and `brain_register` -- could
/// be deleted outright with the suite at `4 passed; 0 failed`. Anything
/// instrumented on the fixture's path belongs here, or it is not covered.
///
/// Some phases the code contains are deliberately absent because this fixture
/// does not reach them, and adding them would assert a machine rather than the
/// code. They are enumerated once, in [`NOT_ON_THE_FIXTURE_PATH`], and
/// [`test_the_expected_order_names_every_instrumented_phase`] holds this list
/// to `finch::startup::ALL_PHASES` minus exactly those -- so an instrumented
/// phase can no longer be dropped from the production guard *and* from this
/// list and leave the suite green, which is how `program_runtime` could be
/// deleted at the previous tip.
///
/// `daemon_http_connect` *is* here and reports `category=disabled`, which is
/// the point: the phase set is a property of the code, not of whether a
/// daemon happens to be running.
fn expected_order() -> Vec<&'static str> {
    vec![
        "args_parse",
        "config_load",
        "tracing_init",
        "config_load",
        "threshold_router_load",
        "provider_graph_build",
        "metrics_logger_init",
        "daemon_http_connect",
        "repl_construct",
        "memory_open",
        "program_sync",
        "program_runtime",
        "tool_registry",
        "mcp_connect",
        "terminal_init",
        "terminal_owned",
        "ipc_connect",
        "generator_select",
        "provider_graph_build",
        "scheduler_init",
        "tool_definitions",
        "event_loop_construct",
        "input_captured",
        "tui_handoff",
        "generator_resolve",
        "brain_register",
        "startup_header",
        "header_queued",
        "license_notice",
        "config_load",
        "status_prime",
        "llm_worker_spawn",
        "input_ready",
    ]
}

/// Instrumented phases this fixture cannot reach, and why.
///
/// Every entry needs a reason that is about the fixture, not about the phase
/// being inconvenient to assert. An exclusion is how a real gap gets
/// laundered into a documented one, so the reason is the whole content of
/// this list.
const NOT_ON_THE_FIXTURE_PATH: &[(&str, &str)] = &[
    (
        "session_restore",
        "reached only with `--restore-session`, which no fixture passes",
    ),
    (
        "brain_attach",
        "needs a daemon; every fixture sets `use_daemon = false`",
    ),
    (
        "daemon_health_probe",
        "the `GET /health` probe inside `DaemonClient::connect`, which \
         `use_daemon = false` never enters. #364, instrument and reduce \
         interactive TUI time-to-ready, requires this sub-phase; it is \
         covered at its own boundary by \
         `test_the_health_probe_records_its_own_phase_and_says_how_it_ended` \
         in `src/daemon/spawn.rs`, because the supervisor's isolation gate \
         (`FINCH_BRAIN_TEST_ISOLATED=1`) refuses daemon discovery outright, \
         so no PTY fixture here can reach it",
    ),
    (
        "daemon_retry_backoff",
        "the unconditional two-second wait before the retry probe; \
         unreachable here for the same reason. Covered at its call site by \
         `test_the_connect_path_records_the_backoff_between_its_two_probes` \
         in `src/daemon/spawn.rs`, which drives the real connect path against \
         a synthetic PID file and a dead loopback port and asserts the phase \
         order probe/backoff/probe; and at the helper alone by \
         `test_the_retry_fallback_is_recorded_as_a_phase_of_its_own`. The \
         first is the load-bearing one: with only the second, inlining the \
         sleep at the call site left every suite green",
    ),
];

/// [`expected_order`] must name every instrumented phase but the excluded few.
///
/// This is the tie that was missing. At the previous tip a mutant could delete
/// `program_runtime` from its production guard *and* from `expected_order`
/// and the whole suite stayed green, because the expected list was a hand-kept
/// copy accountable to nothing. `finch::startup::ALL_PHASES` is generated from
/// the same declarations as the `PHASE_*` constants, so the list and the
/// instrumentation cannot drift without this failing.
///
/// Note what this does *not* do: it does not derive the privacy allowlist in
/// `test_the_startup_report_leaks_no_private_content`, which stays hand-kept
/// on purpose. Deriving that one would make a newly added phase name
/// self-approving, and catching new names is the whole job it does.
#[test]
fn test_the_expected_order_names_every_instrumented_phase() {
    use finch::startup::{ALL_MARKS, ALL_PHASES};

    let excluded: BTreeSet<&str> = NOT_ON_THE_FIXTURE_PATH
        .iter()
        .map(|(name, _)| *name)
        .collect();

    for (name, reason) in NOT_ON_THE_FIXTURE_PATH {
        assert!(
            ALL_PHASES.contains(name),
            "{name:?} is excluded from the expected order with the reason \
             {reason:?}, but no phase by that name is instrumented at all. An \
             exclusion naming a phase that does not exist excuses nothing and \
             hides the renaming of a phase that does. Instrumented phases are \
             {ALL_PHASES:?}"
        );
    }

    let expected: BTreeSet<&str> = expected_order().into_iter().collect();
    let should_appear: BTreeSet<&str> = ALL_PHASES
        .iter()
        .copied()
        .filter(|name| !excluded.contains(name))
        .collect();

    let missing: Vec<&&str> = should_appear.difference(&expected).collect();
    assert!(
        missing.is_empty(),
        "every instrumented phase this fixture reaches must appear in \
         `expected_order`, or it has no presence assertion anywhere and can be \
         deleted from production with this suite green. Missing {missing:?}. If \
         one of these is genuinely off the fixture's path, add it to \
         NOT_ON_THE_FIXTURE_PATH with the reason -- do not delete it from \
         `expected_order`. Instrumented phases are {ALL_PHASES:?}"
    );

    let phantom: Vec<&&str> = expected
        .difference(&should_appear)
        .filter(|name| !ALL_MARKS.contains(name))
        .collect();
    assert!(
        phantom.is_empty(),
        "`expected_order` names {phantom:?}, which is neither an instrumented \
         phase nor a mark. A misspelling here is satisfied by nothing and \
         asserts nothing. Instrumented phases are {ALL_PHASES:?} and marks are \
         {ALL_MARKS:?}"
    );

    for mark in ALL_MARKS {
        assert!(
            expected.contains(mark),
            "every instant mark must appear in `expected_order`; {mark:?} does \
             not. Marks are the instants that define what time-to-ready is \
             measured against, so an unasserted one can be moved or dropped \
             silently. Marks are {ALL_MARKS:?}"
        );
    }
}

/// Assert `expected` appears in `actual` in order, allowing other entries
/// between — and consuming a distinct position per expected entry, so a list
/// naming `config_load` three times requires three of them.
fn assert_in_order(expected: &[&str], actual: &[String], report: &str) {
    let mut remaining = actual.iter();
    let mut matched: Vec<&str> = Vec::with_capacity(expected.len());
    for want in expected {
        let found = remaining.any(|name| name == want);
        assert!(
            found,
            "startup must record {want:?} after the phases before it. The \
             report is missing it, or has it out of order, or -- when {want:?} \
             appears more than once in the expected order -- records it fewer \
             times than it should. Matched {matched:?} before failing here. \
             Recorded {actual:?} and the report was:\n{report}"
        );
        matched.push(want);
    }
}

/// Assert the report describes a startup that actually reached readiness.
///
/// The presence of the whole phase set is what does the work here. The
/// previous version asserted instead that no entry was recorded after
/// `input_ready`, with a comment claiming that was "what catches" a
/// `startup::ready()` moved earlier. It could not catch anything:
/// `Timeline::ready` renders the report inside the same lock in which it
/// stamps `ready_at`, so every record in the rendered string was pushed at an
/// offset `<= ready_at` by construction and records pushed afterwards never
/// enter the string at all. `at <= ready_at` was an identity, and a
/// `ready()` called at t0 before anything happens passed it.
///
/// What such a `ready()` does violate is this: the report it publishes names
/// none of the work that had not happened yet.
fn assert_reaches_ready(report: &Report, context: &str) {
    let names = report.names();
    assert!(
        report.position("input_ready").is_some(),
        "{context}: the report must record the input_ready mark, or nothing \
         defines time-to-ready; phases were {names:?} and the report was:\n{}",
        report.raw
    );
    assert!(
        report.time_to_ready_ms.is_some(),
        "{context}: input_ready was recorded but no total was reported; \
         report was:\n{}",
        report.raw
    );
    // This is the assertion that does the work, and the only one here that
    // has ever fired: a `startup::ready()` moved earlier publishes a report
    // that names none of the work which had not happened yet, and this fails
    // on the first phase it cannot find. Everything below is cheaper and
    // weaker, and says so.
    assert_in_order(&expected_order(), &names, &report.raw);

    // A rendering invariant, not an independent check on when `ready()` ran.
    // `Timeline::report` sorts by `started_at`, and `input_ready` is pushed at
    // the largest `started_at` in the timeline under the same lock that then
    // renders, so the sort key already puts it last in every tie. Kept because
    // it pins that rendering contract -- a future change to the sort key that
    // buried the readiness mark mid-report would break every reader of this
    // format -- but it cannot catch a mis-stamped `ready()`, and did not fire
    // once across fourteen mutant runs.
    assert_eq!(
        report.position("input_ready"),
        Some(report.entries.len() - 1),
        "{context}: input_ready must render as the last entry. It is stamped \
         at the largest offset in the timeline, so anything sorting after it \
         means the report's start-order rendering is broken, not that startup \
         did more work. Recorded {names:?} and the report was:\n{}",
        report.raw
    );

    // Likewise weak, and kept only as a guard on the printed field: `at_ms`
    // parses (or `Entry::at_ms` panics naming the line), and the one value
    // that parses while meaning nothing is exactly 0.
    let ready_at = report
        .find("input_ready")
        .expect("input_ready present")
        .at_ms(&report.raw);
    assert!(
        ready_at > 0.0,
        "{context}: input_ready is recorded at {ready_at} ms, i.e. at t0. A \
         readiness mark stamped before any startup work has happened reports a \
         total that measures nothing. Report was:\n{}",
        report.raw
    );
}

#[test]
fn test_interactive_startup_reports_every_phase_and_reaches_input_ready() {
    let fixture = Fixture::new(3);
    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);

    let names = report.names();
    assert_reaches_ready(&report, "well-formed 3-Brain inventory");

    // The four readiness instants are distinct and ordered. They are routinely
    // conflated, and conflating them is how "startup is fast" gets claimed on
    // the strength of raw mode being entered early.
    let ordering = [
        "terminal_owned",
        "input_captured",
        "header_queued",
        "input_ready",
    ];
    let positions: Vec<(&str, Option<usize>)> = ordering
        .iter()
        .map(|name| (*name, report.position(name)))
        .collect();
    for (name, position) in &positions {
        assert!(
            position.is_some(),
            "the {name:?} mark must be recorded; marks and phases seen were \
             {names:?} and the report was:\n{}",
            report.raw
        );
    }
    let indices: Vec<usize> = positions.iter().map(|(_, p)| p.unwrap()).collect();
    assert!(
        indices.windows(2).all(|pair| pair[0] < pair[1]),
        "the readiness marks must appear in the order {ordering:?}: the \
         terminal is owned, then keys are buffered, then the header is queued, \
         then keys are acted on. Recorded positions were {positions:?} \
         in {names:?}"
    );

    // Nesting, over the real binary rather than over a synthetic timeline.
    // `memory_open` runs inside `Repl::new`, so its duration is already
    // counted in `repl_construct`'s; a reader who sums the ms column without
    // the depth field double-counts it.
    let repl = report
        .find("repl_construct")
        .expect("repl_construct recorded");
    let memory = report.find("memory_open").expect("memory_open recorded");
    assert_eq!(
        repl.depth,
        Some(0),
        "repl_construct is enclosed by nothing; entry was {repl:?} and the \
         report was:\n{}",
        report.raw
    );
    assert!(
        memory.depth.is_some_and(|depth| depth >= 1),
        "memory_open runs inside Repl::new, so the report must mark it as \
         enclosed -- otherwise its milliseconds are counted twice. Entry was \
         {memory:?} and the report was:\n{}",
        report.raw
    );

    // A phase's `count` is what it carried to completion, not what it
    // attempted. This fixture makes both zero cases unambiguous: no daemon, so
    // no Brain is registered; no MCP servers configured, so none connect. A
    // count of attempts would report 1 and 0 respectively and read, to anyone
    // holding the report, as one Brain registered.
    for (name, expected_count, expected_category) in [
        ("brain_register", 0u64, "offline"),
        ("mcp_connect", 0u64, "none_configured"),
    ] {
        let entry = report
            .find(name)
            .unwrap_or_else(|| panic!("{name} recorded; report was:\n{}", report.raw));
        assert_eq!(
            entry.count,
            Some(expected_count),
            "{name} must count what it carried to completion, not what it \
             attempted: this fixture disables the daemon and configures no MCP \
             server, so nothing was registered or connected. Entry was \
             {entry:?} and the report was:\n{}",
            report.raw
        );
        assert_eq!(
            entry.category.as_deref(),
            Some(expected_category),
            "and the category is where 'it was asked to do something and could \
             not' belongs. Entry was {entry:?} and the report was:\n{}",
            report.raw
        );
    }
    let sync = report.find("program_sync").expect("program_sync recorded");
    assert_eq!(
        sync.category.as_deref(),
        Some("complete"),
        "program_sync must say whether every root it attempted actually \
         synced; its count is a count of successes and means nothing without \
         it. Entry was {sync:?} and the report was:\n{}",
        report.raw
    );

    // This is the ordinary no-argument TUI path. It does not call
    // `Repl::run`, so the raw-mode banner cannot stand in for this assertion.
    session.send_line("/exit");
    let status = session.wait_for_exit();
    let terminal = session.readable_transcript();
    let expected_version = format!("finch v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        status.success()
            && terminal.contains(&expected_version)
            && terminal.contains(EXPECTED_ABOUT),
        "the default no-argument TUI header must visibly identify Finch with \
         the Cargo package version and evidence-backed description. \
         expected_version={expected_version:?} expected_description={EXPECTED_ABOUT:?} \
         status={status:?} terminal:\n{terminal}"
    );
    assert_no_forbidden_identity_phrases(
        "the default no-argument TUI",
        status,
        &[("pty transcript", &terminal)],
        STALE_IDENTITY_PHRASES,
    );
}

#[test]
fn test_the_startup_report_states_how_much_time_no_phase_covers() {
    // At `2e383b52` roughly 6.4 ms of a 13.99 ms total was covered by no phase
    // and the report said nothing about it, so the PR diagnosed the gap by
    // reading code -- and got it wrong, attributing it to daemon round-trips
    // that run inside `brain_register`, after the gap, at 0.003 ms. A report
    // that does not state its own coverage invites exactly that.
    let fixture = Fixture::new(3);
    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);

    let total = report.time_to_ready_ms.expect("a total");
    let accounted = report.accounted_ms.unwrap_or_else(|| {
        panic!(
            "the report must state how much of the total its phases account \
             for. Without it a reader has to diff the at_ms column by hand to \
             discover that half of startup is unattributed. Report was:\n{}",
            report.raw
        )
    });
    let unaccounted = report.unaccounted_ms.unwrap_or_else(|| {
        panic!(
            "the report must state the remainder no phase covers, by name, \
             rather than leaving it to be inferred. Report was:\n{}",
            report.raw
        )
    });

    // `accounted_ms` must be the sum of the *depth-0* phases and nothing
    // else. Recomputed here from the entries the report itself printed, which
    // is the only form of this check that can fail.
    //
    // The obvious assertion cannot. Production computes `unaccounted_ms` as
    // `total_ms - accounted_ms`, so `accounted + unaccounted == total` is an
    // identity: it holds for any `accounted_ms` whatsoever, including one
    // computed from the wrong set of phases. A mutant that made `accounted_ms`
    // count nested phases as well (`depth <= 1`) -- double-counting every
    // child inside `repl_construct` and `event_loop_construct`, and reporting
    // a coverage figure larger than the total it is a fraction of -- passed
    // this whole file with that identity as the only coverage assertion.
    //
    // Recomputing is structural, not a threshold: it compares two numbers the
    // report printed against each other, and holds equally on an idle machine
    // and a loaded one.
    let recomputed: f64 = report
        .entries
        .iter()
        .filter(|entry| entry.kind == "phase" && entry.depth == Some(0))
        .map(|entry| entry.ms(&report.raw))
        .sum();
    let outermost: Vec<&str> = report
        .entries
        .iter()
        .filter(|entry| entry.kind == "phase" && entry.depth == Some(0))
        .map(|entry| entry.name.as_str())
        .collect();
    // Each `ms` is printed to three decimals, so the recomputed sum can differ
    // from the internally-summed figure by half a unit in the last place per
    // term. A tolerance, not a threshold: it bounds the printing, not the
    // machine.
    let rounding = 0.001 * (outermost.len() as f64 + 1.0);
    assert!(
        (recomputed - accounted).abs() <= rounding,
        "accounted_ms must be the sum of the phases the report marks depth=0, \
         and nothing else. The report says accounted_ms={accounted} but its \
         own {} depth-0 phases sum to {recomputed} (tolerance {rounding} for \
         three-decimal printing). The depth-0 phases were {outermost:?}. A \
         coverage figure that counts nested phases double-counts every child \
         and can exceed the total it claims to be part of. Report was:\n{}",
        outermost.len(),
        report.raw
    );
    // And the printed arithmetic must be self-consistent. Weak on its own --
    // production derives `unaccounted_ms` by subtraction, so this can only
    // catch a formatting or parsing fault, never a wrong `accounted_ms`. It
    // is kept for that, and claims nothing more.
    assert!(
        (accounted + unaccounted - total).abs() < 0.01,
        "the two printed figures must add back to the printed total, or one \
         of the three was mangled on the way out: {accounted} + {unaccounted} \
         != {total}. Report was:\n{}",
        report.raw
    );
    assert!(
        accounted > 0.0,
        "the phases must account for something; accounted_ms was {accounted} \
         and the report was:\n{}",
        report.raw
    );
    assert!(
        !outermost.is_empty(),
        "the report must mark at least one phase depth=0, or the depth column \
         is not reporting nesting and accounted_ms is the sum of nothing. \
         Report was:\n{}",
        report.raw
    );

    eprintln!(
        "startup coverage on this run: total {total:.3} ms, accounted \
         {accounted:.3} ms, unaccounted {unaccounted:.3} ms"
    );
}

#[test]
fn test_startup_acts_on_input_once_it_reports_input_ready() {
    // input_ready is a claim that a typed key is acted upon. Prove it by
    // typing one, rather than trusting the mark.
    let fixture = Fixture::new(2);
    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);
    assert_reaches_ready(&report, "input-acts-on-keys fixture");

    session.send_line("/exit");
    let status = session.wait_for_exit();

    assert!(
        status.success(),
        "after reporting input_ready, `/exit` typed at the terminal must be \
         acted upon and exit cleanly; status was {status:?} and the terminal \
         was:\n{}",
        session.transcript()
    );
}

#[test]
fn test_startup_instrumentation_says_nothing_on_the_user_s_terminal() {
    // #364: "No new always-on cost." The instrumentation's tracing events run
    // on every start for every user, and `OutputManagerLayer` suppresses
    // internal INFO only for the module prefixes it lists -- `finch::cli`,
    // `tools`, `generators`, `models`, `local`, `server`. `finch::startup` is
    // not among them, so an `info!` from this module is routed to
    // `output_status!` and painted into the TUI, after `EventLoop::run`'s
    // `output_manager.clear()` so nothing removes it. With `MessageVisitor`
    // discarding every field but `message`, what the user got was a
    // contentless `[startup] finch interactive startup ready` on every launch.
    //
    // This is the surface that was missing: there was no test on the terminal,
    // which is why it shipped.
    let fixture = Fixture::new(2);
    let mut session = Session::spawn(&fixture);
    let _report = session.wait_for_report(&fixture);

    // Exit cleanly first, so the transcript holds everything the process will
    // ever write. Synchronising on the child's death rather than on a sleep.
    session.send_line("/exit");
    let status = session.wait_for_exit();
    let readable = session.readable_transcript();

    assert!(
        !readable.contains("finch interactive startup ready"),
        "the readiness event must not reach the user's terminal. It carries \
         no information there -- `time_to_ready_ms` is a structured field and \
         MessageVisitor drops it -- so every user sees a line that says only \
         that something called startup happened. Exit status was {status:?} \
         and the terminal read:\n{readable}"
    );

    // Nothing at all from this module reaches the terminal on an ordinary
    // launch -- not "nothing bad": nothing. The over-budget warning is the
    // sole exception, it does not fire on this fixture, and it is asserted on
    // its own in the test below with the budget forced. This loop is a
    // belt-and-braces sweep, and on this fixture it inspects an empty set.
    let phase_names: Vec<&str> = expected_order();
    for line in session.readable_lines() {
        if !line.contains("[startup]") {
            continue;
        }
        assert!(
            phase_names.iter().any(|name| line.contains(name)),
            "a startup line on the user's terminal must name the phase it is \
             about; 'startup phase exceeded its budget' tells an operator only \
             that startup was slow. Offending line was {line:?} and the \
             terminal read:\n{readable}"
        );
    }
}

/// #364, "Instrument and reduce Finch interactive TUI time-to-ready": "Slow
/// phases must be visible and actionable, not silently absorbed. A phase that
/// exceeds its budget says so, by name."
///
/// That requirement is about the operator's terminal, and until now nothing
/// drove it there. The sweep in the test above is vacuous on an ordinary
/// launch: no phase on this fixture comes near the 150 ms budget, so there is
/// no `[startup]` line to inspect and the assertion passes over an empty set.
/// Reverting the warning to the contentless "startup phase exceeded its
/// budget" it originally shipped as left this whole file green.
///
/// The honest way to make a real phase exceed a real budget is to lower the
/// budget, not to load the machine: `FINCH_STARTUP_SLOW_BUDGET_MS=0` puts
/// every phase over budget, so the warning fires from the production path,
/// through the real `OutputManagerLayer`, onto a real terminal. Nothing here
/// asserts how long anything took -- only that what the user is shown names
/// the phase and states its duration.
#[test]
fn test_an_over_budget_phase_names_itself_on_the_user_s_terminal() {
    let fixture = Fixture::new(2);
    let mut session =
        Session::spawn_with_env(&fixture, &[], &[("FINCH_STARTUP_SLOW_BUDGET_MS", "0")]);
    let report = session.wait_for_report(&fixture);

    // Every phase is over a zero budget, so the report must mark them SLOW. If
    // it does not, the override never reached the binary and the terminal
    // assertions below would be vacuous for that reason instead.
    let slow_in_report: Vec<&str> = report
        .entries
        .iter()
        .filter(|entry| entry.slow)
        .map(|entry| entry.name.as_str())
        .collect();
    assert!(
        !slow_in_report.is_empty(),
        "with FINCH_STARTUP_SLOW_BUDGET_MS=0 every phase is over budget and \
         the report must mark them SLOW. None were, so the override did not \
         take effect and this test would prove nothing about the warning. \
         Report was:\n{}",
        report.raw
    );

    session.send_line("/exit");
    let status = session.wait_for_exit();
    let lines = session.readable_lines();
    let readable = session.readable_transcript();

    let startup_lines: Vec<&String> = lines.iter().filter(|l| l.contains("[startup]")).collect();
    assert!(
        !startup_lines.is_empty(),
        "an over-budget phase must be visible to the operator, not silently \
         absorbed. With every phase over budget the terminal carried no \
         [startup] line at all. Exit status was {status:?}; the report marked \
         {slow_in_report:?} SLOW and the terminal read:\n{readable}"
    );

    let known: Vec<&str> = expected_order();
    let named: Vec<&&String> = startup_lines
        .iter()
        .filter(|line| known.iter().any(|phase| line.contains(phase)))
        .collect();
    assert!(
        !named.is_empty(),
        "#364 requires that a phase exceeding its budget 'says so, by name'. \
         None of the {} startup lines on the terminal named a phase; \
         'startup phase exceeded its budget' tells an operator only that \
         startup was slow, and that is what shipped. The lines were \
         {startup_lines:?} and the terminal read:\n{readable}",
        startup_lines.len()
    );

    // And the duration, parsed rather than substring-matched, so a line that
    // merely happens to contain digits does not pass. "Slow" without "how
    // slow" is not actionable either.
    let with_duration: Vec<&&&String> = named
        .iter()
        .filter(|line| {
            line.split_once(" ms, over its ")
                .and_then(|(head, _)| head.rsplit(' ').next())
                .and_then(|value| value.parse::<f64>().ok())
                .is_some()
        })
        .collect();
    assert!(
        !with_duration.is_empty(),
        "the over-budget warning must carry how long the phase took, in the \
         message -- MessageVisitor drops every other field, so a duration \
         recorded only as a structured field never reaches this terminal. \
         Lines that named a phase were {named:?} and the terminal \
         read:\n{readable}"
    );
}

#[test]
fn test_raw_mode_startup_publishes_a_report() {
    // `--raw` and `--no-tui` never reach `EventLoop::run`; they fall back to
    // the rustyline REPL in `Repl::run_with_initial_prompt`. Before the
    // `startup::ready()` call there, `FINCH_STARTUP_TIMINGS=... finch --raw`
    // produced no file and no diagnostic -- it silently measured nothing --
    // and that fix shipped with no coverage at all.
    let fixture = Fixture::new(2);
    let mut session = Session::spawn_with_args(&fixture, &["--raw"]);
    let report = session.wait_for_report(&fixture);

    assert!(
        report.time_to_ready_ms.is_some(),
        "the raw/no-TUI REPL must publish a total like the TUI path does, or \
         `--raw` is unmeasurable and says so nowhere. Report was:\n{}",
        report.raw
    );
    let names = report.names();
    assert!(
        report.position("input_ready").is_some(),
        "and it must record the readiness mark that defines the total; \
         recorded {names:?} and the report was:\n{}",
        report.raw
    );
    // The raw path skips the TUI, so it legitimately has no `terminal_init`,
    // no `terminal_owned` and none of the event-loop phases. What it must
    // still show is the shared prologue.
    assert_in_order(
        &[
            "args_parse",
            "config_load",
            "tracing_init",
            "config_load",
            "threshold_router_load",
            "provider_graph_build",
            "metrics_logger_init",
            "daemon_http_connect",
            "repl_construct",
            "memory_open",
            "ipc_connect",
            "input_ready",
        ],
        &names,
        &report.raw,
    );
    assert!(
        report.position("terminal_owned").is_none(),
        "the raw path never enters TUI raw mode, so a terminal_owned mark here \
         would mean the report is describing a start that did not happen. \
         Recorded {names:?}"
    );
}

#[test]
fn test_startup_survives_a_malformed_and_half_written_brain_inventory() {
    let fixture = Fixture::with_inventory(6, |root| {
        // A name the Brain validator rejects.
        std::fs::create_dir_all(root.join("has spaces")).expect("hostile name");
        // A regular file where a Brain directory is expected.
        std::fs::write(root.join("loose.json"), "{}").expect("loose file");
        // An interrupted write: a torn final line.
        let torn = root.join("torn");
        std::fs::create_dir_all(&torn).expect("torn Brain");
        std::fs::write(torn.join("events.jsonl"), "{\"seq\":1}\n{\"seq\":2,\"sen")
            .expect("torn log");
        // A Brain directory that exists and holds nothing at all.
        std::fs::create_dir_all(root.join("empty")).expect("empty Brain");
        // Unparseable metadata.
        let corrupt = root.join("corrupt");
        std::fs::create_dir_all(&corrupt).expect("corrupt Brain");
        std::fs::write(corrupt.join("metadata.json"), "{not json").expect("corrupt metadata");
        std::fs::write(corrupt.join("events.jsonl"), "\u{0}\u{0}\u{0}").expect("corrupt log");
    });

    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);

    // Both runs are held to the full phase list independently, so this is not
    // two identically-mutated reports agreeing with each other -- which is
    // what an equality-only assertion between two runs of the same binary
    // reduces to.
    assert_reaches_ready(&report, "malformed and half-written inventory");

    let clean = Fixture::new(6);
    let mut clean_session = Session::spawn(&clean);
    let clean_report = clean_session.wait_for_report(&clean);
    drop(clean_session);
    assert_reaches_ready(&clean_report, "well-formed 6-Brain inventory");

    assert_eq!(
        report.names(),
        clean_report.names(),
        "a malformed or half-written Brain on disk must not change what the \
         interactive frontend does before the first frame -- it does not read \
         them, and a startup that degrades here is reading something it should \
         not be. Hostile inventory gave:\n{}\nClean inventory gave:\n{}",
        report.raw,
        clean_report.raw
    );
}

#[test]
fn test_startup_records_the_same_phases_at_zero_brains_and_at_four_hundred() {
    // #364 asks for "zero, a few, hundreds". `2e383b52` used 2, 3, 4 and 6 and
    // its module doc claimed "nothing and four hundred", so the two ends of
    // the range -- the two that would actually show enumeration -- were the
    // ones missing.
    let empty = Fixture::new(0);
    let mut empty_session = Session::spawn(&empty);
    let empty_report = empty_session.wait_for_report(&empty);
    drop(empty_session);
    assert_reaches_ready(&empty_report, "empty Brain inventory");

    let many = Fixture::new(400);
    let mut many_session = Session::spawn(&many);
    let many_report = many_session.wait_for_report(&many);
    drop(many_session);
    assert_reaches_ready(&many_report, "four-hundred-Brain inventory");

    assert_eq!(
        empty_report.names(),
        many_report.names(),
        "the frontend must do the same work before the first frame whether \
         the Brain root holds nothing or four hundred Brains: it registers its \
         own home Brain and never enumerates the root. A phase appearing only \
         in the large inventory is the frontend reading the root. Zero \
         gave:\n{}\nFour hundred gave:\n{}",
        empty_report.raw,
        many_report.raw
    );

    // And the one phase that touches a Brain reports a count that does not
    // scale with the inventory.
    for (label, report) in [("zero", &empty_report), ("four hundred", &many_report)] {
        let register = report
            .find("brain_register")
            .expect("brain_register recorded");
        assert!(
            register.count.is_some_and(|count| count <= 1),
            "with {label} Brains on disk the frontend must still touch at most \
             its own; brain_register reported {register:?} and the report \
             was:\n{}",
            report.raw
        );
    }
}

#[test]
fn test_the_startup_report_leaks_no_private_content() {
    // The report is a diagnostic that ends up in bug reports. It must carry
    // counts and static phase names, never a Brain name, a path, or anything
    // read out of the user's home.
    let fixture = Fixture::with_inventory(4, |root| {
        std::fs::create_dir_all(root.join("secret-brain-name")).expect("named Brain");
        std::fs::write(
            root.join("secret-brain-name").join("events.jsonl"),
            "{\"seq\":1,\"kind\":{\"Prompt\":{\"text\":\"unlisted-prompt-text\"}}}\n",
        )
        .expect("named Brain log");
    });

    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);

    // Defence in depth rather than a restatement of the type system: these
    // become reachable the moment anyone adds a `String` or `PathBuf` field to
    // `PhaseDetail`, which is when a reviewer is least likely to notice. The
    // whitelist below is the assertion with real teeth.
    let forbidden = [
        "secret-brain-name",
        "unlisted-prompt-text",
        "brain-0000",
        "/Users",
        "/private",
        ".finch",
    ];
    for needle in forbidden {
        assert!(
            !report.raw.contains(needle),
            "the startup report must not contain {needle:?}: it is a \
             diagnostic that gets pasted into bug reports, so it carries phase \
             names, durations and counts and nothing read from the user's \
             home. Report was:\n{}",
            report.raw
        );
    }

    // And positively: every token that is not a number is a known static name.
    let known: BTreeSet<&str> = [
        "phase",
        "mark",
        "SLOW",
        "args_parse",
        "config_load",
        "tracing_init",
        "threshold_router_load",
        "provider_graph_build",
        "metrics_logger_init",
        "daemon_http_connect",
        "repl_construct",
        "session_restore",
        "ipc_connect",
        "terminal_init",
        "memory_open",
        "program_sync",
        "program_runtime",
        "tool_registry",
        "mcp_connect",
        "brain_register",
        "brain_attach",
        "generator_select",
        "scheduler_init",
        "tool_definitions",
        "event_loop_construct",
        "tui_handoff",
        "generator_resolve",
        "startup_header",
        "license_notice",
        "status_prime",
        "llm_worker_spawn",
        "terminal_owned",
        "input_captured",
        "header_queued",
        "input_ready",
        "connected",
        "unavailable",
        "disabled",
        "raw_mode",
        "failed",
        "registered",
        "attached",
        "offline",
        "cached",
        "persisted",
        "complete",
        "partial",
        "none_configured",
        "all_connected",
        "debug_logging_probe",
        "license_notice",
        "daemon_api_key",
        "profile_rebuild",
    ]
    .into_iter()
    .collect();
    for line in report.raw.lines() {
        if !line.starts_with("phase ") && !line.starts_with("mark ") {
            continue;
        }
        for token in line.split_whitespace() {
            let value = token.split_once('=').map(|(_, v)| v).unwrap_or(token);
            let numeric =
                !value.is_empty() && value.chars().all(|c| c.is_ascii_digit() || c == '.');
            assert!(
                numeric || known.contains(value),
                "unrecognised token {token:?} in startup report line {line:?}. \
                 Every value must be a number or a compile-time constant; a \
                 token that is neither is how a Brain name or a prompt reaches \
                 a diagnostic. If this is a legitimately new phase, add it to \
                 the list in this test. Full report was:\n{}",
                report.raw
            );
        }
    }
}

/// The repeatable before/after benchmark #364 asks for.
///
/// `#[ignore]`, so it is never a correctness gate. #364, "Instrument and
/// reduce Finch interactive TUI time-to-ready", asks for the benchmark to be
/// "kept separate from the correctness gates" and for this suite's assertions
/// to be structural rather than absolute wall-clock thresholds; this reports
/// timings and asserts none. (That requirement lives in #364, whose precedent
/// is #242's four timing attempts -- the last repaired by `a0ea2c64` -- and
/// not in `AGENTS.md`; see this file's header.)
///
/// It lives here rather than in a shell script because the shell version
/// launched the TUI itself through `script(1)` and cleaned up with
/// `pkill -f "$binary"`, which matches any process on the machine whose command
/// line contains that path -- exactly the ownership violation `AGENTS.md`
/// names ("launchers never signal pids; the supervisor terminates, proves
/// quiescence, and reaps the group"). Reusing `Session` gets the supervisor's
/// process discipline, the constructed credential isolation, and the artifact
/// synchronisation for free.
///
/// ```text
/// scripts/bench_startup_time_to_ready.sh [runs] [brains]
/// ```
#[test]
#[ignore = "benchmark: reports timings, asserts no wall-clock property"]
fn bench_startup_time_to_ready() {
    fn env_usize(name: &str, default: usize) -> usize {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    let runs = env_usize("FINCH_BENCH_STARTUP_RUNS", 15);
    let brains = env_usize("FINCH_BENCH_STARTUP_BRAINS", 113);
    assert!(runs > 0, "a benchmark needs at least one run");

    let mut samples: Vec<f64> = Vec::with_capacity(runs);
    let mut last_report = String::new();
    for run in 1..=runs {
        let fixture = Fixture::new(brains);
        let mut session = Session::spawn(&fixture);
        let report = session.wait_for_report(&fixture);
        drop(session);
        let total = report
            .time_to_ready_ms
            .expect("a benchmarked run must report a total");
        eprintln!("run {run:2}/{runs}: {total:.3} ms");
        samples.push(total);
        last_report = report.raw;
    }

    samples.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in a timing"));
    let n = samples.len();
    let median = if n % 2 == 1 {
        samples[n / 2]
    } else {
        (samples[n / 2 - 1] + samples[n / 2]) / 2.0
    };
    // Nearest-rank p90: the smallest sample at or above the 90th percentile.
    // The shell version used `int(n * 0.9)`, which floors -- at 15 samples it
    // reported sample 13 as the p90 when the nearest rank is 14.
    let p90_rank = ((n as f64) * 0.9).ceil().max(1.0) as usize;
    let p90 = samples[p90_rank - 1];

    let commit = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let dirty = Command::new("git")
        .args(["diff", "--quiet"])
        .status()
        .map(|status| !status.success())
        .unwrap_or(false);
    let uname = Command::new("uname")
        .arg("-srm")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();

    eprintln!(
        "\n#364 startup time-to-ready\n\
         commit          {commit}{}\n\
         runs            {n}\n\
         brain inventory {brains} synthetic Brains, 8 events each\n\
         median          {median:.3} ms\n\
         p90 (rank {p90_rank}/{n})  {p90:.3} ms\n\
         min / max       {:.3} / {:.3} ms\n\
         machine         {uname}\n\
         \n\
         scope: frontend time-to-ready only. Every fixture sets\n\
         use_daemon = false, so GET /health is not on this path. For the\n\
         /health enumeration cost run bench_list_versus_count_over_a_realistic\n\
         _brain_root with FINCH_BENCH_BRAIN_ROOT set to a copy of a real Brain\n\
         root.\n\
         \n\
         phase breakdown (last run)\n{last_report}",
        if dirty { " (dirty worktree)" } else { "" },
        samples[0],
        samples[n - 1],
    );
}
