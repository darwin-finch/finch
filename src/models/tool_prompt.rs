// Tool prompt formatting for local models
//
// Formats tool definitions into model-readable system prompts
// and tool results into continuation messages.

use crate::tools::types::{ToolDefinition, ToolResult};
use serde_json::Value;

/// Formats tool definitions and results for local model prompts
pub struct ToolPromptFormatter;

impl ToolPromptFormatter {
    /// Format tool definitions into system prompt text
    ///
    /// Creates a comprehensive system prompt that includes:
    /// - Tool usage instructions
    /// - Available tools with descriptions and parameters
    /// - XML format examples
    ///
    /// # Arguments
    /// * `tools` - Vector of tool definitions to include
    ///
    /// # Returns
    /// Formatted string to append to system prompt
    pub fn format_tools_for_prompt(tools: &[ToolDefinition]) -> String {
        if tools.is_empty() {
            return String::new();
        }

        let mut prompt = String::from("\n\n# Available Tools\n\n");
        prompt.push_str("You have access to tools that can help you accomplish tasks. ");
        prompt.push_str("To use a tool, output XML in the following format:\n\n");
        prompt.push_str("```xml\n");
        prompt.push_str("<tool_use>\n");
        prompt.push_str("  <name>tool_name</name>\n");
        prompt.push_str("  <parameters>{\"param\": \"value\"}</parameters>\n");
        prompt.push_str("</tool_use>\n");
        prompt.push_str("```\n\n");
        prompt.push_str("You can call multiple tools by using multiple <tool_use> blocks.\n\n");
        prompt.push_str("## Available Tools:\n\n");

        for tool in tools {
            prompt.push_str(&format!("### {}\n", tool.name));
            prompt.push_str(&format!("{}\n\n", tool.description));

            // Extract parameters from schema
            if let Some(properties) = tool.input_schema.properties.as_object() {
                prompt.push_str("**Parameters:**\n");
                for (param_name, param_info) in properties {
                    let param_desc = param_info
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("No description");
                    let param_type = param_info
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("string");
                    let is_required = tool.input_schema.required.contains(param_name);
                    let required_marker = if is_required { " (required)" } else { "" };

                    prompt.push_str(&format!(
                        "- `{}` ({}){}: {}\n",
                        param_name, param_type, required_marker, param_desc
                    ));
                }
                prompt.push('\n');
            }

            // Example usage
            prompt.push_str("**Example:**\n");
            prompt.push_str("```xml\n<tool_use>\n");
            prompt.push_str(&format!("  <name>{}</name>\n", tool.name));
            prompt.push_str("  <parameters>");
            prompt.push_str(&Self::generate_example_params(&tool.input_schema));
            prompt.push_str("</parameters>\n");
            prompt.push_str("</tool_use>\n```\n\n");
        }

        prompt.push_str("## Important Rules:\n\n");
        prompt.push_str("1. **Think before acting**: Explain your reasoning before using tools\n");
        prompt
            .push_str("2. **Parameters must be valid JSON**: Ensure proper quoting and escaping\n");
        prompt.push_str(
            "3. **One tool at a time**: Call one tool, wait for results, then continue\n",
        );
        prompt.push_str(
            "4. **Use results**: After receiving tool results, incorporate them into your answer\n",
        );
        prompt.push_str(
            "5. **Provide final answer**: After using tools, give the user a clear response\n\n",
        );

        prompt
    }

    /// Format tool results for continuation prompt
    ///
    /// Creates a message showing tool execution results that prompts
    /// the model to continue based on the tool outputs.
    ///
    /// # Arguments
    /// * `results` - Vector of tool results to format
    ///
    /// # Returns
    /// Formatted string with tool results
    pub fn format_tool_results(results: &[ToolResult]) -> String {
        let mut prompt = String::from("\n\n# Tool Results\n\n");
        prompt.push_str("The tools have been executed. Here are the results:\n\n");

        for result in results {
            prompt.push_str(&format!("<tool_result id=\"{}\">\n", result.tool_use_id));

            if result.is_error {
                prompt.push_str("**ERROR**: ");
            }

            // Truncate very long results
            let content = if result.content.len() > 2000 {
                format!(
                    "{}...\n\n(truncated, {} total characters)",
                    &result.content[..2000],
                    result.content.len()
                )
            } else {
                result.content.clone()
            };

            prompt.push_str(&content);
            prompt.push_str("\n</tool_result>\n\n");
        }

        prompt.push_str("Based on these results, provide your answer to the user's question.\n");
        prompt
    }

