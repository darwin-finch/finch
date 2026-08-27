//! `ReplEvent` — the message bus between async tasks in the event loop.
//!
//! Every cross-task action is encoded as a `ReplEvent` sent over an unbounded
//! `mpsc` channel to the main loop.  Variants fall into three groups:
//!
//! * **Query lifecycle** — `StreamingChunk`, `StreamingComplete`, `ToolResult`,
//!   `ToolApprovalNeeded`, `CancelQuery`.
//! * **Brain** — canonical named-Brain projections and runner requests.

use crate::cli::messages::WorkUnit;
use crate::cli::output_manager::VmOutputProjection;
use crate::runtime::VmEffectEnvelope;
use crate::tools::executor::ToolSignature;
use crate::tools::patterns::ToolPattern;
use crate::tools::types::ToolUse;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RunnerReconnectTarget {
    pub brain: String,
    pub environment: crate::brain::store::BrainEnvironment,
    pub lease_id: Option<crate::brain::store::RunnerLeaseId>,
}

/// Result of a tool execution confirmation prompt
#[derive(Debug, Clone)]
pub enum ConfirmationResult {
    ApproveOnce,
    ApproveExactSession(ToolSignature),
    ApprovePatternSession(ToolPattern),
    ApproveExactPersistent(ToolSignature),
    ApprovePatternPersistent(ToolPattern),
    /// Approve but substitute the tool's input with a user-edited version.
    ApproveWithInput(serde_json::Value),
    Deny,
}

/// Machine-readable identity for failures that cross the provider task boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryFailureKind {
    /// An ordinary provider, parsing, or frontend failure.
    Ordinary,
    /// The spawned provider task itself panicked or was cancelled unexpectedly.
    ProviderTaskTerminated,
}

/// Events that flow through the REPL event loop
#[derive(Debug)]
#[allow(dead_code)]
pub enum ReplEvent {
    /// User submitted input (query or command)
    UserInput {
        input: String,
    },

    /// A query completed successfully with a response
    QueryComplete {
        query_id: Uuid,
        response: String,
    },

    /// A query failed with an error
    QueryFailed {
        query_id: Uuid,
        error: String,
        kind: QueryFailureKind,
    },

    /// A tool execution completed
    ToolResult {
        query_id: Uuid,
        round_token: crate::cli::conversation::ToolRoundToken,
        tool_id: String,
        result: Result<String>,
    },

    /// Provider-native tool calls became part of this query. The frontend
    /// records them before execution so a delegated Brain turn preserves the
    /// real call/approval/result ordering instead of reconstructing it later.
    ToolCallsStarted {
        query_id: Uuid,
        tool_uses: Vec<ToolUse>,
    },

    /// Tool approval is needed (blocking for that query only)
    ToolApprovalNeeded {
        query_id: Uuid,
        tool_use: ToolUse,
        response_tx: oneshot::Sender<ConfirmationResult>,
    },

    /// A verified typed ProgramRun is suspended at one exact capability
    /// boundary. The dialog returns a structured scope choice; the provider
    /// source is never replayed after approval.
    VmApprovalNeeded {
        query_id: Option<Uuid>,
        prompt: crate::vm::ApprovalPrompt,
        response_tx: oneshot::Sender<crate::vm::ApprovalChoice>,
    },

    /// Output is ready to display
    OutputReady {
        message: String,
    },

    /// A portable typed-VM effect to project on the owning terminal client.
    ///
    /// The VM may run in Tokio's blocking pool. It therefore must not write
    /// WorkUnits or drive terminal rendering directly: the client event loop
    /// owns that mutable presentation state. Tokio's MPSC preserves send
    /// order for this per-run sender, while the envelope retains the durable
    /// `(execution_id, sequence)` identity for replay-capable hosts.
    VmEffect {
        query_id: Option<Uuid>,
        projection: VmOutputProjection,
        envelope: VmEffectEnvelope,
    },

    /// Every effect for one provider-response output port has been enqueued.
    /// Completion travels through the same ordered event bus so scrollback
    /// cannot commit the WorkUnit after only its first `say` chunk.
    VmOutputComplete {
        output_unit: Arc<WorkUnit>,
    },

    /// Execute-once VM effects observed while running one provider response.
    /// Named-Brain turns retain these separately from their reducible runtime
    /// checkpoint so restoration cannot imply that an effect should replay.
    VmEffectJournalComplete {
        query_id: Uuid,
        records: Vec<crate::server::RunnerEffectRecord>,
    },

    /// An explicitly entered typed program finished on a background worker.
    /// Its output unit already exists in the shadow buffer; the event loop
    /// owns final status/error projection and the corresponding redraw.
    TypedProgramComplete {
        output_unit: Arc<WorkUnit>,
        result: std::result::Result<crate::runtime::outcome::ExecutionOutcome, String>,
    },

