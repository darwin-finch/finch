// Production-boundary proof for issue #1341, at the actual process boundary:
// the real, compiled `finch` binary is spawned as its own OS process, exactly
// the way `finch_providers::ClaudeCliProvider` spawns it (re-invoked with the
// hidden `CLAUDE_CLI_MCP_BRIDGE_FLAG`), and driven with raw MCP JSON-RPC over
// its stdio, the way the real `claude` CLI would. This test never needs a
// real `claude` CLI or a real login — see `tests/live/claude_cli_mcp_bridge.rs`
// for that (gated by `FINCH_LIVE_TESTS=1`, requires a real login).
//
// The companion same-crate test
// (`src/cli/repl_event/tool_execution.rs::tests::a_tool_call_through_the_real_bridge_handler_executes_through_the_real_interactive_path`)
// drives the bridge's own request handler in-process against the real,
// interactive `ToolExecutionCoordinator` (which fires a real
// `ReplEvent::ToolApprovalNeeded` and is not reachable from an external
// integration test — it is a crate-private orchestration layer). This test
// instead proves the actual *process* boundary: a real second OS process,
// speaking real MCP JSON-RPC, really reaching this crate's real, public
// tool-execution authority (`PermissionManager`/`ToolExecutor`) over the real
// Unix domain socket — never a bespoke, disconnected authority inside the
// bridge process itself.
//
// Together the two tests cover exactly what issue #1341 asks for: a tool
// call from a fake CLI actually reaching and executing through the real
// interactive tool-call path with a real approval decision, not just a
// same-process unit test.

use finch::tools::{
    PermissionCheck, PermissionManager, ToolExecutor, ToolRegistry, ToolUse, WriteTool,
};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn spawn_real_bridge(socket_path: &std::path::Path) -> tokio::process::Child {
    tokio::process::Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg(finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG)
        .env(finch_providers::CLAUDE_CLI_TOOL_SOCKET_ENV, socket_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the real compiled bridge binary")
}

#[tokio::test]
async fn a_real_second_process_forwards_a_tools_call_to_the_real_permission_and_execution_authority(
) {
    let workdir = tempfile::tempdir().expect("workspace for the real write");
    let target_file = workdir.path().join("target.txt");
    std::fs::write(&target_file, "original\n").expect("seed the target file");

    let socket_dir = tempfile::tempdir().expect("socket directory");
    let socket_path = socket_dir.path().join("bridge.sock");
    let listener = tokio::net::UnixListener::bind(&socket_path)
        .expect("bind the bridge socket exactly as ClaudeCliProvider does");

    // Plays the role of the frontend process. Uses only this crate's public
    // tool-execution authority (`finch::tools`), the same real
    // `PermissionManager`/`ToolExecutor`/`WriteTool` an interactive REPL
    // session uses — proving the real second process reaches real authority,
    // not a stub.
    let frontend_target_file = target_file.clone();
    let frontend_task = tokio::spawn(async move {
        let (stream, _addr) = listener
            .accept()
            .await
            .expect("accept the real bridge subprocess's connection");
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .await
            .expect("read the real bridge subprocess's forwarded request");
        let request: finch_providers::ClaudeCliBridgeToolRequest =
            serde_json::from_str(line.trim())
                .expect("the real bridge's request must be well-formed");
        assert_eq!(request.name, "write");

        let mut registry = ToolRegistry::new();
        registry.register(Box::new(WriteTool));
        let pattern_dir = tempfile::tempdir().expect("isolated tool-pattern store");
        let permissions = PermissionManager::new();

        // The real, default (non-peer) permission policy must genuinely
        // require a human decision for a write — proving this socket reaches
        // the same authority an interactive session has, never a bypass and
        // never the old bridge-local `PermissionManager::for_peer()`, which
        // this issue removed.
        let decision = permissions.check_tool_use(&request.name, &request.input);
        assert!(
            matches!(decision, PermissionCheck::AskUser(_)),
            "a write reaching the real default permission policy must require interactive \
             approval, not auto-run or fail closed on its own: {decision:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&frontend_target_file).unwrap(),
            "original\n",
            "nothing may execute before the (simulated) human answers the real approval prompt"
        );

        // Simulate the human approving, then run the exact real `ToolExecutor`
        // path production code runs after approval.
        let executor = ToolExecutor::new(
            registry,
            permissions,
            pattern_dir.path().join("patterns.json"),
        )
        .expect("construct the real executor");
        let tool_use = ToolUse::new(request.name.clone(), request.input.clone());
        let result = executor
            .execute_tool::<fn() -> anyhow::Result<()>>(&tool_use, None, None, None, None, None)
            .await
            .expect("real execution through the real ToolExecutor must not error");
        assert!(!result.is_error, "real execution must succeed: {result:?}");

        let mut stream = reader.into_inner();
        let mut payload = serde_json::to_string(&finch_providers::ClaudeCliBridgeToolResponse {
            is_error: false,
            content: result.content,
        })
        .expect("encode the real response");
        payload.push('\n');
        stream
            .write_all(payload.as_bytes())
            .await
            .expect("answer the real bridge subprocess's still-open connection");
        stream.flush().await.expect("flush the response");
    });

    let mut child = spawn_real_bridge(&socket_path);
    let mut stdin = child.stdin.take().expect("bridge stdin");
    let stdout = child.stdout.take().expect("bridge stdout");
    let mut bridge_out = BufReader::new(stdout).lines();

    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\
              \"params\":{\"protocolVersion\":\"2024-11-05\"}}\n",
        )
        .await
        .expect("send initialize to the real bridge subprocess");
    let init_line =
        tokio::time::timeout(std::time::Duration::from_secs(10), bridge_out.next_line())
            .await
            .expect("the real bridge subprocess must answer initialize")
            .expect("read the initialize response")
            .expect("the real bridge subprocess must not close stdout after initialize");
    assert!(
        init_line.contains("\"protocolVersion\""),
        "initialize must answer with a real protocol handshake: {init_line}"
    );
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .expect("send the initialized notification");

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "write",
            "arguments": {
                "file_path": target_file.to_string_lossy(),
                "content": "written-through-a-real-second-process",
            },
        },
    });
    stdin
        .write_all(format!("{call}\n").as_bytes())
        .await
        .expect("send the real tools/call to the real bridge subprocess");

    let response_line =
        tokio::time::timeout(std::time::Duration::from_secs(10), bridge_out.next_line())
            .await
            .expect("the real bridge subprocess must answer once real execution completes")
            .expect("read the tools/call response")
            .expect("the real bridge subprocess must not close stdout before answering");
    let response: serde_json::Value =
        serde_json::from_str(&response_line).expect("the bridge's reply must be valid JSON");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("a text content block");
    assert!(
        text.contains("written-through-a-real-second-process"),
        "the real bridge subprocess must relay WriteTool's own real diff/confirmation text \
         (naming the real content it wrote), not a fabricated one: {text}"
    );

    frontend_task
        .await
        .expect("the frontend-side task must not panic");
    drop(stdin);
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait())
        .await
        .expect("the real bridge subprocess must exit cleanly once stdin closes")
        .expect("wait for the real bridge subprocess");
    assert!(
        status.success(),
        "the real bridge subprocess must exit 0 on a clean stdin close: {status}"
    );

    assert_eq!(
        std::fs::read_to_string(&target_file).expect("read the target file back"),
        "written-through-a-real-second-process",
        "issue #1341: a tools/call from a real, separately-spawned bridge OS process must reach \
         this crate's real permission policy and real ToolExecutor and really write the file for \
         real, only after a real approval decision — never a bypass, and never a second, \
         disconnected execution authority inside the bridge process itself"
    );
}

