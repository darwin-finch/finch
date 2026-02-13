// Slash command handling

use anyhow::Result;

use crate::metrics::MetricsLogger;
use crate::models::ThresholdValidator;
use crate::router::Router;

/// Output destination for commands
pub enum CommandOutput {
    Status(String),    // Short messages for status bar
    Message(String),   // Long content for scrollback area
}

pub enum Command {
    Help,
    Quit,
    Metrics,
    Memory,
    Debug,
    Training,
    Clear,
    PatternsList,
    PatternsRemove(String),
    PatternsClear,
    PatternsAdd,
    // Plan mode commands
    PlanModeToggle,  // Toggle plan mode on/off (Shift+Tab or /plan without args)
    Plan(String),
    Approve,
    Reject,
    ShowPlan,
    SavePlan,
    Done,
    // Feedback commands for weighted LoRA training
    FeedbackCritical(Option<String>), // High-weight (10x) - critical strategy errors
    FeedbackMedium(Option<String>),   // Medium-weight (3x) - improvements
    FeedbackGood(Option<String>),     // Normal-weight (1x) - good examples
    // Local model testing
    Local { query: String }, // Query local model directly (bypass routing)
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();

        // Handle simple commands without arguments
        match trimmed {
            "/help" => return Some(Command::Help),
            "/quit" | "/exit" => return Some(Command::Quit),
            "/metrics" => return Some(Command::Metrics),
            "/memory" => return Some(Command::Memory),
            "/debug" => return Some(Command::Debug),
            "/training" => return Some(Command::Training),
            "/clear" | "/reset" => return Some(Command::Clear),
            "/approve" | "/execute" => return Some(Command::Approve),
            "/reject" | "/cancel" => return Some(Command::Reject),
            "/show-plan" => return Some(Command::ShowPlan),
            "/save-plan" => return Some(Command::SavePlan),
            "/done" | "/complete" => return Some(Command::Done),
            // Feedback commands (simple form)
            "/critical" => return Some(Command::FeedbackCritical(None)),
            "/medium" => return Some(Command::FeedbackMedium(None)),
            "/good" => return Some(Command::FeedbackGood(None)),
            _ => {}
        }

        // Handle /plan command
        if trimmed == "/plan" {
            // Without arguments: toggle plan mode
            return Some(Command::PlanModeToggle);
        }

        if let Some(rest) = trimmed.strip_prefix("/plan ") {
            let task = rest.trim();
            if !task.is_empty() {
                return Some(Command::Plan(task.to_string()));
            } else {
                // "/plan " with only whitespace: toggle plan mode
                return Some(Command::PlanModeToggle);
            }
        }

        // Handle /feedback commands with optional explanation
        if let Some(rest) = trimmed
            .strip_prefix("/feedback critical ")
            .or_else(|| trimmed.strip_prefix("/feedback high "))
        {
            let explanation = rest.trim();
            return Some(Command::FeedbackCritical(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback critical" || trimmed == "/feedback high" {
            return Some(Command::FeedbackCritical(None));
        }

        if let Some(rest) = trimmed.strip_prefix("/feedback medium ") {
            let explanation = rest.trim();
            return Some(Command::FeedbackMedium(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback medium" {
            return Some(Command::FeedbackMedium(None));
        }

        if let Some(rest) = trimmed
            .strip_prefix("/feedback good ")
            .or_else(|| trimmed.strip_prefix("/feedback normal "))
        {
            let explanation = rest.trim();
            return Some(Command::FeedbackGood(if explanation.is_empty() {
                None
            } else {
                Some(explanation.to_string())
            }));
        }

        if trimmed == "/feedback good" || trimmed == "/feedback normal" {
            return Some(Command::FeedbackGood(None));
        }

        // Handle /local command with query
        if let Some(rest) = trimmed.strip_prefix("/local ") {
            let query = rest.trim();
            if !query.is_empty() {
                return Some(Command::Local {
                    query: query.to_string(),
                });
            }
        }

        // Handle /patterns commands with subcommands
        if trimmed == "/patterns" || trimmed == "/patterns list" {
            return Some(Command::PatternsList);
        }

        if trimmed == "/patterns clear" {
            return Some(Command::PatternsClear);
        }

        if trimmed == "/patterns add" {
            return Some(Command::PatternsAdd);
        }

        // Handle /patterns remove <id> and /patterns rm <id>
        if let Some(rest) = trimmed.strip_prefix("/patterns remove ") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(Command::PatternsRemove(id.to_string()));
            }
        }

        if let Some(rest) = trimmed.strip_prefix("/patterns rm ") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(Command::PatternsRemove(id.to_string()));
            }
        }

