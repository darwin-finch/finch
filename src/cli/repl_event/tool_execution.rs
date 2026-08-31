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
        self.work_unit.append_row_body_line(self.row_idx, text);
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
        // their lines to their row; a typed VM tool's portable `say` events
        // remain part of that genuine tool activity. Neither route uses a
        // process-global "current output" target.
        let program = tool_use.name == "submit_program";
        let vm_output = program.then(|| {
            VmOutputProjection::for_tool_activity(
                Arc::clone(&output_manager),
                Arc::clone(&work_unit),
                row_idx,
            )
        });
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
    use crate::cli::messages::{Message, TranscriptRowKind};
    use crate::runtime::VmEffectEnvelope;
    use crate::vm::{CapabilityKind, CapabilityRequirement, HostSideEffect, ResourceSelector};

    fn emit(execution_id: Uuid, sequence: u64, text: &str) -> VmEffectEnvelope {
        VmEffectEnvelope {
            execution_id,
            effect: crate::vm::VmSideEffect {
                protocol_version: crate::vm::VM_TYPE_SYSTEM_VERSION,
                sequence,
                requirement: CapabilityRequirement {
                    capability: CapabilityKind::SessionEmit,
                    selector: ResourceSelector::None,
                },
                event: HostSideEffect::Emit { text: text.into() },
                output: Vec::new(),
                origin: crate::vm::SourceOrigin::generated("named-brain-submit-program-test"),
            },
        }
    }

    #[test]
    fn named_brain_submit_program_event_bus_commits_emit_once_inside_tool_output() {
        let manager = Arc::new(OutputManager::new(crate::config::ColorScheme::default()));
        manager.disable_stdout();
        let run_id = Uuid::new_v4();
        let activity = manager.start_activity_with_id(
            crate::cli::messages::MessageId::from_uuid(run_id),
            format!("Speculative run {run_id}"),
        );
        let row_idx = activity.add_tool("submit_program (inspect state)");
        let projection = VmOutputProjection::for_tool_activity(
            Arc::clone(&manager),
            activity.work_unit(),
            row_idx,
        );
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let presentation = WorkUnitPresentation {
            work_unit: activity.work_unit(),
            row_idx,
            program: true,
            vm_output: Some(projection),
            event_tx,
        };

        // Compatibility line output and typed `say` output share the tool's
        // semantic body. Deliver a suffix first, duplicate it while pending,
        // then replay the whole accepted prefix after the gap closes.
        presentation.line("live line".into());
        let execution_id = Uuid::new_v4();
        for envelope in [
            emit(execution_id, 1, " after"),
            emit(execution_id, 1, " after"),
            emit(execution_id, 0, "visible"),
            emit(execution_id, 0, "visible"),
            emit(execution_id, 1, " after"),
        ] {
            presentation.vm_effect_envelope(envelope);
        }

        let mut accepted = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            let ReplEvent::VmEffect {
                projection,
                envelope,
            } = event
            else {
                panic!("unexpected event-bus message");
            };
            accepted.push(super::super::event_loop::project_vm_effect_event(
                &projection,
                envelope,
            ));
        }
        assert_eq!(accepted, vec![false, false, true, false, false]);

        // The real tool completion contains `visible` in several serialized
        // outcome fields. Only the accepted VM Emit belongs in the transcript.
        let serialized_outcome = serde_json::json!({
            "status": "completed",
            "output": "visible after",
            "output_chunks": ["visible", " after"],
            "vm_side_effects": [{"event": {"emit": {"text": "visible"}}}]
        })
        .to_string();
        super::super::event_loop::complete_successful_tool_row(
            "submit_program",
            &serde_json::json!({
                "language": "lisp",
                "source": "(begin)",
                "intent": "inspect state"
            }),
            activity.work_unit().as_ref(),
            row_idx,
            &serialized_outcome,
        );
        activity.set_complete();

        let messages = manager.get_messages();
        assert_eq!(messages.len(), 1, "tool execution must stay in Activity");
        let root = messages[0]
            .transcript_row(&crate::config::ColorScheme::default())
            .expect("activity transcript tree");
        assert_eq!(root.kind, TranscriptRowKind::Activity);
        assert_eq!(root.children.len(), 1);
        let tool = &root.children[0];
        assert_eq!(tool.kind, TranscriptRowKind::ToolCall);
        let outputs = tool
            .children
            .iter()
            .filter(|row| row.kind == TranscriptRowKind::ToolOutput)
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 1);
        assert_eq!(
            outputs[0]
                .body
                .iter()
                .filter(|line| line.contains("visible"))
                .count(),
            1
        );
        assert!(outputs[0].body.iter().any(|line| line == "live line"));
        assert!(outputs[0].body.iter().any(|line| line == " after"));
        assert!(messages.iter().all(|message| {
            message
                .transcript_row(&crate::config::ColorScheme::default())
                .is_none_or(|row| row.kind != TranscriptRowKind::Output)
        }));

        let canonical = activity.complete_transcript(&crate::config::ColorScheme::default());
        assert_eq!(canonical.matches("visible").count(), 1, "{canonical}");
        assert_eq!(canonical.matches("live line").count(), 1, "{canonical}");
    }
}
