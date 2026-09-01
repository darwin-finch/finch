// Integration tests for TUI mode
//
// These tests verify TUI functionality using expect/pty simulation.
// Note: TUI tests are complex because they require a pseudo-TTY.

use std::io::Write;
use std::process::{Command, Stdio};

#[cfg(unix)]
use std::io::{ErrorKind, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::time::{Duration, Instant};

#[cfg(unix)]
fn read_pty_until(
    master: &mut std::fs::File,
    transcript: &mut Vec<u8>,
    deadline: Instant,
    ready: impl Fn(&[u8]) -> bool,
) -> bool {
    let mut buffer = [0_u8; 4096];
    while Instant::now() < deadline {
        match master.read(&mut buffer) {
            Ok(0) => return ready(transcript),
            Ok(read) => {
                transcript.extend_from_slice(&buffer[..read]);
                if ready(transcript) {
                    return true;
                }
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) if error.raw_os_error() == Some(nix::libc::EIO) => {
                return ready(transcript);
            }
            Err(error) => panic!("failed reading Finch PTY: {error}"),
        }
    }
    ready(transcript)
}

#[cfg(unix)]
fn count_bytes(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
struct TerminalModes {
    input: nix::libc::tcflag_t,
    output: nix::libc::tcflag_t,
    control: nix::libc::tcflag_t,
    local: nix::libc::tcflag_t,
    characters: [nix::libc::cc_t; nix::libc::NCCS],
}

#[cfg(unix)]
fn terminal_modes(file: &std::fs::File) -> TerminalModes {
    let mut attributes = std::mem::MaybeUninit::<nix::libc::termios>::uninit();
    assert_eq!(
        unsafe { nix::libc::tcgetattr(file.as_raw_fd(), attributes.as_mut_ptr()) },
        0,
        "failed to snapshot PTY termios: {}",
        std::io::Error::last_os_error()
    );
    let attributes = unsafe { attributes.assume_init() };
    TerminalModes {
        input: attributes.c_iflag,
        output: attributes.c_oflag,
        control: attributes.c_cflag,
        // PENDIN is a kernel-maintained reprint status bit, not a configurable
        // terminal mode. Darwin may raise it when a PTY child closes even when
        // no input remains. Compare every user-controlled mode bit exactly.
        local: attributes.c_lflag & !nix::libc::PENDIN,
        characters: attributes.c_cc,
    }
}

#[cfg(unix)]
fn assert_signal_exit(status: std::process::ExitStatus, signal: nix::sys::signal::Signal) {
    assert_eq!(
        status.code(),
        Some(128 + signal as i32),
        "Finch did not preserve conventional {signal:?} termination: {status}"
    );
}

#[cfg(unix)]
const MOUSE_ENABLE_SEQUENCES: [&[u8]; 7] = [
    b"\x1b[?1000h",
    b"\x1b[?1002h",
    b"\x1b[?1003h",
    b"\x1b[?1005h",
    b"\x1b[?1006h",
    b"\x1b[?1015h",
    b"\x1b[?1016h",
];

#[cfg(unix)]
const MOUSE_DISABLE_SEQUENCES: [&[u8]; 7] = [
    b"\x1b[?1000l",
    b"\x1b[?1002l",
    b"\x1b[?1003l",
    b"\x1b[?1005l",
    b"\x1b[?1006l",
    b"\x1b[?1015l",
    b"\x1b[?1016l",
];

#[cfg(unix)]
fn open_owned_pty() -> (std::fs::File, std::fs::File) {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let mut size = nix::libc::winsize {
        ws_row: 24,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let opened = unsafe {
        nix::libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );

    let master = unsafe { std::fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave_fd) };
    let flags = unsafe { nix::libc::fcntl(master.as_raw_fd(), nix::libc::F_GETFL) };
    assert!(
        flags >= 0,
        "F_GETFL failed: {}",
        std::io::Error::last_os_error()
    );
    assert_eq!(
        unsafe {
            nix::libc::fcntl(
                master.as_raw_fd(),
                nix::libc::F_SETFL,
                flags | nix::libc::O_NONBLOCK,
            )
        },
        0,
        "F_SETFL failed: {}",
        std::io::Error::last_os_error()
    );
    (master, slave)
}

#[cfg(unix)]
fn wait_for_child(child: &mut std::process::Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if let Some(status) = child.try_wait().expect("failed to inspect child process") {
            return status;
        }
        assert!(Instant::now() < deadline, "child process did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn supervised_pty_authority_or_skip() -> bool {
    match finch::brain::isolated_test_proof_if_present() {
        Ok(Some(_proof)) => true,
        Ok(None) => {
            eprintln!("skipping environment-owned PTY regression outside scripts/test_brains.sh");
            false
        }
        Err(error) => panic!("invalid supervisor authority for PTY regression: {error:#}"),
    }
}

#[cfg(unix)]
fn write_probe_marker(marker: &[u8]) -> Result<(), &'static str> {
    std::io::stdout()
        .write_all(marker)
        .map_err(|_| "failed to write PTY probe marker")?;
    std::io::stdout()
        .flush()
        .map_err(|_| "failed to flush PTY probe marker")
}

/// Production-boundary regression for Finch's real terminal lifecycle.
///
/// The child inherits the repository test supervisor's isolated process group
/// and disposable HOME. The test owns both PTY descriptors and asks Finch to
/// quit normally; it never signals a PID or starts a daemon.
#[cfg(unix)]
#[test]
fn test_tui_binary_never_captures_native_mouse_input() {
    if !supervised_pty_authority_or_skip() {
        return;
    }

    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn Finch in owned PTY");
    drop(slave);

    let enable_mouse = b"\x1b[?1000h";
    let disable_mouse = b"\x1b[?1000l";
    let ready = b"accept edits on";
    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            |bytes| bytes.windows(ready.len()).any(|window| window == ready),
        ),
        "Finch did not reach its input surface: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert!(
        count_bytes(&transcript, enable_mouse) == 0,
        "Finch enabled mouse reporting before input: {}",
        String::from_utf8_lossy(&transcript),
    );

    master.write_all(b"/quit\r").unwrap();
    master.flush().unwrap();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| count_bytes(bytes, disable_mouse) > 0,
        ),
        "Finch shutdown did not emit its defensive mouse reset: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    assert!(status.success(), "Finch exited unsuccessfully: {status}");
    assert_eq!(count_bytes(&transcript, enable_mouse), 0);
}

