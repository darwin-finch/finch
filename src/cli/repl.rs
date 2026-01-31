// Interactive REPL with Claude Code-style interface

use anyhow::{Context, Result};
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
use crate::tools::executor::{generate_tool_signature, ApprovalSource, ToolSignature};
use crate::tools::implementations::{
    BashTool, GlobTool, GrepTool, ReadTool, WebFetchTool,
};
use crate::tools::patterns::ToolPattern;
use crate::tools::types::{ToolDefinition, ToolInputSchema, ToolUse};
use crate::tools::{PermissionManager, PermissionRule, ToolExecutor, ToolRegistry};

use super::commands::{handle_command, Command};
use super::conversation::ConversationHistory;
use super::input::InputHandler;
use super::menu::{Menu, MenuOption};

/// User's menu choice for tool confirmation
#[derive(Debug, Clone)]
enum ConfirmationChoice {
    ApproveOnce,
    ApproveExactSession,
    ApprovePatternSession,
    ApproveExactPersistent,
    ApprovePatternPersistent,
    Deny,
}

/// Result of a tool execution confirmation prompt
pub enum ConfirmationResult {
    ApproveOnce,
    ApproveExactSession(ToolSignature),
    ApprovePatternSession(ToolPattern),
    ApproveExactPersistent(ToolSignature),
    ApprovePatternPersistent(ToolPattern),
    Deny,
}

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
        // Save and exec - general-purpose session preservation + command execution
        ToolDefinition {
            name: "save_and_exec".to_string(),
            description: "Save conversation and model state, then execute any shell command. \
                         Session is saved to ~/.shammah/restart_state.json (also in $SHAMMAH_SESSION_FILE). \
                         Common examples:\n\
                         - Simple restart: './target/release/shammah --restore-session ~/.shammah/restart_state.json'\n\
                         - With prompt: './target/release/shammah --restore-session ~/.shammah/restart_state.json --initial-prompt \"test\"'\n\
                         - Build first: 'cargo build --release && ./target/release/shammah --restore-session ~/.shammah/restart_state.json'\n\
                         - Any command: 'python my_script.py'".to_string(),
            input_schema: ToolInputSchema::simple(vec![
                ("reason", "Why you're executing this command"),
                ("command", "Shell command to execute (supports &&, ||, pipes, etc.)"),
            ]),
        },
    ]
}

/// REPL operating mode
#[derive(Debug, Clone, PartialEq)]
pub enum ReplMode {
    /// Normal mode - all tools require confirmation
    Normal,
    /// Planning mode - only inspection tools allowed (read, glob, grep, web_fetch)
    Planning {
        task: String,
        plan_path: PathBuf,
        created_at: chrono::DateTime<chrono::Utc>,
    },
    /// Executing mode - all tools enabled after plan approval
    Executing {
        task: String,
        plan_path: PathBuf,
        approved_at: chrono::DateTime<chrono::Utc>,
    },
}

pub struct Repl {
    _config: Config,
    claude_client: ClaudeClient,
    router: Router, // Now contains ThresholdRouter
    metrics_logger: MetricsLogger,
    // Online learning models
    threshold_validator: ThresholdValidator, // Keep validator separate
    // Training metrics
    training_trends: TrainingTrends,
    // Model persistence
    models_dir: Option<PathBuf>,
    // Tool execution
    tool_executor: ToolExecutor,
    // UI state
    is_interactive: bool,
    streaming_enabled: bool,
    debug_enabled: bool,
    // Readline input handler
    input_handler: Option<InputHandler>,
    // Conversation history
    conversation: ConversationHistory,
    // REPL mode (normal, planning, executing)
    mode: ReplMode,
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

        // Load validator only (router is now in Router)
        let threshold_validator = Self::load_validator(models_dir.as_ref(), is_interactive);

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

