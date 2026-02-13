// AskUserQuestion - Tool for Claude to ask the user clarifying questions
//
// Enables the LLM to display interactive dialogs and collect user input during
// task execution. Supports single-select, multi-select, and custom text input.

use crate::cli::llm_dialogs::{validate_input, AskUserQuestionInput, AskUserQuestionOutput};
use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

pub struct AskUserQuestionTool;

#[async_trait]
impl Tool for AskUserQuestionTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the user clarifying questions during task execution. \
         Use this when you need user input to proceed (e.g., choosing between approaches, \
         getting preferences, clarifying requirements). \
         \
         Supports single-select, multi-select, and includes automatic 'Other' option \
         for free-form text input. Can ask 1-4 questions at once. \
         \
         Example uses:\n\
         - \"Which library should we use?\" (single-select)\n\
         - \"Which features do you want?\" (multi-select)\n\
         - \"How should I format the output?\" (with custom text option)\n\
         \
         Available in all modes including plan mode."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::object(vec![
            ("questions", ToolInputSchema::array(
                "Array of 1-4 questions to ask the user",
                ToolInputSchema::object(vec![
                    ("question", ToolInputSchema::string("The question text (e.g., 'How should I format the output?')")),
                    ("header", ToolInputSchema::string("Short label for display (max 12 chars, e.g., 'Format')")),
                    ("options", ToolInputSchema::array(
                        "Available options (2-4 required)",
                        ToolInputSchema::object(vec![
                            ("label", ToolInputSchema::string("Display label (e.g., 'Summary')")),
                            ("description", ToolInputSchema::string("What this option means")),
                        ])
                    )),
                    ("multi_select", ToolInputSchema::optional_bool("Allow multiple selections (default: false)")),
                ])
            )),
        ])
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> Result<String> {
        // Parse input
        let ask_input: AskUserQuestionInput = serde_json::from_value(input)
            .context("Failed to parse AskUserQuestion input")?;

        // Validate input
        validate_input(&ask_input)
            .map_err(|e| anyhow::anyhow!("Invalid question format: {}", e))?;

        // Check if TUI renderer is available
        let tui_renderer = context.tui_renderer.as_ref()
            .ok_or_else(|| anyhow::anyhow!("TUI dialogs not available in this context"))?;

        // Show dialog and collect answers
        let mut tui = tui_renderer.lock().await;
        let output = tui.show_llm_question(&ask_input)
            .context("Failed to display question dialog")?;
        drop(tui); // Release lock

        // Format output for Claude
        let mut result = String::from("User responses:\n\n");

        for (question_text, answer) in &output.answers {
            result.push_str(&format!("Q: {}\nA: {}\n\n", question_text, answer));
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        let tool = AskUserQuestionTool;
        assert_eq!(tool.name(), "AskUserQuestion");
    }

    #[test]
    fn test_tool_description() {
        let tool = AskUserQuestionTool;
        let desc = tool.description();
        assert!(desc.contains("Ask the user"));
        assert!(desc.contains("clarifying questions"));
    }

    #[test]
    fn test_input_schema() {
        let tool = AskUserQuestionTool;
        let schema = tool.input_schema();

        // Verify schema structure
        if let ToolInputSchema::Object { properties, .. } = schema {
            assert!(properties.contains_key("questions"));
        } else {
            panic!("Expected object schema");
        }
    }
}
