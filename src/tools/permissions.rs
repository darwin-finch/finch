// Permission system for tool execution
//
// Implements constitutional constraints: "Would 1000 users do this?"
// Multi-layer defense: Allow, Ask, or Deny tool execution

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tracing::{debug, warn};

/// Permission decision for a tool execution
#[derive(Debug, Clone, PartialEq)]
pub enum PermissionCheck {
    /// Execute immediately without user confirmation
    Allow,

    /// Prompt user with explanation before executing
    AskUser(String),

    /// Block execution with reason
    Deny(String),
}

/// Permission rule configuration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PermissionRule {
    Allow,
    Ask,
    Deny,
}

/// Configuration for a specific tool's permissions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolPermissionConfig {
    pub enabled: bool,
    pub rule: PermissionRule,
    #[serde(default)]
    pub allowed_patterns: Vec<String>,
    #[serde(default)]
    pub blocked_patterns: Vec<String>,
}

impl Default for ToolPermissionConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rule: PermissionRule::Ask,
            allowed_patterns: Vec::new(),
            blocked_patterns: Vec::new(),
        }
    }
}

/// Permission manager - checks if tool execution is allowed
pub struct PermissionManager {
    /// Per-tool configuration
    configs: HashMap<String, ToolPermissionConfig>,

    /// Default rule for tools without explicit config
    default_rule: PermissionRule,

    /// Maximum number of tool turns (prevent infinite loops)
    pub max_tool_turns: usize,
}

impl PermissionManager {
    /// Create new permission manager with default settings
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
        }
    }

    /// Load from configuration
    pub fn from_config(configs: HashMap<String, ToolPermissionConfig>) -> Self {
        Self {
            configs,
            default_rule: PermissionRule::Ask,
            max_tool_turns: 25,
        }
    }

    /// Set default rule for unconfigured tools
    pub fn with_default_rule(mut self, rule: PermissionRule) -> Self {
        self.default_rule = rule;
        self
    }

    /// Set maximum tool turns
    pub fn with_max_turns(mut self, max_turns: usize) -> Self {
        self.max_tool_turns = max_turns;
        self
    }

    /// Register tool-specific configuration
    pub fn register_tool_config(&mut self, tool_name: String, config: ToolPermissionConfig) {
        self.configs.insert(tool_name, config);
    }

    /// Check if tool execution is permitted
    pub fn check_tool_use(&self, tool_name: &str, input: &Value) -> PermissionCheck {
        // Get tool config or use default
        let config = self.configs.get(tool_name);

        // Check if tool is enabled
        if let Some(cfg) = config {
            if !cfg.enabled {
                return PermissionCheck::Deny(format!("Tool '{}' is disabled", tool_name));
            }
        }

        // Apply constitutional constraints (safety checks)
        if let Some(reason) = self.check_constitutional_constraints(tool_name, input) {
            return PermissionCheck::Deny(reason);
        }

        // Apply tool-specific patterns
        if let Some(cfg) = config {
            if let Some(reason) = self.check_patterns(tool_name, input, cfg) {
                return reason;
            }
        }

        // Use configured rule or default
        match config.map(|c| &c.rule).unwrap_or(&self.default_rule) {
            PermissionRule::Allow => PermissionCheck::Allow,
            PermissionRule::Ask => PermissionCheck::AskUser(format!("Execute {} tool?", tool_name)),
            PermissionRule::Deny => {
                PermissionCheck::Deny(format!("Tool '{}' is not allowed", tool_name))
            }
        }
    }

    /// Apply constitutional constraints (safety checks)
    fn check_constitutional_constraints(&self, tool_name: &str, input: &Value) -> Option<String> {
        match tool_name {
            "bash" => self.check_bash_safety(input),
            "read" => self.check_read_safety(input),
            "web_fetch" => self.check_web_fetch_safety(input),
            _ => None,
        }
    }

    /// Check if bash command is safe
    fn check_bash_safety(&self, input: &Value) -> Option<String> {
        let command = input.get("command")?.as_str()?;

        // Blocked patterns (always deny)
        let dangerous_patterns = vec![
            ("rm -rf", "Recursive deletion is dangerous"),
            ("dd if=", "Disk operations are dangerous"),
            (":(){ :|:& };:", "Fork bombs are blocked"),
            ("sudo", "Privilege escalation requires manual execution"),
            ("chmod 777", "Unsafe permission changes are blocked"),
            ("> /dev/", "Direct device access is dangerous"),
            ("mkfs", "Filesystem operations are dangerous"),
            ("fdisk", "Disk partitioning is dangerous"),
        ];

        for (pattern, reason) in dangerous_patterns {
            if command.contains(pattern) {
                warn!("Blocked dangerous bash command: {}", command);
                return Some(format!("Blocked: {}", reason));
            }
        }

        None
    }

    /// Check if file read is safe
    fn check_read_safety(&self, input: &Value) -> Option<String> {
        let file_path = input.get("file_path")?.as_str()?;

        // Block system files
        let system_paths = vec![
            "/etc/passwd",
            "/etc/shadow",
            "/etc/sudoers",
            "/dev/",
            "/proc/",
            "/sys/",
        ];

        for blocked_path in system_paths {
            if file_path.starts_with(blocked_path) {
                warn!("Blocked system file access: {}", file_path);
                return Some(format!(
                    "Blocked: Access to system files ({}) is not allowed",
                    blocked_path
                ));
            }
        }

        None
    }

    /// Check if web fetch is safe
    fn check_web_fetch_safety(&self, input: &Value) -> Option<String> {
        let url = input.get("url")?.as_str()?;

        // Block dangerous URL schemes
        let blocked_schemes = vec!["file://", "javascript:", "data:", "vbscript:"];

        for scheme in blocked_schemes {
            if url.to_lowercase().starts_with(scheme) {
                warn!("Blocked dangerous URL scheme: {}", url);
                return Some(format!("Blocked: URL scheme '{}' is not allowed", scheme));
            }
        }

        // Block private IP ranges
        if Self::is_private_url(url) {
            warn!("Blocked private IP access: {}", url);
            return Some("Blocked: Access to private IP addresses is not allowed".to_string());
        }

        None
    }

    /// Check if URL points to private IP
    fn is_private_url(url: &str) -> bool {
        // Simple check for common private IPs
        let private_patterns = vec![
            "127.0.0.1",
            "localhost",
            "192.168.",
            "10.",
            "172.16.",
            "172.17.",
            "172.18.",
            "172.19.",
            "172.20.",
            "172.21.",
            "172.22.",
            "172.23.",
            "172.24.",
            "172.25.",
            "172.26.",
            "172.27.",
            "172.28.",
            "172.29.",
            "172.30.",
            "172.31.",
        ];

        private_patterns.iter().any(|p| url.contains(p))
    }

    /// Check tool-specific allowed/blocked patterns
    fn check_patterns(
        &self,
        tool_name: &str,
        input: &Value,
        config: &ToolPermissionConfig,
    ) -> Option<PermissionCheck> {
        let input_str = serde_json::to_string(input).ok()?;

        // Check blocked patterns first
        for pattern in &config.blocked_patterns {
            if input_str.contains(pattern) {
                debug!("Tool {} blocked by pattern: {}", tool_name, pattern);
                return Some(PermissionCheck::Deny(format!(
                    "Blocked by pattern: {}",
                    pattern
                )));
            }
        }

        // If allowed patterns specified, input must match one
        if !config.allowed_patterns.is_empty() {
            let matches = config
                .allowed_patterns
                .iter()
                .any(|p| input_str.contains(p));
            if !matches {
                return Some(PermissionCheck::AskUser(format!(
                    "Tool {} input doesn't match allowed patterns",
                    tool_name
                )));
            }
        }

        None
    }
}

