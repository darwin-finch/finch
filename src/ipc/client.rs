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

use crate::claude::{ContentBlock, Message};
use crate::generators::StreamChunk;
use crate::ipc::schema::finch_ipc_capnp::{
    self, brain_runner, finch_daemon, stream_receiver,
};
use crate::ipc::transport::sock_path;
use crate::tools::types::{ToolDefinition, ToolUse};

pub struct BrainRunnerBootstrap {
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
}

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
        let client: finch_daemon::Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

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
    // Co-Forth
    // -----------------------------------------------------------------------

    /// Send a Forth program to the daemon; get back the full data stack + output.
    /// The top of the returned stack is the "return value" — one number.
    pub async fn eval_forth(&self, program: &str) -> Result<(Vec<i64>, String)> {
        let mut req = self.client.eval_forth_request();
        req.get().set_program(program);
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let error = r.get_error()?.to_str()?;
        if !error.is_empty() {
            anyhow::bail!("{}", error);
        }
        let stack = r.get_stack()?.iter().collect::<Vec<i64>>();
        let output = r.get_output()?.to_str()?.to_string();
        Ok((stack, output))
    }

    // Health
    // -----------------------------------------------------------------------

    pub async fn ping(&self) -> Result<String> {
        let req = self.client.ping_request();
        let reply = req.send().promise.await?;
        Ok(reply.get()?.get_version()?.to_str()?.to_string())
    }

    /// Register this frontend as the callback for its current named-Brain
    /// runner lease. The callback stays on this connection's LocalSet.
    pub async fn register_brain_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::shared::RunnerLeaseId,
        event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>,
    ) -> Result<BrainRunnerBootstrap> {
        let runner: brain_runner::Client =
            capnp_rpc::new_client(BrainRunnerImpl { event_tx });
        let mut request = self.client.register_brain_runner_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_lease_id(&lease_id.0.to_string());
            params.set_runner(runner);
        }
        let reply = request
            .send()
            .promise
            .await
            .map_err(map_runner_registration_error)?;
        let response = reply.get()?;
        Ok(BrainRunnerBootstrap {
            runtime_revision: response.get_runtime_revision(),
            checkpoint: serde_json::from_slice(response.get_checkpoint_json()?)
                .context("daemon returned an invalid named-Brain checkpoint")?,
        })
    }
}

fn map_runner_registration_error(error: capnp::Error) -> anyhow::Error {
    if error.kind == capnp::ErrorKind::Unimplemented {
        anyhow::anyhow!(
            "the running Finch daemon uses an older IPC schema; restart the daemon and reconnect"
        )
    } else {
        anyhow::Error::new(error)
    }
}

struct BrainRunnerImpl {
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>,
}

