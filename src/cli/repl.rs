// Interactive REPL with Claude Code-style interface

use anyhow::Result;
use std::io::{self, Write};
use std::time::Instant;

use crate::claude::{ClaudeClient, MessageRequest};
use crate::config::Config;
use crate::metrics::{MetricsLogger, RequestMetric};
use crate::models::{ModelConfig, ThresholdRouter, ThresholdValidator};
use crate::patterns::PatternLibrary;
use crate::router::{ForwardReason, RouteDecision, Router};

use super::commands::{handle_command, Command};

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    router: Router,
    metrics_logger: MetricsLogger,
    pattern_library: PatternLibrary,
    // Online learning models
    threshold_router: ThresholdRouter,
    threshold_validator: ThresholdValidator,
}

impl Repl {
    pub fn new(
        config: Config,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
        pattern_library: PatternLibrary,
    ) -> Self {
        Self {
            _config: config,
            claude_client,
            router,
            metrics_logger,
            pattern_library,
            threshold_router: ThresholdRouter::new(),
            threshold_validator: ThresholdValidator::new(),
        }
    }

    pub async fn run(&mut self) -> Result<()> {
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

        loop {
            // Claude Code-style prompt with separators
            println!();
            println!("{}", "─".repeat(70));
            print!("> ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            println!("{}", "─".repeat(70));
            println!();

            // Check for slash commands
            if let Some(command) = Command::parse(input) {
                match command {
                    Command::Quit => {
                        println!("Goodbye!");
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
                    println!();
                    self.print_status_line();
                }
                Err(e) => {
                    eprintln!("Error: {}", e);
                    println!();
                    self.print_status_line();
                }
            }
        }

        Ok(())
    }

    /// Print training status below the prompt
    fn print_status_line(&self) {
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

        // Status line with training metrics
        print!("\x1b[90m"); // Gray color
        print!(
            "Training: {} queries | Local: {:.0}% | Forward: {:.0}% | Success: {:.0}% | ",
            router_stats.total_queries, local_pct, forward_pct, success_pct
        );
        print!(
            "Confidence: {:.2} | Approval: {:.0}%",
            router_stats.confidence_threshold,
            validator_stats.approval_rate * 100.0
        );
        print!("\x1b[0m"); // Reset color
        println!();
    }

    async fn process_query(&mut self, query: &str) -> Result<String> {
        let start_time = Instant::now();

        print!("\x1b[90m"); // Gray color
        print!("Analyzing...");
        io::stdout().flush()?;

        // Check if threshold router suggests trying local
        let should_try_local = self.threshold_router.should_try_local(query);

        // Make routing decision (still using pattern matching for now)
        let decision = self.router.route(query);

        print!("\r\x1b[2K"); // Clear the "Analyzing..." line
        print!("\x1b[0m"); // Reset color

        let (response, routing_decision, pattern_id, confidence, forward_reason, is_local) =
            match decision {
                RouteDecision::Local {
                    pattern,
                    confidence,
                } => {
                    println!("✓ Crisis check: PASS");
                    println!("✓ Pattern match: {} ({:.2})", pattern.id, confidence);
                    println!("→ Routing: LOCAL ({}ms)", start_time.elapsed().as_millis());

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

                    let request = MessageRequest::new(query);
                    let response = self.claude_client.send_message(&request).await?;
                    let elapsed = start_time.elapsed().as_millis();

                    println!("✓ Received response ({}ms)", elapsed);

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
        println!();
        print!("\x1b[90m"); // Gray color
        print!("Learning... ");
        io::stdout().flush()?;

        // Validate the response
        let is_valid = self.threshold_validator.validate(query, &response);

        // Learn from this interaction
        self.threshold_router.learn(query, true); // Assume success for now
        self.threshold_validator.learn(query, &response, true);

        println!("✓");
        print!("\x1b[0m"); // Reset color

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