#[tokio::test]
async fn a_real_second_process_never_falls_back_to_local_execution_without_the_socket() {
    // Hostile configuration (issue #1341): if the bridge's own env is somehow
    // missing the socket path, the real subprocess must fail the call closed
    // -- never silently execute locally the way the old #1309 shape did.
    let workdir = tempfile::tempdir().unwrap();
    let target_file = workdir.path().join("target.txt");
    std::fs::write(&target_file, "must never be touched\n").unwrap();

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_finch"))
        .arg(finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the real compiled bridge binary");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut bridge_out = BufReader::new(stdout).lines();

    stdin
        .write_all(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\
              \"params\":{\"protocolVersion\":\"2024-11-05\"}}\n",
        )
        .await
        .unwrap();
    bridge_out.next_line().await.unwrap().unwrap();

    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "write",
            "arguments": {"file_path": target_file.to_string_lossy(), "content": "hijacked"},
        },
    });
    stdin
        .write_all(format!("{call}\n").as_bytes())
        .await
        .unwrap();
    let response_line =
        tokio::time::timeout(std::time::Duration::from_secs(10), bridge_out.next_line())
            .await
            .expect("a missing socket must still fail fast, not hang")
            .unwrap()
            .unwrap();
    let response: serde_json::Value = serde_json::from_str(&response_line).unwrap();
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains(finch_providers::CLAUDE_CLI_TOOL_SOCKET_ENV),
        "the error must name the missing configuration: {text}"
    );

    drop(stdin);
    let _ = child.wait().await;
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "must never be touched\n",
        "a misconfigured bridge subprocess must never execute locally as a fallback"
    );
}
