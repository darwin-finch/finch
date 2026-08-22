//! Structured provider tools for bounded child-agent fork/join.

use crate::runtime::scheduler::{
    AgentBudget, AgentIdentity, AgentRole, AgentScheduler, AgentTaskSpec,
};
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub struct AgentSpawnTool {
    scheduler: Arc<AgentScheduler>,
    parent: Option<AgentIdentity>,
}

impl AgentSpawnTool {
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self {
        Self {
            scheduler,
            parent: None,
        }
    }

    pub fn child(scheduler: Arc<AgentScheduler>, parent: AgentIdentity) -> Self {
        Self {
            scheduler,
            parent: Some(parent),
        }
    }
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn name(&self) -> &str {
        "spawn_agent"
    }

    fn description(&self) -> &str {
        "Fork a bounded child agent. Returns a structured identity immediately; use await_agent or poll_agent to join it. The child receives explicit parent/root identity, selected model, VM revision, and only the supplied task context."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: json!({
                "task": {"type": "string", "description": "Self-contained task for the child."},
                "role": {"type": "string", "enum": ["general", "explore", "research", "code"]},
                "background": {"type": "string", "description": "Optional bounded context copied into the child's initial message."},
                "provider": {"type": "string", "description": "Optional explicit provider selector."},
                "model": {"type": "string", "description": "Optional explicit model selector."},
                "max_turns": {"type": "integer", "minimum": 1, "maximum": 10},
                "timeout_ms": {"type": "integer", "minimum": 1},
                "max_output_bytes": {"type": "integer", "minimum": 1}
            }),
            required: vec!["task".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task = required_string(&input, "task")?;
        let role = match input
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("general")
        {
            "general" => AgentRole::General,
            "explore" => AgentRole::Explore,
            "research" => AgentRole::Research,
            "code" => AgentRole::Code,
            value => bail!("invalid agent role: {value}"),
        };
        let defaults = AgentBudget::default();
        let budget = AgentBudget {
            max_turns: usize_field(&input, "max_turns")?.unwrap_or(defaults.max_turns),
            timeout_ms: u64_field(&input, "timeout_ms")?.unwrap_or(defaults.timeout_ms),
            max_output_bytes: usize_field(&input, "max_output_bytes")?
                .unwrap_or(defaults.max_output_bytes),
        };
        let identity = self
            .scheduler
            .spawn(
                AgentTaskSpec {
                    task,
                    role,
                    background: optional_string(&input, "background")?,
                    provider: optional_string(&input, "provider")?,
                    model: optional_string(&input, "model")?,
                    budget,
                },
                self.parent.as_ref(),
            )
            .await?;
        Ok(serde_json::to_string(&identity)?)
    }
}

pub struct AgentAwaitTool {
    scheduler: Arc<AgentScheduler>,
    caller: Option<AgentIdentity>,
}

impl AgentAwaitTool {
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self {
        Self {
            scheduler,
            caller: None,
        }
    }

    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self {
        Self {
            scheduler,
            caller: Some(caller),
        }
    }
}

#[async_trait]
impl Tool for AgentAwaitTool {
    fn name(&self) -> &str {
        "await_agent"
    }

    fn description(&self) -> &str {
        "Join a child-agent task and return its structured terminal result."
    }

    fn input_schema(&self) -> ToolInputSchema {
        task_id_schema()
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task_id = parse_task_id(&input)?;
        self.scheduler
            .authorize(task_id, self.caller.as_ref())
            .await?;
        Ok(serde_json::to_string(&self.scheduler.wait(task_id).await?)?)
    }
}

pub struct AgentPollTool {
    scheduler: Arc<AgentScheduler>,
    caller: Option<AgentIdentity>,
}

impl AgentPollTool {
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self {
        Self {
            scheduler,
            caller: None,
        }
    }

    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self {
        Self {
            scheduler,
            caller: Some(caller),
        }
    }
}

#[async_trait]
impl Tool for AgentPollTool {
    fn name(&self) -> &str {
        "poll_agent"
    }

    fn description(&self) -> &str {
        "Read a child-agent task's current structured state without blocking."
    }

    fn input_schema(&self) -> ToolInputSchema {
        task_id_schema()
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task_id = parse_task_id(&input)?;
        self.scheduler
            .authorize(task_id, self.caller.as_ref())
            .await?;
        Ok(serde_json::to_string(&self.scheduler.poll(task_id).await?)?)
    }
}

pub struct AgentCancelTool {
    scheduler: Arc<AgentScheduler>,
    caller: Option<AgentIdentity>,
}

impl AgentCancelTool {
    pub fn new(scheduler: Arc<AgentScheduler>) -> Self {
        Self {
            scheduler,
            caller: None,
        }
    }

    pub fn child(scheduler: Arc<AgentScheduler>, caller: AgentIdentity) -> Self {
        Self {
            scheduler,
            caller: Some(caller),
        }
    }
}

#[async_trait]
impl Tool for AgentCancelTool {
    fn name(&self) -> &str {
        "cancel_agent"
    }

    fn description(&self) -> &str {
        "Request cancellation of a running child-agent task."
    }

    fn input_schema(&self) -> ToolInputSchema {
        task_id_schema()
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task_id = parse_task_id(&input)?;
        self.scheduler
            .authorize(task_id, self.caller.as_ref())
            .await?;
        self.scheduler.cancel(task_id).await?;
        Ok(json!({"task_id": task_id, "cancel_requested": true}).to_string())
    }
}

fn task_id_schema() -> ToolInputSchema {
    ToolInputSchema::simple(vec![("task_id", "UUID returned by spawn_agent")])
}

fn parse_task_id(input: &Value) -> Result<Uuid> {
    Uuid::parse_str(&required_string(input, "task_id")?).context("invalid task_id UUID")
}

fn required_string(input: &Value, field: &str) -> Result<String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("missing required '{field}' parameter"))
}

fn optional_string(input: &Value, field: &str) -> Result<Option<String>> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => bail!("'{field}' must be a string"),
    }
}

fn usize_field(input: &Value, field: &str) -> Result<Option<usize>> {
    u64_field(input, field)?
        .map(|value| usize::try_from(value).context(format!("'{field}' is too large")))
        .transpose()
}

fn u64_field(input: &Value, field: &str) -> Result<Option<u64>> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a positive integer")),
    }
}
