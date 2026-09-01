//! Thread-safe dispatch boundary between daemon request handlers and the
//! frontend process that owns one named Brain's execution environment.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::brain::store::{AttachmentId, ConnectionId, ProgramLanguage, RunId, RunnerLeaseId};

/// Kernel-derived identity of the process at the other end of one Unix IPC
/// connection. The start token prevents a recycled PID from inheriting an
/// earlier process's quarantine.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
pub(crate) struct RunnerProcessIdentity {
    pub(crate) pid: u32,
    pub(crate) start_token: u64,
}

impl RunnerProcessIdentity {
    pub(crate) fn for_pid(pid: u32) -> Result<Self> {
        let start_token = process_start_token(pid)?
            .with_context(|| format!("runner peer process {pid} no longer exists"))?;
        Ok(Self { pid, start_token })
    }

    fn still_exists(self) -> Result<bool> {
        Ok(process_start_token(self.pid)?.is_some_and(|token| token == self.start_token))
    }
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Result<Option<u64>> {
    let record = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(record) => record,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let suffix = record
        .rsplit_once(") ")
        .context("runner peer process record is malformed")?
        .1;
    // The suffix starts at field 3 (state); starttime is field 22.
    let token = suffix
        .split_whitespace()
        .nth(19)
        .context("runner peer process record has no start time")?
        .parse()?;
    Ok(Some(token))
}

#[cfg(target_os = "macos")]
fn process_start_token(pid: u32) -> Result<Option<u64>> {
    let mut information: nix::libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let expected = std::mem::size_of_val(&information) as nix::libc::c_int;
    let length = unsafe {
        nix::libc::proc_pidinfo(
            pid as nix::libc::c_int,
            nix::libc::PROC_PIDTBSDINFO,
            0,
            (&mut information as *mut nix::libc::proc_bsdinfo).cast(),
            expected,
        )
    };
    if length == expected {
        let seconds = u64::try_from(information.pbi_start_tvsec)
            .context("runner peer process start seconds are invalid")?;
        let micros = u64::try_from(information.pbi_start_tvusec)
            .context("runner peer process start microseconds are invalid")?;
        return Ok(Some(
            seconds.saturating_mul(1_000_000).saturating_add(micros),
        ));
    }
    let alive = unsafe { nix::libc::kill(pid as nix::libc::pid_t, 0) } == 0;
    if !alive && std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::ESRCH) {
        return Ok(None);
    }
    anyhow::bail!("could not read runner peer process start identity")
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_token(_pid: u32) -> Result<Option<u64>> {
    anyhow::bail!("runner process identity is unsupported on this Unix platform")
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
struct RunnerProcessLedger {
    /// Confirmed forced-ejection identities. These remain terminal until the
    /// kernel proves that exact PID/start pair no longer exists.
    quarantined: HashSet<RunnerProcessIdentity>,
    /// Successfully registered callback processes whose graceful release has
    /// not yet been fsynced. On restart these are uncertain, not confirmed
    /// quarantines, but both states must reject a still-live identity.
    runner_bearing: HashSet<RunnerProcessIdentity>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum RunnerProcessLedgerFile {
    Current(RunnerProcessLedger),
    Legacy(Vec<RunnerProcessIdentity>),
}

#[derive(Clone, Default)]
struct RunnerQuarantineStore {
    path: Option<Arc<PathBuf>>,
    ledger: Arc<Mutex<RunnerProcessLedger>>,
    #[cfg(test)]
    fail_quarantine_promotions: Arc<std::sync::atomic::AtomicUsize>,
}

impl RunnerQuarantineStore {
    fn at(
        path: PathBuf,
    ) -> Result<(
        Self,
        HashSet<RunnerProcessIdentity>,
        HashSet<RunnerProcessIdentity>,
    )> {
        let mut ledger = Self::load(&path)?;
        let before = (ledger.quarantined.len(), ledger.runner_bearing.len());
        ledger
            .quarantined
            .retain(|identity| identity.still_exists().unwrap_or(true));
        ledger
            .runner_bearing
            .retain(|identity| identity.still_exists().unwrap_or(true));
        let store = Self {
            path: Some(Arc::new(path)),
            ledger: Arc::new(Mutex::new(ledger.clone())),
            #[cfg(test)]
            fail_quarantine_promotions: Arc::default(),
        };
        if before != (ledger.quarantined.len(), ledger.runner_bearing.len()) {
            store.persist(&ledger)?;
        }
        Ok((store, ledger.quarantined, ledger.runner_bearing))
    }

    fn load(path: &std::path::Path) -> Result<RunnerProcessLedger> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RunnerProcessLedger::default());
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        match serde_json::from_slice::<RunnerProcessLedgerFile>(&bytes)
            .context("parse runner process quarantine ledger")?
        {
            RunnerProcessLedgerFile::Current(ledger) => Ok(ledger),
            RunnerProcessLedgerFile::Legacy(entries) => Ok(RunnerProcessLedger {
                quarantined: entries.into_iter().collect(),
                runner_bearing: HashSet::new(),
            }),
        }
    }

    fn persist(&self, ledger: &RunnerProcessLedger) -> Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let parent = path
            .parent()
            .context("runner quarantine path has no parent")?;
        std::fs::create_dir_all(parent)?;
        let bytes = serde_json::to_vec(ledger)?;
        let temporary = parent.join(format!(
            ".runner-process-quarantine-{}.tmp",
            uuid::Uuid::new_v4().simple()
        ));
        let result = (|| -> Result<()> {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)?;
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.with_context(|| format!("persist runner process quarantine at {}", path.display()))
    }

    /// Persist the fail-closed runner-bearing marker before registration can
    /// be acknowledged to the frontend.
    fn mark_runner_bearing(&self, identity: RunnerProcessIdentity) -> Result<()> {
        let mut ledger = self
            .ledger
            .lock()
            .expect("runner process ledger lock poisoned");
        if !ledger.runner_bearing.insert(identity) {
            return Ok(());
        }
        if let Err(error) = self.persist(&ledger) {
            ledger.runner_bearing.remove(&identity);
            return Err(error);
        }
        Ok(())
    }

    /// Promote an already durable runner-bearing identity to a confirmed
    /// quarantine. If this write fails, the prior fsynced runner-bearing
    /// marker remains authoritative across restart.
    fn quarantine(&self, identity: RunnerProcessIdentity) -> Result<()> {
        #[cfg(test)]
        if self
            .fail_quarantine_promotions
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            anyhow::bail!("injected runner quarantine promotion persistence failure");
        }
        let mut ledger = self
            .ledger
            .lock()
            .expect("runner process ledger lock poisoned");
        if !ledger.quarantined.insert(identity) {
            return Ok(());
        }
        if let Err(error) = self.persist(&ledger) {
            ledger.quarantined.remove(&identity);
            return Err(error);
        }
        Ok(())
    }

    #[cfg(test)]
    fn fail_next_quarantine_promotion(&self) {
        self.fail_quarantine_promotions.store(1, Ordering::SeqCst);
    }

    /// Release uncertainty only after callback quiescence and durable effect
    /// reconciliation. A failed fsync leaves both live and restarted brokers
    /// fail-closed for this exact OS identity.
    fn release_runner_bearing(
        &self,
        identity: RunnerProcessIdentity,
        confirmed_quarantine: bool,
    ) -> Result<()> {
        let mut ledger = self
            .ledger
            .lock()
            .expect("runner process ledger lock poisoned");
        let previous = ledger.clone();
        if confirmed_quarantine {
            ledger.quarantined.insert(identity);
        }
        if !ledger.runner_bearing.remove(&identity)
            && (!confirmed_quarantine || previous.quarantined.contains(&identity))
        {
            return Ok(());
        }
        if let Err(error) = self.persist(&ledger) {
            *ledger = previous;
            return Err(error);
        }
        Ok(())
    }
}

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
    pub(crate) dispatch_gate: Arc<DispatchGate>,
    pub(crate) run_dispatch_gate: Arc<RunDispatchGate>,
    pub(crate) enforce_run_fence: bool,
}

