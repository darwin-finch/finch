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
//! `AGENTS.md` forbids wall-clock threshold assertions, and the lesson is
//! concrete: `a0ea2c64` ("assert hydration state, not a wall-clock ratio")
//! replaced the last of four such assertions on #242, one of which reached
//! 35/35 green CI while depending on the machine being busy. So nothing here
//! asserts a duration, a ratio, or a deadline as a *property*. The timings are
//! recorded and reported; what is asserted is structure -- which phases ran, in
//! what order, how they nest, how many things each carried to completion, and
//! that the set does not change with the size or the sanity of the Brain
//! inventory on disk.
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
use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Failure deadline for reaching readiness. Not a measurement: a test that
/// takes longer than this has hung, and the assertion says what was on the
/// terminal when it did.
const READY_DEADLINE: Duration = Duration::from_secs(90);

/// Failure deadline for a clean exit after `/exit`.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

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

        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .args(args)
            .stdin(Stdio::from(slave_in))
            .stdout(Stdio::from(slave_out))
            .stderr(Stdio::from(slave_err))
            .env("HOME", &fixture.home)
            .env("XDG_CONFIG_HOME", fixture.home.join(".config"))
            .env("XDG_CACHE_HOME", fixture.home.join(".cache"))
            .env("XDG_DATA_HOME", fixture.home.join(".local/share"))
            .env("HF_HOME", fixture.home.join(".cache/huggingface"))
            .env("TERM", "xterm-256color")
            .env("FINCH_STARTUP_TIMINGS", &fixture.timings)
            // Blocks daemon discovery, reuse and auto-spawn without needing a
            // supervisor proof (`src/daemon/spawn.rs`).
            .env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1")
            // No provider credential can be picked up from the developer's
            // environment, so no request can be made even in principle.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("XAI_API_KEY")
            .env_remove("GEMINI_API_KEY")
            .env_remove("GOOGLE_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("RUST_LOG")
            // Constructed isolation, not incidental. Under the mandated
            // launcher the parent carries the supervisor's whole environment,
            // and the child would inherit a proof descriptor it was not given,
            // a Brain root that no longer matches its HOME, and a socket path
            // belonging to another process. None of it is read on the
            // interactive path today, so the fixture's determinism rested on
            // that reachability argument rather than on a clean environment.
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

    /// The transcript with terminal control sequences removed and runs of
    /// whitespace collapsed, so an assertion about what a user can *read* is
    /// not defeated by the renderer having placed a colour escape or a cursor
    /// move between two words of the same message.
    fn readable_transcript(&self) -> String {
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
        out.split_whitespace().collect::<Vec<_>>().join(" ")
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
/// Three phases the code contains are deliberately absent because this fixture
/// does not reach them, and adding them would assert a machine rather than the
/// code: `session_restore` (no `--restore-session`), `brain_attach` and the
/// fourth `config_load`, `category=daemon_api_key` (both need a daemon, and
/// every fixture sets `use_daemon = false`). `daemon_http_connect` *is* here
/// and reports `category=disabled`, which is the point: the phase set is a
/// property of the code, not of whether a daemon happens to be running.
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
    assert_in_order(&expected_order(), &names, &report.raw);
    assert_eq!(
        report.position("input_ready"),
        Some(report.entries.len() - 1),
        "{context}: input_ready must be the last entry in the report. It is \
         stamped immediately before the loop that consumes keys, so anything \
         recorded after it is startup work the total does not include. \
         Recorded {names:?} and the report was:\n{}",
        report.raw
    );
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

    // Structural, not a threshold: the two figures must partition the total.
    // No assertion here on how *large* the unaccounted share is -- that is a
    // wall-clock property of a machine, and `AGENTS.md` forbids asserting one.
    assert!(
        (accounted + unaccounted - total).abs() < 0.01,
        "accounted_ms + unaccounted_ms must be the reported total, or the \
         coverage figure is not a coverage figure: {accounted} + \
         {unaccounted} != {total}. Report was:\n{}",
        report.raw
    );
    assert!(
        accounted > 0.0,
        "the phases must account for something; accounted_ms was {accounted} \
         and the report was:\n{}",
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

    // The one startup line a user may legitimately see is an over-budget
    // warning, and #364 requires that it "says so, by name". This does not
    // usually fire; when a loaded machine makes it fire, it must be
    // actionable rather than being "startup was slow".
    let phase_names: Vec<&str> = expected_order();
    for line in readable.split(" ⚠️ ").chain(readable.lines()) {
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
/// `#[ignore]`, so it is never a correctness gate: `AGENTS.md` forbids
/// asserting a wall-clock startup property, and this asserts none. It reports.
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
