//! Daemon-owned rendezvous for Brain approval continuations.
//!
//! The environment runner may present an approval request, but it does not
//! choose who is allowed to answer it. The daemon binds each pending request
//! to the initiating attachment and resumes the runner only after that exact
//! attachment submits a decision.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};
use tokio::sync::{oneshot, Mutex as AsyncMutex};

use crate::brain::store::{AttachmentId, BrainApprovalAudience, BrainId, ConnectionId};

const APPROVAL_WINDOW: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Copy)]
pub(crate) struct ApprovalDeadline {
    pub expires_ms: u64,
    instant: tokio::time::Instant,
}

impl ApprovalDeadline {
    fn full_window() -> Self {
        Self {
            expires_ms: crate::brain::store::unix_millis()
                .saturating_add(u64::try_from(APPROVAL_WINDOW.as_millis()).unwrap_or(u64::MAX)),
            instant: tokio::time::Instant::now() + APPROVAL_WINDOW,
        }
    }

    pub(crate) fn clamped_to_turn(
        hard_deadline: tokio::time::Instant,
        hard_deadline_ms: u64,
    ) -> Result<Self> {
        let now = tokio::time::Instant::now();
        let now_ms = crate::brain::store::unix_millis();
        let monotonic_remaining = hard_deadline
            .checked_duration_since(now)
            .unwrap_or_default();
        let wall_remaining_ms = hard_deadline_ms.saturating_sub(now_ms);
        let remaining_ms = u64::try_from(monotonic_remaining.as_millis())
            .unwrap_or(u64::MAX)
            .min(wall_remaining_ms)
            .min(u64::try_from(APPROVAL_WINDOW.as_millis()).unwrap_or(u64::MAX));
        anyhow::ensure!(
            remaining_ms > 0,
            "named Brain turn has no remaining approval budget"
        );
        Ok(Self {
            expires_ms: now_ms.saturating_add(remaining_ms),
            instant: now + std::time::Duration::from_millis(remaining_ms),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApprovalKey {
    brain_id: BrainId,
    request_seq: u64,
    approval_id: String,
}

struct PendingApproval {
    request_seq: u64,
    audience: BrainApprovalAudience,
    connection_id: Option<ConnectionId>,
    response_tx: Option<oneshot::Sender<Result<serde_json::Value, String>>>,
    delivered_decision: Option<serde_json::Value>,
}

#[derive(Clone, Default)]
pub struct BrainApprovalBroker {
    pending: Arc<Mutex<HashMap<ApprovalKey, PendingApproval>>>,
    mutation_locks: Arc<Mutex<HashMap<ApprovalKey, Weak<AsyncMutex<()>>>>>,
}

pub struct ApprovalRegistration {
    broker: BrainApprovalBroker,
    key: ApprovalKey,
    deadline: ApprovalDeadline,
    response_rx: Option<oneshot::Receiver<Result<serde_json::Value, String>>>,
}

pub struct ClaimedApproval {
    pub request_seq: u64,
    pub audience: BrainApprovalAudience,
    response_tx: Option<oneshot::Sender<Result<serde_json::Value, String>>>,
}

impl BrainApprovalBroker {
    /// Serialize durable decisions for one approval without taking the Brain's
    /// turn lane. The originating turn intentionally holds that lane while its
    /// runner waits for this decision.
    pub fn mutation_lock(
        &self,
        brain_id: BrainId,
        request_seq: u64,
        approval_id: &str,
    ) -> Arc<AsyncMutex<()>> {
        let key = ApprovalKey {
            brain_id,
            request_seq,
            approval_id: approval_id.to_string(),
        };
        let mut locks = self
            .mutation_locks
            .lock()
            .expect("approval mutation lock map poisoned");
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    pub fn register(
        &self,
        request_seq: u64,
        approval_id: impl Into<String>,
        audience: BrainApprovalAudience,
    ) -> Result<ApprovalRegistration> {
        self.register_inner(
            request_seq,
            approval_id,
            audience,
            None,
            ApprovalDeadline::full_window(),
        )
    }

    pub fn register_for_connection(
        &self,
        request_seq: u64,
        approval_id: impl Into<String>,
        audience: BrainApprovalAudience,
        connection_id: ConnectionId,
    ) -> Result<ApprovalRegistration> {
        self.register_inner(
            request_seq,
            approval_id,
            audience,
            Some(connection_id),
            ApprovalDeadline::full_window(),
        )
    }

    pub(crate) fn register_for_connection_with_authority<T>(
        &self,
        request_seq: u64,
        approval_id: impl Into<String>,
        audience: BrainApprovalAudience,
        connection_id: ConnectionId,
        deadline: ApprovalDeadline,
        authorize: impl FnOnce() -> Result<T>,
    ) -> Result<(ApprovalRegistration, T)> {
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
        // The authority check and durable approval publication run while the
        // same lock used by cancel_connection is held. Detach first revokes
        // the canonical connection and then takes this lock, so registration
        // is ordered wholly before its cancellation or fails wholly after it.
        let authorized = authorize()?;
        pending.insert(
            key.clone(),
            PendingApproval {
                request_seq,
                audience,
                connection_id: Some(connection_id),
                response_tx: Some(response_tx),
                delivered_decision: None,
            },
        );
        Ok((
            ApprovalRegistration {
                broker: self.clone(),
                key,
                deadline,
                response_rx: Some(response_rx),
            },
            authorized,
        ))
    }

    fn register_inner(
        &self,
        request_seq: u64,
        approval_id: impl Into<String>,
        audience: BrainApprovalAudience,
        connection_id: Option<ConnectionId>,
        deadline: ApprovalDeadline,
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
                connection_id,
                response_tx: Some(response_tx),
                delivered_decision: None,
            },
        );
        Ok(ApprovalRegistration {
            broker: self.clone(),
            key,
            deadline,
            response_rx: Some(response_rx),
        })
    }

    pub fn inspect_connection(
        &self, brain_id: BrainId, request_seq: u64, approval_id: &str,
        attachment_id: AttachmentId, connection_id: ConnectionId,
    ) -> Result<BrainApprovalAudience> {
        let key = ApprovalKey { brain_id, request_seq, approval_id: approval_id.to_string() };
        let pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending.get(&key).with_context(|| {
            format!("approval '{approval_id}' is not pending for request {request_seq}")
        })?;
        anyhow::ensure!(request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience");
        anyhow::ensure!(request.connection_id.is_none()
            || request.connection_id == Some(connection_id),
            "connection is not the approval audience generation");
        Ok(request.audience.clone())
    }

    pub fn deliver_connection(
        &self, brain_id: BrainId, request_seq: u64, approval_id: &str,
        attachment_id: AttachmentId, connection_id: ConnectionId,
        decision: serde_json::Value,
    ) -> Result<()> {
        let key = ApprovalKey { brain_id, request_seq, approval_id: approval_id.to_string() };
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending.get_mut(&key).with_context(|| {
            format!("approval '{approval_id}' is not pending for request {request_seq}")
        })?;
        anyhow::ensure!(request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience");
        anyhow::ensure!(request.connection_id.is_none()
            || request.connection_id == Some(connection_id),
            "connection is not the approval audience generation");
        if let Some(delivered) = &request.delivered_decision {
            anyhow::ensure!(delivered == &decision,
                "approval was already delivered with a different decision");
            return Ok(());
        }
        let response_tx = request.response_tx.take()
            .context("approval continuation is no longer deliverable")?;
        response_tx.send(Ok(decision.clone()))
            .map_err(|_| anyhow::anyhow!("approval continuation closed before delivery"))?;
        request.delivered_decision = Some(decision);
        Ok(())
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
            response_tx: request.response_tx,
        })
    }

    pub fn claim_connection(
        &self, brain_id: BrainId, request_seq: u64, approval_id: &str,
        attachment_id: AttachmentId, connection_id: ConnectionId,
    ) -> Result<ClaimedApproval> {
        let key = ApprovalKey { brain_id, request_seq, approval_id: approval_id.to_string() };
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending.get(&key).with_context(|| {
            format!("approval '{approval_id}' is not pending for request {request_seq}")
        })?;
        anyhow::ensure!(request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience");
        anyhow::ensure!(request.connection_id.is_none()
            || request.connection_id == Some(connection_id),
            "connection is not the approval audience generation");
        let request = pending.remove(&key).expect("pending approval disappeared while locked");
        Ok(ClaimedApproval { request_seq: request.request_seq,
            audience: request.audience, response_tx: request.response_tx })
    }

    pub fn inspect(
        &self, brain_id: BrainId, request_seq: u64, approval_id: &str,
        attachment_id: AttachmentId,
    ) -> Result<BrainApprovalAudience> {
        let key = ApprovalKey { brain_id, request_seq, approval_id: approval_id.to_string() };
        let pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending.get(&key).with_context(|| {
            format!("approval '{approval_id}' is not pending for request {request_seq}")
        })?;
        anyhow::ensure!(request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience");
        Ok(request.audience.clone())
    }

    pub fn deliver(
        &self, brain_id: BrainId, request_seq: u64, approval_id: &str,
        attachment_id: AttachmentId, decision: serde_json::Value,
    ) -> Result<()> {
        let key = ApprovalKey { brain_id, request_seq, approval_id: approval_id.to_string() };
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let request = pending.get_mut(&key).with_context(|| {
            format!("approval '{approval_id}' is not pending for request {request_seq}")
        })?;
        anyhow::ensure!(request.audience.attachment_id == attachment_id,
            "attachment is not the approval audience");
        if let Some(delivered) = &request.delivered_decision {
            anyhow::ensure!(delivered == &decision,
                "approval was already delivered with a different decision");
            return Ok(());
        }
        let response_tx = request.response_tx.take()
            .context("approval continuation is no longer deliverable")?;
        response_tx.send(Ok(decision.clone()))
            .map_err(|_| anyhow::anyhow!("approval continuation closed before delivery"))?;
        request.delivered_decision = Some(decision);
        Ok(())
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
                if let Some(response_tx) = request.response_tx {
                    let _ = response_tx.send(Err("approval audience disconnected".into()));
                }
            }
        }
        keys.len()
    }

    pub fn cancel_connection(
        &self, brain_id: BrainId, attachment_id: AttachmentId, connection_id: ConnectionId,
    ) -> usize {
        let mut pending = self.pending.lock().expect("approval broker lock poisoned");
        let keys = pending.iter().filter_map(|(key, request)| {
            (key.brain_id == brain_id && request.audience.attachment_id == attachment_id
                && request.connection_id == Some(connection_id)).then_some(key.clone())
        }).collect::<Vec<_>>();
        for key in &keys {
            if let Some(request) = pending.remove(key) {
                if let Some(response_tx) = request.response_tx {
                    let _ = response_tx.send(Err("approval audience disconnected".into()));
                }
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
            if let Some(response_tx) = request.response_tx {
                let _ = response_tx.send(Err(reason.to_string()));
            }
        }
    }
}

impl ApprovalRegistration {
    pub async fn wait(mut self) -> Result<serde_json::Value> {
        let response_rx = self
            .response_rx
            .take()
            .expect("approval registration receiver already consumed");
        let response = tokio::time::timeout_at(self.deadline.instant, response_rx)
            .await
            .with_context(|| {
                format!("approval request expired at {} ms", self.deadline.expires_ms)
            })?
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

    #[tokio::test(start_paused = true)]
    async fn clamped_expiry_is_the_enforced_boundary_and_rejects_late_approval() {
        let broker = BrainApprovalBroker::default();
        let attachment_id = AttachmentId(uuid::Uuid::new_v4());
        let audience = audience(attachment_id);
        let hard_window = std::time::Duration::from_millis(100);
        let registered_ms = crate::brain::store::unix_millis();
        let hard_deadline_ms = registered_ms.saturating_add(100);
        let deadline = ApprovalDeadline::clamped_to_turn(
            tokio::time::Instant::now() + hard_window,
            hard_deadline_ms,
        )
        .unwrap();
        let published_window_ms = deadline.expires_ms.saturating_sub(registered_ms);
        assert!((1..=100).contains(&published_window_ms));
        let registration = broker
            .register_inner(
                7,
                "approval-1",
                audience.clone(),
                None,
                deadline,
            )
            .unwrap();
        let waiter = tokio::spawn(registration.wait());

        tokio::time::advance(std::time::Duration::from_millis(published_window_ms - 1)).await;
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        tokio::time::advance(std::time::Duration::from_millis(1)).await;
        let error = waiter.await.unwrap().unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&deadline.expires_ms.to_string()),
            "{error:#}"
        );

        let late = broker.claim(audience.brain_id, 7, "approval-1", attachment_id);
        let late = match late {
            Ok(_) => panic!("late approval resumed an expired continuation"),
            Err(error) => error,
        };
        assert!(late.to_string().contains("not pending"));

        let exhausted = ApprovalDeadline::clamped_to_turn(
            tokio::time::Instant::now(),
            crate::brain::store::unix_millis(),
        )
        .unwrap_err();
        assert!(exhausted.to_string().contains("no remaining approval budget"));
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
