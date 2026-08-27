use anyhow::Result;
use async_trait::async_trait;
use finch::providers::openai::OpenAIProvider;
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
        Some(10_000),
        Some(10_000),
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
        "Validated provider request was presented to a different provider instance or concrete backend type"
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
        "Validated provider request was presented to a different provider instance or concrete backend type"
    );
    assert_eq!(target.effects.load(Ordering::SeqCst), 0);
}

#[repr(transparent)]
struct TransparentOpenAI(OpenAIProvider);

#[async_trait]
impl ProviderBackend for TransparentOpenAI {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        self.0.send_message_validated(request).await
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<mpsc::Receiver<Result<StreamChunk>>> {
        self.0.send_message_stream_validated(request).await
    }

    fn name(&self) -> &str {
        "openai"
    }

    fn default_model(&self) -> &str {
        "gpt-4o"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        test_capabilities(self.name(), model)
    }
}

#[tokio::test]
async fn test_transparent_wrapper_cannot_forward_token_to_custom_openai_backend() {
    let mut server = mockito::Server::new_async().await;
    let no_http_effect = server
        .mock("POST", "/v1/chat/completions")
        .expect(0)
        .create_async()
        .await;
    let provider = OpenAIProvider::new_compatible(
        "test-key".to_string(),
        server.url(),
        "/v1/chat/completions",
        "/v1/models",
        "gpt-4o".to_string(),
        "openai".to_string(),
    )
    .unwrap();
    let wrapper = TransparentOpenAI(provider);
    assert_eq!(
        &wrapper as *const TransparentOpenAI as *const (),
        &wrapper.0 as *const OpenAIProvider as *const (),
        "repr(transparent) must exercise the same-address alias"
    );
    let request = ProviderRequest::new(vec![]);

    let nonstream_error = wrapper.send_message(&request).await.unwrap_err();
    assert_eq!(
        nonstream_error.to_string(),
        "Validated provider request was presented to a different provider instance or concrete backend type"
    );

    let stream_error = wrapper.send_message_stream(&request).await.unwrap_err();
    assert_eq!(
        stream_error.to_string(),
        "Validated provider request was presented to a different provider instance or concrete backend type"
    );
    no_http_effect.assert_async().await;
}
