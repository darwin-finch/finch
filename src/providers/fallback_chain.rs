// Fallback chain for automatic provider retry
//
// Tries providers in priority order until one succeeds

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::mpsc;

use super::{
    resolve_effective_request, validate_provider_request, CapabilityProvenance, CapabilitySupport,
    LlmProvider, ModelCapabilities, ModelFeature, ProviderBackend, ProviderRequest,
    ProviderResponse, StreamChunk, ValidatedProviderRequest,
};

/// A chain of providers to try in order
pub struct FallbackChain {
    providers: Arc<Vec<Arc<dyn LlmProvider>>>,
}

impl FallbackChain {
    /// Create a new fallback chain with providers in priority order
    pub fn new(providers: Vec<Box<dyn LlmProvider>>) -> Self {
        Self::from_shared(providers.into_iter().map(Arc::from).collect())
    }

    /// Create a fallback chain without reconstructing already validated providers.
    pub fn from_shared(providers: Vec<Arc<dyn LlmProvider>>) -> Self {
        Self {
            providers: Arc::new(providers),
        }
    }

    /// Get the number of providers in the chain
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Check if the chain is empty
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Get the primary provider (first in chain)
    pub fn primary_provider(&self) -> Option<&dyn LlmProvider> {
        self.providers.first().map(|p| p.as_ref())
    }

