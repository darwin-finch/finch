// Conversation history manager for multi-turn interactions

use crate::claude::{ContentBlock, Message};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolRoundToken(Uuid);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRoundResult {
    pub tool_id: String,
    pub content: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRoundError {
    StageAlreadyExists,
    InvalidAssistant(String),
    NoActiveStage,
    StaleToken,
    UnknownTool(String),
    DuplicateResult(String),
    MissingResults(Vec<String>),
    ContinuationUnavailable,
}

impl std::fmt::Display for ToolRoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StageAlreadyExists => write!(formatter, "a tool round is already staged"),
            Self::InvalidAssistant(reason) => write!(formatter, "invalid tool assistant: {reason}"),
            Self::NoActiveStage => write!(formatter, "no tool round is staged"),
            Self::StaleToken => write!(formatter, "tool result belongs to a stale round"),
            Self::UnknownTool(id) => write!(formatter, "unknown tool result id {id}"),
            Self::DuplicateResult(id) => write!(formatter, "duplicate tool result id {id}"),
            Self::MissingResults(ids) => {
                write!(formatter, "missing tool results: {}", ids.join(", "))
            }
            Self::ContinuationUnavailable => write!(formatter, "LLM continuation is unavailable"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRoundProgress {
    Pending,
    Complete,
}

#[derive(Debug, Clone)]
struct StagedToolRound {
    token: ToolRoundToken,
    assistant: Message,
    expected_ids: Vec<String>,
    results: HashMap<String, ToolRoundResult>,
}

/// Manages conversation history for multi-turn interactions with context window management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationHistory {
    messages: Vec<Message>,
    /// Tool-bearing assistant turns are invisible until their matching results
    /// can be appended in the same mutation.
    #[serde(skip)]
    staged_tool_rounds: HashMap<Uuid, StagedToolRound>,
    #[serde(skip)]
    max_messages: usize,
    #[serde(skip)]
    max_tokens_estimate: usize,
    #[serde(skip)]
    compaction_threshold_percent: f32, // Trigger compaction at this % of max tokens (e.g., 0.8 = 80%)
    #[serde(skip)]
    auto_compact_enabled: bool, // Whether auto-compaction is enabled
}

impl ConversationHistory {
    /// Create a new conversation history with default limits
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            staged_tool_rounds: HashMap::new(),
            max_messages: 500, // ~250 turns — plenty for a full coding session
            max_tokens_estimate: 600_000, // ~150k tokens * 4 chars/token (Claude: 200k context)
            compaction_threshold_percent: 0.9, // Compact at 90% of max
            auto_compact_enabled: true, // Auto-compaction enabled by default
        }
    }

    /// Create a conversation history with custom limits
    pub fn with_limits(max_messages: usize, max_tokens_estimate: usize) -> Self {
        Self {
            messages: Vec::new(),
            staged_tool_rounds: HashMap::new(),
            max_messages,
            max_tokens_estimate,
            compaction_threshold_percent: 0.8,
            auto_compact_enabled: true,
        }
    }

    /// Add a user message to the conversation
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text { text: content }],
        });
        self.trim_if_needed();
    }

    /// Add a user message with optional image attachments.
    /// Each image is `(media_type, base64_data)`.
    pub fn add_user_message_with_images(&mut self, text: String, images: &[(String, String)]) {
        let mut blocks: Vec<ContentBlock> = images
            .iter()
            .map(|(media_type, data)| ContentBlock::image(media_type.clone(), data.clone()))
            .collect();
        blocks.push(ContentBlock::Text { text });

        self.messages.push(Message {
            role: "user".to_string(),
            content: blocks,
        });
        self.trim_if_needed();
    }

    /// Add an assistant message to the conversation
    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text { text: content }],
        });
        self.trim_if_needed();
    }

    /// Add a complete message to the conversation
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.trim_if_needed();
    }

    /// Stage a tool-bearing assistant message for `query_id` without exposing
    /// it through any committed-history read path.
    pub fn stage_assistant(
        &mut self,
        query_id: Uuid,
        assistant: Message,
    ) -> std::result::Result<ToolRoundToken, ToolRoundError> {
        if self.staged_tool_rounds.contains_key(&query_id) {
            return Err(ToolRoundError::StageAlreadyExists);
        }
        if assistant.role != "assistant" {
            return Err(ToolRoundError::InvalidAssistant(
                "role must be assistant".to_string(),
            ));
        }
        let expected_ids = assistant
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if expected_ids.is_empty() {
            return Err(ToolRoundError::InvalidAssistant(
                "at least one tool_use is required".to_string(),
            ));
        }
        let unique = expected_ids
            .iter()
            .collect::<std::collections::HashSet<_>>();
        if unique.len() != expected_ids.len() {
            return Err(ToolRoundError::InvalidAssistant(
                "tool_use ids must be unique".to_string(),
            ));
        }
        let token = ToolRoundToken(Uuid::new_v4());
        self.staged_tool_rounds.insert(
            query_id,
            StagedToolRound {
                token,
                assistant,
                expected_ids,
                results: HashMap::new(),
            },
        );
        Ok(token)
    }

    pub fn record_tool_result(
        &mut self,
        query_id: Uuid,
        token: ToolRoundToken,
        tool_id: &str,
        result: &std::result::Result<String, anyhow::Error>,
    ) -> std::result::Result<ToolRoundProgress, ToolRoundError> {
        self.validate_tool_result(query_id, token, tool_id)?;
        let stage = self
            .staged_tool_rounds
            .get_mut(&query_id)
            .ok_or(ToolRoundError::NoActiveStage)?;
        let (content, is_error) = match result {
            Ok(content) => (content.clone(), false),
            Err(error) => (error.to_string(), true),
        };
        stage.results.insert(
            tool_id.to_string(),
            ToolRoundResult {
                tool_id: tool_id.to_string(),
                content,
                is_error,
            },
        );
        Ok(if stage.results.len() == stage.expected_ids.len() {
            ToolRoundProgress::Complete
        } else {
            ToolRoundProgress::Pending
        })
    }

    pub fn validate_tool_result(
        &self,
        query_id: Uuid,
        token: ToolRoundToken,
        tool_id: &str,
    ) -> std::result::Result<(), ToolRoundError> {
        let stage = self
            .staged_tool_rounds
            .get(&query_id)
            .ok_or(ToolRoundError::NoActiveStage)?;
        if stage.token != token {
            return Err(ToolRoundError::StaleToken);
        }
        if !stage
            .expected_ids
            .iter()
            .any(|expected| expected == tool_id)
        {
            return Err(ToolRoundError::UnknownTool(tool_id.to_string()));
        }
        if stage.results.contains_key(tool_id) {
            return Err(ToolRoundError::DuplicateResult(tool_id.to_string()));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn staged_round_debug(
        &self,
        query_id: Uuid,
    ) -> Option<(ToolRoundToken, usize, usize)> {
        self.staged_tool_rounds
            .get(&query_id)
            .map(|round| (round.token, round.expected_ids.len(), round.results.len()))
    }

    pub fn completed_tool_results(
        &self,
        query_id: Uuid,
        token: ToolRoundToken,
    ) -> std::result::Result<Vec<ToolRoundResult>, ToolRoundError> {
        let stage = self
            .staged_tool_rounds
            .get(&query_id)
            .ok_or(ToolRoundError::NoActiveStage)?;
        if stage.token != token {
            return Err(ToolRoundError::StaleToken);
        }
        let missing = stage
            .expected_ids
            .iter()
            .filter(|id| !stage.results.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(ToolRoundError::MissingResults(missing));
        }
        Ok(stage
            .expected_ids
            .iter()
            .map(|id| stage.results[id].clone())
            .collect())
    }

    /// Atomically publish a staged assistant tool call and all its validated
    /// results in assistant declaration order.
    pub fn commit_tool_round(
        &mut self,
        query_id: Uuid,
        token: ToolRoundToken,
    ) -> std::result::Result<Vec<ToolRoundResult>, ToolRoundError> {
        let ordered_results = self.completed_tool_results(query_id, token)?;
        let stage = self
            .staged_tool_rounds
            .remove(&query_id)
            .ok_or(ToolRoundError::NoActiveStage)?;
        let tool_results = Message {
            role: "user".to_string(),
            content: ordered_results
                .iter()
                .map(|result| ContentBlock::ToolResult {
                    tool_use_id: result.tool_id.clone(),
                    content: result.content.clone(),
                    is_error: result.is_error.then_some(true),
                })
                .collect(),
        };
        self.messages.push(stage.assistant);
        self.messages.push(tool_results);
        self.trim_if_needed();
        Ok(ordered_results)
    }

    /// Drop a staged tool call without changing committed history.
    pub fn abort_staged(&mut self, query_id: Uuid) -> bool {
        self.abort_staged_round(query_id).is_some()
    }

    pub fn abort_staged_round(&mut self, query_id: Uuid) -> Option<(ToolRoundToken, Vec<String>)> {
        self.staged_tool_rounds
            .remove(&query_id)
            .map(|stage| (stage.token, stage.expected_ids))
    }

    /// Roll back the immediately preceding atomic commit when continuation
    /// spawning failed after admission. The round becomes staged again.
    pub fn rollback_last_tool_round(
        &mut self,
        query_id: Uuid,
        token: ToolRoundToken,
    ) -> std::result::Result<(), ToolRoundError> {
        let results_message = self.messages.pop().ok_or(ToolRoundError::NoActiveStage)?;
        let assistant = self.messages.pop().ok_or(ToolRoundError::NoActiveStage)?;
        let expected_ids = assistant
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolUse { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let results = results_message
            .content
            .into_iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => Some((
                    tool_use_id.clone(),
                    ToolRoundResult {
                        tool_id: tool_use_id,
                        content,
                        is_error: is_error.unwrap_or(false),
                    },
                )),
                _ => None,
            })
            .collect();
        self.staged_tool_rounds.insert(
            query_id,
            StagedToolRound {
                token,
                assistant,
                expected_ids,
                results,
            },
        );
        Ok(())
    }

    /// Get all messages for API request
    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Clear conversation history (start fresh)
    pub fn clear(&mut self) {
        self.messages.clear();
        self.staged_tool_rounds.clear();
    }

    /// Check if conversation has any messages
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Get number of complete turns (pairs of user + assistant messages)
    pub fn turn_count(&self) -> usize {
        // Each turn = 2 messages (user + assistant)
        self.messages.len() / 2
    }

    /// Get total number of messages
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Create a snapshot of current conversation state
    pub fn snapshot(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Restore conversation from a snapshot
    pub fn restore_snapshot(&mut self, snapshot: Vec<Message>) {
        self.messages = snapshot;
        self.staged_tool_rounds.clear();
    }

    /// Trim old messages if context exceeds limits
    fn trim_if_needed(&mut self) {
        // Trim by message count
        if self.messages.len() > self.max_messages {
            let remove_count = self.messages.len() - self.max_messages;
            self.messages.drain(0..remove_count);
        }

        // Estimate token count (rough: 1 token ≈ 4 characters)
        let total_chars: usize = self.messages.iter().map(|m| m.text().len()).sum();

        if total_chars > self.max_tokens_estimate {
            // Remove oldest messages until under limit
            // BUT: Always keep at least 2 messages (1 user + 1 assistant minimum)
            // This prevents conversation from becoming empty during tool execution
            while self.messages.len() > 2
                && self.messages.iter().map(|m| m.text().len()).sum::<usize>()
                    > self.max_tokens_estimate
            {
                self.messages.remove(0);
            }
        }
    }

    /// Get estimated token count (rough approximation)
    pub fn estimated_tokens(&self) -> usize {
        let total_chars: usize = self.messages.iter().map(|m| m.text().len()).sum();
        total_chars / 4 // Rough estimate: 1 token ≈ 4 characters
    }

    /// Get percentage of context window used (0.0 to 1.0)
    pub fn context_usage_percent(&self) -> f32 {
        let current_tokens = self.estimated_tokens() as f32;
        let max_tokens = (self.max_tokens_estimate / 4) as f32; // Convert char estimate to tokens
        (current_tokens / max_tokens).min(1.0)
    }

    /// Get percentage remaining until auto-compaction (0.0 to 1.0)
    ///
    /// Returns the percentage of context window remaining before compaction triggers.
    /// Example: If threshold is 80% and current usage is 60%, returns 0.25 (25% remaining)
    pub fn compaction_percent_remaining(&self) -> f32 {
        if !self.auto_compact_enabled {
            return 1.0; // Compaction disabled, always 100% remaining
        }

        let usage = self.context_usage_percent();
        let threshold = self.compaction_threshold_percent;

        if usage >= threshold {
            0.0 // At or past threshold
        } else {
            // Calculate remaining percentage relative to threshold
            // e.g., usage=60%, threshold=80% → remaining = (80-60)/80 = 25%
            (threshold - usage) / threshold
        }
    }

    /// Check if compaction should be triggered
    pub fn should_compact(&self) -> bool {
        self.auto_compact_enabled
            && self.context_usage_percent() >= self.compaction_threshold_percent
    }

    /// Enable or disable auto-compaction
    pub fn set_auto_compact(&mut self, enabled: bool) {
        self.auto_compact_enabled = enabled;
    }

    /// Set compaction threshold (0.0 to 1.0, e.g., 0.8 = 80%)
    pub fn set_compaction_threshold(&mut self, threshold: f32) {
        self.compaction_threshold_percent = threshold.clamp(0.0, 1.0);
    }

    /// Save conversation to JSON file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let json =
            serde_json::to_string_pretty(self).context("Failed to serialize conversation")?;

        // Ensure parent directory exists
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent)
                .context("Failed to create directory for conversation state")?;
        }

        fs::write(path.as_ref(), json).with_context(|| {
            format!(
                "Failed to write conversation to {}",
                path.as_ref().display()
            )
        })?;

        Ok(())
    }

    /// Load conversation from JSON file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let json = fs::read_to_string(path.as_ref()).with_context(|| {
            format!(
                "Failed to read conversation from {}",
                path.as_ref().display()
            )
        })?;

        let mut history: ConversationHistory =
            serde_json::from_str(&json).context("Failed to parse conversation JSON")?;

        // Restore default config values (these are skipped during serialization)
        history.max_messages = 500;
        history.max_tokens_estimate = 600_000;
        history.compaction_threshold_percent = 0.9;
        history.auto_compact_enabled = true;

        Ok(history)
    }
}

