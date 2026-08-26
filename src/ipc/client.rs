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
    decode_run, decode_runner_handoff, decode_runner_lease, decode_schedule, decode_snapshot,
    encode_approval_audience, encode_brain_submission, encode_environment,
};
use crate::ipc::checkpoint_codec::{decode_checkpoint, encode_checkpoint};
use crate::ipc::schema::finch_ipc_capnp::{
    self, brain_runner, brain_runner_control, brain_service, brain_wire_receiver, finch_daemon,
    stream_receiver,
};
use crate::ipc::transport::sock_path;
use crate::tools::types::{ToolDefinition, ToolUse};

pub struct BrainRunnerBootstrap {
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
    pub subagent_control:
        mpsc::UnboundedSender<crate::runtime::scheduler::AgentBrainControlRequest>,
}

pub struct BrainSubmissionResult {
    pub accepted: crate::brain::store::BrainEvent,
    pub run: Option<crate::brain::store::BrainRun>,
    pub result: Option<crate::brain::store::BrainEvent>,
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
    _rpc_handle: std::rc::Rc<RpcTask>,
}

struct RpcTask(tokio::task::JoinHandle<()>);

impl Drop for RpcTask {
    fn drop(&mut self) {
        // Dropping JoinHandle detaches rather than cancels. Abort when the
        // final IpcClient clone disappears so the daemon observes connection
        // loss and releases connection-scoped identities/callbacks promptly.
        self.0.abort();
    }
}

impl IpcClient {
    #[cfg(test)]
    pub(crate) fn from_test_client(client: finch_daemon::Client) -> Self {
        Self {
            client,
            _rpc_handle: std::rc::Rc::new(RpcTask(tokio::task::spawn_local(async {}))),
        }
    }

    /// Connect to the daemon's Unix socket.
    pub async fn connect() -> Result<Self> {
        Self::connect_path(sock_path()).await
    }

    /// Connect to an explicitly isolated daemon socket. This is used by the
    /// daemon-upgrade shadow preflight; ordinary clients always use `connect`.
    pub(crate) async fn connect_path(path: std::path::PathBuf) -> Result<Self> {
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

        let client = Self {
            client,
            _rpc_handle: std::rc::Rc::new(RpcTask(handle)),
        };
        client.verify_protocol_compatibility().await?;
        Ok(client)
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
            super::brain_codec::encode_messages(
                p.reborrow().init_messages(messages.len() as u32),
                &messages,
            )?;
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
            super::brain_codec::encode_messages(
                p.reborrow().init_messages(messages.len() as u32),
                &messages,
            )?;
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

    pub async fn brain_snapshot(&self, brain: &str) -> Result<crate::brain::store::BrainSnapshot> {
        let service = self.brain_service().await?;
        let mut request = service.snapshot_request();
        request.get().set_brain(brain);
        let reply = request.send().promise.await?;
        decode_snapshot(reply.get()?.get_snapshot()?)
    }

    pub async fn brain_inspect_run(
        &self,
        brain: &str,
        run_id: crate::brain::store::RunId,
    ) -> Result<crate::brain::store::BrainRun> {
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
        attachment: &crate::brain::store::BrainAttachment,
        run_id: crate::brain::store::RunId,
    ) -> Result<crate::brain::store::BrainRun> {
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

    #[allow(clippy::too_many_arguments)]
    pub async fn brain_create_schedule(
        &self,
        brain: &str,
        attachment: &crate::brain::store::BrainAttachment,
        language: crate::brain::store::ProgramLanguage,
        source: &str,
        grant_ceiling: &crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: &crate::brain::store::BrainScheduleDeliveryPolicy,
    ) -> Result<crate::brain::store::BrainSchedule> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.create_schedule_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_language(match language {
                crate::brain::store::ProgramLanguage::Forth => {
                    finch_ipc_capnp::ProgramLanguage::Forth
                }
                crate::brain::store::ProgramLanguage::Lisp => {
                    finch_ipc_capnp::ProgramLanguage::Lisp
                }
            });
            params.set_source(source);
            crate::ipc::checkpoint_codec::encode_effects(
                params
                    .reborrow()
                    .init_grant_ceiling(grant_ceiling.0.len() as u32),
                grant_ceiling,
            );
            params.set_next_due_ms(next_due_ms);
            if let Some(interval_ms) = interval_ms {
                params.set_has_interval_ms(true);
                params.set_interval_ms(interval_ms);
            }
            let mut policy = params.reborrow().init_policy();
            match delivery_policy {
                crate::brain::store::BrainScheduleDeliveryPolicy::Coalesce => {
                    policy.set_kind(finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce)
                }
                crate::brain::store::BrainScheduleDeliveryPolicy::BoundedCatchUp {
                    max_catch_up,
                    expires_after_ms,
                } => {
                    policy.set_kind(finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp);
                    policy.set_max_catch_up(*max_catch_up);
                    policy.set_expires_after_ms(*expires_after_ms);
                }
            }
        }
        let reply = request.send().promise.await?;
        decode_schedule(reply.get()?.get_schedule()?)
    }

