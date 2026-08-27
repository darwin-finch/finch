use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityKind, CapabilityRequirement, EffectSet};
use super::frontend::{forth::compile_forth_with_functions, lisp::compile_lisp_with_functions};
use super::interpreter::{
    CapabilityHandler, HostSideEffect, InterpreterConfig, VmContinuation, VmSideEffect, VmStep,
    VmTrampoline,
};
use super::ir::{Function, Module};
use super::types::{Type, TypedValue};
use super::{core_vocabulary, VerifiedModule, Verifier, Vocabulary, VM_TYPE_SYSTEM_VERSION};
use crate::programs::ProgramLanguage;
use crate::runtime::fiber::CpuFiberScheduler;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use uuid::Uuid;

/// One durable, idempotently-addressable host-effect record. `effect` retains
/// the portable request while `state` records whether the host has merely
/// seen it, is waiting for consent, or has supplied a result to the exact
/// saved continuation. VM-local rollback never rewrites an acknowledged host
/// fact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectJournalEntry {
    pub effect: VmSideEffect,
    pub state: EffectJournalState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectJournalState {
    Proposed,
    AwaitingApproval,
    /// An authorized request was delivered to an external host adapter and
    /// awaits its correlated typed result. This is distinct from asking the
    /// user to grant a missing capability.
    AwaitingHostResult,
    Acknowledged {
        values: Vec<TypedValue>,
    },
    Denied,
    Cancelled,
    /// The host binding returned a structured fault before it supplied a
    /// resume value. The host may have performed a partial external effect;
    /// callers must surface this prefix rather than calling rollback atomic.
    Failed {
        diagnostic: VmDiagnostic,
    },
}

/// Result of compiling and interpreting one source submission. Authorization
/// is decided at each concrete capability boundary; the VM publishes only a
/// serializable continuation while its persistent stack remains transactional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedExecution {
    pub status: TypedExecutionStatus,
    pub values: Vec<TypedValue>,
    pub output: String,
    #[serde(default)]
    pub output_chunks: Vec<String>,
    #[serde(default)]
    pub side_effects: Vec<HostSideEffect>,
    /// Portable VM event journal. Unlike `side_effects`, this retains ordered
    /// sequence numbers, capabilities, and origins for another harness to
    /// project/replay without Finch-specific rendering.
    #[serde(default)]
    pub vm_side_effects: Vec<VmSideEffect>,
    /// State-bearing companion to `vm_side_effects`. The latter remains for
    /// compatibility with portable effect consumers that only need the event
    /// stream; new persistence and recovery code must retain this journal.
    #[serde(default)]
    pub effect_journal: Vec<EffectJournalEntry>,
    pub effects: EffectSet,
    pub diagnostics: Vec<VmDiagnostic>,
    /// Present when the event loop owns the next VM step. This contains a
    /// verified module and frame data, never submitted source text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspension: Option<TypedSuspension>,
}

/// Durable state for an execution paused at an explicit VM boundary. The
/// daemon persists this next to the Brain's revision and authorization audit;
/// resumption therefore continues the exact verified program rather than
/// reinterpreting or resubmitting source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedSuspension {
    pub module: VerifiedModule,
    pub continuation: VmContinuation,
    /// Value published by the `yield` that created this suspension. `None`
    /// identifies an await/join boundary rather than a producer suspension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yielded_value: Option<TypedValue>,
    pub effects: EffectSet,
    /// Events already emitted before this saved continuation. They are part of
    /// the execution journal and must not be repeated when a run resumes.
    #[serde(default)]
    pub event_journal: Vec<VmSideEffect>,
    #[serde(default)]
    pub effect_journal: Vec<EffectJournalEntry>,
    /// Transactional producer state private to this suspended ProgramRun.
    /// The authoritative runtime registry is unchanged until the run commits.
    #[serde(default)]
    pub producer_fibers: BTreeMap<String, ProducerFiberRecord>,
    /// A parent frame blocked on a bounded local CPU-fiber result. This is a
    /// scheduler wait, not a host capability request and never blocks the UI
    /// thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_cpu_fiber: Option<PendingCpuFiber>,
    /// The host operation that caused an authorization boundary. It stays as
    /// typed data until a grant is made; resumption invokes the binding once
    /// and supplies its typed return values to the saved frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_host_call: Option<PendingHostCall>,
}

