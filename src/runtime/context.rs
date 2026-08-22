use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identity and resource limits attached to one VM execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionContext {
    pub execution_id: Uuid,
    pub manifest_generation: u64,
    pub budget: ExecutionBudget,
}

impl ExecutionContext {
    pub fn new(manifest_generation: u64, budget: ExecutionBudget) -> Self {
        Self {
            execution_id: Uuid::new_v4(),
            manifest_generation,
            budget,
        }
    }
}

/// Hard limits applied before an execution result is returned to a provider.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExecutionBudget {
    pub forth_fuel: usize,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_values: usize,
}

impl Default for ExecutionBudget {
    fn default() -> Self {
        Self {
            forth_fuel: 1_000_000,
            timeout_ms: 30_000,
            max_output_bytes: 256 * 1024,
            max_values: 1_024,
        }
    }
}
