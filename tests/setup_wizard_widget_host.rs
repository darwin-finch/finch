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

// ─── #926: the painted screen, not just the byte stream ──────────────────────
//
// The byte stream can hold text the screen never shows (a frame that scrolled
// itself off the top, tails overwritten mid-row). These regressions replay the
// wizard's bytes through a terminal emulator and assert on what a reader sees,
// which is where the four blit defects lived.

/// A minimal xterm-class terminal: cursor addressing, erase in display/line,
/// deferred autowrap, linefeed scroll, and the double-width emoji cells a real
/// terminal renders. Synchronized-update brackets, mode sets and SGR are
/// consumed as screen noise.
struct WizardVt {
    screen: Vec<Vec<char>>,
    scrolled: usize,
    row: usize,
    col: usize,
}

impl WizardVt {
    fn feed_all(bytes: &[u8]) -> Self {
        let mut vt = Self {
            screen: vec![vec![' '; COLS as usize]; ROWS as usize],
            scrolled: 0,
            row: 0,
            col: 0,
        };
        vt.feed(&String::from_utf8_lossy(bytes));
        vt
    }

    fn feed(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            index = match chars[index] {
                '\x1b' => self.escape(&chars, index + 1),
                '\r' => {
                    self.col = 0;
                    index + 1
                }
                '\n' => {
                    self.line_feed();
                    index + 1
                }
                c if (c as u32) < 0x20 => index + 1,
                c => {
                    self.put(c);
                    index + 1
                }
            };
        }
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
                    if chars[cursor] == '\u{7}'
                        || (chars[cursor] == '\x1b' && chars.get(cursor + 1) == Some(&'\\'))
                    {
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
        if body.starts_with('?') || body.starts_with('>') {
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
                self.row = first.saturating_sub(1).min(ROWS as usize - 1);
                self.col = params
                    .get(1)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(1)
                    .min(COLS as usize - 1);
            }
            'J' => {
                if first >= 2 {
                    self.screen = vec![vec![' '; COLS as usize]; ROWS as usize];
                } else if first == 0 {
                    for column in self.col..COLS as usize {
                        self.screen[self.row][column] = ' ';
                    }
                    for row in (self.row + 1)..ROWS as usize {
                        self.screen[row] = vec![' '; COLS as usize];
                    }
                }
            }
            'K' => {
                if first == 0 {
                    for column in self.col..COLS as usize {
                        self.screen[self.row][column] = ' ';
                    }
                } else if first == 2 {
                    self.screen[self.row] = vec![' '; COLS as usize];
                }
            }
            'A' => self.row = self.row.saturating_sub(count),
            'B' => self.row = (self.row + count).min(ROWS as usize - 1),
            'C' => self.col = (self.col + count).min(COLS as usize - 1),
            'D' => self.col = self.col.saturating_sub(count),
            _ => {}
        }
    }

    fn line_feed(&mut self) {
        if self.row + 1 < ROWS as usize {
            self.row += 1;
        } else {
            self.screen.remove(0);
            self.screen.push(vec![' '; COLS as usize]);
            self.scrolled += 1;
        }
    }

    fn put(&mut self, c: char) {
        let wide = matches!(c as u32,
            0x231A..=0x231B | 0x2614 | 0x2615 | 0x2705 | 0x270A..=0x270B | 0x2728
            | 0x274C | 0x274E | 0x2753..=0x2755 | 0x2757 | 0x2795..=0x2797
            | 0x27B0 | 0x27BF | 0x2B1B..=0x2B1C | 0x2B50 | 0x2B55
            | 0x1F000..=0x1FAFF
        );
        if wide && self.col + 2 > COLS as usize {
            self.col = 0;
            self.line_feed();
        }
        if self.col >= COLS as usize {
            self.col = 0;
            self.line_feed();
        }
        self.screen[self.row][self.col] = c;
        self.col += 1;
        if wide {
            if self.col < COLS as usize {
                self.screen[self.row][self.col] = ' ';
            }
            self.col += 1;
        }
    }

    /// The visible screen, trailing blanks trimmed per row.
    fn rows(&self) -> Vec<String> {
        self.screen
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect()
    }
}