/// Serializes exact-generation invalidation and cancellation with the final
/// non-awaiting IPC `send()`. The broker lock order is registration map, then
/// this gate; cancellation that does not inspect the map takes only the gate.
#[derive(Debug, Default)]
pub(crate) struct DispatchGate(Mutex<()>);

#[derive(Debug, Default)]
struct RunDispatchState {
    fenced: bool,
}

/// Per-admitted-callback gate shared by durable cancellation installation,
/// abort, and the final IPC send. It has no map backedge, so final dispatch
/// may safely lock generation then run while fence insertion holds the
/// durable-fence map, inflight map, then every exact run gate.
#[derive(Debug, Default)]
pub(crate) struct RunDispatchGate(Mutex<RunDispatchState>);

#[derive(Debug)]
pub struct RunnerMemoryProjectionRequest {
    pub brain_id: crate::brain::store::BrainId,
    pub brain: String,
    pub run_id: RunId,
    pub request_seq: u64,
    pub prompt: String,
    /// The rendered output the user saw.
    pub rendered: String,
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
        rendered: String,
        response_tx: oneshot::Sender<Result<usize, String>>,
    ) -> Self {
        Self {
            brain_id,
            brain,
            run_id,
            request_seq,
            prompt,
            rendered,
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
    dispatch_gate: Arc<DispatchGate>,
    in_flight: Arc<AtomicUsize>,
}

impl Registration {
    fn invalidate(&self) {
        let _admission = self
            .dispatch_gate
            .0
            .lock()
            .expect("runner dispatch gate poisoned");
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
    fn compatible_lifetime(&self) -> Option<mpsc::UnboundedSender<RunnerRequest>> {
        match self {
            Self::Compatible(tx) => Some(tx.clone()),
            Self::Bounded(_) => None,
        }
    }

    fn send(
        &self,
        request: RunnerRequest,
        cancel: CancellationToken,
        generation_cancel: CancellationToken,
        deadline: tokio::time::Instant,
        cleanup_timeout: Duration,
        dispatch_gate: Arc<DispatchGate>,
        run_dispatch_gate: Arc<RunDispatchGate>,
        enforce_run_fence: bool,
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
                    dispatch_gate,
                    run_dispatch_gate,
                    enforce_run_fence,
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
    runner_process_owners: HashMap<RunnerProcessIdentity, uuid::Uuid>,
    connection_runner_processes: HashMap<uuid::Uuid, RunnerProcessIdentity>,
    quarantined_runner_processes: HashSet<RunnerProcessIdentity>,
    uncertain_runner_processes: HashSet<RunnerProcessIdentity>,
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
    eject: CancellationToken,
    transport_closed: CancellationToken,
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

    pub(crate) async fn ejection_requested(&self) {
        self.eject.cancelled().await;
    }

    pub(crate) fn publish_ejection(&self) {
        self.eject.cancel();
    }

    pub(crate) async fn wait_transport_closed(&self) {
        self.transport_closed.cancelled().await;
    }

    pub(crate) fn mark_transport_closed(&self) {
        self.transport_closed.cancel();
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
    quarantine_store: RunnerQuarantineStore,
    inflight: Arc<Mutex<HashMap<(String, RunId), HashMap<uuid::Uuid, InflightCancellation>>>>,
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
    #[cfg(test)]
    dispatch_send_pause:
        Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>,
    #[cfg(test)]
    fence_install_pause:
        Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>,
    #[cfg(test)]
    teardown_retry_pause: Arc<
        Mutex<
            Option<(
                tokio::sync::oneshot::Sender<()>,
                tokio::sync::oneshot::Receiver<()>,
            )>,
        >,
    >,
    #[cfg(test)]
    runner_control_finish_response_pause: Arc<Mutex<Option<RunnerControlFinishResponseTestHook>>>,
    #[cfg(test)]
    runner_lifecycle_test_counts: Arc<RunnerLifecycleTestCounts>,
}

#[cfg(test)]
#[derive(Default)]
struct RunnerLifecycleTestCounts {
    claim: AtomicUsize,
    snapshot: AtomicUsize,
    acquire_or_renew: AtomicUsize,
    release: AtomicUsize,
    register: AtomicUsize,
}

#[cfg(test)]
struct RunnerControlFinishResponseTestHook {
    connection_id: uuid::Uuid,
    run_id: RunId,
    operation_id: uuid::Uuid,
    committed: Option<tokio::sync::oneshot::Sender<()>>,
    abandoned: Option<tokio::sync::oneshot::Sender<()>>,
    owner: Option<RunnerControlFinishOwnerHandle>,
}

#[cfg(test)]
pub(crate) struct RunnerControlFinishOwnerHandle(Option<tokio::task::JoinHandle<()>>);

#[cfg(test)]
impl RunnerControlFinishOwnerHandle {
    fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self(Some(handle))
    }

    pub(crate) async fn abort_and_wait(mut self) -> Result<(), tokio::task::JoinError> {
        let handle = self
            .0
            .take()
            .expect("runner-control Finish owner handle missing");
        handle.abort();
        handle.await
    }
}

#[cfg(test)]
impl Drop for RunnerControlFinishOwnerHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
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

    #[cfg(test)]
    pub(crate) async fn wait_retry_pause_for_test(&self) {
        let pause = self
            .broker
            .teardown_retry_pause
            .lock()
            .expect("teardown-retry test hook poisoned")
            .take();
        if let Some((reached, release)) = pause {
            let _ = reached.send(());
            let _ = release.await;
        }
    }

    pub(crate) fn finish(&self) -> Result<()> {
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
        let process_identity = authority
            .connection_runner_processes
            .get(&self.connection_id)
            .copied();
        if let Some(process_identity) = process_identity {
            self.broker.quarantine_store.release_runner_bearing(
                process_identity,
                authority
                    .quarantined_runner_processes
                    .contains(&process_identity),
            )?;
        }
        authority
            .identities
            .retain(|_, owner| *owner != self.connection_id);
        authority
            .leases
            .retain(|_, owner| *owner != self.connection_id);
        authority
            .attachments
            .retain(|_, owner| *owner != self.connection_id);
        if let Some(process_identity) = authority
            .connection_runner_processes
            .remove(&self.connection_id)
        {
            if authority.runner_process_owners.get(&process_identity) == Some(&self.connection_id) {
                authority.runner_process_owners.remove(&process_identity);
            }
        }
        authority.dispatch.remove(&self.connection_id);
        Ok(())
    }
}

struct InflightRequest {
    broker: BrainRunnerBroker,
    key: (String, RunId),
    id: uuid::Uuid,
}

struct InflightCancellation {
    abort: Option<CancellationToken>,
    run_dispatch_gate: Arc<RunDispatchGate>,
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
    run_dispatch_gate: Arc<RunDispatchGate>,
    armed: bool,
}

impl CallbackCancellationGuard {
    fn new(
        broker: BrainRunnerBroker,
        brain: &str,
        run_id: RunId,
        registration_id: RunnerRegistrationId,
        cancel: CancellationToken,
        run_dispatch_gate: Arc<RunDispatchGate>,
    ) -> Self {
        Self {
            broker,
            brain: brain.to_string(),
            run_id,
            registration_id,
            cancel,
            run_dispatch_gate,
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
            {
                let _admission = self
                    .run_dispatch_gate
                    .0
                    .lock()
                    .expect("runner dispatch gate poisoned");
                self.cancel.cancel();
            }
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
        Self::with_deadlines_and_quarantine_path(deadlines, None)
            .expect("ephemeral runner quarantine cannot fail")
    }

    pub(crate) fn with_deadlines_and_quarantine_path(
        deadlines: RunnerDeadlines,
        quarantine_path: Option<PathBuf>,
    ) -> Result<Self> {
        let (quarantine_store, quarantined_runner_processes, uncertain_runner_processes) =
            match quarantine_path {
                Some(path) => RunnerQuarantineStore::at(path)?,
                None => (
                    RunnerQuarantineStore::default(),
                    HashSet::new(),
                    HashSet::new(),
                ),
            };
        let connection_authority = ConnectionAuthority {
            quarantined_runner_processes,
            uncertain_runner_processes,
            ..ConnectionAuthority::default()
        };
        Ok(Self {
            registrations: Arc::default(),
            pending_registrations: Arc::default(),
            registration_changes: Arc::default(),
            connection_authority: Arc::new(Mutex::new(connection_authority)),
            quarantine_store,
            inflight: Arc::default(),
            cancelled_before_dispatch: Arc::default(),
            transient_cancellation_fences: Arc::default(),
            fence_retirement_pending: Arc::default(),
            deadlines,
            #[cfg(test)]
            registration_admission_pause: Arc::default(),
            #[cfg(test)]
            pending_promotion_pause: Arc::default(),
            #[cfg(test)]
            dispatch_send_pause: Arc::default(),
            #[cfg(test)]
            fence_install_pause: Arc::default(),
            #[cfg(test)]
            teardown_retry_pause: Arc::default(),
            #[cfg(test)]
            runner_control_finish_response_pause: Arc::default(),
            #[cfg(test)]
            runner_lifecycle_test_counts: Arc::default(),
        })
    }

    #[cfg(test)]
    pub(crate) fn record_runner_claim_for_test(&self) {
        self.runner_lifecycle_test_counts
            .claim
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn record_runner_snapshot_for_test(&self) {
        self.runner_lifecycle_test_counts
            .snapshot
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn record_runner_acquire_for_test(&self) {
        self.runner_lifecycle_test_counts
            .acquire_or_renew
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn record_runner_release_for_test(&self) {
        self.runner_lifecycle_test_counts
            .release
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn record_runner_register_for_test(&self) {
        self.runner_lifecycle_test_counts
            .register
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn runner_lifecycle_counts_for_test(&self) -> [usize; 5] {
        [
            self.runner_lifecycle_test_counts
                .claim
                .load(Ordering::SeqCst),
            self.runner_lifecycle_test_counts
                .snapshot
                .load(Ordering::SeqCst),
            self.runner_lifecycle_test_counts
                .acquire_or_renew
                .load(Ordering::SeqCst),
            self.runner_lifecycle_test_counts
                .release
                .load(Ordering::SeqCst),
            self.runner_lifecycle_test_counts
                .register
                .load(Ordering::SeqCst),
        ]
    }

    fn track_inflight(
        &self,
        brain: &str,
        run_id: RunId,
        enforce_run_fence: bool,
        abortable: bool,
    ) -> Result<(
        Option<CancellationToken>,
        InflightRequest,
        Arc<RunDispatchGate>,
    )> {
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
        let run_dispatch_gate = Arc::new(RunDispatchGate::default());
        self.inflight
            .lock()
            .expect("runner inflight lock poisoned")
            .entry(key.clone())
            .or_default()
            .insert(
                id,
                InflightCancellation {
                    abort: mapped_abort,
                    run_dispatch_gate: Arc::clone(&run_dispatch_gate),
                },
            );
        Ok((
            abort,
            InflightRequest {
                broker: self.clone(),
                key,
                id,
            },
            run_dispatch_gate,
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
            for request in requests.values_mut() {
                let _admission = request
                    .run_dispatch_gate
                    .0
                    .lock()
                    .expect("runner dispatch gate poisoned");
                if let Some(abort) = request.abort.take() {
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
            let (abort_rx, inflight, run_dispatch_gate) =
                self.track_inflight(brain, run_id, false, false)?;
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
                    Arc::clone(&registration.dispatch_gate),
                    Arc::clone(&run_dispatch_gate),
                    false,
                )
                .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
            let response_rx = self.retain_callback_until_settled(
                response_rx,
                registration_request,
                inflight,
                None,
                cancel.clone(),
                registration.tx.compatible_lifetime(),
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
                        run_dispatch_gate,
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
        let key = (brain.to_string(), run_id);
        let mut durable = self
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned");
        if durable.contains(&key) {
            return false;
        }
        let inflight = self.inflight.lock().expect("runner inflight lock poisoned");
        let mut gates = inflight
            .get(&key)
            .into_iter()
            .flat_map(|requests| requests.values())
            .map(|request| Arc::clone(&request.run_dispatch_gate))
            .collect::<Vec<_>>();
        gates.sort_unstable_by_key(Arc::as_ptr);
        gates.dedup_by(|left, right| Arc::ptr_eq(left, right));
        let mut admissions = gates
            .iter()
            .map(|gate| gate.0.lock().expect("runner run-dispatch gate poisoned"))
            .collect::<Vec<_>>();
        #[cfg(test)]
        if let Some((reached, release)) = self
            .fence_install_pause
            .lock()
            .expect("fence-install test hook poisoned")
            .take()
        {
            let _ = reached.send(());
            let _ = release.recv();
        }
        for admission in &mut admissions {
            admission.fenced = true;
        }
        durable.insert(key)
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
        let key = (brain.to_string(), run_id);
        let mut durable = self
            .cancelled_before_dispatch
            .lock()
            .expect("runner cancellation-fence lock poisoned");
        let inflight = self.inflight.lock().expect("runner inflight lock poisoned");
        let mut gates = inflight
            .get(&key)
            .into_iter()
            .flat_map(|requests| requests.values())
            .map(|request| Arc::clone(&request.run_dispatch_gate))
            .collect::<Vec<_>>();
        gates.sort_unstable_by_key(Arc::as_ptr);
        gates.dedup_by(|left, right| Arc::ptr_eq(left, right));
        let mut admissions = gates
            .iter()
            .map(|gate| gate.0.lock().expect("runner run-dispatch gate poisoned"))
            .collect::<Vec<_>>();
        durable.remove(&key);
        for admission in &mut admissions {
            admission.fenced = false;
        }
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
        run_dispatch_gate: &Arc<RunDispatchGate>,
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
        let run_admission = run_dispatch_gate
            .0
            .lock()
            .expect("runner run-dispatch gate poisoned");
        anyhow::ensure!(
            !durable.contains(&key) && !transient.contains_key(&key) && !run_admission.fenced,
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
                Arc::clone(&registration.dispatch_gate),
                Arc::clone(run_dispatch_gate),
                true,
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
        mut response_rx: oneshot::Receiver<Result<T, E>>,
        registration_request: RegistrationRequest,
        inflight: InflightRequest,
        fence: Option<TransientCancellationFence>,
        cancel: CancellationToken,
        compatible_lifetime: Option<mpsc::UnboundedSender<RunnerRequest>>,
    ) -> oneshot::Receiver<SettledRunnerCallback<T, E>>
    where
        T: Send + 'static,
        E: Send + 'static,
    {
        let (settled_tx, settled_rx) = oneshot::channel();
        tokio::spawn(async move {
            let response = match compatible_lifetime {
                Some(callback_channel) => tokio::select! {
                    biased;
                    response = &mut response_rx => response,
                    _ = cancel.cancelled() => {
                        // Closing is observable through the source-compatible
                        // public Sender. Tokio oneshot cannot separately reveal
                        // Sender destruction after close, so the compatible
                        // receiver channel is the conservative physical owner.
                        response_rx.close();
                        callback_channel.closed().await;
                        response_rx.await
                    }
                },
                // The bounded IPC bridge owns a distinct cancellation token
                // and drops this response sender only after the remote RPC
                // settles or transport ejection revokes its capability graph.
                None => response_rx.await,
            };
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
            dispatch_gate: Arc::new(DispatchGate::default()),
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
                // Keep the old generation installed while taking its gate.
                // The final IPC send takes only that same gate, so it either
                // commits first or observes invalidation; no map/gate lock
                // inversion is possible.
                current.invalidate();
                registrations.insert(brain, registration);
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

    /// Reject a kernel-derived peer identity before it can read runner
    /// recovery state or mutate runner lifecycle authority. This is the
    /// restart-safe counterpart to the frontend's process-local poison bit:
    /// loaded durable uncertainty remains authoritative when ejectProcess was
    /// lost with the old daemon transport.
    pub(crate) fn ensure_runner_process_admitted(
        &self,
        process_identity: RunnerProcessIdentity,
    ) -> Result<()> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        Self::ensure_runner_process_admitted_locked(&authority, process_identity)
    }

    fn ensure_runner_process_admitted_locked(
        authority: &ConnectionAuthority,
        process_identity: RunnerProcessIdentity,
    ) -> Result<()> {
        anyhow::ensure!(
            !authority
                .quarantined_runner_processes
                .contains(&process_identity),
            "{}: runner process identity was quarantined after forced callback ejection",
            crate::ipc::RUNNER_PROCESS_QUARANTINED_CODE
        );
        anyhow::ensure!(
            !authority
                .uncertain_runner_processes
                .contains(&process_identity),
            "{}: runner process identity has an unresolved durable runner-bearing marker from a prior daemon instance",
            crate::ipc::RUNNER_PROCESS_QUARANTINED_CODE
        );
        Ok(())
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

    /// Admit one reverse runner-lifecycle mutation against the exact live
    /// connection, process, and lease generation. The authority lock is
    /// always acquired before the dispatch lock, matching ejection and
    /// teardown; the returned guard keeps the admitted mutation visible to
    /// physical-quiescence accounting without holding either lock.
    pub(crate) fn admit_runner_control(
        &self,
        connection_id: uuid::Uuid,
        process_identity: RunnerProcessIdentity,
        brain: &str,
        lease_id: RunnerLeaseId,
    ) -> Result<ConnectionDispatchGuard> {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        Self::ensure_runner_process_admitted_locked(&authority, process_identity)?;
        anyhow::ensure!(
            authority.connection_runner_processes.get(&connection_id) == Some(&process_identity)
                && authority.runner_process_owners.get(&process_identity) == Some(&connection_id),
            "runner lifecycle capability no longer matches the registered process"
        );
        anyhow::ensure!(
            authority.leases.get(&(brain.to_string(), lease_id)) == Some(&connection_id),
            "runner lease is not owned by this IPC connection"
        );
        let dispatch = authority
            .dispatch
            .get(&connection_id)
            .cloned()
            .context("IPC connection has no dispatch authority")?;
        dispatch
            .try_enter()
            .context("IPC connection runner dispatch is closed")
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
        let process_identity = RunnerProcessIdentity {
            pid: u32::from_le_bytes(connection_id.as_bytes()[..4].try_into().unwrap()),
            start_token: u64::from_le_bytes(connection_id.as_bytes()[8..].try_into().unwrap()),
        };
        self.register_sender_for_connection(
            connection_id,
            process_identity,
            brain.into(),
            lease_id,
            RunnerCallbackSender::Compatible(tx),
        )
    }

    pub(crate) fn register_bounded_for_connection(
        &self,
        connection_id: uuid::Uuid,
        process_identity: RunnerProcessIdentity,
        brain: impl Into<String>,
        lease_id: RunnerLeaseId,
        tx: mpsc::UnboundedSender<BoundedRunnerRequest>,
    ) -> Result<RunnerRegistrationId> {
        self.register_sender_for_connection(
            connection_id,
            process_identity,
            brain.into(),
            lease_id,
            RunnerCallbackSender::Bounded(tx),
        )
    }

    fn register_sender_for_connection(
        &self,
        connection_id: uuid::Uuid,
        process_identity: RunnerProcessIdentity,
        brain: String,
        lease_id: RunnerLeaseId,
        tx: RunnerCallbackSender,
    ) -> Result<RunnerRegistrationId> {
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        anyhow::ensure!(
            authority.leases.get(&(brain.clone(), lease_id)) == Some(&connection_id),
            "runner lease is not owned by this IPC connection"
        );
        Self::ensure_runner_process_admitted_locked(&authority, process_identity)?;
        anyhow::ensure!(
            authority
                .runner_process_owners
                .get(&process_identity)
                .map_or(true, |owner| *owner == connection_id),
            "runner process epoch is already active on another IPC connection"
        );
        anyhow::ensure!(
            authority
                .connection_runner_processes
                .get(&connection_id)
                .map_or(true, |identity| *identity == process_identity),
            "IPC connection attempted to change runner process epoch"
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
        // This fsync is the registration commit point. If the daemon exits or
        // a later forced-quarantine promotion cannot be persisted, restart
        // treats the still-live identity as uncertain and rejects it.
        self.quarantine_store
            .mark_runner_bearing(process_identity)
            .context("persist runner-bearing process identity before registration")?;
        let id = RunnerRegistrationId(uuid::Uuid::new_v4());
        let (active, _) = watch::channel(true);
        let registration = Registration {
            id,
            lease_id,
            connection_id: Some(connection_id),
            tx,
            active,
            generation_cancel: CancellationToken::new(),
            dispatch_gate: Arc::new(DispatchGate::default()),
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
                current.invalidate();
                registrations.insert(brain, registration);
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
        authority
            .runner_process_owners
            .insert(process_identity, connection_id);
        authority
            .connection_runner_processes
            .insert(connection_id, process_identity);
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

    pub(crate) fn open_connection_dispatch(
        &self,
        connection_id: uuid::Uuid,
    ) -> Arc<ConnectionDispatchAdmission> {
        self.connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned")
            .dispatch
            .entry(connection_id)
            .or_default()
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn test_connection_resources_retired(&self, connection_id: uuid::Uuid) -> bool {
        let authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        !authority
            .identities
            .values()
            .any(|owner| *owner == connection_id)
            && !authority
                .leases
                .values()
                .any(|owner| *owner == connection_id)
            && !authority
                .attachments
                .values()
                .any(|owner| *owner == connection_id)
            && !authority.dispatch.contains_key(&connection_id)
            && !authority
                .connection_runner_processes
                .contains_key(&connection_id)
            && !authority
                .runner_process_owners
                .values()
                .any(|owner| *owner == connection_id)
    }

    #[cfg(test)]
    pub(crate) fn test_run_resources_retired(&self, brain: &str, run_id: RunId) -> bool {
        let key = (brain.to_string(), run_id);
        let inflight_retired = !self
            .inflight
            .lock()
            .expect("runner inflight lock poisoned")
            .contains_key(&key);
        let transient_retired = !self
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&key);
        let retirement_owner_retired = !self
            .fence_retirement_pending
            .lock()
            .expect("runner fence-retirement lock poisoned")
            .contains(&key);
        inflight_retired && transient_retired && retirement_owner_retired
    }

    #[cfg(test)]
    pub(crate) fn pause_next_fence_install(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *self
            .fence_install_pause
            .lock()
            .expect("fence-install test hook poisoned") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_teardown_retry(
        &self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *self
            .teardown_retry_pause
            .lock()
            .expect("teardown-retry test hook poisoned") = Some((reached_tx, release_rx));
        (reached_rx, release_tx)
    }

    #[cfg(test)]
    pub(crate) fn pause_next_runner_control_finish_response_for_test(
        &self,
        connection_id: uuid::Uuid,
        run_id: RunId,
    ) -> (
        uuid::Uuid,
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        let operation_id = uuid::Uuid::new_v4();
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (abandoned_tx, abandoned_rx) = tokio::sync::oneshot::channel();
        let mut slot = self
            .runner_control_finish_response_pause
            .lock()
            .expect("runner-control Finish response test hook poisoned");
        assert!(
            slot.is_none(),
            "runner-control Finish test hook already armed"
        );
        *slot = Some(RunnerControlFinishResponseTestHook {
            connection_id,
            run_id,
            operation_id,
            committed: Some(reached_tx),
            abandoned: Some(abandoned_tx),
            owner: None,
        });
        (operation_id, reached_rx, abandoned_rx)
    }

    #[cfg(test)]
    pub(crate) fn take_runner_control_finish_response_pause_for_test(
        &self,
        connection_id: uuid::Uuid,
        run_id: RunId,
    ) -> Option<(
        uuid::Uuid,
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Sender<()>,
    )> {
        let mut slot = self
            .runner_control_finish_response_pause
            .lock()
            .expect("runner-control Finish response test hook poisoned");
        let hook = slot.as_mut()?;
        if hook.connection_id != connection_id || hook.run_id != run_id {
            return None;
        }
        let committed = hook.committed.take()?;
        let abandoned = hook
            .abandoned
            .take()
            .expect("runner-control Finish abandonment hook missing");
        Some((hook.operation_id, committed, abandoned))
    }

    #[cfg(test)]
    pub(crate) fn install_runner_control_finish_owner_for_test(
        &self,
        connection_id: uuid::Uuid,
        run_id: RunId,
        operation_id: uuid::Uuid,
        owner: tokio::task::JoinHandle<()>,
    ) -> std::result::Result<(), (anyhow::Error, RunnerControlFinishOwnerHandle)> {
        let owner = RunnerControlFinishOwnerHandle::new(owner);
        let mut slot = self
            .runner_control_finish_response_pause
            .lock()
            .expect("runner-control Finish response test hook poisoned");
        let Some(hook) = slot.as_mut() else {
            return Err((
                anyhow::anyhow!("runner-control Finish hook disappeared"),
                owner,
            ));
        };
        if hook.connection_id != connection_id
            || hook.run_id != run_id
            || hook.operation_id != operation_id
            || hook.owner.is_some()
        {
            return Err((
                anyhow::anyhow!("runner-control Finish hook identity changed"),
                owner,
            ));
        }
        hook.owner = Some(owner);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn take_runner_control_finish_owner_for_test(
        &self,
        connection_id: uuid::Uuid,
    ) -> Result<Option<(RunId, uuid::Uuid, RunnerControlFinishOwnerHandle)>> {
        let mut slot = self
            .runner_control_finish_response_pause
            .lock()
            .expect("runner-control Finish response test hook poisoned");
        if !slot
            .as_ref()
            .is_some_and(|hook| hook.connection_id == connection_id)
        {
            return Ok(None);
        }
        let mut hook = slot.take().expect("checked runner-control Finish hook");
        let owner = hook
            .owner
            .take()
            .context("runner-control Finish hook has no installed owner")?;
        Ok(Some((hook.run_id, hook.operation_id, owner)))
    }

    #[cfg(test)]
    pub(crate) fn fail_next_quarantine_promotion_for_test(&self) {
        self.quarantine_store.fail_next_quarantine_promotion();
    }

    /// Permanently quarantine this kernel-derived runner process identity,
    /// synchronously close dispatch and invalidate its exact generations,
    /// then publish transport ejection. A confirmed quarantine or its earlier
    /// fsynced runner-bearing fallback and invalidation precede the signal, so
    /// neither a spoofed correlation UUID nor a second callback can race the
    /// fail-closed boundary.
    pub(crate) fn eject_connection(
        &self,
        connection_id: uuid::Uuid,
    ) -> Result<Arc<ConnectionDispatchAdmission>> {
        let mut authority = self
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        let process_identity = *authority
            .connection_runner_processes
            .get(&connection_id)
            .context("IPC connection has no registered runner process identity")?;
        authority
            .quarantined_runner_processes
            .insert(process_identity);
        let quarantine_persistence_error = self.quarantine_store.quarantine(process_identity).err();
        let dispatch = authority
            .dispatch
            .get(&connection_id)
            .cloned()
            .context("IPC connection has no dispatch authority")?;
        dispatch.close();
        let registrations = self
            .registrations
            .read()
            .expect("runner broker lock poisoned");
        let pending = self
            .pending_registrations
            .read()
            .expect("runner pending-registration lock poisoned");
        for registration in registrations.values().chain(pending.values()) {
            if registration.connection_id == Some(connection_id) {
                registration.invalidate();
            }
        }
        drop(pending);
        drop(registrations);
        drop(authority);
        if let Some(error) = quarantine_persistence_error {
            // Registration was acknowledged only after its runner-bearing
            // marker was fsynced. Promotion failure therefore cannot reopen
            // admission after restart: the older durable uncertainty remains
            // authoritative while this daemon retains a confirmed in-memory
            // quarantine.
            tracing::error!(%error, %connection_id,
                "could not promote durable runner-bearing marker to confirmed quarantine before ejection");
        }
        self.registration_changes.notify_waiters();
        Ok(dispatch)
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
        if let Some(replaced) = registrations.get(brain) {
            replaced.invalidate();
        }
        registrations.insert(brain.to_string(), next);
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
        run_dispatch_gate: Arc<RunDispatchGate>,
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
            run_dispatch_gate,
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
                            let _admission = registration
                                .dispatch_gate
                                .0
                                .lock()
                                .expect("runner dispatch gate poisoned");
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
                            let _admission = registration
                                .dispatch_gate
                                .0
                                .lock()
                                .expect("runner dispatch gate poisoned");
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
                        let _admission = registration
                            .dispatch_gate
                            .0
                            .lock()
                            .expect("runner dispatch gate poisoned");
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

        {
            let _admission = registration
                .dispatch_gate
                .0
                .lock()
                .expect("runner dispatch gate poisoned");
            cancel.cancel();
        }

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

    /// Commit one remote callback send atomically with its cancellation,
    /// deadline, and exact-generation token. Registration replacement takes
    /// the same gate before invalidating that token. It deliberately never
    /// acquires a registration-map lock while holding the gate: map writers
    /// use map -> gate, while abort paths use gate only.
    pub(crate) fn admit_runner_dispatch<T>(
        &self,
        dispatch_gate: &Arc<DispatchGate>,
        run_dispatch_gate: &Arc<RunDispatchGate>,
        enforce_run_fence: bool,
        cancel: &CancellationToken,
        generation_cancel: &CancellationToken,
        deadline: tokio::time::Instant,
        send: impl FnOnce() -> T,
    ) -> Option<T> {
        let _admission = dispatch_gate
            .0
            .lock()
            .expect("runner dispatch gate poisoned");
        let run_admission = run_dispatch_gate
            .0
            .lock()
            .expect("runner run-dispatch gate poisoned");
        if cancel.is_cancelled()
            || generation_cancel.is_cancelled()
            || tokio::time::Instant::now() >= deadline
            || (enforce_run_fence && run_admission.fenced)
        {
            return None;
        }
        #[cfg(test)]
        if let Some((reached, release)) = self
            .dispatch_send_pause
            .lock()
            .expect("dispatch-send test hook poisoned")
            .take()
        {
            let _ = reached.send(());
            let _ = release.recv();
        }
        if cancel.is_cancelled()
            || generation_cancel.is_cancelled()
            || tokio::time::Instant::now() >= deadline
            || (enforce_run_fence && run_admission.fenced)
        {
            return None;
        }
        Some(send())
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
        let (abort_rx, inflight, run_dispatch_gate) =
            self.track_inflight(brain, run_id, true, true)?;
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
            &run_dispatch_gate,
        )?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
            cancel.clone(),
            registration.tx.compatible_lifetime(),
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
            run_dispatch_gate,
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
        let (abort_rx, inflight, run_dispatch_gate) =
            self.track_inflight(brain, run_id, true, true)?;
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
            &run_dispatch_gate,
        )?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
            cancel.clone(),
            registration.tx.compatible_lifetime(),
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
            run_dispatch_gate,
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
        let (abort_rx, inflight, run_dispatch_gate) =
            self.track_inflight(brain, run_id, false, false)?;
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
                Arc::clone(&registration.dispatch_gate),
                Arc::clone(&run_dispatch_gate),
                false,
            )
            .map_err(|_| self.disconnect_registration(brain, operation, &registration))?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            None,
            cancel.clone(),
            registration.tx.compatible_lifetime(),
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
            run_dispatch_gate,
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
        rendered: String,
    ) -> Result<usize> {
        self.try_project_memory(
            brain,
            lease_id,
            brain_id,
            run_id,
            request_seq,
            prompt,
            rendered,
        )
        .await
        .map_err(RunnerProjectionError::into_error)
    }

    /// Project memory while preserving whether a failure is systemic for a
    /// replay pass or specific to this turn.
    pub async fn try_project_memory(
        &self,
        brain: &str,
        lease_id: RunnerLeaseId,
        brain_id: crate::brain::store::BrainId,
        run_id: RunId,
        request_seq: u64,
        prompt: String,
        rendered: String,
    ) -> std::result::Result<usize, RunnerProjectionError> {
        let operation = RunnerOperation::ProjectMemory;
        let deadline = tokio::time::Instant::now() + self.deadlines.for_operation(operation);
        let (registration, registration_request) = self
            .registration(brain, lease_id, operation)
            .map_err(RunnerProjectionError::Unavailable)?;
        let (response_tx, response_rx) = oneshot::channel();
        let (abort_rx, inflight, run_dispatch_gate) = self
            .track_inflight(brain, run_id, true, true)
            .map_err(RunnerProjectionError::Unavailable)?;
        let cancel = CancellationToken::new();
        let fence = self
            .send_if_unfenced(
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
                    rendered,
                    response_tx,
                }),
                cancel.clone(),
                deadline,
                &run_dispatch_gate,
            )
            .map_err(RunnerProjectionError::Unavailable)?;
        let response_rx = self.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            Some(fence),
            cancel.clone(),
            registration.tx.compatible_lifetime(),
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
            run_dispatch_gate,
            |message| anyhow::Error::new(RunnerProjectionRejected(message)),
        )
        .await
        .map_err(|error| match error.downcast::<RunnerProjectionRejected>() {
            Ok(RunnerProjectionRejected(message)) => {
                match message.strip_prefix(RUNNER_UNAVAILABLE_PREFIX) {
                    Some(reason) => RunnerProjectionError::Unavailable(anyhow::Error::msg(
                        format!("named Brain '{brain}' runner cannot project memory: {reason}"),
                    )),
                    None => RunnerProjectionError::Rejected(message),
                }
            }
            Err(error) => RunnerProjectionError::Unavailable(error),
        })
    }
}

#[derive(Debug)]
struct RunnerProjectionRejected(String);

impl std::fmt::Display for RunnerProjectionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RunnerProjectionRejected {}

/// Marks a runner reply as a condition that repeats for every later replayed
/// run rather than a rejection specific to one turn.
pub const RUNNER_UNAVAILABLE_PREFIX: &str = "runner-unavailable: ";

/// Why a memory projection did not happen.
#[derive(Debug)]
pub enum RunnerProjectionError {
    /// The runner is absent, stale, disconnected, or otherwise unusable.
    Unavailable(anyhow::Error),
    /// The runner rejected this projection after accepting the callback.
    Rejected(String),
}

impl RunnerProjectionError {
    /// Flatten this classification for callers that do not replay projections.
    pub fn into_error(self) -> anyhow::Error {
        match self {
            Self::Unavailable(error) => error,
            Self::Rejected(message) => anyhow::Error::msg(message),
        }
    }
}

impl std::fmt::Display for RunnerProjectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(error) => write!(f, "{error}"),
            Self::Rejected(message) => write!(f, "{message}"),
        }
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

    fn process_identity(start_token: u64) -> RunnerProcessIdentity {
        RunnerProcessIdentity {
            pid: std::process::id(),
            start_token,
        }
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
            rendered: "source".into(),
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
        let (_abort_rx, inflight, _run_dispatch_gate) =
            broker.track_inflight("brain", run_id, true, true).unwrap();
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
            assert_eq!(request.rendered, "remembered");
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
                    "remembered".into(),
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
                "remembered".into(),
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
        assert!(request.response_tx.is_closed());
        drop(request.response_tx);
        drop(rx);
        let error = dispatch.await.unwrap().unwrap_err();
        assert_dispatch_failure(
            &error,
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );
        assert!(!broker.has_registration("brain", lease_id));
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
        assert!(request
            .response_tx
            .send(Ok(program_result("crossed cancellation")))
            .is_err());
        drop(rx);
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
            receiver_drop_request.response_tx.is_closed(),
            "daemon cancellation did not close the compatible callback response"
        );
        let receiver_drop_run_id = receiver_drop_request.run_id;
        assert!(receiver_drop_request
            .response_tx
            .send(Ok(program_result("late")))
            .is_err());
        tokio::task::yield_now().await;
        assert!(!broker.has_registration("brain", lease_id));
        assert!(broker
            .transient_cancellation_fences
            .lock()
            .expect("runner transient-fence lock poisoned")
            .contains_key(&("brain".into(), receiver_drop_run_id)));
        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), async {
            while broker
                .transient_cancellation_fences
                .lock()
                .expect("runner transient-fence lock poisoned")
                .contains_key(&("brain".into(), receiver_drop_run_id))
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("compatible callback receiver closure did not retire its exact fence");
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
        assert!(!broker.has_registration("brain", lease_id));
        drop(rx);
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
        let (_tracked_abort, inflight, run_dispatch_gate) =
            broker.track_inflight("brain", run_id, true, true).unwrap();
        let cancel = CancellationToken::new();
        let (response_tx, response_rx) = oneshot::channel();
        response_tx
            .send(Ok::<_, RunnerProgramError>(program_result("late")))
            .unwrap();
        let response_rx = broker.retain_callback_until_settled(
            response_rx,
            registration_request,
            inflight,
            None,
            cancel.clone(),
            registration.tx.compatible_lifetime(),
        );
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
                run_dispatch_gate,
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
        assert!(!broker.has_registration("brain", lease_id));
        drop(rx);
        let (memory_tx, mut memory_rx) = mpsc::unbounded_channel();
        let memory_registration = broker.register("brain", lease_id, memory_tx);
        broker
            .wait_registration_active("brain", memory_registration)
            .await
            .unwrap();

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
        let RunnerRequest::ProjectMemory(memory_request) = memory_rx.recv().await.unwrap() else {
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
        assert!(!broker.has_registration("brain", lease_id));
        drop(memory_rx);
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_quarantines_compatible_runner_until_fresh_callback_channel() {
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
        assert!(stuck.response_tx.is_closed());
        tokio::time::advance(CALLBACK_CLEANUP).await;
        assert_dispatch_failure(
            &first.await.unwrap().unwrap_err(),
            RunnerOperation::Program,
            RunnerDispatchFailure::TimedOut,
        );

        let (fresh_tx, mut fresh_rx) = mpsc::unbounded_channel();
        let fresh_registration = broker.register("brain", lease_id, fresh_tx);
        assert!(!broker.has_registration("brain", lease_id));
        drop(stuck.response_tx);
        drop(rx);
        broker
            .wait_registration_active("brain", fresh_registration)
            .await
            .unwrap();
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
        let RunnerRequest::Program(healthy) = fresh_rx.recv().await.unwrap() else {
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
        drop(old_rx);
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
        assert!(old_request
            .response_tx
            .send(Ok(program_result("late old")))
            .is_err());
        drop(old_rx);
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

    #[test]
    fn test_forced_ejection_quarantines_same_process_epoch_but_fresh_epoch_registers() {
        let broker = BrainRunnerBroker::default();
        let lease_id = lease();
        let old_connection = uuid::Uuid::new_v4();
        let same_process_epoch = process_identity(11);
        broker
            .claim_connection_lease(old_connection, "brain", lease_id)
            .unwrap();
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(
                old_connection,
                same_process_epoch,
                "brain",
                lease_id,
                old_tx,
            )
            .unwrap();
        let admission = broker.eject_connection(old_connection).unwrap();
        assert!(!admission.eject.is_cancelled());
        admission.publish_ejection();
        admission.mark_transport_closed();
        broker
            .begin_connection_teardown(old_connection)
            .finish()
            .unwrap();

        let reconnect = uuid::Uuid::new_v4();
        broker
            .claim_connection_lease(reconnect, "brain", lease_id)
            .unwrap();
        let (same_tx, _same_rx) = mpsc::unbounded_channel();
        let error = broker
            .register_bounded_for_connection(
                reconnect,
                same_process_epoch,
                "brain",
                lease_id,
                same_tx,
            )
            .unwrap_err();
        assert!(error.to_string().contains("quarantined"));
        broker
            .begin_connection_teardown(reconnect)
            .finish()
            .unwrap();

        let fresh_connection = uuid::Uuid::new_v4();
        let fresh_epoch = process_identity(12);
        broker
            .claim_connection_lease(fresh_connection, "brain", lease_id)
            .unwrap();
        let (fresh_tx, _fresh_rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(
                fresh_connection,
                fresh_epoch,
                "brain",
                lease_id,
                fresh_tx,
            )
            .unwrap();
        assert!(broker.has_registration("brain", lease_id));
        let authority = broker
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned");
        assert!(authority
            .quarantined_runner_processes
            .contains(&same_process_epoch));
        assert_eq!(
            authority.runner_process_owners.get(&fresh_epoch),
            Some(&fresh_connection)
        );
    }

    #[test]
    fn test_graceful_connection_teardown_fsync_allows_same_process_identity_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let quarantine_path = temp.path().join("runner-process-quarantine-v1.json");
        let broker = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path.clone()),
        )
        .unwrap();
        let lease_id = lease();
        let process_epoch = RunnerProcessIdentity::for_pid(std::process::id()).unwrap();
        let old_connection = uuid::Uuid::new_v4();
        broker
            .claim_connection_lease(old_connection, "brain", lease_id)
            .unwrap();
        let (old_tx, _old_rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(
                old_connection,
                process_epoch,
                "brain",
                lease_id,
                old_tx,
            )
            .unwrap();
        broker
            .begin_connection_teardown(old_connection)
            .finish()
            .unwrap();
        drop(broker);

        let broker = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path),
        )
        .unwrap();

        let reconnect = uuid::Uuid::new_v4();
        broker
            .claim_connection_lease(reconnect, "brain", lease_id)
            .unwrap();
        let (reconnect_tx, _reconnect_rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(
                reconnect,
                process_epoch,
                "brain",
                lease_id,
                reconnect_tx,
            )
            .unwrap();
        assert!(broker.has_registration("brain", lease_id));
    }

    #[test]
    fn test_forced_quarantine_survives_broker_restart_and_pid_start_mismatch_prunes() {
        let temp = tempfile::tempdir().unwrap();
        let quarantine_path = temp.path().join("runner-process-quarantine-v1.json");
        let identity = RunnerProcessIdentity::for_pid(std::process::id()).unwrap();
        let lease_id = lease();
        let connection = uuid::Uuid::new_v4();
        let broker = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path.clone()),
        )
        .unwrap();
        broker
            .claim_connection_lease(connection, "brain", lease_id)
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(connection, identity, "brain", lease_id, tx)
            .unwrap();
        let admission = broker.eject_connection(connection).unwrap();
        admission.publish_ejection();
        admission.mark_transport_closed();
        broker
            .begin_connection_teardown(connection)
            .finish()
            .unwrap();
        drop(broker);

        let restarted = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path.clone()),
        )
        .unwrap();
        let reconnect = uuid::Uuid::new_v4();
        restarted
            .claim_connection_lease(reconnect, "brain", lease_id)
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let error = restarted
            .register_bounded_for_connection(reconnect, identity, "brain", lease_id, tx)
            .unwrap_err();
        assert!(error.to_string().contains("quarantined"));
        drop(restarted);

        let recycled = RunnerProcessIdentity {
            pid: identity.pid,
            start_token: identity.start_token.wrapping_add(1),
        };
        std::fs::write(
            &quarantine_path,
            serde_json::to_vec(&vec![recycled]).unwrap(),
        )
        .unwrap();
        let pruned = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path),
        )
        .unwrap();
        assert!(!pruned
            .connection_authority
            .lock()
            .expect("runner connection-authority lock poisoned")
            .quarantined_runner_processes
            .contains(&recycled));
    }

    #[test]
    fn test_quarantine_promotion_failure_uses_durable_runner_bearing_fallback_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let quarantine_path = temp.path().join("runner-process-quarantine-v1.json");
        let broker = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path.clone()),
        )
        .unwrap();
        let lease_id = lease();
        let connection = uuid::Uuid::new_v4();
        let identity = RunnerProcessIdentity::for_pid(std::process::id()).unwrap();
        broker
            .claim_connection_lease(connection, "brain", lease_id)
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(connection, identity, "brain", lease_id, tx)
            .unwrap();

        broker.fail_next_quarantine_promotion_for_test();
        let admission = broker.eject_connection(connection).unwrap();
        assert!(admission.try_enter().is_none());
        // Model a lost best-effort eject notification followed by daemon
        // process loss: neither in-memory quarantine nor teardown survives.
        drop(broker);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&quarantine_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let restarted = BrainRunnerBroker::with_deadlines_and_quarantine_path(
            RunnerDeadlines::default(),
            Some(quarantine_path),
        )
        .unwrap();
        let reconnect = uuid::Uuid::new_v4();
        restarted
            .claim_connection_lease(reconnect, "brain", lease_id)
            .unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let error = restarted
            .register_bounded_for_connection(reconnect, identity, "brain", lease_id, tx)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("unresolved durable runner-bearing"));
    }

    #[test]
    fn test_final_send_gate_orders_irrevocable_send_before_concurrent_abort() {
        let broker = BrainRunnerBroker::default();
        let run_id = RunId(uuid::Uuid::new_v4());
        let gate = Arc::new(DispatchGate::default());
        let (abort, inflight, run_gate) =
            broker.track_inflight("brain", run_id, true, true).unwrap();
        let abort = abort.unwrap();
        let (response_tx, _response_rx) = oneshot::channel();
        let envelope = Arc::new(BoundedRunnerRequest {
            request: RunnerRequest::Cancel(RunnerCancelRequest {
                brain: "brain".into(),
                run_id,
                response_tx,
            }),
            cancel: CancellationToken::new(),
            generation_cancel: CancellationToken::new(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
            cleanup_timeout: Duration::from_secs(1),
            dispatch_gate: gate,
            run_dispatch_gate: run_gate,
            enforce_run_fence: true,
        });
        let (reached_tx, reached_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        *broker
            .dispatch_send_pause
            .lock()
            .expect("dispatch-send test hook poisoned") = Some((reached_tx, release_rx));
        let sent = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let send_broker = broker.clone();
        let send_envelope = Arc::clone(&envelope);
        let sent_flag = Arc::clone(&sent);
        let send = std::thread::spawn(move || {
            send_broker.admit_runner_dispatch(
                &send_envelope.dispatch_gate,
                &send_envelope.run_dispatch_gate,
                send_envelope.enforce_run_fence,
                &send_envelope.cancel,
                &send_envelope.generation_cancel,
                send_envelope.deadline,
                || sent_flag.store(true, Ordering::Release),
            )
        });
        reached_rx.recv().unwrap();
        let abort_broker = broker.clone();
        let (abort_done_tx, abort_done_rx) = std::sync::mpsc::channel();
        let aborting = std::thread::spawn(move || {
            abort_broker.abort_run("brain", run_id);
            abort_done_tx.send(()).unwrap();
        });
        assert!(abort_done_rx
            .recv_timeout(Duration::from_millis(25))
            .is_err());
        release_tx.send(()).unwrap();
        assert!(send.join().unwrap().is_some());
        aborting.join().unwrap();
        assert!(sent.load(Ordering::Acquire));
        assert!(abort.is_cancelled());
        drop(inflight);
    }

    #[tokio::test]
    async fn test_durable_fence_linearizes_before_program_turn_and_memory_final_send() {
        for operation in [
            RunnerOperation::Program,
            RunnerOperation::Turn,
            RunnerOperation::ProjectMemory,
        ] {
            let broker = BrainRunnerBroker::default();
            let lease_id = lease();
            let run_id = RunId(uuid::Uuid::new_v4());
            let (tx, mut rx) = mpsc::unbounded_channel();
            broker.register_bounded("brain", lease_id, tx);
            let dispatch_broker = broker.clone();
            let dispatch = tokio::spawn(async move {
                match operation {
                    RunnerOperation::Program => dispatch_broker
                        .dispatch_program(
                            "brain",
                            lease_id,
                            run_id,
                            1,
                            ProgramLanguage::Lisp,
                            "program".into(),
                            RunnerProgramInteraction::Interactive,
                            None,
                        )
                        .await
                        .map(|_| ()),
                    RunnerOperation::Turn => dispatch_broker
                        .dispatch_turn(
                            "brain",
                            lease_id,
                            run_id,
                            1,
                            "turn".into(),
                            Vec::new(),
                            test_approval_audience(),
                            None,
                        )
                        .await
                        .map(|_| ()),
                    RunnerOperation::ProjectMemory => dispatch_broker
                        .project_memory(
                            "brain",
                            lease_id,
                            crate::brain::store::BrainId(uuid::Uuid::new_v4()),
                            run_id,
                            1,
                            "prompt".into(),
                            "memory".into(),
                        )
                        .await
                        .map(|_| ()),
                    RunnerOperation::Cancel => unreachable!(),
                }
            });
            let envelope = rx.recv().await.expect("bounded callback was not enqueued");
            let (fence_reached, release_fence) = broker.pause_next_fence_install();
            let fence_broker = broker.clone();
            let fencing = std::thread::spawn(move || {
                assert!(fence_broker.fence_run_cancellation("brain", run_id));
            });
            fence_reached.recv().unwrap();
            let abort_broker = broker.clone();
            let (abort_done_tx, abort_done_rx) = std::sync::mpsc::channel();
            let aborting = std::thread::spawn(move || {
                abort_broker.abort_run("brain", run_id);
                abort_done_tx.send(()).unwrap();
            });
            assert!(abort_done_rx
                .recv_timeout(Duration::from_millis(25))
                .is_err());
            release_fence.send(()).unwrap();
            fencing.join().unwrap();
            aborting.join().unwrap();

            let calls = AtomicUsize::new(0);
            let sent = broker.admit_runner_dispatch(
                &envelope.dispatch_gate,
                &envelope.run_dispatch_gate,
                envelope.enforce_run_fence,
                &envelope.cancel,
                &envelope.generation_cancel,
                envelope.deadline,
                || calls.fetch_add(1, Ordering::AcqRel),
            );
            assert!(sent.is_none(), "{operation:?} crossed the durable fence");
            assert_eq!(calls.load(Ordering::Acquire), 0);
            drop(envelope);
            assert!(dispatch.await.unwrap().is_err());
        }
    }

    #[tokio::test]
    async fn test_ejection_closes_admission_and_invalidates_second_callback_before_signal() {
        let broker = BrainRunnerBroker::default();
        let connection_id = uuid::Uuid::new_v4();
        let lease_id = lease();
        broker
            .claim_connection_lease(connection_id, "brain", lease_id)
            .unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        broker
            .register_bounded_for_connection(
                connection_id,
                process_identity(31),
                "brain",
                lease_id,
                tx,
            )
            .unwrap();
        let dispatch_admission = broker.connection_dispatch_admission(connection_id).unwrap();
        let mut dispatches = Vec::new();
        let mut envelopes = Vec::new();
        for request_seq in 1..=2 {
            let dispatch_broker = broker.clone();
            let run_id = RunId(uuid::Uuid::new_v4());
            dispatches.push(tokio::spawn(async move {
                dispatch_broker
                    .dispatch_program(
                        "brain",
                        lease_id,
                        run_id,
                        request_seq,
                        ProgramLanguage::Lisp,
                        "must not cross ejection".into(),
                        RunnerProgramInteraction::Interactive,
                        None,
                    )
                    .await
            }));
            envelopes.push(rx.recv().await.expect("callback was not enqueued"));
        }

        let ejection = broker.eject_connection(connection_id).unwrap();
        assert!(dispatch_admission.try_enter().is_none());
        assert!(!ejection.eject.is_cancelled());
        let calls = AtomicUsize::new(0);
        for envelope in &envelopes {
            assert!(broker
                .admit_runner_dispatch(
                    &envelope.dispatch_gate,
                    &envelope.run_dispatch_gate,
                    envelope.enforce_run_fence,
                    &envelope.cancel,
                    &envelope.generation_cancel,
                    envelope.deadline,
                    || calls.fetch_add(1, Ordering::AcqRel),
                )
                .is_none());
        }
        assert_eq!(calls.load(Ordering::Acquire), 0);
        ejection.publish_ejection();
        ejection.mark_transport_closed();
        drop(envelopes);
        for dispatch in dispatches {
            assert!(dispatch.await.unwrap().is_err());
        }
        broker
            .begin_connection_teardown(connection_id)
            .finish()
            .unwrap();
    }
}