        None
    }
}

pub fn handle_command(
    command: Command,
    metrics_logger: &MetricsLogger,
    router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter
    validator: Option<&ThresholdValidator>,
    debug_enabled: &mut bool,
) -> Result<CommandOutput> {
    match command {
        // Long-form outputs go to scrollback
        Command::Help => Ok(CommandOutput::Message(format_help())),
        Command::Metrics => Ok(CommandOutput::Message(format_metrics(metrics_logger)?)),
        Command::Training => Ok(CommandOutput::Message(format_training(router, validator)?)),

        // Short outputs go to status bar
        Command::Debug => {
            *debug_enabled = !*debug_enabled;
            Ok(CommandOutput::Status(format!(
                "Debug mode: {}",
                if *debug_enabled { "ON" } else { "OFF" }
            )))
        }
        Command::Quit => Ok(CommandOutput::Status("Goodbye!".to_string())),
        Command::Clear => Ok(CommandOutput::Status("".to_string())), // Handled in REPL directly
        // Pattern commands are now handled directly in REPL
        Command::PatternsList
        | Command::PatternsRemove(_)
        | Command::PatternsClear
        | Command::PatternsAdd => {
            Ok(CommandOutput::Status("Pattern management commands should be handled in REPL.".to_string()))
        }
        // Plan mode commands are handled directly in REPL
        Command::PlanModeToggle
        | Command::Plan(_)
        | Command::Approve
        | Command::Reject
        | Command::ShowPlan
        | Command::SavePlan
        | Command::Done => Ok(CommandOutput::Status("Plan mode commands should be handled in REPL.".to_string())),
        // Feedback commands are handled directly in REPL
        Command::FeedbackCritical(_) | Command::FeedbackMedium(_) | Command::FeedbackGood(_) => {
            Ok(CommandOutput::Status("Feedback commands should be handled in REPL.".to_string()))
        }
        // Local command is handled directly in REPL
        Command::Local { .. } => {
            Ok(CommandOutput::Status("Local command should be handled in REPL.".to_string()))
        }
        // Memory command is handled directly in REPL
        Command::Memory => {
            Ok(CommandOutput::Status("Memory command should be handled in REPL.".to_string()))
        }
    }
}

