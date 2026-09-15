// Multi-provider LLM support
//
// This module provides an abstraction layer over different LLM providers
// (Claude, OpenAI, Grok, Gemini, etc.) allowing users to choose their
// preferred API provider while maintaining a unified interface.

use anyhow::Result;
use async_trait::async_trait;
use std::any::{Any, TypeId};
use tokio::sync::mpsc::Receiver;

mod endpoints;
mod model_catalog;
mod types;

// The provider-neutral conversation wire vocabulary shared by every caller
mod wire_types;

// Provider implementations
mod chatgpt_oauth;
mod chatgpt_subscription;
mod claude;
mod gemini;
mod openai;
mod openai_jwks;

// Provider factory
mod factory;

// Fallback chain (not used in student-teacher architecture)
mod fallback_chain;

// Teacher session management with context optimization
mod teacher_session;

// Universal alignment prompt for cross-provider behavioral consistency
mod alignment;
pub use alignment::{with_alignment, UNIVERSAL_ALIGNMENT_PROMPT};

// Re-export commonly used types
pub use chatgpt_oauth::{
    chatgpt_required_scopes, ChatGptAuthStageError, ChatGptDeviceEndpointError,
    OpenAiChatGptOAuthDialect, OpenAiTokenVerifier, VerifiedOpenAiClaims,
    CHATGPT_OAUTH_PROTOCOL_REVISION, OPENAI_PUBLIC_CLIENT_ID, REQUIRED_TOKEN_ISSUER,
};
pub use claude::ClaudeProvider;
pub use endpoints::ProviderEndpoints;
pub use factory::preflight_provider_config;
pub use factory::{
    create_provider, create_provider_from_config, create_provider_from_entries,
    create_provider_from_entry, create_provider_from_teacher, create_provider_graph_from_config,
    create_provider_graph_from_config_with_resolver, create_provider_profile_from_config,
    create_provider_profile_from_config_with_resolver, create_providers,
    create_providers_from_config, create_providers_from_entries, ProviderGraph, ProviderProfile,
};
pub use fallback_chain::FallbackChain;
pub use gemini::GeminiProvider;
pub use model_catalog::{
    default_cache_dir, fallback_catalog, profile_cache_identity, read_cache, refresh,
    refresh_from_config, refresh_with_fallback, static_fallback, CatalogAuth, CatalogSource,
    ModelCatalog, ModelCatalogProfile, STATIC_FALLBACK_AS_OF,
};
pub use openai::OpenAIProvider;
pub use openai_jwks::OpenAiJwksVerifier;
pub use teacher_session::{
    ConversationState, OptimizationStats, TeacherContextConfig, TeacherSession,
};
pub use types::{
    CapabilityProvenance, CapabilitySupport, ContextWindowCapability, InvocationMetadata,
    ModelCapabilities, ModelFeature, OutputTokenLimitCapability, ProviderAllowance,
    ProviderRequest, ProviderResponse, ProviderUsage, ReasoningCapability, StreamChunk,
    WireProtocol, WireProtocolCapability,
};
pub use wire_types::{ContentBlock, ImageSource, Message};

mod validated_boundary;

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
impl From<ProviderResponse> for crate::claude::MessageResponse {
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
    use crate::tools::{ToolDefinition, ToolInputSchema};
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

#[cfg(test)]
mod wire_type_boundary_tests {
    /// The universal wire types (`Message`, `ContentBlock`, `ImageSource`) are
    /// reached through the providers facade, never through the Claude client
    /// subtree. A stale `crate::claude::<trio>` or `crate::claude::types::<trio>`
    /// reference outside `src/claude` re-couples every caller to the Claude
    /// transport's file layout, which is exactly what the hoist out of
    /// `claude::types` removed. Claude itself consumes the facade types, so the
    /// scan excludes only `src/claude`'s own use of its envelopes.
    #[test]
    fn test_universal_wire_types_are_reached_through_the_providers_facade() {
        let mut hits = Vec::new();
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for tree in ["src", "tests"] {
            let root = manifest.join(tree);
            collect_stale_claude_wire_paths(&root, &manifest.join("src/claude"), &mut hits);
        }
        assert!(
            hits.is_empty(),
            "universal wire types must be used via crate::providers, not the Claude client subtree; found: {hits:?}"
        );
    }

    fn collect_stale_claude_wire_paths(
        dir: &std::path::Path,
        claude_root: &std::path::Path,
        hits: &mut Vec<String>,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.starts_with(claude_root) {
                continue;
            }
            if path.is_dir() {
                collect_stale_claude_wire_paths(&path, claude_root, hits);
                continue;
            }
            let Some(name) = path.to_str() else {
                continue;
            };
            if !name.ends_with(".rs") || name.ends_with("src/providers/mod.rs") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (line_number, line) in contents.lines().enumerate() {
                let stale = line.contains("crate::claude::types::")
                    || line.contains("finch::claude::types::")
                    || line.contains("use crate::claude::{")
                        && line.contains("Message")
                        && !line.contains("MessageRequest")
                        && !line.contains("MessageResponse");
                if stale {
                    hits.push(format!("{}:{}: {}", name, line_number + 1, line.trim()));
                }
            }
        }
    }
}