    /// Streaming response completed (used for non-streaming path)
    StreamingComplete {
        query_id: Uuid,
        full_response: String,
    },

    /// Query statistics update (for status bar)
    StatsUpdate {
        model: String,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
        latency_ms: Option<u64>,
    },

    /// A child-agent lifecycle update for the live task projection.
    AgentLifecycle(crate::runtime::scheduler::AgentEvent),

    /// User requested query cancellation (Ctrl+C)
    CancelQuery,

    /// Request to shut down the REPL
    Shutdown,

    /// Show a dialog and send the result back through `response_tx`.
    ///
    /// Set `active_dialog` on the TUI, release the mutex, and let
    /// `spawn_input_task` deliver the key events.  The render tick routes
    /// `pending_dialog_result` → `response_tx` when the dialog completes.
    ///
    /// This is the ONLY correct way to show a dialog from a spawned task —
    /// do NOT call `TuiRenderer::show_dialog` (it blocks the mutex and the
    /// event loop).
    ShowDialog {
        dialog: crate::cli::tui::Dialog,
        response_tx: tokio::sync::oneshot::Sender<crate::cli::tui::DialogResult>,
    },

    /// Co-Forth poset execution finished.
    PosetComplete {
        result: Result<String>,
    },

    /// Lisp evaluation finished (spawned by handle_user_input when input starts with `(`).
    LispResult {
        result: Result<String>,
    },

    /// Snapshot or live event from the currently attached named brain.
    RemoteBrainMessage {
        target: String,
        message: crate::brain::store::BrainWireMessage,
    },
    RemoteBrainError {
        target: String,
        error: String,
    },
    /// The named-Brain WebSocket ended. Retain the attachment identity so
    /// later input cannot silently fall back to the local Brain, but make the
    /// offline runner state explicit in the status/UI.
    RemoteBrainDisconnected {
        target: String,
    },
    /// Snapshot or event from the durable home attachment. The epoch keeps a
    /// superseded receiver from invalidating its replacement.
    HomeBrainMessage {
        epoch: u64,
        message: crate::brain::store::BrainWireMessage,
    },
    /// The home event watch ended independently of runner callback health.
    HomeBrainWatchFailed {
        epoch: u64,
        error: Option<String>,
    },
    /// Retry local IPC, durable attachment, and runner callback binding.
    ReconnectHomeBrain {
        epoch: u64,
        attempt: u32,
    },
    /// Retry the lease-bound runner callback independently of the event watch.
    ReconnectHomeRunner {
        epoch: u64,
        attempt: u32,
        target: RunnerReconnectTarget,
    },
    /// The expiring lease served by this frontend was renewed, lost, or
    /// reacquired. A renewed lease is not considered active until the event
    /// loop has also registered its Cap'n Proto runner callback. `epoch`
    /// prevents a stopped home-renewal task from overwriting a later handoff.
    RunnerLeaseStatus {
        brain: String,
        environment: crate::brain::store::BrainEnvironment,
        epoch: u64,
        lease_id: Option<crate::brain::store::RunnerLeaseId>,
        detail: String,
    },

    /// A daemon request routed through the callback registered for this
    /// frontend's current named-Brain runner lease.
    NamedBrainProgramRequested(crate::server::RunnerProgramRequest),
    /// A complete provider/tool/VM turn routed to the frontend holding the
    /// named Brain's environment-runner lease.
    NamedBrainTurnRequested(crate::server::RunnerTurnRequest),
    /// Project a daemon-committed successful turn into runner-owned memory.
    NamedBrainMemoryProjectionRequested(crate::server::RunnerMemoryProjectionRequest),
    /// Cancel one exact ProgramRun currently owned by this frontend.
    NamedBrainRunCancelRequested(crate::server::RunnerCancelRequest),
    /// Release frontend-local cancellation state after a delegated program ends.
    NamedBrainProgramFinished(crate::brain::store::RunId),

    /// The daemon has durably committed the exact named-Brain turn that
    /// requested this frontend replacement. It is now safe to leave the
    /// runner lease and exec the verified candidate binary.
    FrontendRestartReady {
        brain: String,
        run_id: crate::brain::store::RunId,
        restart: crate::tools::implementations::restart::DeferredFrontendRestart,
    },
}

