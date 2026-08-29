//! Transport-independent lifecycle operations for named Brains.
//!
//! HTTP and Cap'n Proto adapters authenticate and encode requests, but they
//! must not independently implement attachment, watch, submission, or runner
//! lease semantics.  Embedded hosts can use this service directly.

use anyhow::{ensure, Result};
use tokio::sync::broadcast;

use crate::brain::store::{
    AttachmentId, AttachmentRole, BrainAttachment, BrainEnvironment, BrainEvent, BrainEventKind,
    BrainRun, BrainRunKind, BrainRunStatus, BrainRunnerHandoff, BrainRunnerLease, BrainSchedule,
    BrainScheduleDeliveryPolicy, BrainSnapshot, BrainStore, ConnectionId, ProgramLanguage, RunId,
    RunnerHandoffId, RunnerLeaseId, ScheduleId,
};

use super::{handlers, AgentServer, BrainApprovalBroker, BrainRunnerBroker};

const PENDING_ATTACHMENT_TTL: std::time::Duration = std::time::Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum BrainSubmissionError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Forbidden(String),
    #[error(transparent)]
    State(#[from] anyhow::Error),
}

#[derive(Debug)]
pub struct BrainSubmissionOutcome {
    pub accepted: BrainEvent,
    pub run: Option<crate::brain::store::BrainRun>,
    pub result: Option<BrainEvent>,
}

/// Snapshot plus the already-subscribed event receiver for a newly activated
/// attachment. Events at or below `snapshot.revision` are represented by the
/// snapshot; later events must be projected from `events`.
pub struct BrainWatch {
    pub snapshot: BrainSnapshot,
    pub events: broadcast::Receiver<BrainEvent>,
}

/// The canonical in-process named-Brain lifecycle boundary.
#[derive(Clone)]
pub struct BrainLifecycleService {
    store: BrainStore,
    runners: BrainRunnerBroker,
    approvals: BrainApprovalBroker,
}

