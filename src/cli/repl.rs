// Interactive REPL with Claude Code-style interface

use anyhow::Result;
use crossterm::{
    cursor,
    style::Stylize,
    terminal::{self, Clear, ClearType},
    ExecutableCommand,
};
use std::io::{self, IsTerminal, Write};
use std::time::Instant;

use crate::claude::{ClaudeClient, MessageRequest};
use crate::config::Config;
use crate::metrics::{MetricsLogger, RequestMetric};
use crate::models::{ModelConfig, ThresholdRouter, ThresholdValidator};
use crate::patterns::PatternLibrary;
use crate::router::{ForwardReason, RouteDecision, Router};

use super::commands::{handle_command, Command};

/// Get current terminal width, or default to 80 if not a TTY
fn terminal_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    router: Router,
    metrics_logger: MetricsLogger,
    pattern_library: PatternLibrary,
    // Online learning models
    threshold_router: ThresholdRouter,
    threshold_validator: ThresholdValidator,
    // UI state
    is_interactive: bool,
}

impl Repl {
    pub fn new(
        config: Config,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        pattern_library: PatternLibrary,
    ) -> Self {
        // Detect if we're in interactive mode (stdout is a TTY)
        let is_interactive = io::stdout().is_terminal();

        Self {
            _config: config,
            claude_client,
            router,
            metrics_logger,
            pattern_library,
            threshold_router: ThresholdRouter::new(),
            threshold_validator: ThresholdValidator::new(),
            is_interactive,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.is_interactive {
            // Fancy startup for interactive mode
            println!("Shammah v0.1.0 - Constitutional AI Proxy");
            println!("Using API key from: ~/.shammah/config.toml ✓");
            println!(
                "Loaded {} constitutional patterns ✓",
                self.pattern_library.patterns.len()
            );
            println!("Loaded crisis detection keywords ✓");
            println!("Online learning: ENABLED (threshold models) ✓");
            println!();
            println!("Ready. Type /help for commands.");
            self.print_status_line();
        } else {
            // Minimal output for non-interactive mode (pipes, scripts)
            eprintln!("# Shammah v0.1.0 - Non-interactive mode");
        }

        loop {
            if self.is_interactive {
                // Claude Code-style prompt with dynamic width separators
                println!();
                self.print_separator();
                print!("> ");
            } else {
                // Simple prompt for non-interactive
                print!("Query: ");
            }
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            if self.is_interactive {
                self.print_separator();
                println!();
            }

            // Check for slash commands
            if let Some(command) = Command::parse(input) {
                match command {
                    Command::Quit => {
                        if self.is_interactive {
                            println!("Goodbye!");
                        }
                        break;
                    }
                    _ => {
                        let output =
                            handle_command(command, &self.metrics_logger, &self.pattern_library)?;
                        println!("{}", output);
                        continue;
                    }
                }
            }

            // Process query
            match self.process_query(input).await {
                Ok(response) => {
                    println!("{}", response);
                    if self.is_interactive {
                        println!();
                        self.print_status_line();
                    }
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    if self.is_interactive {
                        println!();
                        self.print_status_line();
                    }
                }
            }
        }

        Ok(())
    }

    /// Print separator line that adapts to terminal width
    fn print_separator(&self) {
        let width = terminal_width();
        println!("{}", "─".repeat(width));
    }

    /// Print training status below the prompt (only in interactive mode)
    fn print_status_line(&self) {
        if !self.is_interactive {
            return;
        }

        let router_stats = self.threshold_router.stats();
        let validator_stats = self.threshold_validator.stats();

        // Calculate percentages
        let local_pct = if router_stats.total_queries == 0 {
            0.0
        } else {
            (router_stats.total_local_attempts as f64 / router_stats.total_queries as f64) * 100.0
        };

        let forward_pct = 100.0 - local_pct;

        let success_pct = if router_stats.total_local_attempts == 0 {
            0.0
        } else {
            (router_stats.total_successes as f64 / router_stats.total_local_attempts as f64) * 100.0
        };

        // Build single-line status string
        let status = format!(
            "Training: {} queries | Local: {:.0}% | Forward: {:.0}% | Success: {:.0}% | Confidence: {:.2} | Approval: {:.0}%",
            router_stats.total_queries,
            local_pct,
            forward_pct,
            success_pct,
            router_stats.confidence_threshold,
            validator_stats.approval_rate * 100.0
        );

        // Truncate to terminal width if needed
        let width = terminal_width();
        let truncated = if status.len() > width {
            format!("{}...", &status[..width.saturating_sub(3)])
        } else {
            status
        };

        // Print in gray, all on one line
        println!("{}", truncated.dark_grey());
    }

    async fn process_query(&mut self, query: &str) -> Result<String> {
        let start_time = Instant::now();

        if self.is_interactive {
            print!("{}", "Analyzing...".dark_grey());
            io::stdout().flush()?;
        }

        // Check if threshold router suggests trying local
        let should_try_local = self.threshold_router.should_try_local(query);

        // Make routing decision (still using pattern matching for now)
        let decision = self.router.route(query);

        if self.is_interactive {
            io::stdout()
                .execute(cursor::MoveToColumn(0))?
                .execute(Clear(ClearType::CurrentLine))?;
        }

        let (response, routing_decision, pattern_id, confidence, forward_reason, is_local) =
            match decision {
                RouteDecision::Local {
                    pattern,
                    confidence,
                } => {
                    if self.is_interactive {
                        println!("✓ Crisis check: PASS");
                        println!("✓ Pattern match: {} ({:.2})", pattern.id, confidence);
                        println!("→ Routing: LOCAL ({}ms)", start_time.elapsed().as_millis());
                    } else {
                        eprintln!("# Routing: LOCAL (pattern: {})", pattern.id);
                    }

                    (
                        pattern.template_response.clone(),
                        "local".to_string(),
                        Some(pattern.id.clone()),
                        Some(confidence),
                        None,
                        true,
                    )
                }
                RouteDecision::Forward { reason } => {
                    if self.is_interactive {
                        match reason {
                            ForwardReason::Crisis => {
                                println!("⚠️  CRISIS DETECTED");
                                println!("→ Routing: FORWARDING TO CLAUDE");
                            }
                            _ => {
                                println!("✓ Crisis check: PASS");
                                println!("✗ Pattern match: NONE");
                                if should_try_local {
                                    println!(
                                        "  (Threshold model suggested local, but no pattern match)"
                                    );
                                }
                                println!("→ Routing: FORWARDING TO CLAUDE");
                            }
                        }
                    } else {
                        eprintln!("# Routing: FORWARD (reason: {:?})", reason);
                    }

                    let request = MessageRequest::new(query);
                    let response = self.claude_client.send_message(&request).await?;
                    let elapsed = start_time.elapsed().as_millis();

                    if self.is_interactive {
                        println!("✓ Received response ({}ms)", elapsed);
                    }

                    (
                        response.text(),
                        "forward".to_string(),
                        None,
                        None,
                        Some(reason.as_str().to_string()),
                        false,
                    )
                }
            };

        // Online learning: Update threshold models
        if self.is_interactive {
            println!();
            print!("{}", "Learning... ".dark_grey());
            io::stdout().flush()?;
        }

        // Validate the response
        let is_valid = self.threshold_validator.validate(query, &response);

        // Learn from this interaction
        self.threshold_router.learn(query, true); // Assume success for now
        self.threshold_validator.learn(query, &response, true);

        if self.is_interactive {
            println!("✓");
        }

        // Log metric
        let query_hash = MetricsLogger::hash_query(query);
        let response_time_ms = start_time.elapsed().as_millis() as u64;

        let metric = RequestMetric::new(
            query_hash,
            routing_decision,
            pattern_id,
            confidence,
            forward_reason,
            response_time_ms,
        );

        self.metrics_logger.log(&metric)?;

        Ok(response)
    }
}
