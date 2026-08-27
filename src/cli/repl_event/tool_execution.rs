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
use crate::cli::conversation::ConversationHistory;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::Tool;
    use async_trait::async_trait;

    struct UnusedTurnControl;
    impl crate::finch_ipc_capnp::brain_turn_control::Server for UnusedTurnControl {}

    struct BlockingReadTool {
        started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        dropped: Arc<std::sync::atomic::AtomicBool>,
        committed: Arc<std::sync::atomic::AtomicBool>,
    }

    struct RecordingWriteTool {
        started: Arc<std::sync::atomic::AtomicBool>,
    }

    struct BlockingWriteTool {
        started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    #[async_trait]
    impl Tool for BlockingWriteTool {
        fn name(&self) -> &str {
            "write"
        }

        fn description(&self) -> &str {
            "unrelated no-timeout holder"
        }

        fn input_schema(&self) -> crate::tools::types::ToolInputSchema {
            crate::tools::types::ToolInputSchema::simple(vec![])
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &crate::tools::types::ToolContext<'_>,
        ) -> anyhow::Result<String> {
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            let release = self.release.lock().unwrap().take().unwrap();
            let _ = release.await;
            Ok("holder released".into())
        }
    }

    #[async_trait]
    impl Tool for RecordingWriteTool {
        fn name(&self) -> &str {
            "write"
        }

        fn description(&self) -> &str {
            "approval cancellation regression tool"
        }

        fn input_schema(&self) -> crate::tools::types::ToolInputSchema {
            crate::tools::types::ToolInputSchema::simple(vec![])
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &crate::tools::types::ToolContext<'_>,
        ) -> anyhow::Result<String> {
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("unexpected execution".into())
        }
    }

    #[async_trait]
    impl Tool for BlockingReadTool {
        fn name(&self) -> &str {
            "read"
        }

        fn description(&self) -> &str {
            "blocking cancellation regression tool"
        }

        fn input_schema(&self) -> crate::tools::types::ToolInputSchema {
            crate::tools::types::ToolInputSchema::simple(vec![])
        }

        async fn execute(
            &self,
            _input: serde_json::Value,
            _context: &crate::tools::types::ToolContext<'_>,
        ) -> anyhow::Result<String> {
            struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for DropFlag {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _drop_flag = DropFlag(Arc::clone(&self.dropped));
            if let Some(started) = self.started.lock().unwrap().take() {
                let _ = started.send(());
            }
            std::future::pending::<()>().await;
            self.committed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok("late effect".into())
        }
    }

    fn write_turn_request(
        mut request: crate::finch_ipc_capnp::brain_turn_request::Builder<'_>,
        run_id: crate::brain::store::RunId,
        request_seq: u64,
        prompt: &str,
    ) {
        request.set_brain("shared");
        request.set_run_id(&run_id.0.to_string());
        request.set_request_seq(request_seq);
        request.set_prompt(prompt);
        request.reborrow().init_context(0);
        crate::ipc::brain_codec::encode_approval_audience(
            request.reborrow().init_approval_audience(),
            &crate::brain::store::BrainApprovalAudience {
                brain_id: crate::brain::store::BrainId(uuid::Uuid::new_v4()),
                brain: "shared".into(),
                attachment_id: crate::brain::store::AttachmentId(uuid::Uuid::new_v4()),
                subject: "runner@box.local".into(),
                role: crate::brain::store::AttachmentRole::Runner,
                environment_generation: 3,
            },
        );
        request.set_control(capnp_rpc::new_client(UnusedTurnControl));
    }

    #[test]
    fn dropped_capnp_turn_cancels_real_tool_before_lane_reuse_and_reconnects() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            let (runner_event_tx, mut runner_event_rx) = tokio::sync::mpsc::unbounded_channel();
            let runner = crate::ipc::client::test_brain_runner_client(runner_event_tx);
            let mut call = runner.run_turn_request();
            write_turn_request(
                call.get().init_request(),
                crate::brain::store::RunId(uuid::Uuid::new_v4()),
                9,
                "run the blocking tool",
            );
            let first_rpc = tokio::task::spawn_local(async move { call.send().promise.await });
            let request = match runner_event_rx.recv().await.unwrap() {
                ReplEvent::NamedBrainTurnRequested(request) => request,
                _ => panic!("expected physical named-Brain Turn request"),
            };

            let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let committed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (holder_started_tx, holder_started_rx) = tokio::sync::oneshot::channel();
            let (holder_release_tx, holder_release_rx) = tokio::sync::oneshot::channel();
            let mut registry = crate::tools::registry::ToolRegistry::new();
            registry.register(Box::new(BlockingReadTool {
                started: std::sync::Mutex::new(Some(started_tx)),
                dropped: Arc::clone(&dropped),
                committed: Arc::clone(&committed),
            }));
            registry.register(Box::new(BlockingWriteTool {
                started: std::sync::Mutex::new(Some(holder_started_tx)),
                release: std::sync::Mutex::new(Some(holder_release_rx)),
            }));
            let permissions = crate::tools::permissions::PermissionManager::new()
                .with_default_rule(crate::tools::permissions::PermissionRule::Allow);
            let patterns = tempfile::tempdir().unwrap();
            let mut executor = crate::tools::executor::ToolExecutor::new(
                registry,
                permissions,
                patterns.path().join("patterns.json"),
            )
            .unwrap();
            let holder_use = crate::tools::types::ToolUse {
                id: "unrelated-holder".into(),
                name: "write".into(),
                input: serde_json::json!({"path": "held", "content": "held"}),
            };
            executor.approve_exact_session(generate_tool_signature(
                &holder_use,
                std::path::Path::new("."),
            ));
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let output = Arc::new(crate::cli::output_manager::OutputManager::default());
            let coordinator = ToolExecutionCoordinator::new(
                event_tx,
                Arc::new(tokio::sync::Mutex::new(executor)),
                Arc::clone(&output),
                Arc::new(RwLock::new(
                    crate::cli::conversation::ConversationHistory::new(),
                )),
                Arc::new(RwLock::new(crate::local::LocalGenerator::new())),
                Arc::new(crate::models::tokenizer::TextTokenizer::stub().unwrap()),
                Arc::new(RwLock::new(crate::cli::ReplMode::Normal)),
                Arc::new(RwLock::new(None)),
            );
            let query_states = Arc::new(super::super::query_state::QueryStateManager::new());
            let holder_query_id = query_states.create_query(Vec::new()).await;
            let holder_metadata = query_states.get_metadata(holder_query_id).await.unwrap();
            let holder_unit = output.start_work_unit("unrelated holder");
            let holder_row = holder_unit.add_row("blocking write");
            coordinator.spawn_tool_execution(
                holder_query_id,
                holder_use,
                holder_unit,
                holder_row,
                holder_metadata.cancellation_token,
                query_states
                    .begin_cancellation_sensitive_work(holder_query_id)
                    .await
                    .unwrap(),
            );
            holder_started_rx.await.unwrap();

            let query_id = query_states.create_query(Vec::new()).await;
            let metadata = query_states.get_metadata(query_id).await.unwrap();
            let query_work = query_states
                .begin_cancellation_sensitive_work(query_id)
                .await
                .unwrap();
            let work_unit = output.start_work_unit("tool cancellation");
            let row = work_unit.add_row("blocking read");
            coordinator.spawn_tool_execution(
                query_id,
                crate::tools::types::ToolUse {
                    id: "blocking-read".into(),
                    name: "read".into(),
                    input: serde_json::json!({}),
                },
                work_unit,
                row,
                metadata.cancellation_token.clone(),
                query_work,
            );
            started_rx.await.unwrap();

            let callback_cancel = request.cancel.clone();
            let cancellation_bridge = {
                let query_states = Arc::clone(&query_states);
                tokio::task::spawn_local(async move {
                    callback_cancel.cancelled().await;
                    let barrier = super::super::event_loop::cancel_query_at_callback_disconnect(
                        query_states.as_ref(),
                        query_id,
                    )
                    .await
                    .expect("admitted turn must retain its exact work fence");
                    barrier.wait().await;
                })
            };
            first_rpc.abort();
            let _ = first_rpc.await;
            tokio::time::timeout(std::time::Duration::from_secs(1), cancellation_bridge)
                .await
                .expect("tool future must be dropped before the lane becomes reusable")
                .unwrap();
            assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
            assert!(!committed.load(std::sync::atomic::Ordering::SeqCst));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), event_rx.recv())
                    .await
                    .is_err()
            );
            assert!(request
                .response_tx
                .send(Err(crate::server::RunnerTurnError {
                    message: "late completion".into(),
                    turn_events: Vec::new(),
                    effect_journal: Vec::new(),
                }))
                .is_err());
            assert!(!holder_release_tx.is_closed());

            let mut replacement_call = runner.run_turn_request();
            write_turn_request(
                replacement_call.get().init_request(),
                crate::brain::store::RunId(uuid::Uuid::new_v4()),
                10,
                "replacement",
            );
            let replacement_rpc =
                tokio::task::spawn_local(async move { replacement_call.send().promise.await });
            let replacement = match runner_event_rx.recv().await.unwrap() {
                ReplEvent::NamedBrainTurnRequested(request) => request,
                _ => panic!("expected replacement named-Brain Turn request"),
            };
            assert!(!replacement.cancel.is_cancelled());
            replacement
                .response_tx
                .send(Err(crate::server::RunnerTurnError {
                    message: "replacement completed".into(),
                    turn_events: Vec::new(),
                    effect_journal: Vec::new(),
                }))
                .unwrap();
            replacement_rpc.await.unwrap().unwrap();
            assert!(!replacement.cancel.is_cancelled());

            holder_release_tx.send(()).unwrap();
            query_states.cancel_query(holder_query_id).await;
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                query_states.wait_for_cancellation_safe(holder_query_id),
            )
            .await
            .expect("unrelated holder must cleanly retire");
        }));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_wins_blocked_post_approval_lock_without_mutation_or_execution() {
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut registry = crate::tools::registry::ToolRegistry::new();
        registry.register(Box::new(RecordingWriteTool {
            started: Arc::clone(&started),
        }));
        let patterns = tempfile::tempdir().unwrap();
        let patterns_path = patterns.path().join("patterns.json");
        let mutation_path = patterns.path().join("must-not-exist");
        let executor = Arc::new(tokio::sync::Mutex::new(
            crate::tools::executor::ToolExecutor::new(
                registry,
                crate::tools::permissions::PermissionManager::new(),
                patterns_path.clone(),
            )
            .unwrap(),
        ));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let output = Arc::new(crate::cli::output_manager::OutputManager::default());
        let coordinator = ToolExecutionCoordinator::new(
            event_tx,
            Arc::clone(&executor),
            Arc::clone(&output),
            Arc::new(RwLock::new(
                crate::cli::conversation::ConversationHistory::new(),
            )),
            Arc::new(RwLock::new(crate::local::LocalGenerator::new())),
            Arc::new(crate::models::tokenizer::TextTokenizer::stub().unwrap()),
            Arc::new(RwLock::new(crate::cli::ReplMode::Normal)),
            Arc::new(RwLock::new(None)),
        );
        let query_states = super::super::query_state::QueryStateManager::new();
        let query_id = query_states.create_query(Vec::new()).await;
        let metadata = query_states.get_metadata(query_id).await.unwrap();
        let tool_use = crate::tools::types::ToolUse {
            id: "approval-race".into(),
            name: "write".into(),
            input: serde_json::json!({
                "path": mutation_path.to_string_lossy(),
                "content": "late"
            }),
        };
        let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));
        let work_unit = output.start_work_unit("approval cancellation");
        let row = work_unit.add_row("blocked write");
        coordinator.spawn_tool_execution(
            query_id,
            tool_use,
            work_unit,
            row,
            metadata.cancellation_token,
            query_states
                .begin_cancellation_sensitive_work(query_id)
                .await
                .unwrap(),
        );
        let approval_tx = match event_rx.recv().await.unwrap() {
            ReplEvent::ToolApprovalNeeded { response_tx, .. } => response_tx,
            _ => panic!("expected approval request"),
        };
        let mut holder = executor.lock().await;
        approval_tx
            .send(ConfirmationResult::ApproveExactPersistent(
                signature.clone(),
            ))
            .unwrap();
        query_states.cancel_query(query_id).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            query_states.wait_for_cancellation_safe(query_id),
        )
        .await
        .expect("cancelled approval lock waiter must release its exact work fence");

        assert!(!started.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!patterns_path.exists());
        assert!(!mutation_path.exists());
        assert!(matches!(
            holder.is_approved(&signature),
            crate::tools::executor::ApprovalSource::NotApproved
        ));
        drop(holder);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), event_rx.recv(),)
                .await
                .is_err()
        );
    }
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
    cancel: tokio_util::sync::CancellationToken,
    // A VM may finish on the blocking pool after its async caller is dropped.
    // Keep the lane fenced until every sink clone owned by that VM is gone.
    _query_work: super::query_state::QueryWorkGuard,
}

