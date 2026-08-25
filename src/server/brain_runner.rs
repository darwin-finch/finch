//! Thread-safe dispatch boundary between daemon request handlers and the
//! frontend process that owns one named Brain's execution environment.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot};

use crate::brain::shared::{ProgramLanguage, RunId, RunnerLeaseId};

#[derive(Debug)]
pub enum RunnerRequest {
    Program(RunnerProgramRequest),
    Turn(RunnerTurnRequest),
}

#[derive(Debug)]
pub struct RunnerProgramRequest {
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub language: ProgramLanguage,
    pub source: String,
    pub response_tx: oneshot::Sender<Result<RunnerProgramResult, String>>,
}

#[derive(Debug, Clone)]
pub struct RunnerProgramResult {
    pub output: String,
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
}

#[derive(Debug)]
pub struct RunnerTurnRequest {
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub prompt: String,
    pub context: Vec<crate::claude::Message>,
    pub approval_audience: crate::brain::shared::BrainApprovalAudience,
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
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct RunnerTurnError {
    pub message: String,
    pub turn_events: Vec<RunnerTurnEvent>,
}

impl From<String> for RunnerTurnError {
    fn from(message: String) -> Self {
        Self {
            message,
            turn_events: Vec::new(),
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
        audience: crate::brain::shared::BrainApprovalAudience,
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
    tx: mpsc::UnboundedSender<RunnerRequest>,
}

/// Registrations contain only Tokio channels and portable values. Cap'n Proto
/// capabilities remain on their connection's LocalSet and are driven by a
/// local bridge task that owns the receiving side of the channel.
#[derive(Clone, Default)]
pub struct BrainRunnerBroker {
    registrations: Arc<RwLock<HashMap<String, Registration>>>,
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
            .insert(brain.into(), Registration { id, lease_id, tx });
        id
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
            .map_err(anyhow::Error::msg)
    }

    pub async fn dispatch_turn(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        context: Vec<crate::claude::Message>,
        approval_audience: crate::brain::shared::BrainApprovalAudience,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease() -> RunnerLeaseId {
        RunnerLeaseId(uuid::Uuid::new_v4())
    }

    fn test_approval_audience() -> crate::brain::shared::BrainApprovalAudience {
        crate::brain::shared::BrainApprovalAudience {
            brain_id: crate::brain::shared::BrainId(uuid::Uuid::new_v4()),
            brain: "brain".into(),
            attachment_id: crate::brain::shared::AttachmentId(uuid::Uuid::new_v4()),
            subject: "driver@box.local".into(),
            role: crate::brain::shared::AttachmentRole::Driver,
            environment_generation: 1,
        }
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
