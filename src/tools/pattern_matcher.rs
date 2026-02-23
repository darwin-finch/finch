// Pattern-based tool selection
//
// Extracts tool uses from query text using regex patterns
// Provides immediate value before neural tool selector is trained

use crate::tools::types::ToolUse;
use anyhow::Result;
use regex::Regex;
use serde_json::json;
use tracing::{debug, instrument};

/// Tool pattern - matches queries to tool invocations
pub struct ToolPattern {
    /// Trigger regex pattern
    trigger: Regex,
    /// Tool name to invoke
    tool_name: String,
    /// Parameter extraction patterns
    param_extractors: Vec<ParamExtractor>,
}

/// Parameter extractor - extracts tool parameters from regex captures
pub struct ParamExtractor {
    /// Parameter name in tool input
    param_name: String,
    /// Capture group index (1-based)
    capture_index: usize,
    /// Optional default value if capture not found
    default_value: Option<String>,
}

impl ToolPattern {
    /// Create new tool pattern
    pub fn new(
        trigger: &str,
        tool_name: String,
        param_extractors: Vec<ParamExtractor>,
    ) -> Result<Self> {
        Ok(Self {
            trigger: Regex::new(trigger)?,
            tool_name,
            param_extractors,
        })
    }

    /// Check if pattern matches query
    pub fn matches(&self, query: &str) -> bool {
        self.trigger.is_match(query)
    }

    /// Extract tool use from query
    pub fn extract(&self, query: &str) -> Option<ToolUse> {
        let captures = self.trigger.captures(query)?;

        let mut input = serde_json::Map::new();

        for extractor in &self.param_extractors {
            // Try to get value from capture or default
            let value = if extractor.capture_index > 0 {
                captures
                    .get(extractor.capture_index)
                    .map(|m| m.as_str().to_string())
                    .or_else(|| extractor.default_value.clone())
            } else {
                extractor.default_value.clone()
            };

            // Only insert if we have a value (skip optional parameters)
            if let Some(v) = value {
                input.insert(extractor.param_name.clone(), json!(v));
            }
        }

        Some(ToolUse::new(
            self.tool_name.clone(),
            serde_json::Value::Object(input),
        ))
    }
}

impl ParamExtractor {
    /// Create extractor from capture group
    pub fn from_capture(param_name: String, capture_index: usize) -> Self {
        Self {
            param_name,
            capture_index,
            default_value: None,
        }
    }

    #[allow(dead_code)]
    /// Create extractor with default value
    pub fn with_default(param_name: String, default_value: String) -> Self {
        Self {
            param_name,
            capture_index: 0,
            default_value: Some(default_value),
        }
    }
}

/// Pattern-based tool matcher
pub struct ToolPatternMatcher {
    patterns: Vec<ToolPattern>,
}

impl ToolPatternMatcher {
    /// Create new matcher with default patterns
    pub fn new() -> Self {
        Self {
            patterns: Vec::new(),
        }
    }

