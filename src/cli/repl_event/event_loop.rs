// Event loop for concurrent REPL - handles user input, queries, and rendering simultaneously

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex, RwLock};
use uuid::Uuid;

use crate::cli::conversation::ConversationHistory;
use crate::cli::output_manager::OutputManager;
use crate::cli::status_bar::StatusBar;
use crate::cli::tui::{spawn_input_task, Dialog, DialogOption, DialogType, TuiRenderer};
use crate::claude::{ClaudeClient, MessageRequest};
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::tools::executor::{generate_tool_signature, ToolExecutor};
use crate::tools::patterns::ToolPattern;
use crate::tools::types::ToolDefinition;

use super::events::ReplEvent;
use super::query_state::{QueryState, QueryStateManager};
use super::tool_execution::ToolExecutionCoordinator;

/// Main event loop for concurrent REPL
pub struct EventLoop {
    /// Channel for receiving events
    event_rx: mpsc::UnboundedReceiver<ReplEvent>,
    /// Channel for sending events
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Channel for receiving user input
    input_rx: mpsc::UnboundedReceiver<String>,

    /// Shared conversation history
    conversation: Arc<RwLock<ConversationHistory>>,

    /// Query state manager
    query_states: Arc<QueryStateManager>,

    /// Claude client for API calls (shared reference)
    claude_client: Arc<ClaudeClient>,

    /// Tool definitions for Claude API
    tool_definitions: Arc<Vec<ToolDefinition>>,

    /// TUI renderer
    tui_renderer: Arc<Mutex<TuiRenderer>>,

    /// Output manager
    output_manager: Arc<OutputManager>,

    /// Status bar
    status_bar: Arc<StatusBar>,

    /// Whether streaming is enabled
    streaming_enabled: bool,

    /// Tool execution coordinator
    tool_coordinator: ToolExecutionCoordinator,

    /// Tool results collected per query (query_id -> Vec<(tool_id, result)>)
    tool_results: Arc<RwLock<std::collections::HashMap<Uuid, Vec<(String, Result<String>)>>>>,
}

impl EventLoop {
    /// Create a new event loop
    pub fn new(
        conversation: Arc<RwLock<ConversationHistory>>,
        claude_client: Arc<ClaudeClient>,
        tool_definitions: Vec<ToolDefinition>,
        tool_executor: Arc<ToolExecutor>,
        tui_renderer: TuiRenderer,
        output_manager: Arc<OutputManager>,
        status_bar: Arc<StatusBar>,
        streaming_enabled: bool,
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
    ) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        // Wrap TUI in Arc<Mutex> for shared access
        let tui_renderer = Arc::new(Mutex::new(tui_renderer));

        // Spawn input handler task
        let input_rx = spawn_input_task(Arc::clone(&tui_renderer));

        // Create tool coordinator
        let tool_coordinator = ToolExecutionCoordinator::new(
            event_tx.clone(),
            Arc::clone(&tool_executor),
            Arc::clone(&conversation),
            Arc::clone(&local_generator),
            Arc::clone(&tokenizer),
        );

