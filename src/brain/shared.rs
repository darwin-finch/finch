//! Daemon-owned state for named, shared brains.
//!
//! A brain is an append-only event log plus a derived stack of programs.  The
//! daemon is the sole writer.  Attached clients receive the same numbered
//! events and can reconstruct identical state without sharing a filesystem.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::sync::broadcast;

const EVENT_CHANNEL_CAPACITY: usize = 256;
const BRAIN_EVENT_SCHEMA_VERSION: u32 = 8;
const BRAIN_METADATA_VERSION: u32 = 1;

/// Stable identity of one durable Brain. Names are mutable human aliases;
/// this ID is what future runs, attachments, cursors, and grants reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BrainId(pub uuid::Uuid);

impl BrainId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }

    fn nil() -> Self {
        Self(uuid::Uuid::nil())
    }
}

/// Stable identity of one client projection of a Brain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttachmentId(pub uuid::Uuid);

impl AttachmentId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Identity of one live transport connection for a durable attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConnectionId(pub uuid::Uuid);

impl ConnectionId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for ConnectionId {
    fn default() -> Self {
        Self(uuid::Uuid::nil())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunnerLeaseId(pub uuid::Uuid);

impl RunnerLeaseId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub uuid::Uuid);

impl RunId {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunKind {
    Interactive,
    Speculative,
    Scheduled,
    Subagent,
    Maintenance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrainRunStatus {
    QueuedForEnvironment,
    Running,
    AwaitingApproval,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl BrainRunStatus {
    fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRun {
    pub run_id: RunId,
    pub kind: BrainRunKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<RunId>,
    pub request_seq: u64,
    pub initiating_attachment_id: AttachmentId,
    pub initiated_by: String,
    pub status: BrainRunStatus,
    pub started_ms: u64,
    pub updated_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainRunnerLease {
    pub lease_id: RunnerLeaseId,
    pub subject: String,
    pub environment_generation: u64,
    pub acquired_ms: u64,
    pub expires_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRole {
    Runner,
    Driver,
    Consultant,
    Observer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainAttachment {
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub acknowledged_seq: u64,
    pub connected: bool,
    pub connection_id: Option<ConnectionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AttachmentCursorFile {
    version: u32,
    brain_id: BrainId,
    cursors: HashMap<AttachmentId, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BrainMetadata {
    version: u32,
    brain_id: BrainId,
    created_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgramLanguage {
    Forth,
    Lisp,
}

/// The one machine/workspace boundary in which a brain may cause effects.
///
/// There is deliberately no separate `execution_head`: the machine that owns
/// the workspace is the only machine allowed to execute the brain's programs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainEnvironment {
    pub machine: String,
    pub workspace: PathBuf,
    pub generation: u64,
}

/// Exact participant/environment boundary to which a Brain-owned approval
/// request is addressed. This is policy input, not a bearer credential;
/// possession of this record does not authorize a decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainApprovalAudience {
    pub brain_id: BrainId,
    pub brain: String,
    pub attachment_id: AttachmentId,
    pub subject: String,
    pub role: AttachmentRole,
    pub environment_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrainEventKind {
    RunnerLeaseAcquired {
        lease: BrainRunnerLease,
    },
    RunnerLeaseReleased {
        lease_id: RunnerLeaseId,
    },
    ClientAttached {
        attachment_id: AttachmentId,
        #[serde(default)]
        connection_id: ConnectionId,
        subject: String,
        role: AttachmentRole,
    },
    ClientDetached {
        attachment_id: AttachmentId,
        #[serde(default)]
        connection_id: ConnectionId,
    },
    RunStarted {
        run: BrainRun,
    },
    RunStatusChanged {
        run_id: RunId,
        status: BrainRunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Prompt {
        text: String,
    },
    ToolCall {
        request_seq: u64,
        tool_id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        request_seq: u64,
        tool_id: String,
        output: String,
        is_error: bool,
    },
    ApprovalRequested {
        request_seq: u64,
        approval_id: String,
        approval_kind: String,
        subject: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        audience: Option<BrainApprovalAudience>,
        detail: serde_json::Value,
    },
    ApprovalDecided {
        request_seq: u64,
        approval_id: String,
        decision: serde_json::Value,
    },
    Program {
        language: ProgramLanguage,
        source: String,
    },
    ProgramPopped {
        program_seq: u64,
    },
    Result {
        request_seq: u64,
        output: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Content-addressed typed-VM state committed after one accepted program.
    /// This is an internal Brain event, not a request to replay source after
    /// restart; the checkpoint bytes live beside the append-only log.
    RuntimeCommitted {
        request_seq: u64,
        runtime_revision: u64,
        checkpoint_sha256: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainEvent {
    /// Version of this durable event envelope. Old logs deserialize as v1 and
    /// are projected into the owning Brain's stable identity while loading.
    #[serde(default = "legacy_brain_event_schema_version")]
    pub schema_version: u32,
    #[serde(default = "BrainId::nil")]
    pub brain_id: BrainId,
    pub seq: u64,
    /// Binds this event to the exact environment revision in which it ran.
    #[serde(default = "initial_environment_generation")]
    pub environment_generation: u64,
    pub sender: String,
    pub created_ms: u64,
    #[serde(flatten)]
    pub kind: BrainEventKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrainProgram {
    pub seq: u64,
    pub sender: String,
    pub language: ProgramLanguage,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainSnapshot {
    pub brain_id: BrainId,
    pub name: String,
    pub environment: BrainEnvironment,
    pub revision: u64,
    pub events: Vec<BrainEvent>,
    pub program_stack: Vec<BrainProgram>,
    pub attachments: Vec<BrainAttachment>,
    pub runner_lease: Option<BrainRunnerLease>,
    #[serde(default)]
    pub runs: Vec<BrainRun>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrainWireMessage {
    Snapshot { brain: BrainSnapshot },
    Event { event: BrainEvent },
}

struct BrainState {
    brain_id: BrainId,
    events: Vec<BrainEvent>,
    program_stack: Vec<BrainProgram>,
    attachments: HashMap<AttachmentId, BrainAttachment>,
    runs: HashMap<RunId, BrainRun>,
    runner_lease: Option<BrainRunnerLease>,
    runtime_checkpoint: Option<RuntimeCheckpointState>,
    runtime_commit_count: u64,
    tx: broadcast::Sender<BrainEvent>,
}

#[derive(Debug, Clone)]
struct RuntimeCheckpointState {
    request_seq: u64,
    durable_revision: u64,
    checkpoint_sha256: String,
}

impl BrainState {
    fn from_events(brain_id: BrainId, events: Vec<BrainEvent>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let mut state = Self {
            brain_id,
            events: Vec::new(),
            program_stack: Vec::new(),
            attachments: HashMap::new(),
            runs: HashMap::new(),
            runner_lease: None,
            runtime_checkpoint: None,
            runtime_commit_count: 0,
            tx,
        };
        for mut event in events {
            if event.brain_id == BrainId::nil() {
                event.brain_id = brain_id;
            }
            state.apply(event);
        }
        state
    }

    fn apply(&mut self, event: BrainEvent) {
        match &event.kind {
            BrainEventKind::RunnerLeaseAcquired { lease } => {
                self.runner_lease = Some(lease.clone());
            }
            BrainEventKind::RunnerLeaseReleased { lease_id } => {
                if self
                    .runner_lease
                    .as_ref()
                    .is_some_and(|lease| lease.lease_id == *lease_id)
                {
                    self.runner_lease = None;
                }
            }
            BrainEventKind::ClientAttached {
                attachment_id,
                connection_id,
                subject,
                role,
            } => {
                let acknowledged_seq = self
                    .attachments
                    .get(attachment_id)
                    .map(|attachment| attachment.acknowledged_seq)
                    .unwrap_or(0);
                self.attachments.insert(
                    *attachment_id,
                    BrainAttachment {
                        attachment_id: *attachment_id,
                        subject: subject.clone(),
                        role: *role,
                        acknowledged_seq,
                        connected: true,
                        connection_id: Some(*connection_id),
                    },
                );
            }
            BrainEventKind::ClientDetached {
                attachment_id,
                connection_id,
            } => {
                if let Some(attachment) = self.attachments.get_mut(attachment_id) {
                    if attachment.connection_id == Some(*connection_id) {
                        attachment.connected = false;
                        attachment.connection_id = None;
                    }
                }
            }
            BrainEventKind::RunStarted { run } => {
                self.runs.insert(run.run_id, run.clone());
            }
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            } => {
                if let Some(run) = self.runs.get_mut(run_id) {
                    run.status = *status;
                    run.updated_ms = event.created_ms;
                    run.detail.clone_from(detail);
                }
            }
            BrainEventKind::Program { language, source } => {
                self.program_stack.push(BrainProgram {
                    seq: event.seq,
                    sender: event.sender.clone(),
                    language: *language,
                    source: source.clone(),
                });
            }
            BrainEventKind::ProgramPopped { program_seq } => {
                if self.program_stack.last().map(|p| p.seq) == Some(*program_seq) {
                    self.program_stack.pop();
                }
            }
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision,
                checkpoint_sha256,
            } => {
                self.runtime_commit_count += 1;
                let durable_revision = self
                    .runtime_checkpoint
                    .as_ref()
                    .map(|checkpoint| checkpoint.durable_revision)
                    .unwrap_or(0)
                    .max(*runtime_revision)
                    .max(self.runtime_commit_count);
                if self.runtime_checkpoint.as_ref().is_none_or(|current| {
                    request_seq >= &current.request_seq
                }) {
                    self.runtime_checkpoint = Some(RuntimeCheckpointState {
                        request_seq: *request_seq,
                        durable_revision,
                        checkpoint_sha256: checkpoint_sha256.clone(),
                    });
                } else if let Some(current) = self.runtime_checkpoint.as_mut() {
                    current.durable_revision = durable_revision;
                }
            }
            BrainEventKind::Prompt { .. }
            | BrainEventKind::ToolCall { .. }
            | BrainEventKind::ToolResult { .. }
            | BrainEventKind::ApprovalRequested { .. }
            | BrainEventKind::ApprovalDecided { .. }
            | BrainEventKind::Result { .. } => {}
        }
        self.events.push(event);
    }
}

/// Persistent registry of named shared brains.
///
/// Each brain is stored as human-browsable JSON Lines under
/// `~/.finch/brains/<name>/events.jsonl`.  The log is authoritative; the
/// program stack is rebuilt from it after a daemon restart.
#[derive(Clone)]
pub struct SharedBrainStore {
    root: Option<PathBuf>,
    environment: BrainEnvironment,
    brains: Arc<RwLock<HashMap<String, BrainState>>>,
    runtimes: Arc<RwLock<HashMap<String, Arc<crate::runtime::ProgramRuntime>>>>,
    runtime_checkpoints:
        Arc<RwLock<HashMap<String, crate::vm::TypedRuntimeCheckpoint>>>,
    /// One ordered turn lane per Brain. HTTP/WebSocket clients may submit
    /// concurrently, but accepted input, VM commit, and its Result event must
    /// remain an indivisible sequence against the authoritative revision.
    execution_locks: Arc<RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl SharedBrainStore {
    pub fn new(machine: impl Into<String>) -> Self {
        let root = dirs::home_dir().map(|p| p.join(".finch").join("brains"));
        Self::with_root(machine, root)
    }

    pub fn with_root(machine: impl Into<String>, root: Option<PathBuf>) -> Self {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_environment(machine, workspace, root)
    }

    pub fn with_environment(
        machine: impl Into<String>,
        workspace: impl Into<PathBuf>,
        root: Option<PathBuf>,
    ) -> Self {
        let workspace = workspace.into();
        let workspace = workspace.canonicalize().unwrap_or(workspace);
        Self {
            root,
            environment: BrainEnvironment {
                machine: machine.into(),
                workspace,
                generation: initial_environment_generation(),
            },
            brains: Arc::new(RwLock::new(HashMap::new())),
            runtimes: Arc::new(RwLock::new(HashMap::new())),
            runtime_checkpoints: Arc::new(RwLock::new(HashMap::new())),
            execution_locks: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn environment(&self) -> &BrainEnvironment {
        &self.environment
    }

    pub(crate) fn execution_lock(&self, name: &str) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let name = Self::validate_name(name)?;
        if let Some(lock) = self
            .execution_locks
            .read()
            .expect("shared brain execution-lock map poisoned")
            .get(name)
            .cloned()
        {
            return Ok(lock);
        }
        let mut locks = self
            .execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned");
        Ok(locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    pub fn validate_name(name: &str) -> Result<&str> {
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            anyhow::bail!("brain name must use 1-64 letters, numbers, '-' or '_'");
        }
        Ok(name)
    }

    pub fn list(&self) -> Result<Vec<String>> {
        self.load_all()?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let mut names: Vec<_> = brains.keys().cloned().collect();
        names.sort();
        Ok(names)
    }

    pub fn snapshot(&self, name: &str) -> Result<BrainSnapshot> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let state = brains.get(name).expect("brain loaded above");
        Ok(BrainSnapshot {
            brain_id: state.brain_id,
            name: name.to_string(),
            environment: self.environment.clone(),
            revision: state.events.last().map(|e| e.seq).unwrap_or(0),
            events: state.events.clone(),
            program_stack: state.program_stack.clone(),
            attachments: sorted_attachments(&state.attachments),
            runner_lease: state
                .runner_lease
                .clone()
                .filter(|lease| lease.expires_ms > unix_millis()),
            runs: sorted_runs(&state.runs),
        })
    }

    pub fn start_run(
        &self,
        name: &str,
        sender: &str,
        kind: BrainRunKind,
        request_seq: u64,
        initiating_attachment_id: AttachmentId,
        status: BrainRunStatus,
    ) -> Result<BrainRun> {
        let now = unix_millis();
        let run = BrainRun {
            run_id: RunId::new(),
            kind,
            parent_run_id: None,
            request_seq,
            initiating_attachment_id,
            initiated_by: sender.trim().to_string(),
            status,
            started_ms: now,
            updated_ms: now,
            detail: None,
        };
        self.push(
            name,
            sender,
            BrainEventKind::RunStarted { run: run.clone() },
        )?;
        Ok(run)
    }

    pub fn transition_run(
        &self,
        name: &str,
        sender: &str,
        run_id: RunId,
        status: BrainRunStatus,
        detail: Option<String>,
    ) -> Result<BrainRun> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let current = state
            .runs
            .get(&run_id)
            .with_context(|| format!("Brain run {} does not exist", run_id.0))?;
        validate_run_transition(current.status, status)?;
        self.push_locked(
            name,
            state,
            sender,
            BrainEventKind::RunStatusChanged {
                run_id,
                status,
                detail,
            },
        )?;
        Ok(state
            .runs
            .get(&run_id)
            .expect("run transition was projected")
            .clone())
    }

    pub fn acquire_runner_lease(
        &self,
        name: &str,
        subject: &str,
        environment_generation: u64,
        lease_id: Option<RunnerLeaseId>,
        ttl_ms: u64,
    ) -> Result<BrainRunnerLease> {
        let name = Self::validate_name(name)?;
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
            anyhow::bail!("runner subject must be 1-128 printable characters");
        }
        if environment_generation != self.environment.generation {
            anyhow::bail!("runner environment generation does not match this Brain");
        }
        if !(5_000..=300_000).contains(&ttl_ms) {
            anyhow::bail!("runner lease TTL must be between 5 and 300 seconds");
        }
        self.ensure_loaded(name)?;
        let now = unix_millis();
        let expires_ms = now.saturating_add(ttl_ms);
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        if let Some(current) = state
            .runner_lease
            .clone()
            .filter(|current| current.expires_ms > now)
        {
            if current.subject != subject || lease_id != Some(current.lease_id) {
                anyhow::bail!("Brain already has a live runner lease");
            }
            state
                .runner_lease
                .as_mut()
                .expect("live runner lease checked above")
                .expires_ms = expires_ms;
            let mut renewed = current;
            renewed.expires_ms = expires_ms;
            return Ok(renewed);
        }
        if lease_id.is_some() {
            anyhow::bail!("runner lease expired; acquire a new lease identity");
        }
        let lease = BrainRunnerLease {
            lease_id: RunnerLeaseId::new(),
            subject: subject.to_string(),
            environment_generation,
            acquired_ms: now,
            expires_ms,
        };
        self.push_locked(
            name,
            state,
            subject,
            BrainEventKind::RunnerLeaseAcquired {
                lease: lease.clone(),
            },
        )?;
        Ok(lease)
    }

    pub fn release_runner_lease(&self, name: &str, lease_id: RunnerLeaseId) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let current = state
            .runner_lease
            .as_ref()
            .context("Brain has no runner lease")?;
        if current.lease_id != lease_id {
            anyhow::bail!("runner lease is no longer current");
        }
        let subject = current.subject.clone();
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::RunnerLeaseReleased { lease_id },
        )?;
        Ok(())
    }

    pub fn expire_runner_lease(
        &self,
        name: &str,
        lease_id: RunnerLeaseId,
        now_ms: u64,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let Some(current) = state.runner_lease.as_ref() else {
            return Ok(false);
        };
        if current.lease_id != lease_id || current.expires_ms > now_ms {
            return Ok(false);
        }
        self.push_locked(
            name,
            state,
            "daemon",
            BrainEventKind::RunnerLeaseReleased { lease_id },
        )?;
        Ok(true)
    }

    pub fn attach(
        &self,
        name: &str,
        subject: &str,
        role: AttachmentRole,
        attachment_id: Option<AttachmentId>,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        let subject = subject.trim();
        if subject.is_empty() || subject.len() > 128 || subject.chars().any(char::is_control) {
            anyhow::bail!("attachment subject must be 1-128 printable characters");
        }
        let attachment_id = attachment_id.unwrap_or_else(AttachmentId::new);
        let connection_id = ConnectionId::new();
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        if let Some(existing) = state.attachments.get(&attachment_id) {
            if existing.subject != subject || existing.role != role {
                anyhow::bail!("attachment identity cannot change subject or role");
            }
            if existing.connected || existing.connection_id.is_some() {
                anyhow::bail!("Brain attachment already has a live or pending connection");
            }
        }
        let acknowledged_seq = state
            .attachments
            .get(&attachment_id)
            .map(|attachment| attachment.acknowledged_seq)
            .unwrap_or(0);
        let attachment = BrainAttachment {
            attachment_id,
            subject: subject.to_string(),
            role,
            acknowledged_seq,
            connected: false,
            connection_id: Some(connection_id),
        };
        state.attachments.insert(attachment_id, attachment.clone());
        Ok(attachment)
    }

    /// Promote an exact pending REST reservation into the live transport
    /// projection. Only this transition writes `ClientAttached`; abandoned
    /// reservations therefore never look like connected participants in the
    /// canonical event log.
    pub fn activate_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        if attachment.connected {
            anyhow::bail!("Brain attachment transport is already active");
        }
        let subject = attachment.subject.clone();
        let role = attachment.role;
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::ClientAttached {
                attachment_id,
                connection_id,
                subject: subject.clone(),
                role,
            },
        )?;
        state
            .attachments
            .get(&attachment_id)
            .cloned()
            .context("activated client missing from Brain projection")
    }

    /// Clear an abandoned pending connection without advancing the Brain log
    /// or its durable acknowledgement cursor. A timer for an older reservation
    /// cannot affect a later connection because both opaque IDs must match.
    pub fn expire_pending_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let Some(attachment) = state.attachments.get_mut(&attachment_id) else {
            return Ok(false);
        };
        if attachment.connected || attachment.connection_id != Some(connection_id) {
            return Ok(false);
        }
        attachment.connection_id = None;
        Ok(true)
    }

    pub fn detach(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<()> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let attachment = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        if !attachment.connected {
            state
                .attachments
                .get_mut(&attachment_id)
                .expect("attachment checked above")
                .connection_id = None;
            return Ok(());
        }
        let subject = attachment.subject.clone();
        self.push_locked(
            name,
            state,
            &subject,
            BrainEventKind::ClientDetached {
                attachment_id,
                connection_id,
            },
        )?;
        Ok(())
    }

    /// Remove a provisional Brain once its last live participant has left.
    ///
    /// Attachment and runner-lease events are transport bookkeeping, not
    /// conversation history. A Brain becomes durable as soon as it contains
    /// a user prompt, submitted program, result, or committed runtime state.
    pub fn remove_if_unused(&self, name: &str) -> Result<bool> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        {
            // Keep the state lock from the eligibility check through removal.
            // Otherwise a concurrent attach could recreate a live participant
            // between the check and deletion of the provisional directory.
            let mut brains = self.brains.write().expect("shared brain lock poisoned");
            let state = brains.get(name).expect("brain loaded above");
            let has_substantive_history = state.events.iter().any(|event| {
                matches!(
                    event.kind,
                    BrainEventKind::Prompt { .. }
                        | BrainEventKind::ToolCall { .. }
                        | BrainEventKind::ToolResult { .. }
                        | BrainEventKind::ApprovalRequested { .. }
                        | BrainEventKind::ApprovalDecided { .. }
                        | BrainEventKind::Program { .. }
                        | BrainEventKind::ProgramPopped { .. }
                        | BrainEventKind::Result { .. }
                        | BrainEventKind::RuntimeCommitted { .. }
                )
            });
            let has_connected_attachment = state
                .attachments
                .values()
                .any(|attachment| attachment.connected);
            if has_substantive_history || has_connected_attachment || state.runner_lease.is_some() {
                return Ok(false);
            }

            if let Some(runtime) = self
                .runtimes
                .read()
                .expect("shared brain runtime lock poisoned")
                .get(name)
                .cloned()
            {
                runtime.clear_authority_sink()?;
            }
            if let Some(root) = &self.root {
                let directory = root.join(name);
                if directory.exists() {
                    std::fs::remove_dir_all(&directory)
                        .with_context(|| format!("remove unused Brain {}", directory.display()))?;
                }
            }
            brains.remove(name);
        }
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .remove(name);
        self.execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned")
            .remove(name);
        Ok(true)
    }

    pub fn require_connection(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        let attachment = brains
            .get(name)
            .and_then(|state| state.attachments.get(&attachment_id))
            .context("unknown Brain attachment")?;
        if !attachment.connected || attachment.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        Ok(attachment.clone())
    }

    /// Persist a projection cursor without appending another numbered Brain
    /// event. Otherwise acknowledging the acknowledgement event would create
    /// an unbounded self-sustaining event stream.
    pub fn acknowledge(
        &self,
        name: &str,
        attachment_id: AttachmentId,
        connection_id: ConnectionId,
        seq: u64,
    ) -> Result<BrainAttachment> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        let head = state.events.last().map(|event| event.seq).unwrap_or(0);
        if seq > head {
            anyhow::bail!("cannot acknowledge event {seq}; Brain head is {head}");
        }
        let previous = state
            .attachments
            .get(&attachment_id)
            .context("unknown Brain attachment")?;
        if !previous.connected || previous.connection_id != Some(connection_id) {
            anyhow::bail!("Brain attachment connection is no longer current");
        }
        let previous = previous.acknowledged_seq;
        if seq < previous {
            anyhow::bail!("attachment cursor cannot move backward from {previous} to {seq}");
        }
        state
            .attachments
            .get_mut(&attachment_id)
            .expect("attachment checked above")
            .acknowledged_seq = seq;
        if let Err(error) = self.write_attachment_cursors(name, state) {
            state
                .attachments
                .get_mut(&attachment_id)
                .expect("attachment checked above")
                .acknowledged_seq = previous;
            return Err(error);
        }
        Ok(state
            .attachments
            .get(&attachment_id)
            .expect("attachment checked above")
            .clone())
    }

    /// Remove a Brain from the active namespace without destroying its log.
    /// Persistent state is moved beside the store into `brains-archive`.
    pub fn archive(&self, name: &str) -> Result<Option<PathBuf>> {
        let name = Self::validate_name(name)?;
        let archived_to = if let Some(root) = &self.root {
            let source = root.join(name);
            if source.exists() {
                let archive_root = root
                    .parent()
                    .map(|parent| parent.join("brains-archive"))
                    .unwrap_or_else(|| root.join("archive"));
                std::fs::create_dir_all(&archive_root)
                    .with_context(|| format!("create {}", archive_root.display()))?;
                let destination = archive_root.join(format!("{name}-{}", unix_millis()));
                std::fs::rename(&source, &destination).with_context(|| {
                    format!("archive {} as {}", source.display(), destination.display())
                })?;
                Some(destination)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(runtime) = self
            .runtimes
            .read()
            .expect("shared brain runtime lock poisoned")
            .get(name)
            .cloned()
        {
            runtime.clear_authority_sink()?;
        }
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .remove(name);
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .remove(name);
        self.execution_locks
            .write()
            .expect("shared brain execution-lock map poisoned")
            .remove(name);
        Ok(archived_to)
    }

    pub fn push(&self, name: &str, sender: &str, kind: BrainEventKind) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let mut brains = self.brains.write().expect("shared brain lock poisoned");
        let state = brains.get_mut(name).expect("brain loaded above");
        self.push_locked(name, state, sender, kind)
    }

    fn push_locked(
        &self,
        name: &str,
        state: &mut BrainState,
        sender: &str,
        kind: BrainEventKind,
    ) -> Result<BrainEvent> {
        let event = BrainEvent {
            schema_version: BRAIN_EVENT_SCHEMA_VERSION,
            brain_id: state.brain_id,
            seq: state.events.last().map(|e| e.seq + 1).unwrap_or(1),
            environment_generation: self.environment.generation,
            sender: sender.trim().to_string(),
            created_ms: unix_millis(),
            kind,
        };
        self.append_event(name, &event)?;
        state.apply(event.clone());
        let _ = state.tx.send(event.clone());
        Ok(event)
    }

    pub fn pop_program(&self, name: &str, sender: &str) -> Result<Option<BrainEvent>> {
        let snapshot = self.snapshot(name)?;
        let Some(program) = snapshot.program_stack.last() else {
            return Ok(None);
        };
        self.push(
            name,
            sender,
            BrainEventKind::ProgramPopped {
                program_seq: program.seq,
            },
        )
        .map(Some)
    }

    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<BrainEvent>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let brains = self.brains.read().expect("shared brain lock poisoned");
        Ok(brains.get(name).expect("brain loaded above").tx.subscribe())
    }

    /// Return the one live typed runtime for a named Brain, restoring its
    /// latest reducible checkpoint on first access after daemon restart.
    pub fn program_runtime(&self, name: &str) -> Result<Arc<crate::runtime::ProgramRuntime>> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(runtime) = self
            .runtimes
            .read()
            .expect("shared brain runtime lock poisoned")
            .get(name)
            .cloned()
        {
            return Ok(runtime);
        }
        let checkpoint = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.clone());
        let mut runtime = match checkpoint {
            Some(checkpoint) => crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
                self.read_runtime_checkpoint(name, &checkpoint.checkpoint_sha256)?,
                checkpoint.durable_revision,
            )?,
            None => crate::runtime::ProgramRuntime::new(),
        };
        if let Some(authority_store) = self.runtime_authority_store(name) {
            authority_store
                .restore_into(&mut runtime)
                .with_context(|| format!("restore authority for named Brain '{name}'"))?;
            let sink_store = authority_store.clone();
            runtime.set_authority_sink(Arc::new(move |state| sink_store.save_state(state)))?;
        }
        let runtime = Arc::new(runtime);
        let mut runtimes = self
            .runtimes
            .write()
            .expect("shared brain runtime lock poisoned");
        Ok(runtimes
            .entry(name.to_string())
            .or_insert_with(|| Arc::clone(&runtime))
            .clone())
    }

    /// Return the durable reducible state a newly connected environment
    /// runner must install before accepting ProgramRuns. Authority and live
    /// host resources are intentionally stored and rebound separately.
    pub fn runner_checkpoint(
        &self,
        name: &str,
    ) -> Result<(u64, crate::vm::TypedRuntimeCheckpoint)> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        let state = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.clone());
        match state {
            Some(state) => Ok((
                state.durable_revision,
                self.read_runtime_checkpoint(name, &state.checkpoint_sha256)?,
            )),
            None => {
                let runtime = crate::runtime::ProgramRuntime::new();
                let snapshot = runtime
                    .revision_history()?
                    .pop()
                    .context("fresh typed runtime has no initial checkpoint")?;
                Ok((
                    snapshot.revision,
                    snapshot
                        .checkpoint
                        .context("fresh typed runtime is not checkpointable")?,
                ))
            }
        }
    }

    /// Journal the latest checkpoint only after a ProgramRuntime commit. The
    /// source event remains the audit record; restart restores state rather
    /// than replaying effects from that source.
    pub fn commit_runtime(
        &self,
        name: &str,
        request_seq: u64,
        runtime_revision: u64,
        runtime: &crate::runtime::ProgramRuntime,
    ) -> Result<BrainEvent> {
        let snapshot = runtime
            .revision_history()?
            .into_iter()
            .find(|snapshot| snapshot.revision == runtime_revision)
            .with_context(|| {
                format!("typed runtime has no revision snapshot {runtime_revision}")
            })?;
        let checkpoint = snapshot.checkpoint.context(
            "typed runtime revision contains host-owned handles and cannot be persisted yet",
        )?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        self.write_runtime_checkpoint(name, &checkpoint_sha256, &encoded)?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.clone(), checkpoint);
        if let Some(authority_store) = self.runtime_authority_store(name) {
            authority_store
                .save(runtime)
                .with_context(|| format!("persist authority for named Brain '{name}'"))?;
        }
        self.push(
            name,
            "daemon",
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision: snapshot.revision,
                checkpoint_sha256,
            },
        )
    }

