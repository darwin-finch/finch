// AskUserQuestion - Tool for Claude to ask the user clarifying questions
//
// Enables the LLM to display interactive dialogs and collect user input during
// task execution. Supports single-select, multi-select, and custom text input.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::Result;
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
         Input format (JSON):\n\
         {\n\
           \"questions\": [\n\
             {\n\
               \"question\": \"Which approach?\",\n\
               \"header\": \"Approach\",\n\
               \"options\": [\n\
                 {\"label\": \"A\", \"description\": \"Fast\", \"markdown\": \"fn foo() {}\"},\n\
                 {\"label\": \"B\", \"description\": \"Simple\"}\n\
               ],\n\
               \"multi_select\": false\n\
             }\n\
           ]\n\
         }\n\
         \
         Supports single-select, multi-select, and automatic 'Other' option \
         for free-form text input. Can ask 1-4 questions at once. \
         Use `markdown` on options to show code previews when that option is focused; \
         the selected option's markdown is echoed back in `annotations` so you know \
         exactly which preview the user approved. \
         \
         Available in all modes including plan mode."
    }

    fn input_schema(&self) -> ToolInputSchema {
        // Manually construct schema for complex nested JSON
        let properties = serde_json::json!({
            "questions": {
                "type": "array",
                "description": "Array of 1-4 questions to ask the user",
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "description": "The question text (e.g., 'How should I format the output?')"
                        },
                        "header": {
                            "type": "string",
                            "description": "Short label for display (max 12 chars, e.g., 'Format')"
                        },
                        "options": {
                            "type": "array",
                            "description": "Available options (2-4 required)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {
                                        "type": "string",
                                        "description": "Display label (e.g., 'Summary')"
                                    },
                                    "description": {
                                        "type": "string",
                                        "description": "What this option means"
                                    },
                                    "markdown": {
                                        "type": "string",
                                        "description": "Optional preview content shown when this option is focused (code snippet, ASCII mockup, diff). Rendered in a monospace preview box."
                                    }
                                },
                                "required": ["label", "description"]
                            }
                        },
                        "multi_select": {
                            "type": "boolean",
                            "description": "Allow multiple selections (default: false)"
                        }
                    },
                    "required": ["question", "header", "options"]
                }
            }
        });

        ToolInputSchema {
            schema_type: "object".to_string(),
            properties,
            required: vec!["questions".to_string()],
        }
    }

    async fn execute(&self, _input: Value, _context: &ToolContext<'_>) -> Result<String> {
        // This should never be called - the event loop intercepts AskUserQuestion
        // before it reaches the tool executor via handle_ask_user_question()
        anyhow::bail!(
            "AskUserQuestion should be intercepted by event loop, not executed as a tool. \
             This is a bug in the event loop implementation."
        )
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
        assert_eq!(schema.schema_type, "object");
        assert_eq!(schema.required, vec!["questions"]);
    }

    #[test]
    fn test_schema_options_contain_markdown_field() {
        let tool = AskUserQuestionTool;
        let schema = tool.input_schema();

        let items = &schema.properties["questions"]["items"];
        let option_props = &items["properties"]["options"]["items"]["properties"];
        assert!(
            option_props.get("markdown").is_some(),
            "options schema must include 'markdown' property"
        );
        assert_eq!(option_props["markdown"]["type"], "string");
    }

    #[test]
    fn test_schema_description_mentions_preview() {
        let tool = AskUserQuestionTool;
        let desc = tool.description();
        assert!(
            desc.contains("markdown") || desc.contains("preview"),
            "description should mention markdown/preview feature"
        );
        assert!(
            desc.contains("annotations"),
            "description should mention annotations output field"
        );
    }
}
