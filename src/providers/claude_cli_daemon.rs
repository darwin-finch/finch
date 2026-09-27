//! Frontend-side thin client for a daemon-owned Claude CLI Subscription
//! session (issue #1354).
//!
//! Implements the exact same `ProviderBackend`/`LlmProvider` contract
//! `finch_providers::ClaudeCliProvider` already implements, so
//! `ToolLoop`/`ToolExecutionCoordinator`/`query_processor.rs` cannot tell
//! the difference and need no changes. Only *how* a round is driven changed:
//! instead of spawning `claude` locally, each round is one
//! `BrainService.claudeCliRound` capnp call to the daemon, which now owns
//! the real `claude` process and its MCP bridge socket. A round that pauses
//! for real tool execution still surfaces as an ordinary
//! `StreamChunk::ToolCallComplete` in the returned stream, with the stream
//! then ending — `crate::client::ipc::IpcClient::brain_claude_cli_round`'s
//! `StreamReceiverImpl` decodes the wire's `claudeCliToolCallPending` chunk
//! back into that exact variant, so a caller downstream of the stream
//! cannot tell a socket read from an IPC hop apart.
//!
//! **Why an actor, not a direct `IpcClient` handle.** `ProviderBackend`
//! requires `Send + Sync` (every provider's futures cross task boundaries),
//! but `IpcClient` is deliberately `!Send`/`!Sync`: capnp-rpc's client
//! objects (`Rc<RpcTask>`, `dyn ClientHook`) are single-threaded, and the
//! whole frontend event loop already runs inside one `tokio::task::LocalSet`
//! for exactly that reason (see `src/cli/repl_event/event_loop.rs`'s own
//! "Must live inside a tokio LocalSet (capnp-rpc !Send)" note). Rather than
//! holding an `IpcClient` directly, [`DaemonClaudeCliProvider`] holds a
//! [`DaemonClaudeCliHandle`] — a plain `Send + Sync` channel handle to a
//! dedicated `spawn_local` task that owns the real `IpcClient` and is the
//! only thing that ever touches it, mirroring the same
//! actor-behind-a-Send-safe-handle shape this codebase already uses
//! daemon-side for frontend-owned runner callbacks (`BrainRunnerBroker`).

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tokio::sync::{mpsc, mpsc::Receiver, oneshot};

use finch_providers::{
    CapabilitySupport, Message, ModelCapabilities, ProviderBackend, ProviderResponse,
    ReasoningCapability, StreamChunk, ToolDefinition, ValidatedProviderRequest,
    CLAUDE_CLI_DEFAULT_MODEL, CLAUDE_CLI_PROVIDER_NAME,
};

use crate::client::ipc::IpcClient;
use crate::providers::ContentBlock;

/// One requested round, sent from any (`Send`) task to the actor task that
/// owns the real `IpcClient`.
struct RoundRequest {
    brain: String,
    messages: Vec<Message>,
    tools: Vec<ToolDefinition>,
    model: Option<String>,
    reply: oneshot::Sender<Result<mpsc::UnboundedReceiver<Result<StreamChunk>>>>,
}

/// `Send + Sync` handle to a dedicated `spawn_local` task that owns one
/// `IpcClient` connection. See the module doc comment.
#[derive(Clone)]
pub struct DaemonClaudeCliHandle {
    tx: mpsc::UnboundedSender<RoundRequest>,
}