impl LiveOutputSink for WorkUnitPresentation {
    fn line(&self, text: String) {
        if self.cancel.is_cancelled() {
            return;
        }
        if self.program {
            self.work_unit.append_response(&text);
        } else {
            self.work_unit.append_row_body_line(self.row_idx, text);
        }
    }

    fn vm_side_effect(&self, effect: crate::vm::VmSideEffect) {
        if self.cancel.is_cancelled() {
            return;
        }
        if let Some(projection) = &self.vm_output {
            projection.project(&effect);
        }
    }

    fn vm_effect_envelope(&self, envelope: crate::runtime::VmEffectEnvelope) {
        if self.cancel.is_cancelled() {
            return;
        }
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

    fn cancellation_token(&self) -> Option<tokio_util::sync::CancellationToken> {
        Some(self.cancel.clone())
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
        tool_use: ToolUse,
        work_unit: Arc<WorkUnit>,
        row_idx: usize,
        cancel: tokio_util::sync::CancellationToken,
        query_work: super::query_state::QueryWorkGuard,
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
            cancel: cancel.clone(),
            _query_work: query_work.clone(),
        });

        tokio::spawn(async move {
            let _query_work = query_work;
            if cancel.is_cancelled() {
                return;
            }
            let mut tool_use = tool_use;
            // Generate tool signature for approval checking
            let signature = generate_tool_signature(&tool_use, std::path::Path::new("."));

            // Check if tool needs approval
            let approval_source = {
                let mut executor = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    executor = tool_executor.lock() => executor,
                };
                if cancel.is_cancelled() {
                    return;
                }
                executor.is_approved(&signature)
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
                let approval = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    approval = response_rx => approval,
                };
                match approval {
                    Ok(confirmation) => {
                        // Process approval result
                        match confirmation {
                            ConfirmationResult::ApproveOnce => {
                                // Approved for this execution only, continue
                            }
                            ConfirmationResult::ApproveExactSession(sig) => {
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancel.is_cancelled() {
                                    return;
                                }
                                executor.approve_exact_session(sig);
                            }
                            ConfirmationResult::ApprovePatternSession(pattern) => {
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancel.is_cancelled() {
                                    return;
                                }
                                executor.approve_pattern_session(pattern);
                            }
                            ConfirmationResult::ApproveExactPersistent(sig) => {
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancel.is_cancelled() {
                                    return;
                                }
                                executor.approve_exact_persistent(sig);
                                if let Err(e) = executor.save_patterns() {
                                    tracing::warn!("Failed to save persistent approval: {}", e);
                                }
                            }
                            ConfirmationResult::ApprovePatternPersistent(pattern) => {
                                let mut executor = tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => return,
                                    executor = tool_executor.lock() => executor,
                                };
                                if cancel.is_cancelled() {
                                    return;
                                }
                                executor.approve_pattern_persistent(pattern);
                                if let Err(e) = executor.save_patterns() {
                                    tracing::warn!("Failed to save persistent pattern: {}", e);
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

            if cancel.is_cancelled() {
                return;
            }

            // Tool approved (or doesn't need approval), execute it
            let conversation_snapshot = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                conversation = conversation.read() => conversation.clone(),
            };
            if cancel.is_cancelled() {
                return;
            }
            let prepared = {
                let executor = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return,
                    executor = tool_executor.lock() => executor,
                };
                if cancel.is_cancelled() {
                    return;
                }
                match executor.prepare_event_loop_execution(&tool_use, poset) {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = event_tx.send(ReplEvent::ToolResult {
                            query_id,
                            tool_id: tool_use.id.clone(),
                            result: Err(error),
                        });
                        return;
                    }
                }
            };
            let timeout_duration = prepared.timeout();
            let execute = prepared.execute(
                &tool_use,
                Some(&conversation_snapshot),
                Arc::clone(&local_generator),
                Arc::clone(&tokenizer),
                Arc::clone(&repl_mode),
                Arc::clone(&plan_content),
                Arc::clone(&live_output),
                &cancel,
            );
            let execution = async {
                match timeout_duration {
                    Some(timeout) => tokio::time::timeout(timeout, execute).await,
                    None => Ok(execute.await),
                }
            };
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                result = execution => result,
            };

            if cancel.is_cancelled() {
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
