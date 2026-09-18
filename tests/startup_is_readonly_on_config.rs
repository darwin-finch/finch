//! Ordinary startup must not rewrite `~/.finch/config.toml` (#76).
//!
//! This lives here, not in a unit test, because the property is about a real
//! home directory. `Config::save()` resolves `dirs::home_dir()`, so a unit
//! test pointing at a temporary path cannot observe production writing to the
//! actual config — and three successive attempts to test this in-crate all
//! passed with the defect restored, because they exercised functions the REPL
//! does not call.
//!
//! An integration test gets its own process, so `HOME` can be set safely.
//!
//! The pty tests at the bottom drive the *real binary* through an ordinary
//! interactive start and a `finch attach` — the path the licence-notice block
//! runs on — because nothing outside production calls `EventLoop::run`. They
//! close the gap the merged startup-write fix (#329) named: a `cfg.save()`
//! re-added anywhere in that block reships #76, and only a real start can see
//! it. `Session::drop` kills and reaps only the child it started; the child is
//! a plain `Command` spawn with no session or process group.

#![cfg(unix)]

use std::path::Path;

/// The bytes and mtime of a file, for comparison across a call.
fn fingerprint(path: &Path) -> (Vec<u8>, std::time::SystemTime) {
    (
        std::fs::read(path).expect("config must exist"),
        std::fs::metadata(path)
            .expect("config must exist")
            .modified()
            .expect("mtime"),
    )
}

/// The startup licence-notice decision leaves `config.toml` untouched.
///
/// `claim_notice_showing_now` is what `EventLoop::run` calls — the same
/// function, resolving the same home directory. Restoring the original defect
/// (`cfg.license.notice_suppress_until = ...; cfg.save();`) makes this fail,
/// which is what every earlier version of this test could not do.
#[test]
fn test_the_startup_notice_decision_does_not_rewrite_the_config() {
    let home = tempfile::tempdir().expect("tempdir");
    let finch = home.path().join(".finch");
    std::fs::create_dir_all(&finch).expect("create .finch");

    let config = finch.join("config.toml");
    let original = b"# a comment a serializer round-trip would drop\n\
                     [license]\n\
                     license_type = \"noncommercial\"\n";
    std::fs::write(&config, original).expect("write config");
    let before = fingerprint(&config);

    // Coarse filesystem timestamps would hide a rewrite inside the same tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    // SAFETY: not "its own process" — libtest runs the tests in this binary on
    // concurrent threads, and an earlier version of this comment said
    // otherwise. What makes it sound is that this is the only test here that
    // mutates the environment (the other passes HOME via `Command::env`), and
    // std serialises `set_var` against its own readers. The residual risk is a
    // non-std `getenv` on another thread, which is why `set_var` is unsafe at
    // all; with one mutator and one reader of this variable, there is none.
    unsafe {
        std::env::set_var("HOME", home.path());
    }

    let shown = finch::config::claim_notice_showing_now(None, chrono::Local::now().date_naive());
    assert!(shown, "nothing recorded yet, so the notice is due");

    let after = fingerprint(&config);
    assert_eq!(
        after.0, before.0,
        "startup rewrote config.toml -- this is the #76 defect"
    );
    assert_eq!(
        after.1, before.1,
        "startup moved config.toml's mtime; an identical-bytes rewrite still \
         tells every backup and sync tool the file changed"
    );

    assert!(
        finch.join("notice_state.toml").exists(),
        "the record belongs in the state file"
    );
}

/// `finch license remove` clears the recorded notice suppression.
///
/// This drives the real binary, because the wiring is what was broken and a
/// helper test could not see it. Before the record moved out of `config.toml`,
/// removal un-suppressed the notice for free — `LicenseRemove` writes
/// `notice_suppress_until: None`. Afterwards the state file won and that
/// stopped working, silently.
///
/// Review of #329 deleted both production call sites and 98 tests still
/// passed; the only thing that had ever noticed one of them was rustc, when it
/// failed to compile. This runs the command a user runs.
#[test]
fn test_license_remove_clears_the_recorded_suppression() {
    let home = tempfile::tempdir().expect("tempdir");
    let finch = home.path().join(".finch");
    std::fs::create_dir_all(&finch).expect("create .finch");

    let state = finch.join("notice_state.toml");
    std::fs::write(&state, "suppress_until = \"2030-01-01\"\n").expect("seed state");
    assert!(state.exists());

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(["license", "remove"])
        .env("HOME", home.path())
        .output()
        .expect("run finch license remove");

    assert!(
        output.status.success(),
        "`finch license remove` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !state.exists(),
        "removing a licence must clear the recorded suppression, or the notice \
         stays hidden until the old date expires"
    );
}

