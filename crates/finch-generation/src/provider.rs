//! Cloud backend that wraps `finch-providers::LlmProvider`.

use crate::backend::GenerationBackend;
use crate::event::GenerationEvent;
use crate::identity::{BackendKind, BackendRef};
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
    /// Wrap a provider using its default model.
    pub fn new(provider: Arc<dyn LlmProvider>) -> Result<Self> {
        let identity = BackendRef::new(
            provider.name(),
            provider.default_model(),
            BackendKind::Cloud,
        )?;
        let descriptor = provider.capabilities(provider.default_model());
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
        let mut provider_request = ProviderRequest::new(request.messages.clone())
            .with_model(request.requested.model.clone())
            .with_stream(true)
            .with_cancellation_token(request.cancellation.clone());
        if let Some(max_tokens) = request.budget.max_output_tokens {
            provider_request = provider_request.with_max_tokens(max_tokens);
        }
        if let Some(tools) = request.tools.clone() {
            provider_request = provider_request.with_tools(tools);
        }

        let mut stream = self.provider.send_message_stream(&provider_request).await?;
        let identity = crate::identity::GenerationIdentity {
            requested: request.requested.clone(),
            resolved: self.identity.clone(),
            actual: self.identity.clone(),
        };
        let (tx, rx) = mpsc::channel(64);
        tokio::spawn(async move {
            let mut sequence = 0;
            while let Some(chunk) = stream.recv().await {
                sequence += 1;
                match chunk {
                    Ok(chunk) => {
                        if let Some(event) = translate_provider_chunk(chunk, &identity, sequence) {
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