impl brain_runner::Server for BrainRunnerImpl {
    fn run_program(
        &mut self,
        params: brain_runner::RunProgramParams,
        mut results: brain_runner::RunProgramResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get().and_then(|params| params.get_request()) {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let brain = request
            .get_brain()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let source = request
            .get_source()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let language = match request.get_language() {
            Ok(finch_ipc_capnp::ProgramLanguage::Forth) => {
                crate::brain::shared::ProgramLanguage::Forth
            }
            Ok(finch_ipc_capnp::ProgramLanguage::Lisp) => {
                crate::brain::shared::ProgramLanguage::Lisp
            }
            Err(error) => return Promise::err(error.into()),
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self
            .event_tx
            .send(crate::cli::repl_event::ReplEvent::NamedBrainProgramRequested(
                crate::server::RunnerProgramRequest {
                    brain,
                    request_seq: request.get_request_seq(),
                    language,
                    source,
                    response_tx,
                },
            ))
            .is_err()
        {
            return Promise::err(capnp::Error::failed("frontend event loop stopped".into()));
        }
        Promise::from_future(async move {
            let response = response_rx
                .await
                .map_err(|_| capnp::Error::failed("frontend dropped runner response".into()))?;
            let mut result = results.get().init_result();
            match response {
                Ok(response) => {
                    result.set_output(&response.output);
                    result.set_runtime_revision(response.runtime_revision);
                    let encoded = serde_json::to_vec(&response.checkpoint)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    result.set_checkpoint_json(&encoded);
                    result.set_error("");
                }
                Err(error) => result.set_error(&error),
            }
            Ok(())
        })
    }

    fn run_turn(
        &mut self,
        params: brain_runner::RunTurnParams,
        mut results: brain_runner::RunTurnResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get().and_then(|params| params.get_request()) {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let brain = request
            .get_brain()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let prompt = request
            .get_prompt()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let context = match request
            .get_context_json()
            .map_err(anyhow::Error::new)
            .and_then(|value| serde_json::from_slice(value).map_err(anyhow::Error::new))
        {
            Ok(context) => context,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self
            .event_tx
            .send(crate::cli::repl_event::ReplEvent::NamedBrainTurnRequested(
                crate::server::RunnerTurnRequest {
                    brain,
                    request_seq: request.get_request_seq(),
                    prompt,
                    context,
                    response_tx,
                },
            ))
            .is_err()
        {
            return Promise::err(capnp::Error::failed("frontend event loop stopped".into()));
        }
        Promise::from_future(async move {
            let response = response_rx
                .await
                .map_err(|_| capnp::Error::failed("frontend dropped runner response".into()))?;
            let mut result = results.get().init_result();
            match response {
                Ok(response) => {
                    result.set_source(&response.source);
                    result.set_language(match response.language {
                        crate::brain::shared::ProgramLanguage::Forth => {
                            finch_ipc_capnp::ProgramLanguage::Forth
                        }
                        crate::brain::shared::ProgramLanguage::Lisp => {
                            finch_ipc_capnp::ProgramLanguage::Lisp
                        }
                    });
                    result.set_output(&response.output);
                    let mut tool_events = result
                        .reborrow()
                        .init_tool_events(response.tool_events.len() as u32);
                    for (index, event) in response.tool_events.iter().enumerate() {
                        let mut encoded = tool_events.reborrow().get(index as u32);
                        match event {
                            crate::server::RunnerToolEvent::Call {
                                tool_id,
                                name,
                                input,
                            } => {
                                encoded.set_kind(finch_ipc_capnp::BrainToolEventKind::Call);
                                encoded.set_tool_id(tool_id);
                                encoded.set_name(name);
                                let input = serde_json::to_vec(input)
                                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                                encoded.set_input_json(&input);
                            }
                            crate::server::RunnerToolEvent::Result {
                                tool_id,
                                output,
                                is_error,
                            } => {
                                encoded.set_kind(finch_ipc_capnp::BrainToolEventKind::Result);
                                encoded.set_tool_id(tool_id);
                                encoded.set_output(output);
                                encoded.set_is_error(*is_error);
                            }
                        }
                    }
                    result.set_runtime_revision(response.runtime_revision);
                    let encoded = serde_json::to_vec(&response.checkpoint)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    result.set_checkpoint_json(&encoded);
                    result.set_error("");
                }
                Err(error) => result.set_error(&error),
            }
            Ok(())
        })
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
            Ok(Which::TextDelta(t)) => t
                .and_then(|s| {
                    s.to_str()
                        .map(|s| s.to_string())
                        .map_err(|e| capnp::Error::failed(e.to_string()))
                })
                .map(StreamChunk::TextDelta)
                .map_err(|e| anyhow::anyhow!("{}", e)),
            Ok(Which::ToolUseComplete(tu)) => tu
                .and_then(|tu| {
                    let id = tu
                        .get_id()?
                        .to_str()
                        .map_err(|e| capnp::Error::failed(e.to_string()))?
                        .to_string();
                    let name = tu
                        .get_name()?
                        .to_str()
                        .map_err(|e| capnp::Error::failed(e.to_string()))?
                        .to_string();
                    let input_str = tu.get_input_json()?.to_str()?.to_string();
                    let input: serde_json::Value =
                        serde_json::from_str(&input_str).unwrap_or(serde_json::Value::Null);
                    Ok(StreamChunk::ContentBlockComplete(ContentBlock::ToolUse {
                        id,
                        name,
                        input,
                    }))
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
                e.and_then(|s| s
                    .to_str()
                    .map(|s| s.to_string())
                    .map_err(|e| capnp::Error::failed(e.to_string())))
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
        let mut content = m.init_content(msg.content.len() as u32);
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
    let text = r
        .get_text()?
        .to_str()
        .map_err(|e| capnp::Error::failed(e.to_string()))?
        .to_string();
    let model = r
        .get_model()?
        .to_str()
        .map_err(|e| capnp::Error::failed(e.to_string()))?
        .to_string();
    let input_tokens = r.get_input_tokens();
    let output_tokens = r.get_output_tokens();
    let latency_ms = r.get_latency_ms();

    let mut tool_uses = Vec::new();
    for tu in r.get_tool_uses()?.iter() {
        let input: serde_json::Value =
            serde_json::from_str(tu.get_input_json()?.to_str()?).unwrap_or(serde_json::Value::Null);
        tool_uses.push(ToolUse {
            id: tu
                .get_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string()))?
                .to_string(),
            name: tu
                .get_name()?
                .to_str()
                .map_err(|e| capnp::Error::failed(e.to_string()))?
                .to_string(),
            input,
        });
    }

    Ok(QueryResponse {
        text,
        tool_uses,
        model,
        input_tokens: if input_tokens == 0 {
            None
        } else {
            Some(input_tokens)
        },
        output_tokens: if output_tokens == 0 {
            None
        } else {
            Some(output_tokens)
        },
        latency_ms: if latency_ms == 0 {
            None
        } else {
            Some(latency_ms)
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_registration_explains_an_older_daemon_schema() {
        let error = map_runner_registration_error(capnp::Error::unimplemented(
            "remote method missing".into(),
        ));
        let message = error.to_string();
        assert!(message.contains("older IPC schema"));
        assert!(message.contains("restart the daemon"));
    }

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
