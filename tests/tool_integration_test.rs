// Integration test for tool parsing and formatting
//
// Tests the full flow: format tools → parse tool calls → execute

use finch::models::{ToolCallParser, ToolPromptFormatter};
use finch::tools::types::{ToolDefinition, ToolInputSchema};

#[cfg(unix)]
use finch::tools::implementations::EditTool;
#[cfg(unix)]
use finch::tools::types::ToolContext;
#[cfg(unix)]
use finch::tools::Tool;
#[cfg(unix)]
use nix::pty::openpty;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::process::{Command, Stdio};

#[test]
fn test_tool_prompt_formatting() {
    let tools = vec![
        ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to the file")]),
        },
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a shell command".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("command", "Command to execute"),
                ("description", "What the command does"),
            ]),
        },
    ];

    let formatted = ToolPromptFormatter::format_tools_for_prompt(&tools);

    // Verify format contains key elements
    assert!(formatted.contains("# Available Tools"));
    assert!(formatted.contains("### read"));
    assert!(formatted.contains("### bash"));
    assert!(formatted.contains("Read a file from disk"));
    assert!(formatted.contains("Execute a shell command"));
    assert!(formatted.contains("file_path"));
    assert!(formatted.contains("command"));
    assert!(formatted.contains("<tool_use>"));
    assert!(formatted.contains("<name>"));
    assert!(formatted.contains("<parameters>"));
}

#[test]
fn test_tool_call_parsing_single() {
    let output = r#"I'll read the file for you.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Let me know if you need anything else."#;

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "read");
    assert_eq!(tool_uses[0].input["file_path"], "/tmp/test.txt");
    assert!(tool_uses[0].id.starts_with("toolu_"));
}

#[test]
fn test_tool_call_parsing_multiple() {
    let output = r#"First, I'll glob for files:

<tool_use>
  <name>glob</name>
  <parameters>{"pattern": "**/*.rs"}</parameters>
</tool_use>

Then I'll grep for the pattern:

<tool_use>
  <name>grep</name>
  <parameters>{"pattern": "TODO", "path": "."}</parameters>
</tool_use>

Done!"#;

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 2);
    assert_eq!(tool_uses[0].name, "glob");
    assert_eq!(tool_uses[0].input["pattern"], "**/*.rs");
    assert_eq!(tool_uses[1].name, "grep");
    assert_eq!(tool_uses[1].input["pattern"], "TODO");
}

#[test]
fn test_tool_call_parsing_compact() {
    let output =
        "<tool_use><name>bash</name><parameters>{\"command\":\"ls -la\"}</parameters></tool_use>";

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "bash");
    assert_eq!(tool_uses[0].input["command"], "ls -la");
}

#[test]
fn test_tool_call_parsing_invalid_json() {
    let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{invalid json}</parameters>
</tool_use>
"#;

    let result = ToolCallParser::parse(output);
    assert!(result.is_err());
}

#[test]
fn test_extract_text() {
    let output = r#"I'll help you with that.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Let me know if you need anything else."#;

    let text = ToolCallParser::extract_text(output);

    assert!(!text.contains("<tool_use>"));
    assert!(!text.contains("read"));
    assert!(!text.contains("file_path"));
    assert!(text.contains("I'll help you"));
    assert!(text.contains("Let me know"));
}

#[test]
fn test_has_tool_calls() {
    assert!(ToolCallParser::has_tool_calls("<tool_use>"));
    assert!(ToolCallParser::has_tool_calls("text <tool_use> more text"));
    assert!(!ToolCallParser::has_tool_calls("no tools here"));
    assert!(!ToolCallParser::has_tool_calls(""));
}

#[test]
fn test_tool_call_with_complex_json() {
    let output = r#"
<tool_use>
  <name>grep</name>
  <parameters>{
    "pattern": "fn main",
    "path": "src/",
    "case_insensitive": true,
    "max_results": 10
  }</parameters>
</tool_use>
"#;

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "grep");
    assert_eq!(tool_uses[0].input["pattern"], "fn main");
    assert_eq!(tool_uses[0].input["path"], "src/");
    assert_eq!(tool_uses[0].input["case_insensitive"], true);
    assert_eq!(tool_uses[0].input["max_results"], 10);
}

