use crate::programs::{ExecutionEffect, ProgramValue};
use crate::vm::{interpreter::HostSideEffect, ApprovalPrompt, CapabilityRequirement, VmDiagnostic};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Completed,
    AuthorizationRequired,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackend {
    Forth,
    TypedVm,
    LispCompiledToForth,
    LispNative,
}

/// Provider-neutral result of evaluating Forth or Lisp source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub execution_id: Uuid,
    pub status: ExecutionStatus,
    pub values: Vec<ProgramValue>,
    pub output: String,
    #[serde(default)]
    pub output_chunks: Vec<String>,
    #[serde(default)]
    pub side_effects: Vec<HostSideEffect>,
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub vm_diagnostics: Vec<VmDiagnostic>,
    #[serde(default)]
    pub required_capabilities: Vec<CapabilityRequirement>,
    #[serde(default)]
    pub approval_prompts: Vec<ApprovalPrompt>,
    pub input_revision: u64,
    pub output_revision: u64,
    pub effect: ExecutionEffect,
    pub backend: ExecutionBackend,
    pub elapsed_ms: u64,
}

impl ExecutionOutcome {
    pub fn failed(
        execution_id: Uuid,
        revision: u64,
        effect: ExecutionEffect,
        backend: ExecutionBackend,
        diagnostic: impl Into<String>,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            execution_id,
            status: ExecutionStatus::Failed,
            values: Vec::new(),
            output: String::new(),
            output_chunks: Vec::new(),
            side_effects: Vec::new(),
            diagnostics: vec![diagnostic.into()],
            vm_diagnostics: Vec::new(),
            required_capabilities: Vec::new(),
            approval_prompts: Vec::new(),
            input_revision: revision,
            output_revision: revision,
            effect,
            backend,
            elapsed_ms,
        }
    }
}
