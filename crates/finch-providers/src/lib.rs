//! Finch provider transports, OAuth, catalogs, and credential ports.
//!
//! This crate owns the provider-neutral contracts and the per-dialect adapters.
//! Finch application configuration, Brain, TUI, daemon, and tool execution stay
//! outside. Child modules are private; the `pub use` list is the public surface.

use anyhow::Result;
use async_trait::async_trait;
use std::any::{Any, TypeId};
use tokio::sync::mpsc::Receiver;

mod alignment;
mod anthropic;
mod chatgpt_oauth;
#[cfg(feature = "chatgpt")]
mod chatgpt_subscription;
#[cfg(feature = "claude")]
mod claude;
mod credentials;
mod endpoints;
mod fallback_chain;
#[cfg(feature = "gemini")]
mod gemini;
mod model_catalog;
#[cfg(feature = "openai")]
mod openai;
#[cfg(feature = "chatgpt")]
mod openai_jwks;
mod ports;
mod reasoning;
mod retry;
mod teacher_session;
mod tool_bindings;
mod tool_contract;
mod types;
mod validated_boundary;
mod wire_types;

pub mod oauth;

pub use alignment::{with_alignment, UNIVERSAL_ALIGNMENT_PROMPT};
pub use anthropic::{MessageRequest, MessageResponse, StreamDelta, StreamEvent};
pub use chatgpt_oauth::{
    chatgpt_required_scopes, ChatGptAuthStageError, ChatGptDeviceEndpointError,
    OpenAiChatGptOAuthDialect, OpenAiTokenVerifier, VerifiedOpenAiClaims,
    CHATGPT_OAUTH_PROTOCOL_REVISION, OPENAI_PUBLIC_CLIENT_ID, REQUIRED_TOKEN_ISSUER,
};
#[cfg(feature = "chatgpt")]
pub use chatgpt_subscription::ChatGptSubscriptionProvider;
#[cfg(feature = "claude")]
pub use claude::ClaudeProvider;
pub use credentials::{
    credential_dependencies, credential_index, normalize_origin, required_audience,
    validate_authenticated_endpoints, validate_binding, AudienceBinding, CredentialBinding,
    CredentialKind, CredentialLifecycle, CredentialProvider, CredentialResolver, EndpointFamily,
    EnvironmentCredentialResolver, LifecycleRevocation, ProviderCredential, ResolvedCredential,
    ResolvedSecret,
};
pub use endpoints::ProviderEndpoints;
pub use fallback_chain::FallbackChain;
#[cfg(feature = "gemini")]
pub use gemini::GeminiProvider;
pub use model_catalog::{
    default_cache_dir, fallback_catalog, profile_cache_identity, read_cache, refresh,
    refresh_with_fallback, static_fallback, CatalogAuth, CatalogSource, ModelCatalog,
    ModelCatalogProfile, STATIC_FALLBACK_AS_OF,
};
#[cfg(feature = "openai")]
pub use openai::OpenAIProvider;
#[cfg(feature = "chatgpt")]
pub use openai_jwks::OpenAiJwksVerifier;
pub use ports::{
    AuthorizationPresenter, BillingActionConfirmer, Clock, FrozenClock, HttpTransport,
    InstantSleeper, ProviderPorts, ProviderTelemetry, ReqwestTransport, Sleeper, SystemClock,
    TokioSleeper,
};
pub use reasoning::ReasoningEffort;
pub use retry::{with_retry, NonRetriableError};
pub use teacher_session::{
    ConversationState, OptimizationStats, TeacherContextConfig, TeacherSession,
};
pub use tool_bindings::{
    compile_from_definitions, compile_tool_bindings, BoundTool, ResultEncoding, SemanticTool,
    ToolBindingError, ToolBindingTable, ToolOrigin, WireToolIdentity, WireToolKind,
    MAX_ADVERTISED_TOOLS,
};
pub use tool_contract::{ToolDefinition, ToolInputSchema, ToolUse};
pub use types::{
    CapabilityProvenance, CapabilitySupport, ContextWindowCapability, EventProvenance,
    InvocationMetadata, ModelCapabilities, ModelFeature, NativeToolGrant,
    OutputTokenLimitCapability, ProviderAllowance, ProviderRequest, ProviderResponse,
    ProviderUsage, ReasoningCapability, StreamChunk, ToolAuthority, ToolCompilePolicy,
    WireProtocol, WireProtocolCapability,
};
pub use validated_boundary::ValidatedProviderRequest;
pub use wire_types::{ContentBlock, ImageSource, Message};

