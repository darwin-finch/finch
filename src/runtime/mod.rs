//! Provider-neutral execution service for Finch's Forth and Lisp VMs.

pub mod agent_vm;
pub mod automation;
pub mod context;
pub mod outcome;
pub mod scheduler;

use crate::coforth::{Forth, Library};
use crate::lisp::{self, EnvRef, LispCtx, Val};
use crate::programs::{ExecutionEffect, ProgramLanguage, ProgramValue};
use crate::vm::{
    ApprovalPrompt, CapabilityRequest, CapabilityRequirement, EffectSet, SourceOrigin, Type,
    TypedExecutionStatus, TypedRuntime, TypedValue, VmDiagnostic,
};
use anyhow::{bail, Result};
use automation::AutomationBroker;
use automation::AutomationRequest;
use context::{ExecutionBudget, ExecutionContext};
use outcome::{ExecutionBackend, ExecutionOutcome, ExecutionStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, RwLock, Weak};
use std::time::Instant;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmVocabularyEntry {
    pub name: String,
    pub signature: Option<String>,
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
    forth: Arc<Mutex<Forth>>,
    typed: Arc<Mutex<TypedRuntime>>,
    lisp_env: EnvRef,
    lisp_ctx: Arc<LispCtx>,
    revision: Arc<AtomicU64>,
    manifest_generation: AtomicU64,
    submission_gate: tokio::sync::Mutex<()>,
    automation: Arc<AutomationBroker>,
    workspace_root: Arc<PathBuf>,
    memory: RwLock<Option<Arc<crate::memory::MemorySystem>>>,
    network: Arc<Mutex<HashMap<String, TcpStream>>>,
    agent_scheduler: RwLock<Weak<scheduler::AgentScheduler>>,
}

impl ProgramRuntime {
    pub fn new() -> Self {
        Self::with_automation(false)
    }

