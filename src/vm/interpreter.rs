use super::diagnostic::{DiagnosticPhase, SourceOrigin, VmDiagnostic};
use super::effects::{CapabilityKind, CapabilityRequirement, EffectSet, ResourceSelector};
use super::ir::{BlockId, Instruction};
use super::types::{Type, TypedValue};
use super::verifier::VerifiedModule;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiOperation {
    Create,
    Append,
    Replace,
    Status,
    Progress,
    Complete,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HostSideEffect {
    /// Append durable natural-language prose to the response transcript.
    Emit { text: String },
    /// An ordered UI intent applied by the host's shadow buffer. This is not
    /// direct UI mutation by the VM: rendering, replacement, and persistence
    /// remain under the event loop's control.
    Ui {
        operation: UiOperation,
        /// An opaque host-issued `resource<output-handle>` value. Source
        /// programs cannot construct one from a symbol/string; only a host
        /// binding may allocate and return it.
        target: Option<TypedValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<UiProgress>,
    },
    /// A capability-bearing host request. The VM only proposes this typed
    /// operation; a harness records/authorizes/executes it and resumes the
    /// continuation with a typed result.
    Request { arguments: Vec<TypedValue> },
}

/// Bounded or indeterminate progress metadata carried as data, rather than
/// terminal control codes. `total = None` means indeterminate progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UiProgress {
    pub completed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

/// One portable, ordered side-effect requested by the VM. The enclosing
/// runtime/run supplies its own execution ID; together `(execution_id,
/// sequence)` is an idempotency key for an interface loop or remote harness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmSideEffect {
    #[serde(default = "side_effect_protocol_version")]
    pub protocol_version: u32,
    pub sequence: u64,
    pub requirement: CapabilityRequirement,
    pub event: HostSideEffect,
    /// The exact verifier-known values an awaited request must return when
    /// the host sends its correlated `VmResume`. Emitted events are terminal
    /// side effects and therefore have an empty output row.
    #[serde(default)]
    pub output: Vec<Type>,
    pub origin: SourceOrigin,
}

fn side_effect_protocol_version() -> u32 {
    super::VM_TYPE_SYSTEM_VERSION
}

/// Validate a host resume payload against the output row already verified for
/// the awaited instruction.  Hosts never get to smuggle an arbitrary value
/// onto a saved VM stack merely because they own an I/O boundary.
pub(crate) fn validate_host_result(
    expected: &[Type],
    values: &[TypedValue],
    origin: &SourceOrigin,
) -> Result<(), VmDiagnostic> {
    if expected.len() != values.len() {
        let mut diagnostic = VmDiagnostic::error(
            "E-RESUME-003",
            DiagnosticPhase::HostCall,
            format!(
                "host resume returned {} values but the awaited operation requires {}",
                values.len(),
                expected.len()
            ),
            Some(origin.clone()),
        );
        diagnostic.expected_types = expected.to_vec();
        diagnostic.found_types = values.iter().map(TypedValue::value_type).collect();
        return Err(diagnostic);
    }
    for (expected, value) in expected.iter().zip(values) {
        let found = value.value_type();
        if !expected.accepts(&found) {
            return Err(VmDiagnostic::type_mismatch(
                expected.clone(),
                found,
                Some(origin.clone()),
            ));
        }
    }
    Ok(())
}

pub trait CapabilityHandler {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic>;

    /// Handle an awaited effect with its protocol metadata intact. Hosts that
    /// only need the capability arguments can use the default bridge; richer
    /// adapters may project a host-issued resource (such as `output-open`)
    /// before returning the typed resume value.
    fn request_effect(&mut self, effect: &VmSideEffect) -> Result<Vec<TypedValue>, VmDiagnostic> {
        let HostSideEffect::Request { arguments } = &effect.event else {
            return Err(VmDiagnostic::error(
                "E-HOST-002",
                DiagnosticPhase::HostCall,
                "VM await boundary did not carry a host request",
                Some(effect.origin.clone()),
            ));
        };
        self.request(&effect.requirement, arguments.clone(), &effect.origin)
    }

    /// Observe an awaited request as soon as it enters the portable effect
    /// journal. This lets a host event loop render, persist, or hand the
    /// effect to another process before approval/result handling. It is an
    /// observation hook only: `request_effect` remains the binding that may
    /// execute a locally handled request.
    fn observe_awaited_effect(&mut self, _effect: &VmSideEffect) -> Result<(), VmDiagnostic> {
        Ok(())
    }

    /// Return true when this host wants an already-authorized effect to stay
    /// pending for an external event loop rather than executing it
    /// synchronously. The VM retains the verified continuation and the host
    /// must later provide a correlated `VmResume` result. This is how editor,
    /// browser, and IDE proposal workflows avoid blocking the VM runner.
    fn defer_awaited_effect(&self, _effect: &VmSideEffect) -> bool {
        false
    }

    /// Optional user-facing output accumulated by a host binding. Capability
    /// handlers that do not produce text can keep the default implementation.
    fn output(&self) -> String {
        String::new()
    }

    /// Receive each user-visible output chunk as it is produced. The default
    /// keeps non-streaming handlers source-compatible.
    fn emit(&mut self, _chunk: &str) {}

    /// Receive a typed UI side-effect. The compatibility default preserves
    /// the existing `say` host-call behavior; richer UI operations are safely
    /// ignored by minimal handlers until they opt into rendering them.
    fn side_effect(&mut self, effect: &VmSideEffect) -> Result<(), VmDiagnostic> {
        if let HostSideEffect::Emit { text } = &effect.event {
            self.request(
                &effect.requirement,
                vec![TypedValue::String(text.clone())],
                &effect.origin,
            )?;
            self.emit(text);
        }
        Ok(())
    }

    fn output_chunks(&self) -> Vec<String> {
        Vec::new()
    }

    fn side_effects(&self) -> Vec<HostSideEffect> {
        Vec::new()
    }
}

impl<T: CapabilityHandler + ?Sized> CapabilityHandler for &mut T {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        (**self).request(requirement, arguments, origin)
    }

    fn output(&self) -> String {
        (**self).output()
    }

    fn emit(&mut self, chunk: &str) {
        (**self).emit(chunk)
    }

    fn side_effect(&mut self, effect: &VmSideEffect) -> Result<(), VmDiagnostic> {
        (**self).side_effect(effect)
    }

    fn output_chunks(&self) -> Vec<String> {
        (**self).output_chunks()
    }

    fn side_effects(&self) -> Vec<HostSideEffect> {
        (**self).side_effects()
    }

    fn observe_awaited_effect(&mut self, effect: &VmSideEffect) -> Result<(), VmDiagnostic> {
        (**self).observe_awaited_effect(effect)
    }

    fn defer_awaited_effect(&self, effect: &VmSideEffect) -> bool {
        (**self).defer_awaited_effect(effect)
    }
}

pub struct DenyCapabilities;

