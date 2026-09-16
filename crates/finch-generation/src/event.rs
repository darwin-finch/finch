//! Normalized generation events, tool calls, and terminal outcomes.

use crate::identity::{GenerationIdentity, RouteDecision};
use crate::readiness::{LoadPhase, ReadinessReport, ResourceMetadata};
use finch_providers::EventProvenance;
use serde_json::Value;
use uuid::Uuid;

/// Stable id for one generate attempt. Superseded ids are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenerationId(Uuid);

impl GenerationId {
    /// Mint a new attempt id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Underlying UUID.
    pub fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for GenerationId {
    fn default() -> Self {
        Self::new()
    }
}

/// How thinking/reasoning text should be labelled by a UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReasoningKind {
    /// Provider-authored summary.
    Summary,
    /// Raw thinking text.
    RawText,
    /// Encrypted or otherwise opaque continuation. Never display-only content.
    Opaque,
}

/// Validated semantic tool call. Wire/replay data stays on provenance.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Tool-call id pinned for the event loop.
    pub id: String,
    /// Tool name after adapter validation.
    pub name: String,
    /// Parsed arguments.
    pub input: Value,
    /// Provider/model/event provenance. `opaque_replay` is not display content.
    pub provenance: EventProvenance,
}

/// Tool result the event loop feeds back into the next generation turn.
///
/// This is not Finch `ToolExecutor` output. Execution, permissions, and host
/// effects stay in the application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// Id of the [`ToolCall`] this answers.
    pub tool_call_id: String,
    /// Speakable result text.
    pub content: String,
    /// Whether the call failed.
    pub is_error: bool,
}

impl ToolResult {
    /// Successful result.
    pub fn success(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    /// Failed result.
    pub fn error(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
            is_error: true,
        }
    }
}

/// Token accounting for one attempt.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Usage {
    /// Prompt tokens when reported.
    pub input_tokens: Option<u32>,
    /// Completion tokens when reported.
    pub output_tokens: Option<u32>,
}

/// Subscription allowance snapshot. Not API billing.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Allowance {
    /// Primary allowance used, bounded 0..=100 when present.
    pub primary_used_percent: Option<f32>,
    /// Secondary allowance used, bounded 0..=100 when present.
    pub secondary_used_percent: Option<f32>,
}

/// Secret-free timing and resource metadata for a completed attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct GenerationMetadata {
    /// Identity after any serving-model correction.
    pub identity: GenerationIdentity,
    /// Usage when reported.
    pub usage: Usage,
    /// Allowance when reported.
    pub allowance: Allowance,
    /// Latency in milliseconds from the injected clock.
    pub latency_ms: Option<u64>,
    /// Resource snapshot.
    pub resources: ResourceMetadata,
    /// Why generation stopped, when the backend names it.
    pub stop_reason: Option<String>,
}

/// Exactly-once terminal state for an attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum TerminalOutcome {
    /// Successful completion, possibly with tool calls and no further tokens.
    Completed {
        /// Concatenated visible text.
        text: String,
        /// Validated tool calls emitted during the attempt.
        tool_calls: Vec<ToolCall>,
        /// Timing, usage, and identity.
        metadata: GenerationMetadata,
    },
    /// Caller cancelled the attempt.
    Cancelled {
        /// Identity at cancellation.
        identity: GenerationIdentity,
    },
    /// Budget timeout fired.
    TimedOut {
        /// Identity at timeout.
        identity: GenerationIdentity,
    },
    /// Transport or local runtime disconnected.
    Disconnected {
        /// Secret-free reason.
        reason: String,
        /// Identity at disconnect.
        identity: GenerationIdentity,
    },
    /// Backend failed before a successful terminal.
    Failed {
        /// Secret-free cause.
        cause: String,
        /// Identity at failure.
        identity: GenerationIdentity,
    },
}

impl TerminalOutcome {
    /// Identity recorded on this terminal.
    pub fn identity(&self) -> &GenerationIdentity {
        match self {
            Self::Completed { metadata, .. } => &metadata.identity,
            Self::Cancelled { identity }
            | Self::TimedOut { identity }
            | Self::Disconnected { identity, .. }
            | Self::Failed { identity, .. } => identity,
        }
    }
}