/// Child-only probe used by the supervised terminal lifecycle regressions.
#[cfg(unix)]
#[test]
fn tui_renderer_terminal_lifecycle_child() -> Result<(), &'static str> {
    let Ok(termination) = std::env::var("FINCH_TEST_TUI_TERMINATION") else {
        return Ok(());
    };
    if std::env::var_os("FINCH_TEST_TUI_WAIT_FOR_START").is_some() {
        write_probe_marker(b"FINCH_TERMIOS_BASELINE_READY")?;
        std::thread::sleep(Duration::from_millis(250));
    }
    if termination == "init-error" {
        std::env::set_var("FINCH_TEST_TUI_FAIL_AFTER_ACTIVATION", "1");
    }
    let output = std::sync::Arc::new(finch::cli::OutputManager::new(
        finch::config::ColorScheme::default(),
    ));
    let status = std::sync::Arc::new(finch::cli::StatusBar::new());
    let renderer =
        finch::cli::tui::TuiRenderer::new(output, status, finch::config::ColorScheme::default());
    if termination == "init-error" {
        std::env::remove_var("FINCH_TEST_TUI_FAIL_AFTER_ACTIVATION");
        return renderer
            .is_err()
            .then_some(())
            .ok_or("injected renderer initialization unexpectedly succeeded");
    }
    let mut renderer = renderer.map_err(|_| "renderer initialization failed")?;

    match termination.as_str() {
        "clean" => renderer.shutdown().map_err(|_| "renderer shutdown failed"),
        "drop" => {
            drop(renderer);
            Ok(())
        }
        "error" => Err("intentional post-activation error"),
        "panic" => panic!("intentional post-activation panic"),
        "panic-double-restore" => {
            finch::cli::tui::emergency_restore_terminal();
            panic!("causal double-restoration mutation")
        }
        "cleanup-end-error" => {
            std::env::set_var("FINCH_TEST_TUI_CLEANUP_FAIL_ONCE", "end_sync");
            write_probe_marker(b"FINCH_CLEANUP_FAILURE_BEGIN")?;
            let result = renderer.shutdown().map_err(|_| "renderer shutdown failed");
            std::env::remove_var("FINCH_TEST_TUI_CLEANUP_FAIL_ONCE");
            result
        }
        "suspend-resume" => {
            write_probe_marker(b"FINCH_SUSPEND_BEGIN")?;
            renderer.suspend().map_err(|_| "renderer suspend failed")?;
            if crossterm::terminal::is_raw_mode_enabled().unwrap_or(true) {
                return Err("raw mode remained enabled after suspend");
            }
            write_probe_marker(b"FINCH_SUSPENDED_RAW_OFF")?;
            write_probe_marker(b"FINCH_RESUME_BEGIN")?;
            renderer.resume().map_err(|_| "renderer resume failed")?;
            if !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false) {
                return Err("raw mode remained disabled after resume");
            }
            write_probe_marker(b"FINCH_RESUMED_RAW_ON")?;
            write_probe_marker(b"FINCH_SHUTDOWN_BEGIN")?;
            renderer.shutdown().map_err(|_| "renderer shutdown failed")
        }
        "suspended-signal" => {
            renderer.suspend().map_err(|_| "renderer suspend failed")?;
            write_probe_marker(b"FINCH_SUSPENDED_SIGNAL_READY")?;
            std::thread::sleep(Duration::from_secs(10));
            renderer
                .resume()
                .map_err(|_| "renderer resumed after shutdown")
        }
        "editor-signal" => {
            finch::cli::tui::supervised_editor_handoff_for_signal()
                .map_err(|_| "failed to suspend editor handoff")?;
            write_probe_marker(b"FINCH_EDITOR_SIGNAL_READY")?;
            std::thread::sleep(Duration::from_secs(10));
            finch::cli::tui::supervised_resume_editor_handoff()
                .map_err(|_| "editor resume was rejected")?;
            Err("editor handoff resumed after shutdown")
        }
        "backpressure-signal" => {
            write_probe_marker(b"FINCH_BACKPRESSURE_SIGNAL_READY")?;
            finch::cli::tui::supervised_hold_backpressured_terminal()
                .map_err(|_| "backpressure probe returned before signal")
        }
        "render-resize" => {
            write_probe_marker(b"FINCH_LIVE_HISTORY_PROBE_BEGIN")?;
            renderer.set_operation_status("TRANSIENT\x1b[31mRED\nSECOND\x07ROW");
            renderer.set_session_label("SESSION\nINJECT\x1b]0;owned\x07");
            for (width, height) in [(120, 24), (63, 11), (100, 20), (41, 9)] {
                renderer.render().map_err(|_| "live render failed")?;
                renderer
                    .handle_resize(width, height)
                    .map_err(|_| "resize invalidation failed")?;
                renderer.render().map_err(|_| "resize render failed")?;
            }
            renderer.shutdown().map_err(|_| "renderer shutdown failed")
        }
        other => panic!("unknown terminal lifecycle probe: {other}"),
    }
}

