// Event types for the concurrent REPL event loop

use crate::tools::executor::ToolSignature;
use crate::tools::patterns::ToolPattern;
use crate::tools::types::ToolUse;
use anyhow::Result;
use tokio::sync::oneshot;
use uuid::Uuid;

/// Result of a tool execution confirmation prompt
#[derive(Debug, Clone)]
pub enum ConfirmationResult {
    ApproveOnce,
    ApproveExactSession(ToolSignature),
    ApprovePatternSession(ToolPattern),
    ApproveExactPersistent(ToolSignature),
    ApprovePatternPersistent(ToolPattern),
    Deny,
}

/// Events that flow through the REPL event loop
#[derive(Debug)]
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
    },

    /// A tool execution completed
    ToolResult {
        query_id: Uuid,
        tool_id: String,
        result: Result<String>,
    },

    /// Tool approval is needed (blocking for that query only)
    ToolApprovalNeeded {
        query_id: Uuid,
        tool_use: ToolUse,
        response_tx: oneshot::Sender<ConfirmationResult>,
    },

    /// Output is ready to display
    OutputReady {
        message: String,
    },

    /// Streaming response started
    StreamingStarted {
        query_id: Uuid,
    },

    /// Streaming response delta
    StreamingDelta {
        query_id: Uuid,
        delta: String,
    },

    /// Streaming response completed
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

    /// User requested query cancellation (Ctrl+C)
    CancelQuery,

    /// Request to shut down the REPL
    Shutdown,
}
