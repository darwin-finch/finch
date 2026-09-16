//! Generation-layer translation from `finch-providers::StreamChunk`.
//!
//! Provider-specific parsers stay in adapters. This module only maps already
//! normalized provider events, including `ContentBlockComplete(ToolUse)` into
//! `ToolCallComplete`. Native adapter emission of `ThinkingDelta` /
//! `ToolCallDelta` remains issue #777.

use crate::event::{Allowance, GenerationEvent, ReasoningKind, ToolCall, Usage};
use crate::identity::GenerationIdentity;
use finch_providers::{ContentBlock, EventProvenance, StreamChunk};

/// Translate one provider stream chunk into a generation event.
///
/// Returns `None` for chunks that are already represented by other events
/// (plain text content-block completion after `TextDelta`).
pub fn translate_provider_chunk(
    chunk: StreamChunk,
    identity: &GenerationIdentity,
    sequence: u64,
) -> Option<GenerationEvent> {
    match chunk {
        StreamChunk::TextDelta(text) => Some(GenerationEvent::TextDelta {
            text,
            provenance: provenance(identity, "text", sequence, None),
        }),
        StreamChunk::ThinkingDelta { text, provenance } => {
            let kind = if provenance.opaque_replay.is_some() {
                ReasoningKind::Opaque
            } else {
                ReasoningKind::RawText
            };
            Some(GenerationEvent::ThinkingDelta {
                text,
                kind,
                provenance,
            })
        }
        StreamChunk::ToolCallDelta {
            id,
            name,
            arguments_delta,
            provenance,
        } => Some(GenerationEvent::ToolCallDelta {
            id,
            name,
            arguments_delta,
            provenance,
        }),
        StreamChunk::ToolCallComplete {
            id,
            name,
            input,
            provenance,
        } => Some(GenerationEvent::ToolCallComplete(ToolCall {
            id,
            name,
            input,
            provenance,
        })),
        StreamChunk::ContentBlockComplete(ContentBlock::ToolUse { id, name, input }) => {
            Some(GenerationEvent::ToolCallComplete(ToolCall {
                id,
                name,
                input,
                provenance: provenance(identity, "tool_call", sequence, None),
            }))
        }
        StreamChunk::ContentBlockComplete(ContentBlock::OpaqueReasoning { encrypted_content }) => {
            Some(GenerationEvent::ThinkingDelta {
                text: String::new(),
                kind: ReasoningKind::Opaque,
                provenance: provenance(identity, "thinking", sequence, Some(encrypted_content)),
            })
        }
        StreamChunk::ContentBlockComplete(ContentBlock::Text { .. })
        | StreamChunk::ContentBlockComplete(ContentBlock::ToolResult { .. })
        | StreamChunk::ContentBlockComplete(ContentBlock::Image { .. }) => None,
        StreamChunk::ResponseMetadata { model } => identity
            .clone()
            .with_actual_model(model)
            .ok()
            .map(GenerationEvent::Identity),
        StreamChunk::Usage {
            input_tokens,
            output_tokens,
        } => Some(GenerationEvent::Usage(Usage {
            input_tokens: Some(input_tokens),
            output_tokens: Some(output_tokens),
        })),
        StreamChunk::Allowance {
            primary_used_percent,
            secondary_used_percent,
        } => Some(GenerationEvent::Allowance(Allowance {
            primary_used_percent,
            secondary_used_percent,
        })),
    }
}

fn provenance(
    identity: &GenerationIdentity,
    event: &str,
    sequence: u64,
    opaque_replay: Option<String>,
) -> EventProvenance {
    EventProvenance {
        provider: identity.actual.provider.clone(),
        model: identity.actual.model.clone(),
        event: event.to_string(),
        sequence,
        opaque_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{BackendKind, BackendRef};
    use serde_json::json;

    fn identity() -> GenerationIdentity {
        GenerationIdentity::pinned(
            BackendRef::new("claude", "claude-sonnet-4", BackendKind::Cloud).unwrap(),
        )
    }

    #[test]
    fn test_translate_content_block_tool_use_becomes_tool_call_complete() {
        let event = translate_provider_chunk(
            StreamChunk::ContentBlockComplete(ContentBlock::ToolUse {
                id: "toolu_1".into(),
                name: "read".into(),
                input: json!({"file_path": "/tmp/a"}),
            }),
            &identity(),
            3,
        )
        .expect("tool-use block must become ToolCallComplete");
        match event {
            GenerationEvent::ToolCallComplete(call) => {
                assert_eq!(call.id, "toolu_1");
                assert_eq!(call.name, "read");
                assert_eq!(call.input["file_path"], "/tmp/a");
                assert_eq!(call.provenance.provider, "claude");
                assert_eq!(call.provenance.model, "claude-sonnet-4");
                assert_eq!(call.provenance.sequence, 3);
                assert!(
                    call.provenance.opaque_replay.is_none(),
                    "validated tool call must not stash display text in opaque_replay: {call:?}"
                );
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    #[test]
    fn test_translate_opaque_reasoning_is_not_display_text() {
        let event = translate_provider_chunk(
            StreamChunk::ContentBlockComplete(ContentBlock::OpaqueReasoning {
                encrypted_content: "sig-abc".into(),
            }),
            &identity(),
            1,
        )
        .expect("opaque reasoning must become ThinkingDelta");
        match event {
            GenerationEvent::ThinkingDelta {
                text,
                kind,
                provenance,
            } => {
                assert!(
                    text.is_empty(),
                    "opaque continuation leaked into display text: {text:?}"
                );
                assert_eq!(kind, ReasoningKind::Opaque);
                assert_eq!(provenance.opaque_replay.as_deref(), Some("sig-abc"));
            }
            other => panic!("expected ThinkingDelta, got {other:?}"),
        }
    }

    #[test]
    fn test_translate_response_metadata_updates_actual_model_only() {
        let start = identity();
        let event = translate_provider_chunk(
            StreamChunk::ResponseMetadata {
                model: "claude-sonnet-4-served".into(),
            },
            &start,
            0,
        )
        .expect("serving model must become Identity");
        match event {
            GenerationEvent::Identity(updated) => {
                assert_eq!(updated.requested.model, "claude-sonnet-4");
                assert_eq!(updated.resolved.model, "claude-sonnet-4");
                assert_eq!(updated.actual.model, "claude-sonnet-4-served");
            }
            other => panic!("expected Identity, got {other:?}"),
        }
    }

    #[test]
    fn test_translate_text_content_block_is_skipped_after_deltas() {
        let event = translate_provider_chunk(
            StreamChunk::ContentBlockComplete(ContentBlock::text("hello")),
            &identity(),
            2,
        );
        assert!(
            event.is_none(),
            "completed text block must not duplicate TextDelta: {event:?}"
        );
    }

    #[test]
    fn test_translate_invalid_serving_model_is_dropped_not_echoed() {
        let event = translate_provider_chunk(
            StreamChunk::ResponseMetadata {
                model: "bad model\nsecret".into(),
            },
            &identity(),
            0,
        );
        assert!(
            event.is_none(),
            "invalid serving model must not become an Identity event: {event:?}"
        );
    }
}
