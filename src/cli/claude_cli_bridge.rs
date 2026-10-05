// The Claude Code MCP bridge (issue #1309, corrected by issue #1341).
//
// `finch_providers::ClaudeCliProvider` spawns the official `claude` CLI with
// `--tools ""` (its own built-in tools always stay off — see that module's
// doc comment for why) and instead registers this same `finch` binary, via
// `--mcp-config`, as an MCP server exposing Finch's own tool implementations.
// Claude Code's own process invokes this binary again with exactly
// `finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG` as its only argument (see
// `src/main.rs`); from there control never returns to the ordinary CLI.
//
// This module is a pure JSON-RPC-to-socket translator, never an execution
// authority. It speaks a minimal stdio JSON-RPC subset of MCP (initialize,
// notifications/initialized, tools/list, tools/call — verified directly
// against the real `claude` CLI 2.1.283 on 2026-09-27). A `tools/call` is
// resolved to a plain Finch tool name (using `ToolRegistry` purely for name
// validation and `tools/list` schema advertisement — never for execution),
// then forwarded, line-delimited JSON, over a Unix domain socket named by
// `finch_providers::CLAUDE_CLI_TOOL_SOCKET_ENV` (an env entry the frontend
// puts on this MCP server's own spec in `--mcp-config`, never on `claude`'s
// own environment) to `finch_providers::ClaudeCliProvider`, which is running
// in the frontend process that actually owns the Brain's turn. That process
// executes the call for real through the interactive `ToolLoop` — real
// approval, real file access, the same authority every other provider's tool
// calls already have — and this bridge relays the real result back to
// `claude` once it arrives, however long real interactive approval takes.
//
// This process previously built its own `PermissionManager::for_peer()` and
// executed calls directly (issue #1309's original shape). That was wrong: it
// duplicated execution/approval authority that already exists and works
// correctly for every other provider, and it could never actually prompt a
// human (there is no interactive TUI in this process). It has no separate
// permission policy left to fall back to; a socket failure fails the call
// closed with a named error, never a local execution.

use finch_providers::{
    claude_cli_tool_name_from_wire, ClaudeCliBridgeToolRequest, ClaudeCliBridgeToolResponse,
    CLAUDE_CLI_TOOL_SOCKET_ENV,
};

use crate::tools::ToolRegistry;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Run the stdio MCP bridge loop until stdin closes. Exit code 0 either way:
/// a malformed request gets a JSON-RPC error reply, not a crash, so Claude
/// Code always receives a clean disconnect rather than a broken pipe.
pub async fn run() -> Result<()> {
    let mut registry = ToolRegistry::new();
    register_tool_schemas(&mut registry);
    let socket_path = std::env::var(CLAUDE_CLI_TOOL_SOCKET_ENV).ok();

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let stdout = std::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => handle_request(&registry, socket_path.as_deref(), &request).await,
            Err(error) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {"code": -32700, "message": format!("parse error: {error}")},
            })),
        };
        if let Some(response) = response {
            let mut out = stdout.lock();
            writeln!(out, "{response}").context("writing MCP bridge response to stdout")?;
            out.flush().context("flushing MCP bridge response")?;
        }
    }
    Ok(())
}

/// The tool implementations this bridge advertises over `tools/list` — never
/// executed here. Real execution happens in the frontend, over the socket.
pub(crate) fn register_tool_schemas(registry: &mut ToolRegistry) {
    use crate::tools::{
        BackgroundBashTool, BackgroundPollTool, BackgroundStopTool, BashTool, EditTool, GlobTool,
        GrepTool, ReadTool, WriteTool,
    };
    registry.register(Box::new(ReadTool));
    registry.register(Box::new(WriteTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(GlobTool));
    registry.register(Box::new(GrepTool));
    registry.register(Box::new(BashTool));
    let dummy_tasks = std::sync::Arc::new(crate::brain::BackgroundTaskManager::new());
    registry.register(Box::new(BackgroundBashTool::new(std::sync::Arc::clone(
        &dummy_tasks,
    ))));
    registry.register(Box::new(BackgroundPollTool::new(std::sync::Arc::clone(
        &dummy_tasks,
    ))));
    registry.register(Box::new(BackgroundStopTool::new(dummy_tasks)));
}

/// Handle one JSON-RPC request. Returns `None` for a notification (no `id`,
/// no reply expected — e.g. `notifications/initialized`).
pub(crate) async fn handle_request(
    registry: &ToolRegistry,
    socket_path: Option<&str>,
    request: &Value,
) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": request
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "finch", "version": env!("CARGO_PKG_VERSION")},
            },
        })),
        "notifications/initialized" => None,
        "tools/list" => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"tools": tool_list(registry)},
        })),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or_default();
            let text = forward_tool_call(registry, socket_path, name, arguments).await;
            id.map(|id| {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"content": [{"type": "text", "text": text}]},
                })
            })
        }
        _ if id.is_some() => Some(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": format!("method not found: {method}")},
        })),
        _ => None,
    }
}

