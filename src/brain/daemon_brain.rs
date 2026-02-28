// Daemon Brain Loop
//
// Runs a background research brain in the daemon process. Unlike the REPL brain
// (which pre-gathers context while the user types), the daemon brain:
//
//   1. Investigates a user-assigned task autonomously.
//   2. Asks the user questions via AskUserQuestion (pauses until REPL answers).
//   3. Presents a final plan via PresentPlan (pauses until REPL approves/rejects).
//   4. Survives REPL disconnects (the daemon keeps the brain alive).

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::claude::types::{ContentBlock, Message};
use crate::providers::{LlmProvider, ProviderRequest};
use crate::server::brain_registry::{BrainRegistry, BrainState, PlanResponse};
use crate::tools::implementations::glob::GlobTool;
use crate::tools::implementations::grep::GrepTool;
use crate::tools::implementations::read::ReadTool;
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolDefinition, ToolInputSchema, ToolUse};

/// Maximum turns the daemon brain may run (more than REPL brain — research is thorough).
const DAEMON_BRAIN_MAX_TURNS: usize = 12;

/// System prompt for the daemon brain.
fn daemon_brain_system_prompt(task: &str, cwd: &str) -> String {
    format!(
        "You are a background research agent running in the Finch daemon.\n\
         Task: {task}\n\n\
         Use the available tools to investigate this task thoroughly. When you need \
         information from the user, call ask_user_question. When you have a complete \
         plan ready, call present_plan — the user will approve, request changes, or \
         reject.\n\n\
         Available tools: read, glob, grep, ask_user_question, present_plan.\n\
         Max turns: {max_turns}. Summarise findings in your plan.\n\
         Working directory: {cwd}",
        task = task,
        cwd = cwd,
        max_turns = DAEMON_BRAIN_MAX_TURNS,
    )
}

/// Run the daemon brain loop as a tokio task.
///
/// Called by the spawn handler in `handlers.rs` after inserting the entry.
pub async fn run_daemon_brain_loop(
    id: Uuid,
    task: String,
    registry: Arc<BrainRegistry>,
    provider: Arc<dyn LlmProvider>,
    cwd: String,
) {
    info!("Daemon brain {} starting: {}", id, task);
    match run_loop(id, &task, Arc::clone(&registry), provider.as_ref(), &cwd).await {
        Ok(()) => {
            info!("Daemon brain {} finished", id);
        }
        Err(e) => {
            warn!("Daemon brain {} error: {}", id, e);
            registry
                .append_log(id, format!("[Error] {}", e))
                .await;
        }
    }
    registry.set_dead(id).await;
}

