// Provider session management with context optimization
//
// Tracks conversation context to minimize redundant token usage and provide
// configurable truncation strategies for long conversations.

use anyhow::Result;
use tokio::sync::mpsc;

use super::{LlmProvider, ProviderRequest, ProviderResponse, StreamChunk};
use crate::{ContentBlock, Message};

/// Provider session with context tracking
///
/// Tracks what context has been sent to the cloud provider to enable:
/// - Metrics on new vs repeated context
/// - Optional truncation of old messages
/// - Smart retention strategies (e.g., keep system prompts, drop old tool results)
pub struct ProviderSession {
    provider: std::sync::Arc<dyn LlmProvider>,
    state: ConversationState,
    config: SessionContextConfig,
}

/// Tracks the state of conversation with the provider
#[derive(Debug, Clone, Default)]
pub(crate) struct ConversationState {
    /// Number of messages sent in the last provider call
    last_provider_message_count: usize,

    /// Total input tokens sent to the provider (cumulative)
    total_input_tokens: usize,

    /// Estimated cached tokens (based on repeated context)
    estimated_cached_tokens: usize,

    /// Number of times the provider has been called
    provider_call_count: usize,
}

/// Configuration for provider context management
#[derive(Debug, Clone)]
pub struct SessionContextConfig {
    /// Maximum number of conversation turns to send (0 = unlimited)
    /// One turn = user message + assistant response
    pub max_context_turns: usize,

    /// Drop tool results older than N turns to save tokens (0 = keep all)
    pub tool_result_retention_turns: usize,

    /// Enable prompt caching hints (for Claude, Gemini)
    pub prompt_caching_enabled: bool,
}

impl Default for SessionContextConfig {
    fn default() -> Self {
        Self {
            max_context_turns: 0,           // Unlimited by default
            tool_result_retention_turns: 0, // Keep all by default
            prompt_caching_enabled: true,
        }
    }
}

impl ProviderSession {
    /// Create a new provider session with default config
    #[cfg(test)]
    pub fn new(provider: Box<dyn LlmProvider>) -> Self {
        Self {
            provider: std::sync::Arc::from(provider),
            state: ConversationState::default(),
            config: SessionContextConfig::default(),
        }
    }

    /// Create a new provider session with custom config
    #[cfg(test)]
    pub fn with_config(provider: Box<dyn LlmProvider>, config: SessionContextConfig) -> Self {
        Self {
            provider: std::sync::Arc::from(provider),
            state: ConversationState::default(),
            config,
        }
    }

    /// Create a session from an already validated shared provider.
    pub fn with_shared_provider(
        provider: std::sync::Arc<dyn LlmProvider>,
        config: SessionContextConfig,
    ) -> Self {
        Self {
            provider,
            state: ConversationState::default(),
            config,
        }
    }

    /// Send message with context tracking (Level 1: Minimal)
    ///
    /// Tracks new vs repeated context for metrics and logging.
    /// Always sends full context (APIs require it).
    pub async fn send_message(&mut self, request: &ProviderRequest) -> Result<ProviderResponse> {
        // Calculate metrics
        let total_messages = request.messages.len();
        let new_messages = total_messages.saturating_sub(self.state.last_provider_message_count);
        let repeated_messages = total_messages - new_messages;

        // Estimate tokens (rough: ~100 tokens per message)
        let estimated_total_tokens = total_messages * 100;
        let estimated_new_tokens = new_messages * 100;
        let estimated_cached_tokens = repeated_messages * 100;

        // Log metrics
        tracing::info!(
            provider = %self.provider.name(),
            call_count = self.state.provider_call_count + 1,
            total_messages,
            new_messages,
            repeated_messages,
            estimated_total_tokens,
            estimated_new_tokens,
            estimated_cached_tokens,
            "Provider context metrics"
        );

        // Send to the provider (full context)
        let response = self.provider.send_message(request).await?;

        // Update state
        self.state.last_provider_message_count = total_messages;
        self.state.total_input_tokens += estimated_total_tokens;
        self.state.estimated_cached_tokens += estimated_cached_tokens;
        self.state.provider_call_count += 1;

        Ok(response)
    }

