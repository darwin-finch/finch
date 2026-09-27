// The Claude Code MCP bridge (issue #1309).
//
// `finch_providers::ClaudeCliProvider` spawns the official `claude` CLI with
// `--tools ""` (its own built-in tools always stay off — see that module's
// doc comment for why) and instead registers this same `finch` binary, via
// `--mcp-config`, as an MCP server exposing Finch's own tool implementations.
// Claude Code's own process invokes this binary again with exactly
// `finch_providers::CLAUDE_CLI_MCP_BRIDGE_FLAG` as its only argument (see
// `src/main.rs`); from there control never returns to the ordinary CLI.
//
// This module is the actual execution authority for that path. It speaks a
// minimal stdio JSON-RPC subset of MCP (initialize, notifications/initialized,
// tools/list, tools/call — verified directly against the real `claude` CLI
// 2.1.283 on 2026-09-27) and dispatches each `tools/call` through a fresh,
// non-interactive `PermissionManager`/`ToolRegistry` pair built the same way
// as the rest of Finch's tool surface, never a duplicate implementation.
//
// Permission policy: `PermissionManager::for_peer()` — read/glob/grep run for
// real, write/edit/bash-with-side-effects are not auto-applied. There is no
// interactive TUI in this process to ask a human, so this bridge itself
// checks `check_tool_use` before dispatch and only ever executes on an
// explicit `Allow`; `AskUser` and `Deny` both return a plain-text result
// explaining that the action needs interactive approval in the Finch session,
// never a bypass. `--allowedTools` on the `claude` side only decides whether
// Claude Code will call this server without an approval prompt of its own —
// this manager's own decision is what actually gates execution.

use crate::tools::{
    resolve_workspace_root, BashTool, EditTool, GlobTool, GrepTool, PermissionCheck,
    PermissionManager, ReadTool, ToolContext, ToolRegistry, WriteTool,
};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};