impl CapabilityHandler for DenyCapabilities {
    fn request(
        &mut self,
        requirement: &CapabilityRequirement,
        _arguments: Vec<TypedValue>,
        origin: &SourceOrigin,
    ) -> Result<Vec<TypedValue>, VmDiagnostic> {
        let mut diagnostic = VmDiagnostic::error(
            "E-CAP-002",
            DiagnosticPhase::Authorization,
            format!("capability {:?} was not granted", requirement.capability),
            Some(origin.clone()),
        );
        diagnostic.capability = Some(requirement.clone());
        Err(diagnostic)
    }
}

#[derive(Debug, Clone)]
pub struct InterpreterConfig {
    pub fuel: u64,
    pub grants: EffectSet,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            fuel: 100_000,
            grants: EffectSet::pure(),
        }
    }
}

/// Serializable activation record for the VM's internal trampoline. This is
/// deliberately VM data, not a Rust callback: a daemon can persist it beside
/// an immutable verified-module reference and resume it after reconnect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmFrame {
    pub function: String,
    pub block: BlockId,
    pub instruction: usize,
    /// First stack slot owned by this call. Values below this boundary belong
    /// to its caller and can never be consumed or leaked by the callee.
    pub stack_base: usize,
    /// Number of values this function is allowed to return above `stack_base`.
    /// This duplicates the verifier's static contract as a runtime boundary
    /// check for persisted continuations and interpreter correctness.
    pub output_arity: usize,
    pub locals: Vec<TypedValue>,
    pub captures: Vec<TypedValue>,
}

/// The state closed over by an internal zero-argument VM thunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VmContinuation {
    pub stack: Vec<TypedValue>,
    pub frames: Vec<VmFrame>,
    pub fuel: u64,
    /// Monotonic within one run. The enclosing harness pairs this with its
    /// execution ID to deduplicate/replay side-effect acknowledgements.
    #[serde(default)]
    pub next_effect_sequence: u64,
}

/// One result from running the VM until its next observable boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum VmStep {
    /// A cooperative timeslice boundary. The event loop reschedules this
    /// internal continuation without exposing it to source programs.
    Yielded {
        continuation: VmContinuation,
    },
    Emit {
        effect: VmSideEffect,
        continuation: VmContinuation,
    },
    Await {
        effect: VmSideEffect,
        /// Exact values the host must supply before the saved frame may
        /// resume.  This remains VM metadata rather than an untyped host
        /// convention.
        output: Vec<Type>,
        continuation: VmContinuation,
    },
    /// A pure closure should run on the daemon's bounded CPU-fiber pool. The
    /// event loop owns scheduling and resumes with one typed task handle.
    SpawnCpuFiber {
        closure: TypedValue,
        origin: SourceOrigin,
        continuation: VmContinuation,
    },
    /// Inspect a local CPU-fiber task. The runtime immediately resumes with
    /// `option<T>` while retaining ownership checks outside the VM core.
    PollCpuFiber {
        task: TypedValue,
        origin: SourceOrigin,
        continuation: VmContinuation,
    },
    /// Suspend a parent run on a CPU-fiber terminal result. The event loop
    /// owns readiness notifications and later resumes with `T`.
    JoinCpuFiber {
        task: TypedValue,
        origin: SourceOrigin,
        continuation: VmContinuation,
    },
    /// Request cooperative cancellation of a local CPU-fiber task. The
    /// scheduler owns the worker and resumes this continuation with unit.
    CancelCpuFiber {
        task: TypedValue,
        origin: SourceOrigin,
        continuation: VmContinuation,
    },
    Complete {
        stack: Vec<TypedValue>,
    },
    Failed(VmDiagnostic),
}

/// The handler-free execution core used by the runtime event-loop trampoline.
/// It advances pure instructions synchronously and yields at output and host
/// capability boundaries. `Interpreter::execute` remains a synchronous
/// adapter for existing callers while the runtime migrates to this interface.
pub struct VmTrampoline<'a> {
    module: &'a VerifiedModule,
    fuel: u64,
}

impl<'a> VmTrampoline<'a> {
    pub fn new(module: &'a VerifiedModule, config: &InterpreterConfig) -> Self {
        Self {
            module,
            fuel: config.fuel,
        }
    }

    pub fn start(&self, stack: Vec<TypedValue>) -> Result<VmContinuation, VmDiagnostic> {
        self.start_function(&self.module.module.entry, Vec::new(), stack)
    }

    /// Start an isolated invocation of a verified function. CPU fibers use
    /// this rather than inheriting a parent VM's stack or call frames.
    pub fn start_function(
        &self,
        function: &str,
        captures: Vec<TypedValue>,
        stack: Vec<TypedValue>,
    ) -> Result<VmContinuation, VmDiagnostic> {
        let frame = self.frame_for(function, captures, 0)?;
        Ok(VmContinuation {
            stack,
            frames: vec![frame],
            fuel: self.fuel,
            next_effect_sequence: 0,
        })
    }

    /// Resume a yielded continuation with the typed values supplied by the
    /// event loop (for example, an approved file read or child result).
    pub fn resume(&self, mut continuation: VmContinuation, values: Vec<TypedValue>) -> VmStep {
        continuation.stack.extend(values);
        self.run(continuation)
    }

