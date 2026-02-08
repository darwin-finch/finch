// HTTP client for Claude API

use anyhow::{Context, Result};
use futures::stream::StreamExt;
use reqwest::Client;
use std::time::Duration;
use tokio::sync::mpsc;

use super::retry::with_retry;
use super::streaming::StreamEvent;
use super::types::{MessageRequest, MessageResponse};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const REQUEST_TIMEOUT_SECS: u64 = 60;

#[derive(Clone)]
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

    /// Send a message with streaming response (with retry logic)
    /// Returns a channel that receives text chunks as they arrive
    pub async fn send_message_stream(
        &self,
        request: &MessageRequest,
    ) -> Result<mpsc::Receiver<Result<String>>> {
        with_retry(|| self.send_message_stream_once(request)).await
    }

    /// Send a message with streaming response (no retry)
    /// Returns a channel that receives text chunks as they arrive
    async fn send_message_stream_once(
        &self,
        request: &MessageRequest,
    ) -> Result<mpsc::Receiver<Result<String>>> {
        let (tx, rx) = mpsc::channel(100);

        // Clone request and add stream: true
        let mut request_json = serde_json::to_value(request)?;
        request_json["stream"] = serde_json::json!(true);

        tracing::debug!("Sending streaming request to Claude API");

        let response = self
            .client
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&request_json)
            .send()
            .await
            .context("Failed to send streaming request to Claude API")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "Claude API streaming request failed\n\nStatus: {}\nBody: {}",
                status,
                error_body
            );
        }

        // Spawn task to parse SSE stream
        tokio::spawn(async move {
            let mut stream = response.bytes_stream();
            let mut buffer = Vec::new();

            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(bytes) => {
                        buffer.extend_from_slice(&bytes);

                        // Parse line by line
                        while let Some(newline_pos) = buffer.iter().position(|&b| b == b'\n') {
                            let line_bytes: Vec<u8> = buffer.drain(..=newline_pos).collect();
                            let line = String::from_utf8_lossy(&line_bytes);

                            // SSE format: "data: {...}\n"
                            if let Some(json_str) = line.strip_prefix("data: ") {
                                let json_str = json_str.trim();

                                // Check for end marker
                                if json_str == "[DONE]" {
                                    tracing::debug!("Stream completed");
                                    break;
                                }

                                // Parse event and extract text
                                if let Ok(event) = serde_json::from_str::<StreamEvent>(json_str) {
                                    // Check for tool_use blocks - signal error to abort streaming
                                    if event.is_tool_use_start() {
                                        tracing::debug!("Tool use detected in stream, aborting");
                                        let _ =
                                            tx.send(Err(anyhow::anyhow!("TOOLS_DETECTED"))).await;
                                        break;
                                    }

                                    if event.is_text_delta() {
                                        if let Some(text) = event.text() {
                                            if tx.send(Ok(text.to_string())).await.is_err() {
                                                // Receiver dropped, stop streaming
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e.into())).await;
                        break;
                    }
                }
            }
        });

        Ok(rx)
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