#[cfg(unix)]
fn editor_boundary_case(case: &str) -> (&'static str, &'static str, &'static str) {
    match case {
        "blank-context" => ("alpha\n\nbefore\n\nomega\n", "before", "after"),
        "tab" => ("all:\n\tcargo build\n", "\tcargo build", "    cargo build"),
        "control" => (
            "PROMPT='\u{1b}]0;old\u{7}'\n",
            "PROMPT='\u{1b}]0;old\u{7}'",
            "PROMPT='\u{1b}]0;new\u{7}'",
        ),
        "cancel" | "body-change" | "nonzero" | "editor-fallback" => ("before\n", "before", "after"),
        "changed-whitespace" => ("before\n", "before", "after "),
        other => panic!("unknown editor-boundary case {other:?}"),
    }
}

#[cfg(unix)]
fn editor_boundary_context() -> ToolContext<'static> {
    ToolContext {
        conversation: None,
        save_models: None,
        batch_trainer: None,
        local_generator: None,
        tokenizer: None,
        repl_mode: None,
        plan_content: None,
        live_output: None,
        effect_audit: None,
        poset: None,
    }
}

#[cfg(unix)]
fn run_editor_boundary_child() {
    let case = std::env::var("FINCH_EDITOR_BOUNDARY_CASE")
        .expect("child must receive FINCH_EDITOR_BOUNDARY_CASE");
    let target = std::env::var("FINCH_EDITOR_BOUNDARY_TARGET")
        .expect("child must receive FINCH_EDITOR_BOUNDARY_TARGET");
    let report = std::env::var("FINCH_EDITOR_BOUNDARY_REPORT")
        .expect("child must receive FINCH_EDITOR_BOUNDARY_REPORT");
    let (_, old_string, new_string) = editor_boundary_case(&case);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child must create its tool runtime");
    let result = runtime.block_on(EditTool.execute(
        serde_json::json!({
            "file_path": target,
            "old_string": old_string,
            "new_string": new_string,
        }),
        &editor_boundary_context(),
    ));
    let payload = match result {
        Ok(value) => serde_json::json!({"ok": true, "detail": value}),
        Err(error) => serde_json::json!({"ok": false, "detail": format!("{error:#}")}),
    };
    fs::write(&report, serde_json::to_vec(&payload).unwrap())
        .unwrap_or_else(|error| panic!("child failed to write report {report:?}: {error}"));
}

#[cfg(unix)]
fn write_fake_editor(path: &std::path::Path, identity: &str) {
    let script = format!(
        "#!/bin/sh\n\
         set -eu\n\
         printf '%s' '{identity}' > \"$FINCH_EDITOR_SELECTED\"\n\
         printf '%s' \"$#\" > \"$FINCH_EDITOR_ARGC\"\n\
         printf '%s' \"$1\" > \"$FINCH_EDITOR_ARGV\"\n\
         cp \"$1\" \"$FINCH_EDITOR_CAPTURE\"\n\
         case \"$FINCH_EDITOR_ACTION\" in\n\
           accept) : ;;\n\
           cancel) sed 's/# finch: action=execute/# finch: action=cancel/' \"$1\" > \"$1.next\"; cp \"$1.next\" \"$1\" ;;\n\
           body-change) printf '\\n+not-reviewed\\n' >> \"$1\" ;;\n\
           changed-whitespace) sed 's/[[:space:]]*$//' \"$1\" > \"$1.next\"; cp \"$1.next\" \"$1\" ;;\n\
           nonzero) printf '%s' '37' > \"$FINCH_EDITOR_EXIT\"; exit 37 ;;\n\
           *) exit 92 ;;\n\
         esac\n"
    );
    fs::write(path, script)
        .unwrap_or_else(|error| panic!("failed to write fake editor {}: {error}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|error| {
        panic!(
            "failed to make fake editor {} executable: {error}",
            path.display()
        )
    });
}

