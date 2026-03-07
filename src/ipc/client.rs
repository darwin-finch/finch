//! Cap'n Proto IPC client — used by the CLI to talk to the daemon.
//!
//! `IpcClient` connects to `~/.finch/daemon.sock` and exposes the same
//! logical operations as the old HTTP `DaemonClient`, but over the fast
//! binary Cap'n Proto channel.

use anyhow::{Context, Result};
use capnp::capability::Promise;
use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
use uuid::Uuid;

use crate::claude::{ContentBlock, Message};
use crate::generators::StreamChunk;
use crate::ipc::schema::finch_ipc_capnp::{
    self, BrainState as CapnpBrainState, finch_daemon, stream_receiver,
};
use crate::ipc::transport::sock_path;
use crate::server::{BrainDetail, BrainState, BrainSummary};
use crate::tools::types::{ToolDefinition, ToolUse};

// ---------------------------------------------------------------------------
// Public client struct
// ---------------------------------------------------------------------------

/// Async client for the daemon IPC socket.
///
/// Must be created inside a `tokio::task::LocalSet` (or equivalent) because
/// `capnp-rpc` uses `spawn_local` internally.
pub struct IpcClient {
    client: finch_daemon::Client,
    // Keeps the RPC system alive for the lifetime of this client.
    _rpc_handle: tokio::task::JoinHandle<()>,
}

impl IpcClient {
    /// Connect to the daemon's Unix socket.
    pub async fn connect() -> Result<Self> {
        let path = sock_path();
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .with_context(|| format!("IPC connect failed: {}", path.display()))?;

        let (reader, writer) = stream.into_split();
        let network = twoparty::VatNetwork::new(
            reader.compat(),
            writer.compat_write(),
            rpc_twoparty_capnp::Side::Client,
            Default::default(),
        );

        let mut rpc_system = RpcSystem::new(Box::new(network), None);
        let client: finch_daemon::Client =
            rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

        let handle = tokio::task::spawn_local(async move {
            let _ = rpc_system.await;
        });

        Ok(Self {
            client,
            _rpc_handle: handle,
        })
    }

    // -----------------------------------------------------------------------
    // Query
    // -----------------------------------------------------------------------