    /// Run a bounded slice until it emits, awaits, completes, or fails.
    pub fn run(&self, mut continuation: VmContinuation) -> VmStep {
        loop {
            let Some(frame) = continuation.frames.last_mut() else {
                return VmStep::Complete {
                    stack: continuation.stack,
                };
            };
            let Some(function) = self.module.module.functions.get(&frame.function) else {
                return VmStep::Failed(VmDiagnostic::error(
                    "E-LINK-004",
                    DiagnosticPhase::Linking,
                    format!("missing function '{}' in continuation", frame.function),
                    None,
                ));
            };
            let Some(block) = function.blocks.get(&frame.block) else {
                return VmStep::Failed(self.with_trace(
                    VmDiagnostic::error(
                        "E-RUNTIME-004",
                        DiagnosticPhase::Interpretation,
                        format!("missing block {}", frame.block),
                        None,
                    ),
                    &continuation,
                ));
            };
            let Some(located) = block.instructions.get(frame.instruction).cloned() else {
                return VmStep::Failed(self.with_trace(
                    VmDiagnostic::error(
                        "E-RUNTIME-004",
                        DiagnosticPhase::Interpretation,
                        format!("block {} ended without transferring control", frame.block),
                        None,
                    ),
                    &continuation,
                ));
            };
            if continuation.fuel == 0 {
                return VmStep::Failed(self.with_trace(
                    VmDiagnostic::error(
                        "E-LIMIT-001",
                        DiagnosticPhase::ResourceLimit,
                        "typed VM fuel exhausted",
                        Some(located.origin),
                    ),
                    &continuation,
                ));
            }
            continuation.fuel -= 1;
            // Advance before executing: yielded continuation state begins at
            // the instruction after the visible operation.
            continuation.frames.last_mut().unwrap().instruction += 1;
            let result = match located.instruction {
                Instruction::Constant { value } => {
                    continuation.stack.push(value);
                    Ok(None)
                }
                Instruction::MakeList {
                    element_type,
                    count,
                } => {
                    let Some(start) = continuation.stack.len().checked_sub(count as usize) else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let values = continuation.stack.drain(start..).collect();
                    continuation.stack.push(TypedValue::List {
                        element_type,
                        values,
                    });
                    Ok(None)
                }
                Instruction::Dup => {
                    let Some(value) = continuation.stack.last().cloned() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    continuation.stack.push(value);
                    Ok(None)
                }
                Instruction::Drop => {
                    if continuation.stack.pop().is_none() {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    }
                    Ok(None)
                }
                Instruction::Swap => {
                    let len = continuation.stack.len();
                    if len < 2 {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    }
                    continuation.stack.swap(len - 1, len - 2);
                    Ok(None)
                }
                Instruction::LocalGet { index } => {
                    let Some(value) = continuation
                        .frames
                        .last()
                        .unwrap()
                        .locals
                        .get(index as usize)
                        .cloned()
                    else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-008",
                                DiagnosticPhase::Interpretation,
                                "invalid local index",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    continuation.stack.push(value);
                    Ok(None)
                }
                Instruction::LocalSet { index } => {
                    let Some(value) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let Some(local) = continuation
                        .frames
                        .last_mut()
                        .unwrap()
                        .locals
                        .get_mut(index as usize)
                    else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-008",
                                DiagnosticPhase::Interpretation,
                                "invalid local index",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    *local = value;
                    Ok(None)
                }
                Instruction::CaptureGet { index } => {
                    let Some(value) = continuation
                        .frames
                        .last()
                        .unwrap()
                        .captures
                        .get(index as usize)
                        .cloned()
                    else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-009",
                                DiagnosticPhase::Interpretation,
                                "invalid capture index",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    continuation.stack.push(value);
                    Ok(None)
                }
                Instruction::MakeClosure {
                    function,
                    capture_count,
                    signature,
                } => {
                    let Some(start) = continuation.stack.len().checked_sub(capture_count as usize)
                    else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let captures = continuation.stack.drain(start..).collect();
                    continuation.stack.push(TypedValue::Closure {
                        function,
                        captures,
                        signature,
                    });
                    Ok(None)
                }
                Instruction::Call { function } => {
                    self.call_or_core(&function, Vec::new(), &mut continuation, &located.origin)
                }
                Instruction::CallClosure { .. } => {
                    let Some(closure) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let TypedValue::Closure {
                        function, captures, ..
                    } = closure
                    else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-002",
                                DiagnosticPhase::Interpretation,
                                "attempted to call a non-closure value",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    self.call_or_core(&function, captures, &mut continuation, &located.origin)
                }
                Instruction::OutputOpen => {
                    let Some(TypedValue::String(title)) = continuation.stack.pop() else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-OUTPUT-001",
                                DiagnosticPhase::HostCall,
                                "output-open requires one title string",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    let effect = VmSideEffect {
                        protocol_version: side_effect_protocol_version(),
                        sequence: continuation.next_effect_sequence,
                        requirement: CapabilityRequirement {
                            capability: CapabilityKind::SessionEmit,
                            selector: ResourceSelector::None,
                        },
                        event: HostSideEffect::Request {
                            arguments: vec![TypedValue::String(title)],
                        },
                        output: vec![Type::Resource("output-handle".into())],
                        origin: located.origin,
                    };
                    continuation.next_effect_sequence += 1;
                    return VmStep::Await {
                        effect,
                        output: vec![Type::Resource("output-handle".into())],
                        continuation,
                    };
                }
                Instruction::UiEffect {
                    operation,
                    input,
                    output,
                } => {
                    let Some(start) = continuation.stack.len().checked_sub(input.len()) else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let arguments: Vec<_> = continuation.stack.drain(start..).collect();
                    let Some(TypedValue::Resource { kind, .. }) = arguments.first() else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-OUTPUT-002",
                                DiagnosticPhase::HostCall,
                                "output operation requires an output handle",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    if kind != "output-handle" {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-OUTPUT-003",
                                DiagnosticPhase::HostCall,
                                "output operation received a resource of the wrong kind",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    }
                    let target = arguments.first().cloned();
                    let (text, progress) = match operation {
                        UiOperation::Append
                        | UiOperation::Replace
                        | UiOperation::Status
                        | UiOperation::Fail => {
                            let Some(TypedValue::String(text)) = arguments.get(1) else {
                                return VmStep::Failed(self.with_trace(
                                    VmDiagnostic::error(
                                        "E-OUTPUT-004",
                                        DiagnosticPhase::HostCall,
                                        "output text operation requires a string payload",
                                        Some(located.origin),
                                    ),
                                    &continuation,
                                ));
                            };
                            (Some(text.clone()), None)
                        }
                        UiOperation::Progress => {
                            let Some(TypedValue::Int(completed)) = arguments.get(1) else {
                                return VmStep::Failed(self.with_trace(
                                    VmDiagnostic::error(
                                        "E-OUTPUT-005",
                                        DiagnosticPhase::HostCall,
                                        "output-progress requires a non-negative completed count",
                                        Some(located.origin),
                                    ),
                                    &continuation,
                                ));
                            };
                            let Some(TypedValue::Int(total)) = arguments.get(2) else {
                                return VmStep::Failed(self.with_trace(
                                    VmDiagnostic::error(
                                        "E-OUTPUT-006",
                                        DiagnosticPhase::HostCall,
                                        "output-progress requires a non-negative total count",
                                        Some(located.origin),
                                    ),
                                    &continuation,
                                ));
                            };
                            let (Ok(completed), Ok(total)) =
                                (u64::try_from(*completed), u64::try_from(*total))
                            else {
                                return VmStep::Failed(self.with_trace(
                                    VmDiagnostic::error(
                                        "E-OUTPUT-007",
                                        DiagnosticPhase::HostCall,
                                        "output progress counts must be non-negative",
                                        Some(located.origin),
                                    ),
                                    &continuation,
                                ));
                            };
                            (
                                None,
                                Some(UiProgress {
                                    completed,
                                    total: Some(total),
                                }),
                            )
                        }
                        UiOperation::Complete => (None, None),
                        UiOperation::Create => unreachable!("output-open is an awaited host call"),
                    };
                    if !output.is_empty() {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-OUTPUT-008",
                                DiagnosticPhase::Interpretation,
                                "portable UI operations must not return stack values",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    }
                    let effect = VmSideEffect {
                        protocol_version: side_effect_protocol_version(),
                        sequence: continuation.next_effect_sequence,
                        requirement: CapabilityRequirement {
                            capability: CapabilityKind::SessionEmit,
                            selector: ResourceSelector::None,
                        },
                        event: HostSideEffect::Ui {
                            operation,
                            target,
                            text,
                            progress,
                        },
                        output: Vec::new(),
                        origin: located.origin,
                    };
                    continuation.next_effect_sequence += 1;
                    return VmStep::Emit {
                        effect,
                        continuation,
                    };
                }
                Instruction::CapabilityRequest {
                    requirement,
                    input,
                    output,
                } => {
                    let Some(start) = continuation.stack.len().checked_sub(input.len()) else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let arguments: Vec<_> = continuation.stack.drain(start..).collect();
                    let concrete = match instantiate_requirement(&requirement, &arguments) {
                        Ok(requirement) => requirement,
                        Err(message) => {
                            return VmStep::Failed(self.with_trace(
                                VmDiagnostic::error(
                                    "E-CAP-003",
                                    DiagnosticPhase::Authorization,
                                    message,
                                    Some(located.origin),
                                ),
                                &continuation,
                            ))
                        }
                    };
                    if concrete.capability == CapabilityKind::SessionEmit {
                        let event = match arguments.as_slice() {
                            [TypedValue::String(text)] => {
                                HostSideEffect::Emit { text: text.clone() }
                            }
                            _ => {
                                return VmStep::Failed(self.with_trace(
                                    VmDiagnostic::error(
                                        "E-HOST-001",
                                        DiagnosticPhase::HostCall,
                                        "session.emit requires one string",
                                        Some(located.origin),
                                    ),
                                    &continuation,
                                ))
                            }
                        };
                        let effect = VmSideEffect {
                            protocol_version: side_effect_protocol_version(),
                            sequence: continuation.next_effect_sequence,
                            requirement: concrete,
                            event,
                            output: Vec::new(),
                            origin: located.origin,
                        };
                        continuation.next_effect_sequence += 1;
                        return VmStep::Emit {
                            effect,
                            continuation,
                        };
                    }
                    let effect = VmSideEffect {
                        protocol_version: side_effect_protocol_version(),
                        sequence: continuation.next_effect_sequence,
                        requirement: concrete,
                        event: HostSideEffect::Request { arguments },
                        output: output.clone(),
                        origin: located.origin,
                    };
                    continuation.next_effect_sequence += 1;
                    return VmStep::Await {
                        effect,
                        output,
                        continuation,
                    };
                }
                Instruction::Yield => return VmStep::Yielded { continuation },
                Instruction::DeferCpu => {
                    let Some(closure) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    if !matches!(closure, TypedValue::Closure { .. }) {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-FIBER-003",
                                DiagnosticPhase::Interpretation,
                                "defer-cpu requires a typed closure",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    }
                    return VmStep::SpawnCpuFiber {
                        closure,
                        origin: located.origin,
                        continuation,
                    };
                }
                Instruction::PollCpuFiber => {
                    let Some(task) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    return VmStep::PollCpuFiber {
                        task,
                        origin: located.origin,
                        continuation,
                    };
                }
                Instruction::JoinCpuFiber => {
                    let Some(task) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    return VmStep::JoinCpuFiber {
                        task,
                        origin: located.origin,
                        continuation,
                    };
                }
                Instruction::CancelCpuFiber => {
                    let Some(task) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    return VmStep::CancelCpuFiber {
                        task,
                        origin: located.origin,
                        continuation,
                    };
                }
                Instruction::Jump { target } => {
                    let frame = continuation.frames.last_mut().unwrap();
                    frame.block = target;
                    frame.instruction = 0;
                    Ok(None)
                }
                Instruction::Branch {
                    then_block,
                    else_block,
                } => {
                    let Some(condition) = continuation.stack.pop() else {
                        return VmStep::Failed(self.underflow(&located.origin, &continuation));
                    };
                    let TypedValue::Bool(condition) = condition else {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-003",
                                DiagnosticPhase::Interpretation,
                                "branch condition is not boolean",
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    };
                    let frame = continuation.frames.last_mut().unwrap();
                    frame.block = if condition { then_block } else { else_block };
                    frame.instruction = 0;
                    Ok(None)
                }
                Instruction::Return => {
                    let frame = continuation.frames.last().expect("frame exists");
                    let required = frame.stack_base.saturating_add(frame.output_arity);
                    if continuation.stack.len() < required {
                        return VmStep::Failed(self.with_trace(
                            VmDiagnostic::error(
                                "E-RUNTIME-010",
                                DiagnosticPhase::Interpretation,
                                format!(
                                    "word '{}' returned fewer than its declared {} values",
                                    frame.function, frame.output_arity
                                ),
                                Some(located.origin),
                            ),
                            &continuation,
                        ));
                    }
                    let results = continuation
                        .stack
                        .split_off(continuation.stack.len() - frame.output_arity);
                    continuation.stack.truncate(frame.stack_base);
                    continuation.stack.extend(results);
                    continuation.frames.pop();
                    Ok(None)
                }
                Instruction::Trap { code } => {
                    return VmStep::Failed(self.with_trace(
                        VmDiagnostic::error(
                            code,
                            DiagnosticPhase::Interpretation,
                            "program raised a trap",
                            Some(located.origin),
                        ),
                        &continuation,
                    ))
                }
            };
            if let Err(diagnostic) = result {
                return VmStep::Failed(self.with_trace(diagnostic, &continuation));
            }
        }
    }

