// Interactive REPL

use anyhow::Result;
use std::io::{self, Write};
use std::time::Instant;

use crate::claude::{ClaudeClient, MessageRequest};
use crate::config::Config;
use crate::metrics::{MetricsLogger, RequestMetric};
use crate::patterns::PatternLibrary;
use crate::router::{RouteDecision, Router};

use super::commands::{handle_command, Command};

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    router: Router,
    metrics_logger: MetricsLogger,
    pattern_library: PatternLibrary,
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
        }
    }

    pub async fn run(&self) -> Result<()> {
        println!("Shammah v0.1.0 - Constitutional AI Proxy (Phase 1 MVP)");
        println!("Using API key from: ~/.claude/settings.json ✓");
        println!(
            "Loaded {} constitutional patterns ✓",
            self.pattern_library.patterns.len()
        );
        println!("Loaded crisis detection keywords ✓");
        println!("Ready. Type /help for commands.\n");

        loop {
            print!("You: ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }

            // Check for slash commands
            if let Some(command) = Command::parse(input) {
                match command {
                    Command::Quit => {
                        println!("\nGoodbye!");
                        break;
                    }
                    _ => {
                        let output =
                            handle_command(command, &self.metrics_logger, &self.pattern_library)?;
                        println!("\n{}\n", output);
                        continue;
                    }
                }
            }

            // Process query
            match self.process_query(input).await {
                Ok(response) => {
                    println!("\n{}\n", response);
                }
                Err(e) => {
                    eprintln!("\nError: {}\n", e);
                }
            }
        }

        Ok(())
    }

    async fn process_query(&self, query: &str) -> Result<String> {
        let start_time = Instant::now();

        println!("\n[Analyzing...]");

        // Make routing decision
        let decision = self.router.route(query);

        let (response, routing_decision, pattern_id, confidence, forward_reason) = match decision {
            RouteDecision::Local {
                pattern,
                confidence,
            } => {
                println!("├─ Crisis check: PASS");
                println!("├─ Pattern match: {} ({:.2})", pattern.id, confidence);
                println!("└─ Routing: LOCAL ({}ms)", start_time.elapsed().as_millis());

                (
                    pattern.template_response.clone(),
                    "local".to_string(),
                    Some(pattern.id.clone()),
                    Some(confidence),
                    None,
                )
            }
            RouteDecision::Forward { reason } => {
                match reason {
                    crate::router::ForwardReason::Crisis => {
                        println!("├─ ⚠️  CRISIS DETECTED");
                        println!("└─ Routing: FORWARDING TO CLAUDE");
                    }
                    _ => {
                        println!("├─ Crisis check: PASS");
                        println!("├─ Pattern match: NONE");
                        println!("└─ Routing: FORWARDING TO CLAUDE");
                    }
                }

                let request = MessageRequest::new(query);
                let response = self.claude_client.send_message(&request).await?;
                let elapsed = start_time.elapsed().as_millis();

                println!("   ({}ms)", elapsed);

                (
                    response.text(),
                    "forward".to_string(),
                    None,
                    None,
                    Some(reason.as_str().to_string()),
                )
            }
        };

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
