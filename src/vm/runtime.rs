use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityKind, CapabilityRequirement, EffectSet};
use super::frontend::{forth::compile_forth_with_functions, lisp::compile_lisp_with_functions};
use super::interpreter::{
    CapabilityHandler, HostSideEffect, InterpreterConfig, VmContinuation, VmSideEffect, VmStep,
    VmTrampoline,
};
use super::ir::Function;
use super::types::{Type, TypedValue};
use super::{core_vocabulary, VerifiedModule, Vocabulary};
use crate::programs::ProgramLanguage;
use crate::runtime::fiber::CpuFiberScheduler;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

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
    Acknowledged { values: Vec<TypedValue> },
    Denied,
    Cancelled,
    /// The host binding returned a structured fault before it supplied a
    /// resume value. The host may have performed a partial external effect;
    /// callers must surface this prefix rather than calling rollback atomic.
    Failed { diagnostic: VmDiagnostic },
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
    pub effects: EffectSet,
    /// Events already emitted before this saved continuation. They are part of
    /// the execution journal and must not be repeated when a run resumes.
    #[serde(default)]
    pub event_journal: Vec<VmSideEffect>,
    #[serde(default)]
    pub effect_journal: Vec<EffectJournalEntry>,
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

/// Persistent typed stack shared by Finch Lisp and Co-Forth source.
pub struct TypedRuntime {
    stack: Vec<TypedValue>,
    vocabulary: Vocabulary,
    functions: BTreeMap<String, Function>,
    grants: EffectSet,
    cpu_fibers: Arc<CpuFiberScheduler>,
}

