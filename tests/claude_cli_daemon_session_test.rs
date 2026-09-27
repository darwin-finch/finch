//! Production-boundary tests for issue #1354: the daemon owns the Claude CLI
//! Subscription `claude` process and its MCP bridge socket, while tool
//! execution/approval stay wherever `BrainService.claudeCliRound` is called
//! from. These spawn a *real* `finch daemon` subprocess and connect a real
//! `finch::client::IpcClient` to it over its real Cap'n Proto Unix socket —
//! not an in-process mock standing in for either side — and drive the new
//! RPC directly against a fake `claude` binary (`FINCH_TEST_CLAUDE_CLI_BINARY`)
//! so no real Claude subscription login is required.
//!
//! Mirrors `tests/daemon_integration_test.rs`'s `TestDaemon` harness shape
//! (isolated supervisor authority, sealed HOME/IPC socket, coarse liveness
//! bounds rather than latency assertions) but trimmed to what these tests
//! need; the hang-diagnosis kernel-state capture that file carries is not
//! duplicated here.

use anyhow::{Context, Result};
use finch::providers::{ContentBlock, Message};
use finch::tools::ToolDefinition;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestDaemon {
    child: OwnedChild,
    _serial: tokio::sync::OwnedMutexGuard<()>,
    ipc_socket: PathBuf,
    spool: PathBuf,
}

