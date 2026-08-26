//! Thread-safe dispatch boundary between daemon request handlers and the
//! frontend process that owns one named Brain's execution environment.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, watch};

use crate::brain::store::{AttachmentId, ConnectionId, ProgramLanguage, RunId, RunnerLeaseId};

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
    /// Cancel only this callback when its daemon-side owner stops waiting.
    pub cancel: tokio_util::sync::CancellationToken,
    pub response_tx: oneshot::Sender<Result<usize, String>>,
}

#[derive(Debug)]
pub struct RunnerCancelRequest {
    pub brain: String,
    pub run_id: RunId,
    /// Cancel only this callback when its daemon-side owner stops waiting.
    pub cancel: tokio_util::sync::CancellationToken,
    pub response_tx: oneshot::Sender<Result<bool, String>>,
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
    /// Cancel only this callback when its daemon-side owner stops waiting.
    pub cancel: tokio_util::sync::CancellationToken,
    pub response_tx: oneshot::Sender<Result<RunnerProgramResult, RunnerProgramError>>,
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
    /// One authoritative hard deadline for the whole turn. Reverse approval
    /// windows are clamped to this same monotonic/wall-clock boundary.
    pub hard_deadline: tokio::time::Instant,
    pub hard_deadline_ms: u64,
    /// Reverse approval bridge installed by the Cap'n Proto client adapter.
    /// Daemon-side broker requests leave this unset until they cross IPC.
    pub approval_tx: Option<mpsc::UnboundedSender<RunnerApprovalRequest>>,
    /// Cancel only this callback when its daemon-side owner stops waiting.
    pub cancel: tokio_util::sync::CancellationToken,
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
            RunnerDispatchFailure::NoCallback => "has no connected callback",
            RunnerDispatchFailure::StaleLease => "callback belongs to a stale lease",
            RunnerDispatchFailure::Disconnected => "callback disconnected",
            RunnerDispatchFailure::ResponseDropped => "dropped its response",
            RunnerDispatchFailure::TimedOut => "timed out",
            RunnerDispatchFailure::GenerationInvalidated => "callback generation was invalidated",
            RunnerDispatchFailure::RunAborted => "was aborted for this run",
        };
        Self {
            brain: brain.to_string(),
            operation,
            failure,
            detail,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerDeadlines {
    pub program: Duration,
    /// Hard total turn lifetime, including provider generation, one-shot
    /// repair, tool execution, and approval suspension. Every approval expiry
    /// is clamped to the remaining portion of this same boundary.
    pub turn: Duration,
    pub cancel: Duration,
    pub project_memory: Duration,
}

impl Default for RunnerDeadlines {
    fn default() -> Self {
        Self {
            program: Duration::from_secs(5 * 60),
            turn: Duration::from_secs(20 * 60),
            cancel: Duration::from_secs(10),
            project_memory: Duration::from_secs(60),
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
    tx: mpsc::UnboundedSender<RunnerRequest>,
    active: watch::Sender<bool>,
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
#[derive(Clone)]
pub struct BrainRunnerBroker {
    registrations: Arc<RwLock<HashMap<String, Registration>>>,
    connection_authority: Arc<Mutex<ConnectionAuthority>>,
    inflight: Arc<Mutex<HashMap<(String, RunId), HashMap<uuid::Uuid, oneshot::Sender<()>>>>>,
    cancelled_before_dispatch: Arc<Mutex<std::collections::HashSet<(String, RunId)>>>,
    deadlines: RunnerDeadlines,
}

impl Default for BrainRunnerBroker {
    fn default() -> Self {
        Self::with_deadlines(RunnerDeadlines::default())
    }
}

struct InflightRequest {
    broker: BrainRunnerBroker,
    key: (String, RunId),
    id: uuid::Uuid,
}

impl Drop for InflightRequest {
    fn drop(&mut self) {
        let mut inflight = self
            .broker
            .inflight
            .lock()
            .expect("runner inflight lock poisoned");
        if let Some(requests) = inflight.get_mut(&self.key) {
            requests.remove(&self.id);
            if requests.is_empty() {
                inflight.remove(&self.key);
            }
        }
    }
}

impl BrainRunnerBroker {
    pub fn with_deadlines(deadlines: RunnerDeadlines) -> Self {
        Self {
            registrations: Arc::default(),
            connection_authority: Arc::default(),
            inflight: Arc::default(),
            cancelled_before_dispatch: Arc::default(),
            deadlines,
        }
    }

    fn track_inflight(
        &self,
        brain: &str,
        run_id: RunId,
    ) -> (oneshot::Receiver<()>, InflightRequest) {
        let key = (brain.to_string(), run_id);
        let id = uuid::Uuid::new_v4();
        let (abort_tx, abort_rx) = oneshot::channel();
        self.inflight
            .lock()
            .expect("runner inflight lock poisoned")
            .entry(key.clone())
            .or_default()
            .insert(id, abort_tx);
        (
            abort_rx,
            InflightRequest {
                broker: self.clone(),
                key,
                id,
            },
        )
    }

    /// Stop only daemon waits associated with one durable run. This never
    /// revokes the runner lease or any other attachment's work.
    pub(crate) fn abort_run(&self, brain: &str, run_id: RunId) {
        let requests = self
            .inflight
            .lock()
            .expect("runner inflight lock poisoned")
            .remove(&(brain.to_string(), run_id));
        drop(requests);
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
        self.fence_run_cancellation(brain, run_id);
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .with_context(|| format!("named Brain '{brain}' has no connected runner callback"))?;
        anyhow::ensure!(
            registration.lease_id == lease_id,
            "named Brain '{brain}' runner callback belongs to a stale lease"
        );
        let (response_tx, _response_rx) = oneshot::channel();
        let cancel = tokio_util::sync::CancellationToken::new();
        registration
            .tx
            .send(RunnerRequest::Cancel(RunnerCancelRequest {
                brain: brain.to_string(),
                run_id,
                cancel,
                response_tx,
            }))
            .map_err(|_| anyhow::anyhow!("named Brain '{brain}' runner callback disconnected"))
    }

    pub(crate) fn fence_run_cancellation(&self, brain: &str, run_id: RunId) {
        self.cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned")
            .insert((brain.to_string(), run_id));
    }

    pub fn register(
        &self,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<RunnerRequest>,
    ) -> RunnerRegistrationId {
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        let brain = brain.into();
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        if let Some(replaced) = registrations.get(&brain) {
            replaced.active.send_replace(false);
        }
        registrations.insert(
            brain,
            Registration {
                id,
                lease_id,
                connection_id: None,
                tx,
                active: watch::channel(true).0,
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
        let brain = brain.into();
        self.require_connection_lease(connection_id, &brain, lease_id)?;
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        if let Some(replaced) = registrations.get(&brain) {
            replaced.active.send_replace(false);
        }
        registrations.insert(
            brain,
            Registration {
                id,
                lease_id,
                connection_id: Some(connection_id),
                tx,
                active: watch::channel(true).0,
            },
        );
        Ok(id)
    }

    pub(crate) fn disconnect_connection(
        &self,
        connection_id: uuid::Uuid,
    ) -> Vec<(String, AttachmentId, ConnectionId)> {
        let mut authority = self
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
            .retain(|_, registration| {
                let keep = registration.connection_id != Some(connection_id);
                if !keep {
                    registration.active.send_replace(false);
                }
                keep
            });
        attachments
    }

    /// Remove a registration only if it is still the connection that created
    /// it. A late disconnect must not remove a replacement runner callback.
    pub fn unregister(&self, brain: &str, id: RunnerRegistrationId) {
        let mut registrations = self
            .registrations
            .write()
            .expect("runner broker lock poisoned");
        if registrations.get(brain).is_some_and(|entry| entry.id == id) {
            if let Some(registration) = registrations.remove(brain) {
                registration.active.send_replace(false);
            }
        }
    }

    fn registration(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        operation: RunnerOperation,
    ) -> Result<Registration> {
        let registration = self
            .registrations
            .read()
            .expect("runner broker lock poisoned")
            .get(brain)
            .cloned()
            .ok_or_else(|| {
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
        Ok(registration)
    }

    fn disconnect_registration(
        &self,
        brain: &str,
        operation: RunnerOperation,
        registration: &Registration,
    ) -> anyhow::Error {
        registration.active.send_replace(false);
        self.unregister(brain, registration.id);
        RunnerDispatchError::new(brain, operation, RunnerDispatchFailure::Disconnected).into()
    }

    fn cancel_exact_callback(
        &self,
        brain: &str,
        run_id: RunId,
        cancel: &tokio_util::sync::CancellationToken,
    ) {
        self.fence_run_cancellation(brain, run_id);
        cancel.cancel();
    }

    async fn await_response<T, E>(
        &self,
        brain: &str,
        run_id: RunId,
        operation: RunnerOperation,
        registration: &Registration,
        cancel: &tokio_util::sync::CancellationToken,
        deadline: tokio::time::Instant,
        response_rx: oneshot::Receiver<Result<T, E>>,
        abort_rx: oneshot::Receiver<()>,
        map_remote_error: impl FnOnce(E) -> anyhow::Error,
    ) -> Result<T> {
        let mut active = registration.active.subscribe();
        if !*active.borrow() {
            return Err(RunnerDispatchError::new(
                brain,
                operation,
                RunnerDispatchFailure::GenerationInvalidated,
            )
            .into());
        }
        let response = tokio::select! {
            biased;
            _ = abort_rx => {
                self.cancel_exact_callback(brain, run_id, cancel);
                return Err(RunnerDispatchError::new(
                    brain, operation, RunnerDispatchFailure::RunAborted,
                ).into());
            }
            _ = active.changed() => {
                self.cancel_exact_callback(brain, run_id, cancel);
                return Err(RunnerDispatchError::new(
                    brain, operation, RunnerDispatchFailure::GenerationInvalidated,
                ).into());
            }
            response = tokio::time::timeout_at(deadline, response_rx) => response,
        };
        let response = match response {
            Err(_) => {
                self.cancel_exact_callback(brain, run_id, cancel);
                return Err(RunnerDispatchError::new(
                    brain,
                    operation,
                    RunnerDispatchFailure::TimedOut,
                )
                .into());
            }
            Ok(Err(_)) => {
                self.cancel_exact_callback(brain, run_id, cancel);
                return Err(RunnerDispatchError::new(
                    brain,
                    operation,
                    RunnerDispatchFailure::ResponseDropped,
                )
                .into());
            }
            Ok(Ok(response)) => response,
        };
        if !*registration.active.borrow() {
            return Err(RunnerDispatchError::new(
                brain,
                operation,
                RunnerDispatchFailure::GenerationInvalidated,
            )
            .into());
        }
        response.map_err(map_remote_error)
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
        let registration = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, _inflight) = self.track_inflight(brain, run_id);
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let dispatch_fence = self
                .cancelled_before_dispatch
                .lock()
                .expect("runner cancellation-fence lock poisoned");
            anyhow::ensure!(
                !dispatch_fence.contains(&(brain.to_string(), run_id)),
                "named Brain run cancelled before runner dispatch"
            );
            registration
                .tx
                .send(RunnerRequest::Program(RunnerProgramRequest {
                    brain: brain.to_string(),
                    run_id,
                    request_seq,
                    language,
                    source,
                    interaction,
                    grant_ceiling,
                    control_tx: None,
                    cancel: cancel.clone(),
                    response_tx,
                }))
                .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        }
        self.await_response(
            brain,
            run_id,
            operation,
            &registration,
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
        let duration = self.deadlines.for_operation(operation);
        let hard_deadline = tokio::time::Instant::now() + duration;
        let hard_deadline_ms = crate::brain::store::unix_millis()
            .saturating_add(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        let registration = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, _inflight) = self.track_inflight(brain, run_id);
        let cancel = tokio_util::sync::CancellationToken::new();
        {
            let dispatch_fence = self
                .cancelled_before_dispatch
                .lock()
                .expect("runner cancellation-fence lock poisoned");
            anyhow::ensure!(
                !dispatch_fence.contains(&(brain.to_string(), run_id)),
                "named Brain run cancelled before runner dispatch"
            );
            registration
                .tx
                .send(RunnerRequest::Turn(RunnerTurnRequest {
                    brain: brain.to_string(),
                    run_id,
                    request_seq,
                    prompt,
                    context,
                    approval_audience,
                    approval_connection_id,
                    hard_deadline,
                    hard_deadline_ms,
                    approval_tx: None,
                    cancel: cancel.clone(),
                    response_tx,
                }))
                .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        }
        self.await_response(
            brain,
            run_id,
            operation,
            &registration,
            &cancel,
            hard_deadline,
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
        let registration = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, _inflight) = self.track_inflight(brain, run_id);
        let cancel = tokio_util::sync::CancellationToken::new();
        registration
            .tx
            .send(RunnerRequest::Cancel(RunnerCancelRequest {
                brain: brain.to_string(),
                run_id,
                cancel: cancel.clone(),
                response_tx,
            }))
            .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        self.await_response(
            brain,
            run_id,
            operation,
            &registration,
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
        let registration = self.registration(brain, lease_id, operation)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, _inflight) = self.track_inflight(brain, run_id);
        let cancel = tokio_util::sync::CancellationToken::new();
        registration
            .tx
            .send(RunnerRequest::ProjectMemory(
                RunnerMemoryProjectionRequest {
                    brain_id,
                    brain: brain.to_string(),
                    run_id,
                    request_seq,
                    prompt,
                    source,
                    cancel: cancel.clone(),
                    response_tx,
                },
            ))
            .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        self.await_response(
            brain,
            run_id,
            operation,
            &registration,
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

    fn lease() -> RunnerLeaseId {
        RunnerLeaseId(uuid::Uuid::new_v4())
    }

    fn deadline_broker() -> BrainRunnerBroker {
        BrainRunnerBroker::with_deadlines(RunnerDeadlines {
            program: PROGRAM_DEADLINE,
            turn: TURN_DEADLINE,
            cancel: CANCEL_DEADLINE,
            project_memory: MEMORY_DEADLINE,
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

        let disconnected = broker.disconnect_connection(owner);
        assert_eq!(
            disconnected,
            vec![("brain".to_string(), attachment_id, attachment_connection_id,)]
        );
        assert!(!broker.has_registration("brain", lease_id));
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
    async fn never_replying_program_times_out_and_cancels_only_its_exact_request() {
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
        let cancel = request.cancel.clone();

        tokio::time::advance(PROGRAM_DEADLINE).await;
        let error = dispatch.await.unwrap().unwrap_err();
        assert_dispatch_failure(
            &error,
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(cancel.is_cancelled());
        assert!(request
            .response_tx
            .send(Ok(program_result("too late")))
            .is_err());
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn never_replying_turn_times_out_at_its_explicit_whole_turn_deadline() {
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
        assert_dispatch_failure(
            &dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Turn,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(request.cancel.is_cancelled());
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_callback_is_bounded_without_invalidating_the_runner() {
        let broker = deadline_broker();
        let lease_id = lease();
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let dispatch_broker = broker.clone();
        let dispatch =
            tokio::spawn(
                async move { dispatch_broker.cancel_run("brain", lease_id, run_id).await },
            );
        let RunnerRequest::Cancel(request) = rx.recv().await.unwrap() else {
            panic!("expected cancellation request")
        };

        tokio::time::advance(CANCEL_DEADLINE).await;
        assert_dispatch_failure(
            &dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::Cancel,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(request.cancel.is_cancelled());
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn memory_projection_callback_is_bounded_and_late_reply_is_fenced() {
        let broker = deadline_broker();
        let lease_id = lease();
        let brain_id = crate::brain::store::BrainId(uuid::Uuid::new_v4());
        let run_id = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);
        let dispatch_broker = broker.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_broker
                .project_memory(
                    "brain",
                    lease_id,
                    brain_id,
                    run_id,
                    1,
                    "remember".into(),
                    "(say \"remembered\")".into(),
                )
                .await
        });
        let RunnerRequest::ProjectMemory(request) = rx.recv().await.unwrap() else {
            panic!("expected memory request")
        };

        tokio::time::advance(MEMORY_DEADLINE).await;
        assert_dispatch_failure(
            &dispatch.await.unwrap().unwrap_err(),
            RunnerOperation::ProjectMemory,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(request.cancel.is_cancelled());
        assert!(request.response_tx.send(Ok(2)).is_err());
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn one_timeout_does_not_invalidate_an_unrelated_callback_on_the_same_runner() {
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
                    "(say \"stuck\")".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(stuck) = rx.recv().await.unwrap() else {
            panic!("expected first program request")
        };
        tokio::time::advance(PROGRAM_DEADLINE).await;
        assert_dispatch_failure(
            &first.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(stuck.cancel.is_cancelled());

        let second_broker = broker.clone();
        let second = tokio::spawn(async move {
            second_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    healthy_run,
                    2,
                    ProgramLanguage::Lisp,
                    "(say \"healthy\")".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(healthy) = rx.recv().await.unwrap() else {
            panic!("expected second program request")
        };
        assert_eq!(healthy.run_id, healthy_run);
        healthy
            .response_tx
            .send(Ok(program_result("healthy")))
            .unwrap();
        assert_eq!(second.await.unwrap().unwrap().output, "healthy");
        assert!(broker.has_registration("brain", lease_id));
    }

    #[tokio::test(start_paused = true)]
    async fn abort_run_fences_and_cancels_only_the_exact_inflight_run() {
        let broker = deadline_broker();
        let lease_id = lease();
        let aborted_run = RunId(uuid::Uuid::new_v4());
        let healthy_run = RunId(uuid::Uuid::new_v4());
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker.register("brain", lease_id, tx);

        let aborted_broker = broker.clone();
        let aborted = tokio::spawn(async move {
            aborted_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    aborted_run,
                    1,
                    ProgramLanguage::Lisp,
                    "(say \"abort\")".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(aborted_request) = rx.recv().await.unwrap() else {
            panic!("expected aborted program request")
        };
        broker.abort_run("brain", aborted_run);
        assert_dispatch_failure(
            &aborted.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::RunAborted,
        );
        assert!(aborted_request.cancel.is_cancelled());

        let healthy_broker = broker.clone();
        let healthy = tokio::spawn(async move {
            healthy_broker
                .dispatch_program(
                    "brain",
                    lease_id,
                    healthy_run,
                    2,
                    ProgramLanguage::Lisp,
                    "(say \"healthy\")".into(),
                    RunnerProgramInteraction::Interactive,
                    None,
                )
                .await
        });
        let RunnerRequest::Program(healthy_request) = rx.recv().await.unwrap() else {
            panic!("expected healthy program request")
        };
        healthy_request
            .response_tx
            .send(Ok(program_result("healthy")))
            .unwrap();
        assert_eq!(healthy.await.unwrap().unwrap().output, "healthy");
    }
}
