//! Thread-safe dispatch boundary between daemon request handlers and the
//! frontend process that owns one named Brain's execution environment.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use crate::brain::store::{
    AttachmentId, ConnectionId, ProgramLanguage, RunId, RunnerLeaseId,
};

#[derive(Debug)]
pub enum RunnerRequest {
    Program(RunnerProgramRequest),
    Turn(RunnerTurnRequest),
    ProjectMemory(RunnerMemoryProjectionRequest),
    Cancel(RunnerCancelRequest),
}

#[derive(Debug)]
pub struct RunnerMemoryProjectionRequest {
    pub brain_id: crate::brain::store::BrainId,
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub prompt: String,
    pub source: String,
    pub response_tx: oneshot::Sender<Result<usize, String>>,
}

#[derive(Debug)]
pub struct RunnerCancelRequest {
    pub brain: String,
    pub run_id: RunId,
    pub response_tx: oneshot::Sender<Result<bool, String>>,
}

#[derive(Debug)]
pub struct RunnerProgramRequest {
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub language: ProgramLanguage,
    pub source: String,
    pub response_tx: oneshot::Sender<Result<RunnerProgramResult, RunnerProgramError>>,
}

#[derive(Debug, Clone)]
pub struct RunnerProgramResult {
    pub output: String,
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
    pub effect_journal: Vec<RunnerEffectRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunnerEffectRecord {
    pub execution_id: uuid::Uuid,
    pub entry: crate::vm::EffectJournalEntry,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RunnerProgramError {
    pub message: String,
    pub effect_journal: Vec<RunnerEffectRecord>,
}

impl From<String> for RunnerProgramError {
    fn from(message: String) -> Self {
        Self {
            message,
            effect_journal: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct RunnerTurnRequest {
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub prompt: String,
    pub context: Vec<crate::claude::Message>,
    pub approval_audience: crate::brain::store::BrainApprovalAudience,
    /// Reverse approval bridge installed by the Cap'n Proto client adapter.
    /// Daemon-side broker requests leave this unset until they cross IPC.
    pub approval_tx: Option<mpsc::UnboundedSender<RunnerApprovalRequest>>,
    pub response_tx: oneshot::Sender<Result<RunnerTurnResult, RunnerTurnError>>,
}

#[derive(Debug)]
pub struct RunnerApprovalRequest {
    pub event: RunnerTurnEvent,
    pub response_tx: oneshot::Sender<Result<serde_json::Value, String>>,
}

#[derive(Debug, Clone)]
pub struct RunnerTurnResult {
    pub source: String,
    pub language: ProgramLanguage,
    pub output: String,
    pub turn_events: Vec<RunnerTurnEvent>,
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
    pub effect_journal: Vec<RunnerEffectRecord>,
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RunnerTurnError {
    pub message: String,
    pub turn_events: Vec<RunnerTurnEvent>,
    pub effect_journal: Vec<RunnerEffectRecord>,
}

impl From<String> for RunnerTurnError {
    fn from(message: String) -> Self {
        Self {
            message,
            turn_events: Vec::new(),
            effect_journal: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunnerTurnEvent {
    Call {
        tool_id: String,
        name: String,
        input: serde_json::Value,
    },
    Result {
        tool_id: String,
        output: String,
        is_error: bool,
    },
    ApprovalRequested {
        approval_id: String,
        approval_kind: String,
        subject: String,
        audience: crate::brain::store::BrainApprovalAudience,
        detail: serde_json::Value,
    },
    ApprovalDecided {
        approval_id: String,
        decision: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerRegistrationId(uuid::Uuid);

#[derive(Clone)]
struct Registration {
    id: RunnerRegistrationId,
    lease_id: RunnerLeaseId,
    connection_id: Option<uuid::Uuid>,
    tx: mpsc::UnboundedSender<RunnerRequest>,
}

#[derive(Default)]
struct ConnectionAuthority {
    identities: HashMap<String, uuid::Uuid>,
    leases: HashMap<(String, RunnerLeaseId), uuid::Uuid>,
    attachments: HashMap<(String, AttachmentId, ConnectionId), uuid::Uuid>,
}

/// Registrations contain only Tokio channels and portable values. Cap'n Proto
/// capabilities remain on their connection's LocalSet and are driven by a
/// local bridge task that owns the receiving side of the channel.
#[derive(Clone, Default)]
pub struct BrainRunnerBroker {
    registrations: Arc<RwLock<HashMap<String, Registration>>>,
    connection_authority: Arc<Mutex<ConnectionAuthority>>,
}

impl BrainRunnerBroker {
    pub fn register(
        &self,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<RunnerRequest>,
    ) -> RunnerRegistrationId {
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        self.registrations
            .write()
            .expect("runner broker lock poisoned")
            .insert(
                brain.into(),
                Registration {
                    id,
                    lease_id,
                    connection_id: None,
                    tx,
                },
            );
        id
    }

    pub(crate) fn claim_connection_identity(
        &self,
        connection_id: uuid::Uuid,
        subject: &str,
    ) -> Result<()> {
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
            anyhow::bail!("runner subject must be 1-128 printable characters");
        }
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        match authority.identities.get(subject) {
            Some(owner) if *owner != connection_id => {
                anyhow::bail!("runner subject is already claimed by another IPC connection")
            }
            _ => {
                authority
                    .identities
                    .insert(subject.to_string(), connection_id);
                Ok(())
            }
        }
    }

    pub(crate) fn require_connection_identity(
        &self,
        connection_id: uuid::Uuid,
        subject: &str,
    ) -> Result<()> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        if authority.identities.get(subject) != Some(&connection_id) {
            anyhow::bail!("runner subject is not owned by this IPC connection");
        }
        Ok(())
    }

    pub(crate) fn claim_connection_lease(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        lease_id: RunnerLeaseId,
    ) -> Result<()> {
        let key = (brain.to_string(), lease_id);
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        match authority.leases.get(&key) {
            Some(owner) if *owner != connection_id => {
                anyhow::bail!("runner lease is owned by another IPC connection")
            }
            _ => {
                authority.leases.insert(key, connection_id);
                Ok(())
            }
        }
    }

    pub(crate) fn require_connection_lease(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        lease_id: RunnerLeaseId,
    ) -> Result<()> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        if authority.leases.get(&(brain.to_string(), lease_id)) != Some(&connection_id) {
            anyhow::bail!("runner lease is not owned by this IPC connection");
        }
        Ok(())
    }

    pub(crate) fn release_connection_lease(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        lease_id: RunnerLeaseId,
    ) {
        let key = (brain.to_string(), lease_id);
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        if authority.leases.get(&key) == Some(&connection_id) {
            authority.leases.remove(&key);
        }
    }

    pub(crate) fn claim_connection_attachment(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        attachment_id: AttachmentId,
        attachment_connection_id: ConnectionId,
    ) -> Result<()> {
        let key = (
            brain.to_string(),
            attachment_id,
            attachment_connection_id,
        );
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        match authority.attachments.get(&key) {
            Some(owner) if *owner != connection_id => {
                anyhow::bail!("Brain attachment is owned by another IPC connection")
            }
            _ => {
                authority.attachments.insert(key, connection_id);
                Ok(())
            }
        }
    }

    pub(crate) fn require_connection_attachment(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        attachment_id: AttachmentId,
        attachment_connection_id: ConnectionId,
    ) -> Result<()> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        if authority.attachments.get(&(
            brain.to_string(),
            attachment_id,
            attachment_connection_id,
        )) != Some(&connection_id)
        {
            anyhow::bail!("Brain attachment is not owned by this IPC connection");
        }
        Ok(())
    }

    pub(crate) fn release_connection_attachment(
        &self,
        connection_id: uuid::Uuid,
        brain: &str,
        attachment_id: AttachmentId,
        attachment_connection_id: ConnectionId,
    ) {
        let key = (
            brain.to_string(),
            attachment_id,
            attachment_connection_id,
        );
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        if authority.attachments.get(&key) == Some(&connection_id) {
            authority.attachments.remove(&key);
        }
    }

    pub(crate) fn register_for_connection(
        &self,
        connection_id: uuid::Uuid,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<RunnerRequest>,
    ) -> Result<RunnerRegistrationId> {
        let brain = brain.into();
        self.require_connection_lease(connection_id, &brain, lease_id)?;
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        self.registrations
            .write()
            .expect("runner broker lock poisoned")
            .insert(
                brain,
                Registration {
                    id,
                    lease_id,
                    connection_id: Some(connection_id),
                    tx,
                },
            );
        Ok(id)
    }

    pub(crate) fn disconnect_connection(&self, connection_id: uuid::Uuid) {
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        authority
            .identities
            .retain(|_, owner| *owner != connection_id);
        authority.leases.retain(|_, owner| *owner != connection_id);
        authority
            .attachments
            .retain(|_, owner| *owner != connection_id);
        drop(authority);
        self.registrations
            .write()
            .expect("runner broker lock poisoned")
            .retain(|_, registration| registration.connection_id != Some(connection_id));
    }

    /// Remove a registration only if it is still the connection that created
    /// it. A late disconnect must not remove a replacement runner callback.
    pub fn unregister(&self, brain: &str, id: RunnerRegistrationId) {
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        if registrations.get(brain).is_some_and(|entry| entry.id == id) {
            registrations.remove(brain);
        }
    }

    pub fn has_registration(&self, brain: &str, lease_id: RunnerLeaseId) -> bool {
        self.registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .is_some_and(|entry| entry.lease_id == lease_id && !entry.tx.is_closed())
    }

    pub async fn dispatch_program(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
        request_seq: u64,
        language: ProgramLanguage,
        source: String,
    ) -> Result<RunnerProgramResult> {
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .with_context(|| format!("named Brain '{brain}' has no connected runner callback"))?;
        if registration.lease_id != lease_id {
            anyhow::bail!("named Brain '{brain}' runner callback belongs to a stale lease");
        }
        let (response_tx, response_rx) = oneshot::channel();
        registration
            .tx
            .send(RunnerRequest::Program(RunnerProgramRequest {
                brain: brain.to_string(),
                run_id,
                request_seq,
                language,
                source,
                response_tx,
            }))
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner callback disconnected"))?;
        response_rx
            .await
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner dropped its response"))?
            .map_err(anyhow::Error::new)
    }

    pub async fn dispatch_turn(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        context: Vec<crate::claude::Message>,
        approval_audience: crate::brain::store::BrainApprovalAudience,
    ) -> Result<RunnerTurnResult> {
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .with_context(|| format!("named Brain '{brain}' has no connected runner callback"))?;
        if registration.lease_id != lease_id {
            anyhow::bail!("named Brain '{brain}' runner callback belongs to a stale lease");
        }
        let (response_tx, response_rx) = oneshot::channel();
        registration
            .tx
            .send(RunnerRequest::Turn(RunnerTurnRequest {
                brain: brain.to_string(),
                run_id,
                request_seq,
                prompt,
                context,
                approval_audience,
                approval_tx: None,
                response_tx,
            }))
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner callback disconnected"))?;
        response_rx
            .await
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner dropped its response"))?
            .map_err(anyhow::Error::new)
    }

    pub async fn cancel_run(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
    ) -> Result<bool> {
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .with_context(|| format!("named Brain '{brain}' has no connected runner callback"))?;
        if registration.lease_id != lease_id {
            anyhow::bail!("named Brain '{brain}' runner callback belongs to a stale lease");
        }
        let (response_tx, response_rx) = oneshot::channel();
        registration
            .tx
            .send(RunnerRequest::Cancel(RunnerCancelRequest {
                brain: brain.to_string(),
                run_id,
                response_tx,
            }))
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner callback disconnected"))?;
        response_rx
            .await
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner dropped cancel response"))?
            .map_err(anyhow::Error::msg)
    }

    /// Ask the exact leased environment runner to project one already
    /// committed Brain turn into its semantic-memory store. The daemon owns
    /// the trigger and source identity; the frontend remains the sole MemTree
    /// writer.
    pub async fn project_memory(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        brain_id: crate::brain::store::BrainId,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        source: String,
    ) -> Result<usize> {
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .with_context(|| format!("named Brain '{brain}' has no connected runner callback"))?;
        if registration.lease_id != lease_id {
            anyhow::bail!("named Brain '{brain}' runner callback belongs to a stale lease");
        }
        let (response_tx, response_rx) = oneshot::channel();
        registration
            .tx
            .send(RunnerRequest::ProjectMemory(RunnerMemoryProjectionRequest {
                brain_id,
                brain: brain.to_string(),
                run_id,
                request_seq,
                prompt,
                source,
                response_tx,
            }))
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner callback disconnected"))?;
        response_rx
            .await
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner dropped memory response"))?
            .map_err(anyhow::Error::msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> RunnerLeaseId {
        RunnerLeaseId(uuid::Uuid::new_v4())
    }

    fn test_approval_audience() -> crate::brain::store::BrainApprovalAudience {
        crate::brain::store::BrainApprovalAudience {
            brain_id: crate::brain::store::BrainId(uuid::Uuid::new_v4()),
            brain: "brain".into(),
            attachment_id: crate::brain::store::AttachmentId(uuid::Uuid::new_v4()),
            subject: "driver@box.local".into(),
            role: crate::brain::store::AttachmentRole::Driver,
            environment_generation: 1,
        }
    }

    #[test]
    fn runner_identity_lease_and_callback_are_connection_scoped() {
        let broker = BrainRunnerBroker::default();
        let owner = uuid::Uuid::new_v4();
        let intruder = uuid::Uuid::new_v4();
        let lease_id = lease();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let attachment_connection_id = ConnectionId(uuid::Uuid::new_v4());

        broker
            .claim_connection_identity(owner, "runner-a@box.local")
            .unwrap();
        assert!(broker
            .claim_connection_identity(intruder, "runner-a@box.local")
            .is_err());
        assert!(broker
            .require_connection_identity(intruder, "runner-a@box.local")
            .is_err());

        broker
            .claim_connection_lease(owner, "brain", lease_id)
            .unwrap();
        let (intruder_tx, _intruder_rx) = mpsc::unbounded_channel();
        assert!(broker
            .register_for_connection(intruder, "brain", lease_id, intruder_tx)
            .is_err());
        let (owner_tx, _owner_rx) = mpsc::unbounded_channel();
        broker
            .register_for_connection(owner, "brain", lease_id, owner_tx)
            .unwrap();
        assert!(broker.has_registration("brain", lease_id));

        broker
            .claim_connection_attachment(
                owner,
                "brain",
                attachment_id,
                attachment_connection_id,
            )
            .unwrap();
        assert!(broker
            .require_connection_attachment(
                intruder,
                "brain",
                attachment_id,
                attachment_connection_id,
            )
            .is_err());

        broker.disconnect_connection(owner);
        assert!(!broker.has_registration("brain", lease_id));
        assert!(broker
            .require_connection_lease(owner, "brain", lease_id)
            .is_err());
        broker
            .claim_connection_identity(intruder, "runner-a@box.local")
            .unwrap();
        assert!(broker
            .require_connection_attachment(
                owner,
                "brain",
                attachment_id,
                attachment_connection_id,
            )
            .is_err());
    }

    #[tokio::test]
    async fn dispatch_is_correlated_to_the_registered_lease() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        tokio::spawn(async move {
            let RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
                panic!("expected program request")
            };
            assert_eq!(request.request_seq, 7);
            assert_eq!(request.run_id, run_id);
            assert_eq!(request.source, "21 2 *");
            let runtime = crate::runtime::ProgramRuntime::new();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .pop()
                .unwrap()
                .checkpoint
                .unwrap();
            request
                .response_tx
                .send(Ok(RunnerProgramResult {
                    output: "42".into(),
                    runtime_revision: 1,
                    checkpoint,
                    effect_journal: Vec::new(),
                }))
                .unwrap();
        });

        let result = broker
            .dispatch_program(
                "brain",
                lease_id,
                run_id,
                7,
                ProgramLanguage::Forth,
                "21 2 *".into(),
            )
            .await
            .unwrap();
        assert_eq!(result.output, "42");
    }

    #[tokio::test]
    async fn stale_lease_cannot_use_a_replacement_callback() {
        let broker = BrainRunnerBroker::default();
        let current = lease();
        let stale = lease();
        let (tx, _rx) = mpsc::unbounded_channel();
        broker.register("brain", current, tx);

        let error = broker
            .dispatch_program(
                "brain",
                stale,
                RunId(uuid::Uuid::new_v4()),
                1,
                ProgramLanguage::Lisp,
                "(+ 1 1)".into(),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("stale lease"));
    }

    #[tokio::test]
    async fn memory_projection_is_correlated_to_the_registered_lease_and_run() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let brain_id = crate::brain::store::BrainId(uuid::Uuid::new_v4());
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        tokio::spawn(async move {
            let RunnerRequest::ProjectMemory(request) = rx.recv().await.unwrap() else {
                panic!("expected memory projection request")
            };
            assert_eq!(request.brain_id, brain_id);
            assert_eq!(request.run_id, run_id);
            assert_eq!(request.request_seq, 9);
            assert_eq!(request.prompt, "remember this");
            assert_eq!(request.source, "(say \"remembered\")");
            request.response_tx.send(Ok(2)).unwrap();
        });

        assert_eq!(
            broker
                .project_memory(
                    "brain",
                    lease_id,
                    brain_id,
                    run_id,
                    9,
                    "remember this".into(),
                    "(say \"remembered\")".into(),
                )
                .await
                .unwrap(),
            2
        );

        let replacement = lease();
        let (replacement_tx, _replacement_rx) = mpsc::unbounded_channel();
        broker.register("brain", replacement, replacement_tx);
        assert!(broker
            .project_memory(
                "brain",
                lease_id,
                brain_id,
                run_id,
                9,
                "remember this".into(),
                "(say \"remembered\")".into(),
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("stale lease"));
    }

    #[tokio::test]
    async fn full_turn_dispatch_carries_canonical_context() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        tokio::spawn(async move {
            let RunnerRequest::Turn(request) = rx.recv().await.unwrap() else {
                panic!("expected full turn request")
            };
            assert_eq!(request.prompt, "double it");
            assert_eq!(request.run_id, run_id);
            assert_eq!(request.context.len(), 1);
            assert_eq!(request.context[0].text(), "21");
            assert_eq!(request.approval_audience.brain, "brain");
            let runtime = crate::runtime::ProgramRuntime::new();
            let checkpoint = runtime
                .revision_history()
                .unwrap()
                .pop()
                .unwrap()
                .checkpoint
                .unwrap();
            request
                .response_tx
                .send(Ok(RunnerTurnResult {
                    source: "(say \"42\")".into(),
                    language: ProgramLanguage::Lisp,
                    output: "42".into(),
                    turn_events: Vec::new(),
                    runtime_revision: 1,
                    checkpoint,
                    effect_journal: Vec::new(),
                }))
                .unwrap();
        });

        let result = broker
            .dispatch_turn(
                "brain",
                lease_id,
                run_id,
                8,
                "double it".into(),
                vec![crate::claude::Message::user("21")],
                test_approval_audience(),
            )
            .await
            .unwrap();
        assert_eq!(result.source, "(say \"42\")");
        assert_eq!(result.output, "42");
    }

    #[tokio::test]
    async fn cancellation_targets_one_registered_run() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        tokio::spawn(async move {
            let RunnerRequest::Cancel(request) = rx.recv().await.unwrap() else {
                panic!("expected cancellation request")
            };
            assert_eq!(request.run_id, run_id);
            request.response_tx.send(Ok(true)).unwrap();
        });

        assert!(broker.cancel_run("brain", lease_id, run_id).await.unwrap());
    }

    #[test]
    fn late_unregister_does_not_remove_replacement() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let (first_tx, _first_rx) = mpsc::unbounded_channel();
        let first = broker.register("brain", lease_id, first_tx);
        let (second_tx, _second_rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, second_tx);

        broker.unregister("brain", first);
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test]
    async fn replacing_a_registration_closes_the_old_callback_bridge() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let (first_tx, mut first_rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, first_tx);
        let (second_tx, _second_rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, second_tx);

        assert!(first_rx.recv().await.is_none());
    }
}
