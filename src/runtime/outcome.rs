use crate::programs::{ExecutionEffect, ProgramValue};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionBackend {
    Forth,
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
    pub diagnostics: Vec<String>,
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
            diagnostics: vec![diagnostic.into()],
            input_revision: revision,
            output_revision: revision,
            effect,
            backend,
            elapsed_ms,
        }
    }
}
