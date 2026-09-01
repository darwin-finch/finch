//! Thread-safe dispatch boundary between daemon request handlers and the
//! frontend process that owns one named Brain's execution environment.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::brain::store::{AttachmentId, ConnectionId, ProgramLanguage, RunId, RunnerLeaseId};

#[derive(Debug)]
pub enum RunnerRequest {
    Program(RunnerProgramRequest),
    Turn(RunnerTurnRequest),
    ProjectMemory(RunnerMemoryProjectionRequest),
    Cancel(RunnerCancelRequest),
}

/// Process-local callback lifecycle used by the daemon's IPC bridge. Public
/// request payloads remain source-compatible while the broker can still
/// cancel and bound one exact admitted callback generation.
#[derive(Debug)]
pub(crate) struct BoundedRunnerRequest {
    pub(crate) request: RunnerRequest,
    pub(crate) cancel: CancellationToken,
    pub(crate) generation_cancel: CancellationToken,
    pub(crate) deadline: tokio::time::Instant,
    pub(crate) cleanup_timeout: Duration,
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

impl RunnerMemoryProjectionRequest {
    /// Construct a memory callback request.
    pub fn new(
        brain_id: crate::brain::store::BrainId,
        brain: String,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        source: String,
        response_tx: oneshot::Sender<Result<usize, String>>,
    ) -> Self {
        Self {
            brain_id,
            brain,
            run_id,
            request_seq,
            prompt,
            source,
            response_tx,
        }
    }
}

#[derive(Debug)]
pub struct RunnerCancelRequest {
    pub brain: String,
    pub run_id: RunId,
    pub response_tx: oneshot::Sender<Result<bool, String>>,
}

impl RunnerCancelRequest {
    /// Construct a cancellation callback request.
    pub fn new(
        brain: String,
        run_id: RunId,
        response_tx: oneshot::Sender<Result<bool, String>>,
    ) -> Self {
        Self {
            brain,
            run_id,
            response_tx,
        }
    }
}

#[derive(Debug)]
pub struct RunnerProgramRequest {
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub language: ProgramLanguage,
    pub source: String,
    pub interaction: RunnerProgramInteraction,
    pub grant_ceiling: Option<crate::vm::EffectSet>,
    /// Send-safe proxy for the run-scoped daemon capability installed only
    /// after this request crosses the runner IPC boundary.
    pub control_tx: Option<mpsc::UnboundedSender<RunnerProgramControlRequest>>,
    /// Run-scoped write-ahead audit capability. Host bindings must reserve
    /// and begin through this proxy before applying a physical effect.
    pub effect_audit: Option<RunnerEffectAuditControl>,
    pub response_tx: oneshot::Sender<Result<RunnerProgramResult, RunnerProgramError>>,
}

impl RunnerProgramRequest {
    /// Construct a program callback using safe defaults for optional control capabilities.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        brain: String,
        run_id: RunId,
        request_seq: u64,
        language: ProgramLanguage,
        source: String,
        interaction: RunnerProgramInteraction,
        grant_ceiling: Option<crate::vm::EffectSet>,
        response_tx: oneshot::Sender<Result<RunnerProgramResult, RunnerProgramError>>,
    ) -> Self {
        Self {
            brain,
            run_id,
            request_seq,
            language,
            source,
            interaction,
            grant_ceiling,
            control_tx: None,
            effect_audit: None,
            response_tx,
        }
    }
}

#[derive(Debug)]
pub enum RunnerProgramControlRequest {
    CreateSchedule {
        language: ProgramLanguage,
        source: String,
        grant_ceiling: crate::vm::EffectSet,
        next_due_ms: u64,
        interval_ms: Option<u64>,
        delivery_policy: crate::brain::store::BrainScheduleDeliveryPolicy,
        response_tx: oneshot::Sender<Result<crate::brain::store::BrainSchedule, String>>,
    },
    InspectSchedule {
        schedule_id: crate::brain::store::ScheduleId,
        response_tx: oneshot::Sender<Result<Option<crate::brain::store::BrainSchedule>, String>>,
    },
    CancelSchedule {
        schedule_id: crate::brain::store::ScheduleId,
        response_tx: oneshot::Sender<Result<bool, String>>,
    },
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerProgramInteraction {
    Interactive,
    Noninteractive,
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
    pub approval_connection_id: Option<crate::brain::store::ConnectionId>,
    /// Reverse approval bridge installed by the Cap'n Proto client adapter.
    /// Daemon-side broker requests leave this unset until they cross IPC.
    pub approval_tx: Option<mpsc::UnboundedSender<RunnerApprovalRequest>>,
    /// Run-scoped write-ahead audit capability for physical host effects.
    pub effect_audit: Option<RunnerEffectAuditControl>,
    pub response_tx: oneshot::Sender<Result<RunnerTurnResult, RunnerTurnError>>,
}

impl RunnerTurnRequest {
    /// Construct a provider-turn callback using safe defaults for optional capabilities.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        brain: String,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        context: Vec<crate::claude::Message>,
        approval_audience: crate::brain::store::BrainApprovalAudience,
        approval_connection_id: Option<crate::brain::store::ConnectionId>,
        response_tx: oneshot::Sender<Result<RunnerTurnResult, RunnerTurnError>>,
    ) -> Self {
        Self {
            brain,
            run_id,
            request_seq,
            prompt,
            context,
            approval_audience,
            approval_connection_id,
            approval_tx: None,
            effect_audit: None,
            response_tx,
        }
    }
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
    /// Exact ordered provider/tool continuation messages, including opaque reasoning.
    pub continuation_messages: Vec<crate::claude::Message>,
    /// Completed provider identity/accounting for durable Brain provenance.
    pub invocation_metadata: Option<crate::providers::types::InvocationMetadata>,
    pub turn_events: Vec<RunnerTurnEvent>,
    pub runtime_revision: u64,
    pub checkpoint: crate::vm::TypedRuntimeCheckpoint,
    pub effect_journal: Vec<RunnerEffectRecord>,
    pub commit_ack: Option<RunnerTurnCommitAck>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerTurnCommitNotice {
    pub status: crate::brain::store::BrainRunStatus,
    pub detail: String,
}

/// Send-safe proxy for a frontend-owned post-commit continuation. The Cap'n
/// Proto adapter keeps the actual capability on its LocalSet and forwards
/// notices through this channel.
#[derive(Debug, Clone)]
pub struct RunnerTurnCommitAck {
    tx: mpsc::UnboundedSender<RunnerTurnCommitNotice>,
}

impl RunnerTurnCommitAck {
    pub fn new(tx: mpsc::UnboundedSender<RunnerTurnCommitNotice>) -> Self {
        Self { tx }
    }

    pub fn acknowledge(
        &self,
        status: crate::brain::store::BrainRunStatus,
        detail: impl Into<String>,
    ) -> Result<(), String> {
        self.tx
            .send(RunnerTurnCommitNotice {
                status,
                detail: detail.into(),
            })
            .map_err(|_| "runner commit acknowledgement receiver disconnected".to_string())
    }

    pub(crate) fn tx(&self) -> &mpsc::UnboundedSender<RunnerTurnCommitNotice> {
        &self.tx
    }
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

/// Callback operation whose independently bounded lifecycle failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerOperation {
    Program,
    Turn,
    Cancel,
    ProjectMemory,
}

impl std::fmt::Display for RunnerOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Program => "program",
            Self::Turn => "turn",
            Self::Cancel => "cancel",
            Self::ProjectMemory => "memory projection",
        })
    }
}

/// Stable fail-closed classification for one callback dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerDispatchFailure {
    NoCallback,
    StaleLease,
    Disconnected,
    ResponseDropped,
    TimedOut,
    GenerationInvalidated,
    RunAborted,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("named Brain '{brain}' runner {operation} {detail}")]
pub struct RunnerDispatchError {
    pub brain: String,
    pub operation: RunnerOperation,
    pub failure: RunnerDispatchFailure,
    detail: &'static str,
}

impl RunnerDispatchError {
    fn new(brain: &str, operation: RunnerOperation, failure: RunnerDispatchFailure) -> Self {
        let detail = match failure {
            RunnerDispatchFailure::NoCallback => "has no connected runner callback",
            RunnerDispatchFailure::StaleLease => "callback belongs to a stale lease",
            RunnerDispatchFailure::Disconnected => "callback disconnected",
            RunnerDispatchFailure::ResponseDropped => "dropped its response",
            RunnerDispatchFailure::TimedOut => "timed out",
            RunnerDispatchFailure::GenerationInvalidated => "callback generation was invalidated",
            RunnerDispatchFailure::RunAborted => "run cancelled",
        };
        Self {
            brain: brain.to_string(),
            operation,
            failure,
            detail,
        }
    }
}

/// Independent hard limits for callbacks into the leased frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerDeadlines {
    pub program: Duration,
    pub turn: Duration,
    pub cancel: Duration,
    pub project_memory: Duration,
    /// User-facing cleanup wait for proving that a cancelled callback has
    /// released its exact response sender. Expiry returns to the caller but
    /// quarantines the generation until detached physical settlement.
    pub callback_cleanup: Duration,
}

impl Default for RunnerDeadlines {
    fn default() -> Self {
        Self {
            program: Duration::from_secs(5 * 60),
            // Longer than the daemon's 15-minute addressed approval window,
            // while still bounding provider generation, one repair, and tools.
            turn: Duration::from_secs(20 * 60),
            cancel: Duration::from_secs(10),
            project_memory: Duration::from_secs(60),
            // The IPC bridge gives frontend cancel + original-RPC settlement
            // one shared ten-second budget. Leave a small scheduling margin
            // before declaring that callback generation non-quiescent.
            callback_cleanup: Duration::from_secs(12),
        }
    }
}