// ---------------------------------------------------------------------------
// The real binary, on a pty: ordinary start and attach (#76).
//
// The notice block runs inside `EventLoop::run`, which nothing but production
// calls. Seeding a home and driving `finch` through a real start — then
// fingerprinting `config.toml` — is the only test that can see a `cfg.save()`
// re-added there, so this is the regression the merged notice-state fix named
// as its own gap.
// ---------------------------------------------------------------------------

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const READY_DEADLINE: Duration = Duration::from_secs(90);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
const ROWS: u16 = 24;
const COLS: u16 = 100;
const SUPERVISOR_AUTHORITY_FDS: &[i32] = &[9, 10, 11, 12, 108, 109, 110, 111, 112];
const ATTACHED_BRAIN: &str = "quiet-harbour-04c1f9";

/// A disposable HOME seeded the way an ordinary user's is: a hand-written
/// config with comments a serializer would drop, a noncommercial licence (so
/// the notice path runs), and no daemon to spawn.
struct HomeFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

impl HomeFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("disposable HOME");
        let home = temp.path().to_path_buf();
        let finch = home.join(".finch");
        std::fs::create_dir_all(finch.join("brains")).expect("create .finch/brains");
        std::fs::write(
            finch.join("config.toml"),
            r#"# a hand-written comment a serializer round-trip would drop
# and a second one, so any rewrite is visible in the bytes

[client]
use_daemon = false
daemon_address = "http://127.0.0.1:1"
auto_spawn = false
timeout_seconds = 1
auto_discover = false
prefer_local = true

[[providers]]
type = "grok"
api_key = "not-a-real-key"
model = "grok-code-fast-1"
base_url = "http://127.0.0.1:1"
name = "readonly-startup-fixture"

[license]
license_type = "noncommercial"
"#,
        )
        .expect("seed config.toml");
        Self { _temp: temp, home }
    }

    fn config(&self) -> PathBuf {
        self.home.join(".finch/config.toml")
    }

    fn notice_state(&self) -> PathBuf {
        self.home.join(".finch/notice_state.toml")
    }
}