pub fn format_help() -> String {
    format!(
        "\x1b[1;36m╔═══════════════════════════════════════════════════════════════════════╗\x1b[0m\n\
         \x1b[1;36m║\x1b[0m                  \x1b[1;32mShammah Help - Commands & Shortcuts\x1b[0m                  \x1b[1;36m║\x1b[0m\n\
         \x1b[1;36m╚═══════════════════════════════════════════════════════════════════════╝\x1b[0m\n\n\
         \x1b[1;33m📋 Basic Commands:\x1b[0m\n\
         \x1b[36m  /help\x1b[0m              Show this help message\n\
         \x1b[36m  /quit\x1b[0m              Exit the REPL (also: Ctrl+D)\n\
         \x1b[36m  /clear\x1b[0m             Clear conversation history (start fresh)\n\
         \x1b[36m  /debug\x1b[0m             Toggle debug output\n\
         \x1b[36m  /metrics\x1b[0m           Display usage statistics\n\
         \x1b[36m  /memory\x1b[0m            Show memory usage (system and process)\n\
         \x1b[36m  /training\x1b[0m          Show detailed training statistics\n\n\
         \x1b[1;33m🤖 Model Commands:\x1b[0m\n\
         \x1b[36m  /local <query>\x1b[0m     Query local model directly (bypass routing)\n\
         \x1b[0m                     Example: /local What is 2+2?\n\n\
         \x1b[1;33m🔒 Tool Confirmation Patterns:\x1b[0m\n\
         \x1b[36m  /patterns\x1b[0m          List all saved confirmation patterns\n\
         \x1b[36m  /patterns add\x1b[0m      Add a new pattern (interactive wizard)\n\
         \x1b[36m  /patterns rm <id>\x1b[0m  Remove a specific pattern by ID\n\
         \x1b[36m  /patterns clear\x1b[0m    Remove all patterns (requires confirmation)\n\
         \x1b[0m\n\
         \x1b[90m  What are patterns?\x1b[0m Saved rules for auto-approving tool executions.\n\
         \x1b[90m  Example:\x1b[0m \"Always allow reading *.rs files\" or \"Allow git status\"\n\n\
         \x1b[1;33m📝 Plan Mode:\x1b[0m\n\
         \x1b[90m  Claude can enter plan mode to explore your codebase in read-only mode.\x1b[0m\n\
         \x1b[90m  It will ask clarifying questions, then present a plan for your approval.\x1b[0m\n\
         \x1b[90m  If you approve, context is cleared and Claude executes the plan.\x1b[0m\n\n\
         \x1b[1;33m🎓 Weighted Feedback (LoRA Fine-Tuning):\x1b[0m\n\
         \x1b[36m  /critical [note]\x1b[0m   Mark response as \x1b[31mcritical error\x1b[0m (10x training weight)\n\
         \x1b[36m  /medium [note]\x1b[0m     Mark response \x1b[33mneeds improvement\x1b[0m (3x weight)\n\
         \x1b[36m  /good [note]\x1b[0m       Mark response as \x1b[32mgood example\x1b[0m (1x weight)\n\
         \x1b[0m\n\
         \x1b[90m  Aliases:\x1b[0m /feedback critical|high|medium|good [note]\n\
         \x1b[0m\n\
         \x1b[90m  Examples:\x1b[0m\n\
         \x1b[90m    /critical\x1b[0m Never use .unwrap() in production code\n\
         \x1b[90m    /medium\x1b[0m Prefer iterator chains over manual loops\n\
         \x1b[90m    /good\x1b[0m This is exactly the right approach\n\n\
         \x1b[1;33m⌨️  Keyboard Shortcuts:\x1b[0m\n\
         \x1b[36m  Ctrl+C\x1b[0m             Cancel current query (interrupts generation)\n\
         \x1b[36m  Ctrl+D\x1b[0m             Exit REPL (same as /quit)\n\
         \x1b[36m  Ctrl+G\x1b[0m             Mark last response as \x1b[32mgood\x1b[0m (1x training weight)\n\
         \x1b[36m  Ctrl+B\x1b[0m             Mark last response as \x1b[31mbad\x1b[0m (10x training weight)\n\
         \x1b[36m  Shift+Tab\x1b[0m          Toggle plan mode on/off\n\
         \x1b[36m  Shift+Enter\x1b[0m        Multi-line input (insert newline)\n\
         \x1b[36m  Shift+PgUp\x1b[0m         Scroll up in history\n\
         \x1b[36m  Shift+PgDown\x1b[0m       Scroll down in history\n\
         \x1b[90m  ↑ / ↓ arrows\x1b[0m       Navigate command history\n\n\
         \x1b[1;33m🛠️  Tool Execution:\x1b[0m\n\
         When Claude needs to use tools (read files, run commands, etc.), you'll\n\
         be asked to approve each action. You can:\n\
         \x1b[32m  • Approve once\x1b[0m              Execute this time only\n\
         \x1b[32m  • Approve for session\x1b[0m      Allow during this session\n\
         \x1b[32m  • Remember pattern\x1b[0m         Always allow (saves to /patterns)\n\
         \x1b[31m  • Deny\x1b[0m                     Reject the action\n\n\
         Available tools: Read, Glob, Grep, WebFetch, Bash, Restart\n\n\
         \x1b[1;33m📚 Learn More:\x1b[0m\n\
         \x1b[36m  GitHub:\x1b[0m   https://github.com/schancel/shammah\n\
         \x1b[36m  Issues:\x1b[0m   https://github.com/schancel/shammah/issues\n\
         \x1b[36m  Docs:\x1b[0m     See README.md and docs/ folder\n\n\
         \x1b[1;33m💡 Quick Start:\x1b[0m\n\
         Just type your question! Examples:\n\
         \x1b[90m  • How do I implement a binary search in Rust?\x1b[0m\n\
         \x1b[90m  • Can you read my Cargo.toml and explain the dependencies?\x1b[0m\n\
         \x1b[90m  • Find all TODO comments in my code\x1b[0m\n\n\
         \x1b[1;36m─────────────────────────────────────────────────────────────────────────\x1b[0m\n\
         \x1b[90mTip: Use Ctrl+C to cancel long-running queries\x1b[0m"
    )
}

