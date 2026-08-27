// Multi-provider LLM support
//
// This module provides an abstraction layer over different LLM providers
// (Claude, OpenAI, Grok, Gemini, etc.) allowing users to choose their
// preferred API provider while maintaining a unified interface.

use anyhow::Result;
use async_trait::async_trait;
use std::any::{Any, TypeId};
use tokio::sync::mpsc::Receiver;

pub mod endpoints;
pub mod model_catalog;
pub mod types;

// Provider implementations
pub mod claude;
pub mod gemini;
pub mod openai;

// Provider factory
pub mod factory;

// Fallback chain (not used in student-teacher architecture)
pub mod fallback_chain;

// Teacher session management with context optimization
pub mod teacher_session;

// Universal alignment prompt for cross-provider behavioral consistency
pub mod alignment;
pub use alignment::{with_alignment, UNIVERSAL_ALIGNMENT_PROMPT};

// Re-export commonly used types
pub use factory::{
    create_provider, create_provider_from_config, create_provider_from_entries,
    create_provider_from_entry, create_provider_from_teacher, create_provider_graph_from_config,
    create_providers, create_providers_from_config, create_providers_from_entries, ProviderGraph,
    ProviderProfile,
};
pub use fallback_chain::FallbackChain;
pub use teacher_session::{
    ConversationState, OptimizationStats, TeacherContextConfig, TeacherSession,
};
pub use types::{
    CapabilityProvenance, CapabilitySupport, ContextWindowCapability, ModelCapabilities,
    ModelFeature, OutputTokenLimitCapability, ProviderRequest, ProviderResponse,
    ReasoningCapability, StreamChunk, WireProtocol, WireProtocolCapability,
};

mod validated_boundary {
    use super::*;

    /// A request whose effective provider/model identity and optional
    /// capabilities were checked by Finch's non-overridable dispatch boundary.
    ///
    /// The fields and constructor are private to this module, so even a
    /// provider backend elsewhere in the crate cannot fabricate a token.
    ///
    /// ```compile_fail
    /// use finch::providers::ValidatedProviderRequest;
    ///
    /// let _ = ValidatedProviderRequest {
    ///     request: panic!("unreachable"),
    ///     capabilities: panic!("unreachable"),
    /// };
    /// ```
    pub struct ValidatedProviderRequest {
        request: ProviderRequest,
        capabilities: ModelCapabilities,
        target: usize,
        target_type: TypeId,
    }

    impl ValidatedProviderRequest {
        /// Consume this token at the exact provider instance for which it was
        /// validated and return the effective request.
        #[doc(hidden)]
        pub fn into_request_for(
            self,
            provider: &(impl ProviderBackend + ?Sized),
        ) -> Result<ProviderRequest> {
            if self.target != provider_target(provider)
                || self.target_type != ProviderConcreteType::provider_concrete_type_id(provider)
            {
                anyhow::bail!(
                    "Validated provider request was presented to a different provider instance or concrete backend type"
                );
            }
            Ok(self.request)
        }

        /// The exact descriptor used to validate this request.
        pub fn capabilities(&self) -> &ModelCapabilities {
            &self.capabilities
        }
    }

    pub(crate) fn validate_provider_request(
        provider: &(impl ProviderBackend + ?Sized),
        request: &ProviderRequest,
        streaming: bool,
    ) -> Result<ValidatedProviderRequest> {
        let (effective, capabilities) = resolve_effective_request(provider, request)?;
        capabilities.validate_request(
            &effective,
            streaming,
            provider.requested_reasoning_effort(&effective),
        )?;
        Ok(ValidatedProviderRequest {
            request: effective,
            capabilities,
            target: provider_target(provider),
            target_type: ProviderConcreteType::provider_concrete_type_id(provider),
        })
    }

    fn provider_target(provider: &(impl ProviderBackend + ?Sized)) -> usize {
        provider as *const _ as *const () as usize
    }
}

