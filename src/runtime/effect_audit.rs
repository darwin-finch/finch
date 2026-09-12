//! The host-effect audit protocol a runtime execution speaks.
//!
//! A typed program that performs a host effect must have that effect reserved, begun, and
//! finished durably before and after it happens. These are the handles the runtime holds while
//! doing so; the host owns the other end of each channel and decides what durability means.
//!
//! They live here rather than with the daemon that services them because they name nothing but
//! `crate::vm` — putting them one layer up is what made the runtime depend on the server.

use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, PartialEq)]
pub enum RunnerHostEffectOutcome {
    Acknowledged { values: Vec<crate::vm::TypedValue> },
    NotApplied { reason: String },
    FailedPartial { detail: String },
}

#[derive(Debug)]
pub(crate) enum RunnerEffectAuditControlRequest {
    Reserve {
        execution_id: uuid::Uuid,
        effect: crate::vm::VmSideEffect,
        response_tx: oneshot::Sender<Result<RunnerEffectAuditReservation, String>>,
    },
}

/// Send-safe proxy for the daemon-owned run-scoped effect audit capability.
/// It contains no authority provenance and cannot be constructed by external
/// callers.
#[derive(Debug, Clone)]
pub struct RunnerEffectAuditControl {
    tx: mpsc::UnboundedSender<RunnerEffectAuditControlRequest>,
}

impl RunnerEffectAuditControl {
    pub(crate) fn new(tx: mpsc::UnboundedSender<RunnerEffectAuditControlRequest>) -> Self {
        Self { tx }
    }

    pub async fn reserve(
        &self,
        execution_id: uuid::Uuid,
        effect: crate::vm::VmSideEffect,
    ) -> Result<RunnerEffectAuditReservation, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RunnerEffectAuditControlRequest::Reserve {
                execution_id,
                effect,
                response_tx,
            })
            .map_err(|_| "effect audit control disconnected".to_string())?;
        response_rx
            .await
            .map_err(|_| "effect audit reservation response disconnected".to_string())?
    }
}

#[derive(Debug)]
pub(crate) enum RunnerEffectAuditReservationRequest {
    Begin {
        response_tx: oneshot::Sender<Result<RunnerHostEffectPermit, String>>,
    },
    NotApplied {
        reason: String,
        response_tx: oneshot::Sender<Result<(), String>>,
    },
}

/// One accepted intent. Consuming this value either durably begins the host
/// effect and returns its permit, or records that no physical effect occurred.
#[derive(Debug)]
pub struct RunnerEffectAuditReservation {
    tx: mpsc::UnboundedSender<RunnerEffectAuditReservationRequest>,
}

impl RunnerEffectAuditReservation {
    pub(crate) fn new(tx: mpsc::UnboundedSender<RunnerEffectAuditReservationRequest>) -> Self {
        Self { tx }
    }

    pub async fn begin(self) -> Result<RunnerHostEffectPermit, String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RunnerEffectAuditReservationRequest::Begin { response_tx })
            .map_err(|_| "effect audit reservation disconnected".to_string())?;
        response_rx
            .await
            .map_err(|_| "effect audit begin response disconnected".to_string())?
    }

    pub async fn not_applied(self, reason: impl Into<String>) -> Result<(), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RunnerEffectAuditReservationRequest::NotApplied {
                reason: reason.into(),
                response_tx,
            })
            .map_err(|_| "effect audit reservation disconnected".to_string())?;
        response_rx
            .await
            .map_err(|_| "effect audit terminal response disconnected".to_string())?
    }
}

#[derive(Debug)]
pub(crate) struct RunnerHostEffectFinishRequest {
    pub outcome: RunnerHostEffectOutcome,
    pub response_tx: oneshot::Sender<Result<(), String>>,
}

/// Opaque proof that the daemon fsynced `AwaitingHostResult`. This value is
/// neither serializable nor cloneable; the host binding consumes it when
/// recording the physical outcome.
#[derive(Debug)]
pub struct RunnerHostEffectPermit {
    tx: mpsc::UnboundedSender<RunnerHostEffectFinishRequest>,
}

impl RunnerHostEffectPermit {
    pub(crate) fn new(tx: mpsc::UnboundedSender<RunnerHostEffectFinishRequest>) -> Self {
        Self { tx }
    }

    pub async fn finish(self, outcome: RunnerHostEffectOutcome) -> Result<(), String> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(RunnerHostEffectFinishRequest {
                outcome,
                response_tx,
            })
            .map_err(|_| "host effect permit disconnected".to_string())?;
        response_rx
            .await
            .map_err(|_| "host effect finish response disconnected".to_string())?
    }
}