/// Verified directly against the real `claude` CLI (issue #1309): an MCP
/// server declares its tools under their own plain names in `tools/list`.
/// Claude Code applies the `mcp__<server-name-from---mcp-config>__` prefix
/// itself when it advertises the tool to the model and when it calls
/// `tools/call` — a server that pre-namespaces its own `tools/list` names (as
/// an earlier version of this function did) gets double-prefixed
/// (`mcp__finch__mcp__finch__read`), which the model then calls with, and
/// nothing here resolves it back to a real tool. `claude_cli_mcp_wire_name`
/// is what the *caller side* (`finch_providers::claude_cli`, building
/// `--allowedTools`) needs to predict Claude Code's own prefixing; this
/// server-side list must not apply it a second time.
fn tool_list(registry: &ToolRegistry) -> Vec<Value> {
    finch_providers::CLAUDE_CLI_TOOL_NAMES
        .iter()
        .filter_map(|name| registry.get(name))
        .map(|tool| {
            let schema = tool.input_schema();
            json!({
                "name": tool.name(),
                "description": tool.description(),
                "inputSchema": {
                    "type": schema.schema_type,
                    "properties": schema.properties,
                    "required": schema.required,
                },
            })
        })
        .collect()
}

/// Look up and forward one tool call to the frontend over the bridge socket
/// (issue #1341). Never panics on bad input, an unknown tool, or a socket
/// failure — every path returns a plain string the model can read, same as
/// any other Finch tool failure. Never executes anything itself: a missing or
/// unreachable socket fails the call closed, it never falls back to local
/// execution.
///
/// `name` is expected to already be the plain Finch tool name: verified
/// directly against the real `claude` CLI (issue #1309) that a JSON-RPC
/// `tools/call` names the tool exactly as this server's own `tools/list`
/// declared it (`"read"`), not the `mcp__finch__read` form the *model* sees
/// and calls with -- the CLI translates that back to the server's own name
/// before ever reaching here. The `mcp__finch__`-prefixed form is still
/// accepted as a fallback, in case a future CLI version or a different MCP
/// client sends it, so this never regresses to failing closed on a name it
/// could reasonably resolve.
async fn forward_tool_call(
    registry: &ToolRegistry,
    socket_path: Option<&str>,
    name: &str,
    input: Value,
) -> String {
    let finch_name = if registry.get(name).is_some() {
        name
    } else {
        match claude_cli_tool_name_from_wire(name) {
            Some(stripped) => stripped,
            None => return format!("Finch MCP bridge: unrecognized tool name {name:?}"),
        }
    };
    if registry.get(finch_name).is_none() {
        return format!("Finch MCP bridge: tool {finch_name:?} is not registered");
    }
    let Some(socket_path) = socket_path else {
        return format!(
            "Finch MCP bridge: no {CLAUDE_CLI_TOOL_SOCKET_ENV} was set on this process, so \
             there is no frontend to execute {finch_name:?} for real (issue #1341); this is a \
             configuration bug, not a permission decision."
        );
    };
    match relay_over_socket(socket_path, finch_name, input).await {
        Ok(response) => response.content,
        Err(error) => format!(
            "Finch MCP bridge: could not reach the frontend to execute {finch_name:?}: {error}"
        ),
    }
}

