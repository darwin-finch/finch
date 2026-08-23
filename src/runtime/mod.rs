//! Provider-neutral execution service for Finch's Forth and Lisp VMs.

pub mod agent_vm;
pub mod automation;
pub mod context;
pub mod fiber;
pub mod outcome;
pub mod scheduler;

use crate::programs::{ExecutionEffect, ProgramLanguage, ProgramValue};
use crate::scheduling::{ScheduledTask, TaskQueue, TaskScheduler, TaskStatus};
use crate::vm::{
    ApprovalPrompt, CapabilityRequest, CapabilityRequirement, EffectSet, SourceOrigin, Type,
    TypedExecutionStatus, TypedRuntime, TypedSuspension, TypedValue, VmDiagnostic, VmSideEffect,
};
use anyhow::{bail, Result};
use automation::AutomationBroker;
use automation::AutomationRequest;
use context::{ExecutionBudget, ExecutionContext};
use outcome::{ExecutionBackend, ExecutionOutcome, ExecutionStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, RwLock, Weak};
use std::time::Instant;

/// A portable VM event attached to its owning ProgramRun. The VM event itself
/// remains embedder-neutral; the envelope provides the other half of its
/// idempotency key to a host/UI callback.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Host-specific projection of one portable typed VM event. The projection is
/// bound to one ProgramRun, never stored as a process- or Brain-global
/// "active work unit".
pub type TypedEffectSink = Arc<dyn Fn(VmEffectEnvelope) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgramSubmission {
    pub language: ProgramLanguage,
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

/// One session's persistent language runtimes.
pub struct ProgramRuntime {
    typed: Arc<Mutex<TypedRuntime>>,
    revision: Arc<AtomicU64>,
    manifest_generation: AtomicU64,
    submission_gate: tokio::sync::Mutex<()>,
    automation: Arc<AutomationBroker>,
    workspace_root: Arc<PathBuf>,
    /// Optional host-wide resource root. Installing this binding makes
    /// `path<host-machine>` calls available to the host adapter, but conveys
    /// no authority by itself: typed `file.read`/`file.write` grants are
    /// still checked for every call.
    host_machine_root: Arc<RwLock<Option<Arc<PathBuf>>>>,
    memory: RwLock<Option<Arc<crate::memory::MemorySystem>>>,
    network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
    /// Output handles are opaque, per-execution presentation resources.  They
    /// intentionally do not name an ambient "current work unit".
    output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
    /// Opaque, per-ProgramRun streams. The host retains their backing state
    /// and source code never gets a raw path or producer capability back after
    /// opening one. New stream backends belong in `HostStreamBackend`, not in
    /// a parallel registry with a second ownership lifecycle.
    streams: Arc<Mutex<HashMap<String, HostStream>>>,
    output_sink: RwLock<Option<Arc<dyn Fn(String) + Send + Sync>>>,
    schedule_queue: RwLock<Option<Arc<TaskQueue>>>,
    agent_scheduler: RwLock<Weak<scheduler::AgentScheduler>>,
    /// Daemon-owned typed continuations keyed by the execution id visible in
    /// the UI. Approval and resumption use this exact verified program state.
    pending_typed: Mutex<HashMap<uuid::Uuid, PendingTypedExecution>>,
}

/// Host-owned socket metadata. Finch source sees only the opaque resource
/// handle; the host retains the endpoint so each later send can revalidate
/// the grant which originally authorized that connection.
struct NetworkSocket {
    stream: TcpStream,
    host: String,
    port: u16,
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
    backend: HostStreamBackend,
}

/// Private implementations of host-issued `stream<T>` values.  The public
/// type and capability contract live in the typed VM; this enum is merely the
/// host adapter's resource table.  A future workbook or producer stream gets
/// the exact same ownership, close, and ProgramRun-release behavior.
enum HostStreamBackend {
    FileLines(BufReader<std::fs::File>),
    CsvRecords(BufReader<std::fs::File>),
}

#[derive(Clone)]
struct PendingTypedExecution {
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
    defer_program_invocations: bool,
}

/// UI-safe metadata for a daemon-owned typed continuation. The full frame is
/// deliberately not exposed through ordinary client state inspection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTypedExecutionInfo {
    pub execution_id: uuid::Uuid,
    pub input_revision: u64,
    pub manifest_generation: u64,
    /// Required by the sequence-checked resume API when the run is awaiting
    /// a concrete host capability result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_effect_sequence: Option<u64>,
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

    pub fn with_automation(enabled: bool) -> Self {
        let automation = Arc::new(AutomationBroker::new(enabled));
        Self {
            typed: Arc::new(Mutex::new(TypedRuntime::new())),
            revision: Arc::new(AtomicU64::new(0)),
            manifest_generation: AtomicU64::new(1),
            submission_gate: tokio::sync::Mutex::new(()),
            automation,
            workspace_root: Arc::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            host_machine_root: Arc::new(RwLock::new(None)),
            memory: RwLock::new(None),
            network: Arc::new(Mutex::new(HashMap::new())),
            output_handles: Arc::new(Mutex::new(HashMap::new())),
            streams: Arc::new(Mutex::new(HashMap::new())),
            output_sink: RwLock::new(None),
            schedule_queue: RwLock::new(None),
            agent_scheduler: RwLock::new(Weak::new()),
            pending_typed: Mutex::new(HashMap::new()),
        }
    }

    pub fn automation(&self) -> Arc<AutomationBroker> {
        Arc::clone(&self.automation)
    }

    /// Install the host-owned root behind `root<host-machine>`. This is an
    /// availability binding, deliberately separate from capability grants;
    /// callers must still grant a matching `file.read` or `file.write`
    /// selector before a typed program can use it. Bind `/` only when the
    /// user intentionally wants whole-machine scope.
    pub fn bind_host_machine_root(&self, root: impl Into<PathBuf>) -> Result<()> {
        let root = root.into().canonicalize()?;
        if !root.is_dir() {
            bail!("host-machine root is not a directory: {}", root.display());
        }
        *self
            .host_machine_root
            .write()
            .map_err(|_| anyhow::anyhow!("host-machine root binding lock poisoned"))? =
            Some(Arc::new(root));
        Ok(())
    }

    /// Remove the host binding. Pending executions recheck this at their next
    /// host call, so revocation takes effect without widening workspace paths.
    pub fn clear_host_machine_root(&self) -> Result<()> {
        *self
            .host_machine_root
            .write()
            .map_err(|_| anyhow::anyhow!("host-machine root binding lock poisoned"))? = None;
        Ok(())
    }

    /// Attach the host's MemTree service to the typed capability boundary.
    /// Keeping this explicit prevents a VM from accidentally acquiring a
    /// second memory database or an ambient memory authority.
    pub fn attach_memory(&self, memory: Arc<crate::memory::MemorySystem>) {
        *self.memory.write().expect("memory binding lock poisoned") = Some(memory);
    }

    /// Install a live output sink for typed `say` chunks. The ordinary
    /// submission result still contains the complete buffered output.
    pub fn set_typed_output_sink(&self, sink: Option<Arc<dyn Fn(String) + Send + Sync>>) {
        *self.output_sink.write().expect("output sink lock poisoned") = sink;
    }

    pub fn attach_schedule_queue(&self, queue: Arc<TaskQueue>) {
        *self
            .schedule_queue
            .write()
            .expect("schedule queue lock poisoned") = Some(queue);
    }

    /// Construct the durable scheduler with this runtime's typed callback
    /// executor. Keeping construction here prevents callers from accidentally
    /// falling back to an unbound scheduler that cannot execute callbacks.
    pub fn task_scheduler(self: &Arc<Self>) -> Option<TaskScheduler> {
        let queue = self
            .schedule_queue
            .read()
            .expect("schedule queue lock poisoned")
            .clone()?;
        Some(TaskScheduler::with_executor(
            queue,
            Arc::clone(self).scheduled_executor(),
        ))
    }

    /// Build the executor used by the durable scheduler. Each callback gets a
    /// fresh submission context and never inherits an approval decision from
    /// the scheduling call.
    pub fn scheduled_executor(
        self: Arc<Self>,
    ) -> Arc<
        dyn Fn(ScheduledTask) -> futures::future::BoxFuture<'static, Result<String>> + Send + Sync,
    > {
        Arc::new(move |task| {
            let runtime = Arc::clone(&self);
            Box::pin(async move {
                let language = ProgramLanguage::infer_source(&task.task);
                let outcome = runtime
                    .submit_typed_only(ProgramSubmission {
                        language,
                        source: task.task,
                        intent: "scheduled callback".into(),
                        effect: ExecutionEffect::VmRead,
                        declared_capabilities: Vec::new(),
                        manifest_generation: runtime.manifest_generation(),
                        expected_revision: None,
                        budget: None,
                    })
                    .await?;
                if !matches!(outcome.status, ExecutionStatus::Completed) {
                    anyhow::bail!("scheduled callback did not complete: {:?}", outcome.status);
                }
                Ok(outcome.output)
            })
        })
    }

    /// Grant a typed capability after an approval decision. A saved typed
    /// execution rechecks this structured grant when it is resumed.
    pub fn grant_typed_capability(&self, requirement: CapabilityRequirement) -> Result<()> {
        self.typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
            .grant(requirement);
        Ok(())
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
        Ok(pending.get(&execution_id).map(|pending| {
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
                reason,
            }
        }))
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
        let mut diagnostics = vec!["typed VM execution cancelled before completion".into()];
        if let Some(error) = cpu_cancel_error {
            diagnostics.push(format!(
                "CPU worker cancellation was not acknowledged: {error}"
            ));
        }
        Ok(Some(ExecutionOutcome {
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
            required_capabilities: Vec::new(),
            approval_prompts: Vec::new(),
            input_revision: pending.input_revision,
            output_revision: pending.input_revision,
            effect: pending.effect,
            backend: ExecutionBackend::TypedVm,
            elapsed_ms: 0,
        }))
    }

    /// Compatibility boolean form of [`Self::cancel_typed_execution_with_outcome`].
    pub fn cancel_typed_execution(&self, execution_id: uuid::Uuid) -> Result<bool> {
        Ok(self
            .cancel_typed_execution_with_outcome(execution_id)?
            .is_some())
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
        self.release_output_handles(execution_id)?;

        let mut effect_journal = pending.suspension.effect_journal.clone();
        if let Some(entry) = effect_journal.last_mut() {
            entry.state = crate::vm::EffectJournalState::Denied;
        }
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
            required_capabilities: Vec::new(),
            approval_prompts: Vec::new(),
            input_revision: pending.input_revision,
            output_revision: pending.input_revision,
            effect: pending.effect,
            backend: ExecutionBackend::TypedVm,
            elapsed_ms: 0,
        })
    }

    /// Resume a typed execution that previously yielded or awaited approval.
    /// The execution id is stable across the pause; source is never submitted
    /// again. A revision mismatch deliberately invalidates the saved frame,
    /// because applying it to a different Brain state would be unsound.
    pub async fn resume_typed_execution(
        &self,
        execution_id: uuid::Uuid,
    ) -> Result<ExecutionOutcome> {
        self.resume_typed_execution_inner(execution_id, None, None)
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
        self.resume_typed_execution_inner(execution_id, Some(effect_sequence), None)
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
        self.resume_typed_execution_inner(execution_id, Some(effect_sequence), Some(values))
            .await
    }

    async fn resume_typed_execution_inner(
        &self,
        execution_id: uuid::Uuid,
        expected_effect_sequence: Option<u64>,
        external_effect_result: Option<Vec<TypedValue>>,
    ) -> Result<ExecutionOutcome> {
        let _submission = self.submission_gate.lock().await;
        // Keep the standard mutex guard in this lexical block. A resumed VM
        // may run on a Tokio worker, so no non-Send guard may cross the host
        // resume await below.
        let pending = {
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
        if pending.context.manifest_generation != self.manifest_generation() {
            self.release_output_handles(execution_id)?;
            bail!("resumable typed execution has a stale VM manifest generation");
        }
        if pending.input_revision != self.revision() {
            self.release_output_handles(execution_id)?;
            bail!(
                "resumable typed execution has input revision {}; current revision is {}",
                pending.input_revision,
                self.revision()
            );
        }
        let started = Instant::now();
        let external_effect_result = match external_effect_result {
            Some(values) => Some((
                expected_effect_sequence.expect("external result requires an effect sequence"),
                values,
            )),
            None => None,
        };
        let execution = self
            .resume_typed_program(&pending, external_effect_result)
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

        let suspension = execution.suspension.clone();
        if let Some(suspension) = suspension.clone() {
            self.pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?
                .insert(
                    execution_id,
                    PendingTypedExecution {
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
                        defer_program_invocations: pending.defer_program_invocations,
                    },
                );
        }
        if suspension.is_none() {
            self.release_output_handles(execution_id)?;
        }

        Ok(match execution.status {
            TypedExecutionStatus::Completed => {
                let output_revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
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
                ),
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

    pub async fn inspect(&self) -> Result<VmStateSnapshot> {
        let typed = Arc::clone(&self.typed);
        let revision = Arc::clone(&self.revision);
        let manifest_generation = self.manifest_generation();
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
                    documentation: typed
                        .functions()
                        .get(name)
                        .and_then(|function| function.documentation.clone()),
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
        self.submit_as_with_optional_typed_effect_sink(submission, None, None, false)
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
        self.submit_as_with_optional_typed_effect_sink(submission, caller, None, false)
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
        self.submit_as_with_optional_typed_effect_sink(submission, caller, Some(effect_sink), false)
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
        self.submit_as_with_optional_typed_effect_sink(submission, None, Some(effect_sink), true)
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
        defer_program_invocations: bool,
    ) -> Result<ExecutionOutcome> {
        // This is a per-session state transaction, not a process-wide
        // interpreter lock. Independent runtimes and child model loops remain
        // concurrent while revision checks and mutations of this VM are atomic.
        let _submission = self.submission_gate.lock().await;
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
        let context = ExecutionContext::new(generation, submission.budget.unwrap_or_default());
        let started = Instant::now();
        let execution = self
            .execute_typed_program(
                submission.language,
                &submission.source,
                &context,
                &submission.declared_capabilities,
                caller.clone(),
                effect_sink.clone(),
                defer_program_invocations,
            )
            .await?;
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let suspension = execution.suspension.clone();
        if let Some(suspension) = suspension.clone() {
            self.pending_typed
                .lock()
                .map_err(|_| anyhow::anyhow!("pending typed execution lock poisoned"))?
                .insert(
                    context.execution_id,
                    PendingTypedExecution {
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
                        defer_program_invocations,
                    },
                );
        }
        if suspension.is_none() {
            self.release_output_handles(context.execution_id)?;
        }
        Ok(match execution.status {
            TypedExecutionStatus::Completed => {
                let output_revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
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
                ),
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
        language: ProgramLanguage,
        source: &str,
        context: &ExecutionContext,
        declared_capabilities: &[CapabilityRequirement],
        caller: Option<scheduler::AgentIdentity>,
        typed_effect_sink: Option<TypedEffectSink>,
        defer_program_invocations: bool,
    ) -> Result<crate::vm::TypedExecution> {
        let runtime = Arc::clone(&self.typed);
        let automation = Arc::clone(&self.automation);
        let workspace_root = Arc::clone(&self.workspace_root);
        let host_machine_root = Arc::clone(&self.host_machine_root);
        let memory = self
            .memory
            .read()
            .expect("memory binding lock poisoned")
            .clone();
        let network = Arc::clone(&self.network);
        let output_handles = Arc::clone(&self.output_handles);
        let streams = Arc::clone(&self.streams);
        let output_sink = self
            .output_sink
            .read()
            .expect("output sink lock poisoned")
            .clone();
        let schedule_queue = self
            .schedule_queue
            .read()
            .expect("schedule queue lock poisoned")
            .clone();
        let scheduler = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, caller));
        let source = source.to_string();
        let declared = (!declared_capabilities.is_empty())
            .then(|| EffectSet(declared_capabilities.iter().cloned().collect()));
        let fuel = context.budget.forth_fuel.min(u64::MAX as usize) as u64;
        let execution_id = context.execution_id;
        let execution = tokio::task::spawn_blocking(move || {
            runtime
                .lock()
                .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))
                .map(|mut runtime| {
                    let vocabulary = serde_json::to_string(runtime.vocabulary())
                        .unwrap_or_else(|_| "[]".to_string());
                    // A host binding being installed is availability, not
                    // authority.  The runtime's existing grants are the only
                    // source of authority for automation and child agents.
                    let grants = runtime.grants().clone();
                    let mut handler = TypedHostHandler::new(
                        Arc::clone(&automation),
                        Arc::clone(&workspace_root),
                        Arc::clone(&host_machine_root),
                        scheduler,
                        memory,
                        vocabulary,
                        network,
                        output_handles,
                        streams,
                        execution_id,
                        grants,
                        output_sink,
                        typed_effect_sink,
                        schedule_queue,
                        defer_program_invocations,
                    );
                    runtime.execute_with_handler(
                        language,
                        match language {
                            ProgramLanguage::Forth => "provider-response.forth",
                            ProgramLanguage::Lisp => "provider-response.lisp",
                        },
                        &source,
                        fuel,
                        declared.as_ref(),
                        &mut handler,
                    )
                })
        })
        .await??;
        Ok(execution)
    }

    async fn resume_typed_program(
        &self,
        pending: &PendingTypedExecution,
        external_effect_result: Option<(u64, Vec<TypedValue>)>,
    ) -> Result<crate::vm::TypedExecution> {
        let runtime = Arc::clone(&self.typed);
        let automation = Arc::clone(&self.automation);
        let workspace_root = Arc::clone(&self.workspace_root);
        let host_machine_root = Arc::clone(&self.host_machine_root);
        let memory = self
            .memory
            .read()
            .expect("memory binding lock poisoned")
            .clone();
        let network = Arc::clone(&self.network);
        let output_handles = Arc::clone(&self.output_handles);
        let streams = Arc::clone(&self.streams);
        let output_sink = self
            .output_sink
            .read()
            .expect("output sink lock poisoned")
            .clone();
        let typed_effect_sink = pending.effect_sink.clone();
        let defer_program_invocations = pending.defer_program_invocations;
        let schedule_queue = self
            .schedule_queue
            .read()
            .expect("schedule queue lock poisoned")
            .clone();
        let scheduler = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, pending.caller.clone()));
        let suspension = pending.suspension.clone();
        let execution_id = pending.context.execution_id;
        tokio::task::spawn_blocking(move || {
            runtime
                .lock()
                .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))
                .map(|mut runtime| {
                    let vocabulary = serde_json::to_string(runtime.vocabulary())
                        .unwrap_or_else(|_| "[]".to_string());
                    // Resumption has the same authority boundary as initial
                    // execution: bindings make effects possible, never
                    // implicitly granted.
                    let grants = runtime.grants().clone();
                    let mut handler = TypedHostHandler::new(
                        Arc::clone(&automation),
                        Arc::clone(&workspace_root),
                        Arc::clone(&host_machine_root),
                        scheduler,
                        memory,
                        vocabulary,
                        network,
                        output_handles,
                        streams,
                        execution_id,
                        grants,
                        output_sink,
                        typed_effect_sink,
                        schedule_queue,
                        defer_program_invocations,
                    );
                    match external_effect_result {
                        Some((effect_sequence, values)) => runtime.resume_with_effect_result(
                            suspension,
                            effect_sequence,
                            values,
                            &mut handler,
                        ),
                        None => runtime.resume_with_handler(suspension, Vec::new(), &mut handler),
                    }
                })
        })
        .await?
    }
}