impl RunnerDeadlines {
    fn for_operation(self, operation: RunnerOperation) -> Duration {
        match operation {
            RunnerOperation::Program => self.program,
            RunnerOperation::Turn => self.turn,
            RunnerOperation::Cancel => self.cancel,
            RunnerOperation::ProjectMemory => self.project_memory,
        }
    }
}

#[derive(Clone)]
struct Registration {
    id: RunnerRegistrationId,
    lease_id: RunnerLeaseId,
    connection_id: Option<uuid::Uuid>,
    tx: RunnerCallbackSender,
    active: watch::Sender<bool>,
    generation_cancel: CancellationToken,
    in_flight: Arc<AtomicUsize>,
}

impl Registration {
    fn invalidate(&self) {
        self.generation_cancel.cancel();
        self.active.send_replace(false);
    }
}

#[derive(Clone)]
enum RunnerCallbackSender {
    Compatible(mpsc::UnboundedSender<RunnerRequest>),
    Bounded(mpsc::UnboundedSender<BoundedRunnerRequest>),
}

impl RunnerCallbackSender {
    fn send(
        &self,
        request: RunnerRequest,
        cancel: CancellationToken,
        generation_cancel: CancellationToken,
        deadline: tokio::time::Instant,
        cleanup_timeout: Duration,
    ) -> std::result::Result<(), RunnerRequest> {
        match self {
            Self::Compatible(tx) => tx.send(request).map_err(|error| error.0),
            Self::Bounded(tx) => tx
                .send(BoundedRunnerRequest {
                    request,
                    cancel,
                    generation_cancel,
                    deadline,
                    cleanup_timeout,
                })
                .map_err(|error| error.0.request),
        }
    }

    fn is_closed(&self) -> bool {
        match self {
            Self::Compatible(tx) => tx.is_closed(),
            Self::Bounded(tx) => tx.is_closed(),
        }
    }
}

#[derive(Default)]
struct ConnectionAuthority {
    identities: HashMap<String, uuid::Uuid>,
    leases: HashMap<(String, RunnerLeaseId), uuid::Uuid>,
    attachments: HashMap<(String, AttachmentId, ConnectionId), uuid::Uuid>,
    dispatch: HashMap<uuid::Uuid, Arc<ConnectionDispatchAdmission>>,
}

#[derive(Default)]
struct ConnectionDispatchState {
    closed: bool,
    active: usize,
}

#[derive(Default)]
pub(crate) struct ConnectionDispatchAdmission {
    state: Mutex<ConnectionDispatchState>,
    quiesced: tokio::sync::Notify,
}

pub(crate) struct ConnectionDispatchGuard {
    admission: Arc<ConnectionDispatchAdmission>,
}

impl ConnectionDispatchAdmission {
    pub(crate) fn try_enter(self: &Arc<Self>) -> Option<ConnectionDispatchGuard> {
        let mut state = self
            .state
            .lock()
            .expect("connection dispatch lock poisoned");
        if state.closed {
            return None;
        }
        state.active += 1;
        Some(ConnectionDispatchGuard {
            admission: Arc::clone(self),
        })
    }

    fn close(&self) {
        self.state
            .lock()
            .expect("connection dispatch lock poisoned")
            .closed = true;
    }

    async fn wait_quiesced(&self) {
        loop {
            let notified = self.quiesced.notified();
            if self
                .state
                .lock()
                .expect("connection dispatch lock poisoned")
                .active
                == 0
            {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for ConnectionDispatchGuard {
    fn drop(&mut self) {
        let mut state = self
            .admission
            .state
            .lock()
            .expect("connection dispatch lock poisoned");
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            self.admission.quiesced.notify_waiters();
        }
    }
}

/// Registrations contain only Tokio channels and portable values. Cap'n Proto
/// capabilities remain on their connection's LocalSet and are driven by a
/// local bridge task that owns the receiving side of the channel.
#[derive(Clone)]
pub struct BrainRunnerBroker {
    registrations: Arc<RwLock<HashMap<String, Registration>>>,
    pending_registrations: Arc<RwLock<HashMap<String, Registration>>>,
    registration_changes: Arc<tokio::sync::Notify>,
    connection_authority: Arc<Mutex<ConnectionAuthority>>,
    inflight: Arc<Mutex<HashMap<(String, RunId), HashMap<uuid::Uuid, Option<CancellationToken>>>>>,
    cancelled_before_dispatch: Arc<Mutex<std::collections::HashSet<(String, RunId)>>>,
    transient_cancellation_fences:
        Arc<Mutex<HashMap<(String, RunId), std::collections::HashSet<uuid::Uuid>>>>,
    fence_retirement_pending: Arc<Mutex<std::collections::HashSet<(String, RunId)>>>,
    deadlines: RunnerDeadlines,
    #[cfg(test)]
    registration_admission_pause:
        Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>,
    #[cfg(test)]
    pending_promotion_pause:
        Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>,
}

impl Default for BrainRunnerBroker {
    fn default() -> Self {
        Self::with_deadlines(RunnerDeadlines::default())
    }
}

/// Exact authority owned by one IPC connection while its teardown is being
/// durably reconciled. Claims remain fenced until [`finish`](Self::finish)
/// succeeds; dropping this value deliberately leaves them fenced.
pub(crate) struct RunnerConnectionTeardown {
    broker: BrainRunnerBroker,
    connection_id: uuid::Uuid,
    pub(crate) runner_leases: Vec<(String, RunnerLeaseId)>,
    pub(crate) attachments: Vec<(String, AttachmentId, ConnectionId)>,
    dispatch: Arc<ConnectionDispatchAdmission>,
}

impl RunnerConnectionTeardown {
    pub(crate) async fn wait_quiesced(&self) {
        self.dispatch.wait_quiesced().await;
    }

    pub(crate) fn finish(self) -> Result<()> {
        anyhow::ensure!(
            self.dispatch
                .state
                .lock()
                .expect("connection dispatch lock poisoned")
                .active
                == 0,
            "connection callback dispatch is not quiescent"
        );
        let mut authority = self
            .broker
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        authority
            .identities
            .retain(|_, owner| *owner != self.connection_id);
        authority
            .leases
            .retain(|_, owner| *owner != self.connection_id);
        authority
            .attachments
            .retain(|_, owner| *owner != self.connection_id);
        authority.dispatch.remove(&self.connection_id);
        Ok(())
    }
}

struct InflightRequest {
    broker: BrainRunnerBroker,
    key: (String, RunId),
    id: uuid::Uuid,
}

struct RegistrationRequest {
    broker: BrainRunnerBroker,
    brain: String,
    registration_id: RunnerRegistrationId,
    in_flight: Arc<AtomicUsize>,
}

struct SettledRunnerCallback<T, E> {
    response: std::result::Result<Result<T, E>, oneshot::error::RecvError>,
    registration_request: RegistrationRequest,
}

impl Drop for RegistrationRequest {
    fn drop(&mut self) {
        if self.in_flight.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.broker
                .registration_quiesced(&self.brain, self.registration_id);
        }
    }
}

struct CallbackCancellationGuard {
    broker: BrainRunnerBroker,
    brain: String,
    run_id: RunId,
    registration_id: RunnerRegistrationId,
    cancel: CancellationToken,
    armed: bool,
}

impl CallbackCancellationGuard {
    fn new(
        broker: BrainRunnerBroker,
        brain: &str,
        run_id: RunId,
        registration_id: RunnerRegistrationId,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            broker,
            brain: brain.to_string(),
            run_id,
            registration_id,
            cancel,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CallbackCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.broker.fence_run_cancellation(&self.brain, self.run_id);
            self.cancel.cancel();
            // A dropped daemon receiver cannot await physical frontend
            // cleanup. Retire only this exact callback generation so a late
            // guard can never remove a replacement.
            self.broker.unregister(&self.brain, self.registration_id);
        }
    }
}

struct TransientCancellationFence {
    broker: BrainRunnerBroker,
    brain: String,
    run_id: RunId,
    id: uuid::Uuid,
    armed: bool,
}

impl TransientCancellationFence {
    #[cfg(test)]
    fn new(broker: BrainRunnerBroker, brain: &str, run_id: RunId) -> Self {
        let id = broker.fence_callback_cancellation(brain, run_id);
        Self {
            broker,
            brain: brain.to_string(),
            run_id,
            id,
            armed: true,
        }
    }

    fn from_id(broker: BrainRunnerBroker, brain: &str, run_id: RunId, id: uuid::Uuid) -> Self {
        Self {
            broker,
            brain: brain.to_string(),
            run_id,
            id,
            armed: true,
        }
    }
}

impl Drop for TransientCancellationFence {
    fn drop(&mut self) {
        if self.armed {
            self.broker
                .retire_callback_cancellation(&self.brain, self.run_id, self.id);
        }
    }
}

impl Drop for InflightRequest {
    fn drop(&mut self) {
        let mut became_quiescent = false;
        let mut inflight = self
            .broker
            .inflight
            .lock()
            .expect("runner inflight lock poisoned");
        if let Some(requests) = inflight.get_mut(&self.key) {
            requests.remove(&self.id);
            if requests.is_empty() {
                inflight.remove(&self.key);
                became_quiescent = true;
            }
        }
        drop(inflight);
        if became_quiescent {
            self.broker.finish_pending_fence_retirement(&self.key);
        }
    }
}

impl BrainRunnerBroker {
    pub fn with_deadlines(deadlines: RunnerDeadlines) -> Self {
        Self {
            registrations: Arc::default(),
            pending_registrations: Arc::default(),
            registration_changes: Arc::default(),
            connection_authority: Arc::default(),
            inflight: Arc::default(),
            cancelled_before_dispatch: Arc::default(),
            transient_cancellation_fences: Arc::default(),
            fence_retirement_pending: Arc::default(),
            deadlines,
            #[cfg(test)]
            registration_admission_pause: Arc::default(),
            #[cfg(test)]
            pending_promotion_pause: Arc::default(),
        }
    }

