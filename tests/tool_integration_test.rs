// Integration test for tool parsing and formatting
//
// Tests the full flow: format tools → parse tool calls → execute

use finch::models::{ToolCallParser, ToolPromptFormatter};
use finch::tools::{ToolDefinition, ToolInputSchema};

#[cfg(unix)]
use finch::tools::Tool;
#[cfg(unix)]
use finch::tools::ToolContext;
#[cfg(unix)]
use finch::tools::{BashTool, EditTool, WriteTool};
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
        skip_interactive_review: false,
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
         test ! -x \"$1\"\n\
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

#[cfg(unix)]
fn run_write_boundary_child() {
    let case = std::env::var("FINCH_WRITE_BOUNDARY_CASE")
        .expect("child must receive FINCH_WRITE_BOUNDARY_CASE");
    let target = std::env::var("FINCH_WRITE_BOUNDARY_TARGET")
        .expect("child must receive FINCH_WRITE_BOUNDARY_TARGET");
    let report = std::env::var("FINCH_WRITE_BOUNDARY_REPORT")
        .expect("child must receive FINCH_WRITE_BOUNDARY_REPORT");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child must create its tool runtime");
    let content = match case.as_str() {
        "ambiguous-header-line" => "keep\nend\n".to_string(),
        "tab" => "model\tcontent\n".to_string(),
        "control" => "model\u{1b}]0;hidden\u{7}content\n".to_string(),
        "crlf" => "model\r\ncontent\r\n".to_string(),
        "binary" => "model\0content\n".to_string(),
        "long-line" => format!("{}\n", "x".repeat(2048)),
        "oversized" => "x\n".repeat(600_000),
        _ => "model content\n# finch: action=cancel\n".to_string(),
    };
    let result = runtime.block_on(WriteTool.execute(
        serde_json::json!({
            "file_path": target,
            "content": content,
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
fn write_write_fake_editor(path: &std::path::Path) {
    let script = "#!/bin/sh\n\
set -eu\n\
test \"$#\" = 1\n\
cp \"$1\" \"$FINCH_WRITE_CAPTURE\"\n\
test ! -x \"$1\"\n\
case \"$FINCH_WRITE_ACTION\" in\n\
  accept) : ;;\n\
  cancel|ambiguous-header-line) sed 's/# finch: action=execute/# finch: action=cancel/' \"$1\" > \"$1.next\"; cp \"$1.next\" \"$1\" ;;\n\
  chat) printf '# finch: action=chat\\n# ---- Finch proposal body ----\\nprintf owned > %s\\n' \"$FINCH_WRITE_CANARY\" > \"$1\" ;;\n\
  body-change) printf '\\n+not-reviewed\\n' >> \"$1\" ;;\n\
  concurrent-change) printf 'somebody else changed it\\n' > \"$FINCH_WRITE_TARGET\" ;;\n\
  concurrent-create) printf 'somebody else created it\\n' > \"$FINCH_WRITE_TARGET\" ;;\n\
  intermediate-replacement) ln -s \"$FINCH_WRITE_ATTACKER\" \"$FINCH_WRITE_INTERMEDIATE\" ;;\n\
  ancestor-swap) mv \"$FINCH_WRITE_SWAP_PARENT\" \"$FINCH_WRITE_DISPLACED\"; ln -s \"$FINCH_WRITE_ATTACKER\" \"$FINCH_WRITE_SWAP_PARENT\" ;;\n\
  *) exit 92 ;;\n\
esac\n\
exit 0\n";
    fs::write(path, script)
        .unwrap_or_else(|error| panic!("failed to write fake editor {}: {error}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|error| {
        panic!(
            "failed to make fake editor {} executable: {error}",
            path.display()
        )
    });
}

#[cfg(unix)]
fn run_bash_decision_child() {
    let report = std::env::var("FINCH_BASH_REPORT").expect("child must receive report path");
    let original_canary =
        std::env::var("FINCH_BASH_ORIGINAL_CANARY").expect("child must receive original canary");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child must create its tool runtime");
    let result = runtime.block_on(BashTool.execute(
        serde_json::json!({
            "command": format!("printf original > {:?}", original_canary),
            "description": "exercise structural Bash approval",
        }),
        &editor_boundary_context(),
    ));
    let payload = match result {
        Ok(value) => serde_json::json!({"ok": true, "detail": value}),
        Err(error) => serde_json::json!({"ok": false, "detail": format!("{error:#}")}),
    };
    fs::write(&report, serde_json::to_vec(&payload).unwrap())
        .unwrap_or_else(|error| panic!("child failed to write Bash report {report:?}: {error}"));
}

#[cfg(unix)]
fn write_bash_fake_editor(path: &std::path::Path) {
    let script = "#!/bin/sh\n\
set -eu\n\
cp \"$1\" \"$FINCH_BASH_CAPTURE\"\n\
case \"$FINCH_BASH_ACTION\" in\n\
  execute-edited) printf '# finch: action=execute\\n# ---- Finch proposal body ----\\nprintf edited > %s\\n' \"$FINCH_BASH_EDITED_CANARY\" > \"$1\" ;;\n\
  chat) printf '# finch: action=chat\\n# ---- Finch proposal body ----\\nprintf chat-ran > %s\\n' \"$FINCH_BASH_DECISION_CANARY\" > \"$1\" ;;\n\
  cancel) printf '# finch: action=cancel\\n# ---- Finch proposal body ----\\nprintf cancel-ran > %s\\n' \"$FINCH_BASH_DECISION_CANARY\" > \"$1\" ;;\n\
  body-directive) printf '# finch: action=execute\\n# ---- Finch proposal body ----\\n# finch: action=cancel\\nprintf body-ran > %s\\n' \"$FINCH_BASH_EDITED_CANARY\" > \"$1\" ;;\n\
  *) exit 92 ;;\n\
esac\n";
    fs::write(path, script)
        .unwrap_or_else(|error| panic!("failed to write Bash editor {}: {error}", path.display()));
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|error| {
        panic!(
            "failed to make Bash editor {} executable: {error}",
            path.display()
        )
    });
}

/// Bash retains editable-command approval, but only an explicit structural
/// execute decision may reach `bash -c`. Chat and cancel bodies remain data.
#[cfg(unix)]
#[test]
fn test_bash_tool_executes_only_structurally_approved_body() {
    if std::env::var_os("FINCH_BASH_DECISION_CHILD").is_some() {
        run_bash_decision_child();
        return;
    }

    for case in ["execute-edited", "chat", "cancel", "body-directive"] {
        let dir = tempfile::tempdir().expect("Bash decision temp directory");
        let report = dir.path().join("report.json");
        let capture = dir.path().join("artifact.sh");
        let original_canary = dir.path().join("original-ran.txt");
        let edited_canary = dir.path().join("edited-ran.txt");
        let decision_canary = dir.path().join("decision-body-ran.txt");
        let editor = dir.path().join("editor");
        write_bash_fake_editor(&editor);

        let pty = openpty(None, None).unwrap_or_else(|error| panic!("{case}: openpty: {error}"));
        let output = Command::new(std::env::current_exe().expect("integration-test path"))
            .args([
                "--exact",
                "test_bash_tool_executes_only_structurally_approved_body",
                "--nocapture",
            ])
            .env("FINCH_BASH_DECISION_CHILD", "1")
            .env("FINCH_BASH_ACTION", case)
            .env("FINCH_BASH_REPORT", &report)
            .env("FINCH_BASH_CAPTURE", &capture)
            .env("FINCH_BASH_ORIGINAL_CANARY", &original_canary)
            .env("FINCH_BASH_EDITED_CANARY", &edited_canary)
            .env("FINCH_BASH_DECISION_CANARY", &decision_canary)
            .env("VISUAL", &editor)
            .stdin(Stdio::from(pty.slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|error| panic!("{case}: failed to run Bash PTY child: {error}"));
        drop(pty.master);
        assert!(
            output.status.success(),
            "{case}: Bash child failed. stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&fs::read(&report).unwrap())
            .unwrap_or_else(|error| panic!("{case}: invalid report: {error}"));
        let detail = result["detail"]
            .as_str()
            .unwrap_or_else(|| panic!("{case}: report lacks detail: {result}"));
        let artifact = fs::read_to_string(&capture).expect("captured Bash proposal");
        assert!(
            artifact.contains("# ---- Finch proposal body ----")
                && artifact.contains("printf original"),
            "{case}: Bash proposal must expose a structural editable body: {artifact}"
        );
        assert!(
            !original_canary.exists(),
            "{case}: the superseded original command must not execute"
        );
        assert!(
            !decision_canary.exists(),
            "{case}: chat/cancel body text must never execute"
        );
        match case {
            "execute-edited" | "body-directive" => assert!(
                edited_canary.exists(),
                "{case}: the structurally approved edited body did not execute; report: {result}"
            ),
            "chat" => {
                assert!(!edited_canary.exists(), "{case}: no command was approved");
                assert!(detail.contains("printf chat-ran"), "{case}: {detail}");
            }
            "cancel" => {
                assert!(!edited_canary.exists(), "{case}: no command was approved");
                assert!(detail.contains("aborted by user"), "{case}: {detail}");
            }
            _ => unreachable!(),
        }
    }
}

/// Production-boundary regression for write approval.
///
/// The real terminal check, editor process, temporary artifact, decision
/// parser, and filesystem commit all run in the PTY child. The editor fixture
/// also carries a shell canary: chat prose must be returned as data, never run.
#[cfg(unix)]
#[test]
fn test_write_tool_reviews_plaintext_and_never_executes_editor_text() {
    if std::env::var_os("FINCH_WRITE_BOUNDARY_CHILD").is_some() {
        run_write_boundary_child();
        return;
    }

    for case in [
        "accept",
        "cancel",
        "chat",
        "body-change",
        "concurrent-change",
        "concurrent-create",
        "ancestor-swap",
        "intermediate-replacement",
        "ambiguous-header-line",
        "tab",
        "control",
        "crlf",
        "binary",
        "long-line",
        "oversized",
    ] {
        let dir = tempfile::tempdir().expect("write-boundary temp directory");
        let swap_parent = dir.path().join("reviewed-parent");
        let displaced = dir.path().join("displaced-parent");
        let attacker = dir.path().join("attacker-parent");
        let target = if case == "ancestor-swap" {
            fs::create_dir(&swap_parent).expect("seed reviewed parent");
            fs::create_dir(&attacker).expect("seed attacker parent");
            swap_parent.join("target.txt")
        } else if case == "intermediate-replacement" {
            fs::create_dir(&attacker).expect("seed attacker parent");
            dir.path().join("new-parent/child/target.txt")
        } else {
            dir.path().join("target.txt")
        };
        let report = dir.path().join("report.json");
        let capture = dir.path().join("artifact.diff");
        let canary = dir.path().join("chat-executed.txt");
        let editor = dir.path().join("editor");
        let isolated_home = dir.path().join("home");
        let starts_missing = matches!(
            case,
            "accept" | "concurrent-create" | "ancestor-swap" | "intermediate-replacement"
        );
        if !starts_missing {
            let original = if case == "ambiguous-header-line" {
                "keep\n-- removed source comment\nend\n"
            } else {
                "original content\n"
            };
            fs::write(&target, original)
                .unwrap_or_else(|error| panic!("{case}: failed to seed target: {error}"));
        }
        write_write_fake_editor(&editor);

        let pty = openpty(None, None).unwrap_or_else(|error| panic!("{case}: openpty: {error}"));
        let output = Command::new(std::env::current_exe().expect("integration-test path"))
            .args([
                "--exact",
                "test_write_tool_reviews_plaintext_and_never_executes_editor_text",
                "--nocapture",
            ])
            .env("FINCH_WRITE_BOUNDARY_CHILD", "1")
            .env("FINCH_WRITE_BOUNDARY_CASE", case)
            .env("FINCH_WRITE_BOUNDARY_TARGET", &target)
            .env("FINCH_WRITE_TARGET", &target)
            .env("FINCH_WRITE_BOUNDARY_REPORT", &report)
            .env("FINCH_WRITE_ACTION", case)
            .env("FINCH_WRITE_CAPTURE", &capture)
            .env("FINCH_WRITE_CANARY", &canary)
            .env("FINCH_WRITE_SWAP_PARENT", &swap_parent)
            .env("FINCH_WRITE_DISPLACED", &displaced)
            .env("FINCH_WRITE_ATTACKER", &attacker)
            .env("FINCH_WRITE_INTERMEDIATE", dir.path().join("new-parent"))
            .env("HOME", &isolated_home)
            .env("VISUAL", &editor)
            .stdin(Stdio::from(pty.slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|error| panic!("{case}: failed to run PTY child: {error}"));
        drop(pty.master);
        assert!(
            output.status.success(),
            "{case}: write-boundary child failed with status {:?}.\nstdout:\n{}\nstderr:\n{}",
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
        let fidelity_refusal = matches!(
            case,
            "tab" | "control" | "crlf" | "binary" | "long-line" | "oversized"
        );
        let artifact = if fidelity_refusal {
            assert!(
                !capture.exists(),
                "{case}: an unfaithful diff must be refused before opening the editor"
            );
            None
        } else {
            let artifact = fs::read_to_string(&capture).unwrap_or_else(|error| {
                panic!(
                    "{case}: missing captured artifact: {error}; report: {result}; child stderr: {}",
                    String::from_utf8_lossy(&output.stderr)
                )
            });
            if case != "ambiguous-header-line" {
                assert!(
                    artifact.contains("--- ")
                        && artifact.contains("+++ ")
                        && artifact.contains("+model content")
                        && artifact.contains("+# finch: action=cancel"),
                    "{case}: reviewer must receive the model's exact content as a readable diff.\nArtifact:\n{artifact}"
                );
            }
            assert!(
                !artifact.contains("base64")
                    && !artifact.contains("python3")
                    && !artifact.contains("PYEOF"),
                "{case}: review artifact must be plaintext data, not an encoded executable.\nArtifact:\n{artifact}"
            );
            Some(artifact)
        };
        assert!(
            !canary.exists(),
            "{case}: editor-returned chat prose was executed as shell code"
        );

        match case {
            "accept" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: approved write failed: {result}"
                );
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "model content\n# finch: action=cancel\n",
                    "{case}: a body directive must remain ordinary file content"
                );
            }
            "cancel" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: cancel should return normally: {result}"
                );
                assert!(detail.contains("aborted by user"), "{case}: {detail}");
                assert_eq!(fs::read_to_string(&target).unwrap(), "original content\n");
            }
            "chat" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: chat should return normally: {result}"
                );
                assert!(
                    detail.contains("printf owned"),
                    "{case}: chat prose was not returned: {detail}"
                );
                assert!(
                    !detail.lines().any(|line| line.starts_with("--- ")),
                    "{case}: a chat refusal must not be rendered as an applied diff: {detail}"
                );
                assert_eq!(fs::read_to_string(&target).unwrap(), "original content\n");
            }
            "body-change" => {
                assert_eq!(
                    result["ok"], true,
                    "{case}: body refusal should return normally: {result}"
                );
                assert!(detail.contains("edited during review"), "{case}: {detail}");
                assert_eq!(fs::read_to_string(&target).unwrap(), "original content\n");
            }
            "concurrent-change" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: stale review must fail: {result}\nArtifact:\n{artifact:?}"
                );
                assert!(detail.contains("changed while"), "{case}: {detail}");
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "somebody else changed it\n"
                );
            }
            "concurrent-create" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: a raced creation must fail: {result}"
                );
                assert!(detail.contains("created while"), "{case}: {detail}");
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "somebody else created it\n"
                );
            }
            "ancestor-swap" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: swapped ancestry must fail: {result}"
                );
                assert!(detail.contains("ancestor"), "{case}: {detail}");
                assert!(
                    !attacker.join("target.txt").exists() && !displaced.join("target.txt").exists(),
                    "{case}: a swapped destination must not receive reviewed bytes"
                );
            }
            "intermediate-replacement" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: replaced intermediate directory must fail: {result}"
                );
                assert!(
                    detail.contains("created while")
                        || detail.contains("replaced")
                        || detail.contains("ancestry changed"),
                    "{case}: exclusive publication must refuse a replaced intermediate: {detail}"
                );
                assert!(
                    !attacker.join("child/target.txt").exists(),
                    "{case}: replaced ancestry must not receive reviewed bytes"
                );
            }
            "ambiguous-header-line" => {
                let artifact = artifact.unwrap_or_else(|| {
                    panic!("{case}: editor must open for a faithful header-looking removal")
                });
                assert!(
                    artifact.contains("--- ") && artifact.contains("+++ "),
                    "{case}: file-header --- / +++ must still be present.\nArtifact:\n{artifact}"
                );
                assert!(
                    artifact.contains("-- removed source comment"),
                    "{case}: the removed '-- ' line must appear in the review; FileDiff used to \
                     drop it as a second file header.\nArtifact:\n{artifact}"
                );
                assert_eq!(
                    result["ok"], true,
                    "{case}: cancelling a faithful header-looking review should return normally: {result}"
                );
                assert!(detail.contains("aborted by user"), "{case}: {detail}");
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "keep\n-- removed source comment\nend\n",
                    "{case}: cancelling must preserve the source, including the '-- ' line"
                );
            }
            "tab" | "control" | "crlf" | "binary" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: lossy review must fail: {result}"
                );
                assert!(
                    detail.contains("byte-faithful"),
                    "{case}: refusal must explain the review fidelity boundary: {detail}"
                );
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "original content\n",
                    "{case}: refusing an unfaithful preview must preserve the target"
                );
            }
            "long-line" | "oversized" => {
                assert_eq!(
                    result["ok"], false,
                    "{case}: truncated review must fail: {result}"
                );
                assert!(
                    detail.contains("review would"),
                    "{case}: renderer self-consistency must refuse a truncated or elided preview: {detail}"
                );
                assert_eq!(
                    fs::read_to_string(&target).unwrap(),
                    "original content\n",
                    "{case}: refusing a truncated preview must preserve the target"
                );
            }
            _ => unreachable!(),
        }
    }
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
        let isolated_home = dir.path().join("home");
        let hook = isolated_home.join(".finch/hooks/post-save");
        let hook_canary = dir.path().join("hook-ran.txt");
        let visual = dir.path().join("visual-editor");
        let editor = dir.path().join("fallback-editor");
        let (original, _old_string, new_string) = editor_boundary_case(case);
        fs::write(&target, original)
            .unwrap_or_else(|error| panic!("{case}: failed to seed target: {error}"));
        write_fake_editor(&visual, "VISUAL");
        write_fake_editor(&editor, "EDITOR");
        fs::create_dir_all(hook.parent().expect("hook parent"))
            .unwrap_or_else(|error| panic!("{case}: failed to create isolated hook dir: {error}"));
        fs::write(
            &hook,
            format!("#!/bin/sh\nprintf ran > {:?}\n", hook_canary),
        )
        .unwrap_or_else(|error| panic!("{case}: failed to seed isolated hook: {error}"));
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("{case}: failed to chmod isolated hook: {error}"));

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
            .env("HOME", &isolated_home)
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
        assert!(
            !hook_canary.exists(),
            "{case}: interactive diff approval must not execute the undisclosed HOME post-save hook"
        );

        let applies = matches!(case, "blank-context" | "editor-fallback");
        let (expected_ok, diagnostic) = match case {
            "tab" => (false, Some("TAB")),
            "control" => (false, Some("ESCAPE")),
            "cancel" | "nonzero" => (true, Some("aborted by user")),
            "body-change" | "changed-whitespace" => (true, Some("was edited during review")),
            "blank-context" | "editor-fallback" => (true, None),
            _ => unreachable!(),
        };
        assert_eq!(
            result["ok"], expected_ok,
            "{case}: tool outcome disagrees with the boundary contract; report: {result}"
        );
        if let Some(expected) = diagnostic {
            assert!(
                detail.contains(expected),
                "{case}: refusal must explain itself with {expected:?}; detail: {detail}"
            );
        }
        let expected_bytes = if applies {
            original.replacen("before", new_string, 1)
        } else {
            original.to_string()
        };
        assert_eq!(
            final_bytes,
            expected_bytes.as_bytes(),
            "{case}: final file bytes disagree with the reviewed decision; report: {result}"
        );
        if case == "nonzero" {
            assert_eq!(
                fs::read_to_string(&editor_exit).unwrap_or_default(),
                "37",
                "{case}: fixture did not exercise the requested editor exit status"
            );
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
