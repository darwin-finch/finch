// Integration test for tool parsing and formatting
//
// Tests the full flow: format tools → parse tool calls → execute

use finch::models::{ToolCallParser, ToolPromptFormatter};
use finch::tools::types::{ToolDefinition, ToolInputSchema};

#[test]
fn test_tool_prompt_formatting() {
    let tools = vec![
        ToolDefinition {
            name: "read".to_string(),
            description: "Read a file from disk".to_string(),
            input_schema: ToolInputSchema::simple(vec![("file_path", "Path to the file")]),
        },
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute a shell command".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("command", "Command to execute"),
                ("description", "What the command does"),
            ]),
        },
    ];

    let formatted = ToolPromptFormatter::format_tools_for_prompt(&tools);

    // Verify format contains key elements
    assert!(formatted.contains("# Available Tools"));
    assert!(formatted.contains("### read"));
    assert!(formatted.contains("### bash"));
    assert!(formatted.contains("Read a file from disk"));
    assert!(formatted.contains("Execute a shell command"));
    assert!(formatted.contains("file_path"));
    assert!(formatted.contains("command"));
    assert!(formatted.contains("<tool_use>"));
    assert!(formatted.contains("<name>"));
    assert!(formatted.contains("<parameters>"));
}

#[test]
fn test_tool_call_parsing_single() {
    let output = r#"I'll read the file for you.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Let me know if you need anything else."#;

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "read");
    assert_eq!(tool_uses[0].input["file_path"], "/tmp/test.txt");
    assert!(tool_uses[0].id.starts_with("toolu_"));
}

#[test]
fn test_tool_call_parsing_multiple() {
    let output = r#"First, I'll glob for files:

<tool_use>
  <name>glob</name>
  <parameters>{"pattern": "**/*.rs"}</parameters>
</tool_use>

Then I'll grep for the pattern:

<tool_use>
  <name>grep</name>
  <parameters>{"pattern": "TODO", "path": "."}</parameters>
</tool_use>

Done!"#;

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 2);
    assert_eq!(tool_uses[0].name, "glob");
    assert_eq!(tool_uses[0].input["pattern"], "**/*.rs");
    assert_eq!(tool_uses[1].name, "grep");
    assert_eq!(tool_uses[1].input["pattern"], "TODO");
}

#[test]
fn test_tool_call_parsing_compact() {
    let output =
        "<tool_use><name>bash</name><parameters>{\"command\":\"ls -la\"}</parameters></tool_use>";

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "bash");
    assert_eq!(tool_uses[0].input["command"], "ls -la");
}

#[test]
fn test_tool_call_parsing_invalid_json() {
    let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{invalid json}</parameters>
</tool_use>
"#;

    let result = ToolCallParser::parse(output);
    assert!(result.is_err());
}

#[test]
fn test_extract_text() {
    let output = r#"I'll help you with that.

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>

Let me know if you need anything else."#;

    let text = ToolCallParser::extract_text(output);

    assert!(!text.contains("<tool_use>"));
    assert!(!text.contains("read"));
    assert!(!text.contains("file_path"));
    assert!(text.contains("I'll help you"));
    assert!(text.contains("Let me know"));
}

#[test]
fn test_has_tool_calls() {
    assert!(ToolCallParser::has_tool_calls("<tool_use>"));
    assert!(ToolCallParser::has_tool_calls("text <tool_use> more text"));
    assert!(!ToolCallParser::has_tool_calls("no tools here"));
    assert!(!ToolCallParser::has_tool_calls(""));
}

#[test]
fn test_tool_call_with_complex_json() {
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

    let tool_uses = ToolCallParser::parse(output).expect("Failed to parse");

    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].name, "grep");
    assert_eq!(tool_uses[0].input["pattern"], "fn main");
    assert_eq!(tool_uses[0].input["path"], "src/");
    assert_eq!(tool_uses[0].input["case_insensitive"], true);
    assert_eq!(tool_uses[0].input["max_results"], 10);
}