    fn track_inflight(
        &self,
        brain: &str,
        run_id: RunId,
        enforce_run_fence: bool,
        abortable: bool,
    ) -> Result<(Option<CancellationToken>, InflightRequest)> {
        let key = (brain.to_string(), run_id);
        let id = uuid::Uuid::new_v4();
        let (mapped_abort, abort) = if abortable {
            let abort = CancellationToken::new();
            (Some(abort.clone()), Some(abort))
        } else {
            (None, None)
        };
        // Admission and terminal fence retirement share this lock order. A
        // terminal owner therefore cannot observe zero admitted callbacks,
        // clear the fence, and race a callback into the inflight map.
        let durable = self
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned");
        let transient = self
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned");
        if enforce_run_fence {
            anyhow::ensure!(
                !durable.contains(&key) && !transient.contains_key(&key),
                "named Brain run cancelled before runner admission"
            );
        }
        self.inflight
            .lock()
            .expect("runner inflight lock poisoned")
            .entry(key.clone())
            .or_default()
            .insert(id, mapped_abort);
        Ok((
            abort,
            InflightRequest {
                broker: self.clone(),
                key,
                id,
            },
        ))
    }

    /// Stop only daemon data-plane waits associated with one durable run.
    /// Its admitted cancellation control remains live so teardown cannot
    /// cancel the message meant to stop the frontend. This never revokes the
    /// runner lease or any other attachment's work.
    pub(crate) fn abort_run(&self, brain: &str, run_id: RunId) {
        let key = (brain.to_string(), run_id);
        let mut inflight = self.inflight.lock().expect("runner inflight lock poisoned");
        if let Some(requests) = inflight.get_mut(&key) {
            for abort in requests.values_mut() {
                if let Some(abort) = abort.take() {
                    abort.cancel();
                }
            }
        }
    }

    /// Tell the exact leased runner to cancel one run without making the
    /// caller own the reply future. Connection teardown uses this before
    /// aborting the daemon-side wait so process-local cleanup cannot silently
    /// leave the frontend executing effects.
    pub(crate) fn request_run_cancellation(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
    ) -> Result<()> {
        let installed_fence = self.fence_run_cancellation(brain, run_id);
        let result = (|| {
            let runtime = tokio::runtime::Handle::try_current()
                .context("runner cancellation requires an active Tokio runtime")?;
            let operation = RunnerOperation::Cancel;
            let (registration, registration_request) =
                self.registration(brain, lease_id, operation)?;
            let (abort_rx, inflight) = self.track_inflight(brain, run_id, false, false)?;
            let (response_tx, response_rx) = oneshot::channel();
            let cancel = CancellationToken::new();
            let deadline = tokio::time::Instant::now() + self.deadlines.cancel;
            registration
                .tx
                .send(
                    RunnerRequest::Cancel(RunnerCancelRequest {
                        brain: brain.to_string(),
                        run_id,
                        response_tx,
                    }),
                    cancel.clone(),
                    registration.generation_cancel.clone(),
                    deadline,
                    self.deadlines.callback_cleanup,
                )
                .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
            let response_rx = self.retain_callback_until_settled(
                response_rx,
                registration_request,
                inflight,
                None,
            );
            let broker = self.clone();
            let brain = brain.to_string();
            runtime.spawn(async move {
                let _ = broker
                    .await_response(
                        &brain,
                        operation,
                        registration,
                        run_id,
                        &cancel,
                        deadline,
                        response_rx,
                        abort_rx,
                        anyhow::Error::msg,
                    )
                    .await;
            });
            Ok(())
        })();
        if result.is_err() && installed_fence {
            self.remove_durable_run_cancellation_fence(brain, run_id);
        }
        result
    }

    pub(crate) fn fence_run_cancellation(&self, brain: &str, run_id: RunId) -> bool {
        self.cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .insert((brain.to_string(), run_id))
    }

    /// Retire the pre-dispatch fence only after the run is durably terminal
    /// and every admitted callback has been aborted or physically settled.
    pub(crate) fn retire_run_cancellation(&self, brain: &str, run_id: RunId) {
        let key = (brain.to_string(), run_id);
        let mut durable = self
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned");
        let mut transient = self
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned");
        let inflight = self.inflight.lock().expect("runner inflight lock poisoned");
        if inflight
            .get(&key)
            .is_some_and(|requests| !requests.is_empty())
        {
            self.fence_retirement_pending
                .lock()
                .expect("runner fence-retirement lock poisoned")
                .insert(key);
            return;
        }
        durable.remove(&key);
        transient.remove(&key);
    }

    pub(crate) fn retire_run_cancellation_when_terminal(
        &self,
        store: crate::brain::store::BrainStore,
        brain: String,
        run_id: RunId,
    ) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let broker = self.clone();
        runtime.spawn(async move {
            let mut delay = Duration::from_millis(10);
            loop {
                if store
                    .inspect_run(&brain, run_id)
                    .is_ok_and(|run| run.status.is_terminal())
                {
                    broker.retire_run_cancellation(&brain, run_id);
                    return;
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(5));
            }
        });
    }

    fn finish_pending_fence_retirement(&self, key: &(String, RunId)) {
        if self
            .fence_retirement_pending
            .lock()
            .expect("runner fence-retirement lock poisoned")
            .remove(key)
        {
            self.clear_run_cancellation_fences(key);
        }
    }

    fn clear_run_cancellation_fences(&self, key: &(String, RunId)) {
        self.cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .remove(key);
        self.transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .remove(key);
    }

    fn remove_durable_run_cancellation_fence(&self, brain: &str, run_id: RunId) {
        self.cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .remove(&(brain.to_string(), run_id));
    }