impl TestDaemon {
    /// Spawn a real, isolated `finch daemon` subprocess with a fake `claude`
    /// binary substituted in for every daemon-owned Claude CLI Subscription
    /// session it creates.
    async fn start() -> Result<Self> {
        static SERIAL: std::sync::OnceLock<std::sync::Arc<tokio::sync::Mutex<()>>> =
            std::sync::OnceLock::new();
        let serial = SERIAL
            .get_or_init(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
            .clone()
            .lock_owned()
            .await;
        let proof = finch::brain::isolated_test_proof()
            .context("issue #1354 daemon session tests require supervisor authority")?;
        let daemon_address = proof.daemon_address().to_owned();
        let ipc_socket = std::env::var_os("FINCH_TEST_IPC_SOCKET")
            .map(PathBuf::from)
            .context("issue #1354 daemon session tests require their sealed IPC socket path")?;
        let brain_password = proof.brain_password()?;
        let home = proof.home;
        let finch_dir = home.join(".finch");
        std::fs::create_dir_all(finch_dir.join("brains"))?;

        // Unique per daemon instance: `proof.home` is one sealed HOME shared
        // by every test in this binary (the supervisor authority is set up
        // once for the whole process), so a fixed spool path here would let
        // a later test's `TestDaemon` read `socket_path`/`pid`/etc. files a
        // previous test's fake `claude` process left behind.
        let spool = home.join(format!(
            "claude-cli-spool-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&spool)?;
        let fake_claude = install_fake_claude(&home, &spool)?;

        write_config(&home, &daemon_address, &brain_password)?;

        let address_file = finch_dir.join(format!("bound-{}.addr", uuid::Uuid::new_v4().simple()));
        let stderr_path =
            finch_dir.join(format!("daemon-{}.stderr", uuid::Uuid::new_v4().simple()));
        let stderr_file = std::fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&stderr_path)?;

        let mut command = Command::new(env!("CARGO_BIN_EXE_finch"));
        command
            .arg("daemon")
            .arg("--bind")
            .arg(&daemon_address)
            .env("FINCH_TEST_BOUND_ADDR_FILE", &address_file)
            .env("FINCH_TEST_CLAUDE_CLI_BINARY", &fake_claude)
            .env("RUST_LOG", "finch=debug")
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr_file.try_clone()?));
        let child = command.spawn().context("spawn isolated Finch daemon")?;
        let mut child = OwnedChild(child);

        // Coarse liveness bound (matches tests/daemon_integration_test.rs):
        // the assertion is "it eventually bound", not a latency claim.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if address_file.exists() {
                break;
            }
            if let Some(status) = child.0.try_wait()? {
                let stderr = bounded_stderr(&stderr_file);
                anyhow::bail!("isolated daemon exited before binding: {status}; stderr={stderr:?}");
            }
            if Instant::now() >= deadline {
                let stderr = bounded_stderr(&stderr_file);
                anyhow::bail!(
                    "isolated daemon did not publish its address within 60s; stderr={stderr:?}"
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        // Wait for the IPC socket file to exist and accept a connection —
        // the address file proves the HTTP listener bound, not the Cap'n
        // Proto one.
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if ipc_socket.exists() && tokio::net::UnixStream::connect(&ipc_socket).await.is_ok() {
                break;
            }
            if let Some(status) = child.0.try_wait()? {
                let stderr = bounded_stderr(&stderr_file);
                anyhow::bail!(
                    "isolated daemon exited before its IPC socket accepted connections: \
                     {status}; stderr={stderr:?}"
                );
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "isolated daemon's IPC socket never accepted a connection within 30s; stderr={:?}",
                bounded_stderr(&stderr_file)
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        Ok(Self {
            child,
            _serial: serial,
            ipc_socket,
            spool,
        })
    }

    async fn connect(&self) -> Result<finch::client::IpcClient> {
        finch::client::IpcClient::connect_path(self.ipc_socket.clone())
            .await
            .context("connecting a fresh frontend to the isolated daemon's IPC socket")
    }

    /// Restart the daemon the way `finch daemon-stop`/an operator restart
    /// really does: SIGTERM, then wait for the OS to reap it — never a
    /// pattern-based kill (CLAUDE.md). Deliberately not `Child::kill()`
    /// (SIGKILL): no process, this one included, can run any cleanup code
    /// in response to SIGKILL — the OS terminates it before a single
    /// instruction of a signal handler, async task, or `Drop` impl can run.
    /// SIGTERM is what a *graceful* restart sends and what this test
    /// exercises; `DaemonLifecycle::stop_daemon` (`src/daemon/lifecycle.rs`)
    /// only escalates to SIGKILL after a timeout with no graceful exit,
    /// which is a real, disclosed limitation this test does not cover: a
    /// hard crash or `kill -9` genuinely does orphan a live `claude` child,
    /// by OS design, not by a gap in `ClaudeCliSessionRegistry`.
    fn sigterm_and_wait(&mut self) -> Result<()> {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.0.id() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .context("send SIGTERM to the isolated daemon")?;
        self.child.0.wait().context("reap the isolated daemon")?;
        Ok(())
    }
}

fn bounded_stderr(file: &std::fs::File) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = file.try_clone().unwrap();
    let _ = file.seek(SeekFrom::Start(0));
    let mut buf = String::new();
    let _ = std::io::Read::take(file, 16 * 1024).read_to_string(&mut buf);
    buf
}

/// A fake `claude` CLI that mirrors `crates/finch-providers/src/claude_cli.rs`
/// test module's own `install_fake_claude("mcp-tool-call")` fixture: on a
/// tool-serving round it connects to the MCP bridge socket embedded in
/// `--mcp-config`'s env entry, emits a `tool_use` block, and blocks
/// (stdout not at EOF) until `$SPOOL/proceed-<call-index>` appears, so a
/// test can act as the bridge and choose exactly when the round settles.
/// Writes its own PID to `$SPOOL/pid` on every invocation (last writer
/// wins), so a test can confirm a live child exists and later confirm it no
/// longer does.
fn install_fake_claude(home: &Path, spool: &Path) -> Result<PathBuf> {
    let bin = home.join("fake-claude-1354");
    let spool_str = spool.display();
    let script = format!(
        r#"#!/bin/bash
SPOOL="{spool_str}"
echo $$ > "$SPOOL/pid"
SID=""
FLAG=""
MCP_CONFIG=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--session-id" ] || [ "$prev" = "--resume" ]; then SID="$a"; FLAG="$prev"; fi
  if [ "$prev" = "--mcp-config" ]; then MCP_CONFIG="$a"; fi
  prev="$a"
done
if [ "$1" = "--version" ]; then echo "2.1.283 (Claude Code)"; exit 0; fi
if [ "$1" = "auth" ]; then echo '{{"loggedIn": true, "authMethod": "claude.ai"}}'; exit 0; fi
STDIN_CONTENT=$(cat)
CALL_INDEX_FILE="$SPOOL/call_index"
CALL_INDEX=$(cat "$CALL_INDEX_FILE" 2>/dev/null || echo 0)
echo $((CALL_INDEX + 1)) > "$CALL_INDEX_FILE"
echo "$CALL_INDEX" >> "$SPOOL/call_order.log"

if [ -z "$MCP_CONFIG" ]; then
  echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
  echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_'"$CALL_INDEX"'","role":"assistant","content":[{{"type":"text","text":"no-tools-round-'"$CALL_INDEX"'"}}]}}}}'
  echo '{{"type":"result","subtype":"success","is_error":false,"result":"no-tools-round-'"$CALL_INDEX"'","stop_reason":"end_turn"}}'
  exit 0
fi
echo '{{"type":"system","subtype":"init","session_id":"'"$SID"'","model":"claude-sonnet-5"}}'
SOCK=$(printf '%s' "$MCP_CONFIG" | grep -oE '"FINCH_CLAUDE_CLI_TOOL_SOCKET":"[^"]*"' | cut -d'"' -f4)
printf '%s' "$SOCK" > "$SPOOL/socket_path"
echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_tool","role":"assistant","content":[{{"type":"tool_use","id":"toolu_1354","name":"mcp__finch__probe_tool","input":{{"key":"value"}}}}]}}}}'
tries=0
while [ ! -f "$SPOOL/proceed" ] && [ "$tries" -lt 1000 ]; do
  sleep 0.02
  tries=$((tries + 1))
done
echo '{{"type":"assistant","message":{{"model":"claude-sonnet-5","id":"msg_final","role":"assistant","content":[{{"type":"text","text":"tool call handled"}}]}}}}'
echo '{{"type":"result","subtype":"success","is_error":false,"result":"tool call handled","stop_reason":"end_turn"}}'
exit 0
"#
    );
    std::fs::write(&bin, script)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    Ok(bin)
}

fn write_config(home: &Path, daemon_address: &str, brain_password: &str) -> Result<()> {
    // [backend] disabled for the same reason tests/daemon_integration_test.rs
    // disables it: real local GGUF model loading otherwise starves the
    // runtime thread the HTTP listener needs to bind in time.
    let config = format!(
        r#"[[providers]]
type = "claude"
api_key = "sk-ant-isolated-1354-test"

[client]
use_daemon = true
daemon_address = {daemon_address:?}
auto_spawn = false
timeout_seconds = 10
auto_discover = false
prefer_local = true

[backend]
enabled = false
execution_target = "cpu"

[server]
enabled = true
bind_address = "127.0.0.1:0"
brain_bind_address = "127.0.0.1:0"
auth_enabled = false
api_keys = []
mode = "daemon-only"
advertise = false
brain_password = {brain_password:?}
"#
    );
    let finch_dir = home.join(".finch");
    std::fs::create_dir_all(&finch_dir)?;
    let mut file = std::fs::File::create(finch_dir.join("config.toml"))?;
    file.write_all(config.as_bytes())?;
    Ok(())
}

fn user_message(text: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::text(text)],
    }
}