    /// Commit reducible state returned by the frontend that owns this Brain's
    /// environment. The daemon validates and journals the checkpoint but does
    /// not execute the source or inherit the frontend's host authority.
    pub fn commit_runner_runtime(
        &self,
        name: &str,
        request_seq: u64,
        runtime_revision: u64,
        checkpoint: crate::vm::TypedRuntimeCheckpoint,
    ) -> Result<BrainEvent> {
        let name = Self::validate_name(name)?;
        self.ensure_loaded(name)?;
        if let Some(current) = self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .get(name)
            .and_then(|state| state.runtime_checkpoint.as_ref())
        {
            if runtime_revision <= current.durable_revision {
                anyhow::bail!(
                    "runner checkpoint revision {runtime_revision} does not advance durable revision {}",
                    current.durable_revision
                );
            }
        }
        let restored = crate::runtime::ProgramRuntime::from_checkpoint_at_revision(
            checkpoint.clone(),
            runtime_revision,
        )?;
        let encoded = serde_json::to_vec(&checkpoint)?;
        let checkpoint_sha256 = hex::encode(Sha256::digest(&encoded));
        self.write_runtime_checkpoint(name, &checkpoint_sha256, &encoded)?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.clone(), checkpoint);
        self.runtimes
            .write()
            .expect("shared brain runtime lock poisoned")
            .insert(name.to_string(), Arc::new(restored));
        self.push(
            name,
            "runner",
            BrainEventKind::RuntimeCommitted {
                request_seq,
                runtime_revision,
                checkpoint_sha256,
            },
        )
    }

