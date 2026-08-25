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
use crate::ipc::brain_codec::{
    decode_approval_audience, decode_attachment, decode_brain_wire_reader, decode_event,
    decode_run, decode_runner_handoff, decode_runner_lease, decode_snapshot, encode_approval_audience,
    encode_brain_submission, encode_environment,
};
use crate::ipc::schema::finch_ipc_capnp::{
    self, brain_runner, brain_service, brain_wire_receiver, finch_daemon, stream_receiver,
};
use crate::ipc::transport::sock_path;
use crate::tools::types::{ToolDefinition, ToolUse};

pub struct BrainRunnerBootstrap {
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
}

pub struct BrainSubmissionResult {
    pub accepted: crate::brain::shared::BrainEvent,
    pub run: Option<crate::brain::shared::BrainRun>,
    pub result: Option<crate::brain::shared::BrainEvent>,
}

// ---------------------------------------------------------------------------
// Public client struct
// ---------------------------------------------------------------------------

/// Async client for the daemon IPC socket.
///
/// Must be created inside a `tokio::task::LocalSet` (or equivalent) because
/// `capnp-rpc` uses `spawn_local` internally.
#[derive(Clone)]
pub struct IpcClient {
    client: finch_daemon::Client,
    // Keeps the RPC system alive for the lifetime of this client.
    _rpc_handle: std::rc::Rc<tokio::task::JoinHandle<()>>,
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
            _rpc_handle: std::rc::Rc::new(handle),
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

    async fn brain_service(&self) -> Result<brain_service::Client> {
        let request = self.client.brain_service_request();
        let reply = request.send().promise.await?;
        Ok(reply.get()?.get_service()?)
    }

    pub async fn brain_snapshot(&self, brain: &str) -> Result<crate::brain::shared::BrainSnapshot> {
        let service = self.brain_service().await?;
        let mut request = service.snapshot_request();
        request.get().set_brain(brain);
        let reply = request.send().promise.await?;
        decode_snapshot(reply.get()?.get_snapshot()?)
    }

    pub async fn brain_inspect_run(
        &self,
        brain: &str,
        run_id: crate::brain::shared::RunId,
    ) -> Result<crate::brain::shared::BrainRun> {
        let service = self.brain_service().await?;
        let mut request = service.inspect_run_request();
        request.get().set_brain(brain);
        request.get().set_run_id(&run_id.0.to_string());
        let reply = request.send().promise.await?;
        decode_run(reply.get()?.get_run()?)
    }

