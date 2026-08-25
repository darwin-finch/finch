//! Daemon-owned rendezvous for Brain approval continuations.
//!
//! The environment runner may present an approval request, but it does not
//! choose who is allowed to answer it. The daemon binds each pending request
//! to the initiating attachment and resumes the runner only after that exact
//! attachment submits a decision.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::sync::oneshot;

use crate::brain::store::{AttachmentId, BrainApprovalAudience, BrainId};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApprovalKey {
    brain_id: BrainId,
    request_seq: u64,
    approval_id: String,
}

struct PendingApproval {
    request_seq: u64,
    audience: BrainApprovalAudience,
    response_tx: oneshot::Sender<Result<serde_json::Value, String>>,
}

#[derive(Clone, Default)]
pub struct BrainApprovalBroker {
    pending: Arc<Mutex<HashMap<ApprovalKey, PendingApproval>>>,
}

pub struct ApprovalRegistration {
    broker: BrainApprovalBroker,
    key: ApprovalKey,
    response_rx: Option<oneshot::Receiver<Result<serde_json::Value, String>>>,
}

pub struct ClaimedApproval {
    pub request_seq: u64,
    pub audience: BrainApprovalAudience,
    response_tx: Option<oneshot::Sender<Result<serde_json::Value, String>>>,
}

impl BrainApprovalBroker {
    pub fn register(
        &self,
        request_seq: u64,
        approval_id: impl Into<String>,
        audience: BrainApprovalAudience,
    ) -> Result<ApprovalRegistration> {
        let key = ApprovalKey {
            brain_id: audience.brain_id,
            request_seq,
            approval_id: approval_id.into(),
        };
        let (response_tx, response_rx) = oneshot::channel();
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        anyhow::ensure!(
            !pending.contains_key(&key),
            "approval '{}' is already pending for Brain {} request {}",
            key.approval_id,
            key.brain_id.0,
            key.request_seq
        );
        pending.insert(
            key.clone(),
            PendingApproval {
                request_seq,
                audience,
                response_tx,
            },
        );
        Ok(ApprovalRegistration {
            broker: self.clone(),
            key,
            response_rx: Some(response_rx),
        })
    }

    pub fn claim(
        &self,
        brain_id: BrainId,
        request_seq: u64,
        approval_id: &str,
        attachment_id: AttachmentId,
    ) -> Result<ClaimedApproval> {
        let key = ApprovalKey {
            brain_id,
            request_seq,
            approval_id: approval_id.to_string(),
        };
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending
            .get(&key)
            .with_context(|| {
                format!("approval '{approval_id}' is not pending for request {request_seq}")
            })?;
        anyhow::ensure!(
            request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience"
        );
        let request = pending
            .remove(&key)
            .expect("pending approval disappeared while locked");
        Ok(ClaimedApproval {
            request_seq: request.request_seq,
            audience: request.audience,
            response_tx: Some(request.response_tx),
        })
    }

    pub fn cancel_attachment(&self, brain_id: BrainId, attachment_id: AttachmentId) -> usize {
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let keys = pending
            .iter()
            .filter_map(|(key, request)| {
                (key.brain_id == brain_id && request.audience.attachment_id == attachment_id)
                    .then_some(key.clone())
            })
            .collect::<Vec<_>>();
        for key in &keys {
            if let Some(request) = pending.remove(key) {
                let _ = request
                    .response_tx
                    .send(Err("approval audience disconnected".into()));
            }
        }
        keys.len()
    }

    fn cancel_key(&self, key: &ApprovalKey, reason: &str) {
        if let Some(request) = self
            .pending
            .lock()
            .expect("approval broker lock poisoned")
            .remove(key)
        {
            let _ = request.response_tx.send(Err(reason.to_string()));
        }
    }
}

impl ApprovalRegistration {
    pub async fn wait(mut self) -> Result<serde_json::Value> {
        let response_rx = self
            .response_rx
            .take()
            .expect("approval registration receiver already consumed");
        let response = tokio::time::timeout(std::time::Duration::from_secs(15 * 60), response_rx)
            .await
            .context("approval request expired after 15 minutes")?
            .context("approval decision channel closed")?;
        self.broker
            .cancel_key(&self.key, "approval request completed");
        response.map_err(anyhow::Error::msg)
    }
}