    pub async fn brain_inspect_schedule(
        &self,
        brain: &str,
        schedule_id: crate::brain::store::ScheduleId,
    ) -> Result<Option<crate::brain::store::BrainSchedule>> {
        let service = self.brain_service().await?;
        let mut request = service.inspect_schedule_request();
        request.get().set_brain(brain);
        request.get().set_schedule_id(&schedule_id.0.to_string());
        let reply = request.send().promise.await?;
        let reply = reply.get()?;
        reply
            .get_found()
            .then(|| reply.get_schedule())
            .transpose()?
            .map(decode_schedule)
            .transpose()
    }

    pub async fn brain_cancel_schedule(
        &self,
        brain: &str,
        attachment: &crate::brain::store::BrainAttachment,
        schedule_id: crate::brain::store::ScheduleId,
    ) -> Result<bool> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.cancel_schedule_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_schedule_id(&schedule_id.0.to_string());
        }
        Ok(request.send().promise.await?.get()?.get_cancelled())
    }

    pub async fn brain_schedule_initialization(
        &self,
        brain: &str,
        attachment: &crate::brain::store::BrainAttachment,
        next_due_ms: u64,
    ) -> Result<crate::brain::store::BrainSchedule> {
        let connection_id = attachment
            .connection_id
            .context("Brain attachment has no live connection")?;
        let service = self.brain_service().await?;
        let mut request = service.schedule_initialization_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_attachment_id(&attachment.attachment_id.0.to_string());
            params.set_connection_id(&connection_id.0.to_string());
            params.set_next_due_ms(next_due_ms);
        }
        let reply = request.send().promise.await?;
        decode_schedule(reply.get()?.get_schedule()?)
    }

    pub async fn brain_attach(
        &self,
        brain: &str,
        subject: &str,
        role: crate::brain::store::AttachmentRole,
        attachment_id: Option<crate::brain::store::AttachmentId>,
    ) -> Result<crate::brain::store::BrainAttachment> {
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
        attachment: &crate::brain::store::BrainAttachment,
        seq: u64,
    ) -> Result<crate::brain::store::BrainAttachment> {
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
        attachment: &crate::brain::store::BrainAttachment,
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
        attachment: &crate::brain::store::BrainAttachment,
        kind: crate::brain::store::BrainEventKind,
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

    pub async fn brain_start_speculative(
        &self,
        brain: &str,
        attachment: &crate::brain::store::BrainAttachment,
        prompt: String,
    ) -> Result<crate::brain::store::BrainRun> {
        self.brain_submit(
            brain,
            attachment,
            crate::brain::store::BrainEventKind::SpeculativePrompt { text: prompt },
        )
        .await?
        .run
        .context("speculative Brain submission did not create a run")
    }

    pub async fn brain_watch(
        &self,
        brain: &str,
        attachment: &crate::brain::store::BrainAttachment,
    ) -> Result<mpsc::UnboundedReceiver<Result<crate::brain::store::BrainWireMessage>>> {
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
        environment: &crate::brain::store::BrainEnvironment,
        lease_id: Option<crate::brain::store::RunnerLeaseId>,
        ttl_ms: u64,
    ) -> Result<crate::brain::store::BrainRunnerLease> {
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

    pub async fn brain_claim_runner_identity(&self, subject: &str) -> Result<()> {
        let service = self.brain_service().await?;
        let mut request = service.claim_runner_identity_request();
        request.get().set_subject(subject);
        request.send().promise.await?;
        Ok(())
    }

    pub async fn brain_release_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::store::RunnerLeaseId,
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
        expected_lease_id: crate::brain::store::RunnerLeaseId,
        environment: &crate::brain::store::BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<crate::brain::store::BrainRunnerHandoff> {
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
        handoff_id: crate::brain::store::RunnerHandoffId,
        environment: &crate::brain::store::BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<crate::brain::store::BrainRunnerLease> {
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
        handoff_id: crate::brain::store::RunnerHandoffId,
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
        let response = reply.get()?;
        ensure_compatible_protocol(response.get_protocol_version())?;
        Ok(response.get_version()?.to_str()?.to_string())
    }

    async fn verify_protocol_compatibility(&self) -> Result<()> {
        let req = self.client.ping_request();
        let reply = req.send().promise.await?;
        ensure_compatible_protocol(reply.get()?.get_protocol_version())
    }

    /// Register this frontend as the callback for its current named-Brain
    /// runner lease. The callback stays on this connection's LocalSet.
    pub async fn register_brain_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::store::RunnerLeaseId,
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
        let control: brain_runner_control::Client = response.get_control()?;
        let (subagent_control, mut subagent_rx) = mpsc::unbounded_channel::<
            crate::runtime::scheduler::AgentBrainControlRequest,
        >();
        tokio::task::spawn_local(async move {
            while let Some(request) = subagent_rx.recv().await {
                match request {
                    crate::runtime::scheduler::AgentBrainControlRequest::Start {
                        parent_run_id,
                        task_id,
                        detail,
                        response_tx,
                    } => {
                        let result = async {
                            let mut call = control.start_subagent_request();
                            {
                                let mut params = call.get();
                                params.set_parent_run_id(&parent_run_id.0.to_string());
                                params.set_task_id(&task_id.to_string());
                                params.set_detail(&detail);
                            }
                            let reply = call.send().promise.await?;
                            decode_run(reply.get()?.get_run()?)
                                .map_err(|error| capnp::Error::failed(error.to_string()))
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                    crate::runtime::scheduler::AgentBrainControlRequest::Finish {
                        run_id,
                        status,
                        detail,
                        response_tx,
                    } => {
                        let result = async {
                            let mut call = control.finish_subagent_request();
                            {
                                let mut params = call.get();
                                params.set_run_id(&run_id.0.to_string());
                                params.set_status(
                                    crate::ipc::brain_codec::run_status_to_capnp(status),
                                );
                                params.set_detail(&detail);
                            }
                            let reply = call.send().promise.await?;
                            decode_run(reply.get()?.get_run()?)
                                .map_err(|error| capnp::Error::failed(error.to_string()))
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                }
            }
        });
        Ok(BrainRunnerBootstrap {
            runtime_revision: response.get_runtime_revision(),
            checkpoint: decode_checkpoint(response.get_checkpoint()?)
                .context("daemon returned an invalid named-Brain checkpoint")?,
            subagent_control,
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

fn ensure_compatible_protocol(protocol_version: u32) -> Result<()> {
    anyhow::ensure!(
        protocol_version == crate::ipc::IPC_PROTOCOL_VERSION,
        "the running Finch daemon uses IPC protocol {protocol_version}, but this frontend requires {}; restart the daemon with the rebuilt Finch binary",
        crate::ipc::IPC_PROTOCOL_VERSION,
    );
    Ok(())
}

struct BrainRunnerImpl {
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>,
}

struct BrainTurnCommitAckImpl {
    tx: tokio::sync::mpsc::UnboundedSender<crate::server::RunnerTurnCommitNotice>,
}

impl finch_ipc_capnp::brain_turn_commit_ack::Server for BrainTurnCommitAckImpl {
    fn committed(
        &mut self,
        params: finch_ipc_capnp::brain_turn_commit_ack::CommittedParams,
        _results: finch_ipc_capnp::brain_turn_commit_ack::CommittedResults,
    ) -> Promise<(), capnp::Error> {
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let status = match params.get_status() {
            Ok(status) => crate::ipc::brain_codec::run_status_from_capnp(status),
            Err(error) => return Promise::err(error.into()),
        };
        let detail = params
            .get_detail()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        if self
            .tx
            .send(crate::server::RunnerTurnCommitNotice { status, detail })
            .is_err()
        {
            return Promise::err(capnp::Error::failed(
                "frontend commit acknowledgement receiver disconnected".into(),
            ));
        }
        Promise::ok(())
    }
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
            Ok(run_id) => crate::brain::store::RunId(run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let language = match request.get_language() {
            Ok(finch_ipc_capnp::ProgramLanguage::Forth) => {
                crate::brain::store::ProgramLanguage::Forth
            }
            Ok(finch_ipc_capnp::ProgramLanguage::Lisp) => {
                crate::brain::store::ProgramLanguage::Lisp
            }
            Err(error) => return Promise::err(error.into()),
        };
        let control = match request.get_control() {
            Ok(control) => control,
            Err(error) => return Promise::err(error),
        };
        let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::server::RunnerProgramControlRequest,
        >();
        tokio::task::spawn_local(async move {
            while let Some(request) = control_rx.recv().await {
                match request {
                    crate::server::RunnerProgramControlRequest::CreateSchedule {
                        language,
                        source,
                        grant_ceiling,
                        next_due_ms,
                        interval_ms,
                        delivery_policy,
                        response_tx,
                    } => {
                        let result = async {
                            let mut call = control.create_schedule_request();
                            {
                                let mut params = call.get();
                                params.set_language(match language {
                                    crate::brain::store::ProgramLanguage::Forth => {
                                        finch_ipc_capnp::ProgramLanguage::Forth
                                    }
                                    crate::brain::store::ProgramLanguage::Lisp => {
                                        finch_ipc_capnp::ProgramLanguage::Lisp
                                    }
                                });
                                params.set_source(&source);
                                crate::ipc::checkpoint_codec::encode_effects(
                                    params.reborrow().init_grant_ceiling(
                                        grant_ceiling.0.len() as u32,
                                    ),
                                    &grant_ceiling,
                                );
                                params.set_next_due_ms(next_due_ms);
                                if let Some(interval_ms) = interval_ms {
                                    params.set_has_interval_ms(true);
                                    params.set_interval_ms(interval_ms);
                                }
                                let mut policy = params.reborrow().init_policy();
                                match delivery_policy {
                                    crate::brain::store::BrainScheduleDeliveryPolicy::Coalesce => {
                                        policy.set_kind(
                                            finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce,
                                        );
                                    }
                                    crate::brain::store::BrainScheduleDeliveryPolicy::BoundedCatchUp {
                                        max_catch_up,
                                        expires_after_ms,
                                    } => {
                                        policy.set_kind(
                                            finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp,
                                        );
                                        policy.set_max_catch_up(max_catch_up);
                                        policy.set_expires_after_ms(expires_after_ms);
                                    }
                                }
                            }
                            let reply = call.send().promise.await?;
                            crate::ipc::brain_codec::decode_schedule(
                                reply.get()?.get_schedule()?,
                            )
                            .map_err(|error| capnp::Error::failed(error.to_string()))
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                    crate::server::RunnerProgramControlRequest::InspectSchedule {
                        schedule_id,
                        response_tx,
                    } => {
                        let result = async {
                            let mut call = control.inspect_schedule_request();
                            call.get().set_schedule_id(&schedule_id.0.to_string());
                            let reply = call.send().promise.await?;
                            let reply = reply.get()?;
                            reply
                                .get_found()
                                .then(|| {
                                    reply
                                        .get_schedule()
                                        .map_err(anyhow::Error::from)
                                        .and_then(crate::ipc::brain_codec::decode_schedule)
                                })
                                .transpose()
                                .map_err(|error| capnp::Error::failed(error.to_string()))
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                    crate::server::RunnerProgramControlRequest::CancelSchedule {
                        schedule_id,
                        response_tx,
                    } => {
                        let result = async {
                            let mut call = control.cancel_schedule_request();
                            call.get().set_schedule_id(&schedule_id.0.to_string());
                            Ok::<_, capnp::Error>(
                                call.send().promise.await?.get()?.get_cancelled(),
                            )
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                }
            }
        });
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
                        interaction: match request.get_interaction() {
                            Ok(finch_ipc_capnp::BrainProgramInteraction::Interactive) => {
                                crate::server::RunnerProgramInteraction::Interactive
                            }
                            Ok(finch_ipc_capnp::BrainProgramInteraction::Noninteractive) => {
                                crate::server::RunnerProgramInteraction::Noninteractive
                            }
                            Err(error) => return Promise::err(error.into()),
                        },
                        grant_ceiling: if request.get_has_grant_ceiling() {
                            match request
                                .get_grant_ceiling()
                                .map_err(anyhow::Error::new)
                                .and_then(crate::ipc::checkpoint_codec::decode_effects)
                            {
                                Ok(grants) => Some(grants),
                                Err(error) => {
                                    return Promise::err(capnp::Error::failed(error.to_string()))
                                }
                            }
                        } else {
                            None
                        },
                        control_tx: Some(control_tx),
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
            let effect_journal = match &response {
                Ok(response) => &response.effect_journal,
                Err(error) => &error.effect_journal,
            };
            encode_runner_effect_records(
                result
                    .reborrow()
                    .init_effect_journal(effect_journal.len() as u32),
                effect_journal,
            )?;
            match response {
                Ok(response) => {
                    result.set_output(&response.output);
                    result.set_runtime_revision(response.runtime_revision);
                    encode_checkpoint(result.reborrow().init_checkpoint(), &response.checkpoint)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    result.set_error("");
                }
                Err(error) => result.set_error(&error.message),
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
            Ok(run_id) => crate::brain::store::RunId(run_id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let context = match request
            .get_context()
            .map_err(anyhow::Error::new)
            .and_then(super::brain_codec::decode_messages)
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
                    super::brain_codec::decode_json_value(response.get()?.get_decision()?)
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
                    approval_connection_id: None,
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
            let effect_journal = match &response {
                Ok(response) => &response.effect_journal,
                Err(error) => &error.effect_journal,
            };
            encode_runner_effect_records(
                result
                    .reborrow()
                    .init_effect_journal(effect_journal.len() as u32),
                effect_journal,
            )?;
            match response {
                Ok(response) => {
                    result.set_source(&response.source);
                    result.set_language(match response.language {
                        crate::brain::store::ProgramLanguage::Forth => {
                            finch_ipc_capnp::ProgramLanguage::Forth
                        }
                        crate::brain::store::ProgramLanguage::Lisp => {
                            finch_ipc_capnp::ProgramLanguage::Lisp
                        }
                    });
                    result.set_output(&response.output);
                    result.set_runtime_revision(response.runtime_revision);
                    encode_checkpoint(result.reborrow().init_checkpoint(), &response.checkpoint)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    result.set_has_commit_ack(response.commit_ack.is_some());
                    if let Some(commit_ack) = response.commit_ack {
                        let client: finch_ipc_capnp::brain_turn_commit_ack::Client =
                            capnp_rpc::new_client(BrainTurnCommitAckImpl {
                                tx: commit_ack.tx().clone(),
                            });
                        result.set_commit_ack(client);
                    }
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
            Ok(run_id) => crate::brain::store::RunId(run_id),
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

    fn project_memory(
        &mut self,
        params: brain_runner::ProjectMemoryParams,
        mut results: brain_runner::ProjectMemoryResults,
    ) -> Promise<(), capnp::Error> {
        let request = match params.get().and_then(|params| params.get_request()) {
            Ok(request) => request,
            Err(error) => return Promise::err(error),
        };
        let parse_uuid = |value: capnp::text::Reader<'_>| {
            value
                .to_str()
                .map_err(anyhow::Error::new)
                .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::new))
        };
        let brain_id = match request.get_brain_id().map_err(anyhow::Error::new).and_then(parse_uuid)
        {
            Ok(value) => crate::brain::store::BrainId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let run_id = match request.get_run_id().map_err(anyhow::Error::new).and_then(parse_uuid) {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
            value
                .ok()
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        if self
            .event_tx
            .send(
                crate::cli::repl_event::ReplEvent::NamedBrainMemoryProjectionRequested(
                    crate::server::RunnerMemoryProjectionRequest {
                        brain_id,
                        brain: text(request.get_brain()),
                        run_id,
                        request_seq: request.get_request_seq(),
                        prompt: text(request.get_prompt()),
                        source: text(request.get_source()),
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
                .map_err(|_| capnp::Error::failed("frontend dropped memory response".into()))?;
            let mut result = results.get();
            match response {
                Ok(inserted) => {
                    result.set_inserted(inserted.try_into().unwrap_or(u32::MAX));
                    result.set_error("");
                }
                Err(error) => result.set_error(&error),
            }
            Ok(())
        })
    }
}

fn encode_runner_effect_records(
    mut encoded: capnp::struct_list::Builder<'_, finch_ipc_capnp::brain_effect_record::Owned>,
    records: &[crate::server::RunnerEffectRecord],
) -> capnp::Result<()> {
    for (index, record) in records.iter().enumerate() {
        crate::ipc::checkpoint_codec::encode_effect_record(
            encoded.reborrow().get(index as u32),
            record.execution_id,
            &record.entry,
        )
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
    }
    Ok(())
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
            super::brain_codec::encode_json_value(encoded.reborrow().init_input(), input)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
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
            super::brain_codec::encode_json_value(encoded.reborrow().init_detail(), detail)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
        }
        crate::server::RunnerTurnEvent::ApprovalDecided {
            approval_id,
            decision,
        } => {
            encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            encoded.set_approval_id(approval_id);
            super::brain_codec::encode_json_value(encoded.reborrow().init_decision(), decision)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Streaming receiver capability (client-side callback)
// ---------------------------------------------------------------------------

struct BrainWireReceiverImpl {
    tx: mpsc::UnboundedSender<Result<crate::brain::store::BrainWireMessage>>,
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
    role: crate::brain::store::AttachmentRole,
) -> finch_ipc_capnp::BrainAttachmentRole {
    match role {
        crate::brain::store::AttachmentRole::Runner => {
            finch_ipc_capnp::BrainAttachmentRole::Runner
        }
        crate::brain::store::AttachmentRole::Driver => {
            finch_ipc_capnp::BrainAttachmentRole::Driver
        }
        crate::brain::store::AttachmentRole::Consultant => {
            finch_ipc_capnp::BrainAttachmentRole::Consultant
        }
        crate::brain::store::AttachmentRole::Observer => {
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
                    let input = super::brain_codec::decode_json_value(tu.get_input()?)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
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
        let input = super::brain_codec::decode_json_value(tu.get_input()?)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
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
    fn ipc_protocol_handshake_accepts_only_the_current_generation() {
        ensure_compatible_protocol(crate::ipc::IPC_PROTOCOL_VERSION).unwrap();

        let error = ensure_compatible_protocol(0).unwrap_err().to_string();
        assert!(error.contains("restart the daemon"));
        assert!(error.contains("protocol 0"));
    }

    struct BlockingBrainRunner {
        started: std::cell::RefCell<
            Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>,
        >,
        cancellations: std::rc::Rc<
            std::cell::RefCell<
                std::collections::HashMap<
                    crate::brain::store::RunId,
                    tokio::sync::oneshot::Sender<()>,
                >,
            >,
        >,
    }

    impl brain_runner::Server for BlockingBrainRunner {
        fn run_program(
            &mut self,
            params: brain_runner::RunProgramParams,
            mut results: brain_runner::RunProgramResults,
        ) -> Promise<(), capnp::Error> {
            let request = match params.get().and_then(|params| params.get_request()) {
                Ok(request) => request,
                Err(error) => return Promise::err(error),
            };
            let run_id = match request
                .get_run_id()
                .ok()
                .and_then(|value| value.to_str().ok())
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
            {
                Some(run_id) => crate::brain::store::RunId(run_id),
                None => return Promise::err(capnp::Error::failed("invalid run id".into())),
            };
            let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
            self.cancellations.borrow_mut().insert(run_id, cancel_tx);
            if let Some(started) = self.started.borrow_mut().take() {
                let _ = started.send(run_id);
            }
            Promise::from_future(async move {
                let _ = cancel_rx.await;
                let mut result = results.get().init_result();
                result.set_error("named Brain run cancelled");
                Ok(())
            })
        }

        fn run_turn(
            &mut self,
            _params: brain_runner::RunTurnParams,
            _results: brain_runner::RunTurnResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented(
                "blocking smoke runner only accepts programs".into(),
            ))
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
            let run_id = params
                .get_run_id()
                .ok()
                .and_then(|value| value.to_str().ok())
                .and_then(|value| uuid::Uuid::parse_str(value).ok())
                .map(crate::brain::store::RunId);
            let cancelled = run_id
                .and_then(|run_id| self.cancellations.borrow_mut().remove(&run_id))
                .is_some_and(|cancel| cancel.send(()).is_ok());
            results.get().set_cancelled(cancelled);
            results.get().set_error("");
            Promise::ok(())
        }
    }

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

    /// A newly spawned daemon from the current binary must establish both its
    /// event watch and reverse runner callback without a manual restart.
    #[test]
    #[ignore]
    fn test_fresh_daemon_brain_bootstrap_reaches_live_runner() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = IpcClient::connect()
                .await
                .expect("IPC connect — start the rebuilt Finch daemon first");
            let brain = format!("bootstrap-smoke-{}", uuid::Uuid::new_v4().simple());
            let snapshot = client.brain_snapshot(&brain).await.unwrap();
            let subject = format!("smoke@localhost/frontend-{}", uuid::Uuid::new_v4());
            client.brain_claim_runner_identity(&subject).await.unwrap();
            let lease = client
                .brain_acquire_runner(
                    &brain,
                    &subject,
                    &snapshot.environment,
                    None,
                    30_000,
                )
                .await
                .unwrap();
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let _bootstrap = client
                .register_brain_runner(&brain, lease.lease_id, event_tx)
                .await
                .expect("fresh daemon must accept the current runner callback schema");

            let attachment = client
                .brain_attach(
                    &brain,
                    "smoke@localhost",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut events = client.brain_watch(&brain, &attachment).await.unwrap();
            let initial = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .expect("fresh daemon event watch timed out")
                .expect("fresh daemon event watch closed")
                .expect("fresh daemon event watch failed");
            let crate::brain::store::BrainWireMessage::Snapshot { brain: watched } = initial else {
                panic!("fresh daemon watch did not start with a snapshot");
            };
            assert_eq!(watched.brain_id, snapshot.brain_id);
            assert_eq!(
                watched.runner_lease.as_ref().map(|runner| runner.lease_id),
                Some(lease.lease_id)
            );

            let attachment = client
                .brain_acknowledge(&brain, &attachment, watched.revision)
                .await
                .unwrap();
            let attachment_id = attachment.attachment_id;
            drop(events);
            drop(_bootstrap);
            drop(client);

            let replacement = IpcClient::connect().await.unwrap();
            replacement
                .brain_claim_runner_identity(&subject)
                .await
                .expect("replacement IPC connection did not reclaim runner identity");
            let renewed = replacement
                .brain_acquire_runner(
                    &brain,
                    &subject,
                    &snapshot.environment,
                    Some(lease.lease_id),
                    30_000,
                )
                .await
                .expect("replacement IPC connection did not renew durable runner lease");
            let (replacement_tx, _replacement_rx) = tokio::sync::mpsc::unbounded_channel();
            let _replacement_bootstrap = replacement
                .register_brain_runner(&brain, renewed.lease_id, replacement_tx)
                .await
                .expect("replacement IPC connection did not restore runner callback");
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
            let attachment = loop {
                match replacement
                    .brain_attach(
                        &brain,
                        "smoke@localhost",
                        crate::brain::store::AttachmentRole::Driver,
                        Some(attachment_id),
                    )
                    .await
                {
                    Ok(attachment) => break attachment,
                    Err(error)
                        if error
                            .to_string()
                            .contains("already has a live or pending connection")
                            && tokio::time::Instant::now() < deadline =>
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                    Err(error) => panic!(
                        "replacement IPC connection did not restore durable attachment: {error}"
                    ),
                }
            };
            let mut events = replacement.brain_watch(&brain, &attachment).await.unwrap();
            let resumed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .expect("replacement event watch timed out")
                .expect("replacement event watch closed")
                .expect("replacement event watch failed");
            let crate::brain::store::BrainWireMessage::Snapshot { brain: resumed } = resumed else {
                panic!("replacement watch did not start with a snapshot");
            };
            assert_eq!(resumed.brain_id, snapshot.brain_id);
            assert!(
                attachment.acknowledged_seq >= watched.revision,
                "durable attachment cursor moved backward"
            );

            replacement.brain_detach(&brain, &attachment).await.unwrap();
            replacement
                .brain_release_runner(&brain, renewed.lease_id)
                .await
                .unwrap();
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
                    crate::brain::store::AttachmentRole::Driver,
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
            let crate::brain::store::BrainWireMessage::Snapshot { brain: initial } = initial else {
                panic!("Brain watch did not begin with a snapshot");
            };
            assert_eq!(initial.brain_id, snapshot.brain_id);

            let relay = client
                .brain_submit(
                    brain,
                    &attachment,
                    crate::brain::store::BrainEventKind::ParticipantMessage {
                        text: "human-only collaboration message".into(),
                    },
                )
                .await
                .unwrap();
            assert!(relay.run.is_none());
            assert!(relay.result.is_none());
            let relayed = tokio::time::timeout(std::time::Duration::from_secs(2), incoming.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(
                relayed,
                crate::brain::store::BrainWireMessage::Event {
                    event: crate::brain::store::BrainEvent {
                        kind: crate::brain::store::BrainEventKind::ParticipantMessage { ref text },
                        ..
                    }
                } if text == "human-only collaboration message"
            ));

            let outcome = client
                .brain_submit(
                    brain,
                    &attachment,
                    crate::brain::store::BrainEventKind::Prompt {
                        text: "queue this smoke-test turn".into(),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                outcome.run.as_ref().map(|run| run.status),
                Some(crate::brain::store::BrainRunStatus::QueuedForEnvironment)
            );
            assert!(outcome.result.is_none());
            assert!(outcome.accepted.seq > relay.accepted.seq);
            let run = outcome.run.unwrap();
            let inspected = client.brain_inspect_run(brain, run.run_id).await.unwrap();
            assert_eq!(inspected.run_id, run.run_id);
            let cancelled = client
                .brain_cancel_run(brain, &attachment, run.run_id)
                .await
                .unwrap();
            assert_eq!(
                cancelled.status,
                crate::brain::store::BrainRunStatus::Cancelled
            );
            let acknowledged = client
                .brain_acknowledge(brain, &attachment, outcome.accepted.seq)
                .await
                .unwrap();
            assert_eq!(acknowledged.acknowledged_seq, outcome.accepted.seq);
            client.brain_detach(brain, &acknowledged).await.unwrap();
        }));
    }

    /// Prove that cancellation overtakes an active runner RPC on the live
    /// Cap'n Proto connection and reaches the exact run before it completes.
    #[test]
    #[ignore]
    fn test_brain_running_cancellation() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = IpcClient::connect()
                .await
                .expect("IPC connect — is `finch daemon` running?");
            let brain = format!("codex-cancel-{}", &uuid::Uuid::new_v4().to_string()[..8]);
            let snapshot = client.brain_snapshot(&brain).await.unwrap();
            let attachment = client
                .brain_attach(
                    &brain,
                    "codex-cancel@localhost",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let _incoming = client.brain_watch(&brain, &attachment).await.unwrap();
            client
                .brain_claim_runner_identity("codex-blocking-runner@localhost")
                .await
                .unwrap();
            let lease = client
                .brain_acquire_runner(
                    &brain,
                    "codex-blocking-runner@localhost",
                    &snapshot.environment,
                    None,
                    60_000,
                )
                .await
                .unwrap();
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let runner: brain_runner::Client = capnp_rpc::new_client(BlockingBrainRunner {
                started: std::cell::RefCell::new(Some(started_tx)),
                cancellations: std::rc::Rc::new(std::cell::RefCell::new(
                    std::collections::HashMap::new(),
                )),
            });
            let mut registration = client.client.register_brain_runner_request();
            registration.get().set_brain(&brain);
            registration
                .get()
                .set_lease_id(&lease.lease_id.0.to_string());
            registration.get().set_runner(runner);
            registration.send().promise.await.unwrap();

            let submit_client = client.clone();
            let submit_brain = brain.clone();
            let submit_attachment = attachment.clone();
            let submission = tokio::task::spawn_local(async move {
                submit_client
                    .brain_submit(
                        &submit_brain,
                        &submit_attachment,
                        crate::brain::store::BrainEventKind::Program {
                            language: crate::brain::store::ProgramLanguage::Lisp,
                            source: "(say \"this must not complete\")".into(),
                        },
                    )
                    .await
            });
            let run_id = tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
                .await
                .unwrap()
                .unwrap();
            let cancelled = client
                .brain_cancel_run(&brain, &attachment, run_id)
                .await
                .unwrap();
            assert_eq!(
                cancelled.status,
                crate::brain::store::BrainRunStatus::Cancelled
            );
            let outcome = submission.await.unwrap().unwrap();
            assert_eq!(outcome.run.unwrap().run_id, run_id);
            assert_eq!(
                client.brain_inspect_run(&brain, run_id).await.unwrap().status,
                crate::brain::store::BrainRunStatus::Cancelled
            );
            client.brain_release_runner(&brain, lease.lease_id).await.unwrap();
            client.brain_detach(&brain, &attachment).await.unwrap();
        }));
    }

    #[test]
    #[ignore = "requires a running Finch daemon on the default local socket"]
    fn test_brain_runner_lease_cannot_be_hijacked_by_another_ipc_connection() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let owner = IpcClient::connect().await.unwrap();
            let intruder = IpcClient::connect().await.unwrap();
            let brain = format!("codex-runner-authority-{}", uuid::Uuid::new_v4());
            let subject = "owner/frontend-authority";
            let snapshot = owner.brain_snapshot(&brain).await.unwrap();

            owner.brain_claim_runner_identity(subject).await.unwrap();
            assert!(intruder.brain_claim_runner_identity(subject).await.is_err());
            let lease = owner
                .brain_acquire_runner(
                    &brain,
                    subject,
                    &snapshot.environment,
                    None,
                    60_000,
                )
                .await
                .unwrap();

            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            assert!(intruder
                .register_brain_runner(&brain, lease.lease_id, event_tx)
                .await
                .is_err());
            assert!(intruder
                .brain_acquire_runner(
                    &brain,
                    subject,
                    &snapshot.environment,
                    Some(lease.lease_id),
                    60_000,
                )
                .await
                .is_err());
            assert!(intruder
                .brain_release_runner(&brain, lease.lease_id)
                .await
                .is_err());

            owner
                .brain_release_runner(&brain, lease.lease_id)
                .await
                .unwrap();
        }));
    }

    #[test]
    #[ignore = "requires a running Finch daemon on the default local socket"]
    fn test_brain_attachment_cannot_be_replayed_by_another_ipc_connection() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let owner = IpcClient::connect().await.unwrap();
            let intruder = IpcClient::connect().await.unwrap();
            let brain = format!("codex-attachment-authority-{}", uuid::Uuid::new_v4());
            let attachment = owner
                .brain_attach(
                    &brain,
                    "owner/attachment-authority",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut owner_watch = owner.brain_watch(&brain, &attachment).await.unwrap();
            let initial = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                owner_watch.recv(),
            )
            .await
            .unwrap()
            .unwrap()
            .unwrap();
            assert!(matches!(
                initial,
                crate::brain::store::BrainWireMessage::Snapshot { .. }
            ));

            assert!(intruder
                .brain_submit(
                    &brain,
                    &attachment,
                    crate::brain::store::BrainEventKind::ParticipantMessage {
                        text: "forged message".into(),
                    },
                )
                .await
                .is_err());
            assert!(intruder
                .brain_acknowledge(&brain, &attachment, 0)
                .await
                .is_err());
            assert!(intruder.brain_detach(&brain, &attachment).await.is_err());
            let mut forged_watch = intruder.brain_watch(&brain, &attachment).await.unwrap();
            let watch_error = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                forged_watch.recv(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(watch_error.is_err());

            let accepted = owner
                .brain_submit(
                    &brain,
                    &attachment,
                    crate::brain::store::BrainEventKind::ParticipantMessage {
                        text: "owner message".into(),
                    },
                )
                .await
                .unwrap();
            owner
                .brain_acknowledge(&brain, &attachment, accepted.accepted.seq)
                .await
                .unwrap();
            owner.brain_detach(&brain, &attachment).await.unwrap();
        }));
    }
}
