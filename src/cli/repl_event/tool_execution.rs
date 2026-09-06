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

use std::sync::Arc;
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

fn finish_cancelled_before_effect(
    event_tx: &mpsc::UnboundedSender<ReplEvent>,
    query_id: Uuid,
    round_token: ToolRoundToken,
    tool_id: &str,
    runner_requires_result: bool,
) {
    if !runner_requires_result {
        return;
    }

    // `effect_audit` is bound only by the named-Brain runner path. That path
    // retains every published tool id until a ToolResult boundary arrives, so
    // it needs one cancellation result to quiesce. The event loop consumes and
    // discards that result after cancellation; ordinary queries are cleaned up
    // directly by CancelQuery and must not receive a late/orphan result.
    let _ = event_tx.send(ReplEvent::ToolResult {
        query_id,
        round_token,
        tool_id: tool_id.to_string(),
        result: Err(anyhow::anyhow!("Tool execution cancelled before dispatch")),
    });
}

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

    /// Deterministic test handshake immediately before the first executor-lock
    /// acquisition. Production builds have no scheduling hook.
    #[cfg(test)]
    before_initial_executor_lock: Option<Arc<tokio::sync::Barrier>>,
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
            #[cfg(test)]
            before_initial_executor_lock: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_before_initial_executor_lock_barrier(
        mut self,
        barrier: Arc<tokio::sync::Barrier>,
    ) -> Self {
        self.before_initial_executor_lock = Some(barrier);
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
        round_token: ToolRoundToken,
        tool_use: ToolUse,
        work_unit: Arc<WorkUnit>,
        row_idx: usize,
        effect_audit: Option<crate::server::RunnerEffectAuditControl>,
        cancellation_token: tokio_util::sync::CancellationToken,
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
        let runner_requires_cancel_result = effect_audit.is_some();
        #[cfg(test)]
        let before_initial_executor_lock = self.before_initial_executor_lock.clone();

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
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval
            #[cfg(test)]
            if let Some(barrier) = before_initial_executor_lock {
                barrier.wait().await;
            }
            let approval_source = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    tracing::debug!(query_id = %query_id, tool = %tool_use.name,
                        "discarding cancelled tool before approval lookup");
                    finish_cancelled_before_effect(
                        &event_tx,
                        query_id,
                        round_token,
                        &tool_use.id,
                        runner_requires_cancel_result,
                    );
                    return;
                }
                mut executor = tool_executor.lock() => executor.is_approved(&signature),
            };

            let is_auto_approved =
                crate::tools::permissions::legacy_tool_effect(&tool_use.name, &tool_use.input)
                    .runs_autonomously();

            // Planning mode is a read-only exploration contract. Even commands
            // that the legacy shell-prefix classifier considers read-only must
            // cross a fresh human confirmation boundary on every invocation.
            let planning_bash = matches!(&*repl_mode.read().await, ReplMode::Planning { .. })
                && matches!(tool_use.name.as_str(), "bash" | "Bash");

            let needs_approval = planning_bash
                || (!is_auto_approved
                    && matches!(
                        approval_source,
                        crate::tools::executor::ApprovalSource::NotApproved
                    ));

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
                    _ = cancellation_token.cancelled() => {
                        tracing::debug!(query_id = %query_id, tool = %tool_use.name,
                            "discarding cancelled tool while awaiting approval");
                        finish_cancelled_before_effect(
                            &event_tx,
                            query_id,
                            round_token,
                            &tool_use.id,
                            runner_requires_cancel_result,
                        );
                        return;
                    }
                    confirmation = response_rx => confirmation,
                };
                match confirmation {
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
            let conversation_snapshot = conversation.read().await.clone();

            // Cancellation must win while an execution waits behind another
            // tool holding the executor. Otherwise a detached capability-
            // reduction call can mutate mode after its query was cancelled.
            let mut executor = tokio::select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    tracing::debug!(query_id = %query_id, tool = %tool_use.name,
                        "discarding cancelled tool before executor dispatch");
                    finish_cancelled_before_effect(
                        &event_tx,
                        query_id,
                        round_token,
                        &tool_use.id,
                        runner_requires_cancel_result,
                    );
                    return;
                }
                executor = tool_executor.lock() => executor,
            };

            // Wire the poset into the executor so tool calls auto-record trace nodes.
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
                effect_audit,
            );
            let result = if crate::tools::permissions::canonical_plan_tool_name(&tool_use.name)
                == "enter_plan_mode"
            {
                // EnterPlanMode is session-local and cancellation-safe: dropping
                // its future before its mode write cannot leave a detached host
                // effect. Do not apply this pattern to physical tools whose
                // subprocess or blocking work may outlive a dropped future.
                match timeout_duration {
                    Some(timeout) => tokio::select! {
                        biased;
                        _ = cancellation_token.cancelled() => {
                            tracing::debug!(query_id = %query_id, tool = %tool_use.name,
                                "discarding cancelled session-local plan transition");
                            finish_cancelled_before_effect(
                                &event_tx,
                                query_id,
                                round_token,
                                &tool_use.id,
                                runner_requires_cancel_result,
                            );
                            return;
                        }
                        result = tokio::time::timeout(timeout, execute) => result,
                    },
                    None => tokio::select! {
                        biased;
                        _ = cancellation_token.cancelled() => {
                            tracing::debug!(query_id = %query_id, tool = %tool_use.name,
                                "discarding cancelled session-local plan transition");
                            finish_cancelled_before_effect(
                                &event_tx,
                                query_id,
                                round_token,
                                &tool_use.id,
                                runner_requires_cancel_result,
                            );
                            return;
                        }
                        result = execute => Ok(result),
                    },
                }
            } else {
                match timeout_duration {
                    Some(timeout) => tokio::time::timeout(timeout, execute).await,
                    None => Ok(execute.await),
                }
            };

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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::permissions::{PermissionManager, PermissionRule, ToolPermissionConfig};
    use crate::tools::registry::{Tool, ToolRegistry};
    use crate::tools::types::{ToolContext, ToolInputSchema};
    use async_trait::async_trait;

    struct CoordinatorFixture {
        coordinator: ToolExecutionCoordinator,
        events: mpsc::UnboundedReceiver<ReplEvent>,
        conversation: Arc<RwLock<ConversationHistory>>,
        executor: Arc<tokio::sync::Mutex<ToolExecutor>>,
        mode: Arc<RwLock<ReplMode>>,
    }

    fn coordinator_fixture(
        registry: ToolRegistry,
        permissions: PermissionManager,
        mode: ReplMode,
        barrier: Option<Arc<tokio::sync::Barrier>>,
    ) -> CoordinatorFixture {
        let executor = Arc::new(tokio::sync::Mutex::new(
            ToolExecutor::new(
                registry,
                permissions,
                std::env::temp_dir()
                    .join(format!("finch-plan-policy-{}.json", uuid::Uuid::new_v4())),
            )
            .expect("create isolated coordinator executor"),
        ));
        let (event_tx, events) = mpsc::unbounded_channel();
        let output = Arc::new(OutputManager::new(crate::config::ColorScheme::default()));
        output.disable_stdout();
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
        let mode = Arc::new(RwLock::new(mode));
        let mut coordinator = ToolExecutionCoordinator::new(
            event_tx,
            Arc::clone(&executor),
            output,
            Arc::clone(&conversation),
            Arc::new(RwLock::new(LocalGenerator::new())),
            Arc::new(TextTokenizer::stub().expect("stub tokenizer")),
            Arc::clone(&mode),
            Arc::new(RwLock::new(None)),
        );
        if let Some(barrier) = barrier {
            coordinator = coordinator.with_before_initial_executor_lock_barrier(barrier);
        }
        CoordinatorFixture {
            coordinator,
            events,
            conversation,
            executor,
            mode,
        }
    }

    async fn stage_tool_round(
        conversation: &Arc<RwLock<ConversationHistory>>,
        query_id: Uuid,
        tool_use: &ToolUse,
    ) -> ToolRoundToken {
        conversation
            .write()
            .await
            .stage_assistant(
                query_id,
                crate::claude::Message {
                    role: "assistant".to_string(),
                    content: vec![crate::claude::ContentBlock::ToolUse {
                        id: tool_use.id.clone(),
                        name: tool_use.name.clone(),
                        input: tool_use.input.clone(),
                    }],
                },
            )
            .expect("stage coordinator tool round")
    }

    struct SentinelBashTool {
        sentinel: std::path::PathBuf,
    }

    #[async_trait]
    impl Tool for SentinelBashTool {
        fn name(&self) -> &str {
            "bash"
        }

        fn description(&self) -> &str {
            "test sentinel for the coordinator's Planning-mode Bash boundary"
        }

        fn input_schema(&self) -> ToolInputSchema {
            ToolInputSchema::simple(vec![("command", "host command")])
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &ToolContext<'_>,
        ) -> anyhow::Result<String> {
            std::fs::write(&self.sentinel, b"executed")?;
            Ok("sentinel executed".to_string())
        }
    }

    #[tokio::test]
    async fn planning_bash_always_requests_confirmation_before_a_host_effect() {
        let temp = tempfile::tempdir().expect("create isolated Bash sentinel directory");
        let commands = ["env sh -c 'touch sentinel'", "find . -delete"];
        for (index, command) in commands.into_iter().enumerate() {
            let sentinel = temp.path().join(format!("sentinel-{index}"));
            let mut registry = ToolRegistry::new();
            registry.register(Box::new(SentinelBashTool {
                sentinel: sentinel.clone(),
            }));
            let mut fixture = coordinator_fixture(
                registry,
                PermissionManager::new().with_default_rule(PermissionRule::Ask),
                ReplMode::Planning {
                    task: "inspect safely".to_string(),
                    plan_path: temp.path().join("plan.md"),
                    created_at: chrono::Utc::now(),
                },
                None,
            );
            let query_id = Uuid::new_v4();
            let tool_use = ToolUse {
                id: format!("planning-bash-{index}"),
                name: "bash".to_string(),
                input: serde_json::json!({"command": command}),
            };
            let round_token = stage_tool_round(&fixture.conversation, query_id, &tool_use).await;
            fixture.coordinator.spawn_tool_execution(
                query_id,
                round_token,
                tool_use,
                Arc::new(WorkUnit::new("Testing")),
                0,
                None,
                tokio_util::sync::CancellationToken::new(),
            );

            let first =
                tokio::time::timeout(std::time::Duration::from_secs(2), fixture.events.recv())
                    .await
                    .expect("Planning Bash reached coordinator boundary")
                    .expect("Planning Bash event channel stayed open");
            match first {
                ReplEvent::ToolApprovalNeeded {
                    tool_use,
                    response_tx,
                    ..
                } => {
                    assert_eq!(
                        tool_use.input["command"], command,
                        "approval must identify the exact Planning Bash command; command={command}"
                    );
                    response_tx
                        .send(ConfirmationResult::Deny)
                        .expect("deny Planning Bash request");
                }
                other => panic!(
                    "Planning Bash bypassed explicit coordinator confirmation; command={command}, first_event={other:?}, sentinel_exists={}",
                    sentinel.exists()
                ),
            }

            let terminal =
                tokio::time::timeout(std::time::Duration::from_secs(2), fixture.events.recv())
                    .await
                    .expect("denied Planning Bash reached terminal result")
                    .expect("Planning Bash result channel stayed open");
            assert!(
                matches!(terminal, ReplEvent::ToolResult { result: Err(ref error), .. }
                    if error.to_string().contains("denied")),
                "denied Planning Bash must emit one denial result; command={command}, event={terminal:?}, sentinel_exists={}",
                sentinel.exists()
            );
            assert!(
                !sentinel.exists(),
                "Planning Bash performed its host effect after denial; command={command}, sentinel={}",
                sentinel.display()
            );
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), fixture.events.recv())
                    .await
                    .is_err(),
                "Planning Bash emitted an orphan event after its denial; command={command}"
            );
        }
    }

    #[tokio::test]
    async fn cancelled_enter_plan_mode_cannot_mutate_after_executor_queue() {
        use crate::tools::implementations::EnterPlanModeTool;

        let temp = tempfile::tempdir().expect("create isolated cancelled-plan directory");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EnterPlanModeTool::with_plans_dir(
            temp.path().join("plans"),
        )));
        let mut fixture = coordinator_fixture(
            registry,
            PermissionManager::new().with_default_rule(PermissionRule::Ask),
            ReplMode::Normal,
            Some(Arc::clone(&barrier)),
        );
        let query_states = crate::cli::repl_event::query_state::QueryStateManager::new();
        let query_id = query_states.create_query(Vec::new()).await;
        let cancellation_token = query_states
            .get_metadata(query_id)
            .await
            .expect("query metadata exists")
            .cancellation_token;
        let tool_use = ToolUse {
            id: "cancelled-enter-plan-mode".to_string(),
            name: "enter_plan_mode".to_string(),
            input: serde_json::json!({"reason": "must never execute after cancellation"}),
        };
        let round_token = stage_tool_round(&fixture.conversation, query_id, &tool_use).await;
        let executor_guard = fixture.executor.lock().await;
        fixture.coordinator.spawn_tool_execution(
            query_id,
            round_token,
            tool_use,
            Arc::new(WorkUnit::new("Testing")),
            0,
            None,
            cancellation_token,
        );
        barrier.wait().await;
        assert!(
            query_states.cancel_query(query_id).await,
            "queued enter_plan_mode query must transition to cancelled"
        );
        drop(executor_guard);

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), fixture.events.recv())
                .await
                .is_err(),
            "cancelled queued enter_plan_mode must quiesce without an orphan result or approval"
        );
        assert!(
            matches!(*fixture.mode.read().await, ReplMode::Normal),
            "cancelled queued enter_plan_mode mutated session mode after cancellation; mode={:?}",
            *fixture.mode.read().await
        );
        assert!(
            !temp.path().join("plans").exists(),
            "cancelled queued enter_plan_mode created plan storage after cancellation"
        );
    }

    #[tokio::test]
    async fn cancelled_runner_enter_plan_mode_emits_one_quiescence_result_without_effect() {
        use crate::tools::implementations::EnterPlanModeTool;

        let temp = tempfile::tempdir().expect("create isolated runner cancellation directory");
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EnterPlanModeTool::with_plans_dir(
            temp.path().join("plans"),
        )));
        let mut fixture = coordinator_fixture(
            registry,
            PermissionManager::new().with_default_rule(PermissionRule::Ask),
            ReplMode::Normal,
            Some(Arc::clone(&barrier)),
        );
        let query_states = crate::cli::repl_event::query_state::QueryStateManager::new();
        let query_id = query_states.create_query(Vec::new()).await;
        let cancellation_token = query_states
            .get_metadata(query_id)
            .await
            .expect("runner query metadata exists")
            .cancellation_token;
        let (audit_tx, _audit_rx) = mpsc::unbounded_channel();
        let effect_audit = crate::server::RunnerEffectAuditControl::new(audit_tx);
        let tool_use = ToolUse {
            id: "cancelled-runner-enter-plan-mode".to_string(),
            name: "enter_plan_mode".to_string(),
            input: serde_json::json!({"reason": "must never execute after runner cancellation"}),
        };
        let round_token = stage_tool_round(&fixture.conversation, query_id, &tool_use).await;
        let executor_guard = fixture.executor.lock().await;
        fixture.coordinator.spawn_tool_execution(
            query_id,
            round_token,
            tool_use,
            Arc::new(WorkUnit::new("Testing")),
            0,
            Some(effect_audit),
            cancellation_token,
        );
        barrier.wait().await;
        assert!(
            query_states.cancel_query(query_id).await,
            "queued named-Brain enter_plan_mode query must transition to cancelled"
        );
        drop(executor_guard);

        let terminal =
            tokio::time::timeout(std::time::Duration::from_secs(2), fixture.events.recv())
                .await
                .expect("cancelled runner tool reached its quiescence boundary")
                .expect("runner tool event channel stayed open");
        assert!(
            matches!(terminal, ReplEvent::ToolResult {
                ref tool_id,
                result: Err(ref error),
                ..
            } if tool_id == "cancelled-runner-enter-plan-mode"
                && error.to_string().contains("cancelled before dispatch")),
            "named-Brain cancellation must emit exactly one consumable ToolResult; event={terminal:?}, mode={:?}, plans_exist={}",
            *fixture.mode.read().await,
            temp.path().join("plans").exists()
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), fixture.events.recv())
                .await
                .is_err(),
            "cancelled named-Brain enter_plan_mode emitted more than one terminal event"
        );
        assert!(
            matches!(*fixture.mode.read().await, ReplMode::Normal),
            "cancelled named-Brain enter_plan_mode mutated session mode; mode={:?}",
            *fixture.mode.read().await
        );
        assert!(
            !temp.path().join("plans").exists(),
            "cancelled named-Brain enter_plan_mode created plan storage"
        );
    }

    #[tokio::test]
    async fn configured_canonical_denials_govern_legacy_tool_dispatch() {
        use crate::tools::implementations::{EnterPlanModeTool, TodoWriteTool};

        let temp = tempfile::tempdir().expect("create isolated alias-denial directory");
        let todo_list = Arc::new(RwLock::new(crate::tools::todo::TodoList::default()));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EnterPlanModeTool::with_plans_dir(
            temp.path().join("plans"),
        )));
        registry.register_alias("EnterPlanMode", "enter_plan_mode");
        registry.register(Box::new(TodoWriteTool::new(Arc::clone(&todo_list))));
        registry.register_alias("TodoWrite", "todo_write");
        let mut permissions = PermissionManager::new().with_default_rule(PermissionRule::Allow);
        for tool_name in ["enter_plan_mode", "todo_write"] {
            permissions.register_tool_config(
                tool_name.to_string(),
                ToolPermissionConfig {
                    enabled: true,
                    rule: PermissionRule::Deny,
                    allowed_patterns: Vec::new(),
                    blocked_patterns: Vec::new(),
                },
            );
        }
        let mut fixture = coordinator_fixture(registry, permissions, ReplMode::Normal, None);

        let cases = [
            ToolUse {
                id: "denied-legacy-enter".to_string(),
                name: "EnterPlanMode".to_string(),
                input: serde_json::json!({"reason": "must remain denied"}),
            },
            ToolUse {
                id: "denied-legacy-todo".to_string(),
                name: "TodoWrite".to_string(),
                input: serde_json::json!({
                    "todos": [{
                        "id": "1",
                        "content": "must not be installed",
                        "status": "pending",
                        "priority": "high"
                    }]
                }),
            },
        ];
        for tool_use in cases {
            let query_id = Uuid::new_v4();
            let round_token = stage_tool_round(&fixture.conversation, query_id, &tool_use).await;
            fixture.coordinator.spawn_tool_execution(
                query_id,
                round_token,
                tool_use.clone(),
                Arc::new(WorkUnit::new("Testing")),
                0,
                None,
                tokio_util::sync::CancellationToken::new(),
            );
            let terminal =
                tokio::time::timeout(std::time::Duration::from_secs(2), fixture.events.recv())
                    .await
                    .expect("configured alias denial reached a terminal result")
                    .expect("configured alias denial event channel stayed open");
            assert!(
                matches!(terminal, ReplEvent::ToolResult { result: Ok(ref content), .. }
                    if content.contains("not allowed")),
                "canonical Deny must govern legacy alias without approval or execution; tool={}, event={terminal:?}, mode={:?}, todo_count={}",
                tool_use.name,
                *fixture.mode.read().await,
                todo_list.read().await.len()
            );
        }
        assert!(
            matches!(*fixture.mode.read().await, ReplMode::Normal),
            "denied EnterPlanMode alias changed mode; mode={:?}",
            *fixture.mode.read().await
        );
        assert_eq!(
            todo_list.read().await.len(),
            0,
            "denied TodoWrite alias changed the todo projection"
        );
        assert!(
            !temp.path().join("plans").exists(),
            "denied EnterPlanMode alias created plan storage"
        );
    }
}