        // Create permission manager (allow all for now)
        let permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);

        // Determine patterns path
        let patterns_path = dirs::home_dir()
            .map(|home| home.join(".shammah").join("tool_patterns.json"))
            .unwrap_or_else(|| PathBuf::from(".shammah/tool_patterns.json"));

        // Create tool executor
        let tool_executor = ToolExecutor::new(tool_registry, permissions, patterns_path)
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to initialize tool executor: {}", e);
                eprintln!("Tool pattern persistence may not work correctly");
                // Create fresh registry and try with temp path
                let mut fallback_registry = ToolRegistry::new();
                fallback_registry.register(Box::new(ReadTool));
                fallback_registry.register(Box::new(GlobTool));
                fallback_registry.register(Box::new(GrepTool));
                fallback_registry.register(Box::new(WebFetchTool::new()));
                fallback_registry.register(Box::new(BashTool));
                ToolExecutor::new(
                    fallback_registry,
                    PermissionManager::new().with_default_rule(PermissionRule::Allow),
                    std::env::temp_dir().join("shammah_patterns_fallback.json"),
                )
                .expect("Failed to create fallback tool executor")
            });

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
            router, // Contains ThresholdRouter now
            metrics_logger,
            threshold_validator,
            training_trends: TrainingTrends::new(20), // Track last 20 queries
            models_dir,
            tool_executor,
            is_interactive,
            streaming_enabled,
            debug_enabled: false,
            input_handler,
            conversation: ConversationHistory::new(),
            mode: ReplMode::Normal,
        }
    }

    /// Load validator from disk or create new one
    fn load_validator(models_dir: Option<&PathBuf>, is_interactive: bool) -> ThresholdValidator {
        let Some(models_dir) = models_dir else {
            return ThresholdValidator::new();
        };

        let validator_path = models_dir.join("threshold_validator.json");
        if validator_path.exists() {
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
        }
    }

    /// Save models to disk
    fn save_models(&mut self) -> Result<()> {
        let Some(ref models_dir) = self.models_dir else {
            // Still save patterns even if no models directory
            self.tool_executor.save_patterns()?;
            return Ok(());
        };

        std::fs::create_dir_all(models_dir)?;

        if self.is_interactive {
            print!("{}", "Saving models and patterns... ".dark_grey());
            io::stdout().flush()?;
        }

        // Save router (includes threshold router)
        self.router.save(models_dir.join("threshold_router.json"))?;

        // Save validator separately
        self.threshold_validator
            .save(models_dir.join("threshold_validator.json"))?;

        // Save tool patterns
        self.tool_executor.save_patterns()?;

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

        // Track tool calls to detect infinite loops
        let mut tool_call_history: Vec<(String, String)> = Vec::new();

        while current_response.has_tool_uses() && iteration < MAX_ITERATIONS {
            iteration += 1;

            let tool_uses = current_response.tool_uses();

            if self.is_interactive {
                println!("🔧 Executing {} tool(s)...", tool_uses.len());
            }

            // Check for repeated tool calls (infinite loop detection)
            for tool_use in &tool_uses {
                let input_hash = format!("{:?}", tool_use.input);
                let signature = (tool_use.name.clone(), input_hash.clone());

                // Count how many times we've seen this exact tool call
                let repeat_count = tool_call_history
                    .iter()
                    .filter(|sig| *sig == &signature)
                    .count();

                if repeat_count >= 2 {
                    if self.is_interactive {
                        eprintln!(
                            "⚠️  Warning: Tool '{}' called {} times with same input",
                            tool_use.name,
                            repeat_count + 1
                        );
                        eprintln!("⚠️  Possible infinite loop detected. Breaking...");
                    }

                    // Add error message to conversation explaining the issue
                    let error_msg = format!(
                        "Tool execution stopped: Detected infinite loop. \
                         Tool '{}' was called {} times with the same input.",
                        tool_use.name,
                        repeat_count + 1
                    );

                    return Ok(error_msg);
                }

                tool_call_history.push(signature);
            }

            // Execute all tool uses
            let mut tool_results = Vec::new();
            for tool_use in &tool_uses {
                if self.is_interactive {
                    println!("  → {}", tool_use.name);
                }

                // Check mode-based permissions first
                if !Self::is_tool_allowed_in_mode(&tool_use.name, &self.mode) {
                    use crate::tools::types::ToolResult;
                    let error_result = ToolResult::error(
                        tool_use.id.clone(),
                        format!(
                            "Tool '{}' is not allowed in planning mode.\n\
                             Reason: This tool can modify system state.\n\
                             Available tools: read, glob, grep, web_fetch\n\
                             Type /approve to execute your plan with all tools enabled.",
                            tool_use.name
                        ),
                    );
                    tool_results.push(error_result);
                    if self.is_interactive {
                        println!("    ✗ Blocked by plan mode");
                    }
                    continue;
                }

                // Generate tool signature for approval checking
                let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let signature = generate_tool_signature(tool_use, &working_dir);

                // Check if pre-approved in cache
                let approval_source = self.tool_executor.is_approved(&signature);

                match approval_source {
                    ApprovalSource::NotApproved => {
                        // Show prompt if interactive
                        if self.is_interactive {
                            match self.confirm_tool_execution(tool_use, &signature)? {
                                ConfirmationResult::ApproveOnce => {
                                    println!("  ✓ Approved");
                                }
                                ConfirmationResult::ApproveExactSession(sig) => {
                                    self.tool_executor.approve_exact_session(sig);
                                    println!("  ✓ Approved (remembered for session)");
                                }
                                ConfirmationResult::ApprovePatternSession(pattern) => {
                                    println!("  ✓ Approved pattern: {} (session)", pattern.pattern);
                                    self.tool_executor.approve_pattern_session(pattern);
                                }
                                ConfirmationResult::ApproveExactPersistent(sig) => {
                                    self.tool_executor.approve_exact_persistent(sig);
                                    println!("  ✓ Approved (saved permanently)");
                                }
                                ConfirmationResult::ApprovePatternPersistent(pattern) => {
                                    println!(
                                        "  ✓ Approved pattern: {} (saved permanently)",
                                        pattern.pattern
                                    );
                                    self.tool_executor.approve_pattern_persistent(pattern);
                                }
                                ConfirmationResult::Deny => {
                                    use crate::tools::types::ToolResult;
                                    let error_result = ToolResult::error(
                                        tool_use.id.clone(),
                                        "Tool execution denied by user".to_string(),
                                    );
                                    tool_results.push(error_result);
                                    println!("    ✗ Denied by user");
                                    continue;
                                }
                            }
                        }
                    }
                    ApprovalSource::SessionExact => {
                        // Already approved, execute silently
                    }
                    ApprovalSource::SessionPattern(ref id) => {
                        if self.is_interactive {
                            println!("  ✓ Matched session pattern ({})", &id[..8]);
                        }
                    }
                    ApprovalSource::PersistentExact => {
                        if self.is_interactive {
                            println!("  ✓ Matched saved approval");
                        }
                    }
                    ApprovalSource::PersistentPattern(ref id) => {
                        if self.is_interactive {
                            println!("  ✓ Matched saved pattern ({})", &id[..8]);
                        }
                    }
                }

                // Create save function that captures necessary state
                let models_dir = self.models_dir.clone();
                let router_ref = &self.router;
                let validator_ref = &self.threshold_validator;
                let save_fn = || -> Result<()> {
                    if let Some(ref dir) = models_dir {
                        std::fs::create_dir_all(dir)?;
                        router_ref.save(dir.join("threshold_router.json"))?;
                        validator_ref.save(dir.join("threshold_validator.json"))?;
                    }
                    Ok(())
                };

                let result = self
                    .tool_executor
                    .execute_tool(tool_use, Some(&self.conversation), Some(save_fn))
                    .await?;

                // Display tool result to user (Phase 1: Visibility)
                if self.is_interactive {
                    if result.is_error {
                        println!("    ✗ Error: {}", result.content);
                    } else {
                        println!("    ✓ Success");

                        // Show preview of result (first 500 chars)
                        let preview = if result.content.len() > 500 {
                            format!(
                                "{}... [truncated, {} chars total]",
                                &result.content[..500],
                                result.content.len()
                            )
                        } else {
                            result.content.clone()
                        };

                        // Indent output for readability
                        for line in preview.lines() {
                            println!("      {}", line);
                        }
                    }
                    println!(); // Blank line after each tool
                }

                tool_results.push(result);
            }

            // Build tool result message for Claude using XML-like structure
            // This format is easier for Claude to parse
            let mut tool_result_text = String::new();
            for (idx, result) in tool_results.iter().enumerate() {
                let tool_name = tool_uses[idx].name.as_str();

                if result.is_error {
                    tool_result_text.push_str(&format!(
                        "<tool_result>\n\
                         <tool_name>{}</tool_name>\n\
                         <status>error</status>\n\
                         <content>{}</content>\n\
                         </tool_result>\n\n",
                        tool_name, result.content
                    ));
                } else {
                    tool_result_text.push_str(&format!(
                        "<tool_result>\n\
                         <tool_name>{}</tool_name>\n\
                         <status>success</status>\n\
                         <content>{}</content>\n\
                         </tool_result>\n\n",
                        tool_name, result.content
                    ));
                }
            }

            // Important: Add Claude's tool-use response to conversation first
            // This maintains the user/assistant alternation required by the API
            let assistant_text = current_response.text();

            if assistant_text.is_empty() {
                // Response contains ONLY tool_use blocks, no text
                // We MUST add something to maintain conversation alternation
                self.conversation
                    .add_assistant_message("[Tool request]".to_string());

                if self.is_interactive {
                    println!("    (Claude requesting tool execution)");
                }
            } else {
                // Response has both text and tool_use blocks
                self.conversation
                    .add_assistant_message(assistant_text.clone());

                if self.is_interactive && !assistant_text.trim().is_empty() {
                    println!("    Claude: {}", assistant_text);
                }
            }

            // Then add tool results as a user message
            self.conversation.add_user_message(tool_result_text);

            // Re-invoke Claude with tool results
            let request = MessageRequest::with_context(self.conversation.get_messages())
                .with_tools(create_tool_definitions());

            current_response = self.claude_client.send_message(&request).await?;
        }

        // Handle max iterations or completion
        if iteration >= MAX_ITERATIONS {
            if self.is_interactive {
                eprintln!(
                    "⚠️  Warning: Max tool iterations reached ({})",
                    MAX_ITERATIONS
                );
                eprintln!("⚠️  Claude may be stuck in a loop. Returning last response.");
            }

            // IMPORTANT: Still add final response to conversation
            // Even if we hit max iterations, we need to maintain conversation state
            let final_text = current_response.text();
            if !final_text.is_empty() {
                self.conversation.add_assistant_message(final_text.clone());
            }
        }

        // Validate conversation state (debug check)
        let messages = self.conversation.get_messages();
        if messages.is_empty() {
            eprintln!("⚠️  ERROR: Conversation became empty after tool loop!");
            eprintln!("⚠️  This is a bug - please report to developers");
        }

        // Check for empty messages
        for (i, msg) in messages.iter().enumerate() {
            if msg.content.is_empty() {
                eprintln!("⚠️  WARNING: Message {} has empty content", i);
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

    /// Restore conversation from a saved state
    pub fn restore_conversation(&mut self, history: ConversationHistory) {
        self.conversation = history;
    }

    /// Run REPL with an optional initial prompt
    pub async fn run_with_initial_prompt(&mut self, initial_prompt: Option<String>) -> Result<()> {
        if let Some(prompt) = initial_prompt {
            // Process initial prompt before starting interactive loop
            if self.is_interactive {
                println!("\nProcessing initial prompt: \"{}\"", prompt);
                println!();
            }
            let response = self.process_query(&prompt).await?;
            if self.is_interactive {
                println!("{}", response);
            }
        }

        // Continue with normal REPL loop
        self.run().await
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
                    let prompt = self.get_prompt();
                    let handler = self.input_handler.as_mut().unwrap();
                    handler.read_line(&prompt)?
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
                    // Note: Can't use colored prompts in basic mode, so use plain text
                    let prompt = match &self.mode {
                        ReplMode::Normal => "> ",
                        ReplMode::Planning { .. } => "plan> ",
                        ReplMode::Executing { .. } => "exec> ",
                    };
                    print!("{}", prompt);
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
                    Command::PatternsList => {
                        let output = self.list_patterns()?;
                        println!("{}", output);
                        continue;
                    }
                    Command::PatternsRemove(ref id) => {
                        let output = self.remove_pattern(id)?;
                        println!("{}", output);
                        continue;
                    }
                    Command::PatternsClear => {
                        let output = self.clear_patterns()?;
                        println!("{}", output);
                        continue;
                    }
                    Command::PatternsAdd => {
                        let output = self.add_pattern_interactive()?;
                        println!("{}", output);
                        continue;
                    }
                    Command::Plan(ref task) => {
                        self.handle_plan_command(task.clone()).await?;
                        continue;
                    }
                    Command::Approve => {
                        self.handle_approve_command().await?;
                        continue;
                    }
                    Command::Reject => {
                        self.handle_reject_command().await?;
                        continue;
                    }
                    Command::ShowPlan => {
                        self.handle_show_plan_command().await?;
                        continue;
                    }
                    _ => {
                        let output = handle_command(
                            command,
                            &self.metrics_logger,
                            Some(&self.router), // CHANGED: pass router instead of threshold_router
                            Some(&self.threshold_validator),
                            &mut self.debug_enabled,
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

    /// Prompt user to confirm tool execution
    fn confirm_tool_execution(
        &mut self,
        tool_use: &ToolUse,
        signature: &ToolSignature,
    ) -> Result<ConfirmationResult> {
        // Display tool information
        println!();
        println!("Tool Execution Request:");
        println!("─────────────────────────");
        println!("Tool: {}", tool_use.name);

        // Show relevant parameters
        self.display_tool_params(tool_use);

        println!();

        // Build menu options
        let options = vec![
            MenuOption::with_description(
                "Yes (once only)",
                "Execute this time, ask again next time",
                ConfirmationChoice::ApproveOnce,
            ),
            MenuOption::with_description(
                "Yes, and remember exact command for this session",
                "Won't ask again for this exact command in this session",
                ConfirmationChoice::ApproveExactSession,
            ),
            MenuOption::with_description(
                "Yes, and remember pattern for this session",
                "Won't ask again for similar commands in this session",
                ConfirmationChoice::ApprovePatternSession,
            ),
            MenuOption::with_description(
                "Yes, and ALWAYS allow this exact command",
                "Save permanently - never ask again for this exact command",
                ConfirmationChoice::ApproveExactPersistent,
            ),
            MenuOption::with_description(
                "Yes, and ALWAYS allow this pattern",
                "Save permanently - never ask again for similar commands",
                ConfirmationChoice::ApprovePatternPersistent,
            ),
            MenuOption::with_description(
                "No (deny)",
                "Block this tool execution",
                ConfirmationChoice::Deny,
            ),
        ];

        // Show menu
        let choice = Menu::select(
            "Do you want to proceed?",
            options,
            Some("[↑↓ or j/k to move, Enter to select, or type 1-6]"),
        )?;

        // Convert choice to ConfirmationResult
        match choice {
            ConfirmationChoice::ApproveOnce => Ok(ConfirmationResult::ApproveOnce),
            ConfirmationChoice::ApproveExactSession => {
                Ok(ConfirmationResult::ApproveExactSession(signature.clone()))
            }
            ConfirmationChoice::ApprovePatternSession => {
                let pattern = self.build_pattern_from_signature(signature)?;
                Ok(ConfirmationResult::ApprovePatternSession(pattern))
            }
            ConfirmationChoice::ApproveExactPersistent => {
                Ok(ConfirmationResult::ApproveExactPersistent(signature.clone()))
            }
            ConfirmationChoice::ApprovePatternPersistent => {
                let pattern = self.build_pattern_from_signature(signature)?;
                Ok(ConfirmationResult::ApprovePatternPersistent(pattern))
            }
            ConfirmationChoice::Deny => Ok(ConfirmationResult::Deny),
        }
    }

    /// Helper to read a single line choice
    fn read_choice(&mut self, prompt: &str) -> Result<Option<String>> {
        if let Some(ref mut handler) = self.input_handler {
            handler.read_line(prompt)
        } else {
            print!("{}", prompt);
            io::stdout().flush()?;
            let mut line = String::new();
            io::stdin().read_line(&mut line)?;
            Ok(Some(line.trim().to_string()))
        }
    }

    /// Build a pattern from a signature by prompting user for wildcard choices
    fn build_pattern_from_signature(&mut self, signature: &ToolSignature) -> Result<ToolPattern> {
        // Parse signature to extract components
        let (command_part, dir_part) = self.parse_signature_components(signature);

        println!();

        // Generate options based on tool type
        let pattern_options = if let (Some(ref cmd), Some(ref dir)) = (&command_part, &dir_part) {
            let base_cmd = cmd.split_whitespace().next().unwrap_or(cmd);
            vec![
                format!("{} * in {}", base_cmd, dir),
                format!("{} in *", cmd),
                format!("{} * in *", base_cmd),
            ]
        } else if signature.tool_name == "read" {
            // For read tool, offer path-based patterns
            vec![
                signature.context_key.clone(), // Exact file
                format!("{}/**", Self::get_dir_from_context(&signature.context_key)),
                format!("reading *"),
            ]
        } else {
            // Generic patterns
            vec![
                signature.context_key.clone(),
                format!("{} *", signature.tool_name),
            ]
        };

        // Build menu options with None for "Other" choice
        let mut menu_options: Vec<MenuOption<Option<String>>> = pattern_options
            .into_iter()
            .map(|p| MenuOption::new(p.clone(), Some(p)))
            .collect();

        // Add "Other" option for custom pattern
        menu_options.push(MenuOption::with_description(
            "✏️  Other (type custom pattern)",
            "Enter your own pattern",
            None,
        ));

        let selection = Menu::select(
            "What should the pattern match?",
            menu_options,
            Some("Choose a pattern or select Other to type your own"),
        )?;

        let pattern_str = match selection {
            Some(pattern) => pattern,
            None => {
                // User selected "Other" - prompt for custom pattern
                Menu::text_input(
                    "Enter custom pattern:",
                    None,
                    Some("Use * for wildcards, ** for recursive"),
                )?
            }
        };

        Ok(ToolPattern::new(
            pattern_str.clone(),
            signature.tool_name.clone(),
            format!("Pattern: {}", pattern_str),
        ))
    }

    /// Parse signature context_key into command and directory components
    fn parse_signature_components(
        &self,
        signature: &ToolSignature,
    ) -> (Option<String>, Option<String>) {
        match signature.tool_name.as_str() {
            "bash" | "save_and_exec" => {
                // Format: "command args in /dir"
                if let Some(pos) = signature.context_key.rfind(" in ") {
                    let command = signature.context_key[..pos].to_string();
                    let dir = signature.context_key[pos + 4..].to_string();
                    (Some(command), Some(dir))
                } else {
                    (Some(signature.context_key.clone()), None)
                }
            }
            "read" => {
                // Format: "reading /path/to/file"
                if let Some(pos) = signature.context_key.find(' ') {
                    let path = signature.context_key[pos + 1..].to_string();
                    (None, Some(path))
                } else {
                    (None, Some(signature.context_key.clone()))
                }
            }
            "grep" => {
                // Format: "pattern 'text' in /dir"
                if let Some(pos) = signature.context_key.rfind(" in ") {
                    let pattern = signature.context_key[..pos].to_string();
                    let dir = signature.context_key[pos + 4..].to_string();
                    (Some(pattern), Some(dir))
                } else {
                    (Some(signature.context_key.clone()), None)
                }
            }
            _ => (None, None),
        }
    }

    /// Extract directory from a context string
    /// Check if tool is allowed in current mode
    fn is_tool_allowed_in_mode(tool_name: &str, mode: &ReplMode) -> bool {
        match mode {
            ReplMode::Normal | ReplMode::Executing { .. } => {
                // All tools allowed (subject to normal confirmation)
                true
            }
            ReplMode::Planning { .. } => {
                // Only inspection tools allowed
                matches!(tool_name, "read" | "glob" | "grep" | "web_fetch")
            }
        }
    }

    fn get_dir_from_context(context: &str) -> String {
        // For "reading /path/to/file.txt", return "/path/to"
        if let Some(last_slash) = context.rfind('/') {
            if last_slash > 0 {
                return context[..last_slash].to_string();
            }
        }
        ".".to_string()
    }

    /// Display tool parameters in user-friendly format
    fn display_tool_params(&self, tool_use: &ToolUse) {
        match tool_use.name.as_str() {
            "bash" => {
                if let Some(command) = tool_use.input["command"].as_str() {
                    println!("  Command: {}", command);
                }
                if let Some(desc) = tool_use.input.get("description").and_then(|v| v.as_str()) {
                    println!("  Description: {}", desc);
                }
            }
            "read" => {
                if let Some(path) = tool_use.input["file_path"].as_str() {
                    println!("  File: {}", path);
                }
            }
            "web_fetch" => {
                if let Some(url) = tool_use.input["url"].as_str() {
                    println!("  URL: {}", url);
                }
                if let Some(prompt) = tool_use.input.get("prompt").and_then(|v| v.as_str()) {
                    println!("  Prompt: {}", prompt);
                }
            }
            "grep" => {
                if let Some(pattern) = tool_use.input["pattern"].as_str() {
                    println!("  Pattern: {}", pattern);
                }
                if let Some(path) = tool_use.input.get("path").and_then(|v| v.as_str()) {
                    println!("  Path: {}", path);
                }
            }
            "glob" => {
                if let Some(pattern) = tool_use.input["pattern"].as_str() {
                    println!("  Pattern: {}", pattern);
                }
            }
            "save_and_exec" => {
                if let Some(command) = tool_use.input["command"].as_str() {
                    println!("  Command: {}", command);
                }
                if let Some(reason) = tool_use.input.get("reason").and_then(|v| v.as_str()) {
                    println!("  Reason: {}", reason);
                }
            }
            _ => {
                // Generic display for unknown tools
                println!("  Input: {}", tool_use.input);
            }
        }
    }

    // ============================================================================
    // Pattern Management Methods (Phase 3)
    // ============================================================================

    /// Format a duration as a human-readable string
    fn format_duration(duration: chrono::Duration) -> String {
        if duration.num_seconds() < 60 {
            format!("{}s ago", duration.num_seconds())
        } else if duration.num_minutes() < 60 {
            format!("{}m ago", duration.num_minutes())
        } else if duration.num_hours() < 24 {
            format!("{}h ago", duration.num_hours())
        } else {
            format!("{}d ago", duration.num_days())
        }
    }

    /// List all confirmation patterns
    pub fn list_patterns(&self) -> Result<String> {
        let store = self.tool_executor.persistent_store();
        let mut output = String::new();

        output.push_str("Confirmation Patterns\n");
        output.push_str("=====================\n\n");

        // Show patterns
        if store.patterns.is_empty() {
            output.push_str("No patterns configured.\n\n");
        } else {
            output.push_str("Patterns:\n");
            for pattern in &store.patterns {
                let now = chrono::Utc::now();
                let last_used_str = if let Some(last_used) = pattern.last_used {
                    let duration = now - last_used;
                    Self::format_duration(duration)
                } else {
                    "never".to_string()
                };

                let type_str = match pattern.pattern_type {
                    crate::tools::patterns::PatternType::Wildcard => "wildcard",
                    crate::tools::patterns::PatternType::Regex => "regex",
                };
                output.push_str(&format!(
                    "  {} ({})\n",
                    &pattern.id[..8.min(pattern.id.len())],
                    type_str
                ));
                output.push_str(&format!("    Tool: {}\n", pattern.tool_name));
                output.push_str(&format!("    Pattern: {}\n", pattern.pattern));
                output.push_str(&format!("    Description: {}\n", pattern.description));
                output.push_str(&format!(
                    "    Matches: {} | Last used: {}\n",
                    pattern.match_count, last_used_str
                ));
                output.push_str("\n");
            }
        }

        // Show exact approvals
        if store.exact_approvals.is_empty() {
            output.push_str("No exact approvals configured.\n");
        } else {
            output.push_str("Exact Approvals:\n");
            for approval in &store.exact_approvals {
                output.push_str(&format!(
                    "  {} ({})\n",
                    &approval.id[..8.min(approval.id.len())],
                    approval.tool_name
                ));
                output.push_str(&format!("    Signature: {}\n", approval.signature));
                output.push_str(&format!("    Matches: {}\n", approval.match_count));
                output.push_str("\n");
            }
        }

        output.push_str(&format!(
            "Total: {} patterns, {} exact approvals\n",
            store.patterns.len(),
            store.exact_approvals.len()
        ));

        Ok(output)
    }

    /// Remove a pattern by ID (supports partial matching with 8+ chars)
    pub fn remove_pattern(&mut self, id: &str) -> Result<String> {
        let store = self.tool_executor.persistent_store();

        // Find pattern by full or partial ID (8+ chars)
        let matching_pattern = if id.len() >= 8 {
            store
                .patterns
                .iter()
                .find(|p| p.id.starts_with(id))
                .cloned()
        } else {
            None
        };

        let matching_exact = if id.len() >= 8 {
            store
                .exact_approvals
                .iter()
                .find(|a| a.id.starts_with(id))
                .cloned()
        } else {
            None
        };

        if matching_pattern.is_none() && matching_exact.is_none() {
            return Ok(format!("No pattern or approval found with ID: {}", id));
        }

        // Show what we're removing
        if let Some(ref pattern) = matching_pattern {
            println!("Found pattern to remove:");
            println!("  ID: {}", &pattern.id[..8]);
            println!("  Tool: {}", pattern.tool_name);
            println!("  Pattern: {}", pattern.pattern);
            println!("  Match count: {}", pattern.match_count);
            println!();

            // Confirm if match count > 10
            if pattern.match_count > 10 {
                print!(
                    "This pattern has been used {} times. Remove? [y/N]: ",
                    pattern.match_count
                );
                io::stdout().flush()?;

                let mut confirm = String::new();
                io::stdin().read_line(&mut confirm)?;

                if !confirm.trim().eq_ignore_ascii_case("y") {
                    return Ok("Removal cancelled.".to_string());
                }
            }

            // Remove and save
            if self.tool_executor.remove_pattern(&pattern.id) {
                self.tool_executor.save_patterns()?;
                Ok(format!("Removed pattern: {}", &pattern.id[..8]))
            } else {
                Ok(format!("Failed to remove pattern: {}", id))
            }
        } else if let Some(ref approval) = matching_exact {
            println!("Found exact approval to remove:");
            println!("  ID: {}", &approval.id[..8]);
            println!("  Tool: {}", approval.tool_name);
            println!("  Signature: {}", approval.signature);
            println!("  Match count: {}", approval.match_count);
            println!();

            // Confirm if match count > 10
            if approval.match_count > 10 {
                print!(
                    "This approval has been used {} times. Remove? [y/N]: ",
                    approval.match_count
                );
                io::stdout().flush()?;

                let mut confirm = String::new();
                io::stdin().read_line(&mut confirm)?;

                if !confirm.trim().eq_ignore_ascii_case("y") {
                    return Ok("Removal cancelled.".to_string());
                }
            }

            // Remove and save
            if self.tool_executor.remove_pattern(&approval.id) {
                self.tool_executor.save_patterns()?;
                Ok(format!("Removed approval: {}", &approval.id[..8]))
            } else {
                Ok(format!("Failed to remove approval: {}", id))
            }
        } else {
            Ok(format!("No pattern or approval found with ID: {}", id))
        }
    }

    /// Clear all patterns with confirmation
    pub fn clear_patterns(&mut self) -> Result<String> {
        let store = self.tool_executor.persistent_store();
        let total = store.total_count();

        if total == 0 {
            return Ok("No patterns to clear.".to_string());
        }

        println!(
            "This will remove {} pattern(s) and {} exact approval(s).",
            store.patterns.len(),
            store.exact_approvals.len()
        );

        if !Menu::confirm("Are you sure?", false)? {
            return Ok("Clear cancelled.".to_string());
        }

        // Clear and save
        self.tool_executor.clear_persistent_patterns();
        self.tool_executor.save_patterns()?;

        Ok(format!("Cleared {} pattern(s) and approval(s).", total))
    }

    /// Add a pattern interactively
    pub fn add_pattern_interactive(&mut self) -> Result<String> {
        use crate::tools::patterns::PatternType;

        println!("Add Confirmation Pattern");
        println!("========================\n");

        // 1. Pattern type
        let pattern_type_options = vec![
            MenuOption::with_description(
                "Wildcard (*, **)",
                "Use * for wildcards, ** for recursive paths",
                PatternType::Wildcard,
            ),
            MenuOption::with_description(
                "Regex",
                "Use regular expression syntax",
                PatternType::Regex,
            ),
        ];

        let pattern_type = Menu::select(
            "Pattern type:",
            pattern_type_options,
            Some("[↑↓ or j/k to move, Enter to select, or type 1-2]"),
        )?;

        // 2. Tool name
        let tool_name = Menu::text_input(
            "Tool name:",
            None,
            Some("bash, read, grep, glob, web_fetch, save_and_exec"),
        )?;

        if tool_name.is_empty() {
            return Ok("Pattern creation cancelled (no tool name).".to_string());
        }

        // 3. Pattern string (with help)
        println!("\nPattern syntax:");
        match pattern_type {
            PatternType::Wildcard => {
                println!("  * = match anything (single component)");
                println!("  ** = match anything recursively (paths)");
                println!("Examples:");
                println!("  cargo * in /project");
                println!("  reading /project/**");
                println!("  cargo * in *");
            }
            PatternType::Regex => {
                println!("  Standard regex syntax");
                println!("Examples:");
                println!("  ^cargo (test|build)$");
                println!("  reading /project/src/.*\\.rs$");
            }
        }

        let pattern_str = Menu::text_input(
            "Pattern:",
            None,
            Some("Enter the pattern string"),
        )?;

        if pattern_str.is_empty() {
            return Ok("Pattern creation cancelled (no pattern).".to_string());
        }

        // 4. Description
        let description = Menu::text_input(
            "Description:",
            None,
            Some("Brief description of what this pattern allows"),
        )?;

        // 5. Create pattern and validate
        let pattern = ToolPattern::new_with_type(
            pattern_str.clone(),
            tool_name.clone(),
            description,
            pattern_type,
        );

        if let Err(e) = pattern.validate() {
            return Ok(format!("Invalid pattern: {}", e));
        }

        // 6. Optional: Test pattern
        println!("\nPattern created:");
        println!("  Tool: {}", pattern.tool_name);
        println!("  Pattern: {}", pattern.pattern);
        println!("  Type: {:?}", pattern.pattern_type);

        if Menu::confirm("\nTest pattern?", false)? {
            let test_str = Menu::text_input(
                "Enter test string:",
                None,
                Some("String to test against the pattern"),
            )?;

            let test_sig = ToolSignature {
                tool_name: pattern.tool_name.clone(),
                context_key: test_str,
            };

            if pattern.matches(&test_sig) {
                println!("✓ Pattern matches!");
            } else {
                println!("✗ Pattern does not match.");
            }
            println!();
        }

        // 7. Confirm and save
        print!("Save pattern? [Y/n]: ");
        io::stdout().flush()?;

        let mut save_choice = String::new();
        io::stdin().read_line(&mut save_choice)?;

        if save_choice.trim().eq_ignore_ascii_case("n") {
            return Ok("Pattern creation cancelled.".to_string());
        }

        // Add to executor and save
        self.tool_executor
            .approve_pattern_persistent(pattern.clone());
        self.tool_executor.save_patterns()?;

        Ok(format!(
            "Pattern saved: {} ({})",
            &pattern.id[..8],
            pattern.pattern
        ))
    }

    /// Get mode-specific prompt string
    fn get_prompt(&self) -> String {
        match &self.mode {
            ReplMode::Normal => "> ".to_string(),
            ReplMode::Planning { .. } => format!("{} ", "plan>".blue()),
            ReplMode::Executing { .. } => format!("{} ", "exec>".green()),
        }
    }

    /// Print training status below the prompt (only in interactive mode)
    fn print_status_line(&self) {
        if !self.is_interactive {
            return;
        }

        let router_stats = self.router.stats(); // CHANGED: use router.stats()
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

        // Build mode indicator
        let mode_indicator = match &self.mode {
            ReplMode::Normal => String::new(),
            ReplMode::Planning { .. } => format!(" {}", "[PLANNING MODE - Inspection Only]".blue().bold()),
            ReplMode::Executing { .. } => format!(" {}", "[EXECUTING PLAN]".green().bold()),
        };

        // Build single-line status string with training effectiveness and conversation context
        let turn_count = self.conversation.turn_count();
        let context_indicator = if turn_count > 0 {
            format!(" | Context: {} turns", turn_count)
        } else {
            String::new()
        };

        let status = if self.training_trends.measurement_count() > 0 {
            format!(
                "{}Training: {} queries | Local: {:.0}% | Success: {:.0}% | Quality: {:.2} | Similarity: {:.2} | Confidence: {:.2}{}",
                mode_indicator,
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
                "{}Training: {} queries | Local: {:.0}% | Success: {:.0}% | Confidence: {:.2} | Approval: {:.0}%{}",
                mode_indicator,
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

    pub async fn process_query(&mut self, query: &str) -> Result<String> {
        let start_time = Instant::now();

        // Add user message to conversation history
        self.conversation.add_user_message(query.to_string());

        if self.is_interactive {
            print!("{}", "Analyzing...".dark_grey());
            io::stdout().flush()?;
        }

        // Make routing decision (uses threshold router internally)
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
                if self.is_interactive {
                    println!("✓ Crisis check: PASS");
                    println!("✓ Threshold check: PASS (confidence: {:.2})", confidence);
                    println!("→ Routing: LOCAL GENERATION");
                }

                // FUTURE: This is where local generation will happen
                // For now, fall back to forwarding with a notice
                if self.is_interactive {
                    println!("⚠️  Note: Local generation not yet trained");
                    println!("→ Forwarding to Claude for now");
                }

                // Forward to Claude (temporary until generator is trained)
                let request = MessageRequest::with_context(self.conversation.get_messages())
                    .with_tools(create_tool_definitions());
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

                routing_decision_str = "local_attempted".to_string();
                pattern_id = Some(local_pattern_id);
                routing_confidence = Some(confidence);
                forward_reason = Some("untrained_generator".to_string());
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
                            println!("✗ Threshold check: FAIL (confidence too low)");
                            println!("→ Routing: FORWARDING TO CLAUDE");
                        }
                    }
                }

                // Use full conversation context with tool definitions
                let request = MessageRequest::with_context(self.conversation.get_messages())
                    .with_tools(create_tool_definitions());

                // Try streaming first, fallback to buffered if tools detected
                let use_streaming = self.streaming_enabled && self.is_interactive;

                if use_streaming {
                    // Streaming path - will abort if tools detected
                    let rx = self.claude_client.send_message_stream(&request).await?;
                    match self.display_streaming_response(rx).await {
                        Ok(text) => {
                            // Streaming succeeded (no tools)
                            claude_response = text;
                        }
                        Err(e) if e.to_string().contains("TOOLS_DETECTED") => {
                            // Tools detected in stream - fallback to buffered mode
                            if self.is_interactive {
                                println!("\n🔧 Tools needed - switching to buffered mode...");
                            }
                            let response = self.claude_client.send_message(&request).await?;

                            let elapsed = start_time.elapsed().as_millis();
                            if self.is_interactive {
                                println!("✓ Received response ({}ms)", elapsed);
                            }

                            // Execute tool loop
                            claude_response = self.execute_tool_loop(response).await?;
                        }
                        Err(e) => {
                            // Real error - propagate
                            return Err(e);
                        }
                    }
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
        self.router.learn(query, was_successful); // CHANGED: use router.learn()
        self.threshold_validator
            .learn(query, &claude_response, quality_score >= 0.7);

        // Update training trends
        self.training_trends
            .add_measurement(quality_score, similarity_score);

        // Checkpoint every 10 queries
        let router_stats = self.router.stats(); // CHANGED: use router.stats()
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

        // Auto-save plan if in planning mode
        if let ReplMode::Planning { plan_path, .. } = &self.mode {
            // Detect if response contains a plan
            if claude_response.contains("# Plan:") || claude_response.contains("## Analysis") {
                if let Err(e) = std::fs::write(plan_path, &claude_response) {
                    eprintln!("Warning: Failed to save plan: {}", e);
                } else if self.is_interactive {
                    println!("\n✓ Plan saved to: {}", plan_path.display());
                    println!("Type /show-plan to review, /approve to execute, /reject to cancel.");
                }
            }
        }

        // Add assistant response to conversation history
        self.conversation
            .add_assistant_message(claude_response.clone());

        Ok(claude_response)
    }

    /// Handle /plan command - enter planning mode
    async fn handle_plan_command(&mut self, task: String) -> Result<()> {
        use chrono::Utc;

        // Check if already in plan mode
        if matches!(self.mode, ReplMode::Planning { .. } | ReplMode::Executing { .. }) {
            println!(
                "⚠️  Already in {} mode. Finish current task first.",
                match self.mode {
                    ReplMode::Planning { .. } => "planning",
                    ReplMode::Executing { .. } => "executing",
                    _ => unreachable!(),
                }
            );
            return Ok(());
        }

        // Create plans directory
        let plans_dir = dirs::home_dir()
            .context("Home directory not found")?
            .join(".shammah")
            .join("plans");
        std::fs::create_dir_all(&plans_dir)?;

        // Generate plan filename
        let timestamp = Utc::now().format("%Y%m%d_%H%M%S");
        let plan_path = plans_dir.join(format!("plan_{}.md", timestamp));

        // Transition to planning mode
        self.mode = ReplMode::Planning {
            task: task.clone(),
            plan_path: plan_path.clone(),
            created_at: Utc::now(),
        };

        println!("{}", "✓ Entered planning mode".blue().bold());
        println!("📋 Task: {}", task);
        println!("📁 Plan will be saved to: {}", plan_path.display());
        println!();
        println!("{}", "Available tools:".green());
        println!("  read, glob, grep, web_fetch");
        println!("{}", "Blocked tools:".red());
        println!("  bash, save_and_exec");
        println!();
        println!("Ask me to explore the codebase and generate a plan.");
        println!(
            "{}",
            "Type /show-plan to view, /approve to execute, /reject to cancel."
                .dark_grey()
        );

        // Add mode change notification to conversation
        self.conversation.add_user_message(format!(
            "[System: Entered planning mode for task: {}]\n\
             Available tools: read, glob, grep, web_fetch\n\
             Blocked tools: bash, save_and_exec\n\
             Please explore the codebase and generate a detailed plan.",
            task
        ));

        if self.is_interactive {
            println!();
            self.print_status_line();
        }

        Ok(())
    }

    /// Handle /approve command - approve plan and start execution
    async fn handle_approve_command(&mut self) -> Result<()> {
        use chrono::Utc;
        use crate::cli::menu::{Menu, MenuOption};

        match &self.mode {
            ReplMode::Planning {
                task, plan_path, ..
            } => {
                // Clone values we need before mutating self.mode
                let task_clone = task.clone();
                let plan_path_clone = plan_path.clone();

                println!("{}", "✓ Plan approved!".green().bold());
                println!();

                // Show context clearing options
                println!("The plan has been saved to: {}", plan_path_clone.display());
                println!();
                println!("Would you like to:");
                println!("  1. Clear conversation and execute plan (recommended)");
                println!("  2. Keep conversation history and execute");
                println!();

                let options = vec![
                    MenuOption::with_description(
                        "Clear context (recommended)",
                        "Reduces token usage and focuses execution on the plan",
                        true,
                    ),
                    MenuOption::with_description(
                        "Keep context",
                        "Preserves exploration history in conversation",
                        false,
                    ),
                ];

                let clear_context = Menu::select(
                    "Choose execution mode:",
                    options,
                    Some("[↑↓ or j/k to move, Enter to select, or type 1-2]"),
                )?;

                // Transition to executing mode
                self.mode = ReplMode::Executing {
                    task: task_clone,
                    plan_path: plan_path_clone.clone(),
                    approved_at: Utc::now(),
                };

                if clear_context {
                    // Clear conversation and add plan as context
                    println!();
                    println!("{}", "Clearing conversation context...".blue());
                    self.conversation.clear();

                    // Read plan file and add as initial context
                    if plan_path_clone.exists() {
                        let plan_content = std::fs::read_to_string(&plan_path_clone)
                            .context("Failed to read plan file")?;
                        self.conversation.add_user_message(format!(
                            "Please execute this plan:\n\n{}",
                            plan_content
                        ));
                        println!("{}", "✓ Context cleared. Plan loaded as primary reference.".green());
                    } else {
                        println!("{}", "⚠️  Plan file not found. Adding approval message only.".yellow());
                        self.conversation.add_user_message(
                            "[System: Plan approved! All tools are now enabled. \
                             You may execute bash commands and modify files.]"
                                .to_string(),
                        );
                    }
                } else {
                    // Keep history, just add approval message
                    println!();
                    println!("{}", "Keeping conversation context...".blue());
                    self.conversation.add_user_message(
                        "[System: Plan approved! All tools are now enabled. \
                         You may execute bash commands and modify files.]"
                            .to_string(),
                    );
                }

                println!();
                println!(
                    "{}",
                    "All tools enabled. Please proceed with implementation.".green()
                );

                if self.is_interactive {
                    println!();
                    self.print_status_line();
                }
            }
            ReplMode::Normal => {
                println!("⚠️  No plan to approve. Use /plan first.");
            }
            ReplMode::Executing { .. } => {
                println!("⚠️  Already executing plan.");
            }
        }
        Ok(())
    }

    /// Handle /reject command - reject plan and return to normal mode
    async fn handle_reject_command(&mut self) -> Result<()> {
        match &self.mode {
            ReplMode::Planning { .. } | ReplMode::Executing { .. } => {
                println!("{}", "✗ Plan rejected. Returning to normal mode.".yellow());
                self.mode = ReplMode::Normal;
                self.conversation
                    .add_user_message("[System: Plan rejected by user.]".to_string());

                if self.is_interactive {
                    println!();
                    self.print_status_line();
                }
            }
            ReplMode::Normal => {
                println!("⚠️  No active plan to reject.");
            }
        }
        Ok(())
    }

    /// Handle /show-plan command - display current plan
    async fn handle_show_plan_command(&mut self) -> Result<()> {
        match &self.mode {
            ReplMode::Planning { plan_path, .. } | ReplMode::Executing { plan_path, .. } => {
                if plan_path.exists() {
                    let content = std::fs::read_to_string(plan_path)?;
                    println!("\n{}", "=".repeat(60));
                    println!("PLAN:");
                    println!("{}", "=".repeat(60));
                    println!("{}", content);
                    println!("{}", "=".repeat(60));
                } else {
                    println!("⚠️  Plan file not yet created.");
                }
            }
            ReplMode::Normal => {
                println!("⚠️  No active plan. Use /plan to start.");
            }
        }
        Ok(())
    }
}