impl BrainLifecycleService {
    pub fn from_server(server: &AgentServer) -> Self {
        Self {
            store: server.brain_store().clone(),
            runners: server.brain_runners().clone(),
            approvals: server.brain_approvals().clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn register_test_runner(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        tx: tokio::sync::mpsc::UnboundedSender<crate::server::RunnerRequest>,
    ) {
        self.runners.register(brain, lease_id, tx);
    }

    #[cfg(test)]
    pub(crate) fn register_test_approval(
        &self,
        request_seq: u64,
        approval_id: &str,
        audience: crate::brain::store::BrainApprovalAudience,
    ) -> anyhow::Result<crate::server::brain_approval::ApprovalRegistration> {
        self.approvals.register(request_seq, approval_id, audience)
    }

    #[cfg(test)]
    pub(crate) fn push_test_event(
        &self,
        brain: &str,
        sender: &str,
        kind: BrainEventKind,
    ) -> anyhow::Result<crate::brain::store::BrainEvent> {
        self.store.push(brain, sender, kind)
    }

    #[cfg(test)]
    pub(crate) fn new(
        store: BrainStore,
        runners: BrainRunnerBroker,
        approvals: BrainApprovalBroker,
    ) -> Self {
        Self {
            store,
            runners,
            approvals,
        }
    }

    pub fn snapshot(&self, brain: &str) -> Result<BrainSnapshot> {
        self.store.snapshot(brain)
    }

    pub fn initialization(&self, brain: &str) -> Result<crate::brain::store::BrainInitialization> {
        self.store.initialization(brain)
    }

    /// Journal the reviewed initialization module as a one-shot schedule.
    /// Merely creating, listing, or attaching to a Brain never calls this.
    pub fn schedule_initialization(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        next_due_ms: u64,
    ) -> Result<BrainSchedule> {
        self.store
            .schedule_initialization(brain, attachment_id, connection_id, next_due_ms)
    }

    pub fn schedule_initialization_with_receipt(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        next_due_ms: u64,
        receipt: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<BrainSchedule> {
        self.store.schedule_initialization_with_receipt(
            brain,
            attachment_id,
            connection_id,
            next_due_ms,
            receipt,
        )
    }

    pub fn list(&self) -> Result<Vec<String>> {
        self.store.list()
    }

    /// Create one empty Brain in this daemon's indivisible environment.
    /// Callers provide only the alias; machine/workspace authority always
    /// comes from the owning store.
    pub async fn create(&self, brain: &str) -> Result<BrainSnapshot> {
        let lock = self.store.execution_lock(brain)?;
        let _creation = lock.lock_owned().await;
        ensure!(
            !self.store.list()?.iter().any(|name| name == brain),
            "Brain '{brain}' already exists"
        );
        self.store.snapshot(brain)
    }

    pub fn start_run_with_parent(
        &self,
        brain: &str,
        sender: &str,
        kind: BrainRunKind,
        request_seq: u64,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
        parent_run_id: Option<RunId>,
    ) -> Result<BrainRun> {
        self.store.start_run_with_parent(
            brain,
            sender,
            kind,
            request_seq,
            initiating_attachment_id,
            status,
            parent_run_id,
        )
    }

    pub fn inspect_run(&self, brain: &str, run_id: RunId) -> Result<BrainRun> {
        self.store.inspect_run(brain, run_id)
    }

    /// Register a frontend-owned child agent as a canonical run beneath the
    /// active parent. The task UUID is the RunId, so a lost response can be
    /// retried without creating a second child.
    pub fn start_subagent_for_run(
        &self,
        brain: &str,
        parent_run_id: RunId,
        task_id: uuid::Uuid,
        detail: Option<String>,
    ) -> Result<BrainRun> {
        let parent = self.store.inspect_run(brain, parent_run_id)?;
        let run = self.store.start_run_with_parent_id(
            brain,
            &parent.initiated_by,
            RunId(task_id),
            BrainRunKind::Subagent,
            parent.request_seq,
            parent.initiating_attachment_id,
            BrainRunStatus::Running,
            Some(parent_run_id),
            detail,
        )?;
        Ok(run)
    }

    pub fn transition_subagent_run(
        &self,
        brain: &str,
        run_id: RunId,
        status: BrainRunStatus,
        detail: Option<String>,
    ) -> Result<BrainRun> {
        let run = self.store.inspect_run(brain, run_id)?;
        ensure!(run.kind == BrainRunKind::Subagent, "run is not a subagent");
        ensure!(
            matches!(
                status,
                BrainRunStatus::Completed | BrainRunStatus::Failed | BrainRunStatus::Cancelled
            ),
            "runner may only publish a terminal subagent status"
        );
        if run.status == status {
            return Ok(run);
        }
        self.store
            .transition_run(brain, "runner", run_id, status, detail)
    }

    pub fn inspect_schedule(
        &self,
        brain: &str,
        schedule_id: ScheduleId,
    ) -> Result<Option<BrainSchedule>> {
        self.store.inspect_schedule(brain, schedule_id)
    }

    fn schedule_principal_for_run(
        &self,
        brain: &str,
        run_id: RunId,
        request_seq: u64,
    ) -> Result<BrainRun> {
        let run = self.inspect_run(brain, run_id)?;
        ensure!(
            run.request_seq == request_seq,
            "schedule callback does not match the Brain run request"
        );
        ensure!(
            matches!(
                run.status,
                BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
            ),
            "schedule callback is no longer active for this Brain run"
        );
        Ok(run)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule_for_run(
        &self,
        brain: &str,
        run_id: RunId,
        request_seq: u64,
        maximum_grant_ceiling: Option<&crate::vm::EffectSet>,
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
    ) -> Result<BrainSchedule> {
        let run = self.schedule_principal_for_run(brain, run_id, request_seq)?;
        if let Some(maximum) = maximum_grant_ceiling {
            ensure!(
                maximum.grants(&grant_ceiling),
                "a scheduled Brain run cannot create a schedule with broader authority"
            );
        }
        self.store.create_schedule(
            brain,
            &run.initiated_by,
            run.initiating_attachment_id,
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
        )
    }

    pub fn inspect_schedule_for_run(
        &self,
        brain: &str,
        run_id: RunId,
        request_seq: u64,
        schedule_id: ScheduleId,
    ) -> Result<Option<BrainSchedule>> {
        let run = self.schedule_principal_for_run(brain, run_id, request_seq)?;
        Ok(self
            .store
            .inspect_schedule(brain, schedule_id)?
            .filter(|schedule| {
                schedule.created_by == run.initiated_by
                    && schedule.initiating_attachment_id == run.initiating_attachment_id
            }))
    }

    pub fn cancel_schedule_for_run(
        &self,
        brain: &str,
        run_id: RunId,
        request_seq: u64,
        schedule_id: ScheduleId,
    ) -> Result<bool> {
        let run = self.schedule_principal_for_run(brain, run_id, request_seq)?;
        self.store.cancel_schedule(
            brain,
            &run.initiated_by,
            run.initiating_attachment_id,
            schedule_id,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
    ) -> Result<BrainSchedule> {
        self.create_schedule_with_receipt(
            brain,
            attachment_id,
            connection_id,
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_schedule_with_receipt(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: BrainScheduleDeliveryPolicy,
        mutation: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<BrainSchedule> {
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can create a schedule"
        );
        self.store.create_schedule_with_receipt(
            brain,
            &attachment.subject,
            attachment_id,
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
            mutation,
        )
    }

    pub fn cancel_schedule(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        schedule_id: ScheduleId,
    ) -> Result<bool> {
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can cancel a schedule"
        );
        self.store.cancel_schedule(
            brain,
            &attachment.subject,
            attachment.attachment_id,
            schedule_id,
        )
    }

    pub fn cancel_schedule_with_receipt(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        schedule_id: ScheduleId,
        receipt: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<bool> {
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can cancel a schedule"
        );
        self.store.cancel_schedule_with_receipt(
            brain,
            &attachment.subject,
            attachment.attachment_id,
            schedule_id,
            receipt,
        )
    }

    pub fn attach(
        &self,
        brain: &str,
        subject: &str,
        role: AttachmentRole,
        attachment_id: Option<AttachmentId>,
    ) -> Result<BrainAttachment> {
        ensure!(
            role != AttachmentRole::Runner,
            "runner authority requires a runner lease, not a client attachment"
        );
        let attachment = self.store.attach(brain, subject, role, attachment_id)?;
        let pending_connection = attachment
            .connection_id
            .expect("new Brain attachment has a pending connection");
        self.expire_pending_attachment(
            brain.to_owned(),
            attachment.attachment_id,
            pending_connection,
        );
        Ok(attachment)
    }

    fn expire_pending_attachment(
        &self,
        brain: String,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) {
        let store = self.store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(PENDING_ATTACHMENT_TTL).await;
            if store
                .expire_pending_connection(&brain, attachment_id, connection_id)
                .unwrap_or(false)
            {
                let _ = store.remove_if_unused(&brain);
            }
        });
    }

    /// Resolve an exact, not-yet-activated attachment reservation. Remote
    /// adapters use this to bind authenticated claims before upgrading the
    /// connection; activation itself remains atomic in `watch`.
    pub fn pending_attachment(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        self.snapshot(brain)?
            .attachments
            .into_iter()
            .find(|attachment| {
                attachment.attachment_id == attachment_id
                    && attachment.connection_id == Some(connection_id)
                    && !attachment.connected
            })
            .ok_or_else(|| anyhow::anyhow!("unknown pending Brain attachment connection"))
    }

    pub fn connection(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        self.store
            .require_connection(brain, attachment_id, connection_id)
    }

    /// Activate and subscribe without a snapshot/event race. The subscription
    /// is installed first, activation is recorded second, and the snapshot is
    /// taken last; consumers discard received events through its revision.
    pub fn watch(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainWatch> {
        let events = self.store.subscribe(brain)?;
        self.store
            .activate_connection(brain, attachment_id, connection_id)?;
        match self.store.snapshot(brain) {
            Ok(snapshot) => Ok(BrainWatch { snapshot, events }),
            Err(error) => {
                let _ = self.detach(brain, attachment_id, connection_id);
                Err(error)
            }
        }
    }

    pub fn acknowledge(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        seq: u64,
    ) -> Result<BrainAttachment> {
        self.store
            .acknowledge(brain, attachment_id, connection_id, seq)
    }

    pub fn detach(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<()> {
        // Validate the exact generation before deriving any run authority. A
        // stale socket must not cancel work started by its replacement.
        let ordinary = self.store.retire_connection_and_owned_active_runs(
            brain,
            attachment_id,
            connection_id,
        )?;
        let snapshot = self.store.snapshot(brain)?;
        let brain_id = snapshot.brain_id;
        let reserved = self
            .store
            .pending_reserved_cancellations_for_attachment(brain, attachment_id)?;
        let ordinary = ordinary
            .into_iter()
            .filter(|run_id| !reserved.contains(run_id))
            .collect::<Vec<_>>();
        let runner_lease = snapshot.runner_lease.as_ref().map(|lease| lease.lease_id);
        self.store.detach(brain, attachment_id, connection_id)?;
        self.approvals
            .cancel_connection(brain_id, attachment_id, connection_id);
        // The runner cancellation is addressed to this Brain's exact durable
        // run and current lease. First publish (or durably hand off) the
        // Result+Failed outcome, then enqueue cancellation before aborting the
        // daemon wait. Any late response is fenced by terminal state and its
        // dropped response channel.
        for run_id in ordinary {
            let run = self.store.inspect_run(brain, run_id)?;
            let detail = "initiating Brain connection disconnected".to_string();
            match self.store.terminalize_run_with_result_if_active(
                brain,
                "daemon",
                run_id,
                run.request_seq,
                BrainRunStatus::Failed,
                detail.clone(),
            ) {
                Ok(Some(_)) => {}
                Ok(None) if self.store.inspect_run(brain, run_id)?.status.is_terminal() => {
                    continue;
                }
                Ok(None) => self.store.schedule_disconnect_terminalization_retry(
                    brain.to_string(),
                    "daemon".into(),
                    run_id,
                    run.request_seq,
                    BrainRunStatus::Failed,
                    detail,
                ),
                Err(error) => {
                    tracing::error!(brain = %brain, run_id = %run_id.0, %error,
                        "disconnect terminalization deferred to durable retry owner");
                    self.store.schedule_disconnect_terminalization_retry(
                        brain.to_string(),
                        "daemon".into(),
                        run_id,
                        run.request_seq,
                        BrainRunStatus::Failed,
                        detail,
                    );
                }
            }
            if let Some(lease_id) = runner_lease {
                if let Err(error) = self
                    .runners
                    .request_run_cancellation(brain, lease_id, run_id)
                {
                    tracing::warn!(brain = %brain, run_id = %run_id.0, %error,
                        "could not forward disconnect cancellation to runner");
                }
            }
            self.runners.abort_run(brain, run_id);
        }
        // A cancellation reservation is durable authority, not authority
        // owned by the WebSocket command future. Once that exact initiating
        // generation disconnects, finish its reserved terminal outcome and
        // release only waits for that run, even if the runner never replies.
        for run_id in reserved {
            self.runners.abort_run(brain, run_id);
            match self
                .store
                .complete_reserved_run_cancellation_on_disconnect(brain, "daemon", run_id)
            {
                Ok(true) => {}
                Ok(false) | Err(_) => self.store.schedule_reserved_cancellation_retry(
                    brain.to_string(),
                    "daemon".into(),
                    run_id,
                ),
            }
        }
        self.store.remove_if_unused(brain)?;
        Ok(())
    }

    pub async fn submit(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        kind: BrainEventKind,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        if let BrainEventKind::SpeculativePrompt { text } = kind {
            return self
                .start_speculative(brain, attachment_id, connection_id, text)
                .await;
        }
        let attachment = self
            .connection(brain, attachment_id, connection_id)
            .map_err(BrainSubmissionError::State)?;
        let can_approve = crate::brain::credential::default_participant_scopes(attachment.role)
            .contains(&crate::brain::credential::BrainCredentialScope::BrainApprove);
        self.submit_for_attachment(brain, &attachment, kind, can_approve)
            .await
    }

    /// Explicitly start one cancellable speculative helper through the same
    /// authoritative submission and runner path as every other Brain turn.
    pub async fn start_speculative(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        prompt: String,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        let attachment = self
            .connection(brain, attachment_id, connection_id)
            .map_err(BrainSubmissionError::State)?;
        if attachment.role != AttachmentRole::Driver {
            return Err(BrainSubmissionError::Forbidden(
                "only a Brain driver can start a speculative run".into(),
            ));
        }
        let execution_lock = self
            .store
            .execution_lock(brain)
            .map_err(BrainSubmissionError::State)?;
        let _turn = execution_lock.lock_owned().await;
        let (accepted, run) = self
            .store
            .accept_speculative_run(brain, &attachment.subject, attachment_id, prompt)
            .map_err(BrainSubmissionError::State)?;
        if let Err(error) =
            self.store
                .bind_run_connection(brain, run.run_id, attachment_id, connection_id)
        {
            self.runners.fence_run_cancellation(brain, run.run_id);
            let detail = "initiating Brain connection disconnected".to_string();
            match self.store.terminalize_run_with_result_if_active(
                brain,
                "daemon",
                run.run_id,
                run.request_seq,
                BrainRunStatus::Failed,
                detail.clone(),
            ) {
                Ok(Some(_)) => {}
                Ok(None)
                    if self
                        .store
                        .inspect_run(brain, run.run_id)
                        .is_ok_and(|current| current.status.is_terminal()) => {}
                Ok(None) | Err(_) => self.store.schedule_disconnect_terminalization_retry(
                    brain.to_string(),
                    "daemon".into(),
                    run.run_id,
                    run.request_seq,
                    BrainRunStatus::Failed,
                    detail,
                ),
            }
            self.runners.abort_run(brain, run.run_id);
            return Err(BrainSubmissionError::State(error));
        }
        let snapshot = self.snapshot(brain).map_err(BrainSubmissionError::State)?;
        let ready_lease = snapshot.runner_lease.filter(|lease| {
            lease.environment_generation == snapshot.environment.generation
                && lease.expires_ms > crate::brain::store::unix_millis()
                && self.runners.has_registration(brain, lease.lease_id)
        });
        if let Some(lease) = ready_lease {
            let service = self.clone();
            let brain = brain.to_string();
            let run_id = run.run_id;
            tokio::spawn(async move {
                let _turn = _turn;
                if let Err(error) = handlers::resume_queued_named_brain_runs_in_lane(
                    service.store.clone(),
                    service.runners.clone(),
                    brain.clone(),
                    lease.lease_id,
                )
                .await
                {
                    tracing::warn!(%brain, run_id = %run_id.0, %error, "speculative Brain supervisor failed");
                }
            });
        } else {
            drop(_turn);
        }
        Ok(BrainSubmissionOutcome {
            accepted,
            run: Some(run),
            result: None,
        })
    }

    pub(crate) async fn submit_with_authority(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        kind: BrainEventKind,
        can_approve: bool,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        if let BrainEventKind::SpeculativePrompt { text } = kind {
            return self
                .start_speculative(brain, attachment_id, connection_id, text)
                .await;
        }
        self.submit_with_authority_and_receipt(
            brain,
            attachment_id,
            connection_id,
            kind,
            can_approve,
            None,
        )
        .await
    }

    pub(crate) async fn submit_with_authority_and_receipt(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        kind: BrainEventKind,
        can_approve: bool,
        mutation: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        if let BrainEventKind::SpeculativePrompt { text } = kind {
            return self
                .start_speculative(brain, attachment_id, connection_id, text)
                .await;
        }
        let attachment = self
            .connection(brain, attachment_id, connection_id)
            .map_err(BrainSubmissionError::State)?;
        handlers::submit_named_brain_event_with_authority_and_receipt(
            &self.store,
            &self.runners,
            &self.approvals,
            brain,
            &attachment,
            kind,
            can_approve,
            mutation,
        )
        .await
    }

    async fn submit_for_attachment(
        &self,
        brain: &str,
        attachment: &BrainAttachment,
        kind: BrainEventKind,
        can_approve: bool,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        handlers::submit_named_brain_event_with_authority(
            &self.store,
            &self.runners,
            &self.approvals,
            brain,
            attachment,
            kind,
            can_approve,
        )
        .await
    }

    pub async fn cancel_run(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        run_id: RunId,
    ) -> Result<BrainRun> {
        self.cancel_run_with_receipt(brain, attachment_id, connection_id, run_id, None)
            .await
    }

    pub async fn cancel_run_with_receipt(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        run_id: RunId,
        receipt: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<BrainRun> {
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can cancel a run"
        );
        let mutation_id = receipt.as_ref().map(|receipt| receipt.mutation_id);
        let reserved = match receipt {
            Some(receipt) => Some(
                self.store
                    .reserve_run_cancellation(
                        brain,
                        &attachment.subject,
                        attachment_id,
                        run_id,
                        receipt,
                    )
                    .await?,
            ),
            None => None,
        };
        let run = match &reserved {
            Some(reserved) => reserved.run.clone(),
            None => self.inspect_run(brain, run_id)?,
        };
        ensure!(
            run.initiating_attachment_id == attachment_id,
            "a Brain run can only be cancelled by its initiating attachment"
        );
        if run.status == BrainRunStatus::Cancelled {
            return Ok(run);
        }
        ensure!(
            !run.status.is_terminal() || run.status == BrainRunStatus::Interrupted,
            "Brain run has already finished"
        );
        let owner = self.clone();
        let brain = brain.to_string();
        let sender = attachment.subject;
        // Dropping a WebSocket command future must not drop the only owner of
        // a durable cancellation reservation. Tokio tasks remain owned by the
        // daemon when their JoinHandle is dropped; exact-generation detach
        // below can finish and abort this exact run if the runner withholds its
        // cancellation reply.
        tokio::spawn(async move {
            owner
                .reconcile_reserved_run_cancellation(brain, sender, run, reserved, mutation_id)
                .await
        })
        .await
        .map_err(|error| anyhow::anyhow!("Brain cancellation owner failed: {error}"))?
    }

    async fn reconcile_reserved_run_cancellation(
        &self,
        brain: String,
        sender: String,
        run: BrainRun,
        reserved: Option<crate::brain::store::BrainRunCancellationReservation>,
        mutation_id: Option<uuid::Uuid>,
    ) -> Result<BrainRun> {
        let needs_runner_reconciliation = reserved
            .as_ref()
            .is_some_and(|reserved| reserved.needs_runner_cancel);
        if let Some(mutation_id) = mutation_id {
            self.store.mark_run_cancellation_dispatching(
                &brain,
                &sender,
                run.run_id,
                mutation_id,
            )?;
        }
        if matches!(
            run.status,
            BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
        ) || needs_runner_reconciliation
        {
            self.store
                .reserve_run_publication_cancellation(&brain, run.run_id)
                .await?;
            let snapshot = self.snapshot(&brain)?;
            let lease = match snapshot
                .runner_lease
                .filter(|lease| lease.expires_ms > crate::brain::store::unix_millis())
            {
                Some(lease) => lease,
                None => {
                    self.store
                        .clear_run_cancellation(&brain, run.run_id)
                        .await?;
                    anyhow::bail!("named Brain '{brain}' has no live runner");
                }
            };
            // Cancellation at the runner is idempotent by RunId: `false`
            // means the run is already absent there, which reconciles the
            // durable cancellation intent just as safely as `true`.
            if let Err(error) = self
                .runners
                .cancel_run(&brain, lease.lease_id, run.run_id)
                .await
            {
                let current = self.inspect_run(&brain, run.run_id)?;
                if current.status == BrainRunStatus::Cancelled {
                    return Ok(current);
                }
                self.store
                    .clear_run_cancellation(&brain, run.run_id)
                    .await?;
                return Err(error);
            }
        }
        if let Some(mutation_id) = mutation_id {
            self.store.mark_run_cancellation_reconciled(
                &brain,
                &sender,
                run.run_id,
                mutation_id,
            )?;
        }
        let publication = self
            .store
            .acquire_run_publication(&brain, run.run_id)
            .await?;
        let transition = match reserved {
            Some(_) => self
                .store
                .complete_reserved_run_cancellation(&brain, &sender, run.run_id),
            None => self.store.transition_run(
                &brain,
                &sender,
                run.run_id,
                BrainRunStatus::Cancelled,
                Some("cancelled by initiating driver".into()),
            ),
        };
        let transitioned = match transition {
            Ok(run) => Ok(run),
            Err(error) => {
                let current = self.inspect_run(&brain, run.run_id)?;
                if current.status == BrainRunStatus::Cancelled {
                    Ok(current)
                } else {
                    Err(error)
                }
            }
        };
        drop(publication);
        let transitioned = transitioned?;
        // The runner has acknowledged cancellation and the terminal state is
        // now durable. Release only the daemon-side dispatch wait for this
        // run. `abort_run` does not revoke the runner lease or the detached
        // owner of an already-begun host effect, so that owner can still
        // publish its single authoritative late audit outcome.
        self.runners.abort_run(&brain, run.run_id);
        if !matches!(
            run.status,
            BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
        ) {
            self.store.prune_run_publication(&brain, run.run_id)?;
        }
        Ok(transitioned)
    }

    pub fn acquire_runner(
        &self,
        brain: &str,
        subject: &str,
        environment: &BrainEnvironment,
        lease_id: Option<RunnerLeaseId>,
        ttl_ms: u64,
    ) -> Result<BrainRunnerLease> {
        ensure!(
            environment == self.store.environment(),
            "runner environment does not match the daemon Brain environment"
        );
        let lease = self.store.acquire_runner_lease(
            brain,
            subject,
            environment.generation,
            lease_id,
            ttl_ms,
        )?;
        self.expire_runner_lease(brain.to_owned(), lease.lease_id, lease.expires_ms);
        Ok(lease)
    }

    fn expire_runner_lease(&self, brain: String, lease_id: RunnerLeaseId, expires_ms: u64) {
        let store = self.store.clone();
        tokio::spawn(async move {
            loop {
                let delay_ms = expires_ms.saturating_sub(crate::brain::store::unix_millis());
                if delay_ms == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if store
                .expire_runner_lease(&brain, lease_id, crate::brain::store::unix_millis())
                .is_ok_and(|expired| expired)
            {
                let _ = store.remove_if_unused(&brain);
            }
        });
    }

    pub fn release_runner(&self, brain: &str, lease_id: RunnerLeaseId) -> Result<()> {
        self.store.release_runner_lease(brain, lease_id)?;
        self.store.remove_if_unused(brain)?;
        Ok(())
    }

    pub fn request_runner_handoff(
        &self,
        brain: &str,
        requested_by: &str,
        target_subject: &str,
        expected_lease_id: RunnerLeaseId,
        environment: &BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<BrainRunnerHandoff> {
        self.request_runner_handoff_with_receipt(
            brain,
            requested_by,
            target_subject,
            expected_lease_id,
            environment,
            ttl_ms,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn request_runner_handoff_with_receipt(
        &self,
        brain: &str,
        requested_by: &str,
        target_subject: &str,
        expected_lease_id: RunnerLeaseId,
        environment: &BrainEnvironment,
        ttl_ms: u64,
        mutation: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<BrainRunnerHandoff> {
        let handoff = self.store.request_runner_handoff_with_receipt(
            brain,
            requested_by,
            target_subject,
            expected_lease_id,
            environment.generation,
            ttl_ms,
            mutation,
        )?;
        self.expire_runner_handoff(brain.to_owned(), handoff.handoff_id, handoff.expires_ms);
        Ok(handoff)
    }

    fn expire_runner_handoff(&self, brain: String, handoff_id: RunnerHandoffId, expires_ms: u64) {
        let store = self.store.clone();
        tokio::spawn(async move {
            loop {
                let delay_ms = expires_ms.saturating_sub(crate::brain::store::unix_millis());
                if delay_ms == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if store
                .expire_runner_handoff(&brain, handoff_id, crate::brain::store::unix_millis())
                .is_ok_and(|expired| expired)
            {
                let _ = store.remove_if_unused(&brain);
            }
        });
    }

    pub fn accept_runner_handoff(
        &self,
        brain: &str,
        target_subject: &str,
        handoff_id: RunnerHandoffId,
        environment: &BrainEnvironment,
        ttl_ms: u64,
    ) -> Result<BrainRunnerLease> {
        ensure!(
            environment == self.store.environment(),
            "runner handoff environment does not match the daemon Brain environment"
        );
        let lease = self.store.accept_runner_handoff(
            brain,
            target_subject,
            handoff_id,
            environment.generation,
            ttl_ms,
        )?;
        self.expire_runner_lease(brain.to_owned(), lease.lease_id, lease.expires_ms);
        Ok(lease)
    }

    pub fn cancel_runner_handoff(
        &self,
        brain: &str,
        handoff_id: RunnerHandoffId,
        sender: &str,
    ) -> Result<()> {
        self.store
            .cancel_runner_handoff(brain, handoff_id, sender)?;
        self.store.remove_if_unused(brain)?;
        Ok(())
    }

    pub fn cancel_runner_handoff_with_receipt(
        &self,
        brain: &str,
        handoff_id: RunnerHandoffId,
        sender: &str,
        receipt: Option<crate::brain::store::BrainMutationReceipt>,
    ) -> Result<()> {
        self.store
            .cancel_runner_handoff_with_receipt(brain, handoff_id, sender, receipt)?;
        self.store.remove_if_unused(brain)?;
        Ok(())
    }

    pub async fn resume_queued_runs(
        &self,
        brain: String,
        lease_id: RunnerLeaseId,
    ) -> Result<usize> {
        handlers::resume_queued_named_brain_runs(
            self.store.clone(),
            self.runners.clone(),
            brain,
            lease_id,
        )
        .await
    }

    pub async fn replay_committed_memory(
        &self,
        brain: String,
        lease_id: RunnerLeaseId,
    ) -> Result<usize> {
        handlers::replay_committed_named_brain_memory(
            self.store.clone(),
            self.runners.clone(),
            brain,
            lease_id,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> BrainLifecycleService {
        let temp = tempfile::tempdir().unwrap();
        // Keep this test hermetic while allowing the service to outlive the
        // TempDir handle for the duration of a single test.
        let root = temp.keep();
        BrainLifecycleService::new(
            BrainStore::with_root("box.local", Some(root)),
            BrainRunnerBroker::default(),
            BrainApprovalBroker::default(),
        )
    }

    #[tokio::test]
    async fn explicit_creation_uses_the_daemon_environment_and_rejects_alias_reuse() {
        let service = service();
        let created = service.create("review").await.unwrap();
        assert_eq!(created.name, "review");
        assert_eq!(created.environment.machine, "box.local");
        assert_eq!(created.environment, *service.store.environment());
        assert_eq!(created.revision, 0);
        assert!(created.events.is_empty());
        assert!(service.create("review").await.is_err());
    }

    #[tokio::test]
    async fn explicit_speculative_run_is_durable_inspectable_and_cancellable_after_restart() {
        let root = tempfile::tempdir().unwrap().keep();
        let make_service = || {
            BrainLifecycleService::new(
                BrainStore::with_root("box.local", Some(root.clone())),
                BrainRunnerBroker::default(),
                BrainApprovalBroker::default(),
            )
        };
        let service = make_service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();

        let outcome = service
            .start_speculative(
                "shared",
                driver.attachment_id,
                connection_id,
                "inspect likely context".into(),
            )
            .await
            .unwrap();
        let run = outcome.run.unwrap();
        assert_eq!(outcome.accepted.run_id, Some(run.run_id));
        assert_eq!(run.kind, BrainRunKind::Speculative);
        assert_eq!(run.status, BrainRunStatus::QueuedForEnvironment);
        assert_eq!(run.request_seq, outcome.accepted.seq);
        assert!(matches!(
            outcome.accepted.kind,
            BrainEventKind::SpeculativePrompt { ref text } if text == "inspect likely context"
        ));
        assert_eq!(
            service.inspect_run("shared", run.run_id).unwrap().run_id,
            run.run_id
        );
        assert!(service
            .snapshot("shared")
            .unwrap()
            .runs
            .iter()
            .any(|candidate| candidate.run_id == run.run_id));

        drop(service);
        let restarted = make_service();
        let restored = restarted.inspect_run("shared", run.run_id).unwrap();
        assert_eq!(restored, run);
        assert_eq!(
            restarted
                .snapshot("shared")
                .unwrap()
                .events
                .iter()
                .find(|event| event.seq == run.request_seq)
                .unwrap()
                .run_id,
            Some(run.run_id)
        );
        let driver = restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(driver.attachment_id),
            )
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = restarted
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let cancelled = restarted
            .cancel_run("shared", driver.attachment_id, connection_id, run.run_id)
            .await
            .unwrap();
        assert_eq!(cancelled.status, BrainRunStatus::Cancelled);
    }

    #[tokio::test]
    async fn cancellation_reservation_survives_disconnect_with_withheld_runner_reply() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let environment = service.store.environment().clone();
        let lease = service
            .acquire_runner("shared", "runner", &environment, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        service.runners.register("shared", lease.lease_id, tx);
        let driver_id = driver.attachment_id;

        let accepted = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            service.start_speculative("shared", driver_id, connection_id, "look ahead".into()),
        )
        .await
        .expect("speculative acceptance must not wait for runner completion")
        .unwrap();
        let accepted_run = accepted.run.unwrap();
        assert_eq!(accepted_run.status, BrainRunStatus::QueuedForEnvironment);
        let crate::server::RunnerRequest::Turn(turn) = rx.recv().await.unwrap() else {
            panic!("expected speculative turn")
        };
        let run_id = turn.run_id;
        assert_eq!(run_id, accepted_run.run_id);
        assert_eq!(turn.prompt, "look ahead");

        let receipt = crate::brain::store::BrainMutationReceipt {
            mutation_id: uuid::Uuid::new_v4(),
            attachment_id: driver_id,
            expected_revision: service.snapshot("shared").unwrap().revision,
            environment_generation: environment.generation,
            command_sha256: "cancel-running-speculative".into(),
        };
        let cancelling = {
            let service = service.clone();
            let receipt = receipt.clone();
            tokio::spawn(async move {
                service
                    .cancel_run_with_receipt(
                        "shared",
                        driver_id,
                        connection_id,
                        run_id,
                        Some(receipt),
                    )
                    .await
            })
        };
        let crate::server::RunnerRequest::Cancel(cancel) = rx.recv().await.unwrap() else {
            panic!("expected cancellation request")
        };
        assert_eq!(cancel.run_id, run_id);
        let runtime = crate::runtime::ProgramRuntime::new();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        turn.response_tx
            .send(Ok(crate::server::RunnerTurnResult {
                source: "(say \"late\")".into(),
                language: ProgramLanguage::Lisp,
                output: "late".into(),
                turn_events: vec![crate::server::RunnerTurnEvent::Call {
                    tool_id: "late-tool".into(),
                    name: "late-tool".into(),
                    input: serde_json::json!({"late": true}),
                }],
                runtime_revision: 0,
                checkpoint,
                effect_journal: Vec::new(),
                commit_ack: None,
            }))
            .unwrap();
        service.detach("shared", driver_id, connection_id).unwrap();
        tokio::task::yield_now().await;
        assert_eq!(
            service.inspect_run("shared", run_id).unwrap().status,
            BrainRunStatus::Cancelled
        );
        assert!(
            cancel.response_tx.send(Ok(true)).is_err(),
            "exact runner cancellation wait survived initiating disconnect"
        );
        assert_eq!(
            cancelling.await.unwrap().unwrap().status,
            BrainRunStatus::Cancelled
        );
        assert!(accepted.result.is_none());
        let snapshot = service.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .runs
                .iter()
                .find(|run| run.run_id == run_id)
                .unwrap()
                .status,
            BrainRunStatus::Cancelled
        );
        assert!(!snapshot.events.iter().any(|event| {
            event.run_id == Some(run_id)
                && matches!(
                    event.kind,
                    BrainEventKind::ToolCall { .. }
                        | BrainEventKind::Program { .. }
                        | BrainEventKind::RuntimeCommitted { .. }
                        | BrainEventKind::Result { .. }
                )
        }));
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| { event.mutation.as_ref() == Some(&receipt) })
                .count(),
            1
        );
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id: event_run_id, status, .. }
                        if event_run_id == run_id && status.is_terminal()
                ))
                .count(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn durable_cancellation_reservation_fences_disconnect_before_owner_spawn() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let prompt = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::SpeculativePrompt {
                    text: "pause at durable reservation".into(),
                },
            )
            .unwrap();
        let run = service
            .store
            .start_run(
                "shared",
                "alice",
                crate::brain::store::BrainRunKind::Speculative,
                prompt.seq,
                driver.attachment_id,
                BrainRunStatus::Running,
            )
            .unwrap();
        let receipt = crate::brain::store::BrainMutationReceipt {
            mutation_id: uuid::Uuid::new_v4(),
            attachment_id: driver.attachment_id,
            expected_revision: service.snapshot("shared").unwrap().revision,
            environment_generation: service.store.environment().generation,
            command_sha256: "barrier-cancel".into(),
        };
        let (reserved, release) = service
            .store
            .pause_after_cancellation_reservation_for_test();
        let reserving = {
            let store = service.store.clone();
            let receipt = receipt.clone();
            tokio::spawn(async move {
                store
                    .reserve_run_cancellation(
                        "shared",
                        "alice",
                        driver.attachment_id,
                        run.run_id,
                        receipt,
                    )
                    .await
            })
        };
        tokio::task::spawn_blocking(move || reserved.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(service
            .store
            .run_cancellation_reserved("shared", run.run_id)
            .unwrap());
        service
            .detach("shared", driver.attachment_id, connection_id)
            .unwrap();
        release.send(()).unwrap();
        reserving.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while service.inspect_run("shared", run.run_id).unwrap().status
                != BrainRunStatus::Cancelled
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("durable cancellation reservation lost its disconnect owner");
        assert!(service
            .store
            .terminalize_run_with_result_if_active(
                "shared",
                "runner",
                run.run_id,
                prompt.seq,
                BrainRunStatus::Failed,
                "late turn response".into(),
            )
            .unwrap()
            .is_none());
        let snapshot = service.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| { event.mutation.as_ref() == Some(&receipt) })
                .count(),
            1
        );
        assert!(!snapshot.events.iter().any(|event| {
            event.run_id == Some(run.run_id)
                && matches!(
                    event.kind,
                    BrainEventKind::Program { .. }
                        | BrainEventKind::EffectRecorded { .. }
                        | BrainEventKind::Result { .. }
                )
        }));
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(
                    event.kind,
                    BrainEventKind::RunStatusChanged { run_id, status, .. }
                        if run_id == run.run_id && status.is_terminal()
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn accepted_speculative_run_keeps_fifo_lane_until_supervised_dispatch() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let environment = service.store.environment().clone();
        let lease = service
            .acquire_runner("shared", "runner", &environment, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        service.runners.register("shared", lease.lease_id, tx);

        let speculative = service
            .start_speculative(
                "shared",
                driver.attachment_id,
                connection_id,
                "first".into(),
            )
            .await
            .unwrap();
        let ordinary = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .submit(
                        "shared",
                        driver.attachment_id,
                        connection_id,
                        BrainEventKind::Prompt {
                            text: "second".into(),
                        },
                    )
                    .await
            })
        };
        let crate::server::RunnerRequest::Turn(first) = rx.recv().await.unwrap() else {
            panic!("expected first turn")
        };
        assert_eq!(first.prompt, "first");
        assert_eq!(
            first.request_seq,
            speculative.run.as_ref().unwrap().request_seq
        );
        let first_seq = first.request_seq;
        let checkpoint = crate::runtime::ProgramRuntime::new()
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        first
            .response_tx
            .send(Ok(crate::server::RunnerTurnResult {
                source: "(say \"first\")".into(),
                language: ProgramLanguage::Lisp,
                output: "first".into(),
                turn_events: Vec::new(),
                runtime_revision: 0,
                checkpoint: checkpoint.clone(),
                effect_journal: Vec::new(),
                commit_ack: None,
            }))
            .unwrap();
        let crate::server::RunnerRequest::Turn(second) = rx.recv().await.unwrap() else {
            panic!("expected second turn")
        };
        assert_eq!(second.prompt, "second");
        assert!(first_seq < second.request_seq);
        second
            .response_tx
            .send(Ok(crate::server::RunnerTurnResult {
                source: "(say \"second\")".into(),
                language: ProgramLanguage::Lisp,
                output: "second".into(),
                turn_events: Vec::new(),
                runtime_revision: 0,
                checkpoint,
                effect_journal: Vec::new(),
                commit_ack: None,
            }))
            .unwrap();
        ordinary.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn program_cancellation_atomically_suppresses_late_effect_checkpoint_and_result() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let environment = service.store.environment().clone();
        let lease = service
            .acquire_runner("shared", "runner", &environment, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        service.runners.register("shared", lease.lease_id, tx);
        let driver_id = driver.attachment_id;
        let submitting = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .submit(
                        "shared",
                        driver_id,
                        connection_id,
                        BrainEventKind::Program {
                            language: ProgramLanguage::Lisp,
                            source: "(say \"late-program\")".into(),
                        },
                    )
                    .await
            })
        };
        let crate::server::RunnerRequest::Program(program) = rx.recv().await.unwrap() else {
            panic!("expected program request")
        };
        let run_id = program.run_id;
        let cancelling = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .cancel_run("shared", driver_id, connection_id, run_id)
                    .await
            })
        };
        let crate::server::RunnerRequest::Cancel(cancel) = rx.recv().await.unwrap() else {
            panic!("expected program cancellation")
        };
        cancel.response_tx.send(Ok(true)).unwrap();
        assert_eq!(
            cancelling.await.unwrap().unwrap().status,
            BrainRunStatus::Cancelled
        );
        let runtime = crate::runtime::ProgramRuntime::new();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        let effect = crate::server::RunnerEffectRecord {
            execution_id: uuid::Uuid::new_v4(),
            entry: crate::vm::EffectJournalEntry {
                effect: crate::vm::VmSideEffect {
                    protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                    sequence: 0,
                    requirement: crate::vm::CapabilityRequirement {
                        capability: crate::vm::CapabilityKind::SessionEmit,
                        selector: crate::vm::ResourceSelector::None,
                    },
                    event: crate::vm::HostSideEffect::Emit {
                        text: "late effect".into(),
                    },
                    output: Vec::new(),
                    origin: crate::vm::SourceOrigin::generated("late-program-test"),
                },
                state: crate::vm::EffectJournalState::Acknowledged { values: Vec::new() },
            },
        };
        program
            .response_tx
            .send(Ok(crate::server::RunnerProgramResult {
                output: "late-program".into(),
                runtime_revision: 0,
                checkpoint,
                effect_journal: vec![effect],
            }))
            .unwrap();
        submitting.await.unwrap().unwrap();
        let snapshot = service.snapshot("shared").unwrap();
        assert_eq!(
            service.inspect_run("shared", run_id).unwrap().status,
            BrainRunStatus::Cancelled
        );
        assert!(!snapshot.events.iter().any(|event| {
            event.run_id == Some(run_id)
                && matches!(
                    event.kind,
                    BrainEventKind::EffectRecorded { .. }
                        | BrainEventKind::RuntimeCommitted { .. }
                        | BrainEventKind::Result { .. }
                )
        }));
    }

    #[tokio::test]
    async fn lifecycle_service_owns_attachment_watch_and_cleanup() {
        let service = service();
        let attachment = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = attachment.connection_id.unwrap();
        assert_eq!(
            service
                .pending_attachment("shared", attachment.attachment_id, connection_id)
                .unwrap()
                .subject,
            "alice"
        );

        let watch = service
            .watch("shared", attachment.attachment_id, connection_id)
            .unwrap();
        assert!(watch.snapshot.attachments[0].connected);
        assert!(service
            .pending_attachment("shared", attachment.attachment_id, connection_id)
            .is_err());

        let acknowledged = service
            .acknowledge(
                "shared",
                attachment.attachment_id,
                connection_id,
                watch.snapshot.revision,
            )
            .unwrap();
        assert_eq!(acknowledged.acknowledged_seq, watch.snapshot.revision);
        service
            .detach("shared", attachment.attachment_id, connection_id)
            .unwrap();
        assert!(!service.list().unwrap().iter().any(|name| name == "shared"));
    }

    #[tokio::test]
    async fn initialization_requires_the_exact_active_driver_connection() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let driver_connection = driver.connection_id.unwrap();

        assert!(service
            .schedule_initialization("shared", driver.attachment_id, driver_connection, 1_000,)
            .is_err());
        let _watch = service
            .watch("shared", driver.attachment_id, driver_connection)
            .unwrap();
        let schedule = service
            .schedule_initialization("shared", driver.attachment_id, driver_connection, 1_000)
            .unwrap();
        assert!(schedule.module_identity.is_some());
        assert!(service.snapshot("shared").unwrap().runs.is_empty());

        let consultant = service
            .attach("shared", "bob", AttachmentRole::Consultant, None)
            .unwrap();
        let consultant_connection = consultant.connection_id.unwrap();
        let _consultant_watch = service
            .watch("shared", consultant.attachment_id, consultant_connection)
            .unwrap();
        assert!(service
            .schedule_initialization(
                "shared",
                consultant.attachment_id,
                consultant_connection,
                2_000,
            )
            .unwrap_err()
            .to_string()
            .contains("only an active Brain driver"));

        let runner = service
            .store
            .attach("shared", "runner", AttachmentRole::Runner, None)
            .unwrap();
        let runner_connection = runner.connection_id.unwrap();
        service
            .store
            .activate_connection("shared", runner.attachment_id, runner_connection)
            .unwrap();
        assert!(service
            .schedule_initialization("shared", runner.attachment_id, runner_connection, 2_500,)
            .is_err());

        service
            .detach("shared", driver.attachment_id, driver_connection)
            .unwrap();
        assert!(service
            .schedule_initialization("shared", driver.attachment_id, driver_connection, 3_000,)
            .is_err());
    }

    #[tokio::test]
    async fn effect_audit_cancellation_aborts_dispatch_only_after_ack_and_durable_terminalization()
    {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = driver.connection_id.unwrap();
        let _watch = service
            .watch("shared", driver.attachment_id, connection_id)
            .unwrap();
        let request = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "hold one effect".into(),
                },
            )
            .unwrap();
        let run = service
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                request.seq,
                driver.attachment_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        let environment = service.store.environment().clone();
        let lease = service
            .acquire_runner("shared", "runner", &environment, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        service.runners.register("shared", lease.lease_id, tx);
        let run_id = run.run_id;
        let request_seq = run.request_seq;
        let driver_id = driver.attachment_id;
        let approval_audience = crate::brain::store::BrainApprovalAudience {
            brain_id: service.store.snapshot("shared").unwrap().brain_id,
            brain: "shared".into(),
            attachment_id: driver_id,
            subject: driver.subject.clone(),
            role: driver.role,
            environment_generation: environment.generation,
        };

        let dispatch = {
            let runners = service.runners.clone();
            tokio::spawn(async move {
                runners
                    .dispatch_turn(
                        "shared",
                        lease.lease_id,
                        run_id,
                        request_seq,
                        "hold one effect".into(),
                        Vec::new(),
                        approval_audience,
                        Some(connection_id),
                    )
                    .await
            })
        };
        let crate::server::RunnerRequest::Turn(turn) = rx.recv().await.unwrap() else {
            panic!("expected active turn dispatch")
        };
        let cancelling = {
            let service = service.clone();
            tokio::spawn(async move {
                service
                    .cancel_run("shared", driver_id, connection_id, run_id)
                    .await
            })
        };
        let crate::server::RunnerRequest::Cancel(cancel) = rx.recv().await.unwrap() else {
            panic!("expected exact runner cancellation")
        };
        assert_eq!(cancel.run_id, run_id);
        tokio::task::yield_now().await;
        assert!(
            !dispatch.is_finished(),
            "daemon dispatch aborted before the runner acknowledged cancellation"
        );
        assert!(
            !cancelling.is_finished(),
            "cancellation completed before the runner acknowledgement"
        );
        assert_eq!(
            service.inspect_run("shared", run_id).unwrap().status,
            BrainRunStatus::Running,
            "cancellation became durable before the runner acknowledgement"
        );

        cancel.response_tx.send(Ok(true)).unwrap();
        let cancelled = cancelling.await.unwrap().unwrap();
        assert_eq!(cancelled.status, BrainRunStatus::Cancelled);
        let dispatch_error = dispatch.await.unwrap().unwrap_err();
        assert!(dispatch_error.to_string().contains("run cancelled"));
        assert!(
            turn.response_tx.is_closed(),
            "terminal cancellation retained the daemon dispatch receiver"
        );
    }

    #[tokio::test]
    async fn only_the_initiating_driver_can_cancel_a_running_run() {
        let service = service();
        let attachment = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = attachment.connection_id.unwrap();
        let _watch = service
            .watch("shared", attachment.attachment_id, connection_id)
            .unwrap();
        let other = service
            .attach("shared", "bob", AttachmentRole::Driver, None)
            .unwrap();
        let other_connection_id = other.connection_id.unwrap();
        let _other_watch = service
            .watch("shared", other.attachment_id, other_connection_id)
            .unwrap();
        let request = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "keep working".into(),
                },
            )
            .unwrap();
        let run = service
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                request.seq,
                attachment.attachment_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        let environment = service.store.environment().clone();
        let lease = service
            .acquire_runner("shared", "runner", &environment, None, 60_000)
            .unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        service.runners.register("shared", lease.lease_id, tx);

        let stale_receipt = crate::brain::store::BrainMutationReceipt {
            mutation_id: uuid::Uuid::new_v4(),
            attachment_id: attachment.attachment_id,
            expected_revision: 0,
            environment_generation: environment.generation,
            command_sha256: "cancel-run".into(),
        };
        assert!(service
            .cancel_run_with_receipt(
                "shared",
                attachment.attachment_id,
                connection_id,
                run.run_id,
                Some(stale_receipt),
            )
            .await
            .is_err());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), rx.recv(),)
                .await
                .is_err(),
            "stale cancellation reached the runner"
        );

        assert!(service
            .cancel_run(
                "shared",
                other.attachment_id,
                other_connection_id,
                run.run_id,
            )
            .await
            .is_err());
        tokio::spawn(async move {
            let crate::server::RunnerRequest::Cancel(request) = rx.recv().await.unwrap() else {
                panic!("expected cancellation request")
            };
            assert_eq!(request.run_id, run.run_id);
            request.response_tx.send(Ok(true)).unwrap();
        });
        let cancelled = service
            .cancel_run(
                "shared",
                attachment.attachment_id,
                connection_id,
                run.run_id,
            )
            .await
            .unwrap();
        assert_eq!(cancelled.status, BrainRunStatus::Cancelled);
    }

    #[tokio::test]
    async fn subagent_runs_are_idempotent_and_survive_store_restart() {
        let root = tempfile::tempdir().unwrap().keep();
        let make_service = || {
            BrainLifecycleService::new(
                BrainStore::with_root("box.local", Some(root.clone())),
                BrainRunnerBroker::default(),
                BrainApprovalBroker::default(),
            )
        };
        let service = make_service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let request = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "delegate this".into(),
                },
            )
            .unwrap();
        let parent = service
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                request.seq,
                driver.attachment_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        let task_id = uuid::Uuid::new_v4();
        let child = service
            .start_subagent_for_run(
                "shared",
                parent.run_id,
                task_id,
                Some("inspect scheduler".into()),
            )
            .unwrap();
        let retry = service
            .start_subagent_for_run(
                "shared",
                parent.run_id,
                task_id,
                Some("ignored retry detail".into()),
            )
            .unwrap();
        assert_eq!(retry, child);
        assert_eq!(child.run_id, RunId(task_id));
        assert_eq!(child.parent_run_id, Some(parent.run_id));

        let completed = service
            .transition_subagent_run(
                "shared",
                child.run_id,
                BrainRunStatus::Completed,
                Some("done".into()),
            )
            .unwrap();
        assert_eq!(completed.status, BrainRunStatus::Completed);
        service
            .store
            .transition_run(
                "shared",
                "runner",
                parent.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert_eq!(
            service
                .start_subagent_for_run(
                    "shared",
                    parent.run_id,
                    task_id,
                    Some("retry after parent completion".into()),
                )
                .unwrap(),
            completed
        );
        assert_eq!(
            service
                .transition_subagent_run(
                    "shared",
                    child.run_id,
                    BrainRunStatus::Completed,
                    Some("duplicate terminal publication".into()),
                )
                .unwrap(),
            completed
        );

        drop(service);
        let restarted = make_service();
        assert_eq!(
            restarted.inspect_run("shared", child.run_id).unwrap(),
            completed
        );
        assert!(restarted
            .transition_subagent_run(
                "shared",
                child.run_id,
                BrainRunStatus::Failed,
                Some("conflicting terminal status".into()),
            )
            .is_err());
    }

    #[tokio::test]
    async fn lifecycle_service_rejects_runner_as_participant() {
        let error = service()
            .attach("shared", "runner", AttachmentRole::Runner, None)
            .unwrap_err();
        assert!(error.to_string().contains("runner lease"));
    }

    #[tokio::test]
    async fn approval_is_independent_from_the_consultant_role() {
        let service = service();
        let consultant = service
            .attach("shared", "alice", AttachmentRole::Consultant, None)
            .unwrap();
        let connection_id = consultant.connection_id.unwrap();
        service
            .watch("shared", consultant.attachment_id, connection_id)
            .unwrap();
        let request_seq = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::ParticipantMessage {
                    text: "reviewing".into(),
                },
            )
            .unwrap()
            .seq;
        let snapshot = service.snapshot("shared").unwrap();
        let registration = service
            .approvals
            .register(
                request_seq,
                "approval-1",
                crate::brain::store::BrainApprovalAudience {
                    brain_id: snapshot.brain_id,
                    brain: snapshot.name,
                    attachment_id: consultant.attachment_id,
                    subject: consultant.subject.clone(),
                    role: consultant.role,
                    environment_generation: snapshot.environment.generation,
                },
            )
            .unwrap();
        let decision = BrainEventKind::ApprovalDecided {
            request_seq,
            approval_id: "approval-1".into(),
            decision: serde_json::json!({"choice": "approve_once"}),
        };

        assert!(matches!(
            service
                .submit(
                    "shared",
                    consultant.attachment_id,
                    connection_id,
                    decision.clone(),
                )
                .await,
            Err(BrainSubmissionError::Forbidden(_))
        ));
        service
            .submit_with_authority(
                "shared",
                consultant.attachment_id,
                connection_id,
                decision,
                true,
            )
            .await
            .unwrap();
        assert_eq!(registration.wait().await.unwrap()["choice"], "approve_once");
    }

    #[tokio::test]
    async fn schedule_lifecycle_requires_a_driver_and_preserves_the_grant_ceiling() {
        let service = service();
        let driver = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let driver_connection = driver.connection_id.unwrap();
        let _driver_watch = service
            .watch("shared", driver.attachment_id, driver_connection)
            .unwrap();
        let sibling = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let sibling_connection = sibling.connection_id.unwrap();
        let _sibling_watch = service
            .watch("shared", sibling.attachment_id, sibling_connection)
            .unwrap();
        let observer = service
            .attach("shared", "eve", AttachmentRole::Observer, None)
            .unwrap();
        let observer_connection = observer.connection_id.unwrap();
        let _observer_watch = service
            .watch("shared", observer.attachment_id, observer_connection)
            .unwrap();
        let grant_ceiling = crate::vm::EffectSet(std::collections::BTreeSet::from([
            crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::VmRead,
                selector: crate::vm::ResourceSelector::None,
            },
        ]));

        assert!(service
            .create_schedule(
                "shared",
                observer.attachment_id,
                observer_connection,
                ProgramLanguage::Lisp,
                "(say \"no\")".into(),
                grant_ceiling.clone(),
                100,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .is_err());

        let created = service
            .create_schedule(
                "shared",
                driver.attachment_id,
                driver_connection,
                ProgramLanguage::Lisp,
                "(say \"later\")".into(),
                grant_ceiling.clone(),
                100,
                Some(50),
                BrainScheduleDeliveryPolicy::BoundedCatchUp {
                    max_catch_up: 2,
                    expires_after_ms: 1_000,
                },
            )
            .unwrap();
        assert_eq!(created.created_by, "alice");
        assert_eq!(created.grant_ceiling, grant_ceiling);
        assert_eq!(
            service
                .inspect_schedule("shared", created.schedule_id)
                .unwrap(),
            Some(created.clone())
        );
        assert!(service
            .cancel_schedule(
                "shared",
                sibling.attachment_id,
                sibling_connection,
                created.schedule_id,
            )
            .unwrap_err()
            .to_string()
            .contains("only the schedule creator attachment"));
        assert!(service
            .cancel_schedule(
                "shared",
                driver.attachment_id,
                driver_connection,
                created.schedule_id,
            )
            .unwrap());
        assert!(
            !service
                .inspect_schedule("shared", created.schedule_id)
                .unwrap()
                .unwrap()
                .active
        );
    }

    #[tokio::test]
    async fn program_schedule_control_is_run_scoped_creator_bound_and_attenuated() {
        let service = service();
        let alice = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let alice_connection = alice.connection_id.unwrap();
        service
            .watch("shared", alice.attachment_id, alice_connection)
            .unwrap();
        let sibling = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let sibling_connection = sibling.connection_id.unwrap();
        service
            .watch("shared", sibling.attachment_id, sibling_connection)
            .unwrap();
        let request = service
            .store
            .push(
                "shared",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "(say \"now\")".into(),
                },
            )
            .unwrap();
        let run = service
            .start_run_with_parent(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                request.seq,
                alice.attachment_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        let maximum = crate::vm::EffectSet::from_requirement(crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::VmRead,
            selector: crate::vm::ResourceSelector::None,
        });
        let broader = maximum.union(&crate::vm::EffectSet::from_requirement(
            crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::NetworkConnect,
                selector: crate::vm::ResourceSelector::Network {
                    host: "example.com".into(),
                    ports: vec![443],
                },
            },
        ));
        assert!(service
            .create_schedule_for_run(
                "shared",
                run.run_id,
                request.seq,
                Some(&maximum),
                ProgramLanguage::Lisp,
                "(say \"too broad\")".into(),
                broader,
                100,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap_err()
            .to_string()
            .contains("broader authority"));

        let own = service
            .create_schedule_for_run(
                "shared",
                run.run_id,
                request.seq,
                Some(&maximum),
                ProgramLanguage::Lisp,
                "(say \"later\")".into(),
                maximum.clone(),
                100,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert_eq!(own.created_by, "alice");
        assert_eq!(own.initiating_attachment_id, alice.attachment_id);

        let foreign = service
            .create_schedule(
                "shared",
                sibling.attachment_id,
                sibling_connection,
                ProgramLanguage::Lisp,
                "(say \"sibling\")".into(),
                crate::vm::EffectSet::pure(),
                100,
                None,
                BrainScheduleDeliveryPolicy::Coalesce,
            )
            .unwrap();
        assert!(service
            .inspect_schedule_for_run("shared", run.run_id, request.seq, foreign.schedule_id)
            .unwrap()
            .is_none());
        assert!(service
            .cancel_schedule_for_run("shared", run.run_id, request.seq, foreign.schedule_id)
            .is_err());

        service
            .store
            .transition_run(
                "shared",
                "daemon",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert!(service
            .inspect_schedule_for_run("shared", run.run_id, request.seq, own.schedule_id)
            .is_err());
    }

    #[tokio::test]
    async fn lifecycle_service_transfers_runner_authority_only_to_the_addressed_frontend() {
        let service = service();
        let environment = service.store.environment().clone();
        let source = service
            .acquire_runner("shared", "runner-a", &environment, None, 60_000)
            .unwrap();
        let handoff = service
            .request_runner_handoff(
                "shared",
                "controller",
                "runner-b",
                source.lease_id,
                &environment,
                30_000,
            )
            .unwrap();
        assert!(service
            .accept_runner_handoff(
                "shared",
                "runner-c",
                handoff.handoff_id,
                &environment,
                60_000,
            )
            .is_err());
        let replacement = service
            .accept_runner_handoff(
                "shared",
                "runner-b",
                handoff.handoff_id,
                &environment,
                60_000,
            )
            .unwrap();
        assert_ne!(replacement.lease_id, source.lease_id);
        assert_eq!(
            service.snapshot("shared").unwrap().runner_lease,
            Some(replacement)
        );
    }

    #[tokio::test]
    async fn lifecycle_service_submits_into_the_same_watched_event_log() {
        let service = service();
        let attachment = service
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let connection_id = attachment.connection_id.unwrap();
        let mut watch = service
            .watch("shared", attachment.attachment_id, connection_id)
            .unwrap();

        let outcome = service
            .submit(
                "shared",
                attachment.attachment_id,
                connection_id,
                BrainEventKind::Prompt {
                    text: "inspect the queue".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(outcome.accepted.sender, "alice");
        assert_eq!(
            outcome.run.as_ref().unwrap().status,
            crate::brain::store::BrainRunStatus::QueuedForEnvironment
        );
        assert!(outcome.result.is_none());

        let projected = loop {
            let event = watch.events.recv().await.unwrap();
            if event.seq > watch.snapshot.revision
                && matches!(event.kind, BrainEventKind::Prompt { .. })
            {
                break event;
            }
        };
        assert_eq!(projected.seq, outcome.accepted.seq);
    }
}
