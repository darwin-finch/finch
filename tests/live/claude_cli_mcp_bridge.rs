// Live end-to-end verification for issue #1309: does the real `claude` CLI,
// talking to the real, compiled Finch binary as its own MCP server, actually
// execute a real Finch tool and return the real result -- not a fixture, not
// a fake `claude` script standing in for the CLI.
//
// Gated exactly like the rest of `tests/live/` (`FINCH_LIVE_TESTS=1`,
// `#[ignore]`): this spawns the real `claude` CLI (a real login is required)
// and a real `finch` subprocess, so it never runs in ordinary CI.
//
// This intentionally drives the exact wire contract
// `finch_providers::claude_cli` documents (`--mcp-config` inline JSON,
// `--strict-mcp-config`, `--allowedTools "mcp__finch__<tool>"`) directly with
// `std::process::Command`, using `env!("CARGO_BIN_EXE_finch")` as the MCP
// server command, rather than going through `ClaudeCliProvider` in-process.
// `ClaudeCliProvider` itself resolves the MCP server command from
// `std::env::current_exe()`, which inside a `cargo test` process resolves to
// the *test harness* binary, not `target/debug/finch` -- so exercising the
// real compiled binary as the MCP server here is what makes this a genuine
// production-boundary check, not `ClaudeCliProvider`'s unit tests over again.

use std::io::Write;
use std::process::Stdio;

use super::live_tests_enabled;

fn claude_cli_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[test]
#[ignore]
fn real_claude_cli_executes_a_real_finch_tool_through_the_mcp_bridge() {
    if !live_tests_enabled() {
        eprintln!("skipping: set FINCH_LIVE_TESTS=1 to run live provider tests");
        return;
    }
    if !claude_cli_available() {
        eprintln!(
            "skipping: `claude` CLI not found or not runnable (see finch_providers::claude_cli)"
        );
        return;
    }

    let marker = "FINCH_LIVE_MCP_BRIDGE_MARKER_2f9c8a";
    let probe_dir = tempfile::tempdir().expect("create a scratch dir for the probe file");
    let probe_path = probe_dir.path().join("probe.txt");
    std::fs::write(&probe_path, format!("{marker}\n")).expect("write the probe file");

    let finch_binary = env!("CARGO_BIN_EXE_finch");
    let mcp_config = serde_json::json!({
        "mcpServers": {
            finch_providers::CLAUDE_CLI_MCP_SERVER_NAME: {
                "type": "stdio",
                "command": finch_binary,
                "args": [finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG],
            }
        }
    })
    .to_string();
    let allowed_tools = finch_providers::claude_cli_mcp_wire_name("read");

    let input_line = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!(
                    "Use the {allowed_tools} tool directly (no confirmation) to read {} \
                     and report its exact contents.",
                    probe_path.display()
                ),
            }],
        },
    })
    .to_string();

    let mut child = std::process::Command::new("claude")
        .args([
            "--print",
            "--verbose",
            "--include-partial-messages",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--tools",
            "",
            "--system-prompt",
            "You are a helpful coding assistant.",
            "--mcp-config",
            &mcp_config,
            "--strict-mcp-config",
            "--allowedTools",
            &allowed_tools,
        ])
        // Run the real bridge subprocess from a scratch cwd (unrelated to the
        // probe file), the same isolation `ClaudeCliProvider` gets in
        // practice, so a passing run cannot be explained by cwd proximity.
        .current_dir(probe_dir.path().parent().unwrap_or(probe_dir.path()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the real claude CLI");

    child
        .stdin
        .take()
        .expect("claude stdin")
        .write_all(format!("{input_line}\n").as_bytes())
        .expect("write the turn to claude's stdin");

    let output = child
        .wait_with_output()
        .expect("wait for the real claude CLI to finish");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stdout.contains(marker),
        "the real claude CLI must relay the marker content read for real by Finch's own MCP \
         bridge process (finch binary: {finch_binary}); stdout={stdout}\nstderr={stderr}"
    );
    assert!(
        stdout.contains("\"tool_use\""),
        "the real claude CLI must have emitted a genuine, structured tool_use content block for \
         the MCP tool call, not narrated it in plain text; stdout={stdout}"
    );
}