    fn frame_for(
        &self,
        function: &str,
        captures: Vec<TypedValue>,
        stack_base: usize,
    ) -> Result<VmFrame, VmDiagnostic> {
        let function_def = self.module.module.functions.get(function).ok_or_else(|| {
            VmDiagnostic::error(
                "E-LINK-004",
                DiagnosticPhase::Linking,
                format!("unknown word '{function}'"),
                None,
            )
        })?;
        Ok(VmFrame {
            function: function.to_string(),
            block: function_def.entry,
            instruction: 0,
            stack_base,
            output_arity: function_def.signature.output.values.len(),
            locals: vec![TypedValue::Unit; function_def.locals.len()],
            captures,
        })
    }

    fn call_or_core(
        &self,
        function: &str,
        captures: Vec<TypedValue>,
        continuation: &mut VmContinuation,
        origin: &SourceOrigin,
    ) -> Result<Option<()>, VmDiagnostic> {
        if self.module.module.functions.contains_key(function) {
            let function_def = &self.module.module.functions[function];
            let input_arity = function_def.signature.input.values.len();
            let stack_base = continuation
                .stack
                .len()
                .checked_sub(input_arity)
                .ok_or_else(|| {
                    self.with_trace_at(
                        VmDiagnostic::error(
                            "E-RUNTIME-011",
                            DiagnosticPhase::Interpretation,
                            format!("word '{function}' was called without its declared inputs"),
                            Some(origin.clone()),
                        ),
                        continuation,
                        origin,
                    )
                })?;
            continuation
                .frames
                .push(self.frame_for(function, captures, stack_base)?);
            Ok(None)
        } else {
            execute_core(function, &mut continuation.stack)
                .map_err(|diagnostic| self.with_trace_at(diagnostic, continuation, origin))?;
            Ok(None)
        }
    }

    fn underflow(&self, origin: &SourceOrigin, continuation: &VmContinuation) -> VmDiagnostic {
        self.with_trace(runtime_underflow(origin, Vec::new()), continuation)
    }

