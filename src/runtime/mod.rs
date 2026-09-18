//! Provider-neutral execution service for Finch's Forth and Lisp VMs.

mod abi;
mod agent_vm;
mod agents;
mod archive_store;
mod automation;
mod context;
mod effect_audit;
mod effect_log;
mod host;
mod hostio;
mod mcp;
mod outcome;

use host::*;

pub use abi::{
    DeliveryConsumerIdentity, DeliveryCursor, OutputHandleRef, ProgramRun,
    RuntimeApplicationMessage,
};
pub use agent_vm::{parse_task_id, AgentVmBinding};
pub use agents::{
    AgentActivitySnapshot, AgentBudget, AgentContextReference, AgentEvent, AgentIdentity,
    AgentRole, AgentSpawning, AgentTaskResult, AgentTaskSnapshot, AgentTaskSpec, AgentTaskStatus,
    AgentUsage, AgentUsageState, NoAgentSpawning,
};
pub(crate) use agents::{
    MAX_CONTEXT_ARTIFACT_BYTES, MAX_CONTEXT_FIELD_BYTES, MAX_CONTEXT_REFERENCES,
    MAX_CONTEXT_TOTAL_BYTES, MAX_DEPTH, MAX_OUTPUT_BYTES, MAX_TIMEOUT_MS, MAX_TURNS,
};
pub use archive_store::{ProgramRuntimeArchiveStore, ProgramRuntimeAuthorityStore};
pub use automation::{
    permission_context_key, permission_target_description, AutomationAvailability,
    AutomationBroker, AutomationPermissionResult, AutomationPromptContext,
    AutomationPromptDisposition, AutomationRequest, AutomationState,
};
pub use context::{ExecutionBudget, ExecutionContext};
pub use effect_audit::{
    RunnerEffectAuditControl, RunnerEffectAuditReservation, RunnerHostEffectOutcome,
    RunnerHostEffectPermit,
};
pub(crate) use effect_audit::{
    RunnerEffectAuditControlRequest, RunnerEffectAuditReservationRequest,
    RunnerHostEffectFinishRequest,
};
pub(crate) use effect_log::replay_fence_transition;
pub use effect_log::{
    bind_delivery_log, EffectAuditAuthority, EffectAuditEntry, EffectAuditIdentity,
    EffectAuditIntent, EffectAuditReducer, EffectAuditState, EffectAuditTerminalOutcome,
    EffectAuditTransition, HostEffectPermit, VmEffectDeliveryLog,
    EFFECT_AUDIT_REPLAY_INDEX_BUDGET_BYTES, MAX_ACTIVE_EFFECT_AUDITS_PER_BRAIN,
    MAX_ACTIVE_EFFECT_AUDITS_PER_RUN, MAX_ACTIVE_EFFECT_AUDIT_BYTES_PER_BRAIN,
    MAX_EFFECT_AUDIT_INTENT_BYTES, MAX_EFFECT_AUDIT_JOURNAL_BYTES_PER_BRAIN,
    MAX_EFFECT_AUDIT_OUTCOME_BYTES, MAX_EFFECT_AUDIT_REPLAY_FENCES_PER_BRAIN,
    MAX_EFFECT_AUDIT_REPLAY_FENCE_EVENT_BYTES, MAX_EFFECT_AUDIT_REPLAY_FENCE_TRANSITION_BYTES,
};
pub use outcome::{ExecutionBackend, ExecutionOutcome, ExecutionStatus};

use crate::programs::{ExecutionEffect, ProgramCompilerContext, ProgramLanguage, ProgramValue};
pub(crate) use hostio::workbook_cell_to_string;
use hostio::{
    hex_digest, list_directory_tree, merkle_directory, read_bounded_csv_record,
    read_bounded_utf8_line, read_workbook_range, read_workbook_rows, read_workbook_sheet_names,
    sha256_file_handle, summarize_csv, summarize_workbook, typed_mcp_arguments,
};

use crate::vm::{
    agent_task_result_type, agent_task_snapshot_type, agent_task_spec_type,
    capability_grant_entry_type, core_word_spec, tree_entry_type, tree_listing_type,
    CoreHostBinding, CoreWordImplementation,
};
use crate::vm::{
    ApprovalChoice, ApprovalPrompt, AuthorizationContext, AuthorizationDecision,
    CapabilityAvailability, CapabilityKind, CapabilityLedger, CapabilityPolicy, CapabilityRequest,
    CapabilityRequirement, EffectSet, GrantScope, ResourceSelector, SourceOrigin, Type,
    TypedExecutionStatus, TypedRuntime, TypedRuntimeCheckpoint, TypedSuspension, TypedValue,
    VmDiagnostic, VmSideEffect,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{mpsc, Mutex, RwLock, Weak};
use std::time::Instant;

const LOCAL_CAPABILITY_POLICY_HASH: &str = "finch-local-runtime-v1";

fn default_capability_policy() -> CapabilityPolicy {
    CapabilityPolicy {
        policy_hash: LOCAL_CAPABILITY_POLICY_HASH.into(),
        denied_capabilities: Default::default(),
    }
}

/// A portable VM event attached to its owning ProgramRun. Defined in the
/// dependency-free `finch-tools-api` crate (the tool API's live-output sink
/// receives it) and re-exported here, so `crate::runtime::VmEffectEnvelope`
/// is the same type it always was. The runtime-coupled methods remain on
/// [`VmEffectEnvelopeRuntimeMethods`].
pub use finch_tools_api::{VmEffectEnvelope, VmEffectHandle};

/// Runtime-coupled methods over the re-exported [`VmEffectEnvelope`]. They
/// name `ProgramRun` and the runtime ABI, so they cannot live in the API
/// crate with the type itself.
pub(crate) trait VmEffectEnvelopeRuntimeMethods {
    /// ProgramRun identity carried by this envelope.
    fn program_run(&self) -> ProgramRun;

    /// Concurrent output handle targeted by this event, if any.
    fn output_handle(&self) -> Option<OutputHandleRef>;
}

impl VmEffectEnvelopeRuntimeMethods for VmEffectEnvelope {
    fn program_run(&self) -> ProgramRun {
        ProgramRun::new(self.execution_id)
    }

    fn output_handle(&self) -> Option<OutputHandleRef> {
        abi::output_handle_ref(self.execution_id, &self.effect)
    }
}

/// The portable, correlated reply to one awaited VM effect. An embedder keeps
/// the `(execution_id, sequence)` pair from [`VmEffectEnvelope`] and sends
/// exactly one of these records when its host-side operation finishes, is
/// rejected, or is cancelled. The typed runtime verifies the saved output row
/// before accepting a result and never redispatches the original effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmResume {
    pub execution_id: uuid::Uuid,
    pub sequence: u64,
    pub response: VmResumeResponse,
}

/// Host outcome carried by [`VmResume`]. This is intentionally a typed value
/// transport rather than a stringly status protocol: the verifier knows the
/// output row expected at the suspended capability boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VmResumeResponse {
    Result { values: Vec<TypedValue> },
    Denied { reason: String },
    Cancelled { reason: Option<String> },
}

/// Host-specific projection of one portable typed VM event. The projection is
/// bound to one ProgramRun, never stored as a process- or Brain-global
/// "active work unit".
pub type TypedEffectSink = Arc<dyn Fn(VmEffectEnvelope) + Send + Sync>;

/// Selects which awaited host calls leave the VM suspended for an external
/// embedder. Emitted presentation effects such as `say` are deliberately not
/// included: they have no host result row and therefore continue immediately.
///
/// `ProgramInvocations` preserves Finch's existing editor-proposal behavior.
/// `AllAwaited` is the portable Runtime/Application boundary: an IDE, web
/// host, or daemon can handle every approved host request and return a
/// correlated [`VmResume`] without the VM knowing the host implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferredHostEffects {
    None,
    ProgramInvocations,
    Schedules,
    AllAwaited,
}

impl DeferredHostEffects {
    fn defers(self, effect: &VmSideEffect) -> bool {
        match self {
            Self::None => false,
            Self::ProgramInvocations => {
                effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
            }
            Self::Schedules => matches!(
                effect.requirement.capability,
                crate::vm::CapabilityKind::ScheduleCreate
                    | crate::vm::CapabilityKind::ScheduleRead
                    | crate::vm::CapabilityKind::ScheduleManage
            ),
            Self::AllAwaited => true,
        }
    }
}

