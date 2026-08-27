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
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot, RwLock};
use uuid::Uuid;

use super::events::ConfirmationResult;
use crate::cli::conversation::{ConversationHistory, ToolRoundToken};
use crate::cli::messages::WorkUnit;
use crate::cli::output_manager::{OutputManager, VmOutputProjection};
use crate::cli::ReplMode;
use crate::local::LocalGenerator;
use crate::models::tokenizer::TextTokenizer;
use crate::tools::executor::{generate_tool_signature, ToolExecutor};
use crate::tools::types::{LiveOutput, LiveOutputSink, ToolUse};

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
    tasks: ToolRoundTasks,
    cancelled_effects: Arc<Mutex<HashMap<Uuid, Vec<crate::server::RunnerEffectRecord>>>>,
}

#[derive(Clone, Default)]
pub(super) struct ToolRoundTasks {
    rounds: Arc<Mutex<HashMap<(Uuid, ToolRoundToken), Arc<ToolRoundState>>>>,
}

struct ToolRoundState {
    lifecycle: Mutex<ToolRoundLifecycle>,
    drained: tokio::sync::Notify,
}

struct ToolRoundLifecycle {
    registration_open: bool,
    active: usize,
}

pub(super) struct ToolRoundPermit {
    state: Arc<ToolRoundState>,
}

impl Drop for ToolRoundPermit {
    fn drop(&mut self) {
        let drained = {
            let mut lifecycle = self
                .state
                .lifecycle
                .lock()
                .expect("tool round lifecycle lock poisoned");
            lifecycle.active = lifecycle
                .active
                .checked_sub(1)
                .expect("tool round permit count underflow");
            lifecycle.active == 0
        };
        if drained {
            self.state.drained.notify_waiters();
        }
    }
}

impl ToolRoundTasks {
    pub(super) fn open_dispatch(
        &self,
        query_id: Uuid,
        round_token: ToolRoundToken,
    ) -> Option<ToolRoundPermit> {
        let mut rounds = self
            .rounds
            .lock()
            .expect("tool task registry lock poisoned");
        if rounds.contains_key(&(query_id, round_token)) {
            return None;
        }
        let state = Arc::new(ToolRoundState {
            lifecycle: Mutex::new(ToolRoundLifecycle {
                registration_open: true,
                active: 1,
            }),
            drained: tokio::sync::Notify::new(),
        });
        rounds.insert((query_id, round_token), Arc::clone(&state));
        Some(ToolRoundPermit { state })
    }

    pub(super) fn register(
        &self,
        query_id: Uuid,
        round_token: ToolRoundToken,
    ) -> Option<ToolRoundPermit> {
        let state = self
            .rounds
            .lock()
            .expect("tool task registry lock poisoned")
            .get(&(query_id, round_token))
            .cloned()?;
        let mut lifecycle = state
            .lifecycle
            .lock()
            .expect("tool round lifecycle lock poisoned");
        if !lifecycle.registration_open {
            return None;
        }
        lifecycle.active += 1;
        drop(lifecycle);
        Some(ToolRoundPermit { state })
    }

    pub(super) async fn close_and_wait(&self, query_id: Uuid, round_token: ToolRoundToken) {
        let state = self
            .rounds
            .lock()
            .expect("tool task registry lock poisoned")
            .get(&(query_id, round_token))
            .cloned();
        let Some(state) = state else { return };
        loop {
            let notified = state.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let drained = {
                let mut lifecycle = state
                    .lifecycle
                    .lock()
                    .expect("tool round lifecycle lock poisoned");
                lifecycle.registration_open = false;
                lifecycle.active == 0
            };
            if drained {
                break;
            }
            notified.await;
        }
        let mut rounds = self
            .rounds
            .lock()
            .expect("tool task registry lock poisoned");
        if rounds
            .get(&(query_id, round_token))
            .is_some_and(|registered| Arc::ptr_eq(registered, &state))
        {
            rounds.remove(&(query_id, round_token));
        }
    }

    #[cfg(test)]
    pub(super) fn contains(&self, query_id: Uuid, round_token: ToolRoundToken) -> bool {
        self.rounds
            .lock()
            .expect("tool task registry lock poisoned")
            .contains_key(&(query_id, round_token))
    }

    #[cfg(test)]
    pub(super) fn registration_is_open(&self, query_id: Uuid, round_token: ToolRoundToken) -> bool {
        let state = self
            .rounds
            .lock()
            .expect("tool task registry lock poisoned")
            .get(&(query_id, round_token))
            .cloned();
        state.is_some_and(|state| {
            state
                .lifecycle
                .lock()
                .expect("tool round lifecycle lock poisoned")
                .registration_open
        })
    }
}