    fn read_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
    ) -> Result<crate::vm::TypedRuntimeCheckpoint> {
        if let Some(checkpoint) = self
            .runtime_checkpoints
            .read()
            .expect("shared brain checkpoint lock poisoned")
            .get(checkpoint_sha256)
            .cloned()
        {
            return Ok(checkpoint);
        }
        let root = self
            .root
            .as_ref()
            .context("named Brain checkpoint is not available in this process")?;
        let path = root
            .join(name)
            .join("runtime")
            .join(format!("{checkpoint_sha256}.json"));
        let encoded = std::fs::read(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let actual = hex::encode(Sha256::digest(&encoded));
        if actual != checkpoint_sha256 {
            anyhow::bail!("typed runtime checkpoint hash mismatch for {checkpoint_sha256}");
        }
        let checkpoint: crate::vm::TypedRuntimeCheckpoint = serde_json::from_slice(&encoded)
            .with_context(|| format!("parse {}", path.display()))?;
        self.runtime_checkpoints
            .write()
            .expect("shared brain checkpoint lock poisoned")
            .insert(checkpoint_sha256.to_string(), checkpoint.clone());
        Ok(checkpoint)
    }

    fn write_runtime_checkpoint(
        &self,
        name: &str,
        checkpoint_sha256: &str,
        encoded: &[u8],
    ) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let directory = root.join(name).join("runtime");
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join(format!("{checkpoint_sha256}.json"));
        if path.exists() {
            return Ok(());
        }
        let temporary = directory.join(format!(
            ".{checkpoint_sha256}.{}.tmp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    /// Return the host-policy record associated with this named Brain. This
    /// path is deliberately neither content-addressed nor part of a VM
    /// checkpoint: restoring executable state alone must never restore
    /// authority.
    fn runtime_authority_store(
        &self,
        name: &str,
    ) -> Option<crate::runtime::archive_store::ProgramRuntimeAuthorityStore> {
        self.root.as_ref().map(|root| {
            crate::runtime::archive_store::ProgramRuntimeAuthorityStore::new(
                root.join(name).join("authority.json"),
            )
        })
    }

    fn ensure_loaded(&self, name: &str) -> Result<()> {
        if self
            .brains
            .read()
            .expect("shared brain lock poisoned")
            .contains_key(name)
        {
            return Ok(());
        }
        let brain_id = self.load_or_create_metadata(name)?.brain_id;
        let events = self.read_events(name)?;
        for event in &events {
            if event.schema_version > BRAIN_EVENT_SCHEMA_VERSION {
                anyhow::bail!(
                    "Brain '{name}' contains unsupported event schema version {}",
                    event.schema_version
                );
            }
            if event.brain_id != BrainId::nil() && event.brain_id != brain_id {
                anyhow::bail!(
                    "Brain '{name}' event #{} belongs to a different Brain identity",
                    event.seq
                );
            }
        }
        // Preserve an empty named Brain across daemon restarts, even before
        // its first conversational event is appended.
        if let Some(root) = &self.root {
            let directory = root.join(name);
            std::fs::create_dir_all(&directory)
                .with_context(|| format!("create {}", directory.display()))?;
        }
        let cursors = self.read_attachment_cursors(name, brain_id)?;
        let mut state = BrainState::from_events(brain_id, events);
        for attachment in state.attachments.values_mut() {
            // A process restart disconnects every transport projection;
            // reconnect appends a fresh ClientAttached event.
            attachment.connected = false;
            attachment.connection_id = None;
        }
        for (attachment_id, acknowledged_seq) in cursors {
            if let Some(attachment) = state.attachments.get_mut(&attachment_id) {
                attachment.acknowledged_seq = acknowledged_seq.min(
                    state.events.last().map(|event| event.seq).unwrap_or(0),
                );
            }
        }
        // Queued work has not executed and may be safely offered to the next
        // valid runner lease. A run that was already executing or suspended
        // for approval has unknown external progress after daemon restart and
        // must never be replayed implicitly.
        let orphaned_runs = state
            .runs
            .values()
            .filter(|run| {
                matches!(
                    run.status,
                    BrainRunStatus::Running | BrainRunStatus::AwaitingApproval
                )
            })
            .map(|run| run.run_id)
            .collect::<Vec<_>>();
        for run_id in orphaned_runs {
            let event = BrainEvent {
                schema_version: BRAIN_EVENT_SCHEMA_VERSION,
                brain_id: state.brain_id,
                seq: state.events.last().map(|event| event.seq + 1).unwrap_or(1),
                environment_generation: self.environment.generation,
                sender: "daemon".into(),
                created_ms: unix_millis(),
                kind: BrainEventKind::RunStatusChanged {
                    run_id,
                    status: BrainRunStatus::Interrupted,
                    detail: Some("daemon restarted before the run reached a terminal state".into()),
                },
            };
            self.append_event(name, &event)?;
            state.apply(event);
        }
        self.brains
            .write()
            .expect("shared brain lock poisoned")
            .entry(name.to_string())
            .or_insert(state);
        Ok(())
    }

    fn read_attachment_cursors(
        &self,
        name: &str,
        brain_id: BrainId,
    ) -> Result<HashMap<AttachmentId, u64>> {
        let Some(root) = &self.root else {
            return Ok(HashMap::new());
        };
        let path = root.join(name).join("attachments.json");
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(HashMap::new());
        };
        let file: AttachmentCursorFile = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse {}", path.display()))?;
        if file.version != 1 || file.brain_id != brain_id {
            anyhow::bail!("attachment cursor identity mismatch at {}", path.display());
        }
        Ok(file.cursors)
    }

    fn write_attachment_cursors(&self, name: &str, state: &BrainState) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let file = AttachmentCursorFile {
            version: 1,
            brain_id: state.brain_id,
            cursors: state
                .attachments
                .iter()
                .map(|(id, attachment)| (*id, attachment.acknowledged_seq))
                .collect(),
        };
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("attachments.json");
        let temporary = directory.join(format!(".attachments.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(&file)?)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, &path)
            .with_context(|| format!("commit {}", path.display()))?;
        Ok(())
    }

    fn load_or_create_metadata(&self, name: &str) -> Result<BrainMetadata> {
        let Some(root) = &self.root else {
            return Ok(BrainMetadata {
                version: BRAIN_METADATA_VERSION,
                brain_id: BrainId::new(),
                created_ms: unix_millis(),
            });
        };
        let directory = root.join(name);
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        let path = directory.join("metadata.json");
        if path.exists() {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read {}", path.display()))?;
            let metadata: BrainMetadata = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?;
            if metadata.version != BRAIN_METADATA_VERSION || metadata.brain_id == BrainId::nil() {
                anyhow::bail!("unsupported or invalid Brain metadata at {}", path.display());
            }
            return Ok(metadata);
        }
        let metadata = BrainMetadata {
            version: BRAIN_METADATA_VERSION,
            brain_id: BrainId::new(),
            created_ms: unix_millis(),
        };
        let encoded = serde_json::to_vec_pretty(&metadata)?;
        let temporary = directory.join(format!(".metadata.{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temporary, encoded)
            .with_context(|| format!("write {}", temporary.display()))?;
        // A rename can replace a winner on Unix. Linking the fully written
        // temporary file creates the final name only if it is still absent,
        // so concurrent daemon starts cannot split one alias into two IDs.
        match std::fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temporary);
                Ok(metadata)
            }
            Err(_error) if path.exists() => {
                let _ = std::fs::remove_file(&temporary);
                let bytes = std::fs::read(&path)
                    .with_context(|| format!("read {} after metadata race", path.display()))?;
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {} after metadata race", path.display()))
            }
            Err(error) => {
                let _ = std::fs::remove_file(&temporary);
                Err(error).with_context(|| format!("commit {}", path.display()))
            }
        }
    }

    fn load_all(&self) -> Result<()> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        let Ok(entries) = std::fs::read_dir(root) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                if let Some(name) = entry.file_name().to_str() {
                    if Self::validate_name(name).is_ok() {
                        self.ensure_loaded(name)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn event_path(&self, name: &str) -> Option<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(name).join("events.jsonl"))
    }

    fn read_events(&self, name: &str) -> Result<Vec<BrainEvent>> {
        let Some(path) = self.event_path(name) else {
            return Ok(Vec::new());
        };
        let Ok(file) = std::fs::File::open(&path) else {
            return Ok(Vec::new());
        };
        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_no, line)| match line {
                Ok(line) if line.trim().is_empty() => None,
                other => Some((line_no, other)),
            })
            .map(|(line_no, line)| {
                let line = line.with_context(|| format!("read {}", path.display()))?;
                serde_json::from_str(&line)
                    .with_context(|| format!("parse {} line {}", path.display(), line_no + 1))
            })
            .collect()
    }

    fn append_event(&self, name: &str, event: &BrainEvent) -> Result<()> {
        let Some(path) = self.event_path(name) else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        serde_json::to_writer(&mut file, event)?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

pub(crate) fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn sorted_attachments(
    attachments: &HashMap<AttachmentId, BrainAttachment>,
) -> Vec<BrainAttachment> {
    let mut attachments = attachments.values().cloned().collect::<Vec<_>>();
    attachments.sort_by_key(|attachment| attachment.attachment_id.0);
    attachments
}

fn sorted_runs(runs: &HashMap<RunId, BrainRun>) -> Vec<BrainRun> {
    let mut runs = runs.values().cloned().collect::<Vec<_>>();
    runs.sort_by_key(|run| (run.request_seq, run.run_id.0));
    runs
}

fn validate_run_transition(from: BrainRunStatus, to: BrainRunStatus) -> Result<()> {
    use BrainRunStatus::*;
    let allowed = match from {
        QueuedForEnvironment => matches!(to, Running | Cancelled | Failed),
        Running => matches!(
            to,
            AwaitingApproval | Completed | Failed | Cancelled | Interrupted
        ),
        AwaitingApproval => matches!(to, Running | Failed | Cancelled | Interrupted),
        Interrupted => matches!(to, Running | Failed | Cancelled),
        Completed | Failed | Cancelled => false,
    };
    if !allowed {
        anyhow::bail!("invalid Brain run transition from {from:?} to {to:?}");
    }
    if from.is_terminal() {
        anyhow::bail!("terminal Brain run cannot transition");
    }
    Ok(())
}

const fn initial_environment_generation() -> u64 {
    1
}

const fn legacy_brain_event_schema_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lifecycle_is_event_sourced_and_terminal_state_is_final() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let prompt = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "inspect".into(),
                },
            )
            .unwrap();
        let run = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                prompt.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .unwrap();
        store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Completed,
                None,
            )
            .unwrap();
        assert!(store
            .transition_run(
                "shared",
                "runner",
                run.run_id,
                BrainRunStatus::Running,
                None,
            )
            .is_err());

        drop(store);
        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = &restarted.snapshot("shared").unwrap().runs[0];
        assert_eq!(restored.run_id, run.run_id);
        assert_eq!(restored.request_seq, prompt.seq);
        assert_eq!(restored.status, BrainRunStatus::Completed);
        assert!(restored.updated_ms >= restored.started_ms);
    }

    #[test]
    fn restart_interrupts_started_runs_without_replaying_queued_runs() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let running_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "started".into(),
                },
            )
            .unwrap();
        let running = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                running_request.seq,
                attachment.attachment_id,
                BrainRunStatus::Running,
            )
            .unwrap();
        let queued_request = store
            .push(
                "shared",
                "alice",
                BrainEventKind::Prompt {
                    text: "queued".into(),
                },
            )
            .unwrap();
        let queued = store
            .start_run(
                "shared",
                "alice",
                BrainRunKind::Interactive,
                queued_request.seq,
                attachment.attachment_id,
                BrainRunStatus::QueuedForEnvironment,
            )
            .unwrap();
        drop(store);

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("shared").unwrap();
        assert_eq!(
            snapshot
                .runs
                .iter()
                .find(|candidate| candidate.run_id == running.run_id)
                .unwrap()
                .status,
            BrainRunStatus::Interrupted
        );
        assert_eq!(
            snapshot
                .runs
                .iter()
                .find(|candidate| candidate.run_id == queued.run_id)
                .unwrap()
                .status,
            BrainRunStatus::QueuedForEnvironment
        );
        assert!(snapshot.events.iter().any(|event| {
            matches!(
                event.kind,
                BrainEventKind::RunStatusChanged {
                    run_id,
                    status: BrainRunStatus::Interrupted,
                    ..
                } if run_id == running.run_id
            )
        }));
    }

    #[test]
    fn program_stack_is_rebuilt_from_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let first = store
            .push(
                "finch",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Forth,
                    source: "2 3 +".into(),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "bob",
                BrainEventKind::Prompt {
                    text: "explain that".into(),
                },
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("finch").unwrap();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(snapshot.program_stack.len(), 1);
        assert_eq!(snapshot.program_stack[0].seq, first.seq);
        assert_eq!(snapshot.environment.machine, "workstation.local");
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn tool_and_approval_lifecycle_is_rebuilt_from_the_event_log() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let brain_id = store.snapshot("finch").unwrap().brain_id;
        let audience = BrainApprovalAudience {
            brain_id,
            brain: "finch".into(),
            attachment_id: AttachmentId(uuid::Uuid::new_v4()),
            subject: "alice".into(),
            role: AttachmentRole::Driver,
            environment_generation: 1,
        };
        store
            .push(
                "finch",
                "provider",
                BrainEventKind::ToolCall {
                    request_seq: 1,
                    tool_id: "tool-1".into(),
                    name: "search_word".into(),
                    input: serde_json::json!({"query": "fib"}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "runner",
                BrainEventKind::ApprovalRequested {
                    request_seq: 1,
                    approval_id: "tool-1".into(),
                    approval_kind: "tool".into(),
                    subject: "search_word".into(),
                    audience: Some(audience.clone()),
                    detail: serde_json::json!({"input": {"query": "fib"}}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "alice",
                BrainEventKind::ApprovalDecided {
                    request_seq: 1,
                    approval_id: "tool-1".into(),
                    decision: serde_json::json!({"choice": "approve_once"}),
                },
            )
            .unwrap();
        store
            .push(
                "finch",
                "runner",
                BrainEventKind::ToolResult {
                    request_seq: 1,
                    tool_id: "tool-1".into(),
                    output: "found".into(),
                    is_error: false,
                },
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("workstation.local", Some(temp.path().into()));
        let snapshot = restarted.snapshot("finch").unwrap();
        assert!(matches!(
            &snapshot.events[0].kind,
            BrainEventKind::ToolCall { tool_id, input, .. }
                if tool_id == "tool-1" && input["query"] == "fib"
        ));
        assert!(matches!(
            &snapshot.events[1].kind,
            BrainEventKind::ApprovalRequested {
                approval_id,
                subject,
                audience: Some(event_audience),
                ..
            }
                if approval_id == "tool-1"
                    && subject == "search_word"
                    && event_audience == &audience
        ));
        assert!(matches!(
            &snapshot.events[2].kind,
            BrainEventKind::ApprovalDecided { approval_id, decision, .. }
                if approval_id == "tool-1" && decision["choice"] == "approve_once"
        ));
        assert!(matches!(
            &snapshot.events[3].kind,
            BrainEventKind::ToolResult { tool_id, output, is_error: false, .. }
                if tool_id == "tool-1" && output == "found"
        ));
    }

    #[test]
    fn legacy_approval_event_without_audience_still_deserializes() {
        let event: BrainEvent = serde_json::from_value(serde_json::json!({
            "schema_version": 6,
            "brain_id": uuid::Uuid::nil(),
            "seq": 1,
            "environment_generation": 1,
            "sender": "runner",
            "created_ms": 0,
            "kind": "approval_requested",
            "request_seq": 1,
            "approval_id": "approval-1",
            "approval_kind": "tool",
            "subject": "search_word",
            "detail": {}
        }))
        .unwrap();
        assert!(matches!(
            event.kind,
            BrainEventKind::ApprovalRequested { audience: None, .. }
        ));
    }

    #[test]
    fn pop_is_an_event_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        store
            .push(
                "brain",
                "alice",
                BrainEventKind::Program {
                    language: ProgramLanguage::Lisp,
                    source: "(+ 1 2)".into(),
                },
            )
            .unwrap();
        let popped = store.pop_program("brain", "alice").unwrap().unwrap();
        assert!(matches!(popped.kind, BrainEventKind::ProgramPopped { .. }));

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert!(restarted
            .snapshot("brain")
            .unwrap()
            .program_stack
            .is_empty());
    }

    #[test]
    fn subscribers_receive_the_authoritative_sequence() {
        let store = SharedBrainStore::with_root("box.local", None);
        let mut first = store.subscribe("brain").unwrap();
        let mut second = store.subscribe("brain").unwrap();
        let event = store
            .push(
                "brain",
                "alice",
                BrainEventKind::Prompt { text: "hi".into() },
            )
            .unwrap();
        assert_eq!(first.try_recv().unwrap(), event);
        assert_eq!(second.try_recv().unwrap(), event);
    }

    #[test]
    fn empty_named_brain_remains_listed_after_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let original = store.snapshot("quiet-brain").unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(restarted.list().unwrap(), vec!["quiet-brain"]);
        let restored = restarted.snapshot("quiet-brain").unwrap();
        assert_eq!(restored.revision, 0);
        assert_eq!(restored.brain_id, original.brain_id);
        assert_ne!(restored.brain_id, BrainId::nil());
        assert!(temp.path().join("quiet-brain/metadata.json").exists());
    }

    #[test]
    fn unused_brain_is_removed_after_its_last_participant_leaves() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let attachment = store
            .attach("provisional", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let lease = store
            .acquire_runner_lease("provisional", "alice", generation, None, 60_000)
            .unwrap();

        store
            .detach(
                "provisional",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        assert!(!store.remove_if_unused("provisional").unwrap());
        store
            .release_runner_lease("provisional", lease.lease_id)
            .unwrap();
        assert!(store.remove_if_unused("provisional").unwrap());
        assert!(!temp.path().join("provisional").exists());
        assert!(!store.list().unwrap().contains(&"provisional".to_string()));
    }

    #[test]
    fn substantive_brain_survives_after_every_participant_leaves() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("durable", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .push(
                "durable",
                "alice",
                BrainEventKind::Prompt {
                    text: "remember this".into(),
                },
            )
            .unwrap();
        store
            .detach(
                "durable",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();

        assert!(!store.remove_if_unused("durable").unwrap());
        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted.snapshot("durable").unwrap().program_stack.len(),
            0
        );
        assert!(restarted
            .snapshot("durable")
            .unwrap()
            .events
            .iter()
            .any(|event| {
                matches!(&event.kind, BrainEventKind::Prompt { text } if text == "remember this")
            }));
    }

    #[test]
    fn legacy_events_are_projected_into_the_persisted_brain_identity() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("legacy");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("events.jsonl"),
            r#"{"seq":1,"environment_generation":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hello"}
"#,
        )
        .unwrap();

        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let snapshot = store.snapshot("legacy").unwrap();
        assert_ne!(snapshot.brain_id, BrainId::nil());
        assert_eq!(snapshot.events[0].schema_version, 1);
        assert_eq!(snapshot.events[0].brain_id, snapshot.brain_id);

        let appended = store
            .push(
                "legacy",
                "bob",
                BrainEventKind::Prompt {
                    text: "again".into(),
                },
            )
            .unwrap();
        assert_eq!(appended.schema_version, BRAIN_EVENT_SCHEMA_VERSION);
        assert_eq!(appended.brain_id, snapshot.brain_id);
    }

    #[test]
    fn concurrent_metadata_creation_converges_on_one_identity() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let workers = (0..8)
            .map(|_| {
                let root = root.clone();
                std::thread::spawn(move || {
                    SharedBrainStore::with_root("box.local", Some(root))
                        .snapshot("shared")
                        .unwrap()
                        .brain_id
                })
            })
            .collect::<Vec<_>>();
        let ids = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert!(ids.iter().all(|id| *id == ids[0]));
        assert_ne!(ids[0], BrainId::nil());
    }

    #[test]
    fn attachment_cursor_is_monotonic_and_survives_restart() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let attachment = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let attachment = store
            .activate_connection(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();
        let head = store.snapshot("shared").unwrap().revision;
        let connection_id = attachment.connection_id.unwrap();
        let acknowledged = store
            .acknowledge("shared", attachment.attachment_id, connection_id, head)
            .unwrap();
        assert_eq!(acknowledged.acknowledged_seq, head);
        assert!(store
            .acknowledge(
                "shared",
                attachment.attachment_id,
                connection_id,
                head + 1,
            )
            .is_err());
        assert!(store
            .acknowledge(
                "shared",
                attachment.attachment_id,
                connection_id,
                head - 1,
            )
            .is_err());
        store
            .detach(
                "shared",
                attachment.attachment_id,
                attachment.connection_id.unwrap(),
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted
            .snapshot("shared")
            .unwrap()
            .attachments
            .into_iter()
            .find(|candidate| candidate.attachment_id == attachment.attachment_id)
            .unwrap();
        assert_eq!(restored.acknowledged_seq, head);
        assert!(!restored.connected);
        let reattached = restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(attachment.attachment_id),
            )
            .unwrap();
        assert!(!reattached.connected);
        assert_eq!(reattached.acknowledged_seq, head);
        let reattached = restarted
            .activate_connection(
                "shared",
                reattached.attachment_id,
                reattached.connection_id.unwrap(),
            )
            .unwrap();
        assert!(reattached.connected);
        assert!(restarted
            .attach(
                "shared",
                "alice",
                AttachmentRole::Observer,
                Some(attachment.attachment_id),
            )
            .is_err());
    }

    #[test]
    fn concurrent_attach_cannot_rebind_one_identity() {
        let store = Arc::new(SharedBrainStore::with_root("box.local", None));
        let attachment_id = AttachmentId::new();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let attempts = [AttachmentRole::Driver, AttachmentRole::Observer]
            .into_iter()
            .map(|role| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store.attach("shared", "alice", role, Some(attachment_id))
                })
            })
            .collect::<Vec<_>>();
        let results = attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
        let winner = results.into_iter().find_map(Result::ok).unwrap();
        store
            .activate_connection(
                "shared",
                winner.attachment_id,
                winner.connection_id.unwrap(),
            )
            .unwrap();
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.attachments.len(), 1);
        assert_eq!(
            snapshot
                .events
                .iter()
                .filter(|event| matches!(event.kind, BrainEventKind::ClientAttached { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn stale_connection_cannot_disconnect_a_reattached_client() {
        let store = SharedBrainStore::with_root("box.local", None);
        let first = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        store
            .activate_connection(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
            )
            .unwrap();
        store
            .detach("shared", first.attachment_id, first.connection_id.unwrap())
            .unwrap();
        let second = store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .unwrap();
        store
            .activate_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap();

        assert_ne!(first.connection_id, second.connection_id);
        assert!(store
            .detach(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
            )
            .is_err());
        let head = store.snapshot("shared").unwrap().revision;
        assert!(store
            .acknowledge(
                "shared",
                first.attachment_id,
                first.connection_id.unwrap(),
                head,
            )
            .is_err());
        assert!(store
            .require_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .is_ok());
    }

    #[test]
    fn abandoned_attachment_reservation_expires_without_advancing_cursor_or_log() {
        let store = SharedBrainStore::with_root("box.local", None);
        let first = store
            .attach("shared", "alice", AttachmentRole::Driver, None)
            .unwrap();
        let first_connection = first.connection_id.unwrap();
        assert!(!first.connected);
        assert_eq!(store.snapshot("shared").unwrap().revision, 0);
        assert!(store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .is_err());

        assert!(store
            .expire_pending_connection("shared", first.attachment_id, first_connection)
            .unwrap());
        let second = store
            .attach(
                "shared",
                "alice",
                AttachmentRole::Driver,
                Some(first.attachment_id),
            )
            .unwrap();
        assert_eq!(second.acknowledged_seq, 0);
        assert_eq!(store.snapshot("shared").unwrap().revision, 0);
        assert!(!store
            .expire_pending_connection("shared", first.attachment_id, first_connection)
            .unwrap());

        let active = store
            .activate_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap();
        assert!(active.connected);
        assert!(!store
            .expire_pending_connection(
                "shared",
                second.attachment_id,
                second.connection_id.unwrap(),
            )
            .unwrap());
        let snapshot = store.snapshot("shared").unwrap();
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.attachments[0].acknowledged_seq, 0);
    }

    #[test]
    fn runner_lease_is_exclusive_renewable_and_event_sourced() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let generation = store.environment().generation;
        let lease = store
            .acquire_runner_lease("shared", "console-a", generation, None, 60_000)
            .unwrap();
        assert!(store
            .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
            .is_err());
        assert!(store
            .acquire_runner_lease(
                "shared",
                "console-a",
                generation,
                Some(RunnerLeaseId(uuid::Uuid::new_v4())),
                60_000,
            )
            .is_err());
        let renewed = store
            .acquire_runner_lease(
                "shared",
                "console-a",
                generation,
                Some(lease.lease_id),
                120_000,
            )
            .unwrap();
        assert_eq!(renewed.lease_id, lease.lease_id);
        assert!(renewed.expires_ms >= lease.expires_ms);

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        assert_eq!(
            restarted
                .snapshot("shared")
                .unwrap()
                .runner_lease
                .unwrap()
                .lease_id,
            lease.lease_id
        );
        restarted
            .release_runner_lease("shared", lease.lease_id)
            .unwrap();
        assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
        assert!(restarted
            .acquire_runner_lease("shared", "console-b", generation + 1, None, 60_000)
            .is_err());
        let replacement = restarted
            .acquire_runner_lease("shared", "console-b", generation, None, 60_000)
            .unwrap();
        assert!(restarted
            .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms - 1)
            .is_ok_and(|expired| !expired));
        assert!(restarted
            .expire_runner_lease("shared", replacement.lease_id, replacement.expires_ms)
            .unwrap());
        assert!(restarted.snapshot("shared").unwrap().runner_lease.is_none());
    }

    #[test]
    fn archive_removes_a_brain_but_preserves_its_log() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("brains");
        let store = SharedBrainStore::with_root("box.local", Some(root.clone()));
        store
            .push("old", "alice", BrainEventKind::Prompt { text: "hi".into() })
            .unwrap();
        let retained_runtime = store.program_runtime("old").unwrap();
        retained_runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();

        let archive = store.archive("old").unwrap().unwrap();
        assert!(!store.list().unwrap().contains(&"old".to_string()));
        assert!(!root.join("old").exists());
        assert!(archive.join("events.jsonl").exists());
        assert!(archive.join("authority.json").exists());

        retained_runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::MemoryRead,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        assert!(
            !root.join("old").exists(),
            "a retained runtime must not recreate its archived authority path"
        );
    }

    #[tokio::test]
    async fn attached_clients_share_one_ordered_turn_lane_per_brain() {
        let store = SharedBrainStore::with_root("box.local", None);
        let first = store.execution_lock("brain").unwrap();
        let same_brain = store.execution_lock("brain").unwrap();
        let other_brain = store.execution_lock("other").unwrap();
        assert!(Arc::ptr_eq(&first, &same_brain));
        assert!(!Arc::ptr_eq(&first, &other_brain));

        let first_turn = first.lock_owned().await;
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let waiting = tokio::spawn(async move {
            let _second_turn = same_brain.lock_owned().await;
            entered_tx.send(()).unwrap();
        });
        tokio::task::yield_now().await;
        assert!(entered_rx.try_recv().is_err());

        drop(first_turn);
        entered_rx.recv().await.unwrap();
        waiting.await.unwrap();
    }

    #[tokio::test]
    async fn named_brain_restores_one_typed_runtime_without_replaying_source() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("brain:event:1".into()),
                source: ": square ( S n:int -- S int ! pure ) n n * ;".into(),
                intent: "define square".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        let committed_revision = outcome.output_revision;
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|revision| revision.checkpoint.clone())
            .unwrap();
        let encoded_checkpoint = serde_json::to_string(&checkpoint).unwrap();
        serde_json::from_str::<crate::vm::TypedRuntimeCheckpoint>(&encoded_checkpoint)
            .expect("checkpoint itself must round-trip through JSON");
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();

        let event_log = std::fs::read_to_string(temp.path().join("brain/events.jsonl")).unwrap();
        for line in event_log.lines() {
            if let Err(error) = serde_json::from_str::<BrainEvent>(line) {
                panic!("checkpoint event must round-trip through JSONL: {error}\n{line}");
            }
        }

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), committed_revision);
        let outcome = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("brain:event:2".into()),
                source: "(square 7)".into(),
                intent: "call restored definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            outcome.status,
            crate::runtime::outcome::ExecutionStatus::Completed
        );
        assert_eq!(outcome.values, vec![crate::programs::ProgramValue::Int(49)]);
        assert_eq!(outcome.output_revision, committed_revision + 1);
    }

    #[tokio::test]
    async fn named_brain_commits_a_validated_frontend_runner_checkpoint() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        store.snapshot("brain").unwrap();
        let runner = crate::runtime::ProgramRuntime::new();
        let outcome = runner
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: Some("runner:event:1".into()),
                source: "(define (triple (n : int)) (* n 3))".into(),
                intent: "frontend runner definition".into(),
                effect: crate::programs::ExecutionEffect::VmWrite,
                declared_capabilities: Vec::new(),
                manifest_generation: runner.manifest_generation(),
                expected_revision: Some(runner.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let checkpoint = runner
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|revision| revision.revision == outcome.output_revision)
            .and_then(|revision| revision.checkpoint)
            .unwrap();
        store
            .commit_runner_runtime("brain", 1, outcome.output_revision, checkpoint)
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("test:restored-runner".into()),
                source: "14 triple".into(),
                intent: "call frontend definition after daemon restart".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(restored.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(42)]);
    }

    #[tokio::test]
    async fn named_brain_restores_scoped_authority_from_its_separate_policy_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let session_id = runtime.capability_session_id();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::ProcessRun,
                    selector: crate::vm::ResourceSelector::None,
                },
                crate::vm::GrantScope::Session { session_id },
                "test-user",
                None,
            )
            .unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: Some("brain:event:1".into()),
                source: "42".into(),
                intent: "create a durable revision".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();

        let authority_path = temp.path().join("brain/authority.json");
        assert!(authority_path.exists());
        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.capability_session_id(), session_id);
        let ledger = restored.capability_ledger().unwrap();
        assert_eq!(ledger.grants.grants.len(), 1);
        assert_eq!(ledger.grants.grants[0].id, grant_id);
        assert!(matches!(
            ledger.grants.grants[0].scope,
            crate::vm::GrantScope::Session { session_id: restored_id } if restored_id == session_id
        ));
        assert_eq!(ledger.audit.len(), 1);
    }

    #[test]
    fn named_brain_persists_grants_and_revocation_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::ProcessRun,
                    selector: crate::vm::ResourceSelector::None,
                },
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();

        let after_grant = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = after_grant.program_runtime("brain").unwrap();
        assert_eq!(restored.capability_ledger().unwrap().grants.grants[0].id, grant_id);

        runtime.revoke_typed_capability(grant_id).unwrap();
        let after_revoke = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = after_revoke.program_runtime("brain").unwrap();
        let ledger = restored.capability_ledger().unwrap();
        assert!(ledger.grants.grants[0].revoked_at_unix_ms.is_some());
        assert_eq!(ledger.audit.len(), 2);
        assert!(matches!(
            ledger.audit[1].action,
            crate::vm::CapabilityAuditAction::Revoked
        ));
    }

    #[test]
    fn named_brain_persists_policy_changes_and_denials_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let requirement = crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ProcessRun,
            selector: crate::vm::ResourceSelector::None,
        };
        let grant_id = runtime
            .issue_typed_capability(
                requirement.clone(),
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let mut denied = std::collections::BTreeSet::new();
        denied.insert(crate::vm::CapabilityKind::ProcessRun);
        assert_eq!(
            runtime
                .apply_capability_policy(
                    crate::vm::CapabilityPolicy {
                        policy_hash: "locked-policy-v2".into(),
                        denied_capabilities: denied.clone(),
                    },
                    "policy-admin",
                )
                .unwrap(),
            vec![grant_id]
        );

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(
            restored.capability_policy().unwrap(),
            crate::vm::CapabilityPolicy {
                policy_hash: "locked-policy-v2".into(),
                denied_capabilities: denied,
            }
        );
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(restored
            .issue_typed_capability(
                requirement,
                crate::vm::GrantScope::Global,
                "test-user",
                None,
            )
            .unwrap_err()
            .to_string()
            .contains("denied by policy"));
    }

    #[tokio::test]
    async fn named_brain_persists_denial_without_a_vm_commit() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let pending = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: "(file-read (path \"Cargo.toml\"))".into(),
                intent: "test durable denial".into(),
                effect: crate::programs::ExecutionEffect::WorkspaceRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        let denied = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                crate::vm::ApprovalChoice::Deny,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(
            denied.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert!(matches!(
            restored
                .capability_ledger()
                .unwrap()
                .authorization_audit
                .last()
                .map(|entry| &entry.decision),
            Some(crate::vm::AuthorizationDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn named_brain_persists_host_authorization_even_when_the_run_rolls_back() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let grant_id = runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
                ),
                crate::vm::GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let failed = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "s\"Cargo.toml\" path file-read drop 1 0 /".into(),
                intent: "read then fail".into(),
                effect: crate::programs::ExecutionEffect::WorkspaceRead,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(
            failed.status,
            crate::runtime::outcome::ExecutionStatus::Failed
        );
        assert_eq!(runtime.revision(), 0, "failed VM state must roll back");

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let ledger = restarted
            .program_runtime("brain")
            .unwrap()
            .capability_ledger()
            .unwrap();
        assert!(matches!(
            ledger.authorization_audit.last().map(|entry| &entry.decision),
            Some(crate::vm::AuthorizationDecision::Allowed { grant_id: used }) if *used == grant_id
        ));
    }

    #[tokio::test]
    async fn named_brain_checkpoint_without_authority_record_restores_without_grants() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "7".into(),
                intent: "checkpoint without authority".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();
        std::fs::remove_file(temp.path().join("brain/authority.json")).unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), outcome.output_revision);
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
    }

    #[tokio::test]
    async fn named_brain_rejects_a_tampered_authority_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let outcome = runtime
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: "1".into(),
                intent: "persist authority envelope".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: runtime.manifest_generation(),
                expected_revision: Some(runtime.revision()),
                budget: None,
            })
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, outcome.output_revision, &runtime)
            .unwrap();
        let authority_path = temp.path().join("brain/authority.json");
        let mut authority: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&authority_path).unwrap()).unwrap();
        authority["authority"]["project_id"] = serde_json::json!("tampered-project");
        std::fs::write(&authority_path, serde_json::to_vec(&authority).unwrap()).unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let error = restarted
            .program_runtime("brain")
            .err()
            .expect("tampered authority must fail closed");
        assert!(error.to_string().contains("restore authority"));
        assert!(format!("{error:#}").contains("integrity check"));
    }

    #[tokio::test]
    async fn out_of_order_checkpoint_events_never_regress_a_brain_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = store.program_runtime("brain").unwrap();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "concurrent checkpoint ordering".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime.submit_typed_only(submit("1", 0)).await.unwrap();
        let second = runtime.submit_typed_only(submit("2", 1)).await.unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), second.output_revision);
        let values = restored
            .inspect()
            .await
            .unwrap()
            .stack
            .into_iter()
            .map(|cell| cell.value)
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            vec![
                crate::programs::ProgramValue::Int(1),
                crate::programs::ProgramValue::Int(2),
            ]
        );
    }

    #[tokio::test]
    async fn legacy_restart_revision_reset_keeps_the_latest_request_state() {
        let temp = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let runtime = crate::runtime::ProgramRuntime::new();
        let submit = |source: &str, revision| crate::runtime::ProgramSubmission {
            language: crate::programs::ProgramLanguage::Forth,
            source_id: None,
            source: source.into(),
            intent: "legacy revision migration".into(),
            effect: crate::programs::ExecutionEffect::Pure,
            declared_capabilities: Vec::new(),
            manifest_generation: runtime.manifest_generation(),
            expected_revision: Some(revision),
            budget: None,
        };
        let first = runtime
            .submit_typed_only(submit(
                ": square ( S n:int -- S int ! pure ) n n * ;",
                0,
            ))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 1, first.output_revision, &runtime)
            .unwrap();
        let second = runtime
            .submit_typed_only(submit("1 drop", first.output_revision))
            .await
            .unwrap();
        store
            .commit_runtime("brain", 2, second.output_revision, &runtime)
            .unwrap();

        // ProgramRuntime::from_checkpoint historically reset its local
        // revision. Simulate an old daemon adding newer state as revision 1.
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|snapshot| snapshot.checkpoint.clone())
            .unwrap();
        let legacy_restarted = crate::runtime::ProgramRuntime::from_checkpoint(checkpoint).unwrap();
        let legacy_commit = legacy_restarted
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Forth,
                source_id: None,
                source: ": cube ( S n:int -- S int ! pure ) n n * n * ;".into(),
                intent: "new state after legacy restart".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: legacy_restarted.manifest_generation(),
                expected_revision: Some(0),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(legacy_commit.output_revision, 1);
        store
            .commit_runtime(
                "brain",
                3,
                legacy_commit.output_revision,
                &legacy_restarted,
            )
            .unwrap();

        let restarted = SharedBrainStore::with_root("box.local", Some(temp.path().into()));
        let restored = restarted.program_runtime("brain").unwrap();
        assert_eq!(restored.revision(), 3);
        let called = restored
            .submit_typed_only(crate::runtime::ProgramSubmission {
                language: crate::programs::ProgramLanguage::Lisp,
                source_id: None,
                source: "(cube 4)".into(),
                intent: "call latest migrated definition".into(),
                effect: crate::programs::ExecutionEffect::Pure,
                declared_capabilities: Vec::new(),
                manifest_generation: restored.manifest_generation(),
                expected_revision: Some(3),
                budget: None,
            })
            .await
            .unwrap();
        assert_eq!(called.values, vec![crate::programs::ProgramValue::Int(64)]);
        assert_eq!(called.output_revision, 4);
    }

    #[test]
    fn names_cannot_escape_the_storage_root() {
        assert!(SharedBrainStore::validate_name("../other").is_err());
        assert!(SharedBrainStore::validate_name("valid-brain_2").is_ok());
    }

    #[test]
    // An environment is an indivisible authority boundary, not two routable heads.
    fn environment_binds_machine_and_workspace_as_one_revision() {
        let state = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SharedBrainStore::with_environment(
            "gpu-box.local",
            workspace.path(),
            Some(state.path().into()),
        );

        store
            .push(
                "project",
                "laptop.local",
                BrainEventKind::Prompt { text: "go".into() },
            )
            .unwrap();
        let snapshot = store.snapshot("project").unwrap();

        assert_eq!(snapshot.environment.machine, "gpu-box.local");
        assert_eq!(
            snapshot.environment.workspace,
            workspace.path().canonicalize().unwrap()
        );
        assert_eq!(snapshot.environment.generation, 1);
        assert_eq!(snapshot.events[0].environment_generation, 1);
    }

    #[test]
    fn old_events_default_to_the_initial_environment_generation() {
        let event: BrainEvent = serde_json::from_str(
            r#"{"seq":1,"sender":"alice","created_ms":0,"kind":"prompt","text":"hi"}"#,
        )
        .unwrap();
        assert_eq!(event.environment_generation, 1);
    }
}
