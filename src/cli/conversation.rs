// Conversation history manager for multi-turn interactions

use crate::claude::{ContentBlock, Message};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Manages conversation history for multi-turn interactions with context window management
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationHistory {
    messages: Vec<Message>,
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
            max_messages: 20, // Keep last 20 messages (10 user + 10 assistant turns)
            max_tokens_estimate: 32_000, // ~8K tokens * 4 chars/token
            compaction_threshold_percent: 0.8, // Compact at 80% of max tokens
            auto_compact_enabled: true, // Auto-compaction enabled by default
        }
    }

    /// Create a conversation history with custom limits
    pub fn with_limits(max_messages: usize, max_tokens_estimate: usize) -> Self {
        Self {
            messages: Vec::new(),
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

    /// Get all messages for API request
    pub fn get_messages(&self) -> Vec<Message> {
        self.messages.clone()
    }

    /// Clear conversation history (start fresh)
    pub fn clear(&mut self) {
        self.messages.clear();
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
        self.auto_compact_enabled && self.context_usage_percent() >= self.compaction_threshold_percent
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
        history.max_messages = 20;
        history.max_tokens_estimate = 32_000;
        history.compaction_threshold_percent = 0.8;
        history.auto_compact_enabled = true;

        Ok(history)
    }
}

impl Default for ConversationHistory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let temp_path = "/tmp/test_conv_shammah.json";
        conv.save(temp_path).expect("Failed to save conversation");

        let loaded = ConversationHistory::load(temp_path).expect("Failed to load conversation");

        assert_eq!(loaded.message_count(), 2);
        let messages = loaded.get_messages();
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].text_content(), "Test message");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].text_content(), "Test response");

        // Clean up
        let _ = std::fs::remove_file(temp_path);
    }
}