    /// Send message with streaming response
    pub async fn send_message_stream(
        &mut self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        // Track metrics (same as non-streaming)
        let total_messages = request.messages.len();
        let new_messages = total_messages.saturating_sub(self.state.last_provider_message_count);
        let repeated_messages = total_messages - new_messages;

        tracing::info!(
            provider = %self.provider.name(),
            call_count = self.state.provider_call_count + 1,
            total_messages,
            new_messages,
            repeated_messages,
            "Provider streaming context metrics"
        );

        let receiver = self.provider.send_message_stream(request).await?;

        // Update state
        self.state.last_provider_message_count = total_messages;
        self.state.provider_call_count += 1;

        Ok(receiver)
    }

    /// Get current conversation state (for metrics/debugging)
    #[cfg(test)]
    pub(crate) fn state(&self) -> &ConversationState {
        &self.state
    }

    /// Get the provider name
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Reset conversation state (e.g., when starting new conversation)
    #[cfg(test)]
    pub fn reset_state(&mut self) {
        self.state = ConversationState::default();
    }

    // ==================== Level 2: Basic - Optional Truncation ====================

    /// Truncate conversation to recent turns only
    #[cfg(test)]
    fn truncate_context(&self, request: &ProviderRequest) -> ProviderRequest {
        if self.config.max_context_turns == 0 || request.messages.is_empty() {
            return request.clone();
        }

        let max_messages = self.config.max_context_turns * 2; // user + assistant per turn

        let messages = if request.messages.len() > max_messages {
            // Keep system messages + recent turns
            let mut result = Vec::new();

            // Preserve system messages (role = "system")
            for msg in &request.messages {
                if msg.role == "system" {
                    result.push(msg.clone());
                }
            }

            // Add recent conversation
            let start_idx = request.messages.len() - max_messages;
            for msg in &request.messages[start_idx..] {
                if msg.role != "system" {
                    result.push(msg.clone());
                }
            }

            result
        } else {
            request.messages.clone()
        };

        ProviderRequest {
            messages,
            ..request.clone()
        }
    }

    // ==================== Level 3: Full - Smart Strategies ====================

    /// Send message with smart context optimization (Level 3: Full)
    ///
    /// Applies multiple optimization strategies:
    /// - Preserves system prompts
    /// - Drops old tool results (configurable retention)
    /// - Truncates to recent turns
    /// - Adds prompt caching hints (for Claude/Gemini)
    pub async fn send_message_with_optimization(
        &mut self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse> {
        let optimized_request = self.optimize_context(request);

        // Detailed metrics
        let original_messages = request.messages.len();
        let optimized_messages = optimized_request.messages.len();
        let dropped = original_messages - optimized_messages;

        let original_tool_results = count_tool_results(&request.messages);
        let optimized_tool_results = count_tool_results(&optimized_request.messages);
        let dropped_tool_results = original_tool_results - optimized_tool_results;

        tracing::info!(
            provider = %self.provider.name(),
            original_messages,
            optimized_messages,
            dropped,
            original_tool_results,
            optimized_tool_results,
            dropped_tool_results,
            "Context optimized with smart strategies"
        );

        self.send_message(&optimized_request).await
    }

    /// Apply all optimization strategies
    fn optimize_context(&self, request: &ProviderRequest) -> ProviderRequest {
        let mut messages = request.messages.clone();

        // Strategy 1: Drop old tool results (keep recent N turns)
        if self.config.tool_result_retention_turns > 0 {
            messages = self.drop_old_tool_results(&messages);
        }

        // Strategy 2: Truncate to max turns (preserving system prompts)
        if self.config.max_context_turns > 0 {
            messages = self.truncate_with_system_preserved(&messages);
        }

        ProviderRequest {
            messages,
            ..request.clone()
        }
    }

    /// Drop tool results older than configured retention period
    fn drop_old_tool_results(&self, messages: &[Message]) -> Vec<Message> {
        if messages.is_empty() {
            return messages.to_vec();
        }

        let retention_count = self.config.tool_result_retention_turns * 2;
        let cutoff_index = messages.len().saturating_sub(retention_count);

        messages
            .iter()
            .enumerate()
            .map(|(idx, msg)| {
                if idx < cutoff_index {
                    // Drop tool results from old messages
                    Message {
                        role: msg.role.clone(),
                        content: msg
                            .content
                            .iter()
                            .filter(|block| !matches!(block, ContentBlock::ToolResult { .. }))
                            .cloned()
                            .collect(),
                    }
                } else {
                    // Keep recent messages as-is
                    msg.clone()
                }
            })
            .filter(|msg| !msg.content.is_empty()) // Drop empty messages
            .collect()
    }

    /// Truncate to recent turns, always preserving system prompts
    fn truncate_with_system_preserved(&self, messages: &[Message]) -> Vec<Message> {
        if messages.is_empty() || self.config.max_context_turns == 0 {
            return messages.to_vec();
        }

        let max_messages = self.config.max_context_turns * 2;

        if messages.len() <= max_messages {
            return messages.to_vec();
        }

        let mut result = Vec::new();

        // Step 1: Collect all system messages
        let system_messages: Vec<Message> = messages
            .iter()
            .filter(|msg| msg.role == "system")
            .cloned()
            .collect();

        // Step 2: Get recent non-system messages
        let non_system_messages: Vec<&Message> =
            messages.iter().filter(|msg| msg.role != "system").collect();

        let recent_start = non_system_messages.len().saturating_sub(max_messages);
        let recent_messages: Vec<Message> = non_system_messages[recent_start..]
            .iter()
            .map(|&m| m.clone())
            .collect();

        // Step 3: Combine (system first, then recent)
        result.extend(system_messages);
        result.extend(recent_messages);

        result
    }
}

/// Count tool results in messages
fn count_tool_results(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|msg| &msg.content)
        .filter(|block| matches!(block, ContentBlock::ToolResult { .. }))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentBlock;
    use crate::{
        CapabilitySupport, ModelCapabilities, ProviderBackend, ReasoningCapability,
        ValidatedProviderRequest,
    };

