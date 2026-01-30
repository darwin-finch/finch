// HTTP client for Claude API

use anyhow::{Context, Result};
use reqwest::Client;
use std::time::Duration;

use super::retry::with_retry;
use super::types::{MessageRequest, MessageResponse};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT_SECS: u64 = 60;

pub struct ClaudeClient {
    client: Client,
    api_key: String,
}

impl ClaudeClient {
    pub fn new(api_key: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self { client, api_key })
    }

    /// Send a message to Claude API with retry logic
    pub async fn send_message(&self, request: &MessageRequest) -> Result<MessageResponse> {
        with_retry(|| self.send_message_once(request)).await
    }

    /// Send a single message request (no retry)
    async fn send_message_once(&self, request: &MessageRequest) -> Result<MessageResponse> {
        tracing::debug!("Sending request to Claude API: {:?}", request);

        let response = self
            .client
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(request)
            .send()
            .await
            .context("Failed to send request to Claude API")?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Claude API request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        let message_response: MessageResponse = response
            .json()
            .await
            .context("Failed to parse Claude API response")?;

        tracing::debug!("Received response: {:?}", message_response);

        Ok(message_response)
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
        assert_eq!(request.messages[0].content, "Hello");
    }
}
