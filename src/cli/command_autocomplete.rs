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
    Brain,
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
            CommandCategory::Brain => write!(f, "🧠 Brain"),
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
                    description: "Exit Finch",
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
                    description: "Show routing statistics and disabled training status",
                    category: CommandCategory::Basic,
                },

                // Model Commands
                CommandSpec {
                    name: "/model",
                    params: None,
                    description: "Show the active named model profile",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/model list",
                    params: None,
                    description: "List configured cloud and local model profiles",
                    category: CommandCategory::Model,
                },
                CommandSpec {
                    name: "/model",
                    params: Some("<name>"),
                    description: "Switch profiles without clearing conversation context",
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

                // Private feedback commands
                CommandSpec {
                    name: "/critical",
                    params: Some("[note]"),
                    description: "Store a private critical-error rating (10x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/medium",
                    params: Some("[note]"),
                    description: "Store a private needs-improvement rating (3x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/good",
                    params: Some("[note]"),
                    description: "Store a private good-response rating (1x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback critical",
                    params: Some("[note]"),
                    description: "Store a private critical-error rating (10x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback high",
                    params: Some("[note]"),
                    description: "Store a private critical-error rating (10x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback medium",
                    params: Some("[note]"),
                    description: "Store a private needs-improvement rating (3x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback good",
                    params: Some("[note]"),
                    description: "Store a private good-response rating (1x weight metadata)",
                    category: CommandCategory::Feedback,
                },
                CommandSpec {
                    name: "/feedback normal",
                    params: Some("[note]"),
                    description: "Store a private good-response rating (1x weight metadata)",
                    category: CommandCategory::Feedback,
                },

                // Durable Brain collaboration
                CommandSpec {
                    name: "/say",
                    params: Some("<text>"),
                    description: "Relay text without invoking the model",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/who",
                    params: None,
                    description: "List connected participants in this Brain",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/whois",
                    params: Some("<subject>"),
                    description: "Show public presence for one participant",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain list",
                    params: None,
                    description: "List named Brain sessions",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brains",
                    params: None,
                    description: "List named Brain sessions",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain runs",
                    params: None,
                    description: "List runs in the attached Brain",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain initialize",
                    params: None,
                    description: "Schedule the reviewed Brain initialization module",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain cancel",
                    params: Some("<run>"),
                    description: "Cancel an initiated run by ID prefix",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain create",
                    params: Some("<name>"),
                    description: "Create an empty Brain on this machine",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain attach",
                    params: Some("<name>[@machine]"),
                    description: "Attach to a local or remote Brain",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain invite",
                    params: Some("[role] [minutes]"),
                    description: "Create a short-lived single-use invitation",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain join",
                    params: Some("<name@machine> <invite>"),
                    description: "Redeem an invitation and attach",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain detach",
                    params: None,
                    description: "Return to this console's home Brain",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain handoff",
                    params: Some("<subject>"),
                    description: "Request an addressed runner transfer",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain handoff identity",
                    params: None,
                    description: "Show this frontend's runner identity",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain handoff accept",
                    params: Some("[id]"),
                    description: "Accept a runner transfer addressed to this frontend",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain handoff cancel",
                    params: Some("[id]"),
                    description: "Cancel a pending runner transfer",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain archive",
                    params: Some("<name>"),
                    description: "Archive an inactive Brain",
                    category: CommandCategory::Brain,
                },
                CommandSpec {
                    name: "/brain password",
                    params: Some("[new]"),
                    description: "Show or rotate the local Brain credential",
                    category: CommandCategory::Brain,
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
        let direct = self
            .commands
            .iter()
            .filter(|cmd| cmd.name.to_lowercase().starts_with(&prefix_lower))
            .cloned()
            .collect::<Vec<_>>();
        if !direct.is_empty() {
            return direct;
        }

        // Once arguments begin, keep the longest matching command visible so
        // its parameter syntax and description remain contextual. A longer
        // subcommand prefix always wins through the direct-match path above.
        let command_len = self
            .commands
            .iter()
            .filter(|command| {
                let name = command.name.to_lowercase();
                prefix_lower.starts_with(&format!("{name} "))
            })
            .map(|command| command.name.len())
            .max();
        let Some(command_len) = command_len else {
            return Vec::new();
        };
        let mut contextual = self
            .commands
            .iter()
            .filter(|command| {
                command.name.len() == command_len
                    && prefix_lower.starts_with(&format!("{} ", command.name.to_lowercase()))
            })
            .cloned()
            .collect::<Vec<_>>();
        if contextual.iter().any(|command| command.params.is_some()) {
            contextual.retain(|command| command.params.is_some());
        }
        contextual
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

        // Match /compact (starts with /co, not /cl)
        let matches = registry.match_prefix("/co");
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
    fn test_match_prefix_keeps_context_for_command_arguments() {
        let registry = CommandRegistry::new();

        let matches = registry.match_prefix("/brain archive old-session");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "/brain archive");
        assert_eq!(matches[0].params, Some("<name>"));

        let exact_with_space = registry.match_prefix("/brain list ");
        assert_eq!(exact_with_space.len(), 1);
        assert_eq!(exact_with_space[0].name, "/brain list");
    }

    #[test]
    fn test_match_prefix_prefers_a_matching_nested_subcommand_over_parent_arguments() {
        let registry = CommandRegistry::new();
        let matches = registry.match_prefix("/brain handoff c");

        assert!(matches
            .iter()
            .all(|command| command.name.starts_with("/brain handoff c")));
        assert!(matches
            .iter()
            .any(|command| command.name == "/brain handoff cancel"));
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

    #[test]
    fn legacy_peer_and_channel_commands_are_not_suggested() {
        let registry = CommandRegistry::new();
        for removed in [
            "/discover",
            "/machines",
            "/peers",
            "/nodes",
            "/join",
            "/part",
            "/connect",
            "/disconnect",
            "/room",
            "/rooms",
            "/self-peer",
            "/balance",
            "/settle",
            "/join-registry",
            "/registry",
            "/gas-send",
        ] {
            assert!(
                registry
                    .all_commands()
                    .iter()
                    .all(|command| command.name != removed),
                "removed command {removed} remains in autocomplete"
            );
        }
    }

    #[test]
    fn authoritative_brain_collaboration_commands_are_suggested() {
        let registry = CommandRegistry::new();
        for supported in [
            "/say",
            "/who",
            "/whois",
            "/brain list",
            "/brains",
            "/brain runs",
            "/brain initialize",
            "/brain cancel",
            "/brain create",
            "/brain attach",
            "/brain invite",
            "/brain join",
            "/brain detach",
            "/brain handoff",
            "/brain handoff identity",
            "/brain handoff accept",
            "/brain handoff cancel",
            "/brain archive",
            "/brain password",
        ] {
            assert!(
                registry
                    .all_commands()
                    .iter()
                    .any(|command| command.name == supported),
                "supported command {supported} is missing from autocomplete"
            );
        }
    }

    #[test]
    fn test_autocomplete_truthfully_describes_feedback_without_training_claims() {
        let registry = CommandRegistry::new();
        let training = registry
            .all_commands()
            .iter()
            .find(|command| command.name == "/training")
            .unwrap();
        assert_eq!(
            training.description,
            "Show routing statistics and disabled training status"
        );

        for command in registry.by_category(CommandCategory::Feedback) {
            assert!(command.description.starts_with("Store a private "));
            let description = command.description.to_ascii_lowercase();
            assert!(description.contains("weight metadata"));
            assert!(!description.contains("priority"));
            assert!(!description.contains("training"));
            assert!(!description.contains("lora"));
        }
    }
}
