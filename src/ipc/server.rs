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
    decode_approval_audience, decode_brain_submission, decode_environment,
    encode_approval_audience, encode_attachment, encode_brain_submission_outcome, encode_event,
    encode_run, encode_runner_handoff, encode_runner_lease, encode_schedule, encode_snapshot,
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
        if let Err(error) =
            self.runners
                .claim_connection_lease(self.connection_id, brain, lease.lease_id)
        {
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
    effect_audit: Option<BrainEffectAuditRpcAuthority>,
}

#[derive(Clone)]
struct BrainEffectAuditRpcAuthority {
    store: crate::brain::store::BrainStore,
    grant: crate::brain::store::EffectAuditAuthorityGrant,
    runners: crate::server::BrainRunnerBroker,
    brain: String,
    lease_id: crate::brain::store::RunnerLeaseId,
    connection_id: Option<uuid::Uuid>,
    active: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl BrainEffectAuditRpcAuthority {
    fn validate_new_work(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.active.load(std::sync::atomic::Ordering::Acquire),
            "effect audit authority is no longer active"
        );
        if let Some(connection_id) = self.connection_id {
            self.runners
                .require_connection_lease(connection_id, &self.brain, self.lease_id)?;
        }
        Ok(())
    }
}

struct BrainEffectReservationImpl {
    authority: BrainEffectAuditRpcAuthority,
    identity: crate::runtime::effect_log::EffectAuditIdentity,
    begun: std::cell::Cell<bool>,
}

struct BrainHostEffectPermitImpl {
    authority: BrainEffectAuditRpcAuthority,
    permit: std::sync::Arc<crate::runtime::effect_log::HostEffectPermit>,
    finished: std::cell::RefCell<Option<crate::runtime::effect_log::EffectAuditTerminalOutcome>>,
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
    capnp_rpc::new_client(BrainTurnControlImpl {
        server,
        brain,
        request_seq,
        expected_audience,
        expected_connection_id,
        effect_audit: None,
    })
}