#[cfg(unix)]
fn slice_between_markers<'a>(transcript: &'a [u8], start: &[u8], end: &[u8]) -> &'a [u8] {
    let start = transcript
        .windows(start.len())
        .position(|window| window == start)
        .expect("start marker missing")
        + start.len();
    let end = transcript[start..]
        .windows(end.len())
        .position(|window| window == end)
        .map(|offset| start + offset)
        .expect("end marker missing");
    &transcript[start..end]
}

/// The real renderer must restore every terminal mode it owns on startup
/// failure, clean shutdown, unwind, and ordinary `Result::Err` paths.
#[cfg(unix)]
#[test]
fn test_tui_renderer_restores_all_terminal_modes_on_all_terminations() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let disable_mouse = b"\x1b[?1000l";
    let disable_paste = b"\x1b[?2004l";
    let pop_keyboard = b"\x1b[<1u";
    let begin_sync = b"\x1b[?2026h";
    let end_sync = b"\x1b[?2026l";

    for termination in ["clean", "drop", "error", "panic", "init-error"] {
        let (mut master, slave) = open_owned_pty();
        let termios_probe = slave.try_clone().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui_renderer_terminal_lifecycle_child",
                "--nocapture",
            ])
            .env("FINCH_TEST_TUI_TERMINATION", termination)
            .env("FINCH_TEST_TUI_WAIT_FOR_START", "1")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("failed to spawn renderer lifecycle probe in owned PTY");
        drop(slave);

        let mut transcript = Vec::new();
        let baseline = b"FINCH_TERMIOS_BASELINE_READY";
        assert!(read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes
                .windows(baseline.len())
                .any(|window| window == baseline),
        ));
        let original_modes = terminal_modes(&termios_probe);
        let observed_cleanup = read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| {
                count_bytes(bytes, disable_mouse) > 0
                    && count_bytes(bytes, disable_paste) > 0
                    && count_bytes(bytes, pop_keyboard) > 0
            },
        );
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
        assert_eq!(
            terminal_modes(&termios_probe),
            original_modes,
            "{termination} path did not restore exact PTY termios"
        );
        let _ = read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(1),
            |_| false,
        );
        assert!(
            observed_cleanup,
            "{termination} path omitted the defensive mouse reset: {}",
            String::from_utf8_lossy(&transcript)
        );
        for sequence in MOUSE_ENABLE_SEQUENCES {
            assert_eq!(
                count_bytes(&transcript, sequence),
                0,
                "{termination} path enabled mouse reporting {sequence:?}"
            );
        }
        for sequence in MOUSE_DISABLE_SEQUENCES {
            assert_eq!(
                count_bytes(&transcript, sequence),
                1,
                "{termination} path did not reset mouse mode {sequence:?} exactly once"
            );
        }
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 1);
        assert_eq!(count_bytes(&transcript, disable_paste), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 1);
        assert_eq!(count_bytes(&transcript, pop_keyboard), 1);
        assert_eq!(
            count_bytes(&transcript, begin_sync),
            count_bytes(&transcript, end_sync),
            "{termination} path left synchronized update unbalanced: {}",
            String::from_utf8_lossy(&transcript)
        );
        if matches!(termination, "clean" | "drop" | "init-error") {
            assert!(status.success(), "{termination} probe failed: {status}");
        } else {
            assert!(
                !status.success(),
                "{termination} probe unexpectedly succeeded"
            );
        }
    }
}

