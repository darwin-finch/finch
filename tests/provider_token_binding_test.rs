use anyhow::Result;
use async_trait::async_trait;
use finch::providers::{
    CapabilitySupport, LlmProvider, ModelCapabilities, ProviderBackend, ProviderRequest,
    ProviderResponse, ReasoningCapability, StreamChunk, ValidatedProviderRequest,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

struct EffectProvider {
    effects: AtomicUsize,
}

#[async_trait]
impl ProviderBackend for EffectProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let _request = request.into_request_for(self)?;
        self.effects.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("effect provider was invoked")
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        let _request = request.into_request_for(self)?;
        self.effects.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("effect provider was invoked")
    }

    fn name(&self) -> &str {
        "same-provider"
    }

    fn default_model(&self) -> &str {
        "same-model"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        test_capabilities(self.name(), model)
    }
}

struct ForwardingProvider {
    target: Arc<EffectProvider>,
}

#[async_trait]
impl ProviderBackend for ForwardingProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        self.target.send_message_validated(request).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        self.target.send_message_stream_validated(request).await
    }

    fn name(&self) -> &str {
        "same-provider"
    }

    fn default_model(&self) -> &str {
        "same-model"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        test_capabilities(self.name(), model)
    }
}

fn test_capabilities(provider: &str, model: &str) -> ModelCapabilities {
    ModelCapabilities::static_metadata(
        provider,
        model,
        "2026-08-26",
        "test fixture",
        CapabilitySupport::Supported,
        CapabilitySupport::Unsupported,
        CapabilitySupport::Unsupported,
        ReasoningCapability::unsupported("2026-08-26", "test fixture"),
        Some(1_000),
        Some(1_000),
        None,
    )
}

#[tokio::test]
async fn test_validated_token_cannot_be_forwarded_to_another_backend_instance() {
    let target = Arc::new(EffectProvider {
        effects: AtomicUsize::new(0),
    });
    let forwarding = ForwardingProvider {
        target: Arc::clone(&target),
    };

    let error = forwarding
        .send_message(&ProviderRequest::new(vec![]))
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "Validated provider request was presented to a different provider instance"
    );
    assert_eq!(target.effects.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_streaming_validated_token_cannot_be_forwarded_to_another_backend_instance() {
    let target = Arc::new(EffectProvider {
        effects: AtomicUsize::new(0),
    });
    let forwarding = ForwardingProvider {
        target: Arc::clone(&target),
    };

    let error = forwarding
        .send_message_stream(&ProviderRequest::new(vec![]))
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "Validated provider request was presented to a different provider instance"
    );
    assert_eq!(target.effects.load(Ordering::SeqCst), 0);
}
