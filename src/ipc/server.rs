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
    cancel: tokio_util::sync::CancellationToken,
}

impl BrainEffectAuditRpcAuthority {
    fn validate_new_work(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.cancel.is_cancelled(),
            "effect audit request has been cancelled"
        );
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
    begun: bool,
}

struct BrainHostEffectPermitImpl {
    authority: BrainEffectAuditRpcAuthority,
    permit: std::sync::Arc<crate::runtime::effect_log::HostEffectPermit>,
    finished: Option<crate::runtime::effect_log::EffectAuditTerminalOutcome>,
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

impl BrainProgramControlImpl {
    fn validate_callback_active(&self) -> anyhow::Result<()> {
        match &self.effect_audit {
            Some(authority) => authority.validate_new_work(),
            None if cfg!(test) => Ok(()),
            None => anyhow::bail!("runner callback authority is unavailable"),
        }
    }
}

impl BrainTurnControlImpl {
    fn validate_callback_active(&self) -> anyhow::Result<()> {
        match &self.effect_audit {
            Some(authority) => authority.validate_new_work(),
            None if cfg!(test) => Ok(()),
            None => anyhow::bail!("runner callback authority is unavailable"),
        }
    }
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
        &mut self,
        params: finch_ipc_capnp::brain_runner_control::StartSubagentParams,
        mut results: finch_ipc_capnp::brain_runner_control::StartSubagentResults,
    ) -> Promise<(), capnp::Error> {
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
        if let Err(error) = self.validate_callback_active() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
        let params = match params.get() {
            Ok(params) => params,
            Err(error) => return Promise::err(error),
        };
        let language = match params.get_language() {
            Ok(language) => program_language_from_capnp(language),
            Err(error) => return Promise::err(error.into()),
        };
        let source = match decode_required_text(params.get_source(), "runner schedule source") {
            Ok(source) => source,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
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
        if let Err(error) = self.validate_callback_active() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
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
        &mut self,
        params: finch_ipc_capnp::brain_program_control::ReserveEffectParams,
        mut results: finch_ipc_capnp::brain_program_control::ReserveEffectResults,
    ) -> Promise<(), capnp::Error> {
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
                        begun: false,
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
                        begun: false,
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
        &mut self,
        _params: finch_ipc_capnp::brain_effect_reservation::BeginParams,
        mut results: finch_ipc_capnp::brain_effect_reservation::BeginResults,
    ) -> Promise<(), capnp::Error> {
        if self.begun {
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
                self.begun = true;
                let permit: finch_ipc_capnp::brain_host_effect_permit::Client =
                    capnp_rpc::new_client(BrainHostEffectPermitImpl {
                        authority: self.authority.clone(),
                        permit: std::sync::Arc::new(permit),
                        finished: None,
                    });
                results.get().set_permit(permit);
                Promise::ok(())
            }
            Err(error) => Promise::err(capnp::Error::failed(error.to_string())),
        }
    }

    fn not_applied(
        &mut self,
        params: finch_ipc_capnp::brain_effect_reservation::NotAppliedParams,
        _results: finch_ipc_capnp::brain_effect_reservation::NotAppliedResults,
    ) -> Promise<(), capnp::Error> {
        if self.begun {
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
        &mut self,
        params: finch_ipc_capnp::brain_host_effect_permit::FinishParams,
        _results: finch_ipc_capnp::brain_host_effect_permit::FinishResults,
    ) -> Promise<(), capnp::Error> {
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
        if let Some(existing) = &self.finished {
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
                self.finished = Some(outcome);
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
        if let Err(error) = self.validate_callback_active() {
            return Promise::err(capnp::Error::failed(error.to_string()));
        }
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
        &mut self,
        params: finch_ipc_capnp::brain_turn_control::ReserveEffectParams,
        mut results: finch_ipc_capnp::brain_turn_control::ReserveEffectResults,
    ) -> Promise<(), capnp::Error> {
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
                        begun: false,
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
            let _ =
                self.lifecycle
                    .detach(&brain, attachment.attachment_id, attachment_connection_id);
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
        let source = match decode_required_text(params.get_source(), "schedule source") {
            Ok(source) => source,
            Err(error) => return Promise::err(capnp::Error::failed(error)),
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
        &mut self,
        params: finch_daemon::QueryStreamParams,
        _results: finch_daemon::QueryStreamResults,
    ) -> Promise<(), capnp::Error> {
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
        let registration_id = match broker.register_bounded_for_connection(
            self.connection_id,
            brain.clone(),
            lease_id,
            tx,
        ) {
            Ok(registration_id) => registration_id,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let dispatch_admission = match broker.connection_dispatch_admission(self.connection_id) {
            Ok(admission) => admission,
            Err(error) => return Promise::err(capnp::Error::failed(error.to_string())),
        };
        let queued_lifecycle = crate::server::BrainLifecycleService::from_server(&server);
        let queued_broker = broker.clone();
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
                        Some(registration_id),
                    )
                    .await;
                });
            }
            broker.unregister(&registered_brain, registration_id);
        });
        // Return the registration bootstrap first. The frontend then marks
        // this lease active before the queued callback reaches its event loop.
        tokio::task::spawn_local(async move {
            if let Err(error) = queued_broker
                .wait_registration_active(&queued_brain, registration_id)
                .await
            {
                tracing::warn!(brain = %queued_brain, %error,
                    "runner registration retired before queued work could resume");
                return;
            }
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

async fn await_runner_rpc<T, F>(
    response_tx: &mut tokio::sync::oneshot::Sender<T>,
    cancel: &tokio_util::sync::CancellationToken,
    rpc: F,
) -> Option<F::Output>
where
    F: std::future::Future,
{
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        _ = response_tx.closed() => None,
        reply = rpc => Some(reply),
    }
}

async fn cancel_frontend_run(
    runner: &finch_ipc_capnp::brain_runner::Client,
    brain: &str,
    run_id: crate::brain::store::RunId,
    deadline: tokio::time::Instant,
) -> bool {
    let mut cancel = runner.cancel_run_request();
    cancel.get().set_brain(brain);
    cancel.get().set_run_id(&run_id.0.to_string());
    match tokio::time::timeout_at(deadline, cancel.send().promise).await {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            tracing::warn!(brain = %brain, run_id = %run_id.0, %error,
                "could not cancel abandoned runner callback");
            false
        }
        Err(_) => {
            tracing::warn!(brain = %brain, run_id = %run_id.0,
                "timed out cancelling abandoned runner callback");
            false
        }
    }
}

async fn cancel_frontend_memory(
    runner: &finch_ipc_capnp::brain_runner::Client,
    brain: &str,
    run_id: crate::brain::store::RunId,
    deadline: tokio::time::Instant,
) -> bool {
    let mut cancel = runner.cancel_memory_request();
    cancel.get().set_brain(brain);
    cancel.get().set_run_id(&run_id.0.to_string());
    match tokio::time::timeout_at(deadline, cancel.send().promise).await {
        Ok(Ok(_)) => true,
        Ok(Err(error)) => {
            tracing::warn!(brain = %brain, run_id = %run_id.0, %error,
                "could not cancel abandoned runner memory projection");
            false
        }
        Err(_) => {
            tracing::warn!(brain = %brain, run_id = %run_id.0,
                "timed out cancelling abandoned runner memory projection");
            false
        }
    }
}

async fn settle_cancelled_frontend_rpc<F>(
    rpc: &mut std::pin::Pin<Box<F>>,
    deadline: tokio::time::Instant,
) -> bool
where
    F: std::future::Future,
{
    tokio::time::timeout_at(deadline, rpc).await.is_ok()
}

async fn forward_runner_request(
    runner: finch_ipc_capnp::brain_runner::Client,
    server: Arc<AgentServer>,
    request: crate::server::BoundedRunnerRequest,
    lease_id: crate::brain::store::RunnerLeaseId,
    connection_id: Option<crate::brain::store::ConnectionId>,
    registration_id: Option<crate::server::RunnerRegistrationId>,
) {
    let crate::server::BoundedRunnerRequest {
        request,
        cancel: callback_cancel,
        deadline,
        cleanup_timeout,
    } = request;
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
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "could not issue runner effect-audit authority"
                    );
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
                            cancel: callback_cancel.clone(),
                        }),
                    });
                payload.set_control(control);
            }
            let mut response_tx = request.response_tx;
            let mut rpc = Box::pin(call.send().promise);
            let reply = await_runner_rpc(&mut response_tx, &callback_cancel, rpc.as_mut()).await;
            if reply.is_none() {
                // Close effect admission before any cancellation RPC awaits.
                // The daemon may terminalize the run as soon as this exact
                // request token fires, so no callback may reserve a new host
                // effect during the bounded remote-cleanup window.
                audit_active.store(false, std::sync::atomic::Ordering::Release);
                callback_cancel.cancel();
                let cleanup_deadline = tokio::time::Instant::now() + cleanup_timeout;
                let (cancel_ack, settled) = tokio::join!(
                    cancel_frontend_run(&runner, &request.brain, request.run_id, cleanup_deadline),
                    settle_cancelled_frontend_rpc(&mut rpc, cleanup_deadline)
                );
                if !(cancel_ack && settled) {
                    if let Some(registration_id) = registration_id {
                        server
                            .brain_runners()
                            .unregister(&request.brain, registration_id);
                    }
                }
            }
            audit_active.store(false, std::sync::atomic::Ordering::Release);
            // An individual RPC error cannot prove transport loss: remote
            // exceptions may claim `Disconnected`, while a torn frame can
            // surface another error kind. Only whole-connection teardown
            // terminalizes begun permits as uncertain.
            let reconciliation = server
                .brain_store()
                .abandon_unbegun_effect_audits(&reconciliation_grant);
            if let Err(error) = reconciliation {
                tracing::warn!(
                    brain = %request.brain,
                    run_id = %request.run_id.0,
                    %error,
                    "could not abandon unbegun runner effect audits"
                );
                return;
            }
            let Some(reply) = reply else {
                return;
            };
            let result = match reply {
                Ok(reply) => decode_runner_program_result(reply.get().and_then(|r| r.get_result())),
                Err(error) => {
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "runner program RPC ended without a response"
                    );
                    return;
                }
            };
            if response_tx.send(result).is_err() {
                callback_cancel.cancel();
            }
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
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "could not issue runner effect-audit authority"
                    );
                    return;
                }
            };
            let reconciliation_grant = audit_grant.clone();
            let audit_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
            let reply = {
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
                                    cancel: callback_cancel.clone(),
                                }),
                            });
                        payload.set_control(control);
                    }
                    encoded
                };
                match encoded {
                    Ok(()) => {
                        let mut response_tx = request.response_tx;
                        let mut rpc = Box::pin(call.send().promise);
                        let reply =
                            await_runner_rpc(&mut response_tx, &callback_cancel, rpc.as_mut())
                                .await;
                        if reply.is_none() {
                            // Fence new effects before waiting for the
                            // frontend to acknowledge physical cancellation.
                            audit_active.store(false, std::sync::atomic::Ordering::Release);
                            callback_cancel.cancel();
                            let cleanup_deadline = tokio::time::Instant::now() + cleanup_timeout;
                            let (cancel_ack, settled) = tokio::join!(
                                cancel_frontend_run(
                                    &runner,
                                    &request.brain,
                                    request.run_id,
                                    cleanup_deadline
                                ),
                                settle_cancelled_frontend_rpc(&mut rpc, cleanup_deadline)
                            );
                            if !(cancel_ack && settled) {
                                if let Some(registration_id) = registration_id {
                                    server
                                        .brain_runners()
                                        .unregister(&request.brain, registration_id);
                                }
                            }
                        }
                        Some((response_tx, reply))
                    }
                    Err(error) => {
                        tracing::warn!(
                            brain = %request.brain,
                            run_id = %request.run_id.0,
                            %error,
                            "could not encode runner turn request"
                        );
                        None
                    }
                }
            };
            audit_active.store(false, std::sync::atomic::Ordering::Release);
            let reconciliation = server
                .brain_store()
                .abandon_unbegun_effect_audits(&reconciliation_grant);
            if let Err(error) = reconciliation {
                tracing::warn!(
                    brain = %request.brain,
                    run_id = %request.run_id.0,
                    %error,
                    "could not abandon unbegun runner effect audits"
                );
                return;
            }
            let Some((response_tx, reply)) = reply else {
                return;
            };
            let Some(reply) = reply else {
                return;
            };
            let result = match reply {
                Ok(reply) => decode_runner_turn_result(reply.get().and_then(|r| r.get_result())),
                Err(error) => {
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "runner turn RPC ended without a response"
                    );
                    return;
                }
            };
            if response_tx.send(result).is_err() {
                callback_cancel.cancel();
            }
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
            let mut response_tx = request.response_tx;
            let mut rpc = Box::pin(call.send().promise);
            let Some(reply) =
                await_runner_rpc(&mut response_tx, &callback_cancel, rpc.as_mut()).await
            else {
                callback_cancel.cancel();
                let cleanup_deadline = tokio::time::Instant::now() + cleanup_timeout;
                let (cancel_ack, settled) = tokio::join!(
                    cancel_frontend_memory(
                        &runner,
                        &request.brain,
                        request.run_id,
                        cleanup_deadline
                    ),
                    settle_cancelled_frontend_rpc(&mut rpc, cleanup_deadline)
                );
                if !(cancel_ack && settled) {
                    if let Some(registration_id) = registration_id {
                        server
                            .brain_runners()
                            .unregister(&request.brain, registration_id);
                    }
                }
                return;
            };
            let result = match reply {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = match decode_required_text(
                            reply.get_error(),
                            "runner memory projection error",
                        ) {
                            Ok(error) => error,
                            Err(error) => {
                                let _ = response_tx.send(Err(error));
                                return;
                            }
                        };
                        if error.is_empty() {
                            Ok(reply.get_inserted() as usize)
                        } else {
                            Err(error)
                        }
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => {
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "runner memory projection RPC ended without a response"
                    );
                    return;
                }
            };
            if response_tx.send(result).is_err() {
                callback_cancel.cancel();
            }
        }
        crate::server::RunnerRequest::Cancel(request) => {
            let mut call = runner.cancel_run_request();
            call.get().set_brain(&request.brain);
            call.get().set_run_id(&request.run_id.0.to_string());
            let response_tx = request.response_tx;
            let reply = tokio::select! {
                biased;
                _ = callback_cancel.cancelled() => None,
                _ = tokio::time::sleep_until(deadline) => None,
                reply = call.send().promise => Some(reply),
            };
            let Some(reply) = reply else {
                return;
            };
            let result = match reply {
                Ok(reply) => match reply.get() {
                    Ok(reply) => {
                        let error = match decode_required_text(
                            reply.get_error(),
                            "runner cancellation error",
                        ) {
                            Ok(error) => error,
                            Err(error) => {
                                let _ = response_tx.send(Err(error));
                                return;
                            }
                        };
                        if error.is_empty() {
                            Ok(reply.get_cancelled())
                        } else {
                            Err(error)
                        }
                    }
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => {
                    tracing::warn!(
                        brain = %request.brain,
                        run_id = %request.run_id.0,
                        %error,
                        "runner cancellation RPC ended without a response"
                    );
                    return;
                }
            };
            let _ = response_tx.send(result);
        }
    }
}

