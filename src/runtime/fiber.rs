//! Bounded native workers for pure typed-VM functions.
//!
//! This is deliberately below the Lisp/Co-Forth surface: a future `defer
//! :cpu` form will create one of these handles, but the worker contract must
//! first be correct on its own. A fiber receives an immutable verified module,
//! explicit captures/arguments, and a private VM stack. It never aliases the
//! parent Brain stack or executes a host capability.

use crate::vm::interpreter::InterpreterConfig;
use crate::vm::{EffectSet, TypedValue, VerifiedModule, VmDiagnostic, VmStep, VmTrampoline};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFiberStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone)]
pub struct CpuFiberSnapshot {
    pub id: Uuid,
    pub status: CpuFiberStatus,
    pub result: Option<Vec<TypedValue>>,
    pub diagnostic: Option<VmDiagnostic>,
}

struct CpuFiberRecord {
    state: Mutex<CpuFiberSnapshot>,
    ready: Condvar,
    cancelled: AtomicBool,
}

/// A bounded scheduler for CPU-heavy, capability-free VM work. It creates at
/// most `max_workers` native threads at once; callers receive a stable UUID
/// that can later become a persistent `fiber<Y,R>` task value.
pub struct CpuFiberScheduler {
    max_workers: usize,
    active_workers: AtomicUsize,
    fibers: Mutex<HashMap<Uuid, Arc<CpuFiberRecord>>>,
}