/// Proof-gated causal controls reenact both rejected mechanisms through the
/// production renderer. These assertions demonstrate that the positive PTYs
/// would fail on the pre-fix relative anchor and panic double-cleanup behavior.
#[cfg(unix)]
#[test]
fn test_tui_causal_controls_detect_relative_anchor_and_double_restoration() {
    if !supervised_pty_authority_or_skip() {
        return;
    }

    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui_renderer_terminal_lifecycle_child",
            "--nocapture",
        ])
        .env("FINCH_TEST_TUI_TERMINATION", "render-resize")
        .env("FINCH_TEST_TUI_MUTATE_RELATIVE_LIVE_ANCHOR", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn relative-anchor causal control");
    drop(slave);
    let mut transcript = Vec::new();
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(10),
        |bytes| bytes
            .windows(b"\x1b[?2004l".len())
            .any(|w| w == b"\x1b[?2004l"),
    ));
    assert!(wait_for_child(&mut child, Instant::now() + Duration::from_secs(10)).success());
    let begin_sync = b"\x1b[?2026h";
    assert!(transcript
        .windows(begin_sync.len() + 3)
        .any(|window| window.starts_with(begin_sync)
            && window[begin_sync.len()..].starts_with(b"\x1b[")));
    assert!(
        transcript.windows(begin_sync.len() + 12).any(|window| {
            if !window.starts_with(begin_sync) {
                return false;
            }
            let command = &window[begin_sync.len()..];
            command.starts_with(b"\x1b[")
                && command[2..]
                    .iter()
                    .take_while(|byte| byte.is_ascii_digit())
                    .count()
                    > 0
                && command
                    .iter()
                    .position(|byte| *byte == b'A')
                    .is_some_and(|end| end < 8)
        }),
        "relative-anchor mutation did not reach the production frame writer"
    );

    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui_renderer_terminal_lifecycle_child",
            "--nocapture",
        ])
        .env("FINCH_TEST_TUI_TERMINATION", "panic-double-restore")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn double-restoration causal control");
    drop(slave);
    let mut transcript = Vec::new();
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(10),
        |bytes| count_bytes(bytes, b"\x1b[?2004l") >= 2,
    ));
    assert!(!wait_for_child(&mut child, Instant::now() + Duration::from_secs(10)).success());
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 2);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 2);
}

/// A cleanup write failure at synchronized-update finalization must not skip
/// later keyboard/paste/mouse/cursor resets, and its retry must close exactly
/// the one cleanup interval that was opened.
#[cfg(unix)]
#[test]
fn test_tui_cleanup_continues_after_injected_end_sync_failure() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui_renderer_terminal_lifecycle_child",
            "--nocapture",
        ])
        .env("FINCH_TEST_TUI_TERMINATION", "cleanup-end-error")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn cleanup-failure probe in owned PTY");
    drop(slave);

    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| count_bytes(bytes, b"\x1b[?2004l") > 0,
        ),
        "cleanup-failure probe omitted paste reset: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert!(status.success(), "cleanup-failure probe failed: {status}");

    let marker = b"FINCH_CLEANUP_FAILURE_BEGIN";
    let start = transcript
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("cleanup-failure marker missing")
        + marker.len();
    let cleanup = &transcript[start..];
    assert_eq!(count_bytes(cleanup, b"\x1b[?1000h"), 0);
    assert!(count_bytes(cleanup, b"\x1b[?1000l") > 0);
    assert!(count_bytes(cleanup, b"\x1b[?2004l") > 0);
    assert!(count_bytes(cleanup, b"\x1b[<1u") > 0);
    assert!(count_bytes(cleanup, b"\x1b[?25h") > 0);
    assert_eq!(
        count_bytes(cleanup, b"\x1b[?2026h"),
        count_bytes(cleanup, b"\x1b[?2026l")
    );
    assert_eq!(count_bytes(cleanup, b"\x1b[?2026h"), 2);
}

/// Suspend and resume must symmetrically hand off raw mode, bracketed paste,
/// and kitty keyboard enhancement state without ever enabling mouse capture.
#[cfg(unix)]
#[test]
fn test_tui_suspend_resume_releases_and_reacquires_owned_terminal_modes() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui_renderer_terminal_lifecycle_child",
            "--nocapture",
        ])
        .env("FINCH_TEST_TUI_TERMINATION", "suspend-resume")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn suspend/resume probe in owned PTY");
    drop(slave);

    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| count_bytes(bytes, b"FINCH_SHUTDOWN_BEGIN") > 0,
        ),
        "suspend/resume probe did not complete: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert!(status.success(), "suspend/resume probe failed: {status}");

    let suspended = slice_between_markers(
        &transcript,
        b"FINCH_SUSPEND_BEGIN",
        b"FINCH_SUSPENDED_RAW_OFF",
    );
    assert_eq!(count_bytes(suspended, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(suspended, b"\x1b[<1u"), 1);
    assert_eq!(count_bytes(suspended, b"\x1b[?2026h"), 1);
    assert_eq!(count_bytes(suspended, b"\x1b[?2026l"), 1);

    let resumed =
        slice_between_markers(&transcript, b"FINCH_RESUME_BEGIN", b"FINCH_RESUMED_RAW_ON");
    assert_eq!(count_bytes(resumed, b"\x1b[?2004h"), 1);
    assert_eq!(count_bytes(resumed, b"\x1b[>1u"), 1);
    assert_eq!(count_bytes(resumed, b"\x1b[<1u"), 0);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
}