async fn run_loop(
    id: Uuid,
    task: &str,
    registry: Arc<BrainRegistry>,
    provider: &dyn LlmProvider,
    cwd: &str,
) -> Result<()> {
    let system = daemon_brain_system_prompt(task, cwd);

    // Build tool set: read/glob/grep + ask_user_question + present_plan
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(ReadTool),
        Box::new(GlobTool),
        Box::new(GrepTool),
        Box::new(DaemonAskUserTool {
            brain_id: id,
            registry: Arc::clone(&registry),
        }),
        Box::new(DaemonPresentPlanTool {
            brain_id: id,
            registry: Arc::clone(&registry),
        }),
    ];
    let tool_defs: Vec<ToolDefinition> = tools.iter().map(|t| t.definition()).collect();

    let mut messages: Vec<Message> = vec![Message::user(task)];

    for turn in 0..DAEMON_BRAIN_MAX_TURNS {
        debug!("Daemon brain {} turn {}/{}", id, turn + 1, DAEMON_BRAIN_MAX_TURNS);

        // Check if the brain has been cancelled externally (state = Dead).
        {
            if let Some(detail) = registry.get_detail(id).await {
                if detail.state == BrainState::Dead {
                    debug!("Daemon brain {} cancelled externally", id);
                    return Ok(());
                }
            }
        }

        let request = ProviderRequest::new(messages.clone())
            .with_system(system.clone())
            .with_max_tokens(4096)
            .with_tools(tool_defs.clone());

        let response = provider.send_message(&request).await?;

        registry
            .append_log(id, format!("[turn {}] {}", turn + 1, response.text()))
            .await;

        if !response.has_tool_uses() {
            // No more tool calls — brain finished without presenting a plan.
            info!(
                "Daemon brain {} finished without presenting a plan after {} turns",
                id,
                turn + 1
            );
            return Ok(());
        }

        messages.push(response.to_message());

        let tool_uses = response.tool_uses();
        let mut result_blocks: Vec<ContentBlock> = Vec::with_capacity(tool_uses.len());

        for tool_use in &tool_uses {
            debug!("Daemon brain {} calling tool: {}", id, tool_use.name);

            // Log tool call
            registry
                .append_log(id, format!("[tool] {}", tool_use.name))
                .await;

            let (content, is_error) = execute_daemon_brain_tool(&tools, tool_use).await;
            let content_preview = &content[..content.len().min(200)];
            registry.append_log(id, format!("[result] {}", content_preview)).await;

            // If present_plan returned "PLAN_APPROVED" the loop can end.
            let plan_approved = tool_use.name == "present_plan" && content.starts_with("PLAN_APPROVED");
            let plan_rejected = tool_use.name == "present_plan" && content.starts_with("PLAN_REJECTED");

            result_blocks.push(ContentBlock::ToolResult {
                tool_use_id: tool_use.id.clone(),
                content: content.clone(),
                is_error: if is_error { Some(true) } else { None },
            });

            if plan_approved {
                messages.push(Message::with_content("user", result_blocks));
                return Ok(());
            }
            if plan_rejected {
                registry.append_log(id, "[Plan rejected by user]".to_string()).await;
                messages.push(Message::with_content("user", result_blocks));
                return Ok(());
            }
        }

        messages.push(Message::with_content("user", result_blocks));
    }

    anyhow::bail!("Daemon brain reached max turns ({})", DAEMON_BRAIN_MAX_TURNS)
}

/// Execute one tool in the daemon brain context (no permission checks).
async fn execute_daemon_brain_tool(
    tools: &[Box<dyn Tool>],
    tool_use: &ToolUse,
) -> (String, bool) {
    let Some(tool) = tools.iter().find(|t| t.name() == tool_use.name) else {
        return (format!("Tool '{}' not available in daemon brain", tool_use.name), true);
    };

    let context = ToolContext {
        conversation: None,
        save_models: None,
        batch_trainer: None,
        local_generator: None,
        tokenizer: None,
        repl_mode: None,
        plan_content: None,
        live_output: None,
    };

    match tool.execute(tool_use.input.clone(), &context).await {
        Ok(s) => (s, false),
        Err(e) => (format!("Error: {}", e), true),
    }
}

// ---------------------------------------------------------------------------
// Custom tool: ask_user_question (daemon variant)
// ---------------------------------------------------------------------------

/// Tool that blocks the daemon brain loop until the REPL POSTs an answer.
struct DaemonAskUserTool {
    brain_id: Uuid,
    registry: Arc<BrainRegistry>,
}

#[async_trait]
impl Tool for DaemonAskUserTool {
    fn name(&self) -> &str {
        "ask_user_question"
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question. The brain will pause until the user answers."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of choices for the user"
                }
            }),
            required: vec!["question".to_string()],
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<String> {
        let question = input["question"]
            .as_str()
            .unwrap_or("?")
            .to_string();
        let options: Vec<String> = input["options"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();

        let (tx, rx) = oneshot::channel::<String>();
        self.registry
            .set_waiting_for_input(self.brain_id, question.clone(), options, tx)
            .await;

        // Block until the REPL posts an answer (or brain is cancelled).
        match rx.await {
            Ok(answer) => Ok(answer),
            Err(_) => anyhow::bail!("Brain was cancelled while waiting for answer"),
        }
    }
}

// ---------------------------------------------------------------------------
// Custom tool: present_plan (daemon variant)
// ---------------------------------------------------------------------------