    /// Create matcher with built-in patterns
    pub fn with_default_patterns() -> Result<Self> {
        let mut matcher = Self::new();

        // Read file patterns
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)read (?:the )?(?:file |contents of )?['"]?([^'"]+)['"]?"#,
            "read".to_string(),
            vec![ParamExtractor::from_capture("file_path".to_string(), 1)],
        )?)?;

        // Grep patterns (before glob to match "search for X in Y" patterns first)
        // Capture pattern up to (but not including) " in " separator
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)(?:search|grep) (?:for )?['"]?([^'"]+?)['"]?\s+in\s+(.+)"#,
            "grep".to_string(),
            vec![
                ParamExtractor::from_capture("pattern".to_string(), 1),
                ParamExtractor::from_capture("path".to_string(), 2),
            ],
        )?)?;

        // Grep without path
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)(?:search|grep) (?:for )?['"]?([^'"]+)['"]?$"#,
            "grep".to_string(),
            vec![ParamExtractor::from_capture("pattern".to_string(), 1)],
        )?)?;

        // Glob patterns (after grep to avoid matching "find" in "search for")
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)(?:find|list|show) (?:all )?files? (?:matching |like )?['"]?([^'"]+)['"]?"#,
            "glob".to_string(),
            vec![ParamExtractor::from_capture("pattern".to_string(), 1)],
        )?)?;

        // Web fetch patterns
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)(?:fetch|get|retrieve) (?:from |contents of )?(?:url |website )?['"]?(https?://[^'"]+)['"]?"#,
            "web_fetch".to_string(),
            vec![ParamExtractor::from_capture("url".to_string(), 1)],
        )?)?;

        // Bash patterns
        matcher.add_pattern(ToolPattern::new(
            r#"(?i)(?:run|execute) (?:command |bash )?['"]?([^'"]+)['"]?"#,
            "bash".to_string(),
            vec![ParamExtractor::from_capture("command".to_string(), 1)],
        )?)?;

        Ok(matcher)
    }

    /// Add a pattern
    pub fn add_pattern(&mut self, pattern: ToolPattern) -> Result<()> {
        self.patterns.push(pattern);
        Ok(())
    }

    /// Extract tool uses from query
    #[instrument(skip(self))]
    pub fn extract_tool_uses(&self, query: &str) -> Result<Vec<ToolUse>> {
        let mut tool_uses = Vec::new();

        for pattern in &self.patterns {
            if let Some(tool_use) = pattern.extract(query) {
                debug!("Extracted tool use: {}", tool_use.name);
                tool_uses.push(tool_use);
                // Only match first pattern (avoid duplicate extractions)
                break;
            }
        }

        Ok(tool_uses)
    }

    /// Check if query matches any tool pattern
    pub fn matches_any(&self, query: &str) -> bool {
        self.patterns.iter().any(|p| p.matches(query))
    }
}

impl Default for ToolPatternMatcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_read_pattern() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let test_cases = vec![
            "read the file /path/to/file.txt",
            "read file /path/to/file.txt",
            "read /path/to/file.txt",
            "Read the contents of /path/to/file.txt",
        ];

        for query in test_cases {
            let tool_uses = matcher.extract_tool_uses(query).unwrap();
            assert_eq!(tool_uses.len(), 1, "Failed for: {}", query);
            assert_eq!(tool_uses[0].name, "read");
            assert!(tool_uses[0]
                .input
                .get("file_path")
                .unwrap()
                .as_str()
                .unwrap()
                .contains("file.txt"));
        }
    }

    #[test]
    fn test_glob_pattern() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let test_cases = vec![
            "find files matching *.rs",
            "list all files like *.txt",
            "show files **/*.json",
        ];

        for query in test_cases {
            let tool_uses = matcher.extract_tool_uses(query).unwrap();
            assert_eq!(tool_uses.len(), 1, "Failed for: {}", query);
            assert_eq!(tool_uses[0].name, "glob");
            assert!(tool_uses[0].input.get("pattern").is_some());
        }
    }

    #[test]
    fn test_grep_pattern() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let query = "search for TODO in src/";
        let tool_uses = matcher.extract_tool_uses(query).unwrap();

        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "grep");
        assert_eq!(
            tool_uses[0].input.get("pattern").unwrap().as_str().unwrap(),
            "TODO"
        );
    }

    #[test]
    fn test_web_fetch_pattern() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let query = "fetch from https://example.com";
        let tool_uses = matcher.extract_tool_uses(query).unwrap();

        assert_eq!(tool_uses.len(), 1);
        assert_eq!(tool_uses[0].name, "web_fetch");
        assert_eq!(
            tool_uses[0].input.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn test_bash_pattern() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let test_cases = vec!["run ls -la", "execute command pwd", "run bash echo hello"];

        for query in test_cases {
            let tool_uses = matcher.extract_tool_uses(query).unwrap();
            assert_eq!(tool_uses.len(), 1, "Failed for: {}", query);
            assert_eq!(tool_uses[0].name, "bash");
            assert!(tool_uses[0].input.get("command").is_some());
        }
    }

    #[test]
    fn test_no_match() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        let query = "What is the meaning of life?";
        let tool_uses = matcher.extract_tool_uses(query).unwrap();

        assert_eq!(tool_uses.len(), 0);
    }

    #[test]
    fn test_matches_any() {
        let matcher = ToolPatternMatcher::with_default_patterns().unwrap();

        assert!(matcher.matches_any("read file.txt"));
        assert!(matcher.matches_any("find files *.rs"));
        assert!(!matcher.matches_any("What is Rust?"));
    }
}
