//! Interactive startup, driven through a real PTY (#364).
//!
//! # Why a PTY
//!
//! `Repl` decides it is interactive with `io::stdout().is_terminal()`
//! (`src/cli/repl.rs:577`). Every existing attempt to test startup misses the
//! TUI entirely for that reason: `tests/tui_integration_test.rs` uses
//! `Stdio::piped()`, so the binary takes the non-interactive branch at
//! `src/main.rs:898`, and `scripts/test_tui_debug.sh` redirects stdout to a
//! file. Nothing in the repository has ever executed the interactive startup
//! path under test. These tests do, by giving the child a pty slave for its
//! three standard descriptors.
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
//! What they do prove about the Brain inventory is narrower and still worth
//! having: the frontend hydrates zero Brains before the first frame, measured
//! by a counter inside `BrainStore::ensure_loaded` that no phase can bypass,
//! at inventories of nothing and of four hundred.
//!
//! # Why the assertions have no clock in them
//!
//! Issue #242 spent four assertions on wall-clock startup properties. Three
//! were wrong; one of the wrong ones reached 35/35 green CI while depending on
//! the machine being busy. So nothing here asserts a duration, a ratio, or a
//! deadline as a *property*. The timings are recorded and reported; what is
//! asserted is structure -- which phases ran, in what order, how many things
//! each handled, and that the set does not change with the size of the Brain
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
//! like a call to X.)
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

/// A parsed startup report.
#[derive(Debug, Clone)]
struct Report {
    raw: String,
    entries: Vec<Entry>,
    time_to_ready_ms: Option<f64>,
}