impl DaemonClaudeCliHandle {
    /// Spawn the owning actor task onto the *current* `LocalSet` (the
    /// caller must already be running inside one — every frontend entry
    /// point that could construct a Brain-scoped provider already is) and
    /// return a `Send`-safe handle to it. `client` becomes exclusively
    /// owned by that task; nothing outside this module ever touches it
    /// again.
    pub fn spawn_local(client: IpcClient) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<RoundRequest>();
        tokio::task::spawn_local(async move {
            while let Some(request) = rx.recv().await {
                let result = client
                    .brain_claude_cli_round(
                        &request.brain,
                        request.messages,
                        request.tools,
                        request.model.as_deref(),
                    )
                    .await;
                // The caller may have stopped waiting (e.g. its own request
                // future was dropped); a failed send here just means no one
                // is listening for this round's result anymore, not a bug.
                let _ = request.reply.send(result);
            }
            // `tx` (and every clone) dropped: no live `DaemonClaudeCliHandle`
            // remains, so this actor and its `IpcClient` connection can shut
            // down. Nothing else to clean up — the connection's own `Drop`
            // handles that.
        });
        Self { tx }
    }

    async fn round(
        &self,
        brain: &str,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
        model: Option<String>,
    ) -> Result<mpsc::UnboundedReceiver<Result<StreamChunk>>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(RoundRequest {
                brain: brain.to_string(),
                messages,
                tools,
                model,
                reply: reply_tx,
            })
            .map_err(|_| {
                anyhow::anyhow!(
                    "daemon-owned Claude CLI Subscription actor for brain '{brain}' is no \
                     longer running (its LocalSet task exited)"
                )
            })?;
        reply_rx.await.context(
            "daemon-owned Claude CLI Subscription actor dropped this round's reply \
                       channel without answering",
        )?
    }
}

/// Frontend-side handle to a specific Brain's daemon-owned Claude CLI
/// Subscription session. See the module doc comment.
#[derive(Clone)]
pub struct DaemonClaudeCliProvider {
    handle: DaemonClaudeCliHandle,
    brain: String,
    model: String,
}

impl DaemonClaudeCliProvider {
    pub fn new(handle: DaemonClaudeCliHandle, brain: String, model: Option<String>) -> Self {
        Self {
            handle,
            brain,
            model: resolve_model(model),
        }
    }

    /// Convenience constructor: connects a fresh `IpcClient` and spawns its
    /// owning actor onto the current `LocalSet` in one step. The caller
    /// must already be running inside a `LocalSet` (every frontend entry
    /// point that selects a Brain's provider already is — see the module
    /// doc comment).
    pub async fn connect(brain: String, model: Option<String>) -> Result<Self> {
        let client = IpcClient::connect()
            .await
            .context("connecting to the Finch daemon for a daemon-owned Claude CLI session")?;
        Ok(Self::new(
            DaemonClaudeCliHandle::spawn_local(client),
            brain,
            model,
        ))
    }

    /// The Brain this session drives (issue #1354). Exposed so the
    /// application-layer selection wiring (`src/providers/factory.rs`) can
    /// identify which live daemon-owned session a constructed provider
    /// fronts, e.g. for logging or session teardown on provider reselection.
    pub fn brain(&self) -> &str {
        &self.brain
    }

    /// Drive one round against the daemon-owned session and collect it into
    /// a local `mpsc::Sender`, matching exactly what
    /// `ClaudeCliProvider::send_message_stream_validated` does for the
    /// local (non-daemon) case: on a genuine finish, emit one trailing
    /// `ContentBlockComplete` built from the accumulated text; on a pause,
    /// the `ToolCallComplete` chunk already relayed live is the last thing
    /// sent and nothing further follows.
    async fn drive_round(
        &self,
        request: &finch_providers::ProviderRequest,
        deltas: Option<mpsc::Sender<Result<StreamChunk>>>,
    ) -> Result<RoundOutcome> {
        let mut rx = self
            .handle
            .round(
                &self.brain,
                request.messages.clone(),
                request.tools.clone().unwrap_or_default(),
                Some(self.model.clone()),
            )
            .await?;

        let mut response_text = String::new();
        while let Some(item) = rx.recv().await {
            let chunk = item?;
            let is_tool_call = matches!(chunk, StreamChunk::ToolCallComplete { .. });
            // The daemon's own reused `ClaudeCliProvider::send_message_stream_validated`
            // already synthesizes exactly one `ContentBlockComplete` on a genuine finish
            // (crates/finch-providers/src/claude_cli.rs), which arrives here over the wire
            // like any other chunk. This capture-and-swallow (never forwarded to `deltas`)
            // matches that same method's own shape on the local, non-daemon path: its
            // internal `execute_turn`/`drive` never emit `ContentBlockComplete` themselves
            // either, leaving exactly one synthesis point below. Forwarding it here too, on
            // top of the one this function synthesizes on `RoundOutcome::Complete`, would
            // double the assistant's finished text for every daemon-owned round.
            let is_content_complete = matches!(chunk, StreamChunk::ContentBlockComplete(_));
            match &chunk {
                StreamChunk::TextDelta(delta) => response_text.push_str(delta),
                StreamChunk::ContentBlockComplete(ContentBlock::Text { text }) => {
                    response_text = text.clone();
                }
                _ => {}
            }
            if let Some(deltas) = &deltas {
                if !is_content_complete {
                    let _ = deltas.send(Ok(chunk)).await;
                }
            }
            if is_tool_call {
                return Ok(RoundOutcome::Paused);
            }
        }
        Ok(RoundOutcome::Complete(response_text))
    }
}

