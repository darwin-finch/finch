// Conversation history manager for multi-turn interactions

use crate::claude::Message;

/// Manages conversation history for multi-turn interactions with context window management
pub struct ConversationHistory {
    messages: Vec<Message>,
    max_messages: usize,
    max_tokens_estimate: usize,
}

impl ConversationHistory {
    /// Create a new conversation history with default limits
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            max_messages: 20,           // Keep last 20 messages (10 user + 10 assistant turns)
            max_tokens_estimate: 32_000, // ~8K tokens * 4 chars/token
        }
    }

    /// Create a conversation history with custom limits
    pub fn with_limits(max_messages: usize, max_tokens_estimate: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_messages,
            max_tokens_estimate,
        }
    }

    /// Add a user message to the conversation
    pub fn add_user_message(&mut self, content: String) {
        self.messages.push(Message {
            role: "user".to_string(),
            content,
        });
        self.trim_if_needed();
    }

    /// Add an assistant message to the conversation
    pub fn add_assistant_message(&mut self, content: String) {
        self.messages.push(Message {
            role: "assistant".to_string(),
            content,
        });
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

    /// Trim old messages if context exceeds limits
    fn trim_if_needed(&mut self) {
        // Trim by message count
        if self.messages.len() > self.max_messages {
            let remove_count = self.messages.len() - self.max_messages;
            self.messages.drain(0..remove_count);
        }

        // Estimate token count (rough: 1 token ≈ 4 characters)
        let total_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();

        if total_chars > self.max_tokens_estimate {
            // Remove oldest messages until under limit
            while !self.messages.is_empty()
                && self.messages.iter().map(|m| m.content.len()).sum::<usize>()
                    > self.max_tokens_estimate
            {
                self.messages.remove(0);
            }
        }
    }

    /// Get estimated token count (rough approximation)
    pub fn estimated_tokens(&self) -> usize {
        let total_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
        total_chars / 4 // Rough estimate: 1 token ≈ 4 characters
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
        assert_eq!(messages[0].content, "What is 2+2?");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "4");
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
        assert_eq!(messages[0].content, "User 1"); // First 2 messages removed
        assert_eq!(messages[1].content, "Assistant 1");
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
}