impl Default for PermissionManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bash_dangerous_commands_blocked() {
        let manager = PermissionManager::new();

        let dangerous_commands = vec![
            "rm -rf /",
            "dd if=/dev/zero of=/dev/sda",
            ":(){ :|:& };:",
            "sudo rm file",
            "chmod 777 /etc",
        ];

        for cmd in dangerous_commands {
            let input = serde_json::json!({"command": cmd});
            let check = manager.check_tool_use("bash", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                cmd
            );
        }
    }

    #[test]
    fn test_system_files_blocked() {
        let manager = PermissionManager::new();

        let system_files = vec!["/etc/passwd", "/etc/shadow", "/dev/null"];

        for file in system_files {
            let input = serde_json::json!({"file_path": file});
            let check = manager.check_tool_use("read", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                file
            );
        }
    }

    #[test]
    fn test_dangerous_url_schemes_blocked() {
        let manager = PermissionManager::new();

        let dangerous_urls = vec![
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
        ];

        for url in dangerous_urls {
            let input = serde_json::json!({"url": url});
            let check = manager.check_tool_use("web_fetch", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                url
            );
        }
    }

    #[test]
    fn test_private_ip_blocked() {
        let manager = PermissionManager::new();

        let private_urls = vec![
            "http://127.0.0.1/",
            "http://localhost/",
            "http://192.168.1.1/",
            "http://10.0.0.1/",
        ];

        for url in private_urls {
            let input = serde_json::json!({"url": url});
            let check = manager.check_tool_use("web_fetch", &input);
            assert!(
                matches!(check, PermissionCheck::Deny(_)),
                "Failed to block: {}",
                url
            );
        }
    }

    #[test]
    fn test_safe_bash_command_requires_ask() {
        let manager = PermissionManager::new();

        let input = serde_json::json!({"command": "ls -la"});
        let check = manager.check_tool_use("bash", &input);
        assert!(matches!(check, PermissionCheck::AskUser(_)));
    }

    #[test]
    fn test_disabled_tool() {
        let mut manager = PermissionManager::new();
        manager.register_tool_config(
            "bash".to_string(),
            ToolPermissionConfig {
                enabled: false,
                rule: PermissionRule::Allow,
                allowed_patterns: vec![],
                blocked_patterns: vec![],
            },
        );

        let input = serde_json::json!({"command": "ls"});
        let check = manager.check_tool_use("bash", &input);
        assert!(matches!(check, PermissionCheck::Deny(_)));
    }

    #[test]
    fn test_allowed_patterns() {
        let mut manager = PermissionManager::new();
        manager.register_tool_config(
            "test".to_string(),
            ToolPermissionConfig {
                enabled: true,
                rule: PermissionRule::Allow,
                allowed_patterns: vec!["safe_pattern".to_string()],
                blocked_patterns: vec![],
            },
        );

        // Should allow matching pattern
        let input = serde_json::json!({"data": "safe_pattern"});
        let check = manager.check_tool_use("test", &input);
        assert!(matches!(check, PermissionCheck::Allow));

        // Should ask for non-matching pattern
        let input = serde_json::json!({"data": "other_pattern"});
        let check = manager.check_tool_use("test", &input);
        assert!(matches!(check, PermissionCheck::AskUser(_)));
    }
}