pub(crate) use validated_boundary::validate_provider_request;

/// Default Claude model used when a transport does not override it.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5";
/// Default completion budget used by Anthropic request envelopes.
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 8000;

const MAX_RESPONSE_MODEL_BYTES: usize = 256;

pub(crate) fn validate_response_model(model: &str) -> Result<()> {
    if model.is_empty()
        || model.len() > MAX_RESPONSE_MODEL_BYTES
        || !model.bytes().all(|byte| byte.is_ascii_graphic())
    {
        anyhow::bail!("Provider response model metadata was invalid");
    }
    Ok(())
}

/// Non-overridable concrete type identity used by validated dispatch tokens.
///
/// The crate provides the blanket implementation for every `'static` type, so an
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
    fn requested_reasoning_effort(&self, _request: &ProviderRequest) -> Option<ReasoningEffort> {
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

impl From<ProviderResponse> for MessageResponse {
    fn from(response: ProviderResponse) -> Self {
        Self {
            id: response.id,
            response_type: "message".to_string(),
            role: response.role,
            content: response.content,
            model: response.model,
            stop_reason: response.stop_reason,
            input_tokens: response.usage.as_ref().map(|usage| usage.input_tokens),
            output_tokens: response.usage.as_ref().map(|usage| usage.output_tokens),
            primary_allowance_used_percent: response
                .allowance
                .as_ref()
                .and_then(|allowance| allowance.primary_used_percent),
            secondary_allowance_used_percent: response
                .allowance
                .as_ref()
                .and_then(|allowance| allowance.secondary_used_percent),
        }
    }
}

#[cfg(test)]
mod capability_contract_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ContractProvider {
        effects: AtomicUsize,
        descriptor_model: &'static str,
        reasoning: Option<ReasoningEffort>,
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
        ) -> Option<ReasoningEffort> {
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
            reasoning: Some(ReasoningEffort::High),
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

    struct ToolsProvider {
        effects: AtomicUsize,
        protocol: WireProtocol,
    }

    #[async_trait]
    impl ProviderBackend for ToolsProvider {
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
            "tools-contract"
        }

        fn default_model(&self) -> &str {
            "model-a"
        }

        fn capabilities(&self, model: &str) -> ModelCapabilities {
            ModelCapabilities::static_metadata(
                self.name(),
                model,
                "2026-09-16",
                "tool binding contract",
                CapabilitySupport::Supported,
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
                ReasoningCapability::unsupported("2026-09-16", "tool binding contract"),
                Some(1_000),
                Some(10_000),
                None,
            )
            .with_wire_protocol(self.protocol, "2026-09-16", "tool binding contract")
        }
    }

    #[tokio::test]
    async fn reserved_wire_collision_fails_before_provider_effect() {
        let provider = ToolsProvider {
            effects: AtomicUsize::new(0),
            protocol: WireProtocol::OpenAiChatGptResponsesLite,
        };
        let error = provider
            .send_message(
                &ProviderRequest::new(vec![])
                    .with_model("model-a")
                    .with_tools(vec![ToolDefinition {
                        name: "finch_spawn_agent".into(),
                        description: "collision".into(),
                        input_schema: ToolInputSchema::simple(vec![]),
                    }]),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("reserved wire tool name"),
            "reserved alias must fail at the validated boundary: {error}"
        );
        assert_eq!(provider.effects.load(Ordering::SeqCst), 0);
    }
}

#[cfg(test)]
mod independence_tests {
    #[test]
    fn crate_manifest_does_not_depend_on_finch() {
        let manifest = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"));
        assert!(
            !manifest.contains("name = \"finch\"") && !manifest.contains("path = \"../..\""),
            "finch-providers must not take a path dependency on the Finch application crate"
        );
        assert!(
            !manifest.contains("finch ="),
            "finch-providers must not depend on the finch package"
        );
    }
}