impl PtyRun {
    /// Replay the whole transcript through the emulator and return the screen
    /// plus how many rows scrolled off the top.
    fn screen(&mut self) -> (Vec<String>, usize) {
        let vt = WizardVt::feed_all(&self.transcript.lock().expect("transcript poisoned").clone());
        (vt.rows(), vt.scrolled)
    }
}

/// The tab labels a reader must see on every wizard screen. These three names
/// appear nowhere else in the wizard's own content, so their absence is the
/// missing-tab-row defect, not a naming collision.
const TAB_LABELS: [&str; 3] = ["Look & Feel", "Model Setup", "Finish"];

fn assert_tabs_visible(screen: &[String], context: &str) {
    for label in TAB_LABELS {
        assert!(
            screen.iter().any(|row| row.contains(label)),
            "INVARIANT (#926): the {context} screen must show the {label:?} tab label — \
             the tab row is the wizard's navigation strip. Screen was:\n{}",
            screen.join("\n")
        );
    }
}

/// Box borders print their width as a glyph run, never as digits (#926
/// defect 1): no screen row may show a dash run interrupted by the gap count.
#[test]
fn test_wizard_borders_paint_only_dash_glyphs_on_the_real_terminal() {
    let fixture = HomeFixture::new();
    let mut run = PtyRun::spawn(&fixture.home, &["setup"]);
    run.wait_for(
        "Finch Setup",
        READY_DEADLINE,
        "the tab block was drawn on the widget host",
    );

    for step in ["the first screen", "the provider screen after a Tab"] {
        let (screen, _) = run.screen();
        for row in &screen {
            let dash_then_digit = row.contains("─")
                && row
                    .chars()
                    .zip(row.chars().skip(1))
                    .any(|(a, b)| a == '─' && b.is_ascii_digit());
            let digit_then_dash = row
                .chars()
                .zip(row.chars().skip(1))
                .any(|(a, b)| a.is_ascii_digit() && b == '─');
            assert!(
                !dash_then_digit && !digit_then_dash,
                "INVARIANT (#926): on {step} a border row carries its width as digits — \
                 the border must repeat the ─ glyph. Offending row: {row:?}\nscreen:\n{}",
                screen.join("\n")
            );
        }
        assert_tabs_visible(&screen, step);
        if step == "the first screen" {
            run.send("\t");
            run.wait_for(
                "AI Providers",
                READY_DEADLINE,
                "the provider list was drawn on the widget host",
            );
        }
    }
}

/// The tab row is visible on the first painted frame of every section, with
/// the section's own content beneath it (#926 defect 4).
#[test]
fn test_wizard_tab_labels_are_visible_on_the_first_frame_of_every_section() {
    let fixture = HomeFixture::new();
    let mut run = PtyRun::spawn(&fixture.home, &["setup"]);
    run.wait_for(
        "Finch Setup",
        READY_DEADLINE,
        "the tab block was drawn on the widget host",
    );

    let sections = ["Look & Feel", "Model Setup", "Style", "Settings", "Finish"];
    for (step, expected_section) in sections.iter().enumerate() {
        if step > 0 {
            run.send("\t");
            // The next section paints on the keypress; wait for its title so
            // the screen replay observes that section's first painted frame.
            let section_marker = match *expected_section {
                "Model Setup" => "AI Providers",
                "Style" => "Choose a Style",
                "Settings" => "Options",
                "Finish" => "Ready to go!",
                _ => unreachable!(),
            };
            run.wait_for(
                section_marker,
                READY_DEADLINE,
                "the next section was drawn on the widget host",
            );
        }
        let (screen, scrolled) = run.screen();
        assert_eq!(
            scrolled,
            0,
            "INVARIANT (#926): the {expected_section} screen must not scroll — a scrolled \
             frame is a frame whose planned rows disagree with the terminal. \
             Screen was:\n{}",
            screen.join("\n")
        );
        assert_tabs_visible(&screen, expected_section);
        assert!(
            screen.iter().any(|row| row.contains(expected_section)),
            "INVARIANT (#926): the {expected_section} tab must be visible on its own \
             screen. Screen was:\n{}",
            screen.join("\n")
        );
    }
}