    /// Try sending message with automatic fallback
    pub async fn send_message_with_fallback(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse> {
        let mut last_error = None;

        for (idx, provider) in self.providers.iter().enumerate() {
            let mut candidate = request.clone();
            candidate.model = provider.default_model().to_string();
            let (mut provider_request, capabilities) =
                match resolve_effective_request(provider.as_ref(), &candidate) {
                    Ok(resolved) => resolved,
                    Err(error) => {
                        tracing::info!(
                            provider = provider.name(),
                            model = candidate.model,
                            error = %error,
                            "Skipping provider because its model is ineligible for this turn"
                        );
                        last_error = Some(error);
                        continue;
                    }
                };
            tracing::debug!(
                "Trying provider {} ({}/{})",
                provider.name(),
                idx + 1,
                self.providers.len()
            );

            // Create a modified request with this provider's model ID,
            // sanitizing orphaned tool_use blocks from previous failed providers
            if let Some(context_limit) = capabilities.context_window.max_tokens {
                let dropped = provider_request.truncate_to_context_limit(context_limit);
                if dropped > 0 {
                    tracing::debug!(
                        provider = provider.name(),
                        dropped_messages = dropped,
                        context_limit,
                        "Truncated conversation history to fit provider context window"
                    );
                }
            }
            provider_request.sanitize_messages();
            let validated =
                match validate_provider_request(provider.as_ref(), &provider_request, false) {
                    Ok(validated) => validated,
                    Err(error) => {
                        last_error = Some(error);
                        continue;
                    }
                };

            match provider.send_message_validated(validated).await {
                Ok(response) => {
                    if idx > 0 {
                        tracing::info!(
                            "Provider {} succeeded after {} failed attempts",
                            provider.name(),
                            idx
                        );
                    } else {
                        tracing::debug!("Primary provider {} succeeded", provider.name());
                    }
                    return Ok(response);
                }
                Err(e) => {
                    tracing::warn!(
                        "Provider {} failed (attempt {}/{}): {}",
                        provider.name(),
                        idx + 1,
                        self.providers.len(),
                        e
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("No providers available"))
            .context("All fallback providers failed"))
    }

    /// Try streaming with automatic fallback
    ///
    /// Tries providers in sequence until one succeeds at the send_message_stream level.
    /// Returns the first successful provider's receiver directly (no wrapper task).
    /// Mid-stream errors are handled by the event loop via QueryFailed.
    pub async fn send_message_stream_with_fallback(
        &self,
        request: &ProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let mut last_error = None;

        for (idx, provider) in self.providers.iter().enumerate() {
            let mut candidate = request.clone();
            candidate.model = provider.default_model().to_string();
            candidate.stream = true;
            let (mut provider_request, capabilities) = match resolve_effective_request(
                provider.as_ref(),
                &candidate,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    tracing::info!(
                        provider = provider.name(),
                        model = candidate.model,
                        error = %error,
                        "Skipping provider because its model is ineligible for this streaming turn"
                    );
                    last_error = Some(error);
                    continue;
                }
            };
            tracing::debug!(
                "Trying streaming with provider {} ({}/{})",
                provider.name(),
                idx + 1,
                self.providers.len()
            );

            // Create a modified request with this provider's model ID,
            // sanitizing orphaned tool_use blocks from previous failed providers
            if let Some(context_limit) = capabilities.context_window.max_tokens {
                let dropped = provider_request.truncate_to_context_limit(context_limit);
                if dropped > 0 {
                    tracing::debug!(
                        provider = provider.name(),
                        dropped_messages = dropped,
                        context_limit,
                        "Truncated conversation history to fit provider context window"
                    );
                }
            }
            provider_request.sanitize_messages();
            let validated =
                match validate_provider_request(provider.as_ref(), &provider_request, true) {
                    Ok(validated) => validated,
                    Err(error) => {
                        last_error = Some(error);
                        continue;
                    }
                };

            match provider.send_message_stream_validated(validated).await {
                Ok(receiver) => {
                    if idx > 0 {
                        tracing::info!(
                            "Provider {} streaming succeeded after {} failed attempts",
                            provider.name(),
                            idx
                        );
                    } else {
                        tracing::debug!("Primary provider {} streaming succeeded", provider.name());
                    }
                    // Return provider's receiver DIRECTLY (no wrapper, no race condition)
                    return Ok(receiver);
                }
                Err(e) => {
                    tracing::warn!(
                        "Provider {} streaming failed (attempt {}/{}): {}",
                        provider.name(),
                        idx + 1,
                        self.providers.len(),
                        e
                    );
                    last_error = Some(e);
                    continue;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::anyhow!("No providers available for streaming"))
            .context("All fallback providers failed for streaming"))
    }
}

// Implement LlmProvider trait for FallbackChain
#[async_trait::async_trait]
impl ProviderBackend for FallbackChain {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        self.send_message_with_fallback(request.request()).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        self.send_message_stream_with_fallback(request.request())
            .await
    }

    fn name(&self) -> &str {
        self.primary_provider()
            .map(|p| p.name())
            .unwrap_or("FallbackChain")
    }

    fn default_model(&self) -> &str {
        self.primary_provider()
            .map(|p| p.default_model())
            .unwrap_or("default")
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        fn aggregate(
            providers: &[Arc<dyn LlmProvider>],
            select: impl Fn(&ModelCapabilities) -> CapabilitySupport,
        ) -> CapabilitySupport {
            let mut saw_unknown = false;
            for provider in providers {
                let capability = provider.capabilities(provider.default_model());
                match select(&capability) {
                    CapabilitySupport::Supported => return CapabilitySupport::Supported,
                    CapabilitySupport::Unknown => saw_unknown = true,
                    CapabilitySupport::Unsupported => {}
                }
            }
            if saw_unknown {
                CapabilitySupport::Unknown
            } else if providers.is_empty() {
                CapabilitySupport::Unknown
            } else {
                CapabilitySupport::Unsupported
            }
        }

        let mut capabilities = ModelCapabilities::unknown(self.name(), model);
        let configured = |support| ModelFeature {
            support,
            provenance: CapabilityProvenance::Configuration,
        };
        capabilities.streaming = configured(aggregate(&self.providers, |c| c.streaming.support));
        capabilities.tools = configured(aggregate(&self.providers, |c| c.tools.support));
        capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude::types::ContentBlock;
    use crate::tools::types::{ToolDefinition, ToolInputSchema};
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Mock provider for testing
    struct MockProvider {
        name: String,
        should_fail: bool,
    }

    struct NoToolProvider;

    struct ImplicitReasoningProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ProviderBackend for ImplicitReasoningProvider {
        async fn send_message_validated(
            &self,
            _: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("reasoning-ineligible provider must be skipped")
        }

        async fn send_message_stream_validated(
            &self,
            _: ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            panic!("reasoning-ineligible provider must be skipped")
        }

        fn name(&self) -> &str {
            "openai"
        }

        fn default_model(&self) -> &str {
            "gpt-4o"
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

        fn requested_reasoning_effort(
            &self,
            _: &ProviderRequest,
        ) -> Option<crate::config::ReasoningEffort> {
            Some(crate::config::ReasoningEffort::High)
        }
    }

    struct ModelBoundProvider {
        model: &'static str,
        tools: CapabilitySupport,
        streaming: CapabilitySupport,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl ProviderBackend for ModelBoundProvider {
        async fn send_message_validated(
            &self,
            request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request.request().model,
                self.model,
                "fallback rebound the selected profile's model"
            );
            Ok(ProviderResponse {
                id: "model-bound".into(),
                model: request.request().model.clone(),
                content: vec![ContentBlock::Text { text: "ok".into() }],
                stop_reason: Some("end_turn".into()),
                role: "assistant".into(),
                provider: "compatible".into(),
            })
        }

        async fn send_message_stream_validated(
            &self,
            request: ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                request.request().model,
                self.model,
                "fallback rebound the selected profile's model"
            );
            let (_tx, rx) = mpsc::channel(1);
            Ok(rx)
        }

        fn name(&self) -> &str {
            "compatible"
        }

        fn default_model(&self) -> &str {
            self.model
        }

        fn capabilities(&self, model: &str) -> ModelCapabilities {
            assert_eq!(model, self.model);
            ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-08-26",
                "test fixture",
                self.streaming,
                self.tools,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-08-26", "test fixture"),
                Some(1_000),
                Some(10_000),
                None,
            )
        }
    }

    #[async_trait::async_trait]
    impl ProviderBackend for NoToolProvider {
        async fn send_message_validated(
            &self,
            _: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            panic!("provider without tools must be skipped before invocation")
        }
        async fn send_message_stream_validated(
            &self,
            _: ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
            panic!("provider without tools must be skipped before invocation")
        }
        fn name(&self) -> &str {
            "no-tools"
        }
        fn default_model(&self) -> &str {
            "no-tools-model"
        }
        fn capabilities(&self, model: &str) -> super::ModelCapabilities {
            super::ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-08-26",
                "test fixture",
                super::CapabilitySupport::Supported,
                super::CapabilitySupport::Unsupported,
                super::CapabilitySupport::Unsupported,
                super::ReasoningCapability::unsupported("2026-08-26", "test fixture"),
                Some(1_000),
                Some(10_000),
                None,
            )
        }
    }

    impl MockProvider {
        fn new(name: &str, should_fail: bool) -> Self {
            Self {
                name: name.to_string(),
                should_fail,
            }
        }
    }

    #[async_trait::async_trait]
    impl ProviderBackend for MockProvider {
        async fn send_message_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            if self.should_fail {
                anyhow::bail!("Mock provider {} failed", self.name);
            }

            Ok(ProviderResponse {
                id: "test-id".to_string(),
                model: "test-model".to_string(),
                content: vec![ContentBlock::Text {
                    text: "Test response".to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                role: "assistant".to_string(),
                provider: self.name.clone(),
            })
        }

        async fn send_message_stream_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
            if self.should_fail {
                anyhow::bail!("Mock provider {} streaming failed", self.name);
            }

            let (tx, rx) = mpsc::channel(1);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(StreamChunk::TextDelta("test".to_string())))
                    .await;
            });
            Ok(rx)
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn default_model(&self) -> &str {
            "test-model"
        }