/// Serializable reducible state of a typed runtime at a successful commit
/// boundary. Host-owned resources intentionally do not appear here: a stream,
/// output handle, or local CPU worker needs its owning application to restore
/// it before it can be used again. Keeping that boundary explicit prevents a
/// restart from turning an opaque ID into accidental ambient authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypedRuntimeCheckpoint {
    pub version: u32,
    pub stack: Vec<TypedValue>,
    /// User-defined functions include generated `lambda$` bodies needed by a
    /// persisted closure. Core vocabulary is reconstructed from the runtime
    /// version rather than copied into every checkpoint.
    #[serde(default)]
    pub functions: BTreeMap<String, Function>,
    /// Cooperative producer continuations are VM state rather than host
    /// resources, so they survive the same checkpoint as their opaque handles.
    #[serde(default)]
    pub producer_fibers: BTreeMap<String, ProducerFiberRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProducerFiberState {
    Ready { continuation: VmContinuation },
    Completed { result: TypedValue },
    Failed { diagnostic: VmDiagnostic },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProducerFiberRecord {
    pub module: VerifiedModule,
    pub yield_type: Type,
    pub result_type: Type,
    pub state: ProducerFiberState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingHostCall {
    pub requirement: CapabilityRequirement,
    pub arguments: Vec<TypedValue>,
    /// The verified ABI expected from the host when it acknowledges this
    /// effect through `VmResume`.
    #[serde(default)]
    pub output: Vec<Type>,
    pub origin: SourceOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingCpuFiber {
    pub task: TypedValue,
    pub origin: SourceOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TypedExecutionStatus {
    Completed,
    /// The VM deliberately reached a cooperative boundary (`yield` or an
    /// emitted event). The next turn is represented by `TypedExecution`'s
    /// `suspension`, not by a source-level continuation value.
    Suspended,
    AuthorizationRequired {
        requirements: Vec<CapabilityRequirement>,
    },
    Failed,
}

struct CpuFiberLease {
    scheduler: Arc<CpuFiberScheduler>,
    owner: Uuid,
    roots: BTreeSet<Uuid>,
}

impl CpuFiberLease {
    fn new(scheduler: Arc<CpuFiberScheduler>) -> Self {
        Self {
            scheduler,
            owner: Uuid::new_v4(),
            roots: BTreeSet::new(),
        }
    }

    fn adopt_spawned(&mut self, id: Uuid) {
        self.roots.insert(id);
    }

    fn replace_roots(&mut self, roots: BTreeSet<Uuid>) {
        for id in roots.difference(&self.roots) {
            self.scheduler
                .attach_owner(*id, self.owner)
                .expect("reachable CPU task must have a scheduler record");
        }
        for id in self.roots.difference(&roots) {
            let _ = self.scheduler.release_owner(*id, self.owner);
        }
        self.roots = roots;
    }
}

impl Clone for CpuFiberLease {
    fn clone(&self) -> Self {
        let owner = Uuid::new_v4();
        for id in &self.roots {
            self.scheduler
                .attach_owner(*id, owner)
                .expect("cloned CPU task lease must reference a scheduler record");
        }
        Self {
            scheduler: Arc::clone(&self.scheduler),
            owner,
            roots: self.roots.clone(),
        }
    }
}

impl Drop for CpuFiberLease {
    fn drop(&mut self) {
        for id in &self.roots {
            let _ = self.scheduler.release_owner(*id, self.owner);
        }
    }
}

/// Persistent typed stack shared by Finch Lisp and Co-Forth source.
pub struct TypedRuntime {
    stack: Vec<TypedValue>,
    vocabulary: Vocabulary,
    functions: BTreeMap<String, Function>,
    grants: EffectSet,
    cpu_fibers: CpuFiberLease,
    producer_fibers: BTreeMap<String, ProducerFiberRecord>,
}

impl Clone for TypedRuntime {
    fn clone(&self) -> Self {
        Self {
            stack: self.stack.clone(),
            vocabulary: self.vocabulary.clone(),
            functions: self.functions.clone(),
            grants: self.grants.clone(),
            cpu_fibers: self.cpu_fibers.clone(),
            producer_fibers: self.producer_fibers.clone(),
        }
    }
}

impl Default for TypedRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl TypedRuntime {
    pub fn new() -> Self {
        let cpu_fibers = Arc::new(CpuFiberScheduler::new(
            std::thread::available_parallelism()
                .map_or(1, |parallelism| parallelism.get().saturating_sub(1).max(1)),
        ));
        Self {
            stack: Vec::new(),
            vocabulary: core_vocabulary(),
            functions: BTreeMap::new(),
            // Producing the requested assistant response is part of the
            // session contract, not an ambient host permission.
            grants: Self::intrinsic_grants(),
            cpu_fibers: CpuFiberLease::new(cpu_fibers),
            producer_fibers: BTreeMap::new(),
        }
    }

    pub fn stack(&self) -> &[TypedValue] {
        &self.stack
    }

    pub fn vocabulary(&self) -> &Vocabulary {
        &self.vocabulary
    }

    pub fn functions(&self) -> &BTreeMap<String, Function> {
        &self.functions
    }

    /// Replace application-supplied host vocabulary without allowing it to
    /// shadow a core word or a source-defined function. These bindings are
    /// availability metadata and are intentionally rebuilt by the host after
    /// checkpoint restoration.
    pub fn replace_host_vocabulary(
        &mut self,
        previous_names: impl IntoIterator<Item = String>,
        replacements: &BTreeMap<String, super::signature::StackSignature>,
    ) -> Result<(), VmDiagnostic> {
        let core = core_vocabulary();
        for name in replacements.keys() {
            if core.contains_key(name) || self.functions.contains_key(name) {
                return Err(VmDiagnostic::error(
                    "E-LINK-006",
                    DiagnosticPhase::Linking,
                    format!("host vocabulary cannot shadow word '{name}'"),
                    Some(SourceOrigin::generated(name.clone())),
                ));
            }
        }
        for name in previous_names {
            if !core.contains_key(&name) && !self.functions.contains_key(&name) {
                self.vocabulary.remove(&name);
            }
        }
        self.vocabulary.extend(replacements.clone());
        Ok(())
    }

    /// Reclaim producer records after their last language-visible handle has
    /// disappeared. Terminal records remain deterministic tombstones while a
    /// duplicate/nested handle is reachable; ready records referenced by a
    /// live producer continuation are retained transitively.
    fn collect_unreachable_producer_fibers(&mut self) {
        let mut pending = VecDeque::new();
        for value in &self.stack {
            collect_producer_fiber_ids(value, &mut pending);
        }

        let mut reachable = BTreeSet::new();
        while let Some(id) = pending.pop_front() {
            if !reachable.insert(id.clone()) {
                continue;
            }
            let Some(record) = self.producer_fibers.get(&id) else {
                continue;
            };
            match &record.state {
                ProducerFiberState::Ready { continuation } => {
                    for value in &continuation.stack {
                        collect_producer_fiber_ids(value, &mut pending);
                    }
                    for frame in &continuation.frames {
                        for value in frame.locals.iter().chain(&frame.captures) {
                            collect_producer_fiber_ids(value, &mut pending);
                        }
                    }
                }
                ProducerFiberState::Completed { result } => {
                    collect_producer_fiber_ids(result, &mut pending);
                }
                ProducerFiberState::Failed { .. } | ProducerFiberState::Cancelled => {}
            }
        }
        self.producer_fibers.retain(|id, _| reachable.contains(id));
    }

    /// Reconcile this private runtime snapshot's CPU-task leases with every
    /// task handle that remains language-reachable after a successful commit.
    /// Producer continuations are durable roots too, even though their values
    /// are not currently visible on the public operand stack.
    fn collect_reachable_cpu_fibers(&mut self) {
        let mut reachable = BTreeSet::new();
        for value in &self.stack {
            collect_cpu_fiber_ids(value, &mut reachable);
        }
        for record in self.producer_fibers.values() {
            match &record.state {
                ProducerFiberState::Ready { continuation } => {
                    for value in &continuation.stack {
                        collect_cpu_fiber_ids(value, &mut reachable);
                    }
                    for frame in &continuation.frames {
                        for value in frame.locals.iter().chain(&frame.captures) {
                            collect_cpu_fiber_ids(value, &mut reachable);
                        }
                    }
                }
                ProducerFiberState::Completed { result } => {
                    collect_cpu_fiber_ids(result, &mut reachable);
                }
                ProducerFiberState::Failed { .. } | ProducerFiberState::Cancelled => {}
            }
        }
        self.cpu_fibers.replace_roots(reachable);
    }

    /// Capture only state that can safely survive outside this process. This
    /// is the VM half of a future durable Brain checkpoint; application-owned
    /// effects and handles stay in their own journal/registry.
    pub fn checkpoint(&self) -> Result<TypedRuntimeCheckpoint, VmDiagnostic> {
        for value in &self.stack {
            ensure_checkpointable(value)?;
            validate_fiber_handle(value, &self.producer_fibers)?;
        }
        for record in self.producer_fibers.values() {
            validate_producer_record(record, &self.producer_fibers)?;
        }
        Ok(TypedRuntimeCheckpoint {
            version: VM_TYPE_SYSTEM_VERSION,
            stack: self.stack.clone(),
            functions: self.functions.clone(),
            producer_fibers: self.producer_fibers.clone(),
        })
    }

    /// Restore a checkpoint into a fresh typed runtime and reverify every
    /// persisted definition against the current core vocabulary. Grants are
    /// intentionally not restored: authority belongs to the host's current
    /// approval policy, never to serialized program state.
    pub fn from_checkpoint(checkpoint: TypedRuntimeCheckpoint) -> Result<Self, Vec<VmDiagnostic>> {
        if checkpoint.version != VM_TYPE_SYSTEM_VERSION {
            return Err(vec![VmDiagnostic::error(
                "E-CHECKPOINT-001",
                DiagnosticPhase::Linking,
                format!(
                    "unsupported typed runtime checkpoint version {}; expected {}",
                    checkpoint.version, VM_TYPE_SYSTEM_VERSION
                ),
                None,
            )]);
        }
        for value in &checkpoint.stack {
            if let Err(diagnostic) = ensure_checkpointable(value) {
                return Err(vec![diagnostic]);
            }
            if let Err(diagnostic) = validate_fiber_handle(value, &checkpoint.producer_fibers) {
                return Err(vec![diagnostic]);
            }
        }
        for (id, fiber) in &checkpoint.producer_fibers {
            if fiber.module.module.version != VM_TYPE_SYSTEM_VERSION {
                return Err(vec![VmDiagnostic::error(
                    "E-CHECKPOINT-005",
                    DiagnosticPhase::Linking,
                    format!("producer fiber {id} uses an obsolete VM version"),
                    None,
                )]);
            }
            Verifier::new(&core_vocabulary()).verify(fiber.module.module.clone())?;
            if let Err(diagnostic) = validate_producer_record(fiber, &checkpoint.producer_fibers) {
                return Err(vec![diagnostic]);
            }
        }

        let mut vocabulary = core_vocabulary();
        for (name, function) in &checkpoint.functions {
            if vocabulary.contains_key(name) {
                return Err(vec![VmDiagnostic::error(
                    "E-CHECKPOINT-004",
                    DiagnosticPhase::Linking,
                    format!("checkpoint definition '{name}' shadows an immutable core word"),
                    None,
                )]);
            }
            if !name.starts_with("lambda$") {
                vocabulary.insert(name.clone(), function.signature.clone());
            }
        }
        if let Some(entry) = checkpoint.functions.keys().next().cloned() {
            Verifier::new(&vocabulary).verify(Module {
                version: VM_TYPE_SYSTEM_VERSION,
                name: "typed-runtime-checkpoint".to_owned(),
                entry,
                functions: checkpoint.functions.clone(),
            })?;
        }

        let mut runtime = Self::new();
        runtime.stack = checkpoint.stack;
        runtime.vocabulary = vocabulary;
        runtime.functions = checkpoint.functions;
        runtime.producer_fibers = checkpoint.producer_fibers;
        Ok(runtime)
    }

    pub fn grants(&self) -> &EffectSet {
        &self.grants
    }

    pub fn intrinsic_grants() -> EffectSet {
        EffectSet::from_requirement(CapabilityRequirement {
            capability: CapabilityKind::SessionEmit,
            selector: super::effects::ResourceSelector::None,
        })
        .union(&EffectSet::from_requirement(CapabilityRequirement {
            capability: CapabilityKind::VmRead,
            selector: super::effects::ResourceSelector::None,
        }))
    }

    pub fn set_grants(&mut self, grants: EffectSet) {
        self.grants = grants;
    }

    pub fn grant(&mut self, requirement: CapabilityRequirement) {
        self.grants = self.grants.union(&EffectSet::from_requirement(requirement));
    }

    /// Cancel the private CPU worker referenced by a saved suspension, if the
    /// parent was waiting on one. This is intentionally separate from host
    /// effect cancellation: CPU fibers are pure local computation and have no
    /// external-effect compensation path.
    pub fn cancel_suspended_cpu_fiber(
        &self,
        suspension: &TypedSuspension,
    ) -> Result<bool, VmDiagnostic> {
        let Some(pending) = &suspension.pending_cpu_fiber else {
            return Ok(false);
        };
        self.cancel_cpu_fiber(&pending.task, &pending.origin)?;
        Ok(true)
    }

    pub fn execute(
        &mut self,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        fuel: u64,
    ) -> TypedExecution {
        self.execute_with_declaration(language, source_id, source, fuel, None)
    }

    pub fn execute_with_declaration(
        &mut self,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        fuel: u64,
        declared: Option<&EffectSet>,
    ) -> TypedExecution {
        let mut handler = RuntimeCapabilities::default();
        self.execute_with_handler(language, source_id, source, fuel, declared, &mut handler)
    }

    /// Execute using a host-owned capability handler. This is the integration
    /// seam for automation, files, agents, memory, scheduling, and provider
    /// services; authorization and VM transactions remain in the shared VM.
    pub fn execute_with_handler<H: CapabilityHandler>(
        &mut self,
        language: ProgramLanguage,
        source_id: &str,
        source: &str,
        fuel: u64,
        declared: Option<&EffectSet>,
        handler: &mut H,
    ) -> TypedExecution {
        let initial_types = self.stack.iter().map(TypedValue::value_type).collect();
        let compiled = match language {
            ProgramLanguage::Forth => compile_forth_with_functions(
                source_id,
                source,
                initial_types,
                &self.vocabulary,
                &self.functions,
            ),
            ProgramLanguage::Lisp => compile_lisp_with_functions(
                source_id,
                source,
                initial_types,
                &self.vocabulary,
                &self.functions,
            ),
        };
        let module = match compiled {
            Ok(module) => module,
            Err(diagnostics) => return TypedExecution::failed(diagnostics),
        };
        let effects = entry_effects(&module);
        if let Some(declared) = declared {
            if !declared.grants(&effects) {
                let mut diagnostic = VmDiagnostic::error(
                    "E-CAP-003",
                    DiagnosticPhase::Verification,
                    format!(
                        "declared capabilities {declared} do not cover inferred requirements {effects}"
                    ),
                    None,
                );
                diagnostic.expected_effects = effects.clone();
                diagnostic.found_effects = declared.clone();
                return TypedExecution {
                    status: TypedExecutionStatus::Failed,
                    values: Vec::new(),
                    output: String::new(),
                    output_chunks: Vec::new(),
                    side_effects: Vec::new(),
                    vm_side_effects: Vec::new(),
                    effect_journal: Vec::new(),
                    effects,
                    diagnostics: vec![diagnostic],
                    suspension: None,
                };
            }
        }
        let trampoline = VmTrampoline::new(
            &module,
            &InterpreterConfig {
                fuel,
                grants: self.grants.clone(),
            },
        );
        let continuation = match trampoline.start(self.stack.clone()) {
            Ok(continuation) => continuation,
            Err(diagnostic) => return TypedExecution::failed(vec![diagnostic]),
        };
        let baseline = self.producer_fibers.clone();
        let cpu_baseline = self.cpu_fibers.roots.clone();
        let execution = self.drive(module, continuation, handler);
        let execution = self.finish_producer_transaction(execution, baseline);
        self.finish_cpu_task_transaction(execution, cpu_baseline)
    }

    /// Continue a previously returned VM boundary. Callers retain this token
    /// in the event loop/Brain record and may resume it after an approval or
    /// external host result. It never reparses the original submission.
    pub fn resume_with_handler<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        values: Vec<TypedValue>,
        handler: &mut H,
    ) -> TypedExecution {
        self.resume_with_handler_inner(suspension, values, None, false, handler)
    }

    /// Resume exactly the host call already captured by `suspension` after
    /// the application has independently authorized its concrete request.
    /// The authorization applies only to that pending boundary: subsequent
    /// calls continue under this runtime's ordinary grant set.
    pub(crate) fn resume_authorized_host_call_with_handler<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        handler: &mut H,
    ) -> TypedExecution {
        self.resume_with_handler_inner(suspension, Vec::new(), None, true, handler)
    }

    /// Acknowledge one already-journaled host effect with values supplied by
    /// an external event loop.  This is the portable `VmResume` path used by
    /// UI/proposal/IDE hosts; it does not invoke the host binding again.
    pub fn resume_with_effect_result<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        effect_sequence: u64,
        values: Vec<TypedValue>,
        handler: &mut H,
    ) -> TypedExecution {
        self.resume_with_handler_inner(
            suspension,
            Vec::new(),
            Some((effect_sequence, values)),
            false,
            handler,
        )
    }

    fn resume_with_handler_inner<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        values: Vec<TypedValue>,
        external_effect_result: Option<(u64, Vec<TypedValue>)>,
        authorize_pending_host_call: bool,
        handler: &mut H,
    ) -> TypedExecution {
        let baseline = self.producer_fibers.clone();
        let cpu_baseline = self.cpu_fibers.roots.clone();
        self.producer_fibers = suspension.producer_fibers.clone();
        let execution = self.resume_with_handler_working(
            suspension,
            values,
            external_effect_result,
            authorize_pending_host_call,
            handler,
        );
        let execution = self.finish_producer_transaction(execution, baseline);
        self.finish_cpu_task_transaction(execution, cpu_baseline)
    }

    fn resume_with_handler_working<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        values: Vec<TypedValue>,
        external_effect_result: Option<(u64, Vec<TypedValue>)>,
        authorize_pending_host_call: bool,
        handler: &mut H,
    ) -> TypedExecution {
        let TypedSuspension {
            module,
            continuation,
            yielded_value: _,
            effects,
            pending_host_call,
            event_journal,
            mut effect_journal,
            pending_cpu_fiber,
            producer_fibers: _,
        } = suspension;
        let values = if let Some(pending_cpu_fiber) = pending_cpu_fiber {
            if external_effect_result.is_some() {
                return self.failed_from_drive(
                    effects,
                    VmDiagnostic::error(
                        "E-RESUME-004",
                        DiagnosticPhase::HostCall,
                        "an external effect result cannot resume a pending CPU task",
                        Some(pending_cpu_fiber.origin),
                    ),
                    event_journal,
                    effect_journal,
                    handler,
                );
            }
            if !values.is_empty() {
                return self.failed_from_drive(
                    effects,
                    VmDiagnostic::error(
                        "E-RESUME-002",
                        DiagnosticPhase::HostCall,
                        "a pending CPU task cannot be resumed with external values",
                        Some(pending_cpu_fiber.origin),
                    ),
                    event_journal,
                    effect_journal,
                    handler,
                );
            }
            match self.cpu_fiber_result(&pending_cpu_fiber.task, &pending_cpu_fiber.origin) {
                Ok(Some(values)) => values,
                Ok(None) => {
                    return self.suspended_cpu_fiber(
                        module,
                        effects,
                        continuation,
                        event_journal,
                        effect_journal,
                        pending_cpu_fiber,
                        handler,
                    );
                }
                Err(diagnostic) => {
                    return self.failed_from_drive(
                        effects,
                        diagnostic,
                        event_journal,
                        effect_journal,
                        handler,
                    )
                }
            }
        } else if let Some(call) = pending_host_call {
            if let Some((sequence, values)) = external_effect_result {
                let actual = event_journal.last().map(|effect| effect.sequence);
                if actual != Some(sequence) {
                    return self.failed_from_drive(
                        effects,
                        VmDiagnostic::error(
                            "E-RESUME-005",
                            DiagnosticPhase::HostCall,
                            format!(
                                "stale host-effect resume: supplied sequence {sequence}, pending sequence is {:?}",
                                actual
                            ),
                            Some(call.origin),
                        ),
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                let requested = EffectSet::from_requirement(call.requirement.clone());
                if !authorize_pending_host_call && !self.grants.grants(&requested) {
                    return self.authorization_required(
                        module,
                        continuation,
                        effects,
                        event_journal,
                        effect_journal,
                        call,
                        handler,
                    );
                }
                let Some(effect) = event_journal.last().cloned() else {
                    return self.failed_from_drive(
                        effects,
                        VmDiagnostic::error(
                            "E-RESUME-006",
                            DiagnosticPhase::HostCall,
                            "a pending host result has no correlated effect journal entry",
                            Some(call.origin),
                        ),
                        event_journal,
                        effect_journal,
                        handler,
                    );
                };
                if let Err(diagnostic) = handler.authorize_awaited_effect(&effect) {
                    fail_last(&mut effect_journal, diagnostic.clone());
                    return self.failed_from_drive(
                        effects,
                        diagnostic,
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                if let Err(diagnostic) =
                    super::interpreter::validate_host_result(&call.output, &values, &call.origin)
                {
                    fail_last(&mut effect_journal, diagnostic.clone());
                    return self.failed_from_drive(
                        effects,
                        diagnostic,
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                acknowledge_last(&mut effect_journal, values.clone());
                values
            } else {
                if !values.is_empty() {
                    return self.failed_from_drive(
                        effects,
                        VmDiagnostic::error(
                            "E-RESUME-001",
                            DiagnosticPhase::HostCall,
                            "a pending host call cannot be resumed with external values",
                            Some(call.origin),
                        ),
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                let requested = EffectSet::from_requirement(call.requirement.clone());
                if !authorize_pending_host_call && !self.grants.grants(&requested) {
                    return self.authorization_required(
                        module,
                        continuation,
                        effects,
                        event_journal,
                        effect_journal,
                        call,
                        handler,
                    );
                }
                let Some(effect) = event_journal.last().cloned() else {
                    return self.failed_from_drive(
                        effects,
                        VmDiagnostic::error(
                            "E-RESUME-006",
                            DiagnosticPhase::HostCall,
                            "a pending host call has no correlated effect journal entry",
                            Some(call.origin),
                        ),
                        event_journal,
                        effect_journal,
                        handler,
                    );
                };
                if let Err(diagnostic) = handler.authorize_awaited_effect(&effect) {
                    fail_last(&mut effect_journal, diagnostic.clone());
                    return self.failed_from_drive(
                        effects,
                        diagnostic,
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                match handler.request_effect(&effect).and_then(|values| {
                    super::interpreter::validate_host_result(&call.output, &values, &call.origin)
                        .map(|()| values)
                }) {
                    Ok(values) => {
                        acknowledge_last(&mut effect_journal, values.clone());
                        values
                    }
                    Err(diagnostic) => {
                        fail_last(&mut effect_journal, diagnostic.clone());
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                }
            }
        } else {
            if external_effect_result.is_some() {
                return self.failed_from_drive(
                    effects,
                    VmDiagnostic::error(
                        "E-RESUME-004",
                        DiagnosticPhase::HostCall,
                        "an external effect result requires a pending host effect",
                        None,
                    ),
                    event_journal,
                    effect_journal,
                    handler,
                );
            }
            values
        };
        let step = {
            let trampoline = VmTrampoline::new(
                &module,
                &InterpreterConfig {
                    fuel: continuation.fuel,
                    grants: self.grants.clone(),
                },
            );
            trampoline.resume(continuation, values)
        };
        self.drive_step(
            module,
            effects,
            step,
            event_journal,
            effect_journal,
            handler,
        )
    }

    fn authorization_required<H: CapabilityHandler>(
        &self,
        module: VerifiedModule,
        continuation: VmContinuation,
        effects: EffectSet,
        event_journal: Vec<VmSideEffect>,
        effect_journal: Vec<EffectJournalEntry>,
        call: PendingHostCall,
        handler: &H,
    ) -> TypedExecution {
        TypedExecution {
            status: TypedExecutionStatus::AuthorizationRequired {
                requirements: vec![call.requirement.clone()],
            },
            values: Vec::new(),
            output: handler.output(),
            output_chunks: handler.output_chunks(),
            side_effects: handler.side_effects(),
            vm_side_effects: event_journal.clone(),
            effect_journal: effect_journal.clone(),
            effects: effects.clone(),
            diagnostics: Vec::new(),
            suspension: Some(TypedSuspension {
                module,
                continuation,
                yielded_value: None,
                effects,
                event_journal,
                effect_journal,
                producer_fibers: BTreeMap::new(),
                pending_cpu_fiber: None,
                pending_host_call: Some(call),
            }),
        }
    }

    fn drive<H: CapabilityHandler>(
        &mut self,
        module: VerifiedModule,
        continuation: VmContinuation,
        handler: &mut H,
    ) -> TypedExecution {
        let effects = entry_effects(&module);
        let step = {
            let trampoline = VmTrampoline::new(
                &module,
                &InterpreterConfig {
                    fuel: continuation.fuel,
                    grants: self.grants.clone(),
                },
            );
            trampoline.run(continuation)
        };
        self.drive_step(module, effects, step, Vec::new(), Vec::new(), handler)
    }

    fn drive_step<H: CapabilityHandler>(
        &mut self,
        module: VerifiedModule,
        effects: EffectSet,
        mut step: VmStep,
        mut event_journal: Vec<VmSideEffect>,
        mut effect_journal: Vec<EffectJournalEntry>,
        handler: &mut H,
    ) -> TypedExecution {
        loop {
            step = match step {
                VmStep::Yielded {
                    value,
                    continuation,
                } => {
                    return self.suspended(
                        module,
                        effects,
                        value,
                        continuation,
                        event_journal,
                        effect_journal,
                        handler,
                    );
                }
                VmStep::Emit {
                    effect,
                    continuation,
                } => {
                    event_journal.push(effect.clone());
                    effect_journal.push(EffectJournalEntry {
                        effect: effect.clone(),
                        state: EffectJournalState::Proposed,
                    });
                    if let Err(diagnostic) = handler.side_effect(&effect) {
                        fail_last(&mut effect_journal, diagnostic.clone());
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    acknowledge_last(&mut effect_journal, Vec::new());
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.run(continuation)
                }
                VmStep::Await {
                    mut effect,
                    output,
                    continuation,
                } => {
                    if let Err(diagnostic) = handler.prepare_awaited_effect(&mut effect) {
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    let HostSideEffect::Request { arguments } = &effect.event else {
                        return self.failed_from_drive(
                            effects,
                            VmDiagnostic::error(
                                "E-HOST-002",
                                DiagnosticPhase::HostCall,
                                "VM await boundary did not carry a host request",
                                Some(effect.origin),
                            ),
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    };
                    let requirement = effect.requirement.clone();
                    let origin = effect.origin.clone();
                    let arguments = arguments.clone();
                    event_journal.push(effect.clone());
                    effect_journal.push(EffectJournalEntry {
                        effect: effect.clone(),
                        state: EffectJournalState::Proposed,
                    });
                    let requested = EffectSet::from_requirement(requirement.clone());
                    if !self.grants.grants(&requested) {
                        if let Err(diagnostic) = handler.observe_awaited_effect(&effect) {
                            fail_last(&mut effect_journal, diagnostic.clone());
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            );
                        }
                        awaiting_last(&mut effect_journal);
                        return TypedExecution {
                            status: TypedExecutionStatus::AuthorizationRequired {
                                requirements: vec![requirement.clone()],
                            },
                            values: Vec::new(),
                            output: handler.output(),
                            output_chunks: handler.output_chunks(),
                            side_effects: handler.side_effects(),
                            vm_side_effects: event_journal.clone(),
                            effect_journal: effect_journal.clone(),
                            effects: effects.clone(),
                            diagnostics: Vec::new(),
                            suspension: Some(TypedSuspension {
                                module,
                                continuation,
                                yielded_value: None,
                                effects,
                                event_journal,
                                effect_journal,
                                producer_fibers: BTreeMap::new(),
                                pending_cpu_fiber: None,
                                pending_host_call: Some(PendingHostCall {
                                    requirement: requirement.clone(),
                                    arguments: arguments.clone(),
                                    output,
                                    origin,
                                }),
                            }),
                        };
                    }
                    if let Err(diagnostic) = handler.authorize_awaited_effect(&effect) {
                        fail_last(&mut effect_journal, diagnostic.clone());
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    if let Err(diagnostic) = handler.observe_awaited_effect(&effect) {
                        fail_last(&mut effect_journal, diagnostic.clone());
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    if handler.defer_awaited_effect(&effect) {
                        if let Some(entry) = effect_journal.last_mut() {
                            entry.state = EffectJournalState::AwaitingHostResult;
                        }
                        return TypedExecution {
                            status: TypedExecutionStatus::Suspended,
                            values: Vec::new(),
                            output: handler.output(),
                            output_chunks: handler.output_chunks(),
                            side_effects: handler.side_effects(),
                            vm_side_effects: event_journal.clone(),
                            effect_journal: effect_journal.clone(),
                            effects: effects.clone(),
                            diagnostics: Vec::new(),
                            suspension: Some(TypedSuspension {
                                module,
                                continuation,
                                yielded_value: None,
                                effects,
                                event_journal,
                                effect_journal,
                                producer_fibers: BTreeMap::new(),
                                pending_cpu_fiber: None,
                                pending_host_call: Some(PendingHostCall {
                                    requirement,
                                    arguments,
                                    output,
                                    origin,
                                }),
                            }),
                        };
                    }
                    let values = match handler.request_effect(&effect).and_then(|values| {
                        super::interpreter::validate_host_result(&output, &values, &effect.origin)
                            .map(|()| values)
                    }) {
                        Ok(values) => {
                            acknowledge_last(&mut effect_journal, values.clone());
                            values
                        }
                        Err(diagnostic) => {
                            fail_last(&mut effect_journal, diagnostic.clone());
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            );
                        }
                    };
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, values)
                }
                VmStep::SpawnFiber {
                    closure,
                    origin,
                    continuation,
                } => {
                    let handle = match self.spawn_producer_fiber(
                        module.clone(),
                        closure,
                        continuation.fuel,
                        &origin,
                    ) {
                        Ok(handle) => handle,
                        Err(diagnostic) => {
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            )
                        }
                    };
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![handle])
                }
                VmStep::NextFiber {
                    fiber,
                    origin,
                    continuation,
                } => {
                    let value = match self.advance_producer_fiber(&fiber, false, &origin) {
                        Ok(value) => value,
                        Err(diagnostic) => {
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            )
                        }
                    };
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![value])
                }
                VmStep::JoinFiber {
                    fiber,
                    origin,
                    continuation,
                } => {
                    let value = match self.advance_producer_fiber(&fiber, true, &origin) {
                        Ok(value) => value,
                        Err(diagnostic) => {
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            )
                        }
                    };
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![value])
                }
                VmStep::CancelFiber {
                    fiber,
                    origin,
                    continuation,
                } => {
                    if let Err(diagnostic) = self.cancel_producer_fiber(&fiber, &origin) {
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![TypedValue::Unit])
                }
                VmStep::SpawnCpuFiber {
                    closure,
                    origin,
                    continuation,
                } => {
                    let TypedValue::Closure { signature, .. } = &closure else {
                        return self.failed_from_drive(
                            effects,
                            VmDiagnostic::error(
                                "E-FIBER-003",
                                DiagnosticPhase::Interpretation,
                                "defer-cpu requires a typed closure",
                                Some(origin),
                            ),
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    };
                    let result_type = signature
                        .output
                        .values
                        .last()
                        .cloned()
                        .unwrap_or(super::types::Type::Unit);
                    let scheduler = Arc::clone(&self.cpu_fibers.scheduler);
                    let owner = self.cpu_fibers.owner;
                    let id = match scheduler.spawn_closure_owned(
                        module.clone(),
                        closure,
                        continuation.fuel,
                        owner,
                    ) {
                        Ok(id) => id,
                        Err(error) => {
                            return self.failed_from_drive(
                                effects,
                                VmDiagnostic::error(
                                    "E-FIBER-008",
                                    DiagnosticPhase::ResourceLimit,
                                    error.to_string(),
                                    Some(origin),
                                ),
                                event_journal,
                                effect_journal,
                                handler,
                            );
                        }
                    };
                    self.cpu_fibers.adopt_spawned(id);
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(
                        continuation,
                        vec![TypedValue::Task {
                            id: id.to_string(),
                            result_type,
                            kind: super::types::TaskKind::CpuFiber,
                        }],
                    )
                }
                VmStep::PollCpuFiber {
                    task,
                    origin,
                    continuation,
                } => {
                    let result_type = task.value_type();
                    let Type::Task(result_type) = result_type else {
                        return self.failed_from_drive(
                            effects,
                            VmDiagnostic::error(
                                "E-FIBER-009",
                                DiagnosticPhase::Interpretation,
                                "task-poll requires task<T>",
                                Some(origin),
                            ),
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    };
                    let value = match self.cpu_fiber_result(&task, &origin) {
                        Ok(Some(mut value)) => TypedValue::Option {
                            inner_type: (*result_type).clone(),
                            value: Some(Box::new(value.remove(0))),
                        },
                        Ok(None) => TypedValue::Option {
                            inner_type: (*result_type).clone(),
                            value: None,
                        },
                        Err(diagnostic) => {
                            return self.failed_from_drive(
                                effects,
                                diagnostic,
                                event_journal,
                                effect_journal,
                                handler,
                            )
                        }
                    };
                    let poll =
                        TypedValue::Record(vec![("task".into(), task), ("value".into(), value)]);
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![poll])
                }
                VmStep::JoinCpuFiber {
                    task,
                    origin,
                    continuation,
                } => match self.cpu_fiber_result(&task, &origin) {
                    Ok(Some(values)) => {
                        let trampoline = VmTrampoline::new(
                            &module,
                            &InterpreterConfig {
                                fuel: continuation.fuel,
                                grants: self.grants.clone(),
                            },
                        );
                        trampoline.resume(continuation, values)
                    }
                    Ok(None) => {
                        return self.suspended_cpu_fiber(
                            module,
                            effects,
                            continuation,
                            event_journal,
                            effect_journal,
                            PendingCpuFiber { task, origin },
                            handler,
                        );
                    }
                    Err(diagnostic) => {
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        )
                    }
                },
                VmStep::CancelCpuFiber {
                    task,
                    origin,
                    continuation,
                } => {
                    if let Err(diagnostic) = self.cancel_cpu_fiber(&task, &origin) {
                        return self.failed_from_drive(
                            effects,
                            diagnostic,
                            event_journal,
                            effect_journal,
                            handler,
                        );
                    }
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![TypedValue::Unit])
                }
                VmStep::Complete { stack } => {
                    return self.complete(module, stack, event_journal, effect_journal, handler)
                }
                VmStep::Failed(diagnostic) => {
                    return self.failed_from_drive(
                        effects,
                        diagnostic,
                        event_journal,
                        effect_journal,
                        handler,
                    )
                }
            };
        }
    }

    fn suspended<H: CapabilityHandler>(
        &self,
        module: VerifiedModule,
        effects: EffectSet,
        yielded_value: TypedValue,
        continuation: VmContinuation,
        event_journal: Vec<VmSideEffect>,
        effect_journal: Vec<EffectJournalEntry>,
        handler: &H,
    ) -> TypedExecution {
        TypedExecution {
            status: TypedExecutionStatus::Suspended,
            values: Vec::new(),
            output: handler.output(),
            output_chunks: handler.output_chunks(),
            side_effects: handler.side_effects(),
            vm_side_effects: event_journal.clone(),
            effect_journal: effect_journal.clone(),
            effects: effects.clone(),
            diagnostics: Vec::new(),
            suspension: Some(TypedSuspension {
                module,
                continuation,
                yielded_value: Some(yielded_value),
                effects,
                event_journal,
                effect_journal,
                producer_fibers: BTreeMap::new(),
                pending_cpu_fiber: None,
                pending_host_call: None,
            }),
        }
    }

    fn suspended_cpu_fiber<H: CapabilityHandler>(
        &self,
        module: VerifiedModule,
        effects: EffectSet,
        continuation: VmContinuation,
        event_journal: Vec<VmSideEffect>,
        effect_journal: Vec<EffectJournalEntry>,
        pending_cpu_fiber: PendingCpuFiber,
        handler: &H,
    ) -> TypedExecution {
        TypedExecution {
            status: TypedExecutionStatus::Suspended,
            values: Vec::new(),
            output: handler.output(),
            output_chunks: handler.output_chunks(),
            side_effects: handler.side_effects(),
            vm_side_effects: event_journal.clone(),
            effect_journal: effect_journal.clone(),
            effects: effects.clone(),
            diagnostics: Vec::new(),
            suspension: Some(TypedSuspension {
                module,
                continuation,
                yielded_value: None,
                effects,
                event_journal,
                effect_journal,
                producer_fibers: BTreeMap::new(),
                pending_cpu_fiber: Some(pending_cpu_fiber),
                pending_host_call: None,
            }),
        }
    }

    fn finish_producer_transaction(
        &mut self,
        mut execution: TypedExecution,
        baseline: BTreeMap<String, ProducerFiberRecord>,
    ) -> TypedExecution {
        if execution.status == TypedExecutionStatus::Completed {
            return execution;
        }
        let working = std::mem::replace(&mut self.producer_fibers, baseline);
        if let Some(suspension) = execution.suspension.as_mut() {
            suspension.producer_fibers = working;
        }
        execution
    }

    fn finish_cpu_task_transaction(
        &mut self,
        execution: TypedExecution,
        baseline: BTreeSet<Uuid>,
    ) -> TypedExecution {
        if execution.status == TypedExecutionStatus::Failed {
            self.cpu_fibers.replace_roots(baseline);
        }
        execution
    }

    fn spawn_producer_fiber(
        &mut self,
        module: VerifiedModule,
        closure: TypedValue,
        fuel: u64,
        origin: &SourceOrigin,
    ) -> Result<TypedValue, VmDiagnostic> {
        let TypedValue::Closure {
            function,
            captures,
            signature,
        } = closure
        else {
            return Err(VmDiagnostic::error(
                "E-FIBER-021",
                DiagnosticPhase::Interpretation,
                "defer requires a typed yielding closure",
                Some(origin.clone()),
            ));
        };
        let Some(suspension) = &signature.suspension else {
            return Err(VmDiagnostic::error(
                "E-FIBER-021",
                DiagnosticPhase::Interpretation,
                "defer requires a closure with a declared yield contract",
                Some(origin.clone()),
            ));
        };
        if !signature.input.values.is_empty() || !signature.effects.is_pure() {
            return Err(VmDiagnostic::error(
                "E-FIBER-022",
                DiagnosticPhase::Interpretation,
                "cooperative defer requires a pure zero-argument closure",
                Some(origin.clone()),
            ));
        }
        if *suspension.resume_type != Type::Unit {
            return Err(VmDiagnostic::error(
                "E-FIBER-023",
                DiagnosticPhase::Interpretation,
                "this runtime version supports only unit-resumed producer fibers",
                Some(origin.clone()),
            ));
        }
        for capture in &captures {
            ensure_checkpointable(capture)?;
            validate_fiber_handle(capture, &self.producer_fibers)?;
        }
        let result_type = signature
            .output
            .values
            .last()
            .cloned()
            .unwrap_or(Type::Unit);
        let yield_type = (*suspension.yield_type).clone();
        let trampoline = VmTrampoline::new(
            &module,
            &InterpreterConfig {
                fuel,
                grants: EffectSet::pure(),
            },
        );
        let continuation = trampoline.start_function(&function, captures, Vec::new())?;
        let id = Uuid::new_v4().to_string();
        self.producer_fibers.insert(
            id.clone(),
            ProducerFiberRecord {
                module,
                yield_type: yield_type.clone(),
                result_type: result_type.clone(),
                state: ProducerFiberState::Ready { continuation },
            },
        );
        Ok(TypedValue::Fiber {
            id,
            yield_type,
            result_type,
        })
    }

    fn advance_producer_fiber(
        &mut self,
        fiber: &TypedValue,
        join: bool,
        origin: &SourceOrigin,
    ) -> Result<TypedValue, VmDiagnostic> {
        let TypedValue::Fiber {
            id,
            yield_type,
            result_type,
        } = fiber
        else {
            return Err(VmDiagnostic::error(
                if join { "E-FIBER-025" } else { "E-FIBER-024" },
                DiagnosticPhase::Interpretation,
                "producer operation requires fiber<Y,R>",
                Some(origin.clone()),
            ));
        };
        let mut record = self.producer_fibers.get(id).cloned().ok_or_else(|| {
            VmDiagnostic::error(
                "E-FIBER-027",
                DiagnosticPhase::Interpretation,
                format!("unknown producer fiber {id}"),
                Some(origin.clone()),
            )
        })?;
        if &record.yield_type != yield_type || &record.result_type != result_type {
            return Err(VmDiagnostic::error(
                "E-FIBER-028",
                DiagnosticPhase::Interpretation,
                "fiber handle types do not match its runtime record",
                Some(origin.clone()),
            ));
        }
        let mut continuation = match record.state.clone() {
            ProducerFiberState::Ready { continuation } => continuation,
            ProducerFiberState::Completed { result } => {
                return Ok(if join {
                    result
                } else {
                    fiber_returned(yield_type.clone(), result_type.clone(), result)
                });
            }
            ProducerFiberState::Failed { diagnostic } => return Err(diagnostic),
            ProducerFiberState::Cancelled => {
                return Err(VmDiagnostic::error(
                    "E-FIBER-029",
                    DiagnosticPhase::Interpretation,
                    "producer fiber was cancelled",
                    Some(origin.clone()),
                ));
            }
        };

        loop {
            let trampoline = VmTrampoline::new(
                &record.module,
                &InterpreterConfig {
                    fuel: continuation.fuel,
                    grants: EffectSet::pure(),
                },
            );
            match trampoline.run(continuation) {
                VmStep::Yielded {
                    value,
                    continuation: next,
                } => {
                    let found = value.value_type();
                    if !yield_type.accepts(&found) {
                        let diagnostic = VmDiagnostic::type_mismatch(
                            yield_type.clone(),
                            found,
                            Some(origin.clone()),
                        );
                        record.state = ProducerFiberState::Failed {
                            diagnostic: diagnostic.clone(),
                        };
                        self.producer_fibers.insert(id.clone(), record);
                        return Err(diagnostic);
                    }
                    if join {
                        continuation = next;
                        continue;
                    }
                    record.state = ProducerFiberState::Ready { continuation: next };
                    self.producer_fibers.insert(id.clone(), record);
                    return Ok(fiber_yielded(
                        yield_type.clone(),
                        result_type.clone(),
                        value,
                    ));
                }
                VmStep::Complete { mut stack } => {
                    if stack.len() != 1 {
                        let diagnostic = VmDiagnostic::error(
                            "E-FIBER-030",
                            DiagnosticPhase::Interpretation,
                            format!(
                                "producer terminal frame returned {} values; expected one",
                                stack.len()
                            ),
                            Some(origin.clone()),
                        );
                        record.state = ProducerFiberState::Failed {
                            diagnostic: diagnostic.clone(),
                        };
                        self.producer_fibers.insert(id.clone(), record);
                        return Err(diagnostic);
                    }
                    let result = stack.remove(0);
                    let found = result.value_type();
                    if !result_type.accepts(&found) {
                        let diagnostic = VmDiagnostic::type_mismatch(
                            result_type.clone(),
                            found,
                            Some(origin.clone()),
                        );
                        record.state = ProducerFiberState::Failed {
                            diagnostic: diagnostic.clone(),
                        };
                        self.producer_fibers.insert(id.clone(), record);
                        return Err(diagnostic);
                    }
                    record.state = ProducerFiberState::Completed {
                        result: result.clone(),
                    };
                    self.producer_fibers.insert(id.clone(), record);
                    return Ok(if join {
                        result
                    } else {
                        fiber_returned(yield_type.clone(), result_type.clone(), result)
                    });
                }
                VmStep::Failed(diagnostic) => {
                    record.state = ProducerFiberState::Failed {
                        diagnostic: diagnostic.clone(),
                    };
                    self.producer_fibers.insert(id.clone(), record);
                    return Err(diagnostic);
                }
                step => {
                    let diagnostic = VmDiagnostic::error(
                        "E-FIBER-031",
                        DiagnosticPhase::HostCall,
                        format!(
                            "pure producer reached unsupported scheduler boundary {}",
                            producer_step_name(&step)
                        ),
                        Some(origin.clone()),
                    );
                    record.state = ProducerFiberState::Failed {
                        diagnostic: diagnostic.clone(),
                    };
                    self.producer_fibers.insert(id.clone(), record);
                    return Err(diagnostic);
                }
            }
        }
    }

    fn cancel_producer_fiber(
        &mut self,
        fiber: &TypedValue,
        origin: &SourceOrigin,
    ) -> Result<(), VmDiagnostic> {
        let TypedValue::Fiber { id, .. } = fiber else {
            return Err(VmDiagnostic::error(
                "E-FIBER-026",
                DiagnosticPhase::Interpretation,
                "fiber-cancel requires fiber<Y,R>",
                Some(origin.clone()),
            ));
        };
        let Some(record) = self.producer_fibers.get_mut(id) else {
            return Err(VmDiagnostic::error(
                "E-FIBER-027",
                DiagnosticPhase::Interpretation,
                format!("unknown producer fiber {id}"),
                Some(origin.clone()),
            ));
        };
        record.state = ProducerFiberState::Cancelled;
        Ok(())
    }

    /// Return `None` while the worker is running, or the single typed terminal
    /// value once it completes. CPU-deferred closures deliberately have one
    /// output so `task<T>` remains an unambiguous persistent stack value.
    fn cpu_fiber_result(
        &self,
        task: &TypedValue,
        origin: &SourceOrigin,
    ) -> Result<Option<Vec<TypedValue>>, VmDiagnostic> {
        let TypedValue::Task {
            id,
            result_type,
            kind: super::types::TaskKind::CpuFiber,
        } = task
        else {
            return Err(VmDiagnostic::error(
                "E-FIBER-011",
                DiagnosticPhase::HostCall,
                "CPU task operation requires a cpu_fiber task handle",
                Some(origin.clone()),
            ));
        };
        let id = uuid::Uuid::parse_str(id).map_err(|_| {
            VmDiagnostic::error(
                "E-FIBER-012",
                DiagnosticPhase::HostCall,
                "CPU task handle has an invalid identifier",
                Some(origin.clone()),
            )
        })?;
        let snapshot = self.cpu_fibers.scheduler.poll(id).map_err(|error| {
            VmDiagnostic::error(
                "E-FIBER-013",
                DiagnosticPhase::HostCall,
                error.to_string(),
                Some(origin.clone()),
            )
        })?;
        match snapshot.status {
            crate::runtime::fiber::CpuFiberStatus::Running => Ok(None),
            crate::runtime::fiber::CpuFiberStatus::Completed => {
                let values = snapshot.result.ok_or_else(|| {
                    VmDiagnostic::error(
                        "E-FIBER-014",
                        DiagnosticPhase::HostCall,
                        "completed CPU task has no result",
                        Some(origin.clone()),
                    )
                })?;
                let [value] = values.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-015",
                        DiagnosticPhase::HostCall,
                        "CPU task returned an invalid stack shape",
                        Some(origin.clone()),
                    ));
                };
                if !result_type.accepts(&value.value_type()) {
                    return Err(VmDiagnostic::type_mismatch(
                        result_type.clone(),
                        value.value_type(),
                        Some(origin.clone()),
                    ));
                }
                Ok(Some(values))
            }
            crate::runtime::fiber::CpuFiberStatus::Failed => {
                Err(snapshot.diagnostic.unwrap_or_else(|| {
                    VmDiagnostic::error(
                        "E-FIBER-016",
                        DiagnosticPhase::HostCall,
                        "CPU task failed without a diagnostic",
                        Some(origin.clone()),
                    )
                }))
            }
            crate::runtime::fiber::CpuFiberStatus::Cancelled => Err(VmDiagnostic::error(
                "E-FIBER-017",
                DiagnosticPhase::Cancellation,
                "CPU task was cancelled",
                Some(origin.clone()),
            )),
        }
    }

    fn cancel_cpu_fiber(
        &self,
        task: &TypedValue,
        origin: &SourceOrigin,
    ) -> Result<(), VmDiagnostic> {
        let TypedValue::Task {
            id,
            kind: super::types::TaskKind::CpuFiber,
            ..
        } = task
        else {
            return Err(VmDiagnostic::error(
                "E-FIBER-011",
                DiagnosticPhase::HostCall,
                "CPU task operation requires a cpu_fiber task handle",
                Some(origin.clone()),
            ));
        };
        let id = uuid::Uuid::parse_str(id).map_err(|_| {
            VmDiagnostic::error(
                "E-FIBER-012",
                DiagnosticPhase::HostCall,
                "CPU task handle has an invalid identifier",
                Some(origin.clone()),
            )
        })?;
        self.cpu_fibers.scheduler.cancel(id).map_err(|error| {
            VmDiagnostic::error(
                "E-FIBER-013",
                DiagnosticPhase::HostCall,
                error.to_string(),
                Some(origin.clone()),
            )
        })
    }

    fn complete<H: CapabilityHandler>(
        &mut self,
        module: VerifiedModule,
        stack: Vec<TypedValue>,
        vm_side_effects: Vec<VmSideEffect>,
        effect_journal: Vec<EffectJournalEntry>,
        handler: &H,
    ) -> TypedExecution {
        let effects = entry_effects(&module);
        let entry = module.module.entry.clone();
        for (name, function) in module.module.functions {
            if name != entry && !self.functions.contains_key(&name) {
                if !name.starts_with("lambda$") {
                    self.vocabulary
                        .insert(name.clone(), function.signature.clone());
                }
                self.functions.insert(name, function);
            }
        }
        self.stack = stack;
        self.collect_unreachable_producer_fibers();
        self.collect_reachable_cpu_fibers();
        TypedExecution {
            status: TypedExecutionStatus::Completed,
            values: self.stack.clone(),
            output: handler.output(),
            output_chunks: handler.output_chunks(),
            side_effects: handler.side_effects(),
            vm_side_effects,
            effect_journal,
            effects,
            diagnostics: Vec::new(),
            suspension: None,
        }
    }

    fn failed_from_drive<H: CapabilityHandler>(
        &self,
        effects: EffectSet,
        diagnostic: VmDiagnostic,
        vm_side_effects: Vec<VmSideEffect>,
        effect_journal: Vec<EffectJournalEntry>,
        handler: &H,
    ) -> TypedExecution {
        TypedExecution {
            status: TypedExecutionStatus::Failed,
            values: Vec::new(),
            output: handler.output(),
            output_chunks: handler.output_chunks(),
            side_effects: handler.side_effects(),
            vm_side_effects,
            effect_journal,
            effects,
            diagnostics: vec![diagnostic],
            suspension: None,
        }
    }
}

fn fiber_end_type(result_type: Type) -> Type {
    Type::Variant(vec![("end".into(), Some(result_type))])
}

fn fiber_yielded(yield_type: Type, result_type: Type, value: TypedValue) -> TypedValue {
    TypedValue::Result {
        ok_type: yield_type,
        error_type: fiber_end_type(result_type),
        is_ok: true,
        value: Box::new(value),
    }
}

fn fiber_returned(yield_type: Type, result_type: Type, value: TypedValue) -> TypedValue {
    let error_type = fiber_end_type(result_type);
    TypedValue::Result {
        ok_type: yield_type,
        error_type,
        is_ok: false,
        value: Box::new(TypedValue::Variant {
            name: "end".into(),
            value: Some(Box::new(value)),
        }),
    }
}

fn producer_step_name(step: &VmStep) -> &'static str {
    match step {
        VmStep::Yielded { .. } => "yield",
        VmStep::Emit { .. } => "emit",
        VmStep::Await { .. } => "await",
        VmStep::SpawnFiber { .. } => "defer",
        VmStep::NextFiber { .. } => "fiber-next",
        VmStep::JoinFiber { .. } => "fiber-join",
        VmStep::CancelFiber { .. } => "fiber-cancel",
        VmStep::SpawnCpuFiber { .. } => "defer-cpu",
        VmStep::PollCpuFiber { .. } => "task-poll",
        VmStep::JoinCpuFiber { .. } => "task-join",
        VmStep::CancelCpuFiber { .. } => "task-cancel",
        VmStep::Complete { .. } => "return",
        VmStep::Failed(_) => "failure",
    }
}

fn validate_producer_record(
    record: &ProducerFiberRecord,
    registry: &BTreeMap<String, ProducerFiberRecord>,
) -> Result<(), VmDiagnostic> {
    match &record.state {
        ProducerFiberState::Ready { continuation } => {
            for value in &continuation.stack {
                ensure_checkpointable(value)?;
                validate_fiber_handle(value, registry)?;
            }
            for frame in &continuation.frames {
                for value in frame.locals.iter().chain(&frame.captures) {
                    ensure_checkpointable(value)?;
                    validate_fiber_handle(value, registry)?;
                }
            }
        }
        ProducerFiberState::Completed { result } => {
            ensure_checkpointable(result)?;
            validate_fiber_handle(result, registry)?;
            let found = result.value_type();
            if !record.result_type.accepts(&found) {
                return Err(VmDiagnostic::type_mismatch(
                    record.result_type.clone(),
                    found,
                    None,
                ));
            }
        }
        ProducerFiberState::Failed { .. } | ProducerFiberState::Cancelled => {}
    }
    Ok(())
}

fn collect_producer_fiber_ids(value: &TypedValue, ids: &mut VecDeque<String>) {
    match value {
        TypedValue::Fiber { id, .. } => ids.push_back(id.clone()),
        TypedValue::List { values, .. } => {
            for value in values {
                collect_producer_fiber_ids(value, ids);
            }
        }
        TypedValue::Map { entries, .. } => {
            for (key, value) in entries {
                collect_producer_fiber_ids(key, ids);
                collect_producer_fiber_ids(value, ids);
            }
        }
        TypedValue::Option { value, .. } | TypedValue::Variant { value, .. } => {
            if let Some(value) = value {
                collect_producer_fiber_ids(value, ids);
            }
        }
        TypedValue::Result { value, .. } | TypedValue::Dynamic { value, .. } => {
            collect_producer_fiber_ids(value, ids);
        }
        TypedValue::Record(fields) => {
            for (_, value) in fields {
                collect_producer_fiber_ids(value, ids);
            }
        }
        TypedValue::Closure { captures, .. } => {
            for value in captures {
                collect_producer_fiber_ids(value, ids);
            }
        }
        TypedValue::Unit
        | TypedValue::Bool(_)
        | TypedValue::Int(_)
        | TypedValue::UInt(_)
        | TypedValue::Float(_)
        | TypedValue::Char(_)
        | TypedValue::Symbol(_)
        | TypedValue::String(_)
        | TypedValue::Bytes(_)
        | TypedValue::Json(_)
        | TypedValue::Path { .. }
        | TypedValue::Task { .. }
        | TypedValue::Stream { .. }
        | TypedValue::Resource { .. } => {}
    }
}

fn collect_cpu_fiber_ids(value: &TypedValue, ids: &mut BTreeSet<Uuid>) {
    match value {
        TypedValue::Task {
            id,
            kind: super::types::TaskKind::CpuFiber,
            ..
        } => {
            if let Ok(id) = Uuid::parse_str(id) {
                ids.insert(id);
            }
        }
        TypedValue::List { values, .. } => {
            for value in values {
                collect_cpu_fiber_ids(value, ids);
            }
        }
        TypedValue::Map { entries, .. } => {
            for (key, value) in entries {
                collect_cpu_fiber_ids(key, ids);
                collect_cpu_fiber_ids(value, ids);
            }
        }
        TypedValue::Option { value, .. } | TypedValue::Variant { value, .. } => {
            if let Some(value) = value {
                collect_cpu_fiber_ids(value, ids);
            }
        }
        TypedValue::Result { value, .. } | TypedValue::Dynamic { value, .. } => {
            collect_cpu_fiber_ids(value, ids);
        }
        TypedValue::Record(fields) => {
            for (_, value) in fields {
                collect_cpu_fiber_ids(value, ids);
            }
        }
        TypedValue::Closure { captures, .. } => {
            for value in captures {
                collect_cpu_fiber_ids(value, ids);
            }
        }
        TypedValue::Unit
        | TypedValue::Bool(_)
        | TypedValue::Int(_)
        | TypedValue::UInt(_)
        | TypedValue::Float(_)
        | TypedValue::Char(_)
        | TypedValue::Symbol(_)
        | TypedValue::String(_)
        | TypedValue::Bytes(_)
        | TypedValue::Json(_)
        | TypedValue::Path { .. }
        | TypedValue::Task {
            kind: super::types::TaskKind::Agent,
            ..
        }
        | TypedValue::Fiber { .. }
        | TypedValue::Stream { .. }
        | TypedValue::Resource { .. } => {}
    }
}

fn validate_fiber_handle(
    value: &TypedValue,
    registry: &BTreeMap<String, ProducerFiberRecord>,
) -> Result<(), VmDiagnostic> {
    match value {
        TypedValue::Fiber {
            id,
            yield_type,
            result_type,
        } => {
            let record = registry.get(id).ok_or_else(|| {
                VmDiagnostic::error(
                    "E-CHECKPOINT-006",
                    DiagnosticPhase::Linking,
                    format!("checkpoint contains an unknown producer fiber handle {id}"),
                    None,
                )
            })?;
            if &record.yield_type != yield_type || &record.result_type != result_type {
                return Err(VmDiagnostic::error(
                    "E-CHECKPOINT-007",
                    DiagnosticPhase::Linking,
                    format!("producer fiber handle {id} has inconsistent types"),
                    None,
                ));
            }
        }
        TypedValue::List { values, .. } => {
            for value in values {
                validate_fiber_handle(value, registry)?;
            }
        }
        TypedValue::Map { entries, .. } => {
            for (key, value) in entries {
                validate_fiber_handle(key, registry)?;
                validate_fiber_handle(value, registry)?;
            }
        }
        TypedValue::Option { value, .. } | TypedValue::Variant { value, .. } => {
            if let Some(value) = value {
                validate_fiber_handle(value, registry)?;
            }
        }
        TypedValue::Result { value, .. } | TypedValue::Dynamic { value, .. } => {
            validate_fiber_handle(value, registry)?;
        }
        TypedValue::Record(fields) => {
            for (_, value) in fields {
                validate_fiber_handle(value, registry)?;
            }
        }
        TypedValue::Closure { captures, .. } => {
            for value in captures {
                validate_fiber_handle(value, registry)?;
            }
        }
        TypedValue::Unit
        | TypedValue::Bool(_)
        | TypedValue::Int(_)
        | TypedValue::UInt(_)
        | TypedValue::Float(_)
        | TypedValue::Char(_)
        | TypedValue::Symbol(_)
        | TypedValue::String(_)
        | TypedValue::Bytes(_)
        | TypedValue::Json(_)
        | TypedValue::Path { .. }
        | TypedValue::Task { .. }
        | TypedValue::Stream { .. }
        | TypedValue::Resource { .. } => {}
    }
    Ok(())
}

fn ensure_checkpointable(value: &TypedValue) -> Result<(), VmDiagnostic> {
    match value {
        TypedValue::List { values, .. } => {
            for value in values {
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Map { entries, .. } => {
            for (key, value) in entries {
                ensure_checkpointable(key)?;
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Option { value, .. } => {
            if let Some(value) = value {
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Result { value, .. } | TypedValue::Dynamic { value, .. } => {
            ensure_checkpointable(value)?;
        }
        TypedValue::Record(fields) => {
            for (_, value) in fields {
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Variant { value, .. } => {
            if let Some(value) = value {
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Closure { captures, .. } => {
            for value in captures {
                ensure_checkpointable(value)?;
            }
        }
        TypedValue::Task { kind, .. } => {
            return Err(VmDiagnostic::error(
                "E-CHECKPOINT-002",
                DiagnosticPhase::HostCall,
                format!(
                    "cannot checkpoint a {kind:?} task handle before its scheduler record is restored"
                ),
                None,
            ));
        }
        TypedValue::Fiber { .. } => {}
        TypedValue::Stream { .. } | TypedValue::Resource { .. } => {
            return Err(VmDiagnostic::error(
                "E-CHECKPOINT-003",
                DiagnosticPhase::HostCall,
                "cannot checkpoint a host-owned stream or resource handle",
                None,
            ));
        }
        TypedValue::Unit
        | TypedValue::Bool(_)
        | TypedValue::Int(_)
        | TypedValue::UInt(_)
        | TypedValue::Float(_)
        | TypedValue::Char(_)
        | TypedValue::Symbol(_)
        | TypedValue::String(_)
        | TypedValue::Bytes(_)
        | TypedValue::Json(_)
        | TypedValue::Path { .. } => {}
    }
    Ok(())
}

fn entry_effects(module: &VerifiedModule) -> EffectSet {
    module
        .functions
        .get(&module.module.entry)
        .map(|function| function.inferred_effects.clone())
        .unwrap_or_default()
}

#[derive(Default)]
struct RuntimeCapabilities {
    output: String,
}

impl CapabilityHandler for RuntimeCapabilities {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        match requirement.capability {
            CapabilityKind::SessionEmit => {
                let [TypedValue::String(text)] = arguments.as_slice() else {
                    return Err(VmDiagnostic::error(
                        "E-HOST-001",
                        DiagnosticPhase::HostCall,
                        "session.emit requires one string",
                        Some(origin.clone()),
                    ));
                };
                if origin.word.as_deref() == Some("output-open") {
                    return Ok(vec![TypedValue::Resource {
                        kind: "output-handle".into(),
                        handle: "runtime-output".into(),
                        generation: 0,
                    }]);
                }
                self.output.push_str(text);
                Ok(vec![TypedValue::Unit])
            }
            _ => {
                let mut diagnostic = VmDiagnostic::error(
                    "E-HOST-002",
                    DiagnosticPhase::HostCall,
                    format!(
                        "capability {:?} is authorized but has no host binding",
                        requirement.capability
                    ),
                    Some(origin.clone()),
                );
                diagnostic.capability = Some(requirement.clone());
                Err(diagnostic)
            }
        }
    }

    fn output(&self) -> String {
        self.output.clone()
    }
}

impl TypedExecution {
    fn failed(diagnostics: Vec<VmDiagnostic>) -> Self {
        Self {
            status: TypedExecutionStatus::Failed,
            values: Vec::new(),
            output: String::new(),
            output_chunks: Vec::new(),
            side_effects: Vec::new(),
            vm_side_effects: Vec::new(),
            effect_journal: Vec::new(),
            effects: EffectSet::pure(),
            diagnostics,
            suspension: None,
        }
    }
}

fn acknowledge_last(journal: &mut [EffectJournalEntry], values: Vec<TypedValue>) {
    if let Some(entry) = journal.last_mut() {
        entry.state = EffectJournalState::Acknowledged { values };
    }
}

fn awaiting_last(journal: &mut [EffectJournalEntry]) {
    if let Some(entry) = journal.last_mut() {
        entry.state = EffectJournalState::AwaitingApproval;
    }
}

fn fail_last(journal: &mut [EffectJournalEntry], diagnostic: VmDiagnostic) {
    if let Some(entry) = journal.last_mut() {
        entry.state = EffectJournalState::Failed { diagnostic };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct LanguageConformanceSuite {
        version: u32,
        cases: Vec<LanguageConformanceCase>,
    }

    #[derive(Deserialize)]
    struct LanguageConformanceCase {
        name: String,
        forth: String,
        lisp: String,
        #[serde(default)]
        expected_values: Option<Vec<TypedValue>>,
        expected_output: String,
    }

    #[derive(Default)]
    struct RecordingHost {
        output: String,
        ui_events: Vec<VmSideEffect>,
    }

    impl CapabilityHandler for RecordingHost {
        fn request(
            &mut self,
            requirement: &CapabilityRequirement,
            arguments: Vec<TypedValue>,
            origin: &SourceOrigin,
        ) -> Result<Vec<TypedValue>, VmDiagnostic> {
            match requirement.capability {
                CapabilityKind::SessionEmit => {
                    let [TypedValue::String(text)] = arguments.as_slice() else {
                        return Err(VmDiagnostic::error(
                            "E-HOST-001",
                            DiagnosticPhase::HostCall,
                            "session.emit requires one string",
                            Some(origin.clone()),
                        ));
                    };
                    if origin.word.as_deref() == Some("output-open") {
                        return Ok(vec![TypedValue::Resource {
                            kind: "output-handle".into(),
                            handle: "test-output".into(),
                            generation: 0,
                        }]);
                    }
                    self.output.push_str(text);
                    Ok(vec![TypedValue::Unit])
                }
                CapabilityKind::FileRead => Ok(vec![TypedValue::Bytes(b"contents".to_vec())]),
                CapabilityKind::MemoryWrite => Ok(vec![TypedValue::Resource {
                    kind: "memory-node".into(),
                    handle: "test-memory-node".into(),
                    generation: 1,
                }]),
                _ => Err(VmDiagnostic::error(
                    "E-HOST-002",
                    DiagnosticPhase::HostCall,
                    "test host has no binding for this capability",
                    Some(origin.clone()),
                )),
            }
        }

        fn output(&self) -> String {
            self.output.clone()
        }

        fn side_effect(&mut self, effect: &VmSideEffect) -> Result<(), VmDiagnostic> {
            match &effect.event {
                HostSideEffect::Emit { text } => {
                    self.request(
                        &effect.requirement,
                        vec![TypedValue::String(text.clone())],
                        &effect.origin,
                    )?;
                }
                HostSideEffect::Ui { .. } => self.ui_events.push(effect.clone()),
                HostSideEffect::Request { .. } => {}
            }
            Ok(())
        }
    }

    #[derive(Default)]
    struct FailingFileHost {
        output: String,
    }

    impl CapabilityHandler for FailingFileHost {
        fn request(
            &mut self,
            requirement: &CapabilityRequirement,
            arguments: Vec<TypedValue>,
            origin: &SourceOrigin,
        ) -> Result<Vec<TypedValue>, VmDiagnostic> {
            match requirement.capability {
                CapabilityKind::SessionEmit => {
                    let [TypedValue::String(text)] = arguments.as_slice() else {
                        return Err(VmDiagnostic::error(
                            "E-HOST-001",
                            DiagnosticPhase::HostCall,
                            "session.emit requires one string",
                            Some(origin.clone()),
                        ));
                    };
                    self.output.push_str(text);
                    Ok(vec![TypedValue::Unit])
                }
                CapabilityKind::FileRead => Err(VmDiagnostic::error(
                    "E-HOST-TEST",
                    DiagnosticPhase::HostCall,
                    "test file binding failed after approval",
                    Some(origin.clone()),
                )),
                _ => Err(VmDiagnostic::error(
                    "E-HOST-002",
                    DiagnosticPhase::HostCall,
                    "test host has no binding for this capability",
                    Some(origin.clone()),
                )),
            }
        }

        fn output(&self) -> String {
            self.output.clone()
        }
    }

    #[test]
    fn lisp_say_compiles_directly_and_emits() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "model-response.lisp",
            "(say \"hello\")",
            1_000,
        );
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(result.output, "hello");
        assert!(result.values.is_empty());
    }

    #[test]
    fn explicit_output_handles_emit_independent_portable_ui_events() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let result = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "output.lisp",
            "(let ((handle (output-open \"download\")))
                (begin
                  (output-status handle \"starting\")
                  (output-progress handle 2 5)
                  (output-replace handle \"halfway\")
                  (output-complete handle)))",
            1_000,
            None,
            &mut host,
        );
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(host.ui_events.len(), 4);
        assert!(matches!(
            host.ui_events[0].event,
            HostSideEffect::Ui { operation: super::super::interpreter::UiOperation::Status, ref target, ref text, progress: None }
                if matches!(target, Some(TypedValue::Resource { kind, handle, .. }) if kind == "output-handle" && handle == "test-output")
                    && text.as_deref() == Some("starting")
        ));
        assert!(matches!(
            host.ui_events[1].event,
            HostSideEffect::Ui { operation: super::super::interpreter::UiOperation::Progress, progress: Some(ref progress), .. }
                if progress.completed == 2 && progress.total == Some(5)
        ));
        assert!(matches!(
            host.ui_events[3].event,
            HostSideEffect::Ui {
                operation: super::super::interpreter::UiOperation::Complete,
                text: None,
                progress: None,
                ..
            }
        ));

        let mut forth_runtime = TypedRuntime::new();
        let mut forth_host = RecordingHost::default();
        let forth = forth_runtime.execute_with_handler(
            ProgramLanguage::Forth,
            "output.forth",
            "s\"download\" output-open dup s\"starting\" output-append output-complete",
            1_000,
            None,
            &mut forth_host,
        );
        assert_eq!(forth.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            forth_host.ui_events.as_slice(),
            [
                VmSideEffect {
                    event: HostSideEffect::Ui {
                        operation: super::super::interpreter::UiOperation::Append,
                        ..
                    },
                    ..
                },
                VmSideEffect {
                    event: HostSideEffect::Ui {
                        operation: super::super::interpreter::UiOperation::Complete,
                        ..
                    },
                    ..
                },
            ]
        ));
    }

    #[test]
    fn core_language_conformance_fixtures_match_across_frontends() {
        let suite: LanguageConformanceSuite = serde_json::from_str(include_str!(
            "../../vocabulary/language/conformance/core.json"
        ))
        .expect("language conformance fixtures must be valid JSON");
        assert_eq!(suite.version, 1, "unsupported fixture version");
        assert!(
            !suite.cases.is_empty(),
            "conformance suite must exercise at least one program"
        );

        for case in suite.cases {
            let mut forth = TypedRuntime::new();
            let forth_result = forth.execute(
                ProgramLanguage::Forth,
                &format!("conformance/{}.forth", case.name),
                &case.forth,
                1_000,
            );
            let mut lisp = TypedRuntime::new();
            let lisp_result = lisp.execute(
                ProgramLanguage::Lisp,
                &format!("conformance/{}.lisp", case.name),
                &case.lisp,
                1_000,
            );

            assert_eq!(
                forth_result.status,
                TypedExecutionStatus::Completed,
                "Forth case '{}' failed: {:?}",
                case.name,
                forth_result.diagnostics
            );
            assert_eq!(
                lisp_result.status,
                TypedExecutionStatus::Completed,
                "Lisp case '{}' failed: {:?}",
                case.name,
                lisp_result.diagnostics
            );
            assert_eq!(
                forth_result.values, lisp_result.values,
                "frontends disagree on values for '{}'",
                case.name
            );
            assert_eq!(
                forth_result.output, lisp_result.output,
                "frontends disagree on output for '{}'",
                case.name
            );
            assert_eq!(
                forth_result.output, case.expected_output,
                "case '{}'",
                case.name
            );
            if let Some(expected_values) = case.expected_values {
                assert_eq!(forth_result.values, expected_values, "case '{}'", case.name);
            }
        }
    }

    #[test]
    fn executable_lisp_script_envelope_enters_the_shared_typed_runtime() {
        let script = crate::programs::parse_finch_script(
            std::path::Path::new("reply.lisp"),
            "#!/usr/local/finch --exec --language=lisp\n; a script comment\n(begin (say \"hello from script\"))\n",
        )
        .unwrap();
        assert_eq!(script.language, ProgramLanguage::Lisp);

        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(script.language, "reply.lisp", &script.source, 1_000);
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(result.output, "hello from script");
        assert!(result.values.is_empty());
    }

    #[test]
    fn approval_resumes_the_verified_frame_after_prior_output() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let pending = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "model-response.lisp",
            "(begin (say \"checking...\") (file-read (path \"Cargo.toml\")))",
            1_000,
            None,
            &mut host,
        );
        assert!(matches!(
            pending.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        assert_eq!(pending.output, "checking...");
        assert!(matches!(
            pending.effect_journal.as_slice(),
            [
                EffectJournalEntry {
                    state: EffectJournalState::Acknowledged { values },
                    ..
                },
                EffectJournalEntry {
                    state: EffectJournalState::AwaitingApproval,
                    ..
                },
            ] if values.is_empty()
        ));
        let suspension = pending
            .suspension
            .expect("authorization must retain the VM frame");
        let suspension: TypedSuspension = serde_json::from_str(
            &serde_json::to_string(&suspension).expect("suspension must serialize"),
        )
        .expect("suspension must deserialize");

        let still_pending = runtime.resume_with_handler(suspension, Vec::new(), &mut host);
        assert!(matches!(
            still_pending.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        let suspension = still_pending
            .suspension
            .expect("an ungranted resume must retain the host call");

        runtime.grant(CapabilityRequirement::file(
            super::super::effects::FileOperation::Read,
            super::super::effects::FileSelector::parse("./**").unwrap(),
        ));
        let completed = runtime.resume_with_handler(suspension, Vec::new(), &mut host);
        assert_eq!(completed.status, TypedExecutionStatus::Completed);
        assert_eq!(completed.output, "checking...");
        assert_eq!(
            completed.values,
            vec![TypedValue::Bytes(b"contents".to_vec())]
        );
        assert!(matches!(
            completed.effect_journal.as_slice(),
            [
                EffectJournalEntry {
                    state: EffectJournalState::Acknowledged { .. },
                    ..
                },
                EffectJournalEntry {
                    state: EffectJournalState::Acknowledged { values },
                    ..
                },
            ] if values == &vec![TypedValue::Bytes(b"contents".to_vec())]
        ));
    }

    #[test]
    fn exact_authorization_resumes_only_the_pending_host_call() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let first = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "allow-once.lisp",
            "(begin (file-read (path \"Cargo.toml\")) (file-read (path \"Cargo.lock\")))",
            1_000,
            None,
            &mut host,
        );
        assert!(matches!(
            first.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));

        let second = runtime.resume_authorized_host_call_with_handler(
            first.suspension.expect("first file read must suspend"),
            &mut host,
        );
        assert!(matches!(
            second.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        assert_eq!(second.effect_journal.len(), 2);
        assert!(matches!(
            second.effect_journal[0].state,
            EffectJournalState::Acknowledged { .. }
        ));
        assert!(matches!(
            second.effect_journal[1].state,
            EffectJournalState::AwaitingApproval
        ));
        assert!(runtime
            .grants()
            .0
            .iter()
            .all(|requirement| { requirement.capability != CapabilityKind::FileRead }));
    }

    #[test]
    fn external_effect_result_resumes_the_exact_journaled_effect_without_redispatch() {
        let mut runtime = TypedRuntime::new();
        // This binding deliberately fails file reads. A successful result
        // therefore proves that `VmResume` did not invoke it again.
        let mut host = FailingFileHost::default();
        let pending = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "external-result.lisp",
            "(file-read (path \"Cargo.toml\"))",
            1_000,
            None,
            &mut host,
        );
        let suspension = pending
            .suspension
            .expect("ungranted file read must retain the VM frame");
        let sequence = suspension
            .event_journal
            .last()
            .expect("file read must be journaled")
            .sequence;
        assert_eq!(
            suspension.event_journal.last().unwrap().output,
            vec![Type::Bytes]
        );
        runtime.grant(CapabilityRequirement::file(
            super::super::effects::FileOperation::Read,
            super::super::effects::FileSelector::parse("./**").unwrap(),
        ));

        let complete = runtime.resume_with_effect_result(
            suspension,
            sequence,
            vec![TypedValue::Bytes(b"provided by host event loop".to_vec())],
            &mut host,
        );
        assert_eq!(complete.status, TypedExecutionStatus::Completed);
        assert_eq!(
            complete.values,
            vec![TypedValue::Bytes(b"provided by host event loop".to_vec())]
        );
        assert!(matches!(
            complete.effect_journal.last(),
            Some(EffectJournalEntry {
                state: EffectJournalState::Acknowledged { values },
                ..
            }) if values == &vec![TypedValue::Bytes(b"provided by host event loop".to_vec())]
        ));
    }

    #[test]
    fn external_effect_result_rejects_wrong_sequence_and_result_arity() {
        let mut runtime = TypedRuntime::new();
        let mut host = FailingFileHost::default();
        let pending = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "external-result-errors.lisp",
            "(file-read (path \"Cargo.toml\"))",
            1_000,
            None,
            &mut host,
        );
        let suspension = pending.suspension.expect("file read must suspend");
        let sequence = suspension.event_journal.last().unwrap().sequence;
        runtime.grant(CapabilityRequirement::file(
            super::super::effects::FileOperation::Read,
            super::super::effects::FileSelector::parse("./**").unwrap(),
        ));

        let stale = runtime.resume_with_effect_result(
            suspension.clone(),
            sequence + 1,
            vec![TypedValue::Bytes(Vec::new())],
            &mut host,
        );
        assert_eq!(stale.status, TypedExecutionStatus::Failed);
        assert_eq!(stale.diagnostics[0].code, "E-RESUME-005");

        let wrong_arity =
            runtime.resume_with_effect_result(suspension, sequence, Vec::new(), &mut host);
        assert_eq!(wrong_arity.status, TypedExecutionStatus::Failed);
        assert_eq!(wrong_arity.diagnostics[0].code, "E-RESUME-003");
    }

    #[test]
    fn definition_does_not_commit_before_its_capability_boundary_completes() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let pending = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "definition-approval.lisp",
            "(define (record (text : string)) (mem-store text)) (record \"memo\")",
            1_000,
            None,
            &mut host,
        );
        assert!(matches!(
            pending.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        assert!(!runtime.functions().contains_key("record"));
        assert!(!runtime.vocabulary().contains_key("record"));

        runtime.grant(CapabilityRequirement {
            capability: CapabilityKind::MemoryWrite,
            selector: super::super::effects::ResourceSelector::Memory {
                tree: "session".into(),
                path: "**".into(),
            },
        });
        let completed = runtime.resume_with_handler(
            pending
                .suspension
                .expect("authorization must retain the module"),
            Vec::new(),
            &mut host,
        );
        assert_eq!(completed.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("record"));
        assert!(runtime.vocabulary().contains_key("record"));
    }

    #[test]
    fn resume_failure_preserves_the_acknowledged_effect_journal_prefix() {
        let mut runtime = TypedRuntime::new();
        let mut host = FailingFileHost::default();
        let pending = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "resume-failure.lisp",
            "(begin (say \"before approval\") (file-read (path \"Cargo.toml\")))",
            1_000,
            None,
            &mut host,
        );
        assert!(matches!(
            pending.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        let suspension = pending.suspension.expect("file read must suspend");
        runtime.grant(CapabilityRequirement::file(
            super::super::effects::FileOperation::Read,
            super::super::effects::FileSelector::parse("./**").unwrap(),
        ));

        let failed = runtime.resume_with_handler(suspension, Vec::new(), &mut host);
        assert_eq!(failed.status, TypedExecutionStatus::Failed);
        assert_eq!(failed.output, "before approval");
        assert_eq!(failed.vm_side_effects.len(), 2);
        assert!(matches!(
            failed.vm_side_effects[0].event,
            HostSideEffect::Emit { ref text } if text == "before approval"
        ));
        assert!(matches!(
            failed.vm_side_effects[1].event,
            HostSideEffect::Request { .. }
        ));
        assert!(matches!(
            failed.effect_journal.as_slice(),
            [
                EffectJournalEntry {
                    state: EffectJournalState::Acknowledged { values },
                    ..
                },
                EffectJournalEntry {
                    state: EffectJournalState::Failed { .. },
                    ..
                },
            ] if values.is_empty()
        ));
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn yield_returns_a_resumable_vm_boundary() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let yielded = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "model-response.lisp",
            "(begin (say \"before\") (yield) (say \"after\"))",
            1_000,
            None,
            &mut host,
        );
        assert_eq!(yielded.status, TypedExecutionStatus::Suspended);
        assert_eq!(yielded.output, "before");
        let completed = runtime.resume_with_handler(
            yielded.suspension.expect("yield must retain VM state"),
            Vec::new(),
            &mut host,
        );
        assert_eq!(completed.status, TypedExecutionStatus::Completed);
        assert_eq!(completed.output, "beforeafter");
        assert!(completed.values.is_empty());
    }

    #[test]
    fn emitted_events_have_stable_serializable_sequence_across_resume() {
        let mut runtime = TypedRuntime::new();
        let mut host = RecordingHost::default();
        let yielded = runtime.execute_with_handler(
            ProgramLanguage::Lisp,
            "events.lisp",
            "(begin (say \"first\") (yield) (say \"second\"))",
            1_000,
            None,
            &mut host,
        );
        assert_eq!(yielded.vm_side_effects.len(), 1);
        assert_eq!(yielded.vm_side_effects[0].sequence, 0);
        assert!(matches!(
            yielded.vm_side_effects[0].event,
            HostSideEffect::Emit { ref text } if text == "first"
        ));
        let suspension: TypedSuspension = serde_json::from_str(
            &serde_json::to_string(yielded.suspension.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        let complete = runtime.resume_with_handler(suspension, Vec::new(), &mut host);
        assert_eq!(complete.vm_side_effects.len(), 2);
        assert_eq!(complete.vm_side_effects[0].sequence, 0);
        assert_eq!(complete.vm_side_effects[1].sequence, 1);
        assert!(matches!(
            complete.vm_side_effects[1].event,
            HostSideEffect::Emit { ref text } if text == "second"
        ));
    }

    #[test]
    fn lisp_and_forth_share_one_typed_stack() {
        let mut runtime = TypedRuntime::new();
        let lisp = runtime.execute(ProgramLanguage::Lisp, "a.lisp", "(+ 2 3)", 1_000);
        assert_eq!(lisp.values, vec![TypedValue::Int(5)]);
        let forth = runtime.execute(ProgramLanguage::Forth, "b.forth", "2 *", 1_000);
        assert_eq!(forth.values, vec![TypedValue::Int(10)]);
        assert_eq!(runtime.stack(), &[TypedValue::Int(10)]);
    }

    #[test]
    fn failure_preserves_acknowledged_output_events_but_rolls_back_vm_stack() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Forth,
            "partial-effect.forth",
            "s\" visible before failure\" say 0 0 /",
            1_000,
        );
        assert_eq!(execution.status, TypedExecutionStatus::Failed);
        assert_eq!(execution.output, "visible before failure");
        assert_eq!(execution.vm_side_effects.len(), 1);
        assert!(matches!(
            execution.vm_side_effects[0].event,
            HostSideEffect::Emit { ref text } if text == "visible before failure"
        ));
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn response_effects_do_not_leave_synthetic_units_on_the_shared_stack() {
        for (language, source) in [
            (ProgramLanguage::Forth, "s\"first\" say s\"second\" say"),
            (
                ProgramLanguage::Lisp,
                "(begin (say \"first\") (say \"second\"))",
            ),
        ] {
            let mut runtime = TypedRuntime::new();
            let execution = runtime.execute(language, "output", source, 1_000);
            assert_eq!(execution.status, TypedExecutionStatus::Completed);
            assert_eq!(execution.output, "firstsecond");
            assert!(execution.values.is_empty());
            assert!(runtime.stack().is_empty());
        }
    }

    #[test]
    fn explicit_cpu_defer_runs_a_captured_lisp_closure_on_a_private_worker_stack() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Lisp,
            "defer.lisp",
            "(let ((value 21)) (defer :cpu (lambda () (* value 2))))",
            1_000,
        );
        assert_eq!(execution.status, TypedExecutionStatus::Completed);
        let [TypedValue::Task {
            id,
            result_type,
            kind,
        }] = execution.values.as_slice()
        else {
            panic!("defer :cpu must leave exactly one typed task handle");
        };
        assert_eq!(*result_type, crate::vm::types::Type::Int);
        assert_eq!(*kind, crate::vm::types::TaskKind::CpuFiber);
        let id = uuid::Uuid::parse_str(id).expect("CPU task id must be a UUID");
        let result = runtime.cpu_fibers.scheduler.join(id).unwrap();
        assert_eq!(
            result.status,
            crate::runtime::fiber::CpuFiberStatus::Completed
        );
        assert_eq!(result.result, Some(vec![TypedValue::Int(42)]));
    }

    #[test]
    fn cooperative_fiber_yields_repeatedly_then_returns() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Lisp,
            "producer.lisp",
            "(let ((fiber (defer (lambda () (begin (yield 3) (yield 5) 8))))) \
             (list (fiber-next fiber) (fiber-next fiber) (fiber-next fiber)))",
            5_000,
        );
        assert_eq!(
            execution.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            execution.diagnostics
        );
        let [TypedValue::List { values, .. }] = execution.values.as_slice() else {
            panic!("three producer steps must be returned as one typed list");
        };
        assert!(matches!(
            values.as_slice(),
            [
                TypedValue::Result { is_ok: true, value: first, .. },
                TypedValue::Result { is_ok: true, value: second, .. },
                TypedValue::Result { is_ok: false, value: end, .. },
            ] if **first == TypedValue::Int(3)
                && **second == TypedValue::Int(5)
                && matches!(&**end, TypedValue::Variant { name, value: Some(value) }
                    if name == "end" && **value == TypedValue::Int(8))
        ));
    }

    #[test]
    fn cooperative_fiber_join_discards_yields_and_returns_terminal_value() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Lisp,
            "join.lisp",
            "(fiber-join (defer (lambda () (begin (yield 1) (yield 2) 42))))",
            5_000,
        );
        assert_eq!(execution.status, TypedExecutionStatus::Completed);
        assert_eq!(execution.values, vec![TypedValue::Int(42)]);
        assert!(runtime.checkpoint().unwrap().producer_fibers.is_empty());
    }

    #[test]
    fn producer_tombstone_lives_until_the_last_duplicate_handle_is_dropped() {
        let mut runtime = TypedRuntime::new();
        let joined = runtime.execute(
            ProgramLanguage::Forth,
            "duplicate-producer.forth",
            ": producer ( S -- S int ! infer ) 1 yield 42 ; \
             ['] producer defer dup fiber-join",
            5_000,
        );
        assert_eq!(
            joined.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            joined.diagnostics
        );
        assert!(matches!(
            joined.values.as_slice(),
            [TypedValue::Fiber { .. }, TypedValue::Int(42)]
        ));
        assert_eq!(runtime.checkpoint().unwrap().producer_fibers.len(), 1);

        let dropped = runtime.execute(
            ProgramLanguage::Forth,
            "drop-producer.forth",
            "swap drop",
            1_000,
        );
        assert_eq!(dropped.status, TypedExecutionStatus::Completed);
        assert_eq!(dropped.values, vec![TypedValue::Int(42)]);
        assert!(runtime.checkpoint().unwrap().producer_fibers.is_empty());
    }

    #[test]
    fn producer_collection_traces_handles_captured_by_live_continuations() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "nested-producers.lisp",
            "(let ((inner (defer (lambda () (begin (yield 1) 7))))) \
               (defer (lambda () (begin (yield 2) (fiber-join inner)))))",
            5_000,
        );
        assert_eq!(
            deferred.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            deferred.diagnostics
        );
        assert!(matches!(
            deferred.values.as_slice(),
            [TypedValue::Fiber { .. }]
        ));
        assert_eq!(runtime.checkpoint().unwrap().producer_fibers.len(), 2);

        let dropped = runtime.execute(ProgramLanguage::Forth, "drop-outer.forth", "drop", 5_000);
        assert_eq!(
            dropped.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            dropped.diagnostics
        );
        assert!(dropped.values.is_empty());
        assert!(runtime.checkpoint().unwrap().producer_fibers.is_empty());
    }

    #[test]
    fn cooperative_fiber_handle_and_continuation_survive_checkpoint() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "persist.lisp",
            "(defer (lambda () (begin (yield 7) 9)))",
            5_000,
        );
        assert_eq!(deferred.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            deferred.values.as_slice(),
            [TypedValue::Fiber { .. }]
        ));

        let checkpoint = runtime.checkpoint().expect("producer is VM-checkpointable");
        let mut restored = TypedRuntime::from_checkpoint(checkpoint)
            .expect("producer module and continuation reverify");
        let next = restored.execute(
            ProgramLanguage::Forth,
            "next.forth",
            "dup fiber-next",
            5_000,
        );
        assert_eq!(next.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            next.values.as_slice(),
            [TypedValue::Fiber { .. }, TypedValue::Result { is_ok: true, value, .. }]
                if **value == TypedValue::Int(7)
        ));
    }

    #[test]
    fn coforth_can_defer_and_advance_the_same_producer_protocol() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Forth,
            "producer.forth",
            ": producer ( S -- S int ! infer ) 4 yield 6 ; \
             ['] producer defer dup fiber-next",
            5_000,
        );
        assert_eq!(
            execution.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            execution.diagnostics
        );
        assert!(matches!(
            execution.values.as_slice(),
            [TypedValue::Fiber { .. }, TypedValue::Result { is_ok: true, value, .. }]
                if **value == TypedValue::Int(4)
        ));
    }

    #[test]
    fn failed_program_rolls_back_a_producer_advance() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "rollback.lisp",
            "(defer (lambda () (begin (yield 7) 9)))",
            5_000,
        );
        assert_eq!(deferred.status, TypedExecutionStatus::Completed);

        let failed = runtime.execute(
            ProgramLanguage::Forth,
            "failed-next.forth",
            "dup fiber-next drop 1 0 /",
            5_000,
        );
        assert_eq!(failed.status, TypedExecutionStatus::Failed);
        assert!(matches!(runtime.stack(), [TypedValue::Fiber { .. }]));

        let retried = runtime.execute(
            ProgramLanguage::Forth,
            "retry-next.forth",
            "dup fiber-next",
            5_000,
        );
        assert_eq!(retried.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            retried.values.as_slice(),
            [TypedValue::Fiber { .. }, TypedValue::Result { is_ok: true, value, .. }]
                if **value == TypedValue::Int(7)
        ));
    }

    #[test]
    fn cancelled_producer_reports_a_stable_error_on_later_use() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "cancel.lisp",
            "(defer (lambda () (begin (yield 7) 9)))",
            5_000,
        );
        assert_eq!(deferred.status, TypedExecutionStatus::Completed);

        let cancelled = runtime.execute(
            ProgramLanguage::Forth,
            "cancel.forth",
            "dup fiber-cancel drop",
            5_000,
        );
        assert_eq!(cancelled.status, TypedExecutionStatus::Completed);
        assert!(matches!(runtime.stack(), [TypedValue::Fiber { .. }]));

        let next = runtime.execute(
            ProgramLanguage::Forth,
            "cancelled-next.forth",
            "dup fiber-next",
            5_000,
        );
        assert_eq!(next.status, TypedExecutionStatus::Failed);
        assert_eq!(next.diagnostics[0].code, "E-FIBER-029");
    }

    #[test]
    fn outer_suspension_carries_uncommitted_producer_state() {
        let mut runtime = TypedRuntime::new();
        let suspended = runtime.execute(
            ProgramLanguage::Lisp,
            "outer-yield.lisp",
            "(let ((fiber (defer (lambda () (begin (yield 11) 13))))) \
             (begin (yield) fiber))",
            5_000,
        );
        assert_eq!(suspended.status, TypedExecutionStatus::Suspended);
        assert!(runtime.stack().is_empty());
        assert!(runtime.producer_fibers.is_empty());
        let suspension = suspended.suspension.expect("saved outer continuation");
        assert_eq!(suspension.producer_fibers.len(), 1);

        let mut host = RuntimeCapabilities::default();
        let resumed = runtime.resume_with_handler(suspension, Vec::new(), &mut host);
        assert_eq!(resumed.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            resumed.values.as_slice(),
            [TypedValue::Fiber { .. }]
        ));
        assert_eq!(runtime.producer_fibers.len(), 1);

        let next = runtime.execute(
            ProgramLanguage::Forth,
            "outer-next.forth",
            "dup fiber-next",
            5_000,
        );
        assert_eq!(next.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            next.values.as_slice(),
            [TypedValue::Fiber { .. }, TypedValue::Result { is_ok: true, value, .. }]
                if **value == TypedValue::Int(11)
        ));
    }

    #[test]
    fn checkpoint_rejects_a_forged_producer_handle() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Lisp,
            "forged.lisp",
            "(defer (lambda () (begin (yield 1) 2)))",
            5_000,
        );
        assert_eq!(execution.status, TypedExecutionStatus::Completed);
        let mut checkpoint = runtime.checkpoint().unwrap();
        let [TypedValue::Fiber { id, .. }] = checkpoint.stack.as_mut_slice() else {
            panic!("checkpoint contains one producer handle");
        };
        *id = Uuid::new_v4().to_string();
        let errors = match TypedRuntime::from_checkpoint(checkpoint) {
            Ok(_) => panic!("opaque fiber IDs must have matching serialized records"),
            Err(errors) => errors,
        };
        assert_eq!(errors[0].code, "E-CHECKPOINT-006");
    }

    #[test]
    fn cpu_task_handle_survives_a_later_turn_and_joins_without_sharing_stacks() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "defer.lisp",
            "(let ((value 21)) (defer :cpu (lambda () (* value 2))))",
            1_000,
        );
        let [TypedValue::Task { id, kind, .. }] = deferred.values.as_slice() else {
            panic!("defer :cpu must leave a task handle on the persistent stack");
        };
        assert_eq!(*kind, crate::vm::types::TaskKind::CpuFiber);
        let id = uuid::Uuid::parse_str(id).unwrap();
        runtime.cpu_fibers.scheduler.join(id).unwrap();
        let concurrent_snapshot = runtime.clone();

        let joined = runtime.execute(ProgramLanguage::Forth, "join.forth", "task-join", 1_000);
        assert_eq!(joined.status, TypedExecutionStatus::Completed);
        assert_eq!(joined.values, vec![TypedValue::Int(42)]);
        assert_eq!(runtime.stack(), &[TypedValue::Int(42)]);
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 1);
        drop(concurrent_snapshot);
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 0);
    }

    #[test]
    fn completed_cpu_task_poll_preserves_handle_and_typed_result() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "defer.lisp",
            "(defer :cpu (lambda () 9))",
            1_000,
        );
        let [TypedValue::Task { id, .. }] = deferred.values.as_slice() else {
            panic!("defer :cpu must leave a task handle");
        };
        runtime
            .cpu_fibers
            .scheduler
            .join(uuid::Uuid::parse_str(id).unwrap())
            .unwrap();
        let polled = runtime.execute(ProgramLanguage::Forth, "poll.forth", "task-poll", 1_000);
        assert_eq!(polled.status, TypedExecutionStatus::Completed);
        assert_eq!(
            polled.values,
            vec![TypedValue::Record(vec![
                (
                    "task".into(),
                    TypedValue::Task {
                        id: id.clone(),
                        result_type: Type::Int,
                        kind: crate::vm::types::TaskKind::CpuFiber,
                    },
                ),
                (
                    "value".into(),
                    TypedValue::Option {
                        inner_type: Type::Int,
                        value: Some(Box::new(TypedValue::Int(9))),
                    },
                ),
            ])]
        );
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 1);

        let joined = runtime.execute(
            ProgramLanguage::Forth,
            "poll-join.forth",
            "\"task\" record-get unwrap task-join",
            1_000,
        );
        assert_eq!(joined.status, TypedExecutionStatus::Completed);
        assert_eq!(joined.values, vec![TypedValue::Int(9)]);
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 0);
    }

    #[test]
    fn cancelling_a_cpu_task_consumes_its_handle_without_blocking_the_run() {
        let mut runtime = TypedRuntime::new();
        let cancelled = runtime.execute(
            ProgramLanguage::Lisp,
            "cancel.lisp",
            "(let ((task (defer :cpu (lambda () (begin (yield) 9))))) (task-cancel task))",
            1_000,
        );
        assert_eq!(cancelled.status, TypedExecutionStatus::Completed);
        assert!(cancelled.values.is_empty());
        assert!(runtime.stack().is_empty());
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 0);
    }

    #[test]
    fn failed_program_releases_cpu_tasks_spawned_only_in_its_working_state() {
        let mut runtime = TypedRuntime::new();
        let failed = runtime.execute(
            ProgramLanguage::Lisp,
            "failed-cpu-task.lisp",
            "(begin (defer :cpu (lambda () 9)) (/ 1 0))",
            1_000,
        );
        assert_eq!(failed.status, TypedExecutionStatus::Failed);
        assert!(runtime.stack().is_empty());
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 0);
    }

    #[test]
    fn cpu_task_leases_trace_duplicate_handles_nested_in_typed_values() {
        let mut runtime = TypedRuntime::new();
        let deferred = runtime.execute(
            ProgramLanguage::Lisp,
            "nested-cpu-task.lisp",
            "(let ((task (defer :cpu (lambda () 9)))) (list task task))",
            1_000,
        );
        assert_eq!(
            deferred.status,
            TypedExecutionStatus::Completed,
            "{:?}",
            deferred.diagnostics
        );
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 1);

        let dropped = runtime.execute(ProgramLanguage::Forth, "drop-task-list.forth", "drop", 10);
        assert_eq!(dropped.status, TypedExecutionStatus::Completed);
        assert_eq!(runtime.cpu_fibers.scheduler.retained_count(), 0);
    }

    #[test]
    fn forth_definition_persists_and_is_callable_from_lisp() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": square ( S int -- S int ! pure ) dup * ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("square"));
        let call = runtime.execute(ProgramLanguage::Lisp, "call.lisp", "(square 9)", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(81)]);
    }

    #[test]
    fn rejected_definition_does_not_enter_dictionary() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": dishonest ( S string -- S ! pure ) say ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Failed);
        assert_eq!(definition.diagnostics[0].code, "E-CAP-001");
        assert!(!runtime.functions().contains_key("dishonest"));
    }

    #[test]
    fn lisp_definition_persists_and_is_callable_from_forth() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "words.lisp",
            "(define (triple (x : int)) (* x 3))",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("triple"));
        let call = runtime.execute(ProgramLanguage::Forth, "call.forth", "7 triple", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(21)]);
    }

    #[test]
    fn recursive_lisp_definition_persists_for_later_program_runs() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "words.lisp",
            "(define (factorial (n : int)) : int \
               (if (<= n 1) 1 (* n (factorial (- n 1)))))",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("factorial"));

        let call = runtime.execute(ProgramLanguage::Forth, "call.forth", "6 factorial", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(720)]);
    }

    #[test]
    fn pure_mutually_recursive_lisp_functions_are_visible_during_compilation() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "words.lisp",
            "(define (even? (n : int)) : bool \
               (if (= n 0) true (odd? (- n 1)))) \
             (define (odd? (n : int)) : bool \
               (if (= n 0) false (even? (- n 1))))",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        let call = runtime.execute(ProgramLanguage::Forth, "call.forth", "42 even?", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Bool(true)]);
    }

    #[test]
    fn recursive_forth_definition_persists_for_later_program_runs() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": factorial ( S n:int -- S int ! pure ) \
               n 1 <= if 1 else n n 1 - factorial * then ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        assert!(runtime.functions().contains_key("factorial"));

        let call = runtime.execute(ProgramLanguage::Lisp, "call.lisp", "(factorial 6)", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Int(720)]);
    }

    #[test]
    fn pure_mutually_recursive_forth_words_are_visible_during_compilation() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": even? ( S n:int -- S bool ! pure ) \
               n 0 = if true else n 1 - odd? then ; \
             : odd? ( S n:int -- S bool ! pure ) \
               n 0 = if false else n 1 - even? then ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        let call = runtime.execute(ProgramLanguage::Lisp, "call.lisp", "(even? 42)", 1_000);
        assert_eq!(call.status, TypedExecutionStatus::Completed);
        assert_eq!(call.values, vec![TypedValue::Bool(true)]);
    }

    #[test]
    fn forth_quotation_references_a_typed_word_and_executes_it() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Forth,
            "quotation.forth",
            ": square ( S int -- S int ! pure ) dup * ; 9 ['] square execute",
            1_000,
        );
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(result.values, vec![TypedValue::Int(81)]);
    }

    #[test]
    fn missing_capability_suspends_before_stack_mutation() {
        let mut runtime = TypedRuntime::new();
        let before = runtime.stack().to_vec();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "memory.lisp",
            "(mem-store \"remember this\")",
            1_000,
        );
        assert!(matches!(
            result.status,
            TypedExecutionStatus::AuthorizationRequired { .. }
        ));
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn runtime_failure_rolls_back_stack() {
        let mut runtime = TypedRuntime::new();
        runtime.execute(ProgramLanguage::Forth, "a.forth", "7", 1_000);
        let before = runtime.stack().to_vec();
        let result = runtime.execute(ProgramLanguage::Forth, "b.forth", "0 /", 1_000);
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn checkpoint_round_trips_stack_and_verified_definitions() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "definition.forth",
            ": square ( S int -- S int ! pure ) dup * ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);
        runtime.execute(ProgramLanguage::Forth, "seed.forth", "6", 1_000);

        let checkpoint = runtime.checkpoint().expect("pure VM state checkpoints");
        assert!(serde_json::to_string(&checkpoint).is_ok());
        let mut restored = TypedRuntime::from_checkpoint(checkpoint)
            .expect("persisted definitions are reverified on restore");
        let result = restored.execute(ProgramLanguage::Lisp, "call.lisp", "(square 6)", 1_000);

        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(restored.stack(), &[TypedValue::Int(6), TypedValue::Int(36)]);
    }

    #[test]
    fn checkpoint_round_trips_lisp_closure_bodies_and_captures() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Lisp,
            "closure-definition.lisp",
            "(define (make-adder (n : int)) (lambda ((x : int)) (+ n x)))",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);

        let checkpoint = runtime.checkpoint().expect("closure state checkpoints");
        assert!(checkpoint
            .functions
            .keys()
            .any(|name| name.starts_with("lambda$")));
        let mut restored = TypedRuntime::from_checkpoint(checkpoint)
            .expect("generated lambda bodies restore with their public definition");
        let result = restored.execute(
            ProgramLanguage::Lisp,
            "closure-call.lisp",
            "((make-adder 7) 35)",
            1_000,
        );

        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(restored.stack(), &[TypedValue::Int(42)]);
    }

    #[test]
    fn checkpoint_round_trips_forth_quotation_bodies_and_captures() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "quotation-definition.forth",
            ": make-adder ( S n:int -- S fn<int,int> ! pure ) \
               [ int -- int ! pure | n + ] ;",
            1_000,
        );
        assert_eq!(definition.status, TypedExecutionStatus::Completed);

        let checkpoint = runtime.checkpoint().expect("quotation state checkpoints");
        assert!(checkpoint
            .functions
            .keys()
            .any(|name| name.starts_with("quote$")));
        let mut restored = TypedRuntime::from_checkpoint(checkpoint)
            .expect("generated quotation bodies restore with their public definition");
        let result = restored.execute(
            ProgramLanguage::Forth,
            "quotation-call.forth",
            "35 7 make-adder execute",
            1_000,
        );

        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert_eq!(restored.stack(), &[TypedValue::Int(42)]);
    }

    #[test]
    fn checkpoint_round_trips_a_closure_bearing_record_across_frontends() {
        let mut runtime = TypedRuntime::new();
        let created = runtime.execute(
            ProgramLanguage::Lisp,
            "record-closure.lisp",
            "{ :run (lambda ((x : int)) (+ x 1)) }",
            1_000,
        );
        assert_eq!(created.status, TypedExecutionStatus::Completed);

        let encoded = serde_json::to_string(
            &runtime
                .checkpoint()
                .expect("closure-bearing record is VM-owned data"),
        )
        .expect("checkpoint must serialize");
        let checkpoint = serde_json::from_str(&encoded).expect("checkpoint must deserialize");
        let mut restored = TypedRuntime::from_checkpoint(checkpoint)
            .expect("closure body and record value must be reverified on restore");
        let invoked = restored.execute(
            ProgramLanguage::Forth,
            "record-closure.forth",
            "\"run\" record-get unwrap 41 swap execute",
            1_000,
        );

        assert_eq!(invoked.status, TypedExecutionStatus::Completed);
        assert_eq!(restored.stack(), &[TypedValue::Int(42)]);
    }

    #[test]
    fn checkpoint_rejects_host_owned_handles() {
        let value = TypedValue::Stream {
            id: "cursor-1".into(),
            element_type: Type::String,
            kind: "file-lines".into(),
            generation: 1,
        };
        let diagnostic = ensure_checkpointable(&value).expect_err("stream needs a host restore");
        assert_eq!(diagnostic.code, "E-CHECKPOINT-003");
    }

    #[test]
    fn checkpoint_restore_refuses_to_shadow_a_core_word() {
        let mut runtime = TypedRuntime::new();
        runtime.execute(
            ProgramLanguage::Forth,
            "definition.forth",
            ": square ( S int -- S int ! pure ) dup * ;",
            1_000,
        );
        let mut checkpoint = runtime.checkpoint().unwrap();
        let mut function = checkpoint.functions.remove("square").unwrap();
        function.name = "+".into();
        checkpoint.functions.insert("+".into(), function);

        let Err(diagnostics) = TypedRuntime::from_checkpoint(checkpoint) else {
            panic!("persisted state cannot redefine core semantics");
        };
        assert_eq!(diagnostics[0].code, "E-CHECKPOINT-004");
    }

    #[test]
    fn closure_capabilities_compose_into_the_caller() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "closure.lisp",
            "(let ((store (lambda ((text : string)) (mem-store text)))) (store \"memo\"))",
            1_000,
        );
        let TypedExecutionStatus::AuthorizationRequired { requirements } = result.status else {
            panic!("expected authorization request: {:?}", result.diagnostics);
        };
        assert_eq!(requirements.len(), 1);
        assert_eq!(requirements[0].capability, CapabilityKind::MemoryWrite);
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn declaration_cannot_hide_an_inferred_capability() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute_with_declaration(
            ProgramLanguage::Lisp,
            "response.lisp",
            "(say \"hidden effect\")",
            1_000,
            Some(&EffectSet::pure()),
        );
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(result.diagnostics[0].code, "E-CAP-003");
        assert!(runtime.stack().is_empty());
    }

    #[test]
    fn lisp_while_is_metered_and_transactional() {
        let mut runtime = TypedRuntime::new();
        runtime.execute(ProgramLanguage::Lisp, "seed.lisp", "7", 1_000);
        let before = runtime.stack().to_vec();
        let result = runtime.execute(ProgramLanguage::Lisp, "loop.lisp", "(while true 1)", 20);
        assert_eq!(result.status, TypedExecutionStatus::Failed);
        assert_eq!(result.diagnostics[0].code, "E-LIMIT-001");
        assert_eq!(runtime.stack(), before);
    }

    #[test]
    fn lisp_while_records_unreached_body_capabilities_without_requesting_them() {
        let mut runtime = TypedRuntime::new();
        let result = runtime.execute(
            ProgramLanguage::Lisp,
            "loop.lisp",
            "(while false (mem-store \"never runs\"))",
            1_000,
        );
        assert_eq!(result.status, TypedExecutionStatus::Completed);
        assert!(result
            .effects
            .0
            .iter()
            .any(|requirement| { requirement.capability == CapabilityKind::MemoryWrite }));
    }
}