/// SIGTERM and SIGHUP received by the real Finch event loop must take the
/// ordinary shutdown path. The test signals only the exact child it spawned;
/// the repository supervisor retains ownership of the enclosing process group.
#[cfg(unix)]
#[test]
fn test_tui_binary_restores_terminal_modes_on_sigterm_and_sighup() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for signal in [
        nix::sys::signal::Signal::SIGTERM,
        nix::sys::signal::Signal::SIGHUP,
    ] {
        let (mut master, slave) = open_owned_pty();
        let original_modes = terminal_modes(&slave);
        let termios_probe = slave.try_clone().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
            .arg("--cloud-only")
            .env(
                "ANTHROPIC_API_KEY",
                "sk-ant-finch-pty-regression-placeholder",
            )
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("failed to spawn Finch signal probe in owned PTY");
        drop(slave);

        let ready = b"accept edits on";
        let mut transcript = Vec::new();
        assert!(
            read_pty_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(20),
                |bytes| bytes.windows(ready.len()).any(|window| window == ready),
            ),
            "Finch signal probe did not reach input: {}",
            String::from_utf8_lossy(&transcript)
        );

        let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
        nix::sys::signal::kill(owned_pid, signal).expect("failed to signal owned Finch child");
        assert!(
            read_pty_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(10),
                |bytes| {
                    count_bytes(bytes, b"\x1b[?2004l") > 0 && count_bytes(bytes, b"\x1b[<1u") > 0
                },
            ),
            "signal {signal:?} omitted terminal cleanup: {}",
            String::from_utf8_lossy(&transcript)
        );
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
        assert_eq!(terminal_modes(&termios_probe), original_modes);
        let _ = read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(1),
            |_| false,
        );
        assert_signal_exit(status, signal);
        for sequence in MOUSE_ENABLE_SEQUENCES {
            assert_eq!(count_bytes(&transcript, sequence), 0);
        }
        assert!(count_bytes(&transcript, b"\x1b[?1000l") > 0);
        assert!(count_bytes(&transcript, b"\x1b[?2004l") > 0);
        assert!(count_bytes(&transcript, b"\x1b[<1u") > 0);
        assert_eq!(
            count_bytes(&transcript, b"\x1b[?2026h"),
            count_bytes(&transcript, b"\x1b[?2026l"),
            "signal {signal:?} left synchronized output open"
        );
    }
}

/// The listener must own SIGTERM before `TuiRenderer::new` changes raw,
/// bracketed-paste, or keyboard state. This pauses the real Finch constructor
/// after activation and proves cleanup occurs before the final input prompt.
#[cfg(unix)]
#[test]
fn test_tui_binary_restores_terminal_modes_on_signal_during_early_startup() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original_modes = terminal_modes(&slave);
    let termios_probe = slave.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_PAUSE_AFTER_ACTIVATION", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn early-startup signal probe in owned PTY");
    drop(slave);

    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            |bytes| {
                count_bytes(bytes, b"\x1b[?2004h") > 0 && count_bytes(bytes, b"\x1b[>1u") > 0
            },
        ),
        "Finch did not reach the post-activation startup window: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert_eq!(count_bytes(&transcript, b"accept edits on"), 0);

    let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
    nix::sys::signal::kill(owned_pid, nix::sys::signal::Signal::SIGTERM)
        .expect("failed to signal owned early-startup Finch child");
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(2),
            |bytes| count_bytes(bytes, b"\x1b[?2004l") > 0 && count_bytes(bytes, b"\x1b[<1u") > 0,
        ),
        "early-startup SIGTERM did not restore modes promptly: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(20));
    assert_eq!(terminal_modes(&termios_probe), original_modes);
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert_signal_exit(status, nix::sys::signal::Signal::SIGTERM);
    assert_eq!(count_bytes(&transcript, b"accept edits on"), 0);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
    assert_eq!(
        count_bytes(&transcript, b"\x1b[?2026h"),
        count_bytes(&transcript, b"\x1b[?2026l")
    );
}

/// A signal in the narrow raw-mode/protocol activation gap must prevent every
/// later protocol enable while still restoring the exact original termios.
#[cfg(unix)]
#[test]
fn test_tui_binary_signal_between_raw_and_protocol_activation_is_linearized() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original_modes = terminal_modes(&slave);
    let termios_probe = slave.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_PAUSE_AFTER_RAW_MODE", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn raw-only startup signal probe");
    drop(slave);

    let marker = b"FINCH_RAW_ONLY_SIGNAL_READY";
    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            |bytes| bytes.windows(marker.len()).any(|window| window == marker),
        ),
        "Finch did not reach the raw-only activation gap: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 0);
    assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 0);

    let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
    nix::sys::signal::kill(owned_pid, nix::sys::signal::Signal::SIGTERM)
        .expect("failed to signal owned raw-only child");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert_signal_exit(status, nix::sys::signal::Signal::SIGTERM);
    assert_eq!(terminal_modes(&termios_probe), original_modes);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 0);
    assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 0);
    for sequence in MOUSE_ENABLE_SEQUENCES {
        assert_eq!(count_bytes(&transcript, sequence), 0);
    }
}

/// The real `/setup` handoff must not resume Finch after a signal once the
/// wizard owns raw mode, the alternate screen, and mouse reporting.
#[cfg(unix)]
#[test]
fn test_tui_binary_signal_during_setup_handoff_restores_without_resume() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original_modes = terminal_modes(&slave);
    let termios_probe = slave.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .env("FINCH_TEST_SETUP_SIGNAL_PAUSE_AFTER_ACTIVATION", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn setup handoff signal probe");
    drop(slave);

    let mut transcript = Vec::new();
    let prompt = b"accept edits on";
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(20),
        |bytes| bytes.windows(prompt.len()).any(|window| window == prompt),
    ));
    master.write_all(b"/setup\r").unwrap();
    master.flush().unwrap();
    let marker = b"FINCH_SETUP_SIGNAL_READY";
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes.windows(marker.len()).any(|window| window == marker),
        ),
        "setup did not reach its activated signal window: {}",
        String::from_utf8_lossy(&transcript)
    );
    let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
    nix::sys::signal::kill(owned_pid, nix::sys::signal::Signal::SIGHUP)
        .expect("failed to signal owned setup child");
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert_signal_exit(status, nix::sys::signal::Signal::SIGHUP);
    assert_eq!(terminal_modes(&termios_probe), original_modes);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000l"), 2);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
}