impl Default for ConversationHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// Conversation compactor that summarizes older messages to reduce token usage
///
/// When conversations grow too large, this compactor:
/// 1. Keeps the most recent messages intact (for context continuity)
/// 2. Summarizes older messages into a single summary message
/// 3. Uses the teacher API to generate high-quality summaries
///
/// # Usage
///
/// ```text
/// use crate::cli::conversation::{ConversationHistory, ConversationCompactor};
/// use crate::providers::fallback_chain::FallbackChain;
///
/// let mut history = ConversationHistory::new();
/// // ... add many messages ...
///
/// // Check if compaction is needed
/// let compactor = ConversationCompactor::new(&fallback_chain);
/// if compactor.should_compact(&history) {
///     // Compact conversation in background
///     compactor.compact(&mut history).await?;
/// }
/// ```
///
/// # Integration Points
///
/// The compactor should be called in the REPL event loop after each query completion:
/// - File: `src/cli/repl_event/event_loop.rs`
/// - Location: After successful query completion, before next query
/// - Async: Yes, runs in background (non-blocking)
///
/// Example integration:
/// ```text
/// // In event_loop.rs, after query completes:
/// if self.compactor.should_compact(&self.conversation) {
///     tokio::spawn(async move {
///         if let Err(e) = compactor.compact(&mut conversation).await {
///             tracing::warn!("Failed to compact conversation: {}", e);
///         }
///     });
/// }
/// ```
#[allow(dead_code)]
pub struct ConversationCompactor<'a> {
    /// Fallback chain for API calls
    fallback_chain: &'a crate::providers::fallback_chain::FallbackChain,
    /// Number of recent messages to keep intact (default: 4)
    keep_recent_count: usize,
    /// Compaction threshold as percentage of max tokens (default: 0.8 = 80%)
    threshold_percent: f32,
}