    pub async fn brain_cancel_run(
        &self,
        brain: &str,
        attachment: &crate::brain::shared::BrainAttachment,
        run_id: crate::brain::shared::RunId,
    ) -> Result<crate::brain::shared::BrainRun> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.cancel_run_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_run_id(&run_id.0.to_string());
        }
        let reply = request.send().promise.await?;
        decode_run(reply.get()?.get_run()?)
    }

    pub async fn brain_attach(
        &self,
        brain: &str,
        subject: &str,
        role: crate::brain::shared::AttachmentRole,
        attachment_id: Option<crate::brain::shared::AttachmentId>,
    ) -> Result<crate::brain::shared::BrainAttachment> {
        let service = self.brain_service().await?;
        let mut request = service.attach_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_subject(subject);
            params.set_role(attachment_role_to_capnp(role));
            if let Some(attachment_id) = attachment_id {
                params.set_has_attachment_id(true);
                params.set_attachment_id(&attachment_id.0.to_string());
            }
        }
        let reply = request.send().promise.await?;
        decode_attachment(reply.get()?.get_attachment()?)
    }

    pub async fn brain_acknowledge(
        &self,
        brain: &str,
        attachment: &crate::brain::shared::BrainAttachment,
        seq: u64,
    ) -> Result<crate::brain::shared::BrainAttachment> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.acknowledge_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_seq(seq);
        }
        let reply = request.send().promise.await?;
        decode_attachment(reply.get()?.get_attachment()?)
    }

    pub async fn brain_detach(
        &self,
        brain: &str,
        attachment: &crate::brain::shared::BrainAttachment,
    ) -> Result<()> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.detach_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
        }
        request.send().promise.await?;
        Ok(())
    }

    pub async fn brain_submit(
        &self,
        brain: &str,
        attachment: &crate::brain::shared::BrainAttachment,
        kind: crate::brain::shared::BrainEventKind,
    ) -> Result<BrainSubmissionResult> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.submit_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            encode_brain_submission(params.init_submission(), &kind)?;
        }
        let reply = request.send().promise.await?;
        let outcome = reply.get()?.get_outcome()?;
        Ok(BrainSubmissionResult {
            accepted: decode_event(outcome.get_accepted()?)?,
            run: outcome
                .get_has_run()
                .then(|| outcome.get_run())
                .transpose()?
                .map(decode_run)
                .transpose()?,
            result: outcome
                .get_has_result()
                .then(|| outcome.get_result())
                .transpose()?
                .map(decode_event)
                .transpose()?,
        })
    }

    pub async fn brain_watch(
        &self,
        brain: &str,
        attachment: &crate::brain::shared::BrainAttachment,
    ) -> Result<mpsc::UnboundedReceiver<Result<crate::brain::shared::BrainWireMessage>>> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let (tx, rx) = mpsc::unbounded_channel();
        let receiver: brain_wire_receiver::Client =
            capnp_rpc::new_client(BrainWireReceiverImpl { tx: tx.clone() });
        let mut request = service.watch_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_receiver(receiver);
        }
        tokio::task::spawn_local(async move {
            if let Err(error) = request.send().promise.await {
                let _ = tx.send(Err(anyhow::Error::new(error)));
            }
        });
        Ok(rx)
    }

    pub async fn brain_acquire_runner(
        &self,
        brain: &str,
        subject: &str,
        environment: &crate::brain::shared::BrainEnvironment,
        lease_id: Option<crate::brain::shared::RunnerLeaseId>,
        ttl_ms: u64,
    ) -> Result<crate::brain::shared::BrainRunnerLease> {
        let service = self.brain_service().await?;
        let mut request = service.acquire_runner_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_subject(subject);
            encode_environment(params.reborrow().init_environment(), environment);
            if let Some(lease_id) = lease_id {
                params.set_has_lease_id(true);
                params.set_lease_id(&lease_id.0.to_string());
            }
            params.set_ttl_ms(ttl_ms);
        }
        let reply = request.send().promise.await?;
        decode_runner_lease(reply.get()?.get_lease()?)
    }

    pub async fn brain_release_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::shared::RunnerLeaseId,
    ) -> Result<()> {
        let service = self.brain_service().await?;
        let mut request = service.release_runner_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_lease_id(&lease_id.0.to_string());
        }
        request.send().promise.await?;
        Ok(())
    }

    pub async fn brain_request_runner_handoff(
        &self,
        brain: &str,
        requested_by: &str,
        target_subject: &str,
        expected_lease_id: crate::brain::shared::RunnerLeaseId,
        environment: &crate::brain::shared::BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<crate::brain::shared::BrainRunnerHandoff> {
        let service = self.brain_service().await?;
        let mut request = service.request_runner_handoff_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_requested_by(requested_by);
            params.set_target_subject(target_subject);
            params.set_expected_lease_id(&expected_lease_id.0.to_string());
            encode_environment(params.reborrow().init_environment(), environment);
            params.set_ttl_ms(ttl_ms);
        }
        let reply = request.send().promise.await?;
        decode_runner_handoff(reply.get()?.get_handoff()?)
    }

    pub async fn brain_accept_runner_handoff(
        &self,
        brain: &str,
        target_subject: &str,
        handoff_id: crate::brain::shared::RunnerHandoffId,
        environment: &crate::brain::shared::BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<crate::brain::shared::BrainRunnerLease> {
        let service = self.brain_service().await?;
        let mut request = service.accept_runner_handoff_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_target_subject(target_subject);
            params.set_handoff_id(&handoff_id.0.to_string());
            encode_environment(params.reborrow().init_environment(), environment);
            params.set_ttl_ms(ttl_ms);
        }
        let reply = request.send().promise.await?;
        decode_runner_lease(reply.get()?.get_lease()?)
    }

    pub async fn brain_cancel_runner_handoff(
        &self,
        brain: &str,
        handoff_id: crate::brain::shared::RunnerHandoffId,
        sender: &str,
    ) -> Result<()> {
        let service = self.brain_service().await?;
        let mut request = service.cancel_runner_handoff_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_handoff_id(&handoff_id.0.to_string());
            params.set_sender(sender);
        }
        request.send().promise.await?;
        Ok(())
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
        let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl { event_tx });
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
        let run_id = match request
            .get_run_id()
            .map_err(anyhow::Error::new)
            .and_then(|value| value.to_str().map_err(anyhow::Error::new))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::new))
        {
            Ok(run_id) => crate::brain::shared::RunId(run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
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
            .send(
                crate::cli::repl_event::ReplEvent::NamedBrainProgramRequested(
                    crate::server::RunnerProgramRequest {
                        brain,
                        run_id,
                        request_seq: request.get_request_seq(),
                        language,
                        source,
                        response_tx,
                    },
                ),
            )
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
        let run_id = match request
            .get_run_id()
            .map_err(anyhow::Error::new)
            .and_then(|value| value.to_str().map_err(anyhow::Error::new))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::new))
        {
            Ok(run_id) => crate::brain::shared::RunId(run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let context = match request
            .get_context_json()
            .map_err(anyhow::Error::new)
            .and_then(|value| serde_json::from_slice(value).map_err(anyhow::Error::new))
        {
            Ok(context) => context,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let approval_audience = match request
            .get_approval_audience()
            .map_err(anyhow::Error::new)
            .and_then(decode_approval_audience)
        {
            Ok(audience) => audience,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let control = match request.get_control() {
            Ok(control) => control,
            Err(error) => return Promise::err(error),
        };
        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::server::RunnerApprovalRequest>();
        tokio::task::spawn_local(async move {
            while let Some(request) = approval_rx.recv().await {
                let result = async {
                    let mut call = control.request_approval_request();
                    encode_brain_turn_event(call.get().init_event(), &request.event)?;
                    let response = call.send().promise.await?;
                    let decision = response.get()?.get_decision_json()?;
                    serde_json::from_slice(decision)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                }
                .await
                .map_err(|error| error.to_string());
                let _ = request.response_tx.send(result);
            }
        });
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self
            .event_tx
            .send(crate::cli::repl_event::ReplEvent::NamedBrainTurnRequested(
                crate::server::RunnerTurnRequest {
                    brain,
                    run_id,
                    request_seq: request.get_request_seq(),
                    prompt,
                    context,
                    approval_audience,
                    approval_tx: Some(approval_tx),
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
            let turn_events = match &response {
                Ok(response) => &response.turn_events,
                Err(error) => &error.turn_events,
            };
            encode_brain_turn_events(result.reborrow(), turn_events)?;
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
                    result.set_runtime_revision(response.runtime_revision);
                    let encoded = serde_json::to_vec(&response.checkpoint)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    result.set_checkpoint_json(&encoded);
                    result.set_error("");
                }
                Err(error) => result.set_error(&error.message),
            }
            Ok(())
        })
    }

    fn cancel_run(
        &mut self,
        params: brain_runner::CancelRunParams,
        mut results: brain_runner::CancelRunResults,
    ) -> Promise<(), capnp::Error> {
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let brain = params
            .get_brain()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let run_id = match params
            .get_run_id()
            .map_err(anyhow::Error::new)
            .and_then(|value| value.to_str().map_err(anyhow::Error::new))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::new))
        {
            Ok(run_id) => crate::brain::shared::RunId(run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self
            .event_tx
            .send(crate::cli::repl_event::ReplEvent::NamedBrainRunCancelRequested(
                crate::server::RunnerCancelRequest {
                    brain,
                    run_id,
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
                .map_err(|_| capnp::Error::failed("frontend dropped cancel response".into()))?;
            let mut result = results.get();
            match response {
                Ok(cancelled) => {
                    result.set_cancelled(cancelled);
                    result.set_error("");
                }
                Err(error) => {
                    result.set_cancelled(false);
                    result.set_error(&error);
                }
            }
            Ok(())
        })
    }
}

fn encode_brain_turn_events(
    mut result: finch_ipc_capnp::brain_turn_result::Builder<'_>,
    events: &[crate::server::RunnerTurnEvent],
) -> capnp::Result<()> {
    let mut turn_events = result.reborrow().init_turn_events(events.len() as u32);
    for (index, event) in events.iter().enumerate() {
        encode_brain_turn_event(turn_events.reborrow().get(index as u32), event)?;
    }
    Ok(())
}

fn encode_brain_turn_event(
    mut encoded: finch_ipc_capnp::brain_turn_event::Builder<'_>,
    event: &crate::server::RunnerTurnEvent,
) -> capnp::Result<()> {
    match event {
        crate::server::RunnerTurnEvent::Call {
            tool_id,
            name,
            input,
        } => {
            encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::Call);
            encoded.set_tool_id(tool_id);
            encoded.set_name(name);
            let input = serde_json::to_vec(input)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encoded.set_input_json(&input);
        }
        crate::server::RunnerTurnEvent::Result {
            tool_id,
            output,
            is_error,
        } => {
            encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::Result);
            encoded.set_tool_id(tool_id);
            encoded.set_output(output);
            encoded.set_is_error(*is_error);
        }
        crate::server::RunnerTurnEvent::ApprovalRequested {
            approval_id,
            approval_kind,
            subject,
            audience,
            detail,
        } => {
            encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested);
            encoded.set_approval_id(approval_id);
            encoded.set_approval_kind(approval_kind);
            encoded.set_subject(subject);
            encode_approval_audience(encoded.reborrow().init_approval_audience(), audience);
            let detail = serde_json::to_vec(detail)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encoded.set_detail_json(&detail);
        }
        crate::server::RunnerTurnEvent::ApprovalDecided {
            approval_id,
            decision,
        } => {
            encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            encoded.set_approval_id(approval_id);
            let decision = serde_json::to_vec(decision)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encoded.set_decision_json(&decision);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming receiver capability (client-side callback)
// ---------------------------------------------------------------------------

struct BrainWireReceiverImpl {
    tx: mpsc::UnboundedSender<Result<crate::brain::shared::BrainWireMessage>>,
}

impl brain_wire_receiver::Server for BrainWireReceiverImpl {
    fn on_message(
        &mut self,
        params: brain_wire_receiver::OnMessageParams,
        _results: brain_wire_receiver::OnMessageResults,
    ) -> Promise<(), capnp::Error> {
        let message = params
            .get()
            .and_then(|params| params.get_message())
            .map_err(anyhow::Error::from)
            .and_then(decode_brain_wire_reader);
        if self.tx.send(message).is_err() {
            return Promise::err(capnp::Error::disconnected(
                "Brain wire receiver was dropped".into(),
            ));
        }
        Promise::ok(())
    }
}

struct StreamReceiverImpl {
    tx: mpsc::UnboundedSender<Result<StreamChunk>>,
}

fn attachment_role_to_capnp(
    role: crate::brain::shared::AttachmentRole,
) -> finch_ipc_capnp::BrainAttachmentRole {
    match role {
        crate::brain::shared::AttachmentRole::Runner => {
            finch_ipc_capnp::BrainAttachmentRole::Runner
        }
        crate::brain::shared::AttachmentRole::Driver => {
            finch_ipc_capnp::BrainAttachmentRole::Driver
        }
        crate::brain::shared::AttachmentRole::Consultant => {
            finch_ipc_capnp::BrainAttachmentRole::Consultant
        }
        crate::brain::shared::AttachmentRole::Observer => {
            finch_ipc_capnp::BrainAttachmentRole::Observer
        }
    }
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

    /// Exercise the complete local named-Brain lifecycle capability against a
    /// running daemon. The fixed Brain name is intended to be archived after
    /// the smoke test; this test is ignored so ordinary unit suites remain
    /// hermetic.
    #[test]
    #[ignore]
    fn test_brain_service_lifecycle() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = IpcClient::connect()
                .await
                .expect("IPC connect — is `finch daemon` running?");
            let brain = "codex-ipc-smoke-20260824";
            let snapshot = client.brain_snapshot(brain).await.unwrap();
            let attachment = client
                .brain_attach(
                    brain,
                    "codex-smoke@localhost",
                    crate::brain::shared::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut incoming = client.brain_watch(brain, &attachment).await.unwrap();
            let initial = tokio::time::timeout(std::time::Duration::from_secs(2), incoming.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let crate::brain::shared::BrainWireMessage::Snapshot { brain: initial } = initial else {
                panic!("Brain watch did not begin with a snapshot");
            };
            assert_eq!(initial.brain_id, snapshot.brain_id);

            let outcome = client
                .brain_submit(
                    brain,
                    &attachment,
                    crate::brain::shared::BrainEventKind::Prompt {
                        text: "queue this smoke-test turn".into(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                outcome.run.as_ref().map(|run| run.status),
                Some(crate::brain::shared::BrainRunStatus::QueuedForEnvironment)
            );
            assert!(outcome.result.is_none());
            assert!(outcome.accepted.seq > initial.revision);
            let acknowledged = client
                .brain_acknowledge(brain, &attachment, outcome.accepted.seq)
                .await
                .unwrap();
            assert_eq!(acknowledged.acknowledged_seq, outcome.accepted.seq);
            client.brain_detach(brain, &acknowledged).await.unwrap();
        }));
    }
}
