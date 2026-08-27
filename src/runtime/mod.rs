//! Provider-neutral execution service for Finch's Forth and Lisp VMs.

pub mod agent_vm;
pub mod archive_store;
pub mod automation;
pub mod context;
pub mod effect_log;
pub mod fiber;
mod mcp;
pub mod outcome;
pub mod scheduler;

use crate::programs::{ExecutionEffect, ProgramLanguage, ProgramValue};
use crate::vm::vocabulary::{
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
use automation::AutomationBroker;
use automation::AutomationRequest;
use context::{ExecutionBudget, ExecutionContext};
use outcome::{ExecutionBackend, ExecutionOutcome, ExecutionStatus};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
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

/// A portable VM event attached to its owning ProgramRun. The VM event itself
/// remains embedder-neutral; the envelope provides the other half of its
/// idempotency key to a host/UI callback.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmEffectEnvelope {
    pub execution_id: uuid::Uuid,
    pub effect: VmSideEffect,
}

/// Stable identity for one journaled VM effect. It is usable as a proposal
/// handle while a `program.invoke` request awaits an editor/IDE result, and
/// is equally valid for every other portable host effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VmEffectHandle {
    pub execution_id: uuid::Uuid,
    pub sequence: u64,
}

impl VmEffectEnvelope {
    pub fn handle(&self) -> VmEffectHandle {
        VmEffectHandle {
            execution_id: self.execution_id,
            sequence: self.effect.sequence,
        }
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

/// Reducible language context needed to compile a captured provider program
/// without retaining the live operand stack or any host authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramCompilerContext {
    pub manifest_generation: u64,
    pub revision: u64,
    pub functions: std::collections::BTreeMap<String, crate::vm::ir::Function>,
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
    mcp_client: RwLock<Option<Arc<crate::tools::mcp::McpClient>>>,
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
    agent_scheduler: RwLock<Weak<scheduler::AgentScheduler>>,
    /// Daemon-owned typed continuations keyed by the execution id visible in
    /// the UI. Approval and resumption use this exact verified program state.
    pending_typed: Mutex<HashMap<uuid::Uuid, PendingTypedExecution>>,
    revision_history: Mutex<Vec<VmRevisionSnapshot>>,
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
    caller: Option<scheduler::AgentIdentity>,
    output: String,
    output_chunks: Vec<String>,
    side_effects: Vec<crate::vm::interpreter::HostSideEffect>,
    effect_sink: Option<TypedEffectSink>,
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
        let automation = Arc::new(AutomationBroker::new(enabled));
        let typed_runtime = TypedRuntime::new();
        let checkpoint = typed_runtime
            .checkpoint()
            .expect("a fresh typed runtime is checkpointable");
        let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
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
            agent_scheduler: RwLock::new(Weak::new()),
            pending_typed: Mutex::new(HashMap::new()),
            revision_history: Mutex::new(vec![VmRevisionSnapshot {
                revision: 0,
                stack: Vec::new(),
                vocabulary: crate::vm::core_vocabulary().into_keys().collect(),
                checkpoint: Some(checkpoint),
                checkpoint_diagnostic: None,
            }]),
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
        client: Arc<crate::tools::mcp::McpClient>,
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
        caller: Option<&scheduler::AgentIdentity>,
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
        caller: Option<&scheduler::AgentIdentity>,
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
        caller: Option<&scheduler::AgentIdentity>,
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
                diagnostics: execution
                    .diagnostics
                    .iter()
                    .map(ToString::to_string)
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

    pub fn attach_agent_scheduler(&self, scheduler: &Arc<scheduler::AgentScheduler>) {
        *self
            .agent_scheduler
            .write()
            .expect("agent scheduler lock poisoned") = Arc::downgrade(scheduler);
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
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            None,
            Some(effect_sink),
            DeferredHostEffects::Schedules,
            grant_ceiling,
        )
        .await
    }

    /// Typed-only variant for a child/agent caller. Provider-facing protocol
    /// submissions use this entry point so a source form unsupported by the
    /// shared VM is reported as such instead of reaching a legacy evaluator.
    pub async fn submit_as_typed_only(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            caller,
            None,
            DeferredHostEffects::None,
            None,
        )
        .await
    }

    /// Typed-only variant retaining the per-ProgramRun presentation binding.
    /// This is the provider wire-protocol entry point.
    pub async fn submit_as_typed_only_with_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_with_optional_typed_effect_sink(
            submission,
            caller,
            Some(effect_sink),
            DeferredHostEffects::None,
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
        )
        .await
    }

    /// Equivalent to [`Self::submit_with_typed_effect_sink`] for a child agent
    /// whose ancestry must be preserved by host capability bindings.
    pub async fn submit_as_with_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
        effect_sink: TypedEffectSink,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only_with_typed_effect_sink(submission, caller, effect_sink)
            .await
    }

    pub async fn submit_as(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
    ) -> Result<ExecutionOutcome> {
        self.submit_as_typed_only(submission, caller).await
    }

    async fn submit_as_with_optional_typed_effect_sink(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
        effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
        grant_ceiling: Option<EffectSet>,
    ) -> Result<ExecutionOutcome> {
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
                diagnostics: execution
                    .diagnostics
                    .iter()
                    .map(ToString::to_string)
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
        caller: Option<scheduler::AgentIdentity>,
        typed_effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
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
            );
            let execution = runtime.execute_with_handler(
                language,
                &source_id,
                &source,
                fuel,
                declared.as_ref(),
                &mut handler,
            );
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

struct TypedHostHandler {
    automation: Arc<AutomationBroker>,
    resource_roots: Arc<RwLock<ResourceRootState>>,
    output: String,
    output_chunks: Vec<String>,
    side_effects: Vec<crate::vm::interpreter::HostSideEffect>,
    scheduler: Option<agent_vm::AgentVmBinding>,
    memory: Option<Arc<crate::memory::MemorySystem>>,
    mcp_client: Option<Arc<crate::tools::mcp::McpClient>>,
    mcp_output_schemas: BTreeMap<String, serde_json::Value>,
    vocabulary: String,
    network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
    output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
    streams: Arc<Mutex<HashMap<String, HostStream>>>,
    execution_id: uuid::Uuid,
    resource_generation: u64,
    authorization: HostAuthorizationAudit,
    network_grants: EffectSet,
    typed_effect_sink: Option<TypedEffectSink>,
    deferred_host_effects: DeferredHostEffects,
    authorization_attempt: Option<HostAuthorizationAttempt>,
}

struct HostAuthorizationAttempt {
    request: CapabilityRequest,
    decision: AuthorizationDecision,
    use_started: bool,
}

struct HostAuthorizationAudit {
    ledger: Arc<Mutex<CapabilityLedger>>,
    policy: Arc<RwLock<CapabilityPolicy>>,
    use_gate: Arc<RwLock<()>>,
    sink: Option<ProgramRuntimeAuthoritySink>,
    context: AuthorizationContext,
    reason: String,
    program_hash: String,
    agent_ancestry: Vec<uuid::Uuid>,
}

impl TypedHostHandler {
    fn new(
        automation: Arc<AutomationBroker>,
        resource_roots: Arc<RwLock<ResourceRootState>>,
        scheduler: Option<agent_vm::AgentVmBinding>,
        memory: Option<Arc<crate::memory::MemorySystem>>,
        mcp_client: Option<Arc<crate::tools::mcp::McpClient>>,
        mcp_output_schemas: BTreeMap<String, serde_json::Value>,
        vocabulary: String,
        network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
        output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
        streams: Arc<Mutex<HashMap<String, HostStream>>>,
        execution_id: uuid::Uuid,
        resource_generation: u64,
        authorization: HostAuthorizationAudit,
        network_grants: EffectSet,
        typed_effect_sink: Option<TypedEffectSink>,
        deferred_host_effects: DeferredHostEffects,
    ) -> Self {
        Self {
            automation,
            resource_roots,
            output: String::new(),
            output_chunks: Vec::new(),
            side_effects: Vec::new(),
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
            network_grants,
            typed_effect_sink,
            deferred_host_effects,
            authorization_attempt: None,
        }
    }

    fn mark_host_use(&mut self) {
        // "Use" is the first point at which the host has either acquired the
        // selected object for observation or is about to issue a potentially
        // mutating external call. Validation, lookup, and opening a read-only
        // object are attempts until they succeed; a write/connect/send/spawn
        // becomes a use immediately before the first syscall that may cause
        // an external effect. Failures before this point roll back the exact
        // authorization fact (and once consumption); failures after it retain
        // the audit because an effect may already have occurred.
        if let Some(attempt) = &mut self.authorization_attempt {
            attempt.use_started = true;
        }
    }

    /// Advance one host-owned stream. The stream's opaque kind/ID is checked
    /// here rather than inferred from a source string, and each backing cursor
    /// remains owned by the ProgramRun that opened it.
    fn stream_next(
        &mut self,
        arguments: &[TypedValue],
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let [TypedValue::Stream {
            id,
            element_type,
            kind,
            generation,
        }] = arguments
        else {
            return Err(host_binding_error(
                origin,
                "stream-next requires one stream<T>",
            ));
        };
        match kind.as_str() {
            "csv-records" if *element_type == Type::list(Type::String) => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams
                    .get_mut(id)
                    .ok_or_else(|| host_binding_error(origin, "CSV stream is unknown or closed"))?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "CSV stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::CsvRecords(reader) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "CSV stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                let record = read_bounded_csv_record(reader)
                    .map_err(|message| host_binding_error(origin, message))?;
                Ok(vec![TypedValue::Option {
                    inner_type: Type::list(Type::String),
                    value: record.map(|fields| {
                        Box::new(TypedValue::List {
                            element_type: Type::String,
                            values: fields.into_iter().map(TypedValue::String).collect(),
                        })
                    }),
                }])
            }
            "file-lines" if *element_type == Type::String => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams.get_mut(id).ok_or_else(|| {
                    host_binding_error(origin, "file line stream is unknown or closed")
                })?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "file line stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::FileLines(reader) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "file line stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                let line = read_bounded_utf8_line(reader)
                    .map_err(|message| host_binding_error(origin, message))?;
                Ok(vec![TypedValue::Option {
                    inner_type: Type::String,
                    value: line.map(|line| Box::new(TypedValue::String(line))),
                }])
            }
            "workbook-rows" if *element_type == Type::list(Type::String) => {
                let mut streams = self
                    .streams
                    .lock()
                    .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
                let stream = streams.get_mut(id).ok_or_else(|| {
                    host_binding_error(origin, "workbook stream is unknown or closed")
                })?;
                if stream.owner != self.execution_id || stream.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "workbook stream does not belong to this ProgramRun",
                    ));
                }
                let HostStreamBackend::WorkbookRows(rows) = &mut stream.backend else {
                    return Err(host_binding_error(
                        origin,
                        "workbook stream backend is malformed",
                    ));
                };
                if let Some(attempt) = &mut self.authorization_attempt {
                    attempt.use_started = true;
                }
                Ok(vec![TypedValue::Option {
                    inner_type: Type::list(Type::String),
                    value: rows.next().map(|row| {
                        Box::new(TypedValue::List {
                            element_type: Type::String,
                            values: row.into_iter().map(TypedValue::String).collect(),
                        })
                    }),
                }])
            }
            _ => Err(host_binding_error(
                origin,
                "stream-next received an unknown or malformed stream",
            )),
        }
    }

    /// Explicit stream cancellation/release. Closing twice fails rather than
    /// silently creating a new cursor or masking an ownership violation.
    fn stream_close(
        &mut self,
        arguments: &[TypedValue],
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let [TypedValue::Stream {
            id,
            element_type,
            kind,
            generation,
        }] = arguments
        else {
            return Err(host_binding_error(
                origin,
                "stream-close requires one stream<T>",
            ));
        };
        let expected_backend = match kind.as_str() {
            "csv-records" if *element_type == Type::list(Type::String) => "csv",
            "file-lines" if *element_type == Type::String => "lines",
            "workbook-rows" if *element_type == Type::list(Type::String) => "workbook",
            _ => {
                return Err(host_binding_error(
                    origin,
                    "stream-close received an unknown or malformed stream",
                ))
            }
        };
        let mut streams = self
            .streams
            .lock()
            .map_err(|_| host_binding_error(origin, "stream registry lock poisoned"))?;
        let stream = streams
            .get(id)
            .ok_or_else(|| host_binding_error(origin, "stream is unknown or closed"))?;
        if stream.owner != self.execution_id || stream.generation != *generation {
            return Err(host_binding_error(
                origin,
                "stream does not belong to this ProgramRun",
            ));
        }
        let backend_matches = matches!(
            (&stream.backend, expected_backend),
            (HostStreamBackend::CsvRecords(_), "csv")
                | (HostStreamBackend::FileLines(_), "lines")
                | (HostStreamBackend::WorkbookRows(_), "workbook")
        );
        if !backend_matches {
            return Err(host_binding_error(origin, "stream backend is malformed"));
        }
        if let Some(attempt) = &mut self.authorization_attempt {
            attempt.use_started = true;
        }
        streams.remove(id);
        Ok(vec![TypedValue::Unit])
    }
}

fn typed_agent_task_spec(
    value: &TypedValue,
    origin: &SourceOrigin,
) -> std::result::Result<scheduler::AgentTaskSpec, VmDiagnostic> {
    let TypedValue::Record(fields) = value else {
        return Err(host_binding_error(
            origin,
            "agent-spawn-with requires an agent task specification record",
        ));
    };
    if value.value_type() != agent_task_spec_type() {
        return Err(host_binding_error(
            origin,
            "agent task specification has the wrong fields or field types",
        ));
    }
    let field = |name: &str| {
        fields
            .iter()
            .find_map(|(field, value)| (field == name).then_some(value))
            .ok_or_else(|| {
                host_binding_error(
                    origin,
                    format!("agent task specification is missing '{name}'"),
                )
            })
    };
    let string = |name: &str| match field(name)? {
        TypedValue::String(value) => Ok(value.clone()),
        _ => Err(host_binding_error(
            origin,
            format!("agent task field '{name}' must be a string"),
        )),
    };
    let integer = |name: &str| match field(name)? {
        TypedValue::Int(value) => Ok(*value),
        _ => Err(host_binding_error(
            origin,
            format!("agent task field '{name}' must be an integer"),
        )),
    };
    let role = match string("role")?.as_str() {
        "general" => scheduler::AgentRole::General,
        "explore" => scheduler::AgentRole::Explore,
        "research" => scheduler::AgentRole::Research,
        "code" => scheduler::AgentRole::Code,
        role => {
            return Err(host_binding_error(
                origin,
                format!("unknown agent role '{role}'"),
            ))
        }
    };
    let optional = |value: String| (!value.trim().is_empty()).then_some(value);
    let max_turns = usize::try_from(integer("max-turns")?)
        .map_err(|_| host_binding_error(origin, "agent max-turns must be non-negative"))?;
    let timeout_ms = u64::try_from(integer("timeout-ms")?)
        .map_err(|_| host_binding_error(origin, "agent timeout-ms must be non-negative"))?;
    let max_output_bytes = usize::try_from(integer("max-output-bytes")?)
        .map_err(|_| host_binding_error(origin, "agent max-output-bytes must be non-negative"))?;
    let context = match field("context-refs")? {
        TypedValue::List { values, .. } => values
            .iter()
            .map(|value| {
                let TypedValue::Record(fields) = value else {
                    return Err(host_binding_error(
                        origin,
                        "agent context reference must be a record",
                    ));
                };
                let string_field = |name: &str| {
                    fields
                        .iter()
                        .find_map(|(field, value)| (field == name).then_some(value))
                        .and_then(|value| match value {
                            TypedValue::String(value) => Some(value.clone()),
                            _ => None,
                        })
                        .ok_or_else(|| {
                            host_binding_error(
                                origin,
                                format!("agent context reference '{name}' must be a string"),
                            )
                        })
                };
                Ok(scheduler::AgentContextReference {
                    kind: string_field("kind")?,
                    id: string_field("id")?,
                    sha256: string_field("sha256")?,
                })
            })
            .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?,
        _ => {
            return Err(host_binding_error(
                origin,
                "agent task field 'context-refs' must be a list of context-reference records",
            ))
        }
    };
    let capability_grant_ids = match field("capabilities")? {
        TypedValue::List { values, .. } => values
            .iter()
            .map(|value| {
                let TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                } = value
                else {
                    return Err(host_binding_error(
                        origin,
                        "agent capability selection requires capability-grant resources",
                    ));
                };
                if kind != "capability-grant" || *generation != 0 {
                    return Err(host_binding_error(
                        origin,
                        "agent capability selection contains an invalid grant resource",
                    ));
                }
                uuid::Uuid::parse_str(handle).map_err(|_| {
                    host_binding_error(origin, "capability-grant resource has an invalid handle")
                })
            })
            .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?,
        _ => {
            return Err(host_binding_error(
                origin,
                "agent task field 'capabilities' must be a list of capability-grant resources",
            ))
        }
    };
    Ok(scheduler::AgentTaskSpec {
        task: string("task")?,
        role,
        background: optional(string("background")?),
        provider: optional(string("provider")?),
        model: optional(string("model")?),
        context,
        capability_grant_ids: Some(capability_grant_ids),
        budget: scheduler::AgentBudget {
            max_turns,
            timeout_ms,
            max_output_bytes,
        },
    })
}

fn agent_task_status_name(status: scheduler::AgentTaskStatus) -> &'static str {
    match status {
        scheduler::AgentTaskStatus::Queued => "queued",
        scheduler::AgentTaskStatus::Running => "running",
        scheduler::AgentTaskStatus::Completed => "completed",
        scheduler::AgentTaskStatus::Failed => "failed",
        scheduler::AgentTaskStatus::Cancelled => "cancelled",
    }
}

fn agent_role_name(role: scheduler::AgentRole) -> &'static str {
    match role {
        scheduler::AgentRole::General => "general",
        scheduler::AgentRole::Explore => "explore",
        scheduler::AgentRole::Research => "research",
        scheduler::AgentRole::Code => "code",
    }
}

fn typed_agent_task_result(
    result: scheduler::AgentTaskResult,
    origin: &SourceOrigin,
) -> std::result::Result<TypedValue, VmDiagnostic> {
    let turns = i64::try_from(result.turns)
        .map_err(|_| host_binding_error(origin, "agent turn count exceeds VM integer range"))?;
    let elapsed_ms = i64::try_from(result.elapsed_ms)
        .map_err(|_| host_binding_error(origin, "agent elapsed time exceeds VM integer range"))?;
    let depth = i64::try_from(result.identity.depth)
        .map_err(|_| host_binding_error(origin, "agent depth exceeds VM integer range"))?;
    let value = TypedValue::Record(vec![
        (
            "task-id".into(),
            TypedValue::String(result.identity.task_id.to_string()),
        ),
        (
            "agent-id".into(),
            TypedValue::String(result.identity.agent_id.to_string()),
        ),
        (
            "status".into(),
            TypedValue::String(agent_task_status_name(result.status).into()),
        ),
        (
            "final-message".into(),
            TypedValue::String(result.final_message),
        ),
        (
            "diagnostics".into(),
            TypedValue::List {
                element_type: Type::String,
                values: result
                    .diagnostics
                    .into_iter()
                    .map(TypedValue::String)
                    .collect(),
            },
        ),
        ("turns".into(), TypedValue::Int(turns)),
        ("elapsed-ms".into(), TypedValue::Int(elapsed_ms)),
        (
            "provider-model".into(),
            TypedValue::String(result.identity.provider_model),
        ),
        (
            "starting-context-hash".into(),
            TypedValue::String(result.identity.starting_context_hash),
        ),
        ("depth".into(), TypedValue::Int(depth)),
    ]);
    debug_assert_eq!(value.value_type(), agent_task_result_type());
    Ok(value)
}

fn typed_agent_task_snapshot(
    snapshot: scheduler::AgentTaskSnapshot,
    origin: &SourceOrigin,
) -> std::result::Result<TypedValue, VmDiagnostic> {
    let depth = i64::try_from(snapshot.identity.depth)
        .map_err(|_| host_binding_error(origin, "agent depth exceeds VM integer range"))?;
    let complete = matches!(
        snapshot.status,
        scheduler::AgentTaskStatus::Completed
            | scheduler::AgentTaskStatus::Failed
            | scheduler::AgentTaskStatus::Cancelled
    );
    let value = TypedValue::Record(vec![
        (
            "task-id".into(),
            TypedValue::String(snapshot.identity.task_id.to_string()),
        ),
        (
            "agent-id".into(),
            TypedValue::String(snapshot.identity.agent_id.to_string()),
        ),
        (
            "status".into(),
            TypedValue::String(agent_task_status_name(snapshot.status).into()),
        ),
        ("task".into(), TypedValue::String(snapshot.task)),
        (
            "role".into(),
            TypedValue::String(agent_role_name(snapshot.role).into()),
        ),
        (
            "provider-model".into(),
            TypedValue::String(snapshot.identity.provider_model),
        ),
        (
            "starting-context-hash".into(),
            TypedValue::String(snapshot.identity.starting_context_hash),
        ),
        ("depth".into(), TypedValue::Int(depth)),
        ("complete".into(), TypedValue::Bool(complete)),
    ]);
    debug_assert_eq!(value.value_type(), agent_task_snapshot_type());
    Ok(value)
}

impl crate::vm::interpreter::CapabilityHandler for TypedHostHandler {
    fn prepare_awaited_effect(
        &mut self,
        effect: &mut VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        let binding = registered_host_binding(&effect.requirement, &effect.origin)?;
        let crate::vm::interpreter::HostSideEffect::Request { arguments } = &effect.event else {
            return Err(host_binding_error(
                &effect.origin,
                "typed host operation requires a typed host request",
            ));
        };
        if effect.requirement.capability == CapabilityKind::NetworkConnect
            && binding == Some(CoreHostBinding::NetworkSend)
        {
            let Some(TypedValue::Resource {
                kind,
                handle,
                generation,
            }) = arguments.first()
            else {
                return Err(host_binding_error(
                    &effect.origin,
                    "network-send requires a socket resource",
                ));
            };
            if kind != "network-socket" {
                return Err(host_binding_error(
                    &effect.origin,
                    "network-send resource is not a network socket",
                ));
            }
            let sockets = self
                .network
                .lock()
                .map_err(|_| host_binding_error(&effect.origin, "network lock poisoned"))?;
            let socket = sockets
                .get(handle)
                .ok_or_else(|| host_binding_error(&effect.origin, "unknown network socket"))?;
            if socket.owner != self.execution_id || socket.generation != *generation {
                return Err(host_binding_error(
                    &effect.origin,
                    "network socket is stale or belongs to another ProgramRun",
                ));
            }
            effect.requirement.selector = ResourceSelector::Network {
                host: socket.host.clone(),
                ports: vec![socket.port],
            };
        } else if effect.requirement.capability == CapabilityKind::FileRead
            && matches!(
                binding,
                Some(
                    CoreHostBinding::FileLinesNext
                        | CoreHostBinding::FileLinesClose
                        | CoreHostBinding::CsvNext
                        | CoreHostBinding::CsvClose
                        | CoreHostBinding::StreamNext
                        | CoreHostBinding::StreamClose
                )
            )
        {
            let Some(TypedValue::Stream { id, generation, .. }) = arguments.first() else {
                return Err(host_binding_error(
                    &effect.origin,
                    "stream operation requires a stream resource",
                ));
            };
            let streams = self
                .streams
                .lock()
                .map_err(|_| host_binding_error(&effect.origin, "stream registry lock poisoned"))?;
            let stream = streams
                .get(id)
                .ok_or_else(|| host_binding_error(&effect.origin, "stream is unknown or closed"))?;
            if stream.owner != self.execution_id || stream.generation != *generation {
                return Err(host_binding_error(
                    &effect.origin,
                    "stream does not belong to this ProgramRun",
                ));
            }
            effect.requirement = stream.requirement.clone();
        } else if effect.requirement.capability == CapabilityKind::ProcessRun {
            if binding != Some(CoreHostBinding::ProcessRun) {
                return Err(host_binding_error(
                    &effect.origin,
                    "process execution requires the registered process-run host binding",
                ));
            }
            let [TypedValue::String(command), TypedValue::List { values, .. }] =
                arguments.as_slice()
            else {
                return Err(host_binding_error(
                    &effect.origin,
                    "process-run requires an executable and string arguments",
                ));
            };
            let process_arguments = values
                .iter()
                .map(|value| match value {
                    TypedValue::String(value) => Ok(value.clone()),
                    _ => Err(host_binding_error(
                        &effect.origin,
                        "process-run arguments must be strings",
                    )),
                })
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let identity = open_process_executable(command, &process_arguments)
                .map(|opened| opened.identity)
                .map_err(|message| host_binding_error(&effect.origin, message))?;
            effect.requirement.selector = ResourceSelector::Process {
                executables: vec![identity.encode()],
            };
        }
        validate_core_host_request(binding, &effect.requirement, arguments, &effect.origin)
    }