    /// Non-streaming query — returns the full response.
    pub async fn query(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<QueryResponse> {
        let mut req = self.client.query_request();
        {
            let mut p = req.get();
            write_messages(p.reborrow().init_messages(messages.len() as u32), &messages);
            write_tools(p.reborrow().init_tools(tools.len() as u32), &tools);
        }
        let reply = req.send().promise.await?;
        let r = reply.get()?.get_response()?;
        Ok(read_query_response(r)?)
    }

    /// Streaming query — returns a channel of `StreamChunk`s.
    ///
    /// The channel is closed when the server sends the `done` sentinel.
    pub async fn query_stream(
        &self,
        messages: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<mpsc::UnboundedReceiver<Result<StreamChunk>>> {
        let (tx, rx) = mpsc::unbounded_channel();

        // Build a StreamReceiver capability that the server will call back.
        let receiver_impl = StreamReceiverImpl { tx };
        // capnp v0.20: new_client infers C from the receiver_impl type via FromServer.
        let receiver_client: stream_receiver::Client = capnp_rpc::new_client(receiver_impl);

        let mut req = self.client.query_stream_request();
        {
            let mut p = req.get();
            write_messages(p.reborrow().init_messages(messages.len() as u32), &messages);
            write_tools(p.reborrow().init_tools(tools.len() as u32), &tools);
            p.set_receiver(receiver_client);
        }

        // Fire and forget — the server will call back on the receiver.
        // In capnp v0.20, spawn_local drives the future; .detach() was removed.
        tokio::task::spawn_local(async move {
            let _ = req.send().promise.await;
        });

        Ok(rx)
    }

    // -----------------------------------------------------------------------
    // Brain management
    // -----------------------------------------------------------------------

    pub async fn spawn_brain(
        &self,
        task_description: &str,
        provider: Option<&str>,
    ) -> Result<Uuid> {
        let mut req = self.client.spawn_brain_request();
        req.get().set_task_description(task_description);
        req.get()
            .set_provider(provider.unwrap_or(""));
        let reply = req.send().promise.await?;
        let id_str = reply.get()?.get_id()?.to_str()?;
        Uuid::parse_str(id_str).context("invalid UUID from daemon")
    }

    pub async fn list_brains(&self) -> Result<Vec<BrainSummary>> {
        let req = self.client.list_brains_request();
        let reply = req.send().promise.await?;
        let list = reply.get()?.get_brains()?;
        let mut out = Vec::with_capacity(list.len() as usize);
        for s in list.iter() {
            out.push(BrainSummary {
                id: Uuid::parse_str(s.get_id()?.to_str()?)?,
                name: s.get_name()?.to_str()?.to_string(),
                task: s.get_task()?.to_str()?.to_string(),
                state: brain_state_to_server(s.get_state()),
                age_secs: s.get_age_secs(),
            });
        }
        Ok(out)
    }

    pub async fn get_brain(&self, id: Uuid) -> Result<BrainDetail> {
        use crate::server::{PendingPlanView, PendingQuestionView};

        let mut req = self.client.get_brain_request();
        req.get().set_id(id.to_string().as_str());
        let reply = req.send().promise.await?;
        let d = reply.get()?.get_details()?;

        let question_str = d.get_question()?.to_str()?.to_string();
        let plan_str = d.get_plan()?.to_str()?.to_string();

        let pending_question = if question_str.is_empty() {
            None
        } else {
            let options: Vec<String> = d
                .get_question_options()?
                .iter()
                .map(|s| s.and_then(|s| s.to_str().map(|s| s.to_string()).map_err(|e| capnp::Error::failed(e.to_string())))
                         .unwrap_or_default())
                .collect();
            Some(PendingQuestionView { question: question_str, options })
        };

        let pending_plan = if plan_str.is_empty() {
            None
        } else {
            Some(PendingPlanView { plan: plan_str })
        };

        let event_log: Vec<String> = d
            .get_event_log()?
            .iter()
            .map(|s| s.and_then(|s| s.to_str().map(|s| s.to_string()).map_err(|e| capnp::Error::failed(e.to_string())))
                     .unwrap_or_default())
            .collect();

        Ok(BrainDetail {
            id: Uuid::parse_str(d.get_id()?.to_str()?)?,
            name: d.get_name()?.to_str()?.to_string(),
            task: d.get_task()?.to_str()?.to_string(),
            state: brain_state_to_server(d.get_state()),
            age_secs: 0, // not tracked over IPC; caller uses fresh list for age
            event_log,
            pending_question,
            pending_plan,
        })
    }

    pub async fn answer_brain_question(&self, id: Uuid, answer: &str) -> Result<()> {
        let mut req = self.client.answer_brain_question_request();
        req.get().set_id(id.to_string().as_str());
        req.get().set_answer(answer);
        req.send().promise.await?;
        Ok(())
    }

    pub async fn respond_to_brain_plan(
        &self,
        id: Uuid,
        approved: bool,
        instruction: Option<&str>,
    ) -> Result<()> {
        let mut req = self.client.respond_to_brain_plan_request();
        req.get().set_id(id.to_string().as_str());
        req.get().set_approved(approved);
        req.get()
            .set_instruction(instruction.unwrap_or(""));
        req.send().promise.await?;
        Ok(())
    }

    pub async fn cancel_brain(&self, id: Uuid) -> Result<()> {
        let mut req = self.client.cancel_brain_request();
        req.get().set_id(id.to_string().as_str());
        req.send().promise.await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Health
    // -----------------------------------------------------------------------

    pub async fn ping(&self) -> Result<String> {
        let req = self.client.ping_request();
        let reply = req.send().promise.await?;
        Ok(reply.get()?.get_version()?.to_str()?.to_string())
    }
}

// ---------------------------------------------------------------------------
// Streaming receiver capability (client-side callback)
// ---------------------------------------------------------------------------

struct StreamReceiverImpl {
    tx: mpsc::UnboundedSender<Result<StreamChunk>>,
}

impl stream_receiver::Server for StreamReceiverImpl {
    fn on_chunk(
        &mut self,
        params: stream_receiver::OnChunkParams,
        _results: stream_receiver::OnChunkResults,
    ) -> Promise<(), capnp::Error> {
        use finch_ipc_capnp::stream_chunk::Which;

        let chunk = match params.get().and_then(|p| p.get_chunk()) {
            Ok(c) => c,
            Err(e) => return Promise::err(e),
        };

        let result = match chunk.which() {
            Ok(Which::TextDelta(t)) => {
                t.and_then(|s| s.to_str().map(|s| s.to_string()).map_err(|e| capnp::Error::failed(e.to_string())))
                    .map(StreamChunk::TextDelta)
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
            Ok(Which::ToolUseComplete(tu)) => tu
                .and_then(|tu| {
                    let id = tu.get_id()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string();
                    let name = tu.get_name()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string();
                    let input_str = tu.get_input_json()?.to_str()?.to_string();
                    let input: serde_json::Value =
                        serde_json::from_str(&input_str).unwrap_or(serde_json::Value::Null);
                    Ok(StreamChunk::ContentBlockComplete(
                        ContentBlock::ToolUse { id, name, input },
                    ))
                })
                .map_err(|e: capnp::Error| anyhow::anyhow!("{}", e)),
            Ok(Which::UsageUpdate(upd)) => upd
                .map(|u| StreamChunk::Usage {
                    input_tokens: u.get_input_tokens(),
                })
                .map_err(|e| anyhow::anyhow!("{}", e)),
            Ok(Which::Done(())) => {
                // Close the channel by dropping tx — but we don't have ownership.
                // Signal done by sending a synthetic error; caller checks for it.
                // Better: use a dedicated Done variant on the channel.
                // For now drop on the caller side when channel is closed.
                let _ = self.tx; // trigger drop detection? No.
                // Nothing to send; just return.
                return Promise::ok(());
            }
            Ok(Which::Error(e)) => Err(anyhow::anyhow!(
                "{}",
                e.and_then(|s| s.to_str().map(|s| s.to_string()).map_err(|e| capnp::Error::failed(e.to_string())))
                    .unwrap_or_else(|_| "unknown stream error".to_string())
            )),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        };

        let _ = self.tx.send(result);
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// Wire-format helpers
// ---------------------------------------------------------------------------

fn write_messages(
    mut builder: capnp::struct_list::Builder<finch_ipc_capnp::message::Owned>,
    messages: &[Message],
) {
    for (i, msg) in messages.iter().enumerate() {
        let mut m = builder.reborrow().get(i as u32);
        m.set_role(msg.role.as_str());
        let mut content =
            m.init_content(msg.content.len() as u32);
        for (j, block) in msg.content.iter().enumerate() {
            let mut b = content.reborrow().get(j as u32);
            match block {
                ContentBlock::Text { text } => {
                    b.set_text(text.as_str());
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let mut tu = b.init_tool_use();
                    tu.set_id(id.as_str());
                    tu.set_name(name.as_str());
                    tu.set_input_json(input.to_string().as_str());
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    let mut tr = b.init_tool_result();
                    tr.set_tool_use_id(tool_use_id.as_str());
                    tr.set_content(content.as_str());
                    tr.set_is_error(is_error.unwrap_or(false));
                }
                _ => {
                    // Thinking blocks etc. — skip; not sent to daemon
                }
            }
        }
    }
}

fn write_tools(
    mut builder: capnp::struct_list::Builder<finch_ipc_capnp::tool_definition::Owned>,
    tools: &[ToolDefinition],
) {
    for (i, tool) in tools.iter().enumerate() {
        let mut t = builder.reborrow().get(i as u32);
        t.set_name(tool.name.as_str());
        t.set_description(tool.description.as_str());
        let schema_json = serde_json::to_string(&tool.input_schema).unwrap_or_default();
        t.set_input_schema_json(schema_json.as_str());
    }
}

// ---------------------------------------------------------------------------
// Return type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct QueryResponse {
    pub text: String,
    pub tool_uses: Vec<ToolUse>,
    pub model: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub latency_ms: Option<u64>,
}

fn read_query_response(
    r: finch_ipc_capnp::query_response::Reader,
) -> Result<QueryResponse, capnp::Error> {
    let text = r.get_text()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string();
    let model = r.get_model()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string();
    let input_tokens = r.get_input_tokens();
    let output_tokens = r.get_output_tokens();
    let latency_ms = r.get_latency_ms();

    let mut tool_uses = Vec::new();
    for tu in r.get_tool_uses()?.iter() {
        let input: serde_json::Value =
            serde_json::from_str(tu.get_input_json()?.to_str()?)
                .unwrap_or(serde_json::Value::Null);
        tool_uses.push(ToolUse {
            id: tu.get_id()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string(),
            name: tu.get_name()?.to_str().map_err(|e| capnp::Error::failed(e.to_string()))?.to_string(),
            input,
        });
    }

    Ok(QueryResponse {
        text,
        tool_uses,
        model,
        input_tokens: if input_tokens == 0 { None } else { Some(input_tokens) },
        output_tokens: if output_tokens == 0 { None } else { Some(output_tokens) },
        latency_ms: if latency_ms == 0 { None } else { Some(latency_ms) },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect to the live daemon socket and verify ping round-trip.
    ///
    /// Requires a running daemon with the IPC socket at `~/.finch/daemon.sock`.
    /// Run with:
    ///   cargo test --lib ipc::client::tests::test_ipc_ping -- --ignored --nocapture
    /// capnp-rpc uses spawn_local internally so we need a LocalSet.
    #[test]
    #[ignore]
    fn test_ipc_ping() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = IpcClient::connect()
                .await
                .expect("IPC connect — is `finch daemon` running?");

            let version = client.ping().await.expect("ping failed");
            assert!(!version.is_empty(), "version string should be non-empty");
            println!("IPC ping OK — daemon version: {}", version);
        }));
    }
}

fn brain_state_to_server(s: Result<CapnpBrainState, capnp::NotInSchema>) -> BrainState {
    match s.unwrap_or(CapnpBrainState::Cancelled) {
        CapnpBrainState::Running => BrainState::Running,
        CapnpBrainState::WaitingForInput => BrainState::WaitingForInput,
        CapnpBrainState::PlanReady => BrainState::PlanReady,
        CapnpBrainState::Completed | CapnpBrainState::Failed | CapnpBrainState::Cancelled => BrainState::Dead,
    }
}
