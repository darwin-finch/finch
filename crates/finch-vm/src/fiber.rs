//! Bounded native workers for pure typed-VM functions.
//!
//! This is deliberately below the Lisp/Co-Forth surface: a future `defer
//! :cpu` form will create one of these handles, but the worker contract must
//! first be correct on its own. A fiber receives an immutable verified module,
//! explicit captures/arguments, and a private VM stack. It never aliases the
//! parent Brain stack or executes a host capability.

use crate::interpreter::{InterpreterConfig, VmStep, VmTrampoline};
use crate::{EffectSet, TypedValue, VerifiedModule, VmDiagnostic};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};
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
    pub status: CpuFiberStatus,
    pub result: Option<Vec<TypedValue>>,
    pub diagnostic: Option<VmDiagnostic>,
}

struct CpuFiberRecord {
    state: Mutex<CpuFiberSnapshot>,
    ready: Condvar,
    cancelled: AtomicBool,
    owners: Mutex<HashSet<Uuid>>,
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

    fn spawn_with_owner(
        self: &Arc<Self>,
        module: VerifiedModule,
        function: impl Into<String>,
        captures: Vec<TypedValue>,
        arguments: Vec<TypedValue>,
        fuel: u64,
        owner: Option<Uuid>,
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
                status: CpuFiberStatus::Running,
                result: None,
                diagnostic: None,
            }),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
            owners: Mutex::new(owner.into_iter().collect()),
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

    /// Spawn a closure while atomically attaching its first private-runtime
    /// lease. This prevents another snapshot's cleanup from observing a new
    /// worker in the registry before its language handle has an owner.
    pub fn spawn_closure_owned(
        self: &Arc<Self>,
        module: VerifiedModule,
        closure: TypedValue,
        fuel: u64,
        owner: Uuid,
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
        self.spawn_with_owner(module, function, captures, Vec::new(), fuel, Some(owner))
    }

    /// Attach one cloned/private runtime snapshot to an existing task record.
    pub fn attach_owner(&self, id: Uuid, owner: Uuid) -> Result<()> {
        let fibers = self
            .fibers
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber registry lock poisoned"))?;
        let record = fibers
            .get(&id)
            .ok_or_else(|| anyhow::anyhow!("unknown CPU fiber {id}"))?;
        record
            .owners
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber owner lock poisoned"))?
            .insert(owner);
        Ok(())
    }

    /// Release one runtime snapshot's reference. The final release removes
    /// the deterministic tombstone; an unobserved running worker is also
    /// cooperatively cancelled while its thread-owned record finishes safely.
    pub fn release_owner(&self, id: Uuid, owner: Uuid) -> Result<bool> {
        let mut fibers = self
            .fibers
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber registry lock poisoned"))?;
        let Some(record) = fibers.get(&id).cloned() else {
            return Ok(false);
        };
        let mut owners = record
            .owners
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU fiber owner lock poisoned"))?;
        owners.remove(&owner);
        if !owners.is_empty() {
            return Ok(false);
        }
        record.cancelled.store(true, Ordering::Release);
        drop(owners);
        fibers.remove(&id);
        Ok(true)
    }

    #[cfg(test)]
    pub(crate) fn retained_count(&self) -> usize {
        self.fibers
            .lock()
            .expect("CPU fiber registry lock poisoned")
            .len()
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

    /// Test-only wait for terminal state. Production callers poll and suspend
    /// the owning VM continuation instead of blocking an event-loop thread.
    #[cfg(test)]
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
            VmStep::Yielded {
                value: TypedValue::Unit,
                continuation,
            } => trampoline.run(continuation),
            VmStep::Yielded { value, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-YIELD-003",
                        crate::DiagnosticPhase::Interpretation,
                        format!(
                            "CPU task cannot discard yielded {}; use a producer fiber",
                            value.value_type()
                        ),
                        Some(crate::SourceOrigin::generated("yield")),
                    )),
                );
                return;
            }
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
                        crate::DiagnosticPhase::HostCall,
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
                        crate::DiagnosticPhase::HostCall,
                        "pure CPU fiber requested a host capability",
                        Some(effect.origin),
                    )),
                );
                return;
            }
            VmStep::SpawnFiber { origin, .. }
            | VmStep::NextFiber { origin, .. }
            | VmStep::JoinFiber { origin, .. }
            | VmStep::CancelFiber { origin, .. } => {
                finish(
                    CpuFiberStatus::Failed,
                    None,
                    Some(VmDiagnostic::error(
                        "E-FIBER-033",
                        crate::DiagnosticPhase::HostCall,
                        "a CPU task cannot own or operate a cooperative producer",
                        Some(origin),
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
                        crate::DiagnosticPhase::HostCall,
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
                        crate::DiagnosticPhase::HostCall,
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
    use crate::{core_vocabulary, ModuleVerified, TypedExecutionStatus, TypedRuntime};
    use finch_language::compile_lisp;

    fn compile_closure(source: &str) -> (ModuleVerified, TypedValue) {
        let module = compile_lisp("fiber.lisp", source, Vec::new(), &core_vocabulary()).unwrap();
        let mut runtime = TypedRuntime::new();
        let execution = runtime.execute(&module, 1_000);
        assert_eq!(
            execution.status,
            TypedExecutionStatus::Completed,
            "production runtime must construct the CPU-fiber closure: source={source:?}, diagnostics={:?}",
            execution.diagnostics
        );
        let [closure @ TypedValue::Closure { .. }] = execution.values.as_slice() else {
            panic!(
                "closure fixture must leave exactly one closure: source={source:?}, values={:?}",
                execution.values
            );
        };
        (module, closure.clone())
    }

    #[test]
    fn pure_cpu_fiber_has_a_private_stack_and_returns_a_typed_result() {
        let (module, closure) = compile_closure("(let ((value 7)) (lambda () (* value value)))");
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        let owner = Uuid::new_v4();
        let id = scheduler
            .spawn_closure_owned(module.into_verified(), closure, 1_000, owner)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let snapshot = scheduler.poll(id).unwrap();
            if snapshot.status != CpuFiberStatus::Running {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pure CPU fiber did not reach terminal state"
            );
            std::thread::yield_now();
        };
        assert_eq!(result.status, CpuFiberStatus::Completed);
        assert_eq!(result.result, Some(vec![TypedValue::Int(49)]));
        assert!(scheduler.release_owner(id, owner).unwrap());
    }

    #[test]
    fn cpu_fibers_reject_effectful_functions_before_spawning() {
        let (module, closure) = compile_closure("(lambda () (say \"no\"))");
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        let error = scheduler
            .spawn_closure_owned(module.into_verified(), closure, 1_000, Uuid::new_v4())
            .expect_err("effectful closure must be rejected before a worker is spawned");
        assert!(
            error.to_string().contains("is not pure"),
            "effect rejection must identify the non-pure function: {error:#}"
        );
    }

    #[test]
    fn deferred_closure_copies_captures_into_a_private_frame() {
        let (module, closure) = compile_closure("(let ((value 42)) (lambda () value))");
        let scheduler = Arc::new(CpuFiberScheduler::new(1));
        let owner = Uuid::new_v4();
        let id = scheduler
            .spawn_closure_owned(module.into_verified(), closure, 1_000, owner)
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = loop {
            let snapshot = scheduler.poll(id).unwrap();
            if snapshot.status != CpuFiberStatus::Running {
                break snapshot;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "captured CPU fiber did not reach terminal state"
            );
            std::thread::yield_now();
        };
        assert_eq!(result.status, CpuFiberStatus::Completed);
        assert_eq!(result.result, Some(vec![TypedValue::Int(42)]));
        assert!(scheduler.release_owner(id, owner).unwrap());
    }
}
