//! Compatibility facade: provider construction from Finch configuration.
//!
//! Transports, OAuth, catalogs, and wire types live in `finch-providers`.
//! This module re-exports that crate and keeps Config-taking factory/catalog
//! mapping in Finch.

mod catalog;
mod factory;

pub use catalog::refresh_from_config;
pub use factory::preflight_provider_config;
pub use factory::{
    create_provider_from_config, create_provider_from_entries, create_provider_from_entry,
    create_provider_from_overlaid_entry, create_provider_graph_from_config,
    create_provider_graph_from_config_with_resolver, create_provider_profile_from_config,
    create_provider_profile_from_config_with_resolver, create_providers_from_config,
    create_providers_from_entries, ProviderGraph, ProviderProfile,
};
pub use finch_providers::{
    chatgpt_required_scopes, claude_required_scopes, default_cache_dir, fallback_catalog,
    grok_required_scopes, profile_cache_identity, read_cache, refresh, refresh_with_fallback,
    static_fallback, with_alignment, CapabilityProvenance, CapabilitySupport, CatalogAuth,
    CatalogSource, ChatGptAuthStageError, ChatGptDeviceEndpointError, ChatGptSubscriptionProvider,
    ClaudeAuthStageError, ClaudeOAuthDialect, ClaudeProvider, ClaudeSubscriptionProvider,
    ContentBlock, ContextWindowCapability, EventProvenance, FallbackChain, GeminiProvider,
    GrokAuthStageError, GrokDeviceEndpointError, GrokJwksVerifier, GrokSubscriptionProvider,
    GrokTokenVerifier, ImageSource, InvocationMetadata, LlmProvider, Message, MessageRequest,
    MessageResponse, ModelCapabilities, ModelCatalog, ModelCatalogProfile, ModelFeature,
    NativeToolGrant, OpenAIProvider, OpenAiChatGptOAuthDialect, OpenAiJwksVerifier,
    OpenAiTokenVerifier, OutputTokenLimitCapability, ProviderAllowance, ProviderBackend,
    ProviderConcreteType, ProviderEndpoints, ProviderRequest, ProviderResponse, ProviderSession,
    ProviderUsage, ReasoningCapability, SemanticTool, SessionContextConfig, StreamChunk,
    ToolAuthority, ToolBindingTable, ToolCompilePolicy, ToolOrigin, ValidatedProviderRequest,
    VerifiedGrokClaims, VerifiedOpenAiClaims, WireProtocol, WireProtocolCapability,
    XaiGrokOAuthDialect, CHATGPT_OAUTH_PROTOCOL_REVISION, CLAUDE_AUTHORIZATION_ENDPOINT,
    CLAUDE_OAUTH_BETA_HEADER, CLAUDE_OAUTH_PROTOCOL_REVISION, CLAUDE_SUBSCRIPTION_CLIENT_ID,
    CLAUDE_TOKEN_ENDPOINT, DEFAULT_CLAUDE_MODEL, GROK_OAUTH_PROTOCOL_REVISION,
    GROK_REQUIRED_TOKEN_ISSUER, GROK_SESSION_TOKEN_HEADER, GROK_SUBSCRIPTION_BASE_URL,
    OPENAI_PUBLIC_CLIENT_ID, REQUIRED_TOKEN_ISSUER, STATIC_FALLBACK_AS_OF,
    UNIVERSAL_ALIGNMENT_PROMPT, XAI_PUBLIC_CLIENT_ID,
};

#[cfg(test)]
mod wire_type_boundary_tests {
    /// The universal wire types (`Message`, `ContentBlock`, `ImageSource`) are
    /// reached through the providers facade, never through the Claude client
    /// subtree.
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
