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
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
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

    #[test]
    fn test_config_serialization() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("npx".to_string()),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-filesystem".to_string(),
            ],
            env: HashMap::new(),
            url: None,
            enabled: true,
        };

        let serialized = toml::to_string(&config).unwrap();
        assert!(serialized.contains("transport = \"stdio\""));
        assert!(serialized.contains("command = \"npx\""));

        let deserialized: McpServerConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.transport, TransportType::Stdio);
        assert_eq!(deserialized.command, Some("npx".to_string()));
        assert_eq!(deserialized.args.len(), 2);
    }

    #[test]
    fn test_config_with_environment_variables() {
        let mut env = HashMap::new();
        env.insert("GITHUB_TOKEN".to_string(), "test_token".to_string());
        env.insert("API_KEY".to_string(), "test_key".to_string());

        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("github-server".to_string()),
            args: vec![],
            env,
            url: None,
            enabled: true,
        };

        assert!(config.validate("github").is_ok());
        assert_eq!(config.env.len(), 2);
        assert_eq!(
            config.env.get("GITHUB_TOKEN"),
            Some(&"test_token".to_string())
        );
    }

    #[test]
    fn test_disabled_server_config() {
        let config = McpServerConfig {
            transport: TransportType::Stdio,
            command: Some("test".to_string()),
            args: vec![],
            env: HashMap::new(),
            url: None,
            enabled: false,
        };

        // Even invalid configs should validate if disabled
        // (validation happens at connection time if enabled)
        assert!(config.validate("test").is_ok());
    }

    #[test]
    fn test_transport_type_serde() {
        let stdio = TransportType::Stdio;
        let sse = TransportType::Sse;

        let stdio_str = serde_json::to_string(&stdio).unwrap();
        let sse_str = serde_json::to_string(&sse).unwrap();

        assert_eq!(stdio_str, "\"stdio\"");
        assert_eq!(sse_str, "\"sse\"");

        let stdio_de: TransportType = serde_json::from_str(&stdio_str).unwrap();
        let sse_de: TransportType = serde_json::from_str(&sse_str).unwrap();

        assert_eq!(stdio_de, TransportType::Stdio);
        assert_eq!(sse_de, TransportType::Sse);
    }
}
