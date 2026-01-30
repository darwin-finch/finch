// Slash command handling

use anyhow::Result;

use crate::metrics::MetricsLogger;
use crate::patterns::PatternLibrary;

pub enum Command {
    Help,
    Quit,
    Metrics,
    Patterns,
    Debug,
}

impl Command {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim() {
            "/help" => Some(Command::Help),
            "/quit" | "/exit" => Some(Command::Quit),
            "/metrics" => Some(Command::Metrics),
            "/patterns" => Some(Command::Patterns),
            "/debug" => Some(Command::Debug),
            _ => None,
        }
    }
}

pub fn handle_command(
    command: Command,
    metrics_logger: &MetricsLogger,
    pattern_library: &PatternLibrary,
) -> Result<String> {
    match command {
        Command::Help => Ok(format_help()),
        Command::Quit => Ok("Goodbye!".to_string()),
        Command::Metrics => format_metrics(metrics_logger),
        Command::Patterns => Ok(format_patterns(pattern_library)),
        Command::Debug => Ok("Debug mode toggled".to_string()),
    }
}

fn format_help() -> String {
    r#"Available commands:
  /help      - Show this help message
  /quit      - Exit the REPL
  /metrics   - Display statistics
  /patterns  - List all patterns
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

    let mut output = format!(
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
    );

    if !summary.top_patterns.is_empty() {
        output.push_str("\nTop patterns:\n");
        for (i, (pattern_id, count)) in summary.top_patterns.iter().enumerate() {
            output.push_str(&format!(
                "  {}. {} ({} matches)\n",
                i + 1,
                pattern_id,
                count
            ));
        }
    }

    Ok(output)
}

fn format_patterns(pattern_library: &PatternLibrary) -> String {
    let mut output = String::from("Constitutional Patterns:\n");

    for (i, pattern) in pattern_library.patterns.iter().enumerate() {
        output.push_str(&format!("  {}. {} ({})\n", i + 1, pattern.name, pattern.id));
    }

    output
}