impl Drop for ApprovalRegistration {
    fn drop(&mut self) {
        self.broker
            .cancel_key(&self.key, "approval requester disconnected");
    }
}

impl ClaimedApproval {
    pub fn complete(mut self, decision: serde_json::Value) {
        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(Ok(decision));
        }
    }

    pub fn fail(mut self, error: impl Into<String>) {
        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(Err(error.into()));
        }
    }
}

impl Drop for ClaimedApproval {
    fn drop(&mut self) {
        if let Some(response_tx) = self.response_tx.take() {
            let _ = response_tx.send(Err("approval decision was not committed".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brain::store::AttachmentRole;

    fn audience(attachment_id: AttachmentId) -> BrainApprovalAudience {
        BrainApprovalAudience {
            brain_id: BrainId(uuid::Uuid::new_v4()),
            brain: "shared".into(),
            attachment_id,
            subject: "alice@box.local".into(),
            role: AttachmentRole::Driver,
            environment_generation: 1,
        }
    }

    #[tokio::test]
    async fn only_the_addressed_attachment_can_resume_an_approval() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let registration = broker.register(7, "approval-1", audience.clone()).unwrap();
        assert!(broker
            .claim(
                audience.brain_id,
                7,
                "approval-1",
                AttachmentId(uuid::Uuid::new_v4()),
            )
            .is_err());
        let claimed = broker
            .claim(audience.brain_id, 7, "approval-1", attachment_id)
            .unwrap();
        assert_eq!(claimed.request_seq, 7);
        claimed.complete(serde_json::json!({"choice": "allow_once"}));
        assert_eq!(registration.wait().await.unwrap()["choice"], "allow_once");
    }

    #[tokio::test]
    async fn disconnect_cancels_every_approval_for_that_attachment() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let registration = broker.register(7, "approval-1", audience.clone()).unwrap();
        assert_eq!(
            broker.cancel_attachment(audience.brain_id, attachment_id),
            1
        );
        assert!(registration
            .wait()
            .await
            .unwrap_err()
            .to_string()
            .contains("disconnected"));
    }

    #[tokio::test]
    async fn cancelling_the_wait_drops_the_pending_continuation() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let registration = broker
            .register(7, "approval-1", audience.clone())
            .unwrap();
        let waiter = tokio::spawn(registration.wait());
        tokio::task::yield_now().await;
        waiter.abort();
        let _ = waiter.await;

        let error = match broker.claim(audience.brain_id, 7, "approval-1", attachment_id) {
            Ok(_) => panic!("cancelled approval remained pending"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("not pending"));
    }

    #[tokio::test]
    async fn stale_request_sequence_cannot_consume_a_pending_approval() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let registration = broker.register(7, "approval-1", audience.clone()).unwrap();

        let stale_error = match broker.claim(audience.brain_id, 6, "approval-1", attachment_id) {
            Ok(_) => panic!("stale sequence consumed the pending approval"),
            Err(error) => error,
        };
        assert!(stale_error.to_string().contains("request 6"));

        let claimed = broker
            .claim(audience.brain_id, 7, "approval-1", attachment_id)
            .unwrap();
        claimed.complete(serde_json::json!({"choice": "allow_once"}));
        assert_eq!(registration.wait().await.unwrap()["choice"], "allow_once");
    }

    #[tokio::test]
    async fn another_brain_cannot_consume_a_pending_approval() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let registration = broker.register(7, "approval-1", audience.clone()).unwrap();

        let error = match broker.claim(
            BrainId(uuid::Uuid::new_v4()),
            7,
            "approval-1",
            attachment_id,
        ) {
            Ok(_) => panic!("another Brain consumed the pending approval"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("not pending"));

        let claimed = broker
            .claim(audience.brain_id, 7, "approval-1", attachment_id)
            .unwrap();
        claimed.complete(serde_json::json!({"choice": "allow_once"}));
        assert_eq!(registration.wait().await.unwrap()["choice"], "allow_once");
    }
}
