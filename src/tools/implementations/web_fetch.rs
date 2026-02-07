// WebFetch tool - fetches content from URLs

use crate::tools::registry::Tool;
use crate::tools::types::{ToolContext, ToolInputSchema};
use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest;
use serde_json::Value;

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .user_agent("Shammah/0.1.0")
                .build()
                .unwrap(),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch content from a URL. Use for retrieving web pages or API data."
    }

    fn input_schema(&self) -> ToolInputSchema {
        ToolInputSchema::simple(vec![
            ("url", "The URL to fetch"),
            ("prompt", "What information to extract (optional)"),
        ])
    }

    async fn execute(&self, input: Value, _context: &ToolContext<'_>) -> Result<String> {
        let url = input["url"].as_str().context("Missing url parameter")?;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to fetch URL: {}", url))?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("HTTP error {}: {}", status, url);
        }

        let body = response.text().await?;

        // Limit to 10,000 chars
        if body.len() > 10_000 {
            Ok(format!(
                "{}\n\n[Response truncated - showing first 10,000 characters of {}]",
                &body[..10_000],
                body.len()
            ))
        } else {
            Ok(body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_web_fetch_google() {
        let tool = WebFetchTool::new();
        let input = serde_json::json!({
            "url": "https://www.google.com"
        });

        let result = tool.execute(input).await;
        // This might fail without internet, so just check it doesn't panic
        if let Ok(content) = result {
            assert!(!content.is_empty());
        }
    }

    #[tokio::test]
    async fn test_web_fetch_invalid_url() {
        let tool = WebFetchTool::new();
        let input = serde_json::json!({
            "url": "https://this-domain-definitely-does-not-exist-12345.com"
        });

        let result = tool.execute(input).await;
        // Should fail with network error
        assert!(result.is_err());
    }
}
