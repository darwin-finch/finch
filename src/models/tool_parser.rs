// Tool call parser for local model outputs
//
// Parses XML-formatted tool calls from model responses using regex

use crate::tools::ToolUse;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// Matches the outer boundaries of a `<tool_use>...</tool_use>` block without
/// asserting anything about what is inside it.
///
/// This is the sole authority for "does this response contain a genuine
/// tool-call attempt" (`has_tool_calls`) and for segmenting a response into
/// independently-parseable blocks (`parse`, `extract_text`): a bare mention
/// of the marker string, or a truncated opening tag with no matching close,
/// has no closing `</tool_use>` and therefore never matches (#1307). What is
/// captured between the tags is validated separately by
/// [`TOOL_USE_INNER_REGEX`] so that one block's malformed inner markup or
/// JSON cannot prevent another well-formed block in the same response from
/// matching.
static TOOL_USE_BLOCK_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)<tool_use>(.*?)</tool_use>").expect("Failed to compile tool_use block regex")
});

/// Matches the `<name>` and `<parameters>` elements inside an already-isolated
/// `<tool_use>` block body (see [`TOOL_USE_BLOCK_REGEX`]).
///
/// The name group is `[^<]*` (zero-or-more), not `[^<]+`: an empty
/// `<name></name>` element is well-formed markup with an invalid value, and
/// must reach [`ToolCallParser::parse`]'s dedicated empty-name diagnostic
/// rather than fail markup matching here and be misreported as malformed
/// structure.
static TOOL_USE_INNER_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?s)^\s*<name>([^<]*)</name>\s*<parameters>(.+?)</parameters>\s*$")
        .expect("Failed to compile tool_use inner regex")
});

/// One `<tool_use>` block that matched [`TOOL_USE_BLOCK_REGEX`] but could not
/// be turned into a [`ToolUse`] -- either its inner markup did not match the
/// expected `<name>...</name><parameters>...</parameters>` shape, its name
/// was empty, or its parameters were not valid JSON.
///
/// This is a diagnostic, not a hard failure: [`ToolCallParser::parse`]
/// collects it alongside whatever other blocks in the same response parsed
/// successfully, instead of discarding all of them (#1307).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallParseError {
    /// The full `<tool_use>...</tool_use>` text that failed to parse.
    pub raw_block: String,
    /// Human-readable reason the block could not be turned into a `ToolUse`.
    pub message: String,
}

/// Result of parsing a response for `<tool_use>` blocks: the tool calls that
/// parsed successfully, plus a diagnostic for each block that did not.
///
/// A response with N well-formed blocks and one malformed block yields N
/// entries in `tool_uses` and one in `errors` -- never an all-or-nothing
/// failure (#1307).
#[derive(Debug, Clone, Default)]
pub struct ToolCallParseOutcome {
    /// Successfully parsed tool calls, in the order they appeared.
    pub tool_uses: Vec<ToolUse>,
    /// One entry per block that matched `<tool_use>...</tool_use>` but could
    /// not be turned into a tool call.
    pub errors: Vec<ToolCallParseError>,
}

/// Parser for extracting tool calls from model output
pub struct ToolCallParser;

