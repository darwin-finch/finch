//! Concurrent, approval-gated tool execution.
//!
//! `ToolExecutionCoordinator` spawns a Tokio task per tool call so multiple
//! tools can run in parallel without blocking the event loop.  Each task:
//!
//! 1. Checks whether the tool needs user approval (via `ToolExecutor::is_approved`).
//! 2. If needed, sends a `ReplEvent::ToolApprovalNeeded` and waits on a oneshot
//!    channel — only *this* task blocks; other tool tasks proceed independently.
//! 3. Executes the tool (with a bounded subprocess timeout, but never timing a
//!    human editor review) and sends the result back as `ReplEvent::ToolResult`.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use uuid::Uuid;

use super::events::ConfirmationResult;
use crate::cli::conversation::{ConversationHistory, ToolRoundToken};
use crate::cli::messages::WorkUnit;
use crate::cli::output_manager::{OutputManager, VmOutputProjection};
use crate::cli::ReplMode;
use crate::local::LocalGenerator;
use crate::models::TextTokenizer;
use crate::tools::{
    generate_tool_signature, ToolExecutor, ToolLoop, ToolLoopResult, ToolLoopTerminal,
};
use crate::tools::{LiveOutput, LiveOutputSink, ToolUse};

use super::events::ReplEvent;

/// Coordinates concurrent tool execution for the event loop
#[derive(Clone)]
pub struct ToolExecutionCoordinator {
    /// Channel to send events back to main loop
    event_tx: mpsc::UnboundedSender<ReplEvent>,

    /// Tool executor (shared, thread-safe)
    tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,

    /// Reactive scrollback host for portable typed VM effects.
    output_manager: Arc<OutputManager>,

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

    /// Co-Forth poset — each tool call auto-pushes a trace node here.
    poset: Option<Arc<tokio::sync::Mutex<crate::poset::Poset>>>,

    /// Per-query ToolLoop plus terminals recorded before attach.
    /// Cancel/timeout/disconnect that wins the map lock first must still
    /// terminalize the loop that is attached afterwards.
    tool_loops: Arc<Mutex<ToolLoopTable>>,
    #[cfg(test)]
    wait_before_attach: Arc<Mutex<Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>>>,
}

struct ToolLoopTable {
    attached: HashMap<Uuid, Arc<Mutex<ToolLoop>>>,
    pending_terminals: HashMap<Uuid, ToolLoopTerminal>,
}

/// One tool's client-side presentation binding. It is intentionally created
/// per invocation, so concurrent VM programs cannot share an ambient output
/// target.
struct WorkUnitPresentation {
    work_unit: Arc<WorkUnit>,
    row_idx: usize,
    program: bool,
    vm_output: Option<VmOutputProjection>,
    event_tx: mpsc::UnboundedSender<ReplEvent>,
}

impl LiveOutputSink for WorkUnitPresentation {
    fn line(&self, text: String) {
        if self.program {
            self.work_unit.append_response(&text);
        } else {
            self.work_unit.append_row_body_line(self.row_idx, text);
        }
    }

    fn vm_side_effect(&self, effect: crate::vm::VmSideEffect) {
        if let Some(projection) = &self.vm_output {
            projection.project(&effect);
        }
    }

    fn vm_effect_envelope(&self, envelope: crate::runtime::VmEffectEnvelope) {
        if let Some(projection) = &self.vm_output {
            // Program tools execute away from the terminal task. Retain the
            // typed `(execution_id, sequence)` envelope and let the REPL
            // event loop own the corresponding WorkUnit mutation.
            let _ = self.event_tx.send(ReplEvent::VmEffect {
                projection: projection.clone(),
                envelope,
            });
        } else {
            self.vm_side_effect(envelope.effect);
        }
    }

    fn defer_program_effects(&self) -> bool {
        self.program
    }
}