    // Mock provider for testing
    struct MockProvider;

    #[async_trait::async_trait]
    impl ProviderBackend for MockProvider {
        async fn send_message_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            Ok(ProviderResponse {
                id: "test".to_string(),
                model: "test".to_string(),
                content: vec![ContentBlock::Text {
                    text: "test response".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                role: "assistant".to_string(),
                provider: "mock".to_string(),
                usage: None,
                allowance: None,
            })
        }

        async fn send_message_stream_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(StreamChunk::TextDelta("test".to_string())))
                    .await;
            });
            Ok(rx)
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn default_model(&self) -> &str {
            "mock-model"
        }

        fn capabilities(&self, model: &str) -> ModelCapabilities {
            ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-08-26",
                "test fixture",
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-08-26", "test fixture"),
                Some(1_000),
                Some(10_000),
                None,
            )
        }
    }

    #[tokio::test]
    async fn test_context_tracking() {
        let provider = Box::new(MockProvider);
        let mut session = ProviderSession::new(provider);

        // First call: 2 messages (new)
        let request1 = ProviderRequest {
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Hello".to_string(),
                    }],
                },
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Hi!".to_string(),
                    }],
                },
            ],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        session.send_message(&request1).await.unwrap();

        assert_eq!(session.state().last_provider_message_count, 2);
        assert_eq!(session.state().provider_call_count, 1);

        // Second call: 4 messages (2 new, 2 repeated)
        let request2 = ProviderRequest {
            messages: vec![
                Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Hello".to_string(),
                    }],
                },
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Hi!".to_string(),
                    }],
                },
                Message {
                    role: "user".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "How are you?".to_string(),
                    }],
                },
                Message {
                    role: "assistant".to_string(),
                    content: vec![ContentBlock::Text {
                        text: "Good!".to_string(),
                    }],
                },
            ],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        session.send_message(&request2).await.unwrap();

        assert_eq!(session.state().last_provider_message_count, 4);
        assert_eq!(session.state().provider_call_count, 2);
        // Estimated: 400 total tokens, 200 cached
        assert!(session.state().estimated_cached_tokens >= 200);
    }

    #[tokio::test]
    async fn test_reset_state() {
        let provider = Box::new(MockProvider);
        let mut session = ProviderSession::new(provider);

        let request = ProviderRequest {
            messages: vec![Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "test".to_string(),
                }],
            }],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        session.send_message(&request).await.unwrap();
        assert_eq!(session.state().provider_call_count, 1);

        session.reset_state();
        assert_eq!(session.state().provider_call_count, 0);
        assert_eq!(session.state().last_provider_message_count, 0);
    }

    // Level 2 tests
    #[tokio::test]
    async fn test_context_truncation() {
        let provider = Box::new(MockProvider);
        let config = SessionContextConfig {
            max_context_turns: 2, // Only keep 2 turns (4 messages)
            tool_result_retention_turns: 0,
            prompt_caching_enabled: true,
        };
        let mut session = ProviderSession::with_config(provider, config);

        // Create 6 messages (3 turns)
        let messages = vec![
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 1 user".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 1 assistant".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 2 user".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 2 assistant".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 3 user".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Turn 3 assistant".to_string(),
                }],
            },
        ];

        let request = ProviderRequest {
            messages: messages.clone(),
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        let truncated = session.truncate_context(&request);

        // Should keep only last 4 messages (2 turns)
        assert_eq!(truncated.messages.len(), 4);
        assert_eq!(
            truncated.messages[0].content[0].as_text().unwrap(),
            "Turn 2 user"
        );
        assert_eq!(
            truncated.messages[3].content[0].as_text().unwrap(),
            "Turn 3 assistant"
        );
    }

    #[tokio::test]
    async fn test_system_prompt_preservation() {
        let provider = Box::new(MockProvider);
        let config = SessionContextConfig {
            max_context_turns: 1, // Only keep 1 turn (2 messages)
            tool_result_retention_turns: 0,
            prompt_caching_enabled: true,
        };
        let mut session = ProviderSession::with_config(provider, config);

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: vec![ContentBlock::Text {
                    text: "System prompt".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Old message".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Old response".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Recent message".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Recent response".to_string(),
                }],
            },
        ];

        let request = ProviderRequest {
            messages,
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        let truncated = session.truncate_context(&request);

        // Should preserve system prompt + last turn (3 messages total)
        assert_eq!(truncated.messages.len(), 3);
        assert_eq!(truncated.messages[0].role, "system");
        assert_eq!(
            truncated.messages[1].content[0].as_text().unwrap(),
            "Recent message"
        );
    }

    // Level 3 tests
    #[tokio::test]
    async fn test_drop_old_tool_results() {
        let provider = Box::new(MockProvider);
        let config = SessionContextConfig {
            max_context_turns: 0,
            tool_result_retention_turns: 1, // Only keep last turn's tool results
            prompt_caching_enabled: true,
        };
        let session = ProviderSession::with_config(provider, config);

        let messages = vec![
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Read file 1".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call1".to_string(),
                        content: "Old file contents".to_string(),
                        is_error: Some(false),
                    },
                ],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Read file 2".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![
                    ContentBlock::ToolUse {
                        id: "call2".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({}),
                    },
                    ContentBlock::ToolResult {
                        tool_use_id: "call2".to_string(),
                        content: "Recent file contents".to_string(),
                        is_error: Some(false),
                    },
                ],
            },
        ];

        let optimized = session.drop_old_tool_results(&messages);

        // Old tool result should be dropped
        assert_eq!(optimized.len(), 4);

        // First assistant message should only have ToolUse (result dropped)
        assert_eq!(optimized[1].content.len(), 1);
        assert!(matches!(
            optimized[1].content[0],
            ContentBlock::ToolUse { .. }
        ));

        // Recent assistant message should have both ToolUse and ToolResult
        assert_eq!(optimized[3].content.len(), 2);
        assert!(matches!(
            optimized[3].content[0],
            ContentBlock::ToolUse { .. }
        ));
        assert!(matches!(
            optimized[3].content[1],
            ContentBlock::ToolResult { .. }
        ));
    }

    #[tokio::test]
    async fn test_full_optimization() {
        let provider = Box::new(MockProvider);
        let config = SessionContextConfig {
            max_context_turns: 2,           // Keep 2 turns
            tool_result_retention_turns: 1, // Keep 1 turn of tool results
            prompt_caching_enabled: true,
        };
        let mut session = ProviderSession::with_config(provider, config);

        let messages = vec![
            Message {
                role: "system".to_string(),
                content: vec![ContentBlock::Text {
                    text: "System".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Old query".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "old".to_string(),
                    content: "Old result".to_string(),
                    is_error: Some(false),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Recent query 1".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Recent response 1".to_string(),
                }],
            },
            Message {
                role: "user".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Recent query 2".to_string(),
                }],
            },
            Message {
                role: "assistant".to_string(),
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "recent".to_string(),
                    content: "Recent result".to_string(),
                    is_error: Some(false),
                }],
            },
        ];

        let request = ProviderRequest {
            messages,
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            cancellation_token: None,
            system: None,
            tool_policy: Default::default(),
        };

        let optimized = session.optimize_context(&request);

        // Should keep: system + last 2 turns (5 messages)
        // Old tool result should be dropped
        assert_eq!(optimized.messages.len(), 5);
        assert_eq!(optimized.messages[0].role, "system");

        // Recent tool result should be kept
        let has_recent_tool_result = optimized
            .messages
            .iter()
            .any(|msg| msg.content.iter().any(|block| {
                matches!(block, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "recent")
            }));
        assert!(has_recent_tool_result);
    }
}