#[cfg(test)]
fn bounded_test_runner_request(
    request: crate::server::RunnerRequest,
) -> crate::server::BoundedRunnerRequest {
    crate::server::BoundedRunnerRequest {
        request,
        cancel: tokio_util::sync::CancellationToken::new(),
        deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(30),
        cleanup_timeout: std::time::Duration::from_secs(2),
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
    forward_runner_request(
        runner,
        server,
        bounded_test_runner_request(request),
        lease_id,
        None,
        None,
    )
    .await
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
    let error = decode_required_text(result.get_error(), "runner program error")?;
    if !error.is_empty() {
        return Err(crate::server::RunnerProgramError {
            message: error,
            effect_journal,
        });
    }
    let checkpoint = decode_checkpoint(result.get_checkpoint().map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    Ok(crate::server::RunnerProgramResult {
        output: decode_required_text(result.get_output(), "runner program output")?,
        runtime_revision: result.get_runtime_revision(),
        checkpoint,
        effect_journal,
    })
}

fn decode_runner_turn_result(
    result: capnp::Result<finch_ipc_capnp::brain_turn_result::Reader<'_>>,
) -> Result<crate::server::RunnerTurnResult, crate::server::RunnerTurnError> {
    let result = result.map_err(|error| error.to_string())?;
    let error = decode_required_text(result.get_error(), "runner turn error")?;
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
            message: error,
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
        source: decode_required_text(result.get_source(), "runner turn source")?,
        language: program_language_from_capnp(
            result.get_language().map_err(|error| error.to_string())?,
        ),
        output: decode_required_text(result.get_output(), "runner turn output")?,
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

fn decode_required_text(
    value: capnp::Result<capnp::text::Reader<'_>>,
    field: &str,
) -> Result<String, String> {
    value
        .map_err(|error| format!("could not read {field}: {error}"))?
        .to_str()
        .map(str::to_owned)
        .map_err(|error| format!("{field} is not valid UTF-8: {error}"))
}

fn decode_runner_turn_event(
    encoded: finch_ipc_capnp::brain_turn_event::Reader<'_>,
) -> Result<crate::server::RunnerTurnEvent, String> {
    let text = |value, field| decode_required_text(value, field);
    let tool_id = text(encoded.get_tool_id(), "runner turn event tool id")?;
    match encoded.get_kind().map_err(|error| error.to_string())? {
        finch_ipc_capnp::BrainTurnEventKind::Call => Ok(crate::server::RunnerTurnEvent::Call {
            tool_id,
            name: text(encoded.get_name(), "runner turn event tool name")?,
            input: super::brain_codec::decode_json_value(
                encoded.get_input().map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?,
        }),
        finch_ipc_capnp::BrainTurnEventKind::Result => Ok(crate::server::RunnerTurnEvent::Result {
            tool_id,
            output: text(encoded.get_output(), "runner turn event output")?,
            is_error: encoded.get_is_error(),
        }),
        finch_ipc_capnp::BrainTurnEventKind::ApprovalRequested => {
            Ok(crate::server::RunnerTurnEvent::ApprovalRequested {
                approval_id: text(encoded.get_approval_id(), "runner approval id")?,
                approval_kind: text(encoded.get_approval_kind(), "runner approval kind")?,
                subject: text(encoded.get_subject(), "runner approval subject")?,
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
                approval_id: text(encoded.get_approval_id(), "runner approval id")?,
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
        require_approval_connection, BrainRpcService, BrainRunnerControlImpl, FinchDaemonImpl,
    };

    #[test]
    fn runner_result_text_rejects_malformed_utf8_instead_of_coercing_empty() {
        let error = super::decode_required_text(
            Ok(capnp::text::Reader(&[0xff, 0xfe])),
            "runner program output",
        )
        .unwrap_err();
        assert!(error.contains("runner program output is not valid UTF-8"));
    }
    use crate::ipc::brain_codec::encode_approval_audience;

    #[test]
    fn capnp_effect_audit_requires_durable_begin_before_terminal_outcome() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let store = crate::brain::store::BrainStore::with_root(
                "box.local",
                Some(temp.path().join("brains")),
            );
            let attachment = store
                .attach(
                    "shared",
                    "alice",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .unwrap();
            let prompt = store
                .push(
                    "shared",
                    "alice",
                    crate::brain::store::BrainEventKind::Prompt {
                        text: "effect".into(),
                    },
                )
                .unwrap();
            let run = store
                .start_run(
                    "shared",
                    "alice",
                    crate::brain::store::BrainRunKind::Interactive,
                    prompt.seq,
                    attachment.attachment_id,
                    crate::brain::store::BrainRunStatus::Running,
                )
                .unwrap();
            let lease = store
                .acquire_runner_lease("shared", "runner", 1, None, 300_000)
                .unwrap();
            let grant = store
                .issue_effect_audit_authority("shared", run.run_id, lease.lease_id, None)
                .unwrap();
            let original_grant = grant.clone();
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::credential::BrainCredentialAuthority::ephemeral([44; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let authority = super::BrainEffectAuditRpcAuthority {
                store: store.clone(),
                grant,
                runners: server.brain_runners().clone(),
                brain: "shared".into(),
                lease_id: lease.lease_id,
                connection_id: None,
                active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
                cancel: tokio_util::sync::CancellationToken::new(),
            };
            let mut cancelled_authority = authority.clone();
            cancelled_authority.cancel = tokio_util::sync::CancellationToken::new();
            cancelled_authority.cancel.cancel();
            assert!(cancelled_authority
                .validate_new_work()
                .unwrap_err()
                .to_string()
                .contains("cancelled"));
            let control: super::finch_ipc_capnp::brain_program_control::Client =
                capnp_rpc::new_client(super::BrainProgramControlImpl {
                    lifecycle: crate::server::BrainLifecycleService::from_server(&server),
                    brain: "shared".into(),
                    run_id: run.run_id,
                    request_seq: prompt.seq,
                    maximum_grant_ceiling: None,
                    effect_audit: Some(authority),
                });
            let execution_id = uuid::Uuid::new_v4();
            let mut reserve = control.reserve_effect_request();
            reserve.get().set_execution_id(&execution_id.to_string());
            let effect = crate::vm::VmSideEffect {
                protocol_version: 1,
                sequence: 0,
                requirement: crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::SessionEmit,
                    selector: crate::vm::ResourceSelector::None,
                },
                output: Vec::new(),
                event: crate::vm::HostSideEffect::Emit {
                    text: "hello".into(),
                },
                origin: crate::vm::SourceOrigin::generated("capnp-effect-audit-test"),
            };
            crate::ipc::checkpoint_codec::encode_vm_side_effect(
                reserve.get().init_effect(),
                &effect,
            )
            .unwrap();
            let reservation = reserve
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reservation()
                .unwrap();
            let permit = reservation
                .begin_request()
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_permit()
                .unwrap();
            let repeated_begin = reservation
                .begin_request()
                .send()
                .promise
                .await
                .err()
                .expect("a raw capability replay must not mint a second permit");
            assert!(repeated_begin.to_string().contains("already begun"));
            assert!(matches!(
                store.snapshot("shared").unwrap().effect_audits[0].state,
                crate::runtime::effect_log::EffectAuditState::AwaitingHostResult
            ));
            let mut invalid_terminal = reservation.not_applied_request();
            invalid_terminal.get().set_reason("too late");
            let invalid_terminal_error = invalid_terminal
                .send()
                .promise
                .await
                .err()
                .expect("begun reservation must reject a permit-free outcome");
            assert!(invalid_terminal_error.to_string().contains("host permit"));

            store
                .release_runner_lease("shared", lease.lease_id)
                .unwrap();
            store
                .acquire_runner_lease("shared", "successor", 1, None, 300_000)
                .unwrap();
            let mut stale_reserve = control.reserve_effect_request();
            stale_reserve
                .get()
                .set_execution_id(&uuid::Uuid::new_v4().to_string());
            crate::ipc::checkpoint_codec::encode_vm_side_effect(
                stale_reserve.get().init_effect(),
                &crate::vm::VmSideEffect {
                    sequence: 1,
                    ..effect.clone()
                },
            )
            .unwrap();
            let stale_reserve_error = stale_reserve
                .send()
                .promise
                .await
                .err()
                .expect("successor lease must invalidate the original reserve capability");
            assert!(stale_reserve_error.to_string().contains("successor"));
            store
                .transition_run(
                    "shared",
                    "daemon",
                    run.run_id,
                    crate::brain::store::BrainRunStatus::Cancelled,
                    Some("turn cancelled before host completion".into()),
                )
                .unwrap();
            let mut finish = permit.finish_request();
            finish.get().init_outcome().init_acknowledged(0);
            finish.send().promise.await.unwrap();
            let mut exact_retry = permit.finish_request();
            exact_retry.get().init_outcome().init_acknowledged(0);
            exact_retry.send().promise.await.unwrap();
            let mut conflicting_retry = permit.finish_request();
            conflicting_retry
                .get()
                .init_outcome()
                .set_failed_partial("changed");
            let conflicting_error = conflicting_retry
                .send()
                .promise
                .await
                .err()
                .expect("a raw permit replay cannot change its terminal outcome");
            assert!(conflicting_error.to_string().contains("different outcome"));

            let snapshot = store.snapshot("shared").unwrap();
            assert!(matches!(snapshot.effect_audits[0].state,
                crate::runtime::effect_log::EffectAuditState::Terminal {
                    outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                        ref outcome_kind
                    }
                } if outcome_kind == "acknowledged"));
            assert!(!snapshot.events.iter().any(|event| matches!(
                event.kind,
                crate::brain::store::BrainEventKind::ToolResult { .. }
            )));

            // Reconstruct the raw server capability after a daemon/store
            // reload. The original callback and lease are stale and the run
            // is terminal, but a caller that lost the original reserve ACK
            // must still learn that its exact identity is durably fenced.
            let restarted = crate::brain::store::BrainStore::with_root(
                "box.local",
                Some(temp.path().join("brains")),
            );
            let restarted_server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    restarted.clone(),
                    crate::brain::credential::BrainCredentialAuthority::ephemeral([45; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let replay_control: super::finch_ipc_capnp::brain_program_control::Client =
                capnp_rpc::new_client(super::BrainProgramControlImpl {
                    lifecycle: crate::server::BrainLifecycleService::from_server(&restarted_server),
                    brain: "shared".into(),
                    run_id: run.run_id,
                    request_seq: prompt.seq,
                    maximum_grant_ceiling: None,
                    effect_audit: Some(super::BrainEffectAuditRpcAuthority {
                        store: restarted.clone(),
                        grant: original_grant,
                        runners: restarted_server.brain_runners().clone(),
                        brain: "shared".into(),
                        lease_id: lease.lease_id,
                        connection_id: None,
                        active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        cancel: tokio_util::sync::CancellationToken::new(),
                    }),
                });
            let mut replay = replay_control.reserve_effect_request();
            replay.get().set_execution_id(&execution_id.to_string());
            crate::ipc::checkpoint_codec::encode_vm_side_effect(
                replay.get().init_effect(),
                &effect,
            )
            .unwrap();
            replay
                .send()
                .promise
                .await
                .unwrap()
                .get()
                .unwrap()
                .get_reservation()
                .unwrap();

            let mut conflicting = replay_control.reserve_effect_request();
            conflicting
                .get()
                .set_execution_id(&execution_id.to_string());
            crate::ipc::checkpoint_codec::encode_vm_side_effect(
                conflicting.get().init_effect(),
                &crate::vm::VmSideEffect {
                    event: crate::vm::HostSideEffect::Emit {
                        text: "changed".into(),
                    },
                    ..effect
                },
            )
            .unwrap();
            let error = conflicting
                .send()
                .promise
                .await
                .err()
                .expect("conflicting raw reserve replay must fail closed after reload");
            assert!(error.to_string().contains("conflicting"));
        }));
    }

    struct EffectEofRunner {
        begin: bool,
        count: usize,
    }

    fn delayed_finish_effect_audit(
        control: crate::server::RunnerEffectAuditControl,
        finish_started: tokio::sync::oneshot::Sender<()>,
        finish_release: std::sync::Arc<tokio::sync::Notify>,
        physical_effects: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> crate::server::RunnerEffectAuditControl {
        let (control_tx, mut control_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::task::spawn_local(async move {
            let Some(crate::server::RunnerEffectAuditControlRequest::Reserve {
                execution_id,
                effect,
                response_tx,
            }) = control_rx.recv().await
            else {
                return;
            };
            let reservation = match control.reserve(execution_id, effect).await {
                Ok(reservation) => reservation,
                Err(error) => {
                    let _ = response_tx.send(Err(error));
                    return;
                }
            };
            let (reservation_tx, mut reservation_rx) = tokio::sync::mpsc::unbounded_channel();
            let _ = response_tx.send(Ok(crate::server::RunnerEffectAuditReservation::new(
                reservation_tx,
            )));
            let Some(request) = reservation_rx.recv().await else {
                return;
            };
            match request {
                crate::server::RunnerEffectAuditReservationRequest::Begin { response_tx } => {
                    let permit = match reservation.begin().await {
                        Ok(permit) => permit,
                        Err(error) => {
                            let _ = response_tx.send(Err(error));
                            return;
                        }
                    };
                    let (permit_tx, mut permit_rx) = tokio::sync::mpsc::unbounded_channel();
                    let _ =
                        response_tx.send(Ok(crate::server::RunnerHostEffectPermit::new(permit_tx)));
                    let Some(request) = permit_rx.recv().await else {
                        return;
                    };
                    physical_effects.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = finish_started.send(());
                    finish_release.notified().await;
                    let result = permit.finish(request.outcome).await;
                    let _ = request.response_tx.send(result);
                }
                crate::server::RunnerEffectAuditReservationRequest::NotApplied {
                    reason,
                    response_tx,
                } => {
                    let result = reservation.not_applied(reason).await;
                    let _ = response_tx.send(result);
                }
            }
        });
        crate::server::RunnerEffectAuditControl::new(control_tx)
    }

    struct ProviderSubmitProgramGenerator {
        input: serde_json::Value,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl crate::generators::Generator for ProviderSubmitProgramGenerator {
        async fn generate(
            &self,
            _messages: Vec<crate::claude::Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> anyhow::Result<crate::generators::GeneratorResponse> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::ensure!(call == 0, "provider continuation escaped cancellation");
            Ok(crate::generators::GeneratorResponse {
                text: String::new(),
                content_blocks: vec![crate::claude::ContentBlock::ToolUse {
                    id: "effect-tool".into(),
                    name: "submit_program".into(),
                    input: self.input.clone(),
                }],
                tool_uses: vec![crate::generators::ToolUse {
                    id: "effect-tool".into(),
                    name: "submit_program".into(),
                    input: self.input.clone(),
                }],
                metadata: crate::generators::ResponseMetadata {
                    generator: "effect-test".into(),
                    model: "effect-test".into(),
                    confidence: None,
                    stop_reason: Some("tool_use".into()),
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: None,
                    primary_allowance_used_percent: None,
                    secondary_allowance_used_percent: None,
                },
            })
        }

        async fn generate_stream(
            &self,
            _messages: Vec<crate::claude::Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> anyhow::Result<
            Option<tokio::sync::mpsc::Receiver<anyhow::Result<crate::generators::StreamChunk>>>,
        > {
            Ok(None)
        }

        fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
            static CAPABILITIES: crate::generators::GeneratorCapabilities =
                crate::generators::GeneratorCapabilities {
                    supports_streaming: false,
                    supports_tools: true,
                    supports_conversation: true,
                    max_context_messages: Some(8),
                };
            &CAPABILITIES
        }

        fn name(&self) -> &str {
            "effect-test"
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runner_reconnect_installs_the_canonical_checkpoint_over_a_late_local_commit() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let store = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test(
                        store,
                        crate::brain::credential::BrainCredentialAuthority::ephemeral([93; 32]),
                        "test-password".into(),
                        temp.path(),
                    )
                    .unwrap(),
                );
                let daemon: super::finch_ipc_capnp::finch_daemon::Client = capnp_rpc::new_client(
                    FinchDaemonImpl::new(std::sync::Arc::clone(&server), uuid::Uuid::new_v4()),
                );
                let ipc = crate::ipc::IpcClient::from_test_client(daemon);
                let snapshot = ipc.brain_snapshot("shared").await.unwrap();
                let subject = "runner@box.local/reconnect-rollback";
                ipc.brain_claim_runner_identity(subject).await.unwrap();
                let lease = ipc
                    .brain_acquire_runner("shared", subject, &snapshot.environment, None, 300_000)
                    .await
                    .unwrap();

                let runtime = std::sync::Arc::new(crate::runtime::ProgramRuntime::new());
                runtime
                    .submit_typed_only(crate::runtime::ProgramSubmission {
                        language: crate::programs::ProgramLanguage::Forth,
                        source_id: Some("cancelled-late-local-commit".into()),
                        source: "1".into(),
                        intent: "prove reconnect rollback uses daemon bootstrap".into(),
                        effect: crate::programs::ExecutionEffect::Pure,
                        declared_capabilities: Vec::new(),
                        manifest_generation: runtime.manifest_generation(),
                        expected_revision: Some(0),
                        budget: None,
                    })
                    .await
                    .unwrap();
                assert_eq!(runtime.revision(), 1);

                let generator: std::sync::Arc<dyn crate::generators::Generator> =
                    std::sync::Arc::new(ProviderSubmitProgramGenerator {
                        input: serde_json::Value::Null,
                        calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    });
                let registry = crate::tools::registry::ToolRegistry::new();
                let permissions = crate::tools::permissions::PermissionManager::new();
                let executor = std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::tools::executor::ToolExecutor::new(
                        registry,
                        permissions,
                        temp.path().join("tool-patterns.json"),
                    )
                    .unwrap(),
                ));
                let event_loop = crate::cli::repl_event::EventLoop::new_named_brain_test_runner(
                    generator,
                    Vec::new(),
                    executor,
                    std::sync::Arc::clone(&runtime),
                );
                let bootstrap = ipc
                    .register_brain_runner(
                        "shared",
                        lease.lease_id,
                        event_loop.named_brain_event_sender_for_test(),
                    )
                    .await
                    .unwrap();
                assert_eq!(bootstrap.runtime_revision, 0);
                event_loop
                    .install_runner_bootstrap(bootstrap)
                    .await
                    .unwrap();
                assert_eq!(runtime.revision(), 0);
                assert!(runtime
                    .revision_history()
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.revision == 0)
                    .unwrap()
                    .stack
                    .is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runner_renewal_reconciles_active_work_before_successor_checkpoint() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let store = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test(
                        store,
                        crate::brain::credential::BrainCredentialAuthority::ephemeral([83; 32]),
                        "test-password".into(),
                        temp.path(),
                    )
                    .unwrap(),
                );
                let daemon: super::finch_ipc_capnp::finch_daemon::Client = capnp_rpc::new_client(
                    FinchDaemonImpl::new(std::sync::Arc::clone(&server), uuid::Uuid::new_v4()),
                );
                let ipc = crate::ipc::IpcClient::from_test_client(daemon);
                let snapshot = ipc.brain_snapshot("shared").await.unwrap();
                let subject = "runner@box.local/renewal-active-work";
                ipc.brain_claim_runner_identity(subject).await.unwrap();
                let lease = ipc
                    .brain_acquire_runner("shared", subject, &snapshot.environment, None, 300_000)
                    .await
                    .unwrap();

                let runtime = std::sync::Arc::new(crate::runtime::ProgramRuntime::new());
                runtime
                    .submit_typed_only(crate::runtime::ProgramSubmission {
                        language: crate::programs::ProgramLanguage::Forth,
                        source_id: Some("cancelled-active-renewal-state".into()),
                        source: "1".into(),
                        intent: "model a local commit crossing cancellation".into(),
                        effect: crate::programs::ExecutionEffect::Pure,
                        declared_capabilities: Vec::new(),
                        manifest_generation: runtime.manifest_generation(),
                        expected_revision: Some(0),
                        budget: None,
                    })
                    .await
                    .unwrap();
                assert_eq!(runtime.revision(), 1);

                let generator: std::sync::Arc<dyn crate::generators::Generator> =
                    std::sync::Arc::new(ProviderSubmitProgramGenerator {
                        input: serde_json::Value::Null,
                        calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    });
                let executor = std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::tools::executor::ToolExecutor::new(
                        crate::tools::registry::ToolRegistry::new(),
                        crate::tools::permissions::PermissionManager::new(),
                        temp.path().join("tool-patterns.json"),
                    )
                    .unwrap(),
                ));
                let mut event_loop = crate::cli::repl_event::EventLoop::new_named_brain_test_runner(
                    generator,
                    Vec::new(),
                    executor,
                    std::sync::Arc::clone(&runtime),
                );
                event_loop.set_ipc_client_for_test(ipc.clone());
                let active_run = crate::brain::store::RunId(uuid::Uuid::new_v4());
                let _program_lifecycle =
                    event_loop.mark_named_brain_program_active_for_test(active_run);
                event_loop
                    .handle_named_brain_event_for_test(
                        crate::cli::repl_event::ReplEvent::RunnerLeaseStatus {
                            brain: "shared".into(),
                            environment: snapshot.environment,
                            epoch: 0,
                            lease_id: Some(lease.lease_id),
                            detail: "renewed".into(),
                        },
                    )
                    .await
                    .unwrap();
                assert!(event_loop.has_pending_runner_bootstrap_for_test());
                assert_eq!(
                    runtime.revision(),
                    1,
                    "renewal must not replace state while the prior callback is active"
                );

                let (reconciliation_tx, reconciliation_rx) = tokio::sync::oneshot::channel();
                event_loop
                    .handle_named_brain_event_for_test(
                        crate::cli::repl_event::ReplEvent::NamedBrainProgramFinished {
                            run_id: active_run,
                            preserve_local_checkpoint: false,
                            reconciliation_tx,
                        },
                    )
                    .await
                    .unwrap();
                reconciliation_rx.await.unwrap().unwrap();
                assert!(!event_loop.has_pending_runner_bootstrap_for_test());
                assert_eq!(runtime.revision(), 0);

                runtime
                    .submit_typed_only(crate::runtime::ProgramSubmission {
                        language: crate::programs::ProgramLanguage::Forth,
                        source_id: Some("successor-after-renewal".into()),
                        source: "2".into(),
                        intent: "prove cancelled ancestry is absent".into(),
                        effect: crate::programs::ExecutionEffect::Pure,
                        declared_capabilities: Vec::new(),
                        manifest_generation: runtime.manifest_generation(),
                        expected_revision: Some(0),
                        budget: None,
                    })
                    .await
                    .unwrap();
                let successor = runtime
                    .revision_history()
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.revision == 1)
                    .unwrap();
                assert_eq!(successor.stack, vec![crate::vm::TypedValue::Int(2)]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_program_cancel_ack_follows_finished_event_and_physical_settlement() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let runtime = std::sync::Arc::new(crate::runtime::ProgramRuntime::new());
                let generator: std::sync::Arc<dyn crate::generators::Generator> =
                    std::sync::Arc::new(ProviderSubmitProgramGenerator {
                        input: serde_json::Value::Null,
                        calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                    });
                let executor = std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::tools::executor::ToolExecutor::new(
                        crate::tools::registry::ToolRegistry::new(),
                        crate::tools::permissions::PermissionManager::new(),
                        temp.path().join("tool-patterns.json"),
                    )
                    .unwrap(),
                ));
                let mut event_loop = crate::cli::repl_event::EventLoop::new_named_brain_test_runner(
                    generator,
                    Vec::new(),
                    executor,
                    runtime,
                );
                let run_id = crate::brain::store::RunId(uuid::Uuid::new_v4());
                let (program_cancel, finished_tx) =
                    event_loop.mark_named_brain_program_active_for_test(run_id);
                let (event_tx, driver) = event_loop.start_named_brain_test_runner("shared".into());
                let (response_tx, mut response_rx) = tokio::sync::oneshot::channel();
                event_tx
                    .send(
                        crate::cli::repl_event::ReplEvent::NamedBrainRunCancelRequested(
                            crate::cli::repl_event::events::BoundedRunnerCancelRequest {
                                request: crate::server::RunnerCancelRequest {
                                    brain: "shared".into(),
                                    run_id,
                                    response_tx,
                                },
                                cancel: tokio_util::sync::CancellationToken::new(),
                                deadline: tokio::time::Instant::now()
                                    + std::time::Duration::from_secs(10),
                            },
                        ),
                    )
                    .unwrap();
                tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    program_cancel.cancelled(),
                )
                .await
                .expect("event loop did not request program cancellation");
                assert!(matches!(
                    response_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));

                let (reconciliation_tx, reconciliation_rx) = tokio::sync::oneshot::channel();
                event_tx
                    .send(
                        crate::cli::repl_event::ReplEvent::NamedBrainProgramFinished {
                            run_id,
                            preserve_local_checkpoint: false,
                            reconciliation_tx,
                        },
                    )
                    .unwrap();
                assert!(matches!(
                    response_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));
                reconciliation_rx.await.unwrap().unwrap();
                assert!(matches!(
                    response_rx.try_recv(),
                    Err(tokio::sync::oneshot::error::TryRecvError::Empty)
                ));
                finished_tx.send_replace(true);
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), &mut response_rx)
                        .await
                        .expect("cancellation ACK did not follow physical settlement")
                        .unwrap()
                        .unwrap(),
                    true
                );
                event_tx
                    .send(crate::cli::repl_event::ReplEvent::Shutdown)
                    .unwrap();
                driver.await.unwrap().unwrap();
            })
            .await;
    }

    impl super::finch_ipc_capnp::brain_runner::Server for EffectEofRunner {
        fn run_program(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let request = match params.get().and_then(|params| params.get_request()) {
                Ok(request) => request,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let control = match request.get_control() {
                Ok(control) => control,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let begin = self.begin;
            let count = self.count;
            capnp::capability::Promise::from_future(async move {
                for sequence in 0..count {
                    let mut reserve = control.reserve_effect_request();
                    reserve
                        .get()
                        .set_execution_id(&uuid::Uuid::new_v4().to_string());
                    crate::ipc::checkpoint_codec::encode_vm_side_effect(
                        reserve.get().init_effect(),
                        &crate::vm::VmSideEffect {
                            protocol_version: 1,
                            sequence: sequence as u64,
                            requirement: crate::vm::CapabilityRequirement {
                                capability: crate::vm::CapabilityKind::SessionEmit,
                                selector: crate::vm::ResourceSelector::None,
                            },
                            output: Vec::new(),
                            event: crate::vm::HostSideEffect::Emit { text: "eof".into() },
                            origin: crate::vm::SourceOrigin::generated("raw-eof-effect-audit"),
                        },
                    )
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                    let reservation = reserve.send().promise.await?.get()?.get_reservation()?;
                    if begin {
                        let _permit = reservation
                            .begin_request()
                            .send()
                            .promise
                            .await?
                            .get()?
                            .get_permit()?;
                    }
                }
                Err(capnp::Error::disconnected(
                    "synthetic raw frontend EOF".into(),
                ))
            })
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }
    }

    struct EffectNormalRunner {
        begin: bool,
        remote_disconnect_error: bool,
        permit_tx: Option<
            tokio::sync::oneshot::Sender<
                Option<super::finch_ipc_capnp::brain_host_effect_permit::Client>,
            >,
        >,
    }

    impl super::finch_ipc_capnp::brain_runner::Server for EffectNormalRunner {
        fn run_program(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            mut results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let control = match params
                .get()
                .and_then(|params| params.get_request())
                .and_then(|request| request.get_control())
            {
                Ok(control) => control,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let begin = self.begin;
            let remote_disconnect_error = self.remote_disconnect_error;
            let permit_tx = self.permit_tx.take().expect("normal runner called twice");
            capnp::capability::Promise::from_future(async move {
                let mut reserve = control.reserve_effect_request();
                reserve
                    .get()
                    .set_execution_id(&uuid::Uuid::new_v4().to_string());
                crate::ipc::checkpoint_codec::encode_vm_side_effect(
                    reserve.get().init_effect(),
                    &crate::vm::VmSideEffect {
                        protocol_version: 1,
                        sequence: 0,
                        requirement: crate::vm::CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::SessionEmit,
                            selector: crate::vm::ResourceSelector::None,
                        },
                        output: Vec::new(),
                        event: crate::vm::HostSideEffect::Emit {
                            text: "normal".into(),
                        },
                        origin: crate::vm::SourceOrigin::generated("raw-normal-effect-audit"),
                    },
                )
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                let reservation = reserve.send().promise.await?.get()?.get_reservation()?;
                let permit = if begin {
                    Some(
                        reservation
                            .begin_request()
                            .send()
                            .promise
                            .await?
                            .get()?
                            .get_permit()?,
                    )
                } else {
                    None
                };
                let _ = permit_tx.send(permit);
                if remote_disconnect_error {
                    return Err(capnp::Error::disconnected(
                        "application exception with a misleading disconnected kind".into(),
                    ));
                }
                results
                    .get()
                    .init_result()
                    .set_error("synthetic normal application return");
                Ok(())
            })
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }
    }

    struct CancelObservedProgramRunner {
        started: Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>,
        cancelled: tokio::sync::mpsc::UnboundedSender<crate::brain::store::RunId>,
        stop: std::sync::Arc<tokio::sync::Notify>,
    }

    impl super::finch_ipc_capnp::brain_runner::Server for CancelObservedProgramRunner {
        fn run_program(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = match params
                .get()
                .and_then(|params| params.get_request())
                .and_then(|request| request.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                }) {
                Ok(run_id) => run_id,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let started = self.started.take().expect("program callback called twice");
            let stop = std::sync::Arc::clone(&self.stop);
            capnp::capability::Promise::from_future(async move {
                let _ = started.send(run_id);
                stop.notified().await;
                Err(capnp::Error::disconnected(
                    "cancelled abandoned program callback".into(),
                ))
            })
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn cancel_run(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            mut results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = params
                .get()
                .and_then(|params| params.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                });
            match run_id {
                Ok(run_id) => {
                    let _ = self.cancelled.send(run_id);
                    self.stop.notify_waiters();
                    results.get().set_cancelled(true);
                    results.get().set_error("");
                    capnp::capability::Promise::ok(())
                }
                Err(error) => capnp::capability::Promise::err(error),
            }
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }
    }

    struct DelayedMemoryCancellationRunner {
        started: Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>,
        cancelled: tokio::sync::mpsc::UnboundedSender<crate::brain::store::RunId>,
        cancel: tokio_util::sync::CancellationToken,
        release: std::sync::Arc<tokio::sync::Notify>,
        finished: tokio::sync::watch::Sender<bool>,
        memory: std::sync::Arc<crate::memory::MemorySystem>,
        provenance: crate::memory::BrainConversationProvenance,
    }

    struct MalformedErrorRunner;

    impl super::finch_ipc_capnp::brain_runner::Server for MalformedErrorRunner {
        fn run_program(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("wire errors only".into()))
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("wire errors only".into()))
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            mut results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            results.get().set_cancelled(true);
            results.get().set_error(capnp::text::Reader(&[0xff, 0xfe]));
            capnp::capability::Promise::ok(())
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            mut results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            results.get().set_inserted(7);
            results.get().set_error(capnp::text::Reader(&[0xff, 0xfe]));
            capnp::capability::Promise::ok(())
        }
    }

    impl super::finch_ipc_capnp::brain_runner::Server for DelayedMemoryCancellationRunner {
        fn run_program(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            _results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("memory only".into()))
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("memory only".into()))
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("memory only".into()))
        }

        fn project_memory(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            mut results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = params
                .get()
                .and_then(|params| params.get_request())
                .and_then(|request| request.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                });
            let run_id = match run_id {
                Ok(run_id) => run_id,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let started = self.started.take().expect("memory callback called twice");
            let cancel = self.cancel.clone();
            let release = std::sync::Arc::clone(&self.release);
            let finished = self.finished.clone();
            let memory = std::sync::Arc::clone(&self.memory);
            let provenance = self.provenance.clone();
            capnp::capability::Promise::from_future(async move {
                let _ = started.send(run_id);
                cancel.cancelled().await;
                release.notified().await;
                let inserted = memory
                    .insert_brain_conversation(
                        "assistant",
                        "late frontend insertion completed",
                        Some("test-model"),
                        Some("shared"),
                        &provenance,
                    )
                    .await
                    .map_err(|error| capnp::Error::failed(error.to_string()))?;
                results.get().set_inserted(if inserted { 1 } else { 0 });
                results.get().set_error("memory projection cancelled");
                finished.send_replace(true);
                Ok(())
            })
        }

        fn cancel_memory(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::CancelMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = params
                .get()
                .and_then(|params| params.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                });
            let run_id = match run_id {
                Ok(run_id) => run_id,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let _ = self.cancelled.send(run_id);
            self.cancel.cancel();
            let mut finished = self.finished.subscribe();
            capnp::capability::Promise::from_future(async move {
                let already_finished = *finished.borrow();
                if !already_finished {
                    finished.changed().await.map_err(|_| {
                        capnp::Error::failed(
                            "memory callback ended without physical settlement".into(),
                        )
                    })?;
                }
                Ok(())
            })
        }
    }

    struct NonQuiescentExpiredProgramRunner {
        started: Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>,
        cancelled: tokio::sync::mpsc::UnboundedSender<crate::brain::store::RunId>,
        release: std::sync::Arc<tokio::sync::Notify>,
        late_effect_rejected: Option<tokio::sync::oneshot::Sender<bool>>,
    }

    impl super::finch_ipc_capnp::brain_runner::Server for NonQuiescentExpiredProgramRunner {
        fn run_program(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            mut results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let request = match params.get().and_then(|params| params.get_request()) {
                Ok(request) => request,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let run_id = match request
                .get_run_id()
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                }) {
                Ok(run_id) => run_id,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let control = match request.get_control() {
                Ok(control) => control,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let started = self
                .started
                .take()
                .expect("expired callback received more than one program");
            let late_effect_rejected = self
                .late_effect_rejected
                .take()
                .expect("expired callback attempted more than one late effect");
            let release = std::sync::Arc::clone(&self.release);
            capnp::capability::Promise::from_future(async move {
                let _ = started.send(run_id);
                release.notified().await;

                let mut reserve = control.reserve_effect_request();
                reserve
                    .get()
                    .set_execution_id(&uuid::Uuid::new_v4().to_string());
                crate::ipc::checkpoint_codec::encode_vm_side_effect(
                    reserve.get().init_effect(),
                    &crate::vm::VmSideEffect {
                        protocol_version: 1,
                        sequence: 0,
                        requirement: crate::vm::CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::SessionEmit,
                            selector: crate::vm::ResourceSelector::None,
                        },
                        output: Vec::new(),
                        event: crate::vm::HostSideEffect::Emit {
                            text: "stale generation effect".into(),
                        },
                        origin: crate::vm::SourceOrigin::generated("expired-runner-late-effect"),
                    },
                )
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
                let rejected = reserve.send().promise.await.is_err();
                let _ = late_effect_rejected.send(rejected);
                results
                    .get()
                    .init_result()
                    .set_error("late stale generation result");
                Ok(())
            })
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn cancel_run(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            mut results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = params
                .get()
                .and_then(|params| params.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                });
            match run_id {
                Ok(run_id) => {
                    let _ = self.cancelled.send(run_id);
                    results.get().set_cancelled(true);
                    results.get().set_error("");
                    capnp::capability::Promise::ok(())
                }
                Err(error) => capnp::capability::Promise::err(error),
            }
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }
    }

    struct ImmediateProgramRunner {
        started: Option<tokio::sync::oneshot::Sender<crate::brain::store::RunId>>,
        checkpoint: crate::vm::TypedRuntimeCheckpoint,
    }

    impl super::finch_ipc_capnp::brain_runner::Server for ImmediateProgramRunner {
        fn run_program(
            &mut self,
            params: super::finch_ipc_capnp::brain_runner::RunProgramParams,
            mut results: super::finch_ipc_capnp::brain_runner::RunProgramResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            let run_id = match params
                .get()
                .and_then(|params| params.get_request())
                .and_then(|request| request.get_run_id())
                .and_then(|value| value.to_str().map_err(capnp::Error::from))
                .and_then(|value| {
                    uuid::Uuid::parse_str(value)
                        .map(crate::brain::store::RunId)
                        .map_err(|error| capnp::Error::failed(error.to_string()))
                }) {
                Ok(run_id) => run_id,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let started = self
                .started
                .take()
                .expect("replacement callback received more than one program");
            let _ = started.send(run_id);
            let mut result = results.get().init_result();
            result.set_output("replacement completed");
            result.set_runtime_revision(1);
            if let Err(error) =
                super::encode_checkpoint(result.reborrow().init_checkpoint(), &self.checkpoint)
            {
                return capnp::capability::Promise::err(capnp::Error::failed(error.to_string()));
            }
            result.set_error("");
            capnp::capability::Promise::ok(())
        }

        fn run_turn(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::RunTurnParams,
            _results: super::finch_ipc_capnp::brain_runner::RunTurnResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn cancel_run(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::CancelRunParams,
            _results: super::finch_ipc_capnp::brain_runner::CancelRunResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }

        fn project_memory(
            &mut self,
            _params: super::finch_ipc_capnp::brain_runner::ProjectMemoryParams,
            _results: super::finch_ipc_capnp::brain_runner::ProjectMemoryResults,
        ) -> capnp::capability::Promise<(), capnp::Error> {
            capnp::capability::Promise::err(capnp::Error::unimplemented("program only".into()))
        }
    }

    async fn observe_cancel_before_forwarding_returns<F>(
        cancelled_rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::brain::store::RunId>,
        forwarding: &mut F,
    ) -> (Option<crate::brain::store::RunId>, bool)
    where
        F: std::future::Future<Output = ()> + Unpin,
    {
        tokio::select! {
            biased;
            cancelled = cancelled_rx.recv() => (cancelled, false),
            _ = forwarding => (cancelled_rx.try_recv().ok(), true),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_abandoning_daemon_rpc_physically_cancels_the_exact_frontend_callback() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Program {
                    language: crate::brain::store::ProgramLanguage::Lisp,
                    source: "stuck".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store,
                crate::brain::credential::BrainCredentialAuthority::ephemeral([47; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let (started_tx, mut started_rx) = tokio::sync::oneshot::channel();
        let (cancelled_tx, mut cancelled_rx) = tokio::sync::mpsc::unbounded_channel();
        let runner: super::finch_ipc_capnp::brain_runner::Client =
            capnp_rpc::new_client(CancelObservedProgramRunner {
                started: Some(started_tx),
                cancelled: cancelled_tx,
                stop: std::sync::Arc::new(tokio::sync::Notify::new()),
            });
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let request = crate::server::RunnerProgramRequest {
            brain: "shared".into(),
            run_id: run.run_id,
            request_seq: prompt.seq,
            language: crate::brain::store::ProgramLanguage::Lisp,
            source: "stuck".into(),
            interaction: crate::server::RunnerProgramInteraction::Interactive,
            grant_ceiling: None,
            control_tx: None,
            effect_audit: None,
            response_tx,
        };
        let mut forwarding = Box::pin(super::forward_test_runner_request(
            runner,
            std::sync::Arc::clone(&server),
            crate::server::RunnerRequest::Program(request),
        ));
        let started = tokio::select! {
            started = &mut started_rx => started.unwrap(),
            _ = &mut forwarding => panic!("program forwarding ended before it started"),
        };
        assert_eq!(started, run.run_id);
        assert!(matches!(
            futures::poll!(&mut forwarding),
            std::task::Poll::Pending
        ));
        assert!(matches!(
            cancelled_rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        drop(response_rx);
        let (cancelled, forwarding_completed) =
            observe_cancel_before_forwarding_returns(&mut cancelled_rx, &mut forwarding).await;
        let cancelled = cancelled.expect("program forwarding ended without physical cancellation");
        assert_eq!(cancelled, run.run_id);
        if !forwarding_completed {
            forwarding.await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_abandoned_memory_projection_waits_for_late_frontend_insertion_to_quiesce() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "remember this".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        let brain_id = store.snapshot("shared").unwrap().brain_id;
        store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store,
                crate::brain::credential::BrainCredentialAuthority::ephemeral([57; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let (started_tx, mut started_rx) = tokio::sync::oneshot::channel();
        let (cancelled_tx, mut cancelled_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let (finished_tx, _finished_rx) = tokio::sync::watch::channel(false);
        let memory = std::sync::Arc::new(
            crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
                db_path: temp.path().join("late-memory.db"),
                use_neural_embeddings: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let runner: super::finch_ipc_capnp::brain_runner::Client =
            capnp_rpc::new_client(DelayedMemoryCancellationRunner {
                started: Some(started_tx),
                cancelled: cancelled_tx,
                cancel: tokio_util::sync::CancellationToken::new(),
                release: std::sync::Arc::clone(&release),
                finished: finished_tx,
                memory: std::sync::Arc::clone(&memory),
                provenance: crate::memory::BrainConversationProvenance {
                    brain_id: brain_id.0.to_string(),
                    run_id: run.run_id.0.to_string(),
                    request_seq: prompt.seq,
                },
            });
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let request = crate::server::RunnerMemoryProjectionRequest {
            brain_id,
            brain: "shared".into(),
            run_id: run.run_id,
            request_seq: prompt.seq,
            prompt: "remember this".into(),
            source: "stored answer".into(),
            response_tx,
        };
        let mut forwarding = Box::pin(super::forward_test_runner_request(
            runner,
            std::sync::Arc::clone(&server),
            crate::server::RunnerRequest::ProjectMemory(request),
        ));
        let started = tokio::select! {
            started = &mut started_rx => started.unwrap(),
            _ = &mut forwarding => panic!("memory forwarding ended before it started"),
        };
        assert_eq!(started, run.run_id);
        drop(response_rx);
        let cancelled = tokio::select! {
            cancelled = cancelled_rx.recv() => cancelled.unwrap(),
            _ = &mut forwarding => panic!("memory forwarding returned before cancellation ACK"),
        };
        assert_eq!(cancelled, run.run_id);
        assert!(matches!(
            futures::poll!(&mut forwarding),
            std::task::Poll::Pending
        ));
        assert_eq!(memory.stats().await.unwrap().conversation_count, 0);

        release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), forwarding)
            .await
            .expect("memory forwarding did not wait for and observe physical settlement");
        assert_eq!(memory.stats().await.unwrap().conversation_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_memory_and_cancel_wire_responses_reject_malformed_error_text() {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "malformed response".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        let brain_id = store.snapshot("shared").unwrap().brain_id;
        store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store,
                crate::brain::credential::BrainCredentialAuthority::ephemeral([59; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let runner: super::finch_ipc_capnp::brain_runner::Client =
            capnp_rpc::new_client(MalformedErrorRunner);

        let (memory_tx, memory_rx) = tokio::sync::oneshot::channel();
        let memory = crate::server::RunnerRequest::ProjectMemory(
            crate::server::RunnerMemoryProjectionRequest {
                brain_id,
                brain: "shared".into(),
                run_id: run.run_id,
                request_seq: prompt.seq,
                prompt: "remember".into(),
                source: "source".into(),
                response_tx: memory_tx,
            },
        );
        let (_, memory_result) = tokio::join!(
            super::forward_test_runner_request(
                runner.clone(),
                std::sync::Arc::clone(&server),
                memory,
            ),
            memory_rx,
        );
        let memory_error = memory_result.unwrap().unwrap_err();
        assert!(memory_error.contains("runner memory projection error is not valid UTF-8"));

        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let cancel = crate::server::RunnerRequest::Cancel(crate::server::RunnerCancelRequest {
            brain: "shared".into(),
            run_id: run.run_id,
            response_tx: cancel_tx,
        });
        let (_, cancel_result) = tokio::join!(
            super::forward_test_runner_request(runner, server, cancel),
            cancel_rx,
        );
        let cancel_error = cancel_result.unwrap().unwrap_err();
        assert!(cancel_error.contains("runner cancellation error is not valid UTF-8"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn test_lease_expiry_waits_for_nonquiescent_ipc_callback_before_replacement_lane() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let store = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test_with_runner_deadlines(
                        store.clone(),
                        crate::brain::credential::BrainCredentialAuthority::ephemeral([92; 32]),
                        "test-password".into(),
                        temp.path(),
                        crate::server::RunnerDeadlines {
                            program: std::time::Duration::from_secs(2),
                            turn: std::time::Duration::from_secs(2),
                            cancel: std::time::Duration::from_secs(2),
                            project_memory: std::time::Duration::from_secs(2),
                            callback_cleanup: std::time::Duration::from_millis(50),
                        },
                    )
                    .unwrap(),
                );
                let lifecycle = crate::server::BrainLifecycleService::from_server(&server);
                let attachment = lifecycle
                    .attach(
                        "shared",
                        "alice",
                        crate::brain::store::AttachmentRole::Driver,
                        None,
                    )
                    .unwrap();
                let attachment_id = attachment.attachment_id;
                let connection_id = attachment.connection_id.unwrap();
                let _watch = lifecycle
                    .watch("shared", attachment_id, connection_id)
                    .unwrap();
                let environment = store.environment().clone();
                let expired = lifecycle
                    .acquire_runner(
                        "shared",
                        "runner@box.local/expired",
                        &environment,
                        None,
                        300_000,
                    )
                    .unwrap();
                let expired_lease_id = expired.lease_id;
                let expired_expires_ms = expired.expires_ms;

                let release_old = std::sync::Arc::new(tokio::sync::Notify::new());
                let (old_started_tx, old_started_rx) = tokio::sync::oneshot::channel();
                let (old_cancelled_tx, mut old_cancelled_rx) =
                    tokio::sync::mpsc::unbounded_channel();
                let (late_effect_tx, late_effect_rx) = tokio::sync::oneshot::channel();
                let old_runner: super::finch_ipc_capnp::brain_runner::Client =
                    capnp_rpc::new_client(NonQuiescentExpiredProgramRunner {
                        started: Some(old_started_tx),
                        cancelled: old_cancelled_tx,
                        release: std::sync::Arc::clone(&release_old),
                        late_effect_rejected: Some(late_effect_tx),
                    });
                let (old_tx, mut old_rx) = tokio::sync::mpsc::unbounded_channel();
                let old_registration = server.brain_runners().register_bounded(
                    "shared",
                    expired_lease_id,
                    old_tx,
                );
                let old_server = std::sync::Arc::clone(&server);
                let old_bridge = tokio::task::spawn_local(async move {
                    while let Some(request) = old_rx.recv().await {
                        super::forward_runner_request(
                            old_runner.clone(),
                            std::sync::Arc::clone(&old_server),
                            request,
                            expired_lease_id,
                            None,
                            Some(old_registration),
                        )
                        .await;
                    }
                });

                let old_lifecycle = lifecycle.clone();
                let mut old_submission = tokio::task::spawn_local(async move {
                    old_lifecycle
                        .submit(
                            "shared",
                            attachment_id,
                            connection_id,
                            crate::brain::store::BrainEventKind::Program {
                                language: crate::brain::store::ProgramLanguage::Lisp,
                                source: "old callback".into(),
                            },
                        )
                        .await
                });
                let old_run_id = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    async {
                        tokio::select! {
                            started = old_started_rx => started.expect("old IPC callback start signal dropped"),
                            outcome = &mut old_submission => panic!(
                                "old submission ended before its IPC callback started: {outcome:?}"
                            ),
                        }
                    },
                )
                .await
                .expect("old IPC callback did not start");

                assert!(lifecycle
                    .expire_runner_lease_if_due("shared", expired_lease_id, expired_expires_ms)
                    .unwrap());
                assert_eq!(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        old_cancelled_rx.recv(),
                    )
                    .await
                    .expect("expired IPC callback did not receive physical cancellation"),
                    Some(old_run_id)
                );

                let replacement = lifecycle
                    .acquire_runner(
                        "shared",
                        "runner@box.local/replacement",
                        &environment,
                        None,
                        300_000,
                    )
                    .unwrap();
                let replacement_lease_id = replacement.lease_id;
                let checkpoint = crate::runtime::ProgramRuntime::new()
                    .revision_history()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .checkpoint
                    .unwrap();
                let (replacement_started_tx, mut replacement_started_rx) =
                    tokio::sync::oneshot::channel();
                let replacement_runner: super::finch_ipc_capnp::brain_runner::Client =
                    capnp_rpc::new_client(ImmediateProgramRunner {
                        started: Some(replacement_started_tx),
                        checkpoint,
                    });
                let (replacement_tx, mut replacement_rx) =
                    tokio::sync::mpsc::unbounded_channel();
                let replacement_registration = server.brain_runners().register_bounded(
                    "shared",
                    replacement_lease_id,
                    replacement_tx,
                );
                let replacement_server = std::sync::Arc::clone(&server);
                let replacement_bridge = tokio::task::spawn_local(async move {
                    while let Some(request) = replacement_rx.recv().await {
                        super::forward_runner_request(
                            replacement_runner.clone(),
                            std::sync::Arc::clone(&replacement_server),
                            request,
                            replacement_lease_id,
                            None,
                            Some(replacement_registration),
                        )
                        .await;
                    }
                });
                let replacement_lifecycle = lifecycle.clone();
                let (replacement_attempted_tx, replacement_attempted_rx) =
                    tokio::sync::oneshot::channel();
                let replacement_submission = tokio::task::spawn_local(async move {
                    let _ = replacement_attempted_tx.send(());
                    replacement_lifecycle
                        .submit(
                            "shared",
                            attachment_id,
                            connection_id,
                            crate::brain::store::BrainEventKind::Program {
                                language: crate::brain::store::ProgramLanguage::Lisp,
                                source: "replacement callback".into(),
                            },
                        )
                        .await
                });

                replacement_attempted_rx
                    .await
                    .expect("replacement submission task did not reach admission");
                assert!(
                    !old_submission.is_finished(),
                    "expired run lane returned before its physical IPC callback settled"
                );
                assert!(
                    !replacement_submission.is_finished()
                        && replacement_started_rx.try_recv().is_err(),
                    "replacement callback overlapped the nonquiescent expired generation"
                );

                let replacement_run_id = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    &mut replacement_started_rx,
                )
                .await
                .expect("replacement callback did not start after bounded cleanup")
                .unwrap();
                assert!(
                    old_submission.is_finished(),
                    "expired run lane did not terminalize after bounded callback cleanup"
                );

                release_old.notify_one();
                assert!(
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        late_effect_rx,
                    )
                    .await
                    .expect("stale callback did not reach its late effect boundary")
                    .is_err(),
                    "expired callback survived physical cancellation and attempted a late effect"
                );
                let old_outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    old_submission,
                )
                .await
                .expect("expired callback did not release its durable lane")
                .unwrap()
                .unwrap();
                assert_eq!(old_outcome.run.unwrap().run_id, old_run_id);
                let replacement_outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    replacement_submission,
                )
                .await
                .expect("replacement callback did not finish")
                .unwrap()
                .unwrap();
                assert_eq!(
                    replacement_outcome.run.unwrap().run_id,
                    replacement_run_id
                );

                let snapshot = store.snapshot("shared").unwrap();
                let old_run = snapshot
                    .runs
                    .iter()
                    .find(|run| run.run_id == old_run_id)
                    .unwrap();
                let replacement_run = snapshot
                    .runs
                    .iter()
                    .find(|run| run.run_id == replacement_run_id)
                    .unwrap();
                assert_eq!(old_run.status, crate::brain::store::BrainRunStatus::Failed);
                assert_eq!(
                    replacement_run.status,
                    crate::brain::store::BrainRunStatus::Completed
                );
                assert_eq!(
                    snapshot
                        .events
                        .iter()
                        .filter(|event| matches!(
                            event.kind,
                            crate::brain::store::BrainEventKind::RunStatusChanged {
                                run_id,
                                status,
                                ..
                            } if run_id == old_run_id && status.is_terminal()
                        ))
                        .count(),
                    1
                );
                assert!(snapshot.effect_audits.is_empty());
                assert!(!snapshot.events.iter().any(|event| matches!(
                    &event.kind,
                    crate::brain::store::BrainEventKind::Result { output, error, .. }
                        if output.contains("late stale generation")
                            || error.as_deref().is_some_and(|error| error.contains("late stale generation"))
                )));

                let reopened = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let reopened_snapshot = reopened.snapshot("shared").unwrap();
                for run_id in [old_run_id, replacement_run_id] {
                    assert_eq!(
                        reopened_snapshot
                            .events
                            .iter()
                            .filter(|event| {
                                event.run_id == Some(run_id)
                                    && matches!(
                                        event.kind,
                                        crate::brain::store::BrainEventKind::Result { .. }
                                    )
                            })
                            .count(),
                        1,
                        "reopen must preserve exactly one canonical result per terminal run"
                    );
                }
                assert!(!reopened_snapshot.events.iter().any(|event| matches!(
                    &event.kind,
                    crate::brain::store::BrainEventKind::Result { output, error, .. }
                        if output.contains("late stale generation")
                            || error.as_deref().is_some_and(|error| error.contains("late stale generation"))
                )));

                server
                    .brain_runners()
                    .invalidate_lease("shared", replacement_lease_id);
                old_bridge.await.unwrap();
                replacement_bridge.await.unwrap();
            })
            .await;
    }

    async fn raw_effect_eof_states(
        begin: bool,
        count: usize,
        mature_history: bool,
    ) -> Vec<crate::runtime::effect_log::EffectAuditState> {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "effect eof".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        if mature_history {
            store
                .seed_mature_effect_audit_history_for_test("shared", 1_024, 3)
                .unwrap();
        }
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store.clone(),
                crate::brain::credential::BrainCredentialAuthority::ephemeral([45; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let request = crate::server::RunnerProgramRequest {
            brain: "shared".into(),
            run_id: run.run_id,
            request_seq: prompt.seq,
            language: crate::brain::store::ProgramLanguage::Forth,
            source: "noop".into(),
            interaction: crate::server::RunnerProgramInteraction::Interactive,
            grant_ceiling: None,
            control_tx: None,
            effect_audit: None,
            response_tx,
        };
        let runner: super::finch_ipc_capnp::brain_runner::Client =
            capnp_rpc::new_client(EffectEofRunner { begin, count });
        super::forward_test_runner_request(
            runner,
            std::sync::Arc::clone(&server),
            crate::server::RunnerRequest::Program(request),
        )
        .await;
        assert!(response_rx.await.is_err());
        store
            .reconcile_effect_audits_for_disconnected_leases("shared", &[lease.lease_id])
            .unwrap();
        store
            .snapshot("shared")
            .unwrap()
            .effect_audits
            .into_iter()
            .map(|entry| entry.state)
            .collect()
    }

    async fn partial_frame_connection_teardown_fixture(
        fail_audit_batch: bool,
    ) -> (
        tempfile::TempDir,
        crate::brain::store::BrainStore,
        std::sync::Arc<crate::server::AgentServer>,
        uuid::Uuid,
        crate::brain::store::RunnerLeaseId,
        anyhow::Result<()>,
    ) {
        use tokio::io::AsyncWriteExt;

        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "partial frame".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let connection_id = uuid::Uuid::new_v4();
        let grant = store
            .issue_effect_audit_authority(
                "shared",
                run.run_id,
                lease.lease_id,
                Some(crate::brain::store::ConnectionId(connection_id)),
            )
            .unwrap();
        store
            .reserve_effect_audit(
                &grant,
                uuid::Uuid::new_v4(),
                crate::vm::VmSideEffect {
                    protocol_version: 1,
                    sequence: 0,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    output: Vec::new(),
                    event: crate::vm::HostSideEffect::Emit {
                        text: "unbegun".into(),
                    },
                    origin: crate::vm::SourceOrigin::generated("partial-frame-unbegun"),
                },
            )
            .unwrap();
        let begun = store
            .reserve_effect_audit(
                &grant,
                uuid::Uuid::new_v4(),
                crate::vm::VmSideEffect {
                    protocol_version: 1,
                    sequence: 1,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    output: Vec::new(),
                    event: crate::vm::HostSideEffect::Emit {
                        text: "begun".into(),
                    },
                    origin: crate::vm::SourceOrigin::generated("partial-frame-begun"),
                },
            )
            .unwrap();
        let _permit = store.begin_effect_audit(&grant, begun).unwrap();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store.clone(),
                crate::brain::credential::BrainCredentialAuthority::ephemeral([49; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let runners = server.brain_runners();
        runners
            .claim_connection_identity(connection_id, "runner@box.local/partial")
            .unwrap();
        runners
            .claim_connection_lease(connection_id, "shared", lease.lease_id)
            .unwrap();
        let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
        runners
            .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
            .unwrap();
        if fail_audit_batch {
            store
                .fail_next_effect_audit_batch_for_test("shared")
                .unwrap();
        }

        let (server_stream, mut peer_stream) = tokio::net::UnixStream::pair().unwrap();
        let handler_server = std::sync::Arc::clone(&server);
        let handler = tokio::task::spawn_local(async move {
            super::handle_connection_with_id(server_stream, handler_server, connection_id).await
        });
        tokio::task::yield_now().await;
        // Declare one eight-byte segment, write only half its payload, then
        // close. This is a real Cap'n Proto partial frame rather than a remote
        // application exception choosing an error kind. RpcSystem is allowed
        // to normalize peer EOF to Ok; bounded lifecycle teardown, not the
        // method-level error value, owns reconciliation.
        peer_stream
            .write_all(&[0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0])
            .await
            .unwrap();
        peer_stream.shutdown().await.unwrap();
        drop(peer_stream);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), handler)
            .await
            .expect("partial-frame connection teardown exceeded two seconds")
            .unwrap();
        (temp, store, server, connection_id, lease.lease_id, result)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_partial_frame_connection_teardown_reconciles_before_and_after_begin() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_temp, store, server, _connection_id, lease_id, _result) =
                    partial_frame_connection_teardown_fixture(false).await;
                assert!(!server.brain_runners().has_registration("shared", lease_id));
                let states = store
                    .snapshot("shared")
                    .unwrap()
                    .effect_audits
                    .into_iter()
                    .map(|entry| entry.state)
                    .collect::<Vec<_>>();
                assert!(states.iter().any(|state| matches!(
                    state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome:
                            crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
                    }
                )));
                assert!(states.iter().any(|state| matches!(
                    state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome:
                            crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss
                    }
                )));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_teardown_transaction_failure_keeps_authority_fenced_until_retry() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (_temp, store, server, connection_id, lease_id, result) =
                    partial_frame_connection_teardown_fixture(true).await;
                result.expect("transient audit failure must be retried by the teardown owner");
                let replacement = uuid::Uuid::new_v4();
                server
                    .brain_runners()
                    .claim_connection_identity(replacement, "runner@box.local/partial")
                    .unwrap();
                server
                    .brain_runners()
                    .claim_connection_lease(replacement, "shared", lease_id)
                    .unwrap();
                assert_eq!(
                    store
                        .reconcile_effect_audits_for_disconnected_leases("shared", &[lease_id])
                        .unwrap(),
                    0
                );
                server
                    .brain_runners()
                    .begin_connection_teardown(connection_id)
                    .finish()
                    .unwrap();
                server
                    .brain_runners()
                    .claim_connection_identity(replacement, "runner@box.local/partial")
                    .unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_connection_teardown_closes_admission_and_drains_pre_snapshot_dispatch() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let temp = tempfile::tempdir().unwrap();
                let store = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let attachment = store
                    .attach(
                        "shared",
                        "alice",
                        crate::brain::store::AttachmentRole::Driver,
                        None,
                    )
                    .unwrap();
                let prompt = store
                    .push(
                        "shared",
                        "alice",
                        crate::brain::store::BrainEventKind::Prompt {
                            text: "queued teardown race".into(),
                        },
                    )
                    .unwrap();
                let run = store
                    .start_run(
                        "shared",
                        "alice",
                        crate::brain::store::BrainRunKind::Interactive,
                        prompt.seq,
                        attachment.attachment_id,
                        crate::brain::store::BrainRunStatus::Running,
                    )
                    .unwrap();
                let lease = store
                    .acquire_runner_lease("shared", "runner", 1, None, 300_000)
                    .unwrap();
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test(
                        store.clone(),
                        crate::brain::credential::BrainCredentialAuthority::ephemeral([50; 32]),
                        "test-password".into(),
                        temp.path(),
                    )
                    .unwrap(),
                );
                let connection_id = uuid::Uuid::new_v4();
                let runners = server.brain_runners();
                runners
                    .claim_connection_identity(connection_id, "runner@box.local/queued")
                    .unwrap();
                runners
                    .claim_connection_lease(connection_id, "shared", lease.lease_id)
                    .unwrap();
                let (callback_tx, _callback_rx) = tokio::sync::mpsc::unbounded_channel();
                runners
                    .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
                    .unwrap();
                let admission = runners
                    .connection_dispatch_admission(connection_id)
                    .unwrap();
                let queued_dispatch = admission
                    .try_enter()
                    .expect("live connection admits queued callback dispatch");
                let run_id = run.run_id;
                let lease_id = lease.lease_id;
                let release = std::sync::Arc::new(tokio::sync::Notify::new());
                let release_task = std::sync::Arc::clone(&release);
                let queued_store = store.clone();
                let queued = tokio::task::spawn_local(async move {
                    let _queued_dispatch = queued_dispatch;
                    release_task.notified().await;
                    let grant = queued_store
                        .issue_effect_audit_authority(
                            "shared",
                            run_id,
                            lease_id,
                            Some(crate::brain::store::ConnectionId(connection_id)),
                        )
                        .unwrap();
                    queued_store
                        .reserve_effect_audit(
                            &grant,
                            uuid::Uuid::new_v4(),
                            crate::vm::VmSideEffect {
                                protocol_version: 1,
                                sequence: 0,
                                requirement: crate::vm::CapabilityRequirement {
                                    capability: crate::vm::CapabilityKind::SessionEmit,
                                    selector: crate::vm::ResourceSelector::None,
                                },
                                output: Vec::new(),
                                event: crate::vm::HostSideEffect::Emit {
                                    text: "queued".into(),
                                },
                                origin: crate::vm::SourceOrigin::generated(
                                    "queued-before-teardown",
                                ),
                            },
                        )
                        .unwrap()
                });

                let teardown = runners.begin_connection_teardown(connection_id);
                assert!(
                    admission.try_enter().is_none(),
                    "teardown must reject new callback work before the audit snapshot"
                );
                release.notify_one();
                teardown.wait_quiesced().await;
                let identity = queued.await.unwrap();
                assert_eq!(
                    store
                        .reconcile_effect_audits_for_disconnected_leases(
                            "shared",
                            &[lease_id],
                        )
                        .unwrap(),
                    1
                );
                teardown.finish().unwrap();
                let snapshot = store.snapshot("shared").unwrap();
                assert!(snapshot.effect_audits.iter().any(|entry|
                    entry.intent.identity == identity
                        && matches!(entry.state,
                            crate::runtime::effect_log::EffectAuditState::Terminal {
                                outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
                            })));
                assert!(snapshot.effect_audits.iter().all(|entry| entry.state.is_terminal()));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_raw_frontend_eof_reconciles_before_and_after_application_boundary() {
        assert!(matches!(
            raw_effect_eof_states(false, 1, false).await.remove(0),
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome:
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
            }
        ));
        assert!(matches!(
            raw_effect_eof_states(true, 1, false).await.remove(0),
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome:
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::UncertainProcessLoss
            }
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_max_quota_raw_eof_terminalizes_once_within_teardown_bound() {
        let states = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            raw_effect_eof_states(
                false,
                crate::runtime::effect_log::MAX_ACTIVE_EFFECT_AUDITS_PER_RUN,
                true,
            ),
        )
        .await
        .expect("max-quota runner EOF exceeded the two-second teardown bound");
        assert_eq!(
            states.len(),
            crate::runtime::effect_log::MAX_ACTIVE_EFFECT_AUDITS_PER_RUN
        );
        assert!(states.into_iter().all(|state| matches!(
            state,
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome:
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
            }
        )));
    }

    async fn raw_normal_effect_state(
        begin: bool,
        remote_disconnect_error: bool,
    ) -> (
        tempfile::TempDir,
        crate::brain::store::BrainStore,
        std::sync::Arc<crate::server::AgentServer>,
        crate::brain::store::RunnerLeaseId,
        tokio::sync::mpsc::UnboundedReceiver<crate::server::RunnerRequest>,
        crate::runtime::effect_log::EffectAuditState,
        Option<super::finch_ipc_capnp::brain_host_effect_permit::Client>,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let store = crate::brain::store::BrainStore::with_root(
            "box.local",
            Some(temp.path().join("brains")),
        );
        let attachment = store
            .attach(
                "shared",
                "alice",
                crate::brain::store::AttachmentRole::Driver,
                None,
            )
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                crate::brain::store::BrainEventKind::Prompt {
                    text: "normal effect".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                crate::brain::store::BrainRunStatus::Running,
            )
            .unwrap();
        let lease = store
            .acquire_runner_lease("shared", "runner", 1, None, 300_000)
            .unwrap();
        let server = std::sync::Arc::new(
            crate::server::AgentServer::for_brain_protocol_test(
                store.clone(),
                crate::brain::credential::BrainCredentialAuthority::ephemeral([46; 32]),
                "test-password".into(),
                temp.path(),
            )
            .unwrap(),
        );
        let connection_id = uuid::Uuid::new_v4();
        server
            .brain_runners()
            .claim_connection_identity(connection_id, "runner@box.local/application-error")
            .unwrap();
        server
            .brain_runners()
            .claim_connection_lease(connection_id, "shared", lease.lease_id)
            .unwrap();
        let (callback_tx, callback_rx) = tokio::sync::mpsc::unbounded_channel();
        server
            .brain_runners()
            .register_for_connection(connection_id, "shared", lease.lease_id, callback_tx)
            .unwrap();
        let (response_tx, response_rx) = tokio::sync::oneshot::channel();
        let request = crate::server::RunnerProgramRequest {
            brain: "shared".into(),
            run_id: run.run_id,
            request_seq: prompt.seq,
            language: crate::brain::store::ProgramLanguage::Forth,
            source: "noop".into(),
            interaction: crate::server::RunnerProgramInteraction::Interactive,
            grant_ceiling: None,
            control_tx: None,
            effect_audit: None,
            response_tx,
        };
        let (permit_tx, permit_rx) = tokio::sync::oneshot::channel();
        let runner: super::finch_ipc_capnp::brain_runner::Client =
            capnp_rpc::new_client(EffectNormalRunner {
                begin,
                remote_disconnect_error,
                permit_tx: Some(permit_tx),
            });
        super::forward_test_runner_request(
            runner,
            std::sync::Arc::clone(&server),
            crate::server::RunnerRequest::Program(request),
        )
        .await;
        let response = response_rx.await;
        if remote_disconnect_error {
            assert!(response.is_err());
        } else {
            assert!(response.unwrap().is_err());
        }
        let permit = permit_rx.await.unwrap();
        let state = store.snapshot("shared").unwrap().effect_audits[0]
            .state
            .clone();
        (
            temp,
            store,
            server,
            lease.lease_id,
            callback_rx,
            state,
            permit,
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_normal_return_abandons_only_unbegun_and_allows_late_finish() {
        let (_temp, _store, _server, _lease_id, _callback_rx, state, permit) =
            raw_normal_effect_state(false, false).await;
        assert!(permit.is_none());
        assert!(matches!(
            state,
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome:
                    crate::runtime::effect_log::EffectAuditTerminalOutcome::AbandonedNotApplied
            }
        ));

        let (_temp, store, _server, _lease_id, _callback_rx, state, permit) =
            raw_normal_effect_state(true, false).await;
        assert!(matches!(
            state,
            crate::runtime::effect_log::EffectAuditState::AwaitingHostResult
        ));
        let permit = permit.expect("begun normal-return effect retained its detached permit");
        let mut finish = permit.finish_request();
        finish.get().init_outcome().init_acknowledged(0);
        finish.send().promise.await.unwrap();
        assert!(
            matches!(store.snapshot("shared").unwrap().effect_audits[0].state,
            crate::runtime::effect_log::EffectAuditState::Terminal {
                outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                    ref outcome_kind
                }
            } if outcome_kind == "acknowledged")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_remote_disconnected_exception_does_not_claim_transport_teardown() {
        let (_temp, store, server, lease_id, _callback_rx, state, permit) =
            raw_normal_effect_state(true, true).await;
        assert!(
            server.brain_runners().has_registration("shared", lease_id),
            "a remote method exception must not revoke the live callback registration"
        );
        assert!(matches!(
            state,
            crate::runtime::effect_log::EffectAuditState::AwaitingHostResult
        ));
        let permit = permit.expect("begun effect retains its detached completion authority");
        let mut finish = permit.finish_request();
        finish.get().init_outcome().init_acknowledged(0);
        finish.send().promise.await.unwrap();
        assert!(
            matches!(store.snapshot("shared").unwrap().effect_audits[0].state,
                crate::runtime::effect_log::EffectAuditState::Terminal {
                    outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                        ref outcome_kind
                    }
                } if outcome_kind == "acknowledged")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn effect_audit_provider_turn_cancel_disconnect_late_finish_has_no_publication() {
        tokio::task::LocalSet::new()
            .run_until(async {
                use crate::tools::registry::Tool;

                let temp = tempfile::tempdir().unwrap();
                let task_output = temp.path().join("task-output");
                std::fs::create_dir_all(&task_output).unwrap();
                let store = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let server = std::sync::Arc::new(
                    crate::server::AgentServer::for_brain_protocol_test(
                        store.clone(),
                        crate::brain::credential::BrainCredentialAuthority::ephemeral([48; 32]),
                        "test-password".into(),
                        temp.path(),
                    )
                    .unwrap(),
                );
                let daemon: super::finch_ipc_capnp::finch_daemon::Client =
                    capnp_rpc::new_client(FinchDaemonImpl::new(
                        std::sync::Arc::clone(&server),
                        uuid::Uuid::new_v4(),
                    ));
                let ipc = crate::ipc::IpcClient::from_test_client(daemon);
                let initial = ipc.brain_snapshot("shared").await.unwrap();
                let runner_subject = "runner@box.local/frontend-audit";
                ipc.brain_claim_runner_identity(runner_subject).await.unwrap();
                let lease = ipc
                    .brain_acquire_runner(
                        "shared",
                        runner_subject,
                        &initial.environment,
                        None,
                        300_000,
                    )
                    .await
                    .unwrap();

                let runtime = std::sync::Arc::new(crate::runtime::ProgramRuntime::new());
                runtime.bind_task_output_root(&task_output).unwrap();
                let requirement = crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Write,
                    crate::vm::FileSelector::parse("${task.output}/**").unwrap(),
                );
                runtime
                    .grant_typed_capability(requirement.clone())
                    .unwrap();
                let input = serde_json::json!({
                    "language": "forth",
                    "source": "s\" late.txt\" task-output-path s\" durable\" bytes task-output-file-write",
                    "intent": "production IPC provider effect audit",
                    "declared_capabilities": [requirement],
                    "manifest_generation": runtime.manifest_generation(),
                });
                let provider_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let generator: std::sync::Arc<dyn crate::generators::Generator> =
                    std::sync::Arc::new(ProviderSubmitProgramGenerator {
                        input,
                        calls: std::sync::Arc::clone(&provider_calls),
                    });
                let submit_tool =
                    crate::tools::implementations::program::SubmitProgramTool::new(
                        std::sync::Arc::clone(&runtime),
                    );
                let definitions = vec![submit_tool.definition()];
                let mut registry = crate::tools::registry::ToolRegistry::new();
                registry.register(Box::new(submit_tool));
                let permissions = crate::tools::permissions::PermissionManager::new()
                    .with_default_rule(crate::tools::permissions::PermissionRule::Allow);
                let executor = std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::tools::executor::ToolExecutor::new(
                        registry,
                        permissions,
                        temp.path().join("tool-patterns.json"),
                    )
                    .unwrap(),
                ));

                let (finish_started_tx, finish_started_rx) = tokio::sync::oneshot::channel();
                let finish_release = std::sync::Arc::new(tokio::sync::Notify::new());
                let finish_sender = std::sync::Arc::new(std::sync::Mutex::new(Some(
                    finish_started_tx,
                )));
                let physical_effects =
                    std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let mut event_loop =
                    crate::cli::repl_event::EventLoop::new_named_brain_test_runner(
                        generator,
                        definitions,
                        executor,
                        runtime,
                    );
                let conversation = event_loop.conversation_for_test();
                let wrapper_release = std::sync::Arc::clone(&finish_release);
                let wrapper_effects = std::sync::Arc::clone(&physical_effects);
                event_loop.set_effect_audit_test_wrapper(std::sync::Arc::new(move |control| {
                    let finish_started = finish_sender
                        .lock()
                        .expect("finish sender lock poisoned")
                        .take()
                        .expect("provider turn reserved more than one host effect");
                    delayed_finish_effect_audit(
                        control,
                        finish_started,
                        std::sync::Arc::clone(&wrapper_release),
                        std::sync::Arc::clone(&wrapper_effects),
                    )
                }));
                let (event_tx, event_driver) =
                    event_loop.start_named_brain_test_runner("shared".into());
                let _bootstrap = ipc
                    .register_brain_runner("shared", lease.lease_id, event_tx.clone())
                    .await
                    .unwrap();
                let attachment = ipc
                    .brain_attach(
                        "shared",
                        "alice",
                        crate::brain::store::AttachmentRole::Driver,
                        None,
                    )
                    .await
                    .unwrap();
                let mut watch = ipc.brain_watch("shared", &attachment).await.unwrap();
                let _initial_watch = watch.recv().await.unwrap().unwrap();
                let submit_ipc = ipc.clone();
                let submit_attachment = attachment.clone();
                let submission = tokio::task::spawn_local(async move {
                    submit_ipc
                        .brain_submit(
                            "shared",
                            &submit_attachment,
                            crate::brain::store::BrainEventKind::Prompt {
                                text: "apply one provider effect".into(),
                            },
                        )
                        .await
                });

                tokio::time::timeout(std::time::Duration::from_secs(2), finish_started_rx)
                    .await
                    .expect("physical effect did not reach its late finish boundary")
                    .unwrap();
                assert_eq!(
                    std::fs::read(task_output.join("late.txt")).unwrap(),
                    b"durable"
                );
                assert_eq!(physical_effects.load(std::sync::atomic::Ordering::SeqCst), 1);
                let run = store
                    .snapshot("shared")
                    .unwrap()
                    .runs
                    .into_iter()
                    .find(|run| run.status == crate::brain::store::BrainRunStatus::Running)
                    .expect("daemon did not create the active provider run");
                let cancelled = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    ipc.brain_cancel_run("shared", &attachment, run.run_id),
                )
                .await
                .expect("real daemon/IPC cancellation exceeded the teardown bound")
                .unwrap();
                assert_eq!(
                    cancelled.status,
                    crate::brain::store::BrainRunStatus::Cancelled
                );
                let submission = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    submission,
                )
                .await
                .expect("real runner cancellation did not quiesce the provider turn")
                .unwrap()
                .unwrap();
                assert_eq!(submission.run.unwrap().run_id, run.run_id);
                let conversation_before_late_finish = serde_json::to_value(
                    conversation.read().await.get_messages(),
                )
                .unwrap();
                event_tx.send(crate::cli::repl_event::ReplEvent::Shutdown).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(2), event_driver)
                    .await
                    .expect("runner EventLoop disconnect exceeded the teardown bound")
                    .unwrap()
                    .unwrap();
                finish_release.notify_one();
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
                let snapshot = loop {
                    let snapshot = store.snapshot("shared").unwrap();
                    if snapshot.effect_audits.first().is_some_and(|audit| {
                        matches!(
                            audit.state,
                            crate::runtime::effect_log::EffectAuditState::Terminal {
                                outcome:
                                    crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                                        ref outcome_kind
                                    }
                            } if outcome_kind == "acknowledged"
                        )
                    }) {
                        break snapshot;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "late authoritative effect finish was not durably acknowledged; state={:?}",
                        snapshot.effect_audits.first().map(|audit| &audit.state)
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                };
                assert_eq!(snapshot.effect_audits.len(), 1);
                assert_eq!(physical_effects.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert_eq!(provider_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert!(matches!(snapshot.effect_audits[0].state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Redacted {
                            ref outcome_kind
                        }
                    } if outcome_kind == "acknowledged"));
                let restarted = crate::brain::store::BrainStore::with_root(
                    "box.local",
                    Some(temp.path().join("brains")),
                );
                let replayed = restarted.snapshot("shared").unwrap();
                assert_eq!(
                    replayed.effect_audits.len(),
                    1,
                    "restart/replay must reconstruct exactly one terminal audit identity"
                );
                assert_eq!(
                    replayed.effect_audits[0].intent.identity,
                    snapshot.effect_audits[0].intent.identity
                );
                assert!(matches!(replayed.effect_audits[0].state,
                    crate::runtime::effect_log::EffectAuditState::Terminal {
                        outcome: crate::runtime::effect_log::EffectAuditTerminalOutcome::Compacted {
                            ref outcome_kind, ..
                        }
                    } if outcome_kind == "acknowledged"));
                assert!(!snapshot.events.iter().any(|event| {
                    matches!(
                        event.kind,
                        crate::brain::store::BrainEventKind::ToolResult { .. }
                            | crate::brain::store::BrainEventKind::Program { .. }
                            | crate::brain::store::BrainEventKind::RuntimeCommitted { .. }
                            | crate::brain::store::BrainEventKind::EffectRecorded { .. }
                    ) || matches!(
                        &event.kind,
                        crate::brain::store::BrainEventKind::Result {
                            error: None,
                            ..
                        }
                    )
                }));
                assert_eq!(
                    serde_json::to_value(conversation.read().await.get_messages()).unwrap(),
                    conversation_before_late_finish,
                    "late effect completion must not append provider or ToolResult history"
                );
            })
            .await;
    }

    struct SocketApprovalRunner {
        failed_tx: Option<tokio::sync::oneshot::Sender<String>>,
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
            let audience = match request
                .get_approval_audience()
                .map_err(anyhow::Error::new)
                .and_then(super::decode_approval_audience)
            {
                Ok(audience) => audience,
                Err(error) => {
                    return capnp::capability::Promise::err(capnp::Error::failed(error.to_string()))
                }
            };
            let control = match request.get_control() {
                Ok(control) => control,
                Err(error) => return capnp::capability::Promise::err(error),
            };
            let failed_tx = self
                .failed_tx
                .take()
                .expect("runner received more than one turn");
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
            let (socket_path, _socket_path_override) =
                match std::env::var_os("FINCH_TEST_IPC_SOCKET") {
                    Some(path) => (std::path::PathBuf::from(path), None),
                    None => {
                        let path = temp.path().join("finch.sock");
                        let guard = crate::ipc::transport::set_test_sock_path(path.clone());
                        (path, Some(guard))
                    }
                };
            let store = crate::brain::store::BrainStore::with_root(
                "box.local",
                Some(temp.path().join("brains")),
            );
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store.clone(),
                    crate::brain::credential::BrainCredentialAuthority::ephemeral([91; 32]),
                    "test-password".into(),
                    temp.path(),
                )
                .unwrap(),
            );
            let shutdown = tokio_util::sync::CancellationToken::new();
            let server_task =
                tokio::task::spawn_local(super::start_ipc_server(server.clone(), shutdown.clone()));
            tokio::task::yield_now().await;
            assert!(
                !server_task.is_finished(),
                "supervised IPC server exited before accepting connections"
            );
            let participant = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                let participant = crate::ipc::IpcClient::connect_path(socket_path.clone()).await?;
                participant.brain_snapshot("shared").await?;
                Ok::<_, anyhow::Error>(participant)
            })
            .await
            .expect("supervised IPC server did not answer a readiness RPC within two seconds")
            .unwrap();
            let attachment = participant
                .brain_attach(
                    "shared",
                    "alice",
                    crate::brain::store::AttachmentRole::Driver,
                    None,
                )
                .await
                .unwrap();
            let mut participant_events = participant
                .brain_watch("shared", &attachment)
                .await
                .unwrap();
            participant_events.recv().await.unwrap().unwrap();

            let runner = crate::ipc::IpcClient::connect_path(socket_path.clone())
                .await
                .unwrap();
            let snapshot = runner.brain_snapshot("shared").await.unwrap();
            runner
                .brain_claim_runner_identity("runner@box.local/socket")
                .await
                .unwrap();
            let lease = runner
                .brain_acquire_runner(
                    "shared",
                    "runner@box.local/socket",
                    &snapshot.environment,
                    None,
                    60_000,
                )
                .await
                .unwrap();
            let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
            let callback: super::finch_ipc_capnp::brain_runner::Client =
                capnp_rpc::new_client(SocketApprovalRunner {
                    failed_tx: Some(failed_tx),
                });
            runner
                .register_test_brain_runner_client("shared", lease.lease_id, callback)
                .await
                .unwrap();

            let run = participant
                .brain_start_speculative("shared", &attachment, "request approval".into())
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                loop {
                    let current = store.snapshot("shared").unwrap();
                    if current.events.iter().any(|event| {
                        matches!(
                            &event.kind,
                            crate::brain::store::BrainEventKind::ApprovalRequested {
                                approval_id, ..
                            } if approval_id == "socket-approval"
                        )
                    }) {
                        assert_eq!(
                            store.inspect_run("shared", run.run_id).unwrap().status,
                            crate::brain::store::BrainRunStatus::AwaitingApproval
                        );
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("reverse approval did not become durable");

            let old_connection = attachment.connection_id.unwrap();
            drop(participant_events);
            drop(participant);
            let error = tokio::time::timeout(std::time::Duration::from_millis(250), failed_rx)
                .await
                .expect("physical IPC loss did not fail approval promptly")
                .unwrap();
            assert!(error.contains("approval audience disconnected"), "{error}");
            tokio::time::timeout(std::time::Duration::from_millis(250), async {
                while store
                    .require_connection("shared", attachment.attachment_id, old_connection)
                    .is_ok()
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("physical IPC loss did not detach exact generation");

            let replacement = crate::ipc::IpcClient::connect_path(socket_path)
                .await
                .unwrap();
            let replacement_attachment = replacement
                .brain_attach(
                    "shared",
                    "alice",
                    crate::brain::store::AttachmentRole::Driver,
                    Some(attachment.attachment_id),
                )
                .await
                .unwrap();
            let replacement_connection = replacement_attachment.connection_id.unwrap();
            let mut replacement_events = replacement
                .brain_watch("shared", &replacement_attachment)
                .await
                .unwrap();
            replacement_events.recv().await.unwrap().unwrap();
            assert!(store
                .require_connection(
                    "shared",
                    replacement_attachment.attachment_id,
                    replacement_connection,
                )
                .is_ok());
            assert_eq!(store.snapshot("shared").unwrap().runner_lease, Some(lease));
            let terminal = store.snapshot("shared").unwrap();
            assert_eq!(
                terminal
                    .events
                    .iter()
                    .filter(|event| matches!(
                        event.kind,
                        crate::brain::store::BrainEventKind::RunStatusChanged {
                            run_id, status, ..
                        } if run_id == run.run_id && status.is_terminal()
                    ))
                    .count(),
                1
            );

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
        assert!(error
            .to_string()
            .contains("approval audience has no live connection generation"));
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
        runners
            .begin_connection_teardown(first_connection)
            .finish()
            .unwrap();

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
            let target =
                crate::brain::remote::RemoteBrainTarget::local("shared", "127.0.0.1:1").unwrap();
            let mut driver =
                crate::brain::remote::AttachedBrainClient::local(target.clone(), ipc.clone());
            driver
                .attach("alice", crate::brain::store::AttachmentRole::Driver, None)
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
                .attach("bob", crate::brain::store::AttachmentRole::Consultant, None)
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
            super::super::brain_codec::encode_continuation_messages(
                result.reborrow().init_continuation_messages(3),
                &[
                    crate::claude::Message::with_content(
                        "assistant",
                        vec![
                            crate::claude::ContentBlock::opaque_reasoning("opaque-tool-token"),
                            crate::claude::ContentBlock::ToolUse {
                                id: "tool-1".into(),
                                name: "search_word".into(),
                                input: serde_json::json!({"query":"fib"}),
                            },
                        ],
                    ),
                    crate::claude::Message::with_content(
                        "user",
                        vec![crate::claude::ContentBlock::tool_result(
                            "tool-1".into(),
                            "found".into(),
                            None,
                        )],
                    ),
                    crate::claude::Message::with_content(
                        "assistant",
                        vec![
                            crate::claude::ContentBlock::opaque_reasoning("opaque-runner-token"),
                            crate::claude::ContentBlock::text("(say \"done\")"),
                        ],
                    ),
                ],
            )
            .unwrap();
            result.set_has_invocation_metadata(true);
            super::super::brain_codec::encode_invocation_metadata(
                result.reborrow().init_invocation_metadata(),
                &crate::providers::types::InvocationMetadata {
                    requested_model: "gpt-5.6".into(),
                    resolved_model: "gpt-5.6".into(),
                    actual_model: "gpt-5.6-sol".into(),
                    input_tokens: Some(5),
                    output_tokens: Some(3),
                    primary_allowance_used_percent: Some(40.0),
                    secondary_allowance_used_percent: None,
                },
            );
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
        assert!(matches!(
            decoded.continuation_messages.last().unwrap().content.as_slice(),
            [
                crate::claude::ContentBlock::OpaqueReasoning { encrypted_content },
                crate::claude::ContentBlock::Text { text },
            ] if encrypted_content == "opaque-runner-token" && text == "(say \"done\")"
        ));
        assert_eq!(
            decoded.invocation_metadata.unwrap().actual_model,
            "gpt-5.6-sol"
        );
    }

    #[test]
    fn runner_turn_error_keeps_partial_lifecycle() {
        let expected_effect = effect_record();
        let mut message = capnp::message::Builder::new_default();
        {
            let mut result =
                message.init_root::<super::finch_ipc_capnp::brain_turn_result::Builder>();
            result.set_error("provider failed after approval");
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

    #[test]
    fn supervised_ipc_listener_ancestor_swap_never_mutates_replacement_path() {
        if std::env::var("FINCH_BRAIN_TEST_ISOLATED").as_deref() != Ok("1") {
            return;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let proof = crate::brain::isolated_test_proof().unwrap();
            let prepared = super::prepare_ipc_listener().await.unwrap();
            assert!(!prepared.remove_on_shutdown);

            let store =
                crate::brain::store::BrainStore::with_root("box.local", Some(proof.root.clone()));
            let server = std::sync::Arc::new(
                crate::server::AgentServer::for_brain_protocol_test(
                    store,
                    crate::brain::credential::BrainCredentialAuthority::ephemeral([92; 32]),
                    "test-password".into(),
                    &proof.home,
                )
                .unwrap(),
            );

            let moved = proof.socket_root.with_file_name(format!(
                "{}.moved-{}",
                proof.socket_root.file_name().unwrap().to_string_lossy(),
                uuid::Uuid::new_v4().simple()
            ));
            let replacement = proof.home.join(".finch");
            let sentinel = replacement.join("ipc-swap-sentinel");
            let attacker_socket = replacement.join("daemon.sock");
            std::fs::write(&sentinel, b"outside-must-not-change").unwrap();
            std::fs::write(&attacker_socket, b"not-a-socket").unwrap();
            std::fs::rename(&proof.socket_root, &moved).unwrap();
            std::os::unix::fs::symlink(&replacement, &proof.socket_root).unwrap();

            let shutdown = tokio_util::sync::CancellationToken::new();
            shutdown.cancel();
            super::serve_ipc_listener(server, shutdown, prepared)
                .await
                .unwrap();

            assert_eq!(
                std::fs::read(&sentinel).unwrap(),
                b"outside-must-not-change"
            );
            assert_eq!(std::fs::read(&attacker_socket).unwrap(), b"not-a-socket");
            assert!(std::fs::symlink_metadata(&attacker_socket)
                .unwrap()
                .file_type()
                .is_file());

            std::fs::remove_file(&proof.socket_root).unwrap();
            std::fs::rename(&moved, &proof.socket_root).unwrap();
            std::fs::remove_file(sentinel).unwrap();
            std::fs::remove_file(attacker_socket).unwrap();
        }));
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
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = shutdown.cancelled() => break,
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _addr)) => {
                            let server = Arc::clone(&server);
                            let connection_shutdown = shutdown.clone();
                            connections.spawn_local(async move {
                                if let Err(e) = handle_connection_with_shutdown(
                                    stream,
                                    server,
                                    uuid::Uuid::new_v4(),
                                    connection_shutdown,
                                ).await {
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
            while let Some(result) = connections.join_next().await {
                if let Err(error) = result {
                    tracing::warn!(%error, "IPC connection task failed during shutdown drain");
                }
            }
        })
        .await;
    if remove_on_shutdown && path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

async fn handle_connection_with_id(
    stream: tokio::net::UnixStream,
    server: Arc<AgentServer>,
    connection_id: uuid::Uuid,
) -> Result<()> {
    handle_connection_with_shutdown(
        stream,
        server,
        connection_id,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
}

async fn handle_connection_with_shutdown(
    stream: tokio::net::UnixStream,
    server: Arc<AgentServer>,
    connection_id: uuid::Uuid,
    shutdown: tokio_util::sync::CancellationToken,
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

    let rpc = RpcSystem::new(Box::new(network), Some(daemon_client.client));
    tokio::pin!(rpc);
    let result = tokio::select! {
        result = &mut rpc => result.map_err(anyhow::Error::from),
        _ = shutdown.cancelled() => Ok(()),
    };
    drop(rpc);
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
        let mut delay = std::time::Duration::from_millis(10);
        loop {
            match server
                .brain_store()
                .reconcile_effect_audits_for_disconnected_leases(&brain, &lease_ids)
            {
                Ok(_) => break,
                Err(error) => {
                    tracing::error!(brain = %brain, %error, retry_ms = delay.as_millis(),
                        "disconnected runner audit reconciliation remains pending");
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(std::time::Duration::from_secs(5));
                }
            }
        }
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
