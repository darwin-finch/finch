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

/// Production-boundary regression for Finch's real terminal lifecycle.
///
/// The child inherits the repository test supervisor's isolated process group
/// and disposable HOME. The test owns both PTY descriptors and asks Finch to
/// quit normally; it never signals a PID or starts a daemon.
#[cfg(unix)]
#[test]
fn test_tui_binary_advertises_selection_override_and_restores_mouse_mode() {
    let isolated_home = std::env::var_os("FINCH_BRAIN_TEST_ROOT")
        .expect("run this PTY regression through scripts/test_brains.sh");
    assert!(!isolated_home.is_empty());

    let mut master_fd = -1;
    let mut slave_fd = -1;
    let size = nix::libc::winsize {
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
            std::ptr::null(),
            &size,
        )
    };
    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );

    let mut master = unsafe { std::fs::File::from_raw_fd(master_fd) };
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

    let mut child = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("--cloud-only")
        .env("ANTHROPIC_API_KEY", "finch-pty-regression-placeholder")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .expect("failed to spawn Finch in owned PTY");
    drop(slave);

    let hint = finch::cli::MOUSE_SELECTION_HINT.as_bytes();
    let enable_mouse = b"\x1b[?1000h";
    let disable_mouse = b"\x1b[?1000l";
    let mut transcript = Vec::new();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(20),
            |bytes| bytes.windows(hint.len()).any(|window| window == hint)
                && bytes
                    .windows(enable_mouse.len())
                    .any(|window| window == enable_mouse),
        ),
        "Finch startup did not advertise selection and enable mouse capture: {}",
        String::from_utf8_lossy(&transcript)
    );

    // Exercise the real event-loop suspend → setup wizard → resume path. The
    // wizard owns mouse reporting while active; after Escape, the default REPL
    // must enable its clickable transcript controls again.
    master.write_all(b"/setup\r").unwrap();
    master.flush().unwrap();
    let alternate_screen = b"\x1b[?1049h";
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes
                .windows(alternate_screen.len())
                .any(|window| window == alternate_screen),
        ),
        "setup wizard did not take ownership of the PTY: {}",
        String::from_utf8_lossy(&transcript)
    );
    master.write_all(b"\x1b").unwrap();
    master.flush().unwrap();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| count_bytes(bytes, enable_mouse) >= 3,
        ),
        "Finch did not restore mouse capture after setup: {}",
        String::from_utf8_lossy(&transcript)
    );

    master.write_all(b"/quit\r").unwrap();
    master.flush().unwrap();
    assert!(
        read_pty_until(
            &mut master,
            &mut transcript,
            Instant::now() + Duration::from_secs(10),
            |bytes| bytes
                .windows(disable_mouse.len())
                .any(|window| window == disable_mouse),
        ),
        "Finch shutdown did not disable mouse capture: {}",
        String::from_utf8_lossy(&transcript)
    );
    assert!(
        count_bytes(&transcript, disable_mouse) >= 3,
        "suspend, wizard cleanup, and shutdown must each disable mouse capture"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("failed to inspect Finch child") {
            break status;
        }
        assert!(Instant::now() < deadline, "Finch did not exit after /quit");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success(), "Finch exited unsuccessfully: {status}");
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