        Self {
            event_rx,
            event_tx,
            input_rx,
            conversation,
            query_states: Arc::new(QueryStateManager::new()),
            claude_client,
            tool_definitions: Arc::new(tool_definitions),
            tui_renderer,
            output_manager,
            status_bar,
            streaming_enabled,
            tool_coordinator,
            tool_results: Arc::new(RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Run the event loop
    pub async fn run(&mut self) -> Result<()> {
        // Render interval (100ms)
        let mut render_interval = tokio::time::interval(Duration::from_millis(100));

        // Cleanup interval (30 seconds)
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(30));

        loop {
            tokio::select! {
                // User input event
                Some(input) = self.input_rx.recv() => {
                    self.handle_user_input(input).await?;
                }

                // REPL event (query complete, tool result, etc.)
                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event).await?;
                }

                // Periodic rendering
                _ = render_interval.tick() => {
                    self.render_tui().await?;
                }

                // Periodic cleanup
                _ = cleanup_interval.tick() => {
                    self.cleanup_old_queries().await;
                }
            }
        }
    }

    /// Handle user input (query or command)
    async fn handle_user_input(&mut self, input: String) -> Result<()> {
        // Check if it's a command
        if input.trim().starts_with('/') {
            // TODO: Handle commands
            self.output_manager
                .write_status(format!("Command not implemented yet: {}", input));
            return Ok(());
        }

        // Check if it's a quit command
        if input.trim().eq_ignore_ascii_case("quit")
            || input.trim().eq_ignore_ascii_case("exit")
        {
            self.event_tx
                .send(ReplEvent::Shutdown)
                .context("Failed to send shutdown event")?;
            return Ok(());
        }

        // Create a new query
        let conversation_snapshot = self.conversation.read().await.snapshot();
        let query_id = self.query_states.create_query(conversation_snapshot).await;

        // Add user message to conversation
        self.conversation
            .write()
            .await
            .add_user_message(input.clone());

        // Spawn query processing task
        self.spawn_query_task(query_id, input).await;

        Ok(())
    }

    /// Spawn a background task to process a query
    async fn spawn_query_task(&self, query_id: Uuid, query: String) {
        let event_tx = self.event_tx.clone();
        let claude_client = Arc::clone(&self.claude_client);
        let tool_definitions = Arc::clone(&self.tool_definitions);
        let conversation = Arc::clone(&self.conversation);
        let query_states = Arc::clone(&self.query_states);
        let tool_coordinator = self.tool_coordinator.clone();

        tokio::spawn(async move {
            Self::process_query_with_tools(
                query_id,
                query,
                event_tx,
                claude_client,
                tool_definitions,
                conversation,
                query_states,
                tool_coordinator,
            )
            .await;
        });
    }

    /// Process a query with potential tool execution loop
    async fn process_query_with_tools(
        query_id: Uuid,
        _query: String,
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        claude_client: Arc<ClaudeClient>,
        tool_definitions: Arc<Vec<ToolDefinition>>,
        conversation: Arc<RwLock<ConversationHistory>>,
        query_states: Arc<QueryStateManager>,
        tool_coordinator: ToolExecutionCoordinator,
    ) {
        const MAX_TOOL_ITERATIONS: usize = 10;
        let mut iteration = 0;

        loop {
            if iteration >= MAX_TOOL_ITERATIONS {
                let _ = event_tx.send(ReplEvent::QueryFailed {
                    query_id,
                    error: format!("Max tool iterations ({}) reached", MAX_TOOL_ITERATIONS),
                });
                return;
            }

            iteration += 1;

            // Get conversation snapshot for this iteration
            let messages = conversation.read().await.get_messages();

            // Create request
            let request = MessageRequest::with_context(messages)
                .with_tools((*tool_definitions).clone());

            // Send to Claude
            let response = match claude_client.send_message(&request).await {
                Ok(r) => r,
                Err(e) => {
                    let _ = event_tx.send(ReplEvent::QueryFailed {
                        query_id,
                        error: format!("{}", e),
                    });
                    return;
                }
            };

            let response_text = response.text();
            let tool_uses = response.tool_uses();

            // Check if response has tool uses
            if tool_uses.is_empty() {
                // No tools, query is complete
                let _ = event_tx.send(ReplEvent::QueryComplete {
                    query_id,
                    response: response_text,
                });
                return;
            }

            // Update state: executing tools
            query_states
                .update_state(
                    query_id,
                    QueryState::ExecutingTools {
                        tools_pending: tool_uses.len(),
                        tools_completed: 0,
                    },
                )
                .await;

            // Add assistant message (tool request) to conversation
            if response_text.is_empty() {
                conversation
                    .write()
                    .await
                    .add_assistant_message("[Tool request]".to_string());
            } else {
                conversation
                    .write()
                    .await
                    .add_assistant_message(response_text.clone());
            }

            // Spawn tool executions concurrently
            for tool_use in tool_uses {
                tool_coordinator.spawn_tool_execution(query_id, tool_use.clone());
            }

            // Wait for all tool results (collect from events)
            // This is handled by the main event loop via ToolResult events
            // For now, we exit and let the event loop handle collection
            // The query will be marked complete when all tools finish

            return; // Exit task, let event loop collect results
        }
    }

    /// Handle an event from the event channel
    async fn handle_event(&mut self, event: ReplEvent) -> Result<()> {
        match event {
            ReplEvent::UserInput { input } => {
                self.handle_user_input(input).await?;
            }

            ReplEvent::QueryComplete { query_id, response } => {
                // Add response to conversation
                self.conversation
                    .write()
                    .await
                    .add_assistant_message(response.clone());

                // Update query state
                self.query_states
                    .update_state(query_id, QueryState::Completed { response: response.clone() })
                    .await;

                // Display response
                self.output_manager.write_claude(&response);
            }

            ReplEvent::QueryFailed { query_id, error } => {
                // Update query state
                self.query_states
                    .update_state(query_id, QueryState::Failed { error: error.clone() })
                    .await;

                // Display error
                self.output_manager.write_error(format!("Query failed: {}", error));
            }

            ReplEvent::ToolResult {
                query_id,
                tool_id,
                result,
            } => {
                self.handle_tool_result(query_id, tool_id, result).await?;
            }

            ReplEvent::ToolApprovalNeeded {
                query_id,
                tool_use,
                response_tx,
            } => {
                self.handle_tool_approval_request(query_id, tool_use, response_tx)
                    .await?;
            }

            ReplEvent::OutputReady { message } => {
                self.output_manager.write_status(message);
            }

            ReplEvent::Shutdown => {
                // TODO: Graceful shutdown
                std::process::exit(0);
            }
        }

        Ok(())
    }

    /// Render the TUI
    async fn render_tui(&self) -> Result<()> {
        let mut tui = self.tui_renderer.lock().await;
        tui.flush_output_safe(&self.output_manager)?;
        Ok(())
    }

    /// Clean up old completed queries
    async fn cleanup_old_queries(&self) {
        self.query_states
            .cleanup_old_queries(Duration::from_secs(30))
            .await;
    }

    /// Handle a tool result
    async fn handle_tool_result(
        &mut self,
        query_id: Uuid,
        tool_id: String,
        result: Result<String>,
    ) -> Result<()> {
        // Display tool result
        match &result {
            Ok(content) => {
                self.output_manager.write_tool(
                    &tool_id,
                    format!("✓ Success ({})", if content.len() > 100 {
                        format!("{}...", &content[..100])
                    } else {
                        content.clone()
                    }),
                );
            }
            Err(e) => {
                self.output_manager
                    .write_tool(&tool_id, format!("✗ Error: {}", e));
            }
        }

        // Store tool result
        self.tool_results
            .write()
            .await
            .entry(query_id)
            .or_insert_with(Vec::new)
            .push((tool_id, result));

        // Check if all tools for this query have completed
        let metadata = self.query_states.get_metadata(query_id).await;
        if let Some(meta) = metadata {
            if let QueryState::ExecutingTools { tools_pending, .. } = meta.state {
                let results_count = self
                    .tool_results
                    .read()
                    .await
                    .get(&query_id)
                    .map(|v| v.len())
                    .unwrap_or(0);

                if results_count >= tools_pending {
                    // All tools completed, format results and add to conversation
                    self.finalize_tool_execution(query_id).await?;
                }
            }
        }

        Ok(())
    }

    /// Finalize tool execution (all tools complete, re-invoke Claude)
    async fn finalize_tool_execution(&mut self, query_id: Uuid) -> Result<()> {
        // Get all tool results for this query
        let results = self
            .tool_results
            .write()
            .await
            .remove(&query_id)
            .unwrap_or_default();

        // Format tool results as user message
        let mut tool_result_text = String::new();
        for (tool_id, result) in results {
            match result {
                Ok(content) => {
                    tool_result_text.push_str(&format!(
                        "<tool_result tool_use_id=\"{}\">\n{}\n</tool_result>\n",
                        tool_id, content
                    ));
                }
                Err(e) => {
                    tool_result_text.push_str(&format!(
                        "<tool_result tool_use_id=\"{}\" is_error=\"true\">\n{}\n</tool_result>\n",
                        tool_id, e
                    ));
                }
            }
        }

        // Add tool results to conversation
        self.conversation
            .write()
            .await
            .add_user_message(tool_result_text);

        // Spawn new query task to continue the conversation
        // This will send another request to Claude with the tool results
        self.spawn_query_task(query_id, String::new()).await;

        Ok(())
    }

    /// Handle tool approval request (show dialog, get user response)
    async fn handle_tool_approval_request(
        &mut self,
        _query_id: Uuid,
        tool_use: crate::tools::types::ToolUse,
        response_tx: tokio::sync::oneshot::Sender<crate::cli::repl::ConfirmationResult>,
    ) -> Result<()> {
        use super::events::ConfirmationResult;

        // Create approval dialog
        let tool_name = &tool_use.name;
        let tool_input = serde_json::to_string_pretty(&tool_use.input)
            .unwrap_or_else(|_| format!("{:?}", tool_use.input));

        let options = vec![
            DialogOption::with_description("Allow Once", "Execute this tool once without saving approval"),
            DialogOption::with_description("Allow Exact (Session)", "Allow this exact tool call for this session"),
            DialogOption::with_description("Allow Pattern (Session)", "Allow similar tool calls for this session"),
            DialogOption::with_description("Allow Exact (Persistent)", "Always allow this exact tool call"),
            DialogOption::with_description("Allow Pattern (Persistent)", "Always allow similar tool calls"),
            DialogOption::with_description("Deny", "Do not execute this tool"),
        ];

        let dialog = Dialog::select(
            format!("Tool '{}' requires approval\n\nInput:\n{}", tool_name, tool_input),
            options,
        );

        // Show dialog and get result
        let mut tui = self.tui_renderer.lock().await;
        let dialog_result = tui.show_dialog(dialog)?;
        drop(tui); // Release lock

        // Convert dialog result to ConfirmationResult
        let confirmation = match dialog_result {
            crate::cli::tui::DialogResult::Selected(index) => match index {
                0 => ConfirmationResult::ApproveOnce,
                1 => {
                    let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));
                    ConfirmationResult::ApproveExactSession(signature)
                }
                2 => {
                    // Create pattern from tool use
                    let pattern = ToolPattern {
                        id: uuid::Uuid::new_v4().to_string(),
                        pattern: format!("{}:*", tool_use.name),
                        tool_name: tool_use.name.clone(),
                        description: format!("Auto-generated pattern for {}", tool_use.name),
                        created_at: chrono::Utc::now(),
                        match_count: 0,
                        pattern_type: crate::tools::patterns::PatternType::Wildcard,
                        last_used: None,
                    };
                    ConfirmationResult::ApprovePatternSession(pattern)
                }
                3 => {
                    let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));
                    ConfirmationResult::ApproveExactPersistent(signature)
                }
                4 => {
                    // Create pattern from tool use
                    let pattern = ToolPattern {
                        id: uuid::Uuid::new_v4().to_string(),
                        pattern: format!("{}:*", tool_use.name),
                        tool_name: tool_use.name.clone(),
                        description: format!("Auto-generated pattern for {}", tool_use.name),
                        created_at: chrono::Utc::now(),
                        match_count: 0,
                        pattern_type: crate::tools::patterns::PatternType::Wildcard,
                        last_used: None,
                    };
                    ConfirmationResult::ApprovePatternPersistent(pattern)
                }
                _ => ConfirmationResult::Deny,
            },
            _ => ConfirmationResult::Deny,
        };

        // Send confirmation back to tool execution task
        let _ = response_tx.send(confirmation);

        Ok(())
    }
}