/// Connect fresh, send one request, read exactly one reply line. A fresh
/// connection per call keeps this side of the protocol trivially correlated
/// (the frontend answers whichever connection asked) without needing a
/// request id of its own — the underlying MCP JSON-RPC id never needs to
/// leave this process. No timeout on the read: real interactive approval on
/// the other end can legitimately take arbitrarily long.
async fn relay_over_socket(
    socket_path: &str,
    finch_name: &str,
    input: Value,
) -> Result<ClaudeCliBridgeToolResponse> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("connecting to the Finch bridge socket at {socket_path}"))?;
    let mut payload = serde_json::to_string(&ClaudeCliBridgeToolRequest {
        name: finch_name.to_string(),
        input,
    })
    .context("encoding the bridge tool-call request")?;
    payload.push('\n');
    stream
        .write_all(payload.as_bytes())
        .await
        .context("sending the tool-call request to the frontend")?;
    stream
        .flush()
        .await
        .context("flushing the tool-call request to the frontend")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .await
        .context("reading the tool-call response from the frontend")?;
    if read == 0 {
        anyhow::bail!("the frontend closed the connection before sending a response");
    }
    serde_json::from_str(line.trim()).context("parsing the frontend's tool-call response")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        register_tool_schemas(&mut registry);
        registry
    }

    #[test]
    fn tool_list_declares_plain_finch_names_not_pre_namespaced_wire_names() {
        // Regression: verified directly against the real claude CLI (issue
        // #1309) that Claude Code applies its own `mcp__<server>__` prefix on
        // top of whatever name `tools/list` declares. An earlier version of
        // `tool_list` pre-applied `claude_cli_mcp_wire_name` here too, which
        // real Claude Code then double-prefixed into
        // `mcp__finch__mcp__finch__read` -- a name nothing here, or Claude
        // Code's own dispatch, could ever resolve back to a real tool.
        let names: Vec<String> = tool_list(&registry())
            .iter()
            .map(|entry| entry["name"].as_str().unwrap().to_string())
            .collect();
        for finch_name in finch_providers::CLAUDE_CLI_TOOL_NAMES {
            assert!(
                names.contains(&finch_name.to_string()),
                "tool_list must advertise the plain name {finch_name:?}, not a pre-namespaced \
                 one, so Claude Code's own single mcp__finch__ prefix is what dispatch later \
                 sees; got {names:?}"
            );
            let double_prefixed = finch_providers::claude_cli_mcp_wire_name(finch_name);
            assert!(
                !names.contains(&double_prefixed),
                "tool_list must never itself declare a pre-namespaced name like \
                 {double_prefixed:?}: got {names:?}"
            );
        }
    }

    #[test]
    fn background_task_tools_are_advertised_by_tool_list() {
        let names: Vec<String> = tool_list(&registry())
            .iter()
            .map(|entry| entry["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"background_bash".to_string()));
        assert!(names.contains(&"background_poll".to_string()));
        assert!(names.contains(&"background_stop".to_string()));
    }

    #[tokio::test]
    async fn a_malformed_wire_name_fails_closed_with_a_named_error_not_a_panic() {
        let text = forward_tool_call(&registry(), None, "not_an_mcp_wire_name", json!({})).await;
        assert!(
            text.contains("unrecognized tool name"),
            "a wire name without the mcp__finch__ prefix must fail closed with an actionable \
             message: {text}"
        );
    }

    #[tokio::test]
    async fn an_unregistered_tool_name_fails_closed_with_a_named_error_not_a_panic() {
        let text = forward_tool_call(&registry(), None, "mcp__finch__nope", json!({})).await;
        assert!(
            text.contains("is not registered"),
            "a well-formed wire name for a tool nothing registers must still fail closed: {text}"
        );
    }

    #[tokio::test]
    async fn a_missing_socket_env_fails_closed_and_never_executes_locally() {
        // Issue #1341: there is no permission policy left in this process to
        // fall back to. A misconfigured (missing) socket must fail the call
        // closed with a message that names the real cause, never silently
        // execute anything here.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.txt");
        std::fs::write(&file, "must never be read by this process\n").unwrap();
        let text = forward_tool_call(
            &registry(),
            None,
            "read",
            json!({"file_path": file.to_string_lossy()}),
        )
        .await;
        assert!(
            text.contains(CLAUDE_CLI_TOOL_SOCKET_ENV),
            "the error must name the missing configuration, not a bare generic failure: {text}"
        );
        assert!(
            !text.contains("must never be read by this process"),
            "a missing socket must never fall back to executing the tool in this process: {text}"
        );
    }

    #[tokio::test]
    async fn a_real_tool_call_is_forwarded_over_the_socket_and_the_real_reply_is_returned() {
        // macOS caps `sockaddr_un.sun_path` at 104 bytes, and the supervised
        // test `TMPDIR` is longer than that, so a socket under the default
        // temp directory cannot be bound ("path must be shorter than
        // SUN_LEN"). Production binds under a short path for the same reason
        // (`ClaudeCliProvider`, `crates/finch-providers/src/claude_cli.rs`).
        let dir = tempfile::Builder::new()
            .prefix("fb-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket_path = dir.path().join("bridge.sock");
        let listener = tokio::net::UnixListener::bind(&socket_path).unwrap_or_else(|error| {
            panic!(
                "bind the bridge socket at {}: {error}",
                socket_path.display()
            )
        });

        let server = tokio::spawn(async move {
            let (stream, _addr) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: ClaudeCliBridgeToolRequest = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(request.name, "read");
            let mut stream = reader.into_inner();
            let mut payload = serde_json::to_string(&ClaudeCliBridgeToolResponse {
                is_error: false,
                content: "real-content-from-the-real-frontend".to_string(),
            })
            .unwrap();
            payload.push('\n');
            stream.write_all(payload.as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });

        let text = forward_tool_call(
            &registry(),
            Some(socket_path.to_str().unwrap()),
            "read",
            json!({"file_path": "/tmp/x"}),
        )
        .await;
        server.await.unwrap();
        assert_eq!(
            text, "real-content-from-the-real-frontend",
            "the bridge must relay exactly what the frontend answered, not fabricate a result"
        );
    }

    #[tokio::test]
    async fn a_dead_socket_fails_closed_with_a_named_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("nothing-listening.sock");
        let text = forward_tool_call(
            &registry(),
            Some(socket_path.to_str().unwrap()),
            "read",
            json!({}),
        )
        .await;
        assert!(
            text.contains("could not reach the frontend"),
            "an unreachable socket must fail closed with a named cause, not panic: {text}"
        );
    }
}