impl Default for TypedRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl TypedRuntime {
    pub fn new() -> Self {
        let response = CapabilityRequirement {
            capability: CapabilityKind::SessionEmit,
            selector: super::effects::ResourceSelector::None,
        };
        let vm_read = CapabilityRequirement {
            capability: CapabilityKind::VmRead,
            selector: super::effects::ResourceSelector::None,
        };
        Self {
            stack: Vec::new(),
            vocabulary: core_vocabulary(),
            functions: BTreeMap::new(),
            // Producing the requested assistant response is part of the
            // session contract, not an ambient host permission.
            grants: EffectSet::from_requirement(response)
                .union(&EffectSet::from_requirement(vm_read)),
            cpu_fibers: Arc::new(CpuFiberScheduler::new(
                std::thread::available_parallelism()
                    .map_or(1, |parallelism| parallelism.get().saturating_sub(1).max(1)),
            )),
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

    pub fn grants(&self) -> &EffectSet {
        &self.grants
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
        self.drive(module, continuation, handler)
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
        self.resume_with_handler_inner(suspension, values, None, handler)
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
            handler,
        )
    }

    fn resume_with_handler_inner<H: CapabilityHandler>(
        &mut self,
        suspension: TypedSuspension,
        values: Vec<TypedValue>,
        external_effect_result: Option<(u64, Vec<TypedValue>)>,
        handler: &mut H,
    ) -> TypedExecution {
        let TypedSuspension {
            module,
            continuation,
            effects,
            pending_host_call,
            event_journal,
            mut effect_journal,
            pending_cpu_fiber,
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
                if !self.grants.grants(&requested) {
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
                if !self.grants.grants(&requested) {
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
                match handler
                    .request(&call.requirement, call.arguments, &call.origin)
                    .and_then(|values| {
                        super::interpreter::validate_host_result(
                            &call.output,
                            &values,
                            &call.origin,
                        )
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
        self.drive_step(module, effects, step, event_journal, effect_journal, handler)
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
                effects,
                event_journal,
                effect_journal,
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
                VmStep::Yielded { continuation } => {
                    return self.suspended(
                        module,
                        effects,
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
                    effect,
                    output,
                    continuation,
                } => {
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
                    let requested = EffectSet::from_requirement(requirement.clone());
                    if !self.grants.grants(&requested) {
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
                                effects,
                                event_journal,
                                effect_journal,
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
                                effects,
                                event_journal,
                                effect_journal,
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
                    trampoline.resume(continuation, values)
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
                    let id = match self.cpu_fibers.spawn_closure(
                        module.clone(),
                        closure,
                        continuation.fuel,
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
                    let trampoline = VmTrampoline::new(
                        &module,
                        &InterpreterConfig {
                            fuel: continuation.fuel,
                            grants: self.grants.clone(),
                        },
                    );
                    trampoline.resume(continuation, vec![value])
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
                effects,
                event_journal,
                effect_journal,
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
                effects,
                event_journal,
                effect_journal,
                pending_cpu_fiber: Some(pending_cpu_fiber),
                pending_host_call: None,
            }),
        }
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
        let snapshot = self.cpu_fibers.poll(id).map_err(|error| {
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
            crate::runtime::fiber::CpuFiberStatus::Failed => Err(snapshot.diagnostic.unwrap_or_else(|| {
                VmDiagnostic::error(
                    "E-FIBER-016",
                    DiagnosticPhase::HostCall,
                    "CPU task failed without a diagnostic",
                    Some(origin.clone()),
                )
            })),
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
        self.cpu_fibers.cancel(id).map_err(|error| {
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
                    self.vocabulary.insert(name.clone(), function.signature.clone());
                }
                self.functions.insert(name, function);
            }
        }
        self.stack = stack;
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
        assert_eq!(result.values, vec![TypedValue::Unit]);
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
            HostSideEffect::Ui { operation: super::super::interpreter::UiOperation::Complete, text: None, progress: None, .. }
        ));

        let mut forth_runtime = TypedRuntime::new();
        let mut forth_host = RecordingHost::default();
        let forth = forth_runtime.execute_with_handler(
            ProgramLanguage::Forth,
            "output.forth",
            "s\"download\" output-open dup s\"starting\" output-append drop output-complete",
            1_000,
            None,
            &mut forth_host,
        );
        assert_eq!(forth.status, TypedExecutionStatus::Completed);
        assert!(matches!(
            forth_host.ui_events.as_slice(),
            [
                VmSideEffect { event: HostSideEffect::Ui { operation: super::super::interpreter::UiOperation::Append, .. }, .. },
                VmSideEffect { event: HostSideEffect::Ui { operation: super::super::interpreter::UiOperation::Complete, .. }, .. },
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
            assert_eq!(forth_result.output, case.expected_output, "case '{}'", case.name);
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
        assert_eq!(result.values, vec![TypedValue::Unit]);
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
        assert_eq!(completed.values, vec![TypedValue::Bytes(b"contents".to_vec())]);
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

        let wrong_arity = runtime.resume_with_effect_result(suspension, sequence, Vec::new(), &mut host);
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
            pending.suspension.expect("authorization must retain the module"),
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
        assert_eq!(completed.values, vec![TypedValue::Unit]);
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
            "s\" visible before failure\" say drop 0 0 /",
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
    fn explicit_cpu_defer_runs_a_captured_lisp_closure_on_a_private_worker_stack() {
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(
            ProgramLanguage::Lisp,
            "defer.lisp",
            "(let ((value 21)) (defer :cpu (lambda () (* value 2))))",
            1_000,
        );
        assert_eq!(execution.status, TypedExecutionStatus::Completed);
        let [TypedValue::Task { id, result_type, kind }] = execution.values.as_slice() else {
            panic!("defer :cpu must leave exactly one typed task handle");
        };
        assert_eq!(*result_type, crate::vm::types::Type::Int);
        assert_eq!(*kind, crate::vm::types::TaskKind::CpuFiber);
        let id = uuid::Uuid::parse_str(id).expect("CPU task id must be a UUID");
        let result = runtime.cpu_fibers.join(id).unwrap();
        assert_eq!(result.status, crate::runtime::fiber::CpuFiberStatus::Completed);
        assert_eq!(result.result, Some(vec![TypedValue::Int(42)]));
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
        runtime.cpu_fibers.join(id).unwrap();

        let joined = runtime.execute(ProgramLanguage::Forth, "join.forth", "task-join", 1_000);
        assert_eq!(joined.status, TypedExecutionStatus::Completed);
        assert_eq!(joined.values, vec![TypedValue::Int(42)]);
        assert_eq!(runtime.stack(), &[TypedValue::Int(42)]);
    }

    #[test]
    fn completed_cpu_task_polls_as_some_of_its_declared_type() {
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
            .join(uuid::Uuid::parse_str(id).unwrap())
            .unwrap();
        let polled = runtime.execute(ProgramLanguage::Forth, "poll.forth", "task-poll", 1_000);
        assert_eq!(polled.status, TypedExecutionStatus::Completed);
        assert_eq!(
            polled.values,
            vec![TypedValue::Option {
                inner_type: Type::Int,
                value: Some(Box::new(TypedValue::Int(9))),
            }]
        );
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
        assert_eq!(cancelled.values, vec![TypedValue::Unit]);
        assert_eq!(runtime.stack(), &[TypedValue::Unit]);
    }

    #[test]
    fn forth_definition_persists_and_is_callable_from_lisp() {
        let mut runtime = TypedRuntime::new();
        let definition = runtime.execute(
            ProgramLanguage::Forth,
            "words.forth",
            ": square ( S int -- S int ! {} ) dup * ;",
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
            ": dishonest ( S string -- S unit ! {} ) say ;",
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
            ": factorial ( S int -- S int ! {} ) \
               locals| n | \
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
            ": even? ( S int -- S bool ! {} ) \
               locals| n | n 0 = if true else n 1 - odd? then ; \
             : odd? ( S int -- S bool ! {} ) \
               locals| n | n 0 = if false else n 1 - even? then ;",
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
            ": square ( S int -- S int ! {} ) dup * ; 9 ['] square execute",
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
        assert!(result.effects.0.iter().any(|requirement| {
            requirement.capability == CapabilityKind::MemoryWrite
        }));
    }
}
