// Tool call parser for local model outputs
//
// Parses XML-formatted tool calls from model responses using regex

use crate::tools::types::ToolUse;
use anyhow::{Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// Regex to match tool_use blocks with name and parameters
///
/// Matches: <tool_use>\s*<name>...</name>\s*<parameters>...</parameters>\s*</tool_use>
static TOOL_USE_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)<tool_use>\s*<name>([^<]+)</name>\s*<parameters>(.+?)</parameters>\s*</tool_use>",
    )
    .expect("Failed to compile tool_use regex")
});

/// Parser for extracting tool calls from model output
pub struct ToolCallParser;

impl ToolCallParser {
    /// Extract all tool uses from output
    ///
    /// Parses XML-formatted tool_use blocks and creates ToolUse objects.
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// Vector of parsed ToolUse objects
    ///
    /// # Errors
    /// Returns error if:
    /// - Tool name is invalid (empty or whitespace-only)
    /// - Parameters are not valid JSON
    pub fn parse(output: &str) -> Result<Vec<ToolUse>> {
        let mut tool_uses = Vec::new();

        for capture in TOOL_USE_REGEX.captures_iter(output) {
            // Extract name (group 1) and parameters (group 2)
            let name = capture
                .get(1)
                .context("Missing tool name in capture")?
                .as_str()
                .trim()
                .to_string();

            let params_str = capture
                .get(2)
                .context("Missing parameters in capture")?
                .as_str()
                .trim();

            // Validate name
            if name.is_empty() {
                tracing::warn!("Skipping tool use with empty name");
                continue;
            }

            // Parse JSON parameters
            let parameters: Value = serde_json::from_str(params_str)
                .with_context(|| format!("Failed to parse parameters as JSON: {}", params_str))?;

            // Create ToolUse with generated ID
            let tool_use = ToolUse::new(name, parameters);
            tool_uses.push(tool_use);
        }

        Ok(tool_uses)
    }

    /// Extract text content (everything outside tool_use tags)
    ///
    /// Removes all <tool_use>...</tool_use> blocks and returns remaining text.
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// Text content with tool_use blocks removed
    pub fn extract_text(output: &str) -> String {
        TOOL_USE_REGEX.replace_all(output, "").trim().to_string()
    }

    /// Check if output contains any tool calls
    ///
    /// Fast check without full parsing.
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// true if output contains at least one <tool_use> tag
    pub fn has_tool_calls(output: &str) -> bool {
        output.contains("<tool_use>")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_tool() {
        let output = r#"
I'll read the file for you.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "read");
        assert_eq!(tool_uses[0].input["file_path"], "/tmp/test.txt");
    }

    #[test]
    fn test_parse_multiple_tools() {
        let output = r#"
First, I'll read the file:

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Then I'll search for the pattern:

<tool_use>
  <name>grep</name>
  <parameters>{"pattern": "TODO", "path": "."}</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 2);
        assert_eq!(tool_uses[0].name, "read");
        assert_eq!(tool_uses[1].name, "grep");
        assert_eq!(tool_uses[1].input["pattern"], "TODO");
    }

    #[test]
    fn test_parse_compact_format() {
        // Test without extra whitespace
        let output =
            "<tool_use><name>bash</name><parameters>{\"command\":\"ls\"}</parameters></tool_use>";

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "bash");
        assert_eq!(tool_uses[0].input["command"], "ls");
    }

    #[test]
    fn test_parse_with_newlines_in_json() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{
    "command": "cargo test",
    "description": "Run tests"
  }</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "bash");
        assert_eq!(tool_uses[0].input["command"], "cargo test");
        assert_eq!(tool_uses[0].input["description"], "Run tests");
    }

    #[test]
    fn test_parse_invalid_json() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{invalid json}</parameters>
</tool_use>
"#;

        let result = ToolCallParser::parse(output);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("parse parameters"));
    }

    #[test]
    fn test_parse_empty_name() {
        let output = r#"
<tool_use>
  <name></name>
  <parameters>{"command": "ls"}</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        // Empty names should be skipped
        assert_eq!(tool_uses.len(), 0);
    }

    #[test]
    fn test_parse_no_tools() {
        let output = "Just a regular response without any tool calls.";

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 0);
    }

    #[test]
    fn test_extract_text() {
        let output = r#"
I'll help you with that.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Let me know if you need anything else.
"#;

        let text = ToolCallParser::extract_text(output);
        assert!(!text.contains("<tool_use>"));
        assert!(!text.contains("read"));
        assert!(text.contains("I'll help you"));
        assert!(text.contains("Let me know"));
    }

    #[test]
    fn test_extract_text_only_tools() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{"command": "ls"}</parameters>
</tool_use>
"#;

        let text = ToolCallParser::extract_text(output);
        // Should be empty after removing tool blocks
        assert_eq!(text, "");
    }

    #[test]
    fn test_extract_text_no_tools() {
        let output = "Just text without any tools.";

        let text = ToolCallParser::extract_text(output);
        assert_eq!(text, output);
    }

    #[test]
    fn test_has_tool_calls() {
        assert!(ToolCallParser::has_tool_calls("<tool_use>"));
        assert!(ToolCallParser::has_tool_calls("text <tool_use> more text"));
        assert!(!ToolCallParser::has_tool_calls("no tools here"));
        assert!(!ToolCallParser::has_tool_calls(""));
    }

    #[test]
    fn test_parse_escaped_json() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{"command": "echo \"hello world\""}</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].input["command"], "echo \"hello world\"");
    }

    #[test]
    fn test_parse_complex_json() {
        let output = r#"
<tool_use>
  <name>grep</name>
  <parameters>{
    "pattern": "fn main",
    "path": "src/",
    "case_insensitive": true,
    "max_results": 10
  }</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "grep");
        assert_eq!(tool_uses[0].input["pattern"], "fn main");
        assert_eq!(tool_uses[0].input["path"], "src/");
        assert_eq!(tool_uses[0].input["case_insensitive"], true);
        assert_eq!(tool_uses[0].input["max_results"], 10);
    }

    #[test]
    fn test_tool_use_id_generated() {
        let output = r#"
<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>
"#;

        let tool_uses = ToolCallParser::parse(output).unwrap();
        assert_eq!(tool_uses.len(), 1);
        // ID should be generated automatically
        assert!(tool_uses[0].id.starts_with("toolu_"));
        assert!(tool_uses[0].id.len() > 6);
    }
}