fn format_metrics(metrics_logger: &MetricsLogger) -> Result<String> {
    let summary = metrics_logger.get_today_summary()?;

    let local_pct = if summary.total > 0 {
        (summary.local_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let forward_pct = if summary.total > 0 {
        (summary.forward_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let crisis_pct = if summary.total > 0 {
        (summary.crisis_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    let no_match_pct = if summary.total > 0 {
        (summary.no_match_count as f64 / summary.total as f64) * 100.0
    } else {
        0.0
    };

    Ok(format!(
        "Metrics (last 24 hours):\n\
        Total requests: {}\n\
        Local: {} ({:.1}%)\n\
        Forwarded: {} ({:.1}%)\n\
          - Crisis: {} ({:.1}%)\n\
          - No match: {} ({:.1}%)\n\
        Avg response time (local): {}ms\n\
        Avg response time (forwarded): {}ms\n",
        summary.total,
        summary.local_count,
        local_pct,
        summary.forward_count,
        forward_pct,
        summary.crisis_count,
        crisis_pct,
        summary.no_match_count,
        no_match_pct,
        summary.avg_local_time,
        summary.avg_forward_time
    ))
}

fn format_training(
    router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter
    validator: Option<&ThresholdValidator>,
) -> Result<String> {
    let mut output = String::new();
    output.push_str("Training Statistics\n");
    output.push_str("===================\n\n");

    if let Some(router) = router {
        let router_stats = router.stats();

        // Overall stats
        output.push_str(&format!("Total Queries: {}\n", router_stats.total_queries));
        output.push_str(&format!(
            "Local Attempts: {}\n",
            router_stats.total_local_attempts
        ));
        output.push_str(&format!(
            "Success Rate: {:.1}%\n",
            router_stats.success_rate * 100.0
        ));
        output.push_str(&format!(
            "Forward Rate: {:.1}%\n",
            router_stats.forward_rate * 100.0
        ));
        output.push_str(&format!(
            "Confidence Threshold: {:.2}\n\n",
            router_stats.confidence_threshold
        ));

        // Per-category breakdown
        output.push_str("Performance by Category:\n");
        let mut categories: Vec<_> = router_stats.categories.iter().collect();
        categories.sort_by_key(|(_, stats)| std::cmp::Reverse(stats.local_attempts));

        for (category, stats) in categories {
            if stats.local_attempts > 0 {
                let success_rate = stats.successes as f64 / stats.local_attempts as f64 * 100.0;
                output.push_str(&format!(
                    "  {:?}: {} attempts, {:.1}% success\n",
                    category, stats.local_attempts, success_rate
                ));
            }
        }
    } else {
        output.push_str("No router statistics available\n");
    }

    if let Some(validator) = validator {
        let validator_stats = validator.stats();

        output.push_str("\nQuality Validation:\n");
        output.push_str(&format!(
            "Total Validations: {}\n",
            validator_stats.total_validations
        ));
        output.push_str(&format!("Approved: {}\n", validator_stats.approved));
        output.push_str(&format!("Rejected: {}\n", validator_stats.rejected));
        output.push_str(&format!(
            "Approval Rate: {:.1}%\n\n",
            validator_stats.approval_rate * 100.0
        ));

        output.push_str("Quality Signals:\n");
        let mut signals: Vec<_> = validator_stats.signal_stats.iter().collect();
        signals.sort_by_key(|(_, stats)| {
            std::cmp::Reverse(stats.present_and_good + stats.present_and_bad)
        });

        for (signal, stats) in signals {
            let total = stats.present_and_good + stats.present_and_bad;
            if total >= 5 {
                // Only show signals with enough data
                let precision = if total > 0 {
                    stats.present_and_good as f64 / total as f64 * 100.0
                } else {
                    0.0
                };
                output.push_str(&format!(
                    "  {:?}: {:.1}% precision ({} samples)\n",
                    signal, precision, total
                ));
            }
        }
    } else {
        output.push_str("\nNo validator statistics available\n");
    }

    Ok(output)
}

// Pattern management command handlers are now in Repl (Phase 3 implementation)
// The command handlers above return a placeholder message since the actual
// handling is done directly in the REPL loop to avoid borrowing issues

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_patterns_list() {
        assert!(matches!(
            Command::parse("/patterns"),
            Some(Command::PatternsList)
        ));
        assert!(matches!(
            Command::parse("/patterns list"),
            Some(Command::PatternsList)
        ));
    }

    #[test]
    fn test_parse_patterns_clear() {
        assert!(matches!(
            Command::parse("/patterns clear"),
            Some(Command::PatternsClear)
        ));
    }

    #[test]
    fn test_parse_patterns_add() {
        assert!(matches!(
            Command::parse("/patterns add"),
            Some(Command::PatternsAdd)
        ));
    }

    #[test]
    fn test_parse_patterns_remove() {
        // Test "remove" alias
        match Command::parse("/patterns remove abc123") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "abc123"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test "rm" alias
        match Command::parse("/patterns rm xyz789") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "xyz789"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test with extra whitespace
        match Command::parse("/patterns remove   abc123  ") {
            Some(Command::PatternsRemove(id)) => assert_eq!(id, "abc123"),
            _ => panic!("Expected PatternsRemove command"),
        }

        // Test empty ID returns None
        assert!(matches!(Command::parse("/patterns remove "), None));
        assert!(matches!(Command::parse("/patterns rm "), None));
    }

    #[test]
    fn test_parse_existing_commands() {
        // Ensure existing commands still work
        assert!(matches!(Command::parse("/help"), Some(Command::Help)));
        assert!(matches!(Command::parse("/quit"), Some(Command::Quit)));
        assert!(matches!(Command::parse("/metrics"), Some(Command::Metrics)));
        assert!(matches!(Command::parse("/debug"), Some(Command::Debug)));
        assert!(matches!(
            Command::parse("/training"),
            Some(Command::Training)
        ));
        assert!(matches!(Command::parse("/clear"), Some(Command::Clear)));
    }

    #[test]
    fn test_parse_invalid_patterns_command() {
        // Invalid subcommands should return None
        assert!(matches!(Command::parse("/patterns invalid"), None));
        assert!(matches!(Command::parse("/patterns remove"), None)); // Missing ID
        assert!(matches!(Command::parse("/patterns rm"), None)); // Missing ID
    }
}