    fn with_trace(
        &self,
        mut diagnostic: VmDiagnostic,
        continuation: &VmContinuation,
    ) -> VmDiagnostic {
        if diagnostic.trace.is_empty() {
            diagnostic.trace = continuation
                .frames
                .iter()
                .map(|frame| frame.function.clone())
                .collect();
        }
        diagnostic
    }

    fn with_trace_at(
        &self,
        mut diagnostic: VmDiagnostic,
        continuation: &VmContinuation,
        _origin: &SourceOrigin,
    ) -> VmDiagnostic {
        if diagnostic.trace.is_empty() {
            diagnostic.trace = continuation
                .frames
                .iter()
                .map(|frame| frame.function.clone())
                .collect();
        }
        diagnostic
    }
}

pub struct Interpreter<'a, H> {
    module: &'a VerifiedModule,
    handler: H,
    config: InterpreterConfig,
}

impl<'a, H: CapabilityHandler> Interpreter<'a, H> {
    pub fn new(module: &'a VerifiedModule, handler: H, config: InterpreterConfig) -> Self {
        Self {
            module,
            handler,
            config,
        }
    }

    /// Execute transactionally against an owned stack. The caller's stack is
    /// changed only after the entry function returns successfully.
    pub fn execute(&mut self, stack: &mut Vec<TypedValue>) -> Result<(), VmDiagnostic> {
        let trampoline = VmTrampoline::new(self.module, &self.config);
        let continuation = trampoline.start(stack.clone())?;
        let mut step = trampoline.run(continuation);
        loop {
            step = match step {
                VmStep::Yielded { continuation } => trampoline.run(continuation),
                VmStep::Emit {
                    effect,
                    continuation,
                } => {
                    let requirement = CapabilityRequirement {
                        capability: CapabilityKind::SessionEmit,
                        selector: ResourceSelector::None,
                    };
                    let requested = EffectSet::from_requirement(requirement.clone());
                    if !self.config.grants.grants(&requested) {
                        let mut diagnostic = VmDiagnostic::error(
                            "E-CAP-002",
                            DiagnosticPhase::Authorization,
                            "session.emit is outside this execution's grants",
                            Some(effect.origin.clone()),
                        );
                        diagnostic.capability = Some(requirement);
                        return Err(diagnostic);
                    }
                    // The continuation already contains the unit produced by
                    // the UI operation; this host projection is intentionally
                    // ignored by the synchronous compatibility adapter.
                    self.handler.side_effect(&effect)?;
                    trampoline.run(continuation)
                }
                VmStep::Await {
                    effect,
                    output,
                    continuation,
                } => {
                    self.handler.observe_awaited_effect(&effect)?;
                    let HostSideEffect::Request { arguments } = effect.event else {
                        return Err(VmDiagnostic::error(
                            "E-HOST-002",
                            DiagnosticPhase::HostCall,
                            "VM await boundary did not carry a host request",
                            Some(effect.origin),
                        ));
                    };
                    let requirement = effect.requirement;
                    let origin = effect.origin;
                    let requested = EffectSet::from_requirement(requirement.clone());
                    if !self.config.grants.grants(&requested) {
                        let mut diagnostic = VmDiagnostic::error(
                            "E-CAP-002",
                            DiagnosticPhase::Authorization,
                            format!(
                                "capability {:?} is outside this execution's grants",
                                requirement.capability
                            ),
                            Some(origin),
                        );
                        diagnostic.capability = Some(requirement);
                        return Err(diagnostic);
                    }
                    let values = self.handler.request(&requirement, arguments, &origin)?;
                    validate_host_result(&output, &values, &origin)?;
                    trampoline.resume(continuation, values)
                }
                VmStep::SpawnCpuFiber { origin, .. } => {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-006",
                        DiagnosticPhase::HostCall,
                        "CPU fibers require the typed runtime event loop",
                        Some(origin),
                    ));
                }
                VmStep::PollCpuFiber { origin, .. }
                | VmStep::JoinCpuFiber { origin, .. }
                | VmStep::CancelCpuFiber { origin, .. } => {
                    return Err(VmDiagnostic::error(
                        "E-FIBER-018",
                        DiagnosticPhase::HostCall,
                        "CPU task operations require the typed runtime event loop",
                        Some(origin),
                    ));
                }
                VmStep::Complete { stack: pending } => {
                    *stack = pending;
                    return Ok(());
                }
                VmStep::Failed(diagnostic) => return Err(diagnostic),
            };
        }
    }
}

fn instantiate_requirement(
    requirement: &CapabilityRequirement,
    arguments: &[TypedValue],
) -> Result<CapabilityRequirement, String> {
    let selector = match &requirement.selector {
        ResourceSelector::FileTemplate { template } => ResourceSelector::File {
            selector: template.instantiate(arguments).map_err(|error| {
                format!("capability selector could not be instantiated: {error}")
            })?,
        },
        ResourceSelector::NetworkTemplate { template } => {
            let (host, port) = template.instantiate(arguments).map_err(|error| {
                format!("capability selector could not be instantiated: {error}")
            })?;
            ResourceSelector::Network {
                host,
                ports: vec![port],
            }
        }
        ResourceSelector::ProcessTemplate { template } => {
            let executable = template.instantiate(arguments).map_err(|error| {
                format!("capability selector could not be instantiated: {error}")
            })?;
            ResourceSelector::Process {
                executables: vec![executable],
            }
        }
        ResourceSelector::ProgramTemplate { template } => {
            let language = template.instantiate(arguments).map_err(|error| {
                format!("capability selector could not be instantiated: {error}")
            })?;
            ResourceSelector::Program {
                languages: vec![language],
            }
        }
        selector => {
            return Ok(CapabilityRequirement {
                capability: requirement.capability.clone(),
                selector: selector.clone(),
            })
        }
    };
    Ok(CapabilityRequirement {
        capability: requirement.capability.clone(),
        selector,
    })
}

