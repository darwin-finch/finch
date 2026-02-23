// Multi-provider LLM support
//
// This module provides an abstraction layer over different LLM providers
// (Claude, OpenAI, Grok, Gemini, etc.) allowing users to choose their
// preferred API provider while maintaining a unified interface.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc::Receiver;

pub mod types;

// Provider implementations
pub mod claude;
pub mod gemini;
pub mod openai;

// Provider factory
pub mod factory;

// Fallback chain (not used in student-teacher architecture)
pub mod fallback_chain;

// Teacher session management with context optimization
pub mod teacher_session;

// Universal alignment prompt for cross-provider behavioral consistency
pub mod alignment;
pub use alignment::{with_alignment, UNIVERSAL_ALIGNMENT_PROMPT};

// Re-export commonly used types
pub use factory::{
    create_provider, create_provider_from_entries, create_provider_from_entry,
    create_provider_from_teacher, create_providers, create_providers_from_entries,
};
pub use fallback_chain::FallbackChain;
pub use teacher_session::{
    ConversationState, OptimizationStats, TeacherContextConfig, TeacherSession,
};
pub use types::{ProviderRequest, ProviderResponse, StreamChunk};

/// Trait for LLM providers
///
/// All LLM providers (Claude, OpenAI, Gemini, etc.) implement this trait,
/// providing a unified interface for sending messages and streaming responses.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a message and get a complete response
    ///
    /// This is the non-streaming version that waits for the full response.
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse>;

    /// Send a message and stream the response
    ///
    /// Returns a channel that receives StreamChunk items (text deltas or complete blocks).
    /// The channel will be closed when the stream is complete.
    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>>;

    /// Get the provider name (e.g., "claude", "openai", "gemini")
    fn name(&self) -> &str;

    /// Get the default model for this provider
    fn default_model(&self) -> &str;

    /// Check if the provider supports streaming
    fn supports_streaming(&self) -> bool {
        true // Most providers support streaming
    }

    /// Check if the provider supports tool/function calling
    fn supports_tools(&self) -> bool {
        true // Most modern providers support tools
    }

    /// Maximum context window in tokens for this provider.
    ///
    /// Used by the fallback chain to truncate conversation history before
    /// sending, preventing 400 "prompt too long" errors.
    fn context_limit_tokens(&self) -> usize {
        128_000 // Conservative default; providers override as needed
    }
}

/// Helper to convert provider response to format compatible with existing code
impl From<ProviderResponse> for crate::claude::types::MessageResponse {
    fn from(response: ProviderResponse) -> Self {
        Self {
            id: response.id,
            response_type: "message".to_string(),
            role: response.role,
            content: response.content,
            model: response.model,
            stop_reason: response.stop_reason,
        }
    }
}
