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
         AskUserQuestion are available. When ready, use PresentPlan to show your plan."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![(
            "reason",
            "Brief explanation of why planning is needed (optional)"
        )])
    }

    async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
        // TODO: Set plan mode state in ToolExecutor
        // For now, just return informational message
        Ok(
            "✅ Entered plan mode.\n\n\
             Available tools:\n\
             • Read - Read file contents\n\
             • Glob - Find files by pattern\n\
             • Grep - Search file contents\n\
             • WebFetch - Fetch documentation\n\
             • AskUserQuestion - Ask clarifying questions\n\n\
             When ready to propose changes, use PresentPlan to show your implementation plan.\n\n\
             ⚠️  Tools like Write, Edit, and Bash are not available in plan mode."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute() {
        let tool = EnterPlanModeTool;
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
        assert!(result.is_ok());
        let message = result.unwrap();
        assert!(message.contains("Entered plan mode"));
        assert!(message.contains("Read"));
    }

    #[test]
    fn test_name() {
        let tool = EnterPlanModeTool;
        assert_eq!(tool.name(), "EnterPlanMode");
    }
}