struct TypedHostHandler {
    automation: Arc<AutomationBroker>,
    workspace_root: Arc<PathBuf>,
    host_machine_root: Arc<RwLock<Option<Arc<PathBuf>>>>,
    output: String,
    output_chunks: Vec<String>,
    side_effects: Vec<crate::vm::interpreter::HostSideEffect>,
    scheduler: Option<agent_vm::AgentVmBinding>,
    memory: Option<Arc<crate::memory::MemorySystem>>,
    vocabulary: String,
    network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
    output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
    streams: Arc<Mutex<HashMap<String, HostStream>>>,
    execution_id: uuid::Uuid,
    network_grants: EffectSet,
    output_sink: Option<Arc<dyn Fn(String) + Send + Sync>>,
    typed_effect_sink: Option<TypedEffectSink>,
    schedule_queue: Option<Arc<TaskQueue>>,
    defer_program_invocations: bool,
}

impl TypedHostHandler {
    fn new(
        automation: Arc<AutomationBroker>,
        workspace_root: Arc<PathBuf>,
        host_machine_root: Arc<RwLock<Option<Arc<PathBuf>>>>,
        scheduler: Option<agent_vm::AgentVmBinding>,
        memory: Option<Arc<crate::memory::MemorySystem>>,
        vocabulary: String,
        network: Arc<Mutex<HashMap<String, NetworkSocket>>>,
        output_handles: Arc<Mutex<HashMap<String, OutputHandleRecord>>>,
        streams: Arc<Mutex<HashMap<String, HostStream>>>,
        execution_id: uuid::Uuid,
        network_grants: EffectSet,
        output_sink: Option<Arc<dyn Fn(String) + Send + Sync>>,
        typed_effect_sink: Option<TypedEffectSink>,
        schedule_queue: Option<Arc<TaskQueue>>,
        defer_program_invocations: bool,
    ) -> Self {
        Self {
            automation,
            workspace_root,
            host_machine_root,
            output: String::new(),
            output_chunks: Vec::new(),
            side_effects: Vec::new(),
            scheduler,
            memory,
            vocabulary,
            network,
            output_handles,
            streams,
            execution_id,
            network_grants,
            output_sink,
            typed_effect_sink,
            schedule_queue,
            defer_program_invocations,
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
                let line = read_bounded_utf8_line(reader)
                    .map_err(|message| host_binding_error(origin, message))?;
                Ok(vec![TypedValue::Option {
                    inner_type: Type::String,
                    value: line.map(|line| Box::new(TypedValue::String(line))),
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
            (HostStreamBackend::CsvRecords(_), "csv") | (HostStreamBackend::FileLines(_), "lines")
        );
        if !backend_matches {
            return Err(host_binding_error(origin, "stream backend is malformed"));
        }
        streams.remove(id);
        Ok(vec![TypedValue::Unit])
    }
}

impl crate::vm::interpreter::CapabilityHandler for TypedHostHandler {
    fn observe_awaited_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<(), VmDiagnostic> {
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
        self.defer_program_invocations
            && effect.requirement.capability == crate::vm::CapabilityKind::ProgramInvoke
    }

    fn request_effect(
        &mut self,
        effect: &VmSideEffect,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let values = match &effect.event {
            crate::vm::interpreter::HostSideEffect::Request { arguments } => {
                self.request(&effect.requirement, arguments.clone(), &effect.origin)?
            }
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

    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &crate::vm::SourceOrigin,
    ) -> std::result::Result<Vec<TypedValue>, VmDiagnostic> {
        let request = match requirement.capability {
            crate::vm::CapabilityKind::SessionEmit => {
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-HOST-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "session.emit requires one string",
                        Some(origin.clone()),
                    ));
                };
                if origin.word.as_deref() == Some("output-open") {
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
                self.output.push_str(text);
                self.output_chunks.push(text.clone());
                self.emit(text);
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::VmRead => {
                if origin.word.as_deref() == Some("vm-vocabulary") {
                    return Ok(vec![TypedValue::String(self.vocabulary.clone())]);
                }
                return Err(host_binding_error(
                    origin,
                    "unknown VM inspection operation",
                ));
            }
            crate::vm::CapabilityKind::AutomationInspect => match origin.word.as_deref() {
                Some("automation-displays") => AutomationRequest::Displays,
                Some("automation-windows") => AutomationRequest::Windows,
                _ => AutomationRequest::Availability,
            },
            crate::vm::CapabilityKind::AutomationWrite => {
                if arguments.len() == 4 {
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
                } else {
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
            }
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
                let path = match arguments.first() {
                    Some(TypedValue::Path { relative, selector }) => self
                        .secure_file_path(selector, relative)
                        .map_err(|message| host_binding_error(origin, message))?,
                    _ => {
                        return Err(host_binding_error(
                            origin,
                            "file read operations require a refined path as their first argument",
                        ));
                    }
                };
                match origin.word.as_deref() {
                    Some("csv-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(origin, "csv-open requires one path"));
                        }
                        let file = std::fs::File::open(path)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
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
                    Some("file-lines-open") => {
                        if arguments.len() != 1 {
                            return Err(host_binding_error(
                                origin,
                                "file-lines-open requires one path",
                            ));
                        }
                        let file = std::fs::File::open(path)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
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
                        let size = std::fs::metadata(path)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?
                            .len();
                        let size = i64::try_from(size).map_err(|_| {
                            host_binding_error(origin, "file is too large to represent")
                        })?;
                        return Ok(vec![TypedValue::Int(size)]);
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
                        let mut file = std::fs::File::open(path)
                            .map_err(|error| host_binding_error(origin, error.to_string()))?;
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
                        let bytes = std::fs::read(path)
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
                let path = self
                    .secure_file_path(selector, relative)
                    .map_err(|message| host_binding_error(origin, message))?;
                std::fs::write(path, bytes)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Unit]);
            }
            crate::vm::CapabilityKind::AgentSpawn => {
                let [TypedValue::String(task)] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "agent-spawn requires one task"));
                };
                let Some(binding) = self.scheduler.clone() else {
                    return Err(host_binding_error(origin, "agent scheduler is unavailable"));
                };
                let spawn_binding = binding.clone();
                let task = task.clone();
                let identity = binding
                    .block_on(async move { spawn_binding.spawn(task).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Task {
                    id: identity.task_id.to_string(),
                    result_type: Type::String,
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
                let result = binding
                    .block_on(async move { wait_binding.wait(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::String(result.final_message)]);
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
                let snapshot = binding
                    .block_on(async move { poll_binding.poll(task_id).await })
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let json = serde_json::to_string(&snapshot)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::String(json)]);
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
            crate::vm::CapabilityKind::ProcessRun => {
                let [TypedValue::String(command), TypedValue::List { values, .. }] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "process-run requires a command and string arguments",
                    ));
                };
                let mut process = std::process::Command::new(command);
                for value in values {
                    let TypedValue::String(value) = value else {
                        return Err(host_binding_error(
                            origin,
                            "process-run arguments must be strings",
                        ));
                    };
                    process.arg(value);
                }
                let output = process
                    .output()
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
                            },
                        );
                    return Ok(vec![TypedValue::Resource {
                        kind: "network-socket".into(),
                        handle,
                        generation: 0,
                    }]);
                }
                let [TypedValue::Resource { kind, handle, .. }, TypedValue::Bytes(payload)] =
                    arguments.as_slice()
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
                let mut sockets = self
                    .network
                    .lock()
                    .map_err(|_| host_binding_error(origin, "network lock poisoned"))?;
                let socket = sockets
                    .get_mut(handle)
                    .ok_or_else(|| host_binding_error(origin, "unknown network socket"))?;
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
            crate::vm::CapabilityKind::ScheduleCreate => {
                let [TypedValue::String(callback), TypedValue::Int(timestamp)] =
                    arguments.as_slice()
                else {
                    return Err(host_binding_error(
                        origin,
                        "schedule-create requires a callback and Unix timestamp",
                    ));
                };
                let Some(queue) = self.schedule_queue.clone() else {
                    return Err(host_binding_error(origin, "schedule queue is unavailable"));
                };
                let callback = callback.clone();
                let timestamp = *timestamp;
                let scheduled_time = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
                    .ok_or_else(|| host_binding_error(origin, "invalid schedule timestamp"))?;
                let id = block_on_host(async move {
                    queue
                        .enqueue(ScheduledTask {
                            id: None,
                            scheduled_time,
                            task: callback.clone(),
                            context: "{}".into(),
                            recurring: None,
                            status: TaskStatus::Pending,
                            created_at: chrono::Utc::now(),
                            last_run: None,
                            retries: 0,
                        })
                        .await
                        .map(|id| id.to_string())
                })
                .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Resource {
                    kind: "schedule".into(),
                    handle: id,
                    generation: 0,
                }]);
            }
            _ => {
                return Err(host_binding_error(
                    origin,
                    "authorized capability has no typed host binding",
                ));
            }
        };
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
                if let Some(sink) = &self.output_sink {
                    sink(text.clone());
                }
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

    fn emit(&mut self, chunk: &str) {
        self.side_effects
            .push(crate::vm::interpreter::HostSideEffect::Emit {
                text: chunk.to_string(),
            });
        if let Some(sink) = &self.output_sink {
            sink(chunk.to_string());
        }
    }
}