/// Terminal restoration must not wait for an already-selected event-loop
/// branch to finish. The branch remains pending for four seconds; SIGHUP must
/// reset modes within two seconds through the independently scheduled owner.
#[cfg(unix)]
#[test]
fn test_tui_binary_restores_terminal_modes_while_event_branch_is_busy() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let original_modes = terminal_modes(&slave);
    let termios_probe = slave.try_clone().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_BUSY_BRANCH", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn busy-branch signal probe in owned PTY");
    drop(slave);

    let mut transcript = Vec::new();
    let ready = b"accept edits on";
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            |bytes| bytes.windows(ready.len()).any(|window| window == ready),
        ),
        "busy-branch signal probe did not reach input: {}",
        String::from_utf8_lossy(&transcript)
    );
    master
        .write_all(b"__finch_test_busy_terminal_branch__\r")
        .expect("failed to submit busy-branch probe");
    master.flush().expect("failed to flush busy-branch probe");
    let busy = b"FINCH_BUSY_TERMINAL_BRANCH_BEGIN";
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes.windows(busy.len()).any(|window| window == busy),
        ),
        "event loop did not enter the supervised busy branch: {}",
        String::from_utf8_lossy(&transcript)
    );

    let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
    nix::sys::signal::kill(owned_pid, nix::sys::signal::Signal::SIGHUP)
        .expect("failed to signal owned busy-branch Finch child");
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(2),
            |bytes| count_bytes(bytes, b"\x1b[?2004l") > 0 && count_bytes(bytes, b"\x1b[<1u") > 0,
        ),
        "busy-branch SIGHUP waited for the selected branch: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    assert_eq!(terminal_modes(&termios_probe), original_modes);
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert_signal_exit(status, nix::sys::signal::Signal::SIGHUP);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
    assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 1);
    assert_eq!(
        count_bytes(&transcript, b"\x1b[?2026h"),
        count_bytes(&transcript, b"\x1b[?2026l")
    );
}

/// Suspended setup/editor handoffs must never resume after shutdown, and a
/// backpressured writer plus repeated signals must still terminate boundedly.
#[cfg(unix)]
#[test]
fn test_tui_signal_termination_is_bounded_across_handoffs_and_backpressure() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    for (mode, marker, first_signal, repeated_signal) in [
        (
            "suspended-signal",
            b"FINCH_SUSPENDED_SIGNAL_READY".as_slice(),
            nix::sys::signal::Signal::SIGTERM,
            None,
        ),
        (
            "editor-signal",
            b"FINCH_EDITOR_SIGNAL_READY".as_slice(),
            nix::sys::signal::Signal::SIGHUP,
            None,
        ),
        (
            "backpressure-signal",
            b"FINCH_BACKPRESSURE_SIGNAL_READY".as_slice(),
            nix::sys::signal::Signal::SIGTERM,
            Some(nix::sys::signal::Signal::SIGHUP),
        ),
    ] {
        let (mut master, slave) = open_owned_pty();
        let original_modes = terminal_modes(&slave);
        let termios_probe = slave.try_clone().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui_renderer_terminal_lifecycle_child",
                "--nocapture",
            ])
            .env("FINCH_TEST_TUI_TERMINATION", mode)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("failed to spawn handoff/backpressure signal probe");
        drop(slave);

        let mut transcript = Vec::new();
        assert!(
            read_pty_until(
                &mut master,
                &mut transcript,
                Instant::now() + Duration::from_secs(10),
                |bytes| bytes.windows(marker.len()).any(|window| window == marker),
            ),
            "{mode} did not reach its signal window: {}",
            String::from_utf8_lossy(&transcript)
        );
        let owned_pid = nix::unistd::Pid::from_raw(child.id() as i32);
        nix::sys::signal::kill(owned_pid, first_signal).expect("failed to signal owned child");
        if let Some(signal) = repeated_signal {
            std::thread::sleep(Duration::from_millis(50));
            nix::sys::signal::kill(owned_pid, signal)
                .expect("failed to repeat signal against owned child");
        }
        let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(2));
        let _ = read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(1),
            |_| false,
        );
        assert_signal_exit(status, first_signal);
        assert_eq!(terminal_modes(&termios_probe), original_modes);
        for sequence in MOUSE_ENABLE_SEQUENCES {
            assert_eq!(count_bytes(&transcript, sequence), 0);
        }
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004h"), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[?2004l"), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[>1u"), 1);
        assert_eq!(count_bytes(&transcript, b"\x1b[<1u"), 1);
    }
}