impl Report {
    fn parse(raw: &str) -> Self {
        let mut entries = Vec::new();
        let mut time_to_ready_ms = None;
        for line in raw.lines() {
            if let Some(value) = line.strip_prefix("time_to_ready_ms=") {
                time_to_ready_ms = value.parse::<f64>().ok();
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
            // supervisor proof (`src/daemon/spawn.rs:34`).
            .env("FINCH_BRAIN_TEST_NO_AUTO_SPAWN", "1")
            // No provider credential can be picked up from the developer's
            // environment, so no request can be made even in principle.
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("SHAMMAH_DEBUG")
            .env_remove("RUST_LOG");

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

    /// Wait for the process to publish its startup report.
    ///
    /// Synchronisation is on the artifact, not on a duration: the file appears
    /// exactly when `startup::ready()` runs, immediately before the event loop
    /// begins consuming keys.
    fn wait_for_report(&mut self, fixture: &Fixture) -> Report {
        let deadline = Instant::now() + READY_DEADLINE;
        loop {
            if let Ok(raw) = std::fs::read_to_string(&fixture.timings) {
                // The write is a single `fs::write`, but read the terminating
                // line before trusting a partial view on any platform.
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
        // child this test spawned, so a failing assertion cannot leave a
        // raw-mode `finch` behind.
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

/// Every phase and mark an interactive start must record, in order.
///
/// `daemon_http_connect` is present but `disabled` in these fixtures, so the
/// list is the same whether or not a daemon exists -- which is the point: the
/// phase set is a property of the code, not of the machine.
fn expected_prefix() -> Vec<&'static str> {
    vec![
        "args_parse",
        "config_load",
        "tracing_init",
        "config_load",
        "threshold_router_load",
        "provider_graph_build",
        "metrics_logger_init",
        "daemon_http_connect",
    ]
}

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
    // input_ready must be the *latest* instant in the timeline, not merely the
    // last line. Moving `startup::ready()` earlier in `EventLoop::run` -- above
    // the Brain registration, say -- would understate time-to-ready while
    // still producing a well-formed report; this is what catches that.
    let ready_at: f64 = report
        .find("input_ready")
        .and_then(|entry| entry.at_ms.parse().ok())
        .expect("input_ready carries an offset");
    for entry in &report.entries {
        let at: f64 = entry.at_ms.parse().unwrap_or(0.0);
        assert!(
            at <= ready_at + f64::EPSILON,
            "{context}: {:?} is recorded at {at} ms, after input_ready at \
             {ready_at} ms. Startup work that happens after the readiness mark \
             is startup work the reported total does not include. Report \
             was:\n{}",
            entry.name,
            report.raw
        );
    }
}

#[test]
fn test_interactive_startup_reports_every_phase_and_reaches_input_ready() {
    let fixture = Fixture::new(3);
    let mut session = Session::spawn(&fixture);
    let report = session.wait_for_report(&fixture);

    let names = report.names();
    for expected in expected_prefix() {
        assert!(
            names.contains(&expected.to_string()),
            "startup must report the {expected:?} phase -- an unmeasured phase \
             is exactly the state #364 exists to end; reported {names:?} and \
             the full report was:\n{}",
            report.raw
        );
    }
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
         terminal is owned, then keys are buffered, then the first frame is \
         painted, then keys are acted on. Recorded positions were {positions:?} \
         in {names:?}"
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
fn test_startup_phase_structure_is_independent_of_the_brain_inventory() {
    // The bounded-work property. If any startup phase enumerated, opened or
    // replayed the Brain root, a 400-Brain inventory would change what the
    // report contains -- an extra phase, a different count, or a phase marked
    // SLOW that is not slow at three Brains.
    let small = Fixture::new(0);
    let large = Fixture::new(400);

    let mut small_session = Session::spawn(&small);
    let small_report = small_session.wait_for_report(&small);
    drop(small_session);

    let mut large_session = Session::spawn(&large);
    let large_report = large_session.wait_for_report(&large);
    drop(large_session);

    assert_reaches_ready(&small_report, "empty Brain inventory");
    assert_reaches_ready(&large_report, "400-Brain inventory");

    assert_eq!(
        small_report.names(),
        large_report.names(),
        "the set and order of startup phases must not depend on how many \
         Brains are on disk: an inventory-sensitive phase set means startup is \
         doing work proportional to accumulated history before the first \
         frame. 0 Brains gave:\n{}\n400 Brains gave:\n{}",
        small_report.raw,
        large_report.raw
    );

    // Counts too, not just names: a phase that handled 400 things instead of
    // one is proportional work even if it kept the same name.
    for small_entry in &small_report.entries {
        let Some(large_entry) = large_report.find(&small_entry.name) else {
            continue;
        };
        assert_eq!(
            small_entry.count, large_entry.count,
            "phase {:?} handled a different number of things at 400 Brains \
             ({:?}) than at 0 ({:?}); startup work before input_ready must be \
             bounded independently of the Brain inventory. 400-Brain report \
             was:\n{}",
            small_entry.name, large_entry.count, small_entry.count, large_report.raw
        );
    }

    // The load-bearing assertion. Every other count in the report is a
    // literal, so comparing them across inventories compares constants and
    // cannot fail. `brains_hydrated` is read by `startup::ready` from a
    // counter `BrainStore::ensure_loaded` increments, so any code that
    // enumerates or replays the Brain root before the first frame shows up
    // here whatever phase it hides in -- including a `read_dir` added inside
    // an existing phase, which the name-and-count comparison above would sail
    // straight past.
    for (label, report) in [("0 Brains", &small_report), ("400 Brains", &large_report)] {
        let hydrated = report.find("brains_hydrated").unwrap_or_else(|| {
            panic!(
                "the report must carry brains_hydrated, or nothing observes \
                 whether startup replayed the Brain root at all. {label} \
                 report was:\n{}",
                report.raw
            )
        });
        assert_eq!(
            hydrated.count,
            Some(0),
            "the interactive frontend must hydrate no Brains before the first \
             frame. It hydrated {:?} with {label} on disk, so startup is doing \
             work proportional to accumulated history. Report was:\n{}",
            hydrated.count,
            report.raw
        );
    }
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

    assert_reaches_ready(&report, "malformed and half-written inventory");

    let clean = Fixture::new(6);
    let mut clean_session = Session::spawn(&clean);
    let clean_report = clean_session.wait_for_report(&clean);
    drop(clean_session);

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
        "tool_registry",
        "mcp_connect",
        "brain_register",
        "brain_attach",
        "brains_hydrated",
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
        "offline",
        "cached",
        "persisted",
        "debug_logging_probe",
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
