// HTTP client for Claude API
//
// This client now acts as a facade over the provider system, allowing
// different LLM providers (Claude, OpenAI, Grok, etc.) to be used interchangeably.

use anyhow::{Context, Result};
use tokio::sync::mpsc;

use super::types::{MessageRequest, MessageResponse};
use crate::generators::StreamChunk;
use crate::providers::{claude::ClaudeProvider, LlmProvider, ProviderRequest};

#[derive(Clone)]
pub struct ClaudeClient {
    provider: std::sync::Arc<dyn LlmProvider>,
}

impl ClaudeClient {
    /// Create a new ClaudeClient with Claude as the provider (for backwards compatibility)
    pub fn new(api_key: String) -> Result<Self> {
        let provider = ClaudeProvider::new(api_key)?;
        Ok(Self {
            provider: std::sync::Arc::new(provider),
        })
    }

    /// Create a ClaudeClient with a custom provider
    pub fn with_provider(provider: Box<dyn LlmProvider>) -> Self {
        Self {
            provider: std::sync::Arc::from(provider),
        }
    }

    /// Get the provider name
    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    /// Convert MessageRequest to ProviderRequest
    fn to_provider_request(&self, request: &MessageRequest) -> ProviderRequest {
        let mut provider_req = ProviderRequest::new(request.messages.clone())
            .with_model(request.model.clone())
            .with_max_tokens(request.max_tokens);

        if let Some(system) = &request.system {
            provider_req = provider_req.with_system(system.clone());
        }

        if let Some(tools) = &request.tools {
            provider_req = provider_req.with_tools(tools.clone());
        }

        provider_req
    }

    /// Send a message to the configured provider with retry logic
    pub async fn send_message(&self, request: &MessageRequest) -> Result<MessageResponse> {
        let provider_request = self.to_provider_request(request);
        let provider_response = self.provider.send_message(&provider_request).await?;

        // Convert ProviderResponse to MessageResponse
        Ok(provider_response.into())
    }

    /// Send a message with streaming response
    /// Returns a channel that receives StreamChunk items (text deltas or complete blocks)
    pub async fn send_message_stream(
        &self,
        request: &MessageRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let provider_request = self.to_provider_request(request).with_stream(true);
        self.provider.send_message_stream(&provider_request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_creation() {
        let client = ClaudeClient::new("test-key".to_string());
        assert!(client.is_ok());
    }

    #[test]
    fn test_message_request_creation() {
        let request = MessageRequest::new("Hello");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, "user");
        assert_eq!(request.messages[0].text(), "Hello");
    }
}