    /// Generate example parameters for a tool
    fn generate_example_params(schema: &crate::tools::types::ToolInputSchema) -> String {
        let mut params = serde_json::Map::new();

        if let Some(properties) = schema.properties.as_object() {
            for (param_name, param_info) in properties.iter().take(3) {
                // Take first 3 params for brevity
                let example_value = match param_info.get("type").and_then(|v| v.as_str()) {
                    Some("string") => Value::String("example_value".to_string()),
                    Some("number") => Value::Number(serde_json::Number::from(42)),
                    Some("boolean") => Value::Bool(true),
                    Some("array") => Value::Array(vec![]),
                    Some("object") => Value::Object(serde_json::Map::new()),
                    _ => Value::String("value".to_string()),
                };
                params.insert(param_name.clone(), example_value);
            }
        }

        serde_json::to_string(&Value::Object(params)).unwrap_or_else(|_| "{}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::types::ToolInputSchema;

    #[test]
    fn test_format_empty_tools() {
        let tools = vec![];
        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_single_tool() {
        let tools = vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to the file")]),
        }];

        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);

        assert!(result.contains("# Available Tools"));
        assert!(result.contains("### read"));
        assert!(result.contains("Read a file from disk"));
        assert!(result.contains("file_path"));
        assert!(result.contains("<tool_use>"));
        assert!(result.contains("<name>read</name>"));
    }

    #[test]
    fn test_format_multiple_tools() {
        let tools = vec![
            ToolDefinition {
                name: "read".to_string(),
                description: "Read a file".to_string(),
                input_schema: ToolInputSchema::simple(vec![("file_path", "File path")]),
            },
            ToolDefinition {
                name: "bash".to_string(),
                description: "Execute a command".to_string(),
                input_schema: ToolInputSchema::simple(vec![
                    ("command", "Command to run"),
                    ("description", "What the command does"),
                ]),
            },
        ];

        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);

        assert!(result.contains("### read"));
        assert!(result.contains("### bash"));
        assert!(result.contains("file_path"));
        assert!(result.contains("command"));
    }

    #[test]
    fn test_format_tool_results() {
        let results = vec![
            ToolResult::success("toolu_123".to_string(), "File contents here".to_string()),
            ToolResult::error("toolu_456".to_string(), "File not found".to_string()),
        ];

        let formatted = ToolPromptFormatter::format_tool_results(&results);

        assert!(formatted.contains("# Tool Results"));
        assert!(formatted.contains("toolu_123"));
        assert!(formatted.contains("File contents here"));
        assert!(formatted.contains("toolu_456"));
        assert!(formatted.contains("ERROR"));
        assert!(formatted.contains("File not found"));
    }

    #[test]
    fn test_format_tool_results_truncation() {
        let long_content = "x".repeat(3000);
        let results = vec![ToolResult::success("toolu_123".to_string(), long_content)];

        let formatted = ToolPromptFormatter::format_tool_results(&results);

        assert!(formatted.contains("truncated"));
        assert!(formatted.contains("3000 total characters"));
        assert!(formatted.len() < 2500); // Should be truncated
    }

    #[test]
    fn test_generate_example_params() {
        let schema = ToolInputSchema::simple(vec![
            ("file_path", "Path to file"),
            ("encoding", "File encoding"),
        ]);

        let example = ToolPromptFormatter::generate_example_params(&schema);

        assert!(example.contains("file_path"));
        assert!(example.contains("example_value"));
        // Should be valid JSON
        assert!(serde_json::from_str::<Value>(&example).is_ok());
    }
}
