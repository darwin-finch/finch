// Tool prompt formatting for local models
//
// Formats tool definitions into model-readable system prompts
// and tool results into continuation messages.

use crate::tools::{ToolDefinition, ToolResult};

/// Formats tool definitions and results for local model prompts
pub struct ToolPromptFormatter;

impl ToolPromptFormatter {
    /// Format tool definitions into system prompt text
    ///
    /// Creates a compact system prompt that includes:
    /// - One shared `<tool_use>` XML format example (not repeated per tool)
    /// - Each tool as a single line: `name(param: type, ...): description`
    ///
    /// This replaced a per-tool `### name` heading plus a full `**Parameters:**`
    /// list plus a full `**Example:**` XML block for every tool (#1310):
    /// with the real, current tool catalog (36 registered tools as of this
    /// writing, not the 26 a same-line-only grep undercounts because
    /// several registrations span multiple lines), that per-tool
    /// boilerplate was a fixed multi-thousand-token tax paid on every
    /// local-model turn before any real conversation content, regardless of
    /// whether the query had anything to do with tools. See
    /// `local::AGENTS.md` for the measured before/after cost and the
    /// regression test that bounds it.
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
            prompt.push_str(&format!(
                "- `{}({})`: {}\n",
                tool.name,
                Self::compact_param_signature(&tool.input_schema),
                tool.description
            ));
        }
        prompt.push('\n');

        prompt.push_str("## Important Rules:\n\n");
        // Rules 1 and 5 are deliberately worded to defer to, not contradict,
        // this Brain's own response-format contract (the VM wire protocol in
        // vocabulary/BOOT.md, injected separately into the system prompt for
        // interactive turns). The pre-#1312 wording told the model to
        // "explain your reasoning before using tools" and to "give the user
        // a clear response" -- directly opposite BOOT.md's "issue tool calls
        // without a prose preamble" and "every text block ... is instead a
        // complete ProgramSubmission" (never plain prose). A local model
        // fed both contracts in the same turn had no way to satisfy either
        // one, which #1312 traced as a likely contributor to a model
        // abandoning both formats for confident, ungrounded prose. Neither
        // rule below asserts a response shape of its own; both defer to
        // whatever format this turn's system prompt already established.
        prompt.push_str(
            "1. **No prose before a tool call**: issue the `<tool_use>` block directly, with \
             no explanation or narration first\n",
        );
        prompt
            .push_str("2. **Parameters must be valid JSON**: Ensure proper quoting and escaping\n");
        prompt.push_str(
            "3. **One tool at a time**: Call one tool, wait for results, then continue\n",
        );
        prompt.push_str(
            "4. **Use results**: After receiving tool results, incorporate them into your answer\n",
        );
        prompt.push_str(
            "5. **A tool call does not replace your final response**: after using a tool (or \
             if you don't need one), still answer in exactly the response format you were \
             already given -- a tool call is a separate, structured request, not a substitute \
             for that answer\n\n",
        );

        prompt
    }

    /// Render a tool's parameters as a compact `name: type` signature
    /// (`name?: type` when the parameter is optional), e.g.
    /// `file_path: string, limit?: number`.
    ///
    /// This deliberately drops each parameter's own `description` field
    /// (unlike the pre-#1310 format's `**Parameters:**` bullet list): the
    /// per-tool line already carries the tool's own description, and the
    /// type plus required/optional marker is enough for the model to
    /// construct a valid call without a second, verbose listing.
    fn compact_param_signature(schema: &crate::tools::ToolInputSchema) -> String {
        let Some(properties) = schema.properties.as_object() else {
            return String::new();
        };

        properties
            .iter()
            .map(|(param_name, param_info)| {
                let param_type = param_info
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("string");
                if schema.required.contains(param_name) {
                    format!("{param_name}: {param_type}")
                } else {
                    format!("{param_name}?: {param_type}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolInputSchema;

    #[test]
    fn test_format_empty_tools() {
        let tools = vec![];
        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);
        assert_eq!(result, "");
    }

    /// Regression for #1312: the pre-fix "Important Rules" told a local
    /// model to "explain your reasoning before using tools" and to "give
    /// the user a clear response" -- directly opposite
    /// `vocabulary/BOOT.md`'s wire-protocol contract ("issue tool calls
    /// without a prose preamble" and "every text block ... is instead a
    /// complete ProgramSubmission", never plain prose). A local model given
    /// both contracts in the same turn had no consistent format to follow.
    /// This asserts the contradicting instructions are gone and the
    /// replacement wording asserts no response shape of its own (so it
    /// cannot re-introduce the same clash with whatever contract the rest
    /// of the system prompt already established).
    #[test]
    fn important_rules_do_not_contradict_the_vm_wire_protocols_no_prose_contract() {
        let tools = vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to the file")]),
        }];

        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);

        assert!(
            !result.to_lowercase().contains("explain your reasoning"),
            "must not instruct a prose preamble before a tool call, contradicting \
             BOOT.md's 'issue tool calls without a prose preamble': {result:?}"
        );
        assert!(
            !result
                .to_lowercase()
                .contains("give the user a clear response"),
            "must not assert a plain-language final-answer shape, contradicting \
             BOOT.md's 'every text block ... is instead a complete ProgramSubmission': \
             {result:?}"
        );
        assert!(
            result.contains("No prose before a tool call"),
            "must instruct issuing the tool call with no narration first, matching \
             BOOT.md's own wording: {result:?}"
        );
        assert!(
            result.contains("does not replace your final response")
                && result.contains("response format you were already given"),
            "the final-answer rule must defer to whatever format this turn's system \
             prompt already established, not assert prose: {result:?}"
        );
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
        assert!(result.contains("read(file_path: string)"));
        assert!(result.contains("Read a file from disk"));
        // The shared XML example appears once, in the intro block, not
        // repeated per tool (#1310).
        assert!(result.contains("<tool_use>"));
        assert!(
            !result.contains("<name>read</name>"),
            "a per-tool XML example block must not be emitted; the single shared \
             `tool_name` placeholder example is the only <tool_use> template: {result:?}"
        );
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

        assert!(result.contains("read(file_path: string)"));
        assert!(result.contains("bash(command: string, description: string)"));
        // No per-tool `<name>...</name>` XML example block, regardless of
        // tool count -- the fixed cost this format no longer multiplies by
        // the number of registered tools (#1310). (The intro's one shared
        // `<tool_use>` template and its prose mention of the tag both stay,
        // so counting raw `<tool_use>` occurrences isn't a precise check
        // here; the per-tool `<name>...</name>` markup is what the old
        // format repeated per tool.)
        assert!(
            !result.contains("<name>read</name>") && !result.contains("<name>bash</name>"),
            "a per-tool XML example block must not be emitted for either tool: {result:?}"
        );
    }

    #[test]
    fn test_compact_param_signature_marks_optional_params() {
        let mut schema = ToolInputSchema::simple(vec![("required_param", "desc")]);
        schema.properties.as_object_mut().unwrap().insert(
            "optional_param".to_string(),
            serde_json::json!({"type": "number", "description": "desc"}),
        );
        // `required` deliberately excludes "optional_param".

        let tools = vec![ToolDefinition {
            name: "example".to_string(),
            description: "An example tool".to_string(),
            input_schema: schema,
        }];

        let result = ToolPromptFormatter::format_tools_for_prompt(&tools);

        assert!(
            result.contains("required_param: string"),
            "a required parameter must render without a `?` marker: {result:?}"
        );
        assert!(
            result.contains("optional_param?: number"),
            "an optional parameter must render with a `?` marker: {result:?}"
        );
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
}
