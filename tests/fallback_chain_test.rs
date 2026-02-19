// Test FallbackChain provider-specific model ID handling
//
// This test suite verifies that:
// 1. Each provider in the chain uses its own model ID
// 2. Model IDs are not inherited from the first provider
// 3. Fallback logic preserves provider-specific configuration

use anyhow::Result;
use finch::providers::{LlmProvider, ProviderRequest};
use finch::claude::{Message, ContentBlock};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Mock provider that tracks what model ID it received
#[derive(Clone)]
struct MockProvider {
    name: String,
    model: String,
    should_fail: bool,
    received_model: Arc<Mutex<Option<String>>>,
}

impl MockProvider {
    fn new(name: &str, model: &str, should_fail: bool) -> Self {
        Self {
            name: name.to_string(),
            model: model.to_string(),
            should_fail,
            received_model: Arc::new(Mutex::new(None)),
        }
    }

    async fn get_received_model(&self) -> Option<String> {
        self.received_model.lock().await.clone()
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn send_message(
        &self,
        request: &ProviderRequest,
    ) -> Result<shammah::providers::ProviderResponse> {
        // Store the model ID we received
        *self.received_model.lock().await = Some(request.model.clone());

        if self.should_fail {
            anyhow::bail!("Mock provider {} failed", self.name);
        }

        Ok(shammah::providers::ProviderResponse {
            id: format!("test-{}", self.name),
            model: request.model.clone(),
            content: vec![ContentBlock::Text {
                text: format!("Response from {}", self.name),
            }],
            stop_reason: Some("end_turn".to_string()),
            role: "assistant".to_string(),
            provider: self.name.clone(),
        })
    }

    async fn send_message_stream(
        &self,
        _request: &ProviderRequest,
    ) -> Result<tokio::sync::mpsc::Receiver<Result<shammah::providers::StreamChunk>>> {
        anyhow::bail!("Streaming not implemented for mock")
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self) -> bool {
        false
    }
}

/// Test that each provider in FallbackChain gets its own model ID
#[tokio::test]
async fn test_fallback_chain_uses_provider_specific_models() -> Result<()> {
    // Create mock providers with different model IDs
    let gemini = MockProvider::new("gemini", "gemini-2.5-flash", true); // Will fail
    let claude = MockProvider::new("claude", "claude-sonnet-4", false); // Will succeed

    let gemini_model_tracker = gemini.received_model.clone();
    let claude_model_tracker = claude.received_model.clone();

    // Create fallback chain
    let providers: Vec<Box<dyn LlmProvider>> = vec![
        Box::new(gemini),
        Box::new(claude),
    ];
    let chain = shammah::providers::FallbackChain::new(providers);

    // Create a request with a DIFFERENT model ID (simulating what the first provider might set)
    let request = ProviderRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Test query".to_string(),
            }],
        }],
        model: "gpt-4".to_string(), // Wrong model for both providers!
        max_tokens: 100,
        tools: None,
        temperature: None,
        stream: false,
    };

    // Send message through chain
    let response = chain.send_message(&request).await?;

    // Verify that Gemini received its own model ID (not "gpt-4")
    let gemini_model = gemini_model_tracker.lock().await.clone();
    assert_eq!(
        gemini_model,
        Some("gemini-2.5-flash".to_string()),
        "Gemini should receive its own model ID, not the request model"
    );

    // Verify that Claude received its own model ID (not "gpt-4")
    let claude_model = claude_model_tracker.lock().await.clone();
    assert_eq!(
        claude_model,
        Some("claude-sonnet-4".to_string()),
        "Claude should receive its own model ID, not the request model"
    );

    // Verify the response came from Claude (fallback succeeded)
    assert_eq!(response.provider, "claude");
    assert!(response.content[0].to_string().contains("Response from claude"));

    Ok(())
}

/// Test that FallbackChain tries all providers in order
#[tokio::test]
async fn test_fallback_chain_tries_providers_in_order() -> Result<()> {
    let provider1 = MockProvider::new("provider1", "model1", true);
    let provider2 = MockProvider::new("provider2", "model2", true);
    let provider3 = MockProvider::new("provider3", "model3", false);

    let tracker1 = provider1.received_model.clone();
    let tracker2 = provider2.received_model.clone();
    let tracker3 = provider3.received_model.clone();

    let providers: Vec<Box<dyn LlmProvider>> = vec![
        Box::new(provider1),
        Box::new(provider2),
        Box::new(provider3),
    ];
    let chain = shammah::providers::FallbackChain::new(providers);

    let request = ProviderRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Test".to_string(),
            }],
        }],
        model: "default".to_string(),
        max_tokens: 100,
        tools: None,
        temperature: None,
        stream: false,
    };

    let response = chain.send_message(&request).await?;

    // All three providers should have been tried
    assert!(tracker1.lock().await.is_some(), "Provider 1 should have been tried");
    assert!(tracker2.lock().await.is_some(), "Provider 2 should have been tried");
    assert!(tracker3.lock().await.is_some(), "Provider 3 should have been tried");

    // Response should be from provider 3
    assert_eq!(response.provider, "provider3");

    Ok(())
}

/// Test that FallbackChain fails if all providers fail
#[tokio::test]
async fn test_fallback_chain_fails_when_all_providers_fail() -> Result<()> {
    let provider1 = MockProvider::new("provider1", "model1", true);
    let provider2 = MockProvider::new("provider2", "model2", true);

    let providers: Vec<Box<dyn LlmProvider>> = vec![
        Box::new(provider1),
        Box::new(provider2),
    ];
    let chain = shammah::providers::FallbackChain::new(providers);

    let request = ProviderRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Test".to_string(),
            }],
        }],
        model: "default".to_string(),
        max_tokens: 100,
        tools: None,
        temperature: None,
        stream: false,
    };

    let result = chain.send_message(&request).await;

    // Should fail because all providers failed
    assert!(result.is_err(), "Chain should fail when all providers fail");
    assert!(result.unwrap_err().to_string().contains("All fallback providers failed"));

    Ok(())
}

/// Test that FallbackChain succeeds on first provider if it works
#[tokio::test]
async fn test_fallback_chain_uses_first_provider_when_available() -> Result<()> {
    let provider1 = MockProvider::new("provider1", "model1", false); // Works!
    let provider2 = MockProvider::new("provider2", "model2", false);

    let tracker2 = provider2.received_model.clone();

    let providers: Vec<Box<dyn LlmProvider>> = vec![
        Box::new(provider1),
        Box::new(provider2),
    ];
    let chain = shammah::providers::FallbackChain::new(providers);

    let request = ProviderRequest {
        messages: vec![Message {
            role: "user".to_string(),
            content: vec![ContentBlock::Text {
                text: "Test".to_string(),
            }],
        }],
        model: "default".to_string(),
        max_tokens: 100,
        tools: None,
        temperature: None,
        stream: false,
    };

    let response = chain.send_message(&request).await?;

    // Should use provider 1 (first in chain)
    assert_eq!(response.provider, "provider1");

    // Provider 2 should NOT have been tried
    assert!(tracker2.lock().await.is_none(), "Provider 2 should not be tried if provider 1 succeeds");

    Ok(())
}
