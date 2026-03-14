// HashCompare tool - hash and compare two files
//
// Returns MD5 hashes of both files and whether they are identical.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::fs;

pub struct HashCompareTool;

fn md5_hex(data: &[u8]) -> String {
    // Simple MD5 implementation via md5 crate if available, else use sha2.
    // We use the standard library approach: compute via std::hash is not crypto,
    // so we shell out to md5sum/md5 for correctness.
    // Instead, compute a simple fingerprint using std — use a rolling XOR+sum.
    // For a real hash, we use the system md5 command via std::process::Command.
    let output = std::process::Command::new("md5")
        .arg("-q")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(data)?;
            let out = child.wait_with_output()?;
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        });

    match output {
        Ok(hash) if !hash.is_empty() => hash,
        _ => {
            // Fallback: md5sum (Linux)
            let out = std::process::Command::new("md5sum")
                .stdin({
                    use std::process::Stdio;
                    Stdio::piped()
                })
                .stdout(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    child.stdin.take().unwrap().write_all(data)?;
                    let o = child.wait_with_output()?;
                    Ok(String::from_utf8_lossy(&o.stdout)
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string())
                })
                .unwrap_or_else(|_| "unavailable".to_string());
            out
        }
    }
}

#[async_trait]
impl Tool for HashCompareTool {
    fn name(&self) -> &str {
        "hash_compare"
    }

    fn description(&self) -> &str {
        "Hash two files and compare them. Returns the MD5 hash of each file and whether they are identical."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "file_a": {
                    "type": "string",
                    "description": "Absolute path to the first file"
                },
                "file_b": {
                    "type": "string",
                    "description": "Absolute path to the second file"
                }
            }),
            required: vec!["file_a".to_string(), "file_b".to_string()],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let file_a = input["file_a"]
            .as_str()
            .context("Missing file_a parameter")?;
        let file_b = input["file_b"]
            .as_str()
            .context("Missing file_b parameter")?;

        let bytes_a =
            fs::read(file_a).with_context(|| format!("Failed to read file: {}", file_a))?;
        let bytes_b =
            fs::read(file_b).with_context(|| format!("Failed to read file: {}", file_b))?;

        let hash_a = md5_hex(&bytes_a);
        let hash_b = md5_hex(&bytes_b);
        let identical = bytes_a == bytes_b;

        Ok(format!(
            "{}: {}\n{}: {}\n{}",
            file_a,
            hash_a,
            file_b,
            hash_b,
            if identical {
                "identical: yes"
            } else {
                "identical: no"
            }
        ))
    }
}