fn tool_result_message(tool_use_id: &str, content: &str) -> Message {
    Message {
        role: "user".to_string(),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: Some(false),
        }],
    }
}

/// A real, `CLAUDE_CLI_TOOL_NAMES`-supported tool name (`"read"`).
/// `ClaudeCliProvider::supported_tool_names` silently drops any requested
/// tool name outside that fixed set — omitting `--mcp-config` entirely and
/// sending a plain no-tools round — so an arbitrary made-up name here would
/// never reach the fake `claude` binary's tool-serving branch at all.
fn read_tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "read".to_string(),
        description: "test tool".to_string(),
        input_schema: finch::tools::ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({}),
            required: vec![],
        },
    }
}

/// Play the real MCP bridge subprocess's own role directly, the same way
/// `crates/finch-providers/src/claude_cli.rs`'s own test module does:
/// connect to the socket path the fake `claude` process wrote to
/// `$SPOOL/socket_path`, and send one line-delimited `tools/call` forward
/// (`name`/`input`), matching `finch_providers::ClaudeCliBridgeToolRequest`'s
/// wire shape. The daemon-owned `drive()` loop is racing this listener's
/// `accept()` against the fake process's stdout, so this is what actually
/// produces the real `StreamChunk::ToolCallComplete` on `rx` — the fake
/// `claude` script's own hardcoded `tool_use` JSON line is cosmetic
/// (`TurnRecord::absorb_line` observation only, never a real chunk).
///
/// The returned connection must be kept alive until the round that answers
/// this call actually settles: the daemon writes its real reply back onto
/// this exact same connection later (`write_bridge_response`), and dropping
/// it early would break that write, failing a round that should otherwise
/// succeed.
async fn simulate_bridge_tool_call(
    spool_dir: &Path,
    name: &str,
    input: serde_json::Value,
) -> Result<tokio::net::UnixStream> {
    wait_for_file(
        &spool_dir.join("socket_path"),
        Instant::now() + Duration::from_secs(10),
    )
    .await?;
    let socket_path = std::fs::read_to_string(spool_dir.join("socket_path"))
        .context("reading the bridge socket path the fake claude process wrote")?;
    let mut stream = tokio::net::UnixStream::connect(socket_path.trim())
        .await
        .context("connecting to the daemon-owned bridge socket as the fake MCP bridge would")?;
    let mut line = serde_json::json!({ "name": name, "input": input }).to_string();
    line.push('\n');
    tokio::io::AsyncWriteExt::write_all(&mut stream, line.as_bytes())
        .await
        .context("forwarding the simulated tools/call request")?;
    Ok(stream)
}

