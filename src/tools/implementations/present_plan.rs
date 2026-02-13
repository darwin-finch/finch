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

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        // Extract and validate plan
        let plan_content = input["plan"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'plan' field in input"))?;

        if plan_content.trim().is_empty() {
            bail!("Plan content cannot be empty");
        }

        // TODO: Show approval dialog at event loop level
        // TODO: Handle approval/rejection/feedback
        // TODO: Clear context on approval

        // For now, just acknowledge the plan was presented
        Ok(format!(
            "📋 **Implementation Plan Presented**\n\n\
             {}\n\n\
             ⚠️  **Note:** Plan approval dialog will be shown to the user.\n\
             (Dialog integration pending - Phase 2 in progress)\n\n\
             Next steps:\n\
             • User will approve, request changes, or reject\n\
             • If approved: context cleared, all tools enabled\n\
             • If changes requested: revise plan and call PresentPlan again\n\
             • If rejected: exit plan mode",
            plan_content
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_with_plan() {
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