    pub fn with_automation(enabled: bool) -> Self {
        let automation = Arc::new(AutomationBroker::new(enabled));
        let lisp_ctx = Arc::new(LispCtx::with_automation(Arc::clone(&automation)));
        let mut forth = Library::precompiled_vm();
        forth.set_automation_broker(Arc::clone(&automation));
        Self {
            forth: Arc::new(Mutex::new(forth)),
            typed: Arc::new(Mutex::new(TypedRuntime::new())),
            lisp_env: lisp::make_env(),
            lisp_ctx,
            revision: Arc::new(AtomicU64::new(0)),
            manifest_generation: AtomicU64::new(1),
            submission_gate: tokio::sync::Mutex::new(()),
            automation,
            workspace_root: Arc::new(
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            memory: RwLock::new(None),
            network: Arc::new(Mutex::new(HashMap::new())),
            agent_scheduler: RwLock::new(Weak::new()),
        }
    }

    pub fn automation(&self) -> Arc<AutomationBroker> {
        Arc::clone(&self.automation)
    }

    /// Attach the host's MemTree service to the typed capability boundary.
    /// Keeping this explicit prevents a VM from accidentally acquiring a
    /// second memory database or an ambient memory authority.
    pub fn attach_memory(&self, memory: Arc<crate::memory::MemorySystem>) {
        *self.memory.write().expect("memory binding lock poisoned") = Some(memory);
    }

    /// Grant a typed capability after an approval decision. The next
    /// submission is still checked against this structured grant.
    pub fn grant_typed_capability(&self, requirement: CapabilityRequirement) -> Result<()> {
        self.typed
            .lock()
            .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))?
            .grant(requirement);
        Ok(())
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
        let forth = Arc::clone(&self.forth);
        let typed = Arc::clone(&self.typed);
        let revision = Arc::clone(&self.revision);
        let manifest_generation = self.manifest_generation();
        tokio::task::spawn_blocking(move || {
            let forth = forth
                .lock()
                .map_err(|_| anyhow::anyhow!("Forth VM lock poisoned"))?;
            let revision = revision.load(Ordering::Acquire);
            let mut stack: Vec<VmStackCell> = forth
                .data_stack()
                .iter()
                .copied()
                .enumerate()
                .map(|(index_from_bottom, value)| VmStackCell {
                    index_from_bottom,
                    type_name: "int".to_string(),
                    value: ProgramValue::Int(value),
                })
                .collect();
            let vocabulary = forth
                .vocabulary_snapshot()
                .into_iter()
                .map(|(name, signature)| VmVocabularyEntry { name, signature })
                .collect();
            drop(forth);
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
            if stack.is_empty() {
                stack = typed_stack
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
            }
            let typed_vocabulary = typed
                .vocabulary()
                .iter()
                .map(|(name, signature)| VmVocabularyEntry {
                    name: name.clone(),
                    signature: Some(signature.to_string()),
                })
                .collect();
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
        self.submit_as(submission, None).await
    }

    pub async fn submit_as(
        &self,
        submission: ProgramSubmission,
        caller: Option<scheduler::AgentIdentity>,
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
        let required_effect = required_effect(submission.language, &submission.source);
        if !effect_allows(submission.effect, required_effect) {
            bail!(
                "declared effect '{}' does not cover derived effect '{}'",
                submission.effect.as_str(),
                required_effect.as_str()
            );
        }
        let vm_local = matches!(
            submission.effect,
            ExecutionEffect::Pure | ExecutionEffect::VmRead | ExecutionEffect::VmWrite
        );
        let enabled_automation = self.automation.is_enabled()
            && matches!(
                submission.effect,
                ExecutionEffect::ExternalRead | ExecutionEffect::ExternalWrite
            )
            && is_automation_only_source(submission.language, &submission.source);
        let enabled_typed_files = matches!(
            submission.effect,
            ExecutionEffect::WorkspaceRead | ExecutionEffect::WorkspaceWrite
        ) && is_typed_file_source(&submission.source);
        let enabled_typed_process = matches!(submission.effect, ExecutionEffect::ExternalWrite)
            && submission
                .source
                .to_ascii_lowercase()
                .contains("process-run");
        let enabled_typed_network = matches!(submission.effect, ExecutionEffect::ExternalWrite)
            && submission.source.to_ascii_lowercase().contains("network-");
        if !vm_local
            && !enabled_automation
            && !enabled_typed_files
            && !enabled_typed_process
            && !enabled_typed_network
        {
            bail!(
                "effect '{}' is not enabled in the initial VM runtime",
                submission.effect.as_str()
            );
        }

        let context = ExecutionContext::new(generation, submission.budget.unwrap_or_default());
        let started = Instant::now();
        if let Some(execution) = self
            .execute_typed_program(
                submission.language,
                &submission.source,
                &context,
                &submission.declared_capabilities,
                caller.clone(),
            )
            .await?
        {
            let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            return Ok(match execution.status {
                TypedExecutionStatus::Completed => {
                    let output_revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
                    ExecutionOutcome {
                        execution_id: context.execution_id,
                        status: ExecutionStatus::Completed,
                        values: typed_values(execution.values)?,
                        output: truncate_output(execution.output, context.budget.max_output_bytes),
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
                TypedExecutionStatus::AuthorizationRequired { requirements } => ExecutionOutcome {
                    execution_id: context.execution_id,
                    status: ExecutionStatus::AuthorizationRequired,
                    values: Vec::new(),
                    output: String::new(),
                    diagnostics: Vec::new(),
                    vm_diagnostics: execution.diagnostics,
                    approval_prompts: approval_prompts(
                        context.execution_id,
                        &requirements,
                        &submission.source,
                        &submission.intent,
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
                    output: String::new(),
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
            });
        }
        let result = match submission.language {
            ProgramLanguage::Forth => self
                .execute_forth(&submission.source, &context, caller.clone())
                .await
                .map(|(values, output)| (values, output, ExecutionBackend::Forth)),
            ProgramLanguage::Lisp => {
                self.execute_lisp(&submission.source, &context, caller)
                    .await
            }
        };
        let elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;

        match result {
            Ok((values, output, backend)) => {
                // Revision is an execution/state generation, conservatively
                // incremented after every successful submission. This makes
                // positional stack composition safe even for words whose stack
                // mutation cannot yet be proven statically.
                let output_revision = self.revision.fetch_add(1, Ordering::AcqRel) + 1;
                Ok(ExecutionOutcome {
                    execution_id: context.execution_id,
                    status: ExecutionStatus::Completed,
                    values,
                    output,
                    diagnostics: Vec::new(),
                    vm_diagnostics: Vec::new(),
                    required_capabilities: Vec::new(),
                    approval_prompts: Vec::new(),
                    input_revision,
                    output_revision,
                    effect: submission.effect,
                    backend,
                    elapsed_ms,
                })
            }
            Err(error) => Ok(ExecutionOutcome::failed(
                context.execution_id,
                input_revision,
                submission.effect,
                match submission.language {
                    ProgramLanguage::Forth => ExecutionBackend::Forth,
                    ProgramLanguage::Lisp => ExecutionBackend::LispNative,
                },
                error.to_string(),
                elapsed_ms,
            )),
        }
    }

    async fn execute_typed_program(
        &self,
        language: ProgramLanguage,
        source: &str,
        context: &ExecutionContext,
        declared_capabilities: &[CapabilityRequirement],
        caller: Option<scheduler::AgentIdentity>,
    ) -> Result<Option<crate::vm::TypedExecution>> {
        let runtime = Arc::clone(&self.typed);
        let automation = Arc::clone(&self.automation);
        let workspace_root = Arc::clone(&self.workspace_root);
        let memory = self
            .memory
            .read()
            .expect("memory binding lock poisoned")
            .clone();
        let network = Arc::clone(&self.network);
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
        let execution = tokio::task::spawn_blocking(move || {
            runtime
                .lock()
                .map_err(|_| anyhow::anyhow!("typed VM lock poisoned"))
                .map(|mut runtime| {
                    let vocabulary = serde_json::to_string(runtime.vocabulary())
                        .unwrap_or_else(|_| "[]".to_string());
                    if automation.is_enabled()
                        && matches!(language, ProgramLanguage::Forth | ProgramLanguage::Lisp)
                    {
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AutomationInspect,
                            selector: crate::vm::ResourceSelector::Automation { application: None },
                        });
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AutomationWrite,
                            selector: crate::vm::ResourceSelector::Automation { application: None },
                        });
                    }
                    if scheduler.is_some() {
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AgentSpawn,
                            selector: crate::vm::ResourceSelector::None,
                        });
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AgentAwait,
                            selector: crate::vm::ResourceSelector::None,
                        });
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AgentPoll,
                            selector: crate::vm::ResourceSelector::None,
                        });
                        runtime.grant(CapabilityRequirement {
                            capability: crate::vm::CapabilityKind::AgentCancel,
                            selector: crate::vm::ResourceSelector::None,
                        });
                    }
                    let mut handler = TypedHostHandler::new(
                        Arc::clone(&automation),
                        Arc::clone(&workspace_root),
                        scheduler,
                        memory,
                        vocabulary,
                        network,
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
        if matches!(execution.status, TypedExecutionStatus::Failed)
            && execution.diagnostics.iter().all(typed_frontend_unsupported)
        {
            Ok(None)
        } else {
            Ok(Some(execution))
        }
    }
}

