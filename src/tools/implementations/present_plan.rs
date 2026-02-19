// PresentPlan - Tool for Claude to present implementation plan for approval

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Result, bail};
use async_trait::async_trait;
use serde_json::Value;

pub struct PresentPlanTool;

#[async_trait]
impl Tool for PresentPlanTool {
    fn name(&self) -> &str {
        "PresentPlan"
    }

    fn description(&self) -> &str {
        "Present your implementation plan to the user for approval. \
         The plan should be detailed and include: what changes will be made, \
         which files will be modified, step-by-step execution order, and any risks. \
         The user can approve (context is cleared, all tools enabled), \
         request changes (you can revise the plan), or reject (exit plan mode). \
         Use this after exploring the codebase in plan mode."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![(
            "plan",
            "Detailed implementation plan in markdown format. Include: overview, affected files, step-by-step changes, testing, and risks"
        )])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        // Extract and validate plan
        let plan_content = input["plan"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'plan' field in input"))?;

        if plan_content.trim().is_empty() {
            bail!("Plan content cannot be empty");
        }

        // Check if repl_mode is available
        let mode = context.repl_mode.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Plan mode not available in this context"))?;

        // Verify we're in planning mode
        let plan_path = {
            let current_mode = mode.read().await;
            match &*current_mode {
                crate::cli::ReplMode::Planning { plan_path, .. } => plan_path.clone(),
                crate::cli::ReplMode::Normal => {
                    return Ok("⚠️  Not in planning mode. Use EnterPlanMode first.".to_string());
                }
                crate::cli::ReplMode::Executing { .. } => {
                    return Ok("⚠️  Already executing plan. Use /done to return to normal mode.".to_string());
                }
            }
        };

        // Store plan content
        if let Some(ref plan_storage) = context.plan_content {
            *plan_storage.write().await = Some(plan_content.to_string());
        }

        // Save plan to file
        std::fs::write(&plan_path, plan_content)?;

        // Return plan with approval instructions
        Ok(format!(
            "📋 **Implementation Plan Presented**\n\n\
             {}\n\n\
             ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n\n\
             ✓ Plan saved to: {}\n\n\
             **Waiting for User Approval**\n\n\
             The plan has been presented to the user. They will now:\n\
             • **Approve** it to proceed with execution (context cleared, all tools enabled)\n\
             • **Request changes** with feedback for you to revise\n\
             • **Reject** it to exit plan mode\n\n\
             ⏸️  You are in **read-only planning mode** until the user approves.\n\
             Only exploration tools (Read, Glob, Grep, WebFetch) are available.\n\n\
             If the user requests changes, revise the plan and call PresentPlan again with the updated version.",
            plan_content,
            plan_path.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_with_plan() {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        use crate::cli::ReplMode;
        use std::path::PathBuf;

        let tool = PresentPlanTool;

        // Set up repl_mode in Planning state
        let plan_path = PathBuf::from("/tmp/test-plan.md");
        let repl_mode = Arc::new(RwLock::new(ReplMode::Planning {
            task: "Test task".to_string(),
            plan_path: plan_path.clone(),
            created_at: chrono::Utc::now(),
        }));
        let plan_content = Arc::new(RwLock::new(None));

        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: Some(repl_mode),
            plan_content: Some(plan_content),
        };

        let result = tool
            .execute(
                serde_json::json!({
                    "plan": "## Plan\n1. Create file\n2. Write code\n3. Test"
                }),
                &context,
            )
            .await;

        assert!(result.is_ok());
        let message = result.unwrap();
        assert!(message.contains("Implementation Plan"));
        assert!(message.contains("Create file"));
    }

    #[tokio::test]
    async fn test_execute_missing_plan() {
        let tool = PresentPlanTool;
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

        let result = tool.execute(serde_json::json!({}), &context).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing 'plan'"));
    }

    #[tokio::test]
    async fn test_execute_empty_plan() {
        let tool = PresentPlanTool;
        let context = ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        };

        let result = tool
            .execute(serde_json::json!({"plan": "   "}), &context)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cannot be empty"));
    }

    #[test]
    fn test_name() {
        let tool = PresentPlanTool;
        assert_eq!(tool.name(), "PresentPlan");
    }
}
