//! The vocabulary of child-agent execution, and the capability the runtime is handed.
//!
//! These types are the boundary between a typed program that asks for a child agent and whatever
//! runs one. The runtime names them; it does not implement the scheduling, because scheduling a
//! child agent means choosing a provider and a model, and a runtime that constructs providers
//! cannot be embedded by anything that has its own.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use finch_vm::EffectSet;

// Bounds a child-agent request is checked against; they belong with the types that enforce them.
pub const MAX_DEPTH: usize = 4;
pub const MAX_TURNS: usize = 10;
pub const MAX_TIMEOUT_MS: u64 = 60 * 60 * 1000;
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_CONTEXT_REFERENCES: usize = 64;
pub const MAX_CONTEXT_FIELD_BYTES: usize = 1024;
pub const MAX_CONTEXT_ARTIFACT_BYTES: usize = 64 * 1024;
pub const MAX_CONTEXT_TOTAL_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    General,
    Explore,
    Research,
    Code,
}

impl Default for AgentRole {
    fn default() -> Self {
        Self::General
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentBudget {
    pub max_turns: usize,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self {
            max_turns: MAX_TURNS,
            timeout_ms: 120_000,
            max_output_bytes: 256 * 1024,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentContextReference {
    pub kind: String,
    pub id: String,
    pub sha256: String,
}

impl AgentContextReference {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [("kind", &self.kind), ("id", &self.id)] {
            if value.trim().is_empty() {
                bail!("agent context reference {name} cannot be empty");
            }
            if value.len() > MAX_CONTEXT_FIELD_BYTES {
                bail!("agent context reference {name} exceeds {MAX_CONTEXT_FIELD_BYTES} bytes");
            }
        }
        if self.sha256.len() != 64 || !self.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("agent context reference sha256 must be exactly 64 hexadecimal digits");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskSpec {
    pub task: String,
    #[serde(default)]
    pub role: AgentRole,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub context: Vec<AgentContextReference>,
    /// `None` inherits the caller's full creation-time ceiling (the compact
    /// `agent-spawn` convenience). `Some`, including an empty set, is an
    /// explicit selection of live opaque grant references.
    #[serde(default)]
    pub capability_grant_ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub budget: AgentBudget,
}

impl AgentTaskSpec {
    pub fn validate(&self) -> Result<()> {
        if self.task.trim().is_empty() {
            bail!("agent task cannot be empty");
        }
        if self.context.len() > MAX_CONTEXT_REFERENCES {
            bail!("agent context has more than {MAX_CONTEXT_REFERENCES} references");
        }
        for reference in &self.context {
            reference.validate()?;
        }
        if !(1..=MAX_TURNS).contains(&self.budget.max_turns) {
            bail!("agent max_turns must be between 1 and {MAX_TURNS}");
        }
        if !(1..=MAX_TIMEOUT_MS).contains(&self.budget.timeout_ms) {
            bail!("agent timeout_ms must be between 1 and {MAX_TIMEOUT_MS}");
        }
        if !(1..=MAX_OUTPUT_BYTES).contains(&self.budget.max_output_bytes) {
            bail!("agent max_output_bytes must be between 1 and {MAX_OUTPUT_BYTES}");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent_id: Uuid,
    pub task_id: Uuid,
    pub parent_agent_id: Option<Uuid>,
    pub root_agent_id: Uuid,
    pub depth: usize,
    pub provider_model: String,
    pub vm_revision: u64,
    pub manifest_generation: u64,
    pub starting_context_hash: String,
    /// Inherited authority fixed when this child is created. Later
    /// session/project/global grants cannot silently widen a live child;
    /// an exact task-scoped user approval remains an explicit escalation.
    #[serde(default)]
    pub grant_ceiling: EffectSet,
    /// Canonical durable run for this child when it was spawned from a named
    /// Brain turn/program. Absent for legacy local-only agent tasks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// The durable run this agent belongs to, if a host is recording one. Held as a plain id
    /// because what a run *is* belongs to the host, not to the runtime.
    pub brain_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Truthfulness of provider-reported token usage across a child's attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUsageState {
    /// Every started provider attempt reported both input and output tokens.
    Complete,
    /// Some usage was reported, but at least one attempt or token direction is missing.
    Partial,
    /// No started provider attempt reported token usage.
    Unavailable,
}

/// Provider-reported usage for a child task. Token fields are absent when that
/// direction has never been reported; Finch never estimates a missing value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentUsage {
    /// Whether the known totals cover every started provider attempt.
    pub state: AgentUsageState,
    /// Sum of provider-reported input tokens, absent when never reported.
    pub input_tokens: Option<u64>,
    /// Sum of provider-reported output tokens, absent when never reported.
    pub output_tokens: Option<u64>,
    /// Attempts that reported at least one token direction.
    pub reported_attempts: usize,
    /// Provider invocations that actually began.
    pub started_attempts: usize,
}

impl Default for AgentUsage {
    fn default() -> Self {
        Self {
            state: AgentUsageState::Unavailable,
            input_tokens: None,
            output_tokens: None,
            reported_attempts: 0,
            started_attempts: 0,
        }
    }
}

/// One authoritative active-task entry used to rebuild a lagged frontend projection.
#[derive(Debug, Clone)]
pub struct AgentActivitySnapshot {
    /// Current task identity, description, role, and lifecycle state.
    pub task: AgentTaskSnapshot,
    /// Provider-reported usage known when the snapshot was taken.
    pub usage: AgentUsage,
    /// Tool currently executing, if any.
    pub active_tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskResult {
    pub identity: AgentIdentity,
    pub status: AgentTaskStatus,
    pub final_message: String,
    pub diagnostics: Vec<String>,
    pub turns: usize,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskSnapshot {
    pub identity: AgentIdentity,
    pub task: String,
    pub role: AgentRole,
    pub status: AgentTaskStatus,
    pub result: Option<AgentTaskResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Authoritative active children used to replace a lagged frontend projection.
    #[serde(skip)]
    Resnapshot {
        active: Vec<AgentActivitySnapshot>,
    },
    TaskQueued {
        snapshot: AgentTaskSnapshot,
    },
    TaskStarted {
        snapshot: AgentTaskSnapshot,
    },
    /// Provider-attempt or reported-usage progress for an active child.
    UsageUpdated {
        task_id: Uuid,
        usage: AgentUsage,
    },
    ToolStarted {
        task_id: Uuid,
        name: String,
    },
    ToolCompleted {
        task_id: Uuid,
        name: String,
        is_error: bool,
    },
    TaskFinished {
        result: AgentTaskResult,
        usage: AgentUsage,
    },
}

/// Somewhere a typed program's child agents can be run.
///
/// Implemented above the runtime, by whatever owns providers and models, and handed down as a
/// `dyn` so this module never names it.
#[async_trait::async_trait]
pub trait AgentSpawning: Send + Sync {
    async fn spawn(
        &self,
        spec: AgentTaskSpec,
        parent: Option<&AgentIdentity>,
    ) -> Result<AgentIdentity>;

    /// Refuse a task that the caller is not the parent of, before anything is read or changed.
    async fn authorize(&self, task_id: Uuid, parent: Option<&AgentIdentity>) -> Result<()>;

    async fn poll(&self, task_id: Uuid) -> Result<AgentTaskSnapshot>;
    async fn wait(&self, task_id: Uuid) -> Result<AgentTaskResult>;
    async fn cancel(&self, task_id: Uuid) -> Result<()>;
}

/// The spawner a runtime has before a host attaches one.
///
/// `Weak::new()` needs a concrete type even when the slot is empty, and this says plainly what an
/// unattached runtime does with a request for a child agent: refuses it.
pub struct NoAgentSpawning;

#[async_trait::async_trait]
impl AgentSpawning for NoAgentSpawning {
    async fn spawn(
        &self,
        _spec: AgentTaskSpec,
        _parent: Option<&AgentIdentity>,
    ) -> Result<AgentIdentity> {
        bail!("no agent scheduler is attached to this runtime")
    }

    async fn authorize(&self, _task_id: Uuid, _parent: Option<&AgentIdentity>) -> Result<()> {
        bail!("no agent scheduler is attached to this runtime")
    }

    async fn poll(&self, _task_id: Uuid) -> Result<AgentTaskSnapshot> {
        bail!("no agent scheduler is attached to this runtime")
    }

    async fn wait(&self, _task_id: Uuid) -> Result<AgentTaskResult> {
        bail!("no agent scheduler is attached to this runtime")
    }

    async fn cancel(&self, _task_id: Uuid) -> Result<()> {
        bail!("no agent scheduler is attached to this runtime")
    }
}
