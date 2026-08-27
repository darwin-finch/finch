//! Cap'n Proto RPC server — runs inside the daemon, listens on the Unix socket.
//!
//! Each inbound connection gets its own `FinchDaemonImpl` backed by the
//! shared `Arc<AgentServer>`.

use std::sync::Arc;

use anyhow::{Context, Result};
use capnp::capability::Promise;
use capnp_rpc::{pry, rpc_twoparty_capnp, twoparty, RpcSystem};
use tokio::net::{UnixListener, UnixStream};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::ipc::brain_codec::{
    decode_approval_audience, decode_brain_submission, decode_environment, encode_approval_audience,
    encode_attachment, encode_brain_submission_outcome, encode_event, encode_runner_handoff,
    encode_run, encode_runner_lease, encode_schedule, encode_snapshot,
};
use crate::ipc::checkpoint_codec::{decode_checkpoint, encode_checkpoint};
use crate::ipc::schema::finch_ipc_capnp::{self, brain_service, finch_daemon};
use crate::server::AgentServer;

// ---------------------------------------------------------------------------
// Server implementation struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FinchDaemonImpl {
    server: Arc<AgentServer>,
    connection_id: uuid::Uuid,
}

impl FinchDaemonImpl {
    fn new(server: Arc<AgentServer>, connection_id: uuid::Uuid) -> Self {
        Self {
            server,
            connection_id,
        }
    }
}

#[derive(Clone)]
struct BrainRpcService {
    lifecycle: crate::server::BrainLifecycleService,
    runners: crate::server::BrainRunnerBroker,
    connection_id: uuid::Uuid,
}

impl BrainRpcService {
    fn acquire_connection_runner(
        &self,
        brain: &str,
        subject: &str,
        environment: &crate::brain::store::BrainEnvironment,
        lease_id: Option<crate::brain::store::RunnerLeaseId>,
        ttl_ms: u64,
    ) -> anyhow::Result<crate::brain::store::BrainRunnerLease> {
        self.runners
            .require_connection_identity(self.connection_id, subject)?;
        let reconnecting_lease = lease_id.is_some();
        let lease = self
            .lifecycle
            .acquire_runner(brain, subject, environment, lease_id, ttl_ms)?;
        if let Err(error) = self.runners.claim_connection_lease(
            self.connection_id,
            brain,
            lease.lease_id,
        ) {
            // Never release a renewed durable lease merely because rebinding
            // lost a race with another still-live connection.
            if !reconnecting_lease {
                let _ = self.lifecycle.release_runner(brain, lease.lease_id);
            }
            return Err(error);
        }
        Ok(lease)
    }
}

/// Reverse per-turn capability used by the leased frontend runner to suspend
/// on an approval without deciding it locally. The daemon records the request
/// and resumes it only from the attachment named by `expected_audience`.
struct BrainTurnControlImpl {
    server: Arc<AgentServer>,
    brain: String,
    request_seq: u64,
    expected_audience: crate::brain::store::BrainApprovalAudience,
    expected_connection_id: Option<crate::brain::store::ConnectionId>,
}

fn require_approval_connection(
    connection_id: Option<crate::brain::store::ConnectionId>,
) -> anyhow::Result<crate::brain::store::ConnectionId> {
    connection_id.context("approval audience has no live connection generation")
}

#[cfg(test)]
pub(crate) fn test_turn_control_client(
    server: Arc<AgentServer>,
    brain: String,
    request_seq: u64,
    expected_audience: crate::brain::store::BrainApprovalAudience,
    expected_connection_id: Option<crate::brain::store::ConnectionId>,
) -> finch_ipc_capnp::brain_turn_control::Client {
    capnp_rpc::new_client(
        BrainTurnControlImpl {
            server,
            brain,
            request_seq,
            expected_audience,
            expected_connection_id,
        },
    )
}

#[cfg(test)]
pub(crate) async fn request_test_turn_approval_with_client(
    control: finch_ipc_capnp::brain_turn_control::Client,
    event: crate::server::RunnerTurnEvent,
) -> Result<serde_json::Value> {
    let mut call = control.request_approval_request();
    let crate::server::RunnerTurnEvent::ApprovalRequested {
        approval_id, approval_kind, subject, audience, detail,
    } = event else {
        anyhow::bail!("test reverse control accepts only approval requests");
    };
    let mut encoded = call.get().init_event();
    encoded.set_kind(finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested);
    encoded.set_approval_id(&approval_id);
    encoded.set_approval_kind(&approval_kind);
    encoded.set_subject(&subject);
    encode_approval_audience(encoded.reborrow().init_approval_audience(), &audience);
    crate::ipc::brain_codec::encode_json_value(encoded.reborrow().init_detail(), &detail)?;
    let response = call.send().promise.await?;
    crate::ipc::brain_codec::decode_json_value(response.get()?.get_decision()?)
}

#[cfg(test)]
pub(crate) async fn request_test_turn_approval(
    server: Arc<AgentServer>,
    brain: String,
    request_seq: u64,
    expected_audience: crate::brain::store::BrainApprovalAudience,
    expected_connection_id: Option<crate::brain::store::ConnectionId>,
    event: crate::server::RunnerTurnEvent,
) -> Result<serde_json::Value> {
    request_test_turn_approval_with_client(
        test_turn_control_client(
            server,
            brain,
            request_seq,
            expected_audience,
            expected_connection_id,
        ),
        event,
    ).await
}

/// Reverse capability scoped to one daemon-authenticated ProgramRun. The
/// frontend may request durable schedule operations, but it never receives
/// participant attachment credentials and cannot substitute another run.
struct BrainProgramControlImpl {
    lifecycle: crate::server::BrainLifecycleService,
    brain: String,
    run_id: crate::brain::store::RunId,
    request_seq: u64,
    maximum_grant_ceiling: Option<crate::vm::EffectSet>,
}

/// Reverse lifecycle capability scoped to one exact runner registration.
/// Every call rechecks both the IPC connection binding and the daemon's live
/// lease, so a stale frontend cannot publish child state after handoff.
struct BrainRunnerControlImpl {
    lifecycle: crate::server::BrainLifecycleService,
    runners: crate::server::BrainRunnerBroker,
    connection_id: uuid::Uuid,
    brain: String,
    lease_id: crate::brain::store::RunnerLeaseId,
}

impl BrainRunnerControlImpl {
    fn validate_lease(&self) -> anyhow::Result<()> {
        self.runners.require_connection_lease(
            self.connection_id,
            &self.brain,
            self.lease_id,
        )?;
        let snapshot = self.lifecycle.snapshot(&self.brain)?;
        anyhow::ensure!(
            snapshot
                .runner_lease
                .as_ref()
                .is_some_and(|lease| lease.lease_id == self.lease_id),
            "runner lifecycle capability no longer matches the active lease"
        );
        Ok(())
    }
}