    fn authorize_awaited_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        if effect.requirement.capability == CapabilityKind::ProcessRun {
            validate_process_effect(effect)?;
        }
        let requested = EffectSet::from_requirement(effect.requirement.clone());
        if TypedRuntime::intrinsic_grants().grants(&requested) {
            return Ok(());
        }
        let arguments = match &effect.event {
            crate::vm::interpreter::HostSideEffect::Request { arguments } => arguments.clone(),
            _ => {
                return Err(VmDiagnostic::error(
                    "E-HOST-002",
                    crate::vm::DiagnosticPhase::HostCall,
                    "VM await boundary did not carry a host request",
                    Some(effect.origin.clone()),
                ));
            }
        };
        let request_key = format!("effect:{}", effect.sequence);
        let request = CapabilityRequest {
            id: uuid::Uuid::new_v5(&self.execution_id, request_key.as_bytes()),
            execution_id: self.execution_id,
            effect_sequence: Some(effect.sequence),
            requirement: effect.requirement.clone(),
            arguments,
            reason: self.authorization.reason.clone(),
            origin: effect.origin.clone(),
            agent_ancestry: self.authorization.agent_ancestry.clone(),
            program_hash: self.authorization.program_hash.clone(),
        };
        let policy = self
            .authorization
            .policy
            .read()
            .map_err(|_| host_binding_error(&effect.origin, "capability policy lock poisoned"))?
            .clone();
        if !policy.permits(&effect.requirement) {
            return Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                format!(
                    "capability {:?} is denied by policy {}",
                    effect.requirement.capability, policy.policy_hash
                ),
                Some(effect.origin.clone()),
            ));
        }
        let mut context = self.authorization.context.clone();
        context.now_unix_ms = unix_time_ms();
        context.policy_hash = policy.policy_hash.clone();
        let mut ledger =
            self.authorization.ledger.lock().map_err(|_| {
                host_binding_error(&effect.origin, "capability ledger lock poisoned")
            })?;
        let recorded = ledger.recorded_authorization(&request, &context);
        let previous = recorded.is_none().then(|| ledger.clone());
        let decision =
            recorded.unwrap_or_else(|| ledger.authorize(&request, &context, "typed-host-boundary"));
        if let (Some(previous), Some(sink)) = (previous.as_ref(), &self.authorization.sink) {
            let Some(project_id) = context.project_id.clone() else {
                *ledger = previous.clone();
                return Err(host_binding_error(
                    &effect.origin,
                    "host authorization has no project identity",
                ));
            };
            let roots = self.resource_roots.read().map_err(|_| {
                host_binding_error(&effect.origin, "resource-root binding lock poisoned")
            })?;
            let state = authority_state_from_parts(
                context.session_id,
                project_id,
                policy,
                ledger.clone(),
                &roots,
            );
            if let Err(error) = sink(state) {
                *ledger = previous.clone();
                return Err(host_binding_error(
                    &effect.origin,
                    format!("persist host authorization audit: {error:#}"),
                ));
            }
        }
        match decision {
            AuthorizationDecision::Allowed { .. } => {
                self.authorization_attempt = Some(HostAuthorizationAttempt {
                    request,
                    decision,
                    use_started: false,
                });
                Ok(())
            }
            AuthorizationDecision::ApprovalRequired => Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                "capability was revoked, expired, or outside its approved scope at the host boundary",
                Some(effect.origin.clone()),
            )),
            AuthorizationDecision::Denied { reason } => Err(VmDiagnostic::error(
                "E-CAP-006",
                crate::vm::DiagnosticPhase::HostCall,
                format!("capability is denied at the host boundary: {reason}"),
                Some(effect.origin.clone()),
            )),
        }
    }

    fn observe_awaited_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        // `output-open` is unusual among awaited host effects in the
        // synchronous Finch adapter.  The adapter immediately issues a
        // program-owned handle and projects a `Ui::Create` event with this
        // effect's sequence below in `request_effect`.  Forwarding the
        // preceding request as well would give one client projection two
        // different events at the same `(execution_id, sequence)`: it would
        // advance its cursor for the no-op request and then discard Create as
        // a duplicate.  A portable deferred host, in contrast, owns handle
        // issuance and must receive the original request.
        let synchronous_output_open = effect.origin.word.as_deref() == Some("output-open")
            && !self.deferred_host_effects.defers(effect);
        if synchronous_output_open {
            return Ok(());
        }
        if self.deferred_host_effects.defers(effect) && self.authorization_attempt.is_some() {
            let Some((request, decision)) = self
                .authorization_attempt
                .as_ref()
                .map(|attempt| (attempt.request.clone(), attempt.decision.clone()))
            else {
                unreachable!("checked above")
            };
            let authority_use_gate = Arc::clone(&self.authorization.use_gate);
            let _authority_use = authority_use_gate
                .read()
                .map_err(|_| host_binding_error(&effect.origin, "authority-use gate poisoned"))?;
            let policy = self
                .authorization
                .policy
                .read()
                .map_err(|_| host_binding_error(&effect.origin, "capability policy lock poisoned"))?
                .clone();
            let mut context = self.authorization.context.clone();
            context.now_unix_ms = unix_time_ms();
            context.policy_hash = policy.policy_hash.clone();
            let ledger_handle = Arc::clone(&self.authorization.ledger);
            let mut ledger = ledger_handle.lock().map_err(|_| {
                host_binding_error(&effect.origin, "capability ledger lock poisoned")
            })?;
            if ledger.recorded_authorization(&request, &context) != Some(decision) {
                let authorized = ledger.clone();
                ledger.rollback_authorization(&request);
                if let Some(sink) = &self.authorization.sink {
                    let Some(project_id) = context.project_id.clone() else {
                        *ledger = authorized;
                        self.authorization_attempt = None;
                        return Err(host_binding_error(
                            &effect.origin,
                            "deferred host authorization has no project identity during invalidation",
                        ));
                    };
                    let roots = self.resource_roots.read().map_err(|_| {
                        host_binding_error(&effect.origin, "resource-root binding lock poisoned")
                    })?;
                    if let Err(error) = sink(authority_state_from_parts(
                        context.session_id,
                        project_id,
                        policy,
                        ledger.clone(),
                        &roots,
                    )) {
                        *ledger = authorized;
                        self.authorization_attempt = None;
                        return Err(host_binding_error(
                            &effect.origin,
                            format!("persist invalidated deferred host authorization: {error:#}"),
                        ));
                    }
                }
                self.authorization_attempt = None;
                return Err(host_binding_error(
                    &effect.origin,
                    "authorization was revoked, expired, or replaced before deferred dispatch",
                ));
            }
            drop(ledger);
            self.mark_host_use();
            if let Some(sink) = &self.typed_effect_sink {
                sink(VmEffectEnvelope {
                    execution_id: self.execution_id,
                    effect: effect.clone(),
                });
            }
            self.authorization_attempt = None;
            return Ok(());
        }
        if let Some(sink) = &self.typed_effect_sink {
            sink(VmEffectEnvelope {
                execution_id: self.execution_id,
                effect: effect.clone(),
            });
        }
        Ok(())
    }

    fn defer_awaited_effect(&self, effect: &VmSideEffect) -> bool {
        // An event-loop/IDE binding can own proposal editing without blocking
        // the runner in `$EDITOR`. The portable effect already carries its
        // exact output row and sequence; the host resumes it later with the
        // accepted/chat/cancel value. Plain `submit` retains the existing
        // synchronous compatibility adapter while that UI is migrated.
        self.deferred_host_effects.defers(effect)
    }

    fn request_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let values = match &effect.event {
            crate::vm::interpreter::HostSideEffect::Request { arguments } => self
                .request_with_authority_lease(
                    &effect.requirement,
                    arguments.clone(),
                    &effect.origin,
                )?,
            _ => {
                return Err(VmDiagnostic::error(
                    "E-HOST-002",
                    crate::vm::DiagnosticPhase::HostCall,
                    "VM await boundary did not carry a host request",
                    Some(effect.origin.clone()),
                ));
            }
        };

        // `output-open` awaits a host-issued opaque handle. Project its
        // corresponding Create event immediately, but retain the original
        // request in the durable VM journal for audit/resume semantics.
        if effect.origin.word.as_deref() == Some("output-open") {
            let (Some(TypedValue::String(title)), Some(target)) = (
                match &effect.event {
                    crate::vm::interpreter::HostSideEffect::Request { arguments } => {
                        arguments.first()
                    }
                    _ => None,
                },
                values.first(),
            ) else {
                return Err(host_binding_error(
                    &effect.origin,
                    "output-open host response is invalid",
                ));
            };
            if let Some(sink) = &self.typed_effect_sink {
                let mut create = effect.clone();
                create.event = crate::vm::interpreter::HostSideEffect::Ui {
                    operation: crate::vm::interpreter::UiOperation::Create,
                    target: Some(target.clone()),
                    text: Some(title.clone()),
                    progress: None,
                };
                sink(VmEffectEnvelope {
                    execution_id: self.execution_id,
                    effect: create,
                });
            }
        }
        Ok(values)
    }

    fn request_with_authority_lease(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let Some((request, decision)) = self
            .authorization_attempt
            .as_ref()
            .map(|attempt| (attempt.request.clone(), attempt.decision.clone()))
        else {
            return self.request(requirement, arguments, origin);
        };
        #[cfg(test)]
        run_authorization_before_lease_hook(self.execution_id, requirement.capability.clone());
        let authority_use_gate = Arc::clone(&self.authorization.use_gate);
        let _authority_use = authority_use_gate
            .read()
            .map_err(|_| host_binding_error(origin, "authority-use gate poisoned"))?;
        let policy = self
            .authorization
            .policy
            .read()
            .map_err(|_| host_binding_error(origin, "capability policy lock poisoned"))?
            .clone();
        let mut context = self.authorization.context.clone();
        context.now_unix_ms = unix_time_ms();
        context.policy_hash = policy.policy_hash.clone();
        #[cfg(test)]
        run_authorization_before_use_hook(self.execution_id, requirement.capability.clone());
        let ledger_handle = Arc::clone(&self.authorization.ledger);
        let mut ledger = ledger_handle
            .lock()
            .map_err(|_| host_binding_error(origin, "capability ledger lock poisoned"))?;
        if ledger.recorded_authorization(&request, &context) != Some(decision) {
            let authorized = ledger.clone();
            ledger.rollback_authorization(&request);
            if let Some(sink) = &self.authorization.sink {
                let Some(project_id) = context.project_id.clone() else {
                    *ledger = authorized;
                    self.authorization_attempt = None;
                    return Err(host_binding_error(
                        origin,
                        "host authorization has no project identity during invalidation",
                    ));
                };
                let roots = self.resource_roots.read().map_err(|_| {
                    host_binding_error(origin, "resource-root binding lock poisoned")
                })?;
                if let Err(error) = sink(authority_state_from_parts(
                    context.session_id,
                    project_id,
                    policy.clone(),
                    ledger.clone(),
                    &roots,
                )) {
                    *ledger = authorized;
                    self.authorization_attempt = None;
                    return Err(host_binding_error(
                        origin,
                        format!("persist invalidated host authorization: {error:#}"),
                    ));
                }
            }
            self.authorization_attempt = None;
            return Err(host_binding_error(
                origin,
                "authorization was revoked, expired, or replaced before host use",
            ));
        }
        drop(ledger);

        // The shared authority-use gate makes host dispatch and revocation or
        // policy/root mutation linearizable without retaining the non-reentrant
        // ledger mutex across host code. Agent spawning intentionally re-enters
        // the ledger to snapshot its grant ceiling.
        let mut result = self.request(requirement, arguments, origin);
        let attempt = self
            .authorization_attempt
            .take()
            .expect("authorization attempt exists while lease is held");
        if !attempt.use_started {
            if result.is_ok() {
                result = Err(host_binding_error(
                    origin,
                    "host binding completed without finalizing its authority use",
                ));
            }
            let mut ledger = ledger_handle
                .lock()
                .map_err(|_| host_binding_error(origin, "capability ledger lock poisoned"))?;
            let authorized = ledger.clone();
            ledger.rollback_authorization(&attempt.request);
            if let Some(sink) = &self.authorization.sink {
                let Some(project_id) = context.project_id else {
                    *ledger = authorized;
                    return Err(host_binding_error(
                        origin,
                        "host authorization has no project identity during rollback",
                    ));
                };
                let roots = self.resource_roots.read().map_err(|_| {
                    host_binding_error(origin, "resource-root binding lock poisoned")
                })?;
                if let Err(error) = sink(authority_state_from_parts(
                    context.session_id,
                    project_id,
                    policy,
                    ledger.clone(),
                    &roots,
                )) {
                    *ledger = authorized;
                    return Err(host_binding_error(
                        origin,
                        format!("persist unused host authorization rollback: {error:#}"),
                    ));
                }
            }
        }
        result
    }

    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let binding = registered_host_binding(requirement, origin)?;
        // `prepare_awaited_effect` rejects forged portable continuations, but
        // embedders may call this trait method directly. Repeat the complete
        // binding check at the final operation boundary so no direct adapter
        // can substitute runtime arguments after authority was derived.
        validate_core_host_request(binding, requirement, &arguments, origin)?;
        let request = match requirement.capability {
            crate::vm::CapabilityKind::SessionEmit => {
                // `output-open` uses the same session-emission authority as
                // ordinary visible output, but its awaited host request
                // returns an opaque handle rather than emitting its title as
                // a response chunk. Recognize it before validating the
                // ordinary one-string `say` ABI.
                if origin.word.as_deref() == Some("output-open") {
                    let [TypedValue::String(_title)] = arguments.as_slice() else {
                        return Err(VmDiagnostic::error(
                            "E-HOST-001",
                            crate::vm::DiagnosticPhase::HostCall,
                            "output-open requires one title string",
                            Some(origin.clone()),
                        ));
                    };
                    let handle = uuid::Uuid::new_v4().to_string();
                    self.output_handles
                        .lock()
                        .map_err(|_| {
                            host_binding_error(origin, "output handle registry lock poisoned")
                        })?
                        .insert(
                            handle.clone(),
                            OutputHandleRecord {
                                owner: self.execution_id,
                                generation: 0,
                            },
                        );
                    return Ok(vec![TypedValue::Resource {
                        kind: "output-handle".into(),
                        handle,
                        generation: 0,
                    }]);
                }
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-HOST-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "session.emit requires one string",
                        Some(origin.clone()),
                    ));
                };
                self.output.push_str(text);
                self.output_chunks.push(text.clone());
                self.emit(text);
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::VmRead => {
                if origin.word.as_deref() == Some("vm-vocabulary") {
                    return Ok(vec![TypedValue::String(self.vocabulary.clone())]);
                }
                if origin.word.as_deref() == Some("capability-list") {
                    let ledger = self.authorization.ledger.lock().map_err(|_| {
                        host_binding_error(origin, "capability ledger lock poisoned")
                    })?;
                    let values = ledger
                        .grants
                        .active_grants_for(&self.authorization.context)
                        .filter(|grant| {
                            self.network_grants
                                .grants(&EffectSet::from_requirement(grant.requirement.clone()))
                        })
                        .map(|grant| {
                            let requirement = serde_json::to_value(&grant.requirement)
                                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                            Ok(TypedValue::Record(vec![
                                (
                                    "grant".into(),
                                    TypedValue::Resource {
                                        kind: "capability-grant".into(),
                                        handle: grant.id.to_string(),
                                        generation: 0,
                                    },
                                ),
                                ("requirement".into(), TypedValue::Json(requirement)),
                            ]))
                        })
                        .collect::<std::result::Result<Vec<_>, VmDiagnostic>>()?;
                    return Ok(vec![TypedValue::List {
                        element_type: capability_grant_entry_type(),
                        values,
                    }]);
                }
                return Err(host_binding_error(
                    origin,
                    "unknown VM inspection operation",
                ));
            }
            crate::vm::CapabilityKind::AutomationInspect => match binding {
                Some(CoreHostBinding::AutomationDisplays) => AutomationRequest::Displays,
                Some(CoreHostBinding::AutomationWindows) => AutomationRequest::Windows,
                Some(CoreHostBinding::AutomationAvailability) => AutomationRequest::Availability,
                _ => {
                    return Err(host_binding_error(
                        origin,
                        "automation inspection requires its exact registered host binding",
                    ))
                }
            },
            crate::vm::CapabilityKind::AutomationWrite => match binding {
                Some(CoreHostBinding::AutomationClick) => {
                    let [TypedValue::Float(x), TypedValue::Float(y), TypedValue::String(button), TypedValue::Int(count)] =
                        arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "automation-click argument types are invalid",
                        ));
                    };
                    AutomationRequest::Click {
                        x: *x,
                        y: *y,
                        button: button.clone(),
                        count: u8::try_from(*count).map_err(|_| {
                            host_binding_error(origin, "click count is out of range")
                        })?,
                    }
                }
                Some(CoreHostBinding::AutomationType) => {
                    let [TypedValue::String(text), TypedValue::Int(delay_ms)] =
                        arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "automation-type argument types are invalid",
                        ));
                    };
                    AutomationRequest::Type {
                        text: text.clone(),
                        delay_ms: u64::try_from(*delay_ms).map_err(|_| {
                            host_binding_error(origin, "delay must be non-negative")
                        })?,
                    }
                }
                _ => {
                    return Err(host_binding_error(
                        origin,
                        "automation mutation requires its exact registered host binding",
                    ))
                }
            },
            crate::vm::CapabilityKind::FileRead => {
                match origin.word.as_deref() {
                    Some("csv-next") | Some("file-lines-next") | Some("stream-next") => {
                        return self.stream_next(&arguments, origin);
                    }
                    Some("csv-close") | Some("file-lines-close") | Some("stream-close") => {
                        return self.stream_close(&arguments, origin);
                    }
                    _ => {}
                }
                let (relative, selector) = match arguments.first() {
                    Some(TypedValue::Path { relative, selector }) => (relative, selector),
                    _ => {
                        return Err(host_binding_error(
                            origin,
                            "file read operations require a refined path as their first argument",
                        ));
                    }
                };
                let mode = if matches!(
                    binding,
                    Some(CoreHostBinding::TreeList | CoreHostBinding::TreeMerkle)
                ) {
                    SecureOpenMode::ReadDirectory
                } else {
                    SecureOpenMode::ReadFile
                };
                let mut file = self
                    .open_secure_resource(selector, relative, mode)
                    .map_err(|message| host_binding_error(origin, message))?;
                self.mark_host_use();
                match origin.word.as_deref() {
                    Some("workbook-open") | Some("workbook-sheet-open") => {
                        let sheet = match arguments.as_slice() {
                            [_] if origin.word.as_deref() == Some("workbook-open") => None,
                            [_, TypedValue::String(sheet)]
                                if origin.word.as_deref() == Some("workbook-sheet-open") =>
                            {
                                Some(sheet.as_str())
                            }
                            _ => {
                                return Err(host_binding_error(
                                    origin,
                                    "workbook-open requires a path; workbook-sheet-open requires a path and sheet name",
                                ))
                            }
                        };
                        let rows = read_workbook_rows(&file, relative, sheet)
                            .map_err(|message| host_binding_error(origin, message))?;
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::WorkbookRows(rows.into_iter()),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::list(Type::String),
                            kind: "workbook-rows".into(),
                            generation: 0,
                        }]);
                    }
                    Some("workbook-sheets") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "workbook-sheets requires one path",
                            ));
                        }
                        let sheets = read_workbook_sheet_names(&file, relative)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::List {
                            element_type: Type::String,
                            values: sheets.into_iter().map(TypedValue::String).collect(),
                        }]);
                    }
                    Some("workbook-range") => {
                        let [_, TypedValue::String(sheet), TypedValue::Int(start_row), TypedValue::Int(start_column), TypedValue::Int(row_count), TypedValue::Int(column_count)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "workbook-range requires path, sheet, start row, start column, row count, and column count",
                            ));
                        };
                        let values = read_workbook_range(
                            &file,
                            relative,
                            sheet,
                            *start_row,
                            *start_column,
                            *row_count,
                            *column_count,
                        )
                        .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::List {
                            element_type: Type::list(Type::String),
                            values: values
                                .into_iter()
                                .map(|row| TypedValue::List {
                                    element_type: Type::String,
                                    values: row.into_iter().map(TypedValue::String).collect(),
                                })
                                .collect(),
                        }]);
                    }
                    Some("workbook-summary") => {
                        let [_, TypedValue::String(sheet), TypedValue::Int(max_rows)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "workbook-summary requires a path, sheet name, and maximum data-row count",
                            ));
                        };
                        let max_rows = usize::try_from(*max_rows).map_err(|_| {
                            host_binding_error(
                                origin,
                                "workbook-summary maximum rows must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_rows) {
                            return Err(host_binding_error(
                                origin,
                                "workbook-summary maximum rows must be between 1 and 100000",
                            ));
                        }
                        let summary = summarize_workbook(&file, relative, sheet, max_rows)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::Json(summary)]);
                    }
                    Some("csv-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "csv-open requires one path"));
                        }
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::CsvRecords(BufReader::new(file)),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::list(Type::String),
                            kind: "csv-records".into(),
                            generation: 0,
                        }]);
                    }
                    Some("csv-summary") => {
                        let [_, TypedValue::Int(max_rows)] = arguments.as_slice() else {
                            return Err(host_binding_error(
                                origin,
                                "csv-summary requires a path and maximum data-row count",
                            ));
                        };
                        let max_rows = usize::try_from(*max_rows).map_err(|_| {
                            host_binding_error(
                                origin,
                                "csv-summary maximum rows must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_rows) {
                            return Err(host_binding_error(
                                origin,
                                "csv-summary maximum rows must be between 1 and 100000",
                            ));
                        }
                        let summary = summarize_csv(BufReader::new(file), max_rows)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::Json(summary)]);
                    }
                    Some("file-lines-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "file-lines-open requires one path",
                            ));
                        }
                        let handle = uuid::Uuid::new_v4().to_string();
                        self.streams
                            .lock()
                            .map_err(|_| {
                                host_binding_error(origin, "stream registry lock poisoned")
                            })?
                            .insert(
                                handle.clone(),
                                HostStream {
                                    owner: self.execution_id,
                                    generation: 0,
                                    requirement: requirement.clone(),
                                    backend: HostStreamBackend::FileLines(BufReader::new(file)),
                                },
                            );
                        return Ok(vec![TypedValue::Stream {
                            id: handle,
                            element_type: Type::String,
                            kind: "file-lines".into(),
                            generation: 0,
                        }]);
                    }
                    Some("file-size") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-size requires one path"));
                        }
                        let size = file
                            .metadata()
                            .map_err(|error| host_binding_error(origin, error.to_string()))?
                            .len();
                        let size = i64::try_from(size).map_err(|_| {
                            host_binding_error(origin, "file is too large to represent")
                        })?;
                        return Ok(vec![TypedValue::Int(size)]);
                    }
                    Some("file-hash") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-hash requires one path"));
                        }
                        let digest = sha256_file_handle(&file)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::String(hex_digest(&digest))]);
                    }
                    Some("tree-list") => {
                        let [_, TypedValue::Int(max_entries)] = arguments.as_slice() else {
                            return Err(host_binding_error(
                                origin,
                                "tree-list requires a directory path and maximum entry count",
                            ));
                        };
                        let max_entries = usize::try_from(*max_entries).map_err(|_| {
                            host_binding_error(
                                origin,
                                "tree-list maximum entries must be between 1 and 100000",
                            )
                        })?;
                        if !(1..=100_000).contains(&max_entries) {
                            return Err(host_binding_error(
                                origin,
                                "tree-list maximum entries must be between 1 and 100000",
                            ));
                        }
                        let (entries, truncated) = list_directory_tree(file, max_entries)
                            .map_err(|message| host_binding_error(origin, message))?;
                        let value = TypedValue::Record(vec![
                            (
                                "entries".into(),
                                TypedValue::List {
                                    element_type: tree_entry_type(),
                                    values: entries,
                                },
                            ),
                            ("truncated".into(), TypedValue::Bool(truncated)),
                        ]);
                        debug_assert_eq!(value.value_type(), tree_listing_type());
                        return Ok(vec![value]);
                    }
                    Some("tree-merkle") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "tree-merkle requires one path",
                            ));
                        }
                        let digest = merkle_directory(file)
                            .map_err(|message| host_binding_error(origin, message))?;
                        return Ok(vec![TypedValue::String(digest)]);
                    }
                    Some("file-slice") => {
                        let [_, TypedValue::Int(offset), TypedValue::Int(length)] =
                            arguments.as_slice()
                        else {
                            return Err(host_binding_error(
                                origin,
                                "file-slice requires a path, non-negative byte offset, and length",
                            ));
                        };
                        let offset = u64::try_from(*offset).map_err(|_| {
                            host_binding_error(origin, "file-slice offset must be non-negative")
                        })?;
                        let length = usize::try_from(*length).map_err(|_| {
                            host_binding_error(origin, "file-slice length must be non-negative")
                        })?;
                        const MAX_FILE_SLICE_BYTES: usize = 8 * 1024 * 1024;
                        if length > MAX_FILE_SLICE_BYTES {
                            return Err(host_binding_error(
                                origin,
                                format!(
                                    "file-slice length exceeds the {MAX_FILE_SLICE_BYTES}-byte per-call limit"
                                ),
                            ));
                        }
                        file.seek(SeekFrom::Start(offset))
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        let mut bytes = vec![0; length];
                        let read = file
                            .read(&mut bytes)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        bytes.truncate(read);
                        return Ok(vec![TypedValue::Bytes(bytes)]);
                    }
                    _ => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "file-read requires one path"));
                        }
                        let mut bytes = Vec::new();
                        file.read_to_end(&mut bytes)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                        return Ok(vec![TypedValue::Bytes(bytes)]);
                    }
                }
            }
            crate::vm::CapabilityKind::FileWrite => {
                let [TypedValue::Path { relative, selector }, TypedValue::Bytes(bytes)] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "file-write requires a path and bytes",
                    ));
                };
                self.mark_host_use();
                let mut file = self
                    .open_secure_resource(selector, relative, SecureOpenMode::WriteFile)
                    .map_err(|message| host_binding_error(origin, message))?;
                file.write_all(bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::AgentSpawn => {
                let [argument] = arguments.as_slice() else {
                    return Err(host_binding_error(
                        origin,
                        "agent spawn requires exactly one task or task specification",
                    ));
                };
                let spec = if origin.word.as_deref() == Some("agent-spawn-with") {
                    typed_agent_task_spec(argument, origin)?
                } else {
                    let TypedValue::String(task) = argument else {
                        return Err(host_binding_error(origin, "agent-spawn requires one task"));
                    };
                    scheduler::AgentTaskSpec {
                        task: task.clone(),
                        role: Default::default(),
                        background: None,
                        provider: None,
                        model: None,
                        context: Vec::new(),
                        capability_grant_ids: None,
                        budget: Default::default(),
                    }
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let spawn_binding = binding.clone();
                self.mark_host_use();
                let identity = binding
                    .block_on(async move { spawn_binding.spawn_spec(spec).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Task {
                    id: identity.task_id.to_string(),
                    result_type: agent_task_result_type(),
                    kind: crate::vm::TaskKind::Agent,
                }]);
            }
            crate::vm::CapabilityKind::AgentAwait => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-await requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let wait_binding = binding.clone();
                self.mark_host_use();
                let result = binding
                    .block_on(async move { wait_binding.wait(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![typed_agent_task_result(result, origin)?]);
            }
            crate::vm::CapabilityKind::AgentPoll => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-poll requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let poll_binding = binding.clone();
                self.mark_host_use();
                let snapshot = binding
                    .block_on(async move { poll_binding.poll(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![typed_agent_task_snapshot(snapshot, origin)?]);
            }
            crate::vm::CapabilityKind::AgentCancel => {
                let [TypedValue::Task { id: task_id, .. }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-cancel requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let task_id = agent_vm::parse_task_id(task_id)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let cancel_binding = binding.clone();
                self.mark_host_use();
                binding
                    .block_on(async move { cancel_binding.cancel(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::MemoryRead => {
                let [TypedValue::String(query)] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "mem-recall requires one query"));
                };
                let Some(memory) = self.memory.clone() else {
                    return Err(host_binding_error(origin, "memory service is unavailable"));
                };
                let query = query.clone();
                self.mark_host_use();
                let values = block_on_host(async move { memory.query(&query, None).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::List {
                    element_type: Type::String,
                    values: values.into_iter().map(TypedValue::String).collect(),
                }]);
            }
            crate::vm::CapabilityKind::MemoryWrite => {
                let [TypedValue::String(content)] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "mem-store requires one string"));
                };
                let Some(memory) = self.memory.clone() else {
                    return Err(host_binding_error(origin, "memory service is unavailable"));
                };
                let content = content.clone();
                self.mark_host_use();
                block_on_host(async move {
                    memory
                        .insert_conversation("assistant", &content, None, None)
                        .await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Resource {
                    kind: "memory-node".into(),
                    handle: uuid::Uuid::new_v4().to_string(),
                    generation: 0,
                }]);
            }
            crate::vm::CapabilityKind::McpCall => {
                let ResourceSelector::Mcp {
                    server: authorized_server,
                    tool: authorized_tool,
                } = &requirement.selector
                else {
                    return Err(host_binding_error(
                        origin,
                        "mcp-call reached the host without a concrete server/tool selector",
                    ));
                };
                let parameters = match arguments.as_slice() {
                    [TypedValue::String(server), TypedValue::String(tool), TypedValue::Json(parameters)] =>
                    {
                        if server != authorized_server || tool != authorized_tool {
                            return Err(host_binding_error(
                                origin,
                                "mcp-call arguments do not match the authorized server/tool selector",
                            ));
                        }
                        parameters.clone()
                    }
                    [parameters]
                        if origin
                            .word
                            .as_deref()
                            .is_some_and(|word| word.starts_with("mcp.")) =>
                    {
                        typed_mcp_arguments(parameters).map_err(|error| {
                            host_binding_error(origin, format!("encode MCP arguments: {error}"))
                        })?
                    }
                    _ => {
                        return Err(host_binding_error(
                            origin,
                            "MCP calls require either server/tool/JSON or one namespaced binding argument",
                        ));
                    }
                };
                let Some(client) = self.mcp_client.clone() else {
                    return Err(host_binding_error(origin, "MCP client is unavailable"));
                };
                let wire_name = format!("mcp_{authorized_server}_{authorized_tool}");
                self.mark_host_use();
                let response = block_on_host(async move {
                    client.execute_tool_value(&wire_name, parameters).await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                if let Some(schema) = origin
                    .word
                    .as_deref()
                    .and_then(|word| self.mcp_output_schemas.get(word))
                {
                    let structured = response.get("structuredContent").ok_or_else(|| {
                        host_binding_error(
                            origin,
                            "MCP result omitted structuredContent required by its output schema",
                        )
                    })?;
                    mcp::validate_output(schema, structured).map_err(|error| {
                        host_binding_error(origin, format!("validate MCP result: {error:#}"))
                    })?;
                }
                return Ok(vec![TypedValue::Json(response)]);
            }
            crate::vm::CapabilityKind::ProcessRun => {
                let [TypedValue::String(command), TypedValue::List { values, .. }] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "process-run requires a command and string arguments",
                    ));
                };
                let arguments = values
                    .iter()
                    .map(|value| match value {
                        TypedValue::String(value) => Ok(value.clone()),
                        _ => Err(host_binding_error(
                            origin,
                            "process-run arguments must be strings",
                        )),
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let executable =
                    validate_process_request(binding, requirement, command, &arguments, origin)?;
                let child = match spawn_open_process(executable) {
                    Ok(child) => child,
                    Err(error) => {
                        return Err(host_binding_error(origin, error));
                    }
                };
                // `Command::spawn` waits for the child-side exec error pipe.
                // Once it returns a child, the verified descriptor has been
                // executed and the authorization is a real use even if the
                // program later exits unsuccessfully.
                self.mark_host_use();
                let output = child
                    .wait_with_output()
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                if !output.status.success() {
                    return Err(host_binding_error(
                        origin,
                        format!("process exited with status {}", output.status),
                    ));
                }
                return Ok(vec![TypedValue::String(
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )]);
            }
            crate::vm::CapabilityKind::ProgramInvoke => {
                let [TypedValue::String(language), TypedValue::String(intent), TypedValue::String(source)] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "proposal-open requires language, intent, and source strings",
                    ));
                };
                let language = language.clone();
                let intent = intent.clone();
                let source = source.clone();
                self.mark_host_use();
                let decision = block_on_host(async move {
                    crate::tools::implementations::propose::propose_artifact_with_decision(
                        &language, &intent, &source,
                    )
                    .await
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let value = match decision {
                    crate::tools::implementations::propose::ProposalDecision::Execute {
                        source,
                    } => TypedValue::Option {
                        inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                        value: Some(Box::new(TypedValue::Result {
                            ok_type: Type::String,
                            error_type: Type::String,
                            is_ok: true,
                            value: Box::new(TypedValue::String(source)),
                        })),
                    },
                    crate::tools::implementations::propose::ProposalDecision::Chat { context } => {
                        TypedValue::Option {
                            inner_type: Type::Result(
                                Box::new(Type::String),
                                Box::new(Type::String),
                            ),
                            value: Some(Box::new(TypedValue::Result {
                                ok_type: Type::String,
                                error_type: Type::String,
                                is_ok: false,
                                value: Box::new(TypedValue::String(context)),
                            })),
                        }
                    }
                    crate::tools::implementations::propose::ProposalDecision::Cancel => {
                        TypedValue::Option {
                            inner_type: Type::Result(
                                Box::new(Type::String),
                                Box::new(Type::String),
                            ),
                            value: None,
                        }
                    }
                };
                return Ok(vec![value]);
            }
            crate::vm::CapabilityKind::NetworkConnect => {
                if origin.word.as_deref() == Some("network-connect") {
                    let [TypedValue::String(host), TypedValue::Int(port)] = arguments.as_slice()
                    else {
                        return Err(host_binding_error(
                            origin,
                            "network-connect requires host and port",
                        ));
                    };
                    let port = u16::try_from(*port)
                        .map_err(|_| host_binding_error(origin, "network port is out of range"))?;
                    let address = (host.as_str(), port)
                        .to_socket_addrs()
                        .map_err(|error| host_binding_error(origin, error.to_string()))?
                        .next()
                        .ok_or_else(|| host_binding_error(origin, "host has no addresses"))?;
                    self.mark_host_use();
                    let stream =
                        TcpStream::connect_timeout(&address, std::time::Duration::from_secs(5))
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
                    let handle = uuid::Uuid::new_v4().to_string();
                    self.network
                        .lock()
                        .map_err(|_| host_binding_error(origin, "network lock poisoned"))?
                        .insert(
                            handle.clone(),
                            NetworkSocket {
                                stream,
                                host: host.clone(),
                                port,
                                owner: self.execution_id,
                                generation: self.resource_generation,
                            },
                        );
                    return Ok(vec![TypedValue::Resource {
                        kind: "network-socket".into(),
                        handle,
                        generation: self.resource_generation,
                    }]);
                }
                let [TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                }, TypedValue::Bytes(payload)] = arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "network-send requires a socket and bytes",
                    ));
                };
                if kind != "network-socket" {
                    return Err(host_binding_error(
                        origin,
                        "resource is not a network socket",
                    ));
                }
                let network = Arc::clone(&self.network);
                let mut sockets = network
                    .lock()
                    .map_err(|_| host_binding_error(origin, "network lock poisoned"))?;
                let socket = sockets
                    .get_mut(handle)
                    .ok_or_else(|| host_binding_error(origin, "unknown network socket"))?;
                if socket.owner != self.execution_id || socket.generation != *generation {
                    return Err(host_binding_error(
                        origin,
                        "network socket is stale or belongs to another ProgramRun",
                    ));
                }
                let endpoint = CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::NetworkConnect,
                    selector: crate::vm::ResourceSelector::Network {
                        host: socket.host.clone(),
                        ports: vec![socket.port],
                    },
                };
                if !self
                    .network_grants
                    .grants(&EffectSet::from_requirement(endpoint))
                {
                    return Err(host_binding_error(
                        origin,
                        "network socket endpoint is no longer covered by an active grant",
                    ));
                }
                self.mark_host_use();
                socket
                    .stream
                    .write_all(payload)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let mut response = vec![0; 4096];
                let size = socket
                    .stream
                    .read(&mut response)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                response.truncate(size);
                return Ok(vec![TypedValue::Bytes(response)]);
            }
            _ => {
                return Err(host_binding_error(
                    origin,
                    "authorized capability has no typed host binding",
                ));
            }
        };
        self.mark_host_use();
        let value = self
            .automation
            .execute(request)
            .map_err(|error| host_binding_error(origin, error.to_string()))?;
        Ok(vec![TypedValue::String(value.to_string())])
    }

    fn output(&self) -> String {
        self.output.clone()
    }

    fn output_chunks(&self) -> Vec<String> {
        self.output_chunks.clone()
    }

    fn side_effects(&self) -> Vec<crate::vm::interpreter::HostSideEffect> {
        self.side_effects.clone()
    }

    fn side_effect(
        &mut self,
        effect: &crate::vm::interpreter::VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
        match &effect.event {
            crate::vm::interpreter::HostSideEffect::Emit { text } => {
                self.output.push_str(text);
                self.output_chunks.push(text.clone());
            }
            crate::vm::interpreter::HostSideEffect::Ui {
                target, operation, ..
            } => {
                let Some(TypedValue::Resource {
                    kind,
                    handle,
                    generation,
                }) = target
                else {
                    return Err(VmDiagnostic::error(
                        "E-OUTPUT-HANDLE-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "UI updates require an output-handle resource",
                        Some(effect.origin.clone()),
                    ));
                };
                let record = self
                    .output_handles
                    .lock()
                    .map_err(|_| {
                        VmDiagnostic::error(
                            "E-OUTPUT-HANDLE-002",
                            crate::vm::DiagnosticPhase::HostCall,
                            "output handle registry is unavailable",
                            Some(effect.origin.clone()),
                        )
                    })?
                    .get(handle)
                    .copied();
                let valid = kind == "output-handle"
                    && record.is_some_and(|record| {
                        record.owner == self.execution_id && record.generation == *generation
                    });
                if !valid {
                    return Err(VmDiagnostic::error(
                        "E-OUTPUT-HANDLE-003",
                        crate::vm::DiagnosticPhase::HostCall,
                        "output handle is unknown, stale, or belongs to another program run",
                        Some(effect.origin.clone()),
                    ));
                }
                if matches!(
                    operation,
                    crate::vm::interpreter::UiOperation::Complete
                        | crate::vm::interpreter::UiOperation::Fail
                ) {
                    self.output_handles
                        .lock()
                        .map_err(|_| {
                            VmDiagnostic::error(
                                "E-OUTPUT-HANDLE-002",
                                crate::vm::DiagnosticPhase::HostCall,
                                "output handle registry is unavailable",
                                Some(effect.origin.clone()),
                            )
                        })?
                        .remove(handle);
                }
            }
            crate::vm::interpreter::HostSideEffect::Request { .. } => {
                return Err(VmDiagnostic::error(
                    "E-HOST-003",
                    crate::vm::DiagnosticPhase::HostCall,
                    "host requests must be handled at a capability boundary, not as emitted UI events",
                    Some(effect.origin.clone()),
                ));
            }
        }
        if let Some(sink) = &self.typed_effect_sink {
            sink(VmEffectEnvelope {
                execution_id: self.execution_id,
                effect: effect.clone(),
            });
        }
        self.side_effects.push(effect.event.clone());
        Ok(())
    }
}

impl TypedHostHandler {
    fn open_secure_resource(
        &self,
        selector: &crate::vm::FileSelector,
        relative: &str,
        mode: SecureOpenMode,
    ) -> std::result::Result<std::fs::File, String> {
        let binding = self
            .resource_roots
            .read()
            .map_err(|_| "resource-root binding lock poisoned".to_string())?
            .bindings
            .get(&selector.root)
            .cloned()
            .ok_or_else(|| format!("{} root is not installed by this host", selector.root))?;
        open_resource_beneath_mode(&binding, selector, relative, mode)
    }
}

