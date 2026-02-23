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
#[allow(dead_code)]
pub enum ReplEvent {
    /// User submitted input (query or command)
    UserInput { input: String },

    /// A query completed successfully with a response
    QueryComplete { query_id: Uuid, response: String },

    /// A query failed with an error
    QueryFailed { query_id: Uuid, error: String },

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
    OutputReady { message: String },

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

    /// User requested query cancellation (Ctrl+C)
    CancelQuery,

    /// Request to shut down the REPL
    Shutdown,
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
}
