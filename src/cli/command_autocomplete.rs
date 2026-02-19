// Command autocomplete system with descriptions and parameter hints
//
// Provides real-time dropdown autocomplete for slash commands as the user types.

use std::fmt;

/// Command definition with description and parameter hints
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// Command name (e.g., "/clear")
    pub name: &'static str,

    /// Optional parameter syntax (e.g., "[instruction]", "<name>")
    pub params: Option<&'static str>,

    /// Human-readable description
    pub description: &'static str,

    /// Category for grouping
    pub category: CommandCategory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandCategory {
    Basic,
    Model,
    Mcp,
    Persona,
    Patterns,
    Feedback,
    Memory,
    Discovery,
}

impl fmt::Display for CommandCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandCategory::Basic => write!(f, "📋 Basic Commands"),
            CommandCategory::Model => write!(f, "🤖 Model Commands"),
            CommandCategory::Mcp => write!(f, "🔌 MCP Plugin"),
            CommandCategory::Persona => write!(f, "🎭 Persona"),
            CommandCategory::Patterns => write!(f, "🔒 Tool Patterns"),
            CommandCategory::Feedback => write!(f, "🎓 Feedback"),
            CommandCategory::Memory => write!(f, "💾 Memory"),
            CommandCategory::Discovery => write!(f, "🔍 Discovery"),
        }
    }
}

impl CommandSpec {
    /// Get full command syntax (name + params)
    pub fn full_syntax(&self) -> String {
        if let Some(params) = self.params {
            format!("{} {}", self.name, params)
        } else {
            self.name.to_string()
        }
    }
}

