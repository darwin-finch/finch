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
            .filter(|schedule| schedule.created_by == run.initiated_by))
    }

    pub fn cancel_schedule_for_run(
        &self,
        brain: &str,
        run_id: RunId,
        request_seq: u64,
        schedule_id: ScheduleId,
    ) -> Result<bool> {
        let run = self.schedule_principal_for_run(brain, run_id, request_seq)?;
        self.store
            .cancel_schedule(brain, &run.initiated_by, schedule_id)
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
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can create a schedule"
        );
        self.store.create_schedule(
            brain,
            &attachment.subject,
            attachment_id,
            language,
            source,
            grant_ceiling,
            next_due_ms,
            interval_ms,
            delivery_policy,
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
        self.store
            .cancel_schedule(brain, &attachment.subject, schedule_id)
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
        let brain_id = self.store.snapshot(brain)?.brain_id;
        self.store.detach(brain, attachment_id, connection_id)?;
        self.approvals.cancel_attachment(brain_id, attachment_id);
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
        let attachment = self
            .connection(brain, attachment_id, connection_id)
            .map_err(BrainSubmissionError::State)?;
        let can_approve = crate::brain::credential::default_participant_scopes(attachment.role)
            .contains(&crate::brain::credential::BrainCredentialScope::BrainApprove);
        self.submit_for_attachment(brain, &attachment, kind, can_approve)
            .await
    }

    pub(crate) async fn submit_with_authority(
        &self,
        brain: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        kind: BrainEventKind,
        can_approve: bool,
    ) -> Result<BrainSubmissionOutcome, BrainSubmissionError> {
        let attachment = self
            .connection(brain, attachment_id, connection_id)
            .map_err(BrainSubmissionError::State)?;
        self.submit_for_attachment(brain, &attachment, kind, can_approve)
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
        let attachment = self.connection(brain, attachment_id, connection_id)?;
        ensure!(
            attachment.role == AttachmentRole::Driver,
            "only a Brain driver can cancel a run"
        );
        let run = self.inspect_run(brain, run_id)?;
        ensure!(
            run.initiating_attachment_id == attachment_id,
            "a Brain run can only be cancelled by its initiating attachment"
        );
        if run.status == BrainRunStatus::Cancelled {
            return Ok(run);
        }
        ensure!(!run.status.is_terminal(), "Brain run has already finished");
        if matches!(
            run.status,
            BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
        ) {
            let snapshot = self.snapshot(brain)?;
            let lease = snapshot
                .runner_lease
                .filter(|lease| lease.expires_ms > crate::brain::store::unix_millis())
                .ok_or_else(|| anyhow::anyhow!("named Brain '{brain}' has no live runner"))?;
            ensure!(
                self.runners.cancel_run(brain, lease.lease_id, run_id).await?,
                "the environment runner is not executing this Brain run"
            );
        }
        match self.store.transition_run(
            brain,
            &attachment.subject,
            run_id,
            BrainRunStatus::Cancelled,
            Some("cancelled by initiating driver".into()),
        ) {
            Ok(run) => Ok(run),
            Err(error) => {
                let current = self.inspect_run(brain, run_id)?;
                if current.status == BrainRunStatus::Cancelled {
                    Ok(current)
                } else {
                    Err(error)
                }
            }
        }
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
        ensure!(
            environment == self.store.environment(),
            "runner handoff environment does not match the daemon Brain environment"
        );
        let handoff = self.store.request_runner_handoff(
            brain,
            requested_by,
            target_subject,
            expected_lease_id,
            environment.generation,
            ttl_ms,
        )?;
        self.expire_runner_handoff(
            brain.to_owned(),
            handoff.handoff_id,
            handoff.expires_ms,
        );
        Ok(handoff)
    }

    fn expire_runner_handoff(
        &self,
        brain: String,
        handoff_id: RunnerHandoffId,
        expires_ms: u64,
    ) {
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
        assert_eq!(
            registration.wait().await.unwrap()["choice"],
            "approve_once"
        );
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
                driver.attachment_id,
                driver_connection,
                created.schedule_id,
            )
            .unwrap());
        assert!(!service
            .inspect_schedule("shared", created.schedule_id)
            .unwrap()
            .unwrap()
            .active);
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
        let bob = service
            .attach("shared", "bob", AttachmentRole::Driver, None)
            .unwrap();
        let bob_connection = bob.connection_id.unwrap();
        service
            .watch("shared", bob.attachment_id, bob_connection)
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
        let maximum = crate::vm::EffectSet::from_requirement(
            crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::VmRead,
                selector: crate::vm::ResourceSelector::None,
            },
        );
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
                bob.attachment_id,
                bob_connection,
                ProgramLanguage::Lisp,
                "(say \"bob\")".into(),
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