/// Switching sections erases the previous section's frame: no theme rows may
/// survive above or beside the AI Providers frame (#926 defect 3).
#[test]
fn test_wizard_section_switch_leaves_no_stale_theme_rows_above_the_new_section() {
    let fixture = HomeFixture::new();
    let mut run = PtyRun::spawn(&fixture.home, &["setup"]);
    run.wait_for(
        "Available Themes",
        READY_DEADLINE,
        "the theme list was drawn on the widget host",
    );
    run.send("\t");
    run.wait_for(
        "AI Providers",
        READY_DEADLINE,
        "the provider list was drawn on the widget host",
    );

    let (screen, scrolled) = run.screen();
    assert_eq!(
        scrolled,
        0,
        "INVARIANT (#926): the section switch must not scroll the terminal. \
         Screen was:\n{}",
        screen.join("\n")
    );
    let stale_markers = [
        "Theme Selection",
        "Available Themes",
        "Dark -",
        "Light -",
        "High Contrast",
        "Solarized",
    ];
    for marker in stale_markers {
        assert!(
            !screen.iter().any(|row| row.contains(marker)),
            "INVARIANT (#926): switching to AI Providers must erase the theme section — \
             no screen row may still hold {marker:?}. Screen was:\n{}",
            screen.join("\n")
        );
    }
    for expected in ["AI Providers", "Paste your API key"] {
        assert!(
            screen.iter().any(|row| row.contains(expected)),
            "the new section must actually be painted after the switch; \
             {expected:?} missing. Screen was:\n{}",
            screen.join("\n")
        );
    }
}

/// A row that shrinks leaves no tail of the previous frame: every box row on
/// the screen is box-complete, first glyph to last (#926 defect 2). A wrapped
/// or shifted row — the mid-row offset class — breaks the box edges.
#[test]
fn test_wizard_shrinking_rows_leave_no_tail_bleed_on_the_real_terminal() {
    let fixture = HomeFixture::new();
    let mut run = PtyRun::spawn(&fixture.home, &["setup"]);
    run.wait_for(
        "Available Themes",
        READY_DEADLINE,
        "the theme list was drawn on the widget host",
    );

    // The theme screen itself carries the emoji preview rows that a
    // one-column measurement mismeasures: its box rows must still close.
    let assert_box_complete = |screen: &[String], context: &str| {
        for row in screen {
            if row.contains('│') {
                assert!(
                    row.starts_with('│') && row.ends_with('│'),
                    "INVARIANT (#926): on {context} a box row is not complete from edge \
                     to edge — content wrapped or a tail bled into it. Row: {row:?}\n\
                     screen:\n{}",
                    screen.join("\n")
                );
            }
        }
    };
    let (theme_screen, scrolled) = run.screen();
    assert_eq!(scrolled, 0, "the theme screen must not scroll");
    assert_box_complete(&theme_screen, "the theme screen");

    run.send("\t");
    run.wait_for(
        "AI Providers",
        READY_DEADLINE,
        "the provider list was drawn on the widget host",
    );
    let (provider_screen, scrolled) = run.screen();
    assert_eq!(
        scrolled, 0,
        "the provider screen must not scroll — the section frame is shorter than \
         the theme frame and must be erased, not grown over"
    );
    assert_box_complete(&provider_screen, "the provider screen");
    // No row may mix the two sections: the shorter new frame's rows must not
    // carry theme tails beside provider content.
    for row in &provider_screen {
        let has_theme_text = row.contains("Theme Selection") || row.contains("Available Themes");
        let has_provider_text = row.contains("AI Providers") || row.contains("Paste your API key");
        assert!(
            !(has_theme_text && has_provider_text),
            "INVARIANT (#926): after the switch a row holds both sections' content — \
             a tail of the previous frame bled through. Row: {row:?}\nscreen:\n{}",
            provider_screen.join("\n")
        );
    }
}
