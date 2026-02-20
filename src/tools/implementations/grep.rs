// Grep tool - searches for patterns in files

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use walkdir::WalkDir;

pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search for a regex pattern in files. Returns matching lines with file path and line number. \
         Use context_lines to include surrounding lines for readability (like grep -C)."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "pattern": {
                    "type": "string",
                    "description": "The regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search (default: current directory)"
                },
                "context_lines": {
                    "type": "integer",
                    "description": "Number of lines before and after each match to include (like grep -C, default: 0)"
                },
                "glob": {
                    "type": "string",
                    "description": "Only search files matching this glob pattern (e.g. \"*.rs\", \"*.ts\")"
                }
            }),
            required: vec!["pattern".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let pattern = input["pattern"]
            .as_str()
            .context("Missing pattern parameter")?;
        let path = input["path"].as_str().unwrap_or(".");
        let context_lines = input["context_lines"].as_u64().unwrap_or(0) as usize;
        let glob_filter = input["glob"].as_str();

        let regex = Regex::new(pattern)
            .with_context(|| format!("Invalid regex pattern: {}", pattern))?;

        let mut output_lines: Vec<String> = Vec::new();
        let mut file_count = 0;
        let mut match_count = 0;
        const MAX_MATCHES: usize = 100;

        'outer: for entry in WalkDir::new(path)
            .max_depth(10)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            // Apply glob filter if provided
            if let Some(glob_pat) = glob_filter {
                let file_name = entry.file_name().to_string_lossy();
                if !glob_match(glob_pat, &file_name) {
                    continue;
                }
            }

            file_count += 1;
            let contents = match fs::read_to_string(entry.path()) {
                Ok(c) => c,
                Err(_) => continue, // skip binary files
            };

            let all_lines: Vec<&str> = contents.lines().collect();
            let total = all_lines.len();

            // Collect all matching line indices
            let mut match_indices: Vec<usize> = all_lines
                .iter()
                .enumerate()
                .filter(|(_, line)| regex.is_match(line))
                .map(|(i, _)| i)
                .collect();

            if match_indices.is_empty() {
                continue;
            }

            // Build the set of lines to print (matches + context), preserving order
            let mut lines_to_print: BTreeSet<usize> = BTreeSet::new();
            for &idx in &match_indices {
                let start = idx.saturating_sub(context_lines);
                let end = (idx + context_lines + 1).min(total);
                for i in start..end {
                    lines_to_print.insert(i);
                }
            }

            let file_path = entry.path().display().to_string();
            let match_set: std::collections::HashSet<usize> = match_indices.iter().copied().collect();

            let mut prev_printed: Option<usize> = None;
            for &line_idx in &lines_to_print {
                // Print separator between non-consecutive groups
                if let Some(prev) = prev_printed {
                    if line_idx > prev + 1 {
                        output_lines.push(format!("{}:---", file_path));
                    }
                }

                let marker = if match_set.contains(&line_idx) { ">" } else { " " };
                output_lines.push(format!(
                    "{}:{}{}: {}",
                    file_path,
                    marker,
                    line_idx + 1,
                    all_lines[line_idx]
                ));
                prev_printed = Some(line_idx);
                match_count += match_set.contains(&line_idx) as usize;

                if match_count >= MAX_MATCHES {
                    output_lines.push(format!("... (stopped at {} matches)", MAX_MATCHES));
                    break 'outer;
                }
            }
        }

        if output_lines.is_empty() {
            Ok(format!("No matches found in {} files.", file_count))
        } else {
            Ok(output_lines.join("\n"))
        }
    }
}

/// Simple glob matching for file extensions (e.g. "*.rs", "*.ts")
fn glob_match(pattern: &str, name: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        return name.ends_with(&format!(".{}", ext));
    }
    // Fallback: substring match
    name.contains(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext<'static> {
        ToolContext {
            conversation: None,
            save_models: None,
            batch_trainer: None,
            local_generator: None,
            tokenizer: None,
            repl_mode: None,
            plan_content: None,
        }
    }

    #[tokio::test]
    async fn test_grep_in_cargo_toml() {
        let result = GrepTool.execute(
            serde_json::json!({"pattern": "name.*=", "path": "Cargo.toml"}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_grep_with_context_lines() {
        let result = GrepTool.execute(
            serde_json::json!({"pattern": "\\[package\\]", "path": "Cargo.toml", "context_lines": 2}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
        let out = result.unwrap();
        // Should contain the match and context around it
        assert!(out.contains("Cargo.toml"));
    }

    #[tokio::test]
    async fn test_grep_glob_filter() {
        let result = GrepTool.execute(
            serde_json::json!({"pattern": "fn main", "path": "src", "glob": "*.rs"}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_grep_invalid_regex() {
        let result = GrepTool.execute(
            serde_json::json!({"pattern": "[invalid(", "path": "."}),
            &ctx(),
        ).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_grep_no_matches() {
        let result = GrepTool.execute(
            serde_json::json!({"pattern": "ZZZNOMATCHZZZ", "path": "Cargo.toml"}),
            &ctx(),
        ).await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("No matches"));
    }
}