/// Production-boundary regression for the interactive edit path.
///
/// The parent gives this same integration-test binary a PTY. The child then
/// calls `EditTool::execute`, whose real terminal check selects the interactive
/// branch and whose real `$VISUAL`/`$EDITOR` resolver launches an executable
/// fake editor with the temporary `.diff` path as argv. This deliberately does
/// not use the injectable unit-test editor seam.
#[cfg(unix)]
#[test]
fn test_edit_tool_uses_real_editor_process_boundary_and_fails_closed() {
    if std::env::var_os("FINCH_EDITOR_BOUNDARY_CHILD").is_some() {
        run_editor_boundary_child();
        return;
    }

    for case in [
        "blank-context",
        "tab",
        "control",
        "cancel",
        "body-change",
        "changed-whitespace",
        "nonzero",
        "editor-fallback",
    ] {
        let dir = tempfile::tempdir().expect("editor-boundary temp directory");
        let target = dir.path().join("target.txt");
        let report = dir.path().join("report.json");
        let capture = dir.path().join("artifact.diff");
        let selected = dir.path().join("selected.txt");
        let argc = dir.path().join("argc.txt");
        let argv = dir.path().join("argv.txt");
        let editor_exit = dir.path().join("editor-exit.txt");
        let visual = dir.path().join("visual-editor");
        let editor = dir.path().join("fallback-editor");
        let (original, _old_string, new_string) = editor_boundary_case(case);
        fs::write(&target, original)
            .unwrap_or_else(|error| panic!("{case}: failed to seed target: {error}"));
        write_fake_editor(&visual, "VISUAL");
        write_fake_editor(&editor, "EDITOR");

        let pty = openpty(None, None).unwrap_or_else(|error| panic!("{case}: openpty: {error}"));
        let mut command = Command::new(std::env::current_exe().expect("integration-test path"));
        command
            .args([
                "--exact",
                "test_edit_tool_uses_real_editor_process_boundary_and_fails_closed",
                "--nocapture",
            ])
            .env("FINCH_EDITOR_BOUNDARY_CHILD", "1")
            .env("FINCH_EDITOR_BOUNDARY_CASE", case)
            .env("FINCH_EDITOR_BOUNDARY_TARGET", &target)
            .env("FINCH_EDITOR_BOUNDARY_REPORT", &report)
            .env(
                "FINCH_EDITOR_ACTION",
                match case {
                    "blank-context" | "editor-fallback" | "tab" | "control" => "accept",
                    other => other,
                },
            )
            .env("FINCH_EDITOR_CAPTURE", &capture)
            .env("FINCH_EDITOR_SELECTED", &selected)
            .env("FINCH_EDITOR_ARGC", &argc)
            .env("FINCH_EDITOR_ARGV", &argv)
            .env("FINCH_EDITOR_EXIT", &editor_exit)
            .env("EDITOR", &editor)
            .stdin(Stdio::from(pty.slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if case == "editor-fallback" {
            command.env_remove("VISUAL");
        } else {
            command.env("VISUAL", &visual);
        }
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("{case}: failed to run PTY child: {error}"));
        drop(pty.master);
        assert!(
            output.status.success(),
            "{case}: tool-boundary child failed with status {:?}.\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let result: serde_json::Value = serde_json::from_slice(
            &fs::read(&report)
                .unwrap_or_else(|error| panic!("{case}: missing child report: {error}")),
        )
        .unwrap_or_else(|error| panic!("{case}: invalid child report: {error}"));
        let detail = result["detail"].as_str().unwrap_or_else(|| {
            panic!("{case}: child result lacks string detail; report: {result}")
        });
        let final_bytes = fs::read(&target)
            .unwrap_or_else(|error| panic!("{case}: failed to read final target: {error}"));

        match case {
            "blank-context" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: readable blank-context edit must succeed; report: {result}"
                );
                assert_eq!(
                    final_bytes,
                    original.replacen("before", new_string, 1).as_bytes(),
                    "{case}: final payload must equal the reviewed proposal; report: {result}"
                );
            }
            "tab" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: a tab-changing edit must fail closed before a lossy artifact opens; report: {result}"
                );
                assert!(
                    detail.contains("TAB") && detail.contains("byte"),
                    "{case}: refusal must name the unrepresentable byte and offset; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: refusal wrote the file"
                );
            }
            "control" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: ANSI/control-changing edit must fail closed; report: {result}"
                );
                assert!(
                    detail.contains("ESCAPE") || detail.contains("U+001B"),
                    "{case}: refusal must identify the hidden ANSI/control byte; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: refusal wrote the file"
                );
            }
            "cancel" => {
                assert!(
                    detail.contains("aborted by user"),
                    "{case}: directive change must be reported as rejection; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: rejection wrote the file"
                );
            }
            "body-change" => {
                assert!(
                    detail.contains("was edited during review"),
                    "{case}: body mutation must be refused; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: body mutation wrote the file"
                );
            }
            "changed-whitespace" => {
                assert!(
                    detail.contains("was edited during review"),
                    "{case}: stripping meaningful changed-line whitespace must be refused; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: editor-hidden trailing whitespace was applied; detail: {detail}"
                );
            }
            "nonzero" => {
                let observed_exit = fs::read_to_string(&editor_exit).unwrap_or_else(|error| {
                    panic!("{case}: fake editor did not record its exit status: {error}")
                });
                assert_eq!(
                    observed_exit, "37",
                    "{case}: fixture did not exercise the requested editor exit status"
                );
                assert!(
                    detail.contains("aborted by user"),
                    "{case}: nonzero editor exit must fail closed; detail: {detail}"
                );
                assert_eq!(
                    final_bytes,
                    original.as_bytes(),
                    "{case}: failed editor wrote the file"
                );
            }
            "editor-fallback" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: EDITOR fallback edit must succeed; report: {result}"
                );
                assert_eq!(
                    final_bytes,
                    original.replacen("before", new_string, 1).as_bytes(),
                    "{case}: EDITOR fallback applied the wrong payload"
                );
            }
            _ => unreachable!(),
        }

        let editor_should_open = !matches!(case, "tab" | "control");
        assert_eq!(
            capture.exists(),
            editor_should_open,
            "{case}: artifact-open state disagrees with the fail-closed decision; detail: {detail}"
        );
        if editor_should_open {
            let observed_argc = fs::read_to_string(&argc)
                .unwrap_or_else(|error| panic!("{case}: missing editor argc record: {error}"));
            assert_eq!(
                observed_argc, "1",
                "{case}: fake editor must receive exactly one artifact argv"
            );
            let artifact_path = fs::read_to_string(&argv)
                .unwrap_or_else(|error| panic!("{case}: missing editor argv record: {error}"));
            assert!(
                artifact_path.ends_with(".diff"),
                "{case}: editor argv must name a .diff artifact, got {artifact_path:?}"
            );
            let artifact = fs::read_to_string(&capture)
                .unwrap_or_else(|error| panic!("{case}: missing captured artifact: {error}"));
            assert!(
                artifact.contains("# finch: action=execute")
                    && artifact.contains("--- ")
                    && artifact.contains("+++ ")
                    && artifact.contains("-before")
                    && artifact.contains("+after"),
                "{case}: actual editor artifact was not the readable proposal.\nArtifact:\n{artifact}"
            );
            let expected_editor = if case == "editor-fallback" {
                "EDITOR"
            } else {
                "VISUAL"
            };
            let observed_editor = fs::read_to_string(&selected)
                .unwrap_or_else(|error| panic!("{case}: missing selected-editor record: {error}"));
            assert_eq!(
                observed_editor, expected_editor,
                "{case}: VISUAL/EDITOR selection did not follow the production precedence"
            );
        }
    }
}
