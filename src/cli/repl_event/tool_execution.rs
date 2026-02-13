// Tool execution coordinator for concurrent tool execution in event loop

use anyhow::{Context, Result};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

use crate::cli::conversation::ConversationHistory;
use crate::cli::ReplMode;
use super::events::ConfirmationResult;
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::tools::executor::{generate_tool_signature, ApprovalSource, ToolExecutor, ToolSignature};
use crate::tools::patterns::ToolPattern;
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
        }
    }

    /// Spawn a task to execute a tool (concurrent, non-blocking)
    ///
    /// This spawns a background task that:
    /// 1. Checks if tool needs approval
    /// 2. If needed, requests approval via event (blocks only this task)
    /// 3. Executes the tool
    /// 4. Sends result back via event channel
    pub fn spawn_tool_execution(&self, query_id: Uuid, tool_use: ToolUse) {
        let event_tx = self.event_tx.clone();
        let tool_executor = Arc::clone(&self.tool_executor);
        let conversation = Arc::clone(&self.conversation);
        let local_generator = Arc::clone(&self.local_generator);
        let tokenizer = Arc::clone(&self.tokenizer);
        let repl_mode = Arc::clone(&self.repl_mode);
        let plan_content = Arc::clone(&self.plan_content);

        tokio::spawn(async move {
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval (is_approved takes &mut self, so we can't call it from Arc)
            // We'll just always show the approval dialog for now in event loop mode
            let needs_approval = true; // TODO: Add is_approved method that doesn't require &mut

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
                                // Save session approval (requires &mut, can't do from Arc)
                                // TODO: Make approval methods thread-safe
                                // For now, approvals are temporary per-execution only
                                let _ = sig; // Suppress warning
                            }
                            ConfirmationResult::ApprovePatternSession(pattern) => {
                                // Save session pattern approval (requires &mut, can't do from Arc)
                                // TODO: Make approval methods thread-safe
                                // For now, approvals are temporary per-execution only
                                let _ = pattern; // Suppress warning
                                /*
                                if let Err(e) = tool_executor.approve_pattern_session(pattern) {
                                    let _ = event_tx.send(ReplEvent::ToolResult {
                                        query_id,
                                        tool_id: tool_use.id.clone(),
                                        result: Err(anyhow::anyhow!(
                                            "Failed to save pattern approval: {}",
                                            e
                                        )),
                                    });
                                    return;
                                }
                                */
                            }
                            ConfirmationResult::ApproveExactPersistent(sig) => {
                                // Save persistent approval (requires &mut, can't do from Arc)
                                // TODO: Make approval methods thread-safe
                                // For now, approvals are temporary per-execution only
                                let _ = sig; // Suppress warning
                            }
                            ConfirmationResult::ApprovePatternPersistent(pattern) => {
                                // Save persistent pattern approval (requires &mut, can't do from Arc)
                                // TODO: Make approval methods thread-safe
                                // For now, approvals are temporary per-execution only
                                let _ = pattern; // Suppress warning
                                /*
                                if let Err(e) = tool_executor.approve_pattern_persistent(pattern) {
                                    let _ = event_tx.send(ReplEvent::ToolResult {
                                        query_id,
                                        tool_id: tool_use.id.clone(),
                                        result: Err(anyhow::anyhow!(
                                            "Failed to save pattern approval: {}",
                                            e
                                        )),
                                    });
                                    return;
                                }
                                */
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
                    )
            )
            .await;

            // Send result back to event loop
            match result {
                Ok(Ok(tool_result)) => {
                    // Tool executed successfully within timeout
                    tracing::info!("[tool_exec] Tool {} succeeded, sending result ({} chars)",
                        tool_use.name, tool_result.content.len());
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
                    tracing::error!("[tool_exec] Tool {} timed out after {} seconds",
                        tool_use.name, timeout_duration.as_secs());
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
