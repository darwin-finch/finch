//! Concurrent, approval-gated tool execution.
//!
//! `ToolExecutionCoordinator` spawns a Tokio task per tool call so multiple
//! tools can run in parallel without blocking the event loop.  Each task:
//!
//! 1. Checks whether the tool needs user approval (via `ToolExecutor::is_approved`).
//! 2. If needed, sends a `ReplEvent::ToolApprovalNeeded` and waits on a oneshot
//!    channel — only *this* task blocks; other tool tasks proceed independently.
//! 3. Executes the tool (with a 30-second timeout) and sends the result back as
//!    `ReplEvent::ToolResult`.

use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

use super::events::ConfirmationResult;
use crate::cli::conversation::ConversationHistory;
use crate::cli::messages::WorkUnit;
use crate::cli::ReplMode;
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::tools::executor::{generate_tool_signature, ToolExecutor};
use crate::tools::types::ToolUse;

use super::events::ReplEvent;

/// Coordinates concurrent tool execution for the event loop
#[derive(Clone)]
pub struct ToolExecutionCoordinator {
    /// Channel to send events back to main loop
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Tool executor (shared, thread-safe)
    tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,

    /// Conversation history (for tools that need context)
    conversation: Arc<RwLock<ConversationHistory>>,

    /// Local generator (for training tools)
    local_generator: Arc<RwLock<LocalGenerator>>,

    /// Tokenizer (for training tools)
    tokenizer: Arc<TextTokenizer>,

    /// REPL mode (for plan mode state)
    repl_mode: Arc<RwLock<ReplMode>>,

    /// Plan content storage
    plan_content: Arc<RwLock<Option<String>>>,

    /// Co-Forth shared stack (AI can push items here via the Push tool)
    stack: Option<Arc<tokio::sync::Mutex<Vec<String>>>>,

    /// Co-Forth poset — each tool call auto-pushes a trace node here.
    poset: Option<Arc<tokio::sync::Mutex<crate::poset::Poset>>>,
}