/// One tool's client-side presentation binding. It is intentionally created
/// per invocation, so concurrent VM programs cannot share an ambient output
/// target.
struct WorkUnitPresentation {
    query_id: Uuid,
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
                query_id: Some(self.query_id),
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
            tasks: ToolRoundTasks::default(),
            cancelled_effects: Arc::new(Mutex::new(HashMap::new())),
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
        cancellation_token: tokio_util::sync::CancellationToken,
        tool_use: ToolUse,
        work_unit: Arc<WorkUnit>,
        row_idx: usize,
    ) -> bool {
        let Some(round_permit) = self.tasks.register(query_id, round_token) else {
            return false;
        };
        let event_tx = self.event_tx.clone();
        let tool_executor = Arc::clone(&self.tool_executor);
        let conversation = Arc::clone(&self.conversation);
        let local_generator = Arc::clone(&self.local_generator);
        let tokenizer = Arc::clone(&self.tokenizer);
        let repl_mode = Arc::clone(&self.repl_mode);
        let plan_content = Arc::clone(&self.plan_content);
        let output_manager = Arc::clone(&self.output_manager);
        let poset = self.poset.clone();
        let cancelled_effects = Arc::clone(&self.cancelled_effects);

        // Build a per-tool presentation binding. Ordinary streaming tools append
        // their lines to their row; a typed VM program's portable `say` events
        // append to the owning generation WorkUnit instead. Neither route uses a
        // process-global "current output" target.
        let program = tool_use.name == "submit_program";
        let vm_output = program
            .then(|| VmOutputProjection::new(Arc::clone(&output_manager), Arc::clone(&work_unit)));
        let live_output: LiveOutput = Arc::new(WorkUnitPresentation {
            query_id,
            work_unit: Arc::clone(&work_unit),
            row_idx,
            program,
            vm_output,
            event_tx: event_tx.clone(),
        });

        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _round_permit = round_permit;
            let _ = start_rx.await;
            let mut tool_use = tool_use;
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval
            let approval_source = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => return,
                mut executor = tool_executor.lock() => executor.is_approved(&signature),
            };

            let is_auto_approved =
                crate::tools::permissions::legacy_tool_effect(&tool_use.name, &tool_use.input)
                    .runs_autonomously();

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
                let confirmation = tokio::select! {
                    biased;
                    _ = cancellation_token.cancelled() => return,
                    confirmation = response_rx => confirmation,
                };
                match confirmation {
                    Ok(confirmation) => {
                        if cancellation_token.is_cancelled() {
                            return;
                        }
                        // Process approval result
                        match confirmation {
                            ConfirmationResult::ApproveOnce => {
                                // Approved for this execution only, continue
                            }
                            ConfirmationResult::ApproveExactSession(sig) => {
                                // Save session approval
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancellation_token.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancellation_token.is_cancelled() {
                                    return;
                                }
                                executor.approve_exact_session(sig);
                            }
                            ConfirmationResult::ApprovePatternSession(pattern) => {
                                // Save session pattern approval
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancellation_token.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancellation_token.is_cancelled() {
                                    return;
                                }
                                executor.approve_pattern_session(pattern);
                            }
                            ConfirmationResult::ApproveExactPersistent(sig) => {
                                // Save persistent approval and write to disk immediately
                                {
                                    let mut executor = tokio::select! {
                                        biased;
                                        _ = cancellation_token.cancelled() => return,
                                        executor = tool_executor.lock() => executor,
                                    };
                                    if cancellation_token.is_cancelled() {
                                        return;
                                    }
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
                                    let mut executor = tokio::select! {
                                        biased;
                                        _ = cancellation_token.cancelled() => return,
                                        executor = tool_executor.lock() => executor,
                                    };
                                    if cancellation_token.is_cancelled() {
                                        return;
                                    }
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
                                // Tool denied, send error result
                                let _ = event_tx.send(ReplEvent::ToolResult {
                                    query_id,
                                    round_token,
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
                            round_token,
                            tool_id: tool_use.id.clone(),
                            result: Err(anyhow::anyhow!("Tool approval cancelled")),
                        });
                        return;
                    }
                }
            }

            // Tool approved (or doesn't need approval), execute it
            if cancellation_token.is_cancelled() {
                if let Ok(Ok(tool_result)) = &result {
                    let records =
                        serde_json::from_str::<crate::runtime::outcome::ExecutionOutcome>(
                            &tool_result.content,
                        )
                        .ok()
                        .map(|outcome| super::query_processor::runner_effect_records(&outcome))
                        .unwrap_or_default();
                    if !records.is_empty() {
                        cancelled_effects
                            .lock()
                            .expect("cancelled effect journal lock poisoned")
                            .entry(query_id)
                            .or_default()
                            .extend(records);
                    }
                }
                return;
            }
            let conversation_snapshot = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => return,
                conversation = conversation.read() => conversation.clone(),
            };

            // Wire the poset into the executor so tool calls auto-record trace nodes.
            let mut executor = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => return,
                executor = tool_executor.lock() => executor,
            };
            if cancellation_token.is_cancelled() {
                return;
            }
            executor.poset = poset.clone();

            // Editor-backed proposal tools explicitly suspend on a human review;
            // that wait is not a process timeout. Those adapters enforce their
            // own timeout only after they have an accepted script to execute.
            let timeout_duration = executor.execution_timeout(&tool_use.name);
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
            );
            let result = match timeout_duration {
                Some(timeout) => tokio::time::timeout(timeout, execute).await,
                None => Ok(execute.await),
            };

            // Once execution has begun, cancellation waits for this task at the
            // round boundary instead of dropping the future and assuming its
            // underlying effect stopped. The stage is already fenced, so no
            // result from a cancelled round is published back to the event loop.
            if cancellation_token.is_cancelled() {
                return;
            }

            // Send result back to event loop
            match result {
                Ok(Ok(tool_result)) => {
                    // Tool executed successfully within timeout
                    tracing::info!(
                        "[tool_exec] Tool {} succeeded, sending result ({} chars)",
                        tool_use.name,
                        tool_result.content.len()
                    );

                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        round_token,
                        tool_id: tool_use.id.clone(),
                        result: Ok(tool_result.content),
                    });
                }
                Ok(Err(e)) => {
                    // Tool executed but returned error
                    tracing::warn!("[tool_exec] Tool {} returned error: {}", tool_use.name, e);
                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        round_token,
                        tool_id: tool_use.id.clone(),
                        result: Err(e),
                    });
                }
                Err(_) => {
                    // Timeout elapsed
                    let seconds = timeout_duration
                        .map(|duration| duration.as_secs())
                        .unwrap_or_default();
                    tracing::error!(
                        "[tool_exec] Tool {} timed out after {} seconds",
                        tool_use.name,
                        seconds
                    );
                    let _ = event_tx.send(ReplEvent::ToolResult {
                        query_id,
                        round_token,
                        tool_id: tool_use.id.clone(),
                        result: Err(anyhow::anyhow!(
                            "Tool execution timed out after {} seconds. \
                             Try restarting or check daemon logs for errors.",
                            seconds
                        )),
                    });
                }
            }
        });
        drop(handle);
        let _ = start_tx.send(());
        true
    }

    pub(super) fn open_round_dispatch(
        &self,
        query_id: Uuid,
        round_token: ToolRoundToken,
    ) -> Option<ToolRoundPermit> {
        self.tasks.open_dispatch(query_id, round_token)
    }

    pub(super) fn register_round_work(
        &self,
        query_id: Uuid,
        round_token: ToolRoundToken,
    ) -> Option<ToolRoundPermit> {
        self.tasks.register(query_id, round_token)
    }

    pub async fn close_and_wait_for_round(&self, query_id: Uuid, round_token: ToolRoundToken) {
        self.tasks.close_and_wait(query_id, round_token).await;
    }

    pub(super) fn take_cancelled_effects(
        &self,
        query_id: Uuid,
    ) -> Vec<crate::server::RunnerEffectRecord> {
        self.cancelled_effects
            .lock()
            .expect("cancelled effect journal lock poisoned")
            .remove(&query_id)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::ToolRoundTasks;
    use crate::claude::{ContentBlock, Message};
    use crate::cli::conversation::ConversationHistory;

    fn staged_round() -> (uuid::Uuid, crate::cli::conversation::ToolRoundToken) {
        let query_id = uuid::Uuid::new_v4();
        let mut conversation = ConversationHistory::new();
        let token = conversation
            .stage_assistant(
                query_id,
                Message {
                    role: "assistant".into(),
                    content: vec![ContentBlock::ToolUse {
                        id: "tool".into(),
                        name: "Read".into(),
                        input: serde_json::json!({}),
                    }],
                },
            )
            .unwrap();
        (query_id, token)
    }

    #[tokio::test]
    async fn closing_round_fences_late_registration_and_waits_for_every_permit() {
        let tasks = ToolRoundTasks::default();
        let (query_id, token) = staged_round();
        let dispatch = tasks.open_dispatch(query_id, token).unwrap();
        let worker = tasks.register(query_id, token).unwrap();

        let closing_tasks = tasks.clone();
        let close = tokio::spawn(async move {
            closing_tasks.close_and_wait(query_id, token).await;
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while tasks.registration_is_open(query_id, token) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("round registration did not close");

        assert!(tasks.register(query_id, token).is_none());
        assert!(!close.is_finished());
        drop(worker);
        tokio::task::yield_now().await;
        assert!(!close.is_finished(), "dispatcher still owns the round");
        drop(dispatch);
        close.await.unwrap();
        assert!(!tasks.contains(query_id, token));
    }

    #[tokio::test]
    async fn duplicate_close_after_round_drain_is_idempotent() {
        let tasks = ToolRoundTasks::default();
        let (query_id, token) = staged_round();
        let dispatch = tasks.open_dispatch(query_id, token).unwrap();
        drop(dispatch);

        tasks.close_and_wait(query_id, token).await;
        tasks.close_and_wait(query_id, token).await;
        assert!(!tasks.contains(query_id, token));
    }
}
