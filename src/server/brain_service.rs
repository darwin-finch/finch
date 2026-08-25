//! Transport-independent lifecycle operations for named Brains.
//!
//! HTTP and Cap'n Proto adapters authenticate and encode requests, but they
//! must not independently implement attachment, watch, submission, or runner
//! lease semantics.  Embedded hosts can use this service directly.

use anyhow::{ensure, Result};
use tokio::sync::broadcast;

use crate::brain::shared::{
    AttachmentId, AttachmentRole, BrainAttachment, BrainEnvironment, BrainEvent, BrainEventKind,
    BrainRunnerLease, BrainSnapshot, ConnectionId, RunnerLeaseId, SharedBrainStore,
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
    pub run: Option<crate::brain::shared::BrainRun>,
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
    store: SharedBrainStore,
    runners: BrainRunnerBroker,
    approvals: BrainApprovalBroker,
}

impl BrainLifecycleService {
    pub fn from_server(server: &AgentServer) -> Self {
        Self {
            store: server.shared_brains().clone(),
            runners: server.brain_runners().clone(),
            approvals: server.brain_approvals().clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn new(
        store: SharedBrainStore,
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
        handlers::submit_named_brain_event(
            &self.store,
            &self.runners,
            &self.approvals,
            brain,
            &attachment,
            kind,
        )
        .await
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
                let delay_ms = expires_ms.saturating_sub(crate::brain::shared::unix_millis());
                if delay_ms == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            if store
                .expire_runner_lease(&brain, lease_id, crate::brain::shared::unix_millis())
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
            SharedBrainStore::with_root("box.local", Some(root)),
            BrainRunnerBroker::default(),
            BrainApprovalBroker::default(),
        )
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
    async fn lifecycle_service_rejects_runner_as_participant() {
        let error = service()
            .attach("shared", "runner", AttachmentRole::Runner, None)
            .unwrap_err();
        assert!(error.to_string().contains("runner lease"));
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
            crate::brain::shared::BrainRunStatus::QueuedForEnvironment
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