/// Resolve a portable host request through the same core-word registry used
/// by the parser, verifier, and provider discovery.  A verified module should
/// already make this true; the host boundary repeats the check so a corrupted
/// cached module or foreign embedder cannot route (for example) a `file-read`
/// request through an unrelated word name with the same coarse capability.
fn registered_host_binding(
    requirement: &CapabilityRequirement,
    origin: &crate::vm::SourceOrigin,
) -> std::result::Result<Option<CoreHostBinding>, VmDiagnostic> {
    let Some(name) = origin.word.as_deref() else {
        // Embedders may produce a host request with a generated origin. Its
        // capability/result rows are still checked by the VM resume boundary.
        return Ok(None);
    };
    let Some(spec) = core_word_spec(name) else {
        if let (&CapabilityKind::McpCall, ResourceSelector::Mcp { server, tool }) =
            (&requirement.capability, &requirement.selector)
        {
            if name == format!("mcp.{server}.{tool}") {
                return Ok(Some(CoreHostBinding::McpCall));
            }
            return Err(host_binding_error(
                origin,
                "namespaced MCP word does not match its concrete server/tool selector",
            ));
        }
        // User-defined calls inherit a source origin at a higher level; they
        // must not be mistaken for missing core host bindings.
        return Ok(None);
    };

    let binding = match spec.implementation {
        CoreWordImplementation::HostEffect(binding) => binding,
        // `output-open` is an explicit VM instruction which deliberately
        // awaits a host-issued opaque resource. It is the only instruction
        // class currently reaching this request adapter.
        CoreWordImplementation::VmInstruction if name == "output-open" => return Ok(None),
        implementation => {
            return Err(host_binding_error(
                origin,
                format!(
                    "core word '{name}' is registered as {implementation:?}, not as a host request"
                ),
            ))
        }
    };

    let declares_capability = spec
        .signature
        .effects
        .0
        .iter()
        .any(|declared| declared.capability == requirement.capability);
    if !declares_capability {
        return Err(host_binding_error(
            origin,
            format!(
                "core word '{name}' is registered without capability {:?}",
                requirement.capability
            ),
        ));
    }
    Ok(Some(binding))
}

/// Re-derive the exact authority and ABI of a core host request from the
/// immutable vocabulary at the final host boundary. The VM normally creates
/// this tuple, but cached/foreign continuations are untrusted here: a coarse
/// capability kind, forged origin, or same-shaped argument list must never
/// select a different host operation.
fn validate_core_host_request(
    binding: Option<CoreHostBinding>,
    requirement: &CapabilityRequirement,
    arguments: &[TypedValue],
    origin: &SourceOrigin,
) -> std::result::Result<(), VmDiagnostic> {
    let word = origin.word.as_deref();
    if binding.is_none() {
        if word == Some("output-open")
            && matches!(arguments, [TypedValue::String(_)])
            && requirement.capability == CapabilityKind::SessionEmit
            && requirement.selector == ResourceSelector::None
        {
            return Ok(());
        }
        return Err(host_binding_error(
            origin,
            "host request has no exact registered core binding",
        ));
    }
    let binding = binding.expect("checked above");

    if let Some(name) = word {
        if let Some(spec) = core_word_spec(name) {
            let declared = spec
                .signature
                .effects
                .0
                .iter()
                .find(|declared| declared.capability == requirement.capability)
                .ok_or_else(|| {
                    host_binding_error(origin, "core binding does not declare this capability")
                })?;
            let expected = crate::vm::interpreter::instantiate_requirement(declared, arguments)
                .map_err(|message| host_binding_error(origin, message))?;
            let dynamically_bound = matches!(
                binding,
                CoreHostBinding::NetworkSend
                    | CoreHostBinding::FileLinesNext
                    | CoreHostBinding::FileLinesClose
                    | CoreHostBinding::CsvNext
                    | CoreHostBinding::CsvClose
                    | CoreHostBinding::StreamNext
                    | CoreHostBinding::StreamClose
                    | CoreHostBinding::ProcessRun
            );
            if !dynamically_bound && expected != *requirement {
                return Err(host_binding_error(
                    origin,
                    "runtime arguments do not derive the authorized capability requirement",
                ));
            }
        }
    }

    let string = |value: &TypedValue| matches!(value, TypedValue::String(_));
    let integer = |value: &TypedValue| matches!(value, TypedValue::Int(_));
    let float = |value: &TypedValue| matches!(value, TypedValue::Float(_));
    let path = |value: &TypedValue| matches!(value, TypedValue::Path { .. });
    let bytes = |value: &TypedValue| matches!(value, TypedValue::Bytes(_));
    let stream = |value: &TypedValue| matches!(value, TypedValue::Stream { .. });
    let resource = |value: &TypedValue, kind: &str| matches!(value, TypedValue::Resource { kind: actual, .. } if actual == kind);
    let task = |value: &TypedValue| matches!(value, TypedValue::Task { .. });
    let valid_arguments = match binding {
        CoreHostBinding::SessionEmit => matches!(arguments, [value] if string(value)),
        CoreHostBinding::VmVocabulary
        | CoreHostBinding::CapabilityList
        | CoreHostBinding::AutomationAvailability
        | CoreHostBinding::AutomationDisplays
        | CoreHostBinding::AutomationWindows => arguments.is_empty(),
        CoreHostBinding::FileRead
        | CoreHostBinding::FileHash
        | CoreHostBinding::TreeMerkle
        | CoreHostBinding::FileSize
        | CoreHostBinding::FileLinesOpen
        | CoreHostBinding::CsvOpen
        | CoreHostBinding::WorkbookOpen
        | CoreHostBinding::WorkbookSheets => matches!(arguments, [value] if path(value)),
        CoreHostBinding::TreeList => {
            matches!(arguments, [first, second] if path(first) && integer(second))
        }
        CoreHostBinding::FileSlice => matches!(arguments, [first, second, third]
            if path(first) && integer(second) && integer(third)),
        CoreHostBinding::WorkbookSheetOpen => {
            matches!(arguments, [first, second] if path(first) && string(second))
        }
        CoreHostBinding::WorkbookRange => matches!(arguments,
            [first, sheet, row, column, rows, columns]
                if path(first) && string(sheet) && integer(row) && integer(column)
                    && integer(rows) && integer(columns)),
        CoreHostBinding::WorkbookSummary => matches!(arguments,
            [first, sheet, rows] if path(first) && string(sheet) && integer(rows)),
        CoreHostBinding::CsvSummary => {
            matches!(arguments, [first, rows] if path(first) && integer(rows))
        }
        CoreHostBinding::FileLinesNext
        | CoreHostBinding::FileLinesClose
        | CoreHostBinding::CsvNext
        | CoreHostBinding::CsvClose
        | CoreHostBinding::StreamNext
        | CoreHostBinding::StreamClose => matches!(arguments, [value] if stream(value)),
        CoreHostBinding::FileWrite => {
            matches!(arguments, [first, second] if path(first) && bytes(second))
        }
        CoreHostBinding::ProcessRun => matches!(arguments,
            [command, TypedValue::List { values, .. }]
                if string(command) && values.iter().all(string)),
        CoreHostBinding::McpCall => {
            matches!(arguments,
            [server, tool, TypedValue::Json(_)] if string(server) && string(tool))
                || arguments.len() == 1 && word.is_some_and(|name| name.starts_with("mcp."))
        }
        CoreHostBinding::ProposalOpen => {
            matches!(arguments, [first, second, third]
                if string(first) && string(second) && string(third))
        }
        CoreHostBinding::NetworkConnect => {
            matches!(arguments, [host, port] if string(host) && integer(port))
        }
        CoreHostBinding::NetworkSend => {
            matches!(arguments, [socket, payload]
                if resource(socket, "network-socket") && bytes(payload))
        }
        CoreHostBinding::MemoryRecall | CoreHostBinding::MemoryStore => {
            matches!(arguments, [value] if string(value))
        }
        CoreHostBinding::ScheduleCreate => {
            matches!(arguments, [source, interval] if string(source) && integer(interval))
        }
        CoreHostBinding::ScheduleGet | CoreHostBinding::ScheduleCancel => {
            matches!(arguments, [value] if resource(value, "schedule"))
        }
        CoreHostBinding::AgentSpawn => matches!(arguments, [value] if string(value)),
        CoreHostBinding::AgentSpawnWith => matches!(arguments, [TypedValue::Record(_)]),
        CoreHostBinding::AgentAwait | CoreHostBinding::AgentPoll | CoreHostBinding::AgentCancel => {
            matches!(arguments, [value] if task(value))
        }
        CoreHostBinding::AutomationClick => matches!(arguments, [x, y, button, count]
            if float(x) && float(y) && string(button) && integer(count)),
        CoreHostBinding::AutomationType => {
            matches!(arguments, [text, delay] if string(text) && integer(delay))
        }
    };
    if !valid_arguments {
        return Err(host_binding_error(
            origin,
            format!("runtime arguments do not match the {binding:?} host ABI"),
        ));
    }

    let stream_kind_matches = match binding {
        CoreHostBinding::FileLinesNext | CoreHostBinding::FileLinesClose => matches!(
            arguments,
            [TypedValue::Stream {
                kind,
                element_type,
                ..
            }] if kind == "file-lines" && *element_type == Type::String
        ),
        CoreHostBinding::CsvNext | CoreHostBinding::CsvClose => matches!(
            arguments,
            [TypedValue::Stream {
                kind,
                element_type,
                ..
            }] if kind == "csv-records" && *element_type == Type::list(Type::String)
        ),
        _ => true,
    };
    if !stream_kind_matches {
        return Err(host_binding_error(
            origin,
            "stream resource kind does not match the registered host binding",
        ));
    }

    if matches!(
        binding,
        CoreHostBinding::FileRead
            | CoreHostBinding::FileHash
            | CoreHostBinding::TreeMerkle
            | CoreHostBinding::FileSize
            | CoreHostBinding::FileLinesOpen
            | CoreHostBinding::CsvOpen
            | CoreHostBinding::WorkbookOpen
            | CoreHostBinding::WorkbookSheets
            | CoreHostBinding::TreeList
            | CoreHostBinding::FileSlice
            | CoreHostBinding::WorkbookSheetOpen
            | CoreHostBinding::WorkbookRange
            | CoreHostBinding::WorkbookSummary
            | CoreHostBinding::CsvSummary
            | CoreHostBinding::FileWrite
    ) {
        let Some(TypedValue::Path {
            selector: runtime_selector,
            relative,
        }) = arguments.first()
        else {
            unreachable!("file binding ABI was validated above")
        };
        let ResourceSelector::File {
            selector: authorized_selector,
        } = &requirement.selector
        else {
            return Err(host_binding_error(
                origin,
                "file host binding requires a concrete file selector",
            ));
        };
        let exact_selector = crate::vm::FileSelectorTemplate {
            root: runtime_selector.root.clone(),
            parts: vec![crate::vm::FileSelectorTemplatePart::Argument {
                index: 0,
                bound: runtime_selector.clone(),
            }],
            upper_bound: runtime_selector.clone(),
        }
        .instantiate(arguments)
        .map_err(|error| host_binding_error(origin, error.to_string()))?;
        if &exact_selector != authorized_selector || !runtime_selector.matches(relative) {
            return Err(host_binding_error(
                origin,
                "runtime path does not match the authorized file selector",
            ));
        }
    }

    if binding == CoreHostBinding::ProcessRun {
        let TypedValue::String(command) = &arguments[0] else {
            unreachable!("validated above")
        };
        let ResourceSelector::Process { executables } = &requirement.selector else {
            return Err(host_binding_error(
                origin,
                "process selector is not concrete",
            ));
        };
        let [encoded] = executables.as_slice() else {
            return Err(host_binding_error(origin, "process selector is not exact"));
        };
        let identity = ProcessExecutableIdentity::decode(encoded)
            .map_err(|message| host_binding_error(origin, message))?;
        if identity.path != *command {
            return Err(host_binding_error(
                origin,
                "process command does not match the authorized executable identity",
            ));
        }
    }
    Ok(())
}

fn host_binding_error(
    origin: &crate::vm::SourceOrigin,
    message: impl Into<String>,
) -> VmDiagnostic {
    VmDiagnostic::error(
        "E-HOST-002",
        crate::vm::DiagnosticPhase::HostCall,
        message,
        Some(origin.clone()),
    )
}

fn validate_process_request(
    binding: Option<CoreHostBinding>,
    requirement: &CapabilityRequirement,
    command: &str,
    arguments: &[String],
    origin: &crate::vm::SourceOrigin,
) -> std::result::Result<OpenedProcessExecutable, VmDiagnostic> {
    if binding != Some(CoreHostBinding::ProcessRun) {
        return Err(host_binding_error(
            origin,
            "process execution requires the registered process-run host binding",
        ));
    }
    let ResourceSelector::Process { executables } = &requirement.selector else {
        return Err(host_binding_error(
            origin,
            "process-run reached the host without a concrete process selector",
        ));
    };
    let [encoded] = executables.as_slice() else {
        return Err(host_binding_error(
            origin,
            "process-run requires exactly one stable executable identity",
        ));
    };
    let approved = ProcessExecutableIdentity::decode(encoded)
        .map_err(|message| host_binding_error(origin, message))?;
    let current = open_process_executable(command, arguments)
        .map_err(|message| host_binding_error(origin, message))?;
    if approved != current.identity {
        return Err(host_binding_error(
            origin,
            "process-run executable identity no longer matches the authorized selector",
        ));
    }
    Ok(current)
}

const PROCESS_IDENTITY_PREFIX: &str = "finch-process-v3:";

fn validate_restored_process_authority(ledger: &CapabilityLedger) -> Result<()> {
    for grant in &ledger.grants.grants {
        if grant.requirement.capability != CapabilityKind::ProcessRun {
            continue;
        }
        let ResourceSelector::Process { executables } = &grant.requirement.selector else {
            bail!("stored process capability grant has an invalid selector");
        };
        for executable in executables {
            let approved = ProcessExecutableIdentity::decode(executable).map_err(|_| {
                anyhow::anyhow!(
                    "legacy process capability grant cannot be restored; reapprove the executable to bind a stable v3 invocation identity"
                )
            })?;
            let current = open_process_executable(&approved.path, &approved.arguments)
                .map_err(anyhow::Error::msg)?
                .identity;
            if current != approved {
                bail!("stored process capability identity changed since approval");
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ProcessExecutableIdentity {
    path: String,
    sha256: String,
    device: u64,
    inode: u64,
    arguments: Vec<String>,
    environment_sha256: String,
    cwd_path: String,
    cwd_device: u64,
    cwd_inode: u64,
}

struct OpenedProcessExecutable {
    identity: ProcessExecutableIdentity,
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    file: std::fs::File,
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    cwd: std::fs::File,
}

impl ProcessExecutableIdentity {
    fn encode(&self) -> String {
        format!(
            "{PROCESS_IDENTITY_PREFIX}{}",
            serde_json::to_string(self).expect("process identity is serializable")
        )
    }

    fn decode(encoded: &str) -> std::result::Result<Self, String> {
        let json = encoded
            .strip_prefix(PROCESS_IDENTITY_PREFIX)
            .ok_or_else(|| "process selector is not a stable executable identity".to_string())?;
        serde_json::from_str(json).map_err(|_| "process selector identity is malformed".into())
    }
}

/// Open an executable to the authority identity persisted in grants and
/// audit. Device/inode and content are derived from the same open descriptor
/// later passed to `fexecve`, so pathname replacement cannot change the
/// object selected for execution.
fn resolve_process_executable(
    command: &str,
) -> std::result::Result<ProcessExecutableIdentity, String> {
    Ok(open_process_executable(command, &[])?.identity)
}

fn open_process_executable(
    command: &str,
    arguments: &[String],
) -> std::result::Result<OpenedProcessExecutable, String> {
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    {
        let _ = (command, arguments);
        return Err(
            "process-run is unsupported on this platform because stable opened-object execution is unavailable"
                .into(),
        );
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    {
        use std::os::fd::FromRawFd;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

        let supplied = Path::new(command);
        if !supplied.is_absolute() {
            return Err("process-run executable must be an absolute canonical path; PATH and relative lookup are forbidden".into());
        }
        let canonical = std::fs::canonicalize(supplied).map_err(|error| {
            format!(
                "resolve process executable '{}': {error}",
                supplied.display()
            )
        })?;
        if canonical != supplied {
            return Err(
                "process-run executable must already be canonical and must not be a symlink".into(),
            );
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&canonical)
            .map_err(|error| {
                format!("open process executable '{}': {error}", canonical.display())
            })?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "inspect process executable '{}': {error}",
                canonical.display()
            )
        })?;
        if !metadata.is_file() {
            return Err("process-run executable must be a regular file".into());
        }
        let path_metadata = std::fs::metadata(&canonical).map_err(|error| {
            format!(
                "recheck process executable '{}': {error}",
                canonical.display()
            )
        })?;
        if metadata.dev() != path_metadata.dev() || metadata.ino() != path_metadata.ino() {
            return Err("process-run executable changed while it was being opened".into());
        }
        ensure_effective_execute_permission(&canonical, &metadata)?;
        let mut source = file.try_clone().map_err(|error| error.to_string())?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|error| error.to_string())?;
        let snapshot_directory = tempfile::tempdir()
            .map_err(|error| format!("create private executable snapshot directory: {error}"))?;
        let snapshot_path = snapshot_directory.path().join("executable");
        let mut snapshot_writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&snapshot_path)
            .map_err(|error| format!("create private executable snapshot: {error}"))?;
        std::io::copy(&mut source, &mut snapshot_writer)
            .map_err(|error| format!("snapshot process executable: {error}"))?;
        snapshot_writer
            .sync_all()
            .map_err(|error| format!("sync process executable snapshot: {error}"))?;
        snapshot_writer
            .set_permissions(std::fs::Permissions::from_mode(0o500))
            .map_err(|error| format!("seal process executable snapshot mode: {error}"))?;
        drop(snapshot_writer);
        let snapshot = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW)
            .open(&snapshot_path)
            .map_err(|error| format!("reopen sealed executable snapshot: {error}"))?;
        std::fs::remove_file(&snapshot_path)
            .map_err(|error| format!("unlink sealed executable snapshot: {error}"))?;
        drop(snapshot_directory);
        let digest = sha256_open_file(&snapshot)?;

        let cwd_path = std::env::current_dir()
            .map_err(|error| format!("resolve process working directory: {error}"))?;
        let cwd_fd = nix::fcntl::open(
            &cwd_path,
            nix::fcntl::OFlag::O_RDONLY
                | nix::fcntl::OFlag::O_DIRECTORY
                | nix::fcntl::OFlag::O_NOFOLLOW
                | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|error| format!("open process working directory: {error}"))?;
        let cwd = unsafe { std::fs::File::from_raw_fd(cwd_fd) };
        let cwd_metadata = cwd
            .metadata()
            .map_err(|error| format!("inspect process working directory: {error}"))?;
        let environment_sha256 = format!("{:x}", Sha256::digest(b""));
        Ok(OpenedProcessExecutable {
            identity: ProcessExecutableIdentity {
                path: canonical.to_string_lossy().into_owned(),
                sha256: hex_digest(&digest),
                device: metadata.dev(),
                inode: metadata.ino(),
                arguments: arguments.to_vec(),
                environment_sha256,
                cwd_path: cwd_path.to_string_lossy().into_owned(),
                cwd_device: cwd_metadata.dev(),
                cwd_inode: cwd_metadata.ino(),
            },
            file: snapshot,
            cwd,
        })
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn ensure_effective_execute_permission(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> std::result::Result<(), String> {
    use nix::unistd::{access, getegid, geteuid, getgroups, AccessFlags};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let mode = metadata.permissions().mode();
    let euid = geteuid().as_raw();
    let egid = getegid().as_raw();
    let executable = if euid == 0 {
        mode & 0o111 != 0
    } else if euid == metadata.uid() {
        mode & 0o100 != 0
    } else {
        let in_group = egid == metadata.gid()
            || getgroups()
                .map_err(|error| format!("read effective process groups: {error}"))?
                .iter()
                .any(|group| group.as_raw() == metadata.gid());
        if in_group {
            mode & 0o010 != 0
        } else {
            mode & 0o001 != 0
        }
    };
    if !executable {
        return Err("process-run executable is not executable by the effective user".into());
    }
    access(path, AccessFlags::X_OK).map_err(|error| {
        format!(
            "process-run executable is not executable by the effective user: launch access check failed: {error}"
        )
    })
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn sha256_open_file(file: &std::fs::File) -> std::result::Result<[u8; 32], String> {
    let mut file = file.try_clone().map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
type ProcessBeforeExecHook = (String, Box<dyn FnOnce() + Send>);

#[cfg(test)]
type AuthorizationBeforeUseHook = (uuid::Uuid, CapabilityKind, Box<dyn FnOnce() + Send>);

#[cfg(test)]
static AUTHORIZATION_BEFORE_USE_HOOK: std::sync::OnceLock<Mutex<Vec<AuthorizationBeforeUseHook>>> =
    std::sync::OnceLock::new();

#[cfg(test)]
static AUTHORIZATION_BEFORE_LEASE_HOOK: std::sync::OnceLock<
    Mutex<Vec<AuthorizationBeforeUseHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn run_authorization_hook(
    hooks: &std::sync::OnceLock<Mutex<Vec<AuthorizationBeforeUseHook>>>,
    execution_id: uuid::Uuid,
    capability: CapabilityKind,
) {
    let mut hook = hooks
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = hook
        .iter()
        .position(|(expected_execution, expected_capability, _)| {
            *expected_execution == execution_id && *expected_capability == capability
        })
    {
        let (_, _, callback) = hook.remove(index);
        callback();
    }
}

#[cfg(test)]
fn run_authorization_before_use_hook(execution_id: uuid::Uuid, capability: CapabilityKind) {
    run_authorization_hook(&AUTHORIZATION_BEFORE_USE_HOOK, execution_id, capability);
}

#[cfg(test)]
fn run_authorization_before_lease_hook(execution_id: uuid::Uuid, capability: CapabilityKind) {
    run_authorization_hook(&AUTHORIZATION_BEFORE_LEASE_HOOK, execution_id, capability);
}

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
static PROCESS_BEFORE_EXEC_HOOK: std::sync::OnceLock<Mutex<Option<ProcessBeforeExecHook>>> =
    std::sync::OnceLock::new();

#[cfg(all(
    test,
    any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )
))]
fn run_process_before_exec_hook(path: &str) {
    let mut hook = PROCESS_BEFORE_EXEC_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("process before-exec test hook lock");
    if hook.as_ref().is_some_and(|(expected, _)| expected == path) {
        let (_, callback) = hook.take().expect("matching process hook");
        callback();
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn spawn_open_process(
    executable: OpenedProcessExecutable,
) -> std::result::Result<std::process::Child, String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::CommandExt;

    let argv = std::iter::once(executable.identity.path.as_str())
        .chain(executable.identity.arguments.iter().map(String::as_str))
        .map(|value| CString::new(value).map_err(|_| "process argument contains NUL".to_string()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let environment = Vec::<CString>::new();
    let fd = executable.file.as_raw_fd();
    let cwd_fd = executable.cwd.as_raw_fd();
    #[cfg(test)]
    run_process_before_exec_hook(&executable.identity.path);
    let mut command = std::process::Command::new("/bin/false");
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            nix::unistd::fchdir(cwd_fd)
                .map_err(|error| std::io::Error::from_raw_os_error(error as i32))?;
            match nix::unistd::fexecve(fd, &argv, &environment) {
                Ok(never) => match never {},
                Err(error) => Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        });
    }
    command.spawn().map_err(|error| error.to_string())
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "dragonfly"
)))]
fn spawn_open_process(
    _executable: OpenedProcessExecutable,
) -> std::result::Result<std::process::Child, String> {
    Err("process-run is unsupported on this platform".into())
}

fn normalize_process_grant(
    requirement: &mut CapabilityRequirement,
) -> std::result::Result<(), String> {
    if requirement.capability != CapabilityKind::ProcessRun {
        return Ok(());
    }
    let ResourceSelector::Process { executables } = &mut requirement.selector else {
        return Err("process-run grants require a typed process selector".into());
    };
    for executable in executables.iter_mut() {
        if executable.starts_with(PROCESS_IDENTITY_PREFIX) {
            let approved = ProcessExecutableIdentity::decode(executable)?;
            let current = open_process_executable(&approved.path, &approved.arguments)?.identity;
            if approved != current {
                return Err("process grant identity does not match the current executable".into());
            }
        } else {
            *executable = resolve_process_executable(executable)?.encode();
        }
    }
    Ok(())
}

fn validate_process_effect(effect: &VmSideEffect) -> std::result::Result<(), VmDiagnostic> {
    let binding = registered_host_binding(&effect.requirement, &effect.origin)?;
    let crate::vm::interpreter::HostSideEffect::Request { arguments } = &effect.event else {
        return Err(host_binding_error(
            &effect.origin,
            "process-run requires a typed host request",
        ));
    };
    let [TypedValue::String(command), TypedValue::List { values, .. }] = arguments.as_slice()
    else {
        return Err(host_binding_error(
            &effect.origin,
            "process-run requires an executable and string arguments",
        ));
    };
    let process_arguments = values
        .iter()
        .map(|value| match value {
            TypedValue::String(value) => Ok(value.clone()),
            _ => Err(host_binding_error(
                &effect.origin,
                "process-run arguments must be strings",
            )),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    validate_process_request(
        binding,
        &effect.requirement,
        command,
        &process_arguments,
        &effect.origin,
    )
    .map(|_| ())
}

/// Convert the statically admitted MCP input subset into JSON. Optional
/// record fields represented by `none` are omitted rather than serialized as
/// null; explicit JSON callers retain full control through generic mcp-call.
fn typed_mcp_arguments(value: &TypedValue) -> std::result::Result<serde_json::Value, String> {
    match value {
        TypedValue::Unit => Ok(serde_json::Value::Null),
        TypedValue::Bool(value) => Ok((*value).into()),
        TypedValue::Int(value) => Ok((*value).into()),
        TypedValue::UInt(value) => Ok((*value).into()),
        TypedValue::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .ok_or_else(|| "non-finite floats are not valid JSON".into()),
        TypedValue::Char(value) => Ok(value.to_string().into()),
        TypedValue::String(value) | TypedValue::Symbol(value) => Ok(value.clone().into()),
        TypedValue::Json(value) => Ok(value.clone()),
        TypedValue::List { values, .. } => values
            .iter()
            .map(typed_mcp_arguments)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        TypedValue::Option {
            value: Some(value), ..
        } => typed_mcp_arguments(value),
        TypedValue::Option { value: None, .. } => Ok(serde_json::Value::Null),
        TypedValue::Record(fields) => {
            let mut object = serde_json::Map::new();
            for (name, value) in fields {
                if matches!(value, TypedValue::Option { value: None, .. }) {
                    continue;
                }
                object.insert(name.clone(), typed_mcp_arguments(value)?);
            }
            Ok(serde_json::Value::Object(object))
        }
        _ => Err(format!(
            "typed value {} is outside the admitted MCP JSON subset",
            value.value_type()
        )),
    }
}

/// Hash a file in bounded chunks, keeping large inputs out of VM memory.
#[cfg(any())]
fn sha256_file(path: &Path) -> std::result::Result<[u8; 32], String> {
    let mut file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

fn sha256_file_handle(file: &std::fs::File) -> std::result::Result<[u8; 32], String> {
    let mut file = file.try_clone().map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

fn hex_digest(digest: &[u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Return the lexicographically first bounded slice of a directory tree.
///
/// The priority queue makes traversal order independent of host `read_dir`
/// order while retaining a strict memory/entry bound. Symlinks are rejected
/// instead of followed so an authorized workspace selector cannot escape
/// through mutable filesystem topology after verification.
fn list_directory_tree(
    root: std::fs::File,
    maximum_entries: usize,
) -> std::result::Result<(Vec<TypedValue>, bool), String> {
    #[cfg(not(unix))]
    {
        let _ = (root, maximum_entries);
        return Err("descriptor-relative tree listing is unsupported on this platform".into());
    }
    #[cfg(unix)]
    {
        let mut directory = nix::dir::Dir::from(root).map_err(|error| error.to_string())?;
        let mut raw_entries = Vec::new();
        let mut scanned = 0usize;
        walk_directory_for_listing(&mut directory, "", &mut raw_entries, &mut scanned, 100_000)?;
        raw_entries.sort_by(|left, right| left.0.cmp(&right.0));
        let truncated = raw_entries.len() > maximum_entries;
        raw_entries.truncate(maximum_entries);
        return Ok((
            raw_entries
                .into_iter()
                .map(|(relative, kind, size)| {
                    TypedValue::Record(vec![
                        ("path".into(), TypedValue::String(relative)),
                        ("kind".into(), TypedValue::String(kind)),
                        ("size".into(), TypedValue::Int(size)),
                    ])
                })
                .collect(),
            truncated,
        ));
    }
    #[cfg(any())]
    {
        const MAX_SCANNED_DIRECTORY_ENTRIES: usize = 100_000;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("tree-list rejects a symlink root".into());
        }
        if !metadata.is_dir() {
            return Err("tree-list requires a directory path".into());
        }

        let mut pending = BinaryHeap::new();
        let mut scanned = 0;
        enqueue_directory_entries(
            root,
            root,
            &mut pending,
            &mut scanned,
            MAX_SCANNED_DIRECTORY_ENTRIES,
        )?;
        let mut entries = Vec::with_capacity(maximum_entries.min(pending.len()));

        while entries.len() < maximum_entries {
            let Some(Reverse((relative, path))) = pending.pop() else {
                break;
            };
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("tree-list rejects symlink '{relative}'"));
            }
            let (kind, size) = if metadata.is_dir() {
                enqueue_directory_entries(
                    root,
                    &path,
                    &mut pending,
                    &mut scanned,
                    MAX_SCANNED_DIRECTORY_ENTRIES,
                )?;
                ("directory", 0)
            } else if metadata.is_file() {
                let size = i64::try_from(metadata.len()).map_err(|_| {
                    format!("tree-list file '{relative}' is too large to represent")
                })?;
                ("file", size)
            } else {
                return Err(format!("tree-list rejects unsupported entry '{relative}'"));
            };
            entries.push(TypedValue::Record(vec![
                ("path".into(), TypedValue::String(relative)),
                ("kind".into(), TypedValue::String(kind.into())),
                ("size".into(), TypedValue::Int(size)),
            ]));
        }
        Ok((entries, !pending.is_empty()))
    }
}

#[cfg(unix)]
fn walk_directory_for_listing(
    directory: &mut nix::dir::Dir,
    prefix: &str,
    entries: &mut Vec<(String, String, i64)>,
    scanned: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    use nix::dir::Dir;
    use nix::fcntl::{AtFlags, OFlag};
    use nix::sys::stat::{fstat, fstatat, Mode, SFlag};
    use std::os::fd::AsRawFd;
    use std::os::fd::FromRawFd;

    let mut names = directory
        .iter()
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                return Ok(None);
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "tree-list cannot represent a non-UTF-8 path".to_string())?;
            Ok(Some(name.to_owned()))
        })
        .collect::<std::result::Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        *scanned += 1;
        if *scanned > maximum_entries {
            return Err(format!(
                "tree-list exceeds the {maximum_entries} scanned-entry limit"
            ));
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let stat = fstatat(
            Some(directory.as_raw_fd()),
            name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| error.to_string())?;
        let kind = SFlag::from_bits_truncate(stat.st_mode);
        if kind.contains(SFlag::S_IFLNK) {
            return Err(format!("tree-list rejects symlink '{relative}'"));
        }
        if kind.contains(SFlag::S_IFDIR) {
            let mut child = Dir::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            entries.push((relative.clone(), "directory".into(), 0));
            walk_directory_for_listing(&mut child, &relative, entries, scanned, maximum_entries)?;
        } else if kind.contains(SFlag::S_IFREG) {
            let fd = nix::fcntl::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            let opened = fstat(file.as_raw_fd()).map_err(|error| error.to_string())?;
            if !SFlag::from_bits_truncate(opened.st_mode).contains(SFlag::S_IFREG) {
                return Err(format!(
                    "tree-list entry changed while opening '{relative}'"
                ));
            }
            let size = i64::try_from(opened.st_size)
                .map_err(|_| format!("tree-list file '{relative}' is too large to represent"))?;
            entries.push((relative, "file".into(), size));
        } else {
            return Err(format!("tree-list rejects unsupported entry '{relative}'"));
        }
    }
    Ok(())
}