pub(crate) use validated_boundary::validate_provider_request;
pub use validated_boundary::ValidatedProviderRequest;

/// Non-overridable concrete type identity used by validated dispatch tokens.
///
/// Finch provides the blanket implementation for every `'static` type, so an
/// external provider can implement [`ProviderBackend`] but cannot spoof this
/// marker with a conflicting implementation.
#[doc(hidden)]
pub trait ProviderConcreteType: Any {
    fn provider_concrete_type_id(&self) -> TypeId;
}

impl<T: Any> ProviderConcreteType for T {
    fn provider_concrete_type_id(&self) -> TypeId {
        TypeId::of::<T>()
    }
}

/// Provider implementation hooks. The raw hooks can only receive an
/// unforgeable [`ValidatedProviderRequest`].
#[async_trait]
#[doc(hidden)]
pub trait ProviderBackend: ProviderConcreteType + Send + Sync {
    /// Provider implementation called only after capability validation.
    /// Implementations must consume the token with
    /// [`ValidatedProviderRequest::into_request_for`] before any side effect.
    async fn send_message_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<ProviderResponse>;

    /// Streaming provider implementation called only after capability validation.
    /// Implementations must consume the token with
    /// [`ValidatedProviderRequest::into_request_for`] before any side effect.
    async fn send_message_stream_validated(
        &self,
        request: ValidatedProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>>;

    /// Get the provider name (e.g., "claude", "openai", "gemini")
    fn name(&self) -> &str;

    /// Get the default model for this provider
    fn default_model(&self) -> &str;

    /// Capabilities of an exact model. Unknown models must remain fail-closed.
    fn capabilities(&self, model: &str) -> ModelCapabilities {
        ModelCapabilities::unknown(self.name(), model)
    }

    /// Whether the selected profile implicitly requests reasoning controls.
    #[doc(hidden)]
    fn requested_reasoning_effort(
        &self,
        _request: &ProviderRequest,
    ) -> Option<crate::config::ReasoningEffort> {
        None
    }
}

pub(crate) fn resolve_effective_request(
    provider: &(impl ProviderBackend + ?Sized),
    request: &ProviderRequest,
) -> Result<(ProviderRequest, ModelCapabilities)> {
    let mut effective = request.clone();
    if effective.model.trim().is_empty() {
        effective.model = provider.default_model().to_string();
    }
    let capabilities = provider.capabilities(&effective.model);
    if capabilities.provider != provider.name() || capabilities.model != effective.model {
        anyhow::bail!(
            "Capability descriptor identity mismatch: requested provider '{}' model '{}', descriptor reported provider '{}' model '{}'",
            provider.name(),
            effective.model,
            capabilities.provider,
            capabilities.model
        );
    }
    Ok((effective, capabilities))
}

/// Non-overridable validated dispatch API shared by every provider backend.
#[async_trait]
pub trait LlmProvider: ProviderBackend {
    /// Send a message and get a complete response.
    async fn send_message(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let validated = validate_provider_request(self, request, false)?;
        self.send_message_validated(validated).await
    }

    /// Send a message and stream the response.
    async fn send_message_stream(
        &self,
        request: &ProviderRequest,
    ) -> Result<Receiver<Result<StreamChunk>>> {
        let validated = validate_provider_request(self, request, true)?;
        self.send_message_stream_validated(validated).await
    }

    /// Compatibility view derived from the exact default-model descriptor.
    fn supports_streaming(&self) -> bool {
        self.capabilities(self.default_model())
            .streaming
            .is_supported()
    }

