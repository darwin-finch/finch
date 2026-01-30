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
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "/help" => Some(Command::Help),
            "/quit" | "/exit" => Some(Command::Quit),
            "/metrics" => Some(Command::Metrics),
            "/debug" => Some(Command::Debug),
            "/training" => Some(Command::Training),
            "/clear" | "/reset" => Some(Command::Clear),
            _ => None,
        }
    }
}

pub fn handle_command(
    command: Command,
    metrics_logger: &MetricsLogger,
    router: Option<&Router>, // CHANGED: Router instead of ThresholdRouter
    validator: Option<&ThresholdValidator>,
) -> Result<String> {
    match command {
        Command::Help => Ok(format_help()),
        Command::Quit => Ok("Goodbye!".to_string()),
        Command::Metrics => format_metrics(metrics_logger),
        Command::Debug => Ok("Debug mode toggled".to_string()),
        Command::Training => format_training(router, validator),
        Command::Clear => Ok("".to_string()), // Handled in REPL directly
    }
}

fn format_help() -> String {
    r#"Available commands:
  /help      - Show this help message
  /quit      - Exit the REPL
  /metrics   - Display statistics
  /training  - Show detailed training statistics
  /clear     - Clear conversation history (start fresh)
  /debug     - Toggle debug output

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