impl CpuFiberScheduler {
    pub fn new(max_workers: usize) -> Self {
        Self {
            max_workers: max_workers.max(1),
            active_workers: AtomicUsize::new(0),
            fibers: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a pure function with only explicit input values and captures.
    /// Effects are rejected before a native worker exists, so a CPU fiber
    /// cannot reach files, processes, UI output, memory, or agent operations.
    pub fn spawn(
        self: &Arc<Self>,
        module: VerifiedModule,
        function: impl Into<String>,
        captures: Vec<TypedValue>,
        arguments: Vec<TypedValue>,
        fuel: u64,
    ) -> Result<Uuid> {
        let function = function.into();
        let definition = module
            .module
            .functions
            .get(&function)
            .ok_or_else(|| anyhow::anyhow!("unknown CPU fiber function '{function}'"))?;
        if !definition.signature.effects.is_pure() {
            bail!("CPU fiber function '{function}' is not pure");
        }
        let active = self.active_workers.fetch_add(1, Ordering::AcqRel) + 1;
        if active > self.max_workers {
            self.active_workers.fetch_sub(1, Ordering::AcqRel);
            bail!("CPU fiber worker limit ({}) reached", self.max_workers);
        }

        let id = Uuid::new_v4();
        let record = Arc::new(CpuFiberRecord {
            state: Mutex::new(CpuFiberSnapshot {
                id,
                status: CpuFiberStatus::Running,
                result: None,
                diagnostic: None,
            }),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
        });
        self.fibers
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber registry lock poisoned"))?
            .insert(id, Arc::clone(&record));

        let scheduler = Arc::clone(self);
        let thread = std::thread::Builder::new()
            .name(format!("finch-cpu-fiber-{id}"))
            .spawn(move || {
                run_fiber(record, module, function, captures, arguments, fuel);
                scheduler.active_workers.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(error) = thread {
            self.active_workers.fetch_sub(1, Ordering::AcqRel);
            self.fibers
                .lock()
                .map_err(|_| anyhow::anyhow!("CPU fiber registry lock poisoned"))?
                .remove(&id);
            bail!("could not start CPU fiber: {error}");
        }
        Ok(id)
    }

    /// Spawn a zero-argument closure exactly as represented by the typed VM.
    /// Its captures are copied into the child activation; no parent frame or
    /// data-stack reference crosses the worker boundary. The language-level
    /// `defer :cpu` form lowers to this operation after checking that the
    /// closure has no remaining positional arguments.
    pub fn spawn_closure(
        self: &Arc<Self>,
        module: VerifiedModule,
        closure: TypedValue,
        fuel: u64,
    ) -> Result<Uuid> {
        let TypedValue::Closure {
            function,
            captures,
            signature,
        } = closure
        else {
            bail!("CPU fiber requires a typed closure");
        };
        if !signature.input.values.is_empty() {
            bail!(
                "CPU fiber closure '{}' requires {} positional arguments; capture them in a zero-argument closure before deferring",
                function,
                signature.input.values.len()
            );
        }
        self.spawn(module, function, captures, Vec::new(), fuel)
    }

    pub fn poll(&self, id: Uuid) -> Result<CpuFiberSnapshot> {
        let record = self.record(id)?;
        let snapshot = record
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber state lock poisoned"))?
            .clone();
        Ok(snapshot)
    }

    /// Wait for terminal state. This blocks only a worker calling `join`, not
    /// Finch's UI/event-loop thread; the language-level join will instead
    /// suspend its VM continuation before calling this operation.
    pub fn join(&self, id: Uuid) -> Result<CpuFiberSnapshot> {
        let record = self.record(id)?;
        let mut state = record
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber state lock poisoned"))?;
        while state.status == CpuFiberStatus::Running {
            state = record
                .ready
                .wait(state)
                .map_err(|_| anyhow::anyhow!("CPU fiber state lock poisoned"))?;
        }
        Ok(state.clone())
    }

    /// Cancellation is cooperative. It prevents commit of a result and takes
    /// effect at a VM boundary; the native thread is never forcefully killed.
    pub fn cancel(&self, id: Uuid) -> Result<()> {
        let record = self.record(id)?;
        record.cancelled.store(true, Ordering::Release);
        Ok(())
    }

    fn record(&self, id: Uuid) -> Result<Arc<CpuFiberRecord>> {
        self.fibers
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber registry lock poisoned"))?
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown CPU fiber {id}"))
    }
}

fn run_fiber(
    record: Arc<CpuFiberRecord>,
    module: VerifiedModule,
    function: String,
    captures: Vec<TypedValue>,
    arguments: Vec<TypedValue>,
    fuel: u64,
) {
    let finish = |status, result, diagnostic| {
        let mut state = record.state.lock().expect("CPU fiber state lock poisoned");
        state.status = status;
        state.result = result;
        state.diagnostic = diagnostic;
        record.ready.notify_all();
    };
    if record.cancelled.load(Ordering::Acquire) {
        finish(CpuFiberStatus::Cancelled, None, None);
        return;
    }
    let trampoline = VmTrampoline::new(
        &module,
        &InterpreterConfig {
            fuel,
            grants: EffectSet::pure(),
        },
    );
    let continuation = match trampoline.start_function(&function, captures, arguments) {
        Ok(continuation) => continuation,
        Err(diagnostic) => {
            finish(CpuFiberStatus::Failed, None, Some(diagnostic));
            return;
        }
    };
    let mut step = trampoline.run(continuation);
    loop {
        if record.cancelled.load(Ordering::Acquire) {
            finish(CpuFiberStatus::Cancelled, None, None);
            return;
        }
        step = match step {
            VmStep::Yielded { continuation } => trampoline.run(continuation),
            VmStep::Complete { stack } => {
                finish(CpuFiberStatus::Completed, Some(stack), None);
                return;
            }
            VmStep::Failed(diagnostic) => {
                finish(CpuFiberStatus::Failed, None, Some(diagnostic));
                return;
            }
            VmStep::Emit { effect, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-FIBER-001",
                        crate::vm::DiagnosticPhase::HostCall,
                        "pure CPU fiber emitted a host event",
                        Some(effect.origin),
                    )),
                );
                return;
            }
            VmStep::Await { effect, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-FIBER-002",
                        crate::vm::DiagnosticPhase::HostCall,
                        "pure CPU fiber requested a host capability",
                        Some(effect.origin),
                    )),
                );
                return;
            }
            VmStep::SpawnCpuFiber { origin, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-FIBER-007",
                        crate::vm::DiagnosticPhase::HostCall,
                        "a CPU fiber cannot spawn another CPU fiber",
                        Some(origin),
                    )),
                );
                return;
            }
            VmStep::PollCpuFiber { origin, .. }
            | VmStep::JoinCpuFiber { origin, .. }
            | VmStep::CancelCpuFiber { origin, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-FIBER-018",
                        crate::vm::DiagnosticPhase::HostCall,
                        "a CPU fiber cannot operate on task handles",
                        Some(origin),
                    )),
                );
                return;
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{core_vocabulary, frontend::forth::compile_forth};

    #[test]
    fn pure_cpu_fiber_has_a_private_stack_and_returns_a_typed_result() {
        let module = compile_forth(
            "fiber.forth",
            ": square ( S int -- S int ! pure ) dup * ;",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        let id = scheduler
            .spawn(
                module,
                "square",
                Vec::new(),
                vec![TypedValue::Int(7)],
                1_000,
            )
            .unwrap();
        let result = scheduler.join(id).unwrap();
        assert_eq!(result.status, CpuFiberStatus::Completed);
        assert_eq!(result.result, Some(vec![TypedValue::Int(49)]));
    }

    #[test]
    fn cpu_fibers_reject_effectful_functions_before_spawning() {
        let module = compile_forth(
            "fiber.forth",
            ": announce ( S -- S ! infer ) s\" no\" say ;",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        assert!(scheduler
            .spawn(module, "announce", Vec::new(), Vec::new(), 1_000)
            .is_err());
    }

    #[test]
    fn deferred_closure_copies_captures_into_a_private_frame() {
        let module = crate::vm::frontend::lisp::compile_lisp(
            "fiber.lisp",
            "(let ((value 42)) (lambda () value))",
            Vec::new(),
            &core_vocabulary(),
        )
        .unwrap();
        let mut closure_stack = Vec::new();
        crate::vm::interpreter::Interpreter::new(
            &module,
            crate::vm::interpreter::DenyCapabilities,
            crate::vm::interpreter::InterpreterConfig::default(),
        )
        .execute(&mut closure_stack)
        .unwrap();
        let closure = closure_stack.pop().expect("lambda leaves one closure");
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        let id = scheduler.spawn_closure(module, closure, 1_000).unwrap();
        let result = scheduler.join(id).unwrap();
        assert_eq!(result.status, CpuFiberStatus::Completed);
        assert_eq!(result.result, Some(vec![TypedValue::Int(42)]));
    }
}
