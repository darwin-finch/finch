// Conversation Compactor — Infinite Context Phase 2
//
// When the sliding window drops older messages from the context sent to the
// provider, this module summarises those messages via a lightweight provider
// call and injects the summary as a `[Summary of earlier context: ...]` prefix
// so the LLM retains awareness of earlier turns without exceeding the context
// window.
//
// # Flow
//
// ```
// all_msgs  ──► apply_sliding_window ──► window (recent N msgs)
//    │                                       │
//    └─ dropped (older msgs) ──► summarise ──► inject prefix ──► final_msgs
// ```
//
// The injected prefix is a user message followed by a brief assistant
// acknowledgement, which keeps the alternating user/assistant pattern required
// by all providers.
//
// # Design notes
//
// * Summarisation is done with a single non-streaming `generate()` call.
//   No tools are sent — we want just text.
// * Only `Text` content blocks are included in the summarisation input;
//   tool-use/tool-result blocks are described generically.
// * Failure is non-fatal: if summarisation fails the window is returned as-is
//   with a warning logged (same behaviour as if the flag were off).

use crate::claude::{ContentBlock, Message};
use crate::generators::Generator;
use anyhow::Result;
use std::sync::Arc;

/// Summarises messages that have slid off the conversation window and injects
/// the summary as a user+assistant prefix.
pub struct ConversationCompactor {
    generator: Arc<dyn Generator>,
}

impl ConversationCompactor {
    pub fn new(generator: Arc<dyn Generator>) -> Self {
        Self { generator }
    }

    /// Compact `dropped` messages into a summary prefix and prepend it to
    /// `window`.  Returns `window` unchanged if `dropped` is empty or if
    /// the summarisation call fails.
    pub async fn compact(&self, dropped: &[Message], window: Vec<Message>) -> Vec<Message> {
        if dropped.is_empty() {
            return window;
        }

        match self.summarize(dropped).await {
            Ok(summary) => inject_summary_prefix(summary, window),
            Err(e) => {
                tracing::warn!("Conversation summarisation failed, keeping window as-is: {e}");
                window
            }
        }
    }

    /// Call the generator to produce a concise summary of `messages`.
    async fn summarize(&self, messages: &[Message]) -> Result<String> {
        let conversation_text = format_messages_for_summary(messages);
        let prompt = format!(
            "Summarise the following conversation history concisely (2-5 sentences). \
             Preserve key decisions, code written, errors fixed, and any context needed \
             to continue the conversation naturally:\n\n{conversation_text}"
        );

        let req = vec![Message::user(prompt)];
        let resp = self.generator.generate(req, None).await?;
        Ok(resp.text.trim().to_string())
    }
}

/// Inject `summary` as a user+assistant pair at the front of `window`.
///
/// The assistant acknowledgement (`"Understood."`) keeps the required
/// alternating user→assistant role ordering expected by all providers.
pub fn inject_summary_prefix(summary: String, mut window: Vec<Message>) -> Vec<Message> {
    let prefix_user = Message::user(format!("[Summary of earlier context: {}]", summary));
    let prefix_assistant = Message::assistant("Understood.");
    // Prepend: [summary_user, summary_assistant, ...window]
    window.insert(0, prefix_assistant);
    window.insert(0, prefix_user);
    window
}

