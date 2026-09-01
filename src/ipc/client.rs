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

pub(crate) struct RunnerProcessIdentity {
    pub(crate) epoch: uuid::Uuid,
    pub(crate) poisoned: std::sync::atomic::AtomicBool,
    /// Linearizes explicit process ejection with every runner-scoped
    /// Cap'n Proto send. The guard is held only through the non-awaiting
    /// `send()` call, never while its promise is awaited.
    runner_rpc_admission: std::sync::Mutex<()>,
    #[cfg(test)]
    runner_rpc_admission_pause: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

fn new_runner_process_identity(epoch: uuid::Uuid) -> std::sync::Arc<RunnerProcessIdentity> {
    std::sync::Arc::new(RunnerProcessIdentity {
        epoch,
        poisoned: std::sync::atomic::AtomicBool::new(false),
        runner_rpc_admission: std::sync::Mutex::new(()),
        #[cfg(test)]
        runner_rpc_admission_pause: std::sync::Mutex::new(None),
    })
}

impl RunnerProcessIdentity {
    fn with_rpc_admission<T>(&self, send: impl FnOnce() -> T) -> Result<T> {
        let Ok(_admission) = self.runner_rpc_admission.lock() else {
            // A panic while this process was admitting a runner RPC leaves
            // its send ordering unknowable. Make that uncertainty monotonic
            // and fail closed instead of panicking through the reconnect
            // supervisor or permitting another lease mutation.
            self.poisoned
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(anyhow::Error::new(RunnerProcessQuarantined));
        };
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            return Err(anyhow::Error::new(RunnerProcessQuarantined));
        }
        Ok(send())
    }

    /// Publish a terminal server rejection in the same total order as every
    /// runner send. A send already holding admission completes before this
    /// store; every later sender observes poison and fails locally.
    fn publish_remote_poison(&self) {
        self.publish_remote_poison_after_admission(|| {});
    }

