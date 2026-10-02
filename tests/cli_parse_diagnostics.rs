//! Production-boundary checks for safe root CLI parse diagnostics.

use std::ffi::OsString;
use std::process::{Command, Output};

fn run_finch(arguments: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(env!("CARGO_BIN_EXE_finch"))
        .args(arguments)
        .output()
        .expect("the supervised Finch binary must run to completion")
}

fn assert_usage_error(output: &Output, context: &str) -> String {
    assert_eq!(
        output.status.code(),
        Some(2),
        "{context}: a root CLI parse failure must retain Clap exit status 2; status={:?} stdout={:?} stderr={:?}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.is_empty(),
        "{context}: a root CLI parse failure must write only to stderr; stdout={:?} stderr={:?}",
        output.stdout,
        output.stderr
    );
    let stderr = String::from_utf8(output.stderr.clone())
        .expect("root CLI parse diagnostics must remain valid UTF-8");
    assert!(
        (stderr.contains("Usage:") || stderr.contains("[possible values:"))
            && stderr.contains("For more information, try '--help'."),
        "{context}: sanitising a reflected value must retain actionable usage or value choices and help; stderr={stderr:?}"
    );
    stderr
}

fn assert_only_structural_newlines(stderr: &str, context: &str) {
    let unsafe_controls: Vec<(usize, char)> = stderr
        .char_indices()
        .filter(|(_, character)| {
            *character != '\n'
                && (character.is_control() || matches!(*character as u32, 0x7f..=0x9f))
        })
        .collect();
    assert!(
        unsafe_controls.is_empty(),
        "{context}: captured non-TTY stderr may contain Finch-owned LF separators but no other C0/C1 controls; unsafe_controls={unsafe_controls:?} stderr={stderr:?}"
    );
}

#[test]
fn test_root_cli_parse_errors_render_hostile_controls_on_one_speakable_line() {
    let cases = [
        ("line-feed", '\n', "\\n"),
        ("carriage-return", '\r', "\\r"),
        ("tab", '\t', "\\t"),
        ("start-of-heading", '\u{0001}', "\\u{0001}"),
        ("unit-separator", '\u{001f}', "\\u{001f}"),
        ("delete", '\u{007f}', "\\u{007f}"),
        ("c1-start", '\u{0080}', "\\u{0080}"),
        ("c1-end", '\u{009f}', "\\u{009f}"),
    ];

    for (name, control, escaped) in cases {
        for (path, arguments) in [
            (
                "invalid enum value",
                vec![
                    "auth".into(),
                    "status".into(),
                    format!("bad{control}value").into(),
                ],
            ),
            (
                "unknown argument",
                vec![format!("--bad{control}option").into()],
            ),
            (
                "unknown subcommand",
                vec![format!("bad{control}command").into()],
            ),
        ] {
            let context = format!("{path} with {name}");
            let output = run_finch(arguments);
            let stderr = assert_usage_error(&output, &context);
            assert_only_structural_newlines(&stderr, &context);
            assert!(
                stderr.contains(&format!("bad{escaped}")),
                "{context}: the hostile value must remain recognizable as one escaped, speakable token; expected_escape={escaped:?} stderr={stderr:?}"
            );
        }
    }
}