async fn wait_for_file(path: &Path, deadline: Instant) -> Result<()> {
    loop {
        if path.exists() {
            return Ok(());
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out waiting for {} to appear",
            path.display()
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether `pid` still names a live process (`kill -0`, sends no signal).
fn pid_is_alive(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
}

// `IpcClient::brain_claude_cli_round` uses `tokio::task::spawn_local`
// internally (capnp-rpc client objects are `!Send`, matching the
// production frontend's own `LocalSet`-based event loop — see
// `src/providers/claude_cli_daemon.rs`'s module doc comment). Each test
// below therefore runs its real scenario inside a `LocalSet`, mirroring
// `src/cli/repl_event/event_loop/tests.rs`'s own established pattern for
// tests that touch `IpcClient`.
#[tokio::test]
#[ignore = "spawns the built daemon binary"]
async fn production_boundary_claude_cli_round_fails_closed_on_a_mismatched_reattach() -> Result<()>
{
    tokio::task::LocalSet::new()
        .run_until(claude_cli_round_fails_closed_on_a_mismatched_reattach_scenario())
        .await
}

async fn claude_cli_round_fails_closed_on_a_mismatched_reattach_scenario() -> Result<()> {
    let daemon = TestDaemon::start().await?;
    let brain = format!("claude-cli-1354-mismatch-{}", uuid::Uuid::new_v4().simple());
    let spool_dir = daemon.spool.clone();

    let client_a = daemon.connect().await?;
    let mut rx = client_a
        .brain_claude_cli_round(
            &brain,
            vec![user_message("please use the tool")],
            vec![read_tool_definition()],
            None,
        )
        .await?;

    // Play the bridge's role directly (see `simulate_bridge_tool_call`'s own
    // doc comment): this is what actually produces the real
    // `ToolCallComplete` below, not the fake script's own stdout.
    let mut bridge_stream = simulate_bridge_tool_call(
        &spool_dir,
        "probe_tool",
        serde_json::json!({"key": "value"}),
    )
    .await?;

    let call_id = match rx.recv().await.context("no chunk received")?? {
        finch::providers::StreamChunk::ToolCallComplete { id, .. } => id,
        other => anyhow::bail!("expected a paused ToolCallComplete, got {other:?}"),
    };
    anyhow::ensure!(
        rx.recv().await.is_none(),
        "a paused round must end the stream with no trailing chunk"
    );

    // A reattaching frontend that does not know about the pending call:
    // its tail does not answer `call_id`.
    let client_b = daemon.connect().await?;
    let mut mismatched = client_b
        .brain_claude_cli_round(
            &brain,
            vec![
                user_message("please use the tool"),
                tool_result_message("some-other-id-entirely", "wrong answer"),
            ],
            vec![read_tool_definition()],
            None,
        )
        .await?;
    let error = match mismatched.recv().await {
        Some(Ok(chunk)) => anyhow::bail!(
            "a mismatched reattach must fail closed with a named error, not stream a chunk: \
             {chunk:?}"
        ),
        Some(Err(error)) => error,
        None => anyhow::bail!(
            "a mismatched reattach must fail closed with a named error, not silently produce no \
             chunks (which would also be consistent with it having silently abandoned the real \
             parked call and started a second claude process)"
        ),
    };
    let error_text = error.to_string();
    assert!(
        error_text.contains("still pending real interactive execution")
            && error_text.contains(&call_id),
        "the mismatch error must name the real pending call so an operator can diagnose it: \
         {error_text}"
    );

    // The real, correct answer must still work afterward — the mismatched
    // attempt must not have disturbed the parked turn or its still-open
    // bridge connection.
    let client_c = daemon.connect().await?;
    let mut rx2 = client_c
        .brain_claude_cli_round(
            &brain,
            vec![
                user_message("please use the tool"),
                tool_result_message(&call_id, "real tool output"),
            ],
            vec![read_tool_definition()],
            None,
        )
        .await?;

    // The real reply must land on the *same* bridge connection the mismatch
    // never touched.
    let mut reader = tokio::io::BufReader::new(&mut bridge_stream);
    let mut reply_line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut reply_line),
    )
    .await
    .context("the resumed round must answer the still-open bridge connection")??;
    assert!(
        reply_line.contains("real tool output"),
        "the bridge reply must carry the real answer: {reply_line:?}"
    );
    std::fs::write(spool_dir.join("proceed"), b"go")?;

    let mut saw_completion = false;
    while let Some(chunk) = rx2.recv().await {
        if let finch::providers::StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) =
            chunk?
        {
            assert_eq!(text, "tool call handled");
            saw_completion = true;
        }
    }
    assert!(
        saw_completion,
        "the real, correctly-matching round must still complete after a mismatched peek"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "spawns the built daemon binary"]
async fn production_boundary_frontend_disconnect_mid_pending_tool_call_does_not_lose_the_session(
) -> Result<()> {
    tokio::task::LocalSet::new()
        .run_until(frontend_disconnect_mid_pending_tool_call_does_not_lose_the_session_scenario())
        .await
}

async fn frontend_disconnect_mid_pending_tool_call_does_not_lose_the_session_scenario() -> Result<()>
{
    let daemon = TestDaemon::start().await?;
    let brain = format!(
        "claude-cli-1354-disconnect-{}",
        uuid::Uuid::new_v4().simple()
    );

    let spool_dir = daemon.spool.clone();
    let client_a = daemon.connect().await?;
    let mut rx = client_a
        .brain_claude_cli_round(
            &brain,
            vec![user_message("please use the tool")],
            vec![read_tool_definition()],
            None,
        )
        .await?;
    let bridge_stream = simulate_bridge_tool_call(
        &spool_dir,
        "probe_tool",
        serde_json::json!({"key": "value"}),
    )
    .await?;
    let call_id = match rx.recv().await.context("no chunk received")?? {
        finch::providers::StreamChunk::ToolCallComplete { id, .. } => id,
        other => anyhow::bail!("expected a paused ToolCallComplete, got {other:?}"),
    };

    // Disconnect the frontend that started the round: drop its client and
    // its receiver entirely, simulating a crash or network loss while the
    // real tool call is still pending (no human has answered it yet). The
    // simulated bridge connection is a *separate* socket, owned by the
    // daemon's own driven `claude` child, not by this frontend connection —
    // it is deliberately left open, matching how a real bridge subprocess
    // (a child of `claude`, not of the frontend) would survive a frontend
    // disconnect too.
    drop(rx);
    drop(client_a);
    // No wait needed here, deterministically: by the time `rx.recv()`
    // above returned the `ToolCallComplete`, `drive_claude_cli_round`
    // (src/server/ipc.rs) had already sent that chunk and `return`ed,
    // releasing the per-Brain lock — the round-1 RPC call is already fully
    // complete server-side. Dropping `client_a` afterward has nothing left
    // to race: there is no daemon-side state that still needs to "notice"
    // this disconnect before client B's round below can proceed correctly.

    // A fresh frontend connects and answers the still-pending call. If the
    // daemon had lost the session on disconnect, this would either hang (no
    // process left to answer) or spawn a confused second `claude` process
    // instead of resuming the real one.
    let client_b = daemon.connect().await?;
    let mut rx2 = client_b
        .brain_claude_cli_round(
            &brain,
            vec![
                user_message("please use the tool"),
                tool_result_message(&call_id, "real tool output"),
            ],
            vec![read_tool_definition()],
            None,
        )
        .await?;

    // The real reply must land on the bridge connection that survived the
    // frontend disconnect.
    let mut reader = tokio::io::BufReader::new(bridge_stream);
    let mut reply_line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut reply_line),
    )
    .await
    .context(
        "the resumed round must answer the bridge connection that survived the disconnect",
    )??;
    assert!(
        reply_line.contains("real tool output"),
        "the bridge reply must carry the real answer: {reply_line:?}"
    );
    std::fs::write(spool_dir.join("proceed"), b"go")?;

    let mut saw_completion = false;
    while let Some(chunk) = rx2.recv().await {
        if let finch::providers::StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) =
            chunk?
        {
            assert_eq!(text, "tool call handled");
            saw_completion = true;
        }
    }
    assert!(
        saw_completion,
        "the daemon-owned session must survive the originating frontend's disconnect while a \
         tool call is pending, and a reattaching frontend must be able to complete it"
    );

    // Exactly-once: the fake claude process was invoked exactly once for
    // the tool-serving round (call_order.log has exactly one entry for it)
    // — the disconnect must not have caused a second `claude` process to be
    // spawned for the same Brain.
    let call_order = std::fs::read_to_string(spool_dir.join("call_order.log"))?;
    let tool_round_invocations = call_order.lines().count();
    assert_eq!(
        tool_round_invocations, 1,
        "exactly one claude process invocation must have served this Brain's whole round-1 \
         (spawn) + resume cycle; a disconnect-triggered respawn would show as a second \
         invocation here: {call_order:?}"
    );
    Ok(())
}