    fn publish_remote_poison_after_admission(&self, admitted: impl FnOnce()) {
        let _admission = self
            .runner_rpc_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        admitted();
        self.poisoned
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

fn runner_process_identity() -> std::sync::Arc<RunnerProcessIdentity> {
    static IDENTITY: std::sync::OnceLock<std::sync::Arc<RunnerProcessIdentity>> =
        std::sync::OnceLock::new();
    std::sync::Arc::clone(
        IDENTITY.get_or_init(|| new_runner_process_identity(uuid::Uuid::new_v4())),
    )
}

#[cfg(test)]
fn fresh_runner_process_identity() -> std::sync::Arc<RunnerProcessIdentity> {
    new_runner_process_identity(uuid::Uuid::new_v4())
}

struct RpcConnectionState {
    process: std::sync::Arc<RunnerProcessIdentity>,
    active_runner_registrations: std::sync::atomic::AtomicUsize,
}

struct RpcTask {
    handle: tokio::task::JoinHandle<()>,
    state: std::sync::Arc<RpcConnectionState>,
}

impl Drop for RpcTask {
    fn drop(&mut self) {
        // Dropping JoinHandle detaches rather than cancels. Abort when the
        // final IpcClient clone disappears so the daemon observes connection
        // loss and releases connection-scoped identities/callbacks promptly.
        self.handle.abort();
    }
}

impl IpcClient {
    #[cfg(test)]
    pub(crate) fn from_test_client(client: finch_daemon::Client) -> Self {
        let state = std::sync::Arc::new(RpcConnectionState {
            process: new_runner_process_identity(uuid::Uuid::new_v4()),
            active_runner_registrations: std::sync::atomic::AtomicUsize::new(0),
        });
        Self {
            client,
            _rpc_handle: std::rc::Rc::new(RpcTask {
                handle: tokio::task::spawn_local(async {}),
                state,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) async fn register_test_brain_runner_client(
        &self,
        brain: &str,
        lease_id: crate::brain::store::RunnerLeaseId,
        runner: brain_runner::Client,
    ) -> Result<()> {
        let mut request = self.client.register_brain_runner_request();
        request.get().set_brain(brain);
        request.get().set_lease_id(&lease_id.0.to_string());
        request.get().set_runner(runner);
        request
            .get()
            .set_process_epoch(&self._rpc_handle.state.process.epoch.to_string());
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        self._rpc_handle
            .state
            .active_runner_registrations
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) async fn explicitly_eject_runner_process_for_test(&self) -> Result<()> {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl {
            event_tx,
            memory_callbacks: Default::default(),
            process: std::sync::Arc::clone(&self._rpc_handle.state.process),
        });
        let mut request = runner.eject_process_request();
        request
            .get()
            .set_reason("deterministic explicit-ejection test boundary");
        request.send().promise.await?;
        Ok(())
    }

    /// Fail locally before a runner restore performs even a health-check or
    /// reconnect RPC. Every later runner-scoped send repeats this check under
    /// the process admission gate to close the check/send race.
    pub(crate) fn ensure_runner_process_available(&self) -> Result<()> {
        self._rpc_handle.state.process.with_rpc_admission(|| ())
    }

    fn send_runner_rpc<T>(&self, send: impl FnOnce() -> T) -> Result<T> {
        // Cap'n Proto request construction only populates a local builder.
        // The closure contains the irrevocable `send()` and returns its
        // promise without polling it, so admission is never held across an
        // await or re-entered by a response callback.
        self._rpc_handle.state.process.with_rpc_admission(send)
    }

    fn observe_runner_rpc_error(&self, error: capnp::Error) -> anyhow::Error {
        let error = map_runner_rpc_error(error);
        if is_runner_process_quarantined(&error) {
            self._rpc_handle.state.process.publish_remote_poison();
        }
        error
    }

    #[cfg(test)]
    pub(crate) fn pause_next_runner_rpc_admission_for_test(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self
            ._rpc_handle
            .state
            .process
            .runner_rpc_admission_pause
            .lock()
            .expect("runner RPC admission pause lock poisoned") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    #[cfg(test)]
    async fn wait_runner_rpc_admission_pause_for_test(&self) {
        let pause = self
            ._rpc_handle
            .state
            .process
            .runner_rpc_admission_pause
            .lock()
            .expect("runner RPC admission pause lock poisoned")
            .take();
        if let Some((reached, release)) = pause {
            let _ = reached.send(());
            let _ = release.await;
        }
    }

    /// Connect to the daemon's Unix socket.
    pub async fn connect() -> Result<Self> {
        Self::connect_path(sock_path()).await
    }

    pub(crate) async fn connect_for_runner() -> Result<Self> {
        let process = runner_process_identity();
        process.with_rpc_admission(|| ())?;
        let path = sock_path();
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .with_context(|| format!("IPC connect failed: {}", path.display()))?;
        Self::from_stream_with_runner_process(stream, process).await
    }

    /// Connect to an explicitly isolated daemon socket. This is used by the
    /// daemon-upgrade shadow preflight; ordinary clients always use `connect`.
    pub(crate) async fn connect_path(path: std::path::PathBuf) -> Result<Self> {
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .with_context(|| format!("IPC connect failed: {}", path.display()))?;

        Self::from_stream(stream).await
    }

    pub(crate) async fn from_stream(stream: tokio::net::UnixStream) -> Result<Self> {
        Self::from_stream_with_process(stream, runner_process_identity()).await
    }

    async fn from_stream_with_process(
        stream: tokio::net::UnixStream,
        process: std::sync::Arc<RunnerProcessIdentity>,
    ) -> Result<Self> {
        Self::from_stream_with_process_scope(stream, process, false).await
    }

    async fn from_stream_with_runner_process(
        stream: tokio::net::UnixStream,
        process: std::sync::Arc<RunnerProcessIdentity>,
    ) -> Result<Self> {
        Self::from_stream_with_process_scope(stream, process, true).await
    }

    async fn from_stream_with_process_scope(
        stream: tokio::net::UnixStream,
        process: std::sync::Arc<RunnerProcessIdentity>,
        runner_scoped_verification: bool,
    ) -> Result<Self> {
        let (reader, writer) = stream.into_split();
        let network = twoparty::VatNetwork::new(
            reader.compat(),
            writer.compat_write(),
            rpc_twoparty_capnp::Side::Client,
            Default::default(),
        );

        let mut rpc_system = RpcSystem::new(Box::new(network), None);
        let client: finch_daemon::Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

        let state = std::sync::Arc::new(RpcConnectionState {
            process,
            active_runner_registrations: std::sync::atomic::AtomicUsize::new(0),
        });
        let task_state = std::sync::Arc::clone(&state);
        let handle = tokio::task::spawn_local(async move {
            let _ = rpc_system.await;
            // EOF, including an ordinary daemon restart, is not proof of
            // forced ejection. Only the explicit callback or a typed durable
            // registration rejection may poison this process identity.
            let _ = task_state;
        });

        let client = Self {
            client,
            _rpc_handle: std::rc::Rc::new(RpcTask { handle, state }),
        };
        if runner_scoped_verification {
            client.verify_runner_protocol_compatibility().await?;
        } else {
            client.verify_protocol_compatibility().await?;
        }
        Ok(client)
    }

    #[cfg(test)]
    pub(crate) async fn from_stream_with_fresh_test_process(
        stream: tokio::net::UnixStream,
    ) -> Result<Self> {
        Self::from_stream_with_process(stream, new_runner_process_identity(uuid::Uuid::new_v4()))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn reconnect_test_process(
        &self,
        stream: tokio::net::UnixStream,
    ) -> Result<Self> {
        Self::from_stream_with_process(
            stream,
            std::sync::Arc::clone(&self._rpc_handle.state.process),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn test_runner_process_poisoned(&self) -> bool {
        self._rpc_handle
            .state
            .process
            .poisoned
            .load(std::sync::atomic::Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn test_runner_process_identity(&self) -> std::sync::Arc<RunnerProcessIdentity> {
        std::sync::Arc::clone(&self._rpc_handle.state.process)
    }

    #[cfg(test)]
    pub(crate) fn test_active_runner_registrations(&self) -> usize {
        self._rpc_handle
            .state
            .active_runner_registrations
            .load(std::sync::atomic::Ordering::Acquire)
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

    async fn runner_brain_service(&self) -> Result<brain_service::Client> {
        #[cfg(test)]
        self.wait_runner_rpc_admission_pause_for_test().await;
        let request = self.client.runner_brain_service_request();
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        Ok(reply.get()?.get_service()?)
    }

    pub async fn brain_snapshot(&self, brain: &str) -> Result<crate::brain::store::BrainSnapshot> {
        let service = self.brain_service().await?;
        let mut request = service.snapshot_request();
        request.get().set_brain(brain);
        let reply = request.send().promise.await?;
        decode_snapshot(reply.get()?.get_snapshot()?)
    }

    pub(crate) async fn brain_runner_snapshot(
        &self,
        brain: &str,
    ) -> Result<crate::brain::store::BrainSnapshot> {
        let service = self.runner_brain_service().await?;
        let mut request = service.snapshot_request();
        request.get().set_brain(brain);
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
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
        let service = self.runner_brain_service().await?;
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
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        decode_runner_lease(reply.get()?.get_lease()?)
    }

    pub async fn brain_claim_runner_identity(&self, subject: &str) -> Result<()> {
        let service = self.runner_brain_service().await?;
        let mut request = service.claim_runner_identity_request();
        request.get().set_subject(subject);
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        Ok(())
    }

    pub async fn brain_release_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::store::RunnerLeaseId,
    ) -> Result<()> {
        let service = self.runner_brain_service().await?;
        let mut request = service.release_runner_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_lease_id(&lease_id.0.to_string());
        }
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        let _ = self
            ._rpc_handle
            .state
            .active_runner_registrations
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |active| (active != 0).then_some(active - 1),
            );
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
        let service = self.runner_brain_service().await?;
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
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
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
        let service = self.runner_brain_service().await?;
        let mut request = service.accept_runner_handoff_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_target_subject(target_subject);
            params.set_handoff_id(&handoff_id.0.to_string());
            encode_environment(params.reborrow().init_environment(), environment);
            params.set_ttl_ms(ttl_ms);
        }
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        decode_runner_lease(reply.get()?.get_lease()?)
    }

    pub async fn brain_cancel_runner_handoff(
        &self,
        brain: &str,
        handoff_id: crate::brain::store::RunnerHandoffId,
        sender: &str,
    ) -> Result<()> {
        let service = self.runner_brain_service().await?;
        let mut request = service.cancel_runner_handoff_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_handoff_id(&handoff_id.0.to_string());
            params.set_sender(sender);
        }
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
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

    pub(crate) async fn runner_ping(&self) -> Result<String> {
        #[cfg(test)]
        self.wait_runner_rpc_admission_pause_for_test().await;
        let request = self.client.ping_request();
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise.await?;
        let response = reply.get()?;
        ensure_compatible_protocol(response.get_protocol_version())?;
        Ok(response.get_version()?.to_str()?.to_string())
    }

    async fn verify_protocol_compatibility(&self) -> Result<()> {
        let req = self.client.ping_request();
        let reply = req.send().promise.await?;
        ensure_compatible_protocol(reply.get()?.get_protocol_version())
    }

    async fn verify_runner_protocol_compatibility(&self) -> Result<()> {
        self.runner_ping().await.map(|_| ())
    }

    /// Register this frontend as the callback for its current named-Brain
    /// runner lease. The callback stays on this connection's LocalSet.
    pub async fn register_brain_runner(
        &self,
        brain: &str,
        lease_id: crate::brain::store::RunnerLeaseId,
        event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>,
    ) -> Result<BrainRunnerBootstrap> {
        self.ensure_runner_process_available()?;
        let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl {
            event_tx,
            memory_callbacks: Default::default(),
            process: std::sync::Arc::clone(&self._rpc_handle.state.process),
        });
        let mut request = self.client.register_brain_runner_request();
        {
            let mut params = request.get();
            params.set_brain(brain);
            params.set_lease_id(&lease_id.0.to_string());
            params.set_runner(runner);
            params.set_process_epoch(&self._rpc_handle.state.process.epoch.to_string());
        }
        #[cfg(test)]
        self.wait_runner_rpc_admission_pause_for_test().await;
        let promise = self.send_runner_rpc(|| request.send().promise)?;
        let reply = promise
            .await
            .map_err(|error| self.observe_runner_rpc_error(error))?;
        self._rpc_handle
            .state
            .active_runner_registrations
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let response = reply.get()?;
        let control: brain_runner_control::Client = response.get_control()?;
        let (subagent_control, mut subagent_rx) =
            mpsc::unbounded_channel::<crate::runtime::scheduler::AgentBrainControlRequest>();
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
                                params.set_status(crate::ipc::brain_codec::run_status_to_capnp(
                                    status,
                                ));
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

fn map_runner_rpc_error(error: capnp::Error) -> anyhow::Error {
    if error.kind == capnp::ErrorKind::Unimplemented {
        anyhow::anyhow!(
            "the running Finch daemon uses an older IPC schema; restart the daemon and reconnect"
        )
    } else if error
        .to_string()
        .contains(crate::ipc::RUNNER_PROCESS_QUARANTINED_CODE)
    {
        anyhow::Error::new(RunnerProcessQuarantined)
    } else {
        anyhow::Error::new(error)
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "runner process was quarantined after forced callback ejection; restart the frontend process"
)]
pub(crate) struct RunnerProcessQuarantined;

pub(crate) fn is_runner_process_quarantined(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<RunnerProcessQuarantined>().is_some())
}

fn ensure_compatible_protocol(protocol_version: u32) -> Result<()> {
    ensure_protocol_generation(protocol_version, crate::ipc::IPC_PROTOCOL_VERSION)
}

fn ensure_protocol_generation(protocol_version: u32, required_version: u32) -> Result<()> {
    anyhow::ensure!(
        protocol_version == required_version,
        "the running Finch daemon uses IPC protocol {protocol_version}, but this frontend requires {required_version}; restart the daemon with the rebuilt Finch binary",
    );
    Ok(())
}

struct BrainRunnerImpl {
    event_tx: tokio::sync::mpsc::UnboundedSender<crate::cli::repl_event::ReplEvent>,
    memory_callbacks: std::rc::Rc<
        std::cell::RefCell<
            std::collections::HashMap<
                (String, crate::brain::store::RunId),
                MemoryProjectionLifecycle,
            >,
        >,
    >,
    process: std::sync::Arc<RunnerProcessIdentity>,
}

#[derive(Clone)]
struct MemoryProjectionLifecycle {
    generation: uuid::Uuid,
    cancel: tokio_util::sync::CancellationToken,
    finished: tokio::sync::watch::Receiver<bool>,
}

struct MemoryProjectionCompletion {
    memory_callbacks: std::rc::Rc<
        std::cell::RefCell<
            std::collections::HashMap<
                (String, crate::brain::store::RunId),
                MemoryProjectionLifecycle,
            >,
        >,
    >,
    lifecycle_key: (String, crate::brain::store::RunId),
    generation: uuid::Uuid,
    finished_tx: tokio::sync::watch::Sender<bool>,
}

impl Drop for MemoryProjectionCompletion {
    fn drop(&mut self) {
        self.finished_tx.send_replace(true);
        let remove = self
            .memory_callbacks
            .borrow()
            .get(&self.lifecycle_key)
            .is_some_and(|lifecycle| lifecycle.generation == self.generation);
        if remove {
            self.memory_callbacks
                .borrow_mut()
                .remove(&self.lifecycle_key);
        }
    }
}

fn required_runner_text(
    value: capnp::Result<capnp::text::Reader<'_>>,
    field: &str,
) -> capnp::Result<String> {
    let value = value?;
    let value = value.to_str().map_err(|error| {
        capnp::Error::failed(format!(
            "runner request text field '{field}' is not valid UTF-8: {error}"
        ))
    })?;
    if value.is_empty() {
        return Err(capnp::Error::failed(format!(
            "runner request is missing required text field '{field}'"
        )));
    }
    Ok(value.to_string())
}

fn effect_audit_reservation_proxy(
    reservation: finch_ipc_capnp::brain_effect_reservation::Client,
) -> crate::server::RunnerEffectAuditReservation {
    let (tx, mut rx) =
        mpsc::unbounded_channel::<crate::server::RunnerEffectAuditReservationRequest>();
    tokio::task::spawn_local(async move {
        let Some(request) = rx.recv().await else {
            return;
        };
        match request {
            crate::server::RunnerEffectAuditReservationRequest::Begin { response_tx } => {
                let result = async {
                    let response = reservation.begin_request().send().promise.await?;
                    let permit = response.get()?.get_permit()?;
                    Ok(host_effect_permit_proxy(permit))
                }
                .await
                .map_err(|error: capnp::Error| error.to_string());
                let _ = response_tx.send(result);
            }
            crate::server::RunnerEffectAuditReservationRequest::NotApplied {
                reason,
                response_tx,
            } => {
                let result = async {
                    let mut call = reservation.not_applied_request();
                    call.get().set_reason(&reason);
                    call.send().promise.await?;
                    Ok(())
                }
                .await
                .map_err(|error: capnp::Error| error.to_string());
                let _ = response_tx.send(result);
            }
        }
    });
    crate::server::RunnerEffectAuditReservation::new(tx)
}

fn host_effect_permit_proxy(
    permit: finch_ipc_capnp::brain_host_effect_permit::Client,
) -> crate::server::RunnerHostEffectPermit {
    let (tx, mut rx) = mpsc::unbounded_channel::<crate::server::RunnerHostEffectFinishRequest>();
    tokio::task::spawn_local(async move {
        let Some(request) = rx.recv().await else {
            return;
        };
        let result = async {
            let mut call = permit.finish_request();
            let mut outcome = call.get().init_outcome();
            match request.outcome {
                crate::server::RunnerHostEffectOutcome::Acknowledged { values } => {
                    crate::ipc::checkpoint_codec::encode_value_list(
                        outcome.init_acknowledged(values.len() as u32),
                        &values,
                        0,
                    )
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                }
                crate::server::RunnerHostEffectOutcome::NotApplied { reason } => {
                    outcome.set_not_applied(&reason);
                }
                crate::server::RunnerHostEffectOutcome::FailedPartial { detail } => {
                    outcome.set_failed_partial(&detail);
                }
            }
            call.send().promise.await?;
            Ok(())
        }
        .await
        .map_err(|error: capnp::Error| error.to_string());
        let _ = request.response_tx.send(result);
    });
    crate::server::RunnerHostEffectPermit::new(tx)
}

fn program_effect_audit_proxy(
    control: finch_ipc_capnp::brain_program_control::Client,
) -> crate::server::RunnerEffectAuditControl {
    let (tx, mut rx) = mpsc::unbounded_channel::<crate::server::RunnerEffectAuditControlRequest>();
    tokio::task::spawn_local(async move {
        while let Some(request) = rx.recv().await {
            let crate::server::RunnerEffectAuditControlRequest::Reserve {
                execution_id,
                effect,
                response_tx,
            } = request;
            let result = async {
                let mut call = control.reserve_effect_request();
                call.get().set_execution_id(&execution_id.to_string());
                crate::ipc::checkpoint_codec::encode_vm_side_effect(
                    call.get().init_effect(),
                    &effect,
                )
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                let response = call.send().promise.await?;
                Ok(effect_audit_reservation_proxy(
                    response.get()?.get_reservation()?,
                ))
            }
            .await
            .map_err(|error: capnp::Error| error.to_string());
            let _ = response_tx.send(result);
        }
    });
    crate::server::RunnerEffectAuditControl::new(tx)
}

pub(crate) fn turn_effect_audit_proxy(
    control: finch_ipc_capnp::brain_turn_control::Client,
) -> crate::server::RunnerEffectAuditControl {
    let (tx, mut rx) = mpsc::unbounded_channel::<crate::server::RunnerEffectAuditControlRequest>();
    tokio::task::spawn_local(async move {
        while let Some(request) = rx.recv().await {
            let crate::server::RunnerEffectAuditControlRequest::Reserve {
                execution_id,
                effect,
                response_tx,
            } = request;
            let result = async {
                let mut call = control.reserve_effect_request();
                call.get().set_execution_id(&execution_id.to_string());
                crate::ipc::checkpoint_codec::encode_vm_side_effect(
                    call.get().init_effect(),
                    &effect,
                )
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                let response = call.send().promise.await?;
                Ok(effect_audit_reservation_proxy(
                    response.get()?.get_reservation()?,
                ))
            }
            .await
            .map_err(|error: capnp::Error| error.to_string());
            let _ = response_tx.send(result);
        }
    });
    crate::server::RunnerEffectAuditControl::new(tx)
}

struct BrainTurnCommitAckImpl {
    tx: tokio::sync::mpsc::UnboundedSender<crate::server::RunnerTurnCommitNotice>,
}

struct RunnerCallbackCancellation {
    cancel: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl RunnerCallbackCancellation {
    fn new(cancel: tokio_util::sync::CancellationToken) -> Self {
        Self {
            cancel,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunnerCallbackCancellation {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
        }
    }
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
        let brain = match required_runner_text(request.get_brain(), "brain") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let source = match required_runner_text(request.get_source(), "source") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
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
        let effect_audit = program_effect_audit_proxy(control.clone());
        let (control_tx, mut control_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::server::RunnerProgramControlRequest>();
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
                            Ok::<_, capnp::Error>(call.send().promise.await?.get()?.get_cancelled())
                        }
                        .await
                        .map_err(|error| error.to_string());
                        let _ = response_tx.send(result);
                    }
                }
            }
        });
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        if self
            .event_tx
            .send(
                crate::cli::repl_event::ReplEvent::NamedBrainProgramRequested(
                    crate::cli::repl_event::events::BoundedRunnerRequest {
                        request: crate::server::RunnerProgramRequest {
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
                                        return Promise::err(capnp::Error::failed(
                                            error.to_string(),
                                        ))
                                    }
                                }
                            } else {
                                None
                            },
                            control_tx: Some(control_tx),
                            effect_audit: Some(effect_audit),
                            response_tx,
                        },
                        cancel: cancel.clone(),
                    },
                ),
            )
            .is_err()
        {
            return Promise::err(capnp::Error::failed("frontend event loop stopped".into()));
        }
        let callback = RunnerCallbackCancellation::new(cancel);
        Promise::from_future(async move {
            let mut callback = callback;
            let response = response_rx
                .await
                .map_err(|_| capnp::Error::failed("frontend dropped runner response".into()))?;
            callback.disarm();
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
        let brain = match required_runner_text(request.get_brain(), "brain") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let prompt = match required_runner_text(request.get_prompt(), "prompt") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
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
        let effect_audit = turn_effect_audit_proxy(control.clone());
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
        let cancel = tokio_util::sync::CancellationToken::new();
        if self
            .event_tx
            .send(crate::cli::repl_event::ReplEvent::NamedBrainTurnRequested(
                crate::cli::repl_event::events::BoundedRunnerRequest {
                    request: crate::server::RunnerTurnRequest {
                        brain,
                        run_id,
                        request_seq: request.get_request_seq(),
                        prompt,
                        context,
                        approval_audience,
                        approval_connection_id: None,
                        approval_tx: Some(approval_tx),
                        effect_audit: Some(effect_audit),
                        response_tx,
                    },
                    cancel: cancel.clone(),
                },
            ))
            .is_err()
        {
            return Promise::err(capnp::Error::failed("frontend event loop stopped".into()));
        }
        let callback = RunnerCallbackCancellation::new(cancel);
        Promise::from_future(async move {
            let mut callback = callback;
            let response = response_rx
                .await
                .map_err(|_| capnp::Error::failed("frontend dropped runner response".into()))?;
            callback.disarm();
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
                    if !response.continuation_messages.is_empty() {
                        super::brain_codec::encode_continuation_messages(
                            result.reborrow().init_continuation_messages(
                                response.continuation_messages.len() as u32,
                            ),
                            &response.continuation_messages,
                        )
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    }
                    if let Some(metadata) = &response.invocation_metadata {
                        result.set_has_invocation_metadata(true);
                        super::brain_codec::encode_invocation_metadata(
                            result.reborrow().init_invocation_metadata(),
                            metadata,
                        );
                    }
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
            .send(
                crate::cli::repl_event::ReplEvent::NamedBrainRunCancelRequested(
                    crate::cli::repl_event::events::BoundedRunnerCancelRequest {
                        request: crate::server::RunnerCancelRequest {
                            brain,
                            run_id,
                            response_tx,
                        },
                        cancel: tokio_util::sync::CancellationToken::new(),
                        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(10),
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
        let brain_id = match request
            .get_brain_id()
            .map_err(anyhow::Error::new)
            .and_then(parse_uuid)
        {
            Ok(value) => crate::brain::store::BrainId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let run_id = match request
            .get_run_id()
            .map_err(anyhow::Error::new)
            .and_then(parse_uuid)
        {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let brain = match required_runner_text(request.get_brain(), "brain") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let prompt = match required_runner_text(request.get_prompt(), "prompt") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let source = match required_runner_text(request.get_source(), "source") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        let (finished_tx, finished_rx) = tokio::sync::watch::channel(false);
        let lifecycle_key = (brain.clone(), run_id);
        let generation = uuid::Uuid::new_v4();
        if self.memory_callbacks.borrow().contains_key(&lifecycle_key) {
            return Promise::err(capnp::Error::failed(
                "memory projection callback is already active for this run".into(),
            ));
        }
        self.memory_callbacks.borrow_mut().insert(
            lifecycle_key.clone(),
            MemoryProjectionLifecycle {
                generation,
                cancel: cancel.clone(),
                finished: finished_rx,
            },
        );
        if self
            .event_tx
            .send(
                crate::cli::repl_event::ReplEvent::NamedBrainMemoryProjectionRequested(
                    crate::cli::repl_event::events::BoundedRunnerRequest {
                        request: crate::server::RunnerMemoryProjectionRequest {
                            brain_id,
                            brain,
                            run_id,
                            request_seq: request.get_request_seq(),
                            prompt,
                            source,
                            response_tx,
                        },
                        cancel: cancel.clone(),
                    },
                ),
            )
            .is_err()
        {
            finished_tx.send_replace(true);
            self.memory_callbacks.borrow_mut().remove(&lifecycle_key);
            return Promise::err(capnp::Error::failed("frontend event loop stopped".into()));
        }
        let callback = RunnerCallbackCancellation::new(cancel);
        let completion = MemoryProjectionCompletion {
            memory_callbacks: std::rc::Rc::clone(&self.memory_callbacks),
            lifecycle_key,
            generation,
            finished_tx,
        };
        Promise::from_future(async move {
            let _completion = completion;
            let mut callback = callback;
            let response = response_rx
                .await
                .map_err(|_| capnp::Error::failed("frontend dropped memory response".into()))?;
            callback.disarm();
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

    fn cancel_memory(
        &mut self,
        params: brain_runner::CancelMemoryParams,
        _results: brain_runner::CancelMemoryResults,
    ) -> Promise<(), capnp::Error> {
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let brain = match required_runner_text(params.get_brain(), "brain") {
            Ok(value) => value,
            Err(error) => return Promise::err(error),
        };
        let run_id = match params
            .get_run_id()
            .map_err(anyhow::Error::new)
            .and_then(|value| value.to_str().map_err(anyhow::Error::new))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::new))
        {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let Some(lifecycle) = self
            .memory_callbacks
            .borrow()
            .get(&(brain, run_id))
            .cloned()
        else {
            return Promise::ok(());
        };
        lifecycle.cancel.cancel();
        Promise::from_future(async move {
            let mut finished = lifecycle.finished;
            let already_finished = *finished.borrow();
            if !already_finished {
                finished.changed().await.map_err(|_| {
                    capnp::Error::failed(
                        "frontend memory projection ended without a settlement acknowledgement"
                            .into(),
                    )
                })?;
            }
            Ok(())
        })
    }

    fn eject_process(
        &mut self,
        params: brain_runner::EjectProcessParams,
        _results: brain_runner::EjectProcessResults,
    ) -> Promise<(), capnp::Error> {
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = required_runner_text(params.get_reason(), "ejection reason") {
            return Promise::err(error);
        }
        // Linearize poison with every runner-scoped non-awaiting RPC send.
        // The monotonic store happens before acknowledging the daemon, so a
        // concurrent final-client Drop cannot turn an observed ejection into
        // an ordinary EOF and no later runner mutation can be admitted.
        // Recovering a poisoned mutex is safe only because this path
        // immediately makes the process identity terminal. No runner RPC can
        // be admitted again through `with_rpc_admission`.
        let _admission = self
            .process
            .runner_rpc_admission
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.process
            .poisoned
            .store(true, std::sync::atomic::Ordering::Release);
        Promise::ok(())
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

pub(crate) fn encode_brain_turn_event(
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
        crate::brain::store::AttachmentRole::Runner => finch_ipc_capnp::BrainAttachmentRole::Runner,
        crate::brain::store::AttachmentRole::Driver => finch_ipc_capnp::BrainAttachmentRole::Driver,
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
                    output_tokens: u.get_output_tokens(),
                })
                .map_err(|e| anyhow::anyhow!("{}", e)),
            Ok(Which::ResponseMetadata(metadata)) => metadata
                .and_then(decode_stream_response_metadata)
                .map_err(|error| anyhow::anyhow!("{}", error)),
            Ok(Which::AllowanceUpdate(update)) => update
                .map(|value| StreamChunk::Allowance {
                    primary_used_percent: value
                        .get_has_primary()
                        .then(|| value.get_primary_used_percent()),
                    secondary_used_percent: value
                        .get_has_secondary()
                        .then(|| value.get_secondary_used_percent()),
                })
                .map_err(|error| anyhow::anyhow!("{}", error)),
            Ok(Which::ContentBlockComplete(block)) => block
                .and_then(decode_stream_content_block)
                .map(StreamChunk::ContentBlockComplete)
                .map_err(|error| anyhow::anyhow!("{}", error)),
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
            Err(error) => match decode_unknown_stream_chunk(error) {
                Some(result) => result,
                None => return Promise::ok(()),
            },
        };

        let _ = self.tx.send(result);
        Promise::ok(())
    }
}

fn decode_stream_content_block(
    block: finch_ipc_capnp::content_block::Reader<'_>,
) -> std::result::Result<ContentBlock, capnp::Error> {
    use finch_ipc_capnp::content_block::Which;
    match block.which()? {
        Which::Text(value) => Ok(ContentBlock::Text {
            text: value?
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))?
                .to_string(),
        }),
        Which::Thinking(value) => Ok(ContentBlock::OpaqueReasoning {
            encrypted_content: value?
                .to_str()
                .map_err(|error| capnp::Error::failed(error.to_string()))?
                .to_string(),
        }),
        Which::Image(value) => {
            let value = value?;
            Ok(ContentBlock::Image {
                source: crate::claude::types::ImageSource {
                    source_type: value
                        .get_source_type()?
                        .to_str()
                        .map_err(|error| capnp::Error::failed(error.to_string()))?
                        .to_string(),
                    media_type: value
                        .get_media_type()?
                        .to_str()
                        .map_err(|error| capnp::Error::failed(error.to_string()))?
                        .to_string(),
                    data: value
                        .get_data()?
                        .to_str()
                        .map_err(|error| capnp::Error::failed(error.to_string()))?
                        .to_string(),
                },
            })
        }
        Which::ToolUse(value) => {
            let value = value?;
            Ok(ContentBlock::ToolUse {
                id: value
                    .get_id()?
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))?
                    .to_string(),
                name: value
                    .get_name()?
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))?
                    .to_string(),
                input: super::brain_codec::decode_json_value(value.get_input()?)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?,
            })
        }
        Which::ToolResult(value) => {
            let value = value?;
            Ok(ContentBlock::ToolResult {
                tool_use_id: value
                    .get_tool_use_id()?
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))?
                    .to_string(),
                content: value
                    .get_content()?
                    .to_str()
                    .map_err(|error| capnp::Error::failed(error.to_string()))?
                    .to_string(),
                is_error: Some(value.get_is_error()),
            })
        }
    }
}

fn decode_stream_response_metadata(
    metadata: finch_ipc_capnp::stream_response_metadata::Reader<'_>,
) -> std::result::Result<StreamChunk, capnp::Error> {
    let model = metadata
        .get_model()?
        .to_str()
        .map_err(|error| capnp::Error::failed(error.to_string()))?;
    crate::generators::validate_response_model(model)
        .map_err(|_| capnp::Error::failed("IPC response model metadata was invalid".into()))?;
    Ok(StreamChunk::ResponseMetadata {
        model: model.to_string(),
    })
}

fn decode_unknown_stream_chunk(_error: capnp::NotInSchema) -> Option<anyhow::Result<StreamChunk>> {
    // Stream metadata is additive. Older clients ignore newer union members
    // while continuing to decode the text/tool/usage chunks they understand.
    None
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

    struct ProtocolFixtureDaemon {
        protocol_version: u32,
        query_calls: std::rc::Rc<std::cell::Cell<u32>>,
    }

    impl finch_daemon::Server for ProtocolFixtureDaemon {
        fn query(
            &mut self,
            _params: finch_daemon::QueryParams,
            _results: finch_daemon::QueryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            self.query_calls.set(self.query_calls.get() + 1);
            capnp::capability::Promise::err(capnp::Error::failed(
                "protocol fixture must not receive a query".into(),
            ))
        }

        fn query_stream(
            &mut self,
            _params: finch_daemon::QueryStreamParams,
            _results: finch_daemon::QueryStreamResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            self.query_calls.set(self.query_calls.get() + 1);
            capnp::capability::Promise::err(capnp::Error::failed(
                "protocol fixture must not receive a stream".into(),
            ))
        }

        fn ping(
            &mut self,
            _params: finch_daemon::PingParams,
            mut results: finch_daemon::PingResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            results.get().set_version("protocol-fixture");
            results.get().set_protocol_version(self.protocol_version);
            capnp::capability::Promise::ok(())
        }
    }

    async fn connect_isolated_live_socket() -> Result<IpcClient> {
        let proof = crate::brain::isolated_test_proof()?;
        let path = std::env::var_os("FINCH_TEST_IPC_SOCKET")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                anyhow::anyhow!("FINCH_TEST_IPC_SOCKET must name the owned test daemon socket")
            })?;
        anyhow::ensure!(
            path == proof.ipc_socket,
            "IPC path is not parent-authorized"
        );
        #[cfg(unix)]
        let before = crate::brain::validate_isolated_test_socket(&proof, &path)?;
        let stream = tokio::net::UnixStream::connect(&path)
            .await
            .with_context(|| format!("IPC connect failed: {}", path.display()))?;
        #[cfg(unix)]
        crate::brain::authenticate_isolated_test_peer(&stream)?;
        let client = IpcClient::from_stream(stream).await?;
        #[cfg(unix)]
        {
            let after = crate::brain::validate_isolated_test_socket(&proof, &path)?;
            anyhow::ensure!(
                before == after,
                "test IPC socket identity changed during connect"
            );
        }
        Ok(client)
    }

    #[test]
    fn ipc_protocol_handshake_accepts_only_the_current_generation() {
        ensure_compatible_protocol(crate::ipc::IPC_PROTOCOL_VERSION).unwrap();

        let error = ensure_compatible_protocol(0).unwrap_err().to_string();
        assert!(error.contains("restart the daemon"));
        assert!(error.contains("protocol 0"));
    }

    #[test]
    fn mixed_ipc_generations_reject_before_query_or_stream_use() {
        assert_eq!(crate::ipc::IPC_PROTOCOL_VERSION, 13);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let old_daemon_calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let daemon: finch_daemon::Client = capnp_rpc::new_client(ProtocolFixtureDaemon {
                protocol_version: 12,
                query_calls: std::rc::Rc::clone(&old_daemon_calls),
            });
            let client = IpcClient::from_test_client(daemon);
            let error = client.ping().await.unwrap_err().to_string();
            assert!(error.contains("protocol 12"));
            assert!(error.contains("requires 13"));
            assert!(error.contains("restart the daemon"));
            assert_eq!(old_daemon_calls.get(), 0);

            let new_daemon_calls = std::rc::Rc::new(std::cell::Cell::new(0));
            let daemon: finch_daemon::Client = capnp_rpc::new_client(ProtocolFixtureDaemon {
                protocol_version: 13,
                query_calls: std::rc::Rc::clone(&new_daemon_calls),
            });
            let request = daemon.ping_request();
            let reply = request.send().promise.await.unwrap();
            let protocol_version = reply.get().unwrap().get_protocol_version();
            let error = ensure_protocol_generation(protocol_version, 12)
                .unwrap_err()
                .to_string();
            assert!(error.contains("protocol 13"));
            assert!(error.contains("requires 12"));
            assert!(error.contains("restart the daemon"));
            assert_eq!(new_daemon_calls.get(), 0);
        }));
    }

    #[test]
    fn response_metadata_schema_roundtrips_and_unknown_union_is_ignored() {
        use finch_ipc_capnp::stream_chunk::Which;

        let mut message = capnp::message::Builder::new_default();
        message
            .init_root::<finch_ipc_capnp::stream_chunk::Builder<'_>>()
            .init_response_metadata()
            .set_model("gpt-5.6-sol-served");
        let reader = message
            .get_root_as_reader::<finch_ipc_capnp::stream_chunk::Reader<'_>>()
            .unwrap();
        let model = match reader.which().unwrap() {
            Which::ResponseMetadata(metadata) => {
                match decode_stream_response_metadata(metadata.unwrap()).unwrap() {
                    StreamChunk::ResponseMetadata { model } => model,
                    _ => panic!("expected response metadata"),
                }
            }
            _ => panic!("expected response metadata"),
        };
        assert_eq!(model, "gpt-5.6-sol-served");
        assert!(decode_unknown_stream_chunk(capnp::NotInSchema(99)).is_none());

        let mut invalid_message = capnp::message::Builder::new_default();
        invalid_message
            .init_root::<finch_ipc_capnp::stream_chunk::Builder<'_>>()
            .init_response_metadata()
            .set_model("bad\nmodel");
        let invalid_reader = invalid_message
            .get_root_as_reader::<finch_ipc_capnp::stream_chunk::Reader<'_>>()
            .unwrap();
        let error = match invalid_reader.which().unwrap() {
            Which::ResponseMetadata(metadata) => {
                decode_stream_response_metadata(metadata.unwrap()).unwrap_err()
            }
            _ => panic!("expected response metadata"),
        };
        let error = error.to_string();
        assert!(error.ends_with("IPC response model metadata was invalid"));
        assert!(!error.contains("bad\nmodel"));
    }

    #[test]
    fn stream_content_block_schema_preserves_text_image_and_opaque_reasoning() {
        let mut image_message = capnp::message::Builder::new_default();
        {
            let mut image = image_message
                .init_root::<finch_ipc_capnp::content_block::Builder<'_>>()
                .init_image();
            image.set_source_type("base64");
            image.set_media_type("image/png");
            image.set_data("aW1hZ2U=");
        }
        let image = decode_stream_content_block(
            image_message
                .get_root_as_reader::<finch_ipc_capnp::content_block::Reader<'_>>()
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            image,
            ContentBlock::Image { source }
                if source.source_type == "base64"
                    && source.media_type == "image/png"
                    && source.data == "aW1hZ2U="
        ));

        let mut reasoning_message = capnp::message::Builder::new_default();
        reasoning_message
            .init_root::<finch_ipc_capnp::content_block::Builder<'_>>()
            .set_thinking("opaque-continuation");
        let reasoning = decode_stream_content_block(
            reasoning_message
                .get_root_as_reader::<finch_ipc_capnp::content_block::Reader<'_>>()
                .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            reasoning,
            ContentBlock::OpaqueReasoning { encrypted_content }
                if encrypted_content == "opaque-continuation"
        ));
    }

    struct UnusedProgramControl;

    impl finch_ipc_capnp::brain_program_control::Server for UnusedProgramControl {
        fn create_schedule(
            &mut self,
            _params: finch_ipc_capnp::brain_program_control::CreateScheduleParams,
            _results: finch_ipc_capnp::brain_program_control::CreateScheduleResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("unused".into()))
        }

        fn inspect_schedule(
            &mut self,
            _params: finch_ipc_capnp::brain_program_control::InspectScheduleParams,
            _results: finch_ipc_capnp::brain_program_control::InspectScheduleResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("unused".into()))
        }

        fn cancel_schedule(
            &mut self,
            _params: finch_ipc_capnp::brain_program_control::CancelScheduleParams,
            _results: finch_ipc_capnp::brain_program_control::CancelScheduleResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("unused".into()))
        }

        fn reserve_effect(
            &mut self,
            _params: finch_ipc_capnp::brain_program_control::ReserveEffectParams,
            _results: finch_ipc_capnp::brain_program_control::ReserveEffectResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::unimplemented("unused".into()))
        }
    }

    #[test]
    fn test_dropped_program_rpc_cancels_the_exact_enqueued_frontend_request() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl {
                event_tx,
                memory_callbacks: Default::default(),
                process: fresh_runner_process_identity(),
            });
            let control: finch_ipc_capnp::brain_program_control::Client =
                capnp_rpc::new_client(UnusedProgramControl);
            let run_id = crate::brain::store::RunId(uuid::Uuid::new_v4());
            let mut call = runner.run_program_request();
            {
                let mut request = call.get().init_request();
                request.set_brain("shared");
                request.set_run_id(&run_id.0.to_string());
                request.set_request_seq(1);
                request.set_language(finch_ipc_capnp::ProgramLanguage::Lisp);
                request.set_source("stuck");
                request.set_interaction(finch_ipc_capnp::BrainProgramInteraction::Interactive);
                request.set_has_grant_ceiling(false);
                request.set_control(control);
            }
            let mut rpc = Box::pin(call.send().promise);
            let request = tokio::select! {
                event = event_rx.recv() => match event.unwrap() {
                    crate::cli::repl_event::ReplEvent::NamedBrainProgramRequested(request) => request,
                    other => panic!("expected program callback, got {other:?}"),
                },
                _ = &mut rpc => panic!("program callback ended before enqueue"),
            };
            assert_eq!(request.request.run_id, run_id);
            assert!(!request.cancel.is_cancelled());
            drop(rpc);
            tokio::task::yield_now().await;
            assert!(request.cancel.is_cancelled());
        }));
    }

    #[test]
    fn test_explicit_ejection_poison_survives_final_client_drop_but_ordinary_drop_does_not_poison()
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let ordinary_process = fresh_runner_process_identity();
            let ordinary_state = std::sync::Arc::new(RpcConnectionState {
                process: std::sync::Arc::clone(&ordinary_process),
                active_runner_registrations: std::sync::atomic::AtomicUsize::new(0),
            });
            let ordinary_client = IpcClient {
                client: capnp_rpc::new_client(ProtocolFixtureDaemon {
                    protocol_version: crate::ipc::IPC_PROTOCOL_VERSION,
                    query_calls: std::rc::Rc::new(std::cell::Cell::new(0)),
                }),
                _rpc_handle: std::rc::Rc::new(RpcTask {
                    handle: tokio::task::spawn_local(async {}),
                    state: ordinary_state,
                }),
            };
            drop(ordinary_client);
            assert!(!ordinary_process
                .poisoned
                .load(std::sync::atomic::Ordering::Acquire));

            let poisoned_process = fresh_runner_process_identity();
            let poisoned_state = std::sync::Arc::new(RpcConnectionState {
                process: std::sync::Arc::clone(&poisoned_process),
                active_runner_registrations: std::sync::atomic::AtomicUsize::new(1),
            });
            let poisoned_client = IpcClient {
                client: capnp_rpc::new_client(ProtocolFixtureDaemon {
                    protocol_version: crate::ipc::IPC_PROTOCOL_VERSION,
                    query_calls: std::rc::Rc::new(std::cell::Cell::new(0)),
                }),
                _rpc_handle: std::rc::Rc::new(RpcTask {
                    handle: tokio::task::spawn_local(async {}),
                    state: poisoned_state,
                }),
            };
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl {
                event_tx,
                memory_callbacks: Default::default(),
                process: std::sync::Arc::clone(&poisoned_process),
            });
            let mut ejection = runner.eject_process_request();
            ejection.get().set_reason("causal test ejection");
            ejection.send().promise.await.unwrap();
            let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
            let error = match poisoned_client
                .register_brain_runner(
                    "shared",
                    crate::brain::store::RunnerLeaseId(uuid::Uuid::new_v4()),
                    event_tx,
                )
                .await
            {
                Err(error) => error,
                Ok(_) => panic!("process-local quarantine allowed runner registration"),
            };
            assert!(is_runner_process_quarantined(&error));
            drop(poisoned_client);
            assert!(poisoned_process
                .poisoned
                .load(std::sync::atomic::Ordering::Acquire));
        }));
    }

    #[test]
    fn test_remote_quarantine_publication_is_totally_ordered_with_runner_sends() {
        let process = fresh_runner_process_identity();
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (admitted_tx, admitted_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let sender_process = std::sync::Arc::clone(&process);
        let sender_sends = std::sync::Arc::clone(&sends);
        let sender = std::thread::spawn(move || {
            sender_process
                .with_rpc_admission(|| {
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    sender_sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                })
                .unwrap();
        });
        admitted_rx.recv().unwrap();

        let publisher_started = std::sync::Arc::new(std::sync::Barrier::new(2));
        let publisher_thread_started = std::sync::Arc::clone(&publisher_started);
        let (publication_admitted_tx, publication_admitted_rx) = std::sync::mpsc::channel();
        let (publication_release_tx, publication_release_rx) = std::sync::mpsc::channel();
        let (published_tx, published_rx) = std::sync::mpsc::channel();
        let poison_process = std::sync::Arc::clone(&process);
        let publisher = std::thread::spawn(move || {
            publisher_thread_started.wait();
            poison_process.publish_remote_poison_after_admission(|| {
                publication_admitted_tx.send(()).unwrap();
                publication_release_rx.recv().unwrap();
            });
            published_tx.send(()).unwrap();
        });
        publisher_started.wait();
        assert!(!process.poisoned.load(std::sync::atomic::Ordering::Acquire));

        release_tx.send(()).unwrap();
        sender.join().unwrap();
        publication_admitted_rx.recv().unwrap();
        assert!(!process.poisoned.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);

        publication_release_tx.send(()).unwrap();
        published_rx.recv().unwrap();
        publisher.join().unwrap();
        assert!(process.poisoned.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(sends.load(std::sync::atomic::Ordering::SeqCst), 1);

        let later = process.with_rpc_admission(|| {
            sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(is_runner_process_quarantined(&later.unwrap_err()));
        assert_eq!(
            sends.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a runner send occurred after terminal poison publication"
        );
    }

    #[test]
    fn test_runner_request_text_fails_closed_at_callback_boundary() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let runner: brain_runner::Client = capnp_rpc::new_client(BrainRunnerImpl {
                event_tx,
                memory_callbacks: Default::default(),
                process: fresh_runner_process_identity(),
            });

            let mut missing_program = runner.run_program_request();
            missing_program.get().init_request().set_source("valid");
            let error = match missing_program.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("missing program brain was accepted"),
            };
            assert!(error.contains("brain"), "unexpected error: {error}");

            let mut invalid_program = runner.run_program_request();
            {
                let mut request = invalid_program.get().init_request();
                request.set_brain("shared");
                request.set_source(capnp::text::Reader(&[0xff, 0xfe]));
            }
            let error = match invalid_program.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("invalid program source was accepted"),
            };
            assert!(error.contains("UTF-8"), "unexpected error: {error}");

            let mut missing_turn = runner.run_turn_request();
            missing_turn.get().init_request().set_brain("shared");
            let error = match missing_turn.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("missing turn prompt was accepted"),
            };
            assert!(error.contains("prompt"), "unexpected error: {error}");

            let mut invalid_turn = runner.run_turn_request();
            invalid_turn
                .get()
                .init_request()
                .set_brain(capnp::text::Reader(&[0xff, 0xfe]));
            let error = match invalid_turn.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("invalid turn brain was accepted"),
            };
            assert!(error.contains("UTF-8"), "unexpected error: {error}");

            let brain_id = uuid::Uuid::new_v4().to_string();
            let run_id = uuid::Uuid::new_v4().to_string();
            let mut missing_memory = runner.project_memory_request();
            {
                let mut request = missing_memory.get().init_request();
                request.set_brain_id(&brain_id);
                request.set_brain("shared");
                request.set_run_id(&run_id);
                request.set_source("valid");
            }
            let error = match missing_memory.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("missing memory prompt was accepted"),
            };
            assert!(error.contains("prompt"), "unexpected error: {error}");

            let mut invalid_memory = runner.project_memory_request();
            {
                let mut request = invalid_memory.get().init_request();
                request.set_brain_id(&brain_id);
                request.set_brain("shared");
                request.set_run_id(&run_id);
                request.set_prompt("valid");
                request.set_source(capnp::text::Reader(&[0xff, 0xfe]));
            }
            let error = match invalid_memory.send().promise.await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("invalid memory source was accepted"),
            };
            assert!(error.contains("UTF-8"), "unexpected error: {error}");

            assert!(matches!(
                event_rx.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }));
    }

    struct BlockingBrainRunner {
        started:
            std::cell::RefCell<Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>>,
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
        let error =
            map_runner_rpc_error(capnp::Error::unimplemented("remote method missing".into()));
        let message = error.to_string();
        assert!(message.contains("older IPC schema"));
        assert!(message.contains("restart the daemon"));
    }

    /// Connect to the live daemon socket and verify ping round-trip.
    ///
    /// Requires an owned daemon socket named by `FINCH_TEST_IPC_SOCKET`.
    /// Run with:
    ///   ./scripts/test_brains.sh cargo test --lib ipc::client::tests::test_ipc_ping -- --ignored --nocapture
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
            let client = connect_isolated_live_socket()
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
            let client = connect_isolated_live_socket()
                .await
                .expect("IPC connect — start the rebuilt Finch daemon first");
            let brain = format!("bootstrap-smoke-{}", uuid::Uuid::new_v4().simple());
            let snapshot = client.brain_snapshot(&brain).await.unwrap();
            let subject = format!("smoke@localhost/frontend-{}", uuid::Uuid::new_v4());
            client.brain_claim_runner_identity(&subject).await.unwrap();
            let lease = client
                .brain_acquire_runner(&brain, &subject, &snapshot.environment, None, 30_000)
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

            let replacement = connect_isolated_live_socket().await.unwrap();
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

    /// Exercise the complete local named-Brain lifecycle capability against an
    /// explicitly owned daemon socket.
    #[test]
    #[ignore]
    fn test_brain_service_lifecycle() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let client = connect_isolated_live_socket()
                .await
                .expect("IPC connect — is `finch daemon` running?");
            let brain = format!("codex-ipc-smoke-{}", uuid::Uuid::new_v4().simple());
            let snapshot = client.brain_snapshot(&brain).await.unwrap();
            let attachment = client
                .brain_attach(
                    &brain,
                    "codex-smoke@localhost",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut incoming = client.brain_watch(&brain, &attachment).await.unwrap();
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
                    &brain,
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
                    &brain,
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
            let inspected = client.brain_inspect_run(&brain, run.run_id).await.unwrap();
            assert_eq!(inspected.run_id, run.run_id);
            let cancelled = client
                .brain_cancel_run(&brain, &attachment, run.run_id)
                .await
                .unwrap();
            assert_eq!(
                cancelled.status,
                crate::brain::store::BrainRunStatus::Cancelled
            );
            let acknowledged = client
                .brain_acknowledge(&brain, &attachment, outcome.accepted.seq)
                .await
                .unwrap();
            assert_eq!(acknowledged.acknowledged_seq, outcome.accepted.seq);
            client.brain_detach(&brain, &acknowledged).await.unwrap();
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
            let client = connect_isolated_live_socket()
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
            registration
                .get()
                .set_process_epoch(&client._rpc_handle.state.process.epoch.to_string());
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
                client
                    .brain_inspect_run(&brain, run_id)
                    .await
                    .unwrap()
                    .status,
                crate::brain::store::BrainRunStatus::Cancelled
            );
            client
                .brain_release_runner(&brain, lease.lease_id)
                .await
                .unwrap();
            client.brain_detach(&brain, &attachment).await.unwrap();
        }));
    }

    #[test]
    #[ignore = "requires an owned daemon at FINCH_TEST_IPC_SOCKET"]
    fn test_brain_runner_lease_cannot_be_hijacked_by_another_ipc_connection() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let owner = connect_isolated_live_socket().await.unwrap();
            let intruder = connect_isolated_live_socket().await.unwrap();
            let brain = format!("codex-runner-authority-{}", uuid::Uuid::new_v4());
            let subject = "owner/frontend-authority";
            let snapshot = owner.brain_snapshot(&brain).await.unwrap();

            owner.brain_claim_runner_identity(subject).await.unwrap();
            assert!(intruder.brain_claim_runner_identity(subject).await.is_err());
            let lease = owner
                .brain_acquire_runner(&brain, subject, &snapshot.environment, None, 60_000)
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
    #[ignore = "requires an owned daemon at FINCH_TEST_IPC_SOCKET"]
    fn test_brain_attachment_cannot_be_replayed_by_another_ipc_connection() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        rt.block_on(local.run_until(async {
            let owner = connect_isolated_live_socket().await.unwrap();
            let intruder = connect_isolated_live_socket().await.unwrap();
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
            let initial =
                tokio::time::timeout(std::time::Duration::from_secs(2), owner_watch.recv())
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
            let watch_error =
                tokio::time::timeout(std::time::Duration::from_secs(2), forged_watch.recv())
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