enum RoundOutcome {
    Complete(String),
    Paused,
}

#[async_trait]
impl ProviderBackend for DaemonClaudeCliProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        match self.drive_round(&request, None).await? {
            RoundOutcome::Complete(text) => Ok(ProviderResponse {
                id: uuid::Uuid::new_v4().to_string(),
                model: request.model.clone(),
                content: vec![ContentBlock::text(text)],
                stop_reason: Some("end_turn".to_string()),
                role: "assistant".to_string(),
                provider: CLAUDE_CLI_PROVIDER_NAME.to_string(),
                usage: None,
                allowance: None,
            }),
            RoundOutcome::Paused => bail!(
                "daemon-owned claude CLI subscription session paused mid-turn with no \
                 streaming sink; the non-streaming path must always answer a pending tool \
                 call itself rather than pausing (issue #1354 — this indicates a bug, not a \
                 recoverable runtime condition)"
            ),
        }
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>> {
        let (request, _bindings) = request.into_request_for(self)?;
        let (tx, rx) = mpsc::channel::<Result<StreamChunk>>(64);
        let worker = self.clone();
        tokio::spawn(async move {
            let result = worker.drive_round(&request, Some(tx.clone())).await;
            match result {
                Ok(RoundOutcome::Complete(text)) => {
                    let _ = tx
                        .send(Ok(StreamChunk::ContentBlockComplete(ContentBlock::text(
                            text,
                        ))))
                        .await;
                }
                Ok(RoundOutcome::Paused) => {
                    // The stream ends here, exactly like every other
                    // provider's stream after a native tool call (and
                    // exactly like the local, non-daemon
                    // `ClaudeCliProvider`): the frontend's real ToolLoop
                    // executes the pending call and the next Finch-level
                    // round resumes this same daemon-owned session.
                }
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                }
            }
        });
        Ok(rx)
    }

    fn name(&self) -> &str {
        CLAUDE_CLI_PROVIDER_NAME
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        // Mirrors `ClaudeCliProvider::capabilities` exactly: tool calls are
        // supported through Finch's own real ToolLoop (now reached over one
        // extra IPC hop, see this module's doc comment), never the CLI's
        // own built-in tools.
        ModelCapabilities::static_metadata(
            CLAUDE_CLI_PROVIDER_NAME,
            model,
            "2026-09-27",
            "daemon-owned Claude CLI Subscription session (issue #1354), same measured contract \
             as the local ClaudeCliProvider it fronts",
            CapabilitySupport::Supported,
            CapabilitySupport::Supported,
            CapabilitySupport::Unsupported,
            ReasoningCapability::unsupported("2026-09-27", "matches ClaudeCliProvider"),
            Some(200_000),
            Some(64_000),
            None,
        )
    }
}

/// `model.unwrap_or_else(|| CLAUDE_CLI_DEFAULT_MODEL.to_string())`, pulled
/// out to a free function so the default-model contract is unit-testable
/// without a real, connected `IpcClient`.
fn resolve_model(model: Option<String>) -> String {
    model.unwrap_or_else(|| CLAUDE_CLI_DEFAULT_MODEL.to_string())
}