/// Registry of all available commands
pub struct CommandRegistry {
    commands: Vec<CommandSpec>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: vec![
                // Basic Commands
                CommandSpec {
                    name: "/help",
                    params: None,
                    description: "Show available commands and shortcuts",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/quit",
                    params: None,
                    description: "Exit Finch (also: Ctrl+D)",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/exit",
                    params: None,
                    description: "Exit Finch (alias for /quit)",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/clear",
                    params: None,
                    description: "Clear conversation history and free up context",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/compact",
                    params: Some("[instruction]"),
                    description: "Clear history but keep a summary in context. Optional: /compact [instruction...]",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/debug",
                    params: None,
                    description: "Toggle debug output",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/metrics",
                    params: None,
                    description: "Display usage statistics",
                    category: CommandCategory::Basic,
                },
                CommandSpec {
                    name: "/training",
                    params: None,
                    description: "Show detailed training statistics",
                    category: CommandCategory::Basic,
                },

                // Model Commands
                CommandSpec {
                    name: "/model",
                    params: None,
                    description: "Show current active model/teacher",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/model list",
                    params: None,
                    description: "List all configured teachers (Claude, Grok, GPT-4, etc.)",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/model",
                    params: Some("<name>"),
                    description: "Switch to a specific teacher (e.g., /model grok)",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/teacher",
                    params: None,
                    description: "Alias for /model commands",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/local",
                    params: Some("<query>"),
                    description: "Query local model directly (bypass routing)",
                    category: CommandCategory::Model,
                },

                // Memory Commands
                CommandSpec {
                    name: "/memory",
                    params: None,
                    description: "Show memory usage (system and process)",
                    category: CommandCategory::Memory,
                },

                // MCP Plugin Commands
                CommandSpec {
                    name: "/mcp",
                    params: None,
                    description: "List connected MCP servers",
                    category: CommandCategory::Mcp,
                },
                CommandSpec {
                    name: "/mcp list",
                    params: None,
                    description: "List connected MCP servers",
                    category: CommandCategory::Mcp,
                },
                CommandSpec {
                    name: "/mcp tools",
                    params: None,
                    description: "List all MCP tools from all servers",
                    category: CommandCategory::Mcp,
                },
                CommandSpec {
                    name: "/mcp tools",
                    params: Some("<server>"),
                    description: "List tools from specific server",
                    category: CommandCategory::Mcp,
                },
                CommandSpec {
                    name: "/mcp refresh",
                    params: None,
                    description: "Refresh tool list from all servers",
                    category: CommandCategory::Mcp,
                },
                CommandSpec {
                    name: "/mcp reload",
                    params: None,
                    description: "Reconnect to all MCP servers",
                    category: CommandCategory::Mcp,
                },

                // Persona Commands
                CommandSpec {
                    name: "/persona",
                    params: None,
                    description: "List available personas",
                    category: CommandCategory::Persona,
                },
                CommandSpec {
                    name: "/persona list",
                    params: None,
                    description: "List available personas",
                    category: CommandCategory::Persona,
                },
                CommandSpec {
                    name: "/persona select",
                    params: Some("<name>"),
                    description: "Switch to a different persona",
                    category: CommandCategory::Persona,
                },
                CommandSpec {
                    name: "/persona show",
                    params: None,
                    description: "Show current persona and system prompt",
                    category: CommandCategory::Persona,
                },

                // Tool Pattern Commands
                CommandSpec {
                    name: "/patterns",
                    params: None,
                    description: "List all saved confirmation patterns",
                    category: CommandCategory::Patterns,
                },
                CommandSpec {
                    name: "/patterns list",
                    params: None,
                    description: "List all saved confirmation patterns",
                    category: CommandCategory::Patterns,
                },
                CommandSpec {
                    name: "/patterns add",
                    params: None,
                    description: "Add a new pattern (interactive wizard)",
                    category: CommandCategory::Patterns,
                },
                CommandSpec {
                    name: "/patterns rm",
                    params: Some("<id>"),
                    description: "Remove a specific pattern by ID",
                    category: CommandCategory::Patterns,
                },
                CommandSpec {
                    name: "/patterns remove",
                    params: Some("<id>"),
                    description: "Remove a specific pattern by ID",
                    category: CommandCategory::Patterns,
                },
                CommandSpec {
                    name: "/patterns clear",
                    params: None,
                    description: "Remove all patterns (requires confirmation)",
                    category: CommandCategory::Patterns,
                },

                // Feedback Commands (LoRA Training)
                CommandSpec {
                    name: "/critical",
                    params: Some("[note]"),
                    description: "Mark response as critical error (10x training weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/medium",
                    params: Some("[note]"),
                    description: "Mark response needs improvement (3x weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/good",
                    params: Some("[note]"),
                    description: "Mark response as good example (1x weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback critical",
                    params: Some("[note]"),
                    description: "Mark response as critical error (10x training weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback high",
                    params: Some("[note]"),
                    description: "Mark response as critical error (10x training weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback medium",
                    params: Some("[note]"),
                    description: "Mark response needs improvement (3x weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback good",
                    params: Some("[note]"),
                    description: "Mark response as good example (1x weight)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback normal",
                    params: Some("[note]"),
                    description: "Mark response as good example (1x weight)",
                    category: CommandCategory::Feedback,
                },

                // Discovery Commands
                CommandSpec {
                    name: "/discover",
                    params: None,
                    description: "Discover Finch daemons on local network",
                    category: CommandCategory::Discovery,
                },
            ],
        }
    }

    /// Get all commands matching a prefix
    pub fn match_prefix(&self, prefix: &str) -> Vec<CommandSpec> {
        if prefix.is_empty() {
            return Vec::new();
        }

        let prefix_lower = prefix.to_lowercase();

        self.commands
            .iter()
            .filter(|cmd| cmd.name.to_lowercase().starts_with(&prefix_lower))
            .cloned()
            .collect()
    }

    /// Get all commands in a category
    pub fn by_category(&self, category: CommandCategory) -> Vec<CommandSpec> {
        self.commands
            .iter()
            .filter(|cmd| cmd.category == category)
            .cloned()
            .collect()
    }

    /// Get all commands
    pub fn all_commands(&self) -> &[CommandSpec] {
        &self.commands
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_match_prefix() {
        let registry = CommandRegistry::new();

        // Match /clear
        let matches = registry.match_prefix("/cl");
        assert!(matches.iter().any(|cmd| cmd.name == "/clear"));
        assert!(matches.iter().any(|cmd| cmd.name == "/compact"));

        // Match /model
        let matches = registry.match_prefix("/mod");
        assert!(matches.iter().any(|cmd| cmd.name == "/model"));

        // Match /mcp
        let matches = registry.match_prefix("/mcp");
        assert!(matches.iter().any(|cmd| cmd.name == "/mcp"));
        assert!(matches.iter().any(|cmd| cmd.name == "/mcp list"));
    }

    #[test]
    fn test_by_category() {
        let registry = CommandRegistry::new();

        let basic = registry.by_category(CommandCategory::Basic);
        assert!(basic.iter().any(|cmd| cmd.name == "/help"));
        assert!(basic.iter().any(|cmd| cmd.name == "/clear"));

        let model = registry.by_category(CommandCategory::Model);
        assert!(model.iter().any(|cmd| cmd.name == "/model"));
        assert!(model.iter().any(|cmd| cmd.name == "/local"));
    }

    #[test]
    fn test_full_syntax() {
        let cmd = CommandSpec {
            name: "/compact",
            params: Some("[instruction]"),
            description: "Test",
            category: CommandCategory::Basic,
        };

        assert_eq!(cmd.full_syntax(), "/compact [instruction]");

        let cmd_no_params = CommandSpec {
            name: "/clear",
            params: None,
            description: "Test",
            category: CommandCategory::Basic,
        };

        assert_eq!(cmd_no_params.full_syntax(), "/clear");
    }
}