    /// Compatibility view derived from the exact default-model descriptor.
    fn supports_tools(&self) -> bool {
        self.capabilities(self.default_model()).tools.is_supported()
    }
}

#[async_trait]
impl<T> LlmProvider for T where T: ProviderBackend + ?Sized {}

/// Helper to convert provider response to format compatible with existing code
impl From<ProviderResponse> for crate::claude::types::MessageResponse {
    fn from(response: ProviderResponse) -> Self {
        Self {
            id: response.id,
            response_type: "message".to_string(),
            role: response.role,
            content: response.content,
            model: response.model,
            stop_reason: response.stop_reason,
        }
    }
}

#[cfg(test)]
mod capability_contract_tests {
    use super::*;
    use crate::tools::types::{ToolDefinition, ToolInputSchema};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ContractProvider {
        effects: AtomicUsize,
        descriptor_model: &'static str,
        reasoning: Option<crate::config::ReasoningEffort>,
    }

    #[async_trait]
    impl ProviderBackend for ContractProvider {
        async fn send_message_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<ProviderResponse> {
            self.effects.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("raw provider effect must not run")
        }

        async fn send_message_stream_validated(
            &self,
            _request: ValidatedProviderRequest,
        ) -> Result<Receiver<Result<StreamChunk>>> {
            self.effects.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("raw provider effect must not run")
        }

        fn name(&self) -> &str {
            "contract"
        }

        fn default_model(&self) -> &str {
            "model-a"
        }

        fn capabilities(&self, _model: &str) -> ModelCapabilities {
            ModelCapabilities::static_metadata(
                self.name(),
                self.descriptor_model,
                "2026-08-26",
                "test fixture",
                CapabilitySupport::Unsupported,
                CapabilitySupport::Unsupported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-08-26", "test fixture"),
                Some(1_000),
                Some(10_000),
                None,
            )
        }

        fn requested_reasoning_effort(
            &self,
            _request: &ProviderRequest,
        ) -> Option<crate::config::ReasoningEffort> {
            self.reasoning
        }
    }

    fn tool() -> ToolDefinition {
        ToolDefinition {
            name: "lookup".into(),
            description: "lookup".into(),
            input_schema: ToolInputSchema::simple(vec![]),
        }
    }

    #[tokio::test]
    async fn descriptor_identity_mismatch_fails_before_provider_effect() {
        let provider = ContractProvider {
            effects: AtomicUsize::new(0),
            descriptor_model: "model-b",
            reasoning: None,
        };
        let error = provider
            .send_message(&ProviderRequest::new(vec![]).with_model("model-a"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains(
            "requested provider 'contract' model 'model-a', descriptor reported provider 'contract' model 'model-b'"
        ));
        assert_eq!(provider.effects.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unsupported_fields_and_modes_fail_before_provider_effect() {
        let provider = ContractProvider {
            effects: AtomicUsize::new(0),
            descriptor_model: "model-a",
            reasoning: None,
        };
        let tool_error = provider
            .send_message(&ProviderRequest::new(vec![]).with_tools(vec![tool()]))
            .await
            .unwrap_err();
        assert!(tool_error
            .to_string()
            .contains("does not support tool calls"));
        let stream_error = provider
            .send_message_stream(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert!(stream_error
            .to_string()
            .contains("does not support streaming"));
        assert_eq!(provider.effects.load(Ordering::SeqCst), 0);

        let reasoning_provider = ContractProvider {
            effects: AtomicUsize::new(0),
            descriptor_model: "model-a",
            reasoning: Some(crate::config::ReasoningEffort::High),
        };
        let reasoning_error = reasoning_provider
            .send_message(&ProviderRequest::new(vec![]))
            .await
            .unwrap_err();
        assert!(reasoning_error
            .to_string()
            .contains("does not support reasoning controls"));
        assert_eq!(reasoning_provider.effects.load(Ordering::SeqCst), 0);

        let output_provider = ContractProvider {
            effects: AtomicUsize::new(0),
            descriptor_model: "model-a",
            reasoning: None,
        };
        let output_error = output_provider
            .send_message(&ProviderRequest::new(vec![]).with_max_tokens(10_001))
            .await
            .unwrap_err();
        assert!(output_error
            .to_string()
            .contains("supports at most 10000 output tokens, but 10001 were requested"));
        assert_eq!(output_provider.effects.load(Ordering::SeqCst), 0);
    }
}