impl ToolCallParser {
    /// Extract all tool uses from output
    ///
    /// Parses XML-formatted tool_use blocks and creates ToolUse objects.
    /// Each `<tool_use>...</tool_use>` block is parsed independently: a
    /// block with malformed inner markup or invalid JSON parameters is
    /// recorded in [`ToolCallParseOutcome::errors`] and skipped, it does not
    /// prevent other, well-formed blocks in the same response from being
    /// returned in [`ToolCallParseOutcome::tool_uses`] (#1307).
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// The tool calls that parsed successfully, plus a diagnostic for each
    /// block that did not.
    pub fn parse(output: &str) -> ToolCallParseOutcome {
        let mut outcome = ToolCallParseOutcome::default();

        for block_capture in TOOL_USE_BLOCK_REGEX.captures_iter(output) {
            let raw_block = block_capture
                .get(0)
                .expect("capture group 0 is always the whole match")
                .as_str()
                .to_string();
            let body = block_capture
                .get(1)
                .expect("capture group 1 is the block body per TOOL_USE_BLOCK_REGEX")
                .as_str();

            let Some(inner) = TOOL_USE_INNER_REGEX.captures(body) else {
                tracing::warn!(raw_block = %raw_block, "skipping tool_use block with malformed markup");
                outcome.errors.push(ToolCallParseError {
                    raw_block,
                    message: "block did not match the expected <name>...</name><parameters>...</parameters> structure".to_string(),
                });
                continue;
            };

            let name = inner
                .get(1)
                .expect("capture group 1 is the name per TOOL_USE_INNER_REGEX")
                .as_str()
                .trim()
                .to_string();
            let params_str = inner
                .get(2)
                .expect("capture group 2 is the parameters per TOOL_USE_INNER_REGEX")
                .as_str()
                .trim();

            if name.is_empty() {
                tracing::warn!(raw_block = %raw_block, "skipping tool use with empty name");
                outcome.errors.push(ToolCallParseError {
                    raw_block,
                    message: "tool name was empty".to_string(),
                });
                continue;
            }

            let parameters: Value = match serde_json::from_str(params_str) {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(raw_block = %raw_block, error = %err, "skipping tool_use block with invalid JSON parameters");
                    outcome.errors.push(ToolCallParseError {
                        raw_block,
                        message: format!("failed to parse parameters as JSON: {err}"),
                    });
                    continue;
                }
            };

            outcome.tool_uses.push(ToolUse::new(name, parameters));
        }

