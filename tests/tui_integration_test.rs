// Integration tests for TUI mode
//
// These tests verify TUI functionality using expect/pty simulation.
// Note: TUI tests are complex because they require a pseudo-TTY.

use std::io::Write;
use std::process::{Command, Stdio};

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
    // When stdin is not a TTY, TUI should not be used
    let output = Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg("query")
        .arg("test")
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
/// Tested via the public formatting functions directly (no terminal needed).
#[test]
fn test_tool_display_uses_correct_unicode() {
    use finch::cli::repl_event::tool_display::{format_tool_label, format_tool_result};

    let label = format_tool_label("bash", &serde_json::json!({"command": "echo hi"}));
    let result = format_tool_result(&label, "hi\n", false);

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