    #[cfg(test)]
    fn fence_callback_cancellation(&self, brain: &str, run_id: RunId) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        self.transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .entry((brain.to_string(), run_id))
            .or_default()
            .insert(id);
        id
    }

    fn retire_callback_cancellation(&self, brain: &str, run_id: RunId, id: uuid::Uuid) {
        let key = (brain.to_string(), run_id);
        let mut fences = self
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned");
        if let Some(owners) = fences.get_mut(&key) {
            owners.remove(&id);
            if owners.is_empty() {
                fences.remove(&key);
            }
        }
    }

    fn send_if_unfenced(
        &self,
        brain: &str,
        run_id: RunId,
        operation: RunnerOperation,
        registration: &Registration,
        request: RunnerRequest,
        cancel: CancellationToken,
        deadline: tokio::time::Instant,
    ) -> Result<TransientCancellationFence> {
        let key = (brain.to_string(), run_id);
        let durable = self
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned");
        let mut transient = self
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned");
        anyhow::ensure!(
            !durable.contains(&key) && !transient.contains_key(&key),
            "named Brain run cancelled before runner dispatch"
        );
        registration
            .tx
            .send(
                request,
                cancel,
                registration.generation_cancel.clone(),
                deadline,
                // The IPC bridge bounds its cancellation-control ACK inside
                // the user cleanup budget, while retaining the original RPC
                // without a time-based physical-quiescence shortcut.
                self.deadlines.callback_cleanup / 2,
            )
            .map_err(|_| self.disconnect_registration(brain, operation, registration))?;
        let id = uuid::Uuid::new_v4();
        transient.entry(key).or_default().insert(id);
        Ok(TransientCancellationFence::from_id(
            self.clone(),
            brain,
            run_id,
            id,
        ))
    }

    fn retain_callback_until_settled<T, E>(
        &self,
        response_rx: oneshot::Receiver<Result<T, E>>,
        registration_request: RegistrationRequest,
        inflight: InflightRequest,
        fence: Option<TransientCancellationFence>,
    ) -> oneshot::Receiver<SettledRunnerCallback<T, E>>
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        let (settled_tx, settled_rx) = oneshot::channel();
        tokio::spawn(async move {
            let response = response_rx.await;
            drop(fence);
            drop(inflight);
            let _ = settled_tx.send(SettledRunnerCallback {
                response,
                registration_request,
            });
        });
        settled_rx
    }

    pub fn register(
        &self,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<RunnerRequest>,
    ) -> RunnerRegistrationId {
        self.register_sender(brain.into(), lease_id, RunnerCallbackSender::Compatible(tx))
    }

    #[cfg(test)]
    pub(crate) fn register_bounded(
        &self,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<BoundedRunnerRequest>,
    ) -> RunnerRegistrationId {
        self.register_sender(brain.into(), lease_id, RunnerCallbackSender::Bounded(tx))
    }

    fn register_sender(
        &self,
        brain: String,
        lease_id: RunnerLeaseId,
        tx: RunnerCallbackSender,
    ) -> RunnerRegistrationId {
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        let (active, _) = watch::channel(true);
        let registration = Registration {
            id,
            lease_id,
            connection_id: None,
            tx,
            active,
            generation_cancel: CancellationToken::new(),
            in_flight: Arc::default(),
        };
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        let mut pending = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        match registrations.get(&brain) {
            None => {
                registrations.insert(brain, registration);
            }
            Some(current) if current.in_flight.load(Ordering::Acquire) == 0 => {
                let replaced = registrations.insert(brain, registration);
                if let Some(replaced) = replaced {
                    replaced.invalidate();
                }
            }
            Some(current) => {
                if current.lease_id != lease_id {
                    current.invalidate();
                }
                if let Some(replaced) = pending.insert(brain, registration) {
                    replaced.invalidate();
                }
            }
        }
        self.registration_changes.notify_waiters();
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
                let dispatch = authority.dispatch.entry(connection_id).or_default();
                anyhow::ensure!(
                    !dispatch
                        .state
                        .lock()
                        .expect("connection dispatch lock poisoned")
                        .closed,
                    "IPC connection is tearing down"
                );
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
                let dispatch = authority.dispatch.entry(connection_id).or_default();
                anyhow::ensure!(
                    !dispatch
                        .state
                        .lock()
                        .expect("connection dispatch lock poisoned")
                        .closed,
                    "IPC connection is tearing down"
                );
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
        let key = (brain.to_string(), attachment_id, attachment_connection_id);
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        match authority.attachments.get(&key) {
            Some(owner) if *owner != connection_id => {
                anyhow::bail!("Brain attachment is owned by another IPC connection")
            }
            _ => {
                let dispatch = authority.dispatch.entry(connection_id).or_default();
                anyhow::ensure!(
                    !dispatch
                        .state
                        .lock()
                        .expect("connection dispatch lock poisoned")
                        .closed,
                    "IPC connection is tearing down"
                );
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
        if authority
            .attachments
            .get(&(brain.to_string(), attachment_id, attachment_connection_id))
            != Some(&connection_id)
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
        let key = (brain.to_string(), attachment_id, attachment_connection_id);
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
        self.register_sender_for_connection(
            connection_id,
            brain.into(),
            lease_id,
            RunnerCallbackSender::Compatible(tx),
        )
    }

    pub(crate) fn register_bounded_for_connection(
        &self,
        connection_id: uuid::Uuid,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<BoundedRunnerRequest>,
    ) -> Result<RunnerRegistrationId> {
        self.register_sender_for_connection(
            connection_id,
            brain.into(),
            lease_id,
            RunnerCallbackSender::Bounded(tx),
        )
    }

    fn register_sender_for_connection(
        &self,
        connection_id: uuid::Uuid,
        brain: String,
        lease_id: RunnerLeaseId,
        tx: RunnerCallbackSender,
    ) -> Result<RunnerRegistrationId> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        anyhow::ensure!(
            authority.leases.get(&(brain.clone(), lease_id)) == Some(&connection_id),
            "runner lease is not owned by this IPC connection"
        );
        let dispatch = authority
            .dispatch
            .get(&connection_id)
            .context("IPC connection has no dispatch authority")?;
        anyhow::ensure!(
            !dispatch
                .state
                .lock()
                .expect("connection dispatch lock poisoned")
                .closed,
            "IPC connection is tearing down"
        );
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        let (active, _) = watch::channel(true);
        let registration = Registration {
            id,
            lease_id,
            connection_id: Some(connection_id),
            tx,
            active,
            generation_cancel: CancellationToken::new(),
            in_flight: Arc::default(),
        };
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        let mut pending = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        match registrations.get(&brain) {
            None => {
                registrations.insert(brain, registration);
            }
            Some(current) if current.in_flight.load(Ordering::Acquire) == 0 => {
                let replaced = registrations.insert(brain, registration);
                if let Some(replaced) = replaced {
                    replaced.invalidate();
                }
            }
            Some(current) => {
                // Never expose a renewal or replacement while an older
                // generation can still call back. Different authority is
                // invalidated immediately; same-authority renewal remains
                // usable only after the admitted request physically settles.
                if current.lease_id != lease_id || current.connection_id != Some(connection_id) {
                    current.invalidate();
                }
                if let Some(replaced) = pending.insert(brain, registration) {
                    replaced.invalidate();
                }
            }
        }
        drop(authority);
        self.registration_changes.notify_waiters();
        Ok(id)
    }

    /// Wait until an exact registered generation is the dispatchable head.
    /// IPC registration bootstrap uses this to avoid resuming queued work on
    /// a pending successor before the previous callback quiesces.
    pub(crate) async fn wait_registration_active(
        &self,
        brain: &str,
        id: RunnerRegistrationId,
    ) -> Result<()> {
        loop {
            let notified = self.registration_changes.notified();
            if self
                .registrations
                .read()
                .expect("runner broker lock poisoned")
                .get(brain)
                .is_some_and(|registration| registration.id == id && *registration.active.borrow())
            {
                return Ok(());
            }
            let still_pending = self
                .pending_registrations
                .read()
                .expect("runner pending-registration lock poisoned")
                .get(brain)
                .is_some_and(|registration| registration.id == id);
            if !still_pending {
                anyhow::bail!("runner callback generation was retired before activation");
            }
            notified.await;
        }
    }

    pub(crate) fn connection_dispatch_admission(
        &self,
        connection_id: uuid::Uuid,
    ) -> Result<Arc<ConnectionDispatchAdmission>> {
        self.connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned")
            .dispatch
            .get(&connection_id)
            .cloned()
            .context("IPC connection has no dispatch authority")
    }

    pub(crate) fn begin_connection_teardown(
        &self,
        connection_id: uuid::Uuid,
    ) -> RunnerConnectionTeardown {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        let attachments = authority
            .attachments
            .iter()
            .filter_map(
                |((brain, attachment_id, attachment_connection_id), owner)| {
                    (*owner == connection_id)
                        .then(|| (brain.clone(), *attachment_id, *attachment_connection_id))
                },
            )
            .collect();
        let runner_leases = authority
            .leases
            .iter()
            .filter_map(|((brain, lease_id), owner)| {
                (*owner == connection_id).then(|| (brain.clone(), *lease_id))
            })
            .collect();
        let dispatch = authority
            .dispatch
            .get(&connection_id)
            .cloned()
            .unwrap_or_default();
        dispatch.close();
        drop(authority);
        // Keep an invalidated head generation in the map until every request
        // admitted through it has physically quiesced. Its RegistrationRequest
        // drop then promotes a pending callback owned by another connection.
        // Removing the head here would lose that promotion edge forever.
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        let mut pending = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        let mut promote = Vec::new();
        registrations.retain(|brain, registration| {
            if registration.connection_id != Some(connection_id) {
                return true;
            }
            registration.invalidate();
            let keep = registration.in_flight.load(Ordering::Acquire) != 0;
            if !keep {
                promote.push(brain.clone());
            }
            keep
        });
        pending.retain(|_, registration| {
            let keep = registration.connection_id != Some(connection_id);
            if !keep {
                registration.invalidate();
            }
            keep
        });
        for brain in promote {
            self.promote_pending_registration_locked(&mut registrations, &mut pending, &brain);
        }
        drop(pending);
        drop(registrations);
        self.registration_changes.notify_waiters();
        RunnerConnectionTeardown {
            broker: self.clone(),
            connection_id,
            runner_leases,
            attachments,
            dispatch,
        }
    }

    /// Remove a registration only if it is still the connection that created
    /// it. A late disconnect must not remove a replacement runner callback.
    pub fn unregister(&self, brain: &str, id: RunnerRegistrationId) {
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        let mut pending = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        if let Some(registration) = registrations.get(brain).filter(|entry| entry.id == id) {
            registration.invalidate();
            if registration.in_flight.load(Ordering::Acquire) == 0 {
                registrations.remove(brain);
                self.promote_pending_registration_locked(&mut registrations, &mut pending, brain);
            }
            drop(pending);
            drop(registrations);
            self.registration_changes.notify_waiters();
            return;
        }
        if pending.get(brain).is_some_and(|entry| entry.id == id) {
            if let Some(registration) = pending.remove(brain) {
                registration.invalidate();
            }
        }
        self.registration_changes.notify_waiters();
    }

    /// Invalidate only the callback registered for one exact durable lease.
    /// A replacement generation is left untouched.
    pub(crate) fn invalidate_lease(&self, brain: &str, lease_id: RunnerLeaseId) -> bool {
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        if !registrations
            .get(brain)
            .is_some_and(|registration| registration.lease_id == lease_id)
        {
            return false;
        }
        let registration = registrations
            .get(brain)
            .expect("matching runner registration disappeared")
            .clone();
        registration.invalidate();
        let mut pending_registrations = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        if let Some(pending) = pending_registrations.remove(brain) {
            if pending.lease_id == lease_id {
                pending.invalidate();
            } else {
                pending_registrations.insert(brain.to_string(), pending);
            }
        }
        if registration.in_flight.load(Ordering::Acquire) == 0 {
            registrations.remove(brain);
            self.promote_pending_registration_locked(
                &mut registrations,
                &mut pending_registrations,
                brain,
            );
        }
        drop(pending_registrations);
        drop(registrations);
        self.registration_changes.notify_waiters();
        true
    }

    fn promote_pending_registration_locked(
        &self,
        registrations: &mut HashMap<String, Registration>,
        pending: &mut HashMap<String, Registration>,
        brain: &str,
    ) {
        #[cfg(test)]
        if let Some((reached, release)) = self
            .pending_promotion_pause
            .lock()
            .expect("pending-promotion test hook poisoned")
            .take()
        {
            let _ = reached.send(());
            let _ = release.recv();
        }
        let next = pending.remove(brain);
        let Some(next) = next else { return };
        if !*next.active.borrow() || next.tx.is_closed() {
            return;
        }
        let replaced = registrations.insert(brain.to_string(), next);
        if let Some(replaced) = replaced {
            replaced.invalidate();
        }
    }

    fn registration_quiesced(&self, brain: &str, id: RunnerRegistrationId) {
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        let mut pending = self
            .pending_registrations
            .write()
            .expect("runner pending-registration lock poisoned");
        let should_promote = registrations.get(brain).is_some_and(|registration| {
            registration.id == id && (pending.contains_key(brain) || !*registration.active.borrow())
        });
        if should_promote {
            if let Some(old) = registrations.remove(brain) {
                old.invalidate();
            }
            self.promote_pending_registration_locked(&mut registrations, &mut pending, brain);
        }
        drop(pending);
        drop(registrations);
        if should_promote {
            self.registration_changes.notify_waiters();
        }
    }

    fn registration(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        operation: RunnerOperation,
    ) -> Result<(Registration, RegistrationRequest)> {
        // Admission is atomic with replacement installation: retain the map
        // read lock through validation and the in-flight increment so a writer
        // cannot install generation B after we clone A but before A is counted.
        let registrations = self
            .registrations
            .read()
            .expect("runner broker lock poisoned");
        let registration = registrations.get(brain).ok_or_else(|| {
            RunnerDispatchError::new(brain, operation, RunnerDispatchFailure::NoCallback)
        })?;
        if registration.lease_id != lease_id {
            return Err(RunnerDispatchError::new(
                brain,
                operation,
                RunnerDispatchFailure::StaleLease,
            )
            .into());
        }
        if !*registration.active.borrow() {
            return Err(RunnerDispatchError::new(
                brain,
                operation,
                RunnerDispatchFailure::GenerationInvalidated,
            )
            .into());
        }
        #[cfg(test)]
        if let Some((reached, release)) = self
            .registration_admission_pause
            .lock()
            .expect("registration admission test hook poisoned")
            .take()
        {
            let _ = reached.send(());
            let _ = release.recv();
        }
        registration.in_flight.fetch_add(1, Ordering::AcqRel);
        let registration = registration.clone();
        drop(registrations);
        let request = RegistrationRequest {
            broker: self.clone(),
            brain: brain.to_string(),
            registration_id: registration.id,
            in_flight: Arc::clone(&registration.in_flight),
        };
        Ok((registration, request))
    }

    fn disconnect_registration(
        &self,
        brain: &str,
        operation: RunnerOperation,
        registration: &Registration,
    ) -> anyhow::Error {
        registration.invalidate();
        self.unregister(brain, registration.id);
        RunnerDispatchError::new(brain, operation, RunnerDispatchFailure::Disconnected).into()
    }

    async fn await_response<T, E>(
        &self,
        brain: &str,
        operation: RunnerOperation,
        registration: Registration,
        run_id: RunId,
        cancel: &CancellationToken,
        deadline: tokio::time::Instant,
        mut response_rx: oneshot::Receiver<SettledRunnerCallback<T, E>>,
        abort: Option<CancellationToken>,
        map_remote_error: impl FnOnce(E) -> anyhow::Error,
    ) -> Result<T>
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        let mut cancellation = CallbackCancellationGuard::new(
            self.clone(),
            brain,
            run_id,
            registration.id,
            cancel.clone(),
        );
        let mut active = registration.active.subscribe();
        let aborted = async {
            match abort.as_ref() {
                Some(abort) => abort.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(aborted);
        let failure = if tokio::time::Instant::now() >= deadline {
            RunnerDispatchFailure::TimedOut
        } else if !*active.borrow() {
            RunnerDispatchFailure::GenerationInvalidated
        } else {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline) => RunnerDispatchFailure::TimedOut,
                _ = &mut aborted => RunnerDispatchFailure::RunAborted,
                _ = active.changed() => RunnerDispatchFailure::GenerationInvalidated,
                response = &mut response_rx => {
                    let settled = match response {
                        Ok(settled) => settled,
                        Err(_) => {
                            cancel.cancel();
                            cancellation.disarm();
                            return Err(RunnerDispatchError::new(
                                brain,
                                operation,
                                RunnerDispatchFailure::ResponseDropped,
                            ).into());
                        }
                    };
                    let SettledRunnerCallback {
                        response,
                        registration_request: _registration_request,
                    } = settled;
                    let response = match response {
                        Ok(response) => response,
                        Err(_) => {
                            cancel.cancel();
                            cancellation.disarm();
                            return Err(RunnerDispatchError::new(
                                brain,
                                operation,
                                RunnerDispatchFailure::ResponseDropped,
                            ).into());
                        }
                    };
                    if !*registration.active.borrow() {
                        cancel.cancel();
                        cancellation.disarm();
                        return Err(RunnerDispatchError::new(
                            brain,
                            operation,
                            RunnerDispatchFailure::GenerationInvalidated,
                        )
                        .into());
                    } else {
                        cancellation.disarm();
                        return response.map_err(map_remote_error);
                    }
                }
            }
        };

        cancel.cancel();

        // Do not release the durable run lane until the exact callback proves
        // quiescence by dropping its response sender. A late payload means the
        // frontend crossed the cancellation boundary and must re-register.
        let cleanup_deadline = tokio::time::Instant::now() + self.deadlines.callback_cleanup;
        enum CleanupOutcome {
            SenderDropped,
            LatePayload,
            StillRunning,
        }
        let cleanup = tokio::select! {
            biased;
            response = &mut response_rx => match response {
                Ok(SettledRunnerCallback {
                    response: Ok(Ok(_)),
                    registration_request,
                }) => {
                    self.unregister(brain, registration.id);
                    drop(registration_request);
                    CleanupOutcome::LatePayload
                }
                Ok(SettledRunnerCallback {
                    response: Ok(Err(_)) | Err(_),
                    registration_request,
                }) => {
                    drop(registration_request);
                    CleanupOutcome::SenderDropped
                }
                Err(_) => CleanupOutcome::SenderDropped,
            },
            _ = tokio::time::sleep_until(cleanup_deadline) => CleanupOutcome::StillRunning,
        };
        match cleanup {
            CleanupOutcome::SenderDropped => {}
            CleanupOutcome::LatePayload => {
                // The callback is physically settled, but it crossed the
                // cancellation boundary and its exact generation may not
                // serve another request. A separately admitted successor is
                // preserved and can promote now that this callback settled.
            }
            CleanupOutcome::StillRunning => {
                // Return the user-facing timeout without making this
                // generation reusable. The detached callback owner already
                // holds both exact admission counts and the transient fence
                // until the original callback settles (or connection
                // teardown drops its response sender).
                self.unregister(brain, registration.id);
            }
        }
        cancellation.disarm();
        Err(RunnerDispatchError::new(brain, operation, failure).into())
    }

    pub fn has_registration(&self, brain: &str, lease_id: RunnerLeaseId) -> bool {
        self.registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .is_some_and(|entry| {
                entry.lease_id == lease_id && *entry.active.borrow() && !entry.tx.is_closed()
            })
    }

    pub(crate) fn has_exact_registration(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        registration_id: RunnerRegistrationId,
    ) -> bool {
        self.registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .is_some_and(|entry| {
                entry.id == registration_id
                    && entry.lease_id == lease_id
                    && *entry.active.borrow()
                    && !entry.tx.is_closed()
            })
    }

    pub async fn dispatch_program(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
        request_seq: u64,
        language: ProgramLanguage,
        source: String,
        interaction: RunnerProgramInteraction,
        grant_ceiling: Option<crate::vm::EffectSet>,
    ) -> Result<RunnerProgramResult> {
        let operation = RunnerOperation::Program;
        let deadline = tokio::time::Instant::now() + self.deadlines.for_operation(operation);
        let (registration, registration_request) = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, inflight) = self.track_inflight(brain, run_id, true, true)?;
        let cancel = CancellationToken::new();
        let fence = self.send_if_unfenced(
            brain,
            run_id,
            operation,
            &registration,
            RunnerRequest::Program(RunnerProgramRequest {
                brain: brain.to_string(),
                run_id,
                request_seq,
                language,
                source,
                interaction,
                grant_ceiling,
                control_tx: None,
                effect_audit: None,
                response_tx,
            }),
            cancel.clone(),
            deadline,
        )?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
        );
        self.await_response(
            brain,
            operation,
            registration,
            run_id,
            &cancel,
            deadline,
            response_rx,
            abort_rx,
            anyhow::Error::new,
        )
        .await
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
        approval_connection_id: Option<crate::brain::store::ConnectionId>,
    ) -> Result<RunnerTurnResult> {
        let operation = RunnerOperation::Turn;
        let deadline = tokio::time::Instant::now() + self.deadlines.for_operation(operation);
        let (registration, registration_request) = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, inflight) = self.track_inflight(brain, run_id, true, true)?;
        let cancel = CancellationToken::new();
        let fence = self.send_if_unfenced(
            brain,
            run_id,
            operation,
            &registration,
            RunnerRequest::Turn(RunnerTurnRequest {
                brain: brain.to_string(),
                run_id,
                request_seq,
                prompt,
                context,
                approval_audience,
                approval_connection_id,
                approval_tx: None,
                effect_audit: None,
                response_tx,
            }),
            cancel.clone(),
            deadline,
        )?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
        );
        self.await_response(
            brain,
            operation,
            registration,
            run_id,
            &cancel,
            deadline,
            response_rx,
            abort_rx,
            anyhow::Error::new,
        )
        .await
    }

    pub async fn cancel_run(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        run_id: RunId,
    ) -> Result<bool> {
        let operation = RunnerOperation::Cancel;
        let deadline = tokio::time::Instant::now() + self.deadlines.for_operation(operation);
        let (registration, registration_request) = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        // Cancel is control-plane traffic for this exact run. It must retain
        // registration/generation admission while bypassing only the run's
        // own data-plane cancellation fence.
        let (abort_rx, inflight) = self.track_inflight(brain, run_id, false, false)?;
        let cancel = CancellationToken::new();
        registration
            .tx
            .send(
                RunnerRequest::Cancel(RunnerCancelRequest {
                    brain: brain.to_string(),
                    run_id,
                    response_tx,
                }),
                cancel.clone(),
                registration.generation_cancel.clone(),
                deadline,
                self.deadlines.callback_cleanup,
            )
            .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        let response_rx =
            self.retain_callback_until_settled(response_rx, registration_request, inflight, None);
        self.await_response(
            brain,
            operation,
            registration,
            run_id,
            &cancel,
            deadline,
            response_rx,
            abort_rx,
            anyhow::Error::msg,
        )
        .await
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
        let operation = RunnerOperation::ProjectMemory;
        let deadline = tokio::time::Instant::now() + self.deadlines.for_operation(operation);
        let (registration, registration_request) = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, inflight) = self.track_inflight(brain, run_id, true, true)?;
        let cancel = CancellationToken::new();
        let fence = self.send_if_unfenced(
            brain,
            run_id,
            operation,
            &registration,
            RunnerRequest::ProjectMemory(RunnerMemoryProjectionRequest {
                brain_id,
                brain: brain.to_string(),
                run_id,
                request_seq,
                prompt,
                source,
                response_tx,
            }),
            cancel.clone(),
            deadline,
        )?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
        );
        self.await_response(
            brain,
            operation,
            registration,
            run_id,
            &cancel,
            deadline,
            response_rx,
            abort_rx,
            anyhow::Error::msg,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROGRAM_DEADLINE: Duration = Duration::from_secs(20);
    const TURN_DEADLINE: Duration = Duration::from_secs(60);
    const CANCEL_DEADLINE: Duration = Duration::from_secs(2);
    const MEMORY_DEADLINE: Duration = Duration::from_secs(10);
    const CALLBACK_CLEANUP: Duration = Duration::from_secs(5);

    fn lease() -> RunnerLeaseId {
        RunnerLeaseId(uuid::Uuid::new_v4())
    }

    fn deadline_broker() -> BrainRunnerBroker {
        BrainRunnerBroker::with_deadlines(RunnerDeadlines {
            program: PROGRAM_DEADLINE,
            turn: TURN_DEADLINE,
            cancel: CANCEL_DEADLINE,
            project_memory: MEMORY_DEADLINE,
            callback_cleanup: CALLBACK_CLEANUP,
        })
    }

    fn assert_dispatch_failure(
        error: &anyhow::Error,
        operation: RunnerOperation,
        failure: RunnerDispatchFailure,
    ) {
        let error = error
            .downcast_ref::<RunnerDispatchError>()
            .expect("runner infrastructure errors must remain classifiable");
        assert_eq!(error.operation, operation);
        assert_eq!(error.failure, failure);
    }

    fn program_result(output: &str) -> RunnerProgramResult {
        let checkpoint = crate::runtime::ProgramRuntime::new()
            .revision_history()
            .unwrap()
            .pop()
            .unwrap()
            .checkpoint
            .unwrap();
        RunnerProgramResult {
            output: output.into(),
            runtime_revision: 1,
            checkpoint,
            effect_journal: Vec::new(),
        }
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
            .claim_connection_attachment(owner, "brain", attachment_id, attachment_connection_id)
            .unwrap();
        assert!(broker
            .require_connection_attachment(
                intruder,
                "brain",
                attachment_id,
                attachment_connection_id,
            )
            .is_err());

        let teardown = broker.begin_connection_teardown(owner);
        assert_eq!(
            teardown.attachments,
            vec![("brain".to_string(), attachment_id, attachment_connection_id,)]
        );
        assert_eq!(
            teardown.runner_leases,
            vec![("brain".to_string(), lease_id)]
        );
        assert!(!broker.has_registration("brain", lease_id));
        assert!(broker
            .claim_connection_lease(intruder, "brain", lease_id)
            .is_err());
        drop(teardown);
        assert!(broker
            .claim_connection_identity(intruder, "runner-a@box.local")
            .is_err());
        assert!(broker
            .require_connection_lease(owner, "brain", lease_id)
            .is_ok());

        let teardown = broker.begin_connection_teardown(owner);
        teardown.finish().unwrap();
        assert!(broker
            .require_connection_lease(owner, "brain", lease_id)
            .is_err());
        broker
            .claim_connection_identity(intruder, "runner-a@box.local")
            .unwrap();
        assert!(broker
            .require_connection_attachment(owner, "brain", attachment_id, attachment_connection_id,)
            .is_err());
    }

    #[test]
    fn test_attachment_claim_is_rejected_after_connection_teardown_closes_dispatch() {
        let broker = BrainRunnerBroker::default();
        let connection_id = uuid::Uuid::new_v4();
        broker
            .claim_connection_identity(connection_id, "runner@box.local")
            .unwrap();
        let _teardown = broker.begin_connection_teardown(connection_id);

        let error = broker
            .claim_connection_attachment(
                connection_id,
                "brain",
                AttachmentId(uuid::Uuid::new_v4()),
                ConnectionId(uuid::Uuid::new_v4()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("tearing down"));
    }

    #[test]
    fn test_registration_admission_is_atomic_with_replacement_installation() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, old_tx);
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *broker
            .registration_admission_pause
            .lock()
            .expect("registration admission test hook poisoned") = Some((reached_tx, release_rx));

        let admitting_broker = broker.clone();
        let admitting = std::thread::spawn(move || {
            admitting_broker
                .registration("brain", lease_id, RunnerOperation::Program)
                .unwrap()
        });
        reached_rx.recv().unwrap();

        let (installed_tx, installed_rx) = std::sync::mpsc::channel();
        let replacing_broker = broker.clone();
        let replacing = std::thread::spawn(move || {
            let (new_tx, new_rx) = mpsc::unbounded_channel();
            replacing_broker.register("brain", lease_id, new_tx);
            installed_tx.send(()).unwrap();
            new_rx
        });
        assert!(installed_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        release_tx.send(()).unwrap();
        let (_registration, request) = admitting.join().unwrap();
        installed_rx.recv().unwrap();
        let _new_rx = replacing.join().unwrap();
        assert_eq!(request.in_flight.load(Ordering::Acquire), 1);
        drop(request);
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test]
    async fn test_teardown_promotes_other_connection_pending_generation_after_inflight_quiesces() {
        let broker = BrainRunnerBroker::default();
        let old_connection = uuid::Uuid::new_v4();
        let new_connection = uuid::Uuid::new_v4();
        let old_lease = lease();
        let new_lease = lease();
        broker
            .claim_connection_lease(old_connection, "brain", old_lease)
            .unwrap();
        broker
            .claim_connection_lease(new_connection, "brain", new_lease)
            .unwrap();
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        broker
            .register_for_connection(old_connection, "brain", old_lease, old_tx)
            .unwrap();

        let dispatch_broker = broker.clone();
        let run_id = RunId(uuid::Uuid::new_v4());
        let dispatch = tokio::spawn(async move {
            dispatch_broker
                .dispatch_program(
                    "brain",
                    old_lease,
                    run_id,
                    1,
                    ProgramLanguage::Lisp,
                    "old".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(old_request) = old_rx.recv().await.unwrap() else {
            panic!("expected old callback")
        };
        let (new_tx, _new_rx) = mpsc::unbounded_channel();
        let new_registration = broker
            .register_for_connection(new_connection, "brain", new_lease, new_tx)
            .unwrap();
        let teardown = broker.begin_connection_teardown(old_connection);
        assert!(!broker.has_registration("brain", old_lease));

        drop(old_request.response_tx);
        assert!(dispatch.await.unwrap().is_err());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            broker.wait_registration_active("brain", new_registration),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(broker.has_registration("brain", new_lease));
        teardown.wait_quiesced().await;
    }

    #[test]
    fn test_pending_promotion_is_atomic_against_concurrent_third_registration() {
        let broker = BrainRunnerBroker::default();
        let old_connection = uuid::Uuid::new_v4();
        let pending_connection = uuid::Uuid::new_v4();
        let concurrent_connection = uuid::Uuid::new_v4();
        let old_lease = lease();
        let pending_lease = lease();
        let concurrent_lease = lease();
        for (connection, lease_id) in [
            (old_connection, old_lease),
            (pending_connection, pending_lease),
            (concurrent_connection, concurrent_lease),
        ] {
            broker
                .claim_connection_lease(connection, "brain", lease_id)
                .unwrap();
        }
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        broker
            .register_for_connection(old_connection, "brain", old_lease, old_tx)
            .unwrap();
        let (_old_registration, admitted) = broker
            .registration("brain", old_lease, RunnerOperation::Program)
            .unwrap();
        let (pending_tx, _pending_rx) = mpsc::unbounded_channel();
        broker
            .register_for_connection(pending_connection, "brain", pending_lease, pending_tx)
            .unwrap();
        let teardown = broker.begin_connection_teardown(old_connection);

        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *broker
            .pending_promotion_pause
            .lock()
            .expect("pending-promotion test hook poisoned") = Some((reached_tx, release_rx));
        let quiescing = std::thread::spawn(move || drop(admitted));
        reached_rx.recv().unwrap();

        let (installed_tx, installed_rx) = std::sync::mpsc::channel();
        let registering_broker = broker.clone();
        let registering = std::thread::spawn(move || {
            let (tx, rx) = mpsc::unbounded_channel();
            registering_broker
                .register_for_connection(concurrent_connection, "brain", concurrent_lease, tx)
                .unwrap();
            installed_tx.send(()).unwrap();
            rx
        });
        assert!(installed_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());
        release_tx.send(()).unwrap();
        quiescing.join().unwrap();
        installed_rx.recv().unwrap();
        let _concurrent_rx = registering.join().unwrap();

        assert!(broker.has_registration("brain", concurrent_lease));
        assert!(!broker.has_registration("brain", pending_lease));
        assert!(!broker
            .pending_registrations
            .read()
            .expect("runner pending-registration lock poisoned")
            .contains_key("brain"));
        teardown.finish().unwrap();
    }

    #[test]
    fn test_public_runner_request_struct_literals_remain_source_compatible() {
        let run_id = RunId(uuid::Uuid::new_v4());
        let (program_tx, _program_rx) = oneshot::channel();
        let _program = RunnerProgramRequest {
            brain: "brain".into(),
            run_id,
            request_seq: 1,
            language: ProgramLanguage::Lisp,
            source: "noop".into(),
            interaction: RunnerProgramInteraction::Interactive,
            grant_ceiling: None,
            control_tx: None,
            effect_audit: None,
            response_tx: program_tx,
        };
        let (turn_tx, _turn_rx) = oneshot::channel();
        let _turn = RunnerTurnRequest {
            brain: "brain".into(),
            run_id,
            request_seq: 1,
            prompt: "prompt".into(),
            context: Vec::new(),
            approval_audience: test_approval_audience(),
            approval_connection_id: None,
            approval_tx: None,
            effect_audit: None,
            response_tx: turn_tx,
        };
        let (memory_tx, _memory_rx) = oneshot::channel();
        let _memory = RunnerMemoryProjectionRequest {
            brain_id: crate::brain::store::BrainId(uuid::Uuid::new_v4()),
            brain: "brain".into(),
            run_id,
            request_seq: 1,
            prompt: "prompt".into(),
            source: "source".into(),
            response_tx: memory_tx,
        };
        let (cancel_tx, _cancel_rx) = oneshot::channel();
        let _cancel = RunnerCancelRequest {
            brain: "brain".into(),
            run_id,
            response_tx: cancel_tx,
        };
    }

    #[test]
    fn test_terminal_cancellation_fences_retire_boundedly_without_inflight_work() {
        let broker = BrainRunnerBroker::default();
        for _ in 0..10_000 {
            let run_id = RunId(uuid::Uuid::new_v4());
            broker.fence_run_cancellation("brain", run_id);
            broker.abort_run("brain", run_id);
            broker.retire_run_cancellation("brain", run_id);
        }
        assert!(broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .is_empty());
    }

    #[tokio::test]
    async fn test_fenced_dispatch_cannot_retire_durable_fence_before_terminal_retirement() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, _rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        broker.fence_run_cancellation("brain", run_id);

        assert!(broker
            .dispatch_program(
                "brain",
                lease_id,
                run_id,
                1,
                ProgramLanguage::Lisp,
                "must not dispatch".into(),
                RunnerProgramInteraction::Interactive,
                None,
            )
            .await
            .is_err());
        assert!(broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&("brain".into(), run_id)));

        broker.abort_run("brain", run_id);
        assert!(broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&("brain".into(), run_id)));
        broker.retire_run_cancellation("brain", run_id);
        assert!(!broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&("brain".into(), run_id)));
    }

    #[tokio::test]
    async fn test_cancel_control_traffic_bypasses_only_its_exact_run_fence() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        broker.fence_run_cancellation("brain", run_id);

        let cancelling = {
            let broker = broker.clone();
            tokio::spawn(async move { broker.cancel_run("brain", lease_id, run_id).await })
        };
        let RunnerRequest::Cancel(request) = rx.recv().await.unwrap() else {
            panic!("fenced run did not enqueue its control-plane cancellation")
        };
        assert_eq!(request.run_id, run_id);
        request.response_tx.send(Ok(true)).unwrap();
        assert!(cancelling.await.unwrap().unwrap());

        assert!(broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&("brain".into(), run_id)));
        assert!(broker
            .dispatch_program(
                "brain",
                lease_id,
                run_id,
                1,
                ProgramLanguage::Lisp,
                "must remain fenced".into(),
                RunnerProgramInteraction::Interactive,
                None,
            )
            .await
            .is_err());
        broker.retire_run_cancellation("brain", run_id);
    }

    #[tokio::test]
    async fn test_fire_and_forget_cancel_remains_admitted_when_data_wait_is_aborted() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register_bounded("brain", lease_id, tx);

        broker
            .request_run_cancellation("brain", lease_id, run_id)
            .unwrap();
        broker.abort_run("brain", run_id);
        let request = rx.recv().await.expect("cancel control was not enqueued");
        assert!(matches!(&request.request, RunnerRequest::Cancel(_)));
        assert!(
            !request.cancel.is_cancelled(),
            "data abort cancelled its own control-plane cancellation"
        );
        let RunnerRequest::Cancel(cancel) = request.request else {
            unreachable!()
        };
        cancel.response_tx.send(Ok(true)).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while broker
                .inflight
                .lock()
                .expect("runner inflight lock poisoned")
                .contains_key(&("brain".to_string(), run_id))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancel control admission did not retire after settlement");
        broker.retire_run_cancellation("brain", run_id);
    }

    #[test]
    fn test_abort_keeps_fence_until_every_admitted_dispatch_settles() {
        let broker = BrainRunnerBroker::default();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (_abort_rx, inflight) = broker.track_inflight("brain", run_id, true, true).unwrap();
        broker.fence_run_cancellation("brain", run_id);

        broker.abort_run("brain", run_id);
        broker.retire_run_cancellation("brain", run_id);
        let key = ("brain".to_string(), run_id);
        assert!(broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&key));
        assert!(broker
            .fence_retirement_pending
            .lock()
            .expect("runner fence-retirement lock poisoned")
            .contains(&key));

        drop(inflight);
        assert!(!broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&key));
        assert!(!broker
            .fence_retirement_pending
            .lock()
            .expect("runner fence-retirement lock poisoned")
            .contains(&key));
    }

    #[test]
    fn test_transient_cancellation_fences_retire_by_exact_owner() {
        let broker = BrainRunnerBroker::default();
        let run_id = RunId(uuid::Uuid::new_v4());
        let first = TransientCancellationFence::new(broker.clone(), "brain", run_id);
        let second = TransientCancellationFence::new(broker.clone(), "brain", run_id);
        let key = ("brain".to_string(), run_id);

        drop(first);
        assert_eq!(
            broker
                .transient_cancellation_fences
                .lock()
                .expect("runner transient-fence lock poisoned")
                .get(&key)
                .map_or(0, std::collections::HashSet::len),
            1
        );
        drop(second);
        assert!(!broker
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&key));
    }

    #[test]
    fn test_failed_cancellation_dispatch_does_not_retain_a_fence() {
        let broker = BrainRunnerBroker::default();
        let run_id = RunId(uuid::Uuid::new_v4());
        assert!(broker
            .request_run_cancellation("brain", lease(), run_id)
            .is_err());
        assert!(!broker
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .contains(&("brain".into(), run_id)));
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
            assert_eq!(request.interaction, RunnerProgramInteraction::Interactive);
            assert!(request.grant_ceiling.is_none());
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
                RunnerProgramInteraction::Interactive,
                None,
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
                RunnerProgramInteraction::Interactive,
                None,
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
                    continuation_messages: Vec::new(),
                    invocation_metadata: None,
                    turn_events: Vec::new(),
                    runtime_revision: 1,
                    checkpoint,
                    effect_journal: Vec::new(),
                    commit_ack: None,
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
                Some(crate::brain::store::ConnectionId(uuid::Uuid::new_v4())),
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

    #[tokio::test(start_paused = true)]
    async fn test_timeout_waits_for_exact_callback_quiescence_before_returning() {
        let broker = deadline_broker();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let dispatch_broker = broker.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    run_id,
                    1,
                    ProgramLanguage::Lisp,
                    "(say \"late\")".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
            panic!("expected program request")
        };
        tokio::time::advance(PROGRAM_DEADLINE).await;
        tokio::task::yield_now().await;
        assert!(!dispatch.is_finished());
        drop(request.response_tx);
        let error = dispatch.await.unwrap().unwrap_err();
        assert_dispatch_failure(
            &error,
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn test_late_callback_payload_retires_only_its_nonquiescent_generation() {
        let broker = deadline_broker();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let dispatch_broker = broker.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    run_id,
                    1,
                    ProgramLanguage::Lisp,
                    "late".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(request) = rx.recv().await.unwrap() else {
            panic!("expected program request")
        };

        tokio::time::advance(PROGRAM_DEADLINE).await;
        tokio::task::yield_now().await;
        request
            .response_tx
            .send(Ok(program_result("crossed cancellation")))
            .unwrap();
        assert_dispatch_failure(
            &dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(!broker.has_registration("brain", lease_id));
    }

    #[tokio::test]
    async fn test_dropped_callback_sender_and_daemon_receiver_retain_lane_until_settlement() {
        let broker = deadline_broker();
        let lease_id = lease();
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);

        let sender_drop_broker = broker.clone();
        let sender_drop = tokio::spawn(async move {
            sender_drop_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    RunId(uuid::Uuid::new_v4()),
                    1,
                    ProgramLanguage::Lisp,
                    "drop sender".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(sender_drop_request) = rx.recv().await.unwrap() else {
            panic!("expected sender-drop request")
        };
        drop(sender_drop_request.response_tx);
        assert_dispatch_failure(
            &sender_drop.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::ResponseDropped,
        );

        let receiver_drop_broker = broker.clone();
        let receiver_drop = tokio::spawn(async move {
            receiver_drop_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    RunId(uuid::Uuid::new_v4()),
                    2,
                    ProgramLanguage::Lisp,
                    "drop receiver".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(receiver_drop_request) = rx.recv().await.unwrap() else {
            panic!("expected receiver-drop request")
        };
        receiver_drop.abort();
        assert!(receiver_drop.await.unwrap_err().is_cancelled());
        tokio::task::yield_now().await;
        assert!(
            !receiver_drop_request.response_tx.is_closed(),
            "detached settlement owner dropped the physical callback receiver"
        );
        assert!(receiver_drop_request
            .response_tx
            .send(Ok(program_result("late")))
            .is_ok());
        tokio::task::yield_now().await;
        assert!(!broker.has_registration("brain", lease_id));
        assert!(!broker
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&("brain".into(), receiver_drop_request.run_id)));
    }

    #[tokio::test(start_paused = true)]
    async fn test_never_replying_turn_times_out_at_its_explicit_whole_turn_deadline() {
        let broker = deadline_broker();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let dispatch_broker = broker.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_broker
                .dispatch_turn(
                    "brain",
                    lease_id,
                    run_id,
                    1,
                    "wait forever".into(),
                    Vec::new(),
                    test_approval_audience(),
                    Some(ConnectionId(uuid::Uuid::new_v4())),
                )
                .await
        });
        let RunnerRequest::Turn(request) = rx.recv().await.unwrap() else {
            panic!("expected turn request")
        };
        tokio::time::advance(TURN_DEADLINE).await;
        tokio::task::yield_now().await;
        drop(request.response_tx);
        assert_dispatch_failure(
            &dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Turn,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn test_elapsed_deadline_wins_over_an_already_ready_callback_response() {
        let broker = deadline_broker();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, _rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let (registration, registration_request) = broker
            .registration("brain", lease_id, RunnerOperation::Program)
            .unwrap();
        let (_tracked_abort, inflight) =
            broker.track_inflight("brain", run_id, true, true).unwrap();
        let cancel = CancellationToken::new();
        let (response_tx, response_rx) = oneshot::channel();
        response_tx
            .send(Ok::<_, RunnerProgramError>(program_result("late")))
            .unwrap();
        let response_rx =
            broker.retain_callback_until_settled(response_rx, registration_request, inflight, None);
        let abort = CancellationToken::new();

        let error = broker
            .await_response(
                "brain",
                RunnerOperation::Program,
                registration,
                run_id,
                &cancel,
                tokio::time::Instant::now(),
                response_rx,
                Some(abort),
                anyhow::Error::new,
            )
            .await
            .unwrap_err();

        assert_dispatch_failure(
            &error,
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(cancel.is_cancelled());
    }

    #[tokio::test(start_paused = true)]
    async fn test_cancellation_and_memory_callbacks_are_independently_bounded() {
        let broker = deadline_broker();
        let lease_id = lease();
        let cancel_run_id = RunId(uuid::Uuid::new_v4());
        let memory_run_id = RunId(uuid::Uuid::new_v4());
        let brain_id = crate::brain::store::BrainId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);

        let cancel_broker = broker.clone();
        let cancel_dispatch = tokio::spawn(async move {
            cancel_broker
                .cancel_run("brain", lease_id, cancel_run_id)
                .await
        });
        let RunnerRequest::Cancel(cancel_request) = rx.recv().await.unwrap() else {
            panic!("expected cancellation request")
        };
        tokio::time::advance(CANCEL_DEADLINE).await;
        tokio::task::yield_now().await;
        drop(cancel_request.response_tx);
        assert_dispatch_failure(
            &cancel_dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Cancel,
            RunnerDispatchFailure::TimedOut,
        );

        let memory_broker = broker.clone();
        let memory_dispatch = tokio::spawn(async move {
            memory_broker
                .project_memory(
                    "brain",
                    lease_id,
                    brain_id,
                    memory_run_id,
                    2,
                    "remember".into(),
                    "remembered".into(),
                )
                .await
        });
        let RunnerRequest::ProjectMemory(memory_request) = rx.recv().await.unwrap() else {
            panic!("expected memory request")
        };
        tokio::time::advance(MEMORY_DEADLINE).await;
        tokio::task::yield_now().await;
        drop(memory_request.response_tx);
        assert_dispatch_failure(
            &memory_dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::ProjectMemory,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn test_one_timeout_does_not_invalidate_later_work_on_the_same_runner() {
        let broker = deadline_broker();
        let lease_id = lease();
        let timed_out_run = RunId(uuid::Uuid::new_v4());
        let healthy_run = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);

        let first_broker = broker.clone();
        let first = tokio::spawn(async move {
            first_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    timed_out_run,
                    1,
                    ProgramLanguage::Lisp,
                    "stuck".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(stuck) = rx.recv().await.unwrap() else {
            panic!("expected first program request")
        };
        tokio::time::advance(PROGRAM_DEADLINE).await;
        tokio::task::yield_now().await;
        assert!(!first.is_finished());
        drop(stuck.response_tx);
        assert_dispatch_failure(
            &first.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );

        let second_broker = broker.clone();
        let second = tokio::spawn(async move {
            second_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    healthy_run,
                    2,
                    ProgramLanguage::Lisp,
                    "healthy".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(healthy) = rx.recv().await.unwrap() else {
            panic!("expected second program request")
        };
        healthy
            .response_tx
            .send(Ok(program_result("healthy")))
            .unwrap();
        assert_eq!(second.await.unwrap().unwrap().output, "healthy");
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn test_cleanup_timeout_retires_only_old_generation_and_fresh_registration_runs() {
        let broker = deadline_broker();
        let old_lease = lease();
        // A same-authority renewal is still an exact successor generation;
        // retiring the wedged generation must not discard it.
        let new_lease = old_lease;
        let old_run = RunId(uuid::Uuid::new_v4());
        let new_run = RunId(uuid::Uuid::new_v4());
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        broker.register("brain", old_lease, old_tx);
        let old_broker = broker.clone();
        let old_dispatch = tokio::spawn(async move {
            old_broker
                .dispatch_program(
                    "brain",
                    old_lease,
                    old_run,
                    1,
                    ProgramLanguage::Lisp,
                    "never settles".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(old_request) = old_rx.recv().await.unwrap() else {
            panic!("expected old program request")
        };

        tokio::time::advance(PROGRAM_DEADLINE).await;
        tokio::task::yield_now().await;
        assert!(!old_dispatch.is_finished());
        tokio::time::advance(CALLBACK_CLEANUP).await;
        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
        let new_registration = broker.register("brain", new_lease, new_tx);
        assert_dispatch_failure(
            &old_dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(!broker.has_registration("brain", old_lease));
        assert!(!broker.has_registration("brain", new_lease));
        assert!(broker
            .pending_registrations
            .read()
            .expect("runner pending-registration lock poisoned")
            .contains_key("brain"));
        let old_key = ("brain".to_string(), old_run);
        assert!(broker
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&old_key));
        drop(old_request);
        broker
            .wait_registration_active("brain", new_registration)
            .await
            .unwrap();
        assert!(broker.has_registration("brain", new_lease));
        assert!(!broker
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&old_key));
        let new_broker = broker.clone();
        let new_dispatch = tokio::spawn(async move {
            new_broker
                .dispatch_program(
                    "brain",
                    new_lease,
                    new_run,
                    2,
                    ProgramLanguage::Lisp,
                    "fresh".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(new_request) = new_rx.recv().await.unwrap() else {
            panic!("expected fresh request")
        };
        new_request
            .response_tx
            .send(Ok(program_result("fresh")))
            .unwrap();
        assert_eq!(new_dispatch.await.unwrap().unwrap().output, "fresh");
    }

    #[tokio::test]
    async fn test_same_connection_lease_renewal_preserves_inflight_and_routes_future_once() {
        let broker = deadline_broker();
        let connection_id = uuid::Uuid::new_v4();
        let lease_id = lease();
        broker
            .claim_connection_identity(connection_id, "runner@box.local")
            .unwrap();
        broker
            .claim_connection_lease(connection_id, "brain", lease_id)
            .unwrap();
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        broker
            .register_for_connection(connection_id, "brain", lease_id, old_tx)
            .unwrap();

        let old_run = RunId(uuid::Uuid::new_v4());
        let old_broker = broker.clone();
        let old_dispatch = tokio::spawn(async move {
            old_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    old_run,
                    1,
                    ProgramLanguage::Lisp,
                    "in flight".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(old_request) = old_rx.recv().await.unwrap() else {
            panic!("expected in-flight request")
        };

        let (renewed_tx, mut renewed_rx) = mpsc::unbounded_channel();
        let renewed_id = broker
            .register_for_connection(connection_id, "brain", lease_id, renewed_tx)
            .unwrap();
        let active_broker = broker.clone();
        let mut renewed_active = Box::pin(async move {
            active_broker
                .wait_registration_active("brain", renewed_id)
                .await
        });
        assert!(
            matches!(
                futures::poll!(&mut renewed_active),
                std::task::Poll::Pending
            ),
            "renewed generation became dispatchable before its predecessor quiesced"
        );

        old_request
            .response_tx
            .send(Ok(program_result("old completed")))
            .unwrap();
        assert_eq!(old_dispatch.await.unwrap().unwrap().output, "old completed");
        renewed_active.await.unwrap();

        let future_run = RunId(uuid::Uuid::new_v4());
        let future_broker = broker.clone();
        let future_dispatch = tokio::spawn(async move {
            future_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    future_run,
                    2,
                    ProgramLanguage::Lisp,
                    "future".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(future_request) = renewed_rx.recv().await.unwrap() else {
            panic!("expected future request on renewed callback")
        };
        assert!(old_rx.try_recv().is_err());
        future_request
            .response_tx
            .send(Ok(program_result("future completed")))
            .unwrap();
        assert_eq!(
            future_dispatch.await.unwrap().unwrap().output,
            "future completed"
        );
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test]
    async fn test_replacement_generation_wakes_old_dispatch_without_losing_new_identity() {
        let broker = deadline_broker();
        let old_lease = lease();
        let new_lease = lease();
        let old_run = RunId(uuid::Uuid::new_v4());
        let new_run = RunId(uuid::Uuid::new_v4());
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        broker.register("brain", old_lease, old_tx);

        let old_broker = broker.clone();
        let old_dispatch = tokio::spawn(async move {
            old_broker
                .dispatch_program(
                    "brain",
                    old_lease,
                    old_run,
                    1,
                    ProgramLanguage::Lisp,
                    "old".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(old_request) = old_rx.recv().await.unwrap() else {
            panic!("expected old request")
        };

        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
        broker.register("brain", new_lease, new_tx);
        tokio::task::yield_now().await;
        assert!(
            !old_dispatch.is_finished(),
            "replacement must not release the old dispatch before callback cleanup"
        );
        old_request
            .response_tx
            .send(Ok(program_result("late old")))
            .unwrap();
        assert_dispatch_failure(
            &old_dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::GenerationInvalidated,
        );
        assert!(!broker.invalidate_lease("brain", old_lease));
        assert!(broker.has_registration("brain", new_lease));

        let new_broker = broker.clone();
        let new_dispatch = tokio::spawn(async move {
            new_broker
                .dispatch_program(
                    "brain",
                    new_lease,
                    new_run,
                    2,
                    ProgramLanguage::Lisp,
                    "new".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(new_request) = new_rx.recv().await.unwrap() else {
            panic!("expected replacement request")
        };
        new_request
            .response_tx
            .send(Ok(program_result("new")))
            .unwrap();
        assert_eq!(new_dispatch.await.unwrap().unwrap().output, "new");
    }
}