/// Create a thread-safe, single-consumer adapter for portable VM effects.
///
/// A `ProgramRuntime` may execute VM instructions on Tokio's blocking pool;
/// the returned sink is therefore safe to install directly on a run while the
/// receiver is owned by an application event loop, IDE bridge, or test
/// harness. Sender order is preserved for one ProgramRun. The runtime still
/// keeps its own effect journal: this channel is a live projection mechanism,
/// not a durable event store or an acknowledgement protocol.
pub fn typed_effect_channel() -> (TypedEffectSink, mpsc::Receiver<VmEffectEnvelope>) {
    let (sender, receiver) = mpsc::channel();
    let sink: TypedEffectSink = Arc::new(move |envelope| {
        // A disconnected UI must not fail or alter a verified VM execution.
        // Its full effect journal remains available in the eventual outcome.
        let _ = sender.send(envelope);
    });
    (sink, receiver)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramSubmission {
    pub language: ProgramLanguage,
    /// Stable identity of the authored source used in diagnostics and the
    /// effect journal. A provider response, file path, scheduler record, or
    /// IDE buffer may supply its own identity; older callers use the
    /// language-specific fallback in the runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    pub source: String,
    pub intent: String,
    pub effect: ExecutionEffect,
    #[serde(default)]
    pub declared_capabilities: Vec<CapabilityRequirement>,
    pub manifest_generation: u64,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub budget: Option<ExecutionBudget>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmStackCell {
    pub index_from_bottom: usize,
    pub type_name: String,
    pub value: ProgramValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmVocabularyEntry {
    pub name: String,
    pub signature: Option<String>,
    /// Source-level documentation for a persisted typed definition, when the
    /// word is not a host-bound core primitive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    /// Immutable application-binding identity, such as an MCP schema hash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HostVocabularyMetadata {
    documentation: String,
    version: String,
    output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedVmStackCell {
    pub index_from_bottom: usize,
    pub value_type: Type,
    pub value: TypedValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmStateSnapshot {
    pub manifest_generation: u64,
    pub revision: u64,
    pub stack: Vec<VmStackCell>,
    pub vocabulary: Vec<VmVocabularyEntry>,
    #[serde(default)]
    pub typed_stack: Vec<TypedVmStackCell>,
    #[serde(default)]
    pub typed_vocabulary: Vec<VmVocabularyEntry>,
    #[serde(default)]
    pub granted_capabilities: Vec<CapabilityRequirement>,
}

/// An immutable in-memory checkpoint at a successful VM commit boundary.
/// External effects stay in their per-run journals; this contains only
/// reducible typed VM state for inspection and later durable persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmRevisionSnapshot {
    pub revision: u64,
    pub stack: Vec<TypedValue>,
    pub vocabulary: Vec<String>,
    /// A serializable, restorable checkpoint when the stack contains no
    /// application-owned handles. The ordinary live revision remains valid
    /// even when this is absent; the host must persist/rebind those handles
    /// before a future restart can recover the revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<TypedRuntimeCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_diagnostic: Option<String>,
}

pub const PROGRAM_RUNTIME_ARCHIVE_VERSION: u32 = 1;
pub const PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION: u32 = 2;
/// Full reducible checkpoints are intentionally expensive. Retain a bounded
/// recent lineage in memory and in the ordinary archive; an application that
/// needs older history owns it in its durable event/checkpoint store.
pub const MAX_RETAINED_VM_REVISIONS: usize = 256;
/// Hard per-runtime bound for private transactional continuations. Existing
/// approvals/yields are never silently evicted: once full, the newly
/// suspending run is cancelled with a structured outcome so the application
/// can journal and present that terminal state.
pub const MAX_PENDING_TYPED_EXECUTIONS: usize = 256;

/// Application-owned persistence hook for host authority. The callback is
/// supplied a complete immutable snapshot and must not call back into the
/// originating `ProgramRuntime`; named Brain storage uses it for one atomic
/// replace of the separate authority record.
pub type ProgramRuntimeAuthoritySink =
    Arc<dyn Fn(ProgramRuntimeAuthorityState) -> Result<()> + Send + Sync>;

/// Versioned bounded window of reducible state for a persistent shared VM.
/// Authority, live host handles, pending external calls, execute-once effect
/// records, and older application-owned history are intentionally absent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramRuntimeArchive {
    pub format_version: u32,
    /// First revision retained in this bounded checkpoint window. Archives
    /// written before this field existed omit it and deserialize as zero;
    /// their first snapshot remains authoritative for compatibility.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub base_revision: u64,
    pub current_revision: u64,
    pub revisions: Vec<VmRevisionSnapshot>,
}

fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

/// Host-owned authority state persisted beside, never inside, the reducible
/// VM archive. Restoring this record is an explicit application policy step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgramRuntimeAuthorityState {
    pub format_version: u32,
    pub session_id: uuid::Uuid,
    pub project_id: String,
    #[serde(default = "default_capability_policy")]
    pub policy: CapabilityPolicy,
    pub ledger: CapabilityLedger,
    #[serde(default)]
    pub resource_roots: Vec<ResourceRootBindingRecord>,
    #[serde(default)]
    pub resource_root_audit: Vec<ResourceRootAuditEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRootBindingRecord {
    pub root: crate::vm::ResourceRoot,
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
    pub generation: u64,
    pub whole_machine: bool,
    pub bound_at_unix_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceRootAuditAction {
    Bound,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRootAuditEntry {
    pub sequence: u64,
    pub root: crate::vm::ResourceRoot,
    pub generation: u64,
    pub path: PathBuf,
    pub whole_machine: bool,
    pub action: ResourceRootAuditAction,
    pub at_unix_ms: u64,
    pub actor: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResourceRootState {
    bindings: BTreeMap<crate::vm::ResourceRoot, Arc<ResourceRootBindingRecord>>,
    audit: Vec<ResourceRootAuditEntry>,
}

/// One session's persistent language runtimes.
pub struct ProgramRuntime {
    typed: Arc<Mutex<TypedRuntime>>,
    revision: Arc<AtomicU64>,
    manifest_generation: AtomicU64,
    submission_gate: tokio::sync::Mutex<()>,
    automation: Arc<AutomationBroker>,
    /// Application-owned filesystem roots. A root binding only establishes
    /// where a refined path resolves; capability grants remain mandatory for
    /// every operation beneath it.
    resource_roots: Arc<RwLock<ResourceRootState>>,
    /// Identity used to evaluate reusable capability scopes. It is host-owned
    /// policy state, not part of reducible VM checkpoints.
    session_id: uuid::Uuid,
    project_id: String,
    memory: RwLock<Option<Arc<crate::memory::MemorySystem>>>,
    /// Host-owned MCP transport. Installing it makes configured servers
    /// callable but never grants authority to any server or tool.
    mcp_client: RwLock<Option<Arc<crate::tools::McpClient>>>,
    host_vocabulary: RwLock<BTreeMap<String, HostVocabularyMetadata>>,
    network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
    /// Output handles are opaque, per-execution presentation resources.  They
    /// intentionally do not name an ambient "current work unit".
    output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
    /// Opaque, per-ProgramRun streams. The host retains their backing state
    /// and source code never gets a raw path or producer capability back after
    /// opening one. New stream backends belong in `HostStreamBackend`, not in
    /// a parallel registry with a second ownership lifecycle.
    streams: Arc<Mutex<HashMap<String, HostStream>>>,
    /// Host authority is application state, separate from reducible VM
    /// checkpoints. Stable grant IDs and audit events can be persisted beside
    /// a Brain/session log, while only the active requirements enter a run.
    capability_ledger: Arc<Mutex<CapabilityLedger>>,
    capability_policy: Arc<RwLock<CapabilityPolicy>>,
    /// Shared/exclusive authority-use gate. Host operations retain a shared
    /// lease through dispatch; revoke, policy, and resource-root mutations
    /// take the exclusive side before they can return to their caller.
    authority_use_gate: Arc<RwLock<()>>,
    authority_sink: Arc<RwLock<Option<ProgramRuntimeAuthoritySink>>>,
    // A capability handed in from above, never a scheduler this module names: running a child
    // agent means choosing a provider and a model, which is the host's job.
    agent_scheduler: RwLock<Weak<dyn agents::AgentSpawning>>,
    /// Daemon-owned typed continuations keyed by the execution id visible in
    /// the UI. Approval and resumption use this exact verified program state.
    pending_typed: Mutex<HashMap<uuid::Uuid, PendingTypedExecution>>,
    revision_history: Mutex<Vec<VmRevisionSnapshot>>,
    /// Optional Brain-owned delivery log. When bound, every ProgramRun
    /// observation sink persists before projecting to the caller.
    delivery_log: Arc<RwLock<Option<Arc<Mutex<VmEffectDeliveryLog>>>>>,
}

/// Host-owned socket metadata. Finch source sees only the opaque resource
/// handle; the host retains the endpoint so each later send can revalidate
/// the grant which originally authorized that connection.
struct NetworkSocket {
    stream: TcpStream,
    host: String,
    port: u16,
    owner: uuid::Uuid,
    generation: u64,
}

/// Host-side ownership record for an output handle. The VM only sees the
/// corresponding opaque `resource<output-handle>` value.
#[derive(Debug, Clone, Copy)]
struct OutputHandleRecord {
    owner: uuid::Uuid,
    generation: u64,
}

struct HostStream {
    owner: uuid::Uuid,
    generation: u64,
    /// Exact authority which minted this cursor. Follow-up operations replace
    /// their static unscoped selector with this requirement before the live
    /// ledger is consulted, so an unrelated file grant cannot keep a cursor
    /// usable after revocation.
    requirement: CapabilityRequirement,
    backend: HostStreamBackend,
}

/// Private implementations of host-issued `stream<T>` values.  The public
/// type and capability contract live in the typed VM; this enum is merely the
/// host adapter's resource table.  A future workbook or producer stream gets
/// the exact same ownership, close, and ProgramRun-release behavior.
enum HostStreamBackend {
    FileLines(BufReader<std::fs::File>),
    CsvRecords(BufReader<std::fs::File>),
    WorkbookRows(std::vec::IntoIter<Vec<String>>),
}

#[derive(Clone)]
struct PendingTypedExecution {
    /// Private transactional VM state captured at the run's input revision.
    /// It is never installed into the shared runtime until this exact run
    /// completes and wins the revision commit.
    working_runtime: TypedRuntime,
    suspension: TypedSuspension,
    context: ExecutionContext,
    input_revision: u64,
    language: ProgramLanguage,
    source: String,
    intent: String,
    effect: ExecutionEffect,
    caller: Option<agents::AgentIdentity>,
    output: String,
    output_chunks: Vec<String>,
    side_effects: Vec<crate::vm::HostSideEffect>,
    effect_sink: Option<TypedEffectSink>,
    effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    deferred_host_effects: DeferredHostEffects,
    /// An execution-specific authority ceiling, used by durable scheduled
    /// callbacks. Ordinary interactive runs intentionally pick up newly
    /// granted authority while they wait for approval; scheduled work must
    /// never gain authority merely because time passed.
    grant_ceiling: Option<EffectSet>,
}

/// UI-safe metadata for a daemon-owned typed continuation. The full frame is
/// deliberately not exposed through ordinary client state inspection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingTypedExecutionInfo {
    pub execution_id: uuid::Uuid,
    pub input_revision: u64,
    pub manifest_generation: u64,
    /// Required by the sequence-checked resume API when the run is awaiting
    /// a concrete host capability result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_effect_sequence: Option<u64>,
    /// Public payload of a typed `yield`. A unit timeslice appears as
    /// `ProgramValue::Nil`; host-effect and task-join suspensions leave this
    /// absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yielded_value: Option<ProgramValue>,
    /// Yield type is always inspectable even when the local payload (for
    /// example a closure) has no portable `ProgramValue` representation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yielded_type: Option<Type>,
    pub reason: PendingTypedReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingTypedReason {
    Yielded,
    /// An approved host operation was emitted to an external event loop and
    /// awaits its correlated typed result (for example an editor proposal).
    AwaitingHostEffect {
        requirement: CapabilityRequirement,
    },
    AuthorizationRequired {
        requirements: Vec<CapabilityRequirement>,
    },
}

impl ProgramRuntime {
    pub fn new() -> Self {
        Self::with_automation(false)
    }

    /// Construct a fresh shared program runtime from reducible typed VM state.
    /// The restored instance begins a new local revision lineage at zero: a
    /// daemon/Brain event log is responsible for assigning durable revision
    /// identity and restoring host-owned resources or approvals around it.
    pub fn from_checkpoint(checkpoint: TypedRuntimeCheckpoint) -> Result<Self> {
        Self::from_checkpoint_at_revision(checkpoint, 0)
    }

    /// Restore reducible state at a durable application-owned revision.
    ///
    /// The checkpoint deliberately contains no Brain identity or authority,
    /// so an application that persists those separately must supply the exact
    /// committed revision from its event journal. This keeps optimistic
    /// concurrency monotonic across process restart without putting host
    /// policy into the embedder-neutral VM checkpoint.
    pub fn from_checkpoint_at_revision(
        checkpoint: TypedRuntimeCheckpoint,
        revision: u64,
    ) -> Result<Self> {
        let typed_runtime =
            TypedRuntime::from_checkpoint(checkpoint.clone()).map_err(|diagnostics| {
                anyhow::anyhow!(
                    "cannot restore typed runtime checkpoint: {}",
                    diagnostics
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })?;
        let runtime = Self::new();
        let vocabulary = typed_runtime.vocabulary().keys().cloned().collect();
        let stack = typed_runtime.stack().to_vec();
        *runtime
            .typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))? = typed_runtime;
        *runtime
            .revision_history
            .lock()
            .map_err(|_| anyhow::anyhow!("revision history lock poisoned"))? =
            vec![VmRevisionSnapshot {
                revision,
                stack,
                vocabulary,
                checkpoint: Some(checkpoint),
                checkpoint_diagnostic: None,
            }];
        runtime.revision.store(revision, Ordering::Release);
        Ok(runtime)
    }

    /// Restore a retained reducible revision window. The current revision must
    /// carry a checkpoint; historical entries may retain only metadata when
    /// their application-owned handles were not serializable. Older revisions
    /// may live in the application event/checkpoint store before `base_revision`.
    pub fn from_archive(archive: ProgramRuntimeArchive) -> Result<Self> {
        if archive.format_version != PROGRAM_RUNTIME_ARCHIVE_VERSION {
            bail!(
                "unsupported ProgramRuntime archive version {}; expected {}",
                archive.format_version,
                PROGRAM_RUNTIME_ARCHIVE_VERSION
            );
        }
        if archive.revisions.is_empty() {
            bail!("ProgramRuntime archive has no revisions");
        }
        if archive
            .revisions
            .windows(2)
            .any(|window| window[0].revision >= window[1].revision)
        {
            bail!("ProgramRuntime archive revisions are not strictly increasing");
        }
        let first_revision = archive
            .revisions
            .first()
            .expect("a non-empty archive has a first revision")
            .revision;
        if archive.base_revision != 0 && archive.base_revision != first_revision {
            bail!(
                "ProgramRuntime archive begins at revision {first_revision}, not declared base revision {}",
                archive.base_revision
            );
        }
        let current = archive
            .revisions
            .last()
            .expect("a non-empty archive has a final revision");
        if current.revision != archive.current_revision {
            bail!(
                "ProgramRuntime archive ends at revision {}, not declared revision {}",
                current.revision,
                archive.current_revision
            );
        }
        let checkpoint = current.checkpoint.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "ProgramRuntime archive revision {} has no restorable checkpoint: {}",
                current.revision,
                current
                    .checkpoint_diagnostic
                    .as_deref()
                    .unwrap_or("host-owned state was not serialized")
            )
        })?;
        let runtime = Self::from_checkpoint_at_revision(checkpoint, archive.current_revision)?;
        *runtime
            .revision_history
            .lock()
            .map_err(|_| anyhow::anyhow!("revision history lock poisoned"))? = archive.revisions;
        Ok(runtime)
    }

    /// Restore reducible VM state and host authority as two independently
    /// validated records. The application chooses whether to supply the
    /// authority record; loading a VM archive alone remains authority-free.
    pub fn from_archive_with_authority(
        archive: ProgramRuntimeArchive,
        authority: ProgramRuntimeAuthorityState,
    ) -> Result<Self> {
        let mut runtime = Self::from_archive(archive)?;
        runtime.restore_authority_state(authority)?;
        Ok(runtime)
    }

    pub fn with_automation(enabled: bool) -> Self {
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::with_automation_in_workspace(enabled, workspace_root)
    }

    pub(crate) fn with_automation_in_workspace(enabled: bool, workspace_root: PathBuf) -> Self {
        let automation = Arc::new(AutomationBroker::new(enabled));
        let typed_runtime = TypedRuntime::new();
        let checkpoint = typed_runtime
            .checkpoint()
            .expect("a fresh typed runtime is checkpointable");
        let project_id = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.clone())
            .to_string_lossy()
            .into_owned();
        let workspace_binding = resource_root_binding_record(
            crate::vm::ResourceRoot::Workspace,
            &workspace_root,
            1,
            false,
            unix_time_ms(),
        )
        .expect("the current workspace is a stable directory");
        let resource_roots = ResourceRootState {
            bindings: BTreeMap::from([(
                crate::vm::ResourceRoot::Workspace,
                Arc::new(workspace_binding.clone()),
            )]),
            audit: vec![ResourceRootAuditEntry {
                sequence: 1,
                root: workspace_binding.root.clone(),
                generation: workspace_binding.generation,
                path: workspace_binding.path.clone(),
                whole_machine: false,
                action: ResourceRootAuditAction::Bound,
                at_unix_ms: workspace_binding.bound_at_unix_ms,
                actor: "runtime-bootstrap".into(),
            }],
        };
        Self {
            typed: Arc::new(Mutex::new(typed_runtime)),
            revision: Arc::new(AtomicU64::new(0)),
            manifest_generation: AtomicU64::new(1),
            submission_gate: tokio::sync::Mutex::new(()),
            automation,
            resource_roots: Arc::new(RwLock::new(resource_roots)),
            session_id: uuid::Uuid::new_v4(),
            project_id,
            memory: RwLock::new(None),
            mcp_client: RwLock::new(None),
            host_vocabulary: RwLock::new(BTreeMap::new()),
            network: Arc::new(Mutex::new(HashMap::new())),
            output_handles: Arc::new(Mutex::new(HashMap::new())),
            streams: Arc::new(Mutex::new(HashMap::new())),
            capability_ledger: Arc::new(Mutex::new(CapabilityLedger::default())),
            capability_policy: Arc::new(RwLock::new(default_capability_policy())),
            authority_use_gate: Arc::new(RwLock::new(())),
            authority_sink: Arc::new(RwLock::new(None)),
            agent_scheduler: RwLock::new(Weak::<agents::NoAgentSpawning>::new()),
            pending_typed: Mutex::new(HashMap::new()),
            revision_history: Mutex::new(vec![VmRevisionSnapshot {
                revision: 0,
                stack: Vec::new(),
                vocabulary: crate::vm::core_vocabulary().into_keys().collect(),
                checkpoint: Some(checkpoint),
                checkpoint_diagnostic: None,
            }]),
            delivery_log: Arc::new(RwLock::new(None)),
        }
    }

    /// Install the application-owned effect delivery log. Subsequent
    /// ProgramRun observation sinks persist each envelope before projecting
    /// to the caller. Exact replay is not re-projected; conflict panics so a
    /// persist failure cannot leave a suspended run unobserved.
    pub fn bind_effect_delivery_log(&self, log: Arc<Mutex<VmEffectDeliveryLog>>) -> Result<()> {
        *self
            .delivery_log
            .write()
            .map_err(|_| anyhow::anyhow!("VM effect delivery log lock poisoned"))? = Some(log);
        Ok(())
    }

    /// The bound delivery log, if this runtime is a production Brain instance.
    pub fn effect_delivery_log(&self) -> Option<Arc<Mutex<VmEffectDeliveryLog>>> {
        self.delivery_log
            .read()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn compose_typed_effect_sink(
        &self,
        downstream: Option<TypedEffectSink>,
    ) -> Option<TypedEffectSink> {
        let log = match self.delivery_log.read() {
            Ok(guard) => guard.clone(),
            Err(_) => panic!("VM effect delivery log lock poisoned"),
        };
        match log {
            Some(log) => Some(bind_delivery_log(log, downstream)),
            None => downstream,
        }
    }

    pub fn automation(&self) -> Arc<AutomationBroker> {
        Arc::clone(&self.automation)
    }

    /// Report whether the application has an implementation for a capability
    /// independently of whether this ProgramRun currently has a grant. This
    /// is selector-aware for authority-bearing roots and never prompts.
    pub fn capability_availability(
        &self,
        requirement: &CapabilityRequirement,
    ) -> CapabilityAvailability {
        use crate::runtime::automation::AutomationState;
        use crate::vm::{ResourceRoot, ResourceSelector};

        let root_availability = |root: &ResourceRoot| match self.resource_roots.read() {
            Ok(roots) if roots.bindings.contains_key(root) => CapabilityAvailability::Available,
            Ok(_) if matches!(root, ResourceRoot::Named(_)) => CapabilityAvailability::Unsupported,
            Ok(_) => CapabilityAvailability::Disabled,
            Err(_) => CapabilityAvailability::Degraded {
                reason: "resource-root binding lock poisoned".into(),
            },
        };
        match requirement.capability {
            CapabilityKind::SessionEmit | CapabilityKind::VmRead => {
                CapabilityAvailability::Available
            }
            CapabilityKind::FileRead | CapabilityKind::FileWrite => match &requirement.selector {
                ResourceSelector::File { selector } => root_availability(&selector.root),
                ResourceSelector::FileTemplate { template } => root_availability(&template.root),
                _ => CapabilityAvailability::Unsupported,
            },
            CapabilityKind::AutomationInspect | CapabilityKind::AutomationWrite => {
                match self.automation.availability().state {
                    AutomationState::Disabled => CapabilityAvailability::Disabled,
                    AutomationState::Unsupported => CapabilityAvailability::Unsupported,
                    AutomationState::PermissionRequired => {
                        CapabilityAvailability::PermissionRequired
                    }
                    AutomationState::Available => CapabilityAvailability::Available,
                }
            }
            CapabilityKind::AgentSpawn
            | CapabilityKind::AgentAwait
            | CapabilityKind::AgentPoll
            | CapabilityKind::AgentCancel => match self.agent_scheduler.read() {
                Ok(scheduler) if scheduler.upgrade().is_some() => CapabilityAvailability::Available,
                Ok(_) => CapabilityAvailability::Disabled,
                Err(_) => CapabilityAvailability::Degraded {
                    reason: "agent scheduler binding lock poisoned".into(),
                },
            },
            CapabilityKind::MemoryRead | CapabilityKind::MemoryWrite => match self.memory.read() {
                Ok(memory) if memory.is_some() => CapabilityAvailability::Available,
                Ok(_) => CapabilityAvailability::Disabled,
                Err(_) => CapabilityAvailability::Degraded {
                    reason: "memory binding lock poisoned".into(),
                },
            },
            // Scheduling is owned by the daemon/Brain host. A bare runtime
            // has no second local queue; portable and named-Brain adapters
            // explicitly defer these effects to their owning host.
            CapabilityKind::ScheduleCreate
            | CapabilityKind::ScheduleRead
            | CapabilityKind::ScheduleManage => CapabilityAvailability::Disabled,
            CapabilityKind::NetworkConnect | CapabilityKind::ProgramInvoke => {
                CapabilityAvailability::Available
            }
            CapabilityKind::ProcessRun => {
                #[cfg(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd",
                    target_os = "dragonfly"
                ))]
                {
                    CapabilityAvailability::Available
                }
                #[cfg(not(any(
                    target_os = "linux",
                    target_os = "android",
                    target_os = "freebsd",
                    target_os = "dragonfly"
                )))]
                {
                    CapabilityAvailability::Unsupported
                }
            }
            CapabilityKind::McpCall => match self.mcp_client.read() {
                Ok(client) if client.is_some() => CapabilityAvailability::Available,
                Ok(_) => CapabilityAvailability::Disabled,
                Err(_) => CapabilityAvailability::Degraded {
                    reason: "MCP client binding lock poisoned".into(),
                },
            },
            CapabilityKind::VmWrite
            | CapabilityKind::MemoryConsolidate
            | CapabilityKind::UnsafeMemory => CapabilityAvailability::Unsupported,
        }
    }

    /// Install the application-owned MCP transport and atomically replace its
    /// discovered, validated namespaced vocabulary. This changes availability
    /// and the manifest generation, never capability grants.
    pub async fn bind_mcp_client(
        &self,
        client: Arc<crate::tools::McpClient>,
    ) -> Result<Vec<String>> {
        let mut rejected = Vec::new();
        let mut signatures = BTreeMap::new();
        let mut metadata = BTreeMap::new();
        for descriptor in client.tool_descriptors().await {
            match mcp::adapt_mcp_descriptor(&descriptor) {
                Ok(binding) => {
                    signatures.insert(binding.word_name.clone(), binding.signature);
                    metadata.insert(
                        binding.word_name,
                        HostVocabularyMetadata {
                            documentation: binding.documentation,
                            version: binding.version,
                            output_schema: binding.output_schema,
                        },
                    );
                }
                Err(error) => rejected.push(format!(
                    "{}.{}: {error:#}",
                    descriptor.server, descriptor.tool
                )),
            }
        }
        let previous_names = self
            .host_vocabulary
            .read()
            .map_err(|_| anyhow::anyhow!("host vocabulary lock poisoned"))?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        self.typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
            .replace_host_vocabulary(previous_names, &signatures)
            .map_err(|diagnostic| anyhow::anyhow!(diagnostic.to_string()))?;
        *self
            .host_vocabulary
            .write()
            .map_err(|_| anyhow::anyhow!("host vocabulary lock poisoned"))? = metadata;
        *self
            .mcp_client
            .write()
            .map_err(|_| anyhow::anyhow!("MCP client binding lock poisoned"))? = Some(client);
        self.manifest_generation.fetch_add(1, Ordering::AcqRel);
        Ok(rejected)
    }

    /// Whether this runtime already has an application-owned MCP transport.
    /// This exposes availability only; it does not reveal transport state or
    /// imply that any MCP capability has been granted.
    pub fn has_mcp_client(&self) -> bool {
        self.mcp_client
            .read()
            .map(|client| client.is_some())
            .unwrap_or(false)
    }

    /// Install the host-owned root behind `root<host-machine>`. This is an
    /// availability binding, deliberately separate from capability grants;
    /// callers must still grant a matching `file.read` or `file.write`
    /// selector before a typed program can use it. Whole-machine scope uses
    /// the deliberately named `bind_whole_machine_root` API.
    pub fn bind_host_machine_root(&self, root: impl Into<PathBuf>) -> Result<()> {
        self.bind_resource_root(
            crate::vm::ResourceRoot::HostMachine,
            root,
            false,
            "local-user",
        )
    }

    /// Deliberately expose the filesystem root as `root<host-machine>`. The
    /// distinct API makes whole-machine availability visible in durable audit
    /// instead of inferring it from an ordinary path binding.
    pub fn bind_whole_machine_root(&self) -> Result<()> {
        self.bind_resource_root(
            crate::vm::ResourceRoot::HostMachine,
            PathBuf::from("/"),
            true,
            "local-user-whole-machine",
        )
    }

    /// Install an application-selected project root. This is distinct from
    /// the current workspace so a host can expose a narrower or broader
    /// project tree without changing process current-directory semantics.
    pub fn bind_project_root(&self, root: impl Into<PathBuf>) -> Result<()> {
        self.bind_resource_root(crate::vm::ResourceRoot::Project, root, false, "local-user")
    }

    /// Install the output directory assigned to this task/session. Programs
    /// can receive write authority here without receiving workspace writes.
    pub fn bind_task_output_root(&self, root: impl Into<PathBuf>) -> Result<()> {
        self.bind_resource_root(
            crate::vm::ResourceRoot::TaskOutput,
            root,
            false,
            "local-user",
        )
    }

    fn bind_resource_root(
        &self,
        kind: crate::vm::ResourceRoot,
        root: impl Into<PathBuf>,
        whole_machine: bool,
        actor: &str,
    ) -> Result<()> {
        let root = root.into();
        if root == Path::new("/") && !whole_machine {
            bail!("binding '/' requires bind_whole_machine_root");
        }
        if whole_machine && (kind != crate::vm::ResourceRoot::HostMachine || root != Path::new("/"))
        {
            bail!("whole-machine binding must be host-machine root '/'");
        }
        let _authority_use = self
            .authority_use_gate
            .write()
            .map_err(|_| anyhow::anyhow!("authority-use gate poisoned"))?;
        let policy = self
            .capability_policy
            .read()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let mut roots = self
            .resource_roots
            .write()
            .map_err(|_| anyhow::anyhow!("resource-root binding lock poisoned"))?;
        let previous = roots.clone();
        let generation = roots
            .audit
            .iter()
            .map(|entry| entry.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("resource-root generation overflow"))?;
        let now = unix_time_ms();
        let binding =
            resource_root_binding_record(kind.clone(), &root, generation, whole_machine, now)?;
        if let Some(replaced) = roots.bindings.remove(&kind) {
            let sequence = roots.audit.len() as u64 + 1;
            roots.audit.push(ResourceRootAuditEntry {
                sequence,
                root: kind.clone(),
                generation: replaced.generation,
                path: replaced.path.clone(),
                whole_machine: replaced.whole_machine,
                action: ResourceRootAuditAction::Revoked,
                at_unix_ms: now,
                actor: actor.into(),
            });
        }
        roots
            .bindings
            .insert(kind.clone(), Arc::new(binding.clone()));
        let sequence = roots.audit.len() as u64 + 1;
        roots.audit.push(ResourceRootAuditEntry {
            sequence,
            root: kind,
            generation,
            path: binding.path,
            whole_machine,
            action: ResourceRootAuditAction::Bound,
            at_unix_ms: now,
            actor: actor.into(),
        });
        if let Some(sink) = self
            .authority_sink
            .read()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
            .clone()
        {
            if let Err(error) = sink(authority_state_from_parts(
                self.session_id,
                self.project_id.clone(),
                policy.clone(),
                ledger.clone(),
                &roots,
            )) {
                *roots = previous;
                return Err(error).context("persist resource-root binding");
            }
        }
        Ok(())
    }

    /// Remove the host binding. Pending executions recheck this at their next
    /// host call, so revocation takes effect without widening workspace paths.
    pub fn clear_host_machine_root(&self) -> Result<()> {
        self.clear_resource_root(&crate::vm::ResourceRoot::HostMachine)
    }

    pub fn clear_project_root(&self) -> Result<()> {
        self.clear_resource_root(&crate::vm::ResourceRoot::Project)
    }

    pub fn clear_task_output_root(&self) -> Result<()> {
        self.clear_resource_root(&crate::vm::ResourceRoot::TaskOutput)
    }

    fn clear_resource_root(&self, kind: &crate::vm::ResourceRoot) -> Result<()> {
        let _authority_use = self
            .authority_use_gate
            .write()
            .map_err(|_| anyhow::anyhow!("authority-use gate poisoned"))?;
        let policy = self
            .capability_policy
            .read()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let mut roots = self
            .resource_roots
            .write()
            .map_err(|_| anyhow::anyhow!("resource-root binding lock poisoned"))?;
        let previous = roots.clone();
        let Some(binding) = roots.bindings.remove(kind) else {
            return Ok(());
        };
        let sequence = roots.audit.len() as u64 + 1;
        roots.audit.push(ResourceRootAuditEntry {
            sequence,
            root: kind.clone(),
            generation: binding.generation,
            path: binding.path.clone(),
            whole_machine: binding.whole_machine,
            action: ResourceRootAuditAction::Revoked,
            at_unix_ms: unix_time_ms(),
            actor: "local-user".into(),
        });
        if let Some(sink) = self
            .authority_sink
            .read()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
            .clone()
        {
            if let Err(error) = sink(authority_state_from_parts(
                self.session_id,
                self.project_id.clone(),
                policy.clone(),
                ledger.clone(),
                &roots,
            )) {
                *roots = previous;
                return Err(error).context("persist resource-root revocation");
            }
        }
        Ok(())
    }

    /// Attach the host's MemTree service to the typed capability boundary.
    /// Keeping this explicit prevents a VM from accidentally acquiring a
    /// second memory database or an ambient memory authority.
    pub fn attach_memory(&self, memory: Arc<crate::memory::MemorySystem>) {
        *self.memory.write().expect("memory binding lock poisoned") = Some(memory);
    }

    /// Grant a typed capability after an approval decision. A saved typed
    /// execution rechecks this structured grant when it is resumed.
    pub fn grant_typed_capability(&self, requirement: CapabilityRequirement) -> Result<uuid::Uuid> {
        self.issue_typed_capability(requirement, GrantScope::Global, "local-user", None)
    }

    /// Record reusable or exact authority without placing it in ambient VM
    /// state. Each ProgramRun derives its effective grants from its own task,
    /// session, and project identity. Exact `once` grants are consumed only
    /// by the correlated pending request.
    pub fn issue_typed_capability(
        &self,
        mut requirement: CapabilityRequirement,
        scope: GrantScope,
        actor: impl Into<String>,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<uuid::Uuid> {
        normalize_process_grant(&mut requirement).map_err(anyhow::Error::msg)?;
        self.mutate_authority(|policy, ledger| {
            if !policy.permits(&requirement) {
                bail!(
                    "capability {:?} is denied by policy {}",
                    requirement.capability,
                    policy.policy_hash
                );
            }
            ledger
                .issue(
                    requirement,
                    scope,
                    policy.policy_hash.clone(),
                    actor,
                    unix_time_ms(),
                    expires_at_unix_ms,
                )
                .map_err(anyhow::Error::msg)
        })
    }

    pub fn capability_session_id(&self) -> uuid::Uuid {
        self.session_id
    }

    pub fn capability_project_id(&self) -> &str {
        &self.project_id
    }

    /// Revoke one recorded grant by stable identity. Pending and future runs
    /// observe the rebuilt active set at their next verified boundary.
    pub fn revoke_typed_capability(&self, grant_id: uuid::Uuid) -> Result<bool> {
        self.mutate_authority(|_, ledger| Ok(ledger.revoke(grant_id, "local-user", unix_time_ms())))
    }

    pub fn capability_ledger(&self) -> Result<CapabilityLedger> {
        Ok(self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?
            .clone())
    }

    pub fn authority_state(&self) -> Result<ProgramRuntimeAuthorityState> {
        let policy = self
            .capability_policy
            .read()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        self.authority_state_with(policy.clone(), ledger.clone())
    }

    fn authority_state_with(
        &self,
        policy: CapabilityPolicy,
        ledger: CapabilityLedger,
    ) -> Result<ProgramRuntimeAuthorityState> {
        let roots = self
            .resource_roots
            .read()
            .map_err(|_| anyhow::anyhow!("resource-root binding lock poisoned"))?;
        Ok(authority_state_from_parts(
            self.session_id,
            self.project_id.clone(),
            policy,
            ledger,
            &roots,
        ))
    }

    pub fn capability_policy(&self) -> Result<CapabilityPolicy> {
        Ok(self
            .capability_policy
            .read()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?
            .clone())
    }

    /// Install application persistence for subsequent authority mutations.
    /// Restoring VM state never installs this hook implicitly.
    pub fn set_authority_sink(&self, sink: ProgramRuntimeAuthoritySink) -> Result<()> {
        *self
            .authority_sink
            .write()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))? = Some(sink);
        Ok(())
    }

    /// Disconnect an archived/detached runtime from its former policy file.
    pub fn clear_authority_sink(&self) -> Result<()> {
        *self
            .authority_sink
            .write()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))? = None;
        Ok(())
    }

    /// Apply one ledger mutation and durably publish the resulting host
    /// authority before exposing it to a ProgramRun. A failed sink restores
    /// the previous in-memory ledger and active compact grants.
    fn mutate_authority<T>(
        &self,
        mutation: impl FnOnce(&CapabilityPolicy, &mut CapabilityLedger) -> Result<T>,
    ) -> Result<T> {
        let _authority_use = self
            .authority_use_gate
            .write()
            .map_err(|_| anyhow::anyhow!("authority-use gate poisoned"))?;
        let sink = self
            .authority_sink
            .read()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
            .clone();
        let policy = self
            .capability_policy
            .read()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?;
        let (result, previous) = {
            let mut ledger = self
                .capability_ledger
                .lock()
                .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
            let previous = ledger.clone();
            let result = mutation(&policy, &mut ledger)?;
            if let Some(sink) = &sink {
                let state = self.authority_state_with(policy.clone(), ledger.clone())?;
                if let Err(error) = sink(state) {
                    *ledger = previous;
                    return Err(error).context("persist ProgramRuntime authority mutation");
                }
            }
            (result, previous)
        };
        drop(policy);
        if let Err(error) = self.refresh_active_grants() {
            if let Ok(mut ledger) = self.capability_ledger.lock() {
                *ledger = previous.clone();
            }
            if let Some(sink) = sink {
                let _ = sink(self.authority_state_with(self.capability_policy()?, previous)?);
            }
            return Err(error);
        }
        Ok(result)
    }

    /// Replace the host-owned policy atomically with the corresponding grant
    /// revocations. A changed policy hash invalidates every active grant from
    /// the previous immutable policy revision. Reusing a revision identity
    /// for different contents fails closed.
    pub fn apply_capability_policy(
        &self,
        policy: CapabilityPolicy,
        actor: impl Into<String>,
    ) -> Result<Vec<uuid::Uuid>> {
        policy.validate().map_err(anyhow::Error::msg)?;
        let _authority_use = self
            .authority_use_gate
            .write()
            .map_err(|_| anyhow::anyhow!("authority-use gate poisoned"))?;
        let actor = actor.into();
        let sink = self
            .authority_sink
            .read()
            .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
            .clone();
        let mut current_policy = self
            .capability_policy
            .write()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))?;
        let mut ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let previous_policy = current_policy.clone();
        let previous_ledger = ledger.clone();
        if current_policy.policy_hash == policy.policy_hash {
            if *current_policy == policy {
                return Ok(Vec::new());
            }
            bail!(
                "capability policy revision {} cannot be reused for different policy contents",
                policy.policy_hash
            );
        }
        let now = unix_time_ms();
        let revoked = ledger
            .grants
            .grants
            .iter()
            .filter(|grant| {
                grant.is_active(now)
                    && (grant.policy_hash != policy.policy_hash
                        || !policy.permits(&grant.requirement))
            })
            .map(|grant| grant.id)
            .collect::<Vec<_>>();
        for grant_id in &revoked {
            let did_revoke = ledger.revoke(*grant_id, actor.clone(), now);
            debug_assert!(did_revoke);
        }
        if let Some(sink) = &sink {
            let state = self.authority_state_with(policy.clone(), ledger.clone())?;
            if let Err(error) = sink(state) {
                *ledger = previous_ledger;
                return Err(error).context("persist ProgramRuntime capability policy");
            }
        }
        *current_policy = policy;
        drop(ledger);
        drop(current_policy);

        if let Err(error) = self.refresh_active_grants() {
            if let Ok(mut current_policy) = self.capability_policy.write() {
                *current_policy = previous_policy.clone();
            }
            if let Ok(mut ledger) = self.capability_ledger.lock() {
                *ledger = previous_ledger.clone();
            }
            if let Some(sink) = sink {
                let _ = sink(self.authority_state_with(previous_policy, previous_ledger)?);
            }
            return Err(error);
        }
        Ok(revoked)
    }

    /// Restore authority only before this runtime is shared with concurrent
    /// callers. Active grants from another policy version are rejected rather
    /// than silently becoming ambient or unexpectedly inactive.
    pub fn restore_authority_state(&mut self, state: ProgramRuntimeAuthorityState) -> Result<()> {
        if state.format_version != PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION {
            bail!(
                "unsupported ProgramRuntime authority state version {}; expected {}",
                state.format_version,
                PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION
            );
        }
        if state.project_id.trim().is_empty() {
            bail!("ProgramRuntime authority state has no project identity");
        }
        state.policy.validate().map_err(anyhow::Error::msg)?;
        validate_restored_process_authority(&state.ledger)?;
        let restored_roots =
            validate_resource_root_authority(&state.resource_roots, &state.resource_root_audit)?;
        let now = unix_time_ms();
        if state.ledger.grants.grants.iter().any(|grant| {
            grant.is_active(now)
                && (grant.policy_hash != state.policy.policy_hash
                    || !state.policy.permits(&grant.requirement))
        }) {
            bail!(
                "ProgramRuntime authority state contains an active grant from another policy or for a denied capability"
            );
        }
        self.session_id = state.session_id;
        self.project_id = state.project_id;
        *self
            .capability_policy
            .write()
            .map_err(|_| anyhow::anyhow!("capability policy lock poisoned"))? = state.policy;
        *self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))? = state.ledger;
        *self
            .resource_roots
            .write()
            .map_err(|_| anyhow::anyhow!("resource-root binding lock poisoned"))? = restored_roots;
        self.refresh_active_grants()
    }

    /// Restore host-owned authority records independently of a VM checkpoint.
    pub fn restore_capability_ledger(&self, ledger: CapabilityLedger) -> Result<()> {
        let policy = self.capability_policy()?;
        validate_restored_process_authority(&ledger)?;
        let now = unix_time_ms();
        if ledger.grants.grants.iter().any(|grant| {
            grant.is_active(now)
                && (grant.policy_hash != policy.policy_hash || !policy.permits(&grant.requirement))
        }) {
            bail!("capability ledger contains an active grant rejected by the current policy");
        }
        *self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))? = ledger;
        self.refresh_active_grants()
    }

    fn refresh_active_grants(&self) -> Result<()> {
        let now = unix_time_ms();
        let policy = self.capability_policy()?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let grants = ledger
            .grants
            .active_requirements_for(&AuthorizationContext {
                now_unix_ms: now,
                task_id: None,
                session_id: self.session_id,
                project_id: Some(self.project_id.clone()),
                policy_hash: policy.policy_hash,
            })
            .cloned()
            .fold(TypedRuntime::intrinsic_grants(), |grants, requirement| {
                grants.union(&EffectSet::from_requirement(requirement))
            });
        self.typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
            .set_grants(grants);
        Ok(())
    }

    pub(crate) fn effective_grants_for(
        &self,
        caller: Option<&agents::AgentIdentity>,
    ) -> Result<EffectSet> {
        let context = self.authorization_context_for(caller)?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let reusable = ledger
            .grants
            .active_requirements_for(&context)
            .cloned()
            .fold(TypedRuntime::intrinsic_grants(), |grants, requirement| {
                grants.union(&EffectSet::from_requirement(requirement))
            });
        let Some(caller) = caller else {
            return Ok(reusable);
        };

        // Session/project/global authority is inherited only within the
        // child's creation-time ceiling. Keep either representable side of a
        // covering pair so a later broad replacement can preserve an older
        // narrow ceiling without widening it (and vice versa).
        let task_grants = ledger
            .grants
            .grants
            .iter()
            .filter(|grant| {
                grant.is_active(context.now_unix_ms)
                    && grant.policy_hash == context.policy_hash
                    && matches!(
                        grant.scope,
                        GrantScope::Task { task_id } if task_id == caller.task_id
                    )
            })
            .map(|grant| grant.requirement.clone())
            .fold(EffectSet::pure(), |grants, requirement| {
                grants.union(&EffectSet::from_requirement(requirement))
            });
        let inherited = attenuate_effects(&reusable, &caller.grant_ceiling);
        Ok(inherited.union(&task_grants))
    }

    /// Resolve host-issued grant identities into a child creation-time
    /// ceiling. IDs are only lookup keys: every spawn rechecks live policy,
    /// caller scope, expiry/revocation, and the caller's existing ceiling.
    pub(crate) fn resolve_capability_grant_subset(
        &self,
        caller: Option<&agents::AgentIdentity>,
        grant_ids: &[uuid::Uuid],
    ) -> Result<EffectSet> {
        let context = self.authorization_context_for(caller)?;
        let available = self.effective_grants_for(caller)?;
        let ledger = self
            .capability_ledger
            .lock()
            .map_err(|_| anyhow::anyhow!("capability ledger lock poisoned"))?;
        let applicable = ledger
            .grants
            .active_grants_for(&context)
            .map(|grant| (grant.id, &grant.requirement))
            .collect::<HashMap<_, _>>();
        let mut seen = std::collections::HashSet::new();
        let mut selected = TypedRuntime::intrinsic_grants();
        for grant_id in grant_ids {
            if !seen.insert(*grant_id) {
                bail!("capability grant {grant_id} was selected more than once");
            }
            let requirement = applicable.get(grant_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "capability grant {grant_id} is unknown, inactive, or outside the caller scope"
                )
            })?;
            let requested = EffectSet::from_requirement((*requirement).clone());
            if !available.grants(&requested) {
                bail!("capability grant {grant_id} exceeds the caller's inherited ceiling");
            }
            selected = selected.union(&requested);
        }
        Ok(selected)
    }

    fn authorization_context_for(
        &self,
        caller: Option<&agents::AgentIdentity>,
    ) -> Result<AuthorizationContext> {
        Ok(AuthorizationContext {
            now_unix_ms: unix_time_ms(),
            task_id: caller.map(|caller| caller.task_id),
            session_id: self.session_id,
            project_id: Some(self.project_id.clone()),
            policy_hash: self.capability_policy()?.policy_hash,
        })
    }

    fn release_output_handles(&self, execution_id: uuid::Uuid) -> Result<()> {
        self.output_handles
            .lock()
            .map_err(|_| anyhow::anyhow!("output handle registry lock poisoned"))?
            .retain(|_, record| record.owner != execution_id);
        self.streams
            .lock()
            .map_err(|_| anyhow::anyhow!("stream registry lock poisoned"))?
            .retain(|_, stream| stream.owner != execution_id);
        self.network
            .lock()
            .map_err(|_| anyhow::anyhow!("network socket registry lock poisoned"))?
            .retain(|_, socket| socket.owner != execution_id);
        Ok(())
    }

    /// A portable presentation host owns allocation of an `output-open`
    /// handle, but the runtime still owns the per-ProgramRun capability table
    /// that validates later `output-*` effects.  Register the externally
    /// supplied opaque resource exactly when its correlated resume is
    /// accepted; source code never gets a route to forge this entry.
    fn register_resumed_output_handle(
        &self,
        execution_id: uuid::Uuid,
        pending: &PendingTypedExecution,
        values: &[TypedValue],
    ) -> Result<()> {
        let is_output_open = pending
            .suspension
            .pending_host_call
            .as_ref()
            .and_then(|call| call.origin.word.as_deref())
            == Some("output-open");
        if !is_output_open {
            return Ok(());
        }

        let [TypedValue::Resource {
            kind,
            handle,
            generation,
        }] = values
        else {
            bail!("portable output-open resume requires one output-handle resource");
        };
        if kind != "output-handle" || handle.is_empty() {
            bail!("portable output-open resume returned an invalid output handle");
        }

        let mut handles = self
            .output_handles
            .lock()
            .map_err(|_| anyhow::anyhow!("output handle registry lock poisoned"))?;
        if handles.contains_key(handle) {
            bail!("portable output-open resume returned an already-live output handle");
        }
        handles.insert(
            handle.clone(),
            OutputHandleRecord {
                owner: execution_id,
                generation: *generation,
            },
        );
        Ok(())
    }

    /// Return a UI-safe summary of a suspended execution without exposing its
    /// stack, captures, or capability arguments to an unrelated client.
    pub fn pending_typed_execution(
        &self,
        execution_id: uuid::Uuid,
    ) -> Result<Option<PendingTypedExecutionInfo>> {
        let pending = self
            .pending_typed
            .lock()
            .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
        let Some(pending) = pending.get(&execution_id) else {
            return Ok(None);
        };
        let yielded_type = pending
            .suspension
            .yielded_value
            .as_ref()
            .map(TypedValue::value_type);
        let yielded_value = pending
            .suspension
            .yielded_value
            .clone()
            .map(typed_value)
            .and_then(Result::ok);
        Ok(Some({
            let resume_effect_sequence =
                pending.suspension.pending_host_call.as_ref().and_then(|_| {
                    pending
                        .suspension
                        .event_journal
                        .last()
                        .map(|effect| effect.sequence)
                });
            let reason = match &pending.suspension.pending_host_call {
                Some(call)
                    if pending
                        .suspension
                        .effect_journal
                        .last()
                        .is_some_and(|entry| {
                            matches!(
                                entry.state,
                                crate::vm::EffectJournalState::AwaitingHostResult
                            )
                        }) =>
                {
                    PendingTypedReason::AwaitingHostEffect {
                        requirement: call.requirement.clone(),
                    }
                }
                Some(call) => PendingTypedReason::AuthorizationRequired {
                    requirements: vec![call.requirement.clone()],
                },
                None => PendingTypedReason::Yielded,
            };
            PendingTypedExecutionInfo {
                execution_id,
                input_revision: pending.input_revision,
                manifest_generation: pending.context.manifest_generation,
                resume_effect_sequence,
                yielded_value,
                yielded_type,
                reason,
            }
        }))
    }

    /// Number of private continuations retained by this runtime. This is
    /// exposed for application diagnostics and long-running lifecycle tests;
    /// source programs cannot use it to enumerate another run's frames.
    pub fn pending_typed_execution_count(&self) -> Result<usize> {
        Ok(self
            .pending_typed
            .lock()
            .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?
            .len())
    }

    /// Retain a newly suspended run without evicting an existing approval or
    /// yield. If the hard bound is already occupied, cancel this new private
    /// snapshot and return its auditable terminal outcome.
    fn retain_pending_typed_execution(
        &self,
        execution_id: uuid::Uuid,
        pending: PendingTypedExecution,
    ) -> Result<Option<ExecutionOutcome>> {
        let mut retained = self
            .pending_typed
            .lock()
            .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
        if retained.contains_key(&execution_id) {
            drop(retained);
            return self
                .cancel_pending_typed_execution(
                    execution_id,
                    pending,
                    Some("execution id is already retained".into()),
                )
                .map(Some);
        }
        if retained.len() < MAX_PENDING_TYPED_EXECUTIONS {
            retained.insert(execution_id, pending);
            return Ok(None);
        }
        drop(retained);
        self.cancel_pending_typed_execution(
            execution_id,
            pending,
            Some(format!(
                "pending execution capacity {MAX_PENDING_TYPED_EXECUTIONS} is exhausted"
            )),
        )
        .map(Some)
    }

    /// Cancel a suspended VM execution and return its durable audit outcome.
    /// This discards only uncommitted VM-local state; it never attempts to undo
    /// an acknowledged external-effect prefix.
    pub fn cancel_typed_execution_with_outcome(
        &self,
        execution_id: uuid::Uuid,
    ) -> Result<Option<ExecutionOutcome>> {
        let pending = self
            .pending_typed
            .lock()
            .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?
            .remove(&execution_id);
        let Some(pending) = pending else {
            return Ok(None);
        };

        self.cancel_pending_typed_execution(execution_id, pending, None)
            .map(Some)
    }

    fn cancel_pending_typed_execution(
        &self,
        execution_id: uuid::Uuid,
        pending: PendingTypedExecution,
        reason: Option<String>,
    ) -> Result<ExecutionOutcome> {
        let cpu_cancel_error = self
            .typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
            .cancel_suspended_cpu_fiber(&pending.suspension)
            .err()
            .map(|diagnostic| diagnostic.to_string());
        self.release_output_handles(execution_id)?;
        let mut effect_journal = pending.suspension.effect_journal.clone();
        if pending.suspension.pending_host_call.is_some() {
            if let Some(entry) = effect_journal.last_mut() {
                entry.state = crate::vm::EffectJournalState::Cancelled;
            }
        }
        let mut diagnostics = vec![match reason {
            Some(reason) if !reason.trim().is_empty() => {
                format!("typed VM execution cancelled before completion: {reason}")
            }
            _ => "typed VM execution cancelled before completion".into(),
        }];
        if let Some(error) = cpu_cancel_error {
            diagnostics.push(format!(
                "CPU worker cancellation was not acknowledged: {error}"
            ));
        }
        let inferred_capabilities = pending.suspension.effects.0.iter().cloned().collect();
        Ok(ExecutionOutcome {
            execution_id,
            status: ExecutionStatus::Cancelled,
            values: Vec::new(),
            output: truncate_output(pending.output, pending.context.budget.max_output_bytes),
            output_chunks: pending.output_chunks,
            side_effects: pending.side_effects,
            vm_side_effects: pending.suspension.event_journal,
            effect_journal,
            diagnostics,
            vm_diagnostics: Vec::new(),
            inferred_capabilities,
            required_capabilities: Vec::new(),
            approval_prompts: Vec::new(),
            input_revision: pending.input_revision,
            output_revision: pending.input_revision,
            effect: pending.effect,
            backend: ExecutionBackend::TypedVm,
            elapsed_ms: 0,
        })
    }

    /// Compatibility boolean form of [`Self::cancel_typed_execution_with_outcome`].
    pub fn cancel_typed_execution(&self, execution_id: uuid::Uuid) -> Result<bool> {
        Ok(self
            .cancel_typed_execution_with_outcome(execution_id)?
            .is_some())
    }

    /// Cancel an awaited portable effect only when it still owns the supplied
    /// `(execution_id, sequence)` boundary. A stale external cancellation
    /// cannot discard a newer suspension for the same ProgramRun.
    pub async fn cancel_typed_execution_for_effect(
        &self,
        execution_id: uuid::Uuid,
        effect_sequence: u64,
        reason: Option<String>,
    ) -> Result<ExecutionOutcome> {
        let pending = {
            let _submission = self.submission_gate.lock().await;
            let mut pending_runs = self
                .pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
            let pending = pending_runs
                .get(&execution_id)
                .ok_or_else(|| anyhow::anyhow!("no resumable typed execution {execution_id}"))?;
            let actual = pending
                .suspension
                .pending_host_call
                .as_ref()
                .and_then(|_| {
                    pending
                        .suspension
                        .event_journal
                        .last()
                        .map(|effect| effect.sequence)
                })
                .ok_or_else(|| anyhow::anyhow!("typed execution is not awaiting a host effect"))?;
            if actual != effect_sequence {
                bail!(
                    "stale typed effect cancellation: supplied sequence {effect_sequence}, pending sequence is {actual}"
                );
            }
            pending_runs
                .remove(&execution_id)
                .expect("execution was checked while its pending lock was held")
        };
        self.cancel_pending_typed_execution(execution_id, pending, reason)
    }

    /// Record a deliberate denial for the exact awaited portable effect and
    /// discard its uncommitted continuation. Unlike cancellation, the audit
    /// journal preserves that the host rejected a capability request rather
    /// than losing its audience or being interrupted.
    pub fn deny_typed_execution_for_effect(
        &self,
        execution_id: uuid::Uuid,
        effect_sequence: u64,
        reason: impl Into<String>,
    ) -> Result<ExecutionOutcome> {
        let reason = reason.into();
        let mut pending_runs = self
            .pending_typed
            .lock()
            .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
        let pending = pending_runs
            .get(&execution_id)
            .ok_or_else(|| anyhow::anyhow!("no resumable typed execution {execution_id}"))?;
        let actual = pending
            .suspension
            .pending_host_call
            .as_ref()
            .and_then(|_| {
                pending
                    .suspension
                    .event_journal
                    .last()
                    .map(|effect| effect.sequence)
            })
            .ok_or_else(|| anyhow::anyhow!("typed execution is not awaiting a host effect"))?;
        if actual != effect_sequence {
            bail!(
                "stale typed effect denial: supplied sequence {effect_sequence}, pending sequence is {actual}"
            );
        }
        let pending = pending_runs
            .remove(&execution_id)
            .expect("execution was checked while its pending lock was held");
        drop(pending_runs);
        self.deny_pending_typed_execution(execution_id, pending, reason)
    }

    fn deny_pending_typed_execution(
        &self,
        execution_id: uuid::Uuid,
        pending: PendingTypedExecution,
        reason: String,
    ) -> Result<ExecutionOutcome> {
        self.release_output_handles(execution_id)?;

        let mut effect_journal = pending.suspension.effect_journal.clone();
        if let Some(entry) = effect_journal.last_mut() {
            entry.state = crate::vm::EffectJournalState::Denied;
        }
        let inferred_capabilities = pending.suspension.effects.0.iter().cloned().collect();
        Ok(ExecutionOutcome {
            execution_id,
            status: ExecutionStatus::Failed,
            values: Vec::new(),
            output: truncate_output(pending.output, pending.context.budget.max_output_bytes),
            output_chunks: pending.output_chunks,
            side_effects: pending.side_effects,
            vm_side_effects: pending.suspension.event_journal,
            effect_journal,
            diagnostics: vec![format!("typed VM host effect denied: {reason}")],
            vm_diagnostics: Vec::new(),
            inferred_capabilities,
            required_capabilities: Vec::new(),
            approval_prompts: Vec::new(),
            input_revision: pending.input_revision,
            output_revision: pending.input_revision,
            effect: pending.effect,
            backend: ExecutionBackend::TypedVm,
            elapsed_ms: 0,
        })
    }

    /// Serialized denial path for the portable resume ABI. The legacy
    /// synchronous helper above remains available to older adapters, while a
    /// remote event host must not race a result/cancellation for the same
    /// `(execution_id, sequence)` pair.
    async fn deny_typed_execution_for_effect_serialized(
        &self,
        execution_id: uuid::Uuid,
        effect_sequence: u64,
        reason: String,
    ) -> Result<ExecutionOutcome> {
        let pending = {
            let _submission = self.submission_gate.lock().await;
            let mut pending_runs = self
                .pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
            let pending = pending_runs
                .get(&execution_id)
                .ok_or_else(|| anyhow::anyhow!("no resumable typed execution {execution_id}"))?;
            let actual = pending
                .suspension
                .pending_host_call
                .as_ref()
                .and_then(|_| {
                    pending
                        .suspension
                        .event_journal
                        .last()
                        .map(|effect| effect.sequence)
                })
                .ok_or_else(|| anyhow::anyhow!("typed execution is not awaiting a host effect"))?;
            if actual != effect_sequence {
                bail!(
                    "stale typed effect denial: supplied sequence {effect_sequence}, pending sequence is {actual}"
                );
            }
            pending_runs
                .remove(&execution_id)
                .expect("execution was checked while its pending lock was held")
        };
        self.deny_pending_typed_execution(execution_id, pending, reason)
    }

    /// Apply one user approval decision to the exact prompt emitted for a
    /// suspended ProgramRun. The prompt is reconstructed from the retained
    /// continuation before any authority is issued, preventing stale or
    /// forged UI data from widening a different request.
    pub async fn resolve_typed_approval(
        &self,
        prompt: &ApprovalPrompt,
        choice: ApprovalChoice,
        actor: impl Into<String>,
    ) -> Result<ExecutionOutcome> {
        let actor = actor.into();
        let (pending, effect_sequence, denied) = {
            let _submission = self.submission_gate.lock().await;
            let mut pending_runs = self
                .pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
            let pending = pending_runs
                .get(&prompt.request.execution_id)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no resumable typed execution {}",
                        prompt.request.execution_id
                    )
                })?;
            let call = pending
                .suspension
                .pending_host_call
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("typed execution is not awaiting authorization"))?;
            let expected = approval_prompts(
                prompt.request.execution_id,
                std::slice::from_ref(&call.requirement),
                &pending.source,
                &pending.intent,
                Some(&pending.suspension),
                pending.caller.as_ref(),
            )
            .into_iter()
            .next()
            .expect("a pending host call creates one approval prompt");
            if &expected != prompt {
                bail!("stale or forged capability approval prompt");
            }
            let effect_sequence = prompt
                .request
                .effect_sequence
                .ok_or_else(|| anyhow::anyhow!("a runtime approval requires an effect sequence"))?;
            let context = self.authorization_context_for(pending.caller.as_ref())?;
            let denied = matches!(&choice, ApprovalChoice::Deny);

            if denied {
                self.mutate_authority(|_, ledger| {
                    ledger.deny(
                        &prompt.request,
                        "denied by user",
                        actor.clone(),
                        context.now_unix_ms,
                    );
                    Ok(())
                })?;
            } else {
                let (requirement, scope) = match choice {
                    ApprovalChoice::Deny => unreachable!("denial handled above"),
                    ApprovalChoice::AllowOnce => (
                        prompt.exact.clone(),
                        GrantScope::Once {
                            request_id: prompt.request.id,
                        },
                    ),
                    ApprovalChoice::AllowTask => {
                        let task_id = pending
                            .caller
                            .as_ref()
                            .map(|caller| caller.task_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "task-scoped approval requires a child task identity"
                                )
                            })?;
                        (prompt.exact.clone(), GrantScope::Task { task_id })
                    }
                    ApprovalChoice::AllowSession => (
                        prompt.exact.clone(),
                        GrantScope::Session {
                            session_id: self.session_id,
                        },
                    ),
                    ApprovalChoice::AllowProjectExact => (
                        prompt.exact.clone(),
                        GrantScope::Project {
                            project_id: self.project_id.clone(),
                        },
                    ),
                    ApprovalChoice::AllowProjectPattern { requirement } => {
                        if !requirement.covers(&prompt.exact) {
                            bail!("project approval pattern does not cover the exact request");
                        }
                        (
                            requirement,
                            GrantScope::Project {
                                project_id: self.project_id.clone(),
                            },
                        )
                    }
                    ApprovalChoice::AllowGlobal => (prompt.exact.clone(), GrantScope::Global),
                };
                self.mutate_authority(|policy, ledger| {
                    if !policy.permits(&requirement) {
                        bail!(
                            "capability {:?} is denied by policy {}",
                            requirement.capability,
                            policy.policy_hash
                        );
                    }
                    let mut current_context = context.clone();
                    current_context.policy_hash = policy.policy_hash.clone();
                    ledger
                        .issue(
                            requirement,
                            scope,
                            policy.policy_hash.clone(),
                            actor.clone(),
                            context.now_unix_ms,
                            None,
                        )
                        .map_err(anyhow::Error::msg)?;
                    if !matches!(
                        ledger.grants.authorize(&prompt.request, &current_context),
                        AuthorizationDecision::Allowed { .. }
                    ) {
                        bail!("new capability grant did not authorize its exact request");
                    }
                    Ok(())
                })?;
            }
            let pending = pending_runs
                .remove(&prompt.request.execution_id)
                .expect("approval target was validated while holding the pending lock");
            (pending, effect_sequence, denied)
        };

        if denied {
            return self.deny_pending_typed_execution(
                prompt.request.execution_id,
                pending,
                "denied by user".into(),
            );
        }
        self.refresh_active_grants()?;
        self.resume_removed_typed_execution(
            prompt.request.execution_id,
            pending,
            Some(effect_sequence),
            None,
            true,
        )
        .await
    }

    /// Resume a typed execution that previously yielded or awaited approval.
    /// The execution id is stable across the pause; source is never submitted
    /// again. A revision mismatch deliberately invalidates the saved frame,
    /// because applying it to a different Brain state would be unsound.
    pub async fn resume_typed_execution(
        &self,
        execution_id: uuid::Uuid,
    ) -> Result<ExecutionOutcome> {
        self.resume_typed_execution_inner(execution_id, None, None, false)
            .await
    }

    /// Resume an awaited host effect only if it is still the same portable
    /// `(execution_id, sequence)` boundary. A stale approval/result is
    /// rejected without consuming the saved continuation.
    pub async fn resume_typed_execution_for_effect(
        &self,
        execution_id: uuid::Uuid,
        effect_sequence: u64,
    ) -> Result<ExecutionOutcome> {
        self.resume_typed_execution_inner(execution_id, Some(effect_sequence), None, false)
            .await
    }

    /// Resume a specific awaited host effect with an externally produced,
    /// verifier-checked result. This is the host-facing half of the portable
    /// `VmResume` protocol: the result is correlated to the exact journal
    /// sequence and is never dispatched through the local host binding again.
    pub async fn resume_typed_execution_with_effect_result(
        &self,
        execution_id: uuid::Uuid,
        effect_sequence: u64,
        values: Vec<TypedValue>,
    ) -> Result<ExecutionOutcome> {
        self.resume_typed_execution_inner(execution_id, Some(effect_sequence), Some(values), false)
            .await
    }

    /// Apply one portable host reply. This is the complete embedder-facing
    /// resume API: all paths are correlated to the original event handle and
    /// retain an auditable effect-journal terminal state.
    pub async fn resume_vm_effect(&self, resume: VmResume) -> Result<ExecutionOutcome> {
        match resume.response {
            VmResumeResponse::Result { values } => {
                self.resume_typed_execution_with_effect_result(
                    resume.execution_id,
                    resume.sequence,
                    values,
                )
                .await
            }
            VmResumeResponse::Denied { reason } => {
                self.deny_typed_execution_for_effect_serialized(
                    resume.execution_id,
                    resume.sequence,
                    reason,
                )
                .await
            }
            VmResumeResponse::Cancelled { reason } => {
                self.cancel_typed_execution_for_effect(resume.execution_id, resume.sequence, reason)
                    .await
            }
        }
    }

    async fn resume_typed_execution_inner(
        &self,
        execution_id: uuid::Uuid,
        expected_effect_sequence: Option<u64>,
        external_effect_result: Option<Vec<TypedValue>>,
        authorize_pending_host_call: bool,
    ) -> Result<ExecutionOutcome> {
        // Serialize only removal of this continuation from the pending table.
        // The private working VM then resumes without blocking unrelated
        // submissions; completion reacquires this gate for the optimistic
        // revision commit.
        let pending = {
            let _submission = self.submission_gate.lock().await;
            let mut pending_runs = self
                .pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?;
            if let Some(expected) = expected_effect_sequence {
                let pending = pending_runs.get(&execution_id).ok_or_else(|| {
                    anyhow::anyhow!("no resumable typed execution {execution_id}")
                })?;
                let actual = pending
                    .suspension
                    .pending_host_call
                    .as_ref()
                    .and_then(|_| {
                        pending
                            .suspension
                            .event_journal
                            .last()
                            .map(|effect| effect.sequence)
                    })
                    .ok_or_else(|| {
                        anyhow::anyhow!("typed execution is not awaiting a host effect")
                    })?;
                if actual != expected {
                    bail!(
                        "stale typed effect resume: expected sequence {expected}, pending sequence is {actual}"
                    );
                }
            }
            pending_runs
                .remove(&execution_id)
                .ok_or_else(|| anyhow::anyhow!("no resumable typed execution {execution_id}"))?
        };
        self.resume_removed_typed_execution(
            execution_id,
            pending,
            expected_effect_sequence,
            external_effect_result,
            authorize_pending_host_call,
        )
        .await
    }

    async fn resume_removed_typed_execution(
        &self,
        execution_id: uuid::Uuid,
        pending: PendingTypedExecution,
        expected_effect_sequence: Option<u64>,
        external_effect_result: Option<Vec<TypedValue>>,
        authorize_pending_host_call: bool,
    ) -> Result<ExecutionOutcome> {
        let started = Instant::now();
        if pending.context.manifest_generation != self.manifest_generation() {
            self.release_output_handles(execution_id)?;
            return Ok(failed_pending_resume(
                execution_id,
                &pending,
                self.revision(),
                "resumable typed execution has a stale VM manifest generation".to_owned(),
                started.elapsed(),
            ));
        }
        if pending.input_revision != self.revision() {
            self.release_output_handles(execution_id)?;
            let current_revision = self.revision();
            return Ok(failed_pending_resume(
                execution_id,
                &pending,
                current_revision,
                format!(
                    "resumable typed execution has input revision {}; current revision is {current_revision}",
                    pending.input_revision,
                ),
                started.elapsed(),
            ));
        }
        let external_effect_result = match external_effect_result {
            Some(values) => Some((
                expected_effect_sequence.expect("external result requires an effect sequence"),
                values,
            )),
            None => None,
        };
        if let Some((_, values)) = &external_effect_result {
            if let Err(error) = self.register_resumed_output_handle(execution_id, &pending, values)
            {
                self.release_output_handles(execution_id)?;
                return Ok(failed_pending_resume(
                    execution_id,
                    &pending,
                    self.revision(),
                    error.to_string(),
                    started.elapsed(),
                ));
            }
        }
        // Grants are authority policy, not a speculative stack/dictionary
        // mutation. Ordinary interactive runs see an approval granted while
        // they were suspended. A scheduled callback instead retains its
        // creation-time ceiling so elapsed time cannot expand authority.
        self.refresh_active_grants()?;
        let mut resumed_runtime = pending.working_runtime.clone();
        if let Some(grant_ceiling) = &pending.grant_ceiling {
            resumed_runtime.set_grants(grant_ceiling.clone());
        } else {
            resumed_runtime.set_grants(self.effective_grants_for(pending.caller.as_ref())?);
        }
        let (working_runtime, execution) = self
            .resume_typed_program(
                resumed_runtime,
                &pending,
                external_effect_result,
                authorize_pending_host_call,
            )
            .await?;
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let mut output = pending.output;
        output.push_str(&execution.output);
        let mut output_chunks = pending.output_chunks;
        output_chunks.extend(execution.output_chunks.clone());
        let mut side_effects = pending.side_effects;
        side_effects.extend(execution.side_effects.clone());
        // Typed execution returns its complete ordered journal, including
        // events retained in the serialized suspension. Do not append the
        // prior projection here or a resumed run would duplicate events.
        let vm_side_effects = execution.vm_side_effects.clone();
        let effect_journal = execution.effect_journal.clone();
        let inferred_capabilities = execution.effects.0.iter().cloned().collect::<Vec<_>>();

        let suspension = execution.suspension.clone();
        if let Some(suspension) = suspension.clone() {
            if let Some(cancelled) = self.retain_pending_typed_execution(
                execution_id,
                PendingTypedExecution {
                    working_runtime: working_runtime.clone(),
                    suspension,
                    context: pending.context.clone(),
                    input_revision: pending.input_revision,
                    language: pending.language,
                    source: pending.source.clone(),
                    intent: pending.intent.clone(),
                    effect: pending.effect,
                    caller: pending.caller.clone(),
                    output: output.clone(),
                    output_chunks: output_chunks.clone(),
                    side_effects: side_effects.clone(),
                    effect_sink: pending.effect_sink.clone(),
                    effect_audit: pending.effect_audit.clone(),
                    deferred_host_effects: pending.deferred_host_effects,
                    grant_ceiling: pending.grant_ceiling.clone(),
                },
            )? {
                return Ok(cancelled);
            }
        }
        if suspension.is_none() {
            self.release_output_handles(execution_id)?;
        }

        let completion_commit = if matches!(execution.status, TypedExecutionStatus::Completed) {
            Some(
                self.commit_working_runtime(pending.input_revision, working_runtime)
                    .await,
            )
        } else {
            None
        };

        Ok(match execution.status {
            TypedExecutionStatus::Completed => {
                let output_revision = match completion_commit.expect("completed run has commit") {
                    Ok(revision) => revision,
                    Err(error) => {
                        return Ok(ExecutionOutcome {
                            execution_id,
                            status: ExecutionStatus::Failed,
                            values: Vec::new(),
                            output: truncate_output(
                                output,
                                pending.context.budget.max_output_bytes,
                            ),
                            output_chunks,
                            side_effects,
                            vm_side_effects,
                            effect_journal,
                            diagnostics: vec![error.to_string()],
                            vm_diagnostics: Vec::new(),
                            inferred_capabilities: inferred_capabilities.clone(),
                            required_capabilities: Vec::new(),
                            approval_prompts: Vec::new(),
                            input_revision: pending.input_revision,
                            output_revision: self.revision(),
                            effect: pending.effect,
                            backend: ExecutionBackend::TypedVm,
                            elapsed_ms,
                        });
                    }
                };
                ExecutionOutcome {
                    execution_id,
                    status: ExecutionStatus::Completed,
                    values: typed_values(execution.values)?,
                    output: truncate_output(output, pending.context.budget.max_output_bytes),
                    output_chunks,
                    side_effects,
                    vm_side_effects,
                    effect_journal,
                    diagnostics: Vec::new(),
                    vm_diagnostics: Vec::new(),
                    inferred_capabilities: inferred_capabilities.clone(),
                    required_capabilities: Vec::new(),
                    approval_prompts: Vec::new(),
                    input_revision: pending.input_revision,
                    output_revision,
                    effect: pending.effect,
                    backend: ExecutionBackend::TypedVm,
                    elapsed_ms,
                }
            }
            TypedExecutionStatus::Suspended => ExecutionOutcome {
                execution_id,
                status: ExecutionStatus::Suspended,
                values: Vec::new(),
                output: truncate_output(output, pending.context.budget.max_output_bytes),
                output_chunks,
                side_effects,
                vm_side_effects,
                effect_journal,
                diagnostics: Vec::new(),
                vm_diagnostics: execution.diagnostics,
                inferred_capabilities: inferred_capabilities.clone(),
                required_capabilities: Vec::new(),
                approval_prompts: Vec::new(),
                input_revision: pending.input_revision,
                output_revision: pending.input_revision,
                effect: pending.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
            TypedExecutionStatus::AuthorizationRequired { requirements } => ExecutionOutcome {
                execution_id,
                status: ExecutionStatus::AuthorizationRequired,
                values: Vec::new(),
                output: truncate_output(output, pending.context.budget.max_output_bytes),
                output_chunks,
                side_effects,
                vm_side_effects,
                effect_journal,
                diagnostics: Vec::new(),
                vm_diagnostics: execution.diagnostics,
                approval_prompts: approval_prompts(
                    execution_id,
                    &requirements,
                    &pending.source,
                    &pending.intent,
                    suspension.as_ref(),
                    pending.caller.as_ref(),
                ),
                inferred_capabilities: inferred_capabilities.clone(),
                required_capabilities: requirements,
                input_revision: pending.input_revision,
                output_revision: pending.input_revision,
                effect: pending.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
            TypedExecutionStatus::Failed => ExecutionOutcome {
                execution_id,
                status: ExecutionStatus::Failed,
                values: Vec::new(),
                output: truncate_output(output, pending.context.budget.max_output_bytes),
                output_chunks,
                side_effects,
                vm_side_effects,
                effect_journal,
                // Render rather than `to_string`: the span, the expected and found types, and the
                // hints are what let a reader — or a model asked to repair this — find the part of
                // the program that is wrong. `Display` drops all of it.
                diagnostics: execution
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.render(Some(&pending.source)))
                    .collect(),
                vm_diagnostics: execution.diagnostics,
                inferred_capabilities,
                required_capabilities: Vec::new(),
                approval_prompts: Vec::new(),
                input_revision: pending.input_revision,
                output_revision: pending.input_revision,
                effect: pending.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
        })
    }

    /// The agent binding a typed program would be handed, built the same way the execution paths
    /// build it.
    ///
    /// Exists so the agent capability can be tested against a fake `AgentSpawning` rather than a
    /// real scheduler, which needs a provider resolver, a generator and a Brain client to exist at
    /// all. The production paths construct this inline; this returns the same thing.
    #[cfg(test)]
    pub(crate) fn agent_binding_for_test(
        &self,
        caller: Option<agents::AgentIdentity>,
    ) -> Option<agent_vm::AgentVmBinding> {
        self.agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, caller))
    }

    /// Attach the application-owned agent scheduler, replacing any prior attachment.
    pub fn attach_agent_scheduler<S: agents::AgentSpawning + 'static>(&self, scheduler: &Arc<S>) {
        *self
            .agent_scheduler
            .write()
            .expect("agent scheduler lock poisoned") =
            Arc::downgrade(scheduler) as Weak<dyn agents::AgentSpawning>;
    }

    pub fn manifest_generation(&self) -> u64 {
        self.manifest_generation.load(Ordering::Acquire)
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Acquire)
    }

    /// Install a newer application-owned reducible checkpoint without
    /// replacing this frontend's host authority or resource bindings. The
    /// submission gate makes hydration mutually exclusive with ProgramRun
    /// commits; retained continuations prevent replacement because they were
    /// compiled against the current revision lineage.
    pub async fn hydrate_reducible_state_if_newer(
        &self,
        checkpoint: TypedRuntimeCheckpoint,
        revision: u64,
    ) -> Result<bool> {
        self.hydrate_reducible_state(checkpoint, revision, true)
            .await
    }

    /// Replace reducible VM state with an exact application-owned checkpoint,
    /// even when its revision is numerically lower than the current lineage.
    /// This is required when one frontend explicitly changes which Brain it
    /// serves: revisions are comparable only within a Brain. Host bindings and
    /// live grants remain frontend-owned and are preserved across the switch.
    pub async fn replace_reducible_state(
        &self,
        checkpoint: TypedRuntimeCheckpoint,
        revision: u64,
    ) -> Result<()> {
        self.hydrate_reducible_state(checkpoint, revision, false)
            .await
            .map(|_| ())
    }

    async fn hydrate_reducible_state(
        &self,
        checkpoint: TypedRuntimeCheckpoint,
        revision: u64,
        only_if_newer: bool,
    ) -> Result<bool> {
        let mut restored =
            TypedRuntime::from_checkpoint(checkpoint.clone()).map_err(|diagnostics| {
                anyhow::anyhow!(
                    "cannot hydrate typed runtime checkpoint: {}",
                    diagnostics
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("; ")
                )
            })?;
        let _submission = self.submission_gate.lock().await;
        let current_revision = self.revision();
        if only_if_newer && revision <= current_revision {
            return Ok(false);
        }
        if self.pending_typed_execution_count()? != 0 {
            bail!("cannot hydrate revision {revision} while typed continuations are pending");
        }

        let host_names = self
            .host_vocabulary
            .read()
            .map_err(|_| anyhow::anyhow!("host vocabulary lock poisoned"))?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let mut typed = self
            .typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?;
        let host_signatures = host_names
            .iter()
            .filter_map(|name| {
                typed
                    .vocabulary()
                    .get(name)
                    .cloned()
                    .map(|signature| (name.clone(), signature))
            })
            .collect::<BTreeMap<_, _>>();
        restored
            .replace_host_vocabulary(Vec::<String>::new(), &host_signatures)
            .map_err(|diagnostic| anyhow::anyhow!(diagnostic.to_string()))?;
        // Reducible checkpoints never carry grants. Keep the live frontend's
        // derived execution ceiling; the capability ledger/policy remains the
        // authoritative source for subsequent ProgramRuns.
        restored.set_grants(typed.grants().clone());
        let stack = restored.stack().to_vec();
        let vocabulary = restored.vocabulary().keys().cloned().collect();
        *typed = restored;
        *self
            .revision_history
            .lock()
            .map_err(|_| anyhow::anyhow!("revision history lock poisoned"))? =
            vec![VmRevisionSnapshot {
                revision,
                stack,
                vocabulary,
                checkpoint: Some(checkpoint),
                checkpoint_diagnostic: None,
            }];
        self.revision.store(revision, Ordering::Release);
        Ok(true)
    }

    /// Snapshot only source-language linking context for report-only replay.
    /// Stack values, grants, pending effects, and host resources are excluded.
    pub fn compiler_context(&self) -> Result<ProgramCompilerContext> {
        let typed = self
            .typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?;
        Ok(ProgramCompilerContext {
            manifest_generation: self.manifest_generation(),
            revision: self.revision(),
            functions: typed.functions().clone(),
        })
    }

    /// Atomically install a private working VM only when the state it was
    /// derived from is still current. Host effects have already been retained
    /// in the caller's journal; a losing commit must never replay them.
    async fn commit_working_runtime(
        &self,
        input_revision: u64,
        mut working_runtime: TypedRuntime,
    ) -> Result<u64> {
        let _submission = self.submission_gate.lock().await;
        let current = self.revision();
        if current != input_revision {
            bail!(
                "stale VM transaction input revision {input_revision}; current revision is {current}"
            );
        }
        let mut typed = self
            .typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?;
        // Per-run execution ceilings are not persistent approval state. In
        // particular, committing a scheduled callback must not erase newer
        // host approval decisions from the shared runtime.
        working_runtime.set_grants(typed.grants().clone());
        let checkpoint = working_runtime.checkpoint();
        *typed = working_runtime;
        let revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
        let mut history = self
            .revision_history
            .lock()
            .map_err(|_| anyhow::anyhow!("revision history lock poisoned"))?;
        history.push(VmRevisionSnapshot {
            revision,
            stack: typed.stack().to_vec(),
            vocabulary: typed.vocabulary().keys().cloned().collect(),
            checkpoint: checkpoint.as_ref().ok().cloned(),
            checkpoint_diagnostic: checkpoint.err().map(|diagnostic| diagnostic.to_string()),
        });
        let excess = history.len().saturating_sub(MAX_RETAINED_VM_REVISIONS);
        if excess != 0 {
            history.drain(..excess);
        }
        Ok(revision)
    }

    pub fn revision_history(&self) -> Result<Vec<VmRevisionSnapshot>> {
        Ok(self
            .revision_history
            .lock()
            .map_err(|_| anyhow::anyhow!("revision history lock poisoned"))?
            .clone())
    }

    pub fn archive(&self) -> Result<ProgramRuntimeArchive> {
        let revisions = self.revision_history()?;
        let current_revision = self.revision();
        let current = revisions
            .last()
            .ok_or_else(|| anyhow::anyhow!("ProgramRuntime has no revision history"))?;
        if current.revision != current_revision {
            bail!(
                "ProgramRuntime revision history ends at {}, current revision is {}",
                current.revision,
                current_revision
            );
        }
        if current.checkpoint.is_none() {
            bail!(
                "ProgramRuntime revision {current_revision} is not archivable: {}",
                current
                    .checkpoint_diagnostic
                    .as_deref()
                    .unwrap_or("host-owned state is still live")
            );
        }
        Ok(ProgramRuntimeArchive {
            format_version: PROGRAM_RUNTIME_ARCHIVE_VERSION,
            base_revision: revisions
                .first()
                .expect("revision history was checked as non-empty")
                .revision,
            current_revision,
            revisions,
        })
    }

    pub async fn inspect(&self) -> Result<VmStateSnapshot> {
        let typed = Arc::clone(&self.typed);
        let revision = Arc::clone(&self.revision);
        let manifest_generation = self.manifest_generation();
        let host_vocabulary = self
            .host_vocabulary
            .read()
            .map_err(|_| anyhow::anyhow!("host vocabulary lock poisoned"))?
            .clone();
        tokio::task::spawn_blocking(move || {
            let revision = revision.load(Ordering::Acquire);
            let typed = typed
                .lock()
                .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?;
            let typed_stack: Vec<_> = typed
                .stack()
                .iter()
                .cloned()
                .enumerate()
                .map(|(index_from_bottom, value)| TypedVmStackCell {
                    index_from_bottom,
                    value_type: value.value_type(),
                    value,
                })
                .collect();
            let typed_vocabulary: Vec<VmVocabularyEntry> = typed
                .vocabulary()
                .iter()
                .map(|(name, signature)| VmVocabularyEntry {
                    name: name.clone(),
                    signature: Some(signature.to_string()),
                    documentation: host_vocabulary
                        .get(name)
                        .map(|metadata| metadata.documentation.clone())
                        .or_else(|| {
                            typed
                                .functions()
                                .get(name)
                                .and_then(|function| function.documentation.clone())
                        }),
                    version: host_vocabulary
                        .get(name)
                        .map(|metadata| metadata.version.clone()),
                })
                .collect();
            // `stack` and `vocabulary` are retained for compatibility with
            // older callers, but now project the same typed VM state as their
            // explicit counterparts. The legacy interpreter can no longer
            // shadow provider-visible state.
            let stack = typed_stack
                .iter()
                .filter_map(|cell| {
                    typed_value(cell.value.clone())
                        .ok()
                        .map(|value| VmStackCell {
                            index_from_bottom: cell.index_from_bottom,
                            type_name: cell.value_type.to_string(),
                            value,
                        })
                })
                .collect();
            let vocabulary = typed_vocabulary.clone();
            let granted_capabilities = typed.grants().0.iter().cloned().collect();
            Ok(VmStateSnapshot {
                manifest_generation,
                revision,
                stack,
                vocabulary,
                typed_stack,
                typed_vocabulary,
                granted_capabilities,
            })
        })
        .await?
    }

    pub async fn submit(&self, submission: ProgramSubmission) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only(submission, None).await
    }

    /// Execute source through the shared typed runtime only. This is the entry
    /// point for executable Finch scripts: an unsupported typed construct is a
    /// diagnostic, never permission to silently run a legacy evaluator.
    pub async fn submit_typed_only(
        &self,
        submission: ProgramSubmission,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            None,
            DeferredHostEffects::None,
            None,
            None,
        )
        .await
    }

    /// Internal scheduled-callback entry point. The persisted ceiling is
    /// authored only by `schedule-create`; callers outside this module cannot
    /// manufacture an authority-bearing `ProgramSubmission` field.
    pub(crate) async fn submit_typed_only_with_grant_ceiling(
        &self,
        submission: ProgramSubmission,
        grant_ceiling: EffectSet,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            None,
            DeferredHostEffects::None,
            Some(grant_ceiling),
            None,
        )
        .await
    }

    /// Run one named-Brain program with schedule effects delegated to the
    /// attached Brain service. An optional ceiling is the persisted authority
    /// of an unattended scheduled run; interactive runs pass `None` and
    /// capture their live grants only when a schedule is actually created.
    pub(crate) async fn submit_typed_only_with_deferred_schedule_effects(
        &self,
        submission: ProgramSubmission,
        effect_sink: TypedEffectSink,
        grant_ceiling: Option<EffectSet>,
        effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            Some(effect_sink),
            DeferredHostEffects::Schedules,
            grant_ceiling,
            effect_audit,
        )
        .await
    }

    /// Typed-only variant for a child/agent caller. Provider-facing protocol
    /// submissions use this entry point so a source form unsupported by the
    /// shared VM is reported as such instead of reaching a legacy evaluator.
    pub async fn submit_as_typed_only(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            caller,
            None,
            DeferredHostEffects::None,
            None,
            None,
        )
        .await
    }

    /// Typed-only variant retaining the per-ProgramRun presentation binding.
    /// This is the provider wire-protocol entry point.
    pub async fn submit_as_typed_only_with_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            caller,
            Some(effect_sink),
            DeferredHostEffects::None,
            None,
            None,
        )
        .await
    }

    /// Submit one ProgramRun with a presentation binding owned by the caller.
    /// The sink receives portable events for this run only; if the run yields,
    /// the binding travels with its saved continuation rather than becoming a
    /// mutable global "current WorkUnit".
    pub async fn submit_with_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only_with_typed_effect_sink(submission, None, effect_sink)
            .await
    }

    /// Submit with an event-loop binding that explicitly owns proposal
    /// editing. Approved `proposal-open` calls suspend as portable effects;
    /// the caller later resumes the exact sequence with accepted/chat/cancel
    /// data. An ordinary presentation sink does not imply this behavior.
    pub async fn submit_with_deferred_program_effects(
        &self,
        submission: ProgramSubmission,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            Some(effect_sink),
            DeferredHostEffects::ProgramInvocations,
            None,
            None,
        )
        .await
    }

    /// Provider-native `submit_program` entry point. A named-Brain turn must
    /// carry its daemon-issued audit capability through every tool round; the
    /// tool cannot reconstruct that authority from Brain/run provenance.
    pub(crate) async fn submit_tool_program(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
        effect_sink: Option<TypedEffectSink>,
        defer_program_effects: bool,
        effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    ) -> Result<ExecutionOutcome> {
        let deferred_host_effects = if defer_program_effects && caller.is_none() {
            DeferredHostEffects::ProgramInvocations
        } else {
            DeferredHostEffects::None
        };
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            caller,
            effect_sink,
            deferred_host_effects,
            None,
            effect_audit,
        )
        .await
    }

    /// Submit with a portable host boundary for every awaited capability.
    /// The caller receives each request through `effect_sink` and resumes the
    /// exact `(execution_id, sequence)` later with [`VmResume`]. This is for
    /// embedders which own filesystem, process, network, or UI operations;
    /// ordinary Finch submissions should use the compatibility host bindings.
    pub async fn submit_with_deferred_host_effects(
        &self,
        submission: ProgramSubmission,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            Some(effect_sink),
            DeferredHostEffects::AllAwaited,
            None,
            None,
        )
        .await
    }

    /// Equivalent to [`Self::submit_with_typed_effect_sink`] for a child agent
    /// whose ancestry must be preserved by host capability bindings.
    pub async fn submit_as_with_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only_with_typed_effect_sink(submission, caller, effect_sink)
            .await
    }

    pub async fn submit_as(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only(submission, caller).await
    }

    async fn submit_as_with_optional_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<agents::AgentIdentity>,
        effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
        grant_ceiling: Option<EffectSet>,
        effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    ) -> Result<ExecutionOutcome> {
        let effect_sink = self.compose_typed_effect_sink(effect_sink);
        // This is a per-session state transaction, not a process-wide
        // interpreter lock. Independent runtimes and child model loops remain
        // concurrent while revision checks and mutations of this VM are atomic.
        let (generation, input_revision, mut working_runtime) = {
            // The gate protects only the snapshot/revision handshake and the
            // eventual optimistic commit. Program execution owns this cloned
            // state privately and therefore does not serialize unrelated
            // ProgramRuns behind a persistent runtime mutex.
            let _submission = self.submission_gate.lock().await;
            self.refresh_active_grants()?;
            let generation = self.manifest_generation();
            if submission.manifest_generation != generation {
                bail!(
                    "stale VM manifest generation {}; current generation is {}",
                    submission.manifest_generation,
                    generation
                );
            }
            let input_revision = self.revision();
            if let Some(expected) = submission.expected_revision {
                if expected != input_revision {
                    bail!(
                        "stale VM revision {}; current revision is {}",
                        expected,
                        input_revision
                    );
                }
            }
            let working_runtime = self
                .typed
                .lock()
                .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
                .clone();
            (generation, input_revision, working_runtime)
        };
        if let Some(grant_ceiling) = &grant_ceiling {
            working_runtime.set_grants(grant_ceiling.clone());
        } else {
            working_runtime.set_grants(self.effective_grants_for(caller.as_ref())?);
        }
        let context = ExecutionContext::new(generation, submission.budget.unwrap_or_default());
        let source_id = submission
            .source_id
            .clone()
            .unwrap_or_else(|| match submission.language {
                ProgramLanguage::Forth => "provider-response.forth".to_string(),
                ProgramLanguage::Lisp => "provider-response.lisp".to_string(),
            });
        let started = Instant::now();
        let (working_runtime, execution) = self
            .execute_typed_program(
                working_runtime,
                submission.language,
                &source_id,
                &submission.source,
                &submission.intent,
                &context,
                &submission.declared_capabilities,
                caller.clone(),
                effect_sink.clone(),
                deferred_host_effects,
                effect_audit.clone(),
            )
            .await?;
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let suspension = execution.suspension.clone();
        if let Some(suspension) = suspension.clone() {
            if let Some(cancelled) = self.retain_pending_typed_execution(
                context.execution_id,
                PendingTypedExecution {
                    working_runtime: working_runtime.clone(),
                    suspension,
                    context: context.clone(),
                    input_revision,
                    language: submission.language,
                    source: submission.source.clone(),
                    intent: submission.intent.clone(),
                    effect: submission.effect,
                    caller: caller.clone(),
                    output: execution.output.clone(),
                    output_chunks: execution.output_chunks.clone(),
                    side_effects: execution.side_effects.clone(),
                    effect_sink,
                    deferred_host_effects,
                    effect_audit,
                    grant_ceiling: grant_ceiling.clone(),
                },
            )? {
                return Ok(cancelled);
            }
        }
        if suspension.is_none() {
            self.release_output_handles(context.execution_id)?;
        }
        let completion_commit = if matches!(execution.status, TypedExecutionStatus::Completed) {
            Some(
                self.commit_working_runtime(input_revision, working_runtime)
                    .await,
            )
        } else {
            None
        };
        let inferred_capabilities = execution.effects.0.iter().cloned().collect::<Vec<_>>();
        Ok(match execution.status {
            TypedExecutionStatus::Completed => {
                let output_revision = match completion_commit.expect("completed run has commit") {
                    Ok(revision) => revision,
                    Err(error) => {
                        return Ok(ExecutionOutcome {
                            execution_id: context.execution_id,
                            status: ExecutionStatus::Failed,
                            values: Vec::new(),
                            output: truncate_output(
                                execution.output,
                                context.budget.max_output_bytes,
                            ),
                            output_chunks: execution.output_chunks,
                            side_effects: execution.side_effects,
                            vm_side_effects: execution.vm_side_effects,
                            effect_journal: execution.effect_journal,
                            diagnostics: vec![error.to_string()],
                            vm_diagnostics: Vec::new(),
                            inferred_capabilities: inferred_capabilities.clone(),
                            required_capabilities: Vec::new(),
                            approval_prompts: Vec::new(),
                            input_revision,
                            output_revision: self.revision(),
                            effect: submission.effect,
                            backend: ExecutionBackend::TypedVm,
                            elapsed_ms,
                        });
                    }
                };
                ExecutionOutcome {
                    execution_id: context.execution_id,
                    status: ExecutionStatus::Completed,
                    values: typed_values(execution.values)?,
                    output: truncate_output(execution.output, context.budget.max_output_bytes),
                    output_chunks: execution.output_chunks,
                    side_effects: execution.side_effects,
                    vm_side_effects: execution.vm_side_effects,
                    effect_journal: execution.effect_journal,
                    diagnostics: Vec::new(),
                    vm_diagnostics: Vec::new(),
                    inferred_capabilities: inferred_capabilities.clone(),
                    required_capabilities: Vec::new(),
                    approval_prompts: Vec::new(),
                    input_revision,
                    output_revision,
                    effect: submission.effect,
                    backend: ExecutionBackend::TypedVm,
                    elapsed_ms,
                }
            }
            TypedExecutionStatus::Suspended => ExecutionOutcome {
                execution_id: context.execution_id,
                status: ExecutionStatus::Suspended,
                values: Vec::new(),
                output: truncate_output(execution.output, context.budget.max_output_bytes),
                output_chunks: execution.output_chunks,
                side_effects: execution.side_effects,
                vm_side_effects: execution.vm_side_effects,
                effect_journal: execution.effect_journal,
                diagnostics: Vec::new(),
                vm_diagnostics: execution.diagnostics,
                inferred_capabilities: inferred_capabilities.clone(),
                required_capabilities: Vec::new(),
                approval_prompts: Vec::new(),
                input_revision,
                output_revision: input_revision,
                effect: submission.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
            TypedExecutionStatus::AuthorizationRequired { requirements } => ExecutionOutcome {
                execution_id: context.execution_id,
                status: ExecutionStatus::AuthorizationRequired,
                values: Vec::new(),
                output: truncate_output(execution.output, context.budget.max_output_bytes),
                output_chunks: execution.output_chunks,
                side_effects: execution.side_effects,
                vm_side_effects: execution.vm_side_effects,
                effect_journal: execution.effect_journal,
                diagnostics: Vec::new(),
                vm_diagnostics: execution.diagnostics,
                approval_prompts: approval_prompts(
                    context.execution_id,
                    &requirements,
                    &submission.source,
                    &submission.intent,
                    suspension.as_ref(),
                    caller.as_ref(),
                ),
                inferred_capabilities: inferred_capabilities.clone(),
                required_capabilities: requirements,
                input_revision,
                output_revision: input_revision,
                effect: submission.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
            TypedExecutionStatus::Failed => ExecutionOutcome {
                execution_id: context.execution_id,
                status: ExecutionStatus::Failed,
                values: Vec::new(),
                output: truncate_output(execution.output, context.budget.max_output_bytes),
                output_chunks: execution.output_chunks,
                side_effects: execution.side_effects,
                vm_side_effects: execution.vm_side_effects,
                effect_journal: execution.effect_journal,
                // Render rather than `to_string`: the span, the expected and found types, and the
                // hints are what let a reader — or a model asked to repair this — find the part of
                // the program that is wrong. `Display` drops all of it.
                diagnostics: execution
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.render(Some(&submission.source)))
                    .collect(),
                vm_diagnostics: execution.diagnostics,
                inferred_capabilities,
                required_capabilities: Vec::new(),
                approval_prompts: Vec::new(),
                input_revision,
                output_revision: input_revision,
                effect: submission.effect,
                backend: ExecutionBackend::TypedVm,
                elapsed_ms,
            },
        })
    }

    async fn execute_typed_program(
        &self,
        mut runtime: TypedRuntime,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        intent: &str,
        context: &ExecutionContext,
        declared_capabilities: &[CapabilityRequirement],
        caller: Option<agents::AgentIdentity>,
        typed_effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
        effect_audit: Option<crate::runtime::effect_audit::RunnerEffectAuditControl>,
    ) -> Result<(TypedRuntime, crate::vm::TypedExecution)> {
        let automation = Arc::clone(&self.automation);
        let resource_roots = Arc::clone(&self.resource_roots);
        let memory = self
            .memory
            .read()
            .expect("memory binding lock poisoned")
            .clone();
        let mcp_client = self
            .mcp_client
            .read()
            .expect("MCP client binding lock poisoned")
            .clone();
        let mcp_output_schemas = self
            .host_vocabulary
            .read()
            .expect("host vocabulary lock poisoned")
            .iter()
            .filter_map(|(name, metadata)| {
                metadata
                    .output_schema
                    .clone()
                    .map(|schema| (name.clone(), schema))
            })
            .collect();
        let network = Arc::clone(&self.network);
        let output_handles = Arc::clone(&self.output_handles);
        let streams = Arc::clone(&self.streams);
        let scheduler = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, caller.clone()));
        let source = source.to_string();
        let source_id = source_id.to_string();
        let declared = (!declared_capabilities.is_empty())
            .then(|| EffectSet(declared_capabilities.iter().cloned().collect()));
        let fuel = context.budget.forth_fuel.min(u64::MAX as usize) as u64;
        let execution_id = context.execution_id;
        let resource_generation = context.manifest_generation;
        let authorization = HostAuthorizationAudit {
            ledger: Arc::clone(&self.capability_ledger),
            policy: Arc::clone(&self.capability_policy),
            use_gate: Arc::clone(&self.authority_use_gate),
            sink: self
                .authority_sink
                .read()
                .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
                .clone(),
            context: self.authorization_context_for(caller.as_ref())?,
            reason: intent.to_string(),
            program_hash: hash_program_source(&source),
            agent_ancestry: agent_ancestry(caller.as_ref()),
        };
        // `spawn_blocking` is load-bearing, not just a courtesy to the
        // scheduler: it moves this off a runtime worker, which is what lets
        // `block_on_host` block its thread waiting for a future that needs the
        // runtime. Drive this inline and `mem-store` deadlocks on a
        // single-worker runtime.
        let (runtime, execution) = tokio::task::spawn_blocking(move || {
            let vocabulary =
                serde_json::to_string(runtime.vocabulary()).unwrap_or_else(|_| "[]".to_string());
            // A host binding being installed is availability, not
            // authority.  The runtime's existing grants are the only
            // source of authority for automation and child agents.
            let grants = runtime.grants().clone();
            let mut handler = TypedHostHandler::new(
                Arc::clone(&automation),
                Arc::clone(&resource_roots),
                scheduler,
                memory,
                mcp_client,
                mcp_output_schemas,
                vocabulary,
                network,
                output_handles,
                streams,
                execution_id,
                resource_generation,
                authorization,
                grants,
                typed_effect_sink,
                deferred_host_effects,
                effect_audit,
            );
            let initial_types = runtime
                .stack()
                .iter()
                .map(crate::vm::TypedValue::value_type)
                .collect();
            let compiled = crate::language::compile_with_functions(
                language,
                &source_id,
                &source,
                initial_types,
                runtime.vocabulary(),
                runtime.functions(),
            );
            let execution = match compiled {
                Ok(module) => {
                    runtime.execute_with_handler(&module, fuel, declared.as_ref(), &mut handler)
                }
                Err(diagnostics) => crate::vm::TypedExecution::failed(diagnostics),
            };
            (runtime, execution)
        })
        .await?;
        Ok((runtime, execution))
    }

    async fn resume_typed_program(
        &self,
        mut runtime: TypedRuntime,
        pending: &PendingTypedExecution,
        external_effect_result: Option<(u64, Vec<TypedValue>)>,
        authorize_pending_host_call: bool,
    ) -> Result<(TypedRuntime, crate::vm::TypedExecution)> {
        let automation = Arc::clone(&self.automation);
        let resource_roots = Arc::clone(&self.resource_roots);
        let memory = self
            .memory
            .read()
            .expect("memory binding lock poisoned")
            .clone();
        let mcp_client = self
            .mcp_client
            .read()
            .expect("MCP client binding lock poisoned")
            .clone();
        let mcp_output_schemas = self
            .host_vocabulary
            .read()
            .expect("host vocabulary lock poisoned")
            .iter()
            .filter_map(|(name, metadata)| {
                metadata
                    .output_schema
                    .clone()
                    .map(|schema| (name.clone(), schema))
            })
            .collect();
        let network = Arc::clone(&self.network);
        let output_handles = Arc::clone(&self.output_handles);
        let streams = Arc::clone(&self.streams);
        let typed_effect_sink = pending.effect_sink.clone();
        let deferred_host_effects = pending.deferred_host_effects;
        let effect_audit = pending.effect_audit.clone();
        let scheduler = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, pending.caller.clone()));
        let suspension = pending.suspension.clone();
        let execution_id = pending.context.execution_id;
        let resource_generation = pending.context.manifest_generation;
        let authorization = HostAuthorizationAudit {
            ledger: Arc::clone(&self.capability_ledger),
            policy: Arc::clone(&self.capability_policy),
            use_gate: Arc::clone(&self.authority_use_gate),
            sink: self
                .authority_sink
                .read()
                .map_err(|_| anyhow::anyhow!("authority sink lock poisoned"))?
                .clone(),
            context: self.authorization_context_for(pending.caller.as_ref())?,
            reason: pending.intent.clone(),
            program_hash: hash_program_source(&pending.source),
            agent_ancestry: agent_ancestry(pending.caller.as_ref()),
        };
        // `spawn_blocking` is load-bearing, not just a courtesy to the
        // scheduler: it moves this off a runtime worker, which is what lets
        // `block_on_host` block its thread waiting for a future that needs the
        // runtime. Drive this inline and `mem-store` deadlocks on a
        // single-worker runtime.
        let (runtime, execution) = tokio::task::spawn_blocking(move || {
            let vocabulary =
                serde_json::to_string(runtime.vocabulary()).unwrap_or_else(|_| "[]".to_string());
            // Resumption has the same authority boundary as initial
            // execution: bindings make effects possible, never
            // implicitly granted.
            let grants = runtime.grants().clone();
            let mut handler = TypedHostHandler::new(
                Arc::clone(&automation),
                Arc::clone(&resource_roots),
                scheduler,
                memory,
                mcp_client,
                mcp_output_schemas,
                vocabulary,
                network,
                output_handles,
                streams,
                execution_id,
                resource_generation,
                authorization,
                grants,
                typed_effect_sink,
                deferred_host_effects,
                effect_audit,
            );
            let execution = match external_effect_result {
                Some((effect_sequence, values)) => runtime.resume_with_effect_result(
                    suspension,
                    effect_sequence,
                    values,
                    &mut handler,
                ),
                None if authorize_pending_host_call => {
                    runtime.resume_authorized_host_call_with_handler(suspension, &mut handler)
                }
                None => runtime.resume_with_handler(suspension, Vec::new(), &mut handler),
            };
            (runtime, execution)
        })
        .await?;
        Ok((runtime, execution))
    }
}

