// Simplified REPL for daemon-only mode
//
// This REPL is a thin client that communicates with the daemon via HTTP.
// All model loading and inference happens in the daemon.
// Tool execution still happens locally for security.

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::client::DaemonClient;
use crate::claude::{ContentBlock, Message};
use crate::config::Config;
use crate::tools::executor::ToolExecutor;
use crate::tools::types::{ToolDefinition, ToolUse};
use super::conversation::ConversationHistory;
use super::input::InputHandler;
use super::tui::TuiRenderer;
use crate::{output_status, output_error};

/// Simplified REPL that talks to daemon
pub struct SimplifiedRepl {
    config: Config,
    daemon_client: DaemonClient,
    conversation: Arc<RwLock<ConversationHistory>>,
    tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
    tool_definitions: Vec<ToolDefinition>,
    input_handler: Option<InputHandler>,
    tui_renderer: Option<Arc<RwLock<TuiRenderer>>>,
}

impl SimplifiedRepl {
    /// Create a new simplified REPL
    pub async fn new(
        config: Config,
        daemon_client: DaemonClient,
        tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
    ) -> Result<Self> {
        // Initialize conversation history
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));

        // Get tool definitions (async)
        let tool_definitions = {
            let executor = tool_executor.lock().await;
            executor.registry()
                .get_all_tools()
                .into_iter()
                .map(|tool| tool.definition())
                .collect()
        };

        // Initialize input handler if interactive
        let input_handler = if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            match InputHandler::new() {
                Ok(handler) => Some(handler),
                Err(e) => {
                    output_error!("Failed to initialize input handler: {}", e);
                    None
                }
            }
        } else {
            None
        };

        // TUI initialization (if enabled in config)
        let tui_renderer = if config.tui_enabled {
            // Get global OutputManager and StatusBar (created in main.rs)
            use crate::cli::global_output::{global_output, global_status};

            match TuiRenderer::new(global_output(), global_status()) {
                Ok(renderer) => {
                    output_status!("✓ TUI mode enabled");
                    Some(Arc::new(RwLock::new(renderer)))
                }
                Err(e) => {
                    output_error!("Failed to initialize TUI: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            config,
            daemon_client,
            conversation,
            tool_executor,
            tool_definitions,
            input_handler,
            tui_renderer,
        })
    }

    /// Run interactive REPL loop
    pub async fn run_interactive(mut self, initial_prompt: Option<String>) -> Result<()> {
        output_status!("Shammah REPL (daemon mode)");
        output_status!("Type /help for commands, /exit to quit");

        // Process initial prompt if provided
        if let Some(prompt) = initial_prompt {
            self.process_query(&prompt).await?;
        }

        // Main REPL loop
        loop {
            // Read user input
            let query = match &mut self.input_handler {
                Some(handler) => {
                    match handler.read_line("> ") {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            // EOF (Ctrl+D)
                            output_status!("Goodbye!");
                            break;
                        }
                        Err(e) => {
                            output_error!("Input error: {}", e);
                            continue;
                        }
                    }
                }
                None => {
                    // Fallback: basic stdin
                    use std::io::{self, BufRead};
                    print!("> ");
                    std::io::Write::flush(&mut std::io::stdout())?;
                    let mut line = String::new();
                    io::stdin().lock().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };

            // Check for empty input
            if query.trim().is_empty() {
                continue;
            }

            // Handle commands
            if query.starts_with('/') {
                if self.handle_command(&query).await? {
                    break; // Exit requested
                }
                continue;
            }

            // Process query
            if let Err(e) = self.process_query(&query).await {
                output_error!("Error processing query: {}", e);
            }
        }

        Ok(())
    }

    /// Process a single query
    async fn process_query(&mut self, query: &str) -> Result<String> {
        // Add user message to conversation
        {
            let mut conv = self.conversation.write().await;
            conv.add_message(Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: query.to_string(),
                }],
            });
        }

        // Send to daemon with tools
        let executor = self.tool_executor.lock().await;
        let response = self.daemon_client
            .query_with_tools(query, self.tool_definitions.clone(), &*executor)
            .await
            .context("Failed to query daemon")?;

        // Add assistant response to conversation
        {
            let mut conv = self.conversation.write().await;
            conv.add_message(Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: response.clone(),
                }],
            });
        }

        output_status!("{}", response);
        Ok(response)
    }

    /// Handle REPL commands
    async fn handle_command(&mut self, command: &str) -> Result<bool> {
        match command {
            "/exit" | "/quit" => {
                output_status!("Goodbye!");
                return Ok(true);
            }
            "/help" => {
                self.show_help();
            }
            "/clear" => {
                let mut conv = self.conversation.write().await;
                *conv = ConversationHistory::new();
                output_status!("Conversation cleared");
            }
            "/history" => {
                let conv = self.conversation.read().await;
                let messages = conv.get_messages();
                output_status!("Conversation history ({} messages):", messages.len());
                for (i, msg) in messages.iter().enumerate() {
                    output_status!("  [{}] {}: {} content blocks", i + 1, msg.role, msg.content.len());
                }
            }
            _ => {
                output_error!("Unknown command: {}", command);
                output_error!("Type /help for available commands");
            }
        }

        Ok(false)
    }

    /// Show help message
    fn show_help(&self) {
        output_status!("Available commands:");
        output_status!("  /help     - Show this help message");
        output_status!("  /exit     - Exit the REPL");
        output_status!("  /clear    - Clear conversation history");
        output_status!("  /history  - Show conversation history");
    }

    /// Restore a saved session
    pub fn restore_session(&mut self, _session_path: &std::path::Path) -> Result<()> {
        // TODO: Implement session restoration
        output_status!("Session restoration not yet implemented in daemon mode");
        Ok(())
    }
}