#[allow(dead_code)]
impl<'a> ConversationCompactor<'a> {
    /// Create a new conversation compactor
    pub fn new(fallback_chain: &'a crate::providers::fallback_chain::FallbackChain) -> Self {
        Self {
            fallback_chain,
            keep_recent_count: 4, // Keep last 4 messages (2 turns)
            threshold_percent: 0.8,
        }
    }

    /// Create with custom settings
    pub fn with_settings(
        fallback_chain: &'a crate::providers::fallback_chain::FallbackChain,
        keep_recent_count: usize,
        threshold_percent: f32,
    ) -> Self {
        Self {
            fallback_chain,
            keep_recent_count,
            threshold_percent: threshold_percent.clamp(0.0, 1.0),
        }
    }

    /// Check if conversation should be compacted
    pub fn should_compact(&self, history: &ConversationHistory) -> bool {
        history.should_compact()
    }

    /// Compact conversation history by summarizing older messages
    ///
    /// Returns the compacted conversation history or an error if compaction fails
    pub async fn compact(&self, history: &mut ConversationHistory) -> anyhow::Result<()> {
        use crate::claude::types::ContentBlock;
        use crate::providers::ProviderRequest;

        // Check if compaction is needed
        if !self.should_compact(history) {
            tracing::debug!("Conversation does not need compaction");
            return Ok(());
        }

        let messages = history.get_messages();

        // If we have fewer messages than keep_recent_count, nothing to compact
        if messages.len() <= self.keep_recent_count {
            tracing::debug!(
                "Not enough messages to compact (need at least {})",
                self.keep_recent_count + 1
            );
            return Ok(());
        }

        // Split messages into "to summarize" and "to keep"
        let split_point = messages.len() - self.keep_recent_count;
        let to_summarize = &messages[..split_point];
        let to_keep = &messages[split_point..];

        tracing::info!(
            "Compacting conversation: {} messages total, summarizing {}, keeping {}",
            messages.len(),
            to_summarize.len(),
            to_keep.len()
        );

        // Build summarization prompt
        let mut conversation_text = String::new();
        for msg in to_summarize {
            conversation_text.push_str(&format!("{}: {}\n\n", msg.role, msg.text()));
        }

        let summarization_prompt = format!(
            "Please provide a concise summary of this conversation. \
             Focus on key topics discussed, decisions made, and important context. \
             Keep it under 200 words.\n\n\
             Conversation:\n{}",
            conversation_text
        );

        // Send summarization request to teacher API
        let request = ProviderRequest {
            model: String::new(), // Use provider default
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: summarization_prompt,
                }],
            }],
            max_tokens: 1024,
            tools: None,
            temperature: None,
            stream: false,
            system: None,
        };

        let response = self
            .fallback_chain
            .send_message_with_fallback(&request)
            .await?;

        // Extract summary text from response
        let summary_text = response
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if summary_text.is_empty() {
            anyhow::bail!("Failed to generate conversation summary (empty response)");
        }

        tracing::debug!("Generated summary: {} chars", summary_text.len());

        // Build compacted conversation:
        // 1. A single summary message (user role, for context)
        // 2. All recent messages (to_keep)
        let mut compacted_messages = Vec::new();

        // Add summary as a user message
        compacted_messages.push(Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: format!("[Summary of previous conversation]\n\n{}", summary_text),
            }],
        });

        // Add recent messages
        compacted_messages.extend(to_keep.iter().cloned());

        // Replace conversation history with compacted version
        history.restore_snapshot(compacted_messages);

        tracing::info!(
            "Conversation compacted: {} → {} messages (saved ~{} tokens)",
            messages.len(),
            history.message_count(),
            to_summarize
                .iter()
                .map(|m| m.text().len() / 4)
                .sum::<usize>()
                - summary_text.len() / 4
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_assistant(ids: &[&str]) -> Message {
        Message {
            role: "assistant".to_string(),
            content: ids
                .iter()
                .map(|id| ContentBlock::ToolUse {
                    id: (*id).to_string(),
                    name: "Read".to_string(),
                    input: serde_json::json!({"path": "README.md"}),
                })
                .collect(),
        }
    }

    #[test]
    fn test_conversation_creation() {
        let conv = ConversationHistory::new();
        assert!(conv.is_empty());
        assert_eq!(conv.turn_count(), 0);
        assert_eq!(conv.message_count(), 0);
    }

    #[test]
    fn test_add_messages() {
        let mut conv = ConversationHistory::new();

        conv.add_user_message("Hello".to_string());
        assert_eq!(conv.message_count(), 1);
        assert_eq!(conv.turn_count(), 0); // No complete turn yet

        conv.add_assistant_message("Hi there!".to_string());
        assert_eq!(conv.message_count(), 2);
        assert_eq!(conv.turn_count(), 1); // Now we have 1 complete turn
    }

    #[test]
    fn test_get_messages() {
        let mut conv = ConversationHistory::new();

        conv.add_user_message("What is 2+2?".to_string());
        conv.add_assistant_message("4".to_string());

        let messages = conv.get_messages();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text_content(), "What is 2+2?");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text_content(), "4");
    }

    #[test]
    fn staged_tool_round_is_invisible_until_atomic_commit() {
        let mut conv = ConversationHistory::new();
        conv.add_user_message("read it".to_string());
        let query_id = Uuid::new_v4();

        let token = conv
            .stage_assistant(query_id, tool_assistant(&["tool-1"]))
            .unwrap();
        assert_eq!(conv.message_count(), 1);
        assert_eq!(conv.get_messages().len(), 1);
        assert_eq!(conv.snapshot().len(), 1);

        assert_eq!(
            conv.record_tool_result(query_id, token, "tool-1", &Ok("contents".to_string())),
            Ok(ToolRoundProgress::Complete)
        );
        conv.commit_tool_round(query_id, token).unwrap();
        let messages = conv.get_messages();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[2].role, "user");
    }

    #[test]
    fn aborted_tool_round_never_becomes_visible_and_rejects_late_commit() {
        let mut conv = ConversationHistory::new();
        conv.add_user_message("read it".to_string());
        let query_id = Uuid::new_v4();

        let token = conv
            .stage_assistant(query_id, tool_assistant(&["tool-1"]))
            .unwrap();
        assert!(conv.abort_staged(query_id));
        assert_eq!(
            conv.record_tool_result(query_id, token, "tool-1", &Ok("late".to_string())),
            Err(ToolRoundError::NoActiveStage)
        );
        assert_eq!(
            conv.commit_tool_round(query_id, token),
            Err(ToolRoundError::NoActiveStage)
        );

        let messages = conv.get_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text(), "read it");
    }

    #[test]
    fn next_query_after_aborted_tool_round_has_valid_committed_history() {
        let mut conv = ConversationHistory::new();
        conv.add_user_message("first query".to_string());
        let cancelled_query = Uuid::new_v4();
        conv.stage_assistant(cancelled_query, tool_assistant(&["cancelled-tool"]))
            .unwrap();
        conv.abort_staged(cancelled_query);
        conv.add_user_message("retry query".to_string());

        assert!(conv.get_messages().iter().all(|message| !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))));
    }

    #[test]
    fn duplicate_unknown_and_missing_results_cannot_complete_a_round() {
        let mut conv = ConversationHistory::new();
        let query_id = Uuid::new_v4();
        let token = conv
            .stage_assistant(query_id, tool_assistant(&["A", "B"]))
            .unwrap();
        let mut completions = 0;
        let first = conv.record_tool_result(query_id, token, "A", &Ok("a".to_string()));
        if first == Ok(ToolRoundProgress::Complete) {
            completions += 1;
        }
        assert_eq!(first, Ok(ToolRoundProgress::Pending));
        assert_eq!(
            conv.record_tool_result(query_id, token, "A", &Ok("duplicate".to_string())),
            Err(ToolRoundError::DuplicateResult("A".to_string()))
        );
        assert_eq!(
            conv.record_tool_result(query_id, token, "X", &Ok("unknown".to_string())),
            Err(ToolRoundError::UnknownTool("X".to_string()))
        );
        assert_eq!(
            conv.commit_tool_round(query_id, token),
            Err(ToolRoundError::MissingResults(vec!["B".to_string()]))
        );
        assert_eq!(completions, 0, "no provider continuation is ready");
        assert!(conv.get_messages().is_empty());
    }

    #[test]
    fn parallel_results_commit_in_assistant_declaration_order() {
        let mut conv = ConversationHistory::new();
        let query_id = Uuid::new_v4();
        let token = conv
            .stage_assistant(query_id, tool_assistant(&["A", "B"]))
            .unwrap();
        let progresses = [
            conv.record_tool_result(query_id, token, "B", &Ok("b".to_string())),
            conv.record_tool_result(query_id, token, "A", &Ok("a".to_string())),
        ];
        assert_eq!(progresses[0], Ok(ToolRoundProgress::Pending));
        assert_eq!(progresses[1], Ok(ToolRoundProgress::Complete));
        assert_eq!(
            progresses
                .iter()
                .filter(|progress| **progress == Ok(ToolRoundProgress::Complete))
                .count(),
            1,
            "exactly one provider continuation is ready"
        );
        conv.commit_tool_round(query_id, token).unwrap();
        assert_eq!(
            conv.record_tool_result(query_id, token, "A", &Ok("late".to_string())),
            Err(ToolRoundError::NoActiveStage)
        );
        let messages = conv.get_messages();
        let ids = messages[1]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["A", "B"]);
    }

    #[test]
    fn stale_prior_round_results_and_stage_collisions_are_rejected() {
        let mut conv = ConversationHistory::new();
        let query_id = Uuid::new_v4();
        let first = conv
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        assert_eq!(
            conv.stage_assistant(query_id, tool_assistant(&["B"])),
            Err(ToolRoundError::StageAlreadyExists)
        );
        conv.record_tool_result(query_id, first, "A", &Ok("a".to_string()))
            .unwrap();
        conv.commit_tool_round(query_id, first).unwrap();
        let second = conv
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        assert_eq!(
            conv.record_tool_result(query_id, first, "A", &Ok("stale".to_string())),
            Err(ToolRoundError::StaleToken)
        );
        assert_eq!(
            conv.record_tool_result(query_id, second, "A", &Ok("current".to_string())),
            Ok(ToolRoundProgress::Complete)
        );
    }

    #[test]
    fn test_clear() {
        let mut conv = ConversationHistory::new();

        conv.add_user_message("Hello".to_string());
        conv.add_assistant_message("Hi!".to_string());
        assert!(!conv.is_empty());

        conv.clear();
        assert!(conv.is_empty());
        assert_eq!(conv.turn_count(), 0);
    }

    #[test]
    fn test_message_count_trimming() {
        let mut conv = ConversationHistory::with_limits(4, 100_000);

        // Add 6 messages (exceeds limit of 4)
        for i in 0..3 {
            conv.add_user_message(format!("User {}", i));
            conv.add_assistant_message(format!("Assistant {}", i));
        }

        // Should have trimmed to last 4 messages
        assert_eq!(conv.message_count(), 4);

        let messages = conv.get_messages();
        assert_eq!(messages[0].text_content(), "User 1"); // First 2 messages removed
        assert_eq!(messages[1].text_content(), "Assistant 1");
    }

    #[test]
    fn test_token_estimation() {
        let mut conv = ConversationHistory::new();

        conv.add_user_message("test".to_string()); // 4 chars = ~1 token
        assert_eq!(conv.estimated_tokens(), 1);

        conv.add_assistant_message("response".to_string()); // 8 chars = ~2 tokens
        assert_eq!(conv.estimated_tokens(), 3);
    }

    #[test]
    fn test_token_based_trimming() {
        // Set very low token limit
        let mut conv = ConversationHistory::with_limits(100, 20); // 20 chars = ~5 tokens

        conv.add_user_message("short".to_string()); // 5 chars
        conv.add_assistant_message("ok".to_string()); // 2 chars
        conv.add_user_message("another message here".to_string()); // 20 chars

        // Total would be 27 chars, exceeds limit of 20
        // Should trim oldest messages
        assert!(conv.message_count() < 3);
        assert!(conv.estimated_tokens() <= 5);
    }

    #[test]
    fn test_conversation_persistence() {
        let mut conv = ConversationHistory::new();
        conv.add_user_message("Test message".to_string());
        conv.add_assistant_message("Test response".to_string());

        let temp_file = tempfile::NamedTempFile::new().expect("Failed to create temporary path");
        let temp_path = temp_file.path();
        conv.save(temp_path).expect("Failed to save conversation");

        let loaded = ConversationHistory::load(temp_path).expect("Failed to load conversation");

        assert_eq!(loaded.message_count(), 2);
        let messages = loaded.get_messages();
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text_content(), "Test message");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text_content(), "Test response");
    }

    #[test]
    fn persistence_exposes_committed_tool_rounds_but_never_staged_rounds() {
        let query_id = Uuid::new_v4();
        let mut staged = ConversationHistory::new();
        staged.add_user_message("inspect".into());
        let token = staged
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        staged
            .record_tool_result(query_id, token, "A", &Ok("value".into()))
            .unwrap();
        let staged_file = tempfile::NamedTempFile::new().unwrap();
        staged.save(staged_file.path()).unwrap();
        let loaded_staged = ConversationHistory::load(staged_file.path()).unwrap();
        assert_eq!(loaded_staged.message_count(), 1);

        staged.commit_tool_round(query_id, token).unwrap();
        let committed_file = tempfile::NamedTempFile::new().unwrap();
        staged.save(committed_file.path()).unwrap();
        let loaded_committed = ConversationHistory::load(committed_file.path()).unwrap();
        assert_eq!(loaded_committed.message_count(), 3);
        assert!(matches!(
            loaded_committed.get_messages()[1].content[0],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            loaded_committed.get_messages()[2].content[0],
            ContentBlock::ToolResult { .. }
        ));
    }
}