fn execute_core(name: &str, stack: &mut Vec<TypedValue>) -> Result<(), VmDiagnostic> {
    let origin = SourceOrigin::generated(name);
    let pop = |stack: &mut Vec<TypedValue>| {
        stack
            .pop()
            .ok_or_else(|| runtime_underflow(&origin, Vec::new()))
    };
    match name {
        "+" | "-" | "*" | "/" | "mod" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::Int(left), TypedValue::Int(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires integer operands"),
                    Some(origin),
                ));
            };
            let value = match name {
                "+" => left.checked_add(right),
                "-" => left.checked_sub(right),
                "*" => left.checked_mul(right),
                "/" if right != 0 => left.checked_div(right),
                "mod" if right != 0 => left.checked_rem(right),
                _ => None,
            }
            .ok_or_else(|| {
                VmDiagnostic::error(
                    if right == 0 && matches!(name, "/" | "mod") {
                        "E-NUM-001"
                    } else {
                        "E-NUM-002"
                    },
                    DiagnosticPhase::Interpretation,
                    if right == 0 && matches!(name, "/" | "mod") {
                        "division by zero"
                    } else {
                        "integer overflow"
                    },
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            stack.push(TypedValue::Int(value));
        }
        "=" | "<" | ">" | "<=" | ">=" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::Int(left), TypedValue::Int(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires integer operands"),
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(match name {
                "=" => left == right,
                "<" => left < right,
                ">" => left > right,
                "<=" => left <= right,
                ">=" => left >= right,
                _ => unreachable!(),
            }));
        }
        "negate" | "abs" => {
            let value = pop(stack)?;
            let TypedValue::Int(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires an integer operand"),
                    Some(origin),
                ));
            };
            let value = if name == "negate" {
                value.checked_neg()
            } else {
                value.checked_abs()
            }
            .ok_or_else(|| {
                VmDiagnostic::error(
                    "E-NUM-002",
                    DiagnosticPhase::Interpretation,
                    "integer overflow",
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            stack.push(TypedValue::Int(value));
        }
        "not" => {
            let value = pop(stack)?;
            let TypedValue::Bool(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-006",
                    DiagnosticPhase::Interpretation,
                    "not requires a boolean operand",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(!value));
        }
        "dup" => {
            let value = stack
                .last()
                .cloned()
                .ok_or_else(|| runtime_underflow(&origin, Vec::new()))?;
            stack.push(value);
        }
        "drop" => {
            pop(stack)?;
        }
        "swap" => {
            if stack.len() < 2 {
                return Err(runtime_underflow(&origin, Vec::new()));
            }
            let len = stack.len();
            stack.swap(len - 1, len - 2);
        }
        "str-cat" => {
            let right = pop(stack)?;
            let left = pop(stack)?;
            let (TypedValue::String(left), TypedValue::String(right)) = (left, right) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-007",
                    DiagnosticPhase::Interpretation,
                    "str-cat requires string operands",
                    Some(origin),
                ));
            };
            let mut result = String::with_capacity(left.len() + right.len());
            result.push_str(&left);
            result.push_str(&right);
            stack.push(TypedValue::String(result));
        }
        "bytes" => {
            let value = pop(stack)?;
            let TypedValue::String(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-011",
                    DiagnosticPhase::Interpretation,
                    "bytes requires a string",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bytes(value.into_bytes()));
        }
        "json-parse" => {
            let value = pop(stack)?;
            let TypedValue::String(source) = value else {
                return Err(VmDiagnostic::error(
                    "E-JSON-001",
                    DiagnosticPhase::Interpretation,
                    "json-parse requires a string",
                    Some(origin),
                ));
            };
            let result = match serde_json::from_str::<serde_json::Value>(&source) {
                Ok(value) => TypedValue::Result {
                    ok_type: Type::Json,
                    error_type: Type::String,
                    is_ok: true,
                    value: Box::new(TypedValue::Json(value)),
                },
                Err(error) => TypedValue::Result {
                    ok_type: Type::Json,
                    error_type: Type::String,
                    is_ok: false,
                    value: Box::new(TypedValue::String(error.to_string())),
                },
            };
            stack.push(result);
        }
        "json-stringify" => {
            let value = pop(stack)?;
            let TypedValue::Json(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-JSON-002",
                    DiagnosticPhase::Interpretation,
                    "json-stringify requires a JSON value",
                    Some(origin),
                ));
            };
            let text = serde_json::to_string(&value).map_err(|error| {
                VmDiagnostic::error(
                    "E-JSON-003",
                    DiagnosticPhase::Interpretation,
                    format!("could not serialize JSON: {error}"),
                    Some(SourceOrigin::generated(name)),
                )
            })?;
            stack.push(TypedValue::String(text));
        }
        "json-get" => {
            let key = pop(stack)?;
            let value = pop(stack)?;
            let (TypedValue::Json(value), TypedValue::String(key)) = (value, key) else {
                return Err(VmDiagnostic::error(
                    "E-JSON-004",
                    DiagnosticPhase::Interpretation,
                    "json-get requires a JSON value and string field name",
                    Some(origin),
                ));
            };
            let found = value.as_object().and_then(|object| object.get(&key)).cloned();
            stack.push(TypedValue::Option {
                inner_type: Type::Json,
                value: found.map(|value| Box::new(TypedValue::Json(value))),
            });
        }
        "json-index" => {
            let index = pop(stack)?;
            let value = pop(stack)?;
            let (TypedValue::Json(value), TypedValue::Int(index)) = (value, index) else {
                return Err(VmDiagnostic::error(
                    "E-JSON-006",
                    DiagnosticPhase::Interpretation,
                    "json-index requires a JSON value and integer index",
                    Some(origin),
                ));
            };
            let found = usize::try_from(index)
                .ok()
                .and_then(|index| value.as_array().and_then(|array| array.get(index)))
                .cloned();
            stack.push(TypedValue::Option {
                inner_type: Type::Json,
                value: found.map(|value| Box::new(TypedValue::Json(value))),
            });
        }
        "json-keys" => {
            let value = pop(stack)?;
            let TypedValue::Json(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-JSON-007",
                    DiagnosticPhase::Interpretation,
                    "json-keys requires a JSON value",
                    Some(origin),
                ));
            };
            let values = value
                .as_object()
                .map(|object| {
                    object
                        .keys()
                        .cloned()
                        .map(TypedValue::String)
                        .collect()
                })
                .unwrap_or_default();
            stack.push(TypedValue::List {
                element_type: Type::String,
                values,
            });
        }
        "json-as-string" | "json-as-int" | "json-as-bool" => {
            let value = pop(stack)?;
            let TypedValue::Json(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-JSON-005",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires a JSON value"),
                    Some(origin),
                ));
            };
            let (inner_type, value) = match name {
                "json-as-string" => (
                    Type::String,
                    value.as_str().map(|value| TypedValue::String(value.to_string())),
                ),
                "json-as-int" => (
                    Type::Int,
                    value.as_i64().map(TypedValue::Int),
                ),
                "json-as-bool" => (
                    Type::Bool,
                    value.as_bool().map(TypedValue::Bool),
                ),
                _ => unreachable!(),
            };
            stack.push(TypedValue::Option {
                inner_type,
                value: value.map(Box::new),
            });
        }
        "int-to-string" => {
            let value = pop(stack)?;
            let TypedValue::Int(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-012",
                    DiagnosticPhase::Interpretation,
                    "int-to-string requires an integer",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::String(value.to_string()));
        }
        "atoi" => {
            let value = pop(stack)?;
            let TypedValue::String(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-013",
                    DiagnosticPhase::Interpretation,
                    "atoi requires a string",
                    Some(origin),
                ));
            };
            let parsed = value.parse::<i64>().map_err(|_| {
                VmDiagnostic::error(
                    "E-PARSE-003",
                    DiagnosticPhase::Interpretation,
                    "atoi could not parse an integer",
                    Some(origin.clone()),
                )
            })?;
            stack.push(TypedValue::Int(parsed));
        }
        "space" => stack.push(TypedValue::String(" ".into())),
        // Source frontends lower `yield` to `Instruction::Yield`. Keeping a
        // no-op core implementation preserves compatibility for older cached
        // modules that encoded it as a normal call.
        "yield" => {}
        "some" => {
            let value = pop(stack)?;
            let inner_type = value.value_type();
            stack.push(TypedValue::Option {
                inner_type,
                value: Some(Box::new(value)),
            });
        }
        "none" => stack.push(TypedValue::Option {
            inner_type: Type::Dynamic,
            value: None,
        }),
        "is-some" => {
            let value = pop(stack)?;
            let TypedValue::Option { value, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-014",
                    DiagnosticPhase::Interpretation,
                    "is-some requires an option",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(value.is_some()));
        }
        "unwrap" => {
            let value = pop(stack)?;
            let TypedValue::Option { value, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-014",
                    DiagnosticPhase::Interpretation,
                    "unwrap requires an option",
                    Some(origin),
                ));
            };
            let Some(value) = value else {
                return Err(VmDiagnostic::error(
                    "E-OPTION-001",
                    DiagnosticPhase::Interpretation,
                    "cannot unwrap none",
                    Some(origin),
                ));
            };
            stack.push(*value);
        }
        "ok" | "err" => {
            let value = pop(stack)?;
            let value_type = value.value_type();
            stack.push(TypedValue::Result {
                ok_type: if name == "ok" {
                    value_type
                } else {
                    Type::Dynamic
                },
                error_type: if name == "err" {
                    value.value_type()
                } else {
                    Type::Dynamic
                },
                is_ok: name == "ok",
                value: Box::new(value),
            });
        }
        "is-ok" => {
            let value = pop(stack)?;
            let TypedValue::Result { is_ok, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-015",
                    DiagnosticPhase::Interpretation,
                    "is-ok requires a result",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Bool(is_ok));
        }
        "result-unwrap" | "result-error" => {
            let value = pop(stack)?;
            let TypedValue::Result { is_ok, value, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-015",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires a result"),
                    Some(origin),
                ));
            };
            let wanted_ok = name == "result-unwrap";
            if is_ok != wanted_ok {
                return Err(VmDiagnostic::error(
                    if wanted_ok {
                        "E-RESULT-001"
                    } else {
                        "E-RESULT-002"
                    },
                    DiagnosticPhase::Interpretation,
                    if wanted_ok {
                        "cannot unwrap an error result"
                    } else {
                        "cannot extract an error from an ok result"
                    },
                    Some(origin),
                ));
            }
            stack.push(*value);
        }
        "path" | "host-path" => {
            let value = pop(stack)?;
            let TypedValue::String(relative) = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-010",
                    DiagnosticPhase::Interpretation,
                    format!("{name} requires a string"),
                    Some(origin),
                ));
            };
            let selector_source = if name == "host-path" {
                "${host-machine}/**"
            } else {
                "./**"
            };
            let selector =
                super::effects::FileSelector::parse(selector_source).map_err(|error| {
                    VmDiagnostic::error(
                        "E-PATH-001",
                        DiagnosticPhase::Interpretation,
                        error.to_string(),
                        Some(SourceOrigin::generated(name)),
                    )
                })?;
            if !selector.matches(&relative) || relative.contains(['*', '?']) {
                return Err(VmDiagnostic::error(
                    "E-PATH-002",
                    DiagnosticPhase::Interpretation,
                    format!("{name} is not a normalized relative path inside its resource root"),
                    Some(SourceOrigin::generated(name)),
                ));
            }
            stack.push(TypedValue::Path { selector, relative });
        }
        "list-length" => {
            let value = pop(stack)?;
            let TypedValue::List { values, .. } = value else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-008",
                    DiagnosticPhase::Interpretation,
                    "list-length requires a list",
                    Some(origin),
                ));
            };
            stack.push(TypedValue::Int(values.len() as i64));
        }
        "list-get" => {
            let index = pop(stack)?;
            let list = pop(stack)?;
            let (TypedValue::List { values, .. }, TypedValue::Int(index)) = (list, index) else {
                return Err(VmDiagnostic::error(
                    "E-RUNTIME-009",
                    DiagnosticPhase::Interpretation,
                    "list-get requires a list and integer index",
                    Some(origin),
                ));
            };
            let index = usize::try_from(index).ok();
            let value = index
                .and_then(|index| values.get(index).cloned())
                .ok_or_else(|| {
                    VmDiagnostic::error(
                        "E-INDEX-001",
                        DiagnosticPhase::Interpretation,
                        "list index is out of bounds",
                        Some(SourceOrigin::generated(name)),
                    )
                })?;
            stack.push(value);
        }
        _ => {
            return Err(VmDiagnostic::error(
                "E-LINK-004",
                DiagnosticPhase::Linking,
                format!("core word '{name}' has no interpreter implementation"),
                Some(origin),
            ));
        }
    }
    Ok(())
}

