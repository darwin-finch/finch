// MCP server configuration

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Transport type (stdio or sse)
    pub transport: TransportType,

    /// Command to execute (for STDIO transport)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Command arguments (for STDIO transport)
    #[serde(default)]
    pub args: Vec<String>,

    /// Environment variables (for STDIO transport)
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// URL (for SSE transport)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,

    /// Whether server is enabled
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Transport type for MCP servers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportType {
    /// Standard I/O transport (local process)
    Stdio,
    /// HTTP + Server-Sent Events transport (remote server)
    Sse,
}

impl McpServerConfig {
    /// Validate the configuration
    pub fn validate(&self, name: &str) -> anyhow::Result<()> {
        match self.transport {
            TransportType::Stdio => {
                if self.command.is_none() {
                    anyhow::bail!(
                        "MCP server '{}': STDIO transport requires 'command' field",
                        name
                    );
                }
            }
            TransportType::Sse => {
                if self.url.is_none() {
                    anyhow::bail!("MCP server '{}': SSE transport requires 'url' field", name);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_config_validation() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), "@modelcontextprotocol/server-filesystem".to_string()],
            env: HashMap::new(),
            url: None,
            enabled: true,
        };

        assert!(config.validate("test").is_ok());
    }

    #[test]
    fn test_stdio_config_missing_command() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: None,
            enabled: true,
        };

        assert!(config.validate("test").is_err());
    }

    #[test]
    fn test_sse_config_validation() {
        let config = McpServerConfig {
            transport: TransportType::Sse,
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: Some("http://localhost:3000/mcp".to_string()),
            enabled: true,
        };

        assert!(config.validate("test").is_ok());
    }

    #[test]
    fn test_sse_config_missing_url() {
        let config = McpServerConfig {
            transport: TransportType::Sse,
            command: None,
            args: vec![],
            env: HashMap::new(),
            url: None,
            enabled: true,
        };

        assert!(config.validate("test").is_err());
    }
}