/// Run the stdio MCP bridge loop until stdin closes. Exit code 0 either way:
/// a malformed request gets a JSON-RPC error reply, not a crash, so Claude
/// Code always receives a clean disconnect rather than a broken pipe.
pub async fn run() -> Result<()> {
    let workspace_root = resolve_workspace_root(
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
    );
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(ReadTool));
    registry.register(Box::new(WriteTool));
    registry.register(Box::new(EditTool));
    registry.register(Box::new(GlobTool));
    registry.register(Box::new(GrepTool));
    registry.register(Box::new(BashTool));
    let permissions = PermissionManager::for_peer().with_workspace_root(workspace_root);

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let stdout = std::io::stdout();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(trimmed) {
            Ok(request) => handle_request(&registry, &permissions, &request).await,
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

/// Handle one JSON-RPC request. Returns `None` for a notification (no `id`,
/// no reply expected — e.g. `notifications/initialized`).
async fn handle_request(
    registry: &ToolRegistry,
    permissions: &PermissionManager,
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
            let text = call_tool(registry, permissions, name, arguments).await;
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

/// Look up, permission-gate, and (only on `Allow`) execute one tool call.
/// Never panics on bad input or an unknown tool — every path returns a plain
/// string the model can read, same as any other Finch tool failure.
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
async fn call_tool(
    registry: &ToolRegistry,
    permissions: &PermissionManager,
    name: &str,
    input: Value,
) -> String {
    let finch_name = if registry.get(name).is_some() {
        name
    } else {
        match finch_providers::claude_cli_tool_name_from_wire(name) {
            Some(stripped) => stripped,
            None => return format!("Finch MCP bridge: unrecognized tool name {name:?}"),
        }
    };
    let Some(tool) = registry.get(finch_name) else {
        return format!("Finch MCP bridge: tool {finch_name:?} is not registered");
    };
    match permissions.check_tool_use(finch_name, &input) {
        PermissionCheck::Deny(reason) => {
            format!("Finch denied this tool call: {reason}")
        }
        PermissionCheck::AskUser(reason) => format!(
            "Finch requires interactive approval for this call ({reason}), which is not \
             available through the automated Claude Code bridge. Ask the person running Finch \
             to run this themselves, or approve it from within the Finch session."
        ),
        PermissionCheck::Allow => {
            let context = ToolContext {
                skip_interactive_review: true,
                ..ToolContext::default()
            };
            match tool.execute(input, &context).await {
                Ok(output) => output,
                Err(error) => format!("Tool {finch_name} failed: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permissions() -> PermissionManager {
        let temp = std::env::temp_dir();
        PermissionManager::for_peer().with_workspace_root(temp)
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(ReadTool));
        registry.register(Box::new(WriteTool));
        registry.register(Box::new(EditTool));
        registry.register(Box::new(GlobTool));
        registry.register(Box::new(GrepTool));
        registry.register(Box::new(BashTool));
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

    #[tokio::test]
    async fn a_malformed_wire_name_fails_closed_with_a_named_error_not_a_panic() {
        let text = call_tool(
            &registry(),
            &permissions(),
            "not_an_mcp_wire_name",
            json!({}),
        )
        .await;
        assert!(
            text.contains("unrecognized tool name"),
            "a wire name without the mcp__finch__ prefix must fail closed with an actionable message: {text}"
        );
    }

    #[tokio::test]
    async fn an_unregistered_tool_name_fails_closed_with_a_named_error_not_a_panic() {
        let text = call_tool(&registry(), &permissions(), "mcp__finch__nope", json!({})).await;
        assert!(
            text.contains("is not registered"),
            "a well-formed wire name for a tool nothing registers must still fail closed: {text}"
        );
    }

    #[tokio::test]
    async fn read_only_tool_actually_executes_through_finchs_own_registry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.txt");
        std::fs::write(&file, "hello-from-the-real-tool\n").unwrap();
        let permissions =
            PermissionManager::for_peer().with_workspace_root(dir.path().to_path_buf());
        // "read", the plain name: this is the real shape confirmed against
        // the actual claude CLI (see call_tool's doc comment). The
        // mcp__finch__-prefixed fallback is covered separately below.
        let text = call_tool(
            &registry(),
            &permissions,
            "read",
            json!({"file_path": file.to_string_lossy()}),
        )
        .await;
        assert!(
            text.contains("hello-from-the-real-tool"),
            "a read-tier call must execute for real and return the file's real content: {text}"
        );
    }

    #[tokio::test]
    async fn a_prefixed_tool_name_is_still_accepted_as_a_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("probe.txt");
        std::fs::write(&file, "hello-from-the-prefixed-fallback\n").unwrap();
        let permissions =
            PermissionManager::for_peer().with_workspace_root(dir.path().to_path_buf());
        let text = call_tool(
            &registry(),
            &permissions,
            "mcp__finch__read",
            json!({"file_path": file.to_string_lossy()}),
        )
        .await;
        assert!(
            text.contains("hello-from-the-prefixed-fallback"),
            "a fully mcp__finch__-prefixed name must still resolve, for a future CLI version or \
             a different MCP client that does send it: {text}"
        );
    }

    #[tokio::test]
    async fn a_mutating_call_is_never_auto_applied_without_interactive_approval() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("target.txt");
        std::fs::write(&file, "original\n").unwrap();
        let permissions =
            PermissionManager::for_peer().with_workspace_root(dir.path().to_path_buf());
        let text = call_tool(
            &registry(),
            &permissions,
            "mcp__finch__write",
            json!({"file_path": file.to_string_lossy(), "content": "overwritten"}),
        )
        .await;
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "original\n",
            "issue #1309: a write call reaching this bridge must never actually modify the \
             file without interactive approval, no side channel around Finch's permission system"
        );
        assert!(
            text.contains("interactive approval"),
            "the reply must say why nothing happened: {text}"
        );
    }
}