/// Requests sent from the TUI event loop to the LLM worker loop.
#[derive(Debug)]
pub enum LlmRequest {
    /// Dispatch an LLM turn.  `text = ""` for tool-continuation turns.
    /// `no_tools` suppresses tool definitions (used for conversational word-push responses).
    Query {
        id: Uuid,
        text: String,
        no_tools: bool,
        /// Tool continuations wait for the atomic history commit before
        /// reading shared conversation state.
        admission: Option<tokio::sync::oneshot::Receiver<()>>,
        admission_ready: Option<tokio::sync::oneshot::Sender<()>>,
        spawned: Option<tokio::sync::oneshot::Sender<()>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn test_confirmation_result_variants_exist() {
        // Verify all ConfirmationResult variants are constructible
        let _once = ConfirmationResult::ApproveOnce;
        let _deny = ConfirmationResult::Deny;
        // These just need to compile — they're message-passing types, not logic types
    }

    #[test]
    fn test_repl_event_user_input() {
        let event = ReplEvent::UserInput {
            input: "hello world".to_string(),
        };
        match event {
            ReplEvent::UserInput { input } => assert_eq!(input, "hello world"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_query_complete() {
        let id = Uuid::new_v4();
        let event = ReplEvent::QueryComplete {
            query_id: id,
            response: "The answer is 42".to_string(),
        };
        match event {
            ReplEvent::QueryComplete { query_id, response } => {
                assert_eq!(query_id, id);
                assert_eq!(response, "The answer is 42");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_query_failed() {
        let id = Uuid::new_v4();
        let event = ReplEvent::QueryFailed {
            query_id: id,
            error: "network timeout".to_string(),
            kind: QueryFailureKind::Ordinary,
        };
        match event {
            ReplEvent::QueryFailed { error, .. } => assert_eq!(error, "network timeout"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_output_ready() {
        let event = ReplEvent::OutputReady {
            message: "streaming chunk".to_string(),
        };
        match event {
            ReplEvent::OutputReady { message } => assert_eq!(message, "streaming chunk"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_streaming_complete() {
        let id = Uuid::new_v4();
        let event = ReplEvent::StreamingComplete {
            query_id: id,
            full_response: "complete response".to_string(),
        };
        match event {
            ReplEvent::StreamingComplete { full_response, .. } => {
                assert_eq!(full_response, "complete response");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_stats_update_all_fields() {
        let event = ReplEvent::StatsUpdate {
            model: "claude-sonnet-4-6".to_string(),
            input_tokens: Some(100),
            output_tokens: Some(250),
            latency_ms: Some(1500),
        };
        match event {
            ReplEvent::StatsUpdate {
                model,
                input_tokens,
                output_tokens,
                latency_ms,
            } => {
                assert_eq!(model, "claude-sonnet-4-6");
                assert_eq!(input_tokens, Some(100));
                assert_eq!(output_tokens, Some(250));
                assert_eq!(latency_ms, Some(1500));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_stats_update_optional_fields_none() {
        let event = ReplEvent::StatsUpdate {
            model: "local".to_string(),
            input_tokens: None,
            output_tokens: None,
            latency_ms: None,
        };
        match event {
            ReplEvent::StatsUpdate {
                input_tokens,
                output_tokens,
                latency_ms,
                ..
            } => {
                assert!(input_tokens.is_none());
                assert!(output_tokens.is_none());
                assert!(latency_ms.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_repl_event_cancel_and_shutdown_are_unit_variants() {
        // These should not carry any data
        let _cancel = ReplEvent::CancelQuery;
        let _shutdown = ReplEvent::Shutdown;
        // If the above compile and run, the test passes
    }

    #[test]
    fn test_tool_approval_needed_via_channel() {
        // ToolApprovalNeeded requires a oneshot channel — exercise construction
        let (tx, _rx) = tokio::sync::oneshot::channel::<ConfirmationResult>();
        let id = Uuid::new_v4();
        let tool_use = crate::tools::types::ToolUse {
            id: "tool_1".to_string(),
            name: "read".to_string(),
            input: serde_json::json!({"file_path": "/tmp/test"}),
        };
        let event = ReplEvent::ToolApprovalNeeded {
            query_id: id,
            tool_use,
            response_tx: tx,
        };
        match event {
            ReplEvent::ToolApprovalNeeded { query_id, .. } => assert_eq!(query_id, id),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn typed_program_completion_keeps_the_output_unit_on_the_event_loop() {
        let unit = Arc::new(WorkUnit::new("typed program output"));
        let event = ReplEvent::TypedProgramComplete {
            output_unit: Arc::clone(&unit),
            result: Err("cancelled before completion".to_string()),
        };
        match event {
            ReplEvent::TypedProgramComplete {
                output_unit,
                result: Err(error),
            } => {
                assert!(Arc::ptr_eq(&unit, &output_unit));
                assert_eq!(error, "cancelled before completion");
            }
            _ => panic!("Wrong variant"),
        }
    }
}