/// Tool that presents a final plan to the user and blocks until they respond.
struct DaemonPresentPlanTool {
    brain_id: Uuid,
    registry: Arc<BrainRegistry>,
}

#[async_trait]
impl Tool for DaemonPresentPlanTool {
    fn name(&self) -> &str {
        "present_plan"
    }

    fn description(&self) -> &str {
        "Present a plan to the user for approval. Pauses until the user approves, requests changes, or rejects."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "plan": {
                    "type": "string",
                    "description": "The plan content (markdown)"
                }
            }),
            required: vec!["plan".to_string()],
        }
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<String> {
        let plan = input["plan"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let (tx, rx) = oneshot::channel::<PlanResponse>();
        self.registry
            .set_plan_ready(self.brain_id, plan, tx)
            .await;

        match rx.await {
            Ok(PlanResponse::Approve) => Ok("PLAN_APPROVED".to_string()),
            Ok(PlanResponse::Reject) => Ok("PLAN_REJECTED".to_string()),
            Ok(PlanResponse::ChangesRequested { feedback }) => {
                Ok(format!(
                    "The user requested changes: {}\n\nPlease revise the plan and call present_plan again.",
                    feedback
                ))
            }
            Err(_) => anyhow::bail!("Brain was cancelled while waiting for plan response"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::Tool;

    #[test]
    fn test_daemon_ask_user_tool_name() {
        let registry = Arc::new(BrainRegistry::new());
        let tool = DaemonAskUserTool {
            brain_id: Uuid::new_v4(),
            registry,
        };
        assert_eq!(tool.name(), "ask_user_question");
        assert_eq!(tool.definition().name, "ask_user_question");
    }

    #[test]
    fn test_daemon_present_plan_tool_name() {
        let registry = Arc::new(BrainRegistry::new());
        let tool = DaemonPresentPlanTool {
            brain_id: Uuid::new_v4(),
            registry,
        };
        assert_eq!(tool.name(), "present_plan");
        assert_eq!(tool.definition().name, "present_plan");
    }

    #[test]
    fn test_daemon_brain_system_prompt_includes_task_and_cwd() {
        let prompt = daemon_brain_system_prompt("investigate cargo slowness", "/Users/test");
        assert!(prompt.contains("investigate cargo slowness"));
        assert!(prompt.contains("/Users/test"));
    }

    #[tokio::test]
    async fn test_ask_user_tool_blocks_until_answered() {
        let registry = Arc::new(BrainRegistry::new());
        let id = Uuid::new_v4();
        registry.insert(id, "test".to_string()).await;

        let tool = DaemonAskUserTool {
            brain_id: id,
            registry: Arc::clone(&registry),
        };

        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let registry_clone = Arc::clone(&registry);
        let exec_handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({"question": "What file?", "options": ["a.rs", "b.rs"]}),
                &ctx,
            )
            .await
        });

        // Give the tool time to register as waiting
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        // Answer from "REPL"
        registry_clone.answer_question(id, "a.rs".to_string()).await.unwrap();

        let result = exec_handle.await.unwrap().unwrap();
        assert_eq!(result, "a.rs");
    }

    #[tokio::test]
    async fn test_present_plan_tool_approve() {
        let registry = Arc::new(BrainRegistry::new());
        let id = Uuid::new_v4();
        registry.insert(id, "test".to_string()).await;

        let tool = DaemonPresentPlanTool {
            brain_id: id,
            registry: Arc::clone(&registry),
        };

        let ctx = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
            live_output: None,
        };
        let registry_clone = Arc::clone(&registry);
        let exec_handle = tokio::spawn(async move {
            tool.execute(serde_json::json!({"plan": "Do the thing"}), &ctx).await
        });

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        registry_clone
            .respond_to_plan(id, PlanResponse::Approve)
            .await
            .unwrap();

        let result = exec_handle.await.unwrap().unwrap();
        assert_eq!(result, "PLAN_APPROVED");
    }
}
