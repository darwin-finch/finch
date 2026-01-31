// Slash command handling

use anyhow::Result;

use crate::metrics::MetricsLogger;
use crate::models::ThresholdValidator;
use crate::router::Router;

pub enum Command {
    Help,
    Quit,
    Metrics,
    Debug,
    Training,
    Clear,
    PatternsList,
    PatternsRemove(String),
    PatternsClear,
    PatternsAdd,
    // Plan mode commands
    Plan(String),
    Approve,
    Reject,
    ShowPlan,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        let trimmed = input.trim();

        // Handle simple commands without arguments
        match trimmed {
            "/help" => return Some(Command::Help),
            "/quit" | "/exit" => return Some(Command::Quit),
            "/metrics" => return Some(Command::Metrics),
            "/debug" => return Some(Command::Debug),
            "/training" => return Some(Command::Training),
            "/clear" | "/reset" => return Some(Command::Clear),
            "/approve" | "/execute" => return Some(Command::Approve),
            "/reject" | "/cancel" => return Some(Command::Reject),
            "/show-plan" => return Some(Command::ShowPlan),
            _ => {}
        }

        // Handle /plan command with task description
        if let Some(rest) = trimmed.strip_prefix("/plan ") {
            let task = rest.trim();
            if !task.is_empty() {
                return Some(Command::Plan(task.to_string()));
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
) -> Result<String> {
    match command {
        Command::Help => Ok(format_help()),
        Command::Quit => Ok("Goodbye!".to_string()),
        Command::Metrics => format_metrics(metrics_logger),
        Command::Debug => {
            *debug_enabled = !*debug_enabled;
            Ok(format!(
                "Debug mode: {}",
                if *debug_enabled { "ON" } else { "OFF" }
            ))
        }
        Command::Training => format_training(router, validator),
        Command::Clear => Ok("".to_string()), // Handled in REPL directly
        // Pattern commands are now handled directly in REPL
        Command::PatternsList
        | Command::PatternsRemove(_)
        | Command::PatternsClear
        | Command::PatternsAdd => {
            Ok("Pattern management commands should be handled in REPL.".to_string())
        }
        // Plan mode commands are handled directly in REPL
        Command::Plan(_) | Command::Approve | Command::Reject | Command::ShowPlan => {
            Ok("Plan mode commands should be handled in REPL.".to_string())
        }
    }
}

fn format_help() -> String {
    r#"Available commands:
  /help             - Show this help message
  /quit             - Exit the REPL
  /metrics          - Display statistics
  /training         - Show detailed training statistics
  /clear            - Clear conversation history (start fresh)
  /debug            - Toggle debug output
  /patterns         - List confirmation patterns
  /patterns add     - Add a new confirmation pattern (interactive)
  /patterns rm <id> - Remove a pattern by ID
  /patterns clear   - Remove all patterns

Plan Mode Commands:
  /plan <task>      - Enter planning mode for a task
  /show-plan        - Display the current plan
  /approve          - Approve plan (prompts to clear context) and start execution
  /reject           - Reject the plan and return to normal mode

Type any question to get started!"#
        .to_string()
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