impl Default for ProgramRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn attenuate_effects(current: &EffectSet, ceiling: &EffectSet) -> EffectSet {
    let mut attenuated = TypedRuntime::intrinsic_grants();
    for requirement in &current.0 {
        if ceiling.grants(&EffectSet::from_requirement(requirement.clone())) {
            attenuated = attenuated.union(&EffectSet::from_requirement(requirement.clone()));
        }
    }
    for requirement in &ceiling.0 {
        if current.grants(&EffectSet::from_requirement(requirement.clone())) {
            attenuated = attenuated.union(&EffectSet::from_requirement(requirement.clone()));
        }
    }
    attenuated
}

fn truncate_output(mut output: String, max_bytes: usize) -> String {
    if output.len() <= max_bytes {
        return output;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !output.is_char_boundary(boundary) {
        boundary -= 1;
    }
    output.truncate(boundary);
    output.push_str("\n[output truncated]");
    output
}

/// Convert a discarded private continuation into an ordinary VM result. A
/// stale revision is a normal optimistic-concurrency outcome, not a host API
/// failure: callers need its emitted output and effect journal to explain the
/// conflict and must never be tempted to replay those effects.
fn failed_pending_resume(
    execution_id: uuid::Uuid,
    pending: &PendingTypedExecution,
    output_revision: u64,
    diagnostic: String,
    elapsed: std::time::Duration,
) -> ExecutionOutcome {
    ExecutionOutcome {
        execution_id,
        status: ExecutionStatus::Failed,
        values: Vec::new(),
        output: truncate_output(
            pending.output.clone(),
            pending.context.budget.max_output_bytes,
        ),
        output_chunks: pending.output_chunks.clone(),
        side_effects: pending.side_effects.clone(),
        vm_side_effects: pending.suspension.event_journal.clone(),
        effect_journal: pending.suspension.effect_journal.clone(),
        diagnostics: vec![diagnostic],
        vm_diagnostics: Vec::new(),
        inferred_capabilities: pending.suspension.effects.0.iter().cloned().collect(),
        required_capabilities: Vec::new(),
        approval_prompts: Vec::new(),
        input_revision: pending.input_revision,
        output_revision,
        effect: pending.effect,
        backend: ExecutionBackend::TypedVm,
        elapsed_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
    }
}

fn approval_prompts(
    execution_id: uuid::Uuid,
    requirements: &[CapabilityRequirement],
    source: &str,
    intent: &str,
    suspension: Option<&TypedSuspension>,
    caller: Option<&agents::AgentIdentity>,
) -> Vec<ApprovalPrompt> {
    let program_hash = hash_program_source(source);
    let agent_ancestry = agent_ancestry(caller);
    if let Some(call) = suspension.and_then(|suspension| suspension.pending_host_call.as_ref()) {
        let effect_sequence = suspension
            .and_then(|suspension| suspension.event_journal.last())
            .map(|effect| effect.sequence);
        let request_key = effect_sequence.map_or_else(
            || {
                format!(
                    "runtime:{}",
                    serde_json::to_string(&call.requirement)
                        .expect("capability requirements are serializable")
                )
            },
            |sequence| format!("effect:{sequence}"),
        );
        return vec![ApprovalPrompt::for_request(CapabilityRequest {
            id: uuid::Uuid::new_v5(&execution_id, request_key.as_bytes()),
            execution_id,
            effect_sequence,
            reason: intent.to_string(),
            requirement: call.requirement.clone(),
            arguments: call.arguments.clone(),
            origin: call.origin.clone(),
            agent_ancestry,
            program_hash,
        })];
    }
    requirements
        .iter()
        .enumerate()
        .map(|(index, requirement)| {
            let request_key = format!(
                "preflight:{index}:{}",
                serde_json::to_string(requirement)
                    .expect("capability requirements are serializable")
            );
            ApprovalPrompt::for_request(CapabilityRequest {
                id: uuid::Uuid::new_v5(&execution_id, request_key.as_bytes()),
                execution_id,
                effect_sequence: None,
                reason: intent.to_string(),
                requirement: requirement.clone(),
                arguments: Vec::new(),
                origin: SourceOrigin::generated("capability-preflight"),
                agent_ancestry: agent_ancestry.clone(),
                program_hash: program_hash.clone(),
            })
        })
        .collect()
}

fn hash_program_source(source: &str) -> String {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn agent_ancestry(caller: Option<&agents::AgentIdentity>) -> Vec<uuid::Uuid> {
    caller.map_or_else(Vec::new, |caller| {
        let mut ancestry = vec![caller.root_agent_id];
        if let Some(parent) = caller.parent_agent_id {
            if !ancestry.contains(&parent) {
                ancestry.push(parent);
            }
        }
        if !ancestry.contains(&caller.agent_id) {
            ancestry.push(caller.agent_id);
        }
        ancestry
    })
}

fn typed_values(values: Vec<TypedValue>) -> Result<Vec<ProgramValue>> {
    values.into_iter().map(typed_value).collect()
}

fn typed_value(value: TypedValue) -> Result<ProgramValue> {
    Ok(match value {
        TypedValue::Unit => ProgramValue::Nil,
        TypedValue::Bool(value) => ProgramValue::Bool(value),
        TypedValue::Int(value) => ProgramValue::Int(value),
        TypedValue::Float(value) => ProgramValue::Float(value),
        TypedValue::Symbol(value) => ProgramValue::Symbol(value),
        TypedValue::String(value) => ProgramValue::String(value),
        TypedValue::Bytes(value) => ProgramValue::Bytes(value),
        TypedValue::Json(value) => ProgramValue::Json(value),
        TypedValue::List { values, .. } => ProgramValue::List(typed_values(values)?),
        TypedValue::Map { entries, .. } => ProgramValue::Map(
            entries
                .into_iter()
                .map(|(key, value)| Ok((typed_value(key)?, typed_value(value)?)))
                .collect::<Result<Vec<_>>>()?,
        ),
        TypedValue::Option { value, .. } => ProgramValue::Option(
            value
                .map(|value| typed_value(*value))
                .transpose()?
                .map(Box::new),
        ),
        TypedValue::Result { is_ok, value, .. } => ProgramValue::Result {
            ok: is_ok,
            value: Box::new(typed_value(*value)?),
        },
        TypedValue::Record(fields) => ProgramValue::Record(
            fields
                .into_iter()
                .map(|(name, value)| Ok((name, typed_value(value)?)))
                .collect::<Result<Vec<_>>>()?,
        ),
        TypedValue::Variant { name, value } => ProgramValue::Variant {
            name,
            value: value
                .map(|value| typed_value(*value))
                .transpose()?
                .map(Box::new),
        },
        TypedValue::Task { id, .. } => ProgramValue::Task(id),
        TypedValue::Fiber {
            id,
            yield_type,
            result_type,
        } => ProgramValue::Fiber {
            id,
            yield_type,
            result_type,
        },
        TypedValue::Stream {
            id,
            kind,
            generation,
            ..
        } => ProgramValue::Resource {
            kind: format!("stream:{kind}"),
            handle: id,
            generation,
        },
        TypedValue::Resource {
            kind,
            handle,
            generation,
        } => ProgramValue::Resource {
            kind,
            handle,
            generation,
        },
        other => bail!("typed VM value is not portable: {other:?}"),
    })
}

#[cfg(test)]
mod tests;