/// Repeated live rendering and viewport reconstruction must use cursor motion,
/// never linefeeds that make transient rows eligible for native history.
#[cfg(unix)]
#[test]
fn test_tui_repeated_live_render_and_resize_never_emits_history_linefeeds() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let marker = b"FINCH_LIVE_HISTORY_PROBE_BEGIN";
    let disable_paste = b"\x1b[?2004l";
    let begin_sync = b"\x1b[?2026h";
    let end_sync = b"\x1b[?2026l";
    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tui_renderer_terminal_lifecycle_child",
            "--nocapture",
        ])
        .env("FINCH_TEST_TUI_TERMINATION", "render-resize")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn live-history probe in owned PTY");
    drop(slave);

    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| count_bytes(bytes, disable_paste) > 0,
        ),
        "live-history probe omitted terminal cleanup: {}",
        String::from_utf8_lossy(&transcript)
    );
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );
    assert!(status.success(), "live-history probe failed: {status}");

    let start = transcript
        .windows(marker.len())
        .position(|window| window == marker)
        .expect("live-history marker missing")
        + marker.len();
    let end = transcript[start..]
        .windows(disable_paste.len())
        .position(|window| window == disable_paste)
        .map(|offset| start + offset)
        .expect("terminal cleanup boundary missing");
    let live = &transcript[start..end];
    assert!(count_bytes(live, b"TRANSIENTRED") > 0);
    assert!(count_bytes(live, b"SECOND") > 0);
    assert_eq!(count_bytes(live, b"\x1b[31m"), 0);
    assert!(!live.contains(&b'\n'), "live output emitted a linefeed");
    assert_eq!(count_bytes(live, begin_sync), count_bytes(live, end_sync));
    assert!(
        count_bytes(live, begin_sync) >= 8,
        "expected repeated renders"
    );
}

#[cfg(unix)]
fn absolute_cursor_rows(bytes: &[u8]) -> Vec<usize> {
    let mut rows = Vec::new();
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let start = index + 2;
        let Some(end) = bytes[start..]
            .iter()
            .position(|byte| byte.is_ascii_alphabetic())
            .map(|offset| start + offset)
        else {
            break;
        };
        if bytes[end] == b'H' {
            let parameters = String::from_utf8_lossy(&bytes[start..end]);
            if let Some(row) = parameters
                .split(';')
                .next()
                .and_then(|row| row.parse::<usize>().ok())
            {
                rows.push(row);
            }
        }
        index = end + 1;
    }
    rows
}

/// Real Finch input drives draft, completion, dialog, and streamed WorkUnit
/// growth/shrink without a resize. Every live frame must re-anchor absolutely
/// inside the 24-row screen and must not add a native-history linefeed.
#[cfg(unix)]
#[test]
fn test_tui_binary_dynamic_live_paths_preserve_screen_cursor_and_transcript() {
    if !supervised_pty_authority_or_skip() {
        return;
    }
    let (mut master, slave) = open_owned_pty();
    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env(
            "ANTHROPIC_API_KEY",
            "sk-ant-finch-pty-regression-placeholder",
        )
        .env("FINCH_TEST_TUI_DYNAMIC_FRAMES", "1")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn dynamic live-frame probe");
    drop(slave);

    let mut transcript = Vec::new();
    let prompt = b"accept edits on";
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(20),
        |bytes| bytes.windows(prompt.len()).any(|window| window == prompt),
    ));
    master
        .write_all(b"__finch_test_dynamic_terminal_frames__\r")
        .unwrap();
    master.flush().unwrap();
    let done = b"FINCH_DYNAMIC_PROBE_DONE";
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes.windows(done.len()).any(|window| window == done),
        ),
        "dynamic live probe did not complete: {}",
        String::from_utf8_lossy(&transcript)
    );
    let after = b"FINCH_CANONICAL_AFTER";
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(5),
        |bytes| bytes.windows(after.len()).any(|window| window == after),
    ));
    master.write_all(b"/quit\r").unwrap();
    master.flush().unwrap();
    let disable_paste = b"\x1b[?2004l";
    assert!(read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(10),
        |bytes| bytes
            .windows(disable_paste.len())
            .any(|window| window == disable_paste),
    ));
    let status = wait_for_child(&mut child, Instant::now() + Duration::from_secs(10));
    assert!(status.success());
    let _ = read_pty_until(
        &mut master,
        &mut transcript,
        Instant::now() + Duration::from_secs(1),
        |_| false,
    );

    let live = slice_between_markers(
        &transcript,
        b"FINCH_DYNAMIC_PROBE_BEGIN",
        b"FINCH_DYNAMIC_PROBE_DONE",
    );
    assert!(!live.contains(&b'\n'), "live paths entered native history");
    assert!(count_bytes(live, b"draft-one") > 0);
    assert!(count_bytes(live, b"Commands") > 0);
    assert!(count_bytes(live, b"FINCH_DYNAMIC_DIALOG") > 0);
    assert!(count_bytes(live, b"stream-four") > 0);
    let anchors = absolute_cursor_rows(live);
    assert!(anchors.len() >= 6, "each erase/draw must anchor absolutely");
    assert!(anchors.iter().all(|row| (1..=24).contains(row)));
    let distinct = anchors
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert!(
        distinct.len() >= 3,
        "dynamic frame height never changed its bottom anchor: {anchors:?}"
    );
    // A full-screen repaint may project canonical rows again with cursor
    // movement, but only the commit boundary may append them to native
    // history with CRLF.
    assert_eq!(count_bytes(&transcript, b"FINCH_CANONICAL_BEFORE\r\n"), 1);
    assert_eq!(count_bytes(&transcript, b"FINCH_CANONICAL_AFTER\r\n"), 1);
    assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
}