/// Render `messages` as plain text suitable for the summarisation prompt.
///
/// * `text` blocks → included verbatim
/// * `tool_use` blocks → described as `[Called tool: <name>]`
/// * `tool_result` blocks → described as `[Tool result for: <tool_use_id>]`
pub fn format_messages_for_summary(messages: &[Message]) -> String {
    messages
        .iter()
        .map(|msg| {
            let role = &msg.role;
            let parts: Vec<String> = msg
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Text { text } => text.clone(),
                    ContentBlock::ToolUse { name, .. } => {
                        format!("[Called tool: {name}]")
                    }
                    ContentBlock::ToolResult { tool_use_id, .. } => {
                        format!("[Tool result for: {tool_use_id}]")
                    }
                    ContentBlock::Image { .. } => "[image]".to_string(),
                })
                .collect();
            format!("{role}: {}", parts.join(" "))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::Message;

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant(text)
    }

    // ── format_messages_for_summary ──────────────────────────────────────────

    #[test]
    fn test_format_empty_messages_gives_empty_string() {
        assert_eq!(format_messages_for_summary(&[]), "");
    }

    #[test]
    fn test_format_single_user_message() {
        let msgs = vec![user("Hello world")];
        let out = format_messages_for_summary(&msgs);
        assert!(out.contains("user:"), "missing role prefix: {out}");
        assert!(out.contains("Hello world"), "missing text: {out}");
    }

    #[test]
    fn test_format_preserves_user_and_assistant_roles() {
        let msgs = vec![user("How do I use async?"), assistant("Use tokio::spawn.")];
        let out = format_messages_for_summary(&msgs);
        assert!(out.contains("user:"), "{out}");
        assert!(out.contains("assistant:"), "{out}");
    }

    #[test]
    fn test_format_tool_use_rendered_generically() {
        let msgs = vec![Message::with_content(
            "assistant",
            vec![ContentBlock::ToolUse {
                id: "tu_1".to_string(),
                name: "Bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
            }],
        )];
        let out = format_messages_for_summary(&msgs);
        assert!(
            out.contains("[Called tool: Bash]"),
            "tool name not in summary text: {out}"
        );
    }

    #[test]
    fn test_format_tool_result_rendered_generically() {
        let msgs = vec![Message::with_content(
            "user",
            vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".to_string(),
                content: "file.txt".to_string(),
                is_error: None,
            }],
        )];
        let out = format_messages_for_summary(&msgs);
        assert!(
            out.contains("[Tool result for: tu_1]"),
            "tool result not in summary text: {out}"
        );
    }

    #[test]
    fn test_format_multiple_messages_separated_by_blank_lines() {
        let msgs = vec![user("Q1"), user("Q2")];
        let out = format_messages_for_summary(&msgs);
        // Double newline between messages
        assert!(
            out.contains("\n\n"),
            "messages should be separated by blank line: {out}"
        );
    }

    // ── inject_summary_prefix ────────────────────────────────────────────────

    #[test]
    fn test_inject_prefix_prepends_two_messages() {
        let window = vec![user("Current question")];
        let result = inject_summary_prefix("Old context.".to_string(), window);

        // Must be [summary_user, summary_assistant, current_question] = 3 total
        assert_eq!(result.len(), 3, "expected 3 messages: {result:?}");
    }

    #[test]
    fn test_inject_prefix_first_message_is_user_with_summary() {
        let window = vec![user("Hello")];
        let result = inject_summary_prefix("Summary text.".to_string(), window);

        assert_eq!(result[0].role, "user");
        let text = match &result[0].content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("expected text block"),
        };
        assert!(
            text.contains("[Summary of earlier context:"),
            "prefix not in first message: {text}"
        );
        assert!(
            text.contains("Summary text."),
            "summary body not in first message: {text}"
        );
    }

    #[test]
    fn test_inject_prefix_second_message_is_assistant_ack() {
        let window = vec![user("Hello")];
        let result = inject_summary_prefix("S".to_string(), window);
        assert_eq!(result[1].role, "assistant");
    }

    #[test]
    fn test_inject_prefix_preserves_window_order() {
        let window = vec![user("Q1"), assistant("A1"), user("Q2")];
        let result = inject_summary_prefix("ctx".to_string(), window);

        // [summary_user, summary_assistant, Q1, A1, Q2]
        assert_eq!(result.len(), 5);
        assert_eq!(result[2].role, "user"); // Q1
        assert_eq!(result[4].role, "user"); // Q2
    }

    #[test]
    fn test_inject_prefix_empty_window_still_has_prefix_pair() {
        let result = inject_summary_prefix("summary".to_string(), vec![]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, "user");
        assert_eq!(result[1].role, "assistant");
    }

    // ── ConversationCompactor (compact() with empty dropped) ─────────────────

    #[tokio::test]
    async fn test_compact_with_empty_dropped_returns_window_unchanged() {
        // Use a dummy generator — it should NOT be called when dropped is empty
        let gen = Arc::new(PanicGenerator);
        let compactor = ConversationCompactor::new(gen);

        let window = vec![user("hello"), assistant("world")];
        let result = compactor.compact(&[], window.clone()).await;
        assert_eq!(result.len(), window.len(), "window must be unchanged");
    }

    // ── Mock generator for testing summarization injection ───────────────────

    struct PanicGenerator;

    #[async_trait::async_trait]
    impl Generator for PanicGenerator {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> Result<crate::generators::GeneratorResponse> {
            panic!("PanicGenerator: generate() should not be called in this test")
        }

        async fn generate_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> Result<Option<tokio::sync::mpsc::Receiver<Result<crate::generators::StreamChunk>>>>
        {
            panic!("PanicGenerator: generate_stream() should not be called")
        }

        fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
            static CAPS: std::sync::OnceLock<crate::generators::GeneratorCapabilities> =
                std::sync::OnceLock::new();
            CAPS.get_or_init(|| crate::generators::GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: false,
                supports_conversation: true,
                max_context_messages: None,
            })
        }

        fn name(&self) -> &str {
            "panic-generator"
        }
    }

    struct FixedSummaryGenerator {
        summary: String,
    }

    #[async_trait::async_trait]
    impl Generator for FixedSummaryGenerator {
        async fn generate(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> Result<crate::generators::GeneratorResponse> {
            Ok(crate::generators::GeneratorResponse {
                text: self.summary.clone(),
                content_blocks: vec![ContentBlock::Text {
                    text: self.summary.clone(),
                }],
                tool_uses: vec![],
                metadata: crate::generators::ResponseMetadata {
                    generator: "fixed".to_string(),
                    model: "fixed".to_string(),
                    confidence: None,
                    stop_reason: None,
                    input_tokens: None,
                    output_tokens: None,
                    latency_ms: None,
                },
            })
        }

        async fn generate_stream(
            &self,
            _messages: Vec<Message>,
            _tools: Option<Vec<crate::tools::types::ToolDefinition>>,
        ) -> Result<Option<tokio::sync::mpsc::Receiver<Result<crate::generators::StreamChunk>>>>
        {
            Ok(None)
        }

        fn capabilities(&self) -> &crate::generators::GeneratorCapabilities {
            static CAPS: std::sync::OnceLock<crate::generators::GeneratorCapabilities> =
                std::sync::OnceLock::new();
            CAPS.get_or_init(|| crate::generators::GeneratorCapabilities {
                supports_streaming: false,
                supports_tools: false,
                supports_conversation: true,
                max_context_messages: None,
            })
        }

        fn name(&self) -> &str {
            "fixed-summary-generator"
        }
    }

    #[tokio::test]
    async fn test_compact_injects_summary_prefix_into_window() {
        let gen = Arc::new(FixedSummaryGenerator {
            summary: "User asked about async Rust.".to_string(),
        });
        let compactor = ConversationCompactor::new(gen);

        let dropped = vec![user("How does async work?"), assistant("Use tokio.")];
        let window = vec![user("Next question")];

        let result = compactor.compact(&dropped, window).await;

        // Should be [summary_user, summary_assistant, "Next question"] = 3
        assert_eq!(result.len(), 3, "expected 3 messages: {result:?}");
        assert_eq!(result[0].role, "user");
        let first_text = match &result[0].content[0] {
            ContentBlock::Text { text } => text.clone(),
            _ => panic!("expected text"),
        };
        assert!(
            first_text.contains("[Summary of earlier context:"),
            "summary prefix missing: {first_text}"
        );
        assert!(
            first_text.contains("User asked about async Rust."),
            "summary body missing: {first_text}"
        );
    }

    #[tokio::test]
    async fn test_compact_result_starts_with_user_message() {
        let gen = Arc::new(FixedSummaryGenerator {
            summary: "Context summary.".to_string(),
        });
        let compactor = ConversationCompactor::new(gen);

        let dropped = vec![user("Old message")];
        let window = vec![user("New message")];

        let result = compactor.compact(&dropped, window).await;
        assert_eq!(
            result[0].role, "user",
            "compact result must start with user message for API compliance"
        );
    }
}
