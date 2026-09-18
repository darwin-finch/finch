//! `finch setup` drives the wizard through the widget host on a real pty (#812).
//!
//! This lives in the integration tree because the property is about the real
//! binary on a real terminal: the wizard must paint its screens through the
//! same widget-tree + shadow-buffer host the conversation uses — the tab row,
//! the provider list, the Finish/confirm screen, and the cancel card all have
//! to arrive as visible text — and cancelling must write no configuration
//! (the #76 guarantee: config changes only through intentional saves).
//!
//! The harness mirrors `startup_is_readonly_on_config.rs`: one pty per run,
//! a disposable HOME, no inherited proof, and a reader thread gathering the
//! transcript. `Session`-style drop kills and reaps only the child it
//! started.

#![cfg(unix)]

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

/// A disposable, empty HOME: no config, so `finch setup` opens the wizard.
struct HomeFixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
}

impl HomeFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("disposable HOME");
        let home = temp.path().to_path_buf();
        std::fs::create_dir_all(home.join(".finch")).expect("create .finch");
        Self { _temp: temp, home }
    }

    fn config(&self) -> PathBuf {
        self.home.join(".finch/config.toml")
    }
}

/// One `finch` run under a pty, with the same isolation the startup tests use.
struct PtyRun {
    child: Child,
    master: OwnedFd,
    transcript: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    reader: Option<std::thread::JoinHandle<()>>,
    reader_done: std::sync::mpsc::Receiver<()>,
}

impl PtyRun {
    fn spawn(home: &std::path::Path, args: &[&str]) -> Self {
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

    fn send(&mut self, bytes: &str) {
        let mut file =
            std::fs::File::from(self.master.try_clone().expect("clone master for writing"));
        file.write_all(bytes.as_bytes()).expect("write to the pty");
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
                     cancelling setup. This deadline is a hang detector, not a latency \
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

/// `finch setup` on a fresh home paints the wizard through the widget host:
/// the tab block and the provider list arrive as visible text, the Finish
/// tab shows the confirm screen, and cancelling (Ctrl+C, then confirm)
/// exits without ever writing `config.toml`.
#[test]
fn test_setup_wizard_drives_provider_list_and_confirm_through_the_widget_host() {
    let fixture = HomeFixture::new();
    let mut run = PtyRun::spawn(&fixture.home, &["setup"]);

    // The wizard opens on the theme tab; the tab block proves the host painted.
    run.wait_for(
        "Finch Setup",
        READY_DEADLINE,
        "the tab block was drawn on the widget host",
    );

    // One Tab to the Models section: the provider list, blitted through the
    // widget host.
    run.send("\t");
    run.wait_for(
        "AI Providers",
        READY_DEADLINE,
        "the provider list was drawn on the widget host",
    );
    let transcript = run.transcript();
    assert!(
        transcript.contains("Not configured"),
        "the unconfigured primary provider row must be visible; terminal was:\n{transcript}"
    );

    // Drive to the Finish tab: the confirm screen.
    for _ in 0..3 {
        run.send("\t");
        std::thread::sleep(Duration::from_millis(120));
    }
    run.wait_for(
        "Ready to go!",
        READY_DEADLINE,
        "the confirm screen was drawn on the widget host",
    );
    let transcript = run.transcript();
    assert!(
        transcript.contains("save & start chatting"),
        "the confirm screen must name its save action; terminal was:\n{transcript}"
    );

    // Cancel: Ctrl+C opens the discard-confirmation card; `y` discards.
    run.send("\x03");
    run.wait_for(
        "Cancel setup?",
        READY_DEADLINE,
        "the cancel-confirmation card claimed its rect",
    );
    run.send("y");

    let status = run.wait_for_exit();
    let transcript = run.transcript();
    assert!(
        transcript.contains("Setup cancelled"),
        "cancelling must be reported; exit was {status:?}, terminal was:\n{transcript}"
    );
    assert!(
        !fixture.config().exists(),
        "a cancelled setup must not create config.toml — configuration is written \
         only through the intentional-save path (#76); terminal was:\n{transcript}"
    );
}