impl finch_ipc_capnp::brain_runner_control::Server for BrainRunnerControlImpl {
    fn start_subagent(
        &mut self,
        params: finch_ipc_capnp::brain_runner_control::StartSubagentParams,
        mut results: finch_ipc_capnp::brain_runner_control::StartSubagentResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.validate_lease() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let parse_uuid = |value: capnp::text::Reader<'_>| {
            value
                .to_str()
                .map_err(anyhow::Error::from)
                .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        };
        let parent_run_id = match params.get_parent_run_id().map_err(anyhow::Error::from).and_then(parse_uuid) {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let task_id = match params.get_task_id().map_err(anyhow::Error::from).and_then(parse_uuid) {
            Ok(value) => value,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let detail = params
            .get_detail()
            .ok()
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        match self
            .lifecycle
            .start_subagent_for_run(&self.brain, parent_run_id, task_id, detail)
        {
            Ok(run) => {
                encode_run(results.get().init_run(), &run);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn finish_subagent(
        &mut self,
        params: finch_ipc_capnp::brain_runner_control::FinishSubagentParams,
        mut results: finch_ipc_capnp::brain_runner_control::FinishSubagentResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(error) = self.validate_lease() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let run_id = match params
            .get_run_id()
            .map_err(anyhow::Error::from)
            .and_then(|value| value.to_str().map_err(anyhow::Error::from))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let status = match params.get_status() {
            Ok(status) => crate::ipc::brain_codec::run_status_from_capnp(status),
            Err(error) => return Promise::err(error.into()),
        };
        let detail = params
            .get_detail()
            .ok()
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        match self
            .lifecycle
            .transition_subagent_run(&self.brain, run_id, status, detail)
        {
            Ok(run) => {
                encode_run(results.get().init_run(), &run);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl finch_ipc_capnp::brain_program_control::Server for BrainProgramControlImpl {
    fn create_schedule(
        &mut self,
        params: finch_ipc_capnp::brain_program_control::CreateScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::CreateScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let language = match params.get_language() {
            Ok(language) => program_language_from_capnp(language),
            Err(error) => return Promise::err(error.into()),
        };
        let source = match params.get_source() {
            Ok(source) => source.to_str().unwrap_or("").to_string(),
            Err(error) => return Promise::err(error),
        };
        let grant_ceiling = match params
            .get_grant_ceiling()
            .map_err(anyhow::Error::from)
            .and_then(crate::ipc::checkpoint_codec::decode_effects)
        {
            Ok(effects) => effects,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let policy = match decode_schedule_policy(params.get_policy()) {
            Ok(policy) => policy,
            Err(error) => return Promise::err(error),
        };
        let schedule = match self.lifecycle.create_schedule_for_run(
            &self.brain,
            self.run_id,
            self.request_seq,
            self.maximum_grant_ceiling.as_ref(),
            language,
            source,
            grant_ceiling,
            params.get_next_due_ms(),
            params
                .get_has_interval_ms()
                .then(|| params.get_interval_ms()),
            policy,
        ) {
            Ok(schedule) => schedule,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_schedule(results.get().init_schedule(), &schedule);
        Promise::ok(())
    }

    fn inspect_schedule(
        &mut self,
        params: finch_ipc_capnp::brain_program_control::InspectScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::InspectScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let schedule_id = match params
            .get()
            .and_then(|params| params.get_schedule_id())
            .map_err(anyhow::Error::from)
            .and_then(|value| value.to_str().map_err(anyhow::Error::from))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        {
            Ok(id) => crate::brain::store::ScheduleId(id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let schedule = match self.lifecycle.inspect_schedule_for_run(
            &self.brain,
            self.run_id,
            self.request_seq,
            schedule_id,
        ) {
            Ok(schedule) => schedule,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let mut response = results.get();
        response.set_found(schedule.is_some());
        if let Some(schedule) = schedule {
            encode_schedule(response.init_schedule(), &schedule);
        }
        Promise::ok(())
    }

    fn cancel_schedule(
        &mut self,
        params: finch_ipc_capnp::brain_program_control::CancelScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::CancelScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let schedule_id = match params
            .get()
            .and_then(|params| params.get_schedule_id())
            .map_err(anyhow::Error::from)
            .and_then(|value| value.to_str().map_err(anyhow::Error::from))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        {
            Ok(id) => crate::brain::store::ScheduleId(id),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        match self.lifecycle.cancel_schedule_for_run(
            &self.brain,
            self.run_id,
            self.request_seq,
            schedule_id,
        ) {
            Ok(cancelled) => {
                results.get().set_cancelled(cancelled);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl finch_ipc_capnp::brain_turn_control::Server for BrainTurnControlImpl {
    fn request_approval(
        &mut self,
        params: finch_ipc_capnp::brain_turn_control::RequestApprovalParams,
        mut results: finch_ipc_capnp::brain_turn_control::RequestApprovalResults,
    ) -> Promise<(), capnp::Error> {
        let encoded = match params.get().and_then(|params| params.get_event()) {
            Ok(encoded) => encoded,
            Err(error) => return Promise::err(error),
        };
        let event = match decode_runner_turn_event(encoded) {
            Ok(event) => event,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
        };
        let crate::server::RunnerTurnEvent::ApprovalRequested {
            approval_id,
            approval_kind,
            subject,
            audience,
            detail,
        } = event
        else {
            return Promise::err(capnp::Error::failed(
                "Brain turn control accepts only approval requests".into(),
            ));
        };
        if audience != self.expected_audience {
            return Promise::err(capnp::Error::failed(format!(
                "runner substituted the approval audience for request {}",
                self.request_seq
            )));
        }

        let connection_id = match require_approval_connection(self.expected_connection_id) {
            Ok(connection_id) => connection_id,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let registration = match self.server.brain_approvals().register_for_connection_with_authority(
            self.request_seq,
            approval_id.clone(),
            audience.clone(),
            connection_id,
            || self.server.brain_store().begin_run_approval_for_connection(
                &self.brain,
                audience.attachment_id,
                connection_id,
                self.request_seq,
                approval_id.clone(),
                approval_kind.clone(),
                subject.clone(),
                audience.clone(),
                detail.clone(),
            ),
        ) {
            Ok(registration) => registration,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let (registration, run_id) = registration;

        let store = self.server.brain_store().clone();
        let brain = self.brain.clone();
        Promise::from_future(async move {
            let decision = match registration.wait().await {
                Ok(decision) => {
                    store
                        .transition_run(
                            &brain,
                            "daemon",
                            run_id,
                            crate::brain::store::BrainRunStatus::Running,
                            None,
                        )
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    decision
                }
                Err(error) => {
                    // The run supervisor exclusively publishes terminal outcomes.
                    return Err(capnp::Error::failed(error.to_string()));
                }
            };
            let mut response = results.get();
            super::brain_codec::encode_json_value(response.reborrow().init_decision(), &decision)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            Ok(())
        })
    }
}

impl brain_service::Server for BrainRpcService {
    fn snapshot(
        &mut self,
        params: brain_service::SnapshotParams,
        mut results: brain_service::SnapshotResults,
    ) -> Promise<(), capnp::Error> {
        let brain = pry!(pry!(params.get()).get_brain())
            .to_str()
            .unwrap_or("")
            .to_string();
        let snapshot = match self.lifecycle.snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        match encode_snapshot(results.get().init_snapshot(), &snapshot) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn attach(
        &mut self,
        params: brain_service::AttachParams,
        mut results: brain_service::AttachResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let subject = pry!(params.get_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let role = match params.get_role() {
            Ok(role) => attachment_role_from_capnp(role),
            Err(error) => return Promise::err(error.into()),
        };
        let attachment_id = if params.get_has_attachment_id() {
            match parse_attachment_id(params.get_attachment_id()) {
                Ok(id) => Some(id),
                Err(error) => return Promise::err(error),
            }
        } else {
            None
        };
        let attachment = match self.lifecycle.attach(&brain, &subject, role, attachment_id) {
            Ok(attachment) => attachment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let attachment_connection_id = attachment
            .connection_id
            .expect("new local Brain attachment has a pending connection");
        if let Err(error) = self.runners.claim_connection_attachment(
            self.connection_id,
            &brain,
            attachment.attachment_id,
            attachment_connection_id,
        ) {
            let _ = self.lifecycle.detach(
                &brain,
                attachment.attachment_id,
                attachment_connection_id,
            );
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn acknowledge(
        &mut self,
        params: brain_service::AcknowledgeParams,
        mut results: brain_service::AcknowledgeResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let attachment = match self.lifecycle.acknowledge(
            &brain,
            attachment_id,
            connection_id,
            params.get_seq(),
        ) {
            Ok(attachment) => attachment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn detach(
        &mut self,
        params: brain_service::DetachParams,
        _results: brain_service::DetachResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        if let Err(error) = self.lifecycle.detach(
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        self.runners.release_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        );
        Promise::ok(())
    }

    fn submit(
        &mut self,
        params: brain_service::SubmitParams,
        mut results: brain_service::SubmitResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let kind = match params
            .get_submission()
            .map_err(anyhow::Error::from)
            .and_then(decode_brain_submission)
        {
            Ok(kind) => kind,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let lifecycle = self.lifecycle.clone();
        // Admission above is still owned by the authenticated RPC request.
        // Once admitted, the exact lifecycle submission is daemon-owned so a
        // dropped Cap'n Proto response future cannot abandon a durable run
        // before its terminal publication.
        let submission = tokio::task::spawn_local(async move {
            lifecycle
                .submit(&brain, attachment_id, connection_id, kind)
                .await
        });
        Promise::from_future(async move {
            let outcome = submission
                .await
                .map_err(|error| {
                    capnp::Error::failed(format!("daemon Brain submission task failed: {error}"))
                })?
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encode_brain_submission_outcome(
                results.get().init_outcome(),
                &outcome.accepted,
                outcome.run.as_ref(),
                outcome.result.as_ref(),
            )
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
            Ok(())
        })
    }

    fn watch(
        &mut self,
        params: brain_service::WatchParams,
        _results: brain_service::WatchResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let receiver = pry!(params.get_receiver());
        let watch = match self.lifecycle.watch(
            &brain,
            attachment_id,
            connection_id,
        ) {
            Ok(watch) => watch,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let snapshot = watch.snapshot;
        let mut events = watch.events;
        let lifecycle = self.lifecycle.clone();
        let runners = self.runners.clone();
        let transport_connection_id = self.connection_id;
        Promise::from_future(async move {
            let mut initial = receiver.on_message_request();
            let initial_result = encode_snapshot(
                initial.get().init_message().init_snapshot(),
                &snapshot,
            )
            .map_err(|error| capnp::Error::failed(error.to_string()))
            .and_then(|()| Ok(initial));
            let initial_error = match initial_result {
                Ok(initial) => initial.send().promise.await.err(),
                Err(error) => Some(error),
            };
            if let Some(error) = initial_error {
                let _ = lifecycle.detach(&brain, attachment_id, connection_id);
                runners.release_connection_attachment(
                    transport_connection_id,
                    &brain,
                    attachment_id,
                    connection_id,
                );
                return Err(error);
            }
            let watch_error = loop {
                let event = match events.recv().await {
                    Ok(event) => event,
                    Err(error) => break Some(capnp::Error::failed(error.to_string())),
                };
                if event.seq <= snapshot.revision {
                    continue;
                }
                let mut call = receiver.on_message_request();
                encode_event(call.get().init_message().init_event(), &event)
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                if call.send().promise.await.is_err() {
                    break None;
                }
            };
            let _ = lifecycle.detach(&brain, attachment_id, connection_id);
            runners.release_connection_attachment(
                transport_connection_id,
                &brain,
                attachment_id,
                connection_id,
            );
            watch_error.map_or(Ok(()), Err)
        })
    }

    fn acquire_runner(
        &mut self,
        params: brain_service::AcquireRunnerParams,
        mut results: brain_service::AcquireRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let subject = pry!(params.get_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let environment = match params
            .get_environment()
            .map_err(anyhow::Error::from)
            .and_then(decode_environment)
        {
            Ok(environment) => environment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let lease_id = if params.get_has_lease_id() {
            match parse_runner_lease_id(params.get_lease_id()) {
                Ok(id) => Some(id),
                Err(error) => return Promise::err(error),
            }
        } else {
            None
        };
        // A reconnect has a new IPC connection ID. The durable subject and
        // lease are validated before the new connection binds the lease.
        let lease = match self.acquire_connection_runner(
            &brain,
            &subject,
            &environment,
            lease_id,
            params.get_ttl_ms(),
        ) {
            Ok(lease) => lease,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_runner_lease(results.get().init_lease(), &lease);
        Promise::ok(())
    }

    fn release_runner(
        &mut self,
        params: brain_service::ReleaseRunnerParams,
        _results: brain_service::ReleaseRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let lease_id = match parse_runner_lease_id(params.get_lease_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_lease(
            self.connection_id,
            &brain,
            lease_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        if let Err(error) = self.lifecycle.release_runner(&brain, lease_id) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        self.runners
            .release_connection_lease(self.connection_id, &brain, lease_id);
        Promise::ok(())
    }

    fn request_runner_handoff(
        &mut self,
        params: brain_service::RequestRunnerHandoffParams,
        mut results: brain_service::RequestRunnerHandoffResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let requested_by = pry!(params.get_requested_by())
            .to_str()
            .unwrap_or("")
            .to_string();
        let target_subject = pry!(params.get_target_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let expected_lease_id = match parse_runner_lease_id(params.get_expected_lease_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let environment = match params
            .get_environment()
            .map_err(anyhow::Error::from)
            .and_then(decode_environment)
        {
            Ok(environment) => environment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let handoff = match self.lifecycle.request_runner_handoff(
            &brain,
            &requested_by,
            &target_subject,
            expected_lease_id,
            &environment,
            params.get_ttl_ms(),
        ) {
            Ok(handoff) => handoff,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_runner_handoff(results.get().init_handoff(), &handoff);
        Promise::ok(())
    }

    fn accept_runner_handoff(
        &mut self,
        params: brain_service::AcceptRunnerHandoffParams,
        mut results: brain_service::AcceptRunnerHandoffResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let target_subject = pry!(params.get_target_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        let handoff_id = match parse_runner_handoff_id(params.get_handoff_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let environment = match params
            .get_environment()
            .map_err(anyhow::Error::from)
            .and_then(decode_environment)
        {
            Ok(environment) => environment,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if let Err(error) = self
            .runners
            .require_connection_identity(self.connection_id, &target_subject)
        {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let lease = match self.lifecycle.accept_runner_handoff(
            &brain,
            &target_subject,
            handoff_id,
            &environment,
            params.get_ttl_ms(),
        ) {
            Ok(lease) => lease,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if let Err(error) = self.runners.claim_connection_lease(
            self.connection_id,
            &brain,
            lease.lease_id,
        ) {
            let _ = self.lifecycle.release_runner(&brain, lease.lease_id);
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        encode_runner_lease(results.get().init_lease(), &lease);
        Promise::ok(())
    }

    fn cancel_runner_handoff(
        &mut self,
        params: brain_service::CancelRunnerHandoffParams,
        _results: brain_service::CancelRunnerHandoffResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let handoff_id = match parse_runner_handoff_id(params.get_handoff_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let sender = pry!(params.get_sender()).to_str().unwrap_or("").to_string();
        if let Err(error) = self
            .lifecycle
            .cancel_runner_handoff(&brain, handoff_id, &sender)
        {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        Promise::ok(())
    }

    fn inspect_run(
        &mut self,
        params: brain_service::InspectRunParams,
        mut results: brain_service::InspectRunResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let run_id = match parse_run_id(params.get_run_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let run = match self.lifecycle.inspect_run(&brain, run_id) {
            Ok(run) => run,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_run(results.get().init_run(), &run);
        Promise::ok(())
    }

    fn cancel_run(
        &mut self,
        params: brain_service::CancelRunParams,
        mut results: brain_service::CancelRunResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let run_id = match parse_run_id(params.get_run_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let lifecycle = self.lifecycle.clone();
        Promise::from_future(async move {
            let run = lifecycle
                .cancel_run(&brain, attachment_id, connection_id, run_id)
                .await
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            encode_run(results.get().init_run(), &run);
            Ok(())
        })
    }

    fn create_schedule(
        &mut self,
        params: brain_service::CreateScheduleParams,
        mut results: brain_service::CreateScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let language = match params.get_language() {
            Ok(language) => program_language_from_capnp(language),
            Err(error) => return Promise::err(error.into()),
        };
        let source = pry!(params.get_source()).to_str().unwrap_or("").to_string();
        let grant_ceiling = match params
            .get_grant_ceiling()
            .map_err(anyhow::Error::from)
            .and_then(crate::ipc::checkpoint_codec::decode_effects)
        {
            Ok(effects) => effects,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let policy = match decode_schedule_policy(params.get_policy()) {
            Ok(policy) => policy,
            Err(error) => return Promise::err(error),
        };
        let schedule = match self.lifecycle.create_schedule(
            &brain,
            attachment_id,
            connection_id,
            language,
            source,
            grant_ceiling,
            params.get_next_due_ms(),
            params
                .get_has_interval_ms()
                .then(|| params.get_interval_ms()),
            policy,
        ) {
            Ok(schedule) => schedule,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        encode_schedule(results.get().init_schedule(), &schedule);
        Promise::ok(())
    }

    fn inspect_schedule(
        &mut self,
        params: brain_service::InspectScheduleParams,
        mut results: brain_service::InspectScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let schedule_id = match parse_schedule_id(params.get_schedule_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        match self.lifecycle.inspect_schedule(&brain, schedule_id) {
            Ok(Some(schedule)) => {
                results.get().set_found(true);
                encode_schedule(results.get().init_schedule(), &schedule);
                Promise::ok(())
            }
            Ok(None) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn cancel_schedule(
        &mut self,
        params: brain_service::CancelScheduleParams,
        mut results: brain_service::CancelScheduleResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let schedule_id = match parse_schedule_id(params.get_schedule_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        match self.lifecycle.cancel_schedule(
            &brain,
            attachment_id,
            connection_id,
            schedule_id,
        ) {
            Ok(cancelled) => {
                results.get().set_cancelled(cancelled);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn schedule_initialization(
        &mut self,
        params: brain_service::ScheduleInitializationParams,
        mut results: brain_service::ScheduleInitializationResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let attachment_id = match parse_attachment_id(params.get_attachment_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        let connection_id = match parse_connection_id(params.get_connection_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) = self.runners.require_connection_attachment(
            self.connection_id,
            &brain,
            attachment_id,
            connection_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        match self.lifecycle.schedule_initialization(
            &brain,
            attachment_id,
            connection_id,
            params.get_next_due_ms(),
        ) {
            Ok(schedule) => {
                encode_schedule(results.get().init_schedule(), &schedule);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn claim_runner_identity(
        &mut self,
        params: brain_service::ClaimRunnerIdentityParams,
        _results: brain_service::ClaimRunnerIdentityResults,
    ) -> Promise<(), capnp::Error> {
        let subject = pry!(pry!(params.get()).get_subject())
            .to_str()
            .unwrap_or("")
            .to_string();
        match self
            .runners
            .claim_connection_identity(self.connection_id, &subject)
        {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

fn parse_attachment_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::AttachmentId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::AttachmentId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_connection_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::ConnectionId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::ConnectionId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_run_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::RunId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::RunId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_schedule_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::ScheduleId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::ScheduleId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_runner_lease_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::RunnerLeaseId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::RunnerLeaseId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

fn parse_runner_handoff_id(
    value: capnp::Result<capnp::text::Reader<'_>>,
) -> Result<crate::brain::store::RunnerHandoffId, capnp::Error> {
    let value = value?.to_str()?;
    uuid::Uuid::parse_str(value)
        .map(crate::brain::store::RunnerHandoffId)
        .map_err(|error| capnp::Error::failed(error.to_string()))
}

// ---------------------------------------------------------------------------
// Helper: read tool definitions
// ---------------------------------------------------------------------------

fn read_tools(
    list: capnp::struct_list::Reader<finch_ipc_capnp::tool_definition::Owned>,
) -> Result<Vec<crate::tools::types::ToolDefinition>, capnp::Error> {
    let mut out = Vec::with_capacity(list.len() as usize);
    for td in list.iter() {
        let schema: crate::tools::types::ToolInputSchema =
            serde_json::from_str(td.get_input_schema_json()?.to_str()?).unwrap_or_else(|_| {
                crate::tools::types::ToolInputSchema {
                    schema_type: "object".to_string(),
                    properties: serde_json::Value::Object(serde_json::Map::new()),
                    required: vec![],
                }
            });
        out.push(crate::tools::types::ToolDefinition {
            name: td.get_name()?.to_str()?.to_string(),
            description: td.get_description()?.to_str()?.to_string(),
            input_schema: schema,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Helper: write QueryResponse into capnp builder
// ---------------------------------------------------------------------------

fn write_query_response(
    mut builder: finch_ipc_capnp::query_response::Builder,
    text: &str,
    tool_uses: &[crate::tools::types::ToolUse],
    model: &str,
    input_tokens: Option<u32>,
    output_tokens: Option<u32>,
    latency_ms: Option<u64>,
) -> capnp::Result<()> {
    builder.set_text(text);
    builder.set_model(model);
    builder.set_input_tokens(input_tokens.unwrap_or(0));
    builder.set_output_tokens(output_tokens.unwrap_or(0));
    builder.set_latency_ms(latency_ms.unwrap_or(0));

    let mut tu_list = builder.init_tool_uses(tool_uses.len() as u32);
    for (i, tu) in tool_uses.iter().enumerate() {
        let mut t = tu_list.reborrow().get(i as u32);
        t.set_id(tu.id.as_str());
        t.set_name(tu.name.as_str());
        super::brain_codec::encode_json_value(t.reborrow().init_input(), &tu.input)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
    }
    Ok(())
}

async fn execute_typed_forth_ipc(program: String) -> Result<(Vec<i64>, String)> {
    let runtime = crate::runtime::ProgramRuntime::new();
    runtime.grant_typed_capability(crate::vm::CapabilityRequirement {
        capability: crate::vm::CapabilityKind::SessionEmit,
        selector: crate::vm::ResourceSelector::None,
    })?;
    let outcome = runtime
        .submit_typed_only(crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: Some("capnp:evalForth".to_string()),
            source: program,
            intent: "execute typed Co-Forth over the local IPC boundary".to_string(),
            effect: crate::programs::ExecutionEffect::Unclassified,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: None,
            budget: None,
        })
        .await?;
    if outcome.status != crate::runtime::outcome::ExecutionStatus::Completed {
        let diagnostic = outcome
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| format!("typed Co-Forth ended as {:?}", outcome.status));
        anyhow::bail!(diagnostic);
    }
    let stack = outcome
        .values
        .iter()
        .map(|value| match value {
            crate::programs::ProgramValue::Int(value) => Ok(*value),
            other => {
                anyhow::bail!("evalForth IPC supports only integer stack results; found {other:?}")
            }
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((stack, outcome.output))
}

// ---------------------------------------------------------------------------
// RPC method implementations
// ---------------------------------------------------------------------------

impl finch_daemon::Server for FinchDaemonImpl {
    // ---- query (non-streaming) -------------------------------------------

    fn query(
        &mut self,
        params: finch_daemon::QueryParams,
        mut results: finch_daemon::QueryResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let messages = pry!(
            super::brain_codec::decode_messages(pry!(p.get_messages()))
                .map_err(|error| capnp::Error::failed(error.to_string()))
        );
        let tools = pry!(read_tools(pry!(p.get_tools())));
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let provider = server
                .primary_provider()
                .ok_or_else(|| capnp::Error::failed("no provider configured".into()))?;

            let mut req = crate::providers::ProviderRequest::new(messages);
            if !tools.is_empty() {
                req = req.with_tools(tools);
            }

            let response = provider
                .send_message(&req)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))?;

            let tool_uses = response.tool_uses();
            write_query_response(
                results.get().init_response(),
                &response.text(),
                &tool_uses,
                &response.model,
                None,
                None,
                None,
            )?;
            Ok(())
        })
    }

    // ---- query_stream (streaming) ----------------------------------------

    fn query_stream(
        &mut self,
        params: finch_daemon::QueryStreamParams,
        _results: finch_daemon::QueryStreamResults,
    ) -> Promise<(), capnp::Error> {
        let p = pry!(params.get());
        let messages = pry!(
            super::brain_codec::decode_messages(pry!(p.get_messages()))
                .map_err(|error| capnp::Error::failed(error.to_string()))
        );
        let tools = pry!(read_tools(pry!(p.get_tools())));
        let receiver = pry!(p.get_receiver());
        let server = Arc::clone(&self.server);

        Promise::from_future(async move {
            let provider = server
                .primary_provider()
                .ok_or_else(|| capnp::Error::failed("no provider configured".into()))?;

            let mut req = crate::providers::ProviderRequest::new(messages);
            if !tools.is_empty() {
                req = req.with_tools(tools);
            }

            if !provider.supports_streaming() {
                // Fall back to blocking send; emit one text chunk then done.
                let response = provider
                    .send_message(&req)
                    .await
                    .map_err(|e| capnp::Error::failed(e.to_string()))?;
                let text = response.text();
                if !text.is_empty() {
                    let mut r = receiver.on_chunk_request();
                    r.get().init_chunk().set_text_delta(text.as_str());
                    r.send().promise.await?;
                }
                let mut r = receiver.on_chunk_request();
                r.get().init_chunk().set_done(());
                r.send().promise.await?;
                return Ok(());
            }

            let mut rx = provider
                .send_message_stream(&req)
                .await
                .map_err(|e| capnp::Error::failed(e.to_string()))?;

            use crate::generators::StreamChunk;
            while let Some(result) = rx.recv().await {
                match result {
                    Ok(StreamChunk::TextDelta(delta)) => {
                        let mut r = receiver.on_chunk_request();
                        r.get().init_chunk().set_text_delta(delta.as_str());
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::Usage { input_tokens }) => {
                        let mut r = receiver.on_chunk_request();
                        let mut upd = r.get().init_chunk().init_usage_update();
                        upd.set_input_tokens(input_tokens);
                        upd.set_output_tokens(0);
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::ContentBlockComplete(block)) => {
                        if let crate::claude::ContentBlock::ToolUse { id, name, input } = block {
                            let mut r = receiver.on_chunk_request();
                            let mut tu = r.get().init_chunk().init_tool_use_complete();
                            tu.set_id(id.as_str());
                            tu.set_name(name.as_str());
                            super::brain_codec::encode_json_value(
                                tu.reborrow().init_input(),
                                &input,
                            )
                            .map_err(|error| capnp::Error::failed(error.to_string()))?;
                            r.send().promise.await?;
                        }
                    }
                    Err(e) => {
                        let mut r = receiver.on_chunk_request();
                        r.get().init_chunk().set_error(e.to_string().as_str());
                        r.send().promise.await?;
                        return Ok(());
                    }
                }
            }

            // Done sentinel
            let mut r = receiver.on_chunk_request();
            r.get().init_chunk().set_done(());
            r.send().promise.await?;
            Ok(())
        })
    }

    // ---- Typed Co-Forth --------------------------------------------------

    fn eval_forth(
        &mut self,
        params: finch_daemon::EvalForthParams,
        mut results: finch_daemon::EvalForthResults,
    ) -> Promise<(), capnp::Error> {
        let program = pry!(pry!(params.get()).get_program())
            .to_str()
            .unwrap_or("")
            .to_owned();

        Promise::from_future(async move {
            let (stack, output) = execute_typed_forth_ipc(program)
                .await
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            let mut response = results.get();
            let mut list = response.reborrow().init_stack(stack.len() as u32);
            for (index, value) in stack.into_iter().enumerate() {
                list.set(index as u32, value);
            }
            response.reborrow().set_output(&output);
            response.set_error("");
            Ok(())
        })
    }

    fn register_brain_runner(
        &mut self,
        params: finch_daemon::RegisterBrainRunnerParams,
        mut results: finch_daemon::RegisterBrainRunnerResults,
    ) -> Promise<(), capnp::Error> {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let lease_text = pry!(params.get_lease_id())
            .to_str()
            .unwrap_or("")
            .to_string();
        let lease_uuid = match uuid::Uuid::parse_str(&lease_text) {
            Ok(value) => value,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let lease_id = crate::brain::store::RunnerLeaseId(lease_uuid);
        let runner = pry!(params.get_runner());
        if let Err(error) = self.server.brain_runners().require_connection_lease(
            self.connection_id,
            &brain,
            lease_id,
        ) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let snapshot = match self.server.brain_store().snapshot(&brain) {
            Ok(snapshot) => snapshot,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        if !snapshot
            .runner_lease
            .as_ref()
            .is_some_and(|lease| lease.lease_id == lease_id)
        {
            return Promise::err(capnp::Error::failed(
                "runner callback does not match the active lease".into(),
            ));
        }

        let (runtime_revision, checkpoint) =
            match self.server.brain_store().runner_checkpoint(&brain) {
                Ok(checkpoint) => checkpoint,
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let broker = self.server.brain_runners().clone();
        let server = Arc::clone(&self.server);
        let registration_id = match broker.register_for_connection(
            self.connection_id,
            brain.clone(),
            lease_id,
            tx,
        ) {
            Ok(registration_id) => registration_id,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let queued_lifecycle = crate::server::BrainLifecycleService::from_server(&server);
        let queued_brain = brain.clone();
        let registered_brain = brain.clone();
        tokio::task::spawn_local(async move {
            while let Some(request) = rx.recv().await {
                let runner = runner.clone();
                let server = Arc::clone(&server);
                let broker = broker.clone();
                let brain = registered_brain.clone();
                tokio::task::spawn_local(async move {
                    if forward_runner_request(runner, server, request).await {
                        broker.unregister(&brain, registration_id);
                    }
                });
            }
            broker.unregister(&registered_brain, registration_id);
        });
        // Return the registration bootstrap first. The frontend then marks
        // this lease active before the queued callback reaches its event loop.
        tokio::task::spawn_local(async move {
            tokio::task::yield_now().await;
            if let Err(error) = queued_lifecycle
                .resume_queued_runs(queued_brain.clone(), lease_id)
                .await
            {
                tracing::warn!(brain = %queued_brain, %error, "could not resume queued Brain runs");
            }
            if let Err(error) = queued_lifecycle
                .replay_committed_memory(queued_brain.clone(), lease_id)
                .await
            {
                tracing::warn!(
                    brain = %queued_brain,
                    %error,
                    "could not replay committed Brain memory"
                );
            }
        });
        let mut response = results.get();
        response.set_runtime_revision(runtime_revision);
        if let Err(error) = encode_checkpoint(response.reborrow().init_checkpoint(), &checkpoint) {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let control: finch_ipc_capnp::brain_runner_control::Client = capnp_rpc::new_client(
            BrainRunnerControlImpl {
                lifecycle: crate::server::BrainLifecycleService::from_server(&self.server),
                runners: self.server.brain_runners().clone(),
                connection_id: self.connection_id,
                brain: brain.clone(),
                lease_id,
            },
        );
        response.set_control(control);
        Promise::ok(())
    }

    fn brain_service(
        &mut self,
        _params: finch_daemon::BrainServiceParams,
        mut results: finch_daemon::BrainServiceResults,
    ) -> Promise<(), capnp::Error> {
        let service: brain_service::Client = capnp_rpc::new_client(BrainRpcService {
            lifecycle: crate::server::BrainLifecycleService::from_server(&self.server),
            runners: self.server.brain_runners().clone(),
            connection_id: self.connection_id,
        });
        results.get().set_service(service);
        Promise::ok(())
    }

    // ---- health ----------------------------------------------------------

    fn ping(
        &mut self,
        _params: finch_daemon::PingParams,
        mut results: finch_daemon::PingResults,
    ) -> Promise<(), capnp::Error> {
        results.get().set_version(env!("CARGO_PKG_VERSION"));
        results
            .get()
            .set_protocol_version(crate::ipc::IPC_PROTOCOL_VERSION);
        Promise::ok(())
    }
}

fn program_language_to_capnp(
    language: crate::brain::store::ProgramLanguage,
) -> finch_ipc_capnp::ProgramLanguage {
    match language {
        crate::brain::store::ProgramLanguage::Forth => finch_ipc_capnp::ProgramLanguage::Forth,
        crate::brain::store::ProgramLanguage::Lisp => finch_ipc_capnp::ProgramLanguage::Lisp,
    }
}

fn attachment_role_from_capnp(
    role: finch_ipc_capnp::BrainAttachmentRole,
) -> crate::brain::store::AttachmentRole {
    match role {
        finch_ipc_capnp::BrainAttachmentRole::Runner => {
            crate::brain::store::AttachmentRole::Runner
        }
        finch_ipc_capnp::BrainAttachmentRole::Driver => {
            crate::brain::store::AttachmentRole::Driver
        }
        finch_ipc_capnp::BrainAttachmentRole::Consultant => {
            crate::brain::store::AttachmentRole::Consultant
        }
        finch_ipc_capnp::BrainAttachmentRole::Observer => {
            crate::brain::store::AttachmentRole::Observer
        }
    }
}

fn program_language_from_capnp(
    language: finch_ipc_capnp::ProgramLanguage,
) -> crate::brain::store::ProgramLanguage {
    match language {
        finch_ipc_capnp::ProgramLanguage::Forth => crate::brain::store::ProgramLanguage::Forth,
        finch_ipc_capnp::ProgramLanguage::Lisp => crate::brain::store::ProgramLanguage::Lisp,
    }
}

fn decode_schedule_policy(
    policy: capnp::Result<finch_ipc_capnp::brain_schedule_delivery_policy::Reader<'_>>,
) -> capnp::Result<crate::brain::store::BrainScheduleDeliveryPolicy> {
    let policy = policy?;
    match policy.get_kind()? {
        finch_ipc_capnp::BrainSchedulePolicyKind::Coalesce => {
            Ok(crate::brain::store::BrainScheduleDeliveryPolicy::Coalesce)
        }
        finch_ipc_capnp::BrainSchedulePolicyKind::BoundedCatchUp => Ok(
            crate::brain::store::BrainScheduleDeliveryPolicy::BoundedCatchUp {
                max_catch_up: policy.get_max_catch_up(),
                expires_after_ms: policy.get_expires_after_ms(),
            },
        ),
    }
}

async fn forward_runner_request(
    runner: finch_ipc_capnp::brain_runner::Client,
    server: Arc<AgentServer>,
    request: crate::server::RunnerRequest,
) -> bool {
    match request {
        crate::server::RunnerRequest::Program(request) => {
            let mut call = runner.run_program_request();
            {
                let mut payload = call.get().init_request();
                payload.set_brain(&request.brain);
                payload.set_run_id(&request.run_id.0.to_string());
                payload.set_request_seq(request.request_seq);
                payload.set_language(program_language_to_capnp(request.language));
                payload.set_source(&request.source);
                payload.set_interaction(match request.interaction {
                    crate::server::RunnerProgramInteraction::Interactive => {
                        finch_ipc_capnp::BrainProgramInteraction::Interactive
                    }
                    crate::server::RunnerProgramInteraction::Noninteractive => {
                        finch_ipc_capnp::BrainProgramInteraction::Noninteractive
                    }
                });
                payload.set_has_grant_ceiling(request.grant_ceiling.is_some());
                if let Some(grant_ceiling) = &request.grant_ceiling {
                    crate::ipc::checkpoint_codec::encode_effects(
                        payload
                            .reborrow()
                            .init_grant_ceiling(grant_ceiling.0.len() as u32),
                        grant_ceiling,
                    );
                }
                let control: finch_ipc_capnp::brain_program_control::Client =
                    capnp_rpc::new_client(BrainProgramControlImpl {
                        lifecycle: crate::server::BrainLifecycleService::from_server(&server),
                        brain: request.brain.clone(),
                        run_id: request.run_id,
                        request_seq: request.request_seq,
                        maximum_grant_ceiling: request.grant_ceiling.clone(),
                    });
                payload.set_control(control);
            }
            let (result, disconnected) = match call.send().promise.await {
                Ok(reply) => (
                    decode_runner_program_result(reply.get().and_then(|r| r.get_result())),
                    false,
                ),
                Err(error) => (Err(error.to_string().into()), true),
            };
            let _ = request.response_tx.send(result);
            disconnected
        }
        crate::server::RunnerRequest::Turn(request) => {
            let (result, disconnected) = {
                let mut call = runner.run_turn_request();
                let encoded = {
                    let mut payload = call.get().init_request();
                    payload.set_brain(&request.brain);
                    payload.set_run_id(&request.run_id.0.to_string());
                    payload.set_request_seq(request.request_seq);
                    payload.set_prompt(&request.prompt);
                    let encoded = super::brain_codec::encode_messages(
                        payload
                            .reborrow()
                            .init_context(request.context.len() as u32),
                        &request.context,
                    )
                    .map_err(|error| error.to_string());
                    if encoded.is_ok() {
                        encode_approval_audience(
                            payload.reborrow().init_approval_audience(),
                            &request.approval_audience,
                        );
                        let control: finch_ipc_capnp::brain_turn_control::Client =
                            capnp_rpc::new_client(BrainTurnControlImpl {
                                server,
                                brain: request.brain.clone(),
                                request_seq: request.request_seq,
                                expected_audience: request.approval_audience.clone(),
                                expected_connection_id: request.approval_connection_id,
                            });
                        payload.set_control(control);
                    }
                    encoded
                };
                match encoded {
                    Ok(()) => match call.send().promise.await {
                        Ok(reply) => (
                            decode_runner_turn_result(reply.get().and_then(|r| r.get_result())),
                            false,
                        ),
                        // A transport failure is not a runner-authored turn
                        // failure. Drop the response sender so the broker's
                        // exact registration disconnect path remains distinct
                        // from RunnerTurnError persistence.
                        Err(_) => return true,
                    },
                    Err(error) => (Err(error.into()), false),
                }
            };
            let _ = request.response_tx.send(result);
            disconnected
        }
        crate::server::RunnerRequest::ProjectMemory(request) => {
            let mut call = runner.project_memory_request();
            {
                let mut payload = call.get().init_request();
                payload.set_brain_id(&request.brain_id.0.to_string());
                payload.set_brain(&request.brain);
                payload.set_run_id(&request.run_id.0.to_string());
                payload.set_request_seq(request.request_seq);
                payload.set_prompt(&request.prompt);
                payload.set_source(&request.source);
            }
            let (result, disconnected) = match call.send().promise.await {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = reply
                            .get_error()
                            .ok()
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("");
                        if error.is_empty() {
                            (Ok(reply.get_inserted() as usize), false)
                        } else {
                            (Err(error.to_string()), false)
                        }
                    }
                    Err(error) => (Err(error.to_string()), false),
                },
                Err(error) => (Err(error.to_string()), true),
            };
            let _ = request.response_tx.send(result);
            disconnected
        }
        crate::server::RunnerRequest::Cancel(request) => {
            let mut call = runner.cancel_run_request();
            call.get().set_brain(&request.brain);
            call.get().set_run_id(&request.run_id.0.to_string());
            let (result, disconnected) = match call.send().promise.await {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = reply
                            .get_error()
                            .ok()
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("");
                        if error.is_empty() {
                            (Ok(reply.get_cancelled()), false)
                        } else {
                            (Err(error.to_string()), false)
                        }
                    }
                    Err(error) => (Err(error.to_string()), false),
                },
                Err(error) => (Err(error.to_string()), true),
            };
            let _ = request.response_tx.send(result);
            disconnected
        }
    }
}

#[cfg(test)]
pub(crate) async fn forward_test_runner_request(
    runner: finch_ipc_capnp::brain_runner::Client,
    server: Arc<AgentServer>,
    request: crate::server::RunnerRequest,
) -> bool {
    forward_runner_request(runner, server, request).await
}

fn decode_runner_program_result(
    result: capnp::Result<finch_ipc_capnp::brain_program_result::Reader<'_>>,
) -> Result<crate::server::RunnerProgramResult, crate::server::RunnerProgramError> {
    let result = result.map_err(|error| error.to_string())?;
    let effect_journal = decode_runner_effect_records(
        result
            .get_effect_journal()
            .map_err(|error| error.to_string())?,
    )?;
    let error = result
        .get_error()
        .ok()
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !error.is_empty() {
        return Err(crate::server::RunnerProgramError {
            message: error.to_string(),
            effect_journal,
        });
    }
    let checkpoint = decode_checkpoint(result.get_checkpoint().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(crate::server::RunnerProgramResult {
        output: result
            .get_output()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
        effect_journal,
    })
}

fn decode_runner_turn_result(
    result: capnp::Result<finch_ipc_capnp::brain_turn_result::Reader<'_>>,
) -> Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError> {
    let result = result.map_err(|error| error.to_string())?;
    let error = result
        .get_error()
        .ok()
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let mut turn_events = Vec::new();
    let encoded_turn_events = result
        .get_turn_events()
        .map_err(|error| error.to_string())?;
    for encoded in encoded_turn_events.iter() {
        turn_events.push(decode_runner_turn_event(encoded)?);
    }
    let effect_journal = decode_runner_effect_records(
        result
            .get_effect_journal()
            .map_err(|error| error.to_string())?,
    )?;
    if !error.is_empty() {
        let kind = match result.get_error_kind().map_err(|error| error.to_string())? {
            finch_ipc_capnp::BrainTurnErrorKind::RunnerAuthored => {
                crate::server::RunnerTurnErrorKind::RunnerAuthored
            }
            finch_ipc_capnp::BrainTurnErrorKind::InfrastructureProviderTaskTerminated => {
                crate::server::RunnerTurnErrorKind::InfrastructureProviderTaskTerminated
            }
            finch_ipc_capnp::BrainTurnErrorKind::RunCancelled => {
                crate::server::RunnerTurnErrorKind::RunCancelled
            }
        };
        return Err(crate::server::RunnerTurnError {
            kind,
            message: error.to_string(),
            turn_events,
            effect_journal,
        });
    }
    let checkpoint = decode_checkpoint(result.get_checkpoint().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let commit_ack = if result.get_has_commit_ack() {
        let capability = result.get_commit_ack().map_err(|error| error.to_string())?;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<
            crate::server::RunnerTurnCommitNotice,
        >();
        tokio::task::spawn_local(async move {
            while let Some(notice) = rx.recv().await {
                let mut call = capability.committed_request();
                call.get().set_status(crate::ipc::brain_codec::run_status_to_capnp(
                    notice.status,
                ));
                call.get().set_detail(&notice.detail);
                if let Err(error) = call.send().promise.await {
                    tracing::warn!(%error, "could not acknowledge committed Brain turn to runner");
                    break;
                }
            }
        });
        Some(crate::server::RunnerTurnCommitAck::new(tx))
    } else {
        None
    };
    Ok(crate::server::RunnerTurnResult {
        source: result
            .get_source()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        language: program_language_from_capnp(
            result.get_language().map_err(|error| error.to_string())?,
        ),
        output: result
            .get_output()
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string(),
        turn_events,
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
        effect_journal,
        commit_ack,
    })
}

fn decode_runner_effect_records(
    encoded: capnp::struct_list::Reader<'_, finch_ipc_capnp::brain_effect_record::Owned>,
) -> Result<Vec<crate::server::RunnerEffectRecord>, String> {
    encoded
        .iter()
        .map(|record| {
            let (execution_id, entry) =
                crate::ipc::checkpoint_codec::decode_effect_record(record)
                    .map_err(|error| error.to_string())?;
            Ok(crate::server::RunnerEffectRecord {
                execution_id,
                entry,
            })
        })
        .collect()
}

fn decode_runner_turn_event(
    encoded: finch_ipc_capnp::brain_turn_event::Reader<'_>,
) -> Result<crate::server::RunnerTurnEvent, String> {
    let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
        value
            .ok()
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let tool_id = text(encoded.get_tool_id());
    match encoded.get_kind().map_err(|error| error.to_string())? {
        finch_ipc_capnp::BrainTurnEventKind::Call => Ok(crate::server::RunnerTurnEvent::Call {
            tool_id,
            name: text(encoded.get_name()),
            input: super::brain_codec::decode_json_value(
                encoded.get_input().map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        }),
        finch_ipc_capnp::BrainTurnEventKind::Result => Ok(crate::server::RunnerTurnEvent::Result {
            tool_id,
            output: text(encoded.get_output()),
            is_error: encoded.get_is_error(),
        }),
        finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested => {
            Ok(crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: text(encoded.get_approval_id()),
                approval_kind: text(encoded.get_approval_kind()),
                subject: text(encoded.get_subject()),
                audience: decode_approval_audience(
                    encoded
                        .get_approval_audience()
                        .map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
                detail: super::brain_codec::decode_json_value(
                    encoded.get_detail().map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            })
        }
        finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided => {
            Ok(crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id: text(encoded.get_approval_id()),
                decision: super::brain_codec::decode_json_value(
                    encoded.get_decision().map_err(|error| error.to_string())?,
                )
                .map_err(|error| error.to_string())?,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        decode_runner_program_result, decode_runner_turn_result, execute_typed_forth_ipc,
        require_approval_connection, BrainRpcService, BrainRunnerControlImpl,
    };
    use crate::ipc::brain_codec::encode_approval_audience;

    struct SocketApprovalRunner {
        failed_tx: Option<tokio::sync::oneshot::Sender<String>>,
    }

    struct HeldTurnRunner {
        started: Option<tokio::sync::oneshot::Sender<()>>,
        release: Option<tokio::sync::oneshot::Receiver<()>>,
    }

    struct ImmediateTurnRunner;

    fn successful_turn(
        mut results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
    ) -> capnp::Result<()> {
        let checkpoint = crate::runtime::ProgramRuntime::new()
            .revision_history()
            .map_err(|error| capnp::Error::failed(error.to_string()))?
            .pop()
            .and_then(|revision| revision.checkpoint)
            .ok_or_else(|| capnp::Error::failed("test runtime has no checkpoint".into()))?;
        let mut result = results.get().init_result();
        result.set_source("(say \"successor\")");
        result.set_language(super::finch_ipc_capnp::ProgramLanguage::Lisp);
        result.set_output("successor");
        result.set_runtime_revision(0);
        super::encode_checkpoint(result.reborrow().init_checkpoint(), &checkpoint)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        result.set_error("");
        result.set_error_kind(super::finch_ipc_capnp::BrainTurnErrorKind::RunnerAuthored);
        Ok(())
    }

    macro_rules! turn_only_methods {
        () => {
            fn run_program(&mut self, _: super::finch_ipc_capnp::brain_runner::RunProgramParams, _: super::finch_ipc_capnp::brain_runner::RunProgramResults) -> capnp::capability::Promise<(), capnp::Error> {
                capnp::capability::Promise::err(capnp::Error::unimplemented("turn-only test runner".into()))
            }
            fn cancel_run(&mut self, _: super::finch_ipc_capnp::brain_runner::CancelRunParams, _: super::finch_ipc_capnp::brain_runner::CancelRunResults) -> capnp::capability::Promise<(), capnp::Error> {
                capnp::capability::Promise::ok(())
            }
            fn project_memory(&mut self, _: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams, _: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults) -> capnp::capability::Promise<(), capnp::Error> {
                capnp::capability::Promise::ok(())
            }
        };
    }

    impl super::finch_ipc_capnp::brain_runner::Server for HeldTurnRunner {
        turn_only_methods!();
        fn run_turn(&mut self, _: super::finch_ipc_capnp::brain_runner::RunTurnParams, results: super::finch_ipc_capnp::brain_runner::RunTurnResults) -> capnp::capability::Promise<(), capnp::Error> {
            let started = self.started.take().expect("held runner called twice");
            let release = self.release.take().expect("held runner called twice");
            capnp::capability::Promise::from_future(async move {
                let _ = started.send(());
                let _ = release.await;
                successful_turn(results)
            })
        }
    }

    impl super::finch_ipc_capnp::brain_runner::Server for ImmediateTurnRunner {
        turn_only_methods!();
        fn run_turn(&mut self, _: super::finch_ipc_capnp::brain_runner::RunTurnParams, results: super::finch_ipc_capnp::brain_runner::RunTurnResults) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::from_future(async move { successful_turn(results) })
        }
    }

    #[test]
    fn raw_runner_eof_fences_held_success_and_successor_completes_next_turn() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                let temp = tempfile::tempdir().unwrap();
                let store = crate::brain::store::BrainStore::with_root("box.local", Some(temp.path().join("brains")));
                let server = std::sync::Arc::new(crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::credential::BrainCredentialAuthority::ephemeral([93; 32]),
                    "test-password".into(),
                    temp.path(),
                ).unwrap());
                async fn connect(server: std::sync::Arc<crate::server::AgentServer>) -> (crate::ipc::IpcClient, tokio::task::JoinHandle<anyhow::Result<()>>) {
                    let (client_stream, server_stream) = tokio::net::UnixStream::pair().unwrap();
                    let server_task = tokio::task::spawn_local(async move { super::handle_connection(server_stream, server).await });
                    let client = crate::ipc::IpcClient::connect_test_stream(client_stream).await.unwrap();
                    (client, server_task)
                }
                let (driver, driver_server) = connect(std::sync::Arc::clone(&server)).await;
                let attachment = driver.brain_attach("shared", "driver", crate::brain::store::AttachmentRole::Driver, None).await.unwrap();
                let mut watch = driver.brain_watch("shared", &attachment).await.unwrap();
                watch.recv().await.unwrap().unwrap();

                let (old_runner, old_server) = connect(std::sync::Arc::clone(&server)).await;
                let environment = old_runner.brain_snapshot("shared").await.unwrap().environment;
                old_runner.brain_claim_runner_identity("runner@box.local/raw").await.unwrap();
                let lease = old_runner.brain_acquire_runner("shared", "runner@box.local/raw", &environment, None, 60_000).await.unwrap();
                let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                let (release_tx, release_rx) = tokio::sync::oneshot::channel();
                old_runner.register_test_brain_runner_client(
                    "shared",
                    lease.lease_id,
                    capnp_rpc::new_client(HeldTurnRunner { started: Some(started_tx), release: Some(release_rx) }),
                ).await.unwrap();
                let first_driver = driver.clone();
                let first_attachment = attachment.clone();
                let first = tokio::task::spawn_local(async move {
                    first_driver.brain_submit("shared", &first_attachment, crate::brain::store::BrainEventKind::Prompt { text: "held old response".into() }).await
                });
                started_rx.await.unwrap();
                let first_run = store.snapshot("shared").unwrap().runs[0].run_id;
                old_runner.abort_test_transport_without_frontend_cancel();
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    while !old_server.is_finished() {
                        tokio::task::yield_now().await;
                    }
                }).await.expect("raw runner EOF did not reach the daemon");

                let (successor, successor_server) = connect(std::sync::Arc::clone(&server)).await;
                successor.brain_claim_runner_identity("runner@box.local/raw").await.unwrap();
                let successor_lease = successor.brain_acquire_runner(
                    "shared",
                    "runner@box.local/raw",
                    &environment,
                    Some(lease.lease_id),
                    60_000,
                ).await.unwrap();
                assert_eq!(successor_lease.lease_id, lease.lease_id);
                successor.register_test_brain_runner_client("shared", successor_lease.lease_id, capnp_rpc::new_client(ImmediateTurnRunner)).await.unwrap();
                assert!(
                    release_tx.send(()).is_err(),
                    "old callback still accepted a reply after raw transport loss"
                );
                let _ = first.await.unwrap();
                let snapshot = store.snapshot("shared").unwrap();
                assert_eq!(store.inspect_run("shared", first_run).unwrap().status, crate::brain::store::BrainRunStatus::Cancelled);
                assert!(snapshot.events.iter().all(|event| event.run_id != Some(first_run) || !matches!(&event.kind,
                    crate::brain::store::BrainEventKind::Program { .. }
                    | crate::brain::store::BrainEventKind::RuntimeCommitted { .. }
                    | crate::brain::store::BrainEventKind::EffectRecorded { .. }
                    | crate::brain::store::BrainEventKind::Result { .. }
                )));
                assert_eq!(snapshot.events.iter().filter(|event| {
                    matches!(&event.kind, crate::brain::store::BrainEventKind::RunStatusChanged {
                        run_id,
                        status,
                        ..
                    } if *run_id == first_run && status.is_terminal())
                }).count(), 1);
                driver.brain_submit("shared", &attachment, crate::brain::store::BrainEventKind::Prompt { text: "successor turn".into() }).await.unwrap();
                assert!(store.snapshot("shared").unwrap().runs.iter().any(|run| run.run_id != first_run && run.status == crate::brain::store::BrainRunStatus::Completed));
                drop(successor);
                drop(driver);
                old_server.abort();
                successor_server.abort();
                driver_server.abort();
            }).await.expect("raw EOF successor race exceeded its bounded deadline");
        }));
    }

    impl super::finch_ipc_capnp::brain_runner::Server for SocketApprovalRunner {
        fn run_program(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented(
                "socket approval runner accepts only turns".into(),
            ))
        }

        fn run_turn(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let request = match params.get().and_then(|params| params.get_request()) {
                Ok(request) => request,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let request_seq = request.get_request_seq();
            let audience = match request.get_approval_audience()
                .map_err(anyhow::Error::new)
                .and_then(super::decode_approval_audience)
            {
                Ok(audience) => audience,
                Err(error) => return capnp::capability::Promise::err(
                    capnp::Error::failed(error.to_string()),
                ),
            };
            let control = match request.get_control() {
                Ok(control) => control,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let failed_tx = self.failed_tx.take().expect("runner received more than one turn");
            capnp::capability::Promise::from_future(async move {
                let mut call = control.request_approval_request();
                crate::ipc::client::encode_brain_turn_event(
                    call.get().init_event(),
                    &crate::server::RunnerTurnEvent::ApprovalRequested {
                        approval_id: "socket-approval".into(),
                        approval_kind: "tool".into(),
                        subject: "bash".into(),
                        audience,
                        detail: serde_json::json!({"input": {"command": "true"}}),
                    },
                )?;
                let error = match call.send().promise.await {
                    Ok(_) => "approval unexpectedly succeeded".to_string(),
                    Err(error) => error.to_string(),
                };
                let _ = failed_tx.send(error.clone());
                Err(capnp::Error::failed(format!(
                    "approval for request {request_seq} failed closed: {error}"
                )))
            })
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented(
                "socket approval runner does not cancel".into(),
            ))
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented(
                "socket approval runner does not project memory".into(),
            ))
        }
    }

    #[test]
    fn unix_socket_disconnect_fails_reverse_approval_for_exact_attachment_generation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let socket_path = temp.path().join("finch.sock");
            let _socket_path = crate::ipc::transport::set_test_sock_path(socket_path.clone());
            let store = crate::brain::store::BrainStore::with_root(
                "box.local", Some(temp.path().join("brains")),
            );
            let server = std::sync::Arc::new(crate::server::AgentServer::for_brain_protocol_test(
                store.clone(),
                crate::brain::credential::BrainCredentialAuthority::ephemeral([91; 32]),
                "test-password".into(),
                temp.path(),
            ).unwrap());
            let shutdown = tokio_util::sync::CancellationToken::new();
            let server_task = tokio::task::spawn_local(super::start_ipc_server(
                server.clone(), shutdown.clone(),
            ));
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                while !socket_path.exists() {
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();

            let participant = crate::ipc::IpcClient::connect_path(socket_path.clone()).await.unwrap();
            let attachment = participant.brain_attach(
                "shared", "alice", crate::brain::store::AttachmentRole::Driver, None,
            ).await.unwrap();
            let mut participant_events = participant.brain_watch("shared", &attachment).await.unwrap();
            participant_events.recv().await.unwrap().unwrap();

            let runner = crate::ipc::IpcClient::connect_path(socket_path.clone()).await.unwrap();
            let snapshot = runner.brain_snapshot("shared").await.unwrap();
            runner.brain_claim_runner_identity("runner@box.local/socket").await.unwrap();
            let lease = runner.brain_acquire_runner(
                "shared", "runner@box.local/socket", &snapshot.environment, None, 60_000,
            ).await.unwrap();
            let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
            let callback: super::finch_ipc_capnp::brain_runner::Client =
                capnp_rpc::new_client(SocketApprovalRunner { failed_tx: Some(failed_tx) });
            runner.register_test_brain_runner_client("shared", lease.lease_id, callback)
                .await.unwrap();

            let run = participant.brain_start_speculative(
                "shared", &attachment, "request approval".into(),
            ).await.unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                loop {
                    let current = store.snapshot("shared").unwrap();
                    if current.events.iter().any(|event| matches!(
                        &event.kind,
                        crate::brain::store::BrainEventKind::ApprovalRequested {
                            approval_id, ..
                        } if approval_id == "socket-approval"
                    )) {
                        assert_eq!(store.inspect_run("shared", run.run_id).unwrap().status,
                            crate::brain::store::BrainRunStatus::AwaitingApproval);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }).await.expect("reverse approval did not become durable");

            let old_connection = attachment.connection_id.unwrap();
            drop(participant_events);
            drop(participant);
            let error = tokio::time::timeout(
                std::time::Duration::from_millis(250), failed_rx,
            ).await.expect("physical IPC loss did not fail approval promptly").unwrap();
            assert!(error.contains("approval audience disconnected"), "{error}");
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                while store.require_connection(
                    "shared", attachment.attachment_id, old_connection,
                ).is_ok() {
                    tokio::task::yield_now().await;
                }
            }).await.expect("physical IPC loss did not detach exact generation");

            let replacement = crate::ipc::IpcClient::connect_path(socket_path).await.unwrap();
            let replacement_attachment = replacement.brain_attach(
                "shared", "alice", crate::brain::store::AttachmentRole::Driver,
                Some(attachment.attachment_id),
            ).await.unwrap();
            let replacement_connection = replacement_attachment.connection_id.unwrap();
            let mut replacement_events = replacement.brain_watch(
                "shared", &replacement_attachment,
            ).await.unwrap();
            replacement_events.recv().await.unwrap().unwrap();
            assert!(store.require_connection(
                "shared", replacement_attachment.attachment_id, replacement_connection,
            ).is_ok());
            assert_eq!(store.snapshot("shared").unwrap().runner_lease, Some(lease));
            let terminal = store.snapshot("shared").unwrap();
            assert_eq!(terminal.events.iter().filter(|event| matches!(
                event.kind,
                crate::brain::store::BrainEventKind::RunStatusChanged {
                    run_id, status, ..
                } if run_id == run.run_id && status.is_terminal()
            )).count(), 1);

            drop(replacement_events);
            drop(replacement);
            drop(runner);
            shutdown.cancel();
            server_task.await.unwrap().unwrap();
        }));
    }

    #[test]
    fn restored_connectionless_turn_fails_closed_if_it_requests_approval() {
        let result = require_approval_connection(None);
        let error = match result {
            Ok(_) => panic!("connectionless restored turn registered an approval"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(
            "approval audience has no live connection generation"
        ));
    }

    struct BrainTestDaemon {
        lifecycle: crate::server::BrainLifecycleService,
        runners: crate::server::BrainRunnerBroker,
        connection_id: uuid::Uuid,
    }

    impl super::finch_ipc_capnp::finch_daemon::Server for BrainTestDaemon {
        fn brain_service(
            &mut self,
            _params: super::finch_ipc_capnp::finch_daemon::BrainServiceParams,
            mut results: super::finch_ipc_capnp::finch_daemon::BrainServiceResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let service: super::finch_ipc_capnp::brain_service::Client =
                capnp_rpc::new_client(BrainRpcService {
                    lifecycle: self.lifecycle.clone(),
                    runners: self.runners.clone(),
                    connection_id: self.connection_id,
                });
            results.get().set_service(service);
            capnp::capability::Promise::ok(())
        }
    }

    fn test_approval_audience() -> crate::brain::store::BrainApprovalAudience {
        crate::brain::store::BrainApprovalAudience {
            brain_id: crate::brain::store::BrainId(
                uuid::Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
            ),
            brain: "shared".into(),
            attachment_id: crate::brain::store::AttachmentId(
                uuid::Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap(),
            ),
            subject: "alice@box.local".into(),
            role: crate::brain::store::AttachmentRole::Driver,
            environment_generation: 3,
        }
    }

    fn effect_record() -> crate::server::RunnerEffectRecord {
        crate::server::RunnerEffectRecord {
            execution_id: uuid::Uuid::new_v4(),
            entry: crate::vm::EffectJournalEntry {
                effect: crate::vm::VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 3,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    event: crate::vm::HostSideEffect::Emit {
                        text: "done".into(),
                    },
                    output: Vec::new(),
                    origin: crate::vm::SourceOrigin::generated("say"),
                },
                state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
            },
        }
    }

    #[tokio::test]
    async fn runner_lifecycle_capability_rejects_a_replaced_lease() {
        let root = tempfile::tempdir().unwrap().keep();
        let store = crate::brain::store::BrainStore::with_root("box.local", Some(root));
        let runners = crate::server::BrainRunnerBroker::default();
        let lifecycle = crate::server::BrainLifecycleService::new(
            store.clone(),
            runners.clone(),
            crate::server::BrainApprovalBroker::default(),
        );
        lifecycle.create("shared").await.unwrap();
        let _driver = lifecycle
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let environment = store.environment().clone();
        let first = lifecycle
            .acquire_runner("shared", "runner-one", &environment, None, 60_000)
            .unwrap();
        let connection_id = uuid::Uuid::new_v4();
        runners
            .claim_connection_lease(connection_id, "shared", first.lease_id)
            .unwrap();
        let control = BrainRunnerControlImpl {
            lifecycle: lifecycle.clone(),
            runners: runners.clone(),
            connection_id,
            brain: "shared".into(),
            lease_id: first.lease_id,
        };
        control.validate_lease().unwrap();

        lifecycle.release_runner("shared", first.lease_id).unwrap();
        let replacement = lifecycle
            .acquire_runner("shared", "runner-two", &environment, None, 60_000)
            .unwrap();
        assert_ne!(replacement.lease_id, first.lease_id);
        let error = control.validate_lease().unwrap_err();
        assert!(error.to_string().contains("active lease"));
    }

    #[tokio::test]
    async fn disconnected_ipc_connection_rebinds_its_durable_runner_lease() {
        let root = tempfile::tempdir().unwrap().keep();
        let store = crate::brain::store::BrainStore::with_root("box.local", Some(root));
        let runners = crate::server::BrainRunnerBroker::default();
        let lifecycle = crate::server::BrainLifecycleService::new(
            store.clone(),
            runners.clone(),
            crate::server::BrainApprovalBroker::default(),
        );
        lifecycle.create("shared").await.unwrap();
        let environment = store.environment().clone();
        let subject = "runner@box.local/frontend-stable";

        let first_connection = uuid::Uuid::new_v4();
        runners
            .claim_connection_identity(first_connection, subject)
            .unwrap();
        let first = BrainRpcService {
            lifecycle: lifecycle.clone(),
            runners: runners.clone(),
            connection_id: first_connection,
        };
        let lease = first
            .acquire_connection_runner("shared", subject, &environment, None, 60_000)
            .unwrap();
        runners.disconnect_connection(first_connection);

        let replacement_connection = uuid::Uuid::new_v4();
        runners
            .claim_connection_identity(replacement_connection, subject)
            .unwrap();
        let replacement = BrainRpcService {
            lifecycle,
            runners: runners.clone(),
            connection_id: replacement_connection,
        };
        let renewed = replacement
            .acquire_connection_runner(
                "shared",
                subject,
                &environment,
                Some(lease.lease_id),
                60_000,
            )
            .unwrap();
        assert_eq!(renewed.lease_id, lease.lease_id);

        let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
        runners
            .register_for_connection(
                replacement_connection,
                "shared",
                renewed.lease_id,
                callback_tx,
            )
            .unwrap();
        assert!(runners.has_registration("shared", renewed.lease_id));
    }

    #[test]
    fn local_initialization_clients_require_their_active_driver_connection() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let store = crate::brain::store::BrainStore::with_root("box.local", None);
            let runners = crate::server::BrainRunnerBroker::default();
            let lifecycle = crate::server::BrainLifecycleService::new(
                store,
                runners.clone(),
                crate::server::BrainApprovalBroker::default(),
            );
            let daemon: super::finch_ipc_capnp::finch_daemon::Client =
                capnp_rpc::new_client(BrainTestDaemon {
                    lifecycle,
                    runners,
                    connection_id: uuid::Uuid::new_v4(),
                });
            let ipc = crate::ipc::IpcClient::from_test_client(daemon);
            let target = crate::brain::remote::RemoteBrainTarget::local(
                "shared",
                "127.0.0.1:1",
            )
            .unwrap();
            let mut driver = crate::brain::remote::AttachedBrainClient::local(
                target.clone(),
                ipc.clone(),
            );
            driver
                .attach(
                    "alice",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut events = driver.watch().await.unwrap();
            assert!(matches!(
                events.recv().await.unwrap(),
                crate::brain::store::BrainWireMessage::Snapshot { .. }
            ));
            assert!(driver
                .schedule_initialization(1_000)
                .await
                .unwrap()
                .module_identity
                .is_some());

            driver.disconnect().await.unwrap();
            assert!(driver.schedule_initialization(2_000).await.is_err());

            let mut consultant = crate::brain::remote::AttachedBrainClient::local(target, ipc);
            consultant
                .attach(
                    "bob",
                    crate::brain::store::AttachmentRole::Consultant,
                    None,
                )
                .await
                .unwrap();
            let mut consultant_events = consultant.watch().await.unwrap();
            assert!(matches!(
                consultant_events.recv().await.unwrap(),
                crate::brain::store::BrainWireMessage::Snapshot { .. }
            ));
            assert!(consultant.schedule_initialization(3_000).await.is_err());
        }));
    }

    #[test]
    fn runner_turn_result_decodes_ordered_capnp_lifecycle() {
        let expected_effect = effect_record();
        let runtime = crate::runtime::ProgramRuntime::new();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
            result.set_source("(say \"done\")");
            result.set_language(super::finch_ipc_capnp::ProgramLanguage::Lisp);
            result.set_output("done");
            result.set_runtime_revision(1);
            super::encode_checkpoint(result.reborrow().init_checkpoint(), &checkpoint).unwrap();
            result.set_error("");
            super::super::checkpoint_codec::encode_effect_record(
                result.reborrow().init_effect_journal(1).get(0),
                expected_effect.execution_id,
                &expected_effect.entry,
            )
            .unwrap();
            let mut events = result.init_turn_events(4);
            let mut call = events.reborrow().get(0);
            call.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Call);
            call.set_tool_id("tool-1");
            call.set_name("search_word");
            super::super::brain_codec::encode_json_value(
                call.reborrow().init_input(),
                &serde_json::json!({"query": "fib"}),
            )
            .unwrap();
            let mut approval = events.reborrow().get(1);
            approval.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested);
            approval.set_approval_id("tool-1");
            approval.set_approval_kind("tool");
            approval.set_subject("search_word");
            encode_approval_audience(
                approval.reborrow().init_approval_audience(),
                &test_approval_audience(),
            );
            super::super::brain_codec::encode_json_value(
                approval.reborrow().init_detail(),
                &serde_json::json!({"input": {"query": "fib"}}),
            )
            .unwrap();
            let mut decision = events.reborrow().get(2);
            decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            decision.set_approval_id("tool-1");
            super::super::brain_codec::encode_json_value(
                decision.reborrow().init_decision(),
                &serde_json::json!({"choice": "approve_once"}),
            )
            .unwrap();
            let mut tool_result = events.reborrow().get(3);
            tool_result.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::Result);
            tool_result.set_tool_id("tool-1");
            tool_result.set_output("found");
            tool_result.set_is_error(false);
        }

        let reader = message
            .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
            .unwrap();
        let decoded = decode_runner_turn_result(Ok(reader)).unwrap();
        assert_eq!(
            decoded.turn_events,
            vec![
                crate::server::RunnerTurnEvent::Call {
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
                crate::server::RunnerTurnEvent::ApprovalRequested {
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: test_approval_audience(),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
                crate::server::RunnerTurnEvent::ApprovalDecided {
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
                crate::server::RunnerTurnEvent::Result {
                    tool_id: "tool-1".into(),
                    output: "found".into(),
                    is_error: false,
                },
            ]
        );
        assert_eq!(decoded.effect_journal, vec![expected_effect]);
    }

    #[test]
    fn runner_turn_error_keeps_partial_lifecycle() {
        let expected_effect = effect_record();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
            result.set_error("provider failed after approval");
            result.set_error_kind(super::finch_ipc_capnp::BrainTurnErrorKind::InfrastructureProviderTaskTerminated);
            super::super::checkpoint_codec::encode_effect_record(
                result.reborrow().init_effect_journal(1).get(0),
                expected_effect.execution_id,
                &expected_effect.entry,
            )
            .unwrap();
            let mut events = result.init_turn_events(1);
            let mut decision = events.reborrow().get(0);
            decision.set_kind(super::finch_ipc_capnp::BrainTurnEventKind::ApprovalDecided);
            decision.set_approval_id("approval-1");
            super::super::brain_codec::encode_json_value(
                decision.reborrow().init_decision(),
                &serde_json::json!({"choice": "deny"}),
            )
            .unwrap();
        }

        let reader = message
            .get_root_as_reader::<super::finch_ipc_capnp::brain_turn_result::Reader>()
            .unwrap();
        let error = decode_runner_turn_result(Ok(reader)).unwrap_err();
        assert_eq!(error.kind, crate::server::RunnerTurnErrorKind::InfrastructureProviderTaskTerminated);
        assert_eq!(error.message, "provider failed after approval");
        assert_eq!(
            error.turn_events,
            vec![crate::server::RunnerTurnEvent::ApprovalDecided {
                approval_id: "approval-1".into(),
                decision: serde_json::json!({"choice": "deny"}),
            }]
        );
        assert_eq!(error.effect_journal, vec![expected_effect]);
    }

    #[test]
    fn runner_program_error_keeps_execute_once_effects() {
        let expected_effect = effect_record();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_program_result::Builder>();
            result.set_error("program failed after emit");
            super::super::checkpoint_codec::encode_effect_record(
                result.reborrow().init_effect_journal(1).get(0),
                expected_effect.execution_id,
                &expected_effect.entry,
            )
            .unwrap();
        }
        let reader = message
            .get_root_as_reader::<super::finch_ipc_capnp::brain_program_result::Reader>()
            .unwrap();
        let error = decode_runner_program_result(Ok(reader)).unwrap_err();
        assert_eq!(error.message, "program failed after emit");
        assert_eq!(error.effect_journal, vec![expected_effect]);
    }

    #[tokio::test]
    async fn eval_forth_uses_typed_signatures_and_runtime() {
        let (stack, output) = execute_typed_forth_ipc(
            ": double ( S n:int -- S int ! pure ) n n + ; 21 double".to_string(),
        )
        .await
        .unwrap();
        assert_eq!(stack, vec![42]);
        assert!(output.is_empty());

        let error = execute_typed_forth_ipc(": legacy dup * ; 4 legacy".to_string())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("E-FORTH-SIG-001"));
    }
}

// ---------------------------------------------------------------------------
// Enum conversion helper
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Accept loop — call this from daemon startup
// ---------------------------------------------------------------------------

/// Bind the Unix socket and accept Cap'n Proto connections in a `LocalSet`.
///
/// This function returns after the daemon cancels its shutdown token.
pub async fn start_ipc_server(
    server: Arc<AgentServer>,
    shutdown: tokio_util::sync::CancellationToken,
) -> Result<()> {
    let path = crate::ipc::transport::sock_path();

    // Remove only a stale socket. Blind unlinking lets a second daemon replace
    // the pathname while the original listener continues serving through its
    // open file descriptor.
    if path.exists() {
        match UnixStream::connect(&path).await {
            Ok(_) => anyhow::bail!(
                "Finch IPC socket already has a live listener at {}",
                path.display()
            ),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                std::fs::remove_file(&path)?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("could not determine whether {} is stale", path.display())
                });
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)?;
    tracing::info!(path = %path.display(), "IPC server listening");

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _addr)) => {
                            let server = Arc::clone(&server);
                            tokio::task::spawn_local(async move {
                                if let Err(e) = handle_connection(stream, server).await {
                                    tracing::warn!("IPC connection error: {}", e);
                                }
                            });
                        }
                        Err(e) => {
                            tracing::error!("IPC accept error: {}", e);
                        }
                    }
                }
            }
        })
        .await;
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

pub(crate) async fn handle_connection(
    stream: tokio::net::UnixStream,
    server: Arc<AgentServer>,
) -> Result<()> {
    let (reader, writer) = stream.into_split();

    let network = twoparty::VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    );

    let connection_id = uuid::Uuid::new_v4();
    let daemon_impl = FinchDaemonImpl::new(Arc::clone(&server), connection_id);
    let daemon_client: finch_daemon::Client = capnp_rpc::new_client(daemon_impl);

    let result = RpcSystem::new(Box::new(network), Some(daemon_client.client))
        .await
        .map_err(anyhow::Error::from);
    let attachments = server
        .brain_runners()
        .disconnect_connection(connection_id);
    let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
    for (brain, attachment_id, attachment_connection_id) in attachments {
        let _ = lifecycle.detach(&brain, attachment_id, attachment_connection_id);
    }
    result
}
