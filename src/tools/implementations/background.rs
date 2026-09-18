// Background command tools — start, poll, and stop long-lived processes
// without blocking the turn (issue #754).
//
// The lifecycle lives in `crate::brain::BackgroundTaskManager`, owned beyond
// the turn: `background_bash` returns a stable task ID immediately, output
// accumulates in bounded ring buffers, and `background_poll` /
// `background_stop` operate on that handle later — across turn boundaries.
// Authority is the same propose/approval path as foreground bash.

use crate::brain::BackgroundTaskManager;
use crate::programs::ExecutionEffect;
use crate::tools::types::{ToolContext, ToolInputSchema};
use crate::tools::Tool;
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

use super::propose::{
    context_should_open_interactive_review, propose_with_decision, ProposalDecision,
};

/// Start a shell command in the background; returns a stable task ID.
pub struct BackgroundBashTool {
    tasks: Arc<BackgroundTaskManager>,
}

impl BackgroundBashTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for BackgroundBashTool {
    fn name(&self) -> &str {
        "background_bash"
    }

    /// Worst case: a background shell command can write outside the workspace,
    /// exactly like foreground bash. A read-only background command still
    /// spawns a long-lived process, so the read-only refinement that foreground
    /// bash earns at approval sites does not apply to this name.
    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Start a long-lived shell command (dev server, build, watch process, test suite) in the \
         background and return its task ID immediately instead of blocking the turn. Output is \
         captured in a bounded buffer; poll it later with background_poll and stop it with \
         background_stop. Use plain bash for commands whose result you need right away."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            ("command", "The bash command to run in the background"),
            ("description", "Brief description of what this command does"),
        ])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        let command = input["command"]
            .as_str()
            .context("Missing command parameter")?;
        let description = input["description"].as_str().unwrap_or("");

        // Same propose/approval path as foreground bash: the editor review is
        // the propose step; permission approval happens at the call site that
        // dispatched this tool.
        let script = if context_should_open_interactive_review(context).await {
            match propose_with_decision(description, command).await? {
                ProposalDecision::Execute { source } => source,
                ProposalDecision::Chat { context } => {
                    return Ok(format!(
                        "Tool call not executed. The user asked for a different command instead:\n{context}"
                    ))
                }
                ProposalDecision::Cancel => return Ok("Tool call aborted by user.".to_string()),
            }
        } else {
            command.to_string()
        };

        let id = self
            .tasks
            .start(&script, description)
            .await
            .context("Failed to start background task")?;
        Ok(format!(
            "Started background task {id}. The turn is not blocked: poll output and status with \
             background_poll using task_id \"{id}\", or stop it with background_stop."
        ))
    }
}

/// Poll a background task's status and captured output.
pub struct BackgroundPollTool {
    tasks: Arc<BackgroundTaskManager>,
}

impl BackgroundPollTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for BackgroundPollTool {
    fn name(&self) -> &str {
        "background_poll"
    }

    /// Polling reads this session's captured process output and mutates
    /// nothing on the host.
    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalRead
    }

    fn description(&self) -> &str {
        "Poll a background task started with background_bash: returns its current state (running, \
         completed with exit code, or stopped) and the captured stdout/stderr output retained by \
         the per-task buffer."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![("task_id", "The task ID returned by background_bash")])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task_id = input["task_id"]
            .as_str()
            .context("Missing task_id parameter")?;
        let snapshot = self
            .tasks
            .poll(task_id)
            .await
            .context("Failed to poll background task")?;
        Ok(snapshot.render())
    }
}

/// Stop a background task: kill its recorded process and reap it.
pub struct BackgroundStopTool {
    tasks: Arc<BackgroundTaskManager>,
}

impl BackgroundStopTool {
    pub fn new(tasks: Arc<BackgroundTaskManager>) -> Self {
        Self { tasks }
    }
}

#[async_trait]
impl Tool for BackgroundStopTool {
    fn name(&self) -> &str {
        "background_stop"
    }

    /// Stopping mutates host process state — it kills the recorded task
    /// process — which is within the same authority envelope as the bash
    /// tool that started it.
    fn effect(&self) -> ExecutionEffect {
        ExecutionEffect::ExternalWrite
    }

    fn description(&self) -> &str {
        "Stop a background task started with background_bash: kills its recorded process, reaps \
         it, and returns the final captured output and terminal status. Idempotent — stopping an \
         already-finished task reports its recorded result."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![("task_id", "The task ID returned by background_bash")])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let task_id = input["task_id"]
            .as_str()
            .context("Missing task_id parameter")?;
        let snapshot = self
            .tasks
            .stop(task_id)
            .await
            .context("Failed to stop background task")?;
        Ok(snapshot.render())
    }
}

#[cfg(test)]
mod tests;