        fn capabilities(&self, model: &str) -> super::ModelCapabilities {
            super::ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-08-26",
                "test fixture",
                super::CapabilitySupport::Supported,
                super::CapabilitySupport::Supported,
                super::CapabilitySupport::Unsupported,
                super::ReasoningCapability::unsupported("2026-08-26", "test fixture"),
                Some(1_000),
                Some(10_000),
                None,
            )
        }
    }

    #[tokio::test]
    async fn test_primary_provider_succeeds() {
        let providers: Vec<Box<dyn LlmProvider>> = vec![
            Box::new(MockProvider::new("primary", false)),
            Box::new(MockProvider::new("fallback", false)),
        ];

        let chain = FallbackChain::new(providers);
        let request = ProviderRequest {
            messages: vec![],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            system: None,
        };

        let result = chain.send_message_with_fallback(&request).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().provider, "primary");
    }

    #[tokio::test]
    async fn test_fallback_to_secondary() {
        let providers: Vec<Box<dyn LlmProvider>> = vec![
            Box::new(MockProvider::new("primary", true)),
            Box::new(MockProvider::new("fallback", false)),
        ];

        let chain = FallbackChain::new(providers);
        let request = ProviderRequest {
            messages: vec![],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            system: None,
        };

        let result = chain.send_message_with_fallback(&request).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().provider, "fallback");
    }

    #[tokio::test]
    async fn test_all_providers_fail() {
        let providers: Vec<Box<dyn LlmProvider>> = vec![
            Box::new(MockProvider::new("primary", true)),
            Box::new(MockProvider::new("fallback", true)),
        ];

        let chain = FallbackChain::new(providers);
        let request = ProviderRequest {
            messages: vec![],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: false,
            system: None,
        };

        let result = chain.send_message_with_fallback(&request).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_streaming_fallback() {
        let providers: Vec<Box<dyn LlmProvider>> = vec![
            Box::new(MockProvider::new("primary", true)),
            Box::new(MockProvider::new("fallback", false)),
        ];

        let chain = FallbackChain::new(providers);
        let request = ProviderRequest {
            messages: vec![],
            model: String::new(),
            max_tokens: 100,
            temperature: None,
            tools: None,
            stream: true,
            system: None,
        };

        let result = chain.send_message_stream_with_fallback(&request).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn tool_turn_skips_incapable_primary_before_invocation() {
        let chain = FallbackChain::new(vec![
            Box::new(NoToolProvider),
            Box::new(MockProvider::new("grok-fallback", false)),
        ]);
        let mut request = ProviderRequest::new(vec![]);
        request.tools = Some(vec![ToolDefinition {
            name: "lookup".into(),
            description: "look up a value".into(),
            input_schema: ToolInputSchema::simple(vec![("key", "lookup key")]),
        }]);
        let response = chain.send_message_with_fallback(&request).await.unwrap();
        assert_eq!(response.provider, "grok-fallback");
    }

    #[tokio::test]
    async fn fallback_skips_ineligible_model_without_rebinding_profile_model() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let chain = FallbackChain::new(vec![
            Box::new(ModelBoundProvider {
                model: "text-only",
                tools: CapabilitySupport::Unsupported,
                streaming: CapabilitySupport::Supported,
                calls: Arc::clone(&first_calls),
            }),
            Box::new(ModelBoundProvider {
                model: "tool-model",
                tools: CapabilitySupport::Supported,
                streaming: CapabilitySupport::Supported,
                calls: Arc::clone(&second_calls),
            }),
        ]);
        let request = ProviderRequest::new(vec![]).with_tools(vec![ToolDefinition {
            name: "lookup".into(),
            description: "lookup".into(),
            input_schema: ToolInputSchema::simple(vec![]),
        }]);
        let response = chain.send_message_with_fallback(&request).await.unwrap();
        assert_eq!(response.model, "tool-model");
        assert_eq!(first_calls.load(Ordering::SeqCst), 0);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn streaming_fallback_skips_ineligible_model_before_invocation() {
        let first_calls = Arc::new(AtomicUsize::new(0));
        let second_calls = Arc::new(AtomicUsize::new(0));
        let chain = FallbackChain::new(vec![
            Box::new(ModelBoundProvider {
                model: "blocking-only",
                tools: CapabilitySupport::Supported,
                streaming: CapabilitySupport::Unsupported,
                calls: Arc::clone(&first_calls),
            }),
            Box::new(ModelBoundProvider {
                model: "streaming",
                tools: CapabilitySupport::Supported,
                streaming: CapabilitySupport::Supported,
                calls: Arc::clone(&second_calls),
            }),
        ]);
        chain
            .send_message_stream_with_fallback(&ProviderRequest::new(vec![]).with_stream(true))
            .await
            .unwrap();
        assert_eq!(first_calls.load(Ordering::SeqCst), 0);
        assert_eq!(second_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn fallback_includes_profile_configured_reasoning_in_eligibility() {
        let primary_calls = Arc::new(AtomicUsize::new(0));
        let chain = FallbackChain::new(vec![
            Box::new(ImplicitReasoningProvider {
                calls: Arc::clone(&primary_calls),
            }),
            Box::new(MockProvider::new("fallback", false)),
        ]);
        let response = chain
            .send_message_with_fallback(&ProviderRequest::new(vec![]))
            .await
            .unwrap();
        assert_eq!(response.provider, "fallback");
        assert_eq!(primary_calls.load(Ordering::SeqCst), 0);
    }
}
