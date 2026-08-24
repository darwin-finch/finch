use crate::programs::{ExecutionEffect, ProgramValue};
use crate::vm::{
    interpreter::{HostSideEffect, VmSideEffect},
    ApprovalPrompt, CapabilityRequirement, EffectJournalEntry, VmDiagnostic,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Completed,
    Suspended,
    AuthorizationRequired,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackend {
    TypedVm,
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
    /// Versioned, harness-neutral event envelopes emitted by the typed VM.
    /// `side_effects` is Finch's legacy projection; consumers that need
    /// sequence/capability/origin data should use this field.
    #[serde(default)]
    pub vm_side_effects: Vec<VmSideEffect>,
    /// Durable state of each portable effect. This lets callers distinguish a
    /// pending approval from an acknowledged prefix when the VM transaction
    /// later fails.
    #[serde(default)]
    pub effect_journal: Vec<EffectJournalEntry>,
    pub diagnostics: Vec<String>,
    #[serde(default)]
    pub vm_diagnostics: Vec<VmDiagnostic>,
    /// Complete statically inferred capability envelope for the submitted
    /// program, including effects on branches not taken at runtime.
    #[serde(default)]
    pub inferred_capabilities: Vec<CapabilityRequirement>,
    /// The subset of inferred capabilities currently missing from the run's
    /// grants. This is empty for an authorized completed execution.
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
            vm_side_effects: Vec::new(),
            effect_journal: Vec::new(),
            diagnostics: vec![diagnostic.into()],
            vm_diagnostics: Vec::new(),
            inferred_capabilities: Vec::new(),
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