impl ToolExecutionCoordinator {
    /// Create a new tool execution coordinator
    pub fn new(
        event_tx: mpsc::UnboundedSender<ReplEvent>,
        tool_executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
        output_manager: Arc<OutputManager>,
        conversation: Arc<RwLock<ConversationHistory>>,
        local_generator: Arc<RwLock<LocalGenerator>>,
        tokenizer: Arc<TextTokenizer>,
        repl_mode: Arc<RwLock<ReplMode>>,
        plan_content: Arc<RwLock<Option<String>>>,
    ) -> Self {
        Self {
            event_tx,
            tool_executor,
            output_manager,
            conversation,
            local_generator,
            tokenizer,
            repl_mode,
            plan_content,
            poset: None,
            tool_loops: Arc::new(Mutex::new(ToolLoopTable {
                attached: HashMap::new(),
                pending_terminals: HashMap::new(),
            })),
            #[cfg(test)]
            wait_before_attach: Arc::new(Mutex::new(None)),
        }
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

    /// Attach the event-loop-owned ToolLoop for this query's tool round.
    ///
    /// A cancel/timeout/disconnect that arrived before this attach is applied
    /// to the loop before it becomes visible to spawn, so terminal always wins.
    pub async fn attach_loop(&self, query_id: Uuid, tool_loop: Arc<Mutex<ToolLoop>>) {
        #[cfg(test)]
        {
            let hook = self.wait_before_attach.lock().await.take();
            if let Some((waiting, resume)) = hook {
                waiting.notify_one();
                resume.notified().await;
            }
        }
        let pending = {
            let mut table = self.tool_loops.lock().await;
            let pending = table.pending_terminals.remove(&query_id);
            table.attached.insert(query_id, Arc::clone(&tool_loop));
            pending
        };
        if let Some(terminal) = pending {
            tool_loop.lock().await.terminalize(terminal);
        }
    }

    /// End the round. Further admits and result appends fail closed.
    ///
    /// When no loop is attached yet, the terminal is recorded so a later
    /// `attach_loop` cannot revive execution.
    pub async fn terminalize(&self, query_id: Uuid, terminal: ToolLoopTerminal) -> bool {
        let attached = {
            let mut table = self.tool_loops.lock().await;
            if let Some(tool_loop) = table.attached.get(&query_id).cloned() {
                tool_loop
            } else if table.pending_terminals.contains_key(&query_id) {
                return false;
            } else {
                table.pending_terminals.insert(query_id, terminal);
                return true;
            }
        };
        let applied = attached.lock().await.terminalize(terminal);
        applied
    }

    /// Drop the round after the query has fully left the tool path.
    pub async fn forget_loop(&self, query_id: Uuid) {
        let mut table = self.tool_loops.lock().await;
        table.attached.remove(&query_id);
        table.pending_terminals.remove(&query_id);
    }

    /// Park `attach_loop` until the test resumes, so cancel can win the race.
    #[cfg(test)]
    pub(crate) async fn arm_wait_before_attach(
        &self,
        waiting: Arc<tokio::sync::Notify>,
        resume: Arc<tokio::sync::Notify>,
    ) {
        *self.wait_before_attach.lock().await = Some((waiting, resume));
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
        round_token: ToolRoundToken,
        tool_use: ToolUse,
        work_unit: Arc<WorkUnit>,
        row_idx: usize,
        effect_audit: Option<crate::server::RunnerEffectAuditControl>,
    ) {
        let event_tx = self.event_tx.clone();
        let tool_executor = Arc::clone(&self.tool_executor);
        let conversation = Arc::clone(&self.conversation);
        let local_generator = Arc::clone(&self.local_generator);
        let tokenizer = Arc::clone(&self.tokenizer);
        let repl_mode = Arc::clone(&self.repl_mode);
        let plan_content = Arc::clone(&self.plan_content);
        let output_manager = Arc::clone(&self.output_manager);
        let poset = self.poset.clone();
        let tool_loops = Arc::clone(&self.tool_loops);

        // Build a per-tool presentation binding. Ordinary streaming tools append
        // their lines to their row; a typed VM program's portable `say` events
        // append to the owning generation WorkUnit instead. Neither route uses a
        // process-global "current output" target.
        let program = tool_use.name == "submit_program";
        let vm_output = program
            .then(|| VmOutputProjection::new(Arc::clone(&output_manager), Arc::clone(&work_unit)));
        let live_output: LiveOutput = Arc::new(WorkUnitPresentation {
            work_unit: Arc::clone(&work_unit),
            row_idx,
            program,
            vm_output,
            event_tx: event_tx.clone(),
        });

        tokio::spawn(async move {
            let mut tool_use = tool_use;
            let tool_loop = {
                let table = tool_loops.lock().await;
                if table.pending_terminals.contains_key(&query_id) {
                    return;
                }
                table.attached.get(&query_id).cloned()
            };
            if let Some(tool_loop) = &tool_loop {
                if tool_loop
                    .lock()
                    .await
                    .admit_execution(&tool_use.id)
                    .is_err()
                {
                    return;
                }
            }
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval
            let approval_source = tool_executor.lock().await.is_approved(&signature);

            // Declared authority of the tool (alias-resolved; unknown names
            // classify as Unclassified and keep requiring approval). bash
            // declares its worst case; the read-only refinement is applied
            // here, at the approval site that consumes the effect.
            let declared_effect = tool_executor
                .lock()
                .await
                .registry()
                .declared_effect(&tool_use.name);
            let is_auto_approved = crate::tools::refined_effect_for_approval(
                declared_effect,
                &tool_use.name,
                &tool_use.input,
            )
            .runs_autonomously();

            let needs_approval = !is_auto_approved
                && matches!(approval_source, crate::tools::ApprovalSource::NotApproved);

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
                            ConfirmationResult::ApproveWithInput(new_input) => {
                                // Approve with user-edited input (e.g. from $EDITOR diff review)
                                tool_use.input = new_input;
                            }
                            ConfirmationResult::Deny => {
                                publish_tool_result(
                                    &event_tx,
                                    &tool_loop,
                                    query_id,
                                    round_token,
                                    tool_use.id.clone(),
                                    Err(anyhow::anyhow!("Tool execution denied by user")),
                                )
                                .await;
                                return;
                            }
                        }
                    }
                    Err(_) => {
                        publish_tool_result(
                            &event_tx,
                            &tool_loop,
                            query_id,
                            round_token,
                            tool_use.id.clone(),
                            Err(anyhow::anyhow!("Tool approval cancelled")),
                        )
                        .await;
                        return;
                    }
                }
            }

