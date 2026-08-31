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
    let enable_mouse = b"\x1b[?1000h";
    let disable_mouse = b"\x1b[?1000l";
    let disable_paste = b"\x1b[?2004l";
    let pop_keyboard = b"\x1b[<1u";
    let begin_sync = b"\x1b[?2026h";
    let end_sync = b"\x1b[?2026l";

    for termination in ["clean", "drop", "error", "panic", "init-error"] {
        let (mut master, slave) = open_owned_pty();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui_renderer_terminal_lifecycle_child",
                "--nocapture",
            ])
            .env("FINCH_TEST_TUI_TERMINATION", termination)
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .spawn()
            .expect("failed to spawn renderer lifecycle probe in owned PTY");
        drop(slave);

        let mut transcript = Vec::new();
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
        assert_eq!(
            count_bytes(&transcript, enable_mouse),
            0,
            "{termination} path enabled mouse reporting"
        );
        assert!(count_bytes(&transcript, disable_paste) > 0);
        assert!(count_bytes(&transcript, pop_keyboard) > 0);
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
        let _ = read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(1),
            |_| false,
        );
        assert!(
            status.success(),
            "signal {signal:?} Finch exit failed: {status}"
        );
        assert_eq!(count_bytes(&transcript, b"\x1b[?1000h"), 0);
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