#[cfg(test)]
pub(crate) async fn request_test_turn_approval_with_client(
    control: finch_ipc_capnp::brain_turn_control::Client,
    event: crate::server::RunnerTurnEvent,
) -> Result<serde_json::Value> {
    let mut call = control.request_approval_request();
    let crate::server::RunnerTurnEvent::ApprovalRequested {
        approval_id,
        approval_kind,
        subject,
        audience,
        detail,
    } = event
    else {
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
    )
    .await
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
    effect_audit: Option<BrainEffectAuditRpcAuthority>,
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
        self.runners
            .require_connection_lease(self.connection_id, &self.brain, self.lease_id)?;
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
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_runner_control::StartSubagentParams,
        mut results: finch_ipc_capnp::brain_runner_control::StartSubagentResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        if let Err(error) = self.validate_lease() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error.into()),
        };
        let parse_uuid = |value: capnp::text::Reader<'_>| {
            value
                .to_str()
                .map_err(anyhow::Error::from)
                .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        };
        let parent_run_id = match params
            .get_parent_run_id()
            .map_err(anyhow::Error::from)
            .and_then(parse_uuid)
        {
            Ok(value) => crate::brain::store::RunId(value),
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let task_id = match params
            .get_task_id()
            .map_err(anyhow::Error::from)
            .and_then(parse_uuid)
        {
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
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_runner_control::FinishSubagentParams,
        mut results: finch_ipc_capnp::brain_runner_control::FinishSubagentResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_program_control::CreateScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::CreateScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_program_control::InspectScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::InspectScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_program_control::CancelScheduleParams,
        mut results: finch_ipc_capnp::brain_program_control::CancelScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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

    fn reserve_effect(
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_program_control::ReserveEffectParams,
        mut results: finch_ipc_capnp::brain_program_control::ReserveEffectResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let Some(authority) = &self.effect_audit else {
            return Promise::err(capnp::Error::failed(
                "effect audit authority is unavailable".into(),
            ));
        };
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let execution_id = match params
            .get_execution_id()
            .map_err(anyhow::Error::from)
            .and_then(|value| value.to_str().map_err(anyhow::Error::from))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        {
            Ok(value) => value,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let effect = match params
            .get_effect()
            .map_err(anyhow::Error::from)
            .and_then(crate::ipc::checkpoint_codec::decode_vm_side_effect)
        {
            Ok(effect) => effect,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        match authority.store.retry_effect_audit_reservation(
            &authority.grant,
            execution_id,
            &effect,
        ) {
            Ok(Some(identity)) => {
                let reservation: finch_ipc_capnp::brain_effect_reservation::Client =
                    capnp_rpc::new_client(BrainEffectReservationImpl {
                        authority: authority.clone(),
                        identity,
                        begun: std::cell::Cell::new(false),
                    });
                results.get().set_reservation(reservation);
                return Promise::ok(());
            }
            Ok(None) => {}
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        }
        if let Err(error) = authority.validate_new_work() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        match authority
            .store
            .reserve_effect_audit(&authority.grant, execution_id, effect)
        {
            Ok(identity) => {
                let reservation: finch_ipc_capnp::brain_effect_reservation::Client =
                    capnp_rpc::new_client(BrainEffectReservationImpl {
                        authority: authority.clone(),
                        identity,
                        begun: std::cell::Cell::new(false),
                    });
                results.get().set_reservation(reservation);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl finch_ipc_capnp::brain_effect_reservation::Server for BrainEffectReservationImpl {
    fn begin(
        self: capnp::capability::Rc<Self>,
        _params: finch_ipc_capnp::brain_effect_reservation::BeginParams,
        mut results: finch_ipc_capnp::brain_effect_reservation::BeginResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        if self.begun.get() {
            return Promise::err(capnp::Error::failed(
                "effect audit reservation was already begun".into(),
            ));
        }
        if let Err(error) = self.authority.validate_new_work() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        match self
            .authority
            .store
            .begin_effect_audit(&self.authority.grant, self.identity)
        {
            Ok(permit) => {
                self.begun.set(true);
                let permit: finch_ipc_capnp::brain_host_effect_permit::Client =
                    capnp_rpc::new_client(BrainHostEffectPermitImpl {
                        authority: self.authority.clone(),
                        permit: std::sync::Arc::new(permit),
                        finished: std::cell::RefCell::new(None),
                    });
                results.get().set_permit(permit);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn not_applied(
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_effect_reservation::NotAppliedParams,
        _results: finch_ipc_capnp::brain_effect_reservation::NotAppliedResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        if self.begun.get() {
            return Promise::err(capnp::Error::failed(
                "begun effect outcome requires its host permit".into(),
            ));
        }
        if let Err(error) = self.authority.validate_new_work() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let reason = match params
            .get()
            .and_then(|params| params.get_reason())
            .and_then(|value| value.to_str().map_err(capnp::Error::from))
        {
            Ok(reason) => reason.to_string(),
            Err(error) => return Promise::err(error),
        };
        match self.authority.store.finish_effect_audit(
            &self.authority.grant,
            None,
            self.identity,
            crate::runtime::effect_log::EffectAuditTerminalOutcome::NotApplied { reason },
        ) {
            Ok(()) => Promise::ok(()),
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl finch_ipc_capnp::brain_host_effect_permit::Server for BrainHostEffectPermitImpl {
    fn finish(
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_host_effect_permit::FinishParams,
        _results: finch_ipc_capnp::brain_host_effect_permit::FinishResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let outcome = match params.get().and_then(|params| params.get_outcome()) {
            Ok(outcome) => outcome,
            Err(error) => return Promise::err(error),
        };
        use finch_ipc_capnp::brain_host_effect_outcome::Which;
        let outcome = match outcome.which() {
            Ok(Which::Acknowledged(values)) => match values
                .map_err(anyhow::Error::from)
                .and_then(|values| crate::ipc::checkpoint_codec::decode_value_list(values, 0))
            {
                Ok(values) => {
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::Acknowledged {
                        response: crate::runtime::VmResumeResponse::Result { values },
                    }
                }
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            },
            Ok(Which::NotApplied(reason)) => {
                let reason =
                    match reason.and_then(|value| value.to_str().map_err(capnp::Error::from)) {
                        Ok(reason) => reason.to_string(),
                        Err(error) => return Promise::err(error),
                    };
                crate::runtime::effect_log::EffectAuditTerminalOutcome::NotApplied { reason }
            }
            Ok(Which::FailedPartial(detail)) => {
                let detail =
                    match detail.and_then(|value| value.to_str().map_err(capnp::Error::from)) {
                        Ok(detail) => detail.to_string(),
                        Err(error) => return Promise::err(error),
                    };
                crate::runtime::effect_log::EffectAuditTerminalOutcome::FailedPartial { detail }
            }
            Err(error) => return Promise::err(error.into()),
        };
        if let Some(existing) = self.finished.borrow().as_ref() {
            if existing == &outcome {
                return Promise::ok(());
            }
            return Promise::err(capnp::Error::failed(
                "host effect permit already finished with a different outcome".into(),
            ));
        }
        match self.authority.store.finish_effect_audit(
            &self.authority.grant,
            Some(&self.permit),
            self.permit.identity(),
            outcome.clone(),
        ) {
            Ok(()) => {
                *self.finished.borrow_mut() = Some(outcome);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl finch_ipc_capnp::brain_turn_control::Server for BrainTurnControlImpl {
    fn request_approval(
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_turn_control::RequestApprovalParams,
        mut results: finch_ipc_capnp::brain_turn_control::RequestApprovalResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        let registration = match self
            .server
            .brain_approvals()
            .register_for_connection_with_authority(
                self.request_seq,
                approval_id.clone(),
                audience.clone(),
                connection_id,
                || {
                    self.server.brain_store().begin_run_approval_for_connection(
                        &self.brain,
                        audience.attachment_id,
                        connection_id,
                        self.request_seq,
                        approval_id.clone(),
                        approval_kind.clone(),
                        subject.clone(),
                        audience.clone(),
                        detail.clone(),
                    )
                },
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

    fn reserve_effect(
        self: capnp::capability::Rc<Self>,
        params: finch_ipc_capnp::brain_turn_control::ReserveEffectParams,
        mut results: finch_ipc_capnp::brain_turn_control::ReserveEffectResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let Some(authority) = &self.effect_audit else {
            return Promise::err(capnp::Error::failed(
                "effect audit authority is unavailable".into(),
            ));
        };
        if let Err(error) = authority.validate_new_work() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let execution_id = match params
            .get_execution_id()
            .map_err(anyhow::Error::from)
            .and_then(|value| value.to_str().map_err(anyhow::Error::from))
            .and_then(|value| uuid::Uuid::parse_str(value).map_err(anyhow::Error::from))
        {
            Ok(value) => value,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let effect = match params
            .get_effect()
            .map_err(anyhow::Error::from)
            .and_then(crate::ipc::checkpoint_codec::decode_vm_side_effect)
        {
            Ok(effect) => effect,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        match authority
            .store
            .reserve_effect_audit(&authority.grant, execution_id, effect)
        {
            Ok(identity) => {
                let reservation: finch_ipc_capnp::brain_effect_reservation::Client =
                    capnp_rpc::new_client(BrainEffectReservationImpl {
                        authority: authority.clone(),
                        identity,
                        begun: std::cell::Cell::new(false),
                    });
                results.get().set_reservation(reservation);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }
}

impl brain_service::Server for BrainRpcService {
    fn snapshot(
        self: capnp::capability::Rc<Self>,
        params: brain_service::SnapshotParams,
        mut results: brain_service::SnapshotResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::AttachParams,
        mut results: brain_service::AttachResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
            let _ =
                self.lifecycle
                    .detach(&brain, attachment.attachment_id, attachment_connection_id);
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn acknowledge(
        self: capnp::capability::Rc<Self>,
        params: brain_service::AcknowledgeParams,
        mut results: brain_service::AcknowledgeResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        let attachment =
            match self
                .lifecycle
                .acknowledge(&brain, attachment_id, connection_id, params.get_seq())
            {
                Ok(attachment) => attachment,
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            };
        encode_attachment(results.get().init_attachment(), &attachment);
        Promise::ok(())
    }

    fn detach(
        self: capnp::capability::Rc<Self>,
        params: brain_service::DetachParams,
        _results: brain_service::DetachResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        if let Err(error) = self.lifecycle.detach(&brain, attachment_id, connection_id) {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::SubmitParams,
        mut results: brain_service::SubmitResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        Promise::from_future(async move {
            let outcome = lifecycle
                .submit(&brain, attachment_id, connection_id, kind)
                .await
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::WatchParams,
        _results: brain_service::WatchResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        let watch = match self.lifecycle.watch(&brain, attachment_id, connection_id) {
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
            let initial_result =
                encode_snapshot(initial.get().init_message().init_snapshot(), &snapshot)
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::AcquireRunnerParams,
        mut results: brain_service::AcquireRunnerResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::ReleaseRunnerParams,
        _results: brain_service::ReleaseRunnerResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let params = pry!(params.get());
        let brain = pry!(params.get_brain()).to_str().unwrap_or("").to_string();
        let lease_id = match parse_runner_lease_id(params.get_lease_id()) {
            Ok(id) => id,
            Err(error) => return Promise::err(error),
        };
        if let Err(error) =
            self.runners
                .require_connection_lease(self.connection_id, &brain, lease_id)
        {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::RequestRunnerHandoffParams,
        mut results: brain_service::RequestRunnerHandoffResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::AcceptRunnerHandoffParams,
        mut results: brain_service::AcceptRunnerHandoffResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        if let Err(error) =
            self.runners
                .claim_connection_lease(self.connection_id, &brain, lease.lease_id)
        {
            let _ = self.lifecycle.release_runner(&brain, lease.lease_id);
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        encode_runner_lease(results.get().init_lease(), &lease);
        Promise::ok(())
    }

    fn cancel_runner_handoff(
        self: capnp::capability::Rc<Self>,
        params: brain_service::CancelRunnerHandoffParams,
        _results: brain_service::CancelRunnerHandoffResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::InspectRunParams,
        mut results: brain_service::InspectRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::CancelRunParams,
        mut results: brain_service::CancelRunResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::CreateScheduleParams,
        mut results: brain_service::CreateScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::InspectScheduleParams,
        mut results: brain_service::InspectScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::CancelScheduleParams,
        mut results: brain_service::CancelScheduleResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        match self
            .lifecycle
            .cancel_schedule(&brain, attachment_id, connection_id, schedule_id)
        {
            Ok(cancelled) => {
                results.get().set_cancelled(cancelled);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn schedule_initialization(
        self: capnp::capability::Rc<Self>,
        params: brain_service::ScheduleInitializationParams,
        mut results: brain_service::ScheduleInitializationResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: brain_service::ClaimRunnerIdentityParams,
        _results: brain_service::ClaimRunnerIdentityResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
) -> Result<Vec<crate::tools::ToolDefinition>, capnp::Error> {
    let mut out = Vec::with_capacity(list.len() as usize);
    for td in list.iter() {
        let schema: crate::tools::ToolInputSchema =
            serde_json::from_str(td.get_input_schema_json()?.to_str()?).unwrap_or_else(|_| {
                crate::tools::ToolInputSchema {
                    schema_type: "object".to_string(),
                    properties: serde_json::Value::Object(serde_json::Map::new()),
                    required: vec![],
                }
            });
        out.push(crate::tools::ToolDefinition {
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
    tool_uses: &[crate::tools::ToolUse],
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
        self: capnp::capability::Rc<Self>,
        params: finch_daemon::QueryParams,
        mut results: finch_daemon::QueryResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let p = pry!(params.get());
        let messages = pry!(super::brain_codec::decode_messages(pry!(p.get_messages()))
            .map_err(|error| capnp::Error::failed(error.to_string())));
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
        self: capnp::capability::Rc<Self>,
        params: finch_daemon::QueryStreamParams,
        _results: finch_daemon::QueryStreamResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
        let p = pry!(params.get());
        let messages = pry!(super::brain_codec::decode_messages(pry!(p.get_messages()))
            .map_err(|error| capnp::Error::failed(error.to_string())));
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
                    Ok(StreamChunk::Usage {
                        input_tokens,
                        output_tokens,
                    }) => {
                        let mut r = receiver.on_chunk_request();
                        let mut upd = r.get().init_chunk().init_usage_update();
                        upd.set_input_tokens(input_tokens);
                        upd.set_output_tokens(output_tokens);
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::ResponseMetadata { model }) => {
                        crate::generators::validate_response_model(&model).map_err(|_| {
                            capnp::Error::failed("IPC response model metadata was invalid".into())
                        })?;
                        let mut r = receiver.on_chunk_request();
                        r.get()
                            .init_chunk()
                            .init_response_metadata()
                            .set_model(model.as_str());
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::Allowance {
                        primary_used_percent,
                        secondary_used_percent,
                    }) => {
                        let mut r = receiver.on_chunk_request();
                        let mut allowance = r.get().init_chunk().init_allowance_update();
                        allowance.set_has_primary(primary_used_percent.is_some());
                        allowance
                            .set_primary_used_percent(primary_used_percent.unwrap_or_default());
                        allowance.set_has_secondary(secondary_used_percent.is_some());
                        allowance
                            .set_secondary_used_percent(secondary_used_percent.unwrap_or_default());
                        r.send().promise.await?;
                    }
                    Ok(StreamChunk::ContentBlockComplete(block)) => {
                        let mut r = receiver.on_chunk_request();
                        let mut encoded = r.get().init_chunk().init_content_block_complete();
                        match block {
                            crate::claude::ContentBlock::Text { text } => encoded.set_text(&text),
                            crate::claude::ContentBlock::Image { source } => {
                                let mut image = encoded.init_image();
                                image.set_source_type(&source.source_type);
                                image.set_media_type(&source.media_type);
                                image.set_data(&source.data);
                            }
                            crate::claude::ContentBlock::ToolUse { id, name, input } => {
                                let mut tool = encoded.init_tool_use();
                                tool.set_id(&id);
                                tool.set_name(&name);
                                super::brain_codec::encode_json_value(
                                    tool.reborrow().init_input(),
                                    &input,
                                )
                                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                            }
                            crate::claude::ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                let mut result = encoded.init_tool_result();
                                result.set_tool_use_id(&tool_use_id);
                                result.set_content(&content);
                                result.set_is_error(is_error.unwrap_or(false));
                            }
                            crate::claude::ContentBlock::OpaqueReasoning { encrypted_content } => {
                                encoded.set_thinking(&encrypted_content);
                            }
                        }
                        r.send().promise.await?;
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
        self: capnp::capability::Rc<Self>,
        params: finch_daemon::EvalForthParams,
        mut results: finch_daemon::EvalForthResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        params: finch_daemon::RegisterBrainRunnerParams,
        mut results: finch_daemon::RegisterBrainRunnerResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        let registration_id =
            match broker.register_for_connection(self.connection_id, brain.clone(), lease_id, tx) {
                Ok(registration_id) => registration_id,
                Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
            };
        let dispatch_admission = match broker.connection_dispatch_admission(self.connection_id) {
            Ok(admission) => admission,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let queued_lifecycle = crate::server::BrainLifecycleService::from_server(&server);
        let queued_brain = brain.clone();
        let registered_brain = brain.clone();
        let registered_connection_id = self.connection_id;
        tokio::task::spawn_local(async move {
            while let Some(request) = rx.recv().await {
                // Deliberately no await may occur between dequeue and
                // admission. The guard is acquired before spawning
                // `forward_runner_request`, whose first Program/Turn action
                // mints the run-scoped audit authority. Connection teardown
                // therefore either rejects this queued request or waits for
                // all authority it can create before taking the durable
                // reconciliation snapshot.
                let Some(dispatch_guard) = dispatch_admission.try_enter() else {
                    // Teardown closed callback admission before taking its
                    // durable audit snapshot. Dropping the receiver also
                    // rejects every queued request without issuing authority.
                    break;
                };
                let runner = runner.clone();
                let server = Arc::clone(&server);
                tokio::task::spawn_local(async move {
                    let _dispatch_guard = dispatch_guard;
                    forward_runner_request(
                        runner,
                        server,
                        request,
                        lease_id,
                        Some(crate::brain::store::ConnectionId(registered_connection_id)),
                    )
                    .await;
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
        let control: finch_ipc_capnp::brain_runner_control::Client =
            capnp_rpc::new_client(BrainRunnerControlImpl {
                lifecycle: crate::server::BrainLifecycleService::from_server(&self.server),
                runners: self.server.brain_runners().clone(),
                connection_id: self.connection_id,
                brain: brain.clone(),
                lease_id,
            });
        response.set_control(control);
        Promise::ok(())
    }

    fn brain_service(
        self: capnp::capability::Rc<Self>,
        _params: finch_daemon::BrainServiceParams,
        mut results: finch_daemon::BrainServiceResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        self: capnp::capability::Rc<Self>,
        _params: finch_daemon::PingParams,
        mut results: finch_daemon::PingResults,
    ) -> impl std::future::Future<Output = std::result::Result<(), capnp::Error>> + 'static {
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
        finch_ipc_capnp::BrainAttachmentRole::Runner => crate::brain::store::AttachmentRole::Runner,
        finch_ipc_capnp::BrainAttachmentRole::Driver => crate::brain::store::AttachmentRole::Driver,
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
    lease_id: crate::brain::store::RunnerLeaseId,
    connection_id: Option<crate::brain::store::ConnectionId>,
) {
    match request {
        crate::server::RunnerRequest::Program(request) => {
            let audit_grant = match server.brain_store().issue_effect_audit_authority(
                &request.brain,
                request.run_id,
                lease_id,
                connection_id,
            ) {
                Ok(grant) => grant,
                Err(error) => {
                    let _ = request.response_tx.send(Err(error.to_string().into()));
                    return;
                }
            };
            let reconciliation_grant = audit_grant.clone();
            let audit_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
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
                        effect_audit: Some(BrainEffectAuditRpcAuthority {
                            store: server.brain_store().clone(),
                            grant: audit_grant,
                            runners: server.brain_runners().clone(),
                            brain: request.brain.clone(),
                            lease_id,
                            connection_id: connection_id.map(|id| id.0),
                            active: std::sync::Arc::clone(&audit_active),
                        }),
                    });
                payload.set_control(control);
            }
            let mut result = match call.send().promise.await {
                Ok(reply) => decode_runner_program_result(reply.get().and_then(|r| r.get_result())),
                Err(error) => Err(error.to_string().into()),
            };
            audit_active.store(false, std::sync::atomic::Ordering::Release);
            // An individual RPC error cannot prove transport loss: remote
            // exceptions may claim `Disconnected`, while a torn frame can
            // surface another error kind. Only whole-connection teardown
            // terminalizes begun permits as uncertain.
            let reconciliation = server
                .brain_store()
                .abandon_unbegun_effect_audits(&reconciliation_grant);
            if let Err(error) = reconciliation {
                result = Err(error.to_string().into());
            }
            let _ = request.response_tx.send(result);
        }
        crate::server::RunnerRequest::Turn(request) => {
            let audit_grant = match server.brain_store().issue_effect_audit_authority(
                &request.brain,
                request.run_id,
                lease_id,
                connection_id,
            ) {
                Ok(grant) => grant,
                Err(error) => {
                    let _ = request.response_tx.send(Err(error.to_string().into()));
                    return;
                }
            };
            let reconciliation_grant = audit_grant.clone();
            let audit_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let mut result = {
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
                                server: Arc::clone(&server),
                                brain: request.brain.clone(),
                                request_seq: request.request_seq,
                                expected_audience: request.approval_audience.clone(),
                                expected_connection_id: request.approval_connection_id,
                                effect_audit: Some(BrainEffectAuditRpcAuthority {
                                    store: server.brain_store().clone(),
                                    grant: audit_grant,
                                    runners: server.brain_runners().clone(),
                                    brain: request.brain.clone(),
                                    lease_id,
                                    connection_id: connection_id.map(|id| id.0),
                                    active: std::sync::Arc::clone(&audit_active),
                                }),
                            });
                        payload.set_control(control);
                    }
                    encoded
                };
                match encoded {
                    Ok(()) => match call.send().promise.await {
                        Ok(reply) => {
                            decode_runner_turn_result(reply.get().and_then(|r| r.get_result()))
                        }
                        Err(error) => Err(error.to_string().into()),
                    },
                    Err(error) => Err(error.into()),
                }
            };
            audit_active.store(false, std::sync::atomic::Ordering::Release);
            let reconciliation = server
                .brain_store()
                .abandon_unbegun_effect_audits(&reconciliation_grant);
            if let Err(error) = reconciliation {
                result = Err(error.to_string().into());
            }
            let _ = request.response_tx.send(result);
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
                payload.set_rendered(&request.rendered);
            }
            let result = match call.send().promise.await {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = reply
                            .get_error()
                            .ok()
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("");
                        if error.is_empty() {
                            Ok(reply.get_inserted() as usize)
                        } else {
                            Err(error.to_string())
                        }
                    }
                    Err(error) => Err(format!(
                        "{}{error}",
                        crate::server::RUNNER_UNAVAILABLE_PREFIX
                    )),
                },
                // The call did not complete at all. That covers the transport
                // — a disconnect, a shutdown, an unimplemented capability — and
                // also every `Promise::err` the frontend raises in
                // `ipc/client.rs`: a params decode failure, a stopped event
                // loop, a dropped response. All of those repeat identically for
                // every later run, so classifying the whole arm as systemic is
                // right today. The one per-request error in that set, a
                // malformed brain/run id, cannot occur — `project_memory` is
                // handed typed `BrainId`/`RunId` values that always render as
                // valid UUIDs. If a per-request failure mode is ever added
                // here, classify it rather than letting it abort a whole pass.
                Err(error) => Err(format!(
                    "{}{error}",
                    crate::server::RUNNER_UNAVAILABLE_PREFIX
                )),
            };
            let _ = request.response_tx.send(result);
        }
        crate::server::RunnerRequest::Cancel(request) => {
            let mut call = runner.cancel_run_request();
            call.get().set_brain(&request.brain);
            call.get().set_run_id(&request.run_id.0.to_string());
            let result = match call.send().promise.await {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = reply
                            .get_error()
                            .ok()
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or("");
                        if error.is_empty() {
                            Ok(reply.get_cancelled())
                        } else {
                            Err(error.to_string())
                        }
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error.to_string()),
            };
            let _ = request.response_tx.send(result);
        }
    }
}

#[cfg(test)]
pub(crate) async fn forward_test_runner_request(
    runner: finch_ipc_capnp::brain_runner::Client,
    server: Arc<AgentServer>,
    request: crate::server::RunnerRequest,
) {
    let brain = match &request {
        crate::server::RunnerRequest::Program(request) => &request.brain,
        crate::server::RunnerRequest::Turn(request) => &request.brain,
        crate::server::RunnerRequest::ProjectMemory(request) => &request.brain,
        crate::server::RunnerRequest::Cancel(request) => &request.brain,
    };
    let lease_id = server
        .brain_store()
        .snapshot(brain)
        .ok()
        .and_then(|snapshot| snapshot.runner_lease.map(|lease| lease.lease_id))
        .unwrap_or(crate::brain::store::RunnerLeaseId(uuid::Uuid::nil()));
    forward_runner_request(runner, server, request, lease_id, None).await
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
        return Err(crate::server::RunnerTurnError {
            message: error.to_string(),
            turn_events,
            effect_journal,
        });
    }
    let checkpoint = decode_checkpoint(result.get_checkpoint().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let commit_ack = if result.get_has_commit_ack() {
        let capability = result.get_commit_ack().map_err(|error| error.to_string())?;
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::server::RunnerTurnCommitNotice>();
        tokio::task::spawn_local(async move {
            while let Some(notice) = rx.recv().await {
                let mut call = capability.committed_request();
                call.get()
                    .set_status(crate::ipc::brain_codec::run_status_to_capnp(notice.status));
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
        continuation_messages: crate::ipc::brain_codec::decode_continuation_messages(
            result
                .get_continuation_messages()
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
        invocation_metadata: result
            .get_has_invocation_metadata()
            .then(|| result.get_invocation_metadata())
            .transpose()
            .map_err(|error| error.to_string())?
            .map(crate::ipc::brain_codec::decode_invocation_metadata)
            .transpose()
            .map_err(|error| error.to_string())?,
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
            let (execution_id, entry) = crate::ipc::checkpoint_codec::decode_effect_record(record)
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
mod tests;

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
    let prepared = prepare_ipc_listener().await?;
    serve_ipc_listener(server, shutdown, prepared).await
}

struct PreparedIpcListener {
    path: std::path::PathBuf,
    listener: UnixListener,
    remove_on_shutdown: bool,
}

async fn prepare_ipc_listener() -> Result<PreparedIpcListener> {
    // A supervised daemon must consume the short, private socket path sealed
    // into its authenticated proof. The supervisor already bound the listener;
    // the child performs no pathname operation at startup or shutdown.
    if let Some(proof) = crate::brain::isolated_test_proof_if_present()? {
        let path = std::env::var_os("FINCH_TEST_IPC_SOCKET")
            .map(std::path::PathBuf::from)
            .context("supervised daemon is missing its sealed IPC socket path")?;
        anyhow::ensure!(
            path == proof.ipc_socket,
            "supervised daemon IPC path is not parent-authorized"
        );
        let listener = proof.duplicate_ipc_listener()?;
        listener.set_nonblocking(true)?;
        return Ok(PreparedIpcListener {
            path,
            listener: UnixListener::from_std(listener)?,
            remove_on_shutdown: false,
        });
    }

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
    Ok(PreparedIpcListener {
        path,
        listener,
        remove_on_shutdown: true,
    })
}

async fn serve_ipc_listener(
    server: Arc<AgentServer>,
    shutdown: tokio_util::sync::CancellationToken,
    prepared: PreparedIpcListener,
) -> Result<()> {
    let PreparedIpcListener {
        path,
        listener,
        remove_on_shutdown,
    } = prepared;
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
    if remove_on_shutdown && path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

async fn handle_connection(stream: tokio::net::UnixStream, server: Arc<AgentServer>) -> Result<()> {
    handle_connection_with_id(stream, server, uuid::Uuid::new_v4()).await
}

async fn handle_connection_with_id(
    stream: tokio::net::UnixStream,
    server: Arc<AgentServer>,
    connection_id: uuid::Uuid,
) -> Result<()> {
    let (reader, writer) = stream.into_split();

    let network = twoparty::VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        Default::default(),
    );

    let daemon_impl = FinchDaemonImpl::new(Arc::clone(&server), connection_id);
    let daemon_client: finch_daemon::Client = capnp_rpc::new_client(daemon_impl);

    let result = RpcSystem::new(Box::new(network), Some(daemon_client.client))
        .await
        .map_err(anyhow::Error::from);
    let teardown = server
        .brain_runners()
        .begin_connection_teardown(connection_id);
    teardown.wait_quiesced().await;
    let mut leases_by_brain =
        std::collections::BTreeMap::<String, Vec<crate::brain::store::RunnerLeaseId>>::new();
    for (brain, lease_id) in &teardown.runner_leases {
        leases_by_brain
            .entry(brain.clone())
            .or_default()
            .push(*lease_id);
    }
    for (brain, lease_ids) in leases_by_brain {
        server
            .brain_store()
            .reconcile_effect_audits_for_disconnected_leases(&brain, &lease_ids)
            .with_context(|| {
                format!("could not reconcile effect audits for disconnected Brain '{brain}'")
            })?;
    }
    let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
    for (brain, attachment_id, attachment_connection_id) in &teardown.attachments {
        if let Err(error) = lifecycle.detach(brain, *attachment_id, *attachment_connection_id) {
            // Audit durability is the authority-critical teardown phase. Once
            // it succeeds, an attachment may already have been retired by an
            // explicit detach or cancellation path. Cleanup is idempotent in
            // effect and must not strand the lease/identity fence forever.
            tracing::warn!(
                brain,
                attachment_id = %attachment_id.0,
                connection_id = %attachment_connection_id.0,
                %error,
                "could not detach disconnected Brain attachment; releasing reconciled connection claims"
            );
        }
    }
    teardown.finish()?;
    result
}