#[cfg(any())]
fn enqueue_directory_entries(
    root: &Path,
    directory: &Path,
    pending: &mut BinaryHeap<Reverse<(String, PathBuf)>>,
    scanned: &mut usize,
    maximum_scanned_entries: usize,
) -> std::result::Result<(), String> {
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        *scanned += 1;
        if *scanned > maximum_scanned_entries {
            return Err(format!(
                "tree-list exceeds the {maximum_scanned_entries} scanned-entry limit"
            ));
        }
        let path = entry.map_err(|error| error.to_string())?.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_str()
            .ok_or_else(|| "tree-list cannot represent a non-UTF-8 path".to_string())?
            .replace(std::path::MAIN_SEPARATOR, "/");
        pending.push(Reverse((relative, path)));
    }
    Ok(())
}

/// Compute a bounded, deterministic digest for a directory tree.  This is an
/// inventory primitive: it deliberately rejects symlinks rather than trying
/// to make a potentially escaping traversal appear safe.
fn merkle_directory(root: std::fs::File) -> std::result::Result<String, String> {
    #[cfg(not(unix))]
    {
        let _ = root;
        return Err("descriptor-relative tree hashing is unsupported on this platform".into());
    }
    #[cfg(unix)]
    {
        let mut directory = nix::dir::Dir::from(root).map_err(|error| error.to_string())?;
        let mut hasher = Sha256::new();
        let mut entries = 0usize;
        merkle_walk_descriptor(&mut directory, "", &mut hasher, &mut entries, 100_000)?;
        return Ok(format!("{:x}", hasher.finalize()));
    }
    #[cfg(any())]
    {
        const MAX_TREE_MERKLE_ENTRIES: usize = 100_000;
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("tree-merkle rejects a symlink root".into());
        }
        if !metadata.is_dir() {
            return Err("tree-merkle requires a directory path".into());
        }

        let mut hasher = Sha256::new();
        let mut entries = 0;
        merkle_walk(
            root,
            root,
            &mut hasher,
            &mut entries,
            MAX_TREE_MERKLE_ENTRIES,
        )?;
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[cfg(unix)]
fn merkle_walk_descriptor(
    directory: &mut nix::dir::Dir,
    prefix: &str,
    hasher: &mut Sha256,
    entries: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    use nix::dir::Dir;
    use nix::fcntl::{AtFlags, OFlag};
    use nix::sys::stat::{fstatat, Mode, SFlag};
    use std::os::fd::{AsRawFd, FromRawFd};

    let mut names = directory
        .iter()
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            let bytes = entry.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                return Ok(None);
            }
            let name = std::str::from_utf8(bytes)
                .map_err(|_| "tree-merkle cannot represent a non-UTF-8 path".to_string())?;
            Ok(Some(name.to_owned()))
        })
        .collect::<std::result::Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    names.sort();
    for name in names {
        *entries += 1;
        if *entries > maximum_entries {
            return Err(format!(
                "tree-merkle exceeds the {maximum_entries}-entry traversal limit"
            ));
        }
        let relative = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let stat = fstatat(
            Some(directory.as_raw_fd()),
            name.as_str(),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|error| error.to_string())?;
        let kind = SFlag::from_bits_truncate(stat.st_mode);
        if kind.contains(SFlag::S_IFLNK) {
            return Err(format!("tree-merkle rejects symlink '{relative}'"));
        }
        if kind.contains(SFlag::S_IFDIR) {
            hasher.update(b"directory\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            let mut child = Dir::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            merkle_walk_descriptor(&mut child, &relative, hasher, entries, maximum_entries)?;
        } else if kind.contains(SFlag::S_IFREG) {
            let fd = nix::fcntl::openat(
                Some(directory.as_raw_fd()),
                name.as_str(),
                OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|error| error.to_string())?;
            let file = unsafe { std::fs::File::from_raw_fd(fd) };
            hasher.update(b"file\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            hasher.update(sha256_file_handle(&file)?);
        } else {
            return Err(format!(
                "tree-merkle rejects unsupported entry '{relative}'"
            ));
        }
    }
    Ok(())
}

#[cfg(any())]
fn merkle_walk(
    root: &Path,
    directory: &Path,
    hasher: &mut Sha256,
    entries: &mut usize,
    maximum_entries: usize,
) -> std::result::Result<(), String> {
    let mut paths = std::fs::read_dir(directory)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| error.to_string())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    paths.sort();

    for path in paths {
        *entries += 1;
        if *entries > maximum_entries {
            return Err(format!(
                "tree-merkle exceeds the {maximum_entries}-entry traversal limit"
            ));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|error| error.to_string())?
            .to_string_lossy();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Err(format!("tree-merkle rejects symlink '{}'", relative));
        }
        if metadata.is_dir() {
            hasher.update(b"directory\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            merkle_walk(root, &path, hasher, entries, maximum_entries)?;
        } else if metadata.is_file() {
            hasher.update(b"file\0");
            hasher.update(relative.as_bytes());
            hasher.update(b"\0");
            hasher.update(sha256_file(&path)?);
        } else {
            return Err(format!(
                "tree-merkle rejects unsupported entry '{}'",
                relative
            ));
        }
    }
    Ok(())
}

/// Open one workbook sheet behind the ordinary host-issued stream lifecycle.
/// Calamine owns format decoding; only one row crosses into the VM on each
/// `stream-next`. The initial backend retains decoded rows because calamine's
/// stable public API exposes an owned worksheet range rather than a streaming
/// XML reader. The explicit cell bound prevents that host projection from
/// becoming an unbounded second copy.
fn read_workbook_rows(
    file: &std::fs::File,
    label: &str,
    requested_sheet: Option<&str>,
) -> std::result::Result<Vec<Vec<String>>, String> {
    use calamine::{open_workbook_auto_from_rs, Reader};

    const MAX_WORKBOOK_CELLS: usize = 10_000_000;
    const MAX_WORKBOOK_BYTES: u64 = 512 * 1024 * 1024;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_WORKBOOK_BYTES {
        return Err(format!(
            "workbook exceeds the {MAX_WORKBOOK_BYTES}-byte snapshot limit"
        ));
    }
    let mut snapshot = file.try_clone().map_err(|error| error.to_string())?;
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(size as usize);
    snapshot
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let reader = std::io::Cursor::new(bytes);
    let mut workbook = open_workbook_auto_from_rs(reader)
        .map_err(|error| format!("cannot open workbook '{label}': {error}"))?;
    let sheet = requested_sheet
        .map(str::to_owned)
        .or_else(|| workbook.sheet_names().first().cloned())
        .ok_or_else(|| format!("workbook '{label}' has no sheets"))?;
    let range = workbook.worksheet_range(&sheet).map_err(|error| {
        format!(
            "cannot read workbook sheet '{}' from '{}': {error}",
            sheet, label
        )
    })?;
    let mut cell_count = 0usize;
    let mut rows = Vec::new();
    for row in range.rows() {
        cell_count = cell_count
            .checked_add(row.len())
            .ok_or_else(|| "workbook cell count overflowed".to_string())?;
        if cell_count > MAX_WORKBOOK_CELLS {
            return Err(format!(
                "workbook sheet exceeds the {MAX_WORKBOOK_CELLS}-cell host cursor limit"
            ));
        }
        rows.push(row.iter().map(workbook_cell_to_string).collect());
    }
    Ok(rows)
}

fn workbook_cell_to_string(cell: &calamine::Data) -> String {
    use calamine::Data;

    match cell {
        Data::Empty => String::new(),
        Data::String(value) => value.clone(),
        Data::Float(value) if value.fract() == 0.0 => format!("{}", *value as i64),
        Data::Float(value) => value.to_string(),
        Data::Int(value) => value.to_string(),
        Data::Bool(value) => value.to_string(),
        Data::Error(value) => format!("#ERR:{value:?}"),
        other => other.to_string(),
    }
}

/// Materialize only a deliberately small rectangular projection into the VM.
/// Calamine currently decodes the owning sheet first, so this bounds the
/// language-visible result rather than claiming to be a streaming decoder.
fn read_workbook_range(
    file: &std::fs::File,
    label: &str,
    sheet: &str,
    start_row: i64,
    start_column: i64,
    row_count: i64,
    column_count: i64,
) -> std::result::Result<Vec<Vec<String>>, String> {
    const MAX_WORKBOOK_RANGE_CELLS: usize = 10_000;

    let start_row = usize::try_from(start_row)
        .map_err(|_| "workbook-range start row must be non-negative".to_string())?;
    let start_column = usize::try_from(start_column)
        .map_err(|_| "workbook-range start column must be non-negative".to_string())?;
    let row_count = usize::try_from(row_count)
        .map_err(|_| "workbook-range row count must be positive".to_string())?;
    let column_count = usize::try_from(column_count)
        .map_err(|_| "workbook-range column count must be positive".to_string())?;
    if row_count == 0 || column_count == 0 {
        return Err("workbook-range row and column counts must be positive".into());
    }
    let cells = row_count
        .checked_mul(column_count)
        .ok_or_else(|| "workbook-range cell count overflowed".to_string())?;
    if cells > MAX_WORKBOOK_RANGE_CELLS {
        return Err(format!(
            "workbook-range exceeds the {MAX_WORKBOOK_RANGE_CELLS}-cell result limit"
        ));
    }

    let rows = read_workbook_rows(file, label, Some(sheet))?;
    Ok(rows
        .iter()
        .skip(start_row)
        .take(row_count)
        .map(|row| {
            (start_column..start_column.saturating_add(column_count))
                .map(|column| row.get(column).cloned().unwrap_or_default())
                .collect()
        })
        .collect())
}

/// Produce the same bounded per-column facts as `csv-summary`, treating the
/// first worksheet row as headers and never returning source rows.
fn summarize_workbook(
    file: &std::fs::File,
    label: &str,
    sheet: &str,
    max_rows: usize,
) -> std::result::Result<serde_json::Value, String> {
    const MAX_WORKBOOK_COLUMNS: usize = 4096;

    let rows = read_workbook_rows(file, label, Some(sheet))?;
    let (headers, data_rows) = rows
        .split_first()
        .ok_or_else(|| "workbook-summary requires a header row".to_string())?;
    if headers.len() > MAX_WORKBOOK_COLUMNS {
        return Err(format!(
            "workbook header exceeds the {MAX_WORKBOOK_COLUMNS}-column summary limit"
        ));
    }

    #[derive(Default)]
    struct ColumnSummary {
        empty: u64,
        non_empty: u64,
        numeric: u64,
        sum: f64,
        min: Option<f64>,
        max: Option<f64>,
    }

    let mut columns: Vec<ColumnSummary> = (0..headers.len())
        .map(|_| ColumnSummary::default())
        .collect();
    let sampled_rows = data_rows.len().min(max_rows);
    for (row_index, row) in data_rows.iter().take(sampled_rows).enumerate() {
        if row.len() > headers.len() {
            return Err(format!(
                "workbook data row {} has {} cells but the header declares {}",
                row_index + 1,
                row.len(),
                headers.len()
            ));
        }
        for (column, summary) in columns.iter_mut().enumerate() {
            let field = row.get(column).map(String::as_str).unwrap_or("").trim();
            if field.is_empty() {
                summary.empty += 1;
                continue;
            }
            summary.non_empty += 1;
            if let Ok(value) = field.parse::<f64>() {
                if value.is_finite() {
                    summary.numeric += 1;
                    summary.sum += value;
                    summary.min = Some(summary.min.map_or(value, |current| current.min(value)));
                    summary.max = Some(summary.max.map_or(value, |current| current.max(value)));
                }
            }
        }
    }

    let columns = headers
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(index, (name, summary))| {
            let mean = (summary.numeric != 0)
                .then(|| summary.sum / summary.numeric as f64)
                .filter(|value| value.is_finite());
            serde_json::json!({
                "index": index,
                "name": name,
                "empty": summary.empty,
                "non_empty": summary.non_empty,
                "numeric": summary.numeric,
                "min": summary.min,
                "max": summary.max,
                "mean": mean,
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "sheet": sheet,
        "headers": headers,
        "sampled_rows": sampled_rows,
        "truncated": data_rows.len() > sampled_rows,
        "columns": columns,
    }))
}

fn read_workbook_sheet_names(
    file: &std::fs::File,
    label: &str,
) -> std::result::Result<Vec<String>, String> {
    use calamine::{open_workbook_auto_from_rs, Reader};

    const MAX_WORKBOOK_BYTES: u64 = 512 * 1024 * 1024;
    let size = file.metadata().map_err(|error| error.to_string())?.len();
    if size > MAX_WORKBOOK_BYTES {
        return Err(format!(
            "workbook exceeds the {MAX_WORKBOOK_BYTES}-byte snapshot limit"
        ));
    }
    let mut snapshot = file.try_clone().map_err(|error| error.to_string())?;
    snapshot
        .seek(SeekFrom::Start(0))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::with_capacity(size as usize);
    snapshot
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    let workbook = open_workbook_auto_from_rs(std::io::Cursor::new(bytes))
        .map_err(|error| format!("cannot open workbook '{label}': {error}"))?;
    Ok(workbook.sheet_names().to_vec())
}

/// Read one UTF-8 line without allowing a malformed/hostile record to grow
/// the VM's resident memory without bound. Newline and an optional preceding
/// CR are framing bytes, not part of the returned string.
fn read_bounded_utf8_line(
    reader: &mut BufReader<std::fs::File>,
) -> std::result::Result<Option<String>, String> {
    const MAX_FILE_LINE_BYTES: usize = 1024 * 1024;
    let mut line = Vec::new();
    loop {
        let (take, complete) = {
            let available = reader.fill_buf().map_err(|error| error.to_string())?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(None);
                }
                (0, true)
            } else if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                let take = newline + 1;
                // A CR immediately before LF is framing, even when the two
                // bytes straddle `BufReader` buffers. It must not consume the
                // advertised content budget.
                let has_cr_before_newline = if newline == 0 {
                    line.last() == Some(&b'\r')
                } else {
                    available[newline - 1] == b'\r'
                };
                let content_len = line.len() + newline - usize::from(has_cr_before_newline);
                if content_len > MAX_FILE_LINE_BYTES {
                    return Err(format!(
                        "file line exceeds the {MAX_FILE_LINE_BYTES}-byte per-line limit"
                    ));
                }
                line.extend_from_slice(&available[..take]);
                (take, true)
            } else {
                // Retain one possible trailing CR until the following buffer
                // tells us whether it forms CRLF framing.
                if line.len() + available.len() > MAX_FILE_LINE_BYTES + 1 {
                    return Err(format!(
                        "file line exceeds the {MAX_FILE_LINE_BYTES}-byte per-line limit"
                    ));
                }
                line.extend_from_slice(available);
                (available.len(), false)
            }
        };
        if take != 0 {
            reader.consume(take);
        }
        if complete {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return String::from_utf8(line).map(Some).map_err(|_| {
                "file line is not valid UTF-8; use file-slice for binary data".into()
            });
        }
    }
}

/// Read one RFC-4180-style CSV record without assuming that a physical line
/// is a record. Quoted fields may contain commas and newlines; doubled quotes
/// are unescaped. The cursor retains its buffered file position between calls
/// and never materializes more than one bounded record in the VM.
fn read_bounded_csv_record(
    reader: &mut BufReader<std::fs::File>,
) -> std::result::Result<Option<Vec<String>>, String> {
    const MAX_CSV_RECORD_BYTES: usize = 8 * 1024 * 1024;

    let mut fields = Vec::new();
    let mut field = Vec::new();
    let mut saw_byte = false;
    let mut in_quotes = false;
    let mut closed_quote = false;
    let mut record_bytes = 0usize;

    loop {
        let mut byte = [0u8; 1];
        let count = reader.read(&mut byte).map_err(|error| error.to_string())?;
        if count == 0 {
            if !saw_byte {
                return Ok(None);
            }
            if in_quotes {
                return Err("CSV record ends inside a quoted field".into());
            }
            fields.push(csv_field_to_string(field)?);
            return Ok(Some(fields));
        }
        saw_byte = true;
        record_bytes += 1;
        if record_bytes > MAX_CSV_RECORD_BYTES {
            return Err(format!(
                "CSV record exceeds the {MAX_CSV_RECORD_BYTES}-byte per-record limit"
            ));
        }

        let byte = byte[0];
        if in_quotes {
            if byte == b'"' {
                let is_escaped_quote = reader
                    .fill_buf()
                    .map_err(|error| error.to_string())?
                    .first()
                    == Some(&b'"');
                if is_escaped_quote {
                    reader.consume(1);
                    record_bytes += 1;
                    if record_bytes > MAX_CSV_RECORD_BYTES {
                        return Err(format!(
                            "CSV record exceeds the {MAX_CSV_RECORD_BYTES}-byte per-record limit"
                        ));
                    }
                    field.push(b'"');
                } else {
                    in_quotes = false;
                    closed_quote = true;
                }
            } else {
                field.push(byte);
            }
            continue;
        }

        if closed_quote {
            match byte {
                b',' => {
                    fields.push(csv_field_to_string(std::mem::take(&mut field))?);
                    closed_quote = false;
                }
                b'\n' => {
                    fields.push(csv_field_to_string(field)?);
                    return Ok(Some(fields));
                }
                b'\r' => {
                    if reader
                        .fill_buf()
                        .map_err(|error| error.to_string())?
                        .first()
                        == Some(&b'\n')
                    {
                        reader.consume(1);
                    }
                    fields.push(csv_field_to_string(field)?);
                    return Ok(Some(fields));
                }
                _ => {
                    return Err(
                        "unexpected data after closing quote in a CSV field; expected comma or record terminator"
                            .into(),
                    )
                }
            }
            continue;
        }

        match byte {
            b'"' if field.is_empty() => in_quotes = true,
            b'"' => return Err("unexpected quote in an unquoted CSV field".into()),
            b',' => fields.push(csv_field_to_string(std::mem::take(&mut field))?),
            b'\n' => {
                fields.push(csv_field_to_string(field)?);
                return Ok(Some(fields));
            }
            b'\r' => {
                if reader
                    .fill_buf()
                    .map_err(|error| error.to_string())?
                    .first()
                    == Some(&b'\n')
                {
                    reader.consume(1);
                }
                fields.push(csv_field_to_string(field)?);
                return Ok(Some(fields));
            }
            other => field.push(other),
        }
    }
}

fn csv_field_to_string(field: Vec<u8>) -> std::result::Result<String, String> {
    String::from_utf8(field)
        .map_err(|_| "CSV field is not valid UTF-8; use file-slice for binary data".into())
}

/// Compute bounded, model-friendly CSV facts without retaining source records.
/// The first record is the header. Every subsequent record must fit that declared
/// width; short rows contribute empty trailing fields. One extra record is read
/// only to report whether the requested sample was truncated.
fn summarize_csv(
    mut reader: BufReader<std::fs::File>,
    max_rows: usize,
) -> std::result::Result<serde_json::Value, String> {
    const MAX_CSV_COLUMNS: usize = 4096;

    let headers = read_bounded_csv_record(&mut reader)?
        .ok_or_else(|| "csv-summary requires a header record".to_string())?;
    if headers.len() > MAX_CSV_COLUMNS {
        return Err(format!(
            "CSV header exceeds the {MAX_CSV_COLUMNS}-column summary limit"
        ));
    }

    #[derive(Default)]
    struct ColumnSummary {
        empty: u64,
        non_empty: u64,
        numeric: u64,
        sum: f64,
        min: Option<f64>,
        max: Option<f64>,
    }

    let mut columns: Vec<ColumnSummary> = (0..headers.len())
        .map(|_| ColumnSummary::default())
        .collect();
    let mut sampled_rows = 0usize;
    while sampled_rows < max_rows {
        let Some(record) = read_bounded_csv_record(&mut reader)? else {
            break;
        };
        if record.len() > headers.len() {
            return Err(format!(
                "CSV data row {} has {} fields but the header declares {}",
                sampled_rows + 1,
                record.len(),
                headers.len()
            ));
        }
        for (index, summary) in columns.iter_mut().enumerate() {
            let field = record.get(index).map(String::as_str).unwrap_or("").trim();
            if field.is_empty() {
                summary.empty += 1;
                continue;
            }
            summary.non_empty += 1;
            if let Ok(value) = field.parse::<f64>() {
                if value.is_finite() {
                    summary.numeric += 1;
                    summary.sum += value;
                    summary.min = Some(summary.min.map_or(value, |current| current.min(value)));
                    summary.max = Some(summary.max.map_or(value, |current| current.max(value)));
                }
            }
        }
        sampled_rows += 1;
    }
    let truncated = read_bounded_csv_record(&mut reader)?.is_some();

    let columns = headers
        .iter()
        .zip(columns)
        .enumerate()
        .map(|(index, (name, summary))| {
            let mean = (summary.numeric != 0)
                .then(|| summary.sum / summary.numeric as f64)
                .filter(|value| value.is_finite());
            serde_json::json!({
                "index": index,
                "name": name,
                "empty": summary.empty,
                "non_empty": summary.non_empty,
                "numeric": summary.numeric,
                "min": summary.min,
                "max": summary.max,
                "mean": mean,
            })
        })
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "headers": headers,
        "sampled_rows": sampled_rows,
        "truncated": truncated,
        "columns": columns,
    }))
}

fn block_on_host<F, T>(future: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("typed host requires a Tokio runtime"))?;
    std::thread::scope(|scope| {
        scope
            .spawn(move || handle.block_on(future))
            .join()
            .map_err(|_| anyhow::anyhow!("typed host worker panicked"))?
    })
}

fn authority_state_from_parts(
    session_id: uuid::Uuid,
    project_id: String,
    policy: CapabilityPolicy,
    ledger: CapabilityLedger,
    roots: &ResourceRootState,
) -> ProgramRuntimeAuthorityState {
    ProgramRuntimeAuthorityState {
        format_version: PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION,
        session_id,
        project_id,
        policy,
        ledger,
        resource_roots: roots
            .bindings
            .values()
            .map(|binding| binding.as_ref().clone())
            .collect(),
        resource_root_audit: roots.audit.clone(),
    }
}

fn resource_root_binding_record(
    root: crate::vm::ResourceRoot,
    supplied: &Path,
    generation: u64,
    whole_machine: bool,
    bound_at_unix_ms: u64,
) -> Result<ResourceRootBindingRecord> {
    let canonical = supplied
        .canonicalize()
        .with_context(|| format!("resolve resource root '{}'", supplied.display()))?;
    let metadata = std::fs::symlink_metadata(&canonical)
        .with_context(|| format!("inspect resource root '{}'", canonical.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "resource root is not a stable directory: {}",
            canonical.display()
        );
    }
    #[cfg(unix)]
    let (device, inode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let (device, inode) = (0, 0);
    Ok(ResourceRootBindingRecord {
        root,
        path: canonical,
        device,
        inode,
        generation,
        whole_machine,
        bound_at_unix_ms,
    })
}

fn validate_resource_root_authority(
    bindings: &[ResourceRootBindingRecord],
    audit: &[ResourceRootAuditEntry],
) -> Result<ResourceRootState> {
    let mut replayed = BTreeMap::<crate::vm::ResourceRoot, (u64, PathBuf, bool, u64)>::new();
    let mut next_generation = 1_u64;
    let mut previous_time = 0_u64;
    for (index, entry) in audit.iter().enumerate() {
        if entry.sequence != index as u64 + 1
            || entry.generation == 0
            || entry.actor.trim().is_empty()
            || entry.at_unix_ms < previous_time
        {
            bail!("resource-root audit sequence is malformed");
        }
        previous_time = entry.at_unix_ms;
        if entry.whole_machine
            && (entry.root != crate::vm::ResourceRoot::HostMachine || entry.path != Path::new("/"))
        {
            bail!("resource-root audit contains an invalid whole-machine binding");
        }
        match entry.action {
            ResourceRootAuditAction::Bound => {
                if entry.generation != next_generation || replayed.contains_key(&entry.root) {
                    bail!("resource-root audit contains an invalid bind transition");
                }
                replayed.insert(
                    entry.root.clone(),
                    (
                        entry.generation,
                        entry.path.clone(),
                        entry.whole_machine,
                        entry.at_unix_ms,
                    ),
                );
                next_generation = next_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow::anyhow!("resource-root generation overflow"))?;
            }
            ResourceRootAuditAction::Revoked => {
                let Some((generation, path, whole_machine, _)) = replayed.remove(&entry.root)
                else {
                    bail!("resource-root audit revokes an inactive root");
                };
                if generation != entry.generation
                    || path != entry.path
                    || whole_machine != entry.whole_machine
                {
                    bail!("resource-root audit revoke does not match the active binding");
                }
            }
        }
    }
    let mut active = BTreeMap::new();
    for binding in bindings {
        if binding.generation == 0 {
            bail!("resource-root generation must be non-zero");
        }
        if binding.whole_machine
            && (binding.root != crate::vm::ResourceRoot::HostMachine
                || binding.path != Path::new("/"))
        {
            bail!("resource-root authority contains an invalid whole-machine binding");
        }
        let current = resource_root_binding_record(
            binding.root.clone(),
            &binding.path,
            binding.generation,
            binding.whole_machine,
            binding.bound_at_unix_ms,
        )?;
        if &current != binding {
            bail!(
                "resource-root identity changed since approval: {}",
                binding.path.display()
            );
        }
        let Some((generation, path, whole_machine, bound_at)) = replayed.get(&binding.root) else {
            bail!("active resource root has no replayed audit authority");
        };
        if *generation != binding.generation
            || *path != binding.path
            || *whole_machine != binding.whole_machine
            || *bound_at != binding.bound_at_unix_ms
        {
            bail!("active resource root does not match its latest audit event");
        }
        if active
            .insert(binding.root.clone(), Arc::new(binding.clone()))
            .is_some()
        {
            bail!("authority state contains duplicate active resource roots");
        }
    }
    if active.len() != replayed.len() {
        bail!("resource-root active bindings do not match replayed audit state");
    }
    Ok(ResourceRootState {
        bindings: active,
        audit: audit.to_vec(),
    })
}

#[derive(Debug, Clone, Copy)]
enum SecureOpenMode {
    ReadFile,
    WriteFile,
    ReadDirectory,
}

