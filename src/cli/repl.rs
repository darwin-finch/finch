// Interactive REPL with Claude Code-style interface

use anyhow::Result;
use crossterm::{
    cursor,
    style::Stylize,
    terminal::{self, Clear, ClearType},
    ExecutableCommand,
};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

use crate::claude::{ClaudeClient, MessageRequest};
use crate::config::Config;
use crate::metrics::{MetricsLogger, RequestMetric, ResponseComparison, TrainingTrends};
use crate::models::{ThresholdRouter, ThresholdValidator};
use crate::router::{ForwardReason, RouteDecision, Router};
use crate::tools::implementations::{BashTool, GlobTool, GrepTool, ReadTool, RestartTool, WebFetchTool};
use crate::tools::types::{ToolDefinition, ToolInputSchema};
use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};

use super::commands::{handle_command, Command};
use super::conversation::ConversationHistory;
use super::input::InputHandler;

/// Get current terminal width, or default to 80 if not a TTY
fn terminal_width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

/// Create tool definitions for Claude API
///
/// These are placeholder definitions until actual tool implementations are ready.
/// Claude will know these tools exist and can invoke them, but execution is not yet implemented.
fn create_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        // Bash command execution
        ToolDefinition {
            name: "bash".to_string(),
            description: "Execute bash commands. Use this for terminal operations like git, npm, docker, file operations, etc.".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("command", "The bash command to execute"),
                ("description", "A brief description of what this command does"),
            ]),
        },
        // Read file contents
        ToolDefinition {
            name: "read".to_string(),
            description: "Read the contents of a file from the filesystem.".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("file_path", "Absolute path to the file to read"),
            ]),
        },
        // Web fetch
        ToolDefinition {
            name: "web_fetch".to_string(),
            description: "Fetch content from a URL. Use this to retrieve information from websites or APIs.".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("url", "The URL to fetch"),
                ("prompt", "What information to extract from the fetched content"),
            ]),
        },
        // Grep/search
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search for patterns in files using ripgrep. Returns matching lines.".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("pattern", "The regex pattern to search for"),
                ("path", "Directory or file to search in (optional, defaults to current directory)"),
            ]),
        },
        // Glob/find files
        ToolDefinition {
            name: "glob".to_string(),
            description: "Find files matching a glob pattern (e.g., \"**/*.rs\", \"src/**/*.ts\").".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("pattern", "The glob pattern to match files against"),
            ]),
        },
    ]
}

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    router: Router,
    metrics_logger: MetricsLogger,
    // Online learning models
    threshold_router: ThresholdRouter,
    threshold_validator: ThresholdValidator,
    // Training metrics
    training_trends: TrainingTrends,
    // Model persistence
    models_dir: Option<PathBuf>,
    // Tool execution
    tool_executor: ToolExecutor,
    // UI state
    is_interactive: bool,
    streaming_enabled: bool,
    // Readline input handler
    input_handler: Option<InputHandler>,
    // Conversation history
    conversation: ConversationHistory,
}

impl Repl {
    pub fn new(
        config: Config,
        claude_client: ClaudeClient,
        router: Router,
        metrics_logger: MetricsLogger,
    ) -> Self {
        // Detect if we're in interactive mode (stdout is a TTY)
        let is_interactive = io::stdout().is_terminal();

        // Set up models directory
        let models_dir = dirs::home_dir().map(|home| home.join(".shammah").join("models"));

        // Try to load existing models, fall back to new
        let (threshold_router, threshold_validator) =
            Self::load_or_create_models(models_dir.as_ref(), is_interactive);

        // Initialize input handler for interactive mode
        let input_handler = if is_interactive {
            match InputHandler::new() {
                Ok(handler) => Some(handler),
                Err(e) => {
                    eprintln!("Warning: Failed to initialize readline: {}", e);
                    eprintln!("Falling back to basic input mode");
                    None
                }
            }
        } else {
            None
        };

        // Initialize tool execution system
        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Box::new(ReadTool));
        tool_registry.register(Box::new(GlobTool));
        tool_registry.register(Box::new(GrepTool));
        tool_registry.register(Box::new(WebFetchTool::new()));
        tool_registry.register(Box::new(BashTool));
        tool_registry.register(Box::new(RestartTool::new()));

