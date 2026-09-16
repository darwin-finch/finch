//! External-style adapter against the public `finch-providers` contract.
//!
//! This example is the downstream seam: a crate user implements
//! [`ProviderBackend`] and receives only a [`ValidatedProviderRequest`].

use anyhow::Result;
use async_trait::async_trait;
use finch_providers::{
    LlmProvider, ModelCapabilities, ProviderBackend, ProviderRequest, ProviderResponse,
    StreamChunk, ValidatedProviderRequest,
};
use tokio::sync::mpsc::Receiver;

struct EchoProvider;

#[async_trait]
impl ProviderBackend for EchoProvider {
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse> {
        let (request, _bindings) = request.into_request_for(self)?;
        let text = request
            .messages
            .last()
            .map(|message| message.text())
            .unwrap_or_default();
        Ok(ProviderResponse {
            id: "echo-1".into(),
            model: self.default_model().into(),
            content: vec![finch_providers::ContentBlock::text(format!("echo:{text}"))],
            stop_reason: Some("end_turn".into()),
            role: "assistant".into(),
            provider: self.name().into(),
            usage: None,
            allowance: None,
        })
    }

    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>> {
        let (_request, _bindings) = request.into_request_for(self)?;
        anyhow::bail!("echo adapter does not stream")
    }

    fn name(&self) -> &str {
        "echo"
    }

    fn default_model(&self) -> &str {
        "echo-1"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        ModelCapabilities::unknown(self.name(), model)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let provider = EchoProvider;
    let response = provider
        .send_message(&ProviderRequest::new(vec![finch_providers::Message::user(
            "ping",
        )]))
        .await;
    match response {
        Ok(response) => {
            println!("{}", response.text());
            Ok(())
        }
        Err(error) => {
            // Unknown capabilities fail closed before the adapter runs.
            println!("validated dispatch rejected unknown model: {error}");
            Ok(())
        }
    }
}