#[cfg(unix)]
fn open_resource_beneath_mode(
    binding: &ResourceRootBindingRecord,
    selector: &crate::vm::FileSelector,
    relative: &str,
    mode: SecureOpenMode,
) -> std::result::Result<std::fs::File, String> {
    use nix::fcntl::{open, openat, OFlag};
    use nix::sys::stat::{fstat, Mode};
    use std::os::fd::{AsRawFd, FromRawFd};

    if !selector.matches(relative) || relative.contains(['*', '?']) {
        return Err("path is outside its declared selector".to_string());
    }
    let components = Path::new(relative)
        .components()
        .map(|component| match component {
            std::path::Component::Normal(name) => Ok(name.to_owned()),
            _ => Err("resource path must contain only normal relative components".to_string()),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err("resource path is empty".into());
    }
    let root_fd = open(
        &binding.path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| format!("open resource root '{}': {error}", binding.path.display()))?;
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    let root_stat = fstat(directory.as_raw_fd()).map_err(|error| error.to_string())?;
    if root_stat.st_dev as u64 != binding.device || root_stat.st_ino as u64 != binding.inode {
        return Err("resource-root identity changed since it was bound".into());
    }
    for component in &components[..components.len() - 1] {
        let fd = openat(
            Some(directory.as_raw_fd()),
            component.as_os_str(),
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| format!("open resource path component: {error}"))?;
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let final_component = components
        .last()
        .expect("non-empty components checked above")
        .as_os_str();
    #[cfg(test)]
    run_resource_before_final_open_hook(relative);
    let (flags, create_mode) = match mode {
        SecureOpenMode::ReadFile => (
            OFlag::O_RDONLY | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ),
        SecureOpenMode::ReadDirectory => (
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ),
        SecureOpenMode::WriteFile => (
            OFlag::O_WRONLY
                | OFlag::O_CREAT
                | OFlag::O_NONBLOCK
                | OFlag::O_NOFOLLOW
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ),
    };
    let fd = openat(
        Some(directory.as_raw_fd()),
        final_component,
        flags,
        create_mode,
    )
    .map_err(|error| format!("open resource object: {error}"))?;
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let opened = fstat(file.as_raw_fd()).map_err(|error| error.to_string())?;
    let opened_kind = nix::sys::stat::SFlag::from_bits_truncate(opened.st_mode);
    match mode {
        SecureOpenMode::ReadFile | SecureOpenMode::WriteFile
            if !opened_kind.contains(nix::sys::stat::SFlag::S_IFREG) =>
        {
            return Err("resource object is not a regular file".into());
        }
        SecureOpenMode::ReadDirectory if !opened_kind.contains(nix::sys::stat::SFlag::S_IFDIR) => {
            return Err("resource object is not a directory".into());
        }
        _ => {}
    }
    if matches!(mode, SecureOpenMode::WriteFile) {
        nix::unistd::ftruncate(&file, 0)
            .map_err(|error| format!("truncate resource object: {error}"))?;
    }
    Ok(file)
}

#[cfg(all(test, unix))]
type ResourceBeforeFinalOpenHook = (String, Box<dyn FnOnce() + Send>);

#[cfg(all(test, unix))]
static RESOURCE_BEFORE_FINAL_OPEN_HOOK: std::sync::OnceLock<
    Mutex<Vec<ResourceBeforeFinalOpenHook>>,
> = std::sync::OnceLock::new();

#[cfg(all(test, unix))]
fn run_resource_before_final_open_hook(relative: &str) {
    let mut hook = RESOURCE_BEFORE_FINAL_OPEN_HOOK
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(index) = hook.iter().position(|(expected, _)| expected == relative) {
        let (_, callback) = hook.remove(index);
        callback();
    }
}

#[cfg(not(unix))]
fn open_resource_beneath_mode(
    _binding: &ResourceRootBindingRecord,
    _selector: &crate::vm::FileSelector,
    _relative: &str,
    _mode: SecureOpenMode,
) -> std::result::Result<std::fs::File, String> {
    Err("descriptor-relative resource access is unsupported on this platform".into())
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
    caller: Option<&scheduler::AgentIdentity>,
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

fn agent_ancestry(caller: Option<&scheduler::AgentIdentity>) -> Vec<uuid::Uuid> {
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
mod tests {
    use super::*;

    fn production_host_handler(runtime: &ProgramRuntime) -> TypedHostHandler {
        let execution_id = uuid::Uuid::new_v4();
        TypedHostHandler::new(
            Arc::clone(&runtime.automation),
            Arc::clone(&runtime.resource_roots),
            None,
            None,
            None,
            BTreeMap::new(),
            "[]".into(),
            Arc::clone(&runtime.network),
            Arc::clone(&runtime.output_handles),
            Arc::clone(&runtime.streams),
            execution_id,
            runtime.manifest_generation(),
            HostAuthorizationAudit {
                ledger: Arc::clone(&runtime.capability_ledger),
                policy: Arc::clone(&runtime.capability_policy),
                use_gate: Arc::clone(&runtime.authority_use_gate),
                sink: None,
                context: runtime.authorization_context_for(None).unwrap(),
                reason: "host-boundary regression".into(),
                program_hash: "test-program".into(),
                agent_ancestry: Vec::new(),
            },
            TypedRuntime::intrinsic_grants(),
            None,
            DeferredHostEffects::None,
        )
    }

    #[test]
    fn host_requests_are_routed_through_registered_core_bindings() {
        let origin = SourceOrigin::generated("file-read");
        let requirement = core_word_spec("file-read")
            .unwrap()
            .signature
            .effects
            .0
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            registered_host_binding(&requirement, &origin).unwrap(),
            Some(CoreHostBinding::FileRead)
        );

        let wrong_requirement = CapabilityRequirement {
            capability: crate::vm::CapabilityKind::SessionEmit,
            selector: crate::vm::ResourceSelector::None,
        };
        assert!(registered_host_binding(&wrong_requirement, &origin).is_err());
        assert!(registered_host_binding(&requirement, &SourceOrigin::generated("+"),).is_err());

        #[cfg(any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "dragonfly"
        ))]
        {
            let process_requirement = CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::Process {
                    executables: vec![resolve_process_executable("/usr/bin/true")
                        .unwrap()
                        .encode()],
                },
            };
            assert_eq!(
                registered_host_binding(
                    &process_requirement,
                    &SourceOrigin::generated("process-run"),
                )
                .unwrap(),
                Some(CoreHostBinding::ProcessRun)
            );
            assert_eq!(
                registered_host_binding(
                    &process_requirement,
                    &SourceOrigin::generated("legacy-process-run"),
                )
                .unwrap(),
                None
            );
            let process_origin = SourceOrigin::generated("process-run");
            assert!(validate_process_request(
                Some(CoreHostBinding::ProcessRun),
                &process_requirement,
                "/usr/bin/true",
                &[],
                &process_origin,
            )
            .is_ok());
            assert!(validate_process_request(
                None,
                &process_requirement,
                "/usr/bin/true",
                &[],
                &SourceOrigin::generated("legacy-process-run"),
            )
            .is_err());
            assert!(validate_process_request(
                Some(CoreHostBinding::ProcessRun),
                &process_requirement,
                "/usr/bin/false",
                &[],
                &process_origin,
            )
            .is_err());
            let empty = CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: ResourceSelector::Process {
                    executables: Vec::new(),
                },
            };
            assert!(validate_process_request(
                Some(CoreHostBinding::ProcessRun),
                &empty,
                "/usr/bin/true",
                &[],
                &process_origin,
            )
            .is_err());
            let multiple = CapabilityRequirement {
                capability: CapabilityKind::ProcessRun,
                selector: ResourceSelector::Process {
                    executables: vec![
                        resolve_process_executable("/usr/bin/true")
                            .unwrap()
                            .encode(),
                        resolve_process_executable("/usr/bin/false")
                            .unwrap()
                            .encode(),
                    ],
                },
            };
            assert!(validate_process_request(
                Some(CoreHostBinding::ProcessRun),
                &multiple,
                "/usr/bin/true",
                &[],
                &process_origin,
            )
            .is_err());
        }
    }

    #[test]
    fn every_host_dispatch_rejects_a_same_capability_wrong_binding_or_abi() {
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::AutomationWrite,
            selector: ResourceSelector::Automation { application: None },
        };
        let click = vec![
            TypedValue::Float(1.0),
            TypedValue::Float(2.0),
            TypedValue::String("left".into()),
            TypedValue::Int(1),
        ];
        let origin = SourceOrigin::generated("automation-click");
        assert!(validate_core_host_request(
            Some(CoreHostBinding::AutomationClick),
            &requirement,
            &click,
            &origin,
        )
        .is_ok());
        assert!(validate_core_host_request(
            Some(CoreHostBinding::AutomationType),
            &requirement,
            &click,
            &origin,
        )
        .is_err());
        assert!(validate_core_host_request(
            Some(CoreHostBinding::AutomationClick),
            &requirement,
            &[TypedValue::String("text".into()), TypedValue::Int(0)],
            &origin,
        )
        .is_err());

        let file_requirement = CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        let broad = crate::vm::FileSelector::parse("./**").unwrap();
        assert!(validate_core_host_request(
            Some(CoreHostBinding::FileRead),
            &file_requirement,
            &[TypedValue::Path {
                selector: broad,
                relative: "README.md".into(),
            }],
            &SourceOrigin::generated("file-read"),
        )
        .is_err());

        let mut checked = 0_usize;
        for (name, spec) in crate::vm::vocabulary::core_word_registry() {
            let CoreWordImplementation::HostEffect(binding) = spec.implementation else {
                continue;
            };
            let Some(declared) = spec.signature.effects.0.first() else {
                panic!("host binding '{name}' has no declared authority");
            };
            let hostile = vec![TypedValue::Unit; 9];
            assert!(
                validate_core_host_request(
                    Some(binding),
                    declared,
                    &hostile,
                    &SourceOrigin::generated(name.clone()),
                )
                .is_err(),
                "host binding '{name}' accepted a hostile ABI",
            );
            checked += 1;
        }
        assert!(checked > 30, "expected the complete core host registry");

        let runtime = ProgramRuntime::new();
        let mut host = production_host_handler(&runtime);
        let error = crate::vm::interpreter::CapabilityHandler::request(
            &mut host,
            &requirement,
            vec![TypedValue::String("hostile".into()), TypedValue::Int(0)],
            &origin,
        )
        .expect_err("the production host boundary must reject automation ABI substitution");
        assert_eq!(error.code, "E-HOST-002");

        let error = crate::vm::interpreter::CapabilityHandler::request(
            &mut host,
            &file_requirement,
            vec![TypedValue::Path {
                selector: crate::vm::FileSelector::parse("./**").unwrap(),
                relative: "README.md".into(),
            }],
            &SourceOrigin::generated("file-read"),
        )
        .expect_err("the production host boundary must reject selector/argument substitution");
        assert_eq!(error.code, "E-HOST-002");
    }

    #[test]
    fn network_send_rejects_a_stale_program_run_generation() {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        let client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let (_server, _) = listener.accept().unwrap();
        let runtime = ProgramRuntime::new();
        let mut host = production_host_handler(&runtime);
        let handle = uuid::Uuid::new_v4().to_string();
        runtime.network.lock().unwrap().insert(
            handle.clone(),
            NetworkSocket {
                stream: client,
                host: "127.0.0.1".into(),
                port,
                owner: host.execution_id,
                generation: host.resource_generation,
            },
        );
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::NetworkConnect,
            selector: ResourceSelector::Network {
                host: "127.0.0.1".into(),
                ports: vec![port],
            },
        };
        let stale_generation = host.resource_generation + 1;
        let error = crate::vm::interpreter::CapabilityHandler::request(
            &mut host,
            &requirement,
            vec![
                TypedValue::Resource {
                    kind: "network-socket".into(),
                    handle,
                    generation: stale_generation,
                },
                TypedValue::Bytes(b"hostile".to_vec()),
            ],
            &SourceOrigin::generated("network-send"),
        )
        .unwrap_err();
        assert_eq!(error.code, "E-HOST-002");
        assert!(error.message.contains("stale"));
    }

    #[test]
    fn bounded_line_reader_preserves_cursor_position_and_normalizes_newlines() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"first\r\nsecond\nlast").unwrap();
        let mut reader = BufReader::new(file.reopen().unwrap());
        assert_eq!(
            read_bounded_utf8_line(&mut reader).unwrap(),
            Some("first".into())
        );
        assert_eq!(
            read_bounded_utf8_line(&mut reader).unwrap(),
            Some("second".into())
        );
        assert_eq!(
            read_bounded_utf8_line(&mut reader).unwrap(),
            Some("last".into())
        );
        assert_eq!(read_bounded_utf8_line(&mut reader).unwrap(), None);
    }

    #[test]
    fn bounded_line_reader_accepts_a_limit_sized_crlf_line() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut writer = file.reopen().unwrap();
        writer.write_all(&vec![b'x'; 1024 * 1024]).unwrap();
        writer.write_all(b"\r\n").unwrap();
        let mut reader = BufReader::new(file.reopen().unwrap());

        assert_eq!(
            read_bounded_utf8_line(&mut reader).unwrap(),
            Some("x".repeat(1024 * 1024))
        );
        assert_eq!(read_bounded_utf8_line(&mut reader).unwrap(), None);
    }

    #[test]
    fn bounded_csv_reader_keeps_quoted_multiline_records_intact() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"name,note\r\nAda,\"first line\nsecond, with comma\"\r\n\"Grace \"\"Amazing\"\"\",done\n")
            .unwrap();
        let mut reader = BufReader::new(file.reopen().unwrap());

        assert_eq!(
            read_bounded_csv_record(&mut reader).unwrap(),
            Some(vec!["name".into(), "note".into()])
        );
        assert_eq!(
            read_bounded_csv_record(&mut reader).unwrap(),
            Some(vec!["Ada".into(), "first line\nsecond, with comma".into()])
        );
        assert_eq!(
            read_bounded_csv_record(&mut reader).unwrap(),
            Some(vec!["Grace \"Amazing\"".into(), "done".into()])
        );
        assert_eq!(read_bounded_csv_record(&mut reader).unwrap(), None);
    }

    #[test]
    fn bounded_csv_reader_rejects_malformed_quote_boundaries() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"one,\"two\"oops\n").unwrap();
        let mut reader = BufReader::new(file.reopen().unwrap());

        assert!(read_bounded_csv_record(&mut reader)
            .unwrap_err()
            .contains("after closing quote"));
    }

    fn submission(
        language: ProgramLanguage,
        source: &str,
        effect: ExecutionEffect,
    ) -> ProgramSubmission {
        ProgramSubmission {
            language,
            source_id: None,
            source: source.to_string(),
            intent: "test".to_string(),
            effect,
            declared_capabilities: Vec::new(),
            manifest_generation: 1,
            expected_revision: None,
            budget: None,
        }
    }

    #[tokio::test]
    async fn typed_only_submission_never_uses_the_legacy_forth_interpreter() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                // The legacy interpreter accepts this classic Forth
                // definition, while the typed frontend correctly requires an
                // explicit stack signature.
                ": legacy-double 2 * ;",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert!(outcome.vm_diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "E-FORTH-SIG-001"
                && diagnostic
                    .primary
                    .as_ref()
                    .and_then(|origin| origin.span.as_ref())
                    .is_some()
        }));

        let state = runtime.inspect().await.unwrap();
        assert!(state
            .vocabulary
            .iter()
            .all(|word| word.name != "legacy-double"));
    }

    #[tokio::test]
    async fn typed_boundary_preserves_symbols_and_results() {
        let runtime = ProgramRuntime::new();
        let symbol = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "'bash",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(symbol.values, vec![ProgramValue::Symbol("bash".into())]);

        let result = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(ok 7)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(
            result.values,
            vec![
                ProgramValue::Symbol("bash".into()),
                ProgramValue::Result {
                    ok: true,
                    value: Box::new(ProgramValue::Int(7)),
                },
            ]
        );
    }

    #[tokio::test]
    async fn rejects_stale_manifest_generation() {
        let runtime = ProgramRuntime::new();
        let mut request = submission(ProgramLanguage::Forth, "1", ExecutionEffect::Pure);
        request.manifest_generation = 0;
        let error = runtime.submit(request).await.unwrap_err();
        assert!(error.to_string().contains("stale VM manifest"));
    }

    #[tokio::test]
    async fn source_cannot_hide_external_effect_behind_pure_declaration() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" path\" path s\" data\" bytes file-write",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert!(outcome
            .required_capabilities
            .iter()
            .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileWrite));
        assert!(outcome
            .inferred_capabilities
            .iter()
            .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileWrite));
        assert!(matches!(
            outcome.inferred_capabilities[0].selector,
            crate::vm::ResourceSelector::FileTemplate { .. }
        ));
        assert!(matches!(
            outcome.required_capabilities[0].selector,
            crate::vm::ResourceSelector::File { .. }
        ));
    }

    #[tokio::test]
    async fn completed_outcome_retains_effects_from_an_untaken_branch() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "false if s\" missing.txt\" path file-read else s\" local\" bytes then",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(outcome.required_capabilities.is_empty());
        assert!(outcome
            .inferred_capabilities
            .iter()
            .any(|requirement| { requirement.capability == crate::vm::CapabilityKind::FileRead }));
    }

    #[tokio::test]
    async fn portable_lisp_uses_the_typed_vm_without_forth_text() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(+ 3 (* 4 2))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
        assert_eq!(outcome.values, vec![ProgramValue::Int(11)]);
    }

    #[tokio::test]
    async fn managed_json_fields_cross_the_public_runtime_boundary() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(json-get (result-unwrap (json-parse \"{\\\"nested\\\":{\\\"answer\\\":42}}\")) \"nested\")",
                ExecutionEffect::Pure,
            ))
            .await
            .expect("managed JSON field lookup succeeds");
        assert_eq!(
            outcome.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::Json(
                serde_json::json!({"answer": 42}),
            ))))]
        );
    }

    #[tokio::test]
    async fn typed_dictionary_is_shared_between_forth_and_lisp_submissions() {
        let runtime = ProgramRuntime::new();
        let definition = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ": square ( S int -- S int ! pure ) dup * ;",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert_eq!(definition.backend, ExecutionBackend::TypedVm);
        let call = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(square 12)",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(call.backend, ExecutionBackend::TypedVm);
        assert_eq!(call.values, vec![ProgramValue::Int(144)]);
    }

    #[tokio::test]
    async fn say_is_a_typed_lisp_response_program() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(say \"hello from Lisp\")",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
        assert_eq!(outcome.output, "hello from Lisp");
    }

    #[tokio::test]
    async fn typed_maps_cross_the_public_program_runtime_boundary_structurally() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(map \"answer\" 42 \"other\" 7)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(
            outcome.values,
            vec![ProgramValue::Map(vec![
                (ProgramValue::String("answer".into()), ProgramValue::Int(42)),
                (ProgramValue::String("other".into()), ProgramValue::Int(7)),
            ])]
        );
    }

    #[tokio::test]
    async fn enabled_automation_still_requires_an_explicit_typed_grant() {
        let runtime = ProgramRuntime::with_automation(true);
        let denied = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(automation-availability)",
                ExecutionEffect::ExternalRead,
            ))
            .await
            .unwrap();
        assert_eq!(denied.status, ExecutionStatus::AuthorizationRequired);

        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::AutomationInspect,
                selector: crate::vm::ResourceSelector::Automation { application: None },
            })
            .unwrap();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(automation-availability)",
                ExecutionEffect::ExternalRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(matches!(
            outcome.values.first(),
            Some(ProgramValue::String(_))
        ));
    }

    #[tokio::test]
    async fn approved_typed_file_read_resumes_with_a_refined_path() {
        let runtime = ProgramRuntime::new();
        let request = submission(
            ProgramLanguage::Lisp,
            "(begin (say \"checking\") (file-read (path \"Cargo.toml\")))",
            ExecutionEffect::WorkspaceRead,
        );
        let pending = runtime.submit(request).await.unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(pending.required_capabilities.len(), 1);
        assert_eq!(pending.output, "checking");
        assert!(matches!(
            pending.effect_journal.as_slice(),
            [
                crate::vm::EffectJournalEntry {
                    state: crate::vm::EffectJournalState::Acknowledged { values },
                    ..
                },
                crate::vm::EffectJournalEntry {
                    state: crate::vm::EffectJournalState::AwaitingApproval,
                    ..
                },
            ] if values.is_empty()
        ));
        assert_eq!(
            pending.approval_prompts[0].request.origin.word.as_deref(),
            Some("file-read")
        );
        let effect_sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("concrete approval requests carry their VM effect sequence");
        assert!(matches!(
            pending.approval_prompts[0].request.arguments.as_slice(),
            [TypedValue::Path { relative, .. }] if relative == "Cargo.toml"
        ));
        let pending_info = runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .expect("authorization should retain a daemon continuation");
        assert_eq!(pending_info.resume_effect_sequence, Some(effect_sequence));
        assert!(matches!(
            pending_info.reason,
            PendingTypedReason::AuthorizationRequired { .. }
        ));
        assert!(runtime
            .resume_typed_execution_for_effect(pending.execution_id, effect_sequence + 1)
            .await
            .is_err());
        assert!(runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .is_some());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let approved = runtime
            .resume_typed_execution_for_effect(pending.execution_id, effect_sequence)
            .await
            .unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.output, "checking");
        assert!(matches!(
            approved.values.first(),
            Some(ProgramValue::Bytes(_))
        ));
        assert!(matches!(
            approved.effect_journal.as_slice(),
            [
                crate::vm::EffectJournalEntry {
                    state: crate::vm::EffectJournalState::Acknowledged { .. },
                    ..
                },
                crate::vm::EffectJournalEntry {
                    state: crate::vm::EffectJournalState::Acknowledged { values },
                    ..
                },
            ] if matches!(values.as_slice(), [TypedValue::Bytes(_)])
        ));
    }

    #[tokio::test]
    async fn approval_requests_are_stable_and_preserve_agent_ancestry() {
        let runtime = ProgramRuntime::new();
        let root_agent_id = uuid::Uuid::new_v4();
        let parent_agent_id = uuid::Uuid::new_v4();
        let caller = scheduler::AgentIdentity {
            agent_id: uuid::Uuid::new_v4(),
            task_id: uuid::Uuid::new_v4(),
            parent_agent_id: Some(parent_agent_id),
            root_agent_id,
            depth: 2,
            provider_model: "test-provider".into(),
            vm_revision: 0,
            manifest_generation: runtime.manifest_generation(),
            starting_context_hash: "test-context".into(),
            grant_ceiling: EffectSet::pure(),
            brain_run_id: None,
        };
        let request = submission(
            ProgramLanguage::Lisp,
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        );
        let pending = runtime
            .submit_as(request, Some(caller.clone()))
            .await
            .unwrap();
        let prompt = &pending.approval_prompts[0];
        assert_eq!(
            prompt.request.agent_ancestry,
            vec![root_agent_id, parent_agent_id, caller.agent_id]
        );

        let stored = runtime
            .pending_typed
            .lock()
            .unwrap()
            .get(&pending.execution_id)
            .cloned()
            .unwrap();
        let rendered_again = approval_prompts(
            pending.execution_id,
            &pending.required_capabilities,
            &stored.source,
            &stored.intent,
            Some(&stored.suspension),
            Some(&caller),
        );
        assert_eq!(rendered_again[0].request.id, prompt.request.id);
        assert_eq!(
            rendered_again[0].request.effect_sequence,
            prompt.request.effect_sequence
        );
    }

    #[tokio::test]
    async fn task_scoped_grants_apply_only_to_the_matching_program_run() {
        let runtime = ProgramRuntime::new();
        let allowed_task = uuid::Uuid::new_v4();
        let requirement = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        runtime
            .issue_typed_capability(
                requirement,
                GrantScope::Task {
                    task_id: allowed_task,
                },
                "test-user",
                None,
            )
            .unwrap();
        let identity = |task_id| scheduler::AgentIdentity {
            agent_id: uuid::Uuid::new_v4(),
            task_id,
            parent_agent_id: None,
            root_agent_id: uuid::Uuid::new_v4(),
            depth: 0,
            provider_model: "test-provider".into(),
            vm_revision: runtime.revision(),
            manifest_generation: runtime.manifest_generation(),
            starting_context_hash: "test-context".into(),
            grant_ceiling: EffectSet::pure(),
            brain_run_id: None,
        };
        let source = || {
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            )
        };

        let allowed = runtime
            .submit_as(source(), Some(identity(allowed_task)))
            .await
            .unwrap();
        assert_eq!(allowed.status, ExecutionStatus::Completed);

        let unrelated = runtime
            .submit_as(source(), Some(identity(uuid::Uuid::new_v4())))
            .await
            .unwrap();
        assert_eq!(unrelated.status, ExecutionStatus::AuthorizationRequired);
    }

    #[tokio::test]
    async fn child_grant_ceiling_blocks_later_ambient_expansion_but_allows_task_approval() {
        let runtime = ProgramRuntime::new();
        let task_id = uuid::Uuid::new_v4();
        let child = scheduler::AgentIdentity {
            agent_id: uuid::Uuid::new_v4(),
            task_id,
            parent_agent_id: None,
            root_agent_id: uuid::Uuid::new_v4(),
            depth: 0,
            provider_model: "test-provider".into(),
            vm_revision: runtime.revision(),
            manifest_generation: runtime.manifest_generation(),
            starting_context_hash: "test-context".into(),
            grant_ceiling: runtime.effective_grants_for(None).unwrap(),
            brain_run_id: None,
        };
        let requirement = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        runtime
            .issue_typed_capability(
                requirement.clone(),
                GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let source = || {
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            )
        };

        let ambient = runtime
            .submit_as(source(), Some(child.clone()))
            .await
            .unwrap();
        assert_eq!(ambient.status, ExecutionStatus::AuthorizationRequired);
        runtime
            .cancel_typed_execution(ambient.execution_id)
            .unwrap();

        runtime
            .issue_typed_capability(requirement, GrantScope::Task { task_id }, "test-user", None)
            .unwrap();
        let explicitly_approved = runtime.submit_as(source(), Some(child)).await.unwrap();
        assert_eq!(explicitly_approved.status, ExecutionStatus::Completed);
    }

    #[tokio::test]
    async fn exact_once_grants_never_enter_ambient_program_run_authority() {
        let runtime = ProgramRuntime::new();
        runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
                ),
                GrantScope::Once {
                    request_id: uuid::Uuid::new_v4(),
                },
                "test-user",
                None,
            )
            .unwrap();

        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
    }

    #[tokio::test]
    async fn allow_once_resumes_exactly_one_runtime_effect() {
        let runtime = ProgramRuntime::new();
        let first = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(begin (file-read (path \"Cargo.toml\")) (file-read (path \"Cargo.lock\")))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let first_prompt = first.approval_prompts[0].clone();
        let second = runtime
            .resolve_typed_approval(&first_prompt, ApprovalChoice::AllowOnce, "test-user")
            .await
            .unwrap();
        assert_eq!(second.status, ExecutionStatus::AuthorizationRequired);
        assert_ne!(
            second.approval_prompts[0].request.id,
            first_prompt.request.id
        );

        let ledger = runtime.capability_ledger().unwrap();
        let once = ledger
            .grants
            .grants
            .iter()
            .find(|grant| {
                matches!(
                    grant.scope,
                    GrantScope::Once { request_id }
                        if request_id == first_prompt.request.id
                )
            })
            .expect("allow-once records an exact grant");
        assert!(once.consumed_at_unix_ms.is_some());
        assert!(matches!(
            ledger
                .authorization_audit
                .last()
                .map(|entry| &entry.decision),
            Some(AuthorizationDecision::Allowed { .. })
        ));
    }

    #[tokio::test]
    async fn session_approval_is_reused_only_after_exact_prompt_validation() {
        let runtime = ProgramRuntime::new();
        let source = || {
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            )
        };
        let pending = runtime.submit(source()).await.unwrap();
        let mut forged = pending.approval_prompts[0].clone();
        forged.request.id = uuid::Uuid::new_v4();
        assert!(runtime
            .resolve_typed_approval(&forged, ApprovalChoice::AllowSession, "test-user")
            .await
            .is_err());
        assert!(runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .is_some());
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());

        let approved = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowSession,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        let reused = runtime.submit(source()).await.unwrap();
        assert_eq!(reused.status, ExecutionStatus::Completed);
        assert!(reused.approval_prompts.is_empty());
        let ledger = runtime.capability_ledger().unwrap();
        let grant_id = ledger.grants.grants[0].id;
        assert_eq!(ledger.authorization_audit.len(), 2);
        assert!(ledger.authorization_audit.iter().all(|entry| matches!(
            entry.decision,
            AuthorizationDecision::Allowed { grant_id: used } if used == grant_id
        )));
        assert_ne!(
            ledger.authorization_audit[0].execution_id, ledger.authorization_audit[1].execution_id,
            "each actual host boundary keeps its owning ProgramRun identity"
        );
    }

    #[tokio::test]
    async fn denied_approval_is_audited_and_discards_the_continuation() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let denied = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::Deny,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(denied.status, ExecutionStatus::Failed);
        assert!(matches!(
            denied.effect_journal.last().map(|entry| &entry.state),
            Some(crate::vm::EffectJournalState::Denied)
        ));
        assert!(runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .is_none());
        assert!(matches!(
            runtime
                .capability_ledger()
                .unwrap()
                .authorization_audit
                .last()
                .map(|entry| &entry.decision),
            Some(AuthorizationDecision::Denied { .. })
        ));
    }

    #[tokio::test]
    async fn typed_file_slice_reads_a_bounded_range_without_loading_the_file() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path 0 9 file-slice",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("file-slice must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Bytes(b"[package]".to_vec())]
        );
    }

    #[tokio::test]
    async fn typed_file_hash_returns_sha256_without_materializing_file_bytes() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path file-hash",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("file-hash must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let outcome = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(matches!(
            outcome.values.as_slice(),
            [ProgramValue::String(digest)]
                if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        ));
    }

    #[test]
    fn tree_merkle_is_stable_and_changes_with_tree_contents() {
        let root = tempfile::tempdir().unwrap();
        let tree = root.path().join("tree");
        std::fs::create_dir_all(tree.join("nested")).unwrap();
        std::fs::write(tree.join("alpha.txt"), "alpha").unwrap();
        std::fs::write(tree.join("nested").join("beta.txt"), "beta").unwrap();

        let first = merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(
            first,
            merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap()
        );

        std::fs::write(tree.join("nested").join("beta.txt"), "changed").unwrap();
        assert_ne!(
            first,
            merkle_directory(std::fs::File::open(&tree).unwrap()).unwrap()
        );
    }

    #[test]
    fn tree_list_is_sorted_bounded_and_structural() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a-dir")).unwrap();
        std::fs::write(root.path().join("z.txt"), "z").unwrap();
        std::fs::write(root.path().join("a-dir").join("b.txt"), "beta").unwrap();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();

        let (entries, truncated) =
            list_directory_tree(std::fs::File::open(root.path()).unwrap(), 3).unwrap();
        assert!(truncated);
        assert_eq!(entries.len(), 3);
        let paths = entries
            .iter()
            .map(|entry| match entry {
                TypedValue::Record(fields) => fields
                    .iter()
                    .find_map(|(name, value)| {
                        (name == "path")
                            .then_some(value)
                            .and_then(|value| match value {
                                TypedValue::String(path) => Some(path.as_str()),
                                _ => None,
                            })
                    })
                    .expect("tree entry path"),
                _ => panic!("tree-list must return records"),
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, vec!["a-dir", "a-dir/b.txt", "a.txt"]);
        assert!(entries
            .iter()
            .all(|entry| entry.value_type() == tree_entry_type()));
    }

    #[tokio::test]
    async fn typed_tree_list_has_identical_lisp_and_forth_results() {
        let mut results = Vec::new();
        for (language, source) in [
            (ProgramLanguage::Lisp, "(tree-list (path \"src/vm\") 5)"),
            (ProgramLanguage::Forth, "s\"src/vm\" path 5 tree-list"),
        ] {
            let runtime = ProgramRuntime::new();
            runtime
                .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./**").unwrap(),
                ))
                .unwrap();
            let outcome = runtime
                .submit_typed_only(submission(language, source, ExecutionEffect::WorkspaceRead))
                .await
                .unwrap();
            assert_eq!(outcome.status, ExecutionStatus::Completed);
            assert!(matches!(
                outcome.values.as_slice(),
                [ProgramValue::Record(fields)]
                    if fields.iter().any(|(name, value)| {
                        name == "truncated" && value == &ProgramValue::Bool(true)
                    })
            ));
            results.push(outcome.values);
        }
        assert_eq!(results[0], results[1]);
    }

    #[tokio::test]
    async fn typed_file_line_cursor_reads_one_bounded_line_at_a_time() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(let ((stream (file-lines-open (path \"Cargo.toml\")))) (stream-next stream))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("file-lines-open must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::String(
                "[package]".into()
            ))))]
        );
    }

    #[tokio::test]
    async fn file_stream_follow_up_rechecks_the_minting_selector_after_revocation() {
        let runtime = ProgramRuntime::new();
        let original_grant = runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let suspended = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path file-lines-open unit yield stream-next",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(suspended.status, ExecutionStatus::Suspended);

        assert!(runtime.revoke_typed_capability(original_grant).unwrap());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./README.md").unwrap(),
            ))
            .unwrap();
        let resumed = runtime
            .resume_typed_execution(suspended.execution_id)
            .await
            .unwrap();
        assert_eq!(resumed.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(
            resumed.required_capabilities,
            vec![crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
            )]
        );
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.authorization_audit.len(), 1);
        assert!(matches!(
            ledger.authorization_audit[0].decision,
            AuthorizationDecision::Allowed { .. }
        ));
    }

    #[tokio::test]
    async fn typed_csv_cursor_reads_one_record_and_releases_its_handle() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path csv-open dup stream-next swap stream-close drop",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("csv-open must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::List(
                vec![ProgramValue::String("[package]".into()),]
            ))))]
        );
    }

    #[tokio::test]
    async fn typed_workbook_cursor_and_sheet_listing_match_across_frontends() {
        let directory = tempfile::tempdir_in(".").unwrap();
        let workbook_path = directory.path().join("typed-workbook.xlsx");
        let mut workbook = rust_xlsxwriter::Workbook::new();
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("First").unwrap();
            sheet.write_string(0, 0, "first").unwrap();
        }
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("Data").unwrap();
            sheet.write_string(0, 0, "answer").unwrap();
            sheet.write_number(0, 1, 42).unwrap();
        }
        {
            let sheet = workbook.add_worksheet();
            sheet.set_name("Stats").unwrap();
            sheet.write_string(0, 0, "name").unwrap();
            sheet.write_string(0, 1, "score").unwrap();
            sheet.write_string(0, 2, "note").unwrap();
            sheet.write_string(1, 0, "Ada").unwrap();
            sheet.write_number(1, 1, 10).unwrap();
            sheet.write_string(1, 2, "ok").unwrap();
            sheet.write_string(2, 0, "Bob").unwrap();
            sheet.write_number(2, 1, 20).unwrap();
            sheet.write_string(3, 0, "Cy").unwrap();
            sheet.write_string(3, 1, "not-a-number").unwrap();
            sheet.write_string(3, 2, "ok").unwrap();
        }
        workbook.save(&workbook_path).unwrap();
        let canonical_workbook = workbook_path.canonicalize().unwrap();
        let canonical_workspace = std::env::current_dir().unwrap().canonicalize().unwrap();
        let relative = canonical_workbook
            .strip_prefix(canonical_workspace)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        for (language, source) in [
            (
                ProgramLanguage::Lisp,
                format!(
                    "(let ((rows (workbook-sheet-open (path \"{relative}\") \"Data\"))) \
                       (let ((row (unwrap (stream-next rows)))) \
                         (begin (stream-close rows) (list-get row 1))))"
                ),
            ),
            (
                ProgramLanguage::Forth,
                format!(
                    "\"{relative}\" path \"Data\" workbook-sheet-open \
                     dup stream-next unwrap 1 list-get swap stream-close drop"
                ),
            ),
        ] {
            let runtime = ProgramRuntime::new();
            runtime
                .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./**").unwrap(),
                ))
                .unwrap();
            let outcome = runtime
                .submit_typed_only(submission(
                    language,
                    &source,
                    ExecutionEffect::WorkspaceRead,
                ))
                .await
                .unwrap();
            assert_eq!(
                outcome.status,
                ExecutionStatus::Completed,
                "{language:?} workbook execution failed: {outcome:#?}"
            );
            assert_eq!(
                outcome.values,
                vec![ProgramValue::String("42".into())],
                "{language:?} did not read the named workbook row"
            );
        }

        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let outcome = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                &format!("(workbook-sheets (path \"{relative}\"))"),
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(
            outcome.values,
            vec![ProgramValue::List(vec![
                ProgramValue::String("First".into()),
                ProgramValue::String("Data".into()),
                ProgramValue::String("Stats".into()),
            ])]
        );

        let expected_range = ProgramValue::List(vec![
            ProgramValue::List(vec![
                ProgramValue::String("Ada".into()),
                ProgramValue::String("10".into()),
            ]),
            ProgramValue::List(vec![
                ProgramValue::String("Bob".into()),
                ProgramValue::String("20".into()),
            ]),
        ]);
        let expected_summary = serde_json::json!({
            "sheet": "Stats",
            "headers": ["name", "score", "note"],
            "sampled_rows": 2,
            "truncated": true,
            "columns": [
                {"index": 0, "name": "name", "empty": 0, "non_empty": 2, "numeric": 0, "min": null, "max": null, "mean": null},
                {"index": 1, "name": "score", "empty": 0, "non_empty": 2, "numeric": 2, "min": 10.0, "max": 20.0, "mean": 15.0},
                {"index": 2, "name": "note", "empty": 1, "non_empty": 1, "numeric": 0, "min": null, "max": null, "mean": null}
            ]
        });
        for (language, range_source, summary_source) in [
            (
                ProgramLanguage::Lisp,
                format!("(workbook-range (path \"{relative}\") \"Stats\" 1 0 2 2)"),
                format!("(workbook-summary (path \"{relative}\") \"Stats\" 2)"),
            ),
            (
                ProgramLanguage::Forth,
                format!("\"{relative}\" path \"Stats\" 1 0 2 2 workbook-range"),
                format!("\"{relative}\" path \"Stats\" 2 workbook-summary"),
            ),
        ] {
            for (source, expected) in [
                (range_source, expected_range.clone()),
                (summary_source, ProgramValue::Json(expected_summary.clone())),
            ] {
                let runtime = ProgramRuntime::new();
                runtime
                    .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                        crate::vm::FileOperation::Read,
                        crate::vm::FileSelector::parse("./**").unwrap(),
                    ))
                    .unwrap();
                let outcome = runtime
                    .submit_typed_only(submission(
                        language,
                        &source,
                        ExecutionEffect::WorkspaceRead,
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    outcome.status,
                    ExecutionStatus::Completed,
                    "{language:?} workbook aggregate failed: {outcome:#?}"
                );
                assert_eq!(outcome.values, vec![expected]);
            }
        }
    }

    #[tokio::test]
    async fn csv_summary_is_bounded_and_identical_across_frontends() {
        let mut file = tempfile::Builder::new()
            .prefix("finch-csv-summary-")
            .suffix(".csv")
            .tempfile_in(".")
            .unwrap();
        file.write_all(b"name,score,note\nAda,10,ok\nBob,20,\nCy,not-a-number,ok\n")
            .unwrap();
        let relative = file
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let expected = serde_json::json!({
            "headers": ["name", "score", "note"],
            "sampled_rows": 2,
            "truncated": true,
            "columns": [
                {"index": 0, "name": "name", "empty": 0, "non_empty": 2, "numeric": 0, "min": null, "max": null, "mean": null},
                {"index": 1, "name": "score", "empty": 0, "non_empty": 2, "numeric": 2, "min": 10.0, "max": 20.0, "mean": 15.0},
                {"index": 2, "name": "note", "empty": 1, "non_empty": 1, "numeric": 0, "min": null, "max": null, "mean": null}
            ]
        });

        for (language, source) in [
            (
                ProgramLanguage::Lisp,
                format!("(csv-summary (path \"{relative}\") 2)"),
            ),
            (
                ProgramLanguage::Forth,
                format!("\"{relative}\" path 2 csv-summary"),
            ),
        ] {
            let runtime = ProgramRuntime::new();
            runtime
                .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./**").unwrap(),
                ))
                .unwrap();
            let outcome = runtime
                .submit_typed_only(submission(
                    language,
                    &source,
                    ExecutionEffect::WorkspaceRead,
                ))
                .await
                .unwrap();
            assert_eq!(outcome.status, ExecutionStatus::Completed);
            assert_eq!(outcome.values, vec![ProgramValue::Json(expected.clone())]);
        }
    }

    #[test]
    fn csv_summary_rejects_rows_wider_than_the_header() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"one,two\n1,2,3\n").unwrap();
        let error = summarize_csv(BufReader::new(file.reopen().unwrap()), 10).unwrap_err();
        assert!(error.contains("has 3 fields but the header declares 2"));
    }

    #[tokio::test]
    async fn typed_csv_cursor_branches_on_a_record_without_unwrapping() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path csv-open dup csv-next if-some 0 list-get say else s\"No records.\" say then csv-close",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("csv-open must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.values, vec![ProgramValue::Nil]);
        assert_eq!(completed.output, "[package]");
    }

    #[tokio::test]
    async fn typed_file_line_cursor_streams_a_text_file_through_a_verified_loop() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\"Cargo.toml\" path file-lines-open \
                 begin: lines true while \
                   dup stream-next if-some \
                     say \
                   else \
                     break lines \
                   then \
                 repeat \
                 stream-close",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("file-lines-open must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();

        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.values, vec![ProgramValue::Nil]);
        assert!(completed.output.starts_with("[package]"));
    }

    #[tokio::test]
    async fn typed_lisp_file_line_cursor_streams_a_text_file_through_a_verified_loop() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(let ((cursor (file-lines-open (path \"Cargo.toml\")))) \
                   (begin \
                     (while :label lines true \
                       (match-option (stream-next cursor) \
                         (some line (say line)) \
                         (none (break lines)))) \
                     (stream-close cursor)))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("file-lines-open must create a portable host request");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();

        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert!(completed.values.is_empty());
        assert!(completed.output.starts_with("[package]"));
    }

    #[tokio::test]
    async fn typed_runtime_accepts_a_portable_external_effect_result() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("awaited host effect must have a stable sequence");
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();

        let completed = runtime
            .resume_vm_effect(VmResume {
                execution_id: pending.execution_id,
                sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Bytes(b"from external event loop".to_vec())],
                },
            })
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Bytes(b"from external event loop".to_vec())]
        );
        assert!(matches!(
            completed.effect_journal.last(),
            Some(crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Acknowledged { values },
                ..
            }) if values == &vec![TypedValue::Bytes(b"from external event loop".to_vec())]
        ));
    }

    #[tokio::test]
    async fn named_brain_schedule_submission_defers_only_the_schedule_host_result() {
        let runtime = ProgramRuntime::new();
        let requirement = crate::vm::CapabilityRequirement {
            capability: crate::vm::CapabilityKind::ScheduleCreate,
            selector: crate::vm::ResourceSelector::Schedule { policy: None },
        };
        runtime.grant_typed_capability(requirement.clone()).unwrap();
        let (sink, receiver) = typed_effect_channel();
        let pending = runtime
            .submit_typed_only_with_deferred_schedule_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(schedule-create \"(say \\\"later\\\")\" 1770000000)",
                    ExecutionEffect::Unclassified,
                ),
                sink,
                None,
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);
        let envelope = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(envelope.execution_id, pending.execution_id);
        assert_eq!(envelope.effect.requirement, requirement);
        assert!(matches!(
            envelope.effect.event,
            crate::vm::HostSideEffect::Request { ref arguments }
                if matches!(arguments.as_slice(),
                    [TypedValue::String(_), TypedValue::Int(1770000000)])
        ));

        let completed = runtime
            .resume_vm_effect(VmResume {
                execution_id: envelope.execution_id,
                sequence: envelope.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Resource {
                        kind: "schedule".into(),
                        handle: uuid::Uuid::new_v4().to_string(),
                        generation: 0,
                    }],
                },
            })
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert!(matches!(
            completed.values.as_slice(),
            [ProgramValue::Resource { kind, .. }] if kind == "schedule"
        ));
    }

    #[tokio::test]
    async fn cancellation_marks_a_pending_capability_request_in_the_effect_journal() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("pending request has a portable sequence");

        let cancelled = runtime
            .resume_vm_effect(VmResume {
                execution_id: pending.execution_id,
                sequence,
                response: VmResumeResponse::Cancelled {
                    reason: Some("host shut down".into()),
                },
            })
            .await
            .expect("pending request should produce a cancelled outcome");
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
        assert!(cancelled.diagnostics[0].contains("host shut down"));
        assert!(matches!(
            cancelled.effect_journal.as_slice(),
            [crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Cancelled,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn stale_portable_cancellation_keeps_the_current_continuation() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("pending request has a portable sequence");

        assert!(runtime
            .resume_vm_effect(VmResume {
                execution_id: pending.execution_id,
                sequence: sequence + 1,
                response: VmResumeResponse::Cancelled { reason: None },
            })
            .await
            .is_err());
        assert!(runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .is_some());

        let cancelled = runtime
            .resume_vm_effect(VmResume {
                execution_id: pending.execution_id,
                sequence,
                response: VmResumeResponse::Cancelled { reason: None },
            })
            .await
            .unwrap();
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
    }

    #[tokio::test]
    async fn portable_denial_records_the_exact_effect_without_resuming_it() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let sequence = pending.approval_prompts[0]
            .request
            .effect_sequence
            .expect("pending request has a portable sequence");

        let denied = runtime
            .resume_vm_effect(VmResume {
                execution_id: pending.execution_id,
                sequence,
                response: VmResumeResponse::Denied {
                    reason: "user declined workspace access".into(),
                },
            })
            .await
            .unwrap();
        assert_eq!(denied.status, ExecutionStatus::Failed);
        assert!(denied.diagnostics[0].contains("user declined workspace access"));
        assert!(matches!(
            denied.effect_journal.last(),
            Some(crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Denied,
                ..
            })
        ));
        assert!(runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn typed_yield_keeps_one_execution_id_and_accumulates_streamed_output() {
        let runtime = ProgramRuntime::new();
        let yielded = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(begin (say \"before\") (yield) (say \"after\"))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(yielded.status, ExecutionStatus::Suspended);
        assert_eq!(yielded.output, "before");

        let completed = runtime
            .resume_typed_execution(yielded.execution_id)
            .await
            .unwrap();
        assert_eq!(completed.execution_id, yielded.execution_id);
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.output, "beforeafter");
        assert!(runtime
            .pending_typed_execution(yielded.execution_id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn typed_effect_sink_is_per_run_and_survives_yield() {
        let runtime = ProgramRuntime::new();
        let first_events = Arc::new(Mutex::new(Vec::new()));
        let first_sink: TypedEffectSink = {
            let first_events = Arc::clone(&first_events);
            Arc::new(move |effect| {
                if let crate::vm::HostSideEffect::Emit { text } = effect.effect.event {
                    first_events.lock().unwrap().push(text);
                }
            })
        };
        let second_events = Arc::new(Mutex::new(Vec::new()));
        let second_sink: TypedEffectSink = {
            let second_events = Arc::clone(&second_events);
            Arc::new(move |effect| {
                if let crate::vm::HostSideEffect::Emit { text } = effect.effect.event {
                    second_events.lock().unwrap().push(text);
                }
            })
        };

        let yielded = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(begin (say \"first-before\") (yield) (say \"first-after\"))",
                    ExecutionEffect::Pure,
                ),
                first_sink,
            )
            .await
            .unwrap();
        runtime
            .resume_typed_execution(yielded.execution_id)
            .await
            .unwrap();
        runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(say \"second\")",
                    ExecutionEffect::Pure,
                ),
                second_sink,
            )
            .await
            .unwrap();

        assert_eq!(
            &*first_events.lock().unwrap(),
            &vec!["first-before".to_string(), "first-after".to_string()]
        );
        assert_eq!(&*second_events.lock().unwrap(), &vec!["second".to_string()]);
    }

    #[tokio::test]
    async fn typed_effect_channel_preserves_one_run_event_order() {
        let runtime = ProgramRuntime::new();
        let (sink, receiver) = typed_effect_channel();
        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(begin (say \"first\") (say \"second\"))",
                    ExecutionEffect::Pure,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);

        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].execution_id, outcome.execution_id);
        assert_eq!(events[0].effect.sequence, 0);
        assert_eq!(events[1].effect.sequence, 1);
        assert!(matches!(
            &events[0].effect.event,
            crate::vm::HostSideEffect::Emit { text } if text == "first"
        ));
        assert!(matches!(
            &events[1].effect.event,
            crate::vm::HostSideEffect::Emit { text } if text == "second"
        ));
    }

    #[tokio::test]
    async fn typed_effect_sink_observes_an_awaited_request_before_approval() {
        let runtime = ProgramRuntime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: TypedEffectSink = {
            let events = Arc::clone(&events);
            Arc::new(move |effect| events.lock().unwrap().push(effect))
        };

        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(file-read (path \"Cargo.toml\"))",
                    ExecutionEffect::WorkspaceRead,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert!(matches!(
            events.lock().unwrap().as_slice(),
            [VmEffectEnvelope {
                effect: VmSideEffect {
                    sequence: 0,
                    event: crate::vm::HostSideEffect::Request { .. },
                    output,
                    ..
                },
                ..
            }] if output == &vec![Type::Bytes]
        ));
    }

    #[tokio::test]
    async fn typed_suspension_can_be_inspected_and_cancelled_without_committing() {
        let runtime = ProgramRuntime::new();
        let yielded = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(begin (say \"before\") (yield) (say \"after\"))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert!(matches!(
            runtime
                .pending_typed_execution(yielded.execution_id)
                .unwrap()
                .expect("yield should retain a daemon continuation")
                .reason,
            PendingTypedReason::Yielded
        ));
        let cancelled = runtime
            .cancel_typed_execution_with_outcome(yielded.execution_id)
            .unwrap()
            .expect("yielded run should produce a cancelled audit outcome");
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
        assert_eq!(cancelled.output, "before");
        assert!(matches!(
            cancelled.effect_journal.as_slice(),
            [crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Acknowledged { values },
                ..
            }] if values.is_empty()
        ));
        assert!(runtime
            .resume_typed_execution(yielded.execution_id)
            .await
            .is_err());
        assert_eq!(runtime.revision(), yielded.input_revision);
    }

    #[tokio::test]
    async fn pending_execution_capacity_cancels_the_new_run_without_eviction() {
        let runtime = ProgramRuntime::new();
        let mut retained_ids = Vec::with_capacity(MAX_PENDING_TYPED_EXECUTIONS);
        for _ in 0..MAX_PENDING_TYPED_EXECUTIONS {
            let yielded = runtime
                .submit(submission(
                    ProgramLanguage::Lisp,
                    "(yield)",
                    ExecutionEffect::Pure,
                ))
                .await
                .unwrap();
            assert_eq!(yielded.status, ExecutionStatus::Suspended);
            retained_ids.push(yielded.execution_id);
        }
        assert_eq!(
            runtime.pending_typed_execution_count().unwrap(),
            MAX_PENDING_TYPED_EXECUTIONS
        );

        let rejected = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(yield)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(rejected.status, ExecutionStatus::Cancelled);
        assert!(rejected.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("pending execution capacity 256 is exhausted")
        }));
        assert_eq!(
            runtime.pending_typed_execution_count().unwrap(),
            MAX_PENDING_TYPED_EXECUTIONS
        );
        assert!(runtime
            .pending_typed_execution(retained_ids[0])
            .unwrap()
            .is_some());
        assert!(runtime
            .pending_typed_execution(rejected.execution_id)
            .unwrap()
            .is_none());

        for execution_id in retained_ids {
            assert!(runtime.cancel_typed_execution(execution_id).unwrap());
        }
        assert_eq!(runtime.pending_typed_execution_count().unwrap(), 0);
    }

    #[tokio::test]
    async fn yielded_execution_resumes_by_exact_id_until_completion() {
        let runtime = ProgramRuntime::new();
        let first = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(begin (say \"one\") (yield) (say \"two\") (yield) (say \"three\"))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(first.status, ExecutionStatus::Suspended);
        assert_eq!(first.output, "one");
        assert!(matches!(
            runtime
                .pending_typed_execution(first.execution_id)
                .unwrap()
                .expect("first yield must persist its continuation")
                .reason,
            PendingTypedReason::Yielded
        ));

        let second = runtime
            .resume_typed_execution(first.execution_id)
            .await
            .unwrap();
        assert_eq!(second.status, ExecutionStatus::Suspended);
        assert_eq!(second.execution_id, first.execution_id);
        assert_eq!(second.output, "onetwo");
        assert!(matches!(
            runtime
                .pending_typed_execution(first.execution_id)
                .unwrap()
                .expect("second yield must replace the saved continuation")
                .reason,
            PendingTypedReason::Yielded
        ));

        let complete = runtime
            .resume_typed_execution(first.execution_id)
            .await
            .unwrap();
        assert_eq!(complete.status, ExecutionStatus::Completed);
        assert_eq!(complete.execution_id, first.execution_id);
        assert_eq!(complete.output, "onetwothree");
        assert_eq!(complete.output_chunks, ["one", "two", "three"]);
        assert!(runtime
            .pending_typed_execution(first.execution_id)
            .unwrap()
            .is_none());
        assert_eq!(runtime.revision(), first.input_revision + 1);
    }

    #[tokio::test]
    async fn typed_yield_payload_is_visible_without_exposing_the_continuation() {
        let runtime = ProgramRuntime::new();
        let yielded = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "42 yield 7",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(yielded.status, ExecutionStatus::Suspended);

        let pending = runtime
            .pending_typed_execution(yielded.execution_id)
            .unwrap()
            .expect("typed yield must retain its continuation");
        assert_eq!(pending.reason, PendingTypedReason::Yielded);
        assert_eq!(pending.yielded_value, Some(ProgramValue::Int(42)));
        assert_eq!(pending.yielded_type, Some(Type::Int));

        let completed = runtime
            .resume_typed_execution(yielded.execution_id)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.values, vec![ProgramValue::Int(7)]);
    }

    #[tokio::test]
    async fn typed_producer_values_cross_the_public_runtime_boundary() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(let ((numbers (defer (lambda () (begin (yield 2) (yield 3) 5))))) (list (fiber-next numbers) (fiber-next numbers) (fiber-next numbers)))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(
            outcome.values,
            vec![ProgramValue::List(vec![
                ProgramValue::Result {
                    ok: true,
                    value: Box::new(ProgramValue::Int(2)),
                },
                ProgramValue::Result {
                    ok: true,
                    value: Box::new(ProgramValue::Int(3)),
                },
                ProgramValue::Result {
                    ok: false,
                    value: Box::new(ProgramValue::Variant {
                        name: "end".into(),
                        value: Some(Box::new(ProgramValue::Int(5))),
                    }),
                },
            ])]
        );
    }

    #[tokio::test]
    async fn typed_fiber_handle_crosses_the_public_runtime_boundary() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(defer (lambda () (begin (yield 2) 5)))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(matches!(
            outcome.values.as_slice(),
            [ProgramValue::Fiber {
                yield_type: Type::Int,
                result_type: Type::Int,
                ..
            }]
        ));
    }

    #[tokio::test]
    async fn typed_capability_request_does_not_mutate_or_fallback() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(mem-store \"remember this\")",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(outcome.output_revision, outcome.input_revision);
        assert_eq!(outcome.required_capabilities.len(), 1);
        assert_eq!(outcome.approval_prompts.len(), 1);
        assert_eq!(
            outcome.approval_prompts[0].exact,
            outcome.required_capabilities[0]
        );
        assert_eq!(
            outcome.required_capabilities[0].capability,
            crate::vm::CapabilityKind::MemoryWrite
        );
    }

    #[tokio::test]
    async fn capability_ledger_restores_and_revokes_runtime_authority_by_id() {
        let requirement = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        let request = || {
            submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            )
        };
        let runtime = ProgramRuntime::new();
        let grant_id = runtime.grant_typed_capability(requirement.clone()).unwrap();
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.grants.grants[0].id, grant_id);
        assert_eq!(ledger.audit.len(), 1);
        assert_eq!(
            runtime.submit(request()).await.unwrap().status,
            ExecutionStatus::Completed
        );

        let restored = ProgramRuntime::new();
        restored.restore_capability_ledger(ledger).unwrap();
        assert_eq!(
            restored.submit(request()).await.unwrap().status,
            ExecutionStatus::Completed
        );
        assert!(restored.revoke_typed_capability(grant_id).unwrap());
        let denied = restored.submit(request()).await.unwrap();
        assert_eq!(denied.status, ExecutionStatus::AuthorizationRequired);
        let ledger = restored.capability_ledger().unwrap();
        assert_eq!(ledger.audit.len(), 2);
        assert_eq!(
            ledger.audit[1].action,
            crate::vm::CapabilityAuditAction::Revoked
        );
    }

    #[test]
    fn failed_authority_sink_rolls_back_a_new_grant() {
        let runtime = ProgramRuntime::new();
        runtime
            .set_authority_sink(Arc::new(|_| {
                Err(anyhow::anyhow!("simulated authority storage failure"))
            }))
            .unwrap();
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::AgentSpawn,
            selector: crate::vm::ResourceSelector::None,
        };
        let error = runtime
            .issue_typed_capability(
                requirement.clone(),
                GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .expect_err("a grant must not survive failed durable policy storage");
        assert!(format!("{error:#}").contains("simulated authority storage failure"));
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
        assert!(!runtime
            .typed
            .lock()
            .unwrap()
            .grants()
            .grants(&EffectSet::from_requirement(requirement)));
    }

    #[test]
    fn process_grant_without_process_selector_has_no_authority_effects() {
        let runtime = ProgramRuntime::new();
        let sink_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime
            .set_authority_sink({
                let sink_calls = Arc::clone(&sink_calls);
                Arc::new(move |_| {
                    sink_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
            })
            .unwrap();
        let before = runtime.capability_ledger().unwrap();

        let error = runtime
            .issue_typed_capability(
                CapabilityRequirement {
                    capability: CapabilityKind::ProcessRun,
                    selector: crate::vm::ResourceSelector::None,
                },
                GrantScope::Global,
                "test-user",
                None,
            )
            .expect_err("process-run authority must bind an exact process selector");

        assert!(format!("{error:#}").contains("typed process selector"));
        assert_eq!(runtime.capability_ledger().unwrap(), before);
        assert_eq!(sink_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn policy_change_revokes_obsolete_and_denied_grants_and_blocks_reissue() {
        let runtime = ProgramRuntime::new();
        let file_read = CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        let agent_spawn = CapabilityRequirement {
            capability: CapabilityKind::AgentSpawn,
            selector: crate::vm::ResourceSelector::None,
        };
        let file_grant = runtime
            .issue_typed_capability(file_read.clone(), GrantScope::Global, "test-user", None)
            .unwrap();
        let agent_grant = runtime
            .issue_typed_capability(agent_spawn, GrantScope::Global, "test-user", None)
            .unwrap();

        let mut denied = std::collections::BTreeSet::new();
        denied.insert(CapabilityKind::FileRead);
        let revoked = runtime
            .apply_capability_policy(
                CapabilityPolicy {
                    policy_hash: "finch-local-runtime-v2".into(),
                    denied_capabilities: denied,
                },
                "policy-admin",
            )
            .unwrap();
        assert_eq!(revoked, vec![file_grant, agent_grant]);
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == file_grant)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(ledger
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == agent_grant)
            .unwrap()
            .revoked_at_unix_ms
            .is_some());
        assert!(runtime
            .issue_typed_capability(file_read, GrantScope::Global, "test-user", None)
            .unwrap_err()
            .to_string()
            .contains("denied by policy"));
        assert!(runtime
            .apply_capability_policy(
                CapabilityPolicy {
                    policy_hash: "finch-local-runtime-v2".into(),
                    denied_capabilities: Default::default(),
                },
                "policy-admin",
            )
            .unwrap_err()
            .to_string()
            .contains("cannot be reused"));

        let replacement_agent_grant = runtime
            .issue_typed_capability(
                CapabilityRequirement {
                    capability: CapabilityKind::AgentSpawn,
                    selector: crate::vm::ResourceSelector::None,
                },
                GrantScope::Global,
                "test-user",
                None,
            )
            .unwrap();
        let revoked = runtime
            .apply_capability_policy(
                CapabilityPolicy {
                    policy_hash: "finch-local-runtime-v3".into(),
                    denied_capabilities: Default::default(),
                },
                "policy-admin",
            )
            .unwrap();
        assert_eq!(revoked, vec![replacement_agent_grant]);
        assert_eq!(
            runtime.capability_policy().unwrap().policy_hash,
            "finch-local-runtime-v3"
        );
    }

    #[test]
    fn failed_authority_sink_rolls_back_policy_and_its_revocations() {
        let runtime = ProgramRuntime::new();
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::AgentSpawn,
            selector: crate::vm::ResourceSelector::None,
        };
        let grant_id = runtime
            .issue_typed_capability(requirement, GrantScope::Global, "test-user", None)
            .unwrap();
        let previous_policy = runtime.capability_policy().unwrap();
        let previous_ledger = runtime.capability_ledger().unwrap();
        runtime
            .set_authority_sink(Arc::new(|_| {
                Err(anyhow::anyhow!("simulated policy storage failure"))
            }))
            .unwrap();

        let error = runtime
            .apply_capability_policy(
                CapabilityPolicy {
                    policy_hash: "finch-local-runtime-v2".into(),
                    denied_capabilities: Default::default(),
                },
                "policy-admin",
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("simulated policy storage failure"));
        assert_eq!(runtime.capability_policy().unwrap(), previous_policy);
        assert_eq!(runtime.capability_ledger().unwrap(), previous_ledger);
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .iter()
            .find(|grant| grant.id == grant_id)
            .unwrap()
            .is_active(unix_time_ms()));
    }

    #[tokio::test]
    async fn typed_memory_host_reads_and_writes_through_attached_memtree() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let memory = Arc::new(
            crate::memory::MemorySystem::new(crate::memory::MemoryConfig {
                db_path: database.path().to_path_buf(),
                use_neural_embeddings: false,
                ..Default::default()
            })
            .unwrap(),
        );
        let runtime = ProgramRuntime::new();
        runtime.attach_memory(memory);
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::MemoryWrite,
                selector: crate::vm::ResourceSelector::Memory {
                    tree: "session".into(),
                    path: "**".into(),
                },
            })
            .unwrap();
        let stored = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(mem-store \"typed memory fact\")",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert_eq!(stored.status, ExecutionStatus::Completed);
        assert!(matches!(
            stored.values.first(),
            Some(ProgramValue::Resource { kind, .. }) if kind == "memory-node"
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typed_mcp_call_uses_concrete_authority_and_managed_json() {
        let script = r#"
IFS= read -r discover
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private","_meta":{"io.modelcontextprotocol/serverInfo":{"name":"fixture","version":"1"}}}}'
IFS= read -r list
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo_value","description":"untrusted fixture prose","inputSchema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"]},"outputSchema":{"type":"object","properties":{"ok":{"type":"boolean"},"typed":{"type":"boolean"},"forth":{"type":"boolean"}}}}]}}'
IFS= read -r call
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"fixture result"}],"structuredContent":{"ok":true},"isError":false}}'
IFS= read -r typed_call
printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"content":[{"type":"text","text":"typed fixture result"}],"structuredContent":{"typed":true},"isError":false}}'
IFS= read -r forth_call
printf '%s\n' '{"jsonrpc":"2.0","id":5,"result":{"content":[{"type":"text","text":"forth fixture result"}],"structuredContent":{"forth":true},"isError":false}}'
"#;
        let config = std::collections::HashMap::from([(
            "fixture".to_string(),
            crate::tools::mcp::McpServerConfig {
                command: Some("sh".to_string()),
                args: vec!["-c".to_string(), script.to_string()],
                transport: crate::tools::mcp::TransportType::Stdio,
                url: None,
                env: std::collections::HashMap::new(),
                enabled: true,
                timeout_secs: 5,
            },
        )]);
        let client = Arc::new(
            crate::tools::mcp::McpClient::from_config(&config)
                .await
                .unwrap(),
        );
        let runtime = ProgramRuntime::new();
        assert!(!runtime.has_mcp_client());
        assert!(runtime.bind_mcp_client(client).await.unwrap().is_empty());
        assert!(runtime.has_mcp_client());
        let state = runtime.inspect().await.unwrap();
        let namespaced = state
            .typed_vocabulary
            .iter()
            .find(|entry| entry.name == "mcp.fixture.echo_value")
            .expect("discovered MCP word");
        assert!(namespaced
            .signature
            .as_deref()
            .unwrap()
            .contains("record{value:string}"));
        assert!(namespaced
            .version
            .as_deref()
            .unwrap()
            .starts_with("sha256:"));
        assert!(namespaced
            .documentation
            .as_deref()
            .unwrap()
            .contains("Untrusted server description"));
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::McpCall,
            selector: ResourceSelector::Mcp {
                server: "fixture".into(),
                tool: "echo_value".into(),
            },
        };
        runtime.grant_typed_capability(requirement).unwrap();

        let mut generic = submission(
            ProgramLanguage::Lisp,
            r#"(mcp-call "fixture" "echo_value" (result-unwrap (json-parse "{\"value\":\"hello\"}")))"#,
            ExecutionEffect::ExternalRead,
        );
        generic.manifest_generation = runtime.manifest_generation();
        let outcome = runtime.submit(generic).await.unwrap();

        assert_eq!(
            outcome.status,
            ExecutionStatus::Completed,
            "diagnostics: {:?}",
            outcome.diagnostics
        );
        assert!(matches!(
            outcome.values.as_slice(),
            [ProgramValue::Json(value)]
                if value["structuredContent"]["ok"] == serde_json::Value::Bool(true)
        ));

        let mut typed = submission(
            ProgramLanguage::Lisp,
            r#"(mcp.fixture.echo_value { :value "typed" })"#,
            ExecutionEffect::ExternalRead,
        );
        typed.manifest_generation = runtime.manifest_generation();
        let outcome = runtime.submit(typed).await.unwrap();
        assert_eq!(
            outcome.status,
            ExecutionStatus::Completed,
            "diagnostics: {:?}",
            outcome.diagnostics
        );
        assert!(
            matches!(
                outcome.values.last(),
                Some(ProgramValue::Json(value))
                    if value["structuredContent"]["typed"] == serde_json::Value::Bool(true)
            ),
            "values: {:?}",
            outcome.values
        );

        let mut forth = submission(
            ProgramLanguage::Forth,
            r#"{ value: "forth" } mcp.fixture.echo_value"#,
            ExecutionEffect::ExternalRead,
        );
        forth.manifest_generation = runtime.manifest_generation();
        let outcome = runtime.submit(forth).await.unwrap();
        assert_eq!(
            outcome.status,
            ExecutionStatus::Completed,
            "diagnostics: {:?}",
            outcome.diagnostics
        );
        assert!(matches!(
            outcome.values.last(),
            Some(ProgramValue::Json(value))
                if value["structuredContent"]["forth"] == serde_json::Value::Bool(true)
        ));
    }

    #[tokio::test]
    async fn inspection_exposes_typed_stack_vocabulary_and_grants() {
        let runtime = ProgramRuntime::new();
        runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(+ 20 22)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let state = runtime.inspect().await.unwrap();
        assert_eq!(state.typed_stack.len(), 1);
        assert_eq!(state.typed_stack[0].value, TypedValue::Int(42));
        assert!(state.typed_vocabulary.iter().any(|word| word.name == "say"));
        assert!(state
            .granted_capabilities
            .iter()
            .any(|grant| grant.capability == crate::vm::CapabilityKind::SessionEmit));
    }

    #[tokio::test]
    async fn inspection_exposes_typed_definition_documentation() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(define (double (n : int)) : int \"Return twice n.\" (* n 2))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);

        let state = runtime.inspect().await.unwrap();
        let double = state
            .typed_vocabulary
            .iter()
            .find(|word| word.name == "double")
            .expect("persisted typed definition");
        assert_eq!(double.documentation.as_deref(), Some("Return twice n."));
    }

    #[tokio::test]
    async fn typed_vm_can_introspect_its_vocabulary() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(vm-vocabulary)",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        let Some(ProgramValue::String(manifest)) = outcome.values.first() else {
            panic!("expected serialized vocabulary");
        };
        assert!(manifest.contains("vm-vocabulary"));
        assert!(manifest.contains("file-read"));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn approved_typed_process_runs_without_a_shell() {
        let runtime = ProgramRuntime::new();
        let request = submission(
            ProgramLanguage::Lisp,
            "(process-run \"/usr/bin/printf\" (list \"ok\"))",
            ExecutionEffect::ExternalWrite,
        );
        let pending = runtime.submit(request.clone()).await.unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let ResourceSelector::Process { executables } = &pending.required_capabilities[0].selector
        else {
            panic!("process approval must expose a stable executable identity");
        };
        let approved_identity = ProcessExecutableIdentity::decode(&executables[0]).unwrap();
        assert_eq!(approved_identity.path, "/usr/bin/printf");
        runtime
            .grant_typed_capability(pending.required_capabilities[0].clone())
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.values, vec![ProgramValue::String("ok".into())]);
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn process_run_rejects_bare_and_relative_commands_without_consulting_path() {
        let runtime = ProgramRuntime::new();
        for command in ["printf", "./printf"] {
            let source = format!(
                "(process-run {} (list \"unused\"))",
                serde_json::to_string(command).unwrap()
            );
            let outcome = runtime
                .submit(submission(
                    ProgramLanguage::Lisp,
                    &source,
                    ExecutionEffect::ExternalWrite,
                ))
                .await
                .unwrap();
            assert_eq!(outcome.status, ExecutionStatus::Failed);
            assert!(outcome.required_capabilities.is_empty());
            assert!(outcome
                .diagnostics
                .iter()
                .any(|message| message.contains("PATH and relative lookup are forbidden")));
        }
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .authorization_audit
            .is_empty());
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn process_run_rejects_symlinks_even_after_retargeting() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let link = directory.path().join("tool");
        symlink("/usr/bin/true", &link).unwrap();
        let source = format!(
            "(process-run {} (list \"unused\"))",
            serde_json::to_string(&link.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let first = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(first.status, ExecutionStatus::Failed);
        std::fs::remove_file(&link).unwrap();
        symlink("/usr/bin/false", &link).unwrap();
        let second = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(second.status, ExecutionStatus::Failed);
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .authorization_audit
            .is_empty());
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn replaced_process_executable_does_not_consume_once_grant() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tool");
        std::fs::copy("/usr/bin/true", &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        let source = format!(
            "(process-run {} (list \"unused\"))",
            serde_json::to_string(&executable.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(
            pending.status,
            ExecutionStatus::AuthorizationRequired,
            "diagnostics: {:?}",
            pending.diagnostics
        );

        std::fs::copy("/usr/bin/false", &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let failed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        let ledger = runtime.capability_ledger().unwrap();
        let once = ledger
            .grants
            .grants
            .last()
            .expect("once grant was recorded");
        assert!(matches!(once.scope, GrantScope::Once { .. }));
        assert!(once.consumed_at_unix_ms.is_none());
        assert!(ledger.authorization_audit.is_empty());
        assert!(!ledger
            .audit
            .iter()
            .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn atomic_path_replacement_executes_the_authorized_open_object() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tool");
        let replacement = directory.path().join("replacement");
        std::fs::copy("/usr/bin/printf", &executable).unwrap();
        std::fs::copy("/usr/bin/false", &replacement).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        let source = format!(
            "(process-run {} (list \"opened-object\"))",
            serde_json::to_string(&executable.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

        let expected = executable.to_string_lossy().into_owned();
        *PROCESS_BEFORE_EXEC_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some((
            expected,
            Box::new(move || std::fs::rename(replacement, executable).unwrap()),
        ));
        let completed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::String("opened-object".into())]
        );
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger
            .grants
            .grants
            .last()
            .unwrap()
            .consumed_at_unix_ms
            .is_some());
        assert!(matches!(
            ledger
                .authorization_audit
                .last()
                .map(|entry| &entry.decision),
            Some(AuthorizationDecision::Allowed { .. })
        ));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn same_inode_mutation_cannot_change_the_snapshotted_process_invocation() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tool");
        std::fs::copy("/usr/bin/printf", &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        let source = format!(
            "(process-run {} (list \"same-inode\"))",
            serde_json::to_string(&executable.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let ResourceSelector::Process { executables } = &pending.required_capabilities[0].selector
        else {
            panic!("process requirement must be concrete")
        };
        let identity = ProcessExecutableIdentity::decode(&executables[0]).unwrap();
        assert_eq!(identity.arguments, vec!["same-inode"]);
        assert_eq!(
            identity.environment_sha256,
            format!("{:x}", Sha256::digest(b""))
        );
        assert_eq!(
            identity.cwd_path,
            std::env::current_dir().unwrap().to_string_lossy()
        );

        let expected_path = executable.to_string_lossy().into_owned();
        let original_inode = std::fs::metadata(&executable).unwrap().ino();
        let mutated = executable.clone();
        *PROCESS_BEFORE_EXEC_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some((
            expected_path,
            Box::new(move || {
                std::fs::copy("/usr/bin/false", &mutated).unwrap();
                std::fs::set_permissions(&mutated, std::fs::Permissions::from_mode(0o700)).unwrap();
                assert_eq!(std::fs::metadata(&mutated).unwrap().ino(), original_inode);
            }),
        ));
        let completed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::String("same-inode".into())]
        );
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[test]
    fn authority_restart_rejects_same_inode_process_mutation() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tool");
        std::fs::copy("/usr/bin/printf", &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        let original_inode = std::fs::metadata(&executable).unwrap().ino();
        let identity = open_process_executable(&executable.to_string_lossy(), &["approved".into()])
            .unwrap()
            .identity;
        let runtime = ProgramRuntime::new();
        let mut ledger = CapabilityLedger::default();
        ledger
            .issue(
                CapabilityRequirement {
                    capability: CapabilityKind::ProcessRun,
                    selector: ResourceSelector::Process {
                        executables: vec![identity.encode()],
                    },
                },
                GrantScope::Global,
                runtime.capability_policy().unwrap().policy_hash,
                "test-user",
                unix_time_ms(),
                None,
            )
            .unwrap();
        std::fs::copy("/usr/bin/false", &executable).unwrap();
        assert_eq!(
            std::fs::metadata(&executable).unwrap().ino(),
            original_inode
        );
        let error = runtime
            .restore_capability_ledger(ledger)
            .expect_err("restart must revalidate executable bytes");
        assert!(format!("{error:#}").contains("identity changed since approval"));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn owner_mode_other_execute_does_not_reach_authorization() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        if nix::unistd::geteuid().as_raw() == 0 {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("tool");
        std::fs::copy("/usr/bin/true", &executable).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o401)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        assert_eq!(
            std::fs::metadata(&executable).unwrap().uid(),
            nix::unistd::geteuid().as_raw()
        );
        let source = format!(
            "(process-run {} (list \"unused\"))",
            serde_json::to_string(&executable.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let failed = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        assert!(failed
            .diagnostics
            .iter()
            .any(|message| message.contains("not executable by the effective user")));
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger.grants.grants.is_empty());
        assert!(ledger.authorization_audit.is_empty());
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn kernel_rejected_launch_does_not_consume_or_audit_once_grant() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("invalid-executable");
        std::fs::write(&executable, b"not an executable image").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let executable = std::fs::canonicalize(executable).unwrap();
        let source = format!(
            "(process-run {} (list \"unused\"))",
            serde_json::to_string(&executable.to_string_lossy()).unwrap()
        );
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &source,
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

        let failed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        let ledger = runtime.capability_ledger().unwrap();
        let once = ledger
            .grants
            .grants
            .last()
            .expect("once grant remains visible");
        assert!(matches!(once.scope, GrantScope::Once { .. }));
        assert!(once.consumed_at_unix_ms.is_none());
        assert!(ledger.authorization_audit.is_empty());
        assert!(!ledger
            .audit
            .iter()
            .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn wrong_process_origin_is_rejected_before_once_grant_issuance() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(process-run \"/usr/bin/true\" (list \"unused\"))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        {
            let mut pending_runs = runtime.pending_typed.lock().unwrap();
            let saved = pending_runs.get_mut(&pending.execution_id).unwrap();
            let forged = SourceOrigin::generated("legacy-process-run");
            saved.suspension.pending_host_call.as_mut().unwrap().origin = forged.clone();
            saved.suspension.event_journal.last_mut().unwrap().origin = forged;
        }
        let error = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("stale or forged"));
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger.grants.grants.is_empty());
        assert!(ledger.authorization_audit.is_empty());
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    #[tokio::test]
    async fn process_run_is_unavailable_without_stable_opened_object_execution() {
        let runtime = ProgramRuntime::new();
        let requirement = CapabilityRequirement {
            capability: CapabilityKind::ProcessRun,
            selector: ResourceSelector::Process {
                executables: Vec::new(),
            },
        };
        assert_eq!(
            runtime.capability_availability(&requirement),
            CapabilityAvailability::Unsupported
        );
        let outcome = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(process-run \"/usr/bin/true\" (list))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert!(!outcome.diagnostics.is_empty());
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger.grants.grants.is_empty());
        assert!(ledger.authorization_audit.is_empty());
    }

    #[tokio::test]
    async fn typed_proposal_open_is_an_explicit_capability_and_returns_edited_artifact_data() {
        let runtime = ProgramRuntime::new();
        let request = submission(
            ProgramLanguage::Lisp,
            "(proposal-open \"python\" \"show an artifact\" \"print('ok')\")",
            ExecutionEffect::ExternalWrite,
        );
        let pending = runtime.submit(request.clone()).await.unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(
            pending.required_capabilities,
            vec![crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            }]
        );

        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let accepted = runtime.submit(request).await.unwrap();
        assert_eq!(accepted.status, ExecutionStatus::Completed);
        assert_eq!(
            accepted.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::String("print('ok')".into())),
            })))]
        );
    }

    #[tokio::test]
    async fn coforth_proposal_open_uses_the_same_typed_host_boundary() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["forth".into()],
                },
            })
            .unwrap();
        let accepted = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"forth\" s\"show an artifact\" s\"1 2 +\" proposal-open",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(accepted.status, ExecutionStatus::Completed);
        assert_eq!(
            accepted.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::String("1 2 +".into())),
            })))]
        );
    }

    #[tokio::test]
    async fn proposal_open_can_suspend_for_an_external_editor_and_resume_once() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink_observed = Arc::clone(&observed);
        let pending = runtime
            .submit_with_deferred_program_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(proposal-open \"python\" \"show an artifact\" \"print('original')\")",
                    ExecutionEffect::ExternalWrite,
                ),
                Arc::new(move |effect| sink_observed.lock().unwrap().push(effect)),
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);
        let event = observed.lock().unwrap().pop().expect("proposal event");
        assert_eq!(event.execution_id, pending.execution_id);
        assert_eq!(
            event.handle(),
            VmEffectHandle {
                execution_id: pending.execution_id,
                sequence: event.effect.sequence,
            }
        );
        assert_eq!(
            event.effect.requirement.capability,
            crate::vm::CapabilityKind::ProgramInvoke
        );
        assert!(matches!(
            event.effect.event,
            crate::vm::interpreter::HostSideEffect::Request { ref arguments }
                if matches!(arguments.as_slice(), [TypedValue::String(language), ..] if language == "python")
        ));
        let info = runtime
            .pending_typed_execution(pending.execution_id)
            .unwrap()
            .expect("proposal continuation");
        assert_eq!(info.resume_effect_sequence, Some(event.effect.sequence));
        assert!(matches!(
            info.reason,
            PendingTypedReason::AwaitingHostEffect { .. }
        ));
        assert!(matches!(
            pending.effect_journal.last().map(|entry| &entry.state),
            Some(crate::vm::EffectJournalState::AwaitingHostResult)
        ));

        let accepted = runtime
            .resume_typed_execution_with_effect_result(
                pending.execution_id,
                event.effect.sequence,
                vec![TypedValue::Option {
                    inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                    value: Some(Box::new(TypedValue::Result {
                        ok_type: Type::String,
                        error_type: Type::String,
                        is_ok: true,
                        value: Box::new(TypedValue::String("print('edited')".into())),
                    })),
                }],
            )
            .await
            .unwrap();
        assert_eq!(accepted.status, ExecutionStatus::Completed);
        assert_eq!(
            accepted.values,
            vec![ProgramValue::Option(Some(Box::new(ProgramValue::Result {
                ok: true,
                value: Box::new(ProgramValue::String("print('edited')".into())),
            })))]
        );
        assert!(matches!(
            accepted.effect_journal.last().map(|entry| &entry.state),
            Some(crate::vm::EffectJournalState::Acknowledged { values })
                if matches!(values.as_slice(), [TypedValue::Option { .. }])
        ));
    }

    #[tokio::test]
    async fn portable_effect_channel_round_trips_a_deferred_proposal_resume() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let (sink, receiver) = typed_effect_channel();
        let pending = runtime
            .submit_with_deferred_program_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(proposal-open \"python\" \"show an artifact\" \"print('original')\")",
                    ExecutionEffect::ExternalWrite,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);

        let envelope = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("portable effect envelope");
        assert_eq!(envelope.execution_id, pending.execution_id);
        assert_eq!(
            envelope.effect.requirement.capability,
            crate::vm::CapabilityKind::ProgramInvoke
        );

        let outcome = runtime
            .resume_vm_effect(VmResume {
                execution_id: envelope.execution_id,
                sequence: envelope.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Option {
                        inner_type: Type::Result(Box::new(Type::String), Box::new(Type::String)),
                        value: Some(Box::new(TypedValue::Result {
                            ok_type: Type::String,
                            error_type: Type::String,
                            is_ok: true,
                            value: Box::new(TypedValue::String("print('edited')".into())),
                        })),
                    }],
                },
            })
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(matches!(
            outcome.effect_journal.last().map(|entry| &entry.state),
            Some(crate::vm::EffectJournalState::Acknowledged { values })
                if matches!(values.as_slice(), [TypedValue::Option { .. }])
        ));
        assert!(
            receiver.try_recv().is_err(),
            "the VM must not redispatch the effect"
        );
    }

    #[tokio::test]
    async fn portable_host_boundary_can_defer_a_file_read_without_touching_the_host() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let (sink, receiver) = typed_effect_channel();
        let pending = runtime
            .submit_with_deferred_host_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(file-read (path \"does-not-need-to-exist.txt\"))",
                    ExecutionEffect::WorkspaceRead,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);

        let envelope = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("file-read effect envelope");
        assert_eq!(envelope.execution_id, pending.execution_id);
        assert_eq!(
            envelope.effect.requirement.capability,
            crate::vm::CapabilityKind::FileRead
        );
        assert_eq!(envelope.effect.output, vec![Type::Bytes]);

        let completed = runtime
            .resume_vm_effect(VmResume {
                execution_id: envelope.execution_id,
                sequence: envelope.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Bytes(b"embedder bytes".to_vec())],
                },
            })
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Bytes(b"embedder bytes".to_vec())]
        );
    }

    #[test]
    fn deferred_effect_sink_can_reenter_authority_without_deadlock() {
        let runtime = Arc::new(ProgramRuntime::new());
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let worker_runtime = Arc::clone(&runtime);
        let worker = std::thread::spawn(move || {
            let sink_runtime = Arc::clone(&worker_runtime);
            let sink: TypedEffectSink = Arc::new(move |_| {
                let ledger = sink_runtime
                    .capability_ledger()
                    .expect("deferred host sink can reenter public authority inspection");
                assert_eq!(ledger.authorization_audit.len(), 1);
            });
            let tokio = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = tokio.block_on(worker_runtime.submit_with_deferred_host_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(file-read (path \"host-owned.txt\"))",
                    ExecutionEffect::WorkspaceRead,
                ),
                sink,
            ));
            result_tx.send(result).unwrap();
        });

        let pending = result_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("deferred host dispatch must not retain the ledger lock across its sink")
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);
        worker.join().unwrap();
    }

    #[tokio::test]
    async fn portable_host_boundary_retains_its_policy_across_multiple_resumes() {
        let runtime = ProgramRuntime::new();
        let grant_id = runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let (sink, receiver) = typed_effect_channel();
        let pending = runtime
            .submit_with_deferred_host_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(begin (file-read (path \"first.txt\")) (file-read (path \"second.txt\")))",
                    ExecutionEffect::WorkspaceRead,
                ),
                sink,
            )
            .await
            .unwrap();
        let first = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first file-read effect");
        assert_eq!(first.execution_id, pending.execution_id);

        let still_pending = runtime
            .resume_vm_effect(VmResume {
                execution_id: first.execution_id,
                sequence: first.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Bytes(b"first".to_vec())],
                },
            })
            .await
            .unwrap();
        assert_eq!(still_pending.status, ExecutionStatus::Suspended);
        let second = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("second file-read effect");
        assert_eq!(second.execution_id, first.execution_id);
        assert_eq!(second.effect.sequence, first.effect.sequence + 1);

        let completed = runtime
            .resume_vm_effect(VmResume {
                execution_id: second.execution_id,
                sequence: second.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Bytes(b"second".to_vec())],
                },
            })
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Bytes(b"second".to_vec())]
        );
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.authorization_audit.len(), 2);
        assert_eq!(
            ledger
                .authorization_audit
                .iter()
                .map(|entry| entry.effect_sequence)
                .collect::<Vec<_>>(),
            vec![Some(first.effect.sequence), Some(second.effect.sequence)]
        );
        assert!(ledger.authorization_audit.iter().all(|entry| matches!(
            entry.decision,
            AuthorizationDecision::Allowed { grant_id: used } if used == grant_id
        )));
    }

    #[tokio::test]
    async fn typed_effect_sink_projects_proposal_request() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(proposal-open \"python\" \"show an artifact\" \"print('ok')\")",
                    ExecutionEffect::ExternalWrite,
                ),
                Arc::new(move |effect| sink_events.lock().unwrap().push(effect)),
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(events.lock().unwrap().iter().any(|effect| {
            effect.effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
        }));
    }

    #[tokio::test]
    async fn proposal_grant_cannot_be_reused_for_a_different_artifact_language() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["python".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(proposal-open \"bash\" \"show an artifact\" \"echo nope\")",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(
            outcome.required_capabilities,
            vec![crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProgramInvoke,
                selector: crate::vm::ResourceSelector::Program {
                    languages: vec!["bash".into()],
                },
            }]
        );
    }

    #[tokio::test]
    async fn proposal_open_rejects_an_unsupported_artifact_language_before_host_dispatch() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(proposal-open \"fortran\" \"show an artifact\" \"program x\")",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert!(outcome
            .vm_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E-CAP-003"));
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn process_grant_cannot_be_reused_for_a_different_executable() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::Process {
                    executables: vec!["/usr/bin/printf".into()],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(process-run \"/usr/bin/true\" (list \"unused\"))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(outcome.required_capabilities.len(), 1);
        let ResourceSelector::Process { executables } = &outcome.required_capabilities[0].selector
        else {
            panic!("process request must use a stable executable identity");
        };
        assert_eq!(
            ProcessExecutableIdentity::decode(&executables[0])
                .unwrap()
                .path,
            "/usr/bin/true"
        );
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    #[tokio::test]
    async fn process_grant_cannot_be_reused_for_different_arguments() {
        let runtime = ProgramRuntime::new();
        let first = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(process-run \"/usr/bin/printf\" (list \"approved\"))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        runtime
            .grant_typed_capability(first.required_capabilities[0].clone())
            .unwrap();
        let mismatched = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(process-run \"/usr/bin/printf\" (list \"hostile\"))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(mismatched.status, ExecutionStatus::AuthorizationRequired);
        let ResourceSelector::Process { executables } =
            &mismatched.required_capabilities[0].selector
        else {
            panic!("process request must bind its exact arguments")
        };
        assert_eq!(
            ProcessExecutableIdentity::decode(&executables[0])
                .unwrap()
                .arguments,
            vec!["hostile"]
        );
    }

    #[tokio::test]
    async fn approved_typed_network_connect_and_send_use_scoped_host_binding() {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut input = [0; 4];
            std::io::Read::read_exact(&mut stream, &mut input).unwrap();
            assert_eq!(&input, b"ping");
            std::io::Write::write_all(&mut stream, b"pong").unwrap();
        });
        let runtime = ProgramRuntime::new();
        let source =
            format!("s\" 127.0.0.1\" {port} network-connect s\" ping\" bytes network-send");
        let request = submission(
            ProgramLanguage::Forth,
            &source,
            ExecutionEffect::ExternalWrite,
        );
        let pending = runtime.submit(request.clone()).await.unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::NetworkConnect,
                selector: crate::vm::ResourceSelector::Network {
                    host: "127.0.0.1".into(),
                    ports: vec![port],
                },
            })
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.values, vec![ProgramValue::Bytes(b"pong".to_vec())]);
        server.join().unwrap();
        assert!(
            runtime.network.lock().unwrap().is_empty(),
            "terminal ProgramRun must drop every owned socket"
        );
    }

    #[tokio::test]
    async fn network_grant_cannot_be_reused_for_a_different_host() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::NetworkConnect,
                selector: crate::vm::ResourceSelector::Network {
                    host: "127.0.0.1".into(),
                    ports: vec![443],
                },
            })
            .unwrap();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"example.test\" 443 network-connect",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(outcome.required_capabilities.len(), 1);
        assert!(matches!(
            outcome.required_capabilities[0].selector,
            crate::vm::ResourceSelector::Network { ref host, ref ports }
                if host == "example.test" && ports == &[443]
        ));
    }

    #[tokio::test]
    async fn same_run_revocation_before_network_send_prevents_payload_and_releases_socket() {
        let listener = match std::net::TcpListener::bind(("127.0.0.1", 0)) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(100)))
                .unwrap();
            let mut byte = [0; 1];
            match (&stream).read(&mut byte) {
                Ok(0) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Ok(size) => panic!("unexpected payload of {size} bytes"),
                Err(error) => panic!("unexpected socket read error: {error}"),
            }
        });
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                &format!("s\"127.0.0.1\" {port} network-connect s\"ping\" bytes network-send"),
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);

        // Both hooks target the same ProgramRun and capability. The first
        // permits connect; the second revokes its freshly issued session
        // grant at the send lease boundary, after the socket exists but
        // before any payload syscall can occur.
        let ledger = Arc::clone(&runtime.capability_ledger);
        let mut hooks = AUTHORIZATION_BEFORE_USE_HOOK
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap();
        hooks.push((
            pending.execution_id,
            CapabilityKind::NetworkConnect,
            Box::new(|| {}),
        ));
        hooks.push((
            pending.execution_id,
            CapabilityKind::NetworkConnect,
            Box::new(move || {
                let mut ledger = ledger.lock().unwrap();
                let grant_id = ledger
                    .grants
                    .grants
                    .iter()
                    .rev()
                    .find(|grant| grant.revoked_at_unix_ms.is_none())
                    .expect("session approval issued an active network grant")
                    .id;
                assert!(ledger.revoke(grant_id, "same-run-revoker", unix_time_ms()));
            }),
        ));
        drop(hooks);

        let sent = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowSession,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(sent.execution_id, pending.execution_id);
        assert_eq!(sent.status, ExecutionStatus::Failed);
        assert!(sent.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("revoked") || diagnostic.contains("replaced")
        }));
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.authorization_audit.len(), 1);
        server.join().unwrap();
        assert!(
            runtime.network.lock().unwrap().is_empty(),
            "failed terminal ProgramRun must drop its connected socket"
        );
    }

    #[tokio::test]
    async fn typed_say_emits_stream_chunks_and_buffers_result() {
        let runtime = ProgramRuntime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink_events = Arc::clone(&events);
        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Forth,
                    "s\" first\" say s\" second\" say",
                    ExecutionEffect::VmRead,
                ),
                Arc::new(move |event| sink_events.lock().unwrap().push(event)),
            )
            .await
            .unwrap();
        assert_eq!(outcome.output, "firstsecond");
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].execution_id, outcome.execution_id);
        assert_eq!(events[0].effect.sequence, 0);
        assert_eq!(events[1].effect.sequence, 1);
        assert_eq!(
            events
                .iter()
                .map(|event| match &event.effect.event {
                    crate::vm::interpreter::HostSideEffect::Emit { text } => text.as_str(),
                    other => panic!("expected emit event, found {other:?}"),
                })
                .collect::<Vec<_>>(),
            vec!["first", "second"]
        );
        assert_eq!(outcome.output_chunks, vec!["first", "second"]);
        assert_eq!(
            outcome.side_effects,
            vec![
                crate::vm::interpreter::HostSideEffect::Emit {
                    text: "first".into()
                },
                crate::vm::interpreter::HostSideEffect::Emit {
                    text: "second".into()
                }
            ]
        );
    }

    #[tokio::test]
    async fn typed_forth_dot_quote_is_a_session_emit_shorthand() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ".\" hello from standard Forth\"",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
        assert_eq!(outcome.output, "hello from standard Forth");
        assert_eq!(
            outcome.side_effects,
            vec![crate::vm::interpreter::HostSideEffect::Emit {
                text: "hello from standard Forth".into(),
            }]
        );
    }

    #[tokio::test]
    async fn typed_forth_s_quote_pushes_text_without_emitting_it() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" retained value\"",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.backend, ExecutionBackend::TypedVm);
        assert_eq!(
            outcome.values,
            vec![crate::programs::ProgramValue::String(
                "retained value".into()
            )]
        );
        assert!(outcome.output.is_empty());
        assert!(outcome.side_effects.is_empty());
        assert!(outcome.effect_journal.is_empty());
    }

    #[tokio::test]
    async fn typed_forth_cr_is_an_explicit_session_emit_newline() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" first\" say cr s\" second\" say",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.output, "first\nsecond");
        assert_eq!(
            outcome.output_chunks,
            vec!["first".to_string(), "\n".to_string(), "second".to_string()]
        );
    }

    #[tokio::test]
    async fn typed_forth_can_say_computed_values_progressively() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"the result of 2+3 is \" say 2 3 + int-to-string space str-cat say s\"is that correct?\" say",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.output, "the result of 2+3 is 5 is that correct?");
    }

    #[tokio::test]
    async fn typed_output_handles_are_owned_by_their_program_run() {
        let runtime = ProgramRuntime::new();
        let opened = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"build\" output-open",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert!(matches!(
            opened.values.as_slice(),
            [ProgramValue::Resource { kind, .. }] if kind == "output-handle"
        ));

        // A later submission may retain the opaque value on the persistent
        // VM stack, but it must not be able to update the previous run's
        // presentation resource.
        let update = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"still working\" output-status",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(update.status, ExecutionStatus::Failed);
        assert!(update
            .vm_diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "E-OUTPUT-HANDLE-003"));
    }

    #[tokio::test]
    async fn synchronous_output_open_projects_one_sequence_ordered_create_event() {
        let runtime = ProgramRuntime::new();
        let output_manager = Arc::new(crate::cli::OutputManager::default());
        output_manager.disable_stdout();
        let response = output_manager.start_work_unit("VM program output");
        response.set_program_output();
        let projection =
            crate::cli::VmOutputProjection::new(Arc::clone(&output_manager), Arc::clone(&response));
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: TypedEffectSink = {
            let events = Arc::clone(&events);
            Arc::new(move |effect| events.lock().unwrap().push(effect))
        };

        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Lisp,
                    "(let ((handle (output-open \"download\")))
                       (begin (output-status handle \"starting\")
                              (output-complete handle)))",
                    ExecutionEffect::VmRead,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Completed);
        let events = events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .map(|envelope| envelope.effect.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1, 2],
            "one visible event must exist for each UI sequence"
        );
        assert!(events.iter().all(|effect| !matches!(
            effect.effect.event,
            crate::vm::interpreter::HostSideEffect::Request { .. }
        )));
        assert!(matches!(
            events.first().map(|effect| &effect.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Create,
                text: Some(title),
                target: Some(TypedValue::Resource { kind, .. }),
                ..
            }) if title == "download" && kind == "output-handle"
        ));
        assert!(matches!(
            events.last().map(|effect| &effect.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Complete,
                ..
            })
        ));
        for event in events.iter() {
            assert!(
                !projection.project_envelope(event.clone()).is_empty(),
                "the UI projection must not discard a same-sequence create event"
            );
        }
        let messages = output_manager.get_messages();
        assert_eq!(messages.len(), 2, "response port plus output handle");
        assert_eq!(
            messages[1].status(),
            crate::cli::messages::MessageStatus::Complete
        );
        assert!(messages[1]
            .format(&crate::config::ColorScheme::default())
            .contains("download"));
    }

    #[tokio::test]
    async fn typed_forth_output_handle_can_be_updated_in_its_creating_run() {
        let runtime = ProgramRuntime::new();
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink: TypedEffectSink = {
            let events = Arc::clone(&events);
            Arc::new(move |effect| events.lock().unwrap().push(effect))
        };

        let outcome = runtime
            .submit_with_typed_effect_sink(
                submission(
                    ProgramLanguage::Forth,
                    "s\"download\" output-open dup s\"starting\" output-status dup 2 5 output-progress output-complete",
                    ExecutionEffect::VmRead,
                ),
                sink,
            )
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert!(outcome.output.is_empty());
        let events = events.lock().unwrap();
        assert!(matches!(
            events.first().map(|effect| &effect.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Create,
                ..
            })
        ));
        assert!(events.iter().any(|effect| matches!(
            &effect.effect.event,
            crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Create,
                text: Some(title),
                ..
            } if title == "download"
        )));
        assert!(events.iter().any(|effect| matches!(
            &effect.effect.event,
            crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Status,
                text: Some(text),
                ..
            } if text == "starting"
        )));
        assert!(events.iter().any(|effect| matches!(
            &effect.effect.event,
            crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Progress,
                progress: Some(crate::vm::interpreter::UiProgress {
                    completed: 2,
                    total: Some(5),
                }),
                ..
            }
        )));
        assert!(matches!(
            events.last().map(|effect| &effect.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Complete,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn portable_output_open_registers_the_host_issued_handle_for_later_updates() {
        let runtime = ProgramRuntime::new();
        let (sink, receiver) = typed_effect_channel();
        let pending = runtime
            .submit_with_deferred_host_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(let ((handle (output-open \"download\"))) \
                       (begin (output-status handle \"starting\") \
                              (output-complete handle)))",
                    ExecutionEffect::VmRead,
                ),
                sink,
            )
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);

        let open = receiver
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("portable output-open request");
        assert_eq!(open.execution_id, pending.execution_id);
        assert!(matches!(
            &open.effect.event,
            crate::vm::interpreter::HostSideEffect::Request { arguments }
                if matches!(arguments.as_slice(), [TypedValue::String(title)] if title == "download")
        ));

        let completed = runtime
            .resume_vm_effect(VmResume {
                execution_id: open.execution_id,
                sequence: open.effect.sequence,
                response: VmResumeResponse::Result {
                    values: vec![TypedValue::Resource {
                        kind: "output-handle".into(),
                        handle: "portable-download".into(),
                        generation: 7,
                    }],
                },
            })
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.output, "");

        let updates = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(
            updates
                .iter()
                .map(|envelope| envelope.effect.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(matches!(
            updates.first().map(|envelope| &envelope.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Status,
                text: Some(text),
                target: Some(TypedValue::Resource { handle, generation, .. }),
                ..
            }) if text == "starting" && handle == "portable-download" && *generation == 7
        ));
        assert!(matches!(
            updates.last().map(|envelope| &envelope.effect.event),
            Some(crate::vm::interpreter::HostSideEffect::Ui {
                operation: crate::vm::interpreter::UiOperation::Complete,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn inspection_reports_ordered_stack_and_vocabulary() {
        let runtime = ProgramRuntime::new();
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "10 20",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let state = runtime.inspect().await.unwrap();
        assert_eq!(state.revision, 1);
        assert_eq!(state.stack.len(), state.typed_stack.len());
        assert_eq!(state.stack[0].value, ProgramValue::Int(10));
        assert_eq!(state.stack[1].value, ProgramValue::Int(20));
        assert!(state.vocabulary.iter().any(|word| word.name == "+"));
        assert_eq!(state.vocabulary, state.typed_vocabulary);
    }

    #[tokio::test]
    async fn revision_history_records_only_successful_commit_boundaries() {
        let runtime = ProgramRuntime::new();
        assert_eq!(runtime.revision_history().unwrap().len(), 1);
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "7",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let suspended = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "unit yield 9",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(suspended.status, ExecutionStatus::Suspended);

        let history = runtime.revision_history().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].revision, 1);
        assert_eq!(history[1].stack, vec![TypedValue::Int(7)]);
        assert!(history[1].checkpoint.is_some());
    }

    #[tokio::test]
    async fn newer_runner_checkpoint_hydrates_without_importing_authority() {
        let source = ProgramRuntime::new();
        let defined = source
            .submit(submission(
                ProgramLanguage::Lisp,
                "(define (double (n : int)) : int (* n 2))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let checkpoint = source
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.revision == defined.output_revision)
            .and_then(|snapshot| snapshot.checkpoint)
            .unwrap();

        let runner = ProgramRuntime::new();
        let local_grant = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./**").unwrap(),
        );
        runner.grant_typed_capability(local_grant.clone()).unwrap();
        let authority_before = runner.capability_ledger().unwrap();
        assert!(runner
            .hydrate_reducible_state_if_newer(checkpoint, defined.output_revision)
            .await
            .unwrap());
        assert_eq!(runner.revision(), defined.output_revision);
        assert_eq!(runner.capability_ledger().unwrap(), authority_before);

        let called = runner
            .submit(submission(
                ProgramLanguage::Forth,
                "21 double",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert!(matches!(called.values.as_slice(), [ProgramValue::Int(42)]));
    }

    #[tokio::test]
    async fn switching_brains_replaces_an_unrelated_higher_revision_lineage() {
        let source = ProgramRuntime::new();
        let defined = source
            .submit(submission(
                ProgramLanguage::Lisp,
                "(define (double (n : int)) : int (* n 2))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let checkpoint = source
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.revision == defined.output_revision)
            .and_then(|snapshot| snapshot.checkpoint)
            .unwrap();

        let runner = ProgramRuntime::new();
        runner
            .submit(submission(
                ProgramLanguage::Forth,
                "10",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        runner
            .submit(submission(
                ProgramLanguage::Forth,
                "20",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert!(runner.revision() > defined.output_revision);

        runner
            .replace_reducible_state(checkpoint, defined.output_revision)
            .await
            .unwrap();
        assert_eq!(runner.revision(), defined.output_revision);
        let called = runner
            .submit(submission(
                ProgramLanguage::Forth,
                "21 double",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert!(matches!(called.values.as_slice(), [ProgramValue::Int(42)]));
    }

    #[tokio::test]
    async fn runner_hydration_never_replaces_pending_continuations() {
        let source = ProgramRuntime::new();
        let first = source
            .submit(submission(
                ProgramLanguage::Forth,
                "1",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let second = source
            .submit(submission(
                ProgramLanguage::Forth,
                "2",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let checkpoint = source
            .revision_history()
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.revision == second.output_revision)
            .and_then(|snapshot| snapshot.checkpoint)
            .unwrap();

        let runner = ProgramRuntime::new();
        let pending = runner
            .submit(submission(
                ProgramLanguage::Forth,
                "unit yield",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);
        let error = runner
            .hydrate_reducible_state_if_newer(checkpoint, first.output_revision + 1)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("continuations are pending"));
        assert_eq!(runner.revision(), 0);
        assert_eq!(runner.pending_typed_execution_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn revision_history_is_a_bounded_restorable_window() {
        let runtime = ProgramRuntime::new();
        let commit_count = MAX_RETAINED_VM_REVISIONS as u64 + 4;
        for _ in 0..commit_count {
            let outcome = runtime
                .submit(submission(
                    ProgramLanguage::Forth,
                    "1 drop",
                    ExecutionEffect::Pure,
                ))
                .await
                .unwrap();
            assert_eq!(outcome.status, ExecutionStatus::Completed);
        }

        let history = runtime.revision_history().unwrap();
        assert_eq!(history.len(), MAX_RETAINED_VM_REVISIONS);
        assert_eq!(history.first().unwrap().revision, 5);
        assert_eq!(history.last().unwrap().revision, commit_count);
        let archive = runtime.archive().unwrap();
        assert_eq!(archive.base_revision, 5);
        assert_eq!(archive.current_revision, commit_count);

        let mut mismatched = archive.clone();
        mismatched.base_revision = 4;
        assert!(ProgramRuntime::from_archive(mismatched)
            .err()
            .expect("mismatched base revision must fail")
            .to_string()
            .contains("not declared base revision 4"));

        let restored = ProgramRuntime::from_archive(archive.clone()).unwrap();
        assert_eq!(restored.revision(), commit_count);
        assert_eq!(
            restored.revision_history().unwrap().len(),
            MAX_RETAINED_VM_REVISIONS
        );
        let next = restored
            .submit(submission(
                ProgramLanguage::Forth,
                "1 drop",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(next.output_revision, commit_count + 1);
        assert_eq!(
            restored
                .revision_history()
                .unwrap()
                .first()
                .unwrap()
                .revision,
            6
        );

        // Version-1 archives written before `base_revision` existed remain
        // loadable even when their lineage began from an application-owned
        // nonzero checkpoint.
        let mut legacy_json = serde_json::to_value(archive).unwrap();
        legacy_json.as_object_mut().unwrap().remove("base_revision");
        ProgramRuntime::from_archive(serde_json::from_value(legacy_json).unwrap()).unwrap();
    }

    #[tokio::test]
    async fn revision_history_checkpoint_restores_persisted_vocabulary() {
        let runtime = ProgramRuntime::new();
        let defined = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ": square ( S int -- S int ! pure ) dup * ;",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(defined.status, ExecutionStatus::Completed);
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "7",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        let history = runtime.revision_history().unwrap();
        let checkpoint = history
            .last()
            .and_then(|snapshot| snapshot.checkpoint.clone())
            .expect("pure revision exposes a restorable VM checkpoint");
        let mut restored = TypedRuntime::from_checkpoint(checkpoint).unwrap();
        let result = restored.execute(ProgramLanguage::Forth, "restore.forth", "square", 1_000);

        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(restored.stack(), &[TypedValue::Int(49)]);
    }

    #[tokio::test]
    async fn program_runtime_restarts_from_a_typed_checkpoint() {
        let runtime = ProgramRuntime::new();
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ": square ( S int -- S int ! pure ) dup * ; 8",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let checkpoint = runtime
            .revision_history()
            .unwrap()
            .last()
            .and_then(|snapshot| snapshot.checkpoint.clone())
            .unwrap();

        let restored = ProgramRuntime::from_checkpoint(checkpoint).unwrap();
        let result = restored
            .submit(submission(
                ProgramLanguage::Lisp,
                "(square 8)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();

        assert_eq!(result.status, ExecutionStatus::Completed);
        assert_eq!(result.input_revision, 0);
        assert_eq!(result.output_revision, 1);
        assert_eq!(
            restored
                .inspect()
                .await
                .unwrap()
                .typed_stack
                .iter()
                .map(|cell| cell.value.clone())
                .collect::<Vec<_>>(),
            vec![TypedValue::Int(8), TypedValue::Int(64)]
        );
    }

    #[tokio::test]
    async fn program_runtime_archive_restores_history_but_not_authority() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./src/**").unwrap(),
            ))
            .unwrap();
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ": square ( S int -- S int ! pure ) dup * ; 8",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let archive = runtime.archive().unwrap();
        assert_eq!(archive.current_revision, 1);
        assert_eq!(archive.revisions.len(), 2);
        let encoded = serde_json::to_string(&archive).unwrap();
        let restored =
            ProgramRuntime::from_archive(serde_json::from_str(&encoded).unwrap()).unwrap();

        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
        assert!(!restored
            .inspect()
            .await
            .unwrap()
            .granted_capabilities
            .iter()
            .any(|requirement| requirement.capability == crate::vm::CapabilityKind::FileRead));
        let result = restored
            .submit(submission(
                ProgramLanguage::Lisp,
                "(square 8)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(result.input_revision, 1);
        assert_eq!(result.output_revision, 2);
        assert_eq!(restored.revision_history().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn authority_state_restores_scoped_grants_beside_the_vm_archive() {
        let runtime = ProgramRuntime::new();
        runtime
            .issue_typed_capability(
                crate::vm::CapabilityRequirement::file(
                    crate::vm::FileOperation::Read,
                    crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
                ),
                GrantScope::Session {
                    session_id: runtime.capability_session_id(),
                },
                "test-user",
                None,
            )
            .unwrap();
        let authority: ProgramRuntimeAuthorityState = serde_json::from_str(
            &serde_json::to_string(&runtime.authority_state().unwrap()).unwrap(),
        )
        .unwrap();
        let restored =
            ProgramRuntime::from_archive_with_authority(runtime.archive().unwrap(), authority)
                .unwrap();

        assert_eq!(
            restored.capability_session_id(),
            runtime.capability_session_id()
        );
        assert_eq!(
            restored.capability_project_id(),
            runtime.capability_project_id()
        );
        let read = restored
            .submit(submission(
                ProgramLanguage::Lisp,
                "(file-read (path \"Cargo.toml\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(read.status, ExecutionStatus::Completed);
    }

    #[test]
    fn authority_restore_rejects_active_grants_from_another_policy() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
            ))
            .unwrap();
        let mut state = runtime.authority_state().unwrap();
        state.ledger.grants.grants[0].policy_hash = "obsolete-policy".into();
        let mut restored = ProgramRuntime::new();
        assert!(restored.restore_authority_state(state).is_err());
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
    }

    #[test]
    fn authority_restore_rejects_legacy_raw_path_process_grants() {
        let runtime = ProgramRuntime::new();
        let policy = runtime.capability_policy().unwrap();
        let mut ledger = CapabilityLedger::default();
        ledger
            .issue(
                CapabilityRequirement {
                    capability: CapabilityKind::ProcessRun,
                    selector: ResourceSelector::Process {
                        executables: vec!["/usr/bin/true".into()],
                    },
                },
                GrantScope::Global,
                policy.policy_hash.clone(),
                "legacy-state",
                unix_time_ms(),
                None,
            )
            .unwrap();

        let error = runtime
            .restore_capability_ledger(ledger.clone())
            .expect_err("raw-path authority must not be restored");
        assert!(format!("{error:#}").contains("legacy process capability grant"));
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());

        let mut restored = ProgramRuntime::new();
        let error = restored
            .restore_authority_state(ProgramRuntimeAuthorityState {
                format_version: PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION,
                session_id: uuid::Uuid::new_v4(),
                project_id: "legacy-project".into(),
                policy,
                ledger,
                resource_roots: Vec::new(),
                resource_root_audit: Vec::new(),
            })
            .expect_err("raw-path authority state must require explicit reapproval");
        assert!(format!("{error:#}").contains("stable v3 invocation identity"));
        assert!(restored
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());
    }

    #[test]
    fn capability_availability_is_separate_from_grants_and_selector_aware() {
        let runtime = ProgramRuntime::new();
        let workspace_read = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector::parse("./Cargo.toml").unwrap(),
        );
        assert_eq!(
            runtime.capability_availability(&workspace_read),
            crate::vm::CapabilityAvailability::Available
        );
        assert!(runtime
            .capability_ledger()
            .unwrap()
            .grants
            .grants
            .is_empty());

        let root = tempfile::tempdir().unwrap();
        let host_read = crate::vm::CapabilityRequirement::file(
            crate::vm::FileOperation::Read,
            crate::vm::FileSelector {
                root: crate::vm::ResourceRoot::HostMachine,
                pattern: "**".into(),
            },
        );
        assert_eq!(
            runtime.capability_availability(&host_read),
            crate::vm::CapabilityAvailability::Disabled
        );
        runtime.bind_host_machine_root(root.path()).unwrap();
        assert_eq!(
            runtime.capability_availability(&host_read),
            crate::vm::CapabilityAvailability::Available
        );
        assert_eq!(
            runtime.capability_availability(&crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::VmWrite,
                selector: crate::vm::ResourceSelector::None,
            }),
            crate::vm::CapabilityAvailability::Unsupported
        );
        assert_eq!(
            runtime.capability_availability(&crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ScheduleCreate,
                selector: crate::vm::ResourceSelector::Schedule { policy: None },
            }),
            crate::vm::CapabilityAvailability::Disabled,
            "a bare runtime must not expose a second local schedule store"
        );
    }

    #[tokio::test]
    async fn public_runtime_preserves_one_typed_stack_across_lisp_and_forth_turns() {
        let runtime = ProgramRuntime::new();
        let lisp = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(+ 2 3)",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(lisp.status, ExecutionStatus::Completed);
        assert_eq!(lisp.output_revision, 1);

        let forth = runtime
            .submit(ProgramSubmission {
                expected_revision: Some(lisp.output_revision),
                ..submission(ProgramLanguage::Forth, "2 *", ExecutionEffect::Pure)
            })
            .await
            .unwrap();
        assert_eq!(forth.status, ExecutionStatus::Completed);
        assert_eq!(forth.output_revision, 2);

        let state = runtime.inspect().await.unwrap();
        assert_eq!(state.typed_stack.len(), 1);
        assert_eq!(state.typed_stack[0].value, TypedValue::Int(10));
    }

    #[tokio::test]
    async fn submission_source_id_is_preserved_in_effect_origins() {
        let runtime = ProgramRuntime::new();
        runtime
            .grant_typed_capability(CapabilityRequirement {
                capability: crate::vm::CapabilityKind::SessionEmit,
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        let outcome = runtime
            .submit(ProgramSubmission {
                source_id: Some("scripts/demo.lisp".into()),
                ..submission(
                    ProgramLanguage::Lisp,
                    "(say \"source aware\")",
                    ExecutionEffect::Pure,
                )
            })
            .await
            .unwrap();

        assert_eq!(outcome.status, ExecutionStatus::Completed);
        assert_eq!(outcome.vm_side_effects.len(), 1);
        assert_eq!(
            outcome.vm_side_effects[0]
                .origin
                .span
                .as_ref()
                .unwrap()
                .source_id,
            "scripts/demo.lisp"
        );
    }

    #[tokio::test]
    async fn rejects_stale_vm_revision() {
        let runtime = ProgramRuntime::new();
        runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "1",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        let mut request = submission(ProgramLanguage::Forth, "2 +", ExecutionEffect::VmWrite);
        request.expected_revision = Some(0);
        let error = runtime.submit(request).await.unwrap_err();
        assert!(error.to_string().contains("stale VM revision"));
    }

    #[tokio::test]
    async fn suspended_runs_keep_private_state_and_reject_a_losing_commit() {
        let runtime = ProgramRuntime::new();
        let suspended = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" before conflict\" say unit yield 1",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(suspended.status, ExecutionStatus::Suspended);
        assert!(runtime.inspect().await.unwrap().typed_stack.is_empty());

        let winner = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "2",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(winner.status, ExecutionStatus::Completed);
        assert_eq!(winner.output_revision, 1);

        let rejected = runtime
            .resume_typed_execution(suspended.execution_id)
            .await
            .expect("a continuation conflict is reported as a typed outcome");
        assert_eq!(rejected.status, ExecutionStatus::Failed);
        assert!(rejected
            .diagnostics
            .iter()
            .any(|message| message.contains("input revision 0; current revision is 1")));
        assert_eq!(rejected.output, "before conflict");
        assert!(!rejected.effect_journal.is_empty());
        let state = runtime.inspect().await.unwrap();
        assert_eq!(state.revision, 1);
        assert_eq!(
            state
                .typed_stack
                .iter()
                .map(|cell| &cell.value)
                .collect::<Vec<_>>(),
            vec![&TypedValue::Int(2)]
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_path_rejects_a_stable_symlink_escape_before_host_io() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = workspace.path().join("outside-link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let selector = crate::vm::FileSelector::parse("./**").unwrap();

        let root = resource_root_binding_record(
            crate::vm::ResourceRoot::Workspace,
            workspace.path(),
            1,
            false,
            1,
        )
        .unwrap();
        assert!(open_resource_beneath_mode(
            &root,
            &selector,
            "outside-link",
            SecureOpenMode::ReadFile,
        )
        .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_relative_open_rejects_final_component_symlink_swap() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().unwrap();
        let victim = workspace.path().join("victim");
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(&victim, b"inside").unwrap();
        std::fs::write(outside.path(), b"outside").unwrap();
        let binding = resource_root_binding_record(
            crate::vm::ResourceRoot::Workspace,
            workspace.path(),
            1,
            false,
            1,
        )
        .unwrap();
        let selector = crate::vm::FileSelector::parse("./**").unwrap();
        let outside_path = outside.path().to_path_buf();
        RESOURCE_BEFORE_FINAL_OPEN_HOOK
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push((
                "victim".into(),
                Box::new(move || {
                    std::fs::remove_file(&victim).unwrap();
                    symlink(outside_path, victim).unwrap();
                }),
            ));
        assert!(open_resource_beneath_mode(
            &binding,
            &selector,
            "victim",
            SecureOpenMode::ReadFile,
        )
        .is_err());
        assert_eq!(std::fs::read(outside.path()).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn descriptor_relative_open_keeps_the_opened_parent_after_path_replacement() {
        let workspace = tempfile::tempdir().unwrap();
        let directory = workspace.path().join("dir");
        let replacement = workspace.path().join("replacement");
        let displaced = workspace.path().join("displaced");
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(directory.join("value"), b"authorized").unwrap();
        std::fs::write(replacement.join("value"), b"replacement").unwrap();
        let binding = resource_root_binding_record(
            crate::vm::ResourceRoot::Workspace,
            workspace.path(),
            1,
            false,
            1,
        )
        .unwrap();
        let selector = crate::vm::FileSelector::parse("./**").unwrap();
        let replacement_target = workspace.path().join("dir");
        RESOURCE_BEFORE_FINAL_OPEN_HOOK
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push((
                "dir/value".into(),
                Box::new(move || {
                    std::fs::rename(directory, displaced).unwrap();
                    std::fs::rename(replacement, replacement_target).unwrap();
                }),
            ));
        let mut file =
            open_resource_beneath_mode(&binding, &selector, "dir/value", SecureOpenMode::ReadFile)
                .unwrap();
        let mut value = String::new();
        file.read_to_string(&mut value).unwrap();
        assert_eq!(value, "authorized");
    }

    #[test]
    fn generic_resource_resolution_does_not_assign_host_roots_to_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let selector = crate::vm::FileSelector::parse("${host-machine}/etc/**").unwrap();
        std::fs::create_dir(workspace.path().join("etc")).unwrap();
        std::fs::write(workspace.path().join("etc/hosts"), b"local").unwrap();

        // Root selection happens in `TypedHostHandler`; the generic canonical
        // check only proves that a child remains under the root selected by
        // the host binding.
        let root = resource_root_binding_record(
            crate::vm::ResourceRoot::HostMachine,
            workspace.path(),
            1,
            false,
            1,
        )
        .unwrap();
        let mut file =
            open_resource_beneath_mode(&root, &selector, "etc/hosts", SecureOpenMode::ReadFile)
                .unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert_eq!(text, "local");
    }

    #[tokio::test]
    async fn host_file_read_requires_an_explicit_host_binding_and_host_grant() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("note.txt"), b"host-only").unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_host_machine_root(root.path()).unwrap();

        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" note.txt\" host-path host-file-read",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let request = &pending.approval_prompts[0].request;
        assert!(matches!(
            request.arguments.as_slice(),
            [TypedValue::Path { selector, relative }]
                if selector.root == crate::vm::ResourceRoot::HostMachine && relative == "note.txt"
        ));
        let sequence = request.effect_sequence.unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(
            completed.values,
            vec![ProgramValue::Bytes(b"host-only".to_vec())]
        );
    }

    #[tokio::test]
    async fn project_and_task_output_roots_are_typed_independent_bindings() {
        let project = tempfile::tempdir().unwrap();
        let task_output = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("input.txt"), b"project-data").unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_project_root(project.path()).unwrap();
        runtime.bind_task_output_root(task_output.path()).unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("${project}/**").unwrap(),
            ))
            .unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Write,
                crate::vm::FileSelector::parse("${task.output}/**").unwrap(),
            ))
            .unwrap();

        let read = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(project-file-read (project-path \"input.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(read.status, ExecutionStatus::Completed);
        assert_eq!(
            read.values,
            vec![ProgramValue::Bytes(b"project-data".to_vec())]
        );

        let write = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" result.txt\" task-output-path s\" task-data\" bytes task-output-file-write",
                ExecutionEffect::WorkspaceWrite,
            ))
            .await
            .unwrap();
        assert_eq!(write.status, ExecutionStatus::Completed);
        assert_eq!(
            std::fs::read(task_output.path().join("result.txt")).unwrap(),
            b"task-data"
        );

        let crossed = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" input.txt\" project-path task-output-file-read",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(crossed.status, ExecutionStatus::Failed);
        assert!(crossed.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("project") && diagnostic.contains("task.output")
        }));
    }

    #[tokio::test]
    async fn host_file_read_fails_when_the_host_binding_is_not_installed() {
        let runtime = ProgramRuntime::new();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" note.txt\" host-path host-file-read",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Failed);
        assert!(completed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("host-machine root is not installed")));
    }

    #[tokio::test]
    async fn workspace_file_word_cannot_consume_a_host_path() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" note.txt\" host-path file-read",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert!(outcome.diagnostics.iter().any(|diagnostic| {
            diagnostic.contains("host-machine") && diagnostic.contains("workspace")
        }));
    }

    #[tokio::test]
    async fn host_file_write_uses_the_same_explicit_binding_and_grant_boundary() {
        let root = tempfile::tempdir().unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_host_machine_root(root.path()).unwrap();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Forth,
                "s\" created.txt\" host-path s\" host-write\" bytes host-file-write",
                ExecutionEffect::WorkspaceWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Write,
                crate::vm::FileSelector::parse("${host-machine}/**").unwrap(),
            ))
            .unwrap();
        let completed = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        assert_eq!(completed.status, ExecutionStatus::Completed);
        assert_eq!(completed.values, vec![ProgramValue::Nil]);
        assert_eq!(
            std::fs::read(root.path().join("created.txt")).unwrap(),
            b"host-write"
        );
    }

    #[tokio::test]
    async fn public_revocation_winning_before_host_use_prevents_file_mutation() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("note.txt"), b"before").unwrap();
        let runtime = Arc::new(ProgramRuntime::new());
        runtime.bind_host_machine_root(root.path()).unwrap();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(host-file-write (host-path \"note.txt\") (bytes \"after\"))",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let sequence = pending.approval_prompts[0].request.effect_sequence.unwrap();
        let grant_id = runtime
            .grant_typed_capability(pending.required_capabilities[0].clone())
            .unwrap();
        let (revoke_tx, revoke_rx) = std::sync::mpsc::channel();
        let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
        AUTHORIZATION_BEFORE_LEASE_HOOK
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push((
                pending.execution_id,
                CapabilityKind::FileWrite,
                Box::new(move || {
                    revoke_tx.send(()).unwrap();
                    assert!(revoked_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect(
                            "public revocation must finish before the shared use lease begins"
                        ));
                }),
            ));
        let revoker_runtime = Arc::clone(&runtime);
        let revoker = std::thread::spawn(move || {
            revoke_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("host use reached the before-lease boundary");
            let revoked = revoker_runtime.revoke_typed_capability(grant_id).unwrap();
            revoked_tx.send(revoked).unwrap();
        });
        let outcome = runtime
            .resume_typed_execution_for_effect(pending.execution_id, sequence)
            .await
            .unwrap();
        revoker.join().unwrap();
        assert_eq!(outcome.status, ExecutionStatus::Failed);
        assert_eq!(
            std::fs::read(root.path().join("note.txt")).unwrap(),
            b"before"
        );
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger.authorization_audit.is_empty());
        assert!(ledger
            .audit
            .iter()
            .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Revoked));
    }

    #[test]
    fn in_flight_deferred_use_blocks_public_revoke_and_root_mutation() {
        let project = tempfile::tempdir().unwrap();
        let runtime = Arc::new(ProgramRuntime::new());
        runtime.bind_project_root(project.path()).unwrap();
        let grant_id = runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("${project}/**").unwrap(),
            ))
            .unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let (outcome_tx, outcome_rx) = std::sync::mpsc::channel();
        let worker_runtime = Arc::clone(&runtime);
        let worker = std::thread::spawn(move || {
            let release_rx = Arc::clone(&release_rx);
            let sink: TypedEffectSink = Arc::new(move |_| {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .expect("test releases the in-flight deferred host use");
            });
            let tokio = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let outcome = tokio.block_on(worker_runtime.submit_with_deferred_host_effects(
                submission(
                    ProgramLanguage::Lisp,
                    "(project-file-read (project-path \"note.txt\"))",
                    ExecutionEffect::WorkspaceRead,
                ),
                sink,
            ));
            outcome_tx.send(outcome).unwrap();
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("deferred sink reached the externally visible use boundary");

        let (revoker_started_tx, revoker_started_rx) = std::sync::mpsc::channel();
        let (revoked_tx, revoked_rx) = std::sync::mpsc::channel();
        let revoker_runtime = Arc::clone(&runtime);
        let revoker = std::thread::spawn(move || {
            revoker_started_tx.send(()).unwrap();
            revoked_tx
                .send(revoker_runtime.revoke_typed_capability(grant_id))
                .unwrap();
        });
        let (clearer_started_tx, clearer_started_rx) = std::sync::mpsc::channel();
        let (cleared_tx, cleared_rx) = std::sync::mpsc::channel();
        let clearer_runtime = Arc::clone(&runtime);
        let clearer = std::thread::spawn(move || {
            clearer_started_tx.send(()).unwrap();
            cleared_tx
                .send(clearer_runtime.clear_project_root())
                .unwrap();
        });
        revoker_started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("public revocation thread starts while the host use is in flight");
        clearer_started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("public root mutation thread starts while the host use is in flight");
        assert!(
            revoked_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "public revocation must wait for the in-flight shared use lease"
        );
        assert!(
            cleared_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "public root mutation must wait for the in-flight shared use lease"
        );

        release_tx.send(()).unwrap();
        let pending = outcome_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("deferred dispatch completes after its sink returns")
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::Suspended);
        assert_eq!(
            revoked_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("public revocation completes after the shared use lease")
                .unwrap(),
            true
        );
        cleared_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("public root mutation completes after the shared use lease")
            .unwrap();
        worker.join().unwrap();
        revoker.join().unwrap();
        clearer.join().unwrap();
        assert!(!runtime
            .authority_state()
            .unwrap()
            .resource_roots
            .iter()
            .any(|binding| binding.root == crate::vm::ResourceRoot::Project));
    }

    #[tokio::test]
    async fn failed_file_open_rolls_back_once_use_and_restart_state() {
        let root = tempfile::tempdir().unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_host_machine_root(root.path()).unwrap();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(host-file-read (host-path \"missing.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        let failed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        let ledger = runtime.capability_ledger().unwrap();
        let grant = ledger.grants.grants.last().expect("once grant remains");
        assert!(matches!(grant.scope, GrantScope::Once { .. }));
        assert!(grant.consumed_at_unix_ms.is_none());
        assert!(ledger.authorization_audit.is_empty());
        assert!(!ledger
            .audit
            .iter()
            .any(|entry| entry.action == crate::vm::CapabilityAuditAction::Consumed));

        let encoded = serde_json::to_vec(&runtime.authority_state().unwrap()).unwrap();
        let state: ProgramRuntimeAuthorityState = serde_json::from_slice(&encoded).unwrap();
        let mut restored = ProgramRuntime::new();
        restored.restore_authority_state(state).unwrap();
        let restored = restored.capability_ledger().unwrap();
        assert!(restored
            .grants
            .grants
            .last()
            .unwrap()
            .consumed_at_unix_ms
            .is_none());
        assert!(restored.authorization_audit.is_empty());
    }

    #[tokio::test]
    async fn rollback_sink_failure_retains_the_last_durable_authorization_state() {
        use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

        let root = tempfile::tempdir().unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_host_machine_root(root.path()).unwrap();
        let saw_authorized = Arc::new(AtomicBool::new(false));
        let durable = Arc::new(Mutex::new(None::<ProgramRuntimeAuthorityState>));
        let sink_saw_authorized = Arc::clone(&saw_authorized);
        let sink_durable = Arc::clone(&durable);
        runtime
            .set_authority_sink(Arc::new(move |state| {
                if !state.ledger.authorization_audit.is_empty() {
                    sink_saw_authorized.store(true, AtomicOrdering::SeqCst);
                } else if sink_saw_authorized.load(AtomicOrdering::SeqCst)
                    && state.ledger.grants.grants.iter().any(|grant| {
                        matches!(grant.scope, GrantScope::Once { .. })
                            && grant.consumed_at_unix_ms.is_none()
                    })
                {
                    anyhow::bail!("injected rollback persistence failure");
                }
                *sink_durable.lock().unwrap() = Some(state);
                Ok(())
            }))
            .unwrap();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(host-file-read (host-path \"missing.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let failed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        assert!(failed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("persist unused host authorization rollback")));
        let ledger = runtime.capability_ledger().unwrap();
        assert_eq!(ledger.authorization_audit.len(), 1);
        assert!(ledger
            .grants
            .grants
            .last()
            .unwrap()
            .consumed_at_unix_ms
            .is_some());
        assert_eq!(
            durable.lock().unwrap().as_ref().unwrap().ledger,
            ledger,
            "memory must remain at the last successfully persisted authority state"
        );
    }

    #[tokio::test]
    async fn unused_authorization_rollback_preserves_an_unrelated_concurrent_grant() {
        let root = tempfile::tempdir().unwrap();
        let runtime = ProgramRuntime::new();
        runtime.bind_host_machine_root(root.path()).unwrap();
        let pending = runtime
            .submit_typed_only(submission(
                ProgramLanguage::Lisp,
                "(host-file-read (host-path \"missing.txt\"))",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap();
        let ledger = Arc::clone(&runtime.capability_ledger);
        let policy_hash = runtime.capability_policy().unwrap().policy_hash;
        AUTHORIZATION_BEFORE_USE_HOOK
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .push((
                pending.execution_id,
                CapabilityKind::FileRead,
                Box::new(move || {
                    ledger
                        .lock()
                        .unwrap()
                        .issue(
                            CapabilityRequirement {
                                capability: CapabilityKind::MemoryRead,
                                selector: ResourceSelector::Memory {
                                    tree: "session".into(),
                                    path: "concurrent".into(),
                                },
                            },
                            GrantScope::Global,
                            policy_hash,
                            "concurrent-user",
                            unix_time_ms(),
                            None,
                        )
                        .unwrap();
                }),
            ));
        let failed = runtime
            .resolve_typed_approval(
                &pending.approval_prompts[0],
                ApprovalChoice::AllowOnce,
                "test-user",
            )
            .await
            .unwrap();
        assert_eq!(failed.status, ExecutionStatus::Failed);
        let ledger = runtime.capability_ledger().unwrap();
        assert!(ledger.grants.grants.iter().any(|grant| {
            grant.requirement.capability == CapabilityKind::MemoryRead
                && grant.created_by == "concurrent-user"
                && grant.is_active(unix_time_ms())
        }));
        assert!(ledger.authorization_audit.is_empty());
    }

    #[test]
    fn resource_root_bind_revoke_and_whole_machine_lifecycle_is_durable() {
        let runtime = ProgramRuntime::new();
        let persisted = Arc::new(Mutex::new(Vec::new()));
        let persisted_sink = Arc::clone(&persisted);
        runtime
            .set_authority_sink(Arc::new(move |state| {
                persisted_sink.lock().unwrap().push(state);
                Ok(())
            }))
            .unwrap();
        let project = tempfile::tempdir().unwrap();
        runtime.bind_project_root(project.path()).unwrap();
        runtime.clear_project_root().unwrap();
        assert!(runtime.bind_host_machine_root(PathBuf::from("/")).is_err());
        runtime.bind_whole_machine_root().unwrap();
        runtime.clear_host_machine_root().unwrap();

        let states = persisted.lock().unwrap();
        assert_eq!(states.len(), 4);
        assert!(states
            .iter()
            .all(|state| state.format_version == PROGRAM_RUNTIME_AUTHORITY_STATE_VERSION));
        let final_state = states.last().unwrap();
        assert!(final_state
            .resource_root_audit
            .iter()
            .any(|entry| entry.root == crate::vm::ResourceRoot::Project
                && entry.action == ResourceRootAuditAction::Bound));
        assert!(final_state
            .resource_root_audit
            .iter()
            .any(|entry| entry.root == crate::vm::ResourceRoot::Project
                && entry.action == ResourceRootAuditAction::Revoked));
        assert!(final_state
            .resource_root_audit
            .iter()
            .any(|entry| entry.whole_machine && entry.action == ResourceRootAuditAction::Bound));
        assert!(!final_state
            .resource_roots
            .iter()
            .any(|binding| binding.root == crate::vm::ResourceRoot::HostMachine));
    }

    #[test]
    fn failed_authority_sink_rolls_back_resource_root_bind_and_revoke() {
        let runtime = ProgramRuntime::new();
        let project = tempfile::tempdir().unwrap();
        let initial = runtime.authority_state().unwrap();
        runtime
            .set_authority_sink(Arc::new(|_| anyhow::bail!("injected root sink failure")))
            .unwrap();
        assert!(runtime.bind_project_root(project.path()).is_err());
        assert_eq!(runtime.authority_state().unwrap(), initial);

        runtime.clear_authority_sink().unwrap();
        runtime.bind_project_root(project.path()).unwrap();
        let bound = runtime.authority_state().unwrap();
        runtime
            .set_authority_sink(Arc::new(|_| anyhow::bail!("injected root sink failure")))
            .unwrap();
        assert!(runtime.clear_project_root().is_err());
        assert_eq!(runtime.authority_state().unwrap(), bound);
    }

    #[test]
    fn authority_restart_rejects_replaced_resource_root_identity() {
        let runtime = ProgramRuntime::new();
        let parent = tempfile::tempdir().unwrap();
        let project = parent.path().join("project");
        let displaced = parent.path().join("displaced");
        std::fs::create_dir(&project).unwrap();
        runtime.bind_project_root(&project).unwrap();
        let state = runtime.authority_state().unwrap();
        std::fs::rename(&project, &displaced).unwrap();
        std::fs::create_dir(&project).unwrap();

        let mut restored = ProgramRuntime::new();
        let error = restored
            .restore_authority_state(state)
            .expect_err("replacement directory must not inherit root authority");
        assert!(format!("{error:#}").contains("identity changed since approval"));
        assert_eq!(
            restored.capability_availability(&CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("${project}/**").unwrap(),
            )),
            CapabilityAvailability::Disabled
        );
    }

    #[test]
    fn authority_restart_rejects_incomplete_resource_root_lifecycle_audit() {
        let runtime = ProgramRuntime::new();
        let project = tempfile::tempdir().unwrap();
        runtime.bind_project_root(project.path()).unwrap();
        runtime.clear_project_root().unwrap();
        let mut state = runtime.authority_state().unwrap();
        let removed = state.resource_root_audit.pop().unwrap();
        assert_eq!(removed.action, ResourceRootAuditAction::Revoked);

        let mut restored = ProgramRuntime::new();
        let error = restored
            .restore_authority_state(state)
            .expect_err("audit replay must detect an omitted revocation");
        assert!(format!("{error:#}").contains("replayed audit state"));
    }
}