/// Test that TUI initializes without crashing
#[test]
#[ignore] // Requires interactive terminal or expect
fn test_tui_initialization() {
    // This test should be run with expect or a PTY library
    // For now, we just verify the binary runs

    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Failed to spawn shammah");

    // Send exit command
    if let Some(mut stdin) = child.stdin.take() {
        writeln!(stdin, "/exit").ok();
    }

    // Wait for exit (with timeout)
    let status = child.wait().expect("Failed to wait for child");
    assert!(status.success() || status.code() == Some(0));
}

/// Test that TUI components are available (basic compilation test)
#[test]
fn test_tui_module_exists() {
    // Just verify the TUI module compiles and is accessible
    // Internal details are tested via unit tests in src/
    assert!(true);
}

/// Test TUI output manager integration
#[test]
fn test_output_manager() {
    use finch::cli::OutputManager;

    let manager = OutputManager::new(finch::config::ColorScheme::default());

    // Test stdout control
    manager.disable_stdout();
    // Just verify it doesn't crash
    manager.enable_stdout();
    // Manager methods work without panicking
}

/// Test that piped input mode doesn't try to use TUI
#[test]
fn test_non_interactive_mode() {
    // When stdin is not a TTY, TUI should not be used. Keep this test local
    // and deterministic: ordinary English invokes the configured provider and
    // used to make the suite wait through network retries.
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("query")
        .arg("(say \"test\")")
        .output()
        .expect("Failed to run query");

    // Should complete without TUI (no escape codes in stderr)
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Basic check - proper TUI wouldn't work in non-interactive mode
    assert!(!stderr.contains("raw mode"));
}

/// E2E: tool message format uses correct Unicode characters (⏺, ⎿)
///
/// This verifies the fix for the old ●/└ characters that caused visual regressions.
/// Uses the WorkUnit API (the live rendering path) — no terminal needed.
#[test]
fn test_tool_display_uses_correct_unicode() {
    use finch::cli::messages::{Message, WorkUnit};
    use finch::cli::repl_event::tool_display::format_tool_label;
    use finch::config::ColorScheme;

    let label = format_tool_label("bash", &serde_json::json!({"command": "echo hi"}));
    let unit = WorkUnit::new("Running");
    let row_idx = unit.add_row(label);
    unit.complete_row(row_idx, "hi");
    unit.set_complete();

    let result = unit.format(&ColorScheme::default());

    // Must use ⏺ (U+23FA) as bullet, NOT ● (U+25CF)
    assert!(
        result.contains('⏺'),
        "Expected ⏺ (U+23FA), got: {:?}",
        result
    );
    assert!(
        !result.contains('●'),
        "Found old ● (U+25CF) — wrong bullet char in: {:?}",
        result
    );

    // Must use ⎿ (U+23BF) as output prefix, NOT └ (U+2514)
    assert!(
        result.contains('⎿'),
        "Expected ⎿ (U+23BF), got: {:?}",
        result
    );
    assert!(
        !result.contains('└'),
        "Found old └ (U+2514) — wrong corner char in: {:?}",
        result
    );
}

/// Input token count — the `↑ N.Nk` status-bar format
///
/// Verifies that `format_token_count` (used to render the status bar during
/// streaming) produces the expected compact representation from the public API.
/// The function is also tested internally; this is an integration-level smoke
/// test confirming the public export is stable.
#[test]
fn test_format_token_count_public_api() {
    use finch::cli::repl_event::tool_display::format_token_count;

    // Below 1000 → plain decimal
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");

    // At 1000 → switch to "N.Nk" notation
    assert_eq!(format_token_count(1000), "1.0k");
    assert_eq!(format_token_count(1500), "1.5k");
    assert_eq!(format_token_count(8192), "8.2k"); // common context window chunk

    // Large counts
    assert_eq!(format_token_count(100_000), "100.0k");
}

/// The status-bar "↑ input" format is correctly assembled from format_token_count.
#[test]
fn test_input_token_status_bar_format() {
    use finch::cli::repl_event::tool_display::format_token_count;

    let input_tokens: u32 = 1250;
    let output_tokens: usize = 300;

    // Simulate the status-bar assembly used in event_loop.rs during streaming
    let status = format!(
        "↑ {} · ↓ {} tokens",
        format_token_count(input_tokens as usize),
        format_token_count(output_tokens),
    );
    assert_eq!(status, "↑ 1.2k · ↓ 300 tokens");
}

/// E2E: binary exits cleanly without panicking when invoked with --version
#[test]
fn test_binary_exits_cleanly() {
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--version")
        .output()
        .expect("Failed to run finch --version");

    // Should not crash or produce panic output
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked"),
        "unexpected panic in stderr: {}",
        stderr
    );
    assert!(
        !stderr.contains("RUST_BACKTRACE"),
        "unexpected backtrace in stderr: {}",
        stderr
    );
}