struct TypedHostHandler {
    automation: Arc<AutomationBroker>,
    workspace_root: Arc<PathBuf>,
    output: String,
    scheduler: Option<agent_vm::AgentVmBinding>,
    memory: Option<Arc<crate::memory::MemorySystem>>,
    vocabulary: String,
    network: Arc<Mutex<HashMap<String, TcpStream>>>,
}

impl TypedHostHandler {
    fn new(
        automation: Arc<AutomationBroker>,
        workspace_root: Arc<PathBuf>,
        scheduler: Option<agent_vm::AgentVmBinding>,
        memory: Option<Arc<crate::memory::MemorySystem>>,
        vocabulary: String,
        network: Arc<Mutex<HashMap<String, TcpStream>>>,
    ) -> Self {
        Self {
            automation,
            workspace_root,
            output: String::new(),
            scheduler,
            memory,
            vocabulary,
            network,
        }
    }
}

impl crate::vm::interpreter::CapabilityHandler for TypedHostHandler {
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
                self.output.push_str(text);
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
                let [TypedValue::Path { relative, selector }] = arguments.as_slice() else {
                    return Err(host_binding_error(origin, "file-read requires one path"));
                };
                let path = secure_workspace_path(&self.workspace_root, selector, relative)
                    .map_err(|message| host_binding_error(origin, message))?;
                let bytes = std::fs::read(path)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                return Ok(vec![TypedValue::Bytes(bytes)]);
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
                let path = secure_workspace_path(&self.workspace_root, selector, relative)
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
                return Ok(vec![TypedValue::Task(identity.task_id.to_string())]);
            }
            crate::vm::CapabilityKind::AgentAwait => {
                let [TypedValue::Task(task_id)] = arguments.as_slice() else {
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
                let [TypedValue::Task(task_id)] = arguments.as_slice() else {
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
                let [TypedValue::Task(task_id)] = arguments.as_slice() else {
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
                        .insert(handle.clone(), stream);
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
                let stream = sockets
                    .get_mut(handle)
                    .ok_or_else(|| host_binding_error(origin, "unknown network socket"))?;
                stream
                    .write_all(payload)
                    .map_err(|error| host_binding_error(origin, error.to_string()))?;
                let mut response = vec![0; 4096];
                let size = stream
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
        let value = self
            .automation
            .execute(request)
            .map_err(|error| host_binding_error(origin, error.to_string()))?;
        Ok(vec![TypedValue::String(value.to_string())])
    }

    fn output(&self) -> String {
        self.output.clone()
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

fn secure_workspace_path(
    root: &PathBuf,
    selector: &crate::vm::FileSelector,
    relative: &str,
) -> std::result::Result<PathBuf, String> {
    if !selector.matches(relative) || relative.contains(['*', '?']) {
        return Err("path is outside its declared selector".to_string());
    }
    let root = root
        .canonicalize()
        .map_err(|error| format!("workspace root is unavailable: {error}"))?;
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
        return Err("path escapes the workspace root".to_string());
    }
    Ok(check)
}

impl ProgramRuntime {
    async fn execute_forth(
        &self,
        source: &str,
        context: &ExecutionContext,
        caller: Option<scheduler::AgentIdentity>,
    ) -> Result<(Vec<ProgramValue>, String)> {
        let vm = Arc::clone(&self.forth);
        let source = source.to_string();
        let budget = context.budget;
        let binding = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, caller));
        tokio::task::spawn_blocking(move || {
            let mut vm = vm
                .lock()
                .map_err(|_| anyhow::anyhow!("Forth VM lock poisoned"))?;
            vm.set_agent_binding(binding);
            let before = vm.data_stack().len();
            let output = vm.exec_with_fuel(&source, budget.forth_fuel)?;
            let stack = vm.data_stack();
            let produced = &stack[before.min(stack.len())..];
            if produced.len() > budget.max_values {
                bail!("execution produced too many values");
            }
            Ok((
                produced.iter().copied().map(ProgramValue::Int).collect(),
                truncate_output(output, budget.max_output_bytes),
            ))
        })
        .await?
    }

    async fn execute_lisp(
        &self,
        source: &str,
        context: &ExecutionContext,
        caller: Option<scheduler::AgentIdentity>,
    ) -> Result<(Vec<ProgramValue>, String, ExecutionBackend)> {
        if let Ok(compiled) = crate::lisp::forth_compiler::compile_source(source) {
            let (values, output) = self
                .execute_forth(&compiled.forth_source, context, caller)
                .await?;
            return Ok((values, output, ExecutionBackend::LispCompiledToForth));
        }
        let binding = self
            .agent_scheduler
            .read()
            .expect("agent scheduler lock poisoned")
            .upgrade()
            .map(|scheduler| agent_vm::AgentVmBinding::new(&scheduler, caller));
        let lisp_ctx = Arc::new(self.lisp_ctx.with_agent(binding));
        let future = lisp::run_in(source, self.lisp_env.clone(), lisp_ctx);
        let value = tokio::time::timeout(
            std::time::Duration::from_millis(context.budget.timeout_ms),
            future,
        )
        .await
        .map_err(|_| anyhow::anyhow!("Lisp execution timed out"))??;
        Ok((
            lisp_values(value)?,
            String::new(),
            ExecutionBackend::LispNative,
        ))
    }
}

fn is_automation_only_source(language: ProgramLanguage, source: &str) -> bool {
    let normalized = source.to_ascii_lowercase();
    let has_automation = normalized.contains("automation-");
    if !has_automation {
        return false;
    }
    let forbidden = match language {
        ProgramLanguage::Forth => [
            "file-write",
            "file-append",
            "exec-capture",
            "applescript",
            "scatter",
            "publish",
            "quarantine",
        ]
        .as_slice(),
        ProgramLanguage::Lisp => {
            ["ssh-connect", "ssh-exec", "ssh-write-file", "ssh-read-file"].as_slice()
        }
    };
    !forbidden.iter().any(|word| normalized.contains(word))
}

fn is_typed_file_source(source: &str) -> bool {
    let source = source.to_ascii_lowercase();
    source.contains("file-read") || source.contains("file-write")
}

fn effect_allows(declared: ExecutionEffect, required: ExecutionEffect) -> bool {
    use ExecutionEffect::*;
    matches!(
        (declared, required),
        (Pure, Pure)
            | (VmRead, Pure | VmRead)
            | (VmWrite, Pure | VmRead | VmWrite)
            | (WorkspaceRead, Pure | VmRead | WorkspaceRead)
            | (ExternalRead, Pure | VmRead | ExternalRead)
            | (
                WorkspaceWrite,
                Pure | VmRead | VmWrite | WorkspaceRead | WorkspaceWrite
            )
            | (
                ExternalWrite,
                Pure | VmRead | VmWrite | ExternalRead | ExternalWrite
            )
            | (Destructive, _)
            | (Unclassified, _)
    )
}

fn required_effect(language: ProgramLanguage, source: &str) -> ExecutionEffect {
    let uses_typed_host_word = source.to_ascii_lowercase().contains("file-read")
        || source.to_ascii_lowercase().contains("file-write")
        || source.to_ascii_lowercase().contains("automation-");
    if language == ProgramLanguage::Lisp
        && !uses_typed_host_word
        && crate::lisp::forth_compiler::compile_source(source).is_ok()
    {
        return ExecutionEffect::Pure;
    }
    let normalized = source.to_ascii_lowercase();
    let contains_any = |words: &[&str]| words.iter().any(|word| normalized.contains(word));

    if contains_any(&[
        "automation-click",
        "automation-type",
        "file-write",
        "ssh-connect",
        "ssh-auth-key",
        "ssh-exec",
        "ssh-write-file",
        "file-append",
        "b64>file",
        "xlsx-write-cell",
        "applescript",
        "exec-capture",
        "quarantine",
        "zip-send",
        "zip-recv",
        "scatter",
        "publish",
        "join-registry",
        "leave-registry",
    ]) {
        return ExecutionEffect::ExternalWrite;
    }
    if contains_any(&[
        "automation-displays",
        "automation-windows",
        "ssh-read-file",
        "ssh-info",
        "scan-procs",
        "scan-net",
        "scan-startup",
        "peers-discover",
    ]) {
        return ExecutionEffect::ExternalRead;
    }
    if contains_any(&["agent-spawn", "agent-cancel"]) {
        return ExecutionEffect::VmWrite;
    }
    if contains_any(&["mem-store", "mem-consolidate"]) {
        return ExecutionEffect::VmWrite;
    }
    if contains_any(&["mem-recall", "mem-read"]) {
        return ExecutionEffect::VmRead;
    }
    if contains_any(&["process-run"]) {
        return ExecutionEffect::ExternalWrite;
    }
    if contains_any(&["network-connect", "network-send"]) {
        return ExecutionEffect::ExternalWrite;
    }
    if contains_any(&["agent-poll", "agent-await"]) {
        return ExecutionEffect::VmRead;
    }
    if contains_any(&[
        "file-fetch",
        "file-read",
        "file-slice",
        "file-size",
        "file-sha256",
        "glob-pool",
        "glob-count",
        "scan-file",
        "scan-dir",
        "scan-strings",
        "xlsx@",
        "xlsx-sheets",
        "(load ",
    ]) {
        return ExecutionEffect::WorkspaceRead;
    }
    if contains_any(&["(define ", "(set! ", "variable ", "constant ", ": "]) {
        return ExecutionEffect::VmWrite;
    }
    ExecutionEffect::Pure
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

fn lisp_values(value: Val) -> Result<Vec<ProgramValue>> {
    match value {
        Val::Nil => Ok(Vec::new()),
        Val::List(values) => values.into_iter().map(lisp_value).collect(),
        other => Ok(vec![lisp_value(other)?]),
    }
}

fn lisp_value(value: Val) -> Result<ProgramValue> {
    Ok(match value {
        Val::Nil => ProgramValue::Nil,
        Val::Bool(value) => ProgramValue::Bool(value),
        Val::Int(value) => ProgramValue::Int(value),
        Val::Float(value) => ProgramValue::Float(value),
        Val::Str(value) | Val::Symbol(value) => ProgramValue::String(value),
        Val::Bytes(value) => ProgramValue::Bytes(value),
        Val::List(values) => ProgramValue::List(
            values
                .into_iter()
                .map(lisp_value)
                .collect::<Result<Vec<_>>>()?,
        ),
        other => bail!("Lisp value is not portable: {other}"),
    })
}

fn approval_prompts(
    execution_id: uuid::Uuid,
    requirements: &[CapabilityRequirement],
    source: &str,
    intent: &str,
) -> Vec<ApprovalPrompt> {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let program_hash = format!("{:016x}", hasher.finish());
    requirements
        .iter()
        .cloned()
        .map(|requirement| {
            ApprovalPrompt::for_request(CapabilityRequest {
                id: uuid::Uuid::new_v4(),
                execution_id,
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

fn typed_frontend_unsupported(diagnostic: &VmDiagnostic) -> bool {
    matches!(
        diagnostic.code.as_str(),
        "E-LINK-002"
            | "E-NAME-001"
            | "E-TYPE-005"
            | "E-LISP-DEF-002"
            | "E-FORTH-SIG-001"
            | "E-FORTH-DEF-003"
    )
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
        TypedValue::String(value) => ProgramValue::String(value),
        TypedValue::Bytes(value) => ProgramValue::Bytes(value),
        TypedValue::List { values, .. } => ProgramValue::List(typed_values(values)?),
        TypedValue::Task(value) => ProgramValue::Task(value),
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
    async fn forth_executes_directly_and_persists_state() {
        let runtime = ProgramRuntime::new();
        let define = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                ": runtime-double 2 * ;",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        assert_eq!(define.status, ExecutionStatus::Completed);
        let call = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "21 runtime-double",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(call.values, vec![ProgramValue::Int(42)]);
    }

    #[tokio::test]
    async fn lisp_executes_directly_and_persists_state() {
        let runtime = ProgramRuntime::new();
        runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(define runtime-n 40)",
                ExecutionEffect::VmWrite,
            ))
            .await
            .unwrap();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(+ runtime-n 2)",
                ExecutionEffect::VmRead,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.values, vec![ProgramValue::Int(42)]);
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
    async fn rejects_effects_not_yet_brokered() {
        let runtime = ProgramRuntime::new();
        let error = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" x\" file-fetch",
                ExecutionEffect::WorkspaceRead,
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not enabled"));
    }

    #[tokio::test]
    async fn source_cannot_hide_external_effect_behind_pure_declaration() {
        let runtime = ProgramRuntime::new();
        let error = runtime
            .submit(submission(
                ProgramLanguage::Forth,
                "s\" data\" s\" path\" file-write",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("derived effect"));
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
    async fn enabled_automation_is_available_through_typed_lisp() {
        let runtime = ProgramRuntime::with_automation(true);
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
            "(file-read (path \"Cargo.toml\"))",
            ExecutionEffect::WorkspaceRead,
        );
        let pending = runtime.submit(request.clone()).await.unwrap();
        assert_eq!(pending.status, ExecutionStatus::AuthorizationRequired);
        assert_eq!(pending.required_capabilities.len(), 1);
        runtime
            .grant_typed_capability(crate::vm::CapabilityRequirement::file(
                crate::vm::FileOperation::Read,
                crate::vm::FileSelector::parse("./**").unwrap(),
            ))
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert!(matches!(
            approved.values.first(),
            Some(ProgramValue::Bytes(_))
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
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.values, vec![ProgramValue::String("ok".into())]);
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
                selector: crate::vm::ResourceSelector::None,
            })
            .unwrap();
        let approved = runtime.submit(request).await.unwrap();
        assert_eq!(approved.status, ExecutionStatus::Completed);
        assert_eq!(approved.values, vec![ProgramValue::Bytes(b"pong".to_vec())]);
        server.join().unwrap();
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
        assert_eq!(state.stack[0].value, ProgramValue::Int(10));
        assert_eq!(state.stack[1].value, ProgramValue::Int(20));
        assert!(state.vocabulary.iter().any(|word| word.name == "+"));
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
}