#[allow(dead_code)]
fn _assert_provider_backend_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<DaemonClaudeCliProvider>();
    assert_send_sync::<DaemonClaudeCliHandle>();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_model_defaults_to_the_claude_cli_default_model() {
        // Regression guard: this must track `ClaudeCliProvider::new`'s own
        // default exactly, since a daemon-owned session and a local one for
        // the same Brain name must never silently disagree about which
        // model a bare `None` means.
        assert_eq!(
            resolve_model(None),
            CLAUDE_CLI_DEFAULT_MODEL,
            "DaemonClaudeCliProvider's default model drifted from ClaudeCliProvider's"
        );
    }

    #[tokio::test]
    async fn round_fails_closed_when_the_actor_task_is_gone() {
        // Hostile timing: the actor's LocalSet task exited (e.g. its
        // IpcClient connection died and the loop returned) but a caller
        // still holds a `DaemonClaudeCliHandle` clone. The round must fail
        // with a clear, named error, never hang forever on a reply that
        // will never arrive.
        let (tx, rx) = mpsc::unbounded_channel::<RoundRequest>();
        drop(rx); // Nothing will ever receive a RoundRequest again.
        let handle = DaemonClaudeCliHandle { tx };
        let error = handle
            .round("some-brain", Vec::new(), Vec::new(), None)
            .await
            .expect_err("a dead actor task must fail the round, not hang");
        assert!(
            error.to_string().contains("no longer running"),
            "the error must name the real cause (actor task gone), not a generic failure: {error}"
        );
    }

    /// A `DaemonClaudeCliHandle` whose "actor" answers exactly one round
    /// with a canned wire chunk sequence, without any real `IpcClient` or
    /// daemon connection — enough to exercise `drive_round`'s own chunk
    /// handling in isolation.
    fn canned_handle(chunks: Vec<Result<StreamChunk>>) -> DaemonClaudeCliHandle {
        let (tx, mut rx) = mpsc::unbounded_channel::<RoundRequest>();
        tokio::spawn(async move {
            let Some(request) = rx.recv().await else {
                return;
            };
            let (chunk_tx, chunk_rx) = mpsc::unbounded_channel();
            for chunk in chunks {
                let _ = chunk_tx.send(chunk);
            }
            let _ = request.reply.send(Ok(chunk_rx));
        });
        DaemonClaudeCliHandle { tx }
    }

    #[tokio::test]
    async fn drive_round_forwards_the_daemons_content_block_complete_at_most_once() {
        // Regression (issue #1354, independent review finding): the daemon's
        // own reused `ClaudeCliProvider::send_message_stream_validated`
        // already synthesizes one `ContentBlockComplete` on a genuine finish
        // and sends it over the wire like any other chunk. Forwarding that
        // wire chunk to `deltas` *and* letting `send_message_stream_validated`
        // synthesize a second one from the accumulated text doubled the
        // assistant's finished response text for every daemon-owned round.
        let handle = canned_handle(vec![
            Ok(StreamChunk::TextDelta("real ".to_string())),
            Ok(StreamChunk::TextDelta("answer".to_string())),
            Ok(StreamChunk::ContentBlockComplete(ContentBlock::text(
                "real answer",
            ))),
        ]);
        let provider = DaemonClaudeCliProvider {
            handle,
            brain: "some-brain".to_string(),
            model: CLAUDE_CLI_DEFAULT_MODEL.to_string(),
        };
        let (tx, mut rx) = mpsc::channel::<Result<StreamChunk>>(16);
        let outcome = provider
            .drive_round(&finch_providers::ProviderRequest::new(Vec::new()), Some(tx))
            .await
            .unwrap();
        match outcome {
            RoundOutcome::Complete(text) => assert_eq!(text, "real answer"),
            RoundOutcome::Paused => panic!("expected Complete, got Paused"),
        }
        let mut forwarded = Vec::new();
        while let Ok(chunk) = rx.try_recv() {
            forwarded.push(chunk.unwrap());
        }
        let content_complete_count = forwarded
            .iter()
            .filter(|chunk| matches!(chunk, StreamChunk::ContentBlockComplete(_)))
            .count();
        assert_eq!(
            content_complete_count, 0,
            "drive_round must never itself forward the wire's ContentBlockComplete chunk to \
             `deltas` — send_message_stream_validated synthesizes the caller-facing one exactly \
             once, from the RoundOutcome::Complete text this function returns: forwarded={forwarded:?}"
        );
    }
}