        outcome
    }

    /// Extract text content (everything outside tool_use tags)
    ///
    /// Removes all `<tool_use>...</tool_use>` blocks -- whether or not their
    /// inner markup or JSON is well-formed -- and returns remaining text.
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// Text content with tool_use blocks removed
    pub fn extract_text(output: &str) -> String {
        TOOL_USE_BLOCK_REGEX
            .replace_all(output, "")
            .trim()
            .to_string()
    }

    /// Check if output contains any tool calls
    ///
    /// Requires a genuine, well-formed `<tool_use>...</tool_use>` block --
    /// matching open and close tags -- not just the bare substring
    /// `<tool_use>`. Text that merely mentions the marker (prose, an error
    /// message, a truncated/incomplete opening tag with no matching close)
    /// does not count as a tool-call attempt (#1307). Inner markup and JSON
    /// validity are checked separately by [`Self::parse`]: a block that
    /// matches here but fails to parse is a genuine, if malformed, attempt.
    ///
    /// # Arguments
    /// * `output` - Raw output from the model
    ///
    /// # Returns
    /// true if output contains at least one well-formed `<tool_use>` block
    pub fn has_tool_calls(output: &str) -> bool {
        TOOL_USE_BLOCK_REGEX.is_match(output)
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

        let outcome = ToolCallParser::parse(output);
        assert_eq!(
            outcome.errors,
            vec![],
            "well-formed single block must not produce diagnostics: {outcome:?}"
        );
        assert_eq!(outcome.tool_uses.len(), 1);
        assert_eq!(outcome.tool_uses[0].name, "read");
        assert_eq!(outcome.tool_uses[0].input["file_path"], "/tmp/test.txt");
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

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.errors, vec![]);
        assert_eq!(outcome.tool_uses.len(), 2);
        assert_eq!(outcome.tool_uses[0].name, "read");
        assert_eq!(outcome.tool_uses[1].name, "grep");
        assert_eq!(outcome.tool_uses[1].input["pattern"], "TODO");
    }

    #[test]
    fn test_parse_compact_format() {
        // Test without extra whitespace
        let output =
            "<tool_use><name>bash</name><parameters>{\"command\":\"ls\"}</parameters></tool_use>";

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 1);
        assert_eq!(outcome.tool_uses[0].name, "bash");
        assert_eq!(outcome.tool_uses[0].input["command"], "ls");
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

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 1);
        assert_eq!(outcome.tool_uses[0].name, "bash");
        assert_eq!(outcome.tool_uses[0].input["command"], "cargo test");
        assert_eq!(outcome.tool_uses[0].input["description"], "Run tests");
    }

    #[test]
    fn test_parse_invalid_json() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{invalid json}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);
        assert_eq!(
            outcome.tool_uses.len(),
            0,
            "no valid tool calls in this response: {outcome:?}"
        );
        assert_eq!(
            outcome.errors.len(),
            1,
            "the malformed block must be diagnosed, not silently dropped: {outcome:?}"
        );
        assert!(
            outcome.errors[0].message.contains("parse parameters"),
            "diagnostic must name the JSON failure: {:?}",
            outcome.errors[0]
        );
        assert!(outcome.errors[0].raw_block.contains("invalid json"));
    }

    /// Regression for #1307 gap 1: a response with several well-formed
    /// `<tool_use>` blocks and one malformed block (invalid JSON
    /// parameters) must still surface all the well-formed calls, plus a
    /// diagnosable failure for the bad one -- not zero calls. Before the
    /// fix, `ToolCallParser::parse` propagated the first JSON error via `?`
    /// and discarded every tool call already collected from the same
    /// response, including the two valid ones on either side of the bad one.
    #[test]
    fn test_parse_one_malformed_block_does_not_discard_valid_blocks_in_same_response() {
        let output = r#"
First, I'll read the file:

<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/a.txt"}</parameters>
</tool_use>

Then this one is malformed:

<tool_use>
  <name>bash</name>
  <parameters>{not valid json at all}</parameters>
</tool_use>

Finally, I'll search:

<tool_use>
  <name>grep</name>
  <parameters>{"pattern": "TODO", "path": "."}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);

        assert_eq!(
            outcome.tool_uses.len(),
            2,
            "the two well-formed blocks must both survive one sibling block's bad JSON: {outcome:?}"
        );
        assert_eq!(outcome.tool_uses[0].name, "read");
        assert_eq!(outcome.tool_uses[0].input["file_path"], "/tmp/a.txt");
        assert_eq!(outcome.tool_uses[1].name, "grep");
        assert_eq!(outcome.tool_uses[1].input["pattern"], "TODO");

        assert_eq!(
            outcome.errors.len(),
            1,
            "exactly the one malformed block must be diagnosed: {outcome:?}"
        );
        assert!(
            outcome.errors[0]
                .raw_block
                .contains("not valid json at all"),
            "the diagnostic must identify which raw block failed: {:?}",
            outcome.errors[0]
        );
        assert!(
            outcome.errors[0].message.contains("parse parameters"),
            "the diagnostic must explain why it failed: {:?}",
            outcome.errors[0]
        );
    }

    /// Regression for #1307 gap 1, markup variant: a block whose inner XML
    /// itself does not match `<name>...</name><parameters>...</parameters>`
    /// (here, a `<name>` tag with no matching `<parameters>` element) must
    /// also be diagnosed rather than silently vanishing, and must not affect
    /// a well-formed sibling block.
    #[test]
    fn test_parse_block_with_malformed_inner_markup_is_diagnosed_not_discarded() {
        let output = r#"
<tool_use>
  <name>bash</name>
  no parameters element here at all
</tool_use>

<tool_use>
  <name>grep</name>
  <parameters>{"pattern": "TODO"}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);

        assert_eq!(
            outcome.tool_uses.len(),
            1,
            "the well-formed grep block must still parse: {outcome:?}"
        );
        assert_eq!(outcome.tool_uses[0].name, "grep");

        assert_eq!(
            outcome.errors.len(),
            1,
            "the block with no <parameters> element must be diagnosed: {outcome:?}"
        );
        assert!(outcome.errors[0]
            .raw_block
            .contains("no parameters element"));
    }

    #[test]
    fn test_parse_empty_name() {
        let output = r#"
<tool_use>
  <name></name>
  <parameters>{"command": "ls"}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);
        // Empty names should be skipped, and diagnosed rather than silently
        // dropped.
        assert_eq!(outcome.tool_uses.len(), 0);
        assert_eq!(outcome.errors.len(), 1);
        assert!(outcome.errors[0].message.contains("empty"));
    }

    #[test]
    fn test_parse_no_tools() {
        let output = "Just a regular response without any tool calls.";

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 0);
        assert_eq!(outcome.errors.len(), 0);
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
        assert!(ToolCallParser::has_tool_calls("<tool_use></tool_use>"));
        assert!(ToolCallParser::has_tool_calls(
            "text <tool_use><name>x</name></tool_use> more text"
        ));
        assert!(!ToolCallParser::has_tool_calls("no tools here"));
        assert!(!ToolCallParser::has_tool_calls(""));
    }

    /// Regression for #1307 gap 2: a bare mention of the marker string, with
    /// no matching close tag, must not be detected as a real tool-call
    /// attempt. Before the fix, `has_tool_calls` was `output.contains("<tool_use>")`,
    /// which misdetected this as a tool call and routed the response down
    /// the tool-parsing path.
    #[test]
    fn test_has_tool_calls_false_positive_prose_mentioning_marker() {
        let prose = "You can call a tool by writing a <tool_use> block in your response.";
        assert!(
            !ToolCallParser::has_tool_calls(prose),
            "prose that merely mentions the <tool_use> marker with no closing tag must not \
             be detected as a real tool call: {prose:?}"
        );
    }

    /// Regression for #1307 gap 2: a truncated/incomplete opening tag (the
    /// model's generation cut off before any closing tag appeared) must not
    /// be detected as a real tool call.
    #[test]
    fn test_has_tool_calls_false_positive_truncated_opening_tag() {
        let truncated =
            "Let me use a tool.\n\n<tool_use>\n  <name>read</name>\n  <parameters>{\"file";
        assert!(
            !ToolCallParser::has_tool_calls(truncated),
            "a truncated <tool_use> block with no closing tag must not be detected as a real \
             tool call: {truncated:?}"
        );
    }

    /// Regression for #1307 gap 2: an error message that quotes the marker
    /// string in prose (no real tag structure at all) must not be detected
    /// as a tool call.
    #[test]
    fn test_has_tool_calls_false_positive_error_message_quoting_marker() {
        let error_text =
            "Error: your last response did not contain a valid <tool_use> tag, please retry.";
        assert!(!ToolCallParser::has_tool_calls(error_text));
    }

    /// A well-formed block embedded in a fenced fenced code example is
    /// indistinguishable from a real attempt by tag structure alone once it
    /// has both a matching open and close tag; `has_tool_calls` answers
    /// "does this look like a genuine attempt", and `parse`/`extract_text`
    /// stay consistent with it. This pins that documented boundary rather
    /// than asserting undecidable code-fence detection.
    #[test]
    fn test_has_tool_calls_true_for_well_formed_block_regardless_of_surrounding_prose() {
        let with_example = "Example:\n```xml\n<tool_use>\n  <name>read</name>\n  <parameters>{}</parameters>\n</tool_use>\n```\n";
        assert!(ToolCallParser::has_tool_calls(with_example));
    }

    #[test]
    fn test_parse_escaped_json() {
        let output = r#"
<tool_use>
  <name>bash</name>
  <parameters>{"command": "echo \"hello world\""}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 1);
        assert_eq!(
            outcome.tool_uses[0].input["command"],
            "echo \"hello world\""
        );
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

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 1);
        assert_eq!(outcome.tool_uses[0].name, "grep");
        assert_eq!(outcome.tool_uses[0].input["pattern"], "fn main");
        assert_eq!(outcome.tool_uses[0].input["path"], "src/");
        assert_eq!(outcome.tool_uses[0].input["case_insensitive"], true);
        assert_eq!(outcome.tool_uses[0].input["max_results"], 10);
    }

    #[test]
    fn test_tool_use_id_generated() {
        let output = r#"
<tool_use>
  <name>read</name>
  <parameters>{"file_path": "/tmp/test.txt"}</parameters>
</tool_use>
"#;

        let outcome = ToolCallParser::parse(output);
        assert_eq!(outcome.tool_uses.len(), 1);
        // ID should be generated automatically
        assert!(outcome.tool_uses[0].id.starts_with("toolu_"));
        assert!(outcome.tool_uses[0].id.len() > 6);
    }
}
