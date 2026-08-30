// Conversation history manager for multi-turn interactions

use crate::claude::{ContentBlock, Message};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Immutable identity for one staged provider tool round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ToolRoundToken(Uuid);

/// One validated result retained in assistant declaration order.
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
    PersistenceUnavailable(String),
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
            Self::PersistenceUnavailable(error) => {
                write!(formatter, "conversation checkpoint is unavailable: {error}")
            }
        }
    }
}

impl std::error::Error for ToolRoundError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRoundProgress {
    Pending,
    Complete,
}

#[derive(Debug, Clone)]
struct StagedToolRound {
    token: ToolRoundToken,
    // Retain the provider's complete ordered assistant payload. Future opaque
    // reasoning/output items can extend ContentBlock without this layer
    // reconstructing or reordering them.
    assistant: Message,
    expected_ids: Vec<String>,
    results: HashMap<String, ToolRoundResult>,
}

/// Manages conversation history for multi-turn interactions with context window management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationHistory {
    messages: Vec<Message>,
    /// Provider-invisible tool rounds. Staged state is deliberately neither
    /// returned by history reads nor serialized during crash recovery.
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

    /// Stage a complete provider assistant payload without making it visible
    /// to request builders, snapshots, compaction, or persistence.
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
        if expected_ids.iter().collect::<HashSet<_>>().len() != expected_ids.len() {
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

    /// Record one result exactly once for the matching staged round.
    pub fn record_tool_result(
        &mut self,
        query_id: Uuid,
        token: ToolRoundToken,
        tool_id: &str,
        result: &std::result::Result<String, anyhow::Error>,
    ) -> std::result::Result<ToolRoundProgress, ToolRoundError> {
        let stage = self
            .staged_tool_rounds
            .get_mut(&query_id)
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

    /// Publish the complete assistant payload and all matching results under
    /// one conversation write lock. No reader can observe only one half.
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
        Ok(ordered_results)
    }

    /// Apply ordinary context limits to the complete pair before its durable
    /// checkpoint and publication permit are released. Callers retain a full
    /// pre-commit clone until those boundaries succeed.
    pub fn finalize_tool_round_commit(&mut self) {
        self.trim_if_needed();
    }

    /// Drop a staged round without changing committed provider history.
    pub fn abort_staged(&mut self, query_id: Uuid) -> bool {
        self.staged_tool_rounds.remove(&query_id).is_some()
    }

    /// Restore the immediately preceding complete round to provider-invisible
    /// staging if the admitted continuation could not be spawned.
    pub fn rollback_last_tool_round(
        &mut self,
        query_id: Uuid,
        token: ToolRoundToken,
    ) -> std::result::Result<(), ToolRoundError> {
        let results_message = self.messages.pop().ok_or(ToolRoundError::NoActiveStage)?;
        let assistant = match self.messages.pop() {
            Some(assistant) => assistant,
            None => {
                self.messages.push(results_message);
                return Err(ToolRoundError::NoActiveStage);
            }
        };
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
            .collect::<HashMap<_, _>>();
        if expected_ids.is_empty()
            || results.len() != expected_ids.len()
            || expected_ids.iter().any(|id| !results.contains_key(id))
        {
            self.messages.push(assistant);
            self.messages.push(Message {
                role: "user".to_string(),
                content: results
                    .into_values()
                    .map(|result| ContentBlock::ToolResult {
                        tool_use_id: result.tool_id,
                        content: result.content,
                        is_error: result.is_error.then_some(true),
                    })
                    .collect(),
            });
            return Err(ToolRoundError::InvalidAssistant(
                "last messages are not a complete tool round".to_string(),
            ));
        }
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

    pub fn staged_round(&self, query_id: Uuid) -> Option<(ToolRoundToken, usize, usize)> {
        self.staged_tool_rounds
            .get(&query_id)
            .map(|stage| (stage.token, stage.expected_ids.len(), stage.results.len()))
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

        // A front trim can cut between an assistant tool call and its result
        // message. Never expose an orphaned result as the new history prefix.
        while self.messages.first().is_some_and(|message| {
            !message.content.is_empty()
                && message
                    .content
                    .iter()
                    .all(|block| matches!(block, ContentBlock::ToolResult { .. }))
        }) {
            self.messages.remove(0);
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
        let path = path.as_ref();

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .context("Failed to create directory for conversation state")?;
        }
        atomic_replace(path, json.as_bytes())
            .with_context(|| format!("Failed to write conversation to {}", path.display()))
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
        history.staged_tool_rounds = HashMap::new();

        Ok(history)
    }
}

fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .context("Conversation path must name a file")?
        .to_string_lossy();
    let temp_path: PathBuf = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let result = (|| -> Result<()> {
        let mut temp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("Failed to create {}", temp_path.display()))?;
        temp.write_all(contents)
            .with_context(|| format!("Failed to write {}", temp_path.display()))?;
        temp.sync_all()
            .with_context(|| format!("Failed to sync {}", temp_path.display()))?;
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "Failed to atomically replace {} with {}",
                path.display(),
                temp_path.display()
            )
        })?;
        #[cfg(unix)]
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .with_context(|| format!("Failed to sync {}", parent.display()))?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
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
        let mut content = vec![ContentBlock::Text {
            text: "opaque-before".to_string(),
        }];
        content.extend(ids.iter().map(|id| ContentBlock::ToolUse {
            id: (*id).to_string(),
            name: "Read".to_string(),
            input: serde_json::json!({"path": id}),
        }));
        content.push(ContentBlock::Text {
            text: "opaque-after".to_string(),
        });
        Message {
            role: "assistant".to_string(),
            content,
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
    fn test_staged_tool_round_is_invisible_until_complete_atomic_commit() {
        let mut history = ConversationHistory::new();
        history.add_user_message("inspect".to_string());
        let query_id = Uuid::new_v4();
        let token = history
            .stage_assistant(query_id, tool_assistant(&["A", "B"]))
            .unwrap();

        assert_eq!(history.message_count(), 1);
        assert_eq!(history.snapshot().len(), 1);
        assert_eq!(
            history.record_tool_result(query_id, token, "B", &Ok("second".to_string())),
            Ok(ToolRoundProgress::Pending)
        );
        assert_eq!(history.get_messages().len(), 1);
        assert_eq!(
            history.record_tool_result(query_id, token, "A", &Ok("first".to_string())),
            Ok(ToolRoundProgress::Complete)
        );
        history.commit_tool_round(query_id, token).unwrap();

        let messages = history.get_messages();
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            messages[1].content.first(),
            Some(ContentBlock::Text { text }) if text == "opaque-before"
        ));
        assert!(matches!(
            messages[1].content.last(),
            Some(ContentBlock::Text { text }) if text == "opaque-after"
        ));
        let ids = messages[2]
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
    fn test_stale_duplicate_unknown_and_cancelled_results_never_publish() {
        let mut history = ConversationHistory::new();
        let query_id = Uuid::new_v4();
        let first = history
            .stage_assistant(query_id, tool_assistant(&["A", "B"]))
            .unwrap();
        assert_eq!(
            history.record_tool_result(query_id, first, "A", &Ok("first".to_string())),
            Ok(ToolRoundProgress::Pending)
        );
        assert_eq!(
            history.record_tool_result(query_id, first, "A", &Ok("duplicate".to_string())),
            Err(ToolRoundError::DuplicateResult("A".to_string()))
        );
        assert_eq!(
            history.record_tool_result(query_id, first, "X", &Ok("unknown".to_string())),
            Err(ToolRoundError::UnknownTool("X".to_string()))
        );
        assert_eq!(
            history.commit_tool_round(query_id, first),
            Err(ToolRoundError::MissingResults(vec!["B".to_string()]))
        );
        assert!(history.abort_staged(query_id));
        let second = history
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        assert_eq!(
            history.record_tool_result(query_id, first, "A", &Ok("stale".to_string())),
            Err(ToolRoundError::StaleToken)
        );
        assert!(history.abort_staged(query_id));
        assert_eq!(
            history.record_tool_result(query_id, second, "A", &Ok("late".to_string())),
            Err(ToolRoundError::NoActiveStage)
        );
        assert!(history.get_messages().is_empty());
    }

    #[test]
    fn test_failed_continuation_rolls_complete_pair_back_to_invisible_stage() {
        let mut history = ConversationHistory::with_limits(2, 600_000);
        history.add_user_message("older-user".to_string());
        history.add_assistant_message("older-assistant".to_string());
        let query_id = Uuid::new_v4();
        let token = history
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        history
            .record_tool_result(query_id, token, "A", &Ok("value".to_string()))
            .unwrap();
        history.commit_tool_round(query_id, token).unwrap();
        assert_eq!(history.message_count(), 4);

        history.rollback_last_tool_round(query_id, token).unwrap();
        assert_eq!(history.message_count(), 2);
        assert_eq!(history.get_messages()[0].text_content(), "older-user");
        assert_eq!(history.get_messages()[1].text_content(), "older-assistant");
        assert_eq!(history.staged_round(query_id), Some((token, 1, 1)));
    }

    #[test]
    fn test_context_trimming_never_exposes_an_orphaned_tool_result_prefix() {
        let mut history = ConversationHistory::with_limits(1, 600_000);
        let query_id = Uuid::new_v4();
        let token = history
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        history
            .record_tool_result(query_id, token, "A", &Ok("value".to_string()))
            .unwrap();
        history.commit_tool_round(query_id, token).unwrap();
        history.finalize_tool_round_commit();

        assert!(history.get_messages().iter().all(|message| !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))));
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
    fn test_opaque_reasoning_continuation_survives_restart_byte_for_byte() {
        let mut conversation = ConversationHistory::new();
        conversation.add_message(Message::with_content(
            "assistant",
            vec![
                ContentBlock::OpaqueReasoning {
                    encrypted_content: "opaque\0continuation+/=".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "call-1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"path":"README.md"}),
                },
            ],
        ));
        let file = tempfile::NamedTempFile::new().unwrap();
        conversation.save(file.path()).unwrap();

        let restarted = ConversationHistory::load(file.path()).unwrap();
        assert!(matches!(
            restarted.get_messages()[0].content.as_slice(),
            [
                ContentBlock::OpaqueReasoning { encrypted_content },
                ContentBlock::ToolUse { id, .. }
            ] if encrypted_content == "opaque\0continuation+/=" && id == "call-1"
        ));
    }

    #[test]
    fn test_persistence_replaces_atomically_and_never_serializes_staged_round() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("conversation.json");
        fs::write(&path, b"old incomplete bytes").unwrap();
        let query_id = Uuid::new_v4();
        let mut history = ConversationHistory::new();
        history.add_user_message("inspect".to_string());
        let token = history
            .stage_assistant(query_id, tool_assistant(&["A"]))
            .unwrap();
        history
            .record_tool_result(query_id, token, "A", &Ok("value".to_string()))
            .unwrap();

        history.save(&path).unwrap();
        let staged_reload = ConversationHistory::load(&path).unwrap();
        assert_eq!(staged_reload.message_count(), 1);

        history.commit_tool_round(query_id, token).unwrap();
        history.save(&path).unwrap();
        let committed_reload = ConversationHistory::load(&path).unwrap();
        assert_eq!(committed_reload.message_count(), 3);
        assert!(matches!(
            committed_reload.get_messages()[1].content[1],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            committed_reload.get_messages()[2].content[0],
            ContentBlock::ToolResult { .. }
        ));
        let leftovers = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }
}
