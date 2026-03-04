// EnterPlanMode - Tool for Claude to signal entering read-only planning mode

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

pub struct EnterPlanModeTool;

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Enter read-only planning mode to explore the codebase before making changes. \
         Use this when you need to research and develop an implementation plan. \
         In plan mode, only read-only tools (Read, Glob, Grep, WebFetch) and \
         AskUserQuestion are available. Use AskUserQuestion to clarify requirements \
         with the user. When ready, use PresentPlan to show your plan."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![(
            "reason",
            "Brief explanation of why planning is needed (optional)",
        )])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        use chrono::Utc;

        let reason = input["reason"].as_str().unwrap_or("Planning session");

        // Check if repl_mode is available
        let mode = context
            .repl_mode
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Plan mode not available in this context"))?;

        // Check if already in plan mode
        {
            let current_mode = mode.read().await;
            if matches!(
                *current_mode,
                crate::cli::ReplMode::Planning { .. } | crate::cli::ReplMode::Executing { .. }
            ) {
                let mode_name = match *current_mode {
                    crate::cli::ReplMode::Planning { .. } => "planning",
                    crate::cli::ReplMode::Executing { .. } => "executing",
                    _ => "unknown",
                };
                return Ok(format!(
                    "⚠️  Already in {} mode. Finish current task first.\n\
                     Use PresentPlan to show your plan, or ask the user to exit plan mode.",
                    mode_name
                ));
            }
        }

        // Create plans directory
        let plans_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Home directory not found"))?
            .join(".finch")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        // Generate plan filename
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to planning mode
        *mode.write().await = crate::cli::ReplMode::Planning {
            task: reason.to_string(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };

        Ok(format!(
            "✅ Entered planning mode.\n\n\
             📋 Task: {}\n\
             📁 Plan file: {}\n\n\
             Available tools: Read, Glob, Grep, WebFetch, AskUserQuestion\n\
             Blocked: Bash, Write, Edit\n\n\
             ⚡ Be efficient: use the MINIMUM number of tool calls needed.\n\
             For simple tasks, 1-3 reads is enough. Do not read files speculatively.\n\
             When you have enough information, call PresentPlan immediately.",
            reason,
            plan_path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute() {
        let tool = EnterPlanModeTool;
        use crate::cli::ReplMode;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let repl_mode = Arc::new(RwLock::new(ReplMode::Normal));
        let plan_content = Arc::new(RwLock::new(None));

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: Some(repl_mode),
            plan_content: Some(plan_content),
            live_output: None,
        };

        let result = tool.execute(serde_json::json!({}), &context).await;
        assert!(result.is_ok());
        let message = result.unwrap();
        assert!(message.contains("Entered planning mode"));
        assert!(message.contains("Read"));
    }

    #[test]
    fn test_name() {
        let tool = EnterPlanModeTool;
        assert_eq!(tool.name(), "EnterPlanMode");
    }
}