/// Normalized generation stream. Adapters and local backends both emit this.
#[derive(Debug, Clone, PartialEq)]
pub enum GenerationEvent {
    /// Attempt id assigned by the supervisor.
    Started {
        /// Fence id.
        id: GenerationId,
        /// Identity at start (requested = resolved = actual unless already known).
        identity: GenerationIdentity,
    },
    /// Readiness snapshot.
    Readiness(ReadinessReport),
    /// Load-phase progress.
    Loading {
        /// Current phase.
        phase: LoadPhase,
        /// Elapsed milliseconds.
        elapsed_ms: u64,
    },
    /// Visible text delta.
    TextDelta {
        /// Incremental visible text.
        text: String,
        /// Provenance.
        provenance: EventProvenance,
    },
    /// Thinking/reasoning delta. Labels must honor [`ReasoningKind`].
    ThinkingDelta {
        /// Incremental thinking text. Empty when only opaque material moved.
        text: String,
        /// How a UI must label this.
        kind: ReasoningKind,
        /// Provenance. Opaque replay is never display content.
        provenance: EventProvenance,
    },
    /// Incremental tool-call argument stream.
    ToolCallDelta {
        /// Tool-call id.
        id: String,
        /// Name once known.
        name: Option<String>,
        /// Argument fragment.
        arguments_delta: String,
        /// Provenance.
        provenance: EventProvenance,
    },
    /// Adapter-validated complete tool call.
    ToolCallComplete(ToolCall),
    /// Token usage snapshot.
    Usage(Usage),
    /// Subscription allowance snapshot.
    Allowance(Allowance),
    /// Identity correction (typically actual serving model).
    Identity(GenerationIdentity),
    /// Recorded routing decision.
    Route(RouteDecision),
    /// Exactly-once terminal. No events follow.
    Terminal(TerminalOutcome),
}

impl GenerationEvent {
    /// True for the unique terminal event.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Terminal(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{BackendKind, BackendRef};

    fn provenance() -> EventProvenance {
        EventProvenance {
            provider: "test".into(),
            model: "script-1".into(),
            event: "tool".into(),
            sequence: 1,
            opaque_replay: None,
        }
    }

    #[test]
    fn test_tool_result_success_and_error_flags() {
        let ok = ToolResult::success("call-1", "file contents");
        let err = ToolResult::error("call-1", "not found");
        assert!(
            !ok.is_error && err.is_error,
            "success/error flags inverted: ok={ok:?} err={err:?}"
        );
        assert_eq!(ok.tool_call_id, "call-1");
    }

    #[test]
    fn test_terminal_outcome_identity_is_available_on_every_variant() {
        let identity = GenerationIdentity::pinned(
            BackendRef::new("test", "script-1", BackendKind::Test).unwrap(),
        );
        for outcome in [
            TerminalOutcome::Cancelled {
                identity: identity.clone(),
            },
            TerminalOutcome::TimedOut {
                identity: identity.clone(),
            },
            TerminalOutcome::Disconnected {
                reason: "peer closed".into(),
                identity: identity.clone(),
            },
            TerminalOutcome::Failed {
                cause: "weights missing".into(),
                identity: identity.clone(),
            },
        ] {
            assert_eq!(
                outcome.identity(),
                &identity,
                "terminal identity missing or wrong: {outcome:?}"
            );
        }
    }

    #[test]
    fn test_thinking_delta_opaque_replay_is_not_display_text() {
        let event = GenerationEvent::ThinkingDelta {
            text: String::new(),
            kind: ReasoningKind::Opaque,
            provenance: EventProvenance {
                opaque_replay: Some("encrypted-continuation".into()),
                ..provenance()
            },
        };
        match event {
            GenerationEvent::ThinkingDelta {
                text,
                kind,
                provenance,
            } => {
                assert!(
                    text.is_empty(),
                    "opaque thinking must not put replay material in display text: {text:?}"
                );
                assert_eq!(kind, ReasoningKind::Opaque);
                assert_eq!(
                    provenance.opaque_replay.as_deref(),
                    Some("encrypted-continuation")
                );
            }
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }
}