#[tokio::test]
#[ignore = "spawns the built daemon binary"]
async fn production_boundary_daemon_restart_kills_the_live_claude_child_with_no_orphan(
) -> Result<()> {
    tokio::task::LocalSet::new()
        .run_until(daemon_restart_kills_the_live_claude_child_with_no_orphan_scenario())
        .await
}

async fn daemon_restart_kills_the_live_claude_child_with_no_orphan_scenario() -> Result<()> {
    let mut daemon = TestDaemon::start().await?;
    let brain = format!("claude-cli-1354-restart-{}", uuid::Uuid::new_v4().simple());
    let spool_dir = daemon.spool.clone();

    let client = daemon.connect().await?;
    let mut rx = client
        .brain_claude_cli_round(
            &brain,
            vec![user_message("please use the tool")],
            vec![read_tool_definition()],
            None,
        )
        .await?;
    let _bridge_stream = simulate_bridge_tool_call(
        &spool_dir,
        "probe_tool",
        serde_json::json!({"key": "value"}),
    )
    .await?;
    match rx.recv().await.context("no chunk received")?? {
        finch::providers::StreamChunk::ToolCallComplete { .. } => {}
        other => anyhow::bail!("expected a paused ToolCallComplete, got {other:?}"),
    };

    wait_for_file(
        &spool_dir.join("pid"),
        Instant::now() + Duration::from_secs(10),
    )
    .await?;
    let child_pid: u32 = std::fs::read_to_string(spool_dir.join("pid"))?
        .trim()
        .parse()
        .context("fake claude did not report a numeric pid")?;
    assert!(
        pid_is_alive(child_pid),
        "the live, parked claude child (pid {child_pid}) must exist before the daemon restart \
         this test exercises"
    );

    // Restart the daemon the way `finch daemon-stop`/an operator restart
    // really does: SIGTERM, then let the OS reap it.
    daemon.sigterm_and_wait()?;

    // The daemon owning the child means the child is *its* subprocess; when
    // the daemon exits, the OS delivers no automatic cleanup on its own for
    // an unrelated process group, so this asserts the daemon's own
    // process-lifecycle contract: `RunningTurn`'s `child` is spawned with
    // `kill_on_drop(true)` and the daemon process holds the last reference
    // to it via `ClaudeCliSessionRegistry`, so the child must not outlive
    // the daemon process it belongs to.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !pid_is_alive(child_pid) {
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "the live claude child (pid {child_pid}) outlived the daemon process that owned it \
             — an orphaned process, not exactly-once cleanup on daemon restart"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}
