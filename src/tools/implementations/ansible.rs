// Ansible tool - execute playbooks and ad-hoc commands across machines
//
// Execute and check. No ceremony.

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::process::Command;

pub struct AnsibleTool;

#[async_trait]
impl Tool for AnsibleTool {
    fn name(&self) -> &str {
        "ansible"
    }

    fn description(&self) -> &str {
        "Execute an Ansible playbook or ad-hoc command across machines. \
         Provide either a playbook path or an ad-hoc module with hosts. \
         Returns stdout, stderr, and exit code."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema {
            schema_type: "object".to_string(),
            properties: serde_json::json!({
                "playbook": {
                    "type": "string",
                    "description": "Path to the Ansible playbook (.yml/.yaml) to run"
                },
                "hosts": {
                    "type": "string",
                    "description": "Host pattern for ad-hoc commands (e.g. 'all', 'webservers', '192.168.1.1')"
                },
                "module": {
                    "type": "string",
                    "description": "Ad-hoc module name (e.g. 'ping', 'shell', 'command')"
                },
                "args": {
                    "type": "string",
                    "description": "Module arguments for ad-hoc commands"
                },
                "inventory": {
                    "type": "string",
                    "description": "Path to inventory file (optional, uses default if omitted)"
                },
                "extra_vars": {
                    "type": "string",
                    "description": "Extra variables as key=value pairs or JSON string"
                },
                "apply": {
                    "type": "boolean",
                    "description": "Actually apply changes. Default is dry-run (check mode). Must be explicitly true to make real changes."
                }
            }),
            required: vec![],
        }
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let playbook = input["playbook"].as_str();
        let hosts = input["hosts"].as_str();
        let module = input["module"].as_str();
        let args = input["args"].as_str();
        let inventory = input["inventory"].as_str();
        let extra_vars = input["extra_vars"].as_str();
        // Default to dry-run (--check) unless apply is explicitly true.
        let apply = input["apply"].as_bool().unwrap_or(false);
        let check = !apply;

        let output = if let Some(playbook) = playbook {
            // Playbook mode
            let mut cmd = Command::new("ansible-playbook");
            cmd.arg(playbook);
            if let Some(inv) = inventory {
                cmd.args(["-i", inv]);
            }
            if let Some(ev) = extra_vars {
                cmd.args(["-e", ev]);
            }
            if check {
                cmd.arg("--check");
            }
            cmd.output().context("Failed to run ansible-playbook")?
        } else if let Some(hosts) = hosts {
            // Ad-hoc mode
            let mut cmd = Command::new("ansible");
            cmd.arg(hosts);
            if let Some(m) = module {
                cmd.args(["-m", m]);
            }
            if let Some(a) = args {
                cmd.args(["-a", a]);
            }
            if let Some(inv) = inventory {
                cmd.args(["-i", inv]);
            }
            if let Some(ev) = extra_vars {
                cmd.args(["-e", ev]);
            }
            if check {
                cmd.arg("--check");
            }
            cmd.output().context("Failed to run ansible")?
        } else {
            return Ok("Provide either a playbook path or hosts + module for ad-hoc execution.".to_string());
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status.code().unwrap_or(-1);

        let mut result = String::new();
        if check {
            result.push_str("[dry-run — pass apply=true to make real changes]\n");
        }
        if !stdout.is_empty() {
            result.push_str(&stdout);
        }
        if !stderr.is_empty() {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&stderr);
        }
        result.push_str(&format!("\nexit: {}", status));

        Ok(result)
    }
}