impl ToolExecutionCoordinator {
    /// Create a new tool execution coordinator
    pub fn new(
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
        conversation: Arc<RwLock<ConversationHistory>>,
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
        repl_mode: Arc<RwLock<ReplMode>>,
        plan_content: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            event_tx,
            tool_executor,
            conversation,
            local_generator,
            tokenizer,
            repl_mode,
            plan_content,
            stack: None,
            poset: None,
        }
    }

    /// Wire the Co-Forth shared stack so the Push tool can write to it.
    pub fn with_stack(mut self, stack: Arc<tokio::sync::Mutex<Vec<String>>>) -> Self {
        self.stack = Some(stack);
        self
    }

    /// Wire the Co-Forth poset so every tool call auto-records a trace node.
    pub fn with_poset(mut self, poset: Arc<tokio::sync::Mutex<crate::poset::Poset>>) -> Self {
        self.poset = Some(poset);
        self
    }

    /// Get access to the tool executor (for MCP commands and other management)
    pub fn tool_executor(&self) -> &Arc<tokio::sync::Mutex<ToolExecutor>> {
        &self.tool_executor
    }

    /// Spawn a task to execute a tool (concurrent, non-blocking)
    ///
    /// This spawns a background task that:
    /// 1. Checks if tool needs approval
    /// 2. If needed, requests approval via event (blocks only this task)
    /// 3. Executes the tool (with live-output streaming for bash)
    /// 4. Sends result back via event channel
    ///
    /// `work_unit` + `row_idx` are used to stream live bash output lines into the
    /// WorkUnit row while the command runs, creating the scrolling preview in the
    /// live area.
    pub fn spawn_tool_execution(
        &self,
        query_id: Uuid,
        tool_use: ToolUse,
        work_unit: Arc<WorkUnit>,
        row_idx: usize,
    ) {
        let event_tx = self.event_tx.clone();
        let tool_executor = Arc::clone(&self.tool_executor);
        let conversation = Arc::clone(&self.conversation);
        let local_generator = Arc::clone(&self.local_generator);
        let tokenizer = Arc::clone(&self.tokenizer);
        let repl_mode = Arc::clone(&self.repl_mode);
        let plan_content = Arc::clone(&self.plan_content);
        let stack = self.stack.clone();
        let poset = self.poset.clone();

        // Build a live-output callback that streams stdout lines into the WorkUnit row.
        // The format() method shows the last 3 body_lines for Running rows, so each
        // new line automatically becomes visible on the next render tick (~100ms).
        let live_output: Arc<dyn Fn(String) + Send + Sync> = {
            let wu = Arc::clone(&work_unit);
            Arc::new(move |line: String| {
                wu.append_row_body_line(row_idx, line);
            })
        };

        tokio::spawn(async move {
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval
            let approval_source = tool_executor.lock().await.is_approved(&signature);

            // Auto-approve certain non-destructive operations
            let is_auto_approved = {
                let tool_name = tool_use.name.as_str();

                // Always auto-approve EnterPlanMode (non-destructive mode change)
                // Always auto-approve TodoWrite/TodoRead (in-memory only, no side effects)
                if tool_name == "EnterPlanMode"
                    || tool_name == "enter_plan_mode"
                    || tool_name == "TodoWrite"
                    || tool_name == "TodoRead"
                {
                    true
                } else {
                    // Auto-approve read-only tools and user interaction tools when in plan mode
                    let current_mode = repl_mode.read().await;
                    let is_plan_mode =
                        matches!(*current_mode, crate::cli::ReplMode::Planning { .. });
                    let is_readonly_tool = matches!(
                        tool_name,
                        "read"
                            | "Read"
                            | "glob"
                            | "Glob"
                            | "grep"
                            | "Grep"
                            | "web_fetch"
                            | "WebFetch"
                            | "AskUserQuestion"
                            | "ask_user_question"
                    );

                    is_plan_mode && is_readonly_tool
                }
            };

            let needs_approval = !is_auto_approved
                && matches!(
                    approval_source,
                    crate::tools::executor::ApprovalSource::NotApproved
                );

            if needs_approval {
                // Request approval from user (non-blocking for other queries)
                let (response_tx, response_rx) = oneshot::channel();

                // Send approval request event
                if event_tx
                    .send(ReplEvent::ToolApprovalNeeded {
                        query_id,
                        tool_use: tool_use.clone(),
                        response_tx,
                    })
                    .is_err()
                {
                    // Event channel closed, cannot continue
                    return;
                }

                // Wait for approval response (blocks only THIS task)
                match response_rx.await {
                    Ok(confirmation) => {
                        // Process approval result
                        match confirmation {
                            ConfirmationResult::ApproveOnce => {
                                // Approved for this execution only, continue
                            }
                            ConfirmationResult::ApproveExactSession(sig) => {
                                // Save session approval
                                tool_executor.lock().await.approve_exact_session(sig);
                            }
                            ConfirmationResult::ApprovePatternSession(pattern) => {
                                // Save session pattern approval
                                tool_executor.lock().await.approve_pattern_session(pattern);
                            }
                            ConfirmationResult::ApproveExactPersistent(sig) => {
                                // Save persistent approval and write to disk immediately
                                {
                                    let mut executor = tool_executor.lock().await;
                                    executor.approve_exact_persistent(sig);
                                    if let Err(e) = executor.save_patterns() {
                                        tracing::warn!("Failed to save persistent approval: {}", e);
                                        // Continue anyway - approval is in memory
                                    }
                                }
                            }
                            ConfirmationResult::ApprovePatternPersistent(pattern) => {
                                // Save persistent pattern approval and write to disk immediately
                                {
                                    let mut executor = tool_executor.lock().await;
                                    executor.approve_pattern_persistent(pattern);
                                    if let Err(e) = executor.save_patterns() {
                                        tracing::warn!("Failed to save persistent pattern: {}", e);
                                        // Continue anyway - pattern is in memory
                                    }
                                }
                            }
                            ConfirmationResult::Deny => {
                                // Tool denied, send error result
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    tool_id: tool_use.id.clone(),
                                    result: Err(anyhow::anyhow!("Tool execution denied by user")),
                                });
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        // Approval channel closed (user cancelled?)
                        let _ = event_tx.send(ReplEvent::ToolResult {
                            query_id,
                            tool_id: tool_use.id.clone(),
                            result: Err(anyhow::anyhow!("Tool approval cancelled")),
                        });
                        return;
                    }
                }
            }

            // Tool approved (or doesn't need approval), execute it
            let conversation_snapshot = conversation.read().await.clone();

            // Wire the poset into the executor so tool calls auto-record trace nodes.
            tool_executor.lock().await.poset = poset.clone();

            // Execute with timeout to prevent system freezing (especially for CPU-heavy operations)
            let timeout_duration = std::time::Duration::from_secs(30);
            let result = tokio::time::timeout(
                timeout_duration,
                tool_executor
                    .lock()
                    .await
                    .execute_tool::<fn() -> anyhow::Result<()>>(
                        &tool_use,
                        Some(&conversation_snapshot),
                        None, // save_fn (not needed in event loop)
                        None, // router (for training)
                        Some(Arc::clone(&local_generator)),
                        Some(Arc::clone(&tokenizer)),
                        Some(Arc::clone(&repl_mode)),
                        Some(Arc::clone(&plan_content)),
                        Some(Arc::clone(&live_output)),
                        stack.clone(), // Co-Forth shared stack
                    ),
            )
            .await;

            // Send result back to event loop
            match result {
                Ok(Ok(tool_result)) => {
                    // Tool executed successfully within timeout
                    tracing::info!(
                        "[tool_exec] Tool {} succeeded, sending result ({} chars)",
                        tool_use.name,
                        tool_result.content.len()
                    );

                    // Push tool result onto the Co-Forth stack so the user can
                    // see what the AI observed before deciding to /run.
                    // Skip internal tools (Push, TodoWrite, etc.) — those manage
                    // their own stack interaction.
                    let skip_stack = matches!(
                        tool_use.name.as_str(),
                        "Push" | "TodoWrite" | "TodoRead" | "EnterPlanMode" | "enter_plan_mode"
                    );
                    if !skip_stack {
                        if let Some(ref s) = stack {
                            let frame = format!(
                                "[{}]\n{}",
                                tool_use.name,
                                tool_result.content.trim()
                            );
                            s.lock().await.push(frame);
                        }
                    }

                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        tool_id: tool_use.id.clone(),
                        result: Ok(tool_result.content),
                    });
                }
                Ok(Err(e)) => {
                    // Tool executed but returned error
                    tracing::warn!("[tool_exec] Tool {} returned error: {}", tool_use.name, e);
                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        tool_id: tool_use.id.clone(),
                        result: Err(e),
                    });
                }
                Err(_) => {
                    // Timeout elapsed
                    tracing::error!(
                        "[tool_exec] Tool {} timed out after {} seconds",
                        tool_use.name,
                        timeout_duration.as_secs()
                    );
                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        tool_id: tool_use.id.clone(),
                        result: Err(anyhow::anyhow!(
                            "Tool execution timed out after {} seconds. \
                             Try restarting or check daemon logs for errors.",
                            timeout_duration.as_secs()
                        )),
                    });
                }
            }
        });
    }
}
