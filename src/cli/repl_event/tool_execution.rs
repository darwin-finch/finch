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
            let approval_source = tool_executor.lock().await.is_approved(&signature);

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
    use crate::claude::{ContentBlock, Message};
    use crate::tools::implementations::{
        AskUserQuestionTool, EnterPlanModeTool, PresentPlanTool, TodoReadTool, TodoWriteTool,
    };
    use crate::tools::permissions::PermissionManager;
    use crate::tools::registry::{Tool, ToolRegistry};
    use crate::tools::todo::TodoList;
    use std::time::Duration;

    /// Liveness bound only — never a statement about how fast dispatch is.
    /// Exceeding it means the tool task produced no terminal event at all.
    const DISPATCH_LIVENESS_BOUND: Duration = Duration::from_secs(60);

    /// What the boundary did with one dispatched tool call.
    #[derive(Debug)]
    enum DispatchOutcome {
        /// `ReplEvent::ToolApprovalNeeded` was emitted for this tool.
        DemandedApproval(String),
        /// A terminal `ReplEvent::ToolResult` arrived for this tool call.
        Terminal { ok: bool, detail: String },
        /// The event channel closed before either.
        ChannelClosed,
    }

    /// One dispatch case: the tool, the mode it runs under, an input its real
    /// implementation accepts, and a fragment of output only that
    /// implementation produces.
    ///
    /// The fragment is what keeps this test from being vacuous in the one way
    /// that matters for a change about name drift. A renamed or unregistered
    /// tool comes back as a terminal `Err("Tool 'x' not found")`, and a tool
    /// reached with the wrong input comes back as
    /// `Ok("Execution error: Missing required parameter ...")`. Either would
    /// otherwise read as "dispatched without approval" while proving nothing.
    struct DispatchCase {
        tool_name: String,
        mode: ReplMode,
        input: serde_json::Value,
        /// Required: terminal `Ok` whose content contains this.
        expect_ok_containing: &'static str,
    }

    /// The Finch-internal tools that #26 and #426 showed were misclassified.
    ///
    /// Names come from the `Tool` implementations, not from literals repeated
    /// here: the original defect was exactly a literal drifting from the name a
    /// tool registers.
    ///
    /// Each mode/input pair is chosen so the real implementation reaches a
    /// terminal state without touching the workspace: `enter_plan_mode` runs
    /// while already planning, so it returns its idempotent result instead of
    /// creating `$HOME/.finch/plans`; `present_plan` runs outside planning
    /// mode, so it returns early instead of writing a plan file; `todo_write`
    /// is unjournaled here, so it mutates only the in-memory list.
    fn vm_local_dispatch_cases() -> Vec<DispatchCase> {
        let todo_list = Arc::new(RwLock::new(TodoList::default()));
        let todo_write = TodoWriteTool::new(Arc::clone(&todo_list));
        let todo_read = TodoReadTool::new(todo_list);

        vec![
            DispatchCase {
                tool_name: todo_write.name().to_string(),
                mode: ReplMode::Normal,
                input: serde_json::json!({
                    "todos": [{
                        "id": "1",
                        "content": "issue-26 dispatch regression",
                        "status": "pending",
                        "priority": "high",
                    }]
                }),
                expect_ok_containing: "Todo list updated",
            },
            DispatchCase {
                tool_name: todo_read.name().to_string(),
                mode: ReplMode::Normal,
                input: serde_json::json!({}),
                expect_ok_containing: "[]",
            },
            DispatchCase {
                tool_name: EnterPlanModeTool.name().to_string(),
                mode: ReplMode::Planning {
                    task: "issue-26 dispatch regression".to_string(),
                    plan_path: std::env::temp_dir()
                        .join(format!("finch-issue-26-{}-unused-plan.md", Uuid::new_v4())),
                    created_at: chrono::Utc::now(),
                },
                input: serde_json::json!({"reason": "issue-26 dispatch regression"}),
                expect_ok_containing: "Already in planning mode",
            },
            DispatchCase {
                tool_name: PresentPlanTool.name().to_string(),
                mode: ReplMode::Normal,
                input: serde_json::json!({"plan": "issue-26 dispatch regression"}),
                expect_ok_containing: "Not in planning mode",
            },
            DispatchCase {
                tool_name: AskUserQuestionTool.name().to_string(),
                mode: ReplMode::Normal,
                input: serde_json::json!({}),
                // This tool is meant to be intercepted by the event loop, so
                // its executor path deliberately refuses. The executor reports
                // that refusal as `Ok("Execution error: ...")`, and the wording
                // is unique to this implementation — proof the call reached the
                // tool rather than falling out as "not found".
                expect_ok_containing: "intercepted by event loop",
            },
        ]
    }

    /// A `ToolExecutor` holding the real tools, with no cached approvals: the
    /// only thing that can keep a dispatched tool out of the approval dialog is
    /// its classification.
    fn executor_with_real_vm_local_tools() -> ToolExecutor {
        let todo_list = Arc::new(RwLock::new(TodoList::default()));
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(TodoWriteTool::new(Arc::clone(&todo_list))));
        registry.register(Box::new(TodoReadTool::new(todo_list)));
        registry.register(Box::new(EnterPlanModeTool));
        registry.register(Box::new(PresentPlanTool));
        registry.register(Box::new(AskUserQuestionTool));

        let patterns_path =
            std::env::temp_dir().join(format!("finch-issue-26-patterns-{}.json", Uuid::new_v4()));
        ToolExecutor::new(registry, PermissionManager::new(), patterns_path)
            .expect("test tool executor")
    }

    async fn dispatch_once(
        tool_name: &str,
        mode: ReplMode,
        input: serde_json::Value,
    ) -> DispatchOutcome {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let conversation = Arc::new(RwLock::new(ConversationHistory::new()));
        let coordinator = ToolExecutionCoordinator::new(
            event_tx,
            Arc::new(tokio::sync::Mutex::new(executor_with_real_vm_local_tools())),
            Arc::new(OutputManager::new(crate::config::ColorScheme::default())),
            Arc::clone(&conversation),
            Arc::new(RwLock::new(crate::local::LocalGenerator::new())),
            Arc::new(crate::models::TextTokenizer::stub().expect("stub tokenizer")),
            Arc::new(RwLock::new(mode)),
            Arc::new(RwLock::new(None)),
        );

        let query_id = Uuid::new_v4();
        let tool_use = ToolUse::new(tool_name.to_string(), input);
        let assistant = Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: tool_use.id.clone(),
                name: tool_use.name.clone(),
                input: tool_use.input.clone(),
            }],
        );
        let round_token = conversation
            .write()
            .await
            .stage_assistant(query_id, assistant)
            .expect("stage the provider assistant round");

        let work_unit = Arc::new(WorkUnit::new("Channeling"));
        let row_idx = work_unit.add_row(tool_name.to_string());
        let tool_id = tool_use.id.clone();

        coordinator.spawn_tool_execution(
            query_id,
            round_token,
            tool_use,
            Arc::clone(&work_unit),
            row_idx,
            None,
        );

        let observed = tokio::time::timeout(DISPATCH_LIVENESS_BOUND, async {
            loop {
                match event_rx.recv().await {
                    Some(ReplEvent::ToolApprovalNeeded {
                        tool_use: asked, ..
                    }) => return DispatchOutcome::DemandedApproval(asked.name),
                    Some(ReplEvent::ToolResult {
                        tool_id: id,
                        result,
                        ..
                    }) if id == tool_id => {
                        return match result {
                            Ok(content) => DispatchOutcome::Terminal {
                                ok: true,
                                detail: content,
                            },
                            Err(error) => DispatchOutcome::Terminal {
                                ok: false,
                                detail: error.to_string(),
                            },
                        }
                    }
                    Some(_) => continue,
                    None => return DispatchOutcome::ChannelClosed,
                }
            }
        })
        .await;

        match observed {
            Ok(outcome) => outcome,
            Err(_) => panic!(
                "the run hung: dispatching tool {tool_name:?} produced neither \
                 ToolApprovalNeeded nor a terminal ToolResult within the liveness bound of {} s. \
                 This bound is not a performance assertion; exceeding it means the tool task never \
                 reached a terminal state.",
                DISPATCH_LIVENESS_BOUND.as_secs()
            ),
        }
    }

    /// Production-boundary regression for #26 (entering plan mode asked
    /// permission to *reduce* capability) and #426 (writing a session task list
    /// raised an approval dialog nobody intended).
    ///
    /// `spawn_tool_execution` is the real path that turns a classification into
    /// an approval dialog. A misclassified Finch-internal tool emits
    /// `ReplEvent::ToolApprovalNeeded` here and then waits on an answer the
    /// user never expected to give.
    ///
    /// The test asserts two things, not one: no approval event, *and* that the
    /// tool actually ran. Without the second, a renamed or unregistered tool
    /// would return `Tool 'x' not found` and still read as a pass — which in a
    /// change about name drift is the exact failure the test exists to catch.
    #[tokio::test]
    async fn test_vm_local_tools_dispatch_without_an_approval_event() {
        let cases = vm_local_dispatch_cases();
        let total = cases.len();
        let mut violations = Vec::new();

        for case in cases {
            let effect = crate::tools::permissions::legacy_tool_effect(
                &case.tool_name,
                &serde_json::json!({}),
            );
            let tool_name = &case.tool_name;
            let fragment = case.expect_ok_containing;

            match dispatch_once(tool_name, case.mode, case.input).await {
                DispatchOutcome::Terminal { ok, detail } => {
                    if !ok || !detail.contains(fragment) {
                        violations.push(format!(
                            "  tool {tool_name:?} raised no approval but did not run: terminal \
                             result was {} {detail:?}; expected Ok containing {fragment:?}. \
                             A \"not found\" or missing-parameter result here means the call never \
                             reached the tool, so the no-approval assertion proves nothing.",
                            if ok { "Ok" } else { "Err" },
                        ));
                    }
                }
                DispatchOutcome::DemandedApproval(asked) => violations.push(format!(
                    "  tool {tool_name:?} emitted ReplEvent::ToolApprovalNeeded (for {asked:?}); \
                     legacy_tool_effect computed {} (runs_autonomously={})",
                    effect.as_str(),
                    effect.runs_autonomously(),
                )),
                DispatchOutcome::ChannelClosed => violations.push(format!(
                    "  tool {tool_name:?} produced no terminal ToolResult before the event \
                     channel closed; legacy_tool_effect computed {}",
                    effect.as_str(),
                )),
            }
        }

        assert!(
            violations.is_empty(),
            "invariant: a Finch-internal tool must run to a terminal result through \
             ToolExecutionCoordinator::spawn_tool_execution without ever opening an approval \
             dialog — entering plan mode is a capability *reduction*, and a session task list is \
             not a workspace or external effect (#26, #426). {} of {} dispatched tools violated \
             it:\n{}",
            violations.len(),
            total,
            violations.join("\n"),
        );
    }
}