        // Create permission manager (allow all for now)
        let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);

        // Create tool executor
        let tool_executor = ToolExecutor::new(tool_registry, permissions);

        if is_interactive {
            eprintln!(
                "✓ Tool execution enabled ({} tools)",
                tool_executor.registry().len()
            );
        }

        let streaming_enabled = config.streaming_enabled;

        Self {
            _config: config,
            claude_client,
            router,
            metrics_logger,
            threshold_router,
            threshold_validator,
            training_trends: TrainingTrends::new(20), // Track last 20 queries
            models_dir,
            tool_executor,
            is_interactive,
            streaming_enabled,
            input_handler,
            conversation: ConversationHistory::new(),
        }
    }

    /// Load models from disk or create new ones
    fn load_or_create_models(
        models_dir: Option<&PathBuf>,
        is_interactive: bool,
    ) -> (ThresholdRouter, ThresholdValidator) {
        let Some(models_dir) = models_dir else {
            return (ThresholdRouter::new(), ThresholdValidator::new());
        };

        let router_path = models_dir.join("threshold_router.json");
        let validator_path = models_dir.join("threshold_validator.json");

        let router = if router_path.exists() {
            match ThresholdRouter::load(&router_path) {
                Ok(router) => {
                    if is_interactive {
                        eprintln!(
                            "✓ Loaded router with {} training queries",
                            router.stats().total_queries
                        );
                    }
                    router
                }
                Err(e) => {
                    if is_interactive {
                        eprintln!("Warning: Failed to load router: {}", e);
                    }
                    ThresholdRouter::new()
                }
            }
        } else {
            ThresholdRouter::new()
        };

        let validator = if validator_path.exists() {
            match ThresholdValidator::load(&validator_path) {
                Ok(validator) => {
                    if is_interactive {
                        eprintln!(
                            "✓ Loaded validator with {} validations",
                            validator.stats().total_validations
                        );
                    }
                    validator
                }
                Err(e) => {
                    if is_interactive {
                        eprintln!("Warning: Failed to load validator: {}", e);
                    }
                    ThresholdValidator::new()
                }
            }
        } else {
            ThresholdValidator::new()
        };

        (router, validator)
    }

    /// Save models to disk
    fn save_models(&self) -> Result<()> {
        let Some(ref models_dir) = self.models_dir else {
            return Ok(());
        };

        std::fs::create_dir_all(models_dir)?;

        if self.is_interactive {
            print!("{}", "Saving models... ".dark_grey());
            io::stdout().flush()?;
        }

        self.threshold_router
            .save(models_dir.join("threshold_router.json"))?;
        self.threshold_validator
            .save(models_dir.join("threshold_validator.json"))?;

        if self.is_interactive {
            println!("✓");
        }

        Ok(())
    }

    /// Execute tools and re-invoke Claude until no more tool uses
    async fn execute_tool_loop(
        &mut self,
        initial_response: crate::claude::MessageResponse,
    ) -> Result<String> {

        let mut current_response = initial_response;
        let mut iteration = 0;
        const MAX_ITERATIONS: u32 = 5; // Prevent infinite loops

        while current_response.has_tool_uses() && iteration < MAX_ITERATIONS {
            iteration += 1;

            let tool_uses = current_response.tool_uses();

            if self.is_interactive {
                println!("🔧 Executing {} tool(s)...", tool_uses.len());
            }

            // Execute all tool uses
            let mut tool_results = Vec::new();
            for tool_use in tool_uses {
                if self.is_interactive {
                    println!("  → {}", tool_use.name);
                }

                let result = self.tool_executor.execute_tool(&tool_use).await?;
                tool_results.push(result);
            }

            // Build tool result message for Claude
            // Format tool results as a user message with structured content
            let mut tool_result_text = String::from("Tool execution results:\n\n");
            for result in &tool_results {
                tool_result_text.push_str(&format!(
                    "Tool: {} (ID: {})\n",
                    result.tool_use_id,
                    result.tool_use_id
                ));
                if result.is_error {
                    tool_result_text.push_str("Status: ERROR\n");
                } else {
                    tool_result_text.push_str("Status: SUCCESS\n");
                }
                tool_result_text.push_str(&format!("Result:\n{}\n\n", result.content));
            }

            // Important: Add Claude's tool-use response to conversation first
            // This maintains the user/assistant alternation required by the API
            self.conversation.add_assistant_message(current_response.text());

            // Then add tool results as a user message
            self.conversation.add_user_message(tool_result_text);

            // Re-invoke Claude with tool results
            let request = MessageRequest::with_context(self.conversation.get_messages())
                .with_tools(create_tool_definitions());

            current_response = self.claude_client.send_message(&request).await?;
        }

        if iteration >= MAX_ITERATIONS {
            if self.is_interactive {
                eprintln!(
                    "⚠️  Warning: Max tool iterations reached ({})",
                    MAX_ITERATIONS
                );
            }
        }

        Ok(current_response.text())
    }

    /// Display streaming response character-by-character
    async fn display_streaming_response(
        &mut self,
        mut rx: mpsc::Receiver<Result<String>>,
    ) -> Result<String> {
        let mut full_response = String::new();
        let mut stdout = io::stdout();

        // Print newline to start response area
        if self.is_interactive {
            println!();
        }

        while let Some(result) = rx.recv().await {
            match result {
                Ok(text_chunk) => {
                    // Print chunk immediately
                    print!("{}", text_chunk);
                    stdout.flush()?;

                    full_response.push_str(&text_chunk);
                }
                Err(e) => {
                    return Err(e);
                }
            }
        }

        // Final newline after response
        if self.is_interactive {
            println!();
        }

        Ok(full_response)
    }

    pub async fn run(&mut self) -> Result<()> {
        if self.is_interactive {
            // Fancy startup for interactive mode
            println!("Shammah v0.1.0 - Constitutional AI Proxy");
            println!("Using API key from: ~/.shammah/config.toml ✓");
            println!("Loaded crisis detection keywords ✓");
            println!("Online learning: ENABLED (threshold models) ✓");
            println!();
            println!("Ready. Type /help for commands.");
            self.print_status_line();
        } else {
            // Minimal output for non-interactive mode (pipes, scripts)
            eprintln!("# Shammah v0.1.0 - Non-interactive mode");
        }

        // Register Ctrl+C handler for graceful shutdown
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = shutdown_flag.clone();

        ctrlc::set_handler(move || {
            flag_clone.store(true, Ordering::SeqCst);
        })?;

        loop {
            // Check for shutdown
            if shutdown_flag.load(Ordering::SeqCst) {
                if self.is_interactive {
                    println!();
                }
                self.save_models()?;
                if self.is_interactive {
                    println!("Models saved. Goodbye!");
                }
                break;
            }

            // Read input using readline or fallback
            let input = if self.input_handler.is_some() {
                // Interactive mode with readline support
                println!();
                self.print_separator();

                let line = {
                    let handler = self.input_handler.as_mut().unwrap();
                    handler.read_line("> ")?
                };

                match line {
                    Some(text) => text,
                    None => {
                        // Ctrl+C or Ctrl+D - graceful exit
                        println!();
                        self.save_models()?;
                        if let Some(ref mut handler) = self.input_handler {
                            if let Err(e) = handler.save_history() {
                                eprintln!("Warning: Failed to save history: {}", e);
                            }
                        }
                        println!("Models saved. Goodbye!");
                        break;
                    }
                }
            } else {
                // Fallback: basic stdin reading (non-interactive or readline failed)
                if self.is_interactive {
                    println!();
                    self.print_separator();
                    print!("> ");
                } else {
                    print!("Query: ");
                }
                io::stdout().flush()?;

                let mut input = String::new();
                if io::stdin().read_line(&mut input).is_err() {
                    break;
                }
                input.trim().to_string()
            };

            if input.is_empty() {
                continue;
            }

            if self.is_interactive {
                self.print_separator();
                println!();
            }

            // Check for slash commands
            if let Some(command) = Command::parse(&input) {
                match command {
                    Command::Quit => {
                        self.save_models()?;
                        if let Some(ref mut handler) = self.input_handler {
                            if let Err(e) = handler.save_history() {
                                eprintln!("Warning: Failed to save history: {}", e);
                            }
                        }
                        if self.is_interactive {
                            println!("Models saved. Goodbye!");
                        }
                        break;
                    }
                    Command::Clear => {
                        self.conversation.clear();
                        println!("Conversation history cleared. Starting fresh.");
                        if self.is_interactive {
                            println!();
                            self.print_status_line();
                        }
                        continue;
                    }
                    _ => {
                        let output = handle_command(
                            command,
                            &self.metrics_logger,
                            Some(&self.threshold_router),
                            Some(&self.threshold_validator),
                        )?;
                        println!("{}", output);
                        continue;
                    }
                }
            }

            // Process query
            match self.process_query(&input).await {
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

        let success_pct = if router_stats.total_local_attempts == 0 {
            0.0
        } else {
            (router_stats.total_successes as f64 / router_stats.total_local_attempts as f64) * 100.0
        };

        // Get training metrics
        let quality_avg = self.training_trends.avg_quality();
        let similarity_avg = self.training_trends.avg_similarity();

        // Build single-line status string with training effectiveness and conversation context
        let turn_count = self.conversation.turn_count();
        let context_indicator = if turn_count > 0 {
            format!(" | Context: {} turns", turn_count)
        } else {
            String::new()
        };

        let status = if self.training_trends.measurement_count() > 0 {
            format!(
                "Training: {} queries | Local: {:.0}% | Success: {:.0}% | Quality: {:.2} | Similarity: {:.2} | Confidence: {:.2}{}",
                router_stats.total_queries,
                local_pct,
                success_pct,
                quality_avg,
                similarity_avg,
                router_stats.confidence_threshold,
                context_indicator
            )
        } else {
            // Fallback if no training data yet
            format!(
                "Training: {} queries | Local: {:.0}% | Success: {:.0}% | Confidence: {:.2} | Approval: {:.0}%{}",
                router_stats.total_queries,
                local_pct,
                success_pct,
                router_stats.confidence_threshold,
                validator_stats.approval_rate * 100.0,
                context_indicator
            )
        };

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

        // Add user message to conversation history
        self.conversation.add_user_message(query.to_string());

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

        // Track local and final responses for comparison
        let mut local_response: Option<String> = None;
        let claude_response: String;
        let routing_decision_str: String;
        let mut pattern_id: Option<String> = None;
        let mut routing_confidence: Option<f64> = None;
        let mut forward_reason: Option<String> = None;

        match decision {
            RouteDecision::Local {
                pattern_id: local_pattern_id,
                confidence,
            } => {
                // This branch is now dead code (router never returns Local)
                // Keep it for backward compatibility, but log a warning
                if self.is_interactive {
                    println!("⚠️  Warning: Unexpected local routing (pattern system removed)");
                    println!("→ Forwarding to Claude instead");
                }

                // Always forward to Claude
                let request = MessageRequest::with_context(self.conversation.get_messages())
                    .with_tools(create_tool_definitions());
                let response = self.claude_client.send_message(&request).await?;

                // Check for tool uses and execute them
                if response.has_tool_uses() {
                    claude_response = self.execute_tool_loop(response).await?;
                } else {
                    claude_response = response.text();
                }

                routing_decision_str = "forward".to_string();
                forward_reason = Some("pattern_system_removed".to_string());
                pattern_id = Some(local_pattern_id);
                routing_confidence = Some(confidence);
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
                }

                // Use full conversation context with tool definitions
                let request = MessageRequest::with_context(self.conversation.get_messages())
                    .with_tools(create_tool_definitions());

                // Disable streaming for now - it doesn't properly handle tool uses
                // TODO: Parse SSE stream for tool_use blocks to enable streaming with tools
                let use_streaming = false; // self.streaming_enabled && self.is_interactive;

                if use_streaming {
                    // Streaming path (disabled until tool detection added)
                    let rx = self.claude_client.send_message_stream(&request).await?;
                    claude_response = self.display_streaming_response(rx).await?;
                } else {
                    // Non-streaming path (supports tool use detection)
                    let response = self.claude_client.send_message(&request).await?;

                    let elapsed = start_time.elapsed().as_millis();
                    if self.is_interactive {
                        println!("✓ Received response ({}ms)", elapsed);
                    }

                    // Check for tool uses and execute them
                    if response.has_tool_uses() {
                        claude_response = self.execute_tool_loop(response).await?;
                    } else {
                        claude_response = response.text();
                    }
                }
                routing_decision_str = "forward".to_string();
                forward_reason = Some(reason.as_str().to_string());
            }
        };

        // Calculate quality and similarity
        let quality_score = self
            .threshold_validator
            .quality_score(query, &claude_response);

        let (similarity_score, divergence) = if let Some(ref local_resp) = local_response {
            use crate::metrics::semantic_similarity;
            let sim = semantic_similarity(local_resp, &claude_response)?;
            (Some(sim), Some(1.0 - sim))
        } else {
            (None, None)
        };

        // Online learning: Update threshold models
        if self.is_interactive {
            println!();
            print!("{}", "Learning... ".dark_grey());
            io::stdout().flush()?;
        }

        // Determine if this was a success (for router learning)
        let was_successful = routing_decision_str == "local" && quality_score >= 0.7;

        // Learn from this interaction
        self.threshold_router.learn(query, was_successful);
        self.threshold_validator
            .learn(query, &claude_response, quality_score >= 0.7);

        // Update training trends
        self.training_trends
            .add_measurement(quality_score, similarity_score);

        // Checkpoint every 10 queries
        let router_stats = self.threshold_router.stats();
        if router_stats.total_queries % 10 == 0 && router_stats.total_queries > 0 {
            let _ = self.save_models(); // Ignore errors during checkpoint
        }

        if self.is_interactive {
            println!("✓");
        }

        // Log metric with comparison data
        let query_hash = MetricsLogger::hash_query(query);
        let response_time_ms = start_time.elapsed().as_millis() as u64;

        let comparison = ResponseComparison {
            local_response: local_response.clone(),
            claude_response: claude_response.clone(),
            quality_score,
            similarity_score,
            divergence,
        };

        let router_confidence = Some(router_stats.confidence_threshold);
        let validator_confidence = Some(quality_score);

        let metric = RequestMetric::new(
            query_hash,
            routing_decision_str,
            pattern_id,
            routing_confidence,
            forward_reason,
            response_time_ms,
            comparison,
            router_confidence,
            validator_confidence,
        );

        self.metrics_logger.log(&metric)?;

        // Add assistant response to conversation history
        self.conversation.add_assistant_message(claude_response.clone());

        Ok(claude_response)
    }
}