impl TypedHostHandler {
    fn secure_file_path(
        &self,
        selector: &crate::vm::FileSelector,
        relative: &str,
    ) -> std::result::Result<PathBuf, String> {
        let root = match selector.root {
            crate::vm::ResourceRoot::Workspace => Arc::clone(&self.workspace_root),
            crate::vm::ResourceRoot::HostMachine => self
                .host_machine_root
                .read()
                .map_err(|_| "host-machine root binding lock poisoned".to_string())?
                .clone()
                .ok_or_else(|| "host-machine root is not installed by this host".to_string())?,
            _ => {
                return Err(format!(
                    "file selector root '{}' is unavailable to this host binding",
                    selector.root
                ));
            }
        };
        secure_resource_path(&root, selector, relative)
    }
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

fn secure_resource_path(
    root: &Path,
    selector: &crate::vm::FileSelector,
    relative: &str,
) -> std::result::Result<PathBuf, String> {
    if !selector.matches(relative) || relative.contains(['*', '?']) {
        return Err("path is outside its declared selector".to_string());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("resource root is unavailable: {error}"))?;
    let candidate = root.join(relative);
    let check = if candidate.exists() {
        candidate
            .canonicalize()
            .map_err(|error| format!("path cannot be canonicalized: {error}"))?
    } else {
        let parent = candidate
            .parent()
            .ok_or_else(|| "path has no parent".to_string())?
            .canonicalize()
            .map_err(|error| format!("path parent cannot be canonicalized: {error}"))?;
        parent.join(
            candidate
                .file_name()
                .ok_or_else(|| "path has no filename".to_string())?,
        )
    };
    if !check.starts_with(&root) {
        return Err("path escapes its resource root".to_string());
    }
    Ok(check)
}

impl Default for ProgramRuntime {
    fn default() -> Self {
        Self::new()
    }
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

fn approval_prompts(
    execution_id: uuid::Uuid,
    requirements: &[CapabilityRequirement],
    source: &str,
    intent: &str,
    suspension: Option<&TypedSuspension>,
) -> Vec<ApprovalPrompt> {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let program_hash = format!("{:016x}", hasher.finish());
    if let Some(call) = suspension.and_then(|suspension| suspension.pending_host_call.as_ref()) {
        return vec![ApprovalPrompt::for_request(CapabilityRequest {
            id: uuid::Uuid::new_v4(),
            execution_id,
            effect_sequence: suspension
                .and_then(|suspension| suspension.event_journal.last())
                .map(|effect| effect.sequence),
            reason: intent.to_string(),
            requirement: call.requirement.clone(),
            arguments: call.arguments.clone(),
            origin: call.origin.clone(),
            agent_ancestry: Vec::new(),
            program_hash,
        })];
    }
    requirements
        .iter()
        .cloned()
        .map(|requirement| {
            ApprovalPrompt::for_request(CapabilityRequest {
                id: uuid::Uuid::new_v4(),
                execution_id,
                effect_sequence: None,
                reason: intent.to_string(),
                requirement,
                arguments: Vec::new(),
                origin: SourceOrigin::generated("capability-preflight"),
                agent_ancestry: Vec::new(),
                program_hash: program_hash.clone(),
            })
        })
        .collect()
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
        TypedValue::Task { id, .. } => ProgramValue::Task(id),
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
                ": square ( S int -- S int ! {} ) dup * ;",
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
            .resume_typed_execution_with_effect_result(
                pending.execution_id,
                sequence,
                vec![TypedValue::Bytes(b"from external event loop".to_vec())],
            )
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

        let cancelled = runtime
            .cancel_typed_execution_with_outcome(pending.execution_id)
            .unwrap()
            .expect("pending request should produce a cancelled outcome");
        assert_eq!(cancelled.status, ExecutionStatus::Cancelled);
        assert!(matches!(
            cancelled.effect_journal.as_slice(),
            [crate::vm::EffectJournalEntry {
                state: crate::vm::EffectJournalState::Cancelled,
                ..
            }]
        ));
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
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ProcessRun,
                selector: crate::vm::ResourceSelector::Process {
                    executables: vec!["/usr/bin/printf".into()],
                },
            })
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.values, vec![ProgramValue::String("ok".into())]);
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
    async fn ordinary_output_sink_keeps_legacy_proposal_projection_synchronous() {
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
        assert!(matches!(
            outcome.required_capabilities[0].selector,
            crate::vm::ResourceSelector::Process { ref executables }
                if executables == &["/usr/bin/true"]
        ));
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
    async fn network_send_rechecks_the_socket_endpoint_against_active_grants() {
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
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::NetworkConnect,
                selector: crate::vm::ResourceSelector::Network {
                    host: "127.0.0.1".into(),
                    ports: vec![port],
                },
            })
            .unwrap();
        let connected = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                &format!("s\"127.0.0.1\" {port} network-connect"),
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(connected.status, ExecutionStatus::Completed);

        // The static socket operation can be invoked with any network grant,
        // but the host must check the socket's actual endpoint rather than
        // treating the opaque resource as ambient authority.
        runtime
            .typed
            .lock()
            .unwrap()
            .set_grants(EffectSet::from_requirement(
                crate::vm::CapabilityRequirement {
                    capability: crate::vm::CapabilityKind::NetworkConnect,
                    selector: crate::vm::ResourceSelector::Network {
                        host: "example.test".into(),
                        ports: vec![port],
                    },
                },
            ));
        let sent = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\"ping\" bytes network-send",
                ExecutionEffect::ExternalWrite,
            ))
            .await
            .unwrap();
        assert_eq!(sent.status, ExecutionStatus::Failed);
        assert!(sent
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("no longer covered")));
        server.join().unwrap();
    }

    #[tokio::test]
    async fn typed_say_emits_stream_chunks_and_buffers_result() {
        let runtime = ProgramRuntime::new();
        let chunks = Arc::new(Mutex::new(Vec::<String>::new()));
        let sink_chunks = Arc::clone(&chunks);
        runtime.set_typed_output_sink(Some(Arc::new(move |chunk| {
            sink_chunks.lock().unwrap().push(chunk);
        })));
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" first\" say s\" second\" say",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.output, "firstsecond");
        assert_eq!(&*chunks.lock().unwrap(), &["first", "second"]);
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
    async fn output_open_projects_a_create_event_before_later_updates() {
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
        let create_index = events
            .iter()
            .position(|effect| {
                matches!(
                    &effect.effect.event,
                    crate::vm::interpreter::HostSideEffect::Ui {
                        operation: crate::vm::interpreter::UiOperation::Create,
                        ..
                    }
                )
            })
            .expect("output-open must project a UI create event");
        let request_index = events
            .iter()
            .position(|effect| {
                matches!(
                    &effect.effect.event,
                    crate::vm::interpreter::HostSideEffect::Request { .. }
                )
            })
            .expect("output-open must retain its portable host request");
        assert!(
            request_index < create_index,
            "the durable host request must be journaled before its host-created UI projection"
        );
        assert!(matches!(
            events.get(create_index).map(|effect| &effect.effect.event),
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
    }

    #[tokio::test]
    async fn typed_schedule_create_persists_callback() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = Arc::new(TaskQueue::new(database.path().to_path_buf()).unwrap());
        let runtime = ProgramRuntime::new();
        runtime.attach_schedule_queue(Arc::clone(&queue));
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement {
                capability: crate::vm::CapabilityKind::ScheduleCreate,
                selector: crate::vm::ResourceSelector::Schedule { policy: None },
            })
            .unwrap();
        let timestamp = chrono::Utc::now().timestamp() + 60;
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                &format!("(schedule-create \"check\" {timestamp})"),
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert!(matches!(
            outcome.values.first(),
            Some(ProgramValue::Resource { kind, .. }) if kind == "schedule"
        ));
        assert_eq!(queue.get_ready_tasks().await.unwrap().len(), 0);
    }

    #[tokio::test]
    async fn durable_scheduler_reenters_typed_runtime_for_callbacks() {
        let database = tempfile::NamedTempFile::new().unwrap();
        let queue = Arc::new(TaskQueue::new(database.path().to_path_buf()).unwrap());
        queue
            .enqueue(ScheduledTask {
                id: None,
                scheduled_time: chrono::Utc::now(),
                task: "s\"scheduled callback\" say".into(),
                context: "{}".into(),
                recurring: None,
                status: TaskStatus::Pending,
                created_at: chrono::Utc::now(),
                last_run: None,
                retries: 0,
            })
            .await
            .unwrap();
        let runtime = Arc::new(ProgramRuntime::new());
        runtime.attach_schedule_queue(Arc::clone(&queue));
        let scheduler = runtime.task_scheduler().expect("scheduler is attached");
        scheduler.run_once().await.unwrap();
        assert!(queue.get_ready_tasks().await.unwrap().is_empty());
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

    #[cfg(unix)]
    #[test]
    fn workspace_path_rejects_a_stable_symlink_escape_before_host_io() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = workspace.path().join("outside-link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        let selector = crate::vm::FileSelector::parse("./**").unwrap();

        let error =
            secure_resource_path(&workspace.path().to_path_buf(), &selector, "outside-link")
                .unwrap_err();
        assert!(error.contains("escapes its resource root"));
    }

    #[test]
    fn generic_resource_resolution_does_not_assign_host_roots_to_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let selector = crate::vm::FileSelector::parse("${host-machine}/etc/**").unwrap();
        std::fs::create_dir(workspace.path().join("etc")).unwrap();

        // Root selection happens in `TypedHostHandler`; the generic canonical
        // check only proves that a child remains under the root selected by
        // the host binding.
        let path =
            secure_resource_path(&workspace.path().to_path_buf(), &selector, "etc/hosts").unwrap();
        assert_eq!(
            path,
            workspace.path().canonicalize().unwrap().join("etc/hosts")
        );
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
}
