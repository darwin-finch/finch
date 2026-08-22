//! Provider-neutral execution service for Finch's Forth and Lisp VMs.

pub mod agent_vm;
pub mod automation;
pub mod context;
pub mod outcome;
pub mod scheduler;

use crate::coforth::{Forth, Library};
use crate::lisp::{self, EnvRef, LispCtx, Val};
use crate::programs::{ExecutionEffect, ProgramLanguage, ProgramValue};
use anyhow::{bail, Result};
use automation::AutomationBroker;
use context::{ExecutionBudget, ExecutionContext};
use outcome::{ExecutionBackend, ExecutionOutcome, ExecutionStatus};
use serde::{Deserialize, Serialize};
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
pub struct VmStateSnapshot {
    pub manifest_generation: u64,
    pub revision: u64,
    pub stack: Vec<VmStackCell>,
    pub vocabulary: Vec<VmVocabularyEntry>,
}

/// One session's persistent language runtimes.
pub struct ProgramRuntime {
    forth: Arc<Mutex<Forth>>,
    lisp_env: EnvRef,
    lisp_ctx: Arc<LispCtx>,
    revision: Arc<AtomicU64>,
    manifest_generation: AtomicU64,
    submission_gate: tokio::sync::Mutex<()>,
    automation: Arc<AutomationBroker>,
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
            lisp_env: lisp::make_env(),
            lisp_ctx,
            revision: Arc::new(AtomicU64::new(0)),
            manifest_generation: AtomicU64::new(1),
            submission_gate: tokio::sync::Mutex::new(()),
            automation,
            agent_scheduler: RwLock::new(Weak::new()),
        }
    }

    pub fn automation(&self) -> Arc<AutomationBroker> {
        Arc::clone(&self.automation)
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
        let revision = Arc::clone(&self.revision);
        let manifest_generation = self.manifest_generation();
        tokio::task::spawn_blocking(move || {
            let forth = forth
                .lock()
                .map_err(|_| anyhow::anyhow!("Forth VM lock poisoned"))?;
            let revision = revision.load(Ordering::Acquire);
            let stack = forth
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
            Ok(VmStateSnapshot {
                manifest_generation,
                revision,
                stack,
                vocabulary,
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
        if !vm_local && !enabled_automation {
            bail!(
                "effect '{}' is not enabled in the initial VM runtime",
                submission.effect.as_str()
            );
        }

        let context = ExecutionContext::new(generation, submission.budget.unwrap_or_default());
        let started = Instant::now();
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
    if language == ProgramLanguage::Lisp
        && crate::lisp::forth_compiler::compile_source(source).is_ok()
    {
        return ExecutionEffect::Pure;
    }
    let normalized = source.to_ascii_lowercase();
    let contains_any = |words: &[&str]| words.iter().any(|word| normalized.contains(word));

    if contains_any(&[
        "automation-click",
        "automation-type",
        "ssh-connect",
        "ssh-auth-key",
        "ssh-exec",
        "ssh-write-file",
        "file-write",
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
    if contains_any(&["agent-poll", "agent-await"]) {
        return ExecutionEffect::VmRead;
    }
    if contains_any(&[
        "file-fetch",
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
    async fn portable_lisp_reports_forth_backend() {
        let runtime = ProgramRuntime::new();
        let outcome = runtime
            .submit(submission(
                ProgramLanguage::Lisp,
                "(+ 3 (* 4 2))",
                ExecutionEffect::Pure,
            ))
            .await
            .unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::LispCompiledToForth);
        assert_eq!(outcome.values, vec![ProgramValue::Int(11)]);
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
            .submit(submission(ProgramLanguage::Forth, "1", ExecutionEffect::Pure))
            .await
            .unwrap();
        let mut request = submission(ProgramLanguage::Forth, "2 +", ExecutionEffect::VmWrite);
        request.expected_revision = Some(0);
        let error = runtime.submit(request).await.unwrap_err();
        assert!(error.to_string().contains("stale VM revision"));
    }
}