fn runtime_underflow(origin: &SourceOrigin, trace: Vec<String>) -> VmDiagnostic {
    let mut diagnostic = VmDiagnostic::error(
        "E-STACK-005",
        DiagnosticPhase::Interpretation,
        "runtime stack underflow",
        Some(origin.clone()),
    );
    diagnostic.trace = trace;
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::ir::{BasicBlock, Function, LocatedInstruction, Module};
    use crate::vm::signature::{StackRow, StackSignature};
    use crate::vm::types::Type;
    use crate::vm::{core_vocabulary, Verifier};
    use std::collections::BTreeMap;

    fn arithmetic_module(instructions: Vec<Instruction>) -> VerifiedModule {
        let function = Function {
            name: "main".into(),
            documentation: None,
            signature: StackSignature::pure(
                StackRow::closed(Vec::new()),
                StackRow::closed(vec![Type::Int]),
            ),
            locals: Vec::new(),
            captures: Vec::new(),
            entry: 0,
            blocks: BTreeMap::from([(
                0,
                BasicBlock {
                    id: 0,
                    instructions: instructions
                        .into_iter()
                        .map(|instruction| LocatedInstruction::generated(instruction, "test"))
                        .collect(),
                },
            )]),
        };
        Verifier::new(&core_vocabulary())
            .verify(Module::single(function))
            .unwrap()
    }

    #[test]
    fn option_and_result_words_preserve_typed_values() {
        let mut stack = vec![TypedValue::Int(7)];
        execute_core("some", &mut stack).unwrap();
        execute_core("is-some", &mut stack).unwrap();
        assert_eq!(stack, vec![TypedValue::Bool(true)]);

        let mut stack = vec![TypedValue::String("failure".into())];
        execute_core("err", &mut stack).unwrap();
        assert!(matches!(
            stack.as_slice(),
            [TypedValue::Result { is_ok: false, .. }]
        ));
        execute_core("is-ok", &mut stack).unwrap();
        assert_eq!(stack, vec![TypedValue::Bool(false)]);

        let mut stack = vec![TypedValue::Int(9)];
        execute_core("ok", &mut stack).unwrap();
        execute_core("result-unwrap", &mut stack).unwrap();
        assert_eq!(stack, vec![TypedValue::Int(9)]);

        let mut stack = vec![TypedValue::String("bad".into())];
        execute_core("err", &mut stack).unwrap();
        let error = execute_core("result-unwrap", &mut stack).unwrap_err();
        assert_eq!(error.code, "E-RESULT-001");
    }

    #[test]
    fn every_pure_core_word_has_an_interpreter_implementation() {
        let vocabulary = core_vocabulary();
        for (name, signature) in vocabulary {
            if !signature.effects.is_pure() {
                continue;
            }
            let mut stack = Vec::new();
            if let Err(error) = execute_core(&name, &mut stack) {
                assert_ne!(
                    error.code, "E-LINK-004",
                    "pure vocabulary word {name} has no interpreter implementation"
                );
            }
        }
    }

    #[test]
    fn executes_verified_arithmetic_transactionally() {
        let module = arithmetic_module(vec![
            Instruction::Constant {
                value: TypedValue::Int(3),
            },
            Instruction::Constant {
                value: TypedValue::Int(4),
            },
            Instruction::Call {
                function: "+".into(),
            },
            Instruction::Return,
        ]);
        let mut stack = Vec::new();
        Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap();
        assert_eq!(stack, vec![TypedValue::Int(7)]);
    }

    #[test]
    fn runtime_failure_rolls_back_stack() {
        let module = arithmetic_module(vec![
            Instruction::Constant {
                value: TypedValue::Int(1),
            },
            Instruction::Constant {
                value: TypedValue::Int(0),
            },
            Instruction::Call {
                function: "/".into(),
            },
            Instruction::Return,
        ]);
        let mut stack = vec![TypedValue::String("existing".into())];
        let error = Interpreter::new(&module, DenyCapabilities, InterpreterConfig::default())
            .execute(&mut stack)
            .unwrap_err();
        assert_eq!(error.code, "E-NUM-001");
        assert_eq!(stack, vec![TypedValue::String("existing".into())]);
    }

    #[test]
    fn trampoline_yields_output_and_keeps_the_rest_as_vm_state() {
        let module = crate::vm::frontend::forth::compile_forth(
            "stream.forth",
            "s\"before\" say 2 3 + int-to-string say",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let trampoline = VmTrampoline::new(&module, &InterpreterConfig::default());
        let first = trampoline.run(trampoline.start(Vec::new()).unwrap());
        let VmStep::Emit {
            effect,
            continuation,
            ..
        } = first
        else {
            panic!("first say must yield an output event");
        };
        assert_eq!(effect.sequence, 0);
        assert!(matches!(effect.event, HostSideEffect::Emit { ref text } if text == "before"));
        assert_eq!(continuation.frames.len(), 1);

        let second = trampoline.run(continuation);
        let VmStep::Emit {
            effect,
            continuation,
            ..
        } = second
        else {
            panic!("second say must yield an output event");
        };
        assert_eq!(effect.sequence, 1);
        assert!(matches!(effect.event, HostSideEffect::Emit { ref text } if text == "5"));
        let complete = trampoline.run(continuation);
        assert!(matches!(complete, VmStep::Complete { stack } if stack.is_empty()));
    }

    #[test]
    fn trampoline_yields_capability_request_then_resumes_with_typed_values() {
        let module = crate::vm::frontend::forth::compile_forth(
            "await.forth",
            "s\"Cargo.toml\" path file-read",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let trampoline = VmTrampoline::new(&module, &InterpreterConfig::default());
        let pending = trampoline.run(trampoline.start(Vec::new()).unwrap());
        let VmStep::Await {
            effect,
            continuation,
            ..
        } = pending
        else {
            panic!("file read must yield a capability request");
        };
        assert_eq!(effect.sequence, 0);
        assert_eq!(effect.requirement.capability, CapabilityKind::FileRead);
        assert!(matches!(
            effect.event,
            HostSideEffect::Request { ref arguments }
                if matches!(arguments.as_slice(), [TypedValue::Path { .. }])
        ));
        let complete = trampoline.resume(continuation, vec![TypedValue::Bytes(vec![1, 2, 3])]);
        let VmStep::Complete { stack } = complete else {
            panic!("resuming a file read must complete the program");
        };
        assert_eq!(stack, vec![TypedValue::Bytes(vec![1, 2, 3])]);
    }

    #[test]
    fn parameterized_network_and_process_requirements_are_concrete_at_the_boundary() {
        let network = CapabilityRequirement {
            capability: CapabilityKind::NetworkConnect,
            selector: ResourceSelector::NetworkTemplate {
                template: crate::vm::effects::NetworkSelectorTemplate {
                    host_argument: 0,
                    port_argument: 1,
                    allowed_hosts: vec!["example.test".into()],
                    allowed_ports: vec![443],
                },
            },
        };
        assert_eq!(
            instantiate_requirement(
                &network,
                &[
                    TypedValue::String("example.test".into()),
                    TypedValue::Int(443)
                ],
            )
            .unwrap()
            .selector,
            ResourceSelector::Network {
                host: "example.test".into(),
                ports: vec![443],
            }
        );

        let process = CapabilityRequirement {
            capability: CapabilityKind::ProcessRun,
            selector: ResourceSelector::ProcessTemplate {
                template: crate::vm::effects::ProcessSelectorTemplate {
                    executable_argument: 0,
                    allowed_executables: vec!["git".into()],
                },
            },
        };
        assert_eq!(
            instantiate_requirement(&process, &[TypedValue::String("git".into())])
                .unwrap()
                .selector,
            ResourceSelector::Process {
                executables: vec!["git".into()],
            }
        );
    }

    #[test]
    fn source_yield_suspends_without_exposing_a_continuation_value() {
        let module = crate::vm::frontend::forth::compile_forth(
            "yield.forth",
            "1 yield 2 +",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let trampoline = VmTrampoline::new(&module, &InterpreterConfig::default());
        let yielded = trampoline.run(trampoline.start(Vec::new()).unwrap());
        let VmStep::Yielded { continuation } = yielded else {
            panic!("yield must return control to the event loop");
        };
        assert_eq!(continuation.stack, vec![TypedValue::Int(1)]);
        let complete = trampoline.run(continuation);
        assert!(
            matches!(complete, VmStep::Complete { stack } if stack == vec![TypedValue::Int(3)])
        );
    }

    #[test]
    fn lisp_yield_is_a_statement_expression_with_unit_type() {
        let module = crate::vm::frontend::lisp::compile_lisp(
            "yield.lisp",
            "(begin (yield) (+ 1 2))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let trampoline = VmTrampoline::new(&module, &InterpreterConfig::default());
        let VmStep::Yielded { continuation } =
            trampoline.run(trampoline.start(Vec::new()).unwrap())
        else {
            panic!("Lisp yield must return control to the event loop");
        };
        let complete = trampoline.run(continuation);
        assert!(
            matches!(complete, VmStep::Complete { stack } if stack == vec![TypedValue::Int(3)])
        );
    }
}
