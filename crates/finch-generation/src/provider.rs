//! Cloud backend that wraps `finch-providers::LlmProvider`.

use crate::backend::GenerationBackend;
use crate::event::GenerationEvent;
use crate::identity::{BackendKind, BackendRef, GenerationIdentity};
use crate::readiness::{ReadinessReport, ResourceMetadata};
use crate::request::{GenerationCapabilities, GenerationRequest, GenerationStrategy};
use crate::translate::translate_provider_chunk;
use anyhow::Result;
use async_trait::async_trait;
use finch_providers::{LlmProvider, ProviderRequest};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{self, Receiver};

/// Generation backend over a provider transport.
pub struct ProviderGenerationBackend {
    provider: Arc<dyn LlmProvider>,
    identity: BackendRef,
    capabilities: GenerationCapabilities,
}

impl ProviderGenerationBackend {
    /// Wrap a provider using its default model as the catalog identity.
    ///
    /// Dispatch still follows [`GenerationRequest::requested`]: resolved and
    /// actual use the requested model when the provider matches, not this
    /// default.
    pub fn new(provider: Arc<dyn LlmProvider>) -> Result<Self> {
        Self::for_model(provider, None)
    }

    /// Wrap a provider pinned to `model`, or the provider default when `None`.
    pub fn for_model(provider: Arc<dyn LlmProvider>, model: Option<&str>) -> Result<Self> {
        let model = model.unwrap_or_else(|| provider.default_model());
        let identity = BackendRef::new(provider.name(), model, BackendKind::Cloud)?;
        let descriptor = provider.capabilities(model);
        Ok(Self {
            provider,
            identity,
            capabilities: GenerationCapabilities {
                streaming: descriptor.streaming.is_supported(),
                tools: descriptor.tools.is_supported(),
                conversation: true,
                strategy: GenerationStrategy::CausalAutoregressive,
                max_context_messages: descriptor.context_window.max_messages,
            },
        })
    }
}

#[async_trait]
impl GenerationBackend for ProviderGenerationBackend {
    fn identity(&self) -> BackendRef {
        self.identity.clone()
    }

    fn capabilities(&self) -> GenerationCapabilities {
        self.capabilities.clone()
    }

    fn strategy(&self) -> GenerationStrategy {
        GenerationStrategy::CausalAutoregressive
    }

    fn readiness(&self) -> ReadinessReport {
        ReadinessReport::ready(Duration::ZERO, ResourceMetadata::default())
    }

    async fn generate(
        &self,
        request: GenerationRequest,
    ) -> Result<Receiver<Result<GenerationEvent>>> {
        let identity =
            GenerationIdentity::for_dispatch(request.requested.clone(), self.identity.clone());
        let mut provider_request = ProviderRequest::new(request.messages.clone())
            .with_model(identity.resolved.model.clone())
            .with_stream(true)
            .with_cancellation_token(request.cancellation.clone());
        if let Some(max_tokens) = request.budget.max_output_tokens {
            provider_request = provider_request.with_max_tokens(max_tokens);
        }
        if let Some(tools) = request.tools.clone() {
            provider_request = provider_request.with_tools(tools);
        }

        let mut stream = self.provider.send_message_stream(&provider_request).await?;
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut sequence = 0;
            let mut identity = identity;
            if tx
                .send(Ok(GenerationEvent::Identity(identity.clone())))
                .await
                .is_err()
            {
                return;
            }
            while let Some(chunk) = stream.recv().await {
                sequence += 1;
                match chunk {
                    Ok(chunk) => {
                        if let Some(event) = translate_provider_chunk(chunk, &identity, sequence) {
                            if let GenerationEvent::Identity(updated) = &event {
                                identity = updated.clone();
                            }
                            if tx.send(Ok(event)).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error)).await;
                        return;
                    }
                }
            }
        });
        Ok(rx)
    }
}