/// One `finch` run under a pty, with the same isolation the attach tests use:
/// disposable HOME, no inherited proof, no supervisor authority fds.
struct PtyRun {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl PtyRun {
    fn spawn(home: &Path, args: &[&str]) -> Self {
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
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text.starts_with("FINCH_BRAIN_TEST_") || name_text.starts_with("FINCH_TEST_") {
                command.env_remove(name);
            }
        }
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

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.transcript.lock().expect("transcript poisoned").clone())
            .into_owned()
    }

    fn wait_for(&mut self, needle: &str, deadline: Duration, what: &str) {
        let expiry = Instant::now() + deadline;
        loop {
            let transcript = self.transcript();
            if transcript.contains(needle) {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "INVARIANT: {what}\nfinch exited with {status:?} before that happened.\n\
                     needle={needle:?}\nterminal was:\n{transcript}"
                );
            }
            if Instant::now() >= expiry {
                panic!(
                    "the run hung: {what} did not happen within {deadline:?} \
                     ({needle:?} never reached the terminal). This deadline is a hang \
                     detector, not a latency assertion. Terminal was:\n{}",
                    self.transcript()
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
                let transcript = self.transcript();
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

impl Drop for PtyRun {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader_done.recv_timeout(Duration::from_secs(5));
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// What one completed finch run should have done to the notice record.
enum NoticeExpectation {
    /// A first run: the notice was due, so the decision is recorded in the
    /// runtime-state file with a suppression date no earlier than today.
    Recorded,
    /// A run inside the suppression window: nothing at all is written, so the
    /// state file stays byte-identical to what was seeded.
    Unchanged(&'static [u8]),
}

/// Everything #76 claims about one completed finch run: the config is
/// byte-for-byte and mtime identical, the notice record did what
/// `notice` says, no save was announced, and no temporary littered the Finch
/// directory.
fn assert_run_left_config_untouched(
    what: &str,
    fixture: &HomeFixture,
    run: &mut PtyRun,
    notice: NoticeExpectation,
) {
    let before = fingerprint(&fixture.config());

    run.wait_for("finch v", READY_DEADLINE, "the startup header was drawn");
    run.send_line("/exit");
    let status = run.wait_for_exit();
    assert!(
        status.success(),
        "the ordinary {what} run must exit cleanly: {status:?}\nterminal was:\n{}",
        run.transcript()
    );

    let after = fingerprint(&fixture.config());
    assert_eq!(
        after.0, before.0,
        "the {what} run rewrote config.toml -- this is the #76 defect. Bytes before:\n{}\nbytes after:\n{}",
        String::from_utf8_lossy(&before.0),
        String::from_utf8_lossy(&after.0),
    );
    assert_eq!(
        after.1, before.1,
        "the {what} run moved config.toml's mtime; an identical-bytes rewrite still \
         tells every backup and sync tool the file changed"
    );
    assert!(
        !run.transcript().contains("Configuration saved"),
        "ordinary startup must not announce a configuration save; terminal was:\n{}",
        run.transcript()
    );
    match notice {
        NoticeExpectation::Recorded => {
            let state = std::fs::read_to_string(fixture.notice_state()).unwrap_or_else(|error| {
                panic!(
                    "a run where the notice was due must record the decision in the \
                     runtime-state file, not the config: {error}"
                )
            });
            let recorded = state
                .split("suppress_until = \"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
            assert_eq!(
                recorded,
                Some(chrono::Local::now().date_naive() + chrono::Duration::days(7)),
                "the {what} run must record a one-week suppression in the state file \
                 (today is {}); file was:\n{state}",
                chrono::Local::now().date_naive()
            );
        }
        NoticeExpectation::Unchanged(seed) => {
            assert_eq!(
                std::fs::read(fixture.notice_state()).unwrap_or_default(),
                seed,
                "a run that shows no notice must record nothing: the state file \
                 must stay byte-identical"
            );
        }
    }
    let litter: Vec<PathBuf> = std::fs::read_dir(fixture.home.join(".finch"))
        .expect("list .finch")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().contains(".tmp"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        litter.is_empty(),
        "no temporary may survive a run; .finch held {litter:?}"
    );
}

/// An ordinary interactive start leaves the user's config byte-for-byte
/// identical, mtime included, records the notice in the state file, and
/// announces no save. Restoring the pre-#329 defect — any `cfg.save()` on the
/// startup path, here or in the notice block — fails this.
#[test]
fn test_an_interactive_start_leaves_the_config_byte_for_byte_identical() {
    let fixture = HomeFixture::new();

    // Coarse filesystem timestamps would hide a rewrite inside the same tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let mut run = PtyRun::spawn(&fixture.home, &[]);
    assert_run_left_config_untouched(
        "interactive start",
        &fixture,
        &mut run,
        NoticeExpectation::Recorded,
    );
}

/// Attaching a named Brain is the same ordinary startup and must behave the
/// same: no rewrite, no mtime move, no announced save. The home carries a
/// recorded suppression, so this is also a run inside the notice window —
/// the quietest case, which still must not write anything at all.
#[test]
fn test_attaching_a_named_brain_leaves_the_config_byte_for_byte_identical() {
    let fixture = HomeFixture::new();
    let seeded: &'static [u8] = b"suppress_until = \"2030-01-01\"\n";
    std::fs::write(fixture.notice_state(), seeded).expect("seed a live notice suppression");

    // Coarse filesystem timestamps would hide a rewrite inside the same tick.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let mut run = PtyRun::spawn(&fixture.home, &["attach", ATTACHED_BRAIN]);
    assert_run_left_config_untouched(
        "attach",
        &fixture,
        &mut run,
        NoticeExpectation::Unchanged(seeded),
    );
}