            // Tool approved (or doesn't need approval), execute it
            let conversation_snapshot = conversation.read().await.clone();

            // Wire the poset into the executor so tool calls auto-record trace nodes.
            tool_executor.lock().await.poset = poset.clone();

            // Editor-backed proposal tools explicitly suspend on a human review;
            // that wait is not a process timeout. Those adapters enforce their
            // own timeout only after they have an accepted script to execute.
            let timeout_duration = tool_executor.lock().await.execution_timeout(&tool_use.name);
            let executor = tool_executor.lock().await;
            let execute = executor.execute_tool::<fn() -> anyhow::Result<()>>(
                &tool_use,
                Some(&conversation_snapshot),
                None, // save_fn (not needed in event loop)
                None, // router (for training)
                Some(Arc::clone(&local_generator)),
                Some(Arc::clone(&tokenizer)),
                Some(Arc::clone(&repl_mode)),
                Some(Arc::clone(&plan_content)),
                Some(Arc::clone(&live_output)),
                effect_audit,
            );
            let result = match timeout_duration {
                Some(timeout) => tokio::time::timeout(timeout, execute).await,
                None => Ok(execute.await),
            };

            // Send result back to event loop
            match result {
                Ok(Ok(tool_result)) => {
                    tracing::info!(
                        "[tool_exec] Tool {} finished, sending result ({} chars, is_error={})",
                        tool_use.name,
                        tool_result.content.len(),
                        tool_result.is_error
                    );
                    let published = if tool_result.is_error {
                        Err(anyhow::anyhow!("{}", tool_result.content))
                    } else {
                        Ok(tool_result.content)
                    };
                    publish_tool_result(
                        &event_tx,
                        &tool_loop,
                        query_id,
                        round_token,
                        tool_use.id.clone(),
                        published,
                    )
                    .await;
                }
                Ok(Err(e)) => {
                    tracing::warn!("[tool_exec] Tool {} returned error: {}", tool_use.name, e);
                    publish_tool_result(
                        &event_tx,
                        &tool_loop,
                        query_id,
                        round_token,
                        tool_use.id.clone(),
                        Err(e),
                    )
                    .await;
                }
                Err(_) => {
                    let seconds = timeout_duration
                        .map(|duration| duration.as_secs())
                        .unwrap_or_default();
                    tracing::error!(
                        "[tool_exec] Tool {} timed out after {} seconds",
                        tool_use.name,
                        seconds
                    );
                    publish_tool_result(
                        &event_tx,
                        &tool_loop,
                        query_id,
                        round_token,
                        tool_use.id.clone(),
                        Err(anyhow::anyhow!(
                            "Tool execution timed out after {} seconds. \
                             Try restarting or check daemon logs for errors.",
                            seconds
                        )),
                    )
                    .await;
                }
            }
        });
    }
}

async fn publish_tool_result(
    event_tx: &mpsc::UnboundedSender<ReplEvent>,
    tool_loop: &Option<Arc<Mutex<ToolLoop>>>,
    query_id: Uuid,
    round_token: ToolRoundToken,
    tool_id: String,
    result: anyhow::Result<String>,
) {
    let mapped = match &result {
        Ok(content) => ToolLoopResult::success(&tool_id, content.clone()),
        Err(error) => ToolLoopResult::error(&tool_id, error.to_string()),
    };
    if let Some(tool_loop) = tool_loop {
        if tool_loop.lock().await.append_result(mapped).is_none() {
            return;
        }
    }
    let _ = event_tx.send(ReplEvent::ToolResult {
        query_id,
        round_token,
        tool_id,
        result,
    });
}