#[test]
fn test_root_cli_parse_error_controls_preserve_unicode_suggestions_and_ansi_safety() {
    let unicode = run_finch(["auth".into(), "status".into(), "café-東京".into()]);
    let unicode_stderr = assert_usage_error(&unicode, "printable Unicode invalid value");
    assert!(
        unicode_stderr.contains("café-東京"),
        "printable Unicode must remain legible in a reflected parse diagnostic; stderr={unicode_stderr:?}"
    );

    let typo = run_finch(["statsu".into()]);
    let typo_stderr = assert_usage_error(&typo, "nearby subcommand typo");
    assert!(
        typo_stderr.contains("tip:") && typo_stderr.contains("status"),
        "sanitising root parse errors must preserve Clap typo suggestions; stderr={typo_stderr:?}"
    );

    let ansi = run_finch([
        "auth".into(),
        "status".into(),
        "evil\u{001b}[31mRED\u{001b}[0m".into(),
    ]);
    let ansi_stderr = assert_usage_error(&ansi, "ANSI-bearing invalid value");
    assert!(
        !ansi_stderr.as_bytes().contains(&0x1b),
        "ANSI-bearing arguments must not place ESC in redirected diagnostics; stderr={ansi_stderr:?}"
    );
    assert!(
        ansi_stderr.contains("evilRED"),
        "existing ANSI stripping must retain the printable argument text; stderr={ansi_stderr:?}"
    );
}

#[cfg(unix)]
#[test]
fn test_root_cli_invalid_utf8_diagnostic_remains_non_reflective() {
    use std::os::unix::ffi::OsStringExt;

    let output = run_finch([
        "auth".into(),
        "status".into(),
        OsString::from_vec(vec![b'b', b'a', b'd', 0xff, b'x']),
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "invalid UTF-8 argv must retain Clap exit status 2; status={:?} stdout={:?} stderr={:?}",
        output.status,
        output.stdout,
        output.stderr
    );
    assert!(
        output.stdout.is_empty()
            && String::from_utf8_lossy(&output.stderr).contains("invalid UTF-8"),
        "invalid UTF-8 argv must remain stderr-only and must not reflect raw bytes; stdout={:?} stderr={:?}",
        output.stdout,
        output.stderr
    );
    assert!(
        !output.stderr.contains(&0xff),
        "invalid UTF-8 argv must not be reflected into stderr; stderr={:?}",
        output.stderr
    );
}

#[cfg(unix)]
#[test]
fn test_root_cli_invalid_utf8_in_argv1_remains_non_panicking_and_non_reflective() {
    use std::os::unix::ffi::OsStringExt;

    let output = run_finch([OsString::from_vec(vec![b'b', b'a', b'd', 0xff, b'x'])]);
    let stderr = assert_usage_error(&output, "invalid UTF-8 in argv[1]");
    assert!(
        stderr.contains("unrecognized subcommand") || stderr.contains("invalid UTF-8"),
        "invalid UTF-8 in argv[1] must fall through to Clap usage diagnostic; stderr={stderr:?}"
    );
    assert!(
        !output.stderr.contains(&0xff),
        "invalid UTF-8 in argv[1] must not reflect raw bytes into stderr; stderr={:?}",
        output.stderr
    );
}

#[cfg(unix)]
#[test]
fn test_root_cli_near_miss_mcp_bridge_flag_with_invalid_utf8_falls_through_to_clap() {
    use std::os::unix::ffi::OsStringExt;

    let mut bad_flag = finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG
        .as_bytes()
        .to_vec();
    bad_flag.push(0xff);
    let output = run_finch([OsString::from_vec(bad_flag)]);
    let stderr = assert_usage_error(&output, "near-miss MCP bridge flag with invalid UTF-8");
    assert!(
        stderr.contains("unexpected argument")
            || stderr.contains("unrecognized subcommand")
            || stderr.contains("invalid UTF-8"),
        "near-miss MCP bridge flag must fall through to Clap usage diagnostic; stderr={stderr:?}"
    );
    assert!(
        !output.stderr.contains(&0xff),
        "near-miss MCP bridge flag must not reflect raw bytes into stderr; stderr={:?}",
        output.stderr
    );

    // Verify that the exact valid hidden flag selects the bridge (exits 0 on EOF stdin),
    // proving near-miss byte sequences cannot select it.
    let valid_bridge = run_finch([finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG.into()]);
    assert_eq!(
        valid_bridge.status.code(),
        Some(0),
        "exact valid MCP bridge flag must select the bridge loop and exit 0 on EOF stdin"
    );
}
